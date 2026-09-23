//! Rendering a note entry's inline markdown.
//!
//! Two outputs from one tokenizer: Pango markup for `GtkLabel` (links and
//! ticket references become `<a>`, with `title=` giving each its hover card),
//! and flat text for the tray, whose menu labels are plain strings at the
//! protocol level — no markup, no links, no tooltips.
//!
//! This is deliberately *inline* markdown only. Block structure is already
//! handled by `store::blocks`; what reaches here is one entry's text.

use std::sync::LazyLock;

use regex::Regex;

use crate::gitlab::refs::{self, Reference};

/// What a GitLab reference resolved to, as far as rendering cares.
#[derive(Debug, Clone, Default)]
pub struct RefTarget {
    pub url: Option<String>,
    pub tooltip: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Emphasis {
    Bold,
    Italic,
    Strike,
}

#[derive(Debug, Clone)]
enum Node {
    Code(String),
    /// `[label](url)`
    Link { label: String, url: String },
    /// A bare URL in the text.
    AutoLink(String),
    Ref { raw: String, target: RefTarget },
    Styled { style: Emphasis, inner: String },
}

/// Priority when two candidate spans overlap: lower wins.
fn rank(node: &Node) -> u8 {
    match node {
        Node::Code(_) => 0,
        Node::Link { .. } => 1,
        Node::AutoLink(_) => 2,
        Node::Ref { .. } => 3,
        Node::Styled { .. } => 4,
    }
}

static CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`([^`]+)`").expect("static regex"));
static LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]*)\]\(([^)\s]+)\)").expect("static regex"));
static AUTOLINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bhttps?://[^\s<>()\[\]]+").expect("static regex"));
static BOLD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\*\*([^*]+)\*\*|__([^_]+)__").expect("static regex"));
static ITALIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\*([^*\s][^*]*)\*|(?:^|[\s(])_([^_\s][^_]*)_").expect("static regex"));
static STRIKE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"~~([^~]+)~~").expect("static regex"));

/// Collects every span worth rendering, resolving overlaps by rank then by
/// start position, so a URL inside a `[label](url)` never doubles up.
fn spans(text: &str, lookup: &dyn Fn(&Reference) -> Option<RefTarget>) -> Vec<(usize, usize, Node)> {
    let mut found: Vec<(usize, usize, Node)> = Vec::new();

    for c in CODE.captures_iter(text) {
        let whole = c.get(0).expect("group 0");
        found.push((whole.start(), whole.end(), Node::Code(c[1].to_string())));
    }
    for c in LINK.captures_iter(text) {
        let whole = c.get(0).expect("group 0");
        found.push((
            whole.start(),
            whole.end(),
            Node::Link {
                label: c[1].to_string(),
                url: c[2].to_string(),
            },
        ));
    }
    for m in AUTOLINK.find_iter(text) {
        found.push((m.start(), m.end(), Node::AutoLink(m.as_str().to_string())));
    }
    // refs::parse already masks fenced code, inline code spans and URLs, so a
    // `#section` in a link target is never mistaken for issue #0.
    for r in refs::parse(text) {
        let target = lookup(&r).unwrap_or_default();
        found.push((
            r.start,
            r.end,
            Node::Ref {
                raw: text[r.start..r.end].to_string(),
                target,
            },
        ));
    }
    for (re, style, delim) in [
        (&*BOLD, Emphasis::Bold, 2usize),
        (&*ITALIC, Emphasis::Italic, 1),
        (&*STRIKE, Emphasis::Strike, 2),
    ] {
        for c in re.captures_iter(text) {
            let Some(inner) = c.get(1).or_else(|| c.get(2)) else {
                continue;
            };
            // The italic pattern may consume a character before the opening
            // delimiter, so the span starts from the inner group, not from the
            // match. The delimiter length is fixed per style.
            let start = inner.start().saturating_sub(delim);
            found.push((
                start,
                c.get(0).expect("group 0").end(),
                Node::Styled {
                    style,
                    inner: inner.as_str().to_string(),
                },
            ));
        }
    }

    found.sort_by_key(|(start, end, node)| (*start, rank(node), std::cmp::Reverse(*end)));

    let mut out: Vec<(usize, usize, Node)> = Vec::new();
    let mut taken_to = 0usize;
    for (start, end, node) in found {
        if start < taken_to {
            continue;
        }
        taken_to = end;
        out.push((start, end, node));
    }
    out
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

