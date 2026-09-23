//! The compact capture window.
//!
//! This is the surface the tray opens: today's entries, editable in place,
//! with one field to add another. It is deliberately not an editor — anything
//! structural happens in the main window.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use chrono::{Local, NaiveDate};
use gtk::glib;
use gtk::prelude::*;
use gtk::gdk;

use crate::i18n::gettext;
use crate::gitlab::refs::Reference;
use crate::store::{blocks, Block, Store};
use crate::ui::markup;
use crate::ui::refs::{open_link, RefResolver};
use crate::ui::dates;

pub struct QuickNote {
    pub window: adw::Window,
    store: Rc<RefCell<Store>>,
    date: Rc<Cell<NaiveDate>>,
    list: gtk::ListBox,
    add: gtk::Entry,
    title: adw::WindowTitle,
    last_workday_box: gtk::Box,
    /// Called after any write, so the tray and the main window can catch up.
    on_changed: RefCell<Option<Box<dyn Fn(NaiveDate)>>>,
    on_open_window: RefCell<Option<Box<dyn Fn(NaiveDate)>>>,
    on_search: RefCell<Option<Box<dyn Fn(NaiveDate)>>>,
    /// Hands references the cache does not know yet to the resolver, so an
    /// entry written here eventually shows its ticket titles too.
    on_resolve: RefCell<Option<Box<dyn Fn(Vec<Reference>)>>>,
}

