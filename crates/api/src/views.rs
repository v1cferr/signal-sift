//! Read models: the JSON shapes of API v1, built straight from SQLite.
//!
//! The CLI uses these same functions (`signalsift media show`), so there is
//! one contract for every client. Fields are additive-only within v1: clients
//! must ignore unknown fields, and nothing here is renamed or removed without
//! a new API version.

use serde::Serialize;
use signalsift_core::timestamp::{format_hms, rfc3339, youtube_link};
use signalsift_storage::Result;
use signalsift_storage::media::{self, TranscriptState};
use signalsift_storage::rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Serialize)]
pub struct ItemSummary {
    pub id: i64,
    pub kind: String,
    pub title: String,
    pub url: String,
    /// Platform id (YouTube video id, feed entry id), when there is one.
    pub external_id: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<String>,
    pub duration_ms: Option<i64>,
    pub transcript: Option<TranscriptView>,
}

#[derive(Debug, Serialize)]
pub struct ItemDetail {
    #[serde(flatten)]
    pub summary: ItemSummary,
    pub discovered_at: String,
    pub description: Option<String>,
    pub segment_count: i64,
    pub references: Vec<ReferenceView>,
}

#[derive(Debug, Serialize)]
pub struct TranscriptView {
    /// `manual_subtitles`, `auto_subtitles` or `asr`.
    pub origin: String,
    pub language: String,
    pub engine: String,
    /// `in_progress`, `partial`, `complete` or `failed`.
    pub status: String,
    /// True unless the transcript is complete. Clients must surface this:
    /// a partial transcript says nothing about the part not yet covered.
    pub partial: bool,
    pub covered_ms: i64,
    /// Share of the media duration covered, 0.0 to 1.0, when the duration is known.
    pub coverage: Option<f64>,
    pub cues: i64,
}

#[derive(Debug, Serialize)]
pub struct ReferenceView {
    pub url: String,
    pub origin: String,
    pub at_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct SegmentView {
    pub id: i64,
    pub seq: i64,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    /// `H:MM:SS`, for display.
    pub start: Option<String>,
    pub title: Option<String>,
    pub text: String,
    /// Deep link to the moment the segment starts, when the platform has one.
    pub link: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Pass as `cursor` to fetch the next page; absent on the last page.
    pub next_cursor: Option<String>,
}

impl From<TranscriptState> for TranscriptView {
    fn from(t: TranscriptState) -> Self {
        let coverage = t
            .duration_ms
            .filter(|d| *d > 0)
            .map(|d| (t.covered_ms as f64 / d as f64).min(1.0));
        Self {
            partial: t.status != "complete",
            origin: t.origin,
            language: t.language,
            engine: t.engine,
            status: t.status,
            covered_ms: t.covered_ms,
            coverage,
            cues: t.cues,
        }
    }
}

struct ItemRow {
    id: i64,
    kind: String,
    title: String,
    url: String,
    author: Option<String>,
    published_at: Option<i64>,
    duration_ms: Option<i64>,
    discovered_at: i64,
    description: Option<String>,
    external_id: Option<String>,
}

const ITEM_COLUMNS: &str = "id, kind, title, canonical_url, author, published_at, duration_ms,
                            discovered_at, content_text, external_id";

fn item_row(
    r: &signalsift_storage::rusqlite::Row<'_>,
) -> signalsift_storage::rusqlite::Result<ItemRow> {
    Ok(ItemRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        title: r.get(2)?,
        url: r.get(3)?,
        author: r.get(4)?,
        published_at: r.get(5)?,
        duration_ms: r.get(6)?,
        discovered_at: r.get(7)?,
        description: r.get(8)?,
        external_id: r.get(9)?,
    })
}

fn summary(conn: &Connection, row: &ItemRow) -> Result<ItemSummary> {
    Ok(ItemSummary {
        id: row.id,
        kind: row.kind.clone(),
        title: row.title.clone(),
        url: row.url.clone(),
        external_id: row.external_id.clone(),
        author: row.author.clone(),
        published_at: row.published_at.map(rfc3339),
        duration_ms: row.duration_ms,
        transcript: media::best_transcript(conn, row.id)?.map(Into::into),
    })
}

/// Newest first. `cursor` is the opaque value from the previous page.
pub fn list_items(conn: &Connection, limit: u32, cursor: Option<i64>) -> Result<Page<ItemSummary>> {
    let limit = limit.clamp(1, 100);
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {ITEM_COLUMNS} FROM items WHERE id < ?1 ORDER BY id DESC LIMIT ?2"
    ))?;
    let rows: Vec<ItemRow> = stmt
        .query_map(params![cursor.unwrap_or(i64::MAX), limit + 1], item_row)?
        .collect::<signalsift_storage::rusqlite::Result<_>>()?;
    let has_more = rows.len() > limit as usize;
    let rows = &rows[..rows.len().min(limit as usize)];
    let items = rows
        .iter()
        .map(|r| summary(conn, r))
        .collect::<Result<Vec<_>>>()?;
    Ok(Page {
        next_cursor: has_more
            .then(|| rows.last().map(|r| r.id.to_string()))
            .flatten(),
        items,
    })
}

