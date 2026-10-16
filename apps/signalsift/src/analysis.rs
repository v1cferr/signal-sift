//! LLM analysis of a stored item: every segment scored, then the item.
//!
//! Each segment's analysis is committed as it completes, so the job is
//! resumable: a re-run skips segments already analysed with the same model,
//! prompt version and interest profile. The llama.cpp server is owned by the
//! caller (a heavy worker keeps it across consecutive analysis jobs and stops
//! it when the queue drains), so nothing stays loaded between batches.

use std::future::Future;
use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::json;
use signalsift_analysis::analyze::AnalysisError;
use signalsift_analysis::llama::{LlamaConfig, LlamaServer};
use signalsift_analysis::prompt::{ContentContext, SegmentAnalysis, language_name};
use signalsift_analysis::provider::InferenceProvider;
use signalsift_analysis::{Analyzer, InterestProfile, PROMPT_VERSION, item_relevance, split_text};
use signalsift_core::Config;
use signalsift_core::timestamp::format_hms;
use signalsift_storage::Db;
use signalsift_storage::analyses::{self, NewAnalysis};
use signalsift_storage::media::{self as repo, NewSegment};
use signalsift_storage::rusqlite::OptionalExtension;

pub enum Outcome {
    Complete {
        segments: usize,
        failed: usize,
        relevance: Option<u8>,
    },
    Interrupted,
}

struct ItemMeta {
    kind: String,
    title: String,
    author: Option<String>,
    content: Option<String>,
}

pub fn llama_config(config: &Config) -> anyhow::Result<LlamaConfig> {
    let model = config
        .analysis
        .model
        .clone()
        .context("analysis.model is not set; see config/signalsift.example.toml")?;
    Ok(LlamaConfig {
        server: config.analysis.llama_server.clone(),
        model,
        context: config.analysis.context,
        gpu_layers: config.analysis.gpu_layers,
        threads: config.analysis.threads,
        startup_timeout: Duration::from_secs(180),
        request_timeout: Duration::from_secs(300),
    })
}

pub fn load_profile(config: &Config) -> anyhow::Result<InterestProfile> {
    let path = config
        .analysis
        .interests_path()
        .context("cannot determine the interest profile path; set analysis.interests_file")?;
    if !path.exists() {
        bail!(
            "no interest profile at {}; copy config/interests.example.toml there and edit it",
            path.display()
        );
    }
    Ok(InterestProfile::load(&path)?)
}

