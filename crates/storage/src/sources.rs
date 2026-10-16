//! Creators, sources, poll state and the HTTP validator cache.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::media::{NewItem, upsert_item_if_new};
use crate::{Result, now_ms};

#[derive(Debug, Clone)]
pub struct NewSource {
    pub kind: String,
    pub url: String,
    pub creator: Option<String>,
    pub title: Option<String>,
    pub poll_interval_secs: i64,
}

/// Adds a source, or updates creator/title/interval if the URL exists.
/// Creators are matched by name, so several sources (blog, channel,
/// podcast) of one person share a creator row.
pub fn upsert_source(conn: &Connection, s: &NewSource) -> Result<i64> {
    let creator_id: Option<i64> = match &s.creator {
        Some(name) => Some(conn.query_row(
            "INSERT INTO creators (name, created_at) VALUES (?1, ?2)
             ON CONFLICT (name) DO UPDATE SET name = excluded.name
             RETURNING id",
            params![name, now_ms()],
            |r| r.get(0),
        )?),
        None => None,
    };
    Ok(conn.query_row(
        "INSERT INTO sources (creator_id, kind, url, title, poll_interval_secs, next_poll_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)
         ON CONFLICT (url) DO UPDATE SET
            creator_id = COALESCE(excluded.creator_id, sources.creator_id),
            title = COALESCE(excluded.title, sources.title),
            poll_interval_secs = excluded.poll_interval_secs
         RETURNING id",
        params![creator_id, s.kind, s.url, s.title, s.poll_interval_secs, now_ms()],
        |r| r.get(0),
    )?)
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceRow {
    pub id: i64,
    pub kind: String,
    pub url: String,
    pub title: Option<String>,
    pub creator: Option<String>,
    pub enabled: bool,
    pub poll_interval_secs: i64,
    pub next_poll_at: i64,
    pub last_success_at: Option<i64>,
    pub last_error: Option<String>,
    pub consecutive_failures: i64,
    pub items: i64,
}

const SOURCE_SELECT: &str =
    "SELECT s.id, s.kind, s.url, s.title, c.name, s.enabled, s.poll_interval_secs,
        s.next_poll_at, s.last_success_at, s.last_error, s.consecutive_failures,
        (SELECT count(*) FROM items i WHERE i.source_id = s.id)
    FROM sources s LEFT JOIN creators c ON c.id = s.creator_id";

fn source_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SourceRow> {
    Ok(SourceRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        url: r.get(2)?,
        title: r.get(3)?,
        creator: r.get(4)?,
        enabled: r.get::<_, i64>(5)? == 1,
        poll_interval_secs: r.get(6)?,
        next_poll_at: r.get(7)?,
        last_success_at: r.get(8)?,
        last_error: r.get(9)?,
        consecutive_failures: r.get(10)?,
        items: r.get(11)?,
    })
}

