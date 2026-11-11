//! The compact capture window.
//!
//! This is the surface the tray opens: today's entries, editable in place,
//! with one field to add another. Both edit markdown source the way the main
//! window does: Enter is a new line, Ctrl+Enter finishes the entry.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use chrono::{Local, NaiveDate};
use gtk::glib;
use gtk::prelude::*;
use gtk::gdk;

use crate::i18n::gettext;
use crate::gitlab::refs::Reference;
use crate::store::{blocks, Block, Kind, Store};
use crate::ui::completion::IssueCompletion;
use crate::ui::editor;
use crate::ui::markup;
use crate::ui::day_pane::DayPane;
use crate::ui::refs::{open_link, RefResolver};
use crate::ui::dates;

pub struct QuickNote {
    pub window: adw::Window,
    store: Rc<RefCell<Store>>,
    date: Rc<Cell<NaiveDate>>,
    list: gtk::ListBox,
    add: sourceview5::View,
    add_buffer: sourceview5::Buffer,
    title: adw::WindowTitle,
    /// The previous workday in a drawer beside the entries, shown and edited
    /// the same way as in the main window.
    drawer: gtk::Box,
    drawer_title: gtk::Label,
    last_workday: Rc<DayPane>,
    /// Called after any write, so the tray and the main window can catch up.
    on_changed: RefCell<Option<Box<dyn Fn(NaiveDate)>>>,
    on_open_window: RefCell<Option<Box<dyn Fn(NaiveDate)>>>,
    on_search: RefCell<Option<Box<dyn Fn(NaiveDate)>>>,
    /// Hands references the cache does not know yet to the resolver, so an
    /// entry written here eventually shows its ticket titles too.
    on_resolve: RefCell<Option<Box<dyn Fn(Vec<Reference>)>>>,
    on_sync_issues: RefCell<Option<Box<dyn Fn()>>>,
    scroll: gtk::ScrolledWindow,
}

