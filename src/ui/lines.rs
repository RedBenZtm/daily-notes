//! Moving lines up and down in the editor.
//!
//! The text transformation is kept separate from the buffer so it can be
//! tested without a display: given the slice of lines that participate in the
//! move, rotating them by one is the whole operation.

use gtk::glib;

/// Rotates a region of text by one line.
///
/// For a downward move the region is `[block…, next]` and becomes
/// `[next, block…]`; for an upward move it is `[prev, block…]` and becomes
/// `[block…, prev]`. Whether the region ended in a newline is preserved, which
/// is what keeps the last line of the buffer from growing or losing one.
pub fn rotate_region(text: &str, down: bool) -> String {
    let had_trailing_newline = text.ends_with('\n');
    let mut lines: Vec<&str> = text.split('\n').collect();
    if had_trailing_newline {
        lines.pop();
    }
    if lines.len() < 2 {
        return text.to_string();
    }

    if down {
        let last = lines.pop().expect("checked len >= 2");
        lines.insert(0, last);
    } else {
        let first = lines.remove(0);
        lines.push(first);
    }

    let mut out = lines.join("\n");
    if had_trailing_newline {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_a_single_line_down() {
        assert_eq!(rotate_region("a\nb\n", true), "b\na\n");
    }

    #[test]
    fn moves_a_single_line_up() {
        assert_eq!(rotate_region("a\nb\n", false), "b\na\n");
    }

    #[test]
    fn moves_a_block_of_lines_down() {
        // block = a,b ; next = c
        assert_eq!(rotate_region("a\nb\nc\n", true), "c\na\nb\n");
    }

    #[test]
    fn moves_a_block_of_lines_up() {
        // prev = a ; block = b,c
        assert_eq!(rotate_region("a\nb\nc\n", false), "b\nc\na\n");
    }

    #[test]
    fn a_region_without_a_trailing_newline_keeps_it_that_way() {
        // Happens at the very end of the buffer; adding a newline here would
        // silently grow the document every time you move the last line.
        assert_eq!(rotate_region("a\nb", true), "b\na");
        assert_eq!(rotate_region("a\nb", false), "b\na");
    }

    #[test]
    fn blank_lines_are_ordinary_lines() {
        assert_eq!(rotate_region("a\n\n", true), "\na\n");
    }

    #[test]
    fn a_single_line_region_is_unchanged() {
        assert_eq!(rotate_region("a\n", true), "a\n");
        assert_eq!(rotate_region("a", false), "a");
    }

    #[test]
    fn round_trips() {
        let original = "one\ntwo\nthree\n";
        assert_eq!(rotate_region(&rotate_region(original, true), false), original);
    }
}

/// Default accelerators. The Settings page (step 7) will make these
/// configurable; they are named here so there is one place to change.
pub const MOVE_UP: &str = "<Alt><Shift>Up";
pub const MOVE_DOWN: &str = "<Alt><Shift>Down";

/// Moves the line under the cursor, or every line the selection touches, by
/// one position. Returns false when the move would run off the end.
pub fn move_in_buffer(buffer: &sourceview5::Buffer, down: bool) -> bool {
    use gtk::prelude::*;

    let (sel_start, sel_end) = match buffer.selection_bounds() {
        Some(bounds) => bounds,
        None => {
            let iter = buffer.iter_at_mark(&buffer.get_insert());
            (iter, iter)
        }
    };

    let first = sel_start.line();
    let mut last = sel_end.line();
    // A selection that ends exactly at the start of a line does not really
    // include that line — dragging it along would be surprising.
    if last > first && sel_end.line_offset() == 0 {
        last -= 1;
    }

    let last_line = buffer.line_count() - 1;
    let (region_first, region_last) = if down {
        if last >= last_line {
            return false;
        }
        (first, last + 1)
    } else {
        if first == 0 {
            return false;
        }
        (first - 1, last)
    };

    let Some(start) = buffer.iter_at_line(region_first) else {
        return false;
    };
    let end = if region_last < last_line {
        match buffer.iter_at_line(region_last + 1) {
            Some(i) => i,
            None => buffer.end_iter(),
        }
    } else {
        buffer.end_iter()
    };

    let region = buffer.text(&start, &end, true).to_string();
    let rotated = rotate_region(&region, down);
    if rotated == region {
        return false;
    }

    // Where the cursor and selection end up, in line/offset terms.
    let delta = if down { 1 } else { -1 };
    let new_first = first + delta;
    let new_last = last + delta;
    let sel_start_offset = sel_start.line_offset();
    let sel_end_offset = sel_end.line_offset();
    let had_selection = buffer.selection_bounds().is_some();

    // One undo step, not two.
    buffer.begin_user_action();
    let mut s = start;
    let mut e = end;
    buffer.delete(&mut s, &mut e);
    buffer.insert(&mut s, &rotated);
    buffer.end_user_action();

    // Restoring the cursor column is what makes repeated presses feel like
    // the line is being dragged rather than retyped.
    let place = |line: i32, offset: i32| -> Option<gtk::TextIter> {
        let mut iter = buffer.iter_at_line(line)?;
        let max = {
            let mut probe = iter;
            probe.forward_to_line_end();
            probe.line_offset()
        };
        iter.set_line_offset(offset.min(max));
        Some(iter)
    };

    if had_selection {
        if let (Some(a), Some(b)) = (
            place(new_first, sel_start_offset),
            place(new_last, sel_end_offset),
        ) {
            buffer.select_range(&a, &b);
        }
    } else if let Some(a) = place(new_first, sel_start_offset) {
        buffer.place_cursor(&a);
    }

    true
}

/// Installs the accelerators on a view.
pub fn install(view: &sourceview5::View, buffer: &sourceview5::Buffer) {
    use gtk::prelude::*;

    let controller = gtk::ShortcutController::new();
    controller.set_scope(gtk::ShortcutScope::Local);

    for (accel, down) in [(MOVE_UP, false), (MOVE_DOWN, true)] {
        let buffer = buffer.clone();
        let action = gtk::CallbackAction::new(move |widget, _| {
            let moved = move_in_buffer(&buffer, down);
            if moved {
                // Keep the moved line on screen when it runs past the edge.
                if let Some(view) = widget.downcast_ref::<sourceview5::View>() {
                    view.scroll_mark_onscreen(&buffer.get_insert());
                }
            }
            glib::Propagation::Stop
        });
        let Some(trigger) = gtk::ShortcutTrigger::parse_string(accel) else {
            tracing::warn!(accel, "could not parse accelerator");
            continue;
        };
        controller.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(action)));
    }

    view.add_controller(controller);
}

