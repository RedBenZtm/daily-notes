//! Locale-aware date formatting.
//!
//! Deliberately goes through `glib::DateTime`, which honours `LC_TIME`. GNOME
//! exposes "Language" and "Formats" as separate settings and people mix them
//! (German UI, ISO dates), so date strings must not come from the message
//! catalogue.

use chrono::{Datelike, NaiveDate};
use gtk::glib;

fn to_glib(date: NaiveDate) -> Option<glib::DateTime> {
    glib::DateTime::from_local(date.year(), date.month() as i32, date.day() as i32, 0, 0, 0.0).ok()
}

/// "Wed 23 Sep" — the compact form used in the header and the tray.
pub fn short(date: NaiveDate) -> String {
    to_glib(date)
        .and_then(|d| d.format("%a %e %b").ok())
        .map(|s| s.replace("  ", " "))
        .unwrap_or_else(|| date.to_string())
}

/// "Wednesday, 23 September 2026" — the long form for tooltips and headings.
pub fn long(date: NaiveDate) -> String {
    to_glib(date)
        .and_then(|d| d.format("%A, %e %B %Y").ok())
        .map(|s| s.replace("  ", " "))
        .unwrap_or_else(|| date.to_string())
}
