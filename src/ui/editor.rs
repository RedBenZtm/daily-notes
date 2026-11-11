//! The markdown source editor.

use gtk::prelude::*;
use sourceview5::prelude::*;

pub fn build() -> (sourceview5::View, sourceview5::Buffer) {
    let buffer = sourceview5::Buffer::new(None);

    if let Some(lang) = language() {
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

/// The markdown flavour this app writes; see the comment in the spec file.
const LANGUAGE_SPEC: &str = include_str!("../../data/language-specs/daily-notes-markdown.lang");

fn language() -> Option<sourceview5::Language> {
    static SETUP: std::sync::Once = std::sync::Once::new();
    let manager = sourceview5::LanguageManager::default();
    // The manager only reads specs from directories, and only until it first
    // loads one, so the embedded spec is written out before that.
    SETUP.call_once(|| {
        let dir = gtk::glib::user_runtime_dir().join("daily-notes/language-specs");
        let written = std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(dir.join("daily-notes-markdown.lang"), LANGUAGE_SPEC));
        match written {
            Ok(()) => manager.prepend_search_path(&dir.to_string_lossy()),
            Err(e) => tracing::warn!(?e, "could not write the markdown language spec"),
        }
    });
    manager
        .language("daily-notes-markdown")
        .or_else(|| manager.language("markdown"))
}

pub fn text(buffer: &sourceview5::Buffer) -> String {
    let (start, end) = buffer.bounds();
    buffer.text(&start, &end, false).to_string()
}

#[cfg(test)]
mod tests {
    use sourceview5::prelude::*;

    /// Whether the character at `offset` on the only line is drawn bold,
    /// which is how the Adwaita schemes style headings.
    fn bold_at(text: &str, offset: i32) -> bool {
        let (_, buffer) = super::build();
        buffer.set_text(text);
        buffer.ensure_highlight(&buffer.start_iter(), &buffer.end_iter());
        buffer
            .iter_at_offset(offset)
            .tags()
            .iter()
            .any(|t| t.weight() >= 700)
    }

    #[test]
    fn a_leading_reference_is_not_a_heading() {
        // Highlighting needs a display; skip where there is none, e.g. CI.
        if gtk::init().is_err() {
            return;
        }
        assert!(bold_at("# Today", 3), "control: a real heading is bold");
        assert!(!bold_at("#142 fixed the login", 8));
        assert!(!bold_at("##foo bar", 6));
    }
}