/// Analyses one item. `server` is started on demand and left running for the
/// caller to reuse or stop. Returns `Interrupted` when `cancel` resolves.
pub async fn analyze_item(
    config: &Config,
    db: &Db,
    server: &mut Option<LlamaServer>,
    item_id: i64,
    cancel: impl Future<Output = ()>,
) -> anyhow::Result<Outcome> {
    let profile = load_profile(config)?;
    let fingerprint = profile.fingerprint();
    let meta = db
        .read(move |c| {
            Ok(c.query_row(
                "SELECT kind, title, author, content_text FROM items WHERE id = ?1",
                [item_id],
                |r| {
                    Ok(ItemMeta {
                        kind: r.get(0)?,
                        title: r.get(1)?,
                        author: r.get(2)?,
                        content: r.get(3)?,
                    })
                },
            )
            .optional()?)
        })
        .await?
        .with_context(|| format!("item {item_id} not found"))?;

    let mut segments = db.read(move |c| repo::segments(c, item_id)).await?;
    if segments.is_empty() && meta.kind == "article" {
        let parts = split_text(
            meta.content.as_deref().unwrap_or(""),
            config.analysis.article_chunk_chars,
        );
        if parts.is_empty() {
            bail!("item {item_id} has no text to analyse");
        }
        let new: Vec<NewSegment> = parts
            .into_iter()
            .map(|text| NewSegment {
                start_ms: None,
                end_ms: None,
                title: None,
                text,
            })
            .collect();
        db.write(move |c| repo::replace_segments(c, item_id, None, &new))
            .await?;
        segments = db.read(move |c| repo::segments(c, item_id)).await?;
    }
    if segments.is_empty() {
        bail!("item {item_id} has no segments yet (no transcript or text)");
    }

    if server.is_none() {
        *server = Some(LlamaServer::start(&llama_config(config)?).await?);
    }
    let llm = server.as_ref().expect("started above");
    let model = llm.model_id().to_owned();
    // Configured language, else the transcript's known spoken language;
    // only text of unknown language falls back to "same as the content".
    let language = match &config.analysis.language {
        Some(l) => Some(l.clone()),
        None => db
            .read(move |c| repo::best_transcript(c, item_id))
            .await?
            .map(|t| language_name(&t.language))
            .filter(|l| l != "auto"),
    };
    let analyzer = Analyzer {
        provider: llm,
        profile: &profile,
        language: language.as_deref(),
    };

    let (m, f) = (model.clone(), fingerprint.clone());
    let done: Vec<i64> = db
        .read(move |c| analyses::analysed_segments(c, item_id, &m, PROMPT_VERSION, &f))
        .await?;
    let pending: Vec<_> = segments.iter().filter(|s| !done.contains(&s.id)).collect();
    tracing::info!(item_id, segments = segments.len(), pending = pending.len(), model = %model, "analysing");

    tokio::pin!(cancel);
    let mut failed = 0usize;
    for seg in pending {
        let ctx = ContentContext {
            title: &meta.title,
            source: meta.author.as_deref(),
            kind: &meta.kind,
            position: position(seg.start_ms, seg.end_ms),
        };
        let result = tokio::select! {
            r = analyzer.segment(&ctx, &seg.text) => r,
            () = &mut cancel => return Ok(Outcome::Interrupted),
        };
        match result {
            Ok((a, stats)) => {
                let row = to_row(
                    item_id,
                    Some(seg.id),
                    &model,
                    &fingerprint,
                    &a,
                    None,
                    json!(stats),
                );
                db.write(move |c| analyses::upsert(c, &row)).await?;
            }
            // Twice-invalid output: record nothing for this segment rather
            // than a guessed score, and carry on with the others.
            Err(AnalysisError::Output(e)) => {
                tracing::warn!(item_id, segment = seg.id, "segment analysis rejected: {e}");
                failed += 1;
            }
            Err(AnalysisError::Inference(e)) => return Err(e.into()),
        }
    }

    // Item level, from the stored segment analyses of this model and profile.
    let rows = db
        .read(move |c| analyses::segment_analyses(c, item_id))
        .await?;
    let mut scored: Vec<(&repo::SegmentRow, SegmentAnalysis)> = Vec::new();
    for seg in &segments {
        if let Some(r) = rows.iter().find(|r| {
            r.segment_id == Some(seg.id) && r.model == model && r.profile_fingerprint == fingerprint
        }) {
            scored.push((seg, from_row(r)));
        }
    }
    if scored.is_empty() {
        bail!("no segment of item {item_id} could be analysed");
    }
    let analyses_only: Vec<SegmentAnalysis> = scored.iter().map(|(_, a)| a.clone()).collect();
    let (best_idx, relevance) = item_relevance(&analyses_only).expect("non-empty");
    let parts: Vec<(Option<String>, &str)> = scored
        .iter()
        .map(|(s, a)| (position(s.start_ms, s.end_ms), a.summary.as_str()))
        .collect();
    let ctx = ContentContext {
        title: &meta.title,
        source: meta.author.as_deref(),
        kind: &meta.kind,
        position: None,
    };
    let (summary, stats) = tokio::select! {
        r = analyzer.item(&ctx, &parts) => r?,
        () = &mut cancel => return Ok(Outcome::Interrupted),
    };

    // Applications and mentions: from the most relevant segments first.
    let mut by_score: Vec<&SegmentAnalysis> = analyses_only.iter().collect();
    by_score.sort_by_key(|a| std::cmp::Reverse(a.relevance));
    let applications = dedup(by_score.iter().flat_map(|a| a.applications.iter()), 5);
    let mentions = dedup(by_score.iter().flat_map(|a| a.mentions.iter()), 20);
    let interests = dedup(by_score.iter().flat_map(|a| a.matched_interests.iter()), 10);
    let best = &analyses_only[best_idx];
    let item_row = NewAnalysis {
        item_id,
        segment_id: None,
        model: model.clone(),
        prompt_version: PROMPT_VERSION.into(),
        profile_fingerprint: fingerprint,
        summary: summary.summary,
        topics_json: serde_json::to_string(&summary.topics)?,
        matched_interests_json: serde_json::to_string(&interests)?,
        relevance: i64::from(relevance),
        rationale: best.rationale.clone(),
        applications_json: serde_json::to_string(&applications)?,
        mentions_json: serde_json::to_string(&mentions)?,
        best_segment_id: Some(scored[best_idx].0.id),
        stats_json:
            json!({ "item_call": stats, "segments": scored.len(), "rejected_segments": failed })
                .to_string(),
    };
    db.write(move |c| analyses::upsert(c, &item_row)).await?;
    tracing::info!(
        item_id,
        relevance,
        segments = scored.len(),
        failed,
        "item analysed"
    );
    Ok(Outcome::Complete {
        segments: scored.len(),
        failed,
        relevance: Some(relevance),
    })
}

