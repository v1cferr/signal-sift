//! Items, transcripts, cues and segments.
//!
//! Functions take a `&Connection` (or `&mut` for multi-statement writes) and
//! are called inside [`crate::Db::read`] / [`crate::Db::write`], so callers
//! decide transaction boundaries and nothing here touches the async runtime.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{Result, StorageError, now_ms};

#[derive(Debug, Clone)]
pub struct NewItem {
    pub kind: &'static str,
    pub canonical_url: String,
    pub external_id: Option<String>,
    pub title: String,
    pub author: Option<String>,
    pub published_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub content_text: Option<String>,
    pub metadata_json: String,
}

/// Inserts or refreshes an item, keyed by canonical URL. Metadata changes
/// (title edits, a livestream that gained a duration) overwrite; discovery
/// time is kept from the first sighting.
pub fn upsert_item(conn: &Connection, item: &NewItem) -> Result<i64> {
    Ok(conn.query_row(
        "INSERT INTO items (kind, canonical_url, external_id, title, author, published_at,
                            discovered_at, duration_ms, content_text, metadata_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (canonical_url) DO UPDATE SET
            title = excluded.title,
            author = excluded.author,
            published_at = COALESCE(excluded.published_at, items.published_at),
            duration_ms = COALESCE(excluded.duration_ms, items.duration_ms),
            content_text = excluded.content_text,
            metadata_json = excluded.metadata_json
         RETURNING id",
        params![
            item.kind,
            item.canonical_url,
            item.external_id,
            item.title,
            item.author,
            item.published_at,
            now_ms(),
            item.duration_ms,
            item.content_text,
            item.metadata_json,
        ],
        |r| r.get(0),
    )?)
}

/// Inserts an item unless one with the same canonical URL or platform id
/// exists (any unique constraint). Returns the new id, or `None` for a
/// duplicate. Used by feed polling, where existing items must not be touched.
pub fn upsert_item_if_new(
    conn: &Connection,
    source_id: Option<i64>,
    item: &NewItem,
) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "INSERT INTO items (source_id, kind, canonical_url, external_id, title, author,
                                published_at, discovered_at, duration_ms, content_text, metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT DO NOTHING
             RETURNING id",
            params![
                source_id,
                item.kind,
                item.canonical_url,
                item.external_id,
                item.title,
                item.author,
                item.published_at,
                now_ms(),
                item.duration_ms,
                item.content_text,
                item.metadata_json,
            ],
            |r| r.get(0),
        )
        .optional()?)
}

/// Records explicit URLs found in an item. Re-running is a no-op.
pub fn add_references(
    conn: &Connection,
    item_id: i64,
    origin: &str,
    urls: &[(String, Option<i64>)],
) -> Result<usize> {
    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO item_references (item_id, url, at_ms, origin) VALUES (?1, ?2, ?3, ?4)",
    )?;
    let mut added = 0;
    for (url, at_ms) in urls {
        added += stmt.execute(params![item_id, url, at_ms, origin])?;
    }
    Ok(added)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscriptState {
    pub id: i64,
    pub origin: String,
    pub language: String,
    pub engine: String,
    pub status: String,
    pub covered_ms: i64,
    pub duration_ms: Option<i64>,
    pub cues: i64,
}

/// Opens a transcript for writing, resuming when possible.
///
/// * none exists: a new `in_progress` row;
/// * one exists from the same engine and is not complete: it is resumed from
///   its coverage (this is the crash-recovery path);
/// * one exists from another engine or failed: its cues are discarded and it
///   restarts, so a transcript never mixes two engines' output;
/// * a complete one is returned untouched.
pub fn open_transcript(
    conn: &mut Connection,
    item_id: i64,
    origin: &str,
    language: &str,
    engine: &str,
    duration_ms: Option<i64>,
) -> Result<TranscriptState> {
    let tx = conn.transaction()?;
    if let Some(existing) = find_transcript(&tx, item_id, origin, language)? {
        let resumable = existing.engine == engine && existing.status != "failed";
        if existing.status == "complete" || resumable {
            if existing.status != "complete" {
                tx.execute(
                    "UPDATE transcripts SET status = 'in_progress', updated_at = ?2 WHERE id = ?1",
                    params![existing.id, now_ms()],
                )?;
            }
            tx.commit()?;
            return find_transcript(conn, item_id, origin, language)?
                .ok_or_else(|| StorageError::Invalid("transcript vanished".into()));
        }
        tx.execute(
            "DELETE FROM transcript_cues WHERE transcript_id = ?1",
            [existing.id],
        )?;
        tx.execute(
            "UPDATE transcripts SET engine = ?2, status = 'in_progress', covered_ms = 0,
                    duration_ms = ?3, updated_at = ?4 WHERE id = ?1",
            params![existing.id, engine, duration_ms, now_ms()],
        )?;
    } else {
        let now = now_ms();
        tx.execute(
            "INSERT INTO transcripts (item_id, origin, language, engine, status, covered_ms,
                                      duration_ms, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'in_progress', 0, ?5, ?6, ?6)",
            params![item_id, origin, language, engine, duration_ms, now],
        )?;
    }
    tx.commit()?;
    find_transcript(conn, item_id, origin, language)?
        .ok_or_else(|| StorageError::Invalid("transcript vanished".into()))
}

