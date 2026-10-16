//! LLM analyses of items and segments.
//!
//! One row per (item, segment or NULL for the item, model, prompt version).
//! Re-running the same analysis replaces the row, so jobs are idempotent and
//! a changed interest profile simply overwrites scores (the fingerprint
//! column says which profile produced them).

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{Result, now_ms};

#[derive(Debug, Clone)]
pub struct NewAnalysis {
    pub item_id: i64,
    pub segment_id: Option<i64>,
    pub model: String,
    pub prompt_version: String,
    pub profile_fingerprint: String,
    pub summary: String,
    pub topics_json: String,
    pub matched_interests_json: String,
    pub relevance: i64,
    pub rationale: String,
    pub applications_json: String,
    pub mentions_json: String,
    pub best_segment_id: Option<i64>,
    pub stats_json: String,
}

pub fn upsert(conn: &Connection, a: &NewAnalysis) -> Result<i64> {
    Ok(conn.query_row(
        "INSERT INTO analyses (item_id, segment_id, model, prompt_version, profile_fingerprint, summary,
                               topics_json, matched_interests_json, relevance, rationale,
                               applications_json, mentions_json, best_segment_id, stats_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT (item_id, IFNULL(segment_id, 0), model, prompt_version) DO UPDATE SET
            profile_fingerprint = excluded.profile_fingerprint,
            summary = excluded.summary,
            topics_json = excluded.topics_json,
            matched_interests_json = excluded.matched_interests_json,
            relevance = excluded.relevance,
            rationale = excluded.rationale,
            applications_json = excluded.applications_json,
            mentions_json = excluded.mentions_json,
            best_segment_id = excluded.best_segment_id,
            stats_json = excluded.stats_json,
            created_at = excluded.created_at
         RETURNING id",
        params![
            a.item_id,
            a.segment_id,
            a.model,
            a.prompt_version,
            a.profile_fingerprint,
            a.summary,
            a.topics_json,
            a.matched_interests_json,
            a.relevance.clamp(0, 100),
            a.rationale,
            a.applications_json,
            a.mentions_json,
            a.best_segment_id,
            a.stats_json,
            now_ms(),
        ],
        |r| r.get(0),
    )?)
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AnalysisRow {
    pub id: i64,
    pub segment_id: Option<i64>,
    pub model: String,
    pub prompt_version: String,
    pub profile_fingerprint: String,
    pub summary: String,
    pub topics_json: String,
    pub matched_interests_json: String,
    pub relevance: i64,
    pub rationale: String,
    pub applications_json: String,
    pub mentions_json: String,
    pub best_segment_id: Option<i64>,
    pub created_at: i64,
}

const COLUMNS: &str = "id, segment_id, model, prompt_version, profile_fingerprint, summary, topics_json,
    matched_interests_json, relevance, rationale, applications_json, mentions_json, best_segment_id, created_at";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<AnalysisRow> {
    Ok(AnalysisRow {
        id: r.get(0)?,
        segment_id: r.get(1)?,
        model: r.get(2)?,
        prompt_version: r.get(3)?,
        profile_fingerprint: r.get(4)?,
        summary: r.get(5)?,
        topics_json: r.get(6)?,
        matched_interests_json: r.get(7)?,
        relevance: r.get(8)?,
        rationale: r.get(9)?,
        applications_json: r.get(10)?,
        mentions_json: r.get(11)?,
        best_segment_id: r.get(12)?,
        created_at: r.get(13)?,
    })
}

/// The newest item-level analysis.
pub fn item_analysis(conn: &Connection, item_id: i64) -> Result<Option<AnalysisRow>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM analyses WHERE item_id = ?1 AND segment_id IS NULL
                 ORDER BY created_at DESC LIMIT 1"
            ),
            [item_id],
            row,
        )
        .optional()?)
}

/// The newest analysis of each segment of an item.
pub fn segment_analyses(conn: &Connection, item_id: i64) -> Result<Vec<AnalysisRow>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM analyses a WHERE item_id = ?1 AND segment_id IS NOT NULL
           AND created_at = (SELECT MAX(created_at) FROM analyses b
                             WHERE b.item_id = a.item_id AND b.segment_id = a.segment_id)"
    ))?;
    let rows = stmt.query_map([item_id], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Segments of `item_id` already analysed with this model, prompt version and
/// profile: what a resumed analysis job can skip.
pub fn analysed_segments(
    conn: &Connection,
    item_id: i64,
    model: &str,
    prompt_version: &str,
    fingerprint: &str,
) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT segment_id FROM analyses WHERE item_id = ?1 AND segment_id IS NOT NULL
           AND model = ?2 AND prompt_version = ?3 AND profile_fingerprint = ?4",
    )?;
    let rows = stmt.query_map(params![item_id, model, prompt_version, fingerprint], |r| {
        r.get(0)
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;
    use crate::media::{NewItem, NewSegment, replace_segments, upsert_item};

    fn analysis(item: i64, seg: Option<i64>, relevance: i64) -> NewAnalysis {
        NewAnalysis {
            item_id: item,
            segment_id: seg,
            model: "m".into(),
            prompt_version: "v1".into(),
            profile_fingerprint: "f".into(),
            summary: "s".into(),
            topics_json: "[]".into(),
            matched_interests_json: "[]".into(),
            relevance,
            rationale: "r".into(),
            applications_json: "[]".into(),
            mentions_json: "[\"OWASP\"]".into(),
            best_segment_id: None,
            stats_json: "{}".into(),
        }
    }

    #[tokio::test]
    async fn reanalysis_replaces_and_item_rows_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        let (item, segs, done, count) = db
            .write(|c| {
                let item = upsert_item(
                    c,
                    &NewItem {
                        kind: "article",
                        canonical_url: "https://e.org/a".into(),
                        external_id: None,
                        title: "t".into(),
                        author: None,
                        published_at: None,
                        duration_ms: None,
                        content_text: None,
                        metadata_json: "{}".into(),
                    },
                )?;
                let seg = NewSegment {
                    start_ms: None,
                    end_ms: None,
                    title: None,
                    text: "x".into(),
                };
                replace_segments(c, item, None, &[seg])?;
                let sid: i64 = c.query_row("SELECT id FROM segments", [], |r| r.get(0))?;
                upsert(c, &analysis(item, Some(sid), 30))?;
                upsert(c, &analysis(item, Some(sid), 80))?; // re-run
                upsert(c, &analysis(item, None, 80))?;
                upsert(c, &analysis(item, None, 80))?; // NULL segment must not duplicate
                let done = analysed_segments(c, item, "m", "v1", "f")?;
                let count: i64 = c.query_row("SELECT count(*) FROM analyses", [], |r| r.get(0))?;
                Ok((item, segment_analyses(c, item)?, done, count))
            })
            .await
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].relevance, 80);
        assert_eq!(done.len(), 1);
        let item_row = db
            .read(move |c| item_analysis(c, item))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(item_row.mentions_json, "[\"OWASP\"]");
    }
}
