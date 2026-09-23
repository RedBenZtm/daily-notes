//! `#` completion for issues, the way GitLab's own comment box does it.
//!
//! A real `GtkSourceCompletionProvider` rather than a hand-rolled popup, so
//! keyboard handling, positioning and theming come from GtkSourceView.
//!
//! Proposals are served from the local issue cache, which keeps `populate`
//! synchronous — completion must not wait on the network while someone types.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};
use glib::subclass::prelude::*;
use sourceview5::subclass::prelude::*;

use crate::store::Store;

mod proposal {
    use super::*;
    use std::cell::Cell;

    #[derive(Default)]
    pub struct IssueProposal {
        pub iid: Cell<u64>,
        pub title: RefCell<String>,
        pub state: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for IssueProposal {
        const NAME: &'static str = "DailyNotesIssueProposal";
        type Type = super::IssueProposal;
        type Interfaces = (sourceview5::CompletionProposal,);
    }

    impl ObjectImpl for IssueProposal {}
    impl CompletionProposalImpl for IssueProposal {}
}

glib::wrapper! {
    pub struct IssueProposal(ObjectSubclass<proposal::IssueProposal>)
        @implements sourceview5::CompletionProposal;
}

impl IssueProposal {
    fn new(iid: u64, title: &str, state: &str) -> Self {
        let obj: Self = glib::Object::new();
        let imp = proposal::IssueProposal::from_obj(&obj);
        imp.iid.set(iid);
        *imp.title.borrow_mut() = title.to_string();
        *imp.state.borrow_mut() = state.to_string();
        obj
    }

    fn iid(&self) -> u64 {
        proposal::IssueProposal::from_obj(self).iid.get()
    }

    fn title(&self) -> String {
        proposal::IssueProposal::from_obj(self).title.borrow().clone()
    }
}

mod provider {
    use super::*;

    #[derive(Default)]
    pub struct IssueCompletion {
        pub store: RefCell<Option<Rc<RefCell<Store>>>>,
        /// Called with the typed query so the issue list can be refreshed in
        /// the background; the popup itself never waits for it.
        pub on_query: RefCell<Option<Box<dyn Fn()>>>,
        /// What has been typed after `#`. The filter closure reads it, so
        /// updating it and poking the filter is all refilter has to do.
        pub query: Rc<RefCell<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for IssueCompletion {
        const NAME: &'static str = "DailyNotesIssueCompletion";
        type Type = super::IssueCompletion;
        type Interfaces = (sourceview5::CompletionProvider,);
    }

    impl ObjectImpl for IssueCompletion {}

    impl IssueCompletion {
        /// Every candidate issue for the project. Filtering happens in the
        /// model, not here, so that deleting characters can bring entries back.
        fn candidates(&self) -> Vec<(u64, String, String)> {
            let Some(store) = self.store.borrow().clone() else {
                return Vec::new();
            };
            let cfg = crate::config::load();
            if !cfg.gitlab.is_configured() {
                return Vec::new();
            }
            match store
                .borrow()
                .complete_issues(&cfg.gitlab.main_project, "", 500)
            {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::warn!(?e, "could not list issues for completion");
                    Vec::new()
                }
            }
        }
    }

    impl CompletionProviderImpl for IssueCompletion {
        fn is_trigger(&self, _iter: &gtk::TextIter, c: char) -> bool {
            c == '#'
        }

        fn title(&self) -> Option<glib::GString> {
            Some(crate::i18n::gettext("Issues").into())
        }

        fn populate(
            &self,
            context: &sourceview5::CompletionContext,
        ) -> Result<gio::ListModel, glib::Error> {
            // Ask for a refresh of the cache, but serve this popup from what
            // is already there — typing must not wait on the API.
            if let Some(cb) = self.on_query.borrow().as_ref() {
                cb();
            }
            *self.query.borrow_mut() = super::query_of(context);
            let rows = self.candidates();
            if rows.is_empty() {
                // "No proposals" is an error by this interface's contract, not
                // an empty model. Handing back an empty model makes
                // GtkSourceCompletion count the provider as having results and
                // present a zero-width popup — which fails
                // `gdk_popup_present: assertion 'width > 0'` and leaves
                // completion wedged for the rest of the session.
                return Err(no_matches());
            }
            let list = gio::ListStore::new::<super::IssueProposal>();
            for (iid, title, state) in rows {
                list.append(&super::IssueProposal::new(iid, &title, &state));
            }

            // A filtered view over the full candidate list, rather than a
            // pre-filtered store. Narrowing *and* widening then work: deleting
            // back to a bare `#` brings every issue back, where a store that
            // only ever had items removed would be left empty — and an empty
            // model is what wedges the popup.
            let query = self.query.clone();
            let filter = gtk::CustomFilter::new(move |obj| {
                obj.downcast_ref::<super::IssueProposal>()
                    .map(|p| super::matches(p.iid(), &p.title(), &query.borrow()))
                    .unwrap_or(false)
            });
            let model = gtk::FilterListModel::new(Some(list), Some(filter));
            if model.n_items() == 0 {
                return Err(no_matches());
            }
            Ok(model.upcast())
        }

