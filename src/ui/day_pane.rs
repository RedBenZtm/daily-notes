//! One day, rendered or in source, with a floating button to switch.
//!
//! Shared by the main window and the Quick Note's last-workday drawer, so both
//! render, edit and save a day the same way. The pane owns the buffer and its
//! debounced save; hosts only hear about a write after it happened.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use chrono::{Local, NaiveDate};
use gtk::glib;
use gtk::prelude::*;

use crate::gitlab::refs::Reference;
use crate::i18n::gettext;
use crate::store::Store;
use crate::ui::preview::{Preview, SourceHit};
use crate::ui::refs::RefDecorator;
use crate::ui::editor;

/// How long the editor sits idle before the day is written back.
const SAVE_DEBOUNCE_MS: u64 = 600;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Preview,
    Edit,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Preview => "preview",
            Mode::Edit => "edit",
        }
    }
}

pub struct DayPane {
    /// The pane's outermost widget, for the host to place.
    pub widget: gtk::Overlay,
    store: Rc<RefCell<Store>>,
    date: Cell<NaiveDate>,
    buffer: sourceview5::Buffer,
    view: sourceview5::View,
    preview: Rc<Preview>,
    stack: gtk::Stack,
    toggle: gtk::Button,
    /// Set while the buffer is being repopulated, so the resulting `changed`
    /// signal is not mistaken for the user typing.
    loading: Cell<bool>,
    pending_save: RefCell<Option<glib::SourceId>>,
    refs: Rc<RefDecorator>,
    on_saved: RefCell<Option<Box<dyn Fn(NaiveDate)>>>,
    on_sync_issues: RefCell<Option<Box<dyn Fn()>>>,
}

impl DayPane {
    pub fn new(store: Rc<RefCell<Store>>) -> Rc<Self> {
        let (view, buffer) = editor::build();
        let editor_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&view)
            .build();

        let preview = Preview::new(store.clone());
        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(80);
        stack.add_named(&preview.scroll, Some(Mode::Preview.name()));
        stack.add_named(&editor_scroll, Some(Mode::Edit.name()));
        stack.set_visible_child_name(Mode::Preview.name());

        // One button that always offers the *other* mode, floating over the
        // content rather than sitting in a header bar the drawer does not have.
        let toggle = gtk::Button::new();
        toggle.add_css_class("osd");
        toggle.add_css_class("circular");
        toggle.set_halign(gtk::Align::End);
        toggle.set_valign(gtk::Align::Start);
        toggle.set_margin_top(8);
        toggle.set_margin_end(8);
        // Clicking it must not pull focus out of the editor it is about to
        // show or hide; set_mode settles focus itself.
        toggle.set_focus_on_click(false);

        let widget = gtk::Overlay::new();
        widget.set_child(Some(&stack));
        widget.add_overlay(&toggle);

        let refs = RefDecorator::new(&view, &buffer, store.clone());

        let this = Rc::new(Self {
            widget,
            store,
            date: Cell::new(Local::now().date_naive()),
            buffer,
            view,
            preview,
            stack,
            toggle,
            loading: Cell::new(false),
            pending_save: RefCell::new(None),
            refs,
            on_saved: RefCell::new(None),
            on_sync_issues: RefCell::new(None),
        });
        this.sync_toggle();

        // `#` offers the project's open issues, the way GitLab's comment box
        // does. Served from the local cache so typing never waits on the API.
        {
            use sourceview5::prelude::*;
            let provider = crate::ui::completion::IssueCompletion::new(this.store.clone());
            {
                let s = Rc::downgrade(&this);
                provider.connect_query(move || {
                    if let Some(s) = s.upgrade() {
                        s.request_issue_sync();
                    }
                });
            }
            this.view.completion().add_provider(&provider);

            // Deleting back to a bare `#` empties the word being completed, at
            // which point GtkSourceView ends the session — and deletion never
            // re-triggers it, only typing `#` does. So when an edit leaves the
            // caret right after a `#`, offer the list again.
            let completion = this.view.completion();
            this.buffer.connect_changed(move |buf| {
                let mut probe = buf.iter_at_mark(&buf.get_insert());
                if !(probe.backward_char() && probe.char() == '#') {
                    return;
                }
                let completion = completion.clone();
                // Deferred: showing while `changed` is still being emitted
                // re-enters GtkSourceView's handling of the very same edit.
                glib::idle_add_local_once(move || completion.show());
            });
        }

