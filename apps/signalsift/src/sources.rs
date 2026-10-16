//! `signalsift sources`: register, import, list and poll sources.

use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::Subcommand;
use serde::Deserialize;
use signalsift_core::Config;
use signalsift_ingestion::Fetcher;
use signalsift_ingestion::canonical::canonicalize;
use signalsift_storage::sources::{self as repo, NewSource};
use signalsift_storage::{Db, now_ms};
use url::Url;

use crate::collect;

#[derive(Subcommand)]
pub enum SourcesCommand {
    /// Add (or update) a feed: RSS, Atom, JSON Feed or a YouTube channel.
    Add {
        /// Feed URL, or a youtube.com/channel/UC... URL.
        url: String,
        /// Creator name; sources with the same creator are grouped.
        #[arg(long)]
        creator: Option<String>,
        /// Poll interval in seconds (minimum 300).
        #[arg(long)]
        interval: Option<u64>,
    },
    /// Add every source listed in a TOML file (see config/sources.example.toml).
    Import { file: PathBuf },
    /// Print sources and their poll state as JSON.
    List,
    /// Poll now: due sources, every source with --all, or one with --id.
    Poll {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        id: Option<i64>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportFile {
    #[serde(default)]
    creator: Vec<ImportCreator>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportCreator {
    name: String,
    #[serde(default)]
    feeds: Vec<String>,
    #[serde(default)]
    interval: Option<u64>,
}

pub async fn run(config: Config, cmd: SourcesCommand) -> anyhow::Result<()> {
    let db = Db::open(&config.database_path()?)?;
    let default_interval = config.collection.default_interval.as_secs();
    match cmd {
        SourcesCommand::Add {
            url,
            creator,
            interval,
        } => {
            let id = add(&db, &url, creator, interval.unwrap_or(default_interval)).await?;
            let row = db.read(move |c| repo::get_source(c, id)).await?;
            println!("{}", serde_json::to_string_pretty(&row)?);
        }
        SourcesCommand::Import { file } => {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let parsed: ImportFile =
                toml::from_str(&text).with_context(|| format!("parsing {}", file.display()))?;
            let mut added = 0;
            for c in parsed.creator {
                for feed in &c.feeds {
                    add(
                        &db,
                        feed,
                        Some(c.name.clone()),
                        c.interval.unwrap_or(default_interval),
                    )
                    .await?;
                    added += 1;
                }
            }
            eprintln!("imported {added} sources");
        }
        SourcesCommand::List => {
            let rows = db.read(repo::list_sources).await?;
            println!("{}", serde_json::to_string_pretty(&rows)?);
        }
        SourcesCommand::Poll { all, id } => {
            let fetcher = fetcher(&config)?;
            let targets = db
                .read(move |c| match (id, all) {
                    (Some(id), _) => Ok(repo::get_source(c, id)?.into_iter().collect()),
                    (None, true) => repo::list_sources(c),
                    (None, false) => repo::due_sources(c, now_ms(), 1000),
                })
                .await?;
            if targets.is_empty() {
                eprintln!("nothing due; use --all to poll every source");
            }
            let mut reports = Vec::new();
            for s in targets {
                let backfill = config.collection.backfill.as_millis() as i64;
                reports.push(collect::poll_source(&db, &fetcher, &s, backfill).await?);
            }
            println!("{}", serde_json::to_string_pretty(&reports)?);
        }
    }
    Ok(())
}

pub fn fetcher(config: &Config) -> anyhow::Result<Fetcher> {
    Fetcher::new(
        config.collection.fetch_timeout,
        config.collection.max_feed_bytes,
    )
    .context("building the HTTP client")
}

async fn add(db: &Db, input: &str, creator: Option<String>, interval: u64) -> anyhow::Result<i64> {
    if interval < 300 {
        bail!("poll interval must be at least 300 seconds");
    }
    let (kind, url) = classify(input)?;
    let source = NewSource {
        kind: kind.into(),
        url: url.to_string(),
        creator,
        title: None,
        poll_interval_secs: interval as i64,
    };
    Ok(db.write(move |c| repo::upsert_source(c, &source)).await?)
}

/// Normalizes a source URL and infers its kind. YouTube channel pages are
/// turned into the channel's public Atom feed; handles (`@name`) are refused
/// because resolving them needs a page scrape, which `add` does not do.
fn classify(input: &str) -> anyhow::Result<(&'static str, Url)> {
    let url = Url::parse(input.trim()).context("not a URL")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("only http(s) sources are supported");
    }
    let host = url.host_str().unwrap_or("").trim_start_matches("www.");
    if host == "youtube.com" {
        let mut segs = url.path_segments().into_iter().flatten();
        match segs.next() {
            Some("feeds") => return Ok(("youtube_channel", url.clone())),
            Some("channel") => {
                let id = segs
                    .next()
                    .filter(|id| id.starts_with("UC") && id.len() == 24);
                let id = id.context("expected youtube.com/channel/UC<22 characters>")?;
                let feed = Url::parse(&format!(
                    "https://www.youtube.com/feeds/videos.xml?channel_id={id}"
                ))?;
                return Ok(("youtube_channel", feed));
            }
            Some(s) if s.starts_with('@') => bail!(
                "YouTube handles are not resolved automatically; use the channel URL \
                 (youtube.com/channel/UC...) shown under the channel's About > Share"
            ),
            _ => bail!("unsupported YouTube URL; use a channel or feed URL"),
        }
    }
    Ok(("feed", canonicalize(&url)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn youtube_channels_become_feeds() {
        let (kind, url) =
            classify("https://www.youtube.com/channel/UCLW51-XEzuOm5RwPMChHBMw").unwrap();
        assert_eq!(kind, "youtube_channel");
        assert_eq!(
            url.as_str(),
            "https://www.youtube.com/feeds/videos.xml?channel_id=UCLW51-XEzuOm5RwPMChHBMw"
        );
        assert!(classify("https://www.youtube.com/@GutoGalego").is_err());
    }

    #[test]
    fn feeds_are_canonicalized_and_schemes_checked() {
        let (kind, url) = classify("https://akitaonrails.com/index.xml?utm_source=x").unwrap();
        assert_eq!(
            (kind, url.as_str()),
            ("feed", "https://akitaonrails.com/index.xml")
        );
        assert!(classify("file:///etc/passwd").is_err());
    }

    #[test]
    fn example_import_file_parses() {
        let text = include_str!("../../../config/sources.example.toml");
        let parsed: ImportFile = toml::from_str(text).unwrap();
        assert!(parsed.creator.iter().all(|c| !c.feeds.is_empty()));
        for c in &parsed.creator {
            for f in &c.feeds {
                classify(f).unwrap();
            }
        }
    }
}
