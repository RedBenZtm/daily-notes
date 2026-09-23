//! SQLite persistence. The database is the only representation of a note —
//! there are no Markdown files on disk.

pub mod blocks;
pub mod days;
pub mod search;
mod schema;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use rusqlite::Connection;

// `Kind` and `blocks()` are the API steps 6–8 (search, refs, activity) build on.
#[allow(unused_imports)]
pub use blocks::{Block, Kind};

#[allow(unused_imports)]
pub use days::DATE_FMT;

/// `~/.daily_notes/notes.db`.
pub fn default_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(Path::new(&home).join(".daily_notes").join("notes.db"))
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating {}", dir.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        // WAL keeps the debounced saves from blocking reads; foreign_keys is
        // off by default in SQLite and blocks' ON DELETE CASCADE needs it.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;",
        )?;
        schema::migrate(&conn)?;
        Ok(Self { conn })
    }

    pub fn body(&self, date: NaiveDate) -> Result<String> {
        days::body(&self.conn, date)
    }

    pub fn save(&mut self, date: NaiveDate, body: &str) -> Result<()> {
        days::save(&mut self.conn, date, body)
    }

    #[allow(dead_code)]
    pub fn blocks(&self, date: NaiveDate) -> Result<Vec<Block>> {
        days::blocks_of(&self.conn, date)
    }

    pub fn summary(&self, date: NaiveDate) -> Result<Vec<Block>> {
        days::summary(&self.conn, date)
    }

    pub fn last_workday(&self, before: NaiveDate) -> Result<Option<NaiveDate>> {
        days::last_workday(&self.conn, before)
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<NaiveDate>> {
        days::recent(&self.conn, limit)
    }

    /// Small key/value settings that belong with the data rather than in the
    /// config file — e.g. which tray section was last used.
    pub fn meta_get(&self, key: &str) -> Option<String> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row("SELECT v FROM meta WHERE k = ?1", rusqlite::params![key], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(k, v) VALUES (?1, ?2)
             ON CONFLICT(k) DO UPDATE SET v = excluded.v",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<search::Hit>> {
        search::keyword(&self.conn, query, limit)
    }

    pub fn ref_cache_get(&self, key: &str) -> Result<Option<crate::gitlab::cache::Entry>> {
        crate::gitlab::cache::get(&self.conn, key)
    }

    pub fn ref_cache_put(
        &self,
        key: &str,
        kind: &str,
        resolved: Option<&crate::gitlab::client::Resolved>,
    ) -> Result<()> {
        crate::gitlab::cache::put(&self.conn, key, kind, resolved)
    }

    pub fn ref_cache_clear(&self) -> Result<()> {
        crate::gitlab::cache::clear(&self.conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, days::DATE_FMT).unwrap()
    }

    fn store_with(days_: &[(&str, &str)]) -> Store {
        let mut s = Store::open_in_memory().unwrap();
        for (date, body) in days_ {
            s.save(d(date), body).unwrap();
        }
        s
    }

    #[test]
    fn round_trips_a_day() {
        let s = store_with(&[("2026-09-23", "## Today\n- reviewed !87\n")]);
        assert_eq!(s.body(d("2026-09-23")).unwrap(), "## Today\n- reviewed !87\n");
    }

    #[test]
    fn missing_day_reads_as_empty() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.body(d("2026-01-01")).unwrap(), "");
    }

    #[test]
    fn saving_replaces_blocks_rather_than_appending() {
        let mut s = store_with(&[("2026-09-23", "- one
- two
")]);
        assert_eq!(s.summary(d("2026-09-23")).unwrap().len(), 2);
        s.save(d("2026-09-23"), "- only one now
").unwrap();
        let e = s.summary(d("2026-09-23")).unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].text, "only one now");
    }

    #[test]
    fn summary_includes_prose_but_not_code() {
        let s = store_with(&[(
            "2026-09-23",
            "## Heading\n\nA plain paragraph.\n\n- a bullet\n\n```sh\nls -la\n```\n",
        )]);
        let texts: Vec<_> = s
            .summary(d("2026-09-23"))
            .unwrap()
            .into_iter()
            .map(|b| b.text)
            .collect();
        assert_eq!(texts, vec!["Heading", "A plain paragraph.", "a bullet"]);
        // a day of pure prose must not look empty in the tray
        let s2 = store_with(&[("2026-09-22", "Just some prose, no bullets at all.\n")]);
        assert_eq!(s2.summary(d("2026-09-22")).unwrap().len(), 1);
    }

    #[test]
    fn last_workday_skips_weekends_and_gaps() {
        // Fri 18th, then nothing until Wed 23rd.
        let s = store_with(&[("2026-09-18", "- friday\n"), ("2026-09-23", "- today\n")]);
        assert_eq!(s.last_workday(d("2026-09-23")).unwrap(), Some(d("2026-09-18")));
    }

    #[test]
    fn last_workday_ignores_blank_days() {
        let s = store_with(&[
            ("2026-09-21", "- real content\n"),
            ("2026-09-22", "   \n\n"),
            ("2026-09-23", "- today\n"),
        ]);
        assert_eq!(s.last_workday(d("2026-09-23")).unwrap(), Some(d("2026-09-21")));
    }

    #[test]
    fn last_workday_is_none_on_the_first_ever_day() {
        let s = store_with(&[("2026-09-23", "- today\n")]);
        assert_eq!(s.last_workday(d("2026-09-23")).unwrap(), None);
    }

    #[test]
    fn recent_is_newest_first_and_skips_blanks() {
        let s = store_with(&[
            ("2026-09-21", "- a\n"),
            ("2026-09-22", ""),
            ("2026-09-23", "- c\n"),
        ]);
        assert_eq!(s.recent(10).unwrap(), vec![d("2026-09-23"), d("2026-09-21")]);
    }

    #[test]
    fn ref_cache_round_trips_and_remembers_misses() {
        use crate::gitlab::client::Resolved;
        let s = Store::open_in_memory().unwrap();
        assert!(s.ref_cache_get("issue:g/p:1").unwrap().is_none());

        let r = Resolved {
            title: "Fix LDAP login loop".into(),
            state: "opened".into(),
            web_url: "https://gl/g/p/-/issues/1".into(),
            author: "a.user".into(),
        };
        s.ref_cache_put("issue:g/p:1", "issue", Some(&r)).unwrap();
        let got = s.ref_cache_get("issue:g/p:1").unwrap().unwrap();
        assert_eq!(got.resolved.as_ref().unwrap().title, "Fix LDAP login loop");
        assert!(got.is_fresh(chrono::Utc::now().timestamp()));

        // A miss is cached too, so a typo is not re-fetched on every keystroke.
        s.ref_cache_put("issue:g/p:999", "issue", None).unwrap();
        let miss = s.ref_cache_get("issue:g/p:999").unwrap().unwrap();
        assert!(miss.resolved.is_none());

        // Stale once past the TTL.
        let old = crate::gitlab::cache::Entry {
            resolved: None,
            fetched_at: chrono::Utc::now().timestamp() - crate::gitlab::cache::TTL_SECONDS - 1,
        };
        assert!(!old.is_fresh(chrono::Utc::now().timestamp()));

        s.ref_cache_clear().unwrap();
        assert!(s.ref_cache_get("issue:g/p:1").unwrap().is_none());
    }

    #[test]
    fn meta_round_trips_and_overwrites() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.meta_get("tray_section"), None);
        s.meta_set("tray_section", "last_workday").unwrap();
        assert_eq!(s.meta_get("tray_section").as_deref(), Some("last_workday"));
        s.meta_set("tray_section", "today").unwrap();
        assert_eq!(s.meta_get("tray_section").as_deref(), Some("today"));
    }

    #[test]
    fn search_finds_blocks_and_locates_them() {
        let s = store_with(&[
            ("2026-09-21", "- fixed the LDAP login loop\n"),
            ("2026-09-22", "- unrelated note\n- more LDAP work\n"),
        ]);
        let hits = s.search("ldap", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.text.contains("LDAP")));
        // every hit points at a real line of a real day
        for h in &hits {
            let body = s.body(h.date).unwrap();
            assert!(body.lines().nth(h.line).unwrap().contains("LDAP"));
        }
    }

    #[test]
    fn search_is_diacritic_and_case_insensitive() {
        // The reason the tokenizer is unicode61 remove_diacritics 2: German,
        // Spanish and French notes must be findable as typed on any keyboard.
        let s = store_with(&[("2026-09-23", "- Anmeldung schlägt fehl für Müller\n")]);
        assert_eq!(s.search("schlagt", 10).unwrap().len(), 1);
        assert_eq!(s.search("MULLER", 10).unwrap().len(), 1);
        assert_eq!(s.search("müller", 10).unwrap().len(), 1);
    }

    #[test]
    fn search_narrows_as_you_type() {
        let s = store_with(&[("2026-09-23", "- deployment pipeline\n- deposit form\n")]);
        assert_eq!(s.search("dep", 10).unwrap().len(), 2);
        assert_eq!(s.search("deplo", 10).unwrap().len(), 1);
    }

    #[test]
    fn search_index_follows_edits_and_deletions() {
        let mut s = store_with(&[("2026-09-23", "- findable text\n")]);
        assert_eq!(s.search("findable", 10).unwrap().len(), 1);
        s.save(d("2026-09-23"), "- replaced entirely\n").unwrap();
        // the external-content FTS index must have dropped the old row
        assert_eq!(s.search("findable", 10).unwrap().len(), 0);
        assert_eq!(s.search("replaced", 10).unwrap().len(), 1);
    }

    #[test]
    fn a_nonsense_query_returns_nothing_rather_than_failing() {
        let s = store_with(&[("2026-09-23", "- something\n")]);
        assert!(s.search("\"\"\"", 10).unwrap().is_empty());
        assert!(s.search("NOT AND OR", 10).unwrap().is_empty());
    }

    #[test]
    fn migrations_are_idempotent_across_reopens() {
        let dir = std::env::temp_dir().join(format!("dn-test-{}", std::process::id()));
        let path = dir.join("notes.db");
        let _ = std::fs::remove_dir_all(&dir);
        {
            let mut s = Store::open(&path).unwrap();
            s.save(d("2026-09-23"), "- persisted\n").unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.summary(d("2026-09-23")).unwrap()[0].text, "persisted");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
