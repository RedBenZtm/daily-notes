//! Localisation.
//!
//! The app follows the desktop's language: `setlocale(LC_ALL, "")` adopts
//! whatever GNOME set in the environment, so there is no in-app language
//! picker to keep in sync. `LC_MESSAGES` drives UI text and `LC_TIME` drives
//! date formatting — GNOME exposes those as "Language" and "Formats"
//! separately, and users do mix them (German UI, ISO dates).
//!
//! Shipped languages: English (source), German, Spanish, French.

use std::path::PathBuf;

#[allow(unused_imports)]
pub use gettextrs::{gettext, ngettext, pgettext};

pub const DOMAIN: &str = "daily-notes";

/// Where the compiled `.mo` catalogues live.
///
/// `DAILY_NOTES_LOCALEDIR` wins so the app can run translated straight from a
/// build tree; otherwise the XDG data dirs, which is where `install.sh` puts
/// them.
fn locale_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("DAILY_NOTES_LOCALEDIR") {
        return PathBuf::from(dir);
    }
    // Running straight out of a build tree: target/<profile>/daily-notes has
    // its catalogues one level up, in target/locale. Checking this means a
    // dev build is translated without anyone setting an env var — which
    // matters because a desktop keybinding cannot set one.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent().and_then(|p| p.parent()).map(|p| p.join("locale")) {
            if dir.join("de/LC_MESSAGES").join(format!("{DOMAIN}.mo")).exists() {
                return dir;
            }
        }
    }

    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("/usr/share"));
    let local = data_home.join("locale");
    if local.join("de/LC_MESSAGES").join(format!("{DOMAIN}.mo")).exists() {
        local
    } else {
        PathBuf::from("/usr/share/locale")
    }
}

pub fn init() {
    // "" means: take the locale from the environment, i.e. from GNOME.
    //
    // SAFETY: setlocale mutates process-global state and is not thread-safe.
    // This runs once at the top of main(), before any thread — the tokio
    // runtime, the tray, GTK — has been started.
    let locale = unsafe { gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "") };
    let dir = locale_dir();

    if let Err(e) = gettextrs::bindtextdomain(DOMAIN, &dir) {
        tracing::warn!(?e, ?dir, "bindtextdomain failed; falling back to English");
    }
    // Catalogues are UTF-8; without this glibc would transcode to the locale's
    // codeset and mangle anything outside it.
    let _ = gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8");
    if let Err(e) = gettextrs::textdomain(DOMAIN) {
        tracing::warn!(?e, "textdomain failed; falling back to English");
    }

    tracing::info!(
        locale = ?locale.as_ref().map(|l| String::from_utf8_lossy(l).into_owned()),
        locale_dir = ?dir,
        "localisation initialised"
    );
}
