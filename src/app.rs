//! Process wiring: one GTK main loop, one tokio runtime for the tray.
//!
//! Everything with a side effect — opening the database, owning the tray's
//! D-Bus name — happens in `startup`, which GTK fires **only on the primary
//! instance**. Doing it in `main` would make a second `daily-notes
//! --quick-note` stand up a rival tray and a second database handle before
//! discovering it should have forwarded its arguments instead.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use anyhow::Result;
use chrono::Local;
use gtk::gio;
use gtk::glib;
use adw::prelude::*;

use crate::gitlab::{client::Client, ResolveBatch, Resolution};
use crate::store::Store;
use crate::tray::{DailyNotesTray, Entry, Section, TrayCommand, TraySnapshot};
use crate::ui::{dates, quicknote::QuickNote, window::MainWindow};

pub const APP_ID: &str = "de.ztm.DailyNotes";

/// `meta` key holding which tray section the user last worked with.
const TRAY_SECTION: &str = "tray_section";

/// Handle on the tray, usable from the GTK thread.
#[derive(Clone)]
pub struct TrayLink {
    rt: Rc<tokio::runtime::Runtime>,
    /// `None` when the tray could not be created at all. The app is still a
    /// perfectly good notes app without it, so this is never fatal.
    handle: Option<ksni::Handle<DailyNotesTray>>,
}

impl TrayLink {
    pub fn push(&self, snapshot: TraySnapshot) {
        let Some(handle) = self.handle.clone() else {
            return;
        };
        // ksni's update() is async and must run on the runtime that owns the
        // tray's D-Bus connection.
        self.rt.spawn(async move {
            handle
                .update(move |t: &mut DailyNotesTray| t.set_snapshot(snapshot))
                .await;
        });
    }

    pub fn shutdown(&self) {
        if let Some(h) = &self.handle {
            h.shutdown();
        }
    }
}

/// Builds the snapshot the tray draws itself from.
///
/// Runs on the GTK thread because it reads the store and formats dates with
/// `glib::DateTime`, which is what makes the labels follow `LC_TIME`.
pub fn snapshot(store: &Store) -> TraySnapshot {
    let today = Local::now().date_naive();

    let entries_of = |date| -> Vec<Entry> {
        store
            .summary(date)
            .unwrap_or_default()
            .into_iter()
            .map(|b| Entry {
                // The dbusmenu protocol carries plain strings — no markup, no
                // links, no tooltips — so the best the tray can do is show the
                // text a person wrote rather than its markdown source.
                text: crate::ui::markup::to_plain(&b.text),
                line: b.line_start,
            })
            .collect()
    };

    let last = store.last_workday(today).ok().flatten();
    // Which section was last used, remembered across restarts. With no earlier
    // notes there is nothing to expand but today.
    let expanded = match store.meta_get(TRAY_SECTION).as_deref().map(Section::parse) {
        Some(Section::LastWorkday) if last.is_some() => Section::LastWorkday,
        _ => Section::Today,
    };

    TraySnapshot {
        today: Some(today),
        // Translators: tray submenu, {} is a date like "Wed 23 Sep".
        today_label: crate::i18n::gettext("Today · {}").replace("{}", &dates::short(today)),
        today_entries: entries_of(today),
        last_workday: last,
        last_workday_label: match last {
            // Translators: tray submenu, {} is a date like "Tue 22 Sep".
            Some(d) => crate::i18n::gettext("Last workday · {}").replace("{}", &dates::short(d)),
            None => crate::i18n::gettext("No earlier notes"),
        },
        last_workday_entries: last.map(entries_of).unwrap_or_default(),
        expanded,
    }
}