fn link(url: &str, label: &str, tooltip: Option<&str>) -> String {
    match tooltip {
        Some(t) => format!(
            r#"<a href="{}" title="{}">{}</a>"#,
            escape(url),
            escape(t),
            escape(label)
        ),
        None => format!(r#"<a href="{}">{}</a>"#, escape(url), escape(label)),
    }
}

/// Renders to Pango markup for a `GtkLabel`.
pub fn to_pango(text: &str, lookup: &dyn Fn(&Reference) -> Option<RefTarget>) -> String {
    let mut out = String::with_capacity(text.len() + 32);
    let mut at = 0usize;

    for (start, end, node) in spans(text, lookup) {
        out.push_str(&escape(&text[at..start]));
        match node {
            Node::Code(inner) => {
                out.push_str("<tt>");
                out.push_str(&escape(&inner));
                out.push_str("</tt>");
            }
            Node::Link { label, url } => out.push_str(&link(&url, &label, None)),
            Node::AutoLink(url) => out.push_str(&link(&url, &url, None)),
            Node::Ref { raw, target } => match target.url {
                Some(url) => out.push_str(&link(&url, &raw, target.tooltip.as_deref())),
                // Unresolved: still worth a tooltip saying so, but not a link
                // that would go nowhere.
                None => out.push_str(&escape(&raw)),
            },
            Node::Styled { style, inner } => {
                let (open, close) = match style {
                    Emphasis::Bold => ("<b>", "</b>"),
                    Emphasis::Italic => ("<i>", "</i>"),
                    Emphasis::Strike => ("<s>", "</s>"),
                };
                out.push_str(open);
                out.push_str(&escape(&inner));
                out.push_str(close);
            }
        }
        at = end;
    }
    out.push_str(&escape(&text[at..]));
    out
}

/// Renders to flat text, for surfaces that cannot show markup at all — the
/// tray menu, whose labels are plain strings in the dbusmenu protocol.
pub fn to_plain(text: &str) -> String {
    let none = |_: &Reference| None;
    let mut out = String::with_capacity(text.len());
    let mut at = 0usize;

    for (start, end, node) in spans(text, &none) {
        out.push_str(&text[at..start]);
        match node {
            Node::Code(inner) => out.push_str(&inner),
            // The label is what a person wrote to describe the link; the URL
            // is noise in a one-line menu item.
            Node::Link { label, .. } => out.push_str(&label),
            Node::AutoLink(url) => out.push_str(&url),
            Node::Ref { raw, .. } => out.push_str(&raw),
            Node::Styled { inner, .. } => out.push_str(&inner),
        }
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(s: &str) -> String {
        to_plain(s)
    }

    fn pango(s: &str) -> String {
        to_pango(s, &|_| None)
    }

    /// Pretends every reference resolved, so link and tooltip output can be
    /// checked without a database.
    fn resolved(s: &str) -> String {
        to_pango(s, &|r| {
            Some(RefTarget {
                url: Some(format!("https://gl/x/-/issues/{}", r.iid.unwrap_or(0))),
                tooltip: Some("Fix LDAP login loop · open".into()),
            })
        })
    }

    #[test]
    fn a_markdown_link_shows_its_label() {
        // The real case that prompted this: a long link swamping a tray label.
        let s = "PMS-Review [MR zu mehr Eigenschaften](https://example.com/pull/122)";
        assert_eq!(plain(s), "PMS-Review MR zu mehr Eigenschaften");
        assert_eq!(
            pango(s),
            r#"PMS-Review <a href="https://example.com/pull/122">MR zu mehr Eigenschaften</a>"#
        );
    }

    #[test]
    fn a_bare_url_becomes_a_link() {
        assert_eq!(
            pango("see https://example.com/x for details"),
            r#"see <a href="https://example.com/x">https://example.com/x</a> for details"#
        );
        assert_eq!(plain("see https://example.com/x"), "see https://example.com/x");
    }

    #[test]
    fn a_url_inside_a_link_is_not_doubled_up() {
        // Both the link rule and the autolink rule match here; the link must win.
        let out = pango("[label](https://example.com)");
        assert_eq!(out, r#"<a href="https://example.com">label</a>"#);
        assert_eq!(out.matches("<a ").count(), 1);
    }

    #[test]
    fn emphasis_and_code_render() {
        assert_eq!(pango("**bold** and *italic*"), "<b>bold</b> and <i>italic</i>");
        assert_eq!(pango("__bold__"), "<b>bold</b>");
        assert_eq!(pango("~~gone~~"), "<s>gone</s>");
        assert_eq!(pango("run `ls -la` now"), "run <tt>ls -la</tt> now");
        assert_eq!(plain("**bold** and `code`"), "bold and code");
    }

    #[test]
    fn a_resolved_reference_becomes_a_link_with_a_hover_card() {
        assert_eq!(
            resolved("fixed #142"),
            r#"fixed <a href="https://gl/x/-/issues/142" title="Fix LDAP login loop · open">#142</a>"#
        );
    }

    #[test]
    fn an_unresolved_reference_is_text_not_a_dead_link() {
        assert_eq!(pango("fixed #142"), "fixed #142");
    }

    #[test]
    fn a_reference_inside_a_code_span_stays_literal() {
        // refs::parse masks code spans, and the code rule outranks the rest.
        assert_eq!(resolved("use `#142` literally"), "use <tt>#142</tt> literally");
    }

    #[test]
    fn markup_special_characters_are_escaped() {
        // Ampersands are what broke the Settings subtitle; a note saying
        // "a & b" must not produce invalid markup.
        assert_eq!(pango("a & b < c > d"), "a &amp; b &lt; c &gt; d");
        assert_eq!(plain("a & b"), "a & b");
        // ...including inside a link label and a URL
        assert_eq!(
            pango("[a & b](https://x/?q=1&r=2)"),
            r#"<a href="https://x/?q=1&amp;r=2">a &amp; b</a>"#
        );
    }

    #[test]
    fn epic_and_mr_references_resolve_too() {
        let out = resolved("reviewed !87 for &7");
        assert_eq!(out.matches("<a ").count(), 2);
        assert!(out.contains(">!87</a>"));
        assert!(out.contains(">&amp;7</a>"));
    }

    #[test]
    fn multibyte_text_survives_intact() {
        assert_eq!(plain("Änderung für **größer**"), "Änderung für größer");
        assert_eq!(pango("Grüße & *Küsse*"), "Grüße &amp; <i>Küsse</i>");
    }

    #[test]
    fn plain_text_passes_through_unchanged() {
        assert_eq!(plain("just a note"), "just a note");
        assert_eq!(pango("just a note"), "just a note");
        assert_eq!(plain(""), "");
    }

    #[test]
    fn underscores_inside_words_are_not_emphasis() {
        // snake_case identifiers are common in these notes.
        assert_eq!(pango("read_api scope"), "read_api scope");
        assert_eq!(plain("some_var_name"), "some_var_name");
    }

    #[test]
    fn output_is_valid_pango_markup() {
        // The real contract: whatever we emit must parse, or GtkLabel shows
        // nothing at all — the failure mode the Settings subtitle hit.
        for s in [
            "a & b",
            "**bold** `code` [l](https://x/?a=1&b=2) #142 https://y.z/?q=<>",
            "unclosed **bold",
            "~~strike~~ & <tag>",
            "Grüße & *Küsse* @a.user",
        ] {
            let markup = resolved(s);
            assert!(
                renders_in_a_label(&markup),
                "invalid markup for {s:?}: {markup}"
            );
            assert!(
                entities_are_closed(&markup),
                "unescaped ampersand for {s:?}: {markup}"
            );
        }
    }

    /// Validates the way `GtkLabel` actually consumes the string.
    ///
    /// `<a>` is a GtkLabel extension, not Pango markup — GtkLabel pulls the
    /// links out itself and hands the remainder to Pango, which rejects the
    /// tag outright ("Unknown tag 'a'"). So the link tags come out before the
    /// check, or this would fail on perfectly good markup.
    fn renders_in_a_label(markup: &str) -> bool {
        static A_TAG: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"</?a\b[^>]*>").expect("static regex"));
        let without_links = A_TAG.replace_all(markup, "");
        gtk::pango::parse_markup(&without_links, '\u{0}').is_ok()
    }

    /// Every `&` must begin a real entity — the exact mistake that made the
    /// Settings subtitle vanish silently.
    fn entities_are_closed(markup: &str) -> bool {
        static ENTITY: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"&(amp|lt|gt|quot|apos|#\d+|#x[0-9a-fA-F]+);").expect("static regex")
        });
        let stripped = ENTITY.replace_all(markup, "");
        !stripped.contains('&')
    }
}
