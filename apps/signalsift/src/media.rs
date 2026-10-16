//! `signalsift media`: the audiovisual pipeline, end to end.
//!
//! ingest = probe metadata → store item → captions if any, else local ASR in
//! checkpointed chunks → segments. Interrupting it (Ctrl-C, SIGTERM) leaves a
//! transcript marked partial that the next run resumes from its last chunk.
//!
//! This is a manual command, so it does not consult the execution profile:
//! running it is an explicit request. The daemon's scheduler applies profiles.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use clap::{Subcommand, ValueEnum};
use serde::Serialize;
use signalsift_api::views;
use signalsift_core::timestamp::yyyymmdd_to_unix_ms;
use signalsift_core::{Config, links};
use signalsift_media::cue::{Cue, join_at_boundary, normalize};
use signalsift_media::json3;
use signalsift_media::segment::{Chapter, WindowPolicy, segment};
use signalsift_media::subprocess::{Invocation, Limits};
use signalsift_media::whisper::{
    WhisperConfig, default_chunk_limits, plan_chunks, transcribe_chunk,
};
use signalsift_media::youtube::{self, TrackOrigin, VideoInfo, YtDlp};
use signalsift_storage::Db;
use signalsift_storage::media::{self as repo, CueRow, NewItem, NewSegment};