/// Everything the primary instance owns. A remote invocation has none of it.
struct Session {
    store: Rc<RefCell<Store>>,
    link: TrayLink,
    main_window: RefCell<Option<Rc<MainWindow>>>,
    quick_note: RefCell<Option<Rc<QuickNote>>>,
    /// Without a live hold guard the app would quit the moment its last window
    /// closes — but the tray icon must outlive every window. Dropping this
    /// guard is how Quit actually releases the process.
    hold: RefCell<Option<gio::ApplicationHoldGuard>>,
    /// References the editor has seen but not resolved, queued for the
    /// background resolver on the tokio side.
    resolve_tx: async_channel::Sender<ResolveBatch>,
    /// The GitLab token, read from the keyring on this thread and cached.
    /// The resolver must never read it itself — see `ResolveBatch`.
    token: RefCell<Option<String>>,
    /// Asks for the issue list behind `#` completion to be refreshed.
    issues_tx: async_channel::Sender<ResolveBatch>,
    /// Asks for a day's GitLab activity to be fetched.
    events_tx: async_channel::Sender<(chrono::NaiveDate, ResolveBatch)>,
    /// When it was last refreshed, so typing does not hammer the API.
    issues_synced: Cell<Option<std::time::Instant>>,
}

impl Session {
    fn main_window(self: &Rc<Self>, app: &adw::Application) -> Rc<MainWindow> {
        if let Some(w) = self.main_window.borrow().as_ref() {
            return w.clone();
        }
        let w = MainWindow::new(app, self.store.clone(), Some(self.link.clone()));
        {
            let s = self.clone();
            w.connect_resolve_refs(move |needed| s.request_resolve(needed));
        }
        {
            let s = self.clone();
            w.connect_sync_issues(move || s.sync_issues());
        }
        {
            let s = self.clone();
            w.connect_fetch_activity(move |date| s.fetch_activity(date));
        }
        {
            let s = self.clone();
            w.connect_saved(move |date| {
                if let Some(q) = s.quick_note.borrow().as_ref() {
                    q.reload_if_showing(date);
                }
            });
        }
        *self.main_window.borrow_mut() = Some(w.clone());
        w
    }

    fn quick_note(self: &Rc<Self>, app: &adw::Application) -> Rc<QuickNote> {
        if let Some(q) = self.quick_note.borrow().as_ref() {
            return q.clone();
        }
        let q = QuickNote::build(app, self.store.clone());
        {
            // A Quick Note write must reach the tray and any open main window,
            // or the three views drift apart.
            let s = self.clone();
            q.connect_changed(move |date| {
                s.link.push(snapshot(&s.store.borrow()));
                if let Some(w) = s.main_window.borrow().as_ref() {
                    w.reload_if_showing(date);
                }
            });
        }
        {
            let s = self.clone();
            let app = app.clone();
            q.connect_open_window(move |date| {
                let w = s.main_window(&app);
                w.go_to(date);
                w.present();
            });
        }
        {
            let s = self.clone();
            q.connect_resolve_refs(move |needed| s.request_resolve(needed));
        }
        {
            let s = self.clone();
            q.connect_sync_issues(move || s.sync_issues());
        }
        {
            let s = self.clone();
            let app = app.clone();
            q.connect_search(move |date| {
                let w = s.main_window(&app);
                w.go_to(date);
                w.present();
                w.start_search();
            });
        }
        *self.quick_note.borrow_mut() = Some(q.clone());
        q
    }

    fn show_settings(self: &Rc<Self>, parent: &adw::ApplicationWindow) {
        let store = self.store.clone();
        let s = self.clone();
        let settings = crate::ui::settings::Settings::build(&self.link.rt, move || {
            // Resolutions from another instance or another token mean nothing.
            if let Err(e) = store.borrow().ref_cache_clear() {
                tracing::warn!(?e, "could not clear the reference cache");
            }
            s.reload_token();
        });
        {
            let s = self.clone();
            settings.connect_saved(move || {
                s.reload_token();
                s.issues_synced.set(None);
                if let Some(w) = s.main_window.borrow().as_ref() {
                    w.refresh_refs();
                }
            });
        }
        settings.dialog.present(Some(parent));
    }

