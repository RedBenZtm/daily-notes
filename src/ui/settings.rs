//! The preferences dialog: GitLab instance, token, and the main project.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::config;
use crate::gitlab::client::Probe;
use crate::i18n::gettext;

pub struct Settings {
    pub dialog: adw::PreferencesDialog,
    instance: adw::EntryRow,
    token: adw::PasswordEntryRow,
    project: adw::EntryRow,
    group: adw::EntryRow,
    status: adw::ActionRow,
    on_saved: RefCell<Option<Box<dyn Fn()>>>,
}

impl Settings {
    pub fn build(
        rt: &Rc<tokio::runtime::Runtime>,
        clear_cache: impl Fn() + 'static,
    ) -> Rc<Self> {
        let cfg = config::load();

        let instance = adw::EntryRow::builder()
            .title(gettext("Instance URL"))
            .text(&cfg.gitlab.instance)
            .build();

        let token = adw::PasswordEntryRow::builder()
            .title(gettext("Personal access token"))
            .build();
        if config::token().is_some() {
            // Never read the stored secret back into a widget; just say it is
            // there, and treat an untouched field as "leave it alone".
            token.set_text("");
        }

        let project = adw::EntryRow::builder()
            .title(gettext("Main project"))
            .text(&cfg.gitlab.main_project)
            .build();
        let group = adw::EntryRow::builder()
            .title(gettext("Main group"))
            .text(&cfg.gitlab.main_group)
            .build();

        let hint = adw::ActionRow::builder()
            .title(gettext("How references resolve"))
            .build();
        // AdwActionRow parses title and subtitle as Pango markup, and this
        // text contains a literal `&7` — an unterminated entity, so the whole
        // subtitle silently fails to render. It is prose, not markup. Note the
        // order: the subtitle has to be set *after* use-markup is turned off,
        // since the property is applied when the text is parsed.
        hint.set_use_markup(false);
        hint.set_subtitle(&gettext(
            "#123 and !45 resolve in the main project; &7 in the main group. \
             Write group/project#123 to reach another project.",
        ));
        hint.set_activatable(false);

        let token_hint = adw::ActionRow::builder()
            .title(gettext("The token is stored in the GNOME keyring"))
            .build();
        token_hint.set_use_markup(false);
        token_hint.set_subtitle(&gettext(
            "It needs the read_api scope. Leave blank to keep the current one.",
        ));
        token_hint.set_activatable(false);

        let test = gtk::Button::with_label(&gettext("Test connection"));
        test.set_valign(gtk::Align::Center);
        let status = adw::ActionRow::builder().title(gettext("Not tested")).build();
        status.add_suffix(&test);
        // The connection test puts a server-supplied version string in here.
        status.set_use_markup(false);
        status.set_activatable(false);

        let gitlab_group = adw::PreferencesGroup::builder()
            .title(gettext("GitLab"))
            .build();
        // The formats live here rather than as placeholders in the fields:
        // AdwEntryRow's title stays inside the field instead of floating out
        // of the way, so an in-field placeholder renders on top of it.
        // Translators: the example values are formats, not real addresses —
        // keep the URL as-is, but do translate "group/project" and "group".
        gitlab_group.set_description(Some(&gettext(
            "Instance URL: https://gitlab.example.com
Main project: group/project · Main group: group",
        )));
        gitlab_group.add(&instance);
        gitlab_group.add(&token);
        gitlab_group.add(&token_hint);
        gitlab_group.add(&project);
        gitlab_group.add(&group);
        gitlab_group.add(&hint);
        gitlab_group.add(&status);

        let page = adw::PreferencesPage::builder()
            .title(gettext("GitLab"))
            .icon_name("network-server-symbolic")
            .build();
        page.add(&gitlab_group);

        let dialog = adw::PreferencesDialog::builder()
            .title(gettext("Settings"))
            .build();
        dialog.add(&page);

        let this = Rc::new(Self {
            dialog,
            instance,
            token,
            project,
            group,
            status,
            on_saved: RefCell::new(None),
        });

        // Save as you go — a preferences dialog with an OK button is not the
        // GNOME idiom, and a half-entered instance is harmless.
        for row in [&this.instance, &this.project, &this.group] {
            let s = this.clone();
            row.connect_changed(move |_| s.save());
        }
        {
            let s = this.clone();
            let clear_cache = Rc::new(clear_cache);
            this.token.connect_changed(move |e| {
                let text = e.text().to_string();
                if text.is_empty() {
                    return; // untouched: keep whatever is in the keyring
                }
                if let Err(err) = config::set_token(&text) {
                    tracing::error!(?err, "could not store the token");
                    s.status.set_title(&gettext("Could not store the token"));
                } else {
                    // Cached resolutions from the old credentials may be wrong.
                    clear_cache();
                }
            });
        }

        {
            let s = this.clone();
            let rt = rt.clone();
            test.connect_clicked(move |button| {
                s.save();
                let cfg = config::load();
                let Some(tok) = config::token() else {
                    s.status.set_title(&gettext("No token stored yet"));
                    return;
                };
                if cfg.gitlab.base().is_empty() {
                    s.status.set_title(&gettext("Enter the instance URL first"));
                    return;
                }
                button.set_sensitive(false);
                s.status.set_title(&gettext("Testing…"));

                let (tx, rx) = async_channel::bounded::<Probe>(1);
                let base = cfg.gitlab.base().to_string();
                rt.spawn(async move {
                    let probe = match crate::gitlab::client::Client::new(&base, &tok) {
                        Ok(c) => c.probe().await,
                        Err(e) => Probe::Failed(format!("{e:#}")),
                    };
                    let _ = tx.force_send(probe);
                });

                let s2 = s.clone();
                let button = button.clone();
                glib::spawn_future_local(async move {
                    match rx.recv().await {
                        Ok(Probe::Ok(v)) => s2.status.set_title(
                            // Translators: {} is the GitLab version, e.g. "17.4.1".
                            &gettext("Connected to GitLab {}").replace("{}", &v),
                        ),
                        Ok(Probe::Redirect(base)) => {
                            // Worth naming: a redirecting base URL also decodes
                            // the %2F in project paths, so every lookup 404s
                            // with nothing to explain why.
                            tracing::warn!(%base, "the instance URL redirects");
                            s2.status.set_title(
                                // Translators: {} is a URL like https://gitlab.example.com.
                                &gettext("This address redirects. Use {} instead.")
                                    .replace("{}", &base),
                            );
                        }
                        Ok(Probe::Unauthorized) => {
                            s2.status.set_title(&gettext("The token was rejected"))
                        }
                        Ok(Probe::Failed(e)) => {
                            tracing::warn!(%e, "connection test failed");
                            s2.status.set_title(&gettext("Connection failed"))
                        }
                        Err(_) => s2.status.set_title(&gettext("Connection failed")),
                    }
                    button.set_sensitive(true);
                });
            });
        }

        this
    }

    pub fn connect_saved(self: &Rc<Self>, f: impl Fn() + 'static) {
        *self.on_saved.borrow_mut() = Some(Box::new(f));
    }

    fn save(self: &Rc<Self>) {
        let cfg = config::Config {
            gitlab: config::GitLab {
                instance: self.instance.text().to_string(),
                main_project: self.project.text().to_string(),
                main_group: self.group.text().to_string(),
            },
        };
        if let Err(e) = config::save(&cfg) {
            tracing::error!(?e, "could not save the configuration");
            return;
        }
        if let Some(cb) = self.on_saved.borrow().as_ref() {
            cb();
        }
    }
}
