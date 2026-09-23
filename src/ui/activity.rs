//! The GitLab activity drawer: what happened in GitLab on the day on screen.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::NaiveDate;
use gtk::prelude::*;
use gtk::glib;

use crate::gitlab::events;
use crate::i18n::gettext;
use crate::store::Store;

pub struct Activity {
    pub widget: adw::ToolbarView,
    list: gtk::Box,
    status: gtk::Label,
    store: Rc<RefCell<Store>>,
    on_insert: RefCell<Option<Box<dyn Fn(String)>>>,
    on_refresh: RefCell<Option<Box<dyn Fn(NaiveDate)>>>,
}

impl Activity {
    pub fn new(store: Rc<RefCell<Store>>) -> Rc<Self> {
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.set_margin_top(6);
        list.set_margin_bottom(12);

        let status = gtk::Label::new(None);
        status.add_css_class("dimmed");
        status.set_wrap(true);
        status.set_justify(gtk::Justification::Center);
        status.set_margin_top(24);
        status.set_margin_start(12);
        status.set_margin_end(12);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&status);
        body.append(&list);

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&body)
            .build();

        let header = adw::HeaderBar::builder()
            .title_widget(&adw::WindowTitle::new(&gettext("Activity"), ""))
            .show_end_title_buttons(false)
            .show_start_title_buttons(false)
            .build();

        let widget = adw::ToolbarView::new();
        widget.add_top_bar(&header);
        widget.set_content(Some(&scroll));

        Rc::new(Self {
            widget,
            list,
            status,
            store,
            on_insert: RefCell::new(None),
            on_refresh: RefCell::new(None),
        })
    }

    /// Appends a GitLab reference to the day's note.
    pub fn connect_insert(self: &Rc<Self>, f: impl Fn(String) + 'static) {
        *self.on_insert.borrow_mut() = Some(Box::new(f));
    }

    /// Asks for this day's events to be fetched.
    pub fn connect_refresh(self: &Rc<Self>, f: impl Fn(NaiveDate) + 'static) {
        *self.on_refresh.borrow_mut() = Some(Box::new(f));
    }

    pub fn show_day(self: &Rc<Self>, date: NaiveDate) {
        self.render(date);
        if let Some(cb) = self.on_refresh.borrow().as_ref() {
            cb(date);
        }
    }

    pub fn render(self: &Rc<Self>, date: NaiveDate) {
        // Clicking "add to the note" reloads the day, which destroys the very
        // button that was clicked — and it still holds focus. Move focus out
        // before tearing the rows down rather than leaving GTK to trip over a
        // disposed widget.
        if let Some(window) = self.widget.root().and_downcast::<gtk::Window>() {
            if let Some(focus) = gtk::prelude::GtkWindowExt::focus(&window) {
                if focus.is_ancestor(&self.list) {
                    gtk::prelude::GtkWindowExt::set_focus(&window, gtk::Widget::NONE);
                }
            }
        }
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }

        let cfg = crate::config::load();
        if !cfg.gitlab.is_configured() {
            self.status.set_visible(true);
            self.status.set_text(&gettext("GitLab is not set up yet."));
            return;
        }

        let records = self.store.borrow().events(date).unwrap_or_default();
        if records.is_empty() {
            self.status.set_visible(true);
            self.status.set_text(&gettext("Nothing happened in GitLab on this day."));
            return;
        }
        self.status.set_visible(false);

        // Only worth naming the project when the day spans more than one.
        let mut projects: Vec<u64> = records.iter().map(|e| e.project_id).collect();
        projects.dedup();
        projects.sort_unstable();
        projects.dedup();
        let show_project = projects.len() > 1;

        for rec in &records {
            let (path, web_url) = self
                .store
                .borrow()
                .project(rec.project_id)
                .unwrap_or_else(|| (String::new(), String::new()));
            let a = events::describe(rec, &path, &web_url, &cfg.gitlab.main_project);
            self.list.append(&self.row(&a, show_project));
        }
    }

    fn row(self: &Rc<Self>, a: &events::Activity, show_project: bool) -> gtk::Widget {
        let time = gtk::Label::new(Some(&a.at.format("%H:%M").to_string()));
        time.add_css_class("dimmed");
        time.add_css_class("monospace");
        time.set_valign(gtk::Align::Start);
        time.set_xalign(0.0);

        let icon = gtk::Image::from_icon_name(a.icon);
        icon.add_css_class("dimmed");
        icon.set_valign(gtk::Align::Start);

        let text = gtk::Label::new(None);
        text.set_markup(&match &a.url {
            Some(url) => format!(
                r#"<a href="{}">{}</a>"#,
                glib::markup_escape_text(url),
                glib::markup_escape_text(&a.text)
            ),
            None => glib::markup_escape_text(&a.text).to_string(),
        });
        text.set_xalign(0.0);
        text.set_wrap(true);
        text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        text.set_hexpand(true);
        text.connect_activate_link(|l, uri| crate::ui::refs::open_link(l, uri));

        let lines = gtk::Box::new(gtk::Orientation::Vertical, 0);
        lines.append(&text);
        if show_project && !a.project.is_empty() {
            let p = gtk::Label::new(Some(&a.project));
            p.add_css_class("dimmed");
            p.add_css_class("caption");
            p.set_xalign(0.0);
            p.set_ellipsize(gtk::pango::EllipsizeMode::Start);
            lines.append(&p);
        }

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_margin_top(6);
        row.set_margin_bottom(6);
        row.set_margin_start(12);
        row.set_margin_end(6);
        row.append(&time);
        row.append(&icon);
        row.append(&lines);

        // The one place the timeline feeds the notes rather than just sitting
        // beside them. Every activity can be added, not only the ones that
        // name an issue or a merge request.
        let insert = gtk::Button::from_icon_name("list-add-symbolic");
        insert.add_css_class("flat");
        insert.set_valign(gtk::Align::Start);
        insert.set_tooltip_text(Some(&gettext("Add to the note")));
        {
            let s = self.clone();
            let line = a.insert.clone();
            insert.connect_clicked(move |_| {
                if let Some(cb) = s.on_insert.borrow().as_ref() {
                    cb(line.clone());
                }
            });
        }
        row.append(&insert);

        row.upcast()
    }
}