pub fn list_sources(conn: &Connection) -> Result<Vec<SourceRow>> {
    let mut stmt = conn.prepare_cached(&format!("{SOURCE_SELECT} ORDER BY c.name, s.id"))?;
    let rows = stmt.query_map([], source_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get_source(conn: &Connection, id: i64) -> Result<Option<SourceRow>> {
    Ok(conn
        .query_row(
            &format!("{SOURCE_SELECT} WHERE s.id = ?1"),
            [id],
            source_row,
        )
        .optional()?)
}

/// Enabled sources whose next poll time has passed, oldest first.
pub fn due_sources(conn: &Connection, now: i64, limit: u32) -> Result<Vec<SourceRow>> {
    let mut stmt = conn.prepare_cached(&format!(
        "{SOURCE_SELECT} WHERE s.enabled = 1 AND s.next_poll_at <= ?1 ORDER BY s.next_poll_at LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![now, limit], source_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// When the next source becomes due, if any source is enabled.
pub fn next_due_at(conn: &Connection) -> Result<Option<i64>> {
    Ok(conn.query_row(
        "SELECT MIN(next_poll_at) FROM sources WHERE enabled = 1",
        [],
        |r| r.get(0),
    )?)
}

pub fn record_success(
    conn: &Connection,
    id: i64,
    next_poll_at: i64,
    title: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE sources SET last_success_at = ?2, next_poll_at = ?3, last_error = NULL,
                consecutive_failures = 0, title = COALESCE(title, ?4)
         WHERE id = ?1",
        params![id, now_ms(), next_poll_at, title],
    )?;
    Ok(())
}

/// Records a failure and returns the new consecutive-failure count, which the
/// caller uses to compute the backoff.
pub fn record_failure(conn: &Connection, id: i64, error: &str) -> Result<i64> {
    Ok(conn.query_row(
        "UPDATE sources SET last_error = ?2, consecutive_failures = consecutive_failures + 1
         WHERE id = ?1 RETURNING consecutive_failures",
        params![id, truncate(error, 1000)],
        |r| r.get(0),
    )?)
}

pub fn set_next_poll(conn: &Connection, id: i64, next_poll_at: i64) -> Result<()> {
    conn.execute(
        "UPDATE sources SET next_poll_at = ?2 WHERE id = ?1",
        params![id, next_poll_at],
    )?;
    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CachedValidators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

pub fn http_validators(conn: &Connection, url: &str) -> Result<CachedValidators> {
    Ok(conn
        .query_row(
            "SELECT etag, last_modified FROM http_cache WHERE url = ?1",
            [url],
            |r| {
                Ok(CachedValidators {
                    etag: r.get(0)?,
                    last_modified: r.get(1)?,
                })
            },
        )
        .optional()?
        .unwrap_or_default())
}

pub fn store_http_validators(
    conn: &Connection,
    url: &str,
    status: i64,
    v: &CachedValidators,
) -> Result<()> {
    conn.execute(
        "INSERT INTO http_cache (url, etag, last_modified, status, fetched_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (url) DO UPDATE SET etag = excluded.etag, last_modified = excluded.last_modified,
            status = excluded.status, fetched_at = excluded.fetched_at",
        params![url, v.etag, v.last_modified, status, now_ms()],
    )?;
    Ok(())
}

pub struct FeedEntry {
    pub item: NewItem,
    pub links: Vec<String>,
}

/// Stores feed entries in one transaction. Existing items (same canonical URL
/// or same platform id) are left untouched, so re-polling a feed is
/// idempotent. Returns `(item id, index into entries)` for each new item.
pub fn insert_feed_entries(
    conn: &mut Connection,
    source_id: i64,
    entries: &[FeedEntry],
) -> Result<Vec<(i64, usize)>> {
    let tx = conn.transaction()?;
    let mut created = Vec::new();
    for (index, e) in entries.iter().enumerate() {
        if let Some(id) = upsert_item_if_new(&tx, Some(source_id), &e.item)? {
            let refs: Vec<(String, Option<i64>)> =
                e.links.iter().map(|u| (u.clone(), None)).collect();
            crate::media::add_references(&tx, id, "article", &refs)?;
            created.push((id, index));
        }
    }
    tx.commit()?;
    Ok(created)
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;

    fn source(url: &str, creator: &str) -> NewSource {
        NewSource {
            kind: "feed".into(),
            url: url.into(),
            creator: Some(creator.into()),
            title: None,
            poll_interval_secs: 3600,
        }
    }

    fn entry(url: &str, ext: Option<&str>) -> FeedEntry {
        FeedEntry {
            item: NewItem {
                kind: if ext.is_some() { "video" } else { "article" },
                canonical_url: url.into(),
                external_id: ext.map(Into::into),
                title: "t".into(),
                author: None,
                published_at: None,
                duration_ms: None,
                content_text: Some("body".into()),
                metadata_json: "{}".into(),
            },
            links: vec!["https://nixos.org/manual/".into()],
        }
    }

    #[tokio::test]
    async fn creators_group_sources_and_upserts_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        let rows = db
            .write(|c| {
                let a = upsert_source(
                    c,
                    &source("https://akitaonrails.com/index.xml", "Fabio Akita"),
                )?;
                let b = upsert_source(
                    c,
                    &source("https://example.org/akita-channel.xml", "Fabio Akita"),
                )?;
                let a2 = upsert_source(
                    c,
                    &source("https://akitaonrails.com/index.xml", "Fabio Akita"),
                )?;
                assert_eq!(a, a2);
                assert_ne!(a, b);
                list_sources(c)
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|r| r.creator.as_deref() == Some("Fabio Akita"))
        );
        let creators: i64 = db
            .read(|c| Ok(c.query_row("SELECT count(*) FROM creators", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(creators, 1);
    }

    #[tokio::test]
    async fn repolling_creates_nothing_new_and_dedups_by_platform_id() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        let (first, second) = db
            .write(|c| {
                let s = upsert_source(c, &source("https://e.org/feed", "X"))?;
                let batch = vec![
                    entry("https://e.org/a", None),
                    entry(
                        "https://www.youtube.com/watch?v=BZxHviFY_YY",
                        Some("BZxHviFY_YY"),
                    ),
                ];
                let first = insert_feed_entries(c, s, &batch)?;
                // Same video via another URL form, plus the same article again.
                let again = vec![
                    entry("https://e.org/a", None),
                    entry(
                        "https://youtube.com/watch?v=BZxHviFY_YY&x=1",
                        Some("BZxHviFY_YY"),
                    ),
                ];
                let second = insert_feed_entries(c, s, &again)?;
                Ok((first, second))
            })
            .await
            .unwrap();
        assert_eq!(first.len(), 2);
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn failures_accumulate_and_success_resets() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        let row = db
            .write(|c| {
                let s = upsert_source(c, &source("https://e.org/feed", "X"))?;
                assert_eq!(record_failure(c, s, "HTTP 503")?, 1);
                assert_eq!(record_failure(c, s, "HTTP 503")?, 2);
                record_success(c, s, 99, Some("Example"))?;
                get_source(c, s)
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!((row.consecutive_failures, row.next_poll_at), (0, 99));
        assert_eq!(row.last_error, None);
        assert_eq!(row.title.as_deref(), Some("Example"));
    }

    #[tokio::test]
    async fn validators_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        let v = db
            .write(|c| {
                let v = CachedValidators {
                    etag: Some("\"abc\"".into()),
                    last_modified: None,
                };
                store_http_validators(c, "https://e.org/feed", 200, &v)?;
                http_validators(c, "https://e.org/feed")
            })
            .await
            .unwrap();
        assert_eq!(v.etag.as_deref(), Some("\"abc\""));
    }
}
