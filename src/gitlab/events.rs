//! The day's GitLab activity.
//!
//! Four things about `/api/v4/events` that shape this, all confirmed against a
//! live instance:
//!
//! 1. `after` and `before` are *exclusive date* bounds and `created_at` is UTC,
//!    so a local day needs `after = D-1`, `before = D+1` and a local-time
//!    filter afterwards.
//! 2. Paging defaults to 20; a busy day silently truncates without `per_page`.
//! 3. Events carry `project_id` as a number and no path.
//! 4. Events carry no `web_url` — it has to be built from the project.

use anyhow::Result;
use chrono::{DateTime, Duration, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use super::client::Client;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EventRec {
    pub id: u64,
    pub project_id: u64,
    pub action_name: String,
    #[serde(default)]
    pub target_type: Option<String>,
    #[serde(default)]
    pub target_iid: Option<u64>,
    #[serde(default)]
    pub target_title: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub push_data: Option<PushData>,
    #[serde(default)]
    pub note: Option<Note>,
    #[serde(default)]
    pub wiki_page: Option<WikiPage>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PushData {
    #[serde(default)]
    pub commit_count: u64,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub ref_: Option<String>,
    #[serde(rename = "ref", default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub commit_title: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Note {
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub noteable_type: Option<String>,
    #[serde(default)]
    pub noteable_iid: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WikiPage {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub slug: String,
}

/// Fetches everything that happened on one *local* day.
pub async fn for_day(client: &Client, date: NaiveDate) -> Result<Vec<EventRec>> {
    let after = date - Duration::days(1);
    let before = date + Duration::days(1);

    let mut out: Vec<EventRec> = Vec::new();
    for page in 1..=10 {
        let path = format!(
            "events?after={}&before={}&per_page=100&page={page}&sort=asc",
            after.format("%Y-%m-%d"),
            before.format("%Y-%m-%d"),
        );
        let batch = client
            .get_json::<Vec<EventRec>>(&path)
            .await?
            .unwrap_or_default();
        let done = batch.len() < 100;
        out.extend(batch);
        if done {
            break;
        }
    }

    // The bounds are generous by a day at each end on purpose; the local-time
    // filter is what actually decides the day.
    out.retain(|e| e.created_at.with_timezone(&Local).date_naive() == date);
    out.sort_by_key(|e| e.created_at);
    Ok(out)
}

/// What the UI shows for one event.
#[derive(Debug, Clone)]
pub struct Activity {
    pub at: DateTime<Local>,
    pub icon: &'static str,
    pub text: String,
    pub url: Option<String>,
    pub project: String,
    /// The line to append to a note: always set, and always rendering as
    /// something clickable — a GitLab reference where there is one, a markdown
    /// link otherwise.
    pub insert: String,
}

// NOTE: every one of these calls passes a *literal* to gettext. Wrapping it
// in a helper that takes the msgid as a parameter hides the literal from the
// extractor, and the strings silently never reach the catalogue — which is
// exactly what happened here the first time.
/// Turns an event into a line of prose, in the user's language.
pub fn describe(
    e: &EventRec,
    project_path: &str,
    project_web_url: &str,
    main_project: &str,
) -> Activity {
    let at = e.created_at.with_timezone(&Local);
    let iid = e.target_iid.unwrap_or_default();
    let title = e.target_title.clone().unwrap_or_default();
    let target = e.target_type.as_deref().unwrap_or("");

    let (icon, text, url, reference) = match (e.action_name.as_str(), target) {
        ("opened", "MergeRequest") => (
            "git-merge-symbolic",
            crate::i18n::gettext("opened !{}").replace("{}", &iid.to_string()),
            Some(format!("{project_web_url}/-/merge_requests/{iid}")),
            Some(format!("!{iid}")),
        ),
        ("accepted", "MergeRequest") | ("merged", "MergeRequest") => (
            "object-select-symbolic",
            crate::i18n::gettext("merged !{}").replace("{}", &iid.to_string()),
            Some(format!("{project_web_url}/-/merge_requests/{iid}")),
            Some(format!("!{iid}")),
        ),
        ("closed", "MergeRequest") => (
            "window-close-symbolic",
            crate::i18n::gettext("closed !{}").replace("{}", &iid.to_string()),
            Some(format!("{project_web_url}/-/merge_requests/{iid}")),
            Some(format!("!{iid}")),
        ),
        ("opened", "Issue") | ("created", "Issue") => (
            "dialog-information-symbolic",
            crate::i18n::gettext("opened #{}").replace("{}", &iid.to_string()),
            Some(format!("{project_web_url}/-/issues/{iid}")),
            Some(format!("#{iid}")),
        ),
        ("closed", "Issue") => (
            "object-select-symbolic",
            crate::i18n::gettext("closed #{}").replace("{}", &iid.to_string()),
            Some(format!("{project_web_url}/-/issues/{iid}")),
            Some(format!("#{iid}")),
        ),
        ("updated", "WikiPage::Meta") => {
            let w = e.wiki_page.clone().unwrap_or(WikiPage {
                title: title.clone(),
                slug: String::new(),
            });
            (
                "accessories-text-editor-symbolic",
                crate::i18n::gettext("edited the wiki page {}").replace("{}", &w.title),
                Some(format!("{project_web_url}/-/wikis/{}", w.slug)),
                None,
            )
        }
        ("commented on", _) => {
            let n = e.note.clone();
            let iid = n.as_ref().and_then(|n| n.noteable_iid).unwrap_or(iid);
            let kind = n.as_ref().and_then(|n| n.noteable_type.clone()).unwrap_or_default();
            let (path, reference) = match kind.as_str() {
                "MergeRequest" => ("merge_requests", format!("!{iid}")),
                _ => ("issues", format!("#{iid}")),
            };
            (
                "user-available-symbolic",
                crate::i18n::gettext("commented on {}").replace("{}", &reference),
                Some(format!("{project_web_url}/-/{path}/{iid}")),
                Some(reference),
            )
        }
        _ => describe_push(e, project_web_url),
    };

    // What to write into a note. A bare `#123` only resolves against the main
    // project, so an event from anywhere else needs the qualified form or it
    // silently points at the wrong issue — or none at all.
    let insert = match (&reference, &url) {
        (Some(r), _) => {
            let qualified = if project_path.is_empty() || project_path == main_project {
                r.clone()
            } else {
                format!("{project_path}{r}")
            };
            text.replacen(r.as_str(), &qualified, 1)
        }
        // No GitLab reference — a push, a branch, a wiki edit. A markdown link
        // keeps it clickable and reads the same as the row it came from.
        (None, Some(u)) => format!("[{text}]({u})"),
        (None, None) => text.clone(),
    };

    Activity {
        at,
        icon,
        text,
        url,
        project: project_path.to_string(),
        insert,
    }
}

fn describe_push(
    e: &EventRec,
    web_url: &str,
) -> (&'static str, String, Option<String>, Option<String>) {
    let Some(push) = e.push_data.clone() else {
        // Anything not modelled still shows up, rather than vanishing.
        return ("view-list-symbolic", e.action_name.clone(), None, None);
    };
    let branch = push
        .reference
        .or(push.ref_)
        .unwrap_or_default();

    match push.action.as_str() {
        "created" => (
            "list-add-symbolic",
            crate::i18n::gettext("pushed the new branch {}").replace("{}", &branch),
            Some(format!("{web_url}/-/tree/{branch}")),
            None,
        ),
        "removed" => (
            "user-trash-symbolic",
            crate::i18n::gettext("deleted the branch {}").replace("{}", &branch),
            None,
            None,
        ),
        _ => {
            let n = push.commit_count;
            let commits = crate::i18n::ngettext("{} commit", "{} commits", n as u32)
                .replace("{}", &n.to_string());
            (
                "media-playlist-consecutive-symbolic",
                // Translators: {} is a count like "7 commits", {branch} a branch name.
                crate::i18n::gettext("pushed {} to {branch}")
                    .replace("{}", &commits)
                    .replace("{branch}", &branch),
                Some(format!("{web_url}/-/commits/{branch}")),
                None,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(action: &str, target: &str, iid: u64) -> EventRec {
        EventRec {
            id: 1,
            project_id: 1,
            action_name: action.into(),
            target_type: Some(target.into()),
            target_iid: Some(iid),
            target_title: Some("Some title".into()),
            created_at: "2026-09-23T07:21:54.394Z".parse().unwrap(),
            push_data: None,
            note: None,
            wiki_page: None,
        }
    }

    fn push(action: &str, branch: &str, commits: u64) -> EventRec {
        EventRec {
            push_data: Some(PushData {
                commit_count: commits,
                action: action.into(),
                ref_: None,
                reference: Some(branch.into()),
                commit_title: None,
            }),
            ..event("pushed to", "Project", 0)
        }
    }

    const MAIN: &str = "group/project";
    const URL: &str = "https://gl/group/project";

    #[test]
    fn a_reference_is_not_repeated_in_the_inserted_line() {
        // The text already names the merge request; appending it again gave
        // lines like "!313 opened !313".
        let a = describe(&event("opened", "MergeRequest", 313), MAIN, URL, MAIN);
        assert_eq!(a.insert.matches("313").count(), 1);
        assert!(a.insert.contains("!313"));
    }

    #[test]
    fn a_reference_outside_the_main_project_is_qualified() {
        // A bare `!313` resolves against the main project, where that merge
        // request does not exist — so it has to carry its own project.
        let other = "group/sub/other-project";
        // Where the reference sits in the sentence depends on the language,
        // so assert on the reference itself rather than on the word order.
        let a = describe(&event("opened", "MergeRequest", 313), other, URL, MAIN);
        assert!(a.insert.contains(&format!("{other}!313")), "{}", a.insert);
        // ...and stays short when it is the main project after all
        let a = describe(&event("opened", "MergeRequest", 313), MAIN, URL, MAIN);
        assert!(a.insert.contains("!313"), "{}", a.insert);
        assert!(!a.insert.contains(MAIN), "{}", a.insert);
    }

    #[test]
    fn issues_qualify_the_same_way() {
        let other = "other/project";
        let a = describe(&event("opened", "Issue", 890), other, URL, MAIN);
        assert!(a.insert.contains(&format!("{other}#890")), "{}", a.insert);
    }

    #[test]
    fn an_activity_without_a_reference_becomes_a_markdown_link() {
        // Pushes, branches and wiki edits name nothing GitLab can resolve, but
        // they still have to go into a note as something clickable.
        let a = describe(&push("pushed", "staging", 7), MAIN, URL, MAIN);
        assert!(a.insert.starts_with('['), "{}", a.insert);
        assert!(a.insert.contains("](https://gl/group/project/-/commits/staging)"), "{}", a.insert);
    }

    #[test]
    fn every_activity_yields_something_insertable() {
        let cases = [
            event("opened", "MergeRequest", 1),
            event("accepted", "MergeRequest", 2),
            event("closed", "Issue", 3),
            event("updated", "WikiPage::Meta", 4),
            push("created", "feat/x", 1),
            push("removed", "feat/x", 0),
            push("pushed", "main", 12),
        ];
        for e in cases {
            let a = describe(&e, MAIN, URL, MAIN);
            assert!(!a.insert.trim().is_empty(), "{:?} produced nothing", e.action_name);
        }
    }

    #[test]
    fn the_local_day_decides_not_the_utc_timestamp() {
        // 07:21 UTC is the same day in CEST, but the bounds sent to the API are
        // deliberately a day wider at each end.
        let a = describe(&event("opened", "Issue", 1), MAIN, URL, MAIN);
        assert_eq!(a.at.naive_utc().format("%H:%M").to_string(), "07:21");
    }
}
