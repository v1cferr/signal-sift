//! Persistent job queue.
//!
//! Jobs survive restarts because they are rows. Enqueueing is idempotent
//! through `dedup_key` (the same work requested twice is one job). A worker
//! claims a job atomically; a job found `running` at startup was interrupted
//! by a crash or shutdown and goes back to `queued`, which is safe because
//! every job kind is written to be idempotent and resumable.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{Result, now_ms};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkClass {
    Light,
    Heavy,
}

impl WorkClass {
    fn as_str(self) -> &'static str {
        match self {
            WorkClass::Light => "light",
            WorkClass::Heavy => "heavy",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    pub dedup_key: String,
    pub work_class: String,
    pub payload_json: String,
    pub attempts: i64,
    pub max_attempts: i64,
}

/// Queues a job unless one with the same key exists in any state. Returns
/// the new id, or `None` when it was already known.
pub fn enqueue(
    conn: &Connection,
    kind: &str,
    dedup_key: &str,
    class: WorkClass,
    payload_json: &str,
    run_after: i64,
) -> Result<Option<i64>> {
    let now = now_ms();
    Ok(conn
        .query_row(
            "INSERT INTO jobs (kind, dedup_key, work_class, state, payload_json, run_after, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'queued', ?4, ?5, ?6, ?6)
             ON CONFLICT (dedup_key) DO NOTHING
             RETURNING id",
            params![kind, dedup_key, class.as_str(), payload_json, run_after, now],
            |r| r.get(0),
        )
        .optional()?)
}

/// Atomically takes the next ready job of `class`, oldest first, and marks
/// it running. Light and heavy work have separate workers, so a long
/// transcription never delays a cheap caption fetch.
pub fn claim(conn: &Connection, class: WorkClass, now: i64) -> Result<Option<Job>> {
    Ok(conn
        .query_row(
            "UPDATE jobs SET state = 'running', attempts = attempts + 1, updated_at = ?1
             WHERE id = (
                SELECT id FROM jobs
                WHERE state = 'queued' AND run_after <= ?1 AND work_class = ?2
                ORDER BY run_after, id
                LIMIT 1)
             RETURNING id, kind, dedup_key, work_class, payload_json, attempts, max_attempts",
            params![now, class.as_str()],
            |r| {
                Ok(Job {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    dedup_key: r.get(2)?,
                    work_class: r.get(3)?,
                    payload_json: r.get(4)?,
                    attempts: r.get(5)?,
                    max_attempts: r.get(6)?,
                })
            },
        )
        .optional()?)
}

pub fn succeed(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET state = 'succeeded', last_error = NULL, checkpoint_json = NULL, updated_at = ?2
         WHERE id = ?1",
        params![id, now_ms()],
    )?;
    Ok(())
}

/// Records a failure. The job is retried at `retry_at` while attempts remain,
/// otherwise it is marked `failed` and stays visible with its last error.
pub fn fail(conn: &Connection, id: i64, error: &str, retry_at: i64) -> Result<&'static str> {
    let state: String = conn.query_row(
        "UPDATE jobs SET
            state = CASE WHEN attempts >= max_attempts THEN 'failed' ELSE 'queued' END,
            last_error = ?2, run_after = ?3, updated_at = ?4
         WHERE id = ?1 RETURNING state",
        params![
            id,
            error.chars().take(2000).collect::<String>(),
            retry_at,
            now_ms()
        ],
        |r| r.get(0),
    )?;
    Ok(if state == "failed" {
        "failed"
    } else {
        "queued"
    })
}

/// Puts a job back without counting the attempt: used when work is
/// interrupted by the profile or shutdown rather than failing.
pub fn release(conn: &Connection, id: i64, run_after: i64) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET state = 'queued', attempts = MAX(attempts - 1, 0), run_after = ?2, updated_at = ?3
         WHERE id = ?1",
        params![id, run_after, now_ms()],
    )?;
    Ok(())
}