    /// Refreshes the issue list behind `#` completion, at most every few
    /// minutes — it is requested on every keystroke after a `#`.
    fn sync_issues(&self) {
        const EVERY: std::time::Duration = std::time::Duration::from_secs(300);
        if let Some(last) = self.issues_synced.get() {
            if last.elapsed() < EVERY {
                return;
            }
        }
        let cfg = crate::config::load();
        let Some(token) = self.token.borrow().clone() else {
            return;
        };
        if !cfg.gitlab.is_configured() {
            return;
        }
        self.issues_synced.set(Some(std::time::Instant::now()));
        let _ = self.issues_tx.force_send(ResolveBatch {
            refs: Vec::new(),
            base: cfg.gitlab.base().to_string(),
            token,
            main_project: cfg.gitlab.main_project.clone(),
            main_group: cfg.gitlab.main_group.clone(),
        });
    }

    /// Fetches one day's GitLab activity.
    fn fetch_activity(&self, date: chrono::NaiveDate) {
        let cfg = crate::config::load();
        let Some(token) = self.token.borrow().clone() else {
            return;
        };
        if !cfg.gitlab.is_configured() {
            return;
        }
        let _ = self.events_tx.force_send((
            date,
            ResolveBatch {
                refs: Vec::new(),
                base: cfg.gitlab.base().to_string(),
                token,
                main_project: cfg.gitlab.main_project.clone(),
                main_group: cfg.gitlab.main_group.clone(),
            },
        ));
    }

    /// Records which section a day belongs to, so the tray menu reopens there.
    fn remember_section(&self, date: chrono::NaiveDate) {
        let today = Local::now().date_naive();
        let section = if date == today {
            Section::Today
        } else {
            Section::LastWorkday
        };
        let store = self.store.borrow();
        if store.meta_get(TRAY_SECTION).as_deref() == Some(section.as_str()) {
            return;
        }
        if let Err(e) = store.meta_set(TRAY_SECTION, section.as_str()) {
            tracing::warn!(?e, "could not remember the tray section");
            return;
        }
        drop(store);
        // Rebuild now, so the next time the menu opens it is already on the
        // section just used rather than waiting for the next save.
        self.link.push(snapshot(&self.store.borrow()));
    }

    /// Queues references for the background resolver.
    ///
    /// The token is read here, on the GTK thread, and travels with the batch —
    /// the resolver must never touch the keyring itself.
    fn request_resolve(&self, needed: Vec<crate::gitlab::refs::Reference>) {
        if needed.is_empty() {
            return;
        }
        let cfg = crate::config::load();
        let Some(token) = self.token.borrow().clone() else {
            return;
        };
        if !cfg.gitlab.is_configured() {
            return;
        }
        // Unbounded, so the editor never blocks on a slow instance.
        let _ = self.resolve_tx.force_send(ResolveBatch {
            refs: needed,
            base: cfg.gitlab.base().to_string(),
            token,
            main_project: cfg.gitlab.main_project.clone(),
            main_group: cfg.gitlab.main_group.clone(),
        });
    }

    /// Re-reads the token from the keyring. Called at startup and whenever
    /// Settings writes a new one.
    fn reload_token(&self) {
        *self.token.borrow_mut() = crate::config::token();
    }

    fn keep_alive(&self, app: &adw::Application) {
        self.hold.borrow_mut().get_or_insert_with(|| app.hold());
    }

    fn flush(&self) {
        if let Some(w) = self.main_window.borrow().as_ref() {
            w.flush_save();
        }
        if let Some(q) = self.quick_note.borrow().as_ref() {
            q.flush_save();
        }
    }
}

