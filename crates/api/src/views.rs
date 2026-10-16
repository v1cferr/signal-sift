//! Read models: the JSON shapes of API v1, built straight from SQLite.
//!
//! The CLI uses these same functions (`signalsift media show`), so there is
//! one contract for every client. Fields are additive-only within v1: clients
//! must ignore unknown fields, and nothing here is renamed or removed without
//! a new API version.

use serde::Serialize;
use signalsift_core::timestamp::{format_hms, rfc3339, youtube_link};
use signalsift_storage::Result;
use signalsift_storage::analyses;
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
    /// Estimated relevance to the reader's interests (0-100), the maximum
    /// over the item's segments. Absent until analysed. A preference
    /// estimate, not a measure of accuracy or truth.
    pub relevance: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ItemDetail {
    #[serde(flatten)]
    pub summary: ItemSummary,
    pub discovered_at: String,
    pub description: Option<String>,
    pub segment_count: i64,
    pub references: Vec<ReferenceView>,
    pub analysis: Option<AnalysisView>,
}

/// LLM output for an item or a segment. Generated text: useful, fallible,
/// never a statement of fact.
#[derive(Debug, Serialize)]
pub struct AnalysisView {
    pub summary: String,
    pub topics: Vec<String>,
    pub matched_interests: Vec<String>,
    pub relevance: i64,
    pub rationale: String,
    pub applications: Vec<String>,
    /// Tools, projects and documents explicitly named in the content.
    pub mentions: Vec<String>,
    pub model: String,
    pub prompt_version: String,
    pub analysed_at: String,
}

