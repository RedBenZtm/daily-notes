//! Day CRUD and the derived block projection.

use anyhow::Result;
use chrono::NaiveDate;
use rusqlite::{params, Connection};

use super::blocks::{self, Block, Kind};

pub const DATE_FMT: &str = "%Y-%m-%d";

pub fn body(conn: &Connection, date: NaiveDate) -> Result<String> {
    let key = date.format(DATE_FMT).to_string();
    use rusqlite::OptionalExtension;
    let body: Option<String> = conn
        .query_row("SELECT body FROM days WHERE date = ?1", params![key], |r| r.get(0))
        .optional()?;
    Ok(body.unwrap_or_default())
}

/// Writes the day and rebuilds its blocks in one transaction.
///
/// Rebuilding wholesale rather than diffing is deliberate: a day is a few
/// kilobytes, and it makes the projection impossible to get out of step with
/// `days.body`.
pub fn save(conn: &mut Connection, date: NaiveDate, body: &str) -> Result<()> {
    let key = date.format(DATE_FMT).to_string();
    let now = chrono::Utc::now().timestamp();
    let tx = conn.transaction()?;

    tx.execute(
        "INSERT INTO days(date, body, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(date) DO UPDATE SET body = excluded.body, updated_at = excluded.updated_at",
        params![key, body, now],
    )?;
    // The AFTER DELETE trigger keeps blocks_fts in step.
    tx.execute("DELETE FROM blocks WHERE date = ?1", params![key])?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO blocks(date, ord, line_start, line_end, kind, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for b in blocks::parse(body) {
            stmt.execute(params![
                key,
                b.ord as i64,
                b.line_start as i64,
                b.line_end as i64,
                b.kind.as_str(),
                b.text
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn row_to_block(row: &rusqlite::Row<'_>) -> rusqlite::Result<Block> {
    let kind: String = row.get(4)?;
    Ok(Block {
        ord: row.get::<_, i64>(1)? as usize,
        line_start: row.get::<_, i64>(2)? as usize,
        line_end: row.get::<_, i64>(3)? as usize,
        kind: match kind.as_str() {
            "heading" => Kind::Heading,
            "bullet" => Kind::Bullet,
            "code" => Kind::Code,
            _ => Kind::Para,
        },
        text: row.get(5)?,
    })
}

pub fn blocks_of(conn: &Connection, date: NaiveDate) -> Result<Vec<Block>> {
    let key = date.format(DATE_FMT).to_string();
    let mut stmt = conn.prepare(
        "SELECT id, ord, line_start, line_end, kind, text FROM blocks
         WHERE date = ?1 ORDER BY ord",
    )?;
    let rows = stmt.query_map(params![key], row_to_block)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Everything readable about a day, for glancing at it in the tray.
///
/// Not just bullets: people write plain paragraphs too, and a day of prose
/// must not look empty. Code blocks are skipped — they are never what you
/// want to read back during a standup.
pub fn summary(conn: &Connection, date: NaiveDate) -> Result<Vec<Block>> {
    Ok(blocks_of(conn, date)?
        .into_iter()
        .filter(|b| b.kind != Kind::Code)
        .collect())
}

/// The most recent day before `before` that actually has content.
///
/// Defined by the data rather than by the calendar, so weekends, public
/// holidays and vacation all fall out for free — no workweek configuration.
pub fn last_workday(conn: &Connection, before: NaiveDate) -> Result<Option<NaiveDate>> {
    use rusqlite::OptionalExtension;
    let key = before.format(DATE_FMT).to_string();
    let found: Option<String> = conn
        .query_row(
            // "Has content" means "produced at least one block". Testing the
             // body instead would need whitespace trimming SQLite's trim()
             // does not do: it strips spaces only, never newlines or tabs.
             "SELECT date FROM days
              WHERE date < ?1 AND EXISTS (SELECT 1 FROM blocks WHERE blocks.date = days.date)
              ORDER BY date DESC LIMIT 1",
            params![key],
            |r| r.get(0),
        )
        .optional()?;
    Ok(found.and_then(|s| NaiveDate::parse_from_str(&s, DATE_FMT).ok()))
}

/// Days with content, newest first — the main window's sidebar.
pub fn recent(conn: &Connection, limit: usize) -> Result<Vec<NaiveDate>> {
    let mut stmt = conn.prepare(
        "SELECT date FROM days
         WHERE EXISTS (SELECT 1 FROM blocks WHERE blocks.date = days.date)
         ORDER BY date DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |r| r.get::<_, String>(0))?;
    Ok(rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter_map(|s| NaiveDate::parse_from_str(&s, DATE_FMT).ok())
        .collect())
}