pub fn find_transcript(
    conn: &Connection,
    item_id: i64,
    origin: &str,
    language: &str,
) -> Result<Option<TranscriptState>> {
    Ok(conn
        .query_row(
            "SELECT t.id, t.origin, t.language, t.engine, t.status, t.covered_ms, t.duration_ms,
                    (SELECT count(*) FROM transcript_cues c WHERE c.transcript_id = t.id)
             FROM transcripts t WHERE item_id = ?1 AND origin = ?2 AND language = ?3",
            params![item_id, origin, language],
            transcript_row,
        )
        .optional()?)
}

/// The transcript the API should show for an item: complete before partial,
/// then human subtitles before automatic captions before local ASR.
pub fn best_transcript(conn: &Connection, item_id: i64) -> Result<Option<TranscriptState>> {
    Ok(conn
        .query_row(
            "SELECT t.id, t.origin, t.language, t.engine, t.status, t.covered_ms, t.duration_ms,
                    (SELECT count(*) FROM transcript_cues c WHERE c.transcript_id = t.id)
             FROM transcripts t WHERE item_id = ?1 AND status != 'failed'
             ORDER BY (status = 'complete') DESC,
                      CASE origin WHEN 'manual_subtitles' THEN 0 WHEN 'auto_subtitles' THEN 1 ELSE 2 END,
                      covered_ms DESC
             LIMIT 1",
            [item_id],
            transcript_row,
        )
        .optional()?)
}