pub fn run() -> Result<glib::ExitCode> {
    // HANDLES_COMMAND_LINE makes a second `daily-notes --quick-note` hand its
    // arguments to the already-running instance instead of starting a rival.
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    let session: Rc<RefCell<Option<Rc<Session>>>> = Rc::new(RefCell::new(None));

    {
        let session = session.clone();
        app.connect_startup(move |app| {
            tracing::info!(pid = std::process::id(), "primary instance starting up");
            match start_session(app) {
                Ok(s) => *session.borrow_mut() = Some(s),
                Err(e) => {
                    tracing::error!("{e:#}");
                    eprintln!("daily-notes: {e:#}");
                    app.quit();
                }
            }
        });
    }

    {
        let session = session.clone();
        app.connect_activate(move |app| {
            let Some(s) = session.borrow().clone() else {
                return;
            };
            s.keep_alive(app);
            s.main_window(app).present();
            s.link.push(snapshot(&s.store.borrow()));
        });
    }

    {
        let session = session.clone();
        app.connect_command_line(move |app, cl| {
            let wants_quick = cl
                .arguments()
                .iter()
                .any(|a| a.to_string_lossy() == "--quick-note");
            tracing::info!(pid = std::process::id(), wants_quick, "command line");
            let Some(s) = session.borrow().clone() else {
                return glib::ExitCode::FAILURE;
            };
            s.keep_alive(app);
            if wants_quick {
                s.quick_note(app).present_for(Local::now().date_naive());
            } else {
                app.activate();
            }
            glib::ExitCode::SUCCESS
        });
    }

    Ok(app.run())
}

