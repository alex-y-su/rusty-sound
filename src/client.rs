//! Client side of the daemon socket, plus the error type shared with the daemon.

use std::fmt;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// An error with a stable machine-readable code, reported as JSON.
#[derive(Debug)]
pub struct CliError {
    pub code: &'static str,
    pub message: String,
}

impl CliError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CliError {}

pub fn socket_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config/rusty-sound/rusty-sound.sock"))
}

/// Converts any error into the `{"ok": false, ...}` response shape.
pub fn error_value(err: &anyhow::Error) -> Value {
    let (code, message) = match err.downcast_ref::<CliError>() {
        Some(e) => (e.code, e.message.clone()),
        None => ("error", format!("{err:#}")),
    };
    json!({"ok": false, "error": {"code": code, "message": message}})
}

/// Sends one request to the running daemon and returns its `data`.
pub async fn call(req: Value) -> Result<Value> {
    let path = socket_path()?;
    let stream = UnixStream::connect(&path).await.map_err(|_| {
        CliError::new(
            "service_off",
            "the service is not running; run `rusty-sound start`",
        )
    })?;

    let (reader, mut writer) = stream.into_split();
    writer.write_all(format!("{req}\n").as_bytes()).await?;

    let mut lines = BufReader::new(reader).lines();
    let line = lines
        .next_line()
        .await?
        .ok_or_else(|| CliError::new("protocol", "empty response from service"))?;
    let resp: Value = serde_json::from_str(&line)?;

    if resp["ok"].as_bool() == Some(true) {
        Ok(resp["data"].clone())
    } else {
        let code = resp["error"]["code"].as_str().unwrap_or("error");
        let message = resp["error"]["message"].as_str().unwrap_or("unknown error");
        Err(CliError {
            code: leak_code(code),
            message: message.to_string(),
        }
        .into())
    }
}

/// Error codes come from a fixed set; map the wire string back to a static one.
fn leak_code(code: &str) -> &'static str {
    match code {
        "service_off" => "service_off",
        "bad_request" => "bad_request",
        "unknown_command" => "unknown_command",
        "no_session" => "no_session",
        "protocol" => "protocol",
        _ => "error",
    }
}
