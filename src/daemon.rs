//! The service process: holds the session, answers JSON requests on a Unix
//! socket, and exits after an idle period (not while audio plays) or on SIGTERM.

use std::fs;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};

use crate::api::Api;
use crate::client::{self, CliError};
use crate::playback::{self, Msg, PlayerHandle, Track};
use crate::session;

const DEFAULT_IDLE_SECS: u64 = 600;
const DEFAULT_PLAY_MAX: usize = 100;

struct State {
    api: Api,
    creds: (String, String),
    player: Option<PlayerHandle>,
}

impl State {
    fn is_active(&self) -> bool {
        self.player.as_ref().is_some_and(|p| p.is_active())
    }
}

pub async fn serve() -> Result<()> {
    let session = session::load()?.ok_or_else(|| {
        CliError::new("no_session", "no saved session; run `rusty-sound login`")
    })?;
    let creds = (session.client_id.clone(), session.authorization.clone());
    let mut state = State {
        api: Api::new(session)?,
        creds,
        player: None,
    };

    let sock = client::socket_path()?;
    if let Some(dir) = sock.parent() {
        fs::create_dir_all(dir)?;
    }
    // A socket left behind by a killed process would block bind.
    let _ = fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).context("could not bind socket")?;

    let idle = Duration::from_secs(
        std::env::var("RUSTY_SOUND_IDLE_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_IDLE_SECS),
    );
    let mut term = signal(SignalKind::terminate())?;
    eprintln!("serving on {} (idle exit after {}s)", sock.display(), idle.as_secs());

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                if let Err(e) = handle(stream, &mut state).await {
                    eprintln!("connection error: {e:#}");
                }
            }
            _ = tokio::time::sleep(idle) => {
                // Keep serving while audio plays; otherwise exit.
                if !state.is_active() {
                    eprintln!("idle timeout, exiting");
                    break;
                }
            }
            _ = term.recv() => {
                eprintln!("terminated");
                break;
            }
        }
    }

    let _ = fs::remove_file(&sock);
    Ok(())
}

async fn handle(stream: UnixStream, state: &mut State) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let Some(line) = lines.next_line().await? else {
        return Ok(());
    };

    let resp = match serde_json::from_str::<Value>(&line) {
        Ok(req) => match dispatch(state, &req).await {
            Ok(data) => json!({"ok": true, "data": data}),
            Err(e) => client::error_value(&e),
        },
        Err(e) => json!({"ok": false, "error": {"code": "bad_request", "message": e.to_string()}}),
    };
    writer.write_all(format!("{resp}\n").as_bytes()).await?;
    Ok(())
}

async fn dispatch(state: &mut State, req: &Value) -> Result<Value> {
    match req["cmd"].as_str() {
        Some("ping") => Ok(json!({"pong": true})),
        Some("likes") => likes(&state.api, req["max"].as_u64()).await,
        Some("discover") => discover(&state.api).await,
        Some("play") => play(state, req["max"].as_u64(), false).await,
        Some("reset") => play(state, None, true).await,
        Some("pause") => control(state, Msg::Pause),
        Some("resume") => control(state, Msg::Resume),
        Some("next") => control(state, Msg::Next),
        Some("prev") => control(state, Msg::Prev),
        Some("radio") => control(state, Msg::Radio(req["on"].as_bool().unwrap_or(true))),
        Some("clear") => control(state, Msg::Clear),
        Some("now") => Ok(match &state.player {
            Some(p) => serde_json::to_value(p.snapshot())?,
            None => serde_json::to_value(playback::Status::default())?,
        }),
        Some(other) => Err(CliError::new("unknown_command", format!("unknown command: {other}")).into()),
        None => Err(anyhow!(CliError::new("bad_request", "missing \"cmd\""))),
    }
}

/// Sends a control message to the playback thread, if one is running.
fn control(state: &mut State, msg: Msg) -> Result<Value> {
    match &state.player {
        Some(p) if p.tx.send(msg).is_ok() => Ok(json!({"ok": true})),
        _ => Err(CliError::new("not_playing", "nothing is playing; run `rusty-sound play`").into()),
    }
}