impl QuickNote {
    pub fn build(app: &adw::Application, store: Rc<RefCell<Store>>) -> Rc<Self> {
        let title = adw::WindowTitle::new(&gettext("Daily Notes"), "");
        let header = adw::HeaderBar::builder().title_widget(&title).build();

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("boxed-list");

        let add = gtk::Entry::builder()
            .placeholder_text(gettext("Add an entry…"))
            .primary_icon_name("list-add-symbolic")
            .activates_default(false)
            .build();

        let last_workday_btn = gtk::ToggleButton::with_label(&gettext("Last workday"));
        let search = gtk::Button::with_label(&gettext("Search…"));
        let open = gtk::Button::with_label(&gettext("Open"));
        open.add_css_class("suggested-action");

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        actions.set_homogeneous(true);
        actions.append(&last_workday_btn);
        actions.append(&search);
        actions.append(&open);

        let last_workday_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
        last_workday_box.set_visible(false);
        last_workday_box.add_css_class("card");

        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        body.set_margin_top(12);
        body.set_margin_bottom(12);
        body.set_margin_start(12);
        body.set_margin_end(12);
        body.append(&list);
        body.append(&add);
        body.append(&last_workday_box);

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

        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&outer));

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
            title,
            last_workday_box,
            on_changed: RefCell::new(None),
            on_open_window: RefCell::new(None),
            on_search: RefCell::new(None),
            on_resolve: RefCell::new(None),
        });

        {
            let s = this.clone();
            this.add.connect_activate(move |e| {
                let text = e.text().trim().to_string();
                if text.is_empty() {
                    return;
                }
                s.append_entry(&text);
                e.set_text("");
                // stay focused so several entries can be typed in a row
                e.grab_focus();
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

    /// Renders one entry as Pango markup, turning links and resolved ticket
    /// references into `<a>` — GtkLabel makes those clickable, and the `title`
    /// attribute becomes the hover card.
    fn render(self: &Rc<Self>, text: &str) -> String {
        let resolver = RefResolver::new(self.store.clone());
        let markup = markup::to_pango(text, &|r| resolver.lookup(r));
        let pending = resolver.take_pending();
        if !pending.is_empty() {
            if let Some(cb) = self.on_resolve.borrow().as_ref() {
                cb(pending);
            }
        }
        markup
    }

    pub fn present_for(self: &Rc<Self>, date: NaiveDate) {
        self.date.set(date);
        self.reload();
        self.window.present();
        self.add.grab_focus();
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

    fn edit_entry(self: &Rc<Self>, block: &Block, text: &str) {
        let body = if text.trim().is_empty() {
            blocks::remove_block(&self.body(), block)
        } else {
            blocks::replace_block(&self.body(), block, text.trim())
        };
        self.write(&body);
    }

    /// Repaints the rows, e.g. after references resolved in the background.
    pub fn refresh(self: &Rc<Self>) {
        if self.window.is_visible() {
            self.reload();
        }
    }

    fn reload(self: &Rc<Self>) {
        let date = self.date.get();
        self.title.set_title(&dates::short(date));
        let today = Local::now().date_naive();
        self.title.set_subtitle(&if date == today {
            gettext("Today")
        } else {
            dates::long(date)
        });

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

        for block in entries {
            self.list.append(&self.entry_row(block));
        }
    }

    /// A row that swaps itself for an editable field when clicked, can be
    /// dragged to reorder, and carries its own delete button.
    fn entry_row(self: &Rc<Self>, block: Block) -> gtk::ListBoxRow {
        let ord = block.ord;

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::None);

        // --- display mode -------------------------------------------------
        let handle = gtk::Image::from_icon_name("list-drag-handle-symbolic");
        handle.add_css_class("dim-label");
        handle.set_tooltip_text(Some(&gettext("Drag to reorder")));

        let label = gtk::Label::new(None);
        label.set_markup(&self.render(&block.text));
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        label.set_hexpand(true);
        // Let a click on a link open it instead of starting an edit.
        label.connect_activate_link(|label, uri| open_link(label, uri));

        let delete = gtk::Button::from_icon_name("user-trash-symbolic");
        delete.add_css_class("flat");
        delete.set_tooltip_text(Some(&gettext("Delete entry")));
        delete.set_valign(gtk::Align::Center);

        let display = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        display.set_margin_top(6);
        display.set_margin_bottom(6);
        display.set_margin_start(8);
        display.set_margin_end(8);
        display.append(&handle);
        display.append(&label);
        display.append(&delete);

        // --- edit mode ----------------------------------------------------
        let entry = gtk::Entry::builder().text(&block.text).hexpand(true).build();
        let edit = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        edit.set_margin_top(6);
        edit.set_margin_bottom(6);
        edit.set_margin_start(8);
        edit.set_margin_end(8);
        edit.append(&entry);

        stack.add_named(&display, Some("display"));
        stack.add_named(&edit, Some("edit"));
        stack.set_visible_child_name("display");

        let row = gtk::ListBoxRow::builder().child(&stack).activatable(true).build();

        // Click to edit — but on the box *around* the label, not the label
        // itself. GTK delivers to the target first, so a click on a link is
        // claimed by the label's own link handling and never reaches here;
        // anything else bubbles up and starts an edit. With the gesture on the
        // label, it swallowed every click and links were dead.
        let click = gtk::GestureClick::new();
        {
            let stack = stack.clone();
            let entry = entry.clone();
            click.connect_released(move |_, _, _, _| {
                stack.set_visible_child_name("edit");
                entry.grab_focus();
                entry.select_region(-1, -1);
            });
        }
        display.add_controller(click);

        {
            let s = self.clone();
            let block = block.clone();
            delete.connect_clicked(move |_| s.delete_entry(&block));
        }

        // Enter commits.
        {
            let s = self.clone();
            let block = block.clone();
            entry.connect_activate(move |e| s.edit_entry(&block, &e.text()));
        }
        // Esc reverts.
        let key = gtk::EventControllerKey::new();
        {
            let stack = stack.clone();
            let original = block.text.clone();
            let entry = entry.clone();
            key.connect_key_pressed(move |_, k, _, _| {
                if k == gdk::Key::Escape {
                    entry.set_text(&original);
                    stack.set_visible_child_name("display");
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
        entry.add_controller(key);

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

    fn toggle_last_workday(self: &Rc<Self>, show: bool) {
        self.last_workday_box.set_visible(show);
        while let Some(c) = self.last_workday_box.first_child() {
            self.last_workday_box.remove(&c);
        }
        if !show {
            return;
        }

        let store = self.store.borrow();
        let Some(prev) = store.last_workday(self.date.get()).ok().flatten() else {
            let l = gtk::Label::new(Some(&gettext("No earlier notes")));
            l.set_margin_top(12);
            l.set_margin_bottom(12);
            l.add_css_class("dim-label");
            self.last_workday_box.append(&l);
            return;
        };

        let heading = gtk::Label::new(None);
        heading.set_xalign(0.0);
        heading.set_markup(&format!(
            "<b>{}</b>",
            glib::markup_escape_text(&dates::short(prev))
        ));
        heading.set_margin_top(10);
        heading.set_margin_start(12);
        heading.set_margin_end(12);
        self.last_workday_box.append(&heading);

        for b in store.summary(prev).unwrap_or_default() {
            let l = gtk::Label::new(Some(&format!("• {}", b.text)));
            l.set_xalign(0.0);
            l.set_wrap(true);
            l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            l.set_margin_start(12);
            l.set_margin_end(12);
            l.add_css_class("dim-label");
            self.last_workday_box.append(&l);
        }
        let pad = gtk::Box::new(gtk::Orientation::Vertical, 0);
        pad.set_height_request(8);
        self.last_workday_box.append(&pad);
    }
}
