//! The rendered view of a day.
//!
//! Built from the block index rather than from a Markdown-to-HTML library and
//! a web view: the blocks are already parsed, inline rendering already exists
//! for the Quick Note rows, and a `GtkLabel` gives clickable links and
//! per-link hover cards for free.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;


use crate::i18n::gettext;
use crate::store::{Block, Kind, Store};
use crate::ui::markup;
use crate::ui::refs::{open_link, RefResolver};

pub struct Preview {
    pub scroll: gtk::ScrolledWindow,
    content: gtk::Box,
    store: Rc<RefCell<Store>>,
    on_resolve: RefCell<Option<Box<dyn Fn(Vec<crate::gitlab::refs::Reference>)>>>,
    /// Clicking an empty day should drop straight into the editor, which is
    /// otherwise a dead end for a day with nothing in it.
    on_edit: RefCell<Option<Box<dyn Fn()>>>,
}

impl Preview {
    pub fn new(store: Rc<RefCell<Store>>) -> Rc<Self> {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.set_margin_top(16);
        content.set_margin_bottom(96);
        content.set_margin_start(20);
        content.set_margin_end(20);

        let clamp = adw::Clamp::builder()
            .maximum_size(760)
            .tightening_threshold(600)
            .child(&content)
            .build();

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&clamp)
            .build();

        Rc::new(Self {
            scroll,
            content,
            store,
            on_resolve: RefCell::new(None),
            on_edit: RefCell::new(None),
        })
    }

    pub fn connect_resolve_refs(
        self: &Rc<Self>,
        f: impl Fn(Vec<crate::gitlab::refs::Reference>) + 'static,
    ) {
        *self.on_resolve.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_edit_requested(self: &Rc<Self>, f: impl Fn() + 'static) {
        *self.on_edit.borrow_mut() = Some(Box::new(f));
    }

    fn label(&self, markup: &str) -> gtk::Label {
        let l = gtk::Label::new(None);
        l.set_markup(markup);
        l.set_xalign(0.0);
        l.set_wrap(true);
        l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        l.set_selectable(true);
        // set_selectable() also turns focusable on, and a GtkLabel selects all
        // of its text the moment it takes focus. That made the rendered view a
        // field of focus targets: whenever focus had to move — leaving the
        // editor, switching day — GTK handed it to the first label and lit up
        // its text. Selection by mouse is unaffected; the two properties are
        // independent.
        l.set_focusable(false);
        l.connect_activate_link(|label, uri| open_link(label, uri));
        l
    }

    pub fn render(self: &Rc<Self>, blocks: &[Block]) {
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }

        if blocks.is_empty() {
            let status = adw::StatusPage::builder()
                .icon_name("document-edit-symbolic")
                .title(gettext("Nothing written yet"))
                // Translators: {} is a keyboard key, shown literally.
                .description(gettext("Press {} to start writing.").replace("{}", "E"))
                .build();
            status.set_vexpand(true);
            let s = self.clone();
            let click = gtk::GestureClick::new();
            click.connect_released(move |_, _, _, _| {
                if let Some(cb) = s.on_edit.borrow().as_ref() {
                    cb();
                }
            });
            status.add_controller(click);
            self.content.append(&status);
            return;
        }

        let resolver = RefResolver::new(self.store.clone());

        for block in blocks {
            match block.kind {
                Kind::Heading => {
                    let l = self.label(&markup::to_pango(&block.text, &|r| resolver.lookup(r)));
                    // Heading level is not kept in the block index, so one
                    // weight for all of them; they are section markers in a
                    // day's notes, not a document outline.
                    l.add_css_class("title-4");
                    l.set_margin_top(12);
                    self.content.append(&l);
                }
                Kind::Bullet => {
                    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                    row.set_valign(gtk::Align::Start);
                    let dot = gtk::Label::new(Some("•"));
                    dot.set_valign(gtk::Align::Start);
                    dot.add_css_class("dimmed");
                    row.append(&dot);
                    let l = self.label(&markup::to_pango(&block.text, &|r| resolver.lookup(r)));
                    l.set_hexpand(true);
                    row.append(&l);
                    self.content.append(&row);
                }
                Kind::Para => {
                    let l = self.label(&markup::to_pango(&block.text, &|r| resolver.lookup(r)));
                    self.content.append(&l);
                }
                Kind::Code => {
                    // Code is shown verbatim: no inline markdown, no links.
                    let body = block
                        .text
                        .lines()
                        .skip(1)
                        .take(block.text.lines().count().saturating_sub(2))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let l = gtk::Label::new(Some(&body));
                    l.set_xalign(0.0);
                    l.set_wrap(true);
                    l.set_wrap_mode(gtk::pango::WrapMode::Char);
                    l.set_selectable(true);
                    l.add_css_class("monospace");
                    l.set_margin_top(8);
                    l.set_margin_bottom(8);
                    l.set_margin_start(12);
                    l.set_margin_end(12);
                    let frame = gtk::Frame::new(None);
                    frame.add_css_class("card");
                    frame.set_child(Some(&l));
                    self.content.append(&frame);
                }
            }
        }

        let pending = resolver.take_pending();
        if !pending.is_empty() {
            if let Some(cb) = self.on_resolve.borrow().as_ref() {
                cb(pending);
            }
        }
    }

    pub fn scroll_to_top(&self) {
        self.scroll.vadjustment().set_value(0.0);
    }
}

