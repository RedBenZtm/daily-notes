//! `~/.daily_notes/config.toml`, plus the GitLab token in the GNOME keyring.
//!
//! The token never touches the TOML file: it goes to the secret service, so a
//! backup or a `cat` of the config cannot leak it.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const KEYRING_SERVICE: &str = "daily-notes";
const KEYRING_ACCOUNT: &str = "gitlab-token";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub gitlab: GitLab,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitLab {
    /// e.g. `https://gitlab.example.com`. Empty means "not configured".
    #[serde(default)]
    pub instance: String,
    /// Makes a bare `#123` resolve here, e.g. `group/project`.
    #[serde(default)]
    pub main_project: String,
    /// Makes a bare `&7` resolve here.
    #[serde(default)]
    pub main_group: String,
}

impl GitLab {
    pub fn is_configured(&self) -> bool {
        !self.instance.trim().is_empty() && !self.main_project.trim().is_empty()
    }

    /// The instance URL without a trailing slash, so paths can be appended.
    pub fn base(&self) -> &str {
        self.instance.trim().trim_end_matches('/')
    }
}

pub fn dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".daily_notes"))
}

pub fn path() -> Result<PathBuf> {
    Ok(dir()?.join("config.toml"))
}

pub fn load() -> Config {
    let Ok(path) = path() else {
        return Config::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => match toml::from_str(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                // A malformed config must not stop note-taking.
                tracing::error!(?e, path = %path.display(), "config is not valid TOML; using defaults");
                Config::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => {
            tracing::error!(?e, path = %path.display(), "could not read config");
            Config::default()
        }
    }
}

pub fn save(cfg: &Config) -> Result<()> {
    let dir = dir()?;
    std::fs::create_dir_all(&dir)?;
    let path = path()?;
    let text = toml::to_string_pretty(cfg)?;
    // Write-then-rename so a crash cannot leave a half-written config.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn entry() -> Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT).context("opening the keyring entry")
}

/// Runs a keyring operation without letting it take the process down.
///
/// The secret-service backend panics rather than returning an error in at
/// least two situations we have actually hit: its internal mutex poisons
/// ("Mutex failure in credential store: please report a bug"), and it drives
/// zbus with its own `block_on`, which aborts with "Cannot start a runtime
/// from within a runtime" if it is ever reached from a thread already driving
/// async tasks. Losing a token lookup is survivable; losing the GTK main loop
/// to a panic is not — that leaves a tray whose menu silently does nothing.
fn guarded<T>(what: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(_) => {
            tracing::error!(what, "the keyring backend panicked; treating it as unavailable");
            anyhow::bail!("the keyring backend panicked while {what}")
        }
    }
}

pub fn token() -> Option<String> {
    let got = guarded("reading the token", || {
        entry()?.get_password().context("reading the token")
    });
    match got {
        Ok(t) if !t.trim().is_empty() => Some(t),
        Ok(_) => None,
        Err(e) => {
            tracing::debug!(?e, "no GitLab token in the keyring");
            None
        }
    }
}

pub fn set_token(token: &str) -> Result<()> {
    let token = token.to_string();
    guarded("storing the token", move || {
        let e = entry()?;
        if token.trim().is_empty() {
            // delete_credential errors when there is nothing stored; that is a
            // success from the caller's point of view.
            let _ = e.delete_credential();
            return Ok(());
        }
        e.set_password(&token)
            .context("storing the token in the keyring")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_config_is_just_defaults() {
        let cfg = Config::default();
        assert!(!cfg.gitlab.is_configured());
    }

    #[test]
    fn base_url_is_normalised() {
        let g = GitLab {
            instance: "  https://gitlab.example.com/  ".into(),
            main_project: "group/project".into(),
            main_group: String::new(),
        };
        assert_eq!(g.base(), "https://gitlab.example.com");
        assert!(g.is_configured());
    }

    #[test]
    fn round_trips_through_toml() {
        let cfg = Config {
            gitlab: GitLab {
                instance: "https://gitlab.example.com".into(),
                main_project: "group/project".into(),
                main_group: "group".into(),
            },
        };
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.gitlab.main_project, "group/project");
        // the token must never be serialised into the file
        assert!(!text.to_lowercase().contains("token"));
    }

    /// Touches the real secret service, so it is opt-in:
    ///   cargo test -- --ignored keyring_round_trip
    #[test]
    #[ignore]
    fn keyring_round_trip() {
        set_token("test-token-please-ignore").expect("store");
        assert_eq!(token().as_deref(), Some("test-token-please-ignore"));
        set_token("").expect("clear");
        assert_eq!(token(), None);
    }

    #[test]
    fn a_partial_config_still_parses() {
        let cfg: Config = toml::from_str("[gitlab]\ninstance = \"https://x\"\n").unwrap();
        assert_eq!(cfg.gitlab.instance, "https://x");
        assert!(cfg.gitlab.main_project.is_empty());
        assert!(!cfg.gitlab.is_configured());
    }
}
