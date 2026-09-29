pub mod activity;
pub mod completion;
pub mod dates;
pub mod day_pane;
pub mod editor;
pub mod entry_completion;
pub mod lines;
pub mod markup;
pub mod preview;
pub mod quicknote;
pub mod refs;
pub mod settings;
pub mod window;


/// Makes the app's own icons resolvable.
///
/// Both the in-tree directory (so a build tree looks right without installing)
/// and the XDG one the install step writes to.
pub fn init_icons() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let theme = gtk::IconTheme::for_display(&display);
    theme.add_search_path(concat!(env!("CARGO_MANIFEST_DIR"), "/data/icons"));
    if let Some(home) = std::env::var_os("HOME") {
        theme.add_search_path(std::path::Path::new(&home).join(".local/share/icons"));
    }
}

/// Loads the app stylesheet into the default display.
pub fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("../../data/style.css"));
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
