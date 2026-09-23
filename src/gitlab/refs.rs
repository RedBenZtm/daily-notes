//! Finding GitLab references in note text.
//!
//! The syntax is GitLab's own, so a note can be written the same way as an
//! issue comment: `#123`, `!45`, `&7`, `@user`, and the cross-project forms
//! `group/project#123` / `group/project!45`.
//!
//! Offsets are byte offsets into the original text, so the editor can turn a
//! reference straight into a `TextIter` pair.

use std::sync::LazyLock;

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Issue,
    MergeRequest,
    Epic,
    User,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Issue => "issue",
            Kind::MergeRequest => "mr",
            Kind::Epic => "epic",
            Kind::User => "user",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Reference {
    pub kind: Kind,
    /// `None` means "the configured main project" (or main group, for epics).
    pub path: Option<String>,
    pub iid: Option<u64>,
    pub username: Option<String>,
    pub start: usize,
    pub end: usize,
}

impl Reference {
    /// Stable cache key. `path` is resolved against the configuration first,
    /// so two spellings of the same thing share one cache row.
    pub fn cache_key(&self, default_path: &str) -> String {
        let path = self.path.as_deref().unwrap_or(default_path);
        match self.kind {
            Kind::User => format!("user::{}", self.username.clone().unwrap_or_default()),
            _ => format!("{}:{}:{}", self.kind.as_str(), path, self.iid.unwrap_or(0)),
        }
    }
}

// `group/sub/project#123`, `!45`, `&7` — the project part is optional.
static NUMERIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?P<path>[A-Za-z0-9_][A-Za-z0-9_.\-]*(?:/[A-Za-z0-9_.\-]+)+)?(?P<sigil>[#!&])(?P<iid>\d+)",
    )
    .expect("static regex")
});

static USER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"@(?P<user>[A-Za-z0-9_][A-Za-z0-9_.\-]*)").expect("static regex"));

/// Blanks out regions where a reference would be a false positive, keeping
/// byte offsets intact so matches still line up with the original text.
///
/// Three kinds of region: fenced code blocks, inline code spans, and URLs.
/// URLs matter because `…/page#section` is an anchor, not issue 0, and a
/// markdown link's target should not be scanned.
fn mask(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = bytes.to_vec();
    let blank = |from: usize, to: usize, out: &mut Vec<u8>| {
        for b in out.iter_mut().take(to).skip(from) {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    };

    // Fenced blocks, line by line.
    let mut offset = 0usize;
    let mut fence: Option<&str> = None;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let marker = ["```", "~~~"].into_iter().find(|m| trimmed.starts_with(m));
        match (fence, marker) {
            (None, Some(m)) => {
                fence = Some(m);
                blank(offset, offset + line.len(), &mut out);
            }
            (Some(open), Some(m)) if open == m => {
                fence = None;
                blank(offset, offset + line.len(), &mut out);
            }
            (Some(_), _) => blank(offset, offset + line.len(), &mut out),
            (None, None) => {}
        }
        offset += line.len();
    }

    // Inline code spans, on what is left.
    let masked = String::from_utf8_lossy(&out).into_owned();
    let mut out = masked.clone().into_bytes();
    let mut i = 0usize;
    let m = masked.as_bytes();
    while i < m.len() {
        if m[i] == b'`' {
            let ticks = m[i..].iter().take_while(|&&c| c == b'`').count();
            let close = masked[i + ticks..]
                .find(&"`".repeat(ticks))
                .map(|p| i + ticks + p);
            if let Some(close) = close {
                blank(i, close + ticks, &mut out);
                i = close + ticks;
                continue;
            }
        }
        i += 1;
    }

    // URLs.
    let masked = String::from_utf8_lossy(&out).into_owned();
    let mut out = masked.clone().into_bytes();
    static URL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[a-zA-Z][a-zA-Z0-9+.\-]*://\S+").expect("static regex"));
    for m in URL.find_iter(&masked) {
        blank(m.start(), m.end(), &mut out);
    }

    String::from_utf8_lossy(&out).into_owned()
}

/// True when a match may start at this byte offset — i.e. it is not glued to
/// the tail of a longer word.
fn starts_cleanly(text: &str, at: usize) -> bool {
    if at == 0 {
        return true;
    }
    match text[..at].chars().next_back() {
        // A ref may follow punctuation or space, but `abc#1` is not a ref and
        // neither is `v1.2#3`.
        Some(c) => !(c.is_alphanumeric() || c == '_' || c == '/' || c == '.' || c == '-'),
        None => true,
    }
}

