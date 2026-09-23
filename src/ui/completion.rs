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
    }

    #[glib::object_subclass]
    impl ObjectSubclass for IssueCompletion {
        const NAME: &'static str = "DailyNotesIssueCompletion";
        type Type = super::IssueCompletion;
        type Interfaces = (sourceview5::CompletionProvider,);
    }

    impl ObjectImpl for IssueCompletion {}

    impl IssueCompletion {
        /// Issues matching what has been typed, from the local cache.
        fn matches(&self, query: &str) -> Vec<(u64, String, String)> {
            let Some(store) = self.store.borrow().clone() else {
                return Vec::new();
            };
            let cfg = crate::config::load();
            if !cfg.gitlab.is_configured() {
                return Vec::new();
            }
            match store
                .borrow()
                .complete_issues(&cfg.gitlab.main_project, query, 20)
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
            let query = super::query_of(context);
            let rows = self.matches(&query);
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
            Ok(list.upcast())
        }

        fn refilter(&self, context: &sourceview5::CompletionContext, model: &gio::ListModel) {
            // Typing more characters after `#` refilters rather than
            // repopulating, so without this the list would stay as it was.
            let Some(list) = model.downcast_ref::<gio::ListStore>() else {
                return;
            };
            let rows = self.matches(&super::query_of(context));
            list.remove_all();
            for (iid, title, state) in rows {
                list.append(&super::IssueProposal::new(iid, &title, &state));
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
                sourceview5::CompletionColumn::Comment => cell.set_text(Some(&p.title())),
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
