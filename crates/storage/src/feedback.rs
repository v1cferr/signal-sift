//! User feedback on items and segments: the signal future relevance tuning
//! learns from. Stored verbatim; nothing is deleted or hidden because of it.

use rusqlite::{Connection, OptionalExtension, params};

use crate::{Result, StorageError, now_ms};

pub const VERDICTS: &[&str] = &[
    "interested",
    "not_interested",
    "already_known",
    "wrong_topic",
];

pub struct NewFeedback {
    pub item_id: i64,
    pub segment_id: Option<i64>,
    pub verdict: String,
    pub note: Option<String>,
    pub client: Option<String>,
}

/// Inserts feedback after checking that the item exists and that the segment,
/// if given, belongs to it. Returns `Ok(None)` for an unknown item/segment.
pub fn insert(conn: &Connection, f: &NewFeedback) -> Result<Option<i64>> {
    if !VERDICTS.contains(&f.verdict.as_str()) {
        return Err(StorageError::Invalid(format!(
            "unknown verdict {:?}",
            f.verdict
        )));
    }
    let item_exists = conn
        .query_row("SELECT 1 FROM items WHERE id = ?1", [f.item_id], |_| Ok(()))
        .optional()?
        .is_some();
    let segment_ok = match f.segment_id {
        None => true,
        Some(s) => conn
            .query_row(
                "SELECT 1 FROM segments WHERE id = ?1 AND item_id = ?2",
                params![s, f.item_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some(),
    };
    if !item_exists || !segment_ok {
        return Ok(None);
    }
    Ok(Some(conn.query_row(
        "INSERT INTO feedback (item_id, segment_id, verdict, note, client, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) RETURNING id",
        params![
            f.item_id,
            f.segment_id,
            f.verdict,
            f.note,
            f.client,
            now_ms()
        ],
        |r| r.get(0),
    )?))
}
