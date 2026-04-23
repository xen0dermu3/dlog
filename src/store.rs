use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{NaiveDate, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use crate::scanner::CommitRecord;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS scan_snapshot (
    start_date TEXT NOT NULL,
    end_date   TEXT NOT NULL,
    repos      TEXT NOT NULL,
    records    TEXT NOT NULL,
    scanned_at INTEGER NOT NULL,
    PRIMARY KEY (start_date, end_date, repos)
);
CREATE TABLE IF NOT EXISTS worklog_sent (
    ticket     TEXT NOT NULL,
    start_date TEXT NOT NULL,
    end_date   TEXT NOT NULL,
    worklog_id TEXT,
    sent_at    INTEGER NOT NULL,
    PRIMARY KEY (ticket, start_date, end_date)
);
"#;

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (or creates) the on-disk database at `~/.dlog/dlog.sqlite`.
    /// Fails cleanly; the TUI falls back to in-memory-only if this errors.
    pub fn open() -> Result<Self> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(&path)
            .with_context(|| format!("opening SQLite db at {}", path.display()))?;
        conn.execute_batch(SCHEMA).context("creating schema")?;
        Ok(Self { conn })
    }

    pub fn path() -> Result<PathBuf> {
        let base = directories::BaseDirs::new().context("resolving home directory")?;
        Ok(base.home_dir().join(".dlog").join("dlog.sqlite"))
    }

    // --- scan cache ------------------------------------------------------

    pub fn save_scan(
        &self,
        start: NaiveDate,
        end: NaiveDate,
        repos: &[PathBuf],
        records: &[CommitRecord],
    ) -> Result<()> {
        let json = serde_json::to_string(records)?;
        let now = Utc::now().timestamp();
        self.conn.execute(
            "INSERT INTO scan_snapshot (start_date, end_date, repos, records, scanned_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(start_date, end_date, repos) DO UPDATE SET
               records = excluded.records,
               scanned_at = excluded.scanned_at",
            params![
                start.to_string(),
                end.to_string(),
                repos_key(repos),
                json,
                now
            ],
        )?;
        Ok(())
    }

    pub fn load_scan(
        &self,
        start: NaiveDate,
        end: NaiveDate,
        repos: &[PathBuf],
    ) -> Result<Option<Vec<CommitRecord>>> {
        let maybe: Option<String> = self
            .conn
            .query_row(
                "SELECT records FROM scan_snapshot
                 WHERE start_date = ?1 AND end_date = ?2 AND repos = ?3",
                params![start.to_string(), end.to_string(), repos_key(repos)],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match maybe {
            None => Ok(None),
            Some(json) => Ok(Some(
                serde_json::from_str(&json).context("parsing cached scan records")?,
            )),
        }
    }

    pub fn clear_scan(
        &self,
        start: NaiveDate,
        end: NaiveDate,
        repos: &[PathBuf],
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM scan_snapshot
             WHERE start_date = ?1 AND end_date = ?2 AND repos = ?3",
            params![start.to_string(), end.to_string(), repos_key(repos)],
        )?;
        Ok(())
    }

    // --- worklog dedup ---------------------------------------------------

    pub fn record_worklog(
        &self,
        ticket: &str,
        start: NaiveDate,
        end: NaiveDate,
        worklog_id: &str,
    ) -> Result<()> {
        let now = Utc::now().timestamp();
        self.conn.execute(
            "INSERT OR REPLACE INTO worklog_sent
             (ticket, start_date, end_date, worklog_id, sent_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                ticket,
                start.to_string(),
                end.to_string(),
                worklog_id,
                now
            ],
        )?;
        Ok(())
    }

    pub fn worklogs_for_range(
        &self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<HashSet<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT ticket FROM worklog_sent WHERE start_date = ?1 AND end_date = ?2",
        )?;
        let rows = stmt.query_map(params![start.to_string(), end.to_string()], |row| {
            row.get::<_, String>(0)
        })?;
        let mut out = HashSet::new();
        for r in rows {
            out.insert(r?);
        }
        Ok(out)
    }
}

fn repos_key(repos: &[PathBuf]) -> String {
    let mut names: Vec<String> = repos.iter().map(|p| p.display().to_string()).collect();
    names.sort();
    names.join("\n")
}
