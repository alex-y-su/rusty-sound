//! launchd control. The agent never runs at login or on demand by itself
//! (RunAtLoad and KeepAlive are off); `start` and `stop` switch it.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::client;

pub const LABEL: &str = "com.rusty-sound.daemon";

fn home() -> Result<PathBuf> {
    Ok(PathBuf::from(std::env::var("HOME").context("HOME is not set")?))
}

fn plist_path() -> Result<PathBuf> {
    Ok(home()?.join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}

fn log_path() -> Result<PathBuf> {
    Ok(home()?.join("Library/Logs/rusty-sound.log"))
}

fn uid() -> Result<String> {
    let out = Command::new("id").arg("-u").output()?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn target() -> Result<String> {
    Ok(format!("gui/{}/{LABEL}", uid()?))
}

/// Runs launchctl quietly; its own messages would corrupt JSON output.
fn launchctl(args: &[&str]) -> Result<bool> {
    let out = Command::new("launchctl").args(args).output()?;
    Ok(out.status.success())
}

fn is_loaded() -> Result<bool> {
    launchctl(&["print", &target()?])
}

fn write_plist() -> Result<()> {
    let exe = std::env::current_exe()?;
    let log = log_path()?;
    if let Some(dir) = log.parent() {
        fs::create_dir_all(dir)?;
    }
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>serve</string>
    </array>
    <key>RunAtLoad</key>
    <false/>
    <key>KeepAlive</key>
    <false/>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
        exe = exe.display(),
        log = log.display(),
    );
    let path = plist_path()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&path, plist)?;
    Ok(())
}

/// Registers the agent with launchd without starting it.
pub fn install() -> Result<Value> {
    write_plist()?;
    if is_loaded()? {
        launchctl(&["bootout", &format!("gui/{}", uid()?), plist_path()?.to_str().unwrap()])?;
    }
    let ok = launchctl(&[
        "bootstrap",
        &format!("gui/{}", uid()?),
        plist_path()?.to_str().unwrap(),
    ])?;
    anyhow::ensure!(ok, "launchctl bootstrap failed");
    Ok(json!({"installed": true, "plist": plist_path()?.display().to_string()}))
}

/// Starts the agent now. Installs it first if needed.
pub async fn start() -> Result<Value> {
    if !is_loaded()? {
        install()?;
    }
    anyhow::ensure!(launchctl(&["kickstart", &target()?])?, "launchctl kickstart failed");
    // Give the daemon a moment to bind its socket before reporting.
    for _ in 0..50 {
        if status().await?["running"].as_bool() == Some(true) {
            return Ok(json!({"started": true}));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    anyhow::bail!("service did not come up; see {}", log_path()?.display())
}

/// Stops the running agent. The daemon's idle timer also stops it on its own.
pub async fn stop() -> Result<Value> {
    if status().await?["running"].as_bool() != Some(true) {
        return Ok(json!({"stopped": false, "reason": "not running"}));
    }
    launchctl(&["kill", "SIGTERM", &target()?])?;
    for _ in 0..50 {
        if status().await?["running"].as_bool() != Some(true) {
            return Ok(json!({"stopped": true}));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    anyhow::bail!("service did not stop in time")
}

/// Reports whether the agent is registered with launchd and whether it answers.
pub async fn status() -> Result<Value> {
    let running = client::call(json!({"cmd": "ping"})).await.is_ok();
    Ok(json!({
        "loaded": is_loaded()?,
        "running": running,
        "socket": client::socket_path()?.display().to_string(),
        "log": log_path()?.display().to_string(),
    }))
}

/// Stops the agent if it runs, and removes its launchd registration.
pub async fn uninstall() -> Result<Value> {
    let _ = stop().await;
    if is_loaded()? {
        launchctl(&["bootout", &format!("gui/{}", uid()?), plist_path()?.to_str().unwrap()])?;
    }
    let path = plist_path()?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(json!({"uninstalled": true}))
}
