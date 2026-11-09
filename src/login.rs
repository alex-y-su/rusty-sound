//! Opens a visible browser, lets the user sign in, and captures the
//! `client_id` and `Authorization` header from the web app's API requests.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::{EnableParams, EventRequestWillBeSent};
use futures::StreamExt;

use crate::session::{self, Session};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);

pub async fn run() -> Result<Session> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    // Separate profile so the user's normal browser profile is untouched.
    let profile = std::path::PathBuf::from(home).join(".config/rusty-sound/browser-profile");

    let config = BrowserConfig::builder()
        .with_head()
        .window_size(480, 640)
        .user_data_dir(profile)
        .build()
        .map_err(|e| anyhow!("browser config: {e}"))?;

    let (mut browser, mut handler) = Browser::launch(config)
        .await
        .context("could not launch Chrome/Chromium (install Google Chrome or Chromium)")?;
    let pump = tokio::spawn(async move { while handler.next().await.is_some() {} });

    let page = browser.new_page("about:blank").await?;
    // Network events must be enabled before navigating, or we miss the requests.
    page.execute(EnableParams::default()).await?;
    let mut requests = page.event_listener::<EventRequestWillBeSent>().await?;

    eprintln!("Sign in to SoundCloud in the browser window (waiting up to 10 min)...");
    page.goto("https://soundcloud.com/you/likes").await?;

    let found = tokio::time::timeout(LOGIN_TIMEOUT, async {
        while let Some(ev) = requests.next().await {
            if let Some(session) = extract(&ev) {
                return Some(session);
            }
        }
        None
    })
    .await;

    let _ = browser.close().await;
    let _ = browser.wait().await;
    pump.abort();

    match found {
        Ok(Some(session)) => {
            session::save(&session)?;
            eprintln!("Session captured and saved.");
            Ok(session)
        }
        Ok(None) => bail!("browser closed before a session was captured"),
        Err(_) => bail!("timed out waiting for sign-in"),
    }
}

/// Pulls the client_id from the request URL and the Authorization header
/// from the request headers, if this request is an authenticated API call.
fn extract(ev: &EventRequestWillBeSent) -> Option<Session> {
    let url = reqwest::Url::parse(&ev.request.url).ok()?;
    if url.host_str() != Some("api-v2.soundcloud.com") {
        return None;
    }
    let client_id = url
        .query_pairs()
        .find(|(k, _)| k == "client_id")
        .map(|(_, v)| v.into_owned())?;

    let headers = serde_json::to_value(&ev.request.headers).ok()?;
    let authorization = headers
        .as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .and_then(|(_, v)| v.as_str())
        .filter(|v| v.starts_with("OAuth "))?
        .to_string();

    Some(Session {
        client_id,
        authorization,
    })
}
