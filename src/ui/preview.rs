//! The rendered view of a day.
//!
//! Built from the block index rather than from a Markdown-to-HTML library and
//! a web view: the blocks are already parsed, inline rendering already exists
//! for the Quick Note rows, and a `GtkLabel` gives clickable links and
//! per-link hover cards for free.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::glib;
use glib::subclass::types::ObjectSubclassExt as _;


use crate::i18n::gettext;
use crate::store::{Block, Kind, Store};
use crate::ui::markup;
use crate::ui::refs::{open_link, RefResolver};

/// Where in the source a double-click on the rendered view landed.
pub struct SourceHit {
    /// The block's lines, 0-based and inclusive.
    pub line_start: usize,
    pub line_end: usize,
    /// The label's text as shown, and the character clicked in it.
    pub rendered: String,
    pub rendered_char: usize,
    /// How far down the visible area the click was, 0 at the top.
    pub y_fraction: f64,
}

pub struct Preview {
    pub scroll: gtk::ScrolledWindow,
    content: gtk::Box,
    store: Rc<RefCell<Store>>,
    on_resolve: RefCell<Option<Box<dyn Fn(Vec<crate::gitlab::refs::Reference>)>>>,
    /// Clicking an empty day should drop straight into the editor, which is
    /// otherwise a dead end for a day with nothing in it.
    on_edit: RefCell<Option<Box<dyn Fn(Option<SourceHit>)>>>,
    /// Each rendered label with the source lines it came from, so a
    /// double-click can be traced back to a place in the text.
    targets: RefCell<Vec<(gtk::Label, usize, usize)>>,
}

impl Preview {
    pub fn new(store: Rc<RefCell<Store>>) -> Rc<Self> {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.set_margin_top(16);
        content.set_margin_bottom(96);
        content.set_margin_start(20);
        content.set_margin_end(20);

        // Not `adw::Clamp`: it always centres the capped column, so on a wide
        // window the text drifted away from the left edge.
        let clamp = adw::Bin::builder()
            .child(&content)
            .layout_manager(&LeadingClamp::new(760))
            .build();

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&clamp)
            .build();

        let this = Rc::new(Self {
            scroll,
            content,
            store,
            on_resolve: RefCell::new(None),
            on_edit: RefCell::new(None),
            targets: RefCell::new(Vec::new()),
        });

