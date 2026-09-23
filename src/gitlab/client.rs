//! Minimal GitLab REST v4 client — only the handful of endpoints this app needs.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::refs::{Kind, Reference};

/// The outcome of a connection test, so Settings can say something specific.
#[derive(Debug, Clone)]
pub enum Probe {
    Ok(String),
    /// The instance URL redirects; the payload is where it points.
    Redirect(String),
    Unauthorized,
    /// The token works, but cannot see the configured main project — the case
    /// a project access token hits, since it is scoped to one project. Worth
    /// its own message: /version succeeds, so everything looks fine while
    /// every reference silently resolves to "not found".
    ProjectUnreachable(String),
    Failed(String),
}

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
/// Just enough of a project to build URLs and label a row.
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectBrief {
    pub id: u64,
    pub path_with_namespace: String,
    pub name: String,
    pub web_url: String,
}

/// Just enough of an issue to offer it as a completion.
#[derive(Debug, Clone, Deserialize)]
pub struct IssueBrief {
    pub iid: u64,
    pub title: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
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
            // Do not follow redirects. An API base URL should never redirect,
            // and following one silently is actively harmful here: a vanity
            // host that 301s to the canonical one also *decodes* the `%2F` in
            // an encoded project path, so `projects/group%2Fproj/issues/1`
            // becomes `projects/group/proj/issues/1` and every lookup 404s
            // with no hint as to why. Better to report the redirect and name
            // the URL that should have been configured.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("daily-notes/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building the HTTP client")?;
        Ok(Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
        })
    }

    /// Same as `get`, exposed for the modules that build their own paths.
    pub async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<Option<T>> {
        self.get(path).await
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
            s if s.is_redirection() => match redirect_origin(&res) {
                Some(base) => bail!("the instance URL redirects to {base}; configure that instead"),
                None => bail!("the instance URL redirects ({s}); check it"),
            },
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
    ///
    /// Checks the main project too, not just that the credentials work.
    pub async fn probe(&self, main_project: &str) -> Probe {
        let version = self.probe_version().await;
        let Probe::Ok(_) = &version else {
            return version;
        };
        if main_project.trim().is_empty() {
            return version;
        }
        match self
            .get::<serde_json::Value>(&format!("projects/{}", encode_path(main_project)))
            .await
        {
            Ok(Some(_)) => version,
            Ok(None) => Probe::ProjectUnreachable(main_project.to_string()),
            Err(e) => Probe::Failed(format!("{e:#}")),
        }
    }

    async fn probe_version(&self) -> Probe {
        let url = format!("{}/api/v4/version", self.base);
        let res = match self
            .http
            .get(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return Probe::Failed(format!("{e}")),
        };
        match res.status() {
            s if s.is_success() => match res.json::<Version>().await {
                Ok(v) => Probe::Ok(v.version),
                Err(e) => Probe::Failed(format!("{e}")),
            },
            s if s.is_redirection() => match redirect_origin(&res) {
                Some(base) => Probe::Redirect(base),
                None => Probe::Failed(format!("{s}")),
            },
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                Probe::Unauthorized
            }
            s => Probe::Failed(format!("{s}")),
        }
    }

    /// One project, for turning an event's numeric project_id into a path.
    pub async fn project(&self, id: u64) -> Result<Option<ProjectBrief>> {
        self.get::<ProjectBrief>(&format!("projects/{id}")).await
    }

    /// Open issues of a project, newest first, for `#` completion.
    ///
    /// Paged rather than a single request: a busy project has far more than
    /// one page, and completing only the newest 20 would be useless.
    pub async fn open_issues(&self, project: &str, pages: usize) -> Result<Vec<IssueBrief>> {
        let mut out = Vec::new();
        for page in 1..=pages {
            let path = format!(
                "projects/{}/issues?state=opened&per_page=100&page={page}&order_by=updated_at",
                encode_path(project)
            );
            let batch = self.get::<Vec<IssueBrief>>(&path).await?.unwrap_or_default();
            let done = batch.len() < 100;
            out.extend(batch);
            if done {
                break;
            }
        }
        Ok(out)
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

/// The scheme and host a redirect points at, which is what belongs in the
/// instance URL setting.
fn redirect_origin(res: &reqwest::Response) -> Option<String> {
    let location = res.headers().get(reqwest::header::LOCATION)?.to_str().ok()?;
    let url = reqwest::Url::parse(location).ok()?;
    Some(url.origin().ascii_serialization())
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
