//! Free-text search over the block index.
//!
//! Keyword search only for now; step 9 adds the semantic leg and fuses the two
//! with reciprocal rank fusion. The `Hit` shape and the ranked-list contract
//! are already what the fusion step needs, so the UI will not change.

use anyhow::Result;
use chrono::NaiveDate;
use rusqlite::{params, Connection};

use super::blocks::Kind;
use super::days::DATE_FMT;

#[derive(Debug, Clone)]
pub struct Hit {
    pub date: NaiveDate,
    pub line: usize,
    /// Used by the vector leg in step 9 to weight code blocks differently.
    #[allow(dead_code)]
    pub kind: Kind,
    pub text: String,
    /// Higher is better. Read by the fusion step (step 9).
    #[allow(dead_code)]
    pub score: f64,
}

/// Turns what a person typed into an FTS5 query.
///
/// Users type prose, not query syntax — an unbalanced quote or a bare `OR`
/// would otherwise be a syntax error rather than a search. Every term is
/// quoted (so punctuation like `#142` or `api/gateway` is literal) and the
/// last one gets a `*` so results narrow as you type.
pub fn to_fts_query(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split_whitespace()
        .map(|t| t.replace('"', "")) // a quote can only unbalance the query
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() {
        return None;
    }
    let last = terms.len() - 1;
    Some(
        terms
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i == last {
                    format!("\"{t}\"*")
                } else {
                    format!("\"{t}\"")
                }
            })
            .collect::<Vec<_>>()
            .join(" AND "),
    )
}

pub fn keyword(conn: &Connection, input: &str, limit: usize) -> Result<Vec<Hit>> {
    let Some(query) = to_fts_query(input) else {
        return Ok(Vec::new());
    };

    let mut stmt = conn.prepare(
        "SELECT b.date, b.line_start, b.kind, b.text, bm25(blocks_fts) AS rank
         FROM blocks_fts
         JOIN blocks b ON b.id = blocks_fts.rowid
         WHERE blocks_fts MATCH ?1
         ORDER BY rank, b.date DESC
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(params![query, limit as i64], |r| {
        let date: String = r.get(0)?;
        let kind: String = r.get(2)?;
        // bm25() is negative and smaller means better; flip it so callers can
        // treat every score the same way once vector hits join the list.
        let rank: f64 = r.get(4)?;
        Ok(Hit {
            date: NaiveDate::parse_from_str(&date, DATE_FMT).unwrap_or_default(),
            line: r.get::<_, i64>(1)? as usize,
            kind: match kind.as_str() {
                "heading" => Kind::Heading,
                "bullet" => Kind::Bullet,
                "code" => Kind::Code,
                _ => Kind::Para,
            },
            text: r.get(3)?,
            score: -rank,
        })
    })?;

    // An FTS5 syntax error is a bad query, not a broken database: return
    // nothing rather than tearing down the search page.
    match rows.collect::<rusqlite::Result<Vec<_>>>() {
        Ok(hits) => Ok(hits),
        Err(e) => {
            tracing::debug!(?e, %input, "search query rejected");
            Ok(Vec::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_prefix_query() {
        assert_eq!(to_fts_query("ldap"), Some(r#""ldap"*"#.into()));
        assert_eq!(
            to_fts_query("ldap login"),
            Some(r#""ldap" AND "login"*"#.into())
        );
    }

    #[test]
    fn empty_input_is_not_a_query() {
        assert_eq!(to_fts_query(""), None);
        assert_eq!(to_fts_query("   "), None);
    }

    #[test]
    fn punctuation_and_operators_are_literal_not_syntax() {
        // These would all be FTS5 syntax errors if passed through raw.
        assert_eq!(to_fts_query("#142"), Some(r##""#142"*"##.into()));
        assert_eq!(to_fts_query("api/gateway"), Some(r#""api/gateway"*"#.into()));
        assert_eq!(to_fts_query("a OR b"), Some(r#""a" AND "OR" AND "b"*"#.into()));
        assert_eq!(to_fts_query(r#"un"balanced"#), Some(r#""unbalanced"*"#.into()));
    }
}
