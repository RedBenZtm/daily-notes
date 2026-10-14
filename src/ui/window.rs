//! The main window: day navigation, a sidebar of recent days, and the editor.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use chrono::{Duration, Local, NaiveDate};
use gtk::prelude::*;
use gtk::glib;

use crate::i18n::gettext;
use crate::app::TrayLink;
use crate::store::Store;
use crate::ui::day_pane::DayPane;
use crate::ui::{activity::Activity, dates};

/// `meta` key holding whether the activity drawer was left open.
const ACTIVITY_DRAWER: &str = "activity_drawer";

pub struct MainWindow {
    pub window: adw::ApplicationWindow,
    store: Rc<RefCell<Store>>,
    date: Rc<Cell<NaiveDate>>,
    title: adw::WindowTitle,
    sidebar: gtk::ListBox,
    tray: Option<TrayLink>,
    search_bar: gtk::SearchBar,
    search_entry: gtk::SearchEntry,
    sidebar_stack: gtk::Stack,
    results: gtk::ListBox,
    results_status: gtk::Label,
    /// Debounces the query so each keystroke does not hit SQLite.
    pending_search: Rc<RefCell<Option<glib::SourceId>>>,
    /// The day itself, rendered or in source.
    pane: Rc<DayPane>,
    /// Called after each write, so the Quick Note can catch up.
    on_saved: RefCell<Option<Box<dyn Fn(NaiveDate)>>>,
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

        let pane = DayPane::new(store.clone());

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
            .content(&pane.widget)
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
            .default_width(1080)
            .default_height(640)
            .title(gettext("Daily Notes"))
            .content(&split)
            .build();

        let this = Rc::new(Self {
            window,
            store,
            date: Rc::new(Cell::new(Local::now().date_naive())),
            title,
            sidebar,
            tray,
            search_bar: search_bar.clone(),
            search_entry: search_entry.clone(),
            sidebar_stack,
            results,
            results_status,
            pending_search: Rc::new(RefCell::new(None)),
            pane,
            on_saved: RefCell::new(None),
            activity,
            drawer,
        });

        {
            let s = this.clone();
            this.pane.connect_saved(move |date| {
                s.refresh_sidebar();
                s.push_tray();
                if let Some(cb) = s.on_saved.borrow().as_ref() {
                    cb(date);
                }
            });
        }
        this.pane.add_edit_shortcut(&this.window);

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
                if let Some(id) = s.pending_search.borrow_mut().take() {
                    id.remove();
                }
                s.search_entry.set_text("");
                s.sidebar_stack.set_visible_child_name("days");
                s.pane.focus_current_mode();
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
        self.pane.focus_current_mode();
    }

    /// Puts the cursor on a 0-based line and scrolls it into view — how a tray
    /// entry and a search hit navigate to their source.
    pub fn scroll_to_line(self: &Rc<Self>, line: usize) {
        self.pane.scroll_to_line(line);
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
        self.pane.load(date);
        if self.drawer.shows_sidebar() {
            self.activity.show_day(date);
        }
        // The full date sits right below, so the title needs no tooltip.
        self.title.set_title(&dates::relative(date, Local::now().date_naive()));
        self.title.set_subtitle(&dates::long(date));
    }

    pub fn flush_save(self: &Rc<Self>) {
        self.pane.flush_save();
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
        // Closing the bar clears the entry, and that change arrives as a
        // search like any other — sometimes after the bar is already gone.
        // Running it would flip the sidebar back to results with no bar open.
        if !self.search_bar.is_search_mode() {
            return;
        }
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

        let today = Local::now().date_naive();
        for hit in hits {
            let date = gtk::Label::new(Some(&dates::relative(hit.date, today)));
            date.set_tooltip_text(Some(&dates::long(hit.date)));
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
        self.pane.refresh_refs();
    }

    /// Hands newly-seen references to the background resolver.
    pub fn connect_resolve_refs(
        self: &Rc<Self>,
        f: impl Fn(Vec<crate::gitlab::refs::Reference>) + 'static,
    ) {
        self.pane.connect_resolve_refs(f);
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

    pub fn connect_saved(self: &Rc<Self>, f: impl Fn(NaiveDate) + 'static) {
        *self.on_saved.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_sync_issues(self: &Rc<Self>, f: impl Fn() + 'static) {
        self.pane.connect_sync_issues(f);
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
            let text = dates::relative(d, today);
            let label = gtk::Label::new(Some(&text));
            label.set_xalign(0.0);
            label.set_tooltip_text(Some(&dates::long(d)));
            let row = gtk::ListBoxRow::builder().child(&label).build();
            if d == today {
                label.set_markup(&format!("<b>{}</b>", glib::markup_escape_text(&text)));
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
