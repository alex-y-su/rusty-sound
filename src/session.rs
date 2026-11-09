//! Session storage: the captured client_id and OAuth header, kept in a
//! user-only file under the config directory.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct Session {
    pub client_id: String,
    /// Full `Authorization` header value, e.g. `OAuth 2-xxxx-...`.
    pub authorization: String,
}

fn path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config/rusty-sound/session.json"))
}

pub fn load() -> Result<Option<Session>> {
    let p = path()?;
    if !p.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&p)?;
    Ok(Some(serde_json::from_str(&raw)?))
}

pub fn save(session: &Session) -> Result<()> {
    let p = path()?;
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir)?;
    }
    // 0600: readable only by the current user.
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&p)?;
    f.write_all(serde_json::to_string(session)?.as_bytes())?;
    Ok(())
}
