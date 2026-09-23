//! The markdown source editor.

use gtk::prelude::*;
use sourceview5::prelude::*;

pub fn build() -> (sourceview5::View, sourceview5::Buffer) {
    let buffer = sourceview5::Buffer::new(None);

    if let Some(lang) = sourceview5::LanguageManager::default().language("markdown") {
        buffer.set_language(Some(&lang));
    }
    // Follow the desktop's light/dark preference rather than a fixed scheme.
    let scheme_id = if adw::StyleManager::default().is_dark() {
        "Adwaita-dark"
    } else {
        "Adwaita"
    };
    if let Some(scheme) = sourceview5::StyleSchemeManager::default().scheme(scheme_id) {
        buffer.set_style_scheme(Some(&scheme));
    }

    let view = sourceview5::View::with_buffer(&buffer);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_monospace(true);
    view.set_auto_indent(true);
    view.set_indent_on_tab(true);
    view.set_smart_backspace(true);
    view.set_show_line_numbers(false);
    view.set_highlight_current_line(false);
    view.set_top_margin(12);
    view.set_bottom_margin(96); // room to type at the bottom of the day
    view.set_left_margin(16);
    view.set_right_margin(16);
    view.set_pixels_above_lines(2);
    view.set_pixels_below_lines(2);

    // Keep the scheme in step when the user flips light/dark at runtime.
    let buf = buffer.clone();
    adw::StyleManager::default().connect_dark_notify(move |sm| {
        let id = if sm.is_dark() { "Adwaita-dark" } else { "Adwaita" };
        if let Some(scheme) = sourceview5::StyleSchemeManager::default().scheme(id) {
            buf.set_style_scheme(Some(&scheme));
        }
    });

    crate::ui::lines::install(&view, &buffer);

    (view, buffer)
}

pub fn text(buffer: &sourceview5::Buffer) -> String {
    let (start, end) = buffer.bounds();
    buffer.text(&start, &end, false).to_string()
}
