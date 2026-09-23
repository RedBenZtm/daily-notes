//! The GNOME tray icon (StatusNotifierItem), verified against the
//! `appindicatorsupport@rgcjonas.gmail.com` extension.
//!
//! Deliberately free of GTK types: the tray runs on the tokio runtime thread
//! while GTK owns the main thread. The two exchange a pre-rendered
//! [`TraySnapshot`] one way and [`TrayCommand`]s the other, so neither side
//! ever touches the other's objects.

use chrono::NaiveDate;
use ksni::menu::{StandardItem, SubMenu};
use ksni::{Icon, MenuItem, Status, ToolTip, Tray};

use crate::i18n::{gettext, ngettext};

/// What the tray asks the UI to do. Sent across threads, handled on the GTK
/// main loop.
#[derive(Debug, Clone)]
pub enum TrayCommand {
    QuickNote,
    /// Open the main window on a day, optionally scrolled to a line.
    OpenDay(NaiveDate, Option<usize>),
    Search,
    Settings,
    Quit,
}

/// Which day the menu shows expanded.
///
/// A dbusmenu submenu cannot be told to open — the protocol carries only
/// `visible`, `enabled`, `label`, `type`, `children-display`, `icon-name`,
/// `icon-data`, `toggle-type` and `toggle-state`. So "expanded" is done by
/// listing that day's entries inline at the top level and leaving the other
/// day as a submenu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Section {
    #[default]
    Today,
    LastWorkday,
}

impl Section {
    pub fn as_str(self) -> &'static str {
        match self {
            Section::Today => "today",
            Section::LastWorkday => "last_workday",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "last_workday" => Section::LastWorkday,
            _ => Section::Today,
        }
    }
}

/// One entry as the tray shows it.
#[derive(Debug, Clone)]
pub struct Entry {
    pub text: String,
    pub line: usize,
}

/// Everything the tray needs to draw itself.
///
/// Date labels arrive pre-formatted because `LC_TIME` formatting goes through
/// `glib::DateTime` on the UI side; the tray never formats a date itself.
#[derive(Debug, Clone, Default)]
pub struct TraySnapshot {
    pub today: Option<NaiveDate>,
    pub today_label: String,
    pub today_entries: Vec<Entry>,
    pub last_workday: Option<NaiveDate>,
    pub last_workday_label: String,
    pub last_workday_entries: Vec<Entry>,
    /// Which section is listed inline; the other becomes a submenu.
    pub expanded: Section,
}

pub struct DailyNotesTray {
    icon: String,
    snapshot: TraySnapshot,
    tx: async_channel::Sender<TrayCommand>,
}

impl DailyNotesTray {
    pub fn new(icon: String, tx: async_channel::Sender<TrayCommand>) -> Self {
        Self {
            icon,
            snapshot: TraySnapshot::default(),
            tx,
        }
    }

    pub fn set_snapshot(&mut self, snapshot: TraySnapshot) {
        self.snapshot = snapshot;
    }

    fn send(&self, cmd: TrayCommand) {
        // force_send keeps the menu responsive even if the UI is mid-frame;
        // the channel is unbounded so this cannot actually fail unless the UI
        // is gone, in which case there is nothing to do anyway.
        if let Err(e) = self.tx.force_send(cmd) {
            tracing::warn!(?e, "tray command dropped: UI receiver is gone");
        }
    }

    /// Tray menu labels are single-line and unwrapped; the shell truncates
    /// mid-glyph if we do not do it ourselves.
    fn elide(s: &str, max: usize) -> String {
        if s.chars().count() <= max {
            return s.to_string();
        }
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", kept.trim_end())
    }

    /// The entries of one day, plus the action to open it whole.
    fn day_items(&self, date: NaiveDate, entries: &[Entry]) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = entries
            .iter()
            .take(12)
            .map(|e| {
                let line = e.line;
                StandardItem {
                    label: Self::elide(&e.text, 70),
                    activate: Box::new(move |t: &mut Self| {
                        t.send(TrayCommand::OpenDay(date, Some(line)));
                    }),
                    ..Default::default()
                }
                .into()
            })
            .collect();