/// `signalsift analyze <item>`: runs in the foreground with its own server,
/// which is stopped at the end. Ctrl-C keeps the segments analysed so far.
pub async fn run_cli(config: Config, key: String) -> anyhow::Result<()> {
    let db = Db::open(&config.database_path()?)?;
    let item_id = db
        .read(move |c| signalsift_api::views::find_item_id(c, &key))
        .await?
        .context("no such item")?;
    let mut server = None;
    let started = std::time::Instant::now();
    let outcome = analyze_item(
        &config,
        &db,
        &mut server,
        item_id,
        crate::media::interrupted(),
    )
    .await;
    let usage = match server {
        Some(s) => Some(s.stop().await),
        None => None,
    };
    let summary = match outcome? {
        Outcome::Complete {
            segments,
            failed,
            relevance,
        } => json!({
            "item_id": item_id, "status": "complete", "segments": segments,
            "rejected_segments": failed, "relevance": relevance,
        }),
        Outcome::Interrupted => json!({ "item_id": item_id, "status": "interrupted" }),
    };
    let detail = db
        .read(move |c| signalsift_api::views::item_detail(c, item_id))
        .await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "result": summary,
            "wall_s": started.elapsed().as_secs_f64(),
            "server": usage,
            "analysis": detail.and_then(|d| d.analysis),
        }))?
    );
    Ok(())
}

fn position(start: Option<i64>, end: Option<i64>) -> Option<String> {
    let s = start?;
    let e = end.unwrap_or(s);
    Some(format!(
        "{}-{}",
        format_hms(s.max(0) as u64),
        format_hms(e.max(0) as u64)
    ))
}

fn to_row(
    item_id: i64,
    segment_id: Option<i64>,
    model: &str,
    fingerprint: &str,
    a: &SegmentAnalysis,
    best_segment_id: Option<i64>,
    stats: serde_json::Value,
) -> NewAnalysis {
    NewAnalysis {
        item_id,
        segment_id,
        model: model.into(),
        prompt_version: PROMPT_VERSION.into(),
        profile_fingerprint: fingerprint.into(),
        summary: a.summary.clone(),
        topics_json: serde_json::to_string(&a.topics).unwrap_or_else(|_| "[]".into()),
        matched_interests_json: serde_json::to_string(&a.matched_interests)
            .unwrap_or_else(|_| "[]".into()),
        relevance: i64::from(a.relevance),
        rationale: a.rationale.clone(),
        applications_json: serde_json::to_string(&a.applications).unwrap_or_else(|_| "[]".into()),
        mentions_json: serde_json::to_string(&a.mentions).unwrap_or_else(|_| "[]".into()),
        best_segment_id,
        stats_json: stats.to_string(),
    }
}

fn from_row(r: &analyses::AnalysisRow) -> SegmentAnalysis {
    let list = |s: &str| serde_json::from_str::<Vec<String>>(s).unwrap_or_default();
    SegmentAnalysis {
        summary: r.summary.clone(),
        topics: list(&r.topics_json),
        matched_interests: list(&r.matched_interests_json),
        relevance: r.relevance.clamp(0, 100) as u8,
        rationale: r.rationale.clone(),
        applications: list(&r.applications_json),
        mentions: list(&r.mentions_json),
    }
}

fn dedup<'a>(items: impl Iterator<Item = &'a String>, max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for i in items {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(i)) {
            out.push(i.clone());
        }
        if out.len() == max {
            break;
        }
    }
    out
}
