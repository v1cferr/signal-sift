//! SQLite persistence.
//!
//! [`Db`] owns two connections: one writer and one reader. SQLite in WAL mode
//! allows one writer concurrently with readers, so this pair gives the API
//! non-blocking reads while the worker writes, without a connection pool. Every
//! call runs on Tokio's blocking pool, never on the async worker thread.

pub mod media;
mod migrations;
pub mod sources;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};

pub use rusqlite;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("cannot create database directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "database was migrated by a newer SignalSift (schema {found}, this build knows {known})"
    )]
    SchemaTooNew { found: i64, known: i64 },
    #[error("database task panicked or was cancelled")]
    Join(#[from] tokio::task::JoinError),
    #[error("database lock poisoned by a panicking task")]
    Poisoned,
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T, E = StorageError> = std::result::Result<T, E>;

#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    writer: Mutex<Connection>,
    reader: Mutex<Connection>,
}

impl Db {
    /// Opens (creating if needed) the database at `path` and applies pending
    /// migrations. Blocking; call it before the server starts accepting work.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| StorageError::CreateDir {
                path: dir.to_path_buf(),
                source,
            })?;
        }
        let mut writer = Connection::open(path)?;
        configure(&writer)?;
        // WAL is a property of the database file; set it once, on the writer.
        writer.pragma_update(None, "journal_mode", "WAL")?;
        migrations::apply(&mut writer)?;

        let reader = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        configure(&reader)?;

        Ok(Self {
            inner: Arc::new(Inner {
                path: path.to_path_buf(),
                writer: Mutex::new(writer),
                reader: Mutex::new(reader),
            }),
        })
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Runs `f` with the writer connection on the blocking pool.
    pub async fn write<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let mut conn = inner.writer.lock().map_err(|_| StorageError::Poisoned)?;
            f(&mut conn)
        })
        .await?
    }

    /// Runs `f` with the read-only connection on the blocking pool.
    pub async fn read<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let conn = inner.reader.lock().map_err(|_| StorageError::Poisoned)?;
            f(&conn)
        })
        .await?
    }

    /// Row counts and schema version, for `status` and `/v1/status`.
    pub async fn summary(&self) -> Result<Summary> {
        self.read(|conn| {
            let count = |table: &str| -> Result<i64> {
                Ok(conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?)
            };
            let jobs = |state: &str| -> Result<i64> {
                Ok(
                    conn.query_row("SELECT count(*) FROM jobs WHERE state = ?1", [state], |r| {
                        r.get(0)
                    })?,
                )
            };
            Ok(Summary {
                schema_version: migrations::current_version(conn)?,
                sources: count("sources")?,
                items: count("items")?,
                transcripts: count("transcripts")?,
                segments: count("segments")?,
                jobs_queued: jobs("queued")?,
                jobs_running: jobs("running")?,
                jobs_failed: jobs("failed")?,
            })
        })
        .await
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Summary {
    pub schema_version: i64,
    pub sources: i64,
    pub items: i64,
    pub transcripts: i64,
    pub segments: i64,
    pub jobs_queued: i64,
    pub jobs_running: i64,
    pub jobs_failed: i64,
}

fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // NORMAL is durable across application crashes in WAL mode; only a power
    // loss can drop the last transactions, which a re-poll recovers.
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

/// Current time as Unix milliseconds, the representation every table uses.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn opens_migrates_and_reports_an_empty_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("nested/signalsift.db")).unwrap();
        let s = db.summary().await.unwrap();
        assert_eq!(s.schema_version, migrations::latest());
        assert_eq!(s.items, 0);
    }

    #[tokio::test]
    async fn reopening_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signalsift.db");
        drop(Db::open(&path).unwrap());
        let db = Db::open(&path).unwrap();
        assert_eq!(
            db.summary().await.unwrap().schema_version,
            migrations::latest()
        );
    }

    #[tokio::test]
    async fn uses_wal_and_enforces_foreign_keys() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("signalsift.db")).unwrap();
        let mode: String = db
            .read(|c| Ok(c.query_row("PRAGMA journal_mode", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(mode, "wal");
        let orphan = db
            .write(|c| {
                Ok(c.execute(
                    "INSERT INTO transcripts (item_id, origin, language, engine, status, created_at, updated_at)
                     VALUES (999, 'asr', 'pt', 'test', 'complete', 0, 0)",
                    [],
                )?)
            })
            .await;
        assert!(
            orphan.is_err(),
            "insert referencing a missing item must fail"
        );
    }

    #[tokio::test]
    async fn reader_cannot_write() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("signalsift.db")).unwrap();
        let r = db
            .read(|c| {
                Ok(c.execute(
                    "INSERT INTO creators (name, created_at) VALUES ('x', 0)",
                    [],
                )?)
            })
            .await;
        assert!(r.is_err());
    }
}
