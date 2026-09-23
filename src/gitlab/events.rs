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
    /// A GitLab reference like `!313`, for inserting into a note.
    pub reference: Option<String>,
}

// NOTE: every one of these calls passes a *literal* to gettext. Wrapping it
// in a helper that takes the msgid as a parameter hides the literal from the
// extractor, and the strings silently never reach the catalogue — which is
// exactly what happened here the first time.
/// Turns an event into a line of prose, in the user's language.
pub fn describe(e: &EventRec, project_path: &str, project_web_url: &str) -> Activity {
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

    Activity {
        at,
        icon,
        text,
        url,
        project: project_path.to_string(),
        reference,
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
