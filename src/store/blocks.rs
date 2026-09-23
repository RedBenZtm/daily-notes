//! Splits a day's markdown into the blocks that search results point at.
//!
//! `days.body` stays the single source of truth; this is a derived projection,
//! rebuilt for one day at a time on each save. Line numbers are **0-based** to
//! match `gtk::TextBuffer`, so a search hit can be turned into a cursor
//! position without any conversion.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Heading,
    /// A list item, including any indented continuation lines and nested items.
    Bullet,
    Para,
    /// A fenced code block, kept whole so its contents never masquerade as
    /// bullets and never pollute the search index with syntax.
    Code,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Heading => "heading",
            Kind::Bullet => "bullet",
            Kind::Para => "para",
            Kind::Code => "code",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub ord: usize,
    /// 0-based, inclusive.
    pub line_start: usize,
    /// 0-based, inclusive.
    pub line_end: usize,
    pub kind: Kind,
    /// The block's text with list/heading markers stripped and continuation
    /// lines joined, i.e. what a human would read. This is what gets indexed.
    pub text: String,
}

/// True for `- `, `* `, `+ `, `1. ` and `1) ` at the given indent.
fn bullet_marker_len(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();

    let mut chars = trimmed.char_indices();
    match chars.next() {
        Some((_, '-' | '*' | '+')) => {
            // A marker needs whitespace after it, otherwise `-x` or a `---`
            // rule would parse as a list item.
            if trimmed[1..].starts_with([' ', '\t']) {
                return Some(indent + 2);
            }
            None
        }
        Some((_, c)) if c.is_ascii_digit() => {
            for (i, c) in chars {
                if c.is_ascii_digit() {
                    // keep scanning
                } else if c == '.' || c == ')' {
                    if trimmed[i + 1..].starts_with([' ', '\t']) {
                        return Some(indent + i + 2);
                    }
                    return None;
                } else {
                    return None;
                }
                // A "number" longer than this is not a list marker.
                if i > 9 {
                    return None;
                }
            }
            None
        }
        _ => None,
    }
}

fn fence_of(line: &str) -> Option<&str> {
    let t = line.trim_start();
    for marker in ["```", "~~~"] {
        if t.starts_with(marker) {
            return Some(marker);
        }
    }
    None
}

fn heading_len(line: &str) -> Option<usize> {
    if !line.starts_with('#') {
        return None;
    }
    let hashes = line.chars().take_while(|&c| c == '#').count();
    // A heading is `#` + space. `#123` has no space, which is exactly what
    // makes GitLab issue references safe to write at the start of a line.
    if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
        Some(hashes + 1)
    } else {
        None
    }
}

pub fn parse(body: &str) -> Vec<Block> {
    let lines: Vec<&str> = body.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        if line.trim().is_empty() {
            i += 1;
            continue;
        }

        // Fenced code first: inside a fence nothing else is structure.
        if let Some(marker) = fence_of(line) {
            let start = i;
            i += 1;
            while i < lines.len() && fence_of(lines[i]) != Some(marker) {
                i += 1;
            }
            let end = i.min(lines.len() - 1);
            i = (i + 1).min(lines.len());
            push(&mut blocks, start, end, Kind::Code, lines[start..=end].join("\n"));
            continue;
        }

        if let Some(skip) = heading_len(line) {
            push(&mut blocks, i, i, Kind::Heading, line[skip..].trim().to_string());
            i += 1;
            continue;
        }

        if let Some(skip) = bullet_marker_len(line) {
            let start = i;
            let indent = line.len() - line.trim_start().len();
            let mut text = vec![line[skip..].trim().to_string()];
            i += 1;
            // Continuation lines and nested items belong to this entry: they
            // are indented further than the marker and are not blank.
            while i < lines.len() {
                let next = lines[i];
                if next.trim().is_empty() {
                    break;
                }
                let next_indent = next.len() - next.trim_start().len();
                let is_sibling = bullet_marker_len(next).is_some() && next_indent <= indent;
                if is_sibling || (next_indent <= indent && bullet_marker_len(next).is_none() && next_indent == 0) {
                    break;
                }
                let stripped = match bullet_marker_len(next) {
                    Some(s) => next[s..].trim(),
                    None => next.trim(),
                };
                text.push(stripped.to_string());
                i += 1;
            }
            push(&mut blocks, start, i - 1, Kind::Bullet, text.join(" "));
            continue;
        }

        // Plain paragraph: runs to the next blank line or structural line.
        let start = i;
        let mut text = vec![line.trim().to_string()];
        i += 1;
        while i < lines.len() {
            let next = lines[i];
            if next.trim().is_empty()
                || heading_len(next).is_some()
                || bullet_marker_len(next).is_some()
                || fence_of(next).is_some()
            {
                break;
            }
            text.push(next.trim().to_string());
            i += 1;
        }
        push(&mut blocks, start, i - 1, Kind::Para, text.join(" "));
    }

    blocks
}

