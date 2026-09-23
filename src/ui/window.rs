//! The main window: day navigation, a sidebar of recent days, and the editor.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use chrono::{Duration, Local, NaiveDate};
use gtk::prelude::*;
use gtk::glib;

use crate::i18n::gettext;
use crate::app::TrayLink;
use crate::store::Store;
use crate::ui::{activity::Activity, dates, editor, preview::Preview};

/// `meta` key holding whether the activity drawer was left open.
const ACTIVITY_DRAWER: &str = "activity_drawer";

/// How long the editor sits idle before the day is written back.
const SAVE_DEBOUNCE_MS: u32 = 600;

pub struct MainWindow {
    pub window: adw::ApplicationWindow,
    store: Rc<RefCell<Store>>,
    buffer: sourceview5::Buffer,
    date: Rc<Cell<NaiveDate>>,
    /// Set while the buffer is being repopulated, so the resulting `changed`
    /// signal is not mistaken for the user typing.
    loading: Rc<Cell<bool>>,
    pending_save: Rc<RefCell<Option<glib::SourceId>>>,
    title: adw::WindowTitle,
    sidebar: gtk::ListBox,
    view: sourceview5::View,
    tray: Option<TrayLink>,
    search_bar: gtk::SearchBar,
    search_entry: gtk::SearchEntry,
    sidebar_stack: gtk::Stack,
    results: gtk::ListBox,
    results_status: gtk::Label,
    /// Debounces the query so each keystroke does not hit SQLite.
    pending_search: Rc<RefCell<Option<glib::SourceId>>>,
    refs: RefCell<Option<Rc<crate::ui::refs::RefDecorator>>>,
    preview: Rc<Preview>,
    /// "preview" or "edit".
    view_stack: gtk::Stack,
    modes: adw::ToggleGroup,
    on_sync_issues: RefCell<Option<Box<dyn Fn()>>>,
    activity: Rc<Activity>,
    /// The activity drawer, on the trailing edge of the content.
    drawer: adw::OverlaySplitView,
}

impl MainWindow {
    pub fn new(
        app: &adw::Application,
        store: Rc<RefCell<Store>>,
        tray: Option<TrayLink>,
    ) -> Rc<Self> {
        let (view, buffer) = editor::build();

        let title = adw::WindowTitle::new(&gettext("Daily Notes"), "");
        let header = adw::HeaderBar::builder().title_widget(&title).build();

        let prev = gtk::Button::from_icon_name("go-previous-symbolic");
        prev.set_tooltip_text(Some(&gettext("Previous day")));
        let next = gtk::Button::from_icon_name("go-next-symbolic");
        next.set_tooltip_text(Some(&gettext("Next day")));
        let today = gtk::Button::with_label(&gettext("Today"));
        let nav = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        nav.add_css_class("linked");
        nav.append(&prev);
        nav.append(&next);
        header.pack_start(&nav);
        header.pack_start(&today);

        let sidebar = gtk::ListBox::new();
        sidebar.set_selection_mode(gtk::SelectionMode::Single);
        sidebar.add_css_class("navigation-sidebar");
        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&sidebar)
            .build();

