//! Minimal client for the undocumented api-v2 endpoints, using a captured
//! `client_id` and OAuth `Authorization` header.

use anyhow::{Result, bail};
use reqwest::{Client, Url};
use serde_json::Value;

use crate::session::Session;

const BASE: &str = "https://api-v2.soundcloud.com";

pub struct Api {
    http: Client,
    session: Session,
}

impl Api {
    pub fn new(session: Session) -> Result<Self> {
        let http = Client::builder()
            .user_agent("rusty-sound/0.1")
            .build()?;
        Ok(Self { http, session })
    }

    /// GET a path under the API base with the client_id and extra query params.
    pub async fn get(&self, path: &str, params: &[(&str, &str)]) -> Result<Value> {
        let mut url = Url::parse(&format!("{BASE}{path}"))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("client_id", &self.session.client_id);
            for (k, v) in params {
                q.append_pair(k, v);
            }
        }
        self.get_url(url).await
    }

    /// Follow a `next_href` returned by a paginated response.
    pub async fn get_next(&self, href: &str) -> Result<Value> {
        let mut url = Url::parse(href)?;
        if !url.query_pairs().any(|(k, _)| k == "client_id") {
            url.query_pairs_mut()
                .append_pair("client_id", &self.session.client_id);
        }
        self.get_url(url).await
    }

    async fn get_url(&self, url: Url) -> Result<Value> {
        let resp = self
            .http
            .get(url.clone())
            .header("Authorization", &self.session.authorization)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let snippet: String = body.chars().take(200).collect();
            // Strip the query so client_id is not echoed back to the terminal.
            bail!("{status} from {}: {snippet}", url.path());
        }
        Ok(resp.json().await?)
    }

    pub async fn me(&self) -> Result<Value> {
        self.get("/me", &[]).await
    }
}