fn transcript_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TranscriptState> {
    Ok(TranscriptState {
        id: r.get(0)?,
        origin: r.get(1)?,
        language: r.get(2)?,
        engine: r.get(3)?,
        status: r.get(4)?,
        covered_ms: r.get(5)?,
        duration_ms: r.get(6)?,
        cues: r.get(7)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CueRow {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

/// Appends cues and advances coverage atomically. This is the checkpoint: if
/// the process dies, the transcript holds exactly the chunks committed so far.
pub fn append_cues(
    conn: &mut Connection,
    transcript_id: i64,
    cues: &[CueRow],
    covered_ms: i64,
) -> Result<()> {
    let tx = conn.transaction()?;
    let next_seq: i64 = tx.query_row(
        "SELECT IFNULL(MAX(seq) + 1, 0) FROM transcript_cues WHERE transcript_id = ?1",
        [transcript_id],
        |r| r.get(0),
    )?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO transcript_cues (transcript_id, seq, start_ms, end_ms, text)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (i, c) in cues.iter().enumerate() {
            stmt.execute(params![
                transcript_id,
                next_seq + i as i64,
                c.start_ms,
                c.end_ms,
                c.text
            ])?;
        }
    }
    tx.execute(
        "UPDATE transcripts SET covered_ms = MAX(covered_ms, ?2), updated_at = ?3 WHERE id = ?1",
        params![transcript_id, covered_ms, now_ms()],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn last_cue(conn: &Connection, transcript_id: i64) -> Result<Option<CueRow>> {
    Ok(conn
        .query_row(
            "SELECT start_ms, end_ms, text FROM transcript_cues
             WHERE transcript_id = ?1 ORDER BY seq DESC LIMIT 1",
            [transcript_id],
            |r| {
                Ok(CueRow {
                    start_ms: r.get(0)?,
                    end_ms: r.get(1)?,
                    text: r.get(2)?,
                })
            },
        )
        .optional()?)
}

pub fn cues(conn: &Connection, transcript_id: i64) -> Result<Vec<CueRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT start_ms, end_ms, text FROM transcript_cues WHERE transcript_id = ?1 ORDER BY seq",
    )?;
    let rows = stmt.query_map([transcript_id], |r| {
        Ok(CueRow {
            start_ms: r.get(0)?,
            end_ms: r.get(1)?,
            text: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Sets the final status. A transcript whose coverage falls short of the
/// media duration can never be marked complete, whatever the caller asks.
pub fn finish_transcript(conn: &Connection, transcript_id: i64, requested: &str) -> Result<String> {
    let (covered, duration): (i64, Option<i64>) = conn.query_row(
        "SELECT covered_ms, duration_ms FROM transcripts WHERE id = ?1",
        [transcript_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let status = match (requested, duration) {
        ("complete", Some(d)) if covered < d => "partial",
        (s, _) => s,
    };
    conn.execute(
        "UPDATE transcripts SET status = ?2, updated_at = ?3 WHERE id = ?1",
        params![transcript_id, status, now_ms()],
    )?;
    Ok(status.to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SegmentRow {
    pub id: i64,
    pub seq: i64,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub title: Option<String>,
    pub text: String,
}

pub struct NewSegment {
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub title: Option<String>,
    pub text: String,
}

/// Replaces an item's segments for one transcript. Analyses of the old
/// segments cascade away: they described text that no longer exists.
pub fn replace_segments(
    conn: &mut Connection,
    item_id: i64,
    transcript_id: Option<i64>,
    segments: &[NewSegment],
) -> Result<usize> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM segments WHERE item_id = ?1 AND transcript_id IS ?2",
        params![item_id, transcript_id],
    )?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO segments (item_id, transcript_id, seq, start_ms, end_ms, title, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for (i, s) in segments.iter().enumerate() {
            stmt.execute(params![
                item_id,
                transcript_id,
                i as i64,
                s.start_ms,
                s.end_ms,
                s.title,
                s.text
            ])?;
        }
    }
    tx.commit()?;
    Ok(segments.len())
}

pub fn segments(conn: &Connection, item_id: i64) -> Result<Vec<SegmentRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, seq, start_ms, end_ms, title, text FROM segments
         WHERE item_id = ?1 ORDER BY start_ms, seq",
    )?;
    let rows = stmt.query_map([item_id], |r| {
        Ok(SegmentRow {
            id: r.get(0)?,
            seq: r.get(1)?,
            start_ms: r.get(2)?,
            end_ms: r.get(3)?,
            title: r.get(4)?,
            text: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;

    fn item(url: &str) -> NewItem {
        NewItem {
            kind: "video",
            canonical_url: url.into(),
            external_id: Some("N3eU-2VIByQ".into()),
            title: "FABIO AKITA - Flow #663".into(),
            author: Some("Flow Podcast".into()),
            published_at: None,
            duration_ms: Some(15_447_000),
            content_text: None,
            metadata_json: "{}".into(),
        }
    }

    fn cue(start: i64, end: i64, text: &str) -> CueRow {
        CueRow {
            start_ms: start,
            end_ms: end,
            text: text.into(),
        }
    }

    #[tokio::test]
    async fn upsert_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        let (a, b) = db
            .write(|c| {
                let a = upsert_item(c, &item("https://www.youtube.com/watch?v=N3eU-2VIByQ"))?;
                let b = upsert_item(c, &item("https://www.youtube.com/watch?v=N3eU-2VIByQ"))?;
                Ok((a, b))
            })
            .await
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(db.summary().await.unwrap().items, 1);
    }

    #[tokio::test]
    async fn interrupted_transcripts_resume_and_never_claim_completeness() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        let (status, resumed) = db
            .write(|c| {
                let id = upsert_item(c, &item("u"))?;
                let t = open_transcript(c, id, "asr", "pt", "whisper", Some(1_200_000))?;
                append_cues(c, t.id, &[cue(0, 1000, "a"), cue(1000, 2000, "b")], 585_000)?;
                // Simulated crash: reopen with the same engine.
                let resumed = open_transcript(c, id, "asr", "pt", "whisper", Some(1_200_000))?;
                let status = finish_transcript(c, t.id, "complete")?;
                Ok((status, resumed))
            })
            .await
            .unwrap();
        assert_eq!(resumed.covered_ms, 585_000);
        assert_eq!(resumed.cues, 2);
        assert_eq!(
            status, "partial",
            "coverage below duration must not be complete"
        );
    }

    #[tokio::test]
    async fn switching_engines_discards_the_old_partial_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        let t = db
            .write(|c| {
                let id = upsert_item(c, &item("u"))?;
                let t = open_transcript(c, id, "asr", "pt", "whisper base", Some(10_000))?;
                append_cues(c, t.id, &[cue(0, 1000, "a")], 5_000)?;
                open_transcript(c, id, "asr", "pt", "whisper turbo", Some(10_000))
            })
            .await
            .unwrap();
        assert_eq!(
            (t.cues, t.covered_ms, t.engine.as_str()),
            (0, 0, "whisper turbo")
        );
    }

    #[tokio::test]
    async fn cue_sequence_continues_across_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        let all = db
            .write(|c| {
                let id = upsert_item(c, &item("u"))?;
                let t = open_transcript(c, id, "asr", "pt", "w", Some(4000))?;
                append_cues(c, t.id, &[cue(0, 1000, "a")], 2000)?;
                append_cues(c, t.id, &[cue(2000, 3000, "b")], 4000)?;
                assert_eq!(finish_transcript(c, t.id, "complete")?, "complete");
                cues(c, t.id)
            })
            .await
            .unwrap();
        assert_eq!(
            all.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
    }
}