/// Buffer-level tests. These need a GTK display, so they live behind one
/// entry point and are skipped when there is none (e.g. a headless CI run).
#[cfg(test)]
mod buffer_tests {
    use super::*;
    use gtk::prelude::*;

    fn buffer_with(text: &str) -> sourceview5::Buffer {
        let b = sourceview5::Buffer::new(None);
        b.set_text(text);
        b
    }

    fn text_of(b: &sourceview5::Buffer) -> String {
        let (s, e) = b.bounds();
        b.text(&s, &e, true).to_string()
    }

    fn put_cursor(b: &sourceview5::Buffer, line: i32, offset: i32) {
        let mut i = b.iter_at_line(line).unwrap();
        i.set_line_offset(offset);
        b.place_cursor(&i);
    }

    #[test]
    fn move_lines_in_a_real_buffer() {
        if gtk::init().is_err() {
            eprintln!("no display; skipping buffer tests");
            return;
        }

        // cursor on line 1, move down
        let b = buffer_with("alpha\nbeta\ngamma\n");
        put_cursor(&b, 1, 2);
        assert!(move_in_buffer(&b, true));
        assert_eq!(text_of(&b), "alpha\ngamma\nbeta\n");
        // the cursor followed the line, keeping its column
        let cur = b.iter_at_mark(&b.get_insert());
        assert_eq!((cur.line(), cur.line_offset()), (2, 2));

        // and back up again
        assert!(move_in_buffer(&b, false));
        assert_eq!(text_of(&b), "alpha\nbeta\ngamma\n");

        // at the top edge, moving up does nothing
        let b = buffer_with("alpha\nbeta\n");
        put_cursor(&b, 0, 0);
        assert!(!move_in_buffer(&b, false));
        assert_eq!(text_of(&b), "alpha\nbeta\n");

        // at the bottom edge, moving down does nothing
        let b = buffer_with("alpha\nbeta");
        put_cursor(&b, 1, 0);
        assert!(!move_in_buffer(&b, true));
        assert_eq!(text_of(&b), "alpha\nbeta");

        // the final line without a trailing newline moves without gaining one
        let b = buffer_with("alpha\nbeta");
        put_cursor(&b, 1, 0);
        assert!(move_in_buffer(&b, false));
        assert_eq!(text_of(&b), "beta\nalpha");

        // a multi-line selection moves as a block and stays selected
        let b = buffer_with("a\nb\nc\nd\n");
        let start = b.iter_at_line(0).unwrap();
        let end = b.iter_at_line(1).unwrap();
        b.select_range(&start, &end); // covers line 0, ends at start of line 1
        assert!(move_in_buffer(&b, true));
        assert_eq!(text_of(&b), "b\na\nc\nd\n");

        // a selection spanning two whole lines
        let b = buffer_with("a\nb\nc\nd\n");
        let start = b.iter_at_line(0).unwrap();
        let mut end = b.iter_at_line(1).unwrap();
        end.forward_to_line_end();
        b.select_range(&start, &end);
        assert!(move_in_buffer(&b, true));
        assert_eq!(text_of(&b), "c\na\nb\nd\n");
        let (s, e) = b.selection_bounds().expect("selection survives the move");
        assert_eq!((s.line(), e.line()), (1, 2));

        // an undo puts it all back in one step
        let b = buffer_with("a\nb\nc\n");
        put_cursor(&b, 0, 0);
        assert!(move_in_buffer(&b, true));
        assert_eq!(text_of(&b), "b\na\nc\n");
        b.undo();
        assert_eq!(text_of(&b), "a\nb\nc\n");
    }
}
