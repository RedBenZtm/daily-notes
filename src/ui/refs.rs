//! Painting GitLab references in the editor.
//!
//! References are decorated from the cache synchronously, so typing never
//! waits on the network; anything missing or stale is queued for a background
//! refresh and repainted when it lands.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::gitlab::refs::{self, Reference};
use crate::i18n::gettext;
use crate::store::Store;

/// A reference the editor has painted, with where it landed and what it means.
#[derive(Clone)]
struct Painted {
    start: usize,
    end: usize,
    web_url: Option<String>,
    tooltip: String,
}

pub struct RefDecorator {
    view: sourceview5::View,
    buffer: sourceview5::Buffer,
    store: Rc<RefCell<Store>>,
    painted: RefCell<Vec<Painted>>,
    /// Keys queued for the background resolver.
    on_resolve: RefCell<Option<Box<dyn Fn(Vec<Reference>)>>>,
}

impl RefDecorator {
    pub fn new(
        view: &sourceview5::View,
        buffer: &sourceview5::Buffer,
        store: Rc<RefCell<Store>>,
    ) -> Rc<Self> {
        let table = buffer.tag_table();
        // Resolved: solid underline in the accent colour.
        let resolved = gtk::TextTag::builder()
            .name("gitlab-ref")
            .underline(gtk::pango::Underline::Single)
            .build();
        // Known-shaped but not resolved (yet, or at all): a quieter hint.
        let pending = gtk::TextTag::builder()
            .name("gitlab-ref-pending")
            .underline(gtk::pango::Underline::SingleLine)
            .build();
        pending.set_underline(gtk::pango::Underline::Single);
        table.add(&resolved);
        table.add(&pending);

        let this = Rc::new(Self {
            view: view.clone(),
            buffer: buffer.clone(),
            store,
            painted: RefCell::new(Vec::new()),
            on_resolve: RefCell::new(None),
        });

        this.apply_colours();
        let t = this.clone();
        adw::StyleManager::default().connect_dark_notify(move |_| t.apply_colours());

        // Hover card.
        view.set_has_tooltip(true);
        let t = this.clone();
        view.connect_query_tooltip(move |view, x, y, keyboard, tooltip| {
            if keyboard {
                return false;
            }
            let (bx, by) = view.window_to_buffer_coords(gtk::TextWindowType::Widget, x, y);
            let Some(iter) = view.iter_at_location(bx, by) else {
                return false;
            };
            let offset = iter.offset() as usize;
            let painted = t.painted.borrow();
            match painted.iter().find(|p| {
                let (s, e) = t.char_span(p);
                offset >= s && offset < e
            }) {
                Some(p) => {
                    tooltip.set_markup(Some(&p.tooltip));
                    true
                }
                None => false,
            }
        });

        // Click to open in the browser.
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        let t = this.clone();
        click.connect_released(move |gesture, _, x, y| {
            let view = &t.view;
            let (bx, by) = view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
            let Some(iter) = view.iter_at_location(bx, by) else {
                return;
            };
            let offset = iter.offset() as usize;
            let url = {
                let painted = t.painted.borrow();
                painted
                    .iter()
                    .find(|p| {
                        let (s, e) = t.char_span(p);
                        offset >= s && offset < e
                    })
                    .and_then(|p| p.web_url.clone())
            };
            let Some(url) = url else { return };
            // Only hijack the click when it actually landed on a link.
            gesture.set_state(gtk::EventSequenceState::Claimed);
            let launcher = gtk::UriLauncher::new(&url);
            let window = view.root().and_downcast::<gtk::Window>();
            launcher.launch(window.as_ref(), gtk::gio::Cancellable::NONE, |res| {
                if let Err(e) = res {
                    tracing::warn!(?e, "could not open the reference");
                }
            });
        });
        view.add_controller(click);

        this
    }

    pub fn connect_resolve(self: &Rc<Self>, f: impl Fn(Vec<Reference>) + 'static) {
        *self.on_resolve.borrow_mut() = Some(Box::new(f));
    }

    /// Byte offsets from the parser are converted to character offsets, which
    /// is what `TextBuffer` counts in.
    fn char_span(&self, p: &Painted) -> (usize, usize) {
        (p.start, p.end)
    }

    fn apply_colours(&self) {
        let table = self.buffer.tag_table();
        let dark = adw::StyleManager::default().is_dark();
        let accent = if dark { "#78aeed" } else { "#1c71d8" };
        let dim = if dark { "#77767b" } else { "#9a9996" };
        if let Some(t) = table.lookup("gitlab-ref") {
            t.set_foreground(Some(accent));
        }
        if let Some(t) = table.lookup("gitlab-ref-pending") {
            t.set_foreground(Some(dim));
        }
    }

