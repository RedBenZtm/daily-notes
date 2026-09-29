//! Locale-aware date formatting.
//!
//! Deliberately goes through `glib::DateTime`, which honours `LC_TIME`. GNOME
//! exposes "Language" and "Formats" as separate settings and people mix them
//! (German UI, ISO dates), so date strings must not come from the message
//! catalogue.

use chrono::{Datelike, NaiveDate, Weekday};
use gtk::glib;

use crate::i18n::gettext;

fn to_glib(date: NaiveDate) -> Option<glib::DateTime> {
    glib::DateTime::from_local(date.year(), date.month() as i32, date.day() as i32, 0, 0, 0.0).ok()
}

fn format(date: NaiveDate, fmt: &str) -> String {
    to_glib(date)
        .and_then(|d| d.format(fmt).ok())
        .map(|s| s.replace("  ", " "))
        .unwrap_or_else(|| date.to_string())
}

/// "Wed 23 Sep" — the compact form used in the tray.
pub fn short(date: NaiveDate) -> String {
    format(date, "%a %e %b")
}

/// "Wednesday, 23 September 2026" — the long form for tooltips and headings.
pub fn long(date: NaiveDate) -> String {
    format(date, "%A, %e %B %Y")
}

/// "Today", "Yesterday", "Last Friday" — how a person would say it, relative
/// to `today`. Beyond the past week it falls back to the short form, with the
/// year added once it is not this year's.
///
/// Pair it with [`long`] as a tooltip wherever it is shown, since "Last
/// Friday" alone does not say which Friday.
///
/// The words come from the message catalogue rather than `LC_TIME`: they are
/// language, not format. Whole phrases per weekday rather than "Last {}",
/// because the adjective agrees with the day in some languages.
pub fn relative(date: NaiveDate, today: NaiveDate) -> String {
    match (today - date).num_days() {
        0 => gettext("Today"),
        1 => gettext("Yesterday"),
        -1 => gettext("Tomorrow"),
        2..=6 => last_weekday(date.weekday()),
        _ if date.year() == today.year() => short(date),
        _ => format(date, "%a %e %b %Y"),
    }
}

fn last_weekday(day: Weekday) -> String {
    // Translators: a day within the past week, shown instead of its date.
    match day {
        Weekday::Mon => gettext("Last Monday"),
        Weekday::Tue => gettext("Last Tuesday"),
        Weekday::Wed => gettext("Last Wednesday"),
        Weekday::Thu => gettext("Last Thursday"),
        Weekday::Fri => gettext("Last Friday"),
        Weekday::Sat => gettext("Last Saturday"),
        Weekday::Sun => gettext("Last Sunday"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    // 2026-09-29 is a Tuesday.
    const TODAY: &str = "2026-09-29";

    #[test]
    fn names_the_nearest_days() {
        let today = d(TODAY);
        assert_eq!(relative(today, today), "Today");
        assert_eq!(relative(d("2026-09-28"), today), "Yesterday");
        assert_eq!(relative(d("2026-09-30"), today), "Tomorrow");
    }

    #[test]
    fn names_the_weekday_within_the_past_week() {
        let today = d(TODAY);
        assert_eq!(relative(d("2026-09-27"), today), "Last Sunday");
        assert_eq!(relative(d("2026-09-25"), today), "Last Friday");
        assert_eq!(relative(d("2026-09-23"), today), "Last Wednesday");
    }

    #[test]
    fn falls_back_to_the_date_after_a_week() {
        let today = d(TODAY);
        // A week ago would be "Last Tuesday", which reads as today's weekday.
        assert_eq!(relative(d("2026-09-22"), today), short(d("2026-09-22")));
        assert_eq!(relative(d("2026-10-02"), today), short(d("2026-10-02")));
    }

    #[test]
    fn adds_the_year_when_it_differs() {
        let got = relative(d("2025-12-31"), d(TODAY));
        assert!(got.contains("2025"), "{got}");
        assert!(!relative(d("2026-01-02"), d(TODAY)).contains("2026"));
    }
}