        {
            let s = this.clone();
            this.buffer.connect_changed(move |_| s.schedule_save());
        }
        {
            let s = this.clone();
            this.toggle.connect_clicked(move |_| {
                s.set_mode(match s.mode() {
                    Mode::Preview => Mode::Edit,
                    Mode::Edit => Mode::Preview,
                });
            });
        }
        {
            let s = this.clone();
            this.preview.connect_edit_requested(move |hit| s.edit_at(hit));
        }

        // Ctrl+Return finishes editing, mirroring `e` to start. Capture phase,
        // because GtkTextView would otherwise treat it as a plain Return and
        // insert a newline before we ever see it.
        let leave = gtk::EventControllerKey::new();
        leave.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let s = this.clone();
            leave.connect_key_pressed(move |_, key, _, state| {
                let ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
                let enter = matches!(
                    key,
                    gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter | gtk::gdk::Key::ISO_Enter
                );
                if ctrl && enter {
                    s.set_mode(Mode::Preview);
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
        this.view.add_controller(leave);

        this
    }

    /// Tighter margins for a narrow host, e.g. the Quick Note drawer.
    pub fn set_compact(&self) {
        self.preview.set_compact();
        self.view.set_bottom_margin(24);
        self.view.set_left_margin(12);
        self.view.set_right_margin(12);
    }

    /// Makes `e` open the editor anywhere in `window`.
    ///
    /// It sits on the window rather than on the rendered view, because the
    /// rendered view deliberately holds no focus (see focus_current_mode). The
    /// guards below are what keep a bare letter key safe: it only acts in the
    /// rendered view, and never while the caret is in something typable such
    /// as a search entry.
    pub fn add_edit_shortcut(self: &Rc<Self>, window: &impl IsA<gtk::Window>) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let s = self.clone();
        let win = window.as_ref().downgrade();
        keys.connect_key_pressed(move |_, key, _, state| {
            if s.mode() != Mode::Preview || !s.widget.is_drawable() {
                return glib::Propagation::Proceed;
            }
            if let Some(focus) = win.upgrade().and_then(|w| GtkWindowExt::focus(&w)) {
                if focus.is::<gtk::Text>() || focus.is::<gtk::TextView>() {
                    return glib::Propagation::Proceed;
                }
            }
            let plain = !state.intersects(
                gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK,
            );
            if plain && matches!(key, gtk::gdk::Key::e | gtk::gdk::Key::E) {
                s.set_mode(Mode::Edit);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        window.as_ref().add_controller(keys);
    }

    pub fn date(&self) -> NaiveDate {
        self.date.get()
    }

    pub fn mode(&self) -> Mode {
        if self.stack.visible_child_name().as_deref() == Some(Mode::Edit.name()) {
            Mode::Edit
        } else {
            Mode::Preview
        }
    }

    /// Shows `date`, rendered. Anything still unsaved of the day on screen is
    /// written first, so switching can never drop keystrokes.
    pub fn load(self: &Rc<Self>, date: NaiveDate) {
        self.flush_save();
        self.date.set(date);
        let body = match self.store.borrow().body(date) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!(?e, %date, "could not read day");
                String::new()
            }
        };
        self.loading.set(true);
        self.buffer.set_text(&body);
        self.buffer.place_cursor(&self.buffer.end_iter());
        self.loading.set(false);

        self.refs.refresh();
        // Selecting a day always shows it rendered; `e` or the toggle opens
        // the editor. Going through set_mode keeps focus handling in one
        // place — poking the stack directly left focus on the hidden editor,
        // and GTK then moved it to the first focusable widget, a label.
        self.render_preview();
        self.preview.scroll_to_top();
        self.set_mode(Mode::Preview);
    }

    /// Switches between the rendered view and the editor.
    pub fn set_mode(self: &Rc<Self>, mode: Mode) {
        if self.mode() != mode {
            if mode == Mode::Preview {
                // The editor may hold unsaved keystrokes the rendered view
                // would otherwise not show.
                self.flush_save();
                self.render_preview();
            }
            self.stack.set_visible_child_name(mode.name());
            self.sync_toggle();
        }
        // Always, not only on a change: switching days also lands here with
        // the mode already current, and focus still has to be settled.
        self.focus_current_mode();
    }

    fn sync_toggle(&self) {
        match self.mode() {
            Mode::Preview => {
                self.toggle.set_icon_name("document-edit-symbolic");
                // Translators: {} is a keyboard key, shown literally.
                self.toggle
                    .set_tooltip_text(Some(&gettext("Edit ({})").replace("{}", "E")));
            }
            Mode::Edit => {
                self.toggle.set_icon_name("view-reveal-symbolic");
                // Translators: {} is a key combination, shown literally.
                self.toggle.set_tooltip_text(Some(
                    &gettext("Rendered view ({})").replace("{}", "Ctrl+Enter"),
                ));
            }
        }
    }

    pub fn focus_current_mode(&self) {
        // Only the editor asks for focus: pressing `e` means "let me type".
        // Showing the rendered view deliberately leaves focus to GTK. Nothing
        // in the rendered view can steal focus now that its labels are not
        // focusable.
        if self.mode() == Mode::Edit {
            self.view.grab_focus();
        }
    }

    /// Opens the editor with the cursor where a double-click in the rendered
    /// view landed, keeping that line at about the same height on screen.
    fn edit_at(self: &Rc<Self>, hit: Option<SourceHit>) {
        self.set_mode(Mode::Edit);
        let Some(hit) = hit else {
            return;
        };
        let Some(start) = self.buffer.iter_at_line(hit.line_start as i32) else {
            return;
        };
        let mut end = self
            .buffer
            .iter_at_line(hit.line_end as i32)
            .unwrap_or_else(|| self.buffer.end_iter());
        if !end.ends_line() {
            end.forward_to_line_end();
        }
        let source = self.buffer.text(&start, &end, false);
        let offset = source_offset(&source, &hit.rendered, hit.rendered_char);
        let mut iter = start;
        iter.forward_chars(offset as i32);
        self.buffer.place_cursor(&iter);
        // A mark rather than an iter: the editor was hidden until just now,
        // so its lines are not measured yet, and only scroll_to_mark waits
        // for that.
        let yalign = hit.y_fraction.clamp(0.0, 1.0);
        self.view
            .scroll_to_mark(&self.buffer.get_insert(), 0.0, true, 0.0, yalign);
    }

    /// Puts the cursor on a 0-based line and scrolls it into view — how a tray
    /// entry and a search hit navigate to their source.
    pub fn scroll_to_line(self: &Rc<Self>, line: usize) {
        // Landing on a specific line is an editing intent.
        self.set_mode(Mode::Edit);
        let Some(iter) = self.buffer.iter_at_line(line as i32) else {
            return;
        };
        self.buffer.place_cursor(&iter);
        // within_margin 0.15 leaves the line a little off the top edge
        self.view
            .scroll_to_mark(&self.buffer.get_insert(), 0.15, false, 0.0, 0.0);
        self.view.grab_focus();
    }

    fn render_preview(&self) {
        let blocks = self.store.borrow().blocks(self.date.get()).unwrap_or_default();
        self.preview.render(&blocks);
    }

    /// Re-reads references and repaints them. Cheap: it works from the cache.
    pub fn refresh_refs(&self) {
        self.refs.refresh();
        if self.mode() == Mode::Preview {
            self.render_preview();
        }
    }

    fn schedule_save(self: &Rc<Self>) {
        if self.loading.get() {
            return;
        }
        if let Some(id) = self.pending_save.borrow_mut().take() {
            id.remove();
        }
        let s = self.clone();
        let id = glib::timeout_add_local_once(
            std::time::Duration::from_millis(SAVE_DEBOUNCE_MS),
            move || {
                s.pending_save.replace(None);
                s.save_now();
            },
        );
        self.pending_save.replace(Some(id));
    }

    pub fn flush_save(self: &Rc<Self>) {
        let pending = self.pending_save.borrow_mut().take();
        if let Some(id) = pending {
            id.remove();
            self.save_now();
        }
    }

    fn save_now(self: &Rc<Self>) {
        let body = editor::text(&self.buffer);
        let date = self.date.get();
        if let Err(e) = self.store.borrow_mut().save(date, &body) {
            tracing::error!(?e, %date, "could not save day");
            return;
        }
        tracing::debug!(%date, bytes = body.len(), "saved");
        self.refresh_refs();
        if let Some(cb) = self.on_saved.borrow().as_ref() {
            cb(date);
        }
    }

    /// Called after each write of the day, with the day written.
    pub fn connect_saved(&self, f: impl Fn(NaiveDate) + 'static) {
        *self.on_saved.borrow_mut() = Some(Box::new(f));
    }

    /// Hands newly-seen references to the background resolver.
    pub fn connect_resolve_refs(&self, f: impl Fn(Vec<Reference>) + 'static) {
        let f = Rc::new(f);
        {
            let f = f.clone();
            self.refs.connect_resolve(move |needed| f(needed));
        }
        self.preview.connect_resolve_refs(move |needed| f(needed));
    }

    pub fn connect_sync_issues(&self, f: impl Fn() + 'static) {
        *self.on_sync_issues.borrow_mut() = Some(Box::new(f));
    }

    fn request_issue_sync(&self) {
        if let Some(cb) = self.on_sync_issues.borrow().as_ref() {
            cb();
        }
    }
}

/// Maps a character position in a block's rendered text to one in its source.
///
/// The two differ by markup (`**`, list markers, link syntax, ticket titles),
/// so there is no exact mapping. Instead: the word under the click is looked
/// up in the source, taking the occurrence closest to where the position
/// would land proportionally. A click on something that only exists rendered,
/// such as a ticket title, falls back to that proportional estimate.
pub fn source_offset(source: &str, rendered: &str, rendered_char: usize) -> usize {
    let src: Vec<char> = source.chars().collect();
    let ren: Vec<char> = rendered.chars().collect();
    if src.is_empty() {
        return 0;
    }
    let at = rendered_char.min(ren.len());
    let estimate = if ren.is_empty() {
        src.len()
    } else {
        at * src.len() / ren.len()
    };

    let is_word = |c: &char| c.is_alphanumeric() || *c == '_';
    let mut start = at;
    while start > 0 && is_word(&ren[start - 1]) {
        start -= 1;
    }
    let mut end = at;
    while end < ren.len() && is_word(&ren[end]) {
        end += 1;
    }
    let word = &ren[start..end];
    let within = at - start;

    if word.is_empty() || word.len() > src.len() {
        return estimate.min(src.len());
    }
    (0..=src.len() - word.len())
        .filter(|&i| &src[i..i + word.len()] == word)
        .map(|i| i + within)
        .min_by_key(|&p| p.abs_diff(estimate))
        .unwrap_or(estimate)
        .min(src.len())
}

#[cfg(test)]
mod tests {
    use super::source_offset;

    #[test]
    fn lands_inside_the_same_word() {
        // "- fixed **login** bug" renders as "fixed login bug"
        let src = "- fixed **login** bug";
        let ren = "fixed login bug";
        let at = ren.find("gin").unwrap();
        assert_eq!(source_offset(src, ren, at), src.find("gin").unwrap());
    }

    #[test]
    fn picks_the_nearest_repeat() {
        let src = "- a test and a test";
        let ren = "a test and a test";
        let second = ren.rfind("test").unwrap();
        assert_eq!(source_offset(src, ren, second), src.rfind("test").unwrap());
        let first = ren.find("test").unwrap();
        assert_eq!(source_offset(src, ren, first), src.find("test").unwrap());
    }

    #[test]
    fn falls_back_to_proportion_for_rendered_only_text() {
        // A ticket title the source does not contain.
        let src = "- look at #12";
        let ren = "look at #12 Crash on start";
        let at = ren.find("Crash").unwrap();
        let got = source_offset(src, ren, at);
        assert!(got <= src.chars().count());
    }

    #[test]
    fn handles_empty_and_out_of_range() {
        assert_eq!(source_offset("", "abc", 2), 0);
        assert_eq!(source_offset("abc", "", 5), 3);
        assert_eq!(source_offset("abc", "abc", 99), 3);
    }

    #[test]
    fn counts_characters_not_bytes() {
        let src = "- über straße";
        let ren = "über straße";
        let at = ren.chars().position(|c| c == 't').unwrap();
        let want = src.chars().position(|c| c == 't').unwrap();
        assert_eq!(source_offset(src, ren, at), want);
    }
}