pub fn parse(text: &str) -> Vec<Reference> {
    let masked = mask(text);
    let mut out = Vec::new();

    for caps in NUMERIC.captures_iter(&masked) {
        let whole = caps.get(0).expect("group 0");
        if !starts_cleanly(&masked, whole.start()) {
            continue;
        }
        let sigil = &caps["sigil"];
        let path = caps.name("path").map(|m| m.as_str().to_string());
        let kind = match sigil {
            "#" => Kind::Issue,
            "!" => Kind::MergeRequest,
            _ => Kind::Epic,
        };
        let Ok(iid) = caps["iid"].parse::<u64>() else {
            continue;
        };
        if iid == 0 {
            continue;
        }
        out.push(Reference {
            kind,
            path,
            iid: Some(iid),
            username: None,
            start: whole.start(),
            end: whole.end(),
        });
    }

    for caps in USER.captures_iter(&masked) {
        let whole = caps.get(0).expect("group 0");
        if !starts_cleanly(&masked, whole.start()) {
            continue;
        }
        out.push(Reference {
            kind: Kind::User,
            path: None,
            iid: None,
            username: Some(caps["user"].to_string()),
            start: whole.start(),
            end: whole.end(),
        });
    }

    out.sort_by_key(|r| r.start);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<(Kind, Option<String>, Option<u64>, &str)> {
        parse(text)
            .into_iter()
            .map(|r| (r.kind, r.path.clone(), r.iid, &text[r.start..r.end]))
            .collect()
    }

    fn found(text: &str) -> Vec<String> {
        parse(text).into_iter().map(|r| text[r.start..r.end].to_string()).collect()
    }

    #[test]
    fn finds_the_basic_forms() {
        assert_eq!(
            kinds("fixed #142 and reviewed !87, epic &7"),
            vec![
                (Kind::Issue, None, Some(142), "#142"),
                (Kind::MergeRequest, None, Some(87), "!87"),
                (Kind::Epic, None, Some(7), "&7"),
            ]
        );
    }

    #[test]
    fn finds_cross_project_forms() {
        assert_eq!(
            kinds("see api/gateway#12 and group/sub/proj!5"),
            vec![
                (Kind::Issue, Some("api/gateway".into()), Some(12), "api/gateway#12"),
                (
                    Kind::MergeRequest,
                    Some("group/sub/proj".into()),
                    Some(5),
                    "group/sub/proj!5"
                ),
            ]
        );
    }

    #[test]
    fn finds_users() {
        let refs = parse("ping @a.user about it");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, Kind::User);
        assert_eq!(refs[0].username.as_deref(), Some("a.user"));
    }

    #[test]
    fn a_markdown_heading_is_not_a_reference() {
        // The whole reason GitLab refs are safe at the start of a line.
        assert!(found("# Heading\n## Another").is_empty());
    }

    #[test]
    fn a_reference_at_the_start_of_a_line_still_counts() {
        assert_eq!(found("#142 is still broken"), vec!["#142"]);
    }

    #[test]
    fn code_spans_and_fences_are_literal() {
        assert!(found("use `#142` literally").is_empty());
        assert!(found("```\n#142\n!87\n```").is_empty());
        assert!(found("~~~sh\necho '#142'\n~~~").is_empty());
        // ...but text after a closed fence is scanned again
        assert_eq!(found("```\n#1\n```\nthen #142"), vec!["#142"]);
    }

    #[test]
    fn urls_are_not_scanned() {
        // `#anchor` and `#L12` in a URL are not issue references.
        assert!(found("see https://example.com/page#142 for details").is_empty());
        assert!(found("[MR](https://gitlab.example.com/a/b/-/merge_requests/122)").is_empty());
    }

    #[test]
    fn a_ref_glued_to_a_word_is_not_a_ref() {
        assert!(found("abc#123").is_empty());
        assert!(found("v1.2#3").is_empty());
        // but ordinary punctuation before it is fine
        assert_eq!(found("(#142)"), vec!["#142"]);
        assert_eq!(found("see:#142"), vec!["#142"]);
    }

    #[test]
    fn zero_is_not_a_valid_iid() {
        assert!(found("#0").is_empty());
    }

    #[test]
    fn offsets_point_at_the_original_text() {
        let text = "a `#1` b #142 c";
        let refs = parse(text);
        assert_eq!(refs.len(), 1);
        assert_eq!(&text[refs[0].start..refs[0].end], "#142");
    }

    #[test]
    fn multibyte_text_does_not_shift_offsets() {
        let text = "Änderung für #142 — größer";
        let refs = parse(text);
        assert_eq!(refs.len(), 1);
        assert_eq!(&text[refs[0].start..refs[0].end], "#142");
    }

    #[test]
    fn cache_keys_collapse_equivalent_spellings() {
        let a = &parse("#142")[0];
        let b = &parse("group/proj#142")[0];
        assert_eq!(a.cache_key("group/proj"), b.cache_key("group/proj"));
        assert_eq!(a.cache_key("group/proj"), "issue:group/proj:142");
    }
}
