//! Minimal GitLab REST v4 client — only the handful of endpoints this app needs.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::refs::{Kind, Reference};

/// What a resolved reference looks like to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub title: String,
    pub state: String,
    pub web_url: String,
    pub author: String,
}

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

#[derive(Debug, Deserialize)]
struct Issueish {
    title: String,
    state: String,
    web_url: String,
    #[serde(default)]
    author: Option<Author>,
}

#[derive(Debug, Deserialize)]
struct Author {
    username: String,
}

#[derive(Debug, Deserialize)]
struct UserRec {
    username: String,
    name: String,
    web_url: String,
    #[serde(default)]
    state: String,
}

/// Used by the main-project picker in Settings.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct ProjectRec {
    pub id: u64,
    pub path_with_namespace: String,
    pub name_with_namespace: String,
    pub web_url: String,
}

#[derive(Debug, Deserialize)]
struct Version {
    version: String,
}

/// GitLab wants the project path percent-encoded *including* the slashes.
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len() * 3);
    for b in path.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl Client {
    pub fn new(base: &str, token: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            // A slow instance must never make the editor feel stuck; the UI
            // paints from cache and these calls only ever refresh it.
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .user_agent(concat!("daily-notes/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building the HTTP client")?;
        Ok(Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
        })
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<Option<T>> {
        let url = format!("{}/api/v4/{}", self.base, path.trim_start_matches('/'));
        let res = self
            .http
            .get(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .await
            .with_context(|| format!("requesting {url}"))?;

        match res.status() {
            s if s.is_success() => Ok(Some(res.json::<T>().await.context("decoding the response")?)),
            // A reference to something that does not exist, or that this token
            // cannot see, is a normal state — not an error to report.
            reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::FORBIDDEN => Ok(None),
            reqwest::StatusCode::UNAUTHORIZED => {
                bail!("GitLab rejected the access token (401)")
            }
            s => bail!("GitLab returned {s} for {url}"),
        }
    }

    /// Cheap call used by Settings' "Test connection".
    pub async fn version(&self) -> Result<String> {
        let v: Option<Version> = self.get("version").await?;
        Ok(v.map(|v| v.version).unwrap_or_default())
    }

    #[allow(dead_code)]
    pub async fn search_projects(&self, query: &str) -> Result<Vec<ProjectRec>> {
        let q = encode_path(query);
        let path = format!("projects?membership=true&search={q}&per_page=20&order_by=last_activity_at");
        Ok(self.get::<Vec<ProjectRec>>(&path).await?.unwrap_or_default())
    }

    /// Resolves one reference. `Ok(None)` means "no such thing", which is not
    /// an error — the editor just leaves that reference undecorated.
    pub async fn resolve(
        &self,
        r: &Reference,
        default_project: &str,
        default_group: &str,
    ) -> Result<Option<Resolved>> {
        match r.kind {
            Kind::User => {
                let Some(username) = r.username.as_deref() else {
                    return Ok(None);
                };
                let users: Option<Vec<UserRec>> = self
                    .get(&format!("users?username={}", encode_path(username)))
                    .await?;
                Ok(users.and_then(|v| v.into_iter().next()).map(|u| Resolved {
                    title: u.name,
                    state: u.state,
                    web_url: u.web_url,
                    author: u.username,
                }))
            }
            Kind::Epic => {
                let group = r.path.as_deref().unwrap_or(default_group);
                if group.is_empty() {
                    return Ok(None);
                }
                let path = format!(
                    "groups/{}/epics/{}",
                    encode_path(group),
                    r.iid.unwrap_or_default()
                );
                Ok(self.get::<Issueish>(&path).await?.map(Into::into))
            }
            Kind::Issue | Kind::MergeRequest => {
                let project = r.path.as_deref().unwrap_or(default_project);
                if project.is_empty() {
                    return Ok(None);
                }
                let what = if r.kind == Kind::Issue {
                    "issues"
                } else {
                    "merge_requests"
                };
                let path = format!(
                    "projects/{}/{}/{}",
                    encode_path(project),
                    what,
                    r.iid.unwrap_or_default()
                );
                Ok(self.get::<Issueish>(&path).await?.map(Into::into))
            }
        }
    }
}

impl From<Issueish> for Resolved {
    fn from(i: Issueish) -> Self {
        Resolved {
            title: i.title,
            state: i.state,
            web_url: i.web_url,
            author: i.author.map(|a| a.username).unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_paths_are_fully_encoded() {
        // GitLab needs the slashes encoded too, or the path is read as URL
        // segments and the request 404s.
        assert_eq!(encode_path("group/project"), "group%2Fproject");
        assert_eq!(encode_path("group/sub/proj"), "group%2Fsub%2Fproj");
        assert_eq!(encode_path("a.b-c_d"), "a.b-c_d");
        assert_eq!(encode_path("Grüße"), "Gr%C3%BC%C3%9Fe");
        assert_eq!(encode_path("a b"), "a%20b");
    }
}