impl From<&analyses::AnalysisRow> for AnalysisView {
    fn from(r: &analyses::AnalysisRow) -> Self {
        let list = |s: &str| serde_json::from_str::<Vec<String>>(s).unwrap_or_default();
        Self {
            summary: r.summary.clone(),
            topics: list(&r.topics_json),
            matched_interests: list(&r.matched_interests_json),
            relevance: r.relevance,
            rationale: r.rationale.clone(),
            applications: list(&r.applications_json),
            mentions: list(&r.mentions_json),
            model: r.model.clone(),
            prompt_version: r.prompt_version.clone(),
            analysed_at: rfc3339(r.created_at),
        }
    }
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
    pub analysis: Option<AnalysisView>,
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
        relevance: analyses::item_analysis(conn, row.id)?.map(|a| a.relevance),
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
    // Same selection as the segments endpoint (best transcript only).
    let segment_count = media::segments(conn, id)?.len() as i64;
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
        analysis: analyses::item_analysis(conn, id)?.as_ref().map(Into::into),
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
    let analysed = analyses::segment_analyses(conn, id)?;
    let segments = media::segments(conn, id)?
        .into_iter()
        .map(|s| SegmentView {
            analysis: analysed
                .iter()
                .find(|a| a.segment_id == Some(s.id))
                .map(Into::into),
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
pub struct Feed {
    pub generated_at: String,
    pub highlights: Vec<Highlight>,
}

/// One recommendation. The `moment` points at the segment that earned the
/// score, so five relevant minutes in a long video lead straight to them.
#[derive(Debug, Serialize)]
pub struct Highlight {
    pub item_id: i64,
    pub title: String,
    pub source: Option<String>,
    pub kind: String,
    pub url: String,
    pub published_at: Option<String>,
    pub topic: Option<String>,
    pub topics: Vec<String>,
    pub summary: String,
    /// Why it was picked: the rationale of the best-scoring segment.
    pub why: String,
    pub relevance: i64,
    pub moment: Option<Moment>,
    pub processing: Processing,
}

#[derive(Debug, Serialize)]
pub struct Moment {
    pub segment_id: i64,
    pub start: Option<String>,
    pub start_ms: Option<i64>,
    pub link: Option<String>,
    pub summary: String,
}

#[derive(Debug, Serialize)]
pub struct Processing {
    pub status: String,
    pub partial: bool,
    pub coverage: Option<f64>,
}

/// Ranked highlights from items analysed within the last `days` days
/// (by publication, or discovery when unknown). One highlight per item,
/// highest relevance first, newest first among equals. Items the reader
/// marked `not_interested` or `already_known` are left out of the feed (they
/// remain available through /v1/items: nothing is deleted).
pub fn feed(conn: &Connection, limit: u32, days: u32, now_ms: i64) -> Result<Feed> {
    let limit = limit.clamp(1, 50);
    let since = now_ms - i64::from(days.clamp(1, 365)) * 86_400_000;
    let mut stmt = conn.prepare_cached(
        "SELECT a.item_id, a.relevance, i.kind, i.title, i.author, i.canonical_url, i.external_id,
                i.published_at, a.summary, a.topics_json, a.rationale, a.best_segment_id
         FROM analyses a JOIN items i ON i.id = a.item_id
         WHERE a.segment_id IS NULL
           AND a.created_at = (SELECT MAX(created_at) FROM analyses b
                               WHERE b.item_id = a.item_id AND b.segment_id IS NULL)
           AND IFNULL(i.published_at, i.discovered_at) >= ?1
           AND NOT EXISTS (SELECT 1 FROM feedback f WHERE f.item_id = i.id
                             AND f.verdict IN ('not_interested', 'already_known')
                             AND f.created_at = (SELECT MAX(created_at) FROM feedback g WHERE g.item_id = i.id))
         ORDER BY a.relevance DESC, IFNULL(i.published_at, i.discovered_at) DESC
         LIMIT ?2",
    )?;
    struct Row {
        item_id: i64,
        relevance: i64,
        kind: String,
        title: String,
        author: Option<String>,
        url: String,
        external_id: Option<String>,
        published_at: Option<i64>,
        summary: String,
        topics_json: String,
        rationale: String,
        best_segment_id: Option<i64>,
    }
    let rows: Vec<Row> = stmt
        .query_map(params![since, limit], |r| {
            Ok(Row {
                item_id: r.get(0)?,
                relevance: r.get(1)?,
                kind: r.get(2)?,
                title: r.get(3)?,
                author: r.get(4)?,
                url: r.get(5)?,
                external_id: r.get(6)?,
                published_at: r.get(7)?,
                summary: r.get(8)?,
                topics_json: r.get(9)?,
                rationale: r.get(10)?,
                best_segment_id: r.get(11)?,
            })
        })?
        .collect::<signalsift_storage::rusqlite::Result<_>>()?;

    let mut highlights = Vec::with_capacity(rows.len());
    for row in rows {
        let topics: Vec<String> = serde_json::from_str(&row.topics_json).unwrap_or_default();
        let moment = match row.best_segment_id {
            Some(sid) => conn
                .query_row(
                    "SELECT s.start_ms, a.summary FROM segments s
                     LEFT JOIN analyses a ON a.segment_id = s.id
                     WHERE s.id = ?1 ORDER BY a.created_at DESC LIMIT 1",
                    [sid],
                    |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<String>>(1)?)),
                )
                .optional()?
                .map(|(start_ms, summary)| Moment {
                    segment_id: sid,
                    start: start_ms.map(|ms| format_hms(ms.max(0) as u64)),
                    link: match (row.kind.as_str(), &row.external_id, start_ms) {
                        ("video", Some(v), Some(ms)) => {
                            youtube_link(v, ms.max(0) as u64).map(|u| u.to_string())
                        }
                        _ => None,
                    },
                    start_ms,
                    summary: summary.unwrap_or_default(),
                }),
            None => None,
        };
        let processing = match media::best_transcript(conn, row.item_id)? {
            Some(t) => {
                let v = TranscriptView::from(t);
                Processing {
                    status: v.status,
                    partial: v.partial,
                    coverage: v.coverage,
                }
            }
            // Articles have no transcript: their text is complete as fetched.
            None => Processing {
                status: "complete".into(),
                partial: false,
                coverage: None,
            },
        };
        highlights.push(Highlight {
            item_id: row.item_id,
            title: row.title,
            source: row.author,
            kind: row.kind,
            url: row.url,
            published_at: row.published_at.map(rfc3339),
            topic: topics.first().cloned(),
            topics,
            summary: row.summary,
            why: row.rationale,
            relevance: row.relevance,
            moment,
            processing,
        });
    }
    Ok(Feed {
        generated_at: rfc3339(now_ms),
        highlights,
    })
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
    async fn feed_points_at_the_best_segment_and_respects_feedback() {
        use signalsift_storage::analyses::{NewAnalysis, upsert};
        let (db, _dir, id) = seeded().await;
        let row = move |seg: Option<i64>, relevance: i64, best: Option<i64>| NewAnalysis {
            item_id: id,
            segment_id: seg,
            model: "m".into(),
            prompt_version: "v1".into(),
            profile_fingerprint: "f".into(),
            summary: if seg.is_some() {
                "OWASP AI cheat sheet".into()
            } else {
                "AI and security".into()
            },
            topics_json: r#"["security","LLM"]"#.into(),
            matched_interests_json: "[]".into(),
            relevance,
            rationale: "covers prompt injection".into(),
            applications_json: "[]".into(),
            mentions_json: r#"["OWASP"]"#.into(),
            best_segment_id: best,
            stats_json: "{}".into(),
        };
        let now = 1_792_022_400_000 + 86_400_000;
        let feed_now = db
            .write(move |c| {
                let sid: i64 =
                    c.query_row("SELECT id FROM segments WHERE item_id = ?1", [id], |r| {
                        r.get(0)
                    })?;
                upsert(c, &row(Some(sid), 88, None))?;
                upsert(c, &row(None, 88, Some(sid)))?;
                feed(c, 5, 14, now)
            })
            .await
            .unwrap();
        let h = &feed_now.highlights[0];
        assert_eq!((h.relevance, h.topic.as_deref()), (88, Some("security")));
        let m = h.moment.as_ref().unwrap();
        assert_eq!(
            m.link.as_deref(),
            Some("https://www.youtube.com/watch?v=BZxHviFY_YY&t=754s")
        );
        assert!(
            h.processing.partial,
            "the seeded transcript is partial and must say so"
        );

        let after = db
            .write(move |c| {
                c.execute(
                    "INSERT INTO feedback (item_id, verdict, created_at) VALUES (?1, 'not_interested', 1)",
                    [id],
                )?;
                feed(c, 5, 14, now)
            })
            .await
            .unwrap();
        assert!(after.highlights.is_empty());
        // Hidden from the feed, not deleted.
        assert!(
            db.read(move |c| item_detail(c, id))
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn missing_items_are_none_not_empty() {
        let (db, _dir, _) = seeded().await;
        assert!(db.read(|c| item_segments(c, 999)).await.unwrap().is_none());
    }
}