        if items.is_empty() {
            items.push(
                StandardItem {
                    label: gettext("(no entries)"),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }

        items
    }

    /// A day listed inline, under a heading that opens it in the window.
    ///
    /// The heading carries the action rather than a separate row below the
    /// entries. A dbusmenu item is an icon plus a label and nothing else —
    /// there is no trailing widget — so the icon sits where the protocol puts
    /// icons, on the left.
    fn day_inline(&self, label: &str, date: Option<NaiveDate>, entries: &[Entry]) -> Vec<MenuItem<Self>> {
        let Some(date) = date else {
            return vec![StandardItem {
                label: label.into(),
                enabled: false,
                ..Default::default()
            }
            .into()];
        };

        let mut out: Vec<MenuItem<Self>> = vec![StandardItem {
            label: label.into(),
            icon_name: "window-new-symbolic".into(),
            activate: Box::new(move |t: &mut Self| {
                t.send(TrayCommand::OpenDay(date, None));
            }),
            ..Default::default()
        }
        .into()];
        out.extend(self.day_items(date, entries));
        out
    }

    fn day_submenu(&self, label: &str, date: Option<NaiveDate>, entries: &[Entry]) -> MenuItem<Self> {
        let Some(date) = date else {
            return SubMenu {
                label: label.into(),
                enabled: false,
                ..Default::default()
            }
            .into();
        };

        // A submenu's own label opens the submenu, so it cannot double as the
        // "open this day" action the inline heading carries. Putting the action
        // first inside instead keeps both days looking the same: the open
        // affordance leads, with the entries under it.
        let mut items: Vec<MenuItem<Self>> = vec![StandardItem {
            label: gettext("Open in window"),
            icon_name: "window-new-symbolic".into(),
            activate: Box::new(move |t: &mut Self| {
                t.send(TrayCommand::OpenDay(date, None));
            }),
            ..Default::default()
        }
        .into()];
        items.extend(self.day_items(date, entries));

        SubMenu {
            label: label.into(),
            submenu: items,
            ..Default::default()
        }
        .into()
    }
}

impl Tray for DailyNotesTray {
    // Keep `Activate` live so a double-click opens Quick Note. Setting this to
    // true would only make Activate return UnknownMethod — the GNOME extension
    // decides left-click behaviour from introspection, which zbus emits either
    // way, so it would cost the double-click without buying anything.
    const MENU_ON_ACTIVATE: bool = false;

    fn id(&self) -> String {
        "daily-notes".into()
    }

    fn title(&self) -> String {
        gettext("Daily Notes")
    }

    fn icon_name(&self) -> String {
        self.icon.clone()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        Vec::new()
    }

    fn status(&self) -> Status {
        // Anything but Active is created invisible by the extension.
        Status::Active
    }

    fn tool_tip(&self) -> ToolTip {
        let n = self.snapshot.today_entries.len();
        ToolTip {
            title: gettext("Daily Notes"),
            // Translators: {} is the number of note entries written today.
            description: ngettext("{} entry today", "{} entries today", n as u32)
                .replace("{}", &n.to_string()),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayCommand::QuickNote);
    }

    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        // Staying alive is what makes the icon come back by itself after a
        // gnome-shell restart or an extension toggle.
        tracing::warn!(?reason, "watcher offline; waiting to re-register");
        true
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let s = &self.snapshot;
        let mut items: Vec<MenuItem<Self>> = vec![
            StandardItem {
                label: gettext("Quick Note"),
                activate: Box::new(|t: &mut Self| t.send(TrayCommand::QuickNote)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
        ];

        // The chosen day is listed inline — the only way to show a day already
        // "open", since a submenu cannot be told to expand.
        let (inline, collapsed) = match s.expanded {
            Section::Today => (
                self.day_inline(&s.today_label, s.today, &s.today_entries),
                self.day_submenu(&s.last_workday_label, s.last_workday, &s.last_workday_entries),
            ),
            Section::LastWorkday => (
                self.day_inline(&s.last_workday_label, s.last_workday, &s.last_workday_entries),
                self.day_submenu(&s.today_label, s.today, &s.today_entries),
            ),
        };
        items.extend(inline);
        items.push(MenuItem::Separator);
        items.push(collapsed);

        items.extend([
            MenuItem::Separator,
            StandardItem {
                label: gettext("Search…"),
                activate: Box::new(|t: &mut Self| t.send(TrayCommand::Search)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: gettext("Settings"),
                activate: Box::new(|t: &mut Self| t.send(TrayCommand::Settings)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: gettext("Quit"),
                activate: Box::new(|t: &mut Self| t.send(TrayCommand::Quit)),
                ..Default::default()
            }
            .into(),
        ]);
        items
    }
}

/// The in-tree icon, by absolute path.
///
/// The extension treats a leading `/` as a file path, which needs no icon
/// cache and no install step — so a build tree shows the right icon. Step 10
/// installs it into the icon theme and switches this to a themed name.
pub fn icon_path() -> String {
    std::env::var("DAILY_NOTES_ICON").unwrap_or_else(|_| {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data/icons/hicolor/scalable/apps/de.ztm.DailyNotes-symbolic.svg")
            .to_string_lossy()
            .into_owned()
    })
}