    /// Re-reads the buffer, repaints every reference from cache, and returns
    /// the ones that need fetching.
    pub fn refresh(self: &Rc<Self>) {
        let (start, end) = self.buffer.bounds();
        let text = self.buffer.text(&start, &end, true).to_string();

        self.buffer.remove_tag_by_name("gitlab-ref", &start, &end);
        self.buffer
            .remove_tag_by_name("gitlab-ref-pending", &start, &end);
        self.painted.borrow_mut().clear();

        let cfg = crate::config::load();
        if !cfg.gitlab.is_configured() {
            return;
        }

        let found = refs::parse(&text);
        if found.is_empty() {
            return;
        }

        // Byte offset -> char offset, once, for the whole buffer.
        let mut byte_to_char = vec![0usize; text.len() + 1];
        for (chars, (bytes, _)) in text.char_indices().enumerate() {
            byte_to_char[bytes] = chars;
        }
        byte_to_char[text.len()] = text.chars().count();
        // Fill the interior bytes of multi-byte characters so a lookup on any
        // byte of a character lands on that character.
        let mut last = 0;
        for slot in byte_to_char.iter_mut() {
            if *slot == 0 && last != 0 {
                *slot = last;
            } else {
                last = *slot;
            }
        }

        let now = chrono::Utc::now().timestamp();
        let mut needed = Vec::new();

        for r in found {
            let key = r.cache_key(&cfg.gitlab.main_project);
            let entry = self.store.borrow().ref_cache_get(&key).ok().flatten();

            let stale = entry.as_ref().map(|e| !e.is_fresh(now)).unwrap_or(true);
            if stale {
                needed.push(r.clone());
            }

            let cs = *byte_to_char.get(r.start).unwrap_or(&0);
            let ce = *byte_to_char.get(r.end).unwrap_or(&0);
            let (Some(a), Some(b)) = (
                self.buffer.iter_at_offset(cs as i32).into(),
                self.buffer.iter_at_offset(ce as i32).into(),
            ) else {
                continue;
            };

            let raw = &text[r.start..r.end];
            let (tag, tooltip, url) = match entry.and_then(|e| e.resolved) {
                Some(res) => {
                    let state = translate_state(&res.state);
                    let mut tip = format!(
                        "<b>{}</b>\n{}",
                        glib::markup_escape_text(raw),
                        glib::markup_escape_text(&res.title)
                    );
                    if !state.is_empty() || !res.author.is_empty() {
                        tip.push_str(&format!(
                            "\n<small>{}</small>",
                            glib::markup_escape_text(
                                &[state, if res.author.is_empty() { String::new() } else { format!("@{}", res.author) }]
                                    .iter()
                                    .filter(|s| !s.is_empty())
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join(" · ")
                            )
                        ));
                    }
                    ("gitlab-ref", tip, Some(res.web_url))
                }
                None => (
                    "gitlab-ref-pending",
                    format!(
                        "<b>{}</b>\n<small>{}</small>",
                        glib::markup_escape_text(raw),
                        glib::markup_escape_text(&gettext("Not found, or not visible to this token"))
                    ),
                    None,
                ),
            };

            self.buffer.apply_tag_by_name(tag, &a, &b);
            self.painted.borrow_mut().push(Painted {
                start: cs,
                end: ce,
                web_url: url,
                tooltip,
            });
        }

        if !needed.is_empty() {
            if let Some(cb) = self.on_resolve.borrow().as_ref() {
                cb(needed);
            }
        }
    }
}

/// GitLab's states, in the user's language.
pub fn translate_state(state: &str) -> String {
    match state {
        "opened" => gettext("open"),
        "closed" => gettext("closed"),
        "merged" => gettext("merged"),
        "locked" => gettext("locked"),
        "active" => gettext("active"),
        "" => String::new(),
        other => other.to_string(),
    }
}

/// Looks GitLab references up in the cache while rendering, collecting the
/// ones that need fetching.
///
/// Shared by everything that paints references — the Quick Note rows and the
/// rendered day view — so there is one definition of "resolved" and one place
/// that decides what to re-fetch.
pub struct RefResolver {
    store: Rc<RefCell<Store>>,
    default_project: String,
    configured: bool,
    pending: RefCell<Vec<crate::gitlab::refs::Reference>>,
}

impl RefResolver {
    pub fn new(store: Rc<RefCell<Store>>) -> Self {
        let cfg = crate::config::load();
        Self {
            store,
            default_project: cfg.gitlab.main_project.clone(),
            configured: cfg.gitlab.is_configured(),
            pending: RefCell::new(Vec::new()),
        }
    }

    pub fn lookup(
        &self,
        r: &crate::gitlab::refs::Reference,
    ) -> Option<crate::ui::markup::RefTarget> {
        if !self.configured {
            return None;
        }
        let key = r.cache_key(&self.default_project);
        let entry = self.store.borrow().ref_cache_get(&key).ok().flatten();
        let now = chrono::Utc::now().timestamp();
        match entry {
            Some(e) => {
                if !e.is_fresh(now) {
                    self.pending.borrow_mut().push(r.clone());
                }
                e.resolved.map(|res| {
                    let state = translate_state(&res.state);
                    let mut tip = res.title.clone();
                    if !state.is_empty() {
                        tip.push_str(" · ");
                        tip.push_str(&state);
                    }
                    crate::ui::markup::RefTarget {
                        url: Some(res.web_url),
                        tooltip: Some(tip),
                    }
                })
            }
            None => {
                self.pending.borrow_mut().push(r.clone());
                None
            }
        }
    }

    /// References that were missing or stale, for the background resolver.
    pub fn take_pending(self) -> Vec<crate::gitlab::refs::Reference> {
        self.pending.into_inner()
    }
}

/// Opens a link from a rendered label, rather than letting the default
/// handler do it, so failures are logged instead of silent.
pub fn open_link(widget: &impl IsA<gtk::Widget>, uri: &str) -> glib::Propagation {
    let launcher = gtk::UriLauncher::new(uri);
    let window = widget.as_ref().root().and_downcast::<gtk::Window>();
    launcher.launch(window.as_ref(), gtk::gio::Cancellable::NONE, |res| {
        if let Err(e) = res {
            tracing::warn!(?e, "could not open the link");
        }
    });
    glib::Propagation::Stop
}
