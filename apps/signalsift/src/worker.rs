//! Background job workers.
//!
//! One light worker (caption fetches) and `runtime.heavy_concurrency` heavy
//! workers (local transcription). Each claims only its own class, so a
//! fourteen-minute transcription never delays a three-second caption fetch.
//!
//! Profiles gate *claiming*: under `gaming` heavy workers claim nothing, and a
//! running heavy job is cancelled (its chunk processes killed) as soon as the
//! profile changes; the transcript stays partial and the job is released
//! without counting an attempt, to resume from its checkpoint later.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;
use signalsift_core::profile::WorkClass as ProfileClass;
use signalsift_core::{Config, Profile};
use signalsift_storage::jobs::{self, Job, WorkClass};
use signalsift_storage::{Db, now_ms};
use tokio::sync::{Notify, watch};

use signalsift_analysis::llama::LlamaServer;

use crate::{analysis, media};

pub use signalsift_storage::jobs::{
    ANALYSIS_ITEM as ANALYSIS, MEDIA_ASR as ASR, MEDIA_CAPTIONS as CAPTIONS,
};

/// Videos younger than this get a second chance at captions before local
/// transcription: YouTube often adds automatic captions hours after upload,
/// and they cost seconds instead of GPU minutes.
const CAPTION_GRACE_MS: i64 = 12 * 3600 * 1000;
const CAPTION_RECHECK_MS: i64 = 6 * 3600 * 1000;
const MAX_IDLE_SLEEP: Duration = Duration::from_secs(15 * 60);

#[derive(Deserialize)]
struct MediaPayload {
    video_id: String,
    /// Set after the first "no captions yet" deferral, so the next run
    /// escalates to transcription instead of deferring forever.
    #[serde(default)]
    rechecked: bool,
}

pub struct Shared {
    pub db: Db,
    pub config: Config,
    pub light_ready: Arc<Notify>,
    pub heavy_ready: Arc<Notify>,
    pub profile: watch::Receiver<Profile>,
    pub shutdown: watch::Receiver<bool>,
}

enum Outcome {
    Done,
    /// Try again at this time without counting an attempt.
    Defer {
        at: i64,
        note: String,
    },
    /// Interrupted by profile or shutdown; release for later.
    Interrupted,
}

pub async fn run(shared: Arc<Shared>, class: WorkClass) {
    let mut profile = shared.profile.clone();
    let mut shutdown = shared.shutdown.clone();
    let ready = match class {
        WorkClass::Light => Arc::clone(&shared.light_ready),
        WorkClass::Heavy => Arc::clone(&shared.heavy_ready),
    };
    let profile_class = match class {
        WorkClass::Light => ProfileClass::LightCollection,
        WorkClass::Heavy => ProfileClass::Heavy,
    };
    // The LLM server is kept across consecutive analysis jobs (loading a
    // model takes seconds) and stopped as soon as this worker has nothing
    // else to analyse, so no model stays resident while idle.
    let mut llm: Option<LlamaServer> = None;

    loop {
        if *shutdown.borrow() {
            stop_llm(&mut llm).await;
            return;
        }
        let allowed = profile.borrow_and_update().allows(profile_class);
        if allowed {
            match shared
                .db
                .write(move |c| jobs::claim(c, class, now_ms()))
                .await
            {
                Ok(Some(job)) => {
                    // Transcription needs the GPU memory the LLM holds.
                    if job.kind != ANALYSIS {
                        stop_llm(&mut llm).await;
                    }
                    execute(&shared, job, &mut llm).await;
                    continue;
                }
                Ok(None) => stop_llm(&mut llm).await,
                Err(e) => tracing::error!("cannot claim a job: {e:#}"),
            }
        }

        let sleep = if allowed {
            match shared.db.read(move |c| jobs::next_ready_at(c, class)).await {
                Ok(Some(at)) => {
                    Duration::from_millis((at - now_ms()).max(1000) as u64).min(MAX_IDLE_SLEEP)
                }
                _ => MAX_IDLE_SLEEP,
            }
        } else {
            stop_llm(&mut llm).await;
            MAX_IDLE_SLEEP
        };
        tokio::select! {
            () = tokio::time::sleep(sleep) => {}
            () = ready.notified() => {}
            Ok(()) = profile.changed() => {}
            Ok(()) = shutdown.changed() => {}
        }
    }
}