fn start_session(app: &adw::Application) -> Result<Rc<Session>> {
    crate::ui::init_icons();
    crate::ui::load_css();

    let path = crate::store::default_path()?;
    let store = Store::open(&path)?;
    tracing::info!(db = %path.display(), "store ready");

    let rt = Rc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("daily-notes-io")
            .build()?,
    );

    let (tx, rx) = async_channel::unbounded::<TrayCommand>();
    // GTK -> tokio: references to look up. tokio -> GTK: what they resolved to.
    let (resolve_tx, resolve_rx) = async_channel::unbounded::<ResolveBatch>();
    let (resolved_tx, resolved_rx) = async_channel::unbounded::<Vec<Resolution>>();
    // Issue list for `#` completion: a request carries the same context a
    // resolve does, and comes back as a whole replacement list.
    let (issues_tx, issues_rx) = async_channel::unbounded::<ResolveBatch>();
    let (issues_done_tx, issues_done_rx) =
        async_channel::unbounded::<(String, Vec<crate::gitlab::client::IssueBrief>)>();
    let (events_tx, events_rx) =
        async_channel::unbounded::<(chrono::NaiveDate, ResolveBatch)>();
    let (events_done_tx, events_done_rx) = async_channel::unbounded::<(
        chrono::NaiveDate,
        Vec<crate::gitlab::events::EventRec>,
        Vec<crate::gitlab::client::ProjectBrief>,
    )>();

    // A tray app is normally ended by the session manager or a `kill`, not by
    // closing a window — so the debounced save has to survive a signal too.
    // Routing through the command channel means the flush happens on the GTK
    // thread, where the buffer actually lives.
    {
        let tx = tx.clone();
        rt.spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            let mut term = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => return tracing::warn!(?e, "cannot watch SIGTERM"),
            };
            let mut int = match signal(SignalKind::interrupt()) {
                Ok(s) => s,
                Err(e) => return tracing::warn!(?e, "cannot watch SIGINT"),
            };
            tokio::select! {
                _ = term.recv() => tracing::info!("SIGTERM"),
                _ = int.recv()  => tracing::info!("SIGINT"),
            }
            let _ = tx.force_send(TrayCommand::Quit);
        });
    }

    let tray = DailyNotesTray::new(crate::tray::icon_path(), tx.clone());
    // assume_sni_available turns "no StatusNotifierWatcher yet" into a soft
    // error routed to Tray::watcher_offline, so the icon appears by itself
    // once the shell (or the appindicator extension) comes back. Starting
    // before the desktop is ready, or while the extension is reloading, is
    // normal — and either way a missing tray must never stop note-taking,
    // hence the non-fatal fallback below.
    let handle = match rt.block_on(async {
        use ksni::TrayMethods;
        tray.assume_sni_available(true).spawn().await
    }) {
        Ok(h) => {
            tracing::info!("tray registered");
            Some(h)
        }
        Err(e) => {
            tracing::warn!(?e, "no tray icon; continuing without one");
            None
        }
    };

    // The resolver: one task, fed by the editor, writing back what it learns.
    // Building the client per batch means a token or instance change in
    // Settings takes effect on the next keystroke with no restart.
    rt.spawn(async move {
        while let Ok(batch) = resolve_rx.recv().await {
            let client = match Client::new(&batch.base, &batch.token) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(?e, "could not build the GitLab client");
                    continue;
                }
            };

            let mut out = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for r in &batch.refs {
                let key = r.cache_key(&batch.main_project);
                if !seen.insert(key.clone()) {
                    continue;
                }
                match client
                    .resolve(r, &batch.main_project, &batch.main_group)
                    .await
                {
                    Ok(resolved) => out.push(Resolution {
                        key,
                        kind: r.kind.as_str(),
                        resolved,
                    }),
                    Err(e) => {
                        // A network blip must not poison the cache with a
                        // "not found" — leave the entry alone and retry later.
                        tracing::warn!(?e, %key, "could not resolve reference");
                    }
                }
            }
            if !out.is_empty() {
                let _ = resolved_tx.force_send(out);
            }
        }
    });

    rt.spawn(async move {
        while let Ok(req) = issues_rx.recv().await {
            let client = match Client::new(&req.base, &req.token) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(?e, "could not build the GitLab client");
                    continue;
                }
            };
            // Three pages is plenty to complete against without turning a
            // keystroke into a long sync on a project with hundreds of issues.
            match client.open_issues(&req.main_project, 3).await {
                Ok(issues) => {
                    tracing::debug!(count = issues.len(), project = %req.main_project, "issue list refreshed");
                    let _ = issues_done_tx.force_send((req.main_project, issues));
                }
                Err(e) => tracing::warn!(?e, "could not list issues"),
            }
        }
    });

    // The day's activity, plus whatever projects it mentions: events carry a
    // numeric project_id and no path, so the paths are fetched alongside and
    // cached — that table is effectively permanent.
    {
        let events_done_tx = events_done_tx.clone();
        rt.spawn(async move {
            let mut known: std::collections::HashSet<u64> = std::collections::HashSet::new();
            while let Ok((date, req)) = events_rx.recv().await {
                let client = match Client::new(&req.base, &req.token) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(?e, "could not build the GitLab client");
                        continue;
                    }
                };
                let events = match crate::gitlab::events::for_day(&client, date).await {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!(?e, %date, "could not fetch the day's activity");
                        continue;
                    }
                };
                let mut projects = Vec::new();
                let mut wanted: Vec<u64> = events.iter().map(|e| e.project_id).collect();
                wanted.sort_unstable();
                wanted.dedup();
                for id in wanted {
                    if !known.insert(id) {
                        continue;
                    }
                    match client.project(id).await {
                        Ok(Some(p)) => projects.push(p),
                        Ok(None) => {}
                        Err(e) => tracing::warn!(?e, id, "could not resolve a project"),
                    }
                }
                tracing::debug!(%date, count = events.len(), "activity fetched");
                let _ = events_done_tx.force_send((date, events, projects));
            }
        });
    }

    let session = Rc::new(Session {
        store: Rc::new(RefCell::new(store)),
        link: TrayLink { rt, handle },
        main_window: RefCell::new(None),
        quick_note: RefCell::new(None),
        hold: RefCell::new(None),
        resolve_tx,
        token: RefCell::new(None),
        issues_tx,
        events_tx,
        issues_synced: Cell::new(None),
    });
    session.reload_token();
    session.link.push(snapshot(&session.store.borrow()));

    // Exported on D-Bus by GtkApplication, so `gapplication action
    // de.ztm.DailyNotes quick-note` drives the running instance too.
    {
        let action = gio::SimpleAction::new("quick-note", None);
        let tx = tx.clone();
        action.connect_activate(move |_, _| {
            let _ = tx.force_send(TrayCommand::QuickNote);
        });
        app.add_action(&action);
    }

    {
        let s = session.clone();
        glib::spawn_future_local(async move {
            while let Ok((project, issues)) = issues_done_rx.recv().await {
                if let Err(e) = s.store.borrow_mut().put_issues(&project, &issues) {
                    tracing::warn!(?e, "could not store the issue list");
                }
            }
        });
    }

    {
        let s = session.clone();
        glib::spawn_future_local(async move {
            while let Ok((date, events, projects)) = events_done_rx.recv().await {
                {
                    let store = s.store.borrow();
                    for p in &projects {
                        if let Err(e) = store.put_project(p) {
                            tracing::warn!(?e, "could not cache a project");
                        }
                    }
                }
                if let Err(e) = s.store.borrow_mut().put_events(date, &events) {
                    tracing::warn!(?e, "could not cache the day's activity");
                }
                if let Some(w) = s.main_window.borrow().as_ref() {
                    w.refresh_activity();
                }
            }
        });
    }

    // Fold resolved references into the cache and repaint.
    {
        let s = session.clone();
        glib::spawn_future_local(async move {
            while let Ok(batch) = resolved_rx.recv().await {
                {
                    let store = s.store.borrow();
                    for r in &batch {
                        if let Err(e) = store.ref_cache_put(&r.key, r.kind, r.resolved.as_ref()) {
                            tracing::warn!(?e, key = %r.key, "could not cache reference");
                        }
                    }
                }
                if let Some(w) = s.main_window.borrow().as_ref() {
                    w.refresh_refs();
                }
                if let Some(q) = s.quick_note.borrow().as_ref() {
                    q.refresh();
                }
                s.link.push(snapshot(&s.store.borrow()));
            }
        });
    }

    // Drain tray commands on the GTK main loop.
    {
        let app = app.clone();
        let s = session.clone();
        glib::spawn_future_local(async move {
            while let Ok(cmd) = rx.recv().await {
                tracing::debug!(?cmd, "tray command");
                s.keep_alive(&app);
                // A panic while handling one command used to kill this loop
                // for good: the tray kept drawing its menu, every click did
                // nothing, and even SIGTERM stopped working because shutdown
                // is routed through here. One failed click is survivable; a
                // permanently deaf UI is not.
                let handled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                match cmd {
                    TrayCommand::QuickNote => {
                        s.quick_note(&app).present_for(Local::now().date_naive());
                    }
                    TrayCommand::OpenDay(date, line) => {
                        // Touching a day makes its section the expanded one, so
                        // the menu opens where you were last working.
                        s.remember_section(date);
                        let w = s.main_window(&app);
                        w.go_to(date);
                        if let Some(line) = line {
                            w.scroll_to_line(line);
                        }
                        w.present();
                    }
                    TrayCommand::Search => {
                        let w = s.main_window(&app);
                        w.present();
                        w.start_search();
                    }
                    TrayCommand::Settings => {
                        let w = s.main_window(&app);
                        w.present();
                        s.show_settings(&w.window);
                    }
                    TrayCommand::Quit => {
                        s.flush();
                        s.link.shutdown();
                        s.hold.borrow_mut().take();
                        app.quit();
                        return true;
                    }
                }
                false
                }));
                match handled {
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(_) => tracing::error!("panic while handling a tray command; continuing"),
                }
            }
            tracing::debug!("tray command loop finished");
        });
    }

    Ok(session)
}
