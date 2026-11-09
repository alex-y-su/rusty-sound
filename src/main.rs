mod api;
mod client;
mod daemon;
mod login;
mod playback;
mod service;
mod session;

use std::process::ExitCode;

use anyhow::{Result, bail};
use serde_json::{Value, json};

const USAGE: &str = "usage: rusty-sound [--json] <command>

  login              sign in with Chrome and save the session (interactive)
  install            register the launchd agent (does not start it)
  start | stop       switch the service on or off
  status             show whether the service is registered and running
  likes [max]        list your likes
  discover           list Discover sections and their playlists
  play [max]         queue your liked tracks and play them
  reset              stop, turn radio off, and start over from the top of your likes
  pause | resume     pause or resume playback
  next | prev       skip to the next or previous track
  radio [off]        queue tracks related to the current one, topping up as you go
  clear              stop playback and empty the queue
  now                show what is playing
  uninstall          stop and unregister the service
  serve              run the service in the foreground (used by launchd)";

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let json_out = raw.iter().any(|a| a == "--json");
    let args: Vec<String> = raw.into_iter().filter(|a| a != "--json").collect();

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };

    match rt.block_on(run(&args)) {
        Ok(data) => {
            if json_out {
                println!("{}", json!({"ok": true, "data": data}));
            } else if !data.is_null() {
                print!("{}", render(&args, &data));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            if json_out {
                println!("{}", client::error_value(&e));
            } else {
                eprintln!("error: {e:#}");
            }
            ExitCode::from(1)
        }
    }
}

async fn run(args: &[String]) -> Result<Value> {
    match args.first().map(String::as_str) {
        Some("login") => {
            // Only one browser login at a time: stop the service so it can't hold stale state.
            service::stop().await?;
            login::run().await?;
            Ok(json!({"saved": true}))
        }
        Some("install") => service::install(),
        Some("start") => service::start().await,
        Some("stop") => service::stop().await,
        Some("status") => service::status().await,
        Some("uninstall") => service::uninstall().await,
        Some("serve") => {
            daemon::serve().await?;
            Ok(Value::Null)
        }
        Some("likes") => {
            let max = match args.get(1) {
                Some(s) => Some(s.parse::<u64>()?),
                None => None,
            };
            client::call(json!({"cmd": "likes", "max": max})).await
        }
        Some("discover") => client::call(json!({"cmd": "discover"})).await,
        Some("reset") => client::call(json!({"cmd": "reset"})).await,
        Some("play") => {
            let max = match args.get(1) {
                Some(s) => Some(s.parse::<u64>()?),
                None => None,
            };
            client::call(json!({"cmd": "play", "max": max})).await
        }
        Some("radio") => {
            let on = args.get(1).map(String::as_str) != Some("off");
            client::call(json!({"cmd": "radio", "on": on})).await
        }
        Some(cmd @ ("pause" | "resume" | "next" | "prev" | "clear" | "now")) => {
            client::call(json!({"cmd": cmd})).await
        }
        Some(other) => bail!("unknown command: {other}\n\n{USAGE}"),
        None => bail!("{USAGE}"),
    }
}

/// Human-readable output for the default (non-JSON) mode.
fn render(args: &[String], data: &Value) -> String {
    match args.first().map(String::as_str) {
        Some("likes") => {
            let mut s = format!("Likes for {}:\n", data["user"].as_str().unwrap_or("?"));
            for (i, l) in data["likes"].as_array().into_iter().flatten().enumerate() {
                let title = l["title"].as_str().unwrap_or("untitled");
                let artist = l["artist"].as_str().unwrap_or("unknown");
                let tag = if l["kind"] == "playlist" { "[playlist] " } else { "" };
                let dur = match l["duration_ms"].as_u64() {
                    Some(ms) => format!(" [{}]", fmt_duration(ms)),
                    None => String::new(),
                };
                s.push_str(&format!("{:>4}. {tag}{title} — {artist}{dur}\n", i + 1));
            }
            s
        }
        Some("discover") => {
            let mut s = String::new();
            for sec in data["sections"].as_array().into_iter().flatten() {
                s.push_str(&format!("\n== {}\n", sec["title"].as_str().unwrap_or("untitled")));
                for pl in sec["playlists"].as_array().into_iter().flatten() {
                    s.push_str(&format!("  - {}\n", pl["title"].as_str().unwrap_or("untitled")));
                }
            }
            s
        }
        Some("status") => format!(
            "loaded: {}\nrunning: {}\n",
            data["loaded"], data["running"]
        ),
        Some("now") => {
            let state = if data["playing"].as_bool() == Some(true) {
                if data["paused"].as_bool() == Some(true) { "paused" } else { "playing" }
            } else {
                "stopped"
            };
            let now = data["now"].as_str().unwrap_or("-");
            format!("{state}: {now} (queued: {})\n", data["queued"])
        }
        Some("play") | Some("reset") => format!("queued {} tracks\n", data["queued"]),
        _ => format!("{data}\n"),
    }
}

fn fmt_duration(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}