fn push(blocks: &mut Vec<Block>, line_start: usize, line_end: usize, kind: Kind, text: String) {
    if text.trim().is_empty() && kind != Kind::Code {
        return;
    }
    blocks.push(Block {
        ord: blocks.len(),
        line_start,
        line_end,
        kind,
        text,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(body: &str) -> Vec<(Kind, usize, usize, String)> {
        parse(body)
            .into_iter()
            .map(|b| (b.kind, b.line_start, b.line_end, b.text))
            .collect()
    }

    #[test]
    fn heading_and_bullets() {
        let got = kinds("## Today\n- reviewed !87\n- fixed #142\n");
        assert_eq!(
            got,
            vec![
                (Kind::Heading, 0, 0, "Today".into()),
                (Kind::Bullet, 1, 1, "reviewed !87".into()),
                (Kind::Bullet, 2, 2, "fixed #142".into()),
            ]
        );
    }

    #[test]
    fn issue_ref_at_line_start_is_not_a_heading() {
        // The whole reason headings require `# ` — `#142` must stay a reference.
        let got = kinds("#142 is still broken\n");
        assert_eq!(got, vec![(Kind::Para, 0, 0, "#142 is still broken".into())]);
    }

    #[test]
    fn multiline_bullet_is_one_block() {
        let got = kinds("- pairing on api/gateway#12\n  and it went well\n- next thing\n");
        assert_eq!(
            got,
            vec![
                (Kind::Bullet, 0, 1, "pairing on api/gateway#12 and it went well".into()),
                (Kind::Bullet, 2, 2, "next thing".into()),
            ]
        );
    }

    #[test]
    fn nested_items_stay_with_their_parent() {
        let got = kinds("- deploy\n  - staging\n  - prod\n- unrelated\n");
        assert_eq!(
            got,
            vec![
                (Kind::Bullet, 0, 2, "deploy staging prod".into()),
                (Kind::Bullet, 3, 3, "unrelated".into()),
            ]
        );
    }

    #[test]
    fn fenced_code_is_opaque() {
        let body = "- before\n\n```sh\n- not a bullet\n# not a heading\n```\n\n- after\n";
        let got = kinds(body);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].0, Kind::Bullet);
        assert_eq!(got[1].0, Kind::Code);
        assert_eq!((got[1].1, got[1].2), (2, 5));
        assert_eq!(got[2], (Kind::Bullet, 7, 7, "after".into()));
    }

    #[test]
    fn horizontal_rule_is_not_a_bullet() {
        let got = kinds("---\n");
        assert_eq!(got, vec![(Kind::Para, 0, 0, "---".into())]);
    }

    #[test]
    fn ordered_lists_count_as_bullets() {
        let got = kinds("1. first\n2) second\n");
        assert_eq!(
            got,
            vec![
                (Kind::Bullet, 0, 0, "first".into()),
                (Kind::Bullet, 1, 1, "second".into()),
            ]
        );
    }

    #[test]
    fn blank_lines_do_not_create_blocks() {
        assert!(parse("\n\n   \n").is_empty());
        assert!(parse("").is_empty());
    }

    #[test]
    fn ord_is_dense_and_ascending() {
        let b = parse("# A\n\n- x\n\npara\n");
        assert_eq!(b.iter().map(|b| b.ord).collect::<Vec<_>>(), vec![0, 1, 2]);
    }
}

