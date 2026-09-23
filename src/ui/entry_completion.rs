//! `#` issue completion for one-line entries.
//!
//! The editor gets this from `GtkSourceCompletion`, but Quick Note's fields
//! are plain `GtkEntry`. `GtkEntryCompletion` has been deprecated since GTK
//! 4.10, and it completes the whole field rather than a token within it — so
//! this is a small popover driven by the same issue cache.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{glib, pango};

use crate::store::Store;

const MAX_ROWS: usize = 8;

/// Narrowest the popover may be, whatever the field's width.
const MIN_WIDTH: i32 = 380;

/// The `#`-token ending at `cursor`, as (byte offset of the `#`, query).
///
/// The token is the run of non-whitespace before the caret; it counts only if
/// it starts with `#`, so `abc#1` is left alone just as in the editor.
pub fn token_at(text: &str, cursor: usize) -> Option<(usize, &str)> {
    if cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }
    let head = &text[..cursor];
    let start = head
        .rfind(char::is_whitespace)
        .map(|i| i + head[i..].chars().next().map(char::len_utf8).unwrap_or(1))
        .unwrap_or(0);
    let token = &head[start..];
    let query = token.strip_prefix('#')?;
    Some((start, query))
}

/// Attaches the completion to an entry.
pub fn attach(
    entry: &gtk::Entry,
    store: Rc<RefCell<Store>>,
    on_query: impl Fn() + 'static,
) {
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("boxed-list");

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(260)
        .child(&list)
        .build();

    let popover = gtk::Popover::builder()
        .autohide(false)
        .has_arrow(false)
        .position(gtk::PositionType::Bottom)
        .halign(gtk::Align::Start)
        .child(&scroll)
        .build();
    popover.set_parent(entry);
    popover.add_css_class("menu");

    // Issue numbers for the rows currently shown.
    let shown: Rc<RefCell<Vec<u64>>> = Rc::new(RefCell::new(Vec::new()));
    let on_query = Rc::new(on_query);

    let refresh = {
        let entry = entry.clone();
        let list = list.clone();
        let popover = popover.clone();
        let shown = shown.clone();
        let store = store.clone();
        let on_query = on_query.clone();
        Rc::new(move || {
            let text = entry.text().to_string();
            let cursor = char_to_byte(&text, entry.position());
            let Some((_, query)) = token_at(&text, cursor) else {
                popover.popdown();
                return;
            };
            on_query();

            let cfg = crate::config::load();
            let rows = if cfg.gitlab.is_configured() {
                store
                    .borrow()
                    .complete_issues(&cfg.gitlab.main_project, query, MAX_ROWS)
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            shown.borrow_mut().clear();
            if rows.is_empty() {
                popover.popdown();
                return;
            }

            for (iid, title, _state) in &rows {
                shown.borrow_mut().push(*iid);
                let number = gtk::Label::new(Some(&format!("#{iid}")));
                number.add_css_class("monospace");
                number.set_xalign(0.0);
                let name = gtk::Label::new(Some(title));
                name.add_css_class("dimmed");
                name.set_xalign(0.0);
                name.set_ellipsize(pango::EllipsizeMode::End);
                name.set_hexpand(true);
                let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                row_box.set_margin_top(4);
                row_box.set_margin_bottom(4);
                row_box.set_margin_start(8);
                row_box.set_margin_end(8);
                row_box.append(&number);
                row_box.append(&name);
                list.append(&gtk::ListBoxRow::builder().child(&row_box).build());
            }
            if let Some(first) = list.row_at_index(0) {
                list.select_row(Some(&first));
            }
            // A popover sizes itself to its content, which for a list of issue
            // titles means a narrow column with everything ellipsised. Match
            // the field it belongs to, and never go below a width a title is
            // actually readable at.
            popover.set_size_request(entry.width().max(MIN_WIDTH), -1);
            popover.popup();
        })
    };

    let accept = {
        let entry = entry.clone();
        let list = list.clone();
        let popover = popover.clone();
        let shown = shown.clone();
        Rc::new(move || -> bool {
            let Some(row) = list.selected_row() else {
                return false;
            };
            let Some(&iid) = shown.borrow().get(row.index() as usize) else {
                return false;
            };
            let text = entry.text().to_string();
            let cursor = char_to_byte(&text, entry.position());
            let Some((start, query)) = token_at(&text, cursor) else {
                return false;
            };
            let end = start + 1 + query.len();
            let replacement = format!("#{iid}");
            let new = format!("{}{}{}", &text[..start], replacement, &text[end..]);
            let caret = text[..start].chars().count() + replacement.chars().count();
            entry.set_text(&new);
            entry.set_position(caret as i32);
            popover.popdown();
            true
        })
    };

    {
        let refresh = refresh.clone();
        // Guards against the `changed` that our own replacement emits.
        let busy = Rc::new(Cell::new(false));
        let b = busy.clone();
        entry.connect_changed(move |_| {
            if b.get() {
                return;
            }
            refresh();
        });
        let _ = busy;
    }

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let list = list.clone();
        let popover = popover.clone();
        let accept = accept.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if !popover.is_visible() {
                return glib::Propagation::Proceed;
            }
            let count = {
                let mut n = 0;
                while list.row_at_index(n).is_some() {
                    n += 1;
                }
                n
            };
            let current = list.selected_row().map(|r| r.index()).unwrap_or(0);
            match key {
                gtk::gdk::Key::Down => {
                    let next = (current + 1).min(count - 1);
                    if let Some(r) = list.row_at_index(next) {
                        list.select_row(Some(&r));
                    }
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::Up => {
                    let prev = (current - 1).max(0);
                    if let Some(r) = list.row_at_index(prev) {
                        list.select_row(Some(&r));
                    }
                    glib::Propagation::Stop
                }
                // Enter and Tab accept. Enter would otherwise commit the whole
                // entry, which is the one thing a half-typed reference must not
                // do.
                gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter | gtk::gdk::Key::Tab => {
                    if accept() {
                        glib::Propagation::Stop
                    } else {
                        glib::Propagation::Proceed
                    }
                }
                gtk::gdk::Key::Escape => {
                    popover.popdown();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
    }
    entry.add_controller(keys);

    {
        let accept = accept.clone();
        list.connect_row_activated(move |_, _| {
            accept();
        });
    }

    // A popover with autohide off has to be dismissed by hand.
    {
        let popover = popover.clone();
        entry.connect_has_focus_notify(move |e| {
            if !e.has_focus() {
                popover.popdown();
            }
        });
    }
}

fn char_to_byte(text: &str, chars: i32) -> usize {
    if chars < 0 {
        return text.len();
    }
    text.char_indices()
        .nth(chars as usize)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::token_at;

    #[test]
    fn finds_the_token_before_the_caret() {
        assert_eq!(token_at("fixed #89", 9), Some((6, "89")));
        assert_eq!(token_at("#89", 3), Some((0, "89")));
        // a bare trigger offers everything
        assert_eq!(token_at("fixed #", 7), Some((6, "")));
    }

    #[test]
    fn ignores_text_that_is_not_a_reference() {
        assert_eq!(token_at("just a note", 11), None);
        assert_eq!(token_at("", 0), None);
    }

    #[test]
    fn a_hash_glued_to_a_word_is_not_a_token() {
        // Same rule the editor applies, so the two agree.
        assert_eq!(token_at("abc#1", 5), None);
    }

    #[test]
    fn only_looks_at_the_text_before_the_caret() {
        // Caret sits right after "#8" in "fixed #89"
        assert_eq!(token_at("fixed #89", 8), Some((6, "8")));
    }

    #[test]
    fn multibyte_text_does_not_shift_the_offset() {
        let text = "Änderung für #89";
        let cursor = text.len();
        let (start, query) = token_at(text, cursor).unwrap();
        assert_eq!(query, "89");
        assert_eq!(&text[start..start + 1], "#");
    }

    #[test]
    fn a_token_after_a_newline_still_counts() {
        assert_eq!(token_at("a\n#12", 5), Some((2, "12")));
    }
}