        // Double-click edits, right where it landed. Capture phase, so it is
        // seen before a label turns the second click into a word selection;
        // single clicks are left alone and still select text or open links.
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let s = Rc::downgrade(&this);
            click.connect_pressed(move |gesture, n_press, x, y| {
                let Some(s) = s.upgrade() else { return };
                if n_press != 2 || s.on_edit.borrow().is_none() {
                    return;
                }
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let hit = s.hit_at(x, y);
                if let Some(cb) = s.on_edit.borrow().as_ref() {
                    cb(hit);
                }
            });
        }
        this.scroll.add_controller(click);

        this
    }

    /// The label under (or nearest to) a point in the scrolled window.
    fn hit_at(&self, x: f64, y: f64) -> Option<SourceHit> {
        let point = gtk::graphene::Point::new(x as f32, y as f32);
        let targets = self.targets.borrow();
        let (label, line_start, line_end, local) = targets
            .iter()
            .filter_map(|(label, a, b)| {
                let p = self.scroll.compute_point(label, &point)?;
                let h = label.height() as f32;
                let dy = if p.y() < 0.0 {
                    -p.y()
                } else if p.y() > h {
                    p.y() - h
                } else {
                    0.0
                };
                Some((label, *a, *b, p, dy))
            })
            .min_by(|a, b| a.4.total_cmp(&b.4))
            .map(|(l, a, b, p, _)| (l, a, b, p))?;

        let layout = label.layout();
        let (ox, oy) = label.layout_offsets();
        let scale = gtk::pango::SCALE as f32;
        let (_, index, trailing) = layout.xy_to_index(
            ((local.x() - ox as f32) * scale) as i32,
            ((local.y() - oy as f32) * scale) as i32,
        );
        let text = layout.text().to_string();
        let byte = (index.max(0) as usize).min(text.len());
        let rendered_char = text[..byte].chars().count() + trailing.max(0) as usize;

        let page = self.scroll.height().max(1) as f64;
        Some(SourceHit {
            line_start,
            line_end,
            rendered: text,
            rendered_char,
            y_fraction: y / page,
        })
    }

    pub fn connect_resolve_refs(
        self: &Rc<Self>,
        f: impl Fn(Vec<crate::gitlab::refs::Reference>) + 'static,
    ) {
        *self.on_resolve.borrow_mut() = Some(Box::new(f));
    }

    /// Tighter padding for a narrow host, e.g. the Quick Note drawer; the
    /// default leaves room below for the main window's floating controls.
    pub fn set_compact(&self) {
        self.content.set_margin_top(12);
        self.content.set_margin_bottom(12);
        self.content.set_margin_start(12);
        self.content.set_margin_end(12);
    }

    /// Asked to edit: by a click on an empty day (no position), or by a
    /// double-click on the text (the position it landed on).
    pub fn connect_edit_requested(self: &Rc<Self>, f: impl Fn(Option<SourceHit>) + 'static) {
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
        let mut targets = self.targets.borrow_mut();
        targets.clear();

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
                    cb(None);
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
                    targets.push((l.clone(), block.line_start, block.line_end));
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
                    targets.push((l.clone(), block.line_start, block.line_end));
                    row.append(&l);
                    self.content.append(&row);
                }
                Kind::Para => {
                    let l = self.label(&markup::to_pango(&block.text, &|r| resolver.lookup(r)));
                    targets.push((l.clone(), block.line_start, block.line_end));
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
                    targets.push((l.clone(), block.line_start, block.line_end));
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

        drop(targets);
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


mod leading_clamp {
    use std::cell::Cell;

    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use gtk::glib;

    #[derive(Default)]
    pub struct LeadingClamp {
        pub maximum: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LeadingClamp {
        const NAME: &'static str = "DailyNotesLeadingClamp";
        type Type = super::LeadingClamp;
        type ParentType = gtk::LayoutManager;
    }

    impl ObjectImpl for LeadingClamp {}

    impl LayoutManagerImpl for LeadingClamp {
        fn request_mode(&self, _widget: &gtk::Widget) -> gtk::SizeRequestMode {
            // Wrapped labels get taller as they get narrower.
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(
            &self,
            widget: &gtk::Widget,
            orientation: gtk::Orientation,
            for_size: i32,
        ) -> (i32, i32, i32, i32) {
            let Some(child) = widget.first_child() else {
                return (0, 0, -1, -1);
            };
            match orientation {
                gtk::Orientation::Horizontal => {
                    let (min, nat, _, _) = child.measure(orientation, -1);
                    (min, nat.min(self.maximum.get()).max(min), -1, -1)
                }
                _ => {
                    let width = if for_size < 0 { -1 } else { self.child_width(&child, for_size) };
                    child.measure(orientation, width)
                }
            }
        }

        fn allocate(&self, widget: &gtk::Widget, width: i32, height: i32, baseline: i32) {
            let Some(child) = widget.first_child() else {
                return;
            };
            let w = self.child_width(&child, width);
            let x = match widget.direction() {
                gtk::TextDirection::Rtl => width - w,
                _ => 0,
            };
            child.size_allocate(&gtk::Allocation::new(x, 0, w, height), baseline);
        }
    }

    impl LeadingClamp {
        /// The available width, capped at the maximum but never below what
        /// the child needs.
        fn child_width(&self, child: &gtk::Widget, available: i32) -> i32 {
            let (min, _, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
            available.min(self.maximum.get()).max(min)
        }
    }
}

glib::wrapper! {
    /// Caps a child's width like `adw::Clamp`, but keeps it on the leading
    /// edge instead of centring it in the spare width.
    pub struct LeadingClamp(ObjectSubclass<leading_clamp::LeadingClamp>)
        @extends gtk::LayoutManager;
}

impl LeadingClamp {
    fn new(maximum: i32) -> Self {
        let obj: Self = glib::Object::new();
        leading_clamp::LeadingClamp::from_obj(&obj).maximum.set(maximum);
        obj
    }
}