/// Rewrites the lines a block occupies, keeping the original list marker.
///
/// Quick Note edits one entry at a time; the rest of the day — including
/// whatever indentation and marker style was used — must survive untouched.
/// A block that spanned continuation lines collapses to a single line, which
/// is what the user sees and edits in the compact view.
pub fn replace_block(body: &str, block: &Block, new_text: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    if block.line_start >= lines.len() {
        return body.to_string();
    }
    let end = block.line_end.min(lines.len() - 1);

    let first = lines[block.line_start];
    let prefix = match block.kind {
        // Reuse the exact marker, e.g. "  - ", "* ", "1. ".
        Kind::Bullet => bullet_marker_len(first)
            .map(|n| &first[..n])
            .unwrap_or("- "),
        Kind::Heading => heading_len(first).map(|n| &first[..n]).unwrap_or("# "),
        _ => "",
    };

    let mut out: Vec<String> = lines[..block.line_start].iter().map(|s| s.to_string()).collect();
    out.push(format!("{prefix}{new_text}"));
    out.extend(lines[end + 1..].iter().map(|s| s.to_string()));

    let mut joined = out.join("\n");
    if body.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

/// Removes the lines a block occupies.
pub fn remove_block(body: &str, block: &Block) -> String {
    let lines: Vec<&str> = body.lines().collect();
    if block.line_start >= lines.len() {
        return body.to_string();
    }
    let end = block.line_end.min(lines.len() - 1);
    let mut out: Vec<String> = lines[..block.line_start].iter().map(|s| s.to_string()).collect();
    out.extend(lines[end + 1..].iter().map(|s| s.to_string()));
    let mut joined = out.join("\n");
    if body.ends_with('\n') && !joined.is_empty() {
        joined.push('\n');
    }
    joined
}

/// Appends a new list item, matching the marker style already in use.
pub fn append_entry(body: &str, text: &str) -> String {
    // Follow whatever marker the day already uses rather than imposing "-".
    let marker = parse(body)
        .iter()
        .rev()
        .find(|b| b.kind == Kind::Bullet)
        .and_then(|b| {
            let lines: Vec<&str> = body.lines().collect();
            lines
                .get(b.line_start)
                .and_then(|l| bullet_marker_len(l).map(|n| l[..n].to_string()))
        })
        // An ordered list would need renumbering, so only reuse simple markers.
        .filter(|m| m.trim_start().starts_with(['-', '*', '+']))
        .unwrap_or_else(|| "- ".to_string());

    let mut out = body.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&marker);
    out.push_str(text);
    out.push('\n');
    out
}

#[cfg(test)]
mod edit_tests {
    use super::*;

    fn block_at(body: &str, ord: usize) -> Block {
        parse(body).into_iter().nth(ord).unwrap()
    }

    #[test]
    fn replace_keeps_the_original_marker_and_indent() {
        let body = "  * first\n  * second\n";
        let b = block_at(body, 1);
        assert_eq!(replace_block(body, &b, "changed"), "  * first\n  * changed\n");
    }

    #[test]
    fn replace_collapses_a_multiline_entry() {
        let body = "- one\n  continued\n- two\n";
        let b = block_at(body, 0);
        assert_eq!(replace_block(body, &b, "one, edited"), "- one, edited\n- two\n");
    }

    #[test]
    fn replace_leaves_surrounding_structure_alone() {
        let body = "## Heading\n\n- a\n- b\n\nsome prose\n";
        let b = block_at(body, 2); // the "- b" bullet
        assert_eq!(
            replace_block(body, &b, "b!"),
            "## Heading\n\n- a\n- b!\n\nsome prose\n"
        );
    }

    #[test]
    fn remove_drops_exactly_the_block() {
        let body = "- a\n- b\n- c\n";
        let b = block_at(body, 1);
        assert_eq!(remove_block(body, &b), "- a\n- c\n");
    }

    #[test]
    fn append_follows_the_existing_marker_style() {
        assert_eq!(append_entry("* a\n", "b"), "* a\n* b\n");
        assert_eq!(append_entry("- a\n", "b"), "- a\n- b\n");
        // ordered lists would need renumbering, so fall back to a dash
        assert_eq!(append_entry("1. a\n", "b"), "1. a\n- b\n");
        assert_eq!(append_entry("", "first"), "- first\n");
        // no trailing newline in the source must not glue the lines together
        assert_eq!(append_entry("- a", "b"), "- a\n- b\n");
    }