/// Queues liked tracks and starts playback if needed.
/// Queues liked tracks. `replace` drops the current queue and starts from the top.
async fn play(state: &mut State, max: Option<u64>, replace: bool) -> Result<Value> {
    let max = max.map(|m| m as usize).unwrap_or(DEFAULT_PLAY_MAX);
    let (_, items) = fetch_likes(&state.api, Some(max)).await?;
    let tracks: Vec<Track> = items
        .iter()
        .filter_map(|item| playback::track_from_json(item.get("track")?))
        .collect();
    if tracks.is_empty() {
        return Err(CliError::new("nothing_to_play", "no playable liked tracks").into());
    }
    let n = tracks.len();

    // Reuse the playback thread if it's still alive; otherwise start one.
    let mut msg = if replace { Msg::Replace(tracks) } else { Msg::Queue(tracks) };
    if let Some(p) = &state.player {
        match p.tx.send(msg) {
            Ok(()) => return Ok(json!({"queued": n})),
            Err(e) => msg = e.0,
        }
    }
    let p = playback::spawn(state.creds.clone());
    p.tx
        .send(msg)
        .map_err(|_| CliError::new("playback_failed", "could not start playback"))?;
    state.player = Some(p);
    Ok(json!({"queued": n}))
}

/// Raw like items (tracks and playlists), newest first, paged until `max`.
async fn fetch_likes(api: &Api, max: Option<usize>) -> Result<(String, Vec<Value>)> {
    let me = api.me().await?;
    let user_id = me["id"].as_u64().context("/me response has no id")?;
    let username = me["username"].as_str().unwrap_or("").to_string();

    let mut out: Vec<Value> = Vec::new();
    let mut page = api
        .get(
            &format!("/users/{user_id}/likes"),
            &[("limit", "200"), ("offset", "0")],
        )
        .await?;

    loop {
        for item in page["collection"].as_array().into_iter().flatten() {
            if max.is_some_and(|m| out.len() >= m) {
                break;
            }
            if item.get("track").is_some_and(|t| t.is_object())
                || item.get("playlist").is_some_and(|p| p.is_object())
            {
                out.push(item.clone());
            }
        }
        let full = max.is_some_and(|m| out.len() >= m);
        match page["next_href"].as_str() {
            Some(href) if !href.is_empty() && !full => page = api.get_next(href).await?,
            _ => break,
        }
    }
    Ok((username, out))
}

async fn likes(api: &Api, max: Option<u64>) -> Result<Value> {
    let (username, items) = fetch_likes(api, max.map(|m| m as usize)).await?;
    let entries: Vec<Value> = items.iter().filter_map(like_entry).collect();
    Ok(json!({"user": username, "count": entries.len(), "likes": entries}))
}

fn like_entry(item: &Value) -> Option<Value> {
    if let Some(t) = item.get("track").filter(|t| t.is_object()) {
        return Some(json!({
            "kind": "track",
            "title": t["title"],
            "artist": t["user"]["username"],
            "duration_ms": t["duration"],
            "url": t["permalink_url"],
        }));
    }
    if let Some(p) = item.get("playlist").filter(|p| p.is_object()) {
        return Some(json!({
            "kind": "playlist",
            "title": p["title"],
            "artist": p["user"]["username"],
            "track_count": p["track_count"],
            "url": p["permalink_url"],
        }));
    }
    None
}

/// Discover sections, each with its first few playlists.
async fn discover(api: &Api) -> Result<Value> {
    let page = api.get("/mixed-selections", &[("limit", "20")]).await?;
    let mut sections = Vec::new();
    for section in page["collection"].as_array().into_iter().flatten() {
        let playlists: Vec<Value> = section["items"]["collection"]
            .as_array()
            .into_iter()
            .flatten()
            .take(6)
            .map(|pl| json!({"title": pl["title"], "track_count": pl["track_count"]}))
            .collect();
        sections.push(json!({"title": section["title"], "playlists": playlists}));
    }
    Ok(json!({"sections": sections}))
}
