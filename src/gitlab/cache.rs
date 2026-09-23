//! The `gitlab_refs` table: what a reference resolved to, and when.
//!
//! The editor paints from this cache synchronously so typing is never blocked
//! on the network; anything stale is refreshed in the background.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use super::client::Resolved;

/// How long a cached resolution is treated as current. Titles rarely change;
/// state (opened/merged/closed) does, which is what sets the ceiling.
pub const TTL_SECONDS: i64 = 15 * 60;

#[derive(Debug, Clone)]
pub struct Entry {
    pub resolved: Option<Resolved>,
    pub fetched_at: i64,
}

impl Entry {
    pub fn is_fresh(&self, now: i64) -> bool {
        now - self.fetched_at < TTL_SECONDS
    }
}

pub fn get(conn: &Connection, key: &str) -> Result<Option<Entry>> {
    let row = conn
        .query_row(
            "SELECT title, state, web_url, author, fetched_at FROM gitlab_refs WHERE key = ?1",
            params![key],
            |r| {
                let title: Option<String> = r.get(0)?;
                Ok((
                    title,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()?;

    Ok(row.map(|(title, state, web_url, author, fetched_at)| Entry {
        // A NULL title records "looked it up, there is nothing there" — worth
        // remembering so a typo is not re-fetched on every keystroke.
        resolved: title.map(|title| Resolved {
            title,
            state: state.unwrap_or_default(),
            web_url: web_url.unwrap_or_default(),
            author: author.unwrap_or_default(),
        }),
        fetched_at,
    }))
}

pub fn put(conn: &Connection, key: &str, kind: &str, resolved: Option<&Resolved>) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT INTO gitlab_refs(key, kind, title, state, web_url, author, fetched_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(key) DO UPDATE SET
           title = excluded.title, state = excluded.state, web_url = excluded.web_url,
           author = excluded.author, fetched_at = excluded.fetched_at",
        params![
            key,
            kind,
            resolved.map(|r| r.title.as_str()),
            resolved.map(|r| r.state.as_str()),
            resolved.map(|r| r.web_url.as_str()),
            resolved.map(|r| r.author.as_str()),
            now
        ],
    )?;
    Ok(())
}

/// Drops every cached resolution — used when the instance or token changes,
/// since entries from one instance mean nothing on another.
pub fn clear(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM gitlab_refs", [])?;
    Ok(())
}