async fn stop_llm(llm: &mut Option<LlamaServer>) {
    if let Some(server) = llm.take() {
        let usage = server.stop().await;
        tracing::info!(
            peak_rss_mib = usage.peak_rss_kib / 1024,
            peak_vram_mib = usage.peak_vram_kib / 1024,
            "language model unloaded"
        );
    }
}

async fn execute(shared: &Shared, job: Job, llm: &mut Option<LlamaServer>) {
    let id = job.id;
    tracing::info!(job = id, kind = %job.kind, attempt = job.attempts, "job started");
    let started = std::time::Instant::now();
    let result = match job.kind.as_str() {
        CAPTIONS => captions_job(shared, &job).await,
        ASR => asr_job(shared, &job).await,
        ANALYSIS => analysis_job(shared, &job, llm).await,
        other => Err(anyhow::anyhow!("unknown job kind {other}")),
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;

    let record = match result {
        Ok(Outcome::Done) => {
            tracing::info!(job = id, elapsed_ms, "job succeeded");
            shared.db.write(move |c| jobs::succeed(c, id)).await
        }
        Ok(Outcome::Defer { at, note }) => {
            tracing::info!(job = id, %note, "job deferred");
            shared
                .db
                .write(move |c| jobs::defer(c, id, at, &note))
                .await
        }
        Ok(Outcome::Interrupted) => {
            tracing::info!(job = id, elapsed_ms, "job interrupted; released for later");
            shared
                .db
                .write(move |c| jobs::release(c, id, now_ms()))
                .await
        }
        Err(e) => {
            let message = format!("{e:#}");
            // 10 min, 20 min, 40 min, ... capped at 12 h.
            let backoff =
                ((10 * 60 * 1000i64) << (job.attempts - 1).clamp(0, 6)).min(12 * 3600 * 1000);
            let retry_at = now_ms() + backoff;
            let state = shared
                .db
                .write(move |c| jobs::fail(c, id, &message.clone(), retry_at))
                .await;
            match &state {
                Ok(s) => tracing::warn!(job = id, state = s, "job failed: {e:#}"),
                Err(_) => tracing::error!(job = id, "job failed and could not be recorded: {e:#}"),
            }
            state.map(|_| ())
        }
    };
    if let Err(e) = record {
        tracing::error!(job = id, "cannot record job outcome: {e:#}");
    }
}

async fn captions_job(shared: &Shared, job: &Job) -> anyhow::Result<Outcome> {
    let payload: MediaPayload =
        serde_json::from_str(&job.payload_json).context("invalid payload")?;
    let mut p = media::prepare(&shared.config, &shared.db, &payload.video_id).await?;
    if let Some(tid) = media::captions_step(&shared.config, &shared.db, &mut p).await? {
        media::segments_step(&shared.db, &p, tid).await?;
        queue_analysis(shared, p.item_id, tid).await?;
        return Ok(Outcome::Done);
    }

    if shared.config.asr.model.is_none() {
        tracing::info!(video = %payload.video_id, "no captions and no speech model configured");
        return Ok(Outcome::Done);
    }
    let published = p
        .info
        .upload_date
        .as_deref()
        .and_then(signalsift_core::timestamp::yyyymmdd_to_unix_ms);
    let young = published.is_some_and(|t| now_ms() - t < CAPTION_GRACE_MS);
    if young && !payload.rechecked {
        // Mark the payload so the next run escalates instead of deferring again.
        let id = job.id;
        let marked =
            serde_json::json!({ "video_id": payload.video_id, "rechecked": true }).to_string();
        shared
            .db
            .write(move |c| {
                c.execute(
                    "UPDATE jobs SET payload_json = ?2 WHERE id = ?1",
                    (id, &marked),
                )?;
                Ok(())
            })
            .await?;
        return Ok(Outcome::Defer {
            at: now_ms() + CAPTION_RECHECK_MS,
            note: "no captions yet; checking again before transcribing".into(),
        });
    }

    let payload_json = serde_json::json!({ "video_id": payload.video_id }).to_string();
    let key = format!("asr:{}", payload.video_id);
    let queued = shared
        .db
        .write(move |c| jobs::enqueue(c, ASR, &key, WorkClass::Heavy, &payload_json, 0))
        .await?;
    if queued.is_some() {
        tracing::info!(video = %payload.video_id, "no captions; local transcription queued");
        shared.heavy_ready.notify_one();
    }
    Ok(Outcome::Done)
}

async fn asr_job(shared: &Shared, job: &Job) -> anyhow::Result<Outcome> {
    let payload: MediaPayload =
        serde_json::from_str(&job.payload_json).context("invalid payload")?;
    let mut p = media::prepare(&shared.config, &shared.db, &payload.video_id).await?;
    let tid = media::asr_step(
        &shared.config,
        &shared.db,
        &mut p,
        None,
        heavy_work_revoked(shared.profile.clone(), shared.shutdown.clone()),
    )
    .await?;
    // Segments are rebuilt even for a partial transcript, so what was covered
    // is already browsable (flagged partial by the API).
    media::segments_step(&shared.db, &p, tid).await?;
    let status: String = shared
        .db
        .read(move |c| {
            Ok(
                c.query_row("SELECT status FROM transcripts WHERE id = ?1", [tid], |r| {
                    r.get(0)
                })?,
            )
        })
        .await?;
    if status == "complete" {
        queue_analysis(shared, p.item_id, tid).await?;
        Ok(Outcome::Done)
    } else {
        Ok(Outcome::Interrupted)
    }
}

/// Queues analysis of a complete transcript, if analysis is configured.
async fn queue_analysis(shared: &Shared, item_id: i64, transcript_id: i64) -> anyhow::Result<()> {
    if shared.config.analysis.model.is_none() {
        return Ok(());
    }
    let version = format!("transcript-{transcript_id}");
    let queued = shared
        .db
        .write(move |c| jobs::enqueue_analysis(c, item_id, &version))
        .await?;
    if queued.is_some() {
        shared.heavy_ready.notify_one();
    }
    Ok(())
}

#[derive(Deserialize)]
struct AnalysisPayload {
    item_id: i64,
}

async fn analysis_job(
    shared: &Shared,
    job: &Job,
    llm: &mut Option<LlamaServer>,
) -> anyhow::Result<Outcome> {
    let payload: AnalysisPayload =
        serde_json::from_str(&job.payload_json).context("invalid payload")?;
    let cancel = heavy_work_revoked(shared.profile.clone(), shared.shutdown.clone());
    match analysis::analyze_item(&shared.config, &shared.db, llm, payload.item_id, cancel).await {
        Ok(analysis::Outcome::Complete { .. }) => Ok(Outcome::Done),
        Ok(analysis::Outcome::Interrupted) => {
            // Heavy work was revoked: free the GPU now, not at the next loop.
            stop_llm(llm).await;
            Ok(Outcome::Interrupted)
        }
        Err(e) => {
            // A crashed or wedged server must not be reused by the next job.
            stop_llm(llm).await;
            Err(e)
        }
    }
}

/// Resolves when heavy work is no longer allowed: the profile changed to one
/// that forbids it, or the daemon is shutting down.
async fn heavy_work_revoked(
    mut profile: watch::Receiver<Profile>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow_and_update() || !profile.borrow_and_update().allows(ProfileClass::Heavy)
        {
            return;
        }
        tokio::select! {
            Ok(()) = profile.changed() => {}
            Ok(()) = shutdown.changed() => {}
            else => std::future::pending::<()>().await,
        }
    }
}
