//! Schema migrations, keyed on SQLite's `user_version`.
//!
//! Each migration is append-only: never edit one that has shipped, add the
//! next. `blocks_vec` deliberately arrives in a later migration because it
//! needs the sqlite-vec extension loaded, and a missing extension must not
//! stop the app from taking notes.

use anyhow::{Context, Result};
use rusqlite::Connection;

/// Ordered list of migrations; the index + 1 is the resulting `user_version`.
const MIGRATIONS: &[&str] = &[
    // v1 — notes, the derived block index, and the GitLab caches.
    r#"
    CREATE TABLE days (
      date       TEXT PRIMARY KEY,
      body       TEXT NOT NULL DEFAULT '',
      updated_at INTEGER NOT NULL
    );

    CREATE TABLE blocks (
      id         INTEGER PRIMARY KEY,
      date       TEXT NOT NULL REFERENCES days(date) ON DELETE CASCADE,
      ord        INTEGER NOT NULL,
      line_start INTEGER NOT NULL,
      line_end   INTEGER NOT NULL,
      kind       TEXT NOT NULL,
      text       TEXT NOT NULL
    );
    CREATE INDEX blocks_date ON blocks(date, ord);

    -- remove_diacritics 2 is what makes German, Spanish and French search
    -- work the way people actually type: "uber" finds "über", "nino" finds
    -- "niño". Mode 2 (unlike 1) also handles multi-codepoint sequences.
    CREATE VIRTUAL TABLE blocks_fts USING fts5(
      text, content='blocks', content_rowid='id',
      tokenize="unicode61 remove_diacritics 2"
    );

    -- External-content FTS5 does not track its table by itself; deletes need
    -- the special 'delete' command with the ORIGINAL text or the index rots.
    CREATE TRIGGER blocks_ai AFTER INSERT ON blocks BEGIN
      INSERT INTO blocks_fts(rowid, text) VALUES (new.id, new.text);
    END;
    CREATE TRIGGER blocks_ad AFTER DELETE ON blocks BEGIN
      INSERT INTO blocks_fts(blocks_fts, rowid, text) VALUES ('delete', old.id, old.text);
    END;
    CREATE TRIGGER blocks_au AFTER UPDATE ON blocks BEGIN
      INSERT INTO blocks_fts(blocks_fts, rowid, text) VALUES ('delete', old.id, old.text);
      INSERT INTO blocks_fts(rowid, text) VALUES (new.id, new.text);
    END;

    CREATE TABLE gitlab_refs (
      key        TEXT PRIMARY KEY,
      kind       TEXT NOT NULL,
      project    TEXT,
      iid        INTEGER,
      title      TEXT,
      state      TEXT,
      web_url    TEXT,
      author     TEXT,
      fetched_at INTEGER NOT NULL
    );

    CREATE TABLE gl_events (
      id          INTEGER PRIMARY KEY,
      local_date  TEXT NOT NULL,
      created_at  TEXT NOT NULL,
      project_id  INTEGER NOT NULL,
      action      TEXT NOT NULL,
      target_type TEXT,
      target_iid  INTEGER,
      title       TEXT,
      raw         TEXT NOT NULL
    );
    CREATE INDEX gl_events_day ON gl_events(local_date, created_at);

    CREATE TABLE gl_projects (
      id                  INTEGER PRIMARY KEY,
      path_with_namespace TEXT NOT NULL,
      name                TEXT NOT NULL,
      web_url             TEXT NOT NULL
    );

    CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
    "#,
    // v2 — the issue list behind `#` completion in the editor.
    r#"
    CREATE TABLE gl_issues (
      project TEXT NOT NULL,
      iid     INTEGER NOT NULL,
      title   TEXT NOT NULL,
      state   TEXT NOT NULL,
      web_url TEXT NOT NULL,
      PRIMARY KEY (project, iid)
    );
    CREATE INDEX gl_issues_title ON gl_issues(project, title);
    "#,
];

pub fn migrate(conn: &Connection) -> Result<()> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current as usize > MIGRATIONS.len() {
        anyhow::bail!(
            "database is at schema version {current} but this build only knows {}; \
             it was written by a newer version of daily-notes",
            MIGRATIONS.len()
        );
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let version = i + 1;
        conn.execute_batch(sql)
            .with_context(|| format!("applying schema migration v{version}"))?;
        // PRAGMA does not accept bound parameters.
        conn.execute_batch(&format!("PRAGMA user_version = {version}"))?;
        tracing::info!(version, "schema migrated");
    }
    Ok(())
}