    #[test]
    fn append_then_parse_round_trips() {
        let body = append_entry(&append_entry("", "one"), "two");
        let texts: Vec<_> = parse(&body).into_iter().map(|b| b.text).collect();
        assert_eq!(texts, vec!["one", "two"]);
    }
}

/// Moves the block at `from` to sit at position `to` in the block order.
///
/// Reordering happens at block granularity, so a multi-line entry travels
/// with its continuation lines. Blank lines stay where they are rather than
/// travelling with a block — they are spacing between entries, not part of
/// one, and dragging an entry around should not reflow the whole day.
pub fn move_block(body: &str, from: usize, to: usize) -> String {
    let blocks = parse(body);
    if from >= blocks.len() || to >= blocks.len() || from == to {
        return body.to_string();
    }
    let lines: Vec<&str> = body.lines().collect();
    let src = &blocks[from];
    let dst = &blocks[to];

    let src_end = src.line_end.min(lines.len().saturating_sub(1));
    let moved: Vec<&str> = lines[src.line_start..=src_end].to_vec();
    let moved_len = moved.len();

    // Insert before the target when moving up, after it when moving down —
    // which is what dropping an item onto another one means in a list.
    let anchor = if to < from {
        dst.line_start
    } else {
        dst.line_end.min(lines.len().saturating_sub(1)) + 1
    };

    let mut rest: Vec<&str> = Vec::with_capacity(lines.len());
    rest.extend_from_slice(&lines[..src.line_start]);
    rest.extend_from_slice(&lines[src_end + 1..]);

    // The anchor was computed against the original line numbering; removing
    // the source first shifts everything after it up.
    let insert_at = if anchor > src.line_start {
        anchor - moved_len
    } else {
        anchor
    }
    .min(rest.len());

    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    out.extend_from_slice(&rest[..insert_at]);
    out.extend_from_slice(&moved);
    out.extend_from_slice(&rest[insert_at..]);

    let mut joined = out.join("\n");
    if body.ends_with('\n') && !joined.is_empty() {
        joined.push('\n');
    }
    joined
}

#[cfg(test)]
mod move_tests {
    use super::*;

    fn texts(body: &str) -> Vec<String> {
        parse(body).into_iter().map(|b| b.text).collect()
    }

    #[test]
    fn moves_an_entry_up() {
        let body = "- a\n- b\n- c\n";
        assert_eq!(move_block(body, 2, 0), "- c\n- a\n- b\n");
        assert_eq!(texts(&move_block(body, 2, 0)), vec!["c", "a", "b"]);
    }

    #[test]
    fn moves_an_entry_down() {
        let body = "- a\n- b\n- c\n";
        assert_eq!(move_block(body, 0, 2), "- b\n- c\n- a\n");
    }

    #[test]
    fn moves_by_one_in_both_directions() {
        let body = "- a\n- b\n- c\n";
        assert_eq!(move_block(body, 0, 1), "- b\n- a\n- c\n");
        assert_eq!(move_block(body, 1, 0), "- b\n- a\n- c\n");
    }

    #[test]
    fn a_multiline_entry_travels_whole() {
        let body = "- a\n  still a\n- b\n";
        assert_eq!(move_block(body, 0, 1), "- b\n- a\n  still a\n");
        assert_eq!(texts(&move_block(body, 0, 1)), vec!["b", "a still a"]);
    }

    #[test]
    fn blank_lines_stay_as_spacing() {
        // The blank line separates the two entries; it must not follow one of
        // them around and collapse the layout.
        let body = "- a\n\n- b\n";
        assert_eq!(move_block(body, 1, 0), "- b\n- a\n\n");
    }

    #[test]
    fn reordering_across_a_heading_keeps_the_heading_put() {
        let body = "## Today\n- a\n- b\n";
        assert_eq!(move_block(body, 2, 1), "## Today\n- b\n- a\n");
    }

    #[test]
    fn out_of_range_and_self_moves_are_no_ops() {
        let body = "- a\n- b\n";
        assert_eq!(move_block(body, 0, 0), body);
        assert_eq!(move_block(body, 5, 0), body);
        assert_eq!(move_block(body, 0, 9), body);
    }

    #[test]
    fn round_trips_back_to_the_original_order() {
        let body = "- a\n- b\n- c\n";
        let once = move_block(body, 0, 2);
        assert_eq!(move_block(&once, 2, 0), body);
    }
}
