//! Feed collection: one poll of one source, and the daemon's polling loop.

use std::time::Duration;

use anyhow::Context;
use serde::Serialize;
use signalsift_core::Profile;
use signalsift_core::profile::WorkClass;
use signalsift_ingestion::feed::{self, Entry};
use signalsift_ingestion::{Fetched, Fetcher, Validators, schedule};
use signalsift_storage::media::NewItem;
use signalsift_storage::sources::{self as repo, CachedValidators, FeedEntry, SourceRow};
use signalsift_storage::{Db, now_ms};
use tokio::sync::watch;
use url::Url;

#[derive(Debug, Serialize)]
pub struct PollReport {
    pub source_id: i64,
    pub url: String,
    /// `updated`, `not_modified` or `failed`.
    pub outcome: &'static str,
    pub entries: usize,
    pub new_items: usize,
    pub bytes: usize,
    pub wall_ms: u64,
    pub error: Option<String>,
}

/// Polls one source and records the outcome and next poll time. Errors are
/// recorded on the source (with backoff), not propagated: one broken feed
/// must not stop the others.
pub async fn poll_source(
    db: &Db,
    fetcher: &Fetcher,
    source: &SourceRow,
) -> anyhow::Result<PollReport> {
    let started = std::time::Instant::now();
    let url = Url::parse(&source.url).context("stored source URL is invalid")?;
    let key = source.url.clone();
    let cached = db.read(move |c| repo::http_validators(c, &key)).await?;
    let validators = Validators {
        etag: cached.etag,
        last_modified: cached.last_modified,
    };

    let (id, interval) = (source.id, source.poll_interval_secs);
    let mut report = PollReport {
        source_id: id,
        url: source.url.clone(),
        outcome: "failed",
        entries: 0,
        new_items: 0,
        bytes: 0,
        wall_ms: 0,
        error: None,
    };

    let result: anyhow::Result<()> = async {
        match fetcher.get(&url, &validators).await? {
            Fetched::NotModified => {
                report.outcome = "not_modified";
                db.write(move |c| {
                    repo::record_success(
                        c,
                        id,
                        schedule::after_success(now_ms(), interval, id),
                        None,
                    )
                })
                .await?;
            }
            Fetched::Body {
                body,
                validators,
                status,
                ..
            } => {
                report.bytes = body.len();
                let parsed = feed::parse(&body, &url)?;
                report.entries = parsed.entries.len();
                let entries: Vec<FeedEntry> =
                    parsed.entries.into_iter().map(to_feed_entry).collect();
                let title = parsed.title;
                let key = url.to_string();
                let created = db
                    .write(move |c| {
                        let created = repo::insert_feed_entries(c, id, &entries)?;
                        repo::store_http_validators(
                            c,
                            &key,
                            i64::from(status),
                            &CachedValidators {
                                etag: validators.etag,
                                last_modified: validators.last_modified,
                            },
                        )?;
                        repo::record_success(
                            c,
                            id,
                            schedule::after_success(now_ms(), interval, id),
                            title.as_deref(),
                        )?;
                        Ok(created)
                    })
                    .await?;
                report.new_items = created.len();
                report.outcome = "updated";
            }
        }
        Ok(())
    }
    .await;

    if let Err(e) = result {
        let message = format!("{e:#}");
        let failures = db
            .write({
                let message = message.clone();
                move |c| repo::record_failure(c, id, &message)
            })
            .await?;
        let next = schedule::after_failure(now_ms(), interval, failures, id);
        db.write(move |c| repo::set_next_poll(c, id, next)).await?;
        report.error = Some(message);
    }
    report.wall_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

fn to_feed_entry(e: Entry) -> FeedEntry {
    FeedEntry {
        links: e.links.iter().map(Url::to_string).collect(),
        item: NewItem {
            kind: e.kind,
            canonical_url: e.url.to_string(),
            external_id: e.external_id,
            title: e.title,
            author: e.author,
            published_at: e.published_ms,
            duration_ms: None,
            content_text: e.text,
            metadata_json: "{}".into(),
        },
    }
}

/// Upper bound on one sleep, so a source added through the CLI while the
/// daemon runs is picked up within this delay.
const MAX_SLEEP: Duration = Duration::from_secs(15 * 60);

/// Polls due sources one at a time until `shutdown` flips. Sleeps until the
/// next source is due; wakes early on a profile change or shutdown. Under the
/// eco profile nothing is collected.
pub async fn run_loop(
    db: Db,
    fetcher: Fetcher,
    mut profile: watch::Receiver<Profile>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        let current = *profile.borrow_and_update();
        if current.allows(WorkClass::LightCollection) {
            match db.read(|c| repo::due_sources(c, now_ms(), 16)).await {
                Ok(due) => {
                    for source in due {
                        if *shutdown.borrow()
                            || !profile.borrow().allows(WorkClass::LightCollection)
                        {
                            break;
                        }
                        match poll_source(&db, &fetcher, &source).await {
                            Ok(r) => tracing::info!(
                                source = r.source_id,
                                outcome = r.outcome,
                                new_items = r.new_items,
                                bytes = r.bytes,
                                wall_ms = r.wall_ms,
                                error = r.error.as_deref().unwrap_or(""),
                                "polled"
                            ),
                            Err(e) => tracing::error!(source = source.id, "poll failed: {e:#}"),
                        }
                    }
                }
                Err(e) => tracing::error!("cannot read due sources: {e:#}"),
            }
        }

        let sleep = match db.read(repo::next_due_at).await {
            Ok(Some(at)) if current.allows(WorkClass::LightCollection) => {
                Duration::from_millis((at - now_ms()).max(1000) as u64).min(MAX_SLEEP)
            }
            _ => MAX_SLEEP,
        };
        // `Ok(())` patterns: if a sender were dropped, `changed()` would
        // return Err immediately and an unconditional branch would spin.
        tokio::select! {
            () = tokio::time::sleep(sleep) => {}
            Ok(()) = profile.changed() => {}
            Ok(()) = shutdown.changed() => {}
        }
    }
}