#[derive(Subcommand)]
pub enum MediaCommand {
    /// Store a YouTube video: captions when available, local transcription otherwise.
    Ingest {
        /// Video URL or 11-character id.
        url: String,
        /// When to use local speech recognition.
        #[arg(long, value_enum, default_value_t = AsrMode::Auto)]
        asr: AsrMode,
        /// Spoken language for speech recognition (overrides platform metadata).
        #[arg(long)]
        language: Option<String>,
    },
    /// Queue a video in the running daemon instead of processing it here.
    Queue {
        /// Video URL or 11-character id.
        url: String,
        /// Transcribe locally even if captions exist.
        #[arg(long)]
        force_asr: bool,
    },
    /// Print a stored item with transcript status and segments as JSON.
    Show {
        /// Item id or YouTube video id.
        item: String,
        /// Include every transcript cue (large for long videos).
        #[arg(long)]
        cues: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AsrMode {
    /// Only when no usable captions exist.
    Auto,
    /// Never; fail if there are no captions.
    Never,
    /// Always, even if captions exist (for benchmarking against them).
    Force,
}

pub async fn run(config: Config, cmd: MediaCommand) -> anyhow::Result<()> {
    let db = Db::open(&config.database_path()?)?;
    match cmd {
        MediaCommand::Ingest { url, asr, language } => {
            let report = ingest(&config, &db, &url, asr, language.as_deref()).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if report.transcript_status != "complete" {
                bail!(
                    "transcript is {} ({:.1}% covered)",
                    report.transcript_status,
                    report.coverage * 100.0
                );
            }
            Ok(())
        }
        MediaCommand::Queue { url, force_asr } => {
            crate::client::queue_media(&config, &url, force_asr).await
        }
        MediaCommand::Show { item, cues } => show(&db, item, cues).await,
    }
}

#[derive(Serialize)]
struct Report {
    item_id: i64,
    video_id: String,
    title: String,
    duration_ms: Option<u64>,
    transcript_origin: String,
    transcript_status: String,
    coverage: f64,
    cues: usize,
    segments: usize,
    references: usize,
    wall_ms: u64,
    /// Every external process run, with its wall time and sampled resources.
    invocations: Vec<Invocation>,
}

/// Metadata fetched, limits checked, item and description links stored.
pub struct Prepared {
    pub id: String,
    pub item_id: i64,
    pub info: VideoInfo,
    pub references: usize,
    pub invocations: Vec<Invocation>,
    dir: PathBuf,
    info_path: PathBuf,
    ytdlp: YtDlp,
}

pub async fn prepare(config: &Config, db: &Db, input: &str) -> anyhow::Result<Prepared> {
    let id = youtube::video_id(input).context("not a YouTube video URL or id")?;
    let dir = config.cache_dir()?.join("media").join(&id);
    let ytdlp = YtDlp {
        program: config.media.yt_dlp.clone(),
        limits: Limits {
            timeout: config.media.subprocess_timeout,
            max_stdout: config.media.max_output_bytes,
        },
    };
    let info_path = dir.join("info.json");
    let (info, inv) = ytdlp
        .probe(&id, &info_path)
        .await
        .context("fetching video metadata")?;
    if info.is_live_now() {
        bail!("{id} is live or upcoming; ingest it after the broadcast ends");
    }
    if let Some(d) = info.duration_ms()
        && d > config.media.max_duration.as_millis() as u64
    {
        bail!(
            "{id} lasts {} s, above media.max_duration ({} s)",
            d / 1000,
            config.media.max_duration.as_secs()
        );
    }
    let item_id = store_item(db, &info).await?;
    let references = store_description_links(db, item_id, &info).await?;
    tracing::info!(item_id, video = %id, title = %info.title, "item stored");
    Ok(Prepared {
        id,
        item_id,
        info,
        references,
        invocations: vec![inv],
        dir,
        info_path,
        ytdlp,
    })
}

/// Stores the best caption track. `None` when the video has no usable one.
pub async fn captions_step(
    config: &Config,
    db: &Db,
    p: &mut Prepared,
) -> anyhow::Result<Option<i64>> {
    let Some(track) = youtube::choose_track(&p.info, &config.media.subtitle_languages) else {
        return Ok(None);
    };
    tracing::info!(key = %track.key, origin = track.origin.as_str(), "using captions");
    let (path, inv) = p
        .ytdlp
        .download_subtitles(&p.info_path, &track, &p.dir.join("subtitles"))
        .await
        .context("downloading captions")?;
    p.invocations.push(inv);
    let bytes = read_capped(&path, config.media.max_output_bytes).await?;
    let cues = normalize(json3::parse(&bytes)?);
    let _ = tokio::fs::remove_file(&path).await;
    let engine = format!("youtube {} via yt-dlp", track.origin.as_str());
    let tid = store_full_transcript(
        db,
        p.item_id,
        track.origin,
        &track.language,
        &engine,
        &p.info,
        cues,
    )
    .await?;
    Ok(Some(tid))
}

/// Local transcription, resumable from the last committed chunk. When
/// `cancel` resolves, the running chunk's processes are killed and the
/// transcript is left partial. Returns the transcript id; its status says
/// whether it is complete.
pub async fn asr_step(
    config: &Config,
    db: &Db,
    p: &mut Prepared,
    language: Option<&str>,
    cancel: impl std::future::Future<Output = ()>,
) -> anyhow::Result<i64> {
    let lang = language
        .map(str::to_owned)
        .or_else(|| p.info.language.clone())
        .unwrap_or_else(|| config.asr.language.clone());
    let (ytdlp, info, info_path, dir, item_id) =
        (&p.ytdlp, &p.info, &p.info_path, &p.dir, p.item_id);
    transcribe(
        config,
        db,
        ytdlp,
        info,
        info_path,
        dir,
        item_id,
        &lang,
        &mut p.invocations,
        cancel,
    )
    .await
}

pub async fn segments_step(db: &Db, p: &Prepared, transcript_id: i64) -> anyhow::Result<usize> {
    build_segments(db, p.item_id, transcript_id, &p.info).await
}

async fn ingest(
    config: &Config,
    db: &Db,
    input: &str,
    asr: AsrMode,
    language: Option<&str>,
) -> anyhow::Result<Report> {
    let started = Instant::now();
    let mut p = prepare(config, db, input).await?;
    let captions = match asr {
        AsrMode::Force => None,
        AsrMode::Auto | AsrMode::Never => captions_step(config, db, &mut p).await?,
    };
    let transcript_id = match (captions, asr) {
        (Some(tid), _) => tid,
        (None, AsrMode::Never) => {
            bail!("{} has no usable captions and --asr never was given", p.id)
        }
        (None, _) => asr_step(config, db, &mut p, language, interrupted()).await?,
    };

    let segments = segments_step(db, &p, transcript_id).await?;
    let state = db
        .read(move |c| Ok(c.query_row(
            "SELECT status, covered_ms, duration_ms, origin, (SELECT count(*) FROM transcript_cues WHERE transcript_id = ?1)
             FROM transcripts WHERE id = ?1",
            [transcript_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, String>(3)?, r.get::<_, i64>(4)?)),
        )?))
        .await?;
    let coverage = state
        .2
        .filter(|d| *d > 0)
        .map_or(1.0, |d| (state.1 as f64 / d as f64).min(1.0));

    Ok(Report {
        item_id: p.item_id,
        video_id: p.id.clone(),
        title: p.info.title.clone(),
        duration_ms: p.info.duration_ms(),
        transcript_origin: state.3,
        transcript_status: state.0,
        coverage,
        cues: state.4 as usize,
        segments,
        references: p.references,
        wall_ms: started.elapsed().as_millis() as u64,
        invocations: p.invocations,
    })
}

async fn store_item(db: &Db, info: &VideoInfo) -> anyhow::Result<i64> {
    let metadata = serde_json::json!({
        "platform": "youtube",
        "channel_id": info.channel_id,
        "uploader_id": info.uploader_id,
        "live_status": info.live_status,
        "language": info.language,
        "chapters": info.chapters,
    });
    let item = NewItem {
        kind: "video",
        canonical_url: youtube::watch_url(&info.id),
        external_id: Some(info.id.clone()),
        title: info.title.clone(),
        author: info.channel.clone(),
        published_at: info.upload_date.as_deref().and_then(yyyymmdd_to_unix_ms),
        duration_ms: info.duration_ms().map(|d| d as i64),
        content_text: info.description.clone(),
        metadata_json: metadata.to_string(),
    };
    Ok(db.write(move |c| repo::upsert_item(c, &item)).await?)
}

async fn store_description_links(db: &Db, item_id: i64, info: &VideoInfo) -> anyhow::Result<usize> {
    let urls: Vec<(String, Option<i64>)> =
        links::extract_urls(info.description.as_deref().unwrap_or(""))
            .into_iter()
            .map(|u| (u.to_string(), None))
            .collect();
    let count = urls.len();
    db.write(move |c| repo::add_references(c, item_id, "description", &urls))
        .await?;
    Ok(count)
}

async fn store_full_transcript(
    db: &Db,
    item_id: i64,
    origin: TrackOrigin,
    language: &str,
    engine: &str,
    info: &VideoInfo,
    cues: Vec<Cue>,
) -> anyhow::Result<i64> {
    let (language, engine) = (language.to_owned(), engine.to_owned());
    let duration = info.duration_ms().map(|d| d as i64);
    // Captions cover the whole video by construction; coverage is the
    // duration (or the last cue when the platform reports none).
    let covered = duration.unwrap_or_else(|| cues.last().map_or(0, |c| c.end_ms as i64));
    let rows = to_rows(&cues);
    Ok(db
        .write(move |c| {
            let t =
                repo::open_transcript(c, item_id, origin.as_str(), &language, &engine, duration)?;
            if t.status != "complete" {
                c.execute(
                    "DELETE FROM transcript_cues WHERE transcript_id = ?1",
                    [t.id],
                )?;
                repo::append_cues(c, t.id, &rows, covered)?;
                repo::finish_transcript(c, t.id, "complete")?;
            }
            Ok(t.id)
        })
        .await?)
}

#[allow(clippy::too_many_arguments)]
async fn transcribe(
    config: &Config,
    db: &Db,
    ytdlp: &YtDlp,
    info: &VideoInfo,
    info_path: &Path,
    dir: &Path,
    item_id: i64,
    language: &str,
    invocations: &mut Vec<Invocation>,
    cancel: impl std::future::Future<Output = ()>,
) -> anyhow::Result<i64> {
    let model = config.asr.model.clone().context(
        "no captions available and no speech model configured; set asr.model \
         (see docs/benchmarks/whisper-arc-b580.md)",
    )?;
    let duration_ms = info
        .duration_ms()
        .context("platform reports no duration; cannot plan transcription chunks")?;
    let cfg = WhisperConfig {
        whisper_cli: config.asr.whisper_cli.clone(),
        ffmpeg: config.media.ffmpeg.clone(),
        model,
        vad_model: config.asr.vad_model.clone(),
        language: language.to_owned(),
        threads: config.asr.threads,
        use_gpu: config.asr.use_gpu,
        limits: default_chunk_limits(config.asr.chunk),
    };
    let engine = cfg.engine_id();
    let (lang, eng) = (language.to_owned(), engine.clone());
    let eng_lookup = engine.clone();
    if let Some(done) = db
        .read(move |c| repo::complete_transcript_by_engine(c, item_id, "asr", &eng_lookup))
        .await?
    {
        tracing::info!(transcript = done.id, language = %done.language, "complete transcript from this engine exists; reusing it");
        return Ok(done.id);
    }
    let t = db
        .write(move |c| {
            repo::open_transcript(c, item_id, "asr", &lang, &eng, Some(duration_ms as i64))
        })
        .await?;
    if t.status == "complete" {
        tracing::info!(transcript = t.id, "transcript already complete");
        return Ok(t.id);
    }

    // Audio downloads get a longer limit than metadata calls: a multi-hour
    // podcast is ~100 MB even at the low bitrate chosen.
    let audio_dl = YtDlp {
        program: ytdlp.program.clone(),
        limits: Limits {
            timeout: Duration::from_secs(1800),
            max_stdout: ytdlp.limits.max_stdout,
        },
    };
    let (audio, inv) = match audio_dl.download_audio(info_path, &dir.join("audio")).await {
        Ok(r) => r,
        Err(e) => {
            // Nothing transcribed yet: record the attempt as failed rather
            // than leave an empty "in progress" transcript behind. A partial
            // one keeps its status and resumes on the next attempt.
            let (tid, covered) = (t.id, t.covered_ms);
            if covered == 0 {
                db.write(move |c| repo::finish_transcript(c, tid, "failed"))
                    .await?;
            }
            return Err(anyhow::Error::new(e).context("downloading audio"));
        }
    };
    invocations.push(inv);

    let chunks = plan_chunks(
        duration_ms,
        config.asr.chunk.as_millis() as u64,
        config.asr.overlap.as_millis() as u64,
    );
    let resume_from = t.covered_ms as u64;
    let pending: Vec<_> = chunks
        .into_iter()
        .filter(|c| c.keep_end_ms > resume_from)
        .collect();
    tracing::info!(
        transcript = t.id,
        engine = %engine,
        resume_from_ms = resume_from,
        chunks = pending.len(),
        "transcribing"
    );

    let tid = t.id;
    let work = async {
        for chunk in pending {
            let r = transcribe_chunk(&cfg, &audio, chunk, &dir.join("work")).await?;
            let last = db.read(move |c| repo::last_cue(c, tid)).await?;
            let prev = last.map(|l| Cue {
                start_ms: l.start_ms as u64,
                end_ms: l.end_ms as u64,
                text: l.text,
            });
            let cues = join_at_boundary(prev.as_ref(), r.cues);
            let rows = to_rows(&cues);
            let covered = chunk.keep_end_ms as i64;
            db.write(move |c| repo::append_cues(c, tid, &rows, covered))
                .await?;
            tracing::info!(
                chunk = chunk.index,
                cues = cues.len(),
                covered_ms = covered,
                whisper_ms = r.whisper.wall_ms,
                peak_vram_mib = r.whisper.usage.peak_vram_kib / 1024,
                "chunk committed"
            );
            invocations.push(r.ffmpeg);
            invocations.push(r.whisper);
        }
        anyhow::Ok(())
    };

    let outcome = tokio::select! {
        r = work => r.map(|()| true),
        () = cancel => Ok(false),
    };
    let final_status = match &outcome {
        Ok(true) => "complete",
        Ok(false) => {
            tracing::warn!("cancelled; transcript kept as partial and resumable");
            "partial"
        }
        Err(e) => {
            tracing::error!("transcription stopped: {e:#}");
            "partial"
        }
    };
    let status = db
        .write(move |c| repo::finish_transcript(c, tid, final_status))
        .await?;
    if status == "complete" && !config.asr.keep_audio {
        let _ = tokio::fs::remove_dir_all(dir.join("audio")).await;
    }
    let _ = tokio::fs::remove_dir_all(dir.join("work")).await;
    outcome.map(|_| tid)
}

async fn build_segments(
    db: &Db,
    item_id: i64,
    transcript_id: i64,
    info: &VideoInfo,
) -> anyhow::Result<usize> {
    let chapters: Vec<Chapter> = info
        .chapters
        .iter()
        .flatten()
        .map(|c| Chapter {
            start_ms: (c.start_time * 1000.0) as u64,
            end_ms: (c.end_time * 1000.0) as u64,
            title: c.title.clone(),
        })
        .collect();
    Ok(db
        .write(move |c| {
            let cues: Vec<Cue> = repo::cues(c, transcript_id)?
                .into_iter()
                .map(|r| Cue {
                    start_ms: r.start_ms as u64,
                    end_ms: r.end_ms as u64,
                    text: r.text,
                })
                .collect();
            let segs: Vec<NewSegment> = segment(&cues, &chapters, WindowPolicy::default())
                .into_iter()
                .map(|s| NewSegment {
                    start_ms: Some(s.start_ms as i64),
                    end_ms: Some(s.end_ms as i64),
                    title: s.title,
                    text: s.text,
                })
                .collect();
            repo::replace_segments(c, item_id, Some(transcript_id), &segs)
        })
        .await?)
}

async fn show(db: &Db, key: String, with_cues: bool) -> anyhow::Result<()> {
    let out = db
        .read(move |c| {
            let Some(id) = views::find_item_id(c, &key)? else {
                return Ok(None);
            };
            let Some(item) = views::item_detail(c, id)? else {
                return Ok(None);
            };
            let segments = views::item_segments(c, id)?.unwrap_or_default();
            let cues = match (with_cues, repo::best_transcript(c, id)?) {
                (true, Some(t)) => Some(repo::cues(c, t.id)?),
                _ => None,
            };
            Ok(Some(
                serde_json::json!({ "item": item, "segments": segments, "cues": cues }),
            ))
        })
        .await?
        .context("no such item")?;
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn to_rows(cues: &[Cue]) -> Vec<CueRow> {
    cues.iter()
        .map(|c| CueRow {
            start_ms: c.start_ms as i64,
            end_ms: c.end_ms as i64,
            text: c.text.clone(),
        })
        .collect()
}

async fn read_capped(path: &PathBuf, limit: u64) -> anyhow::Result<Vec<u8>> {
    let len = tokio::fs::metadata(path).await?.len();
    if len > limit {
        bail!(
            "{} is {len} bytes, above media.max_output_bytes",
            path.display()
        );
    }
    Ok(tokio::fs::read(path).await?)
}

/// Resolves on SIGINT or SIGTERM.
pub(crate) async fn interrupted() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut int), Ok(mut term)) = (
            signal(SignalKind::interrupt()),
            signal(SignalKind::terminate()),
        ) else {
            std::future::pending::<()>().await;
            return;
        };
        tokio::select! {
            _ = int.recv() => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