        // The sidebar shows recent days, or search results while searching —
        // one column, two jobs, so the editor never loses width.
        let results = gtk::ListBox::new();
        results.set_selection_mode(gtk::SelectionMode::None);
        results.add_css_class("navigation-sidebar");
        let results_status = gtk::Label::new(None);
        results_status.add_css_class("dim-label");
        results_status.set_margin_top(18);
        results_status.set_wrap(true);
        results_status.set_justify(gtk::Justification::Center);
        let results_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        results_box.append(&results_status);
        results_box.append(&results);
        let results_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&results_box)
            .build();

        let sidebar_stack = gtk::Stack::new();
        sidebar_stack.add_named(&sidebar_scroll, Some("days"));
        sidebar_stack.add_named(&results_scroll, Some("results"));
        sidebar_stack.set_visible_child_name("days");

        let sidebar_title = adw::WindowTitle::new(&gettext("Recent days"), "");
        let sidebar_page = adw::ToolbarView::new();
        sidebar_page.add_top_bar(
            &adw::HeaderBar::builder()
                .title_widget(&sidebar_title)
                .show_end_title_buttons(false)
                .build(),
        );
        sidebar_page.set_content(Some(&sidebar_stack));

        let search_entry = gtk::SearchEntry::new();
        search_entry.set_placeholder_text(Some(&gettext("Search all notes…")));
        search_entry.set_hexpand(true);
        // key_capture_widget is set once the window exists, so typing
        // anywhere in the window opens the search bar.
        let search_bar = gtk::SearchBar::builder().child(&search_entry).build();

        let editor_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&view)
            .build();

        let preview = Preview::new(store.clone());
        let view_stack = gtk::Stack::new();
        view_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        view_stack.set_transition_duration(80);
        view_stack.add_named(&preview.scroll, Some("preview"));
        view_stack.add_named(&editor_scroll, Some("edit"));
        view_stack.set_visible_child_name("preview");

        // A segmented control, the way an editor offers preview vs source.
        let modes = adw::ToggleGroup::new();
        modes.add(
            adw::Toggle::builder()
                .name("preview")
                .icon_name("view-reveal-symbolic")
                // Translators: {} is a key combination, shown literally.
                .tooltip(gettext("Rendered view ({})").replace("{}", "Ctrl+Enter"))
                .build(),
        );
        modes.add(
            adw::Toggle::builder()
                .name("edit")
                .icon_name("document-edit-symbolic")
                // Translators: {} is a keyboard key, shown literally.
                .tooltip(gettext("Edit ({})").replace("{}", "E"))
                .build(),
        );
        modes.add_css_class("flat");
        let activity_toggle = gtk::ToggleButton::new();
        activity_toggle.set_icon_name("gitlab-symbolic");
        activity_toggle.set_tooltip_text(Some(&gettext("GitLab activity")));

        let search_toggle = gtk::ToggleButton::new();
        search_toggle.set_icon_name("system-search-symbolic");
        search_toggle.set_tooltip_text(Some(&gettext("Search all notes")));
        // pack_end fills right to left, so the first one packed is the
        // rightmost button.
        header.pack_end(&activity_toggle);
        header.pack_end(&search_toggle);
        header.pack_end(&modes);
        search_toggle
            .bind_property("active", &search_bar, "search-mode-enabled")
            .bidirectional()
            .sync_create()
            .build();

        // The activity drawer lives on the trailing edge, nested inside the
        // day sidebar's split so both can be open at once.
        let activity = Activity::new(store.clone());
        let drawer = adw::OverlaySplitView::builder()
            .sidebar_position(gtk::PackType::End)
            .sidebar(&activity.widget)
            .content(&view_stack)
            .max_sidebar_width(420.0)
            .min_sidebar_width(280.0)
            .sidebar_width_fraction(0.35)
            .show_sidebar(false)
            .build();

        activity_toggle
            .bind_property("active", &drawer, "show-sidebar")
            .bidirectional()
            .sync_create()
            .build();

        let content = adw::ToolbarView::new();
        content.add_top_bar(&header);
        content.add_top_bar(&search_bar);
        content.set_content(Some(&drawer));

        let split = adw::OverlaySplitView::builder()
            .sidebar(&sidebar_page)
            .content(&content)
            .max_sidebar_width(240.0)
            .build();

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .default_width(900)
            .default_height(640)
            .title(gettext("Daily Notes"))
            .content(&split)
            .build();

        let this = Rc::new(Self {
            window,
            store,
            buffer,
            date: Rc::new(Cell::new(Local::now().date_naive())),
            loading: Rc::new(Cell::new(false)),
            pending_save: Rc::new(RefCell::new(None)),
            title,
            sidebar,
            view: view.clone(),
            tray,
            search_bar: search_bar.clone(),
            search_entry: search_entry.clone(),
            sidebar_stack,
            results,
            results_status,
            pending_search: Rc::new(RefCell::new(None)),
            refs: RefCell::new(None),
            preview,
            view_stack,
            modes,
            on_sync_issues: RefCell::new(None),
            activity,
            drawer,
        });

        let decorator = crate::ui::refs::RefDecorator::new(&view, &this.buffer, this.store.clone());
        *this.refs.borrow_mut() = Some(decorator);

        // `#` offers the project's open issues, the way GitLab's comment box
        // does. Served from the local cache so typing never waits on the API.
        {
            use sourceview5::prelude::*;
            let provider = crate::ui::completion::IssueCompletion::new(this.store.clone());
            {
                let s = this.clone();
                provider.connect_query(move || s.request_issue_sync());
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
            this.modes.connect_active_notify(move |g| {
                let name = if g.active() == 0 { "preview" } else { "edit" };
                s.set_mode(name);
            });
        }
        {
            let s = this.clone();
            this.preview.connect_edit_requested(move || s.set_mode("edit"));
        }

        // `e` opens the editor. It sits on the window rather than on the
        // rendered view, because the rendered view deliberately holds no focus
        // (see focus_current_mode). The guards below are what keep a bare
        // letter key safe: it only acts in the rendered view, and never while
        // the caret is in something typable such as the search entry.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let s = this.clone();
            keys.connect_key_pressed(move |_, key, _, state| {
                if s.view_stack.visible_child_name().as_deref() != Some("preview") {
                    return glib::Propagation::Proceed;
                }
                if let Some(focus) = gtk::prelude::GtkWindowExt::focus(&s.window) {
                    if focus.is::<gtk::Text>() || focus.is::<gtk::TextView>() {
                        return glib::Propagation::Proceed;
                    }
                }
                let plain = !state.intersects(
                    gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK,
                );
                if plain && matches!(key, gtk::gdk::Key::e | gtk::gdk::Key::E) {
                    s.set_mode("edit");
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
        this.window.add_controller(keys);

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
                    s.set_mode("preview");
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
        this.view.add_controller(leave);

        // Restore the drawer to however it was left.
        let open = this
            .store
            .borrow()
            .meta_get(ACTIVITY_DRAWER)
            .as_deref()
            == Some("open");
        activity_toggle.set_active(open);
        {
            let s = this.clone();
            this.drawer.connect_show_sidebar_notify(move |d| {
                let value = if d.shows_sidebar() { "open" } else { "closed" };
                if let Err(e) = s.store.borrow().meta_set(ACTIVITY_DRAWER, value) {
                    tracing::warn!(?e, "could not remember the activity drawer state");
                }
                if d.shows_sidebar() {
                    s.activity.show_day(s.date.get());
                }
            });
        }
        {
            let s = this.clone();
            this.activity.connect_insert(move |line| s.append_to_day(&line));
        }

        search_bar.set_key_capture_widget(Some(&this.window));
        this.connect_signals(&prev, &next, &today);
        this.load(this.date.get());
        this.refresh_sidebar();
        view.grab_focus();
        this
    }

    fn connect_signals(self: &Rc<Self>, prev: &gtk::Button, next: &gtk::Button, today: &gtk::Button) {
        let s = self.clone();
        prev.connect_clicked(move |_| s.go_to(s.date.get() - Duration::days(1)));
        let s = self.clone();
        next.connect_clicked(move |_| s.go_to(s.date.get() + Duration::days(1)));
        let s = self.clone();
        today.connect_clicked(move |_| s.go_to(Local::now().date_naive()));

        let s = self.clone();
        self.buffer.connect_changed(move |_| s.schedule_save());

        let s = self.clone();
        self.sidebar.connect_row_activated(move |_, row| {
            let key: Option<String> = unsafe { row.data::<String>("date").map(|p| p.as_ref().clone()) };
            if let Some(key) = key {
                if let Ok(d) = NaiveDate::parse_from_str(&key, crate::store::days::DATE_FMT) {
                    s.go_to(d);
                }
            }
        });

        let s = self.clone();
        self.results.connect_row_activated(move |_, row| {
            let hit: Option<(NaiveDate, usize)> =
                unsafe { row.data::<(NaiveDate, usize)>("hit").map(|p| *p.as_ref()) };
            if let Some((date, line)) = hit {
                s.go_to(date);
                s.scroll_to_line(line);
            }
        });

        let s = self.clone();
        self.search_entry.connect_search_changed(move |_| s.schedule_search());
        let s = self.clone();
        self.search_bar.connect_notify_local(Some("search-mode-enabled"), move |bar, _| {
            if bar.is_search_mode() {
                s.search_entry.grab_focus();
                s.run_search();
            } else {
                s.search_entry.set_text("");
                s.sidebar_stack.set_visible_child_name("days");
                s.view.grab_focus();
            }
        });

        // Closing hides rather than destroys: the app lives on in the tray,
        // and "Open in window" has to be able to bring this very window back.
        // Letting it be destroyed would leave a dangling handle behind.
        let s = self.clone();
        self.window.connect_close_request(move |w| {
            s.flush_save();
            w.set_visible(false);
            glib::Propagation::Stop
        });
    }

    pub fn present(self: &Rc<Self>) {
        self.window.present();
        self.focus_current_mode();
    }

    fn focus_current_mode(self: &Rc<Self>) {
        // Only the editor asks for focus: pressing `e` means "let me type".
        // Showing the rendered view deliberately leaves focus to GTK, so the
        // shortcut and the toggle button behave identically — clicking the
        // button focuses it without a focus ring, which grab_focus() from code
        // cannot reproduce anyway. Nothing in the rendered view can steal
        // focus now that its labels are not focusable.
        if self.view_stack.visible_child_name().as_deref() == Some("edit") {
            self.view.grab_focus();
        }
    }

    /// Switches between the rendered view and the editor.
    pub fn set_mode(self: &Rc<Self>, mode: &str) {
        if self.view_stack.visible_child_name().as_deref() != Some(mode) {
            if mode == "preview" {
                // The editor may hold unsaved keystrokes the rendered view
                // would otherwise not show.
                self.flush_save();
                self.render_preview();
            }
            self.view_stack.set_visible_child_name(mode);
        }
        let index = if mode == "edit" { 1 } else { 0 };
        if self.modes.active() != index {
            self.modes.set_active(index);
        }
        // Always, not only on a change: switching days also lands here with
        // the mode already current, and focus still has to be settled.
        self.focus_current_mode();
    }

    fn render_preview(self: &Rc<Self>) {
        let blocks = self.store.borrow().blocks(self.date.get()).unwrap_or_default();
        self.preview.render(&blocks);
    }

    /// Puts the cursor on a 0-based line and scrolls it into view — how a tray
    /// entry and, later, a search hit navigate to their source.
    pub fn scroll_to_line(self: &Rc<Self>, line: usize) {
        // Landing on a specific line is an editing intent.
        self.set_mode("edit");
        let Some(mut iter) = self.buffer.iter_at_line(line as i32) else {
            return;
        };
        self.buffer.place_cursor(&iter);
        // within_margin 0.15 leaves the line a little off the top edge
        self.view.scroll_to_iter(&mut iter, 0.15, false, 0.0, 0.0);
        self.view.grab_focus();
    }

    pub fn go_to(self: &Rc<Self>, date: NaiveDate) {
        if date == self.date.get() {
            return;
        }
        self.flush_save();
        self.date.set(date);
        self.load(date);
        self.refresh_sidebar();
    }

    /// Picks up a change another view made to the day on screen.
    ///
    /// Quick Note writes straight to the store, so an open main window showing
    /// the same day would otherwise hold a stale buffer and overwrite it on
    /// its next save.
    pub fn reload_if_showing(self: &Rc<Self>, date: NaiveDate) {
        if self.date.get() == date {
            self.flush_save();
            self.load(date);
            self.refresh_sidebar();
        }
    }

    fn load(self: &Rc<Self>, date: NaiveDate) {
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

        self.refresh_refs();
        // Selecting a day always shows it rendered; `e` or the toggle opens
        // the editor. Going through set_mode keeps focus handling in one
        // place — poking the stack directly left focus on the hidden editor,
        // and GTK then moved it to the first focusable widget, a label.
        self.render_preview();
        self.preview.scroll_to_top();
        self.set_mode("preview");
        if self.drawer.shows_sidebar() {
            self.activity.show_day(date);
        }
        self.title.set_title(&dates::short(date));
        let today = Local::now().date_naive();
        self.title.set_subtitle(&if date == today {
            gettext("Today")
        } else {
            dates::long(date)
        });
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
            std::time::Duration::from_millis(SAVE_DEBOUNCE_MS as u64),
            move || {
                s.pending_save.replace(None);
                s.save_now();
            },
        );
        self.pending_save.replace(Some(id));
    }

    pub fn flush_save(self: &Rc<Self>) {
        if let Some(id) = self.pending_save.borrow_mut().take() {
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
        self.refresh_sidebar();
        self.push_tray();
        self.refresh_refs();
        if self.view_stack.visible_child_name().as_deref() == Some("preview") {
            self.render_preview();
        }
    }

    /// Opens the search bar with the sidebar already switched to results.
    pub fn start_search(self: &Rc<Self>) {
        self.search_bar.set_search_mode(true);
        self.search_entry.grab_focus();
    }

    fn schedule_search(self: &Rc<Self>) {
        if let Some(id) = self.pending_search.borrow_mut().take() {
            id.remove();
        }
        let s = self.clone();
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
            s.pending_search.replace(None);
            s.run_search();
        });
        self.pending_search.replace(Some(id));
    }

    fn run_search(self: &Rc<Self>) {
        let query = self.search_entry.text().to_string();
        self.sidebar_stack.set_visible_child_name("results");

        while let Some(child) = self.results.first_child() {
            self.results.remove(&child);
        }

        if query.trim().is_empty() {
            self.results_status.set_visible(true);
            self.results_status.set_text(&gettext("Type to search your notes."));
            return;
        }

        // Search what is on screen too: the current day may not be saved yet.
        self.flush_save();

        let hits = match self.store.borrow().search(&query, 100) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(?e, "search failed");
                self.results_status.set_visible(true);
                self.results_status.set_text(&gettext("Search failed."));
                return;
            }
        };

        if hits.is_empty() {
            self.results_status.set_visible(true);
            // Translators: {} is the text the user searched for.
            self.results_status
                .set_text(&gettext("No notes match “{}”.").replace("{}", query.trim()));
            return;
        }

        self.results_status.set_visible(true);
        self.results_status.set_text(
            // Translators: {} is the number of search results.
            &crate::i18n::ngettext("{} result", "{} results", hits.len() as u32)
                .replace("{}", &hits.len().to_string()),
        );

        for hit in hits {
            let date = gtk::Label::new(Some(&dates::short(hit.date)));
            date.set_xalign(0.0);
            date.add_css_class("dim-label");
            date.add_css_class("caption");

            let text = gtk::Label::new(Some(&hit.text));
            text.set_xalign(0.0);
            text.set_wrap(true);
            text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            text.set_lines(3);
            text.set_ellipsize(gtk::pango::EllipsizeMode::End);

            let vbox = gtk::Box::new(gtk::Orientation::Vertical, 2);
            vbox.set_margin_top(8);
            vbox.set_margin_bottom(8);
            vbox.set_margin_start(12);
            vbox.set_margin_end(12);
            vbox.append(&date);
            vbox.append(&text);

            let row = gtk::ListBoxRow::builder().child(&vbox).activatable(true).build();
            unsafe {
                row.set_data("hit", (hit.date, hit.line));
            }
            self.results.append(&row);
        }
    }

    /// Re-reads references and repaints them. Cheap: it works from the cache.
    pub fn refresh_refs(self: &Rc<Self>) {
        if let Some(d) = self.refs.borrow().as_ref() {
            d.refresh();
        }
        if self.view_stack.visible_child_name().as_deref() == Some("preview") {
            self.render_preview();
        }
    }

    /// Hands newly-seen references to the background resolver.
    pub fn connect_resolve_refs(
        self: &Rc<Self>,
        f: impl Fn(Vec<crate::gitlab::refs::Reference>) + 'static,
    ) {
        let f = Rc::new(f);
        if let Some(d) = self.refs.borrow().as_ref() {
            let f = f.clone();
            d.connect_resolve(move |needed| f(needed));
        }
        self.preview.connect_resolve_refs(move |needed| f(needed));
    }

    /// Appends a line to the day on screen — used by the activity drawer.
    pub fn append_to_day(self: &Rc<Self>, line: &str) {
        self.flush_save();
        let date = self.date.get();
        let body = self.store.borrow().body(date).unwrap_or_default();
        let updated = crate::store::blocks::append_entry(&body, line);
        if let Err(e) = self.store.borrow_mut().save(date, &updated) {
            tracing::error!(?e, %date, "could not append to the day");
            return;
        }
        self.load(date);
        self.refresh_sidebar();
        self.push_tray();
    }

    /// Repaints the drawer, e.g. after its events were fetched.
    pub fn refresh_activity(self: &Rc<Self>) {
        if self.drawer.shows_sidebar() {
            self.activity.render(self.date.get());
        }
    }

    pub fn connect_fetch_activity(self: &Rc<Self>, f: impl Fn(NaiveDate) + 'static) {
        self.activity.connect_refresh(f);
        // The drawer restores its state while the window is being built, which
        // is before this callback exists — so that first request would go
        // nowhere. Make it now that there is someone to answer it.
        if self.drawer.shows_sidebar() {
            self.activity.show_day(self.date.get());
        }
    }

    /// Asks for the issue list to be refreshed, at most occasionally.
    fn request_issue_sync(self: &Rc<Self>) {
        if let Some(cb) = self.on_sync_issues.borrow().as_ref() {
            cb();
        }
    }

    pub fn connect_sync_issues(self: &Rc<Self>, f: impl Fn() + 'static) {
        *self.on_sync_issues.borrow_mut() = Some(Box::new(f));
    }

    /// Keeps the tray menu in step with what was just written.
    fn push_tray(self: &Rc<Self>) {
        if let Some(tray) = &self.tray {
            tray.push(crate::app::snapshot(&self.store.borrow()));
        }
    }

    fn refresh_sidebar(self: &Rc<Self>) {
        while let Some(child) = self.sidebar.first_child() {
            self.sidebar.remove(&child);
        }
        let today = Local::now().date_naive();
        let current = self.date.get();

        let mut days = match self.store.borrow().recent(60) {
            Ok(d) => d,
            Err(e) => {
                tracing::error!(?e, "could not list recent days");
                Vec::new()
            }
        };
        // The day being edited belongs in the list even before it has content.
        if !days.contains(&current) {
            days.push(current);
            days.sort_unstable_by(|a, b| b.cmp(a));
        }

        for d in days {
            let label = gtk::Label::new(Some(&dates::short(d)));
            label.set_xalign(0.0);
            let row = gtk::ListBoxRow::builder().child(&label).build();
            if d == today {
                label.set_markup(&format!("<b>{}</b>", glib::markup_escape_text(&dates::short(d))));
            }
            unsafe {
                row.set_data("date", d.format(crate::store::days::DATE_FMT).to_string());
            }
            self.sidebar.append(&row);
            if d == current {
                self.sidebar.select_row(Some(&row));
            }
        }
    }
}