/// Reschedules a job for later without counting an attempt (e.g. "captions
/// may appear later").
pub fn defer(conn: &Connection, id: i64, run_after: i64, note: &str) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET state = 'queued', attempts = MAX(attempts - 1, 0), run_after = ?2,
                last_error = ?3, updated_at = ?4
         WHERE id = ?1",
        params![id, run_after, note, now_ms()],
    )?;
    Ok(())
}

/// Startup recovery: jobs left `running` were interrupted; queue them again.
pub fn requeue_interrupted(conn: &Connection) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE jobs SET state = 'queued', attempts = MAX(attempts - 1, 0), updated_at = ?1
         WHERE state = 'running'",
        [now_ms()],
    )?)
}

/// Earliest time a queued job of `class` becomes ready.
pub fn next_ready_at(conn: &Connection, class: WorkClass) -> Result<Option<i64>> {
    Ok(conn.query_row(
        "SELECT MIN(run_after) FROM jobs WHERE state = 'queued' AND work_class = ?1",
        [class.as_str()],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;

    async fn db() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Db::open(&dir.path().join("t.db")).unwrap(), dir)
    }

    #[tokio::test]
    async fn enqueue_is_idempotent() {
        let (db, _d) = db().await;
        let (a, b) = db
            .write(|c| {
                Ok((
                    enqueue(c, "media.captions", "captions:X", WorkClass::Light, "{}", 0)?,
                    enqueue(c, "media.captions", "captions:X", WorkClass::Light, "{}", 0)?,
                ))
            })
            .await
            .unwrap();
        assert!(a.is_some());
        assert_eq!(b, None);
    }

    #[tokio::test]
    async fn classes_are_claimed_separately_in_order() {
        let (db, _d) = db().await;
        let (light, heavy, none) = db
            .write(|c| {
                enqueue(c, "media.asr", "asr:X", WorkClass::Heavy, "{}", 0)?;
                enqueue(c, "media.captions", "captions:B", WorkClass::Light, "{}", 5)?;
                enqueue(c, "media.captions", "captions:A", WorkClass::Light, "{}", 1)?;
                let light = claim(c, WorkClass::Light, 10)?.unwrap();
                let heavy = claim(c, WorkClass::Heavy, 10)?.unwrap();
                let none = claim(c, WorkClass::Heavy, 10)?;
                Ok((light, heavy, none))
            })
            .await
            .unwrap();
        assert_eq!(
            light.dedup_key, "captions:A",
            "oldest ready light job first"
        );
        assert_eq!(heavy.kind, "media.asr");
        assert!(none.is_none());
    }

    #[tokio::test]
    async fn future_jobs_are_not_claimed_early() {
        let (db, _d) = db().await;
        let job = db
            .write(|c| {
                enqueue(c, "k", "k:1", WorkClass::Light, "{}", 1000)?;
                claim(c, WorkClass::Light, 999)
            })
            .await
            .unwrap();
        assert!(job.is_none());
    }

    #[tokio::test]
    async fn interrupted_jobs_come_back_without_burning_attempts() {
        let (db, _d) = db().await;
        let job = db
            .write(|c| {
                enqueue(c, "media.asr", "asr:X", WorkClass::Heavy, "{}", 0)?;
                claim(c, WorkClass::Heavy, 0)?; // running when the "crash" happens
                assert_eq!(requeue_interrupted(c)?, 1);
                claim(c, WorkClass::Heavy, 0)
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.attempts, 1);
    }

    #[tokio::test]
    async fn failures_retry_until_max_attempts() {
        let (db, _d) = db().await;
        let states = db
            .write(|c| {
                let id = enqueue(c, "k", "k:1", WorkClass::Light, "{}", 0)?.unwrap();
                c.execute("UPDATE jobs SET max_attempts = 2 WHERE id = ?1", [id])?;
                let mut states = vec![];
                for _ in 0..2 {
                    claim(c, WorkClass::Light, i64::MAX)?.unwrap();
                    states.push(fail(c, id, "boom", 0)?);
                }
                Ok(states)
            })
            .await
            .unwrap();
        assert_eq!(states, ["queued", "failed"]);
    }
}