impl QuickNote {
    pub fn build(app: &adw::Application, store: Rc<RefCell<Store>>) -> Rc<Self> {
        let title = adw::WindowTitle::new(&gettext("Daily Notes"), "");
        let header = adw::HeaderBar::builder().title_widget(&title).build();

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("boxed-list");

        let (add, add_buffer) = editor::build();
        // GtkTextView has no placeholder of its own.
        let placeholder = gtk::Label::new(Some(&gettext("Add an entry…")));
        placeholder.add_css_class("dim-label");
        placeholder.set_halign(gtk::Align::Start);
        placeholder.set_valign(gtk::Align::Start);
        placeholder.set_margin_top(6);
        placeholder.set_margin_start(8);
        placeholder.set_can_target(false);
        add_buffer
            .bind_property("text", &placeholder, "visible")
            .transform_to(|_, text: String| Some(text.is_empty()))
            .sync_create()
            .build();
        let add_field = gtk::Overlay::new();
        add_field.set_child(Some(&add));
        add_field.add_overlay(&placeholder);
        let add_frame = gtk::Frame::new(None);
        add_frame.set_child(Some(&add_field));

        // Mnemonics, so the buttons are reachable without leaving the entry.
        // The underscore is part of the translatable string: which letter is
        // free depends on the language, so translators place it, not us. These
        // are separate msgids from the tray's labels for that reason — a bare
        // underscore means something else entirely in a dbusmenu label.
        let last_workday_btn = gtk::ToggleButton::with_mnemonic(&gettext("_Last workday"));
        let search = gtk::Button::with_mnemonic(&gettext("_Search…"));
        let open = gtk::Button::with_mnemonic(&gettext("_Open"));
        open.add_css_class("suggested-action");

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        actions.set_homogeneous(true);
        actions.append(&last_workday_btn);
        actions.append(&search);
        actions.append(&open);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        body.set_margin_top(12);
        body.set_margin_bottom(12);
        body.set_margin_start(12);
        body.set_margin_end(12);
        body.append(&list);
        body.append(&add_frame);

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(520)
            .vexpand(true)
            .child(&body)
            .build();

        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        outer.append(&scroll);
        let action_bar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        action_bar.add_css_class("toolbar");
        actions.set_margin_top(6);
        actions.set_margin_bottom(6);
        actions.set_margin_start(12);
        actions.set_margin_end(12);
        action_bar.append(&actions);
        outer.append(&action_bar);
        outer.set_hexpand(true);

        let drawer_title = gtk::Label::new(None);
        drawer_title.set_xalign(0.0);
        drawer_title.add_css_class("heading");
        drawer_title.set_margin_top(12);
        drawer_title.set_margin_start(12);
        drawer_title.set_margin_end(12);

        let last_workday = DayPane::new(store.clone());
        last_workday.set_compact();
        last_workday.widget.set_vexpand(true);

        let drawer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        drawer.set_width_request(DRAWER_WIDTH);
        drawer.add_css_class("view");
        drawer.append(&drawer_title);
        drawer.append(&last_workday.widget);

        let drawer_side = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        drawer_side.set_visible(false);
        drawer_side.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        drawer_side.append(&drawer);

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        content.append(&outer);
        content.append(&drawer_side);

        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&content));

        let window = adw::Window::builder()
            .application(app)
            .default_width(420)
            .default_height(380)
            .resizable(true)
            .title(gettext("Quick Note"))
            .content(&view)
            .build();

        let this = Rc::new(Self {
            window,
            store,
            date: Rc::new(Cell::new(Local::now().date_naive())),
            list,
            add,
            add_buffer,
            title,
            drawer: drawer_side,
            drawer_title,
            last_workday,
            on_changed: RefCell::new(None),
            on_open_window: RefCell::new(None),
            on_search: RefCell::new(None),
            on_resolve: RefCell::new(None),
            on_sync_issues: RefCell::new(None),
            scroll: scroll.clone(),
        });

        // An editor that just grew a line has only now been laid out at its new
        // height, which is when the scrolled window reconfigures its adjustment.
        {
            let s = Rc::downgrade(&this);
            scroll.vadjustment().connect_changed(move |_| {
                if let Some(s) = s.upgrade() {
                    s.reveal_focused_cursor();
                }
            });
        }

        {
            let s = this.clone();
            this.wire_entry_editor(&this.add, move || {
                let text = editor::text(&s.add_buffer);
                if text.trim().is_empty() {
                    return;
                }
                s.add_buffer.set_text("");
                s.append_entry(text.trim_end());
                // stay focused so several entries can be typed in a row
                s.focus_add();
            });
        }
        {
            let s = this.clone();
            this.last_workday
                .connect_resolve_refs(move |needed| s.request_resolve(needed));
        }
        {
            let s = this.clone();
            this.last_workday.connect_sync_issues(move || s.sync_issues());
        }
        {
            // An edit in the drawer is a write like any other: the tray and
            // the main window have to hear about it.
            let s = this.clone();
            this.last_workday.connect_saved(move |date| {
                if let Some(cb) = s.on_changed.borrow().as_ref() {
                    cb(date);
                }
            });
        }
        {
            let s = this.clone();
            last_workday_btn.connect_toggled(move |b| s.toggle_last_workday(b.is_active()));
        }
        {
            let s = this.clone();
            open.connect_clicked(move |_| {
                let d = s.date.get();
                if let Some(cb) = s.on_open_window.borrow().as_ref() {
                    cb(d);
                }
                s.window.set_visible(false);
            });
        }
        {
            let s = this.clone();
            search.connect_clicked(move |_| {
                let d = s.date.get();
                if let Some(cb) = s.on_search.borrow().as_ref() {
                    cb(d);
                }
                s.window.set_visible(false);
            });
        }

        // Esc hides; the app keeps running in the tray.
        let key = gtk::EventControllerKey::new();
        {
            let s = this.clone();
            key.connect_key_pressed(move |_, k, _, _| {
                if k == gtk::gdk::Key::Escape {
                    s.window.set_visible(false);
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
        this.window.add_controller(key);

        // Closing hides: the app lives on in the tray.
        this.window.connect_close_request(|w| {
            w.set_visible(false);
            glib::Propagation::Stop
        });

        this.reload();
        this
    }

    pub fn connect_changed(self: &Rc<Self>, f: impl Fn(NaiveDate) + 'static) {
        *self.on_changed.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_open_window(self: &Rc<Self>, f: impl Fn(NaiveDate) + 'static) {
        *self.on_open_window.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_search(self: &Rc<Self>, f: impl Fn(NaiveDate) + 'static) {
        *self.on_search.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_resolve_refs(self: &Rc<Self>, f: impl Fn(Vec<Reference>) + 'static) {
        *self.on_resolve.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_sync_issues(self: &Rc<Self>, f: impl Fn() + 'static) {
        *self.on_sync_issues.borrow_mut() = Some(Box::new(f));
    }

    fn sync_issues(self: &Rc<Self>) {
        if let Some(cb) = self.on_sync_issues.borrow().as_ref() {
            cb();
        }
    }

    /// Renders one entry as Pango markup, turning links and resolved ticket
    /// references into `<a>` — GtkLabel makes those clickable, and the `title`
    /// attribute becomes the hover card.
    fn render(self: &Rc<Self>, text: &str) -> String {
        let resolver = RefResolver::new(self.store.clone());
        let markup = markup::to_pango(text, &|r| resolver.lookup(r));
        self.request_resolve(resolver.take_pending());
        markup
    }

    fn request_resolve(&self, pending: Vec<Reference>) {
        if pending.is_empty() {
            return;
        }
        if let Some(cb) = self.on_resolve.borrow().as_ref() {
            cb(pending);
        }
    }

    pub fn present_for(self: &Rc<Self>, date: NaiveDate) {
        self.date.set(date);
        self.reload();
        // Not on every reload: that runs after each entry added here, and
        // would throw the drawer out of editing mid-sentence.
        if self.drawer.is_visible() {
            self.render_last_workday();
        }
        self.window.present();

        // Deferred: the rows were just rebuilt, so nothing has been laid out
        // yet and the field has no position to scroll to. On the next idle the
        // geometry is real.
        let s = self.clone();
        glib::idle_add_local_once(move || {
            s.add.grab_focus();
            s.reveal_add_field();
        });
    }

    /// Back to the new-entry field once an entry is done with, so the next one
    /// can be typed straight away.
    fn focus_add(self: &Rc<Self>) {
        self.add.grab_focus();
        // Deferred: the rows were just rebuilt, so nothing is laid out yet.
        let s = self.clone();
        glib::idle_add_local_once(move || s.reveal_add_field());
    }

    /// Sets up a markdown editor for writing one entry: the main window's
    /// editor, made compact, with `#` completion; Ctrl+Enter calls `commit`.
    fn wire_entry_editor(self: &Rc<Self>, view: &sourceview5::View, commit: impl Fn() + 'static) {
        {
            let s = Rc::downgrade(self);
            view.buffer().connect_cursor_position_notify(move |_| {
                let s = s.clone();
                // Deferred: the line the caret moved to may not be laid out yet.
                glib::idle_add_local_once(move || {
                    if let Some(s) = s.upgrade() {
                        s.reveal_focused_cursor();
                    }
                });
            });
        }
        view.set_hexpand(true);
        view.set_top_margin(6);
        view.set_bottom_margin(6);
        view.set_left_margin(8);
        view.set_right_margin(8);
        {
            use sourceview5::prelude::*;
            let provider = IssueCompletion::new(self.store.clone());
            let s = self.clone();
            provider.connect_query(move || s.sync_issues());
            view.completion().add_provider(&provider);
        }
        // Capture phase, as in the main editor: GtkTextView would otherwise
        // insert a newline before we ever see it.
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        key.connect_key_pressed(move |_, k, _, state| {
            let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
            if ctrl && matches!(k, gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter) {
                commit();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        view.add_controller(key);
    }

    /// Brings the new-entry field into view, however many entries are above it.
    fn reveal_add_field(self: &Rc<Self>) {
        self.reveal_area(self.add.upcast_ref(), 0, self.add.height());
    }

    /// Keeps the caret of the focused editor in view. The editors here grow
    /// with their text rather than scrolling themselves, so a long entry would
    /// otherwise push the line being typed off the bottom of the window.
    fn reveal_focused_cursor(&self) {
        let Some(view) = gtk::prelude::GtkWindowExt::focus(&self.window)
            .and_then(|w| w.downcast::<sourceview5::View>().ok())
        else {
            return;
        };
        let buffer = view.buffer();
        let rect = view.iter_location(&buffer.iter_at_mark(&buffer.get_insert()));
        let (_, y) = view.buffer_to_window_coords(gtk::TextWindowType::Widget, 0, rect.y());
        // A little of the line below too, or the caret sits flush on the edge.
        self.reveal_area(view.upcast_ref(), y, rect.height() + 12);
    }

    /// Scrolls just enough to show `height` pixels of `widget` from `y` down.
    fn reveal_area(&self, widget: &gtk::Widget, y: i32, height: i32) {
        let Some(point) =
            widget.compute_point(&self.scroll, &gtk::graphene::Point::new(0.0, y as f32))
        else {
            return;
        };
        let y = f64::from(point.y());
        let adj = self.scroll.vadjustment();
        let height = f64::from(height);
        let page = adj.page_size();
        // y is relative to the viewport, so it is negative above the visible
        // area and past `page` below it.
        if y < 0.0 {
            adj.set_value(adj.value() + y);
        } else if y + height > page {
            adj.set_value(adj.value() + (y + height - page));
        }
    }

    fn notify(self: &Rc<Self>) {
        let d = self.date.get();
        if let Some(cb) = self.on_changed.borrow().as_ref() {
            cb(d);
        }
    }

    fn body(&self) -> String {
        self.store.borrow().body(self.date.get()).unwrap_or_default()
    }

    fn write(self: &Rc<Self>, body: &str) {
        let date = self.date.get();
        if let Err(e) = self.store.borrow_mut().save(date, body) {
            tracing::error!(?e, %date, "quick note save failed");
            return;
        }
        self.reload();
        self.notify();
    }

    fn append_entry(self: &Rc<Self>, text: &str) {
        let body = blocks::append_entry(&self.body(), text);
        self.write(&body);
    }

    fn delete_entry(self: &Rc<Self>, block: &Block) {
        let body = blocks::remove_block(&self.body(), block);
        self.write(&body);
    }

    fn move_entry(self: &Rc<Self>, from: usize, to: usize) {
        let body = blocks::move_block(&self.body(), from, to);
        self.write(&body);
    }

    /// Writes an entry's edited source back in its place; emptied, it goes.
    fn edit_entry(self: &Rc<Self>, block: &Block, text: &str) {
        let body = if text.trim().is_empty() {
            blocks::remove_block(&self.body(), block)
        } else {
            blocks::replace_block(&self.body(), block, text.trim_end())
        };
        self.write(&body);
    }

    /// Repaints the rows, e.g. after references resolved in the background.
    pub fn refresh(self: &Rc<Self>) {
        if self.window.is_visible() {
            self.reload();
            if self.drawer.is_visible() {
                self.last_workday.refresh_refs();
            }
        }
    }

    /// Picks up a write the main window made, so neither view overwrites the
    /// other with a stale copy of the same day.
    pub fn reload_if_showing(self: &Rc<Self>, date: NaiveDate) {
        if date == self.date.get() {
            self.reload();
        }
        if self.drawer.is_visible() && date == self.last_workday.date() {
            self.last_workday.load(date);
        }
    }

    /// Writes out anything still being typed in the drawer.
    pub fn flush_save(self: &Rc<Self>) {
        self.last_workday.flush_save();
    }

    fn reload(self: &Rc<Self>) {
        let date = self.date.get();
        // The full date sits right below, so the title needs no tooltip.
        self.title.set_title(&dates::relative(date, Local::now().date_naive()));
        self.title.set_subtitle(&dates::long(date));

        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }

        let entries = self.store.borrow().summary(date).unwrap_or_default();
        if entries.is_empty() {
            let row = adw::ActionRow::builder()
                .title(gettext("Nothing yet today"))
                .subtitle(gettext("Type below to add your first entry."))
                .build();
            row.set_sensitive(false);
            self.list.append(&row);
            return;
        }

        let body = self.body();
        for block in entries {
            self.list.append(&self.entry_row(block, &body));
        }
    }

    /// A row that swaps itself for a small markdown editor when clicked, can
    /// be dragged to reorder, and carries its own delete button.
    fn entry_row(self: &Rc<Self>, block: Block, body: &str) -> gtk::ListBoxRow {
        let ord = block.ord;

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::None);

        // --- display mode -------------------------------------------------
        let handle = gtk::Image::from_icon_name("list-drag-handle-symbolic");
        handle.add_css_class("dim-label");
        handle.set_tooltip_text(Some(&gettext("Drag to reorder")));

        // A list entry shows its nested items indented under it.
        let items = if block.kind == Kind::Bullet {
            blocks::list_items(body, &block)
        } else {
            Vec::new()
        };
        let shown: Vec<(usize, String)> = if items.is_empty() {
            vec![(0, self.render(&block.text))]
        } else {
            items
                .iter()
                .map(|i| match i.depth {
                    0 => (0, self.render(&i.text)),
                    d => (d, format!("{} {}", ["•", "◦", "▪"][d % 3], self.render(&i.text))),
                })
                .collect()
        };
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        for (depth, markup) in shown {
            let label = gtk::Label::new(None);
            label.set_markup(&markup);
            label.set_xalign(0.0);
            label.set_wrap(true);
            label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            label.set_margin_start(depth as i32 * 16);
            // Let a click on a link open it instead of starting an edit.
            label.connect_activate_link(|label, uri| open_link(label, uri));
            text.append(&label);
        }

        let delete = gtk::Button::from_icon_name("user-trash-symbolic");
        delete.add_css_class("flat");
        delete.set_tooltip_text(Some(&gettext("Delete entry")));
        delete.set_valign(gtk::Align::Center);
        {
            let s = self.clone();
            let block = block.clone();
            delete.connect_clicked(move |_| s.delete_entry(&block));
        }

        let display = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        display.set_margin_top(6);
        display.set_margin_bottom(6);
        display.set_margin_start(8);
        display.set_margin_end(8);
        display.append(&handle);
        display.append(&text);
        display.append(&delete);

        // --- edit mode ----------------------------------------------------
        // The entry's source lines as written, in the main window's editor:
        // Enter is a new line, Ctrl+Enter saves, Esc cancels.
        let source = body
            .lines()
            .skip(block.line_start)
            .take(block.line_end + 1 - block.line_start)
            .collect::<Vec<_>>()
            .join("\n");
        let (view, buffer) = editor::build();
        buffer.set_text(&source);
        {
            let s = self.clone();
            let block = block.clone();
            let buffer = buffer.clone();
            self.wire_entry_editor(&view, move || {
                s.edit_entry(&block, &editor::text(&buffer));
                s.focus_add();
            });
        }
        let edit = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        edit.set_margin_top(6);
        edit.set_margin_bottom(6);
        edit.set_margin_start(8);
        edit.set_margin_end(8);
        edit.append(&view);

        stack.add_named(&display, Some("display"));
        stack.add_named(&edit, Some("edit"));
        stack.set_visible_child_name("display");

        let row = gtk::ListBoxRow::builder().child(&stack).activatable(true).build();

        // Click to edit — but on the box *around* the labels, not the labels
        // themselves. GTK delivers to the target first, so a click on a link is
        // claimed by the label's own link handling and never reaches here;
        // anything else bubbles up and starts an edit. With the gesture on the
        // label, it swallowed every click and links were dead.
        let click = gtk::GestureClick::new();
        {
            let stack = stack.clone();
            let view = view.clone();
            let buffer = buffer.clone();
            click.connect_released(move |_, _, _, _| {
                stack.set_visible_child_name("edit");
                view.grab_focus();
                buffer.place_cursor(&buffer.end_iter());
            });
        }
        display.add_controller(click);

        // Esc reverts. Bubble phase, so an open completion popup closes first.
        let cancel = gtk::EventControllerKey::new();
        {
            let s = self.clone();
            let stack = stack.clone();
            let buffer = buffer.clone();
            cancel.connect_key_pressed(move |_, k, _, _| {
                if k == gdk::Key::Escape {
                    buffer.set_text(&source);
                    stack.set_visible_child_name("display");
                    s.focus_add();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
        view.add_controller(cancel);

        // --- drag to reorder ---------------------------------------------
        // The payload is the block's position, which is all move_block needs.
        let drag = gtk::DragSource::new();
        drag.set_actions(gdk::DragAction::MOVE);
        drag.connect_prepare(move |_, _, _| {
            Some(gdk::ContentProvider::for_value(&(ord as i32).to_value()))
        });
        {
            let row = row.clone();
            drag.connect_drag_begin(move |src, _| {
                // Drag the row's own appearance, so the cursor carries the
                // entry rather than a generic icon.
                let paintable = gtk::WidgetPaintable::new(Some(&row)).current_image();
                src.set_icon(Some(&paintable), 0, 0);
                row.add_css_class("dnd-source");
            });
        }
        {
            let row = row.clone();
            drag.connect_drag_end(move |_, _, _| row.remove_css_class("dnd-source"));
        }
        row.add_controller(drag);

        let drop = gtk::DropTarget::new(glib::Type::I32, gdk::DragAction::MOVE);
        {
            let s = self.clone();
            drop.connect_drop(move |_, value, _, _| {
                let Ok(from) = value.get::<i32>() else {
                    return false;
                };
                if from < 0 || from as usize == ord {
                    return false;
                }
                s.move_entry(from as usize, ord);
                true
            });
        }
        {
            let row = row.clone();
            drop.connect_enter(move |_, _, _| {
                row.add_css_class("dnd-over");
                gdk::DragAction::MOVE
            });
        }
        {
            let row = row.clone();
            drop.connect_leave(move |_| row.remove_css_class("dnd-over"));
        }
        row.add_controller(drop);

        row
    }

    /// Opens or closes the drawer, growing the window by its width rather
    /// than squeezing today's entries — unless the window is maximized or
    /// tiled, where its size is not ours to change.
    fn toggle_last_workday(self: &Rc<Self>, show: bool) {
        if show == self.drawer.is_visible() {
            return;
        }
        if show {
            self.render_last_workday();
        } else {
            self.last_workday.flush_save();
        }
        self.drawer.set_visible(show);

        let w = &self.window;
        let tiled = w
            .surface()
            .and_downcast::<gdk::Toplevel>()
            .is_some_and(|t| t.state().contains(gdk::ToplevelState::TILED));
        if w.is_maximized() || w.is_fullscreen() || tiled {
            return;
        }
        let (width, height) = (w.width(), w.height());
        if width <= 0 {
            return;
        }
        let delta = DRAWER_WIDTH + 1; // + the separator
        let width = if show { width + delta } else { (width - delta).max(1) };
        w.set_default_size(width, height);
    }

    fn render_last_workday(self: &Rc<Self>) {
        let store = self.store.borrow();
        match store.last_workday(self.date.get()).ok().flatten() {
            Some(prev) => {
                drop(store);
                self.drawer_title.set_text(&dates::relative(prev, Local::now().date_naive()));
                self.drawer_title.set_tooltip_text(Some(&dates::long(prev)));
                self.drawer_title.remove_css_class("dim-label");
                self.last_workday.load(prev);
                self.last_workday.widget.set_visible(true);
            }
            None => {
                drop(store);
                // Not the preview's own empty page: that one invites writing,
                // and there is nothing to write here.
                self.drawer_title.set_text(&gettext("No earlier notes"));
                self.drawer_title.set_tooltip_text(None);
                self.drawer_title.add_css_class("dim-label");
                self.last_workday.widget.set_visible(false);
            }
        }
    }
}

/// How much the drawer adds to the window.
const DRAWER_WIDTH: i32 = 360;