pub fn item_detail(conn: &Connection, id: i64) -> Result<Option<ItemDetail>> {
    let Some(row) = conn
        .query_row(
            &format!("SELECT {ITEM_COLUMNS} FROM items WHERE id = ?1"),
            [id],
            item_row,
        )
        .optional()?
    else {
        return Ok(None);
    };
    let segment_count = conn.query_row(
        "SELECT count(*) FROM segments WHERE item_id = ?1",
        [id],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare_cached(
        "SELECT url, origin, at_ms FROM item_references WHERE item_id = ?1 ORDER BY IFNULL(at_ms, -1), id",
    )?;
    let references = stmt
        .query_map([id], |r| {
            Ok(ReferenceView {
                url: r.get(0)?,
                origin: r.get(1)?,
                at_ms: r.get(2)?,
            })
        })?
        .collect::<signalsift_storage::rusqlite::Result<_>>()?;
    Ok(Some(ItemDetail {
        summary: summary(conn, &row)?,
        discovered_at: rfc3339(row.discovered_at),
        description: row.description,
        segment_count,
        references,
    }))
}

/// `None` when the item does not exist (as opposed to an item with no segments).
pub fn item_segments(conn: &Connection, id: i64) -> Result<Option<Vec<SegmentView>>> {
    let Some((kind, external_id)) = conn
        .query_row(
            "SELECT kind, external_id FROM items WHERE id = ?1",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let video_id = (kind == "video").then_some(external_id).flatten();
    let segments = media::segments(conn, id)?
        .into_iter()
        .map(|s| SegmentView {
            link: match (&video_id, s.start_ms) {
                (Some(v), Some(ms)) => youtube_link(v, ms.max(0) as u64).map(|u| u.to_string()),
                _ => None,
            },
            start: s.start_ms.map(|ms| format_hms(ms.max(0) as u64)),
            id: s.id,
            seq: s.seq,
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            title: s.title,
            text: s.text,
        })
        .collect();
    Ok(Some(segments))
}

#[derive(Debug, Serialize)]
pub struct SourceView {
    pub id: i64,
    /// `feed`, `youtube_channel`, `podcast` or `newsletter`.
    pub kind: String,
    pub url: String,
    pub title: Option<String>,
    pub creator: Option<String>,
    pub enabled: bool,
    pub poll_interval_secs: i64,
    pub next_poll_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
    pub consecutive_failures: i64,
    pub items: i64,
}

pub fn list_sources(conn: &Connection) -> Result<Vec<SourceView>> {
    Ok(signalsift_storage::sources::list_sources(conn)?
        .into_iter()
        .map(|s| SourceView {
            id: s.id,
            kind: s.kind,
            url: s.url,
            title: s.title,
            creator: s.creator,
            enabled: s.enabled,
            poll_interval_secs: s.poll_interval_secs,
            // 0 means "due immediately"; show it as absent rather than 1970.
            next_poll_at: (s.next_poll_at > 0).then(|| rfc3339(s.next_poll_at)),
            last_success_at: s.last_success_at.map(rfc3339),
            last_error: s.last_error,
            consecutive_failures: s.consecutive_failures,
            items: s.items,
        })
        .collect())
}

/// Resolves an item by numeric id or by platform id (e.g. a YouTube video id).
pub fn find_item_id(conn: &Connection, key: &str) -> Result<Option<i64>> {
    if let Ok(id) = key.parse::<i64>() {
        return Ok(Some(id));
    }
    Ok(conn
        .query_row(
            "SELECT id FROM items WHERE external_id = ?1 LIMIT 1",
            [key],
            |r| r.get(0),
        )
        .optional()?)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use signalsift_storage::Db;
    use signalsift_storage::media::{
        CueRow, NewItem, NewSegment, append_cues, open_transcript, replace_segments, upsert_item,
    };

    pub(crate) async fn seeded() -> (Db, tempfile::TempDir, i64) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        let id = db
            .write(|c| {
                let id = upsert_item(
                    c,
                    &NewItem {
                        kind: "video",
                        canonical_url: "https://www.youtube.com/watch?v=BZxHviFY_YY".into(),
                        external_id: Some("BZxHviFY_YY".into()),
                        title: "Como a IA Pode Melhorar a Segurança do Seu Software".into(),
                        author: Some("Augusto Galego".into()),
                        published_at: Some(1_792_022_400_000),
                        duration_ms: Some(821_000),
                        content_text: Some("desc".into()),
                        metadata_json: "{}".into(),
                    },
                )?;
                let t = open_transcript(c, id, "auto_subtitles", "pt", "yt-dlp", Some(821_000))?;
                append_cues(
                    c,
                    t.id,
                    &[CueRow {
                        start_ms: 80,
                        end_ms: 1280,
                        text: "Deve não ter horário".into(),
                    }],
                    400_000,
                )?;
                replace_segments(
                    c,
                    id,
                    Some(t.id),
                    &[NewSegment {
                        start_ms: Some(754_999),
                        end_ms: Some(800_000),
                        title: None,
                        text: "OWASP".into(),
                    }],
                )?;
                Ok(id)
            })
            .await
            .unwrap();
        (db, dir, id)
    }

    #[tokio::test]
    async fn partial_transcripts_are_flagged_with_coverage() {
        let (db, _dir, id) = seeded().await;
        let detail = db.read(move |c| item_detail(c, id)).await.unwrap().unwrap();
        let t = detail.summary.transcript.unwrap();
        assert!(t.partial);
        assert!((t.coverage.unwrap() - 400.0 / 821.0).abs() < 1e-9);
        assert_eq!(
            detail.summary.published_at.as_deref(),
            Some("2026-10-15T00:00:00Z")
        );
    }

    #[tokio::test]
    async fn segments_link_to_the_second_they_start() {
        let (db, _dir, id) = seeded().await;
        let segs = db
            .read(move |c| item_segments(c, id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            segs[0].link.as_deref(),
            Some("https://www.youtube.com/watch?v=BZxHviFY_YY&t=754s")
        );
        assert_eq!(segs[0].start.as_deref(), Some("12:34"));
    }

    #[tokio::test]
    async fn missing_items_are_none_not_empty() {
        let (db, _dir, _) = seeded().await;
        assert!(db.read(|c| item_segments(c, 999)).await.unwrap().is_none());
    }
}
