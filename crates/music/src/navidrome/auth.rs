use std::path::PathBuf;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::credentials;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Credentials {
    pub server: String,
    pub username: String,
    pub password: String,
    /// What the last sign-in handed back, so a launch does not sign in again: the bearer token
    /// lasts two days and is renewed when the server turns it away, while the subsonic token
    /// and salt never expire and keep every cover and stream url the same one across launches.
    #[serde(default)]
    pub session: Session,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// The bearer token the native api takes. The server hands a stretched one back on every
    /// answer, and a call it turns down signs in again for a fresh one.
    #[serde(rename = "token", default)]
    pub jwt: String,
    /// The subsonic token and salt the `rest` routes are signed with instead, as navidrome's
    /// own ui streams and fetches covers.
    #[serde(default)]
    pub subsonic_token: String,
    #[serde(default)]
    pub subsonic_salt: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

fn path() -> PathBuf {
    credentials::dir("navidrome").join(credentials::FILE)
}

pub fn normalize_server(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        anyhow::bail!("the server address is empty");
    }
    let with_scheme = match trimmed.contains("://") {
        true => trimmed.to_owned(),
        false => format!("http://{trimmed}"),
    };
    Ok(with_scheme)
}

pub fn load() -> Option<Credentials> {
    let bytes = std::fs::read(path()).ok()?;
    let mut credentials: Credentials = serde_json::from_slice(&bytes).ok()?;
    credentials.server = normalize_server(&credentials.server).ok()?;
    match credentials.username.is_empty() {
        true => None,
        false => Some(credentials),
    }
}

pub fn store(stored: &Credentials) -> Result<()> {
    let bytes =
        serde_json::to_vec_pretty(stored).context("cannot serialize navidrome credentials")?;
    credentials::write(&path(), &bytes).context("cannot store navidrome credentials")
}

pub fn forget() {
    credentials::remove(&path());
}
