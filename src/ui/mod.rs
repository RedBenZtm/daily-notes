pub mod completion;
pub mod dates;
pub mod editor;
pub mod lines;
pub mod markup;
pub mod preview;
pub mod quicknote;
pub mod refs;
pub mod settings;
pub mod window;


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
