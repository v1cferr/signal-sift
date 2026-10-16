//! Versioned schema migrations tracked in `PRAGMA user_version`.
//!
//! Migrations are embedded SQL files applied in order, each inside its own
//! transaction together with the version bump, so a crash mid-migration leaves
//! the previous version intact. Applied files are never edited; a change is a
//! new file.

use rusqlite::Connection;

use crate::{Result, StorageError};

const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_initial.sql"),
    include_str!("../migrations/0002_analysis_details.sql"),
];

pub fn latest() -> i64 {
    MIGRATIONS.len() as i64
}

pub fn current_version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

pub fn apply(conn: &mut Connection) -> Result<()> {
    let found = current_version(conn)?;
    if found > latest() {
        return Err(StorageError::SchemaTooNew {
            found,
            known: latest(),
        });
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(found as usize) {
        let version = index as i64 + 1;
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
        tracing::info!(version, "applied database migration");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_a_database_from_the_future() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", latest() + 1)
            .unwrap();
        assert!(matches!(
            apply(&mut conn),
            Err(StorageError::SchemaTooNew { .. })
        ));
    }
}