        fn refilter(&self, context: &sourceview5::CompletionContext, model: &gio::ListModel) {
            // Update the query and let the filter re-evaluate. The model is
            // never mutated directly: it is a view, so items come back when
            // the query shortens.
            *self.query.borrow_mut() = super::query_of(context);
            if let Some(m) = model.downcast_ref::<gtk::FilterListModel>() {
                if let Some(f) = m.filter() {
                    f.changed(gtk::FilterChange::Different);
                }
            }
        }

        fn display(
            &self,
            _context: &sourceview5::CompletionContext,
            proposal: &sourceview5::CompletionProposal,
            cell: &sourceview5::CompletionCell,
        ) {
            let Some(p) = proposal.downcast_ref::<super::IssueProposal>() else {
                return;
            };
            match cell.column() {
                sourceview5::CompletionColumn::Icon => cell.set_icon_name("view-list-symbolic"),
                // The number is what gets inserted, so it leads.
                sourceview5::CompletionColumn::TypedText => {
                    cell.set_text(Some(&format!("#{}", p.iid())))
                }
                // The title goes right beside the number rather than in the
                // Comment column, which GtkSourceView right-aligns and squeezes
                // out first — leaving a list of bare numbers that say nothing
                // about which ticket is which.
                sourceview5::CompletionColumn::After => {
                    let title = super::elide(&p.title(), 72);
                    cell.set_markup(&format!(
                        "  <span alpha='65%'>{}</span>",
                        glib::markup_escape_text(&title)
                    ));
                }
                // The full title, for the details pane.
                sourceview5::CompletionColumn::Details => cell.set_text(Some(&p.title())),
                _ => cell.set_text(None),
            }
        }

        fn activate(
            &self,
            context: &sourceview5::CompletionContext,
            proposal: &sourceview5::CompletionProposal,
        ) {
            let Some(p) = proposal.downcast_ref::<super::IssueProposal>() else {
                return;
            };
            let Some(buffer) = context.buffer() else {
                return;
            };
            let Some((mut start, mut end)) = context.bounds() else {
                return;
            };
            // bounds() covers the word after the trigger; the `#` itself sits
            // just before it and has to go too, or inserting would double it.
            let mut probe = start;
            if probe.backward_char() && probe.char() == '#' {
                start = probe;
            }
            buffer.begin_user_action();
            buffer.delete(&mut start, &mut end);
            buffer.insert(&mut start, &format!("#{}", p.iid()));
            buffer.end_user_action();
        }
    }
}

glib::wrapper! {
    pub struct IssueCompletion(ObjectSubclass<provider::IssueCompletion>)
        @implements sourceview5::CompletionProvider;
}

impl Default for IssueCompletion {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl IssueCompletion {
    pub fn new(store: Rc<RefCell<Store>>) -> Self {
        let obj: Self = glib::Object::new();
        *provider::IssueCompletion::from_obj(&obj).store.borrow_mut() = Some(store);
        obj
    }

    pub fn connect_query(&self, f: impl Fn() + 'static) {
        *provider::IssueCompletion::from_obj(self).on_query.borrow_mut() = Some(Box::new(f));
    }
}

/// Whether an issue still matches what has been typed.
///
/// Mirrors the SQL behind `populate` so that narrowing an open popup and
/// opening a fresh one agree: digits match the issue number by prefix, and
/// anything matches a title substring, case-insensitively.
pub fn matches(iid: u64, title: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let q = query.to_lowercase();
    iid.to_string().starts_with(&q) || title.to_lowercase().contains(&q)
}

/// Keeps a long title from stretching the popup across the screen.
fn elide(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

fn no_matches() -> glib::Error {
    glib::Error::new(gio::IOErrorEnum::NotFound, "no matching issues")
}

/// What has been typed after the `#`.
fn query_of(context: &sourceview5::CompletionContext) -> String {
    match context.bounds() {
        Some((start, end)) => {
            let text = start.slice(&end).to_string();
            text.trim_start_matches('#').to_string()
        }
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn digits_match_the_issue_number_by_prefix() {
        assert!(matches(890, "Cleanup Daten CMS", "89"));
        assert!(matches(89, "Unrelated", "89"));
        assert!(!matches(12, "Unrelated", "89"));
        // a number appearing in the title counts too
        assert!(matches(12, "Number 89 in the title", "89"));
    }

    #[test]
    fn words_match_the_title_case_insensitively() {
        assert!(matches(890, "Cleanup Daten CMS", "cleanup"));
        assert!(matches(890, "Cleanup Daten CMS", "DATEN"));
        assert!(!matches(890, "Cleanup Daten CMS", "minio"));
    }

    #[test]
    fn an_empty_query_matches_everything() {
        assert!(matches(1, "anything", ""));
    }

    #[test]
    fn narrowing_only_ever_removes() {
        // What refilter relies on: a longer query never matches more.
        let items = [(890, "Cleanup Daten CMS"), (89, "Other"), (12, "Number 89 here")];
        for (short, long) in [("8", "89"), ("89", "890"), ("cle", "clean")] {
            for (iid, title) in items {
                if matches(iid, title, long) {
                    assert!(
                        matches(iid, title, short),
                        "{iid} {title:?} matched {long:?} but not {short:?}"
                    );
                }
            }
        }
    }
}
