//! Shared HTTP clients.
//!
//! `browser` uses `wreq` with Chrome TLS/HTTP2 emulation so the replayed
//! `cf_clearance` cookie is accepted by Cloudflare (a plain client is
//! fingerprinted and gets a 403 "Just a moment" challenge) — used for claude.ai
//! and chatgpt.com. `api` is a plain client for Google's `cloudcode-pa`
//! endpoints, which are not Cloudflare-fronted and need no fingerprint.

use anyhow::{Context, Result, anyhow};
use wreq::{Client, StatusCode};
use wreq_util::Emulation;

/// User-Agent matching the installed Chrome, so the `cf_clearance` cookie
/// (minted by that Chrome) is honored by Cloudflare.
pub const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36";

/// Browser-emulating client (Cloudflare-fronted sites) plus a plain client for
/// Google APIs.
#[derive(Clone)]
pub struct Clients {
    pub browser: Client,
    pub api: Client,
}

pub fn clients() -> Result<Clients> {
    let browser = Client::builder()
        .emulation(Emulation::Chrome137)
        .user_agent(UA)
        .build()
        .context("building browser HTTP client")?;
    let api = Client::builder()
        .build()
        .context("building API HTTP client")?;
    Ok(Clients { browser, api })
}

/// GET `url` and parse the JSON body, turning Cloudflare challenges and HTTP
/// errors into clear messages. Uses the browser (Chrome-emulating) client.
pub async fn get_json(
    client: &Client,
    url: &str,
    cookie: &str,
    bearer: Option<&str>,
    account_id: Option<&str>,
) -> Result<serde_json::Value> {
    let mut req = client.get(url);
    if !cookie.is_empty() {
        req = req.header("Cookie", cookie);
    }
    if let Some(b) = bearer {
        req = req.header("Authorization", format!("Bearer {b}"));
    }
    if let Some(a) = account_id {
        req = req.header("ChatGPT-Account-Id", a);
    }

    let resp = req.send().await.with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    if !status.is_success() {
        if body.contains("Just a moment") || body.to_ascii_lowercase().contains("cloudflare") {
            return Err(anyhow!(
                "Cloudflare challenge (HTTP {}). Open the site in this Chrome profile to refresh its session, then retry.",
                status.as_u16()
            ));
        }
        let snippet: String = body.chars().take(160).collect();
        return Err(anyhow!("HTTP {} from {url}: {snippet}", status.as_u16()));
    }

    serde_json::from_str(&body).with_context(|| format!("parsing JSON from {url}"))
}

/// POST a JSON body and return `(status, parsed-or-Null)`. The caller inspects
/// the status itself (e.g. 401 → refresh+retry, 403 → endpoint not permitted for
/// this token). The body is serialized manually because `wreq`'s `.json()` lives
/// behind a `json` feature this project does not enable; this also matches the
/// manual-parse style of `get_json`.
pub async fn post_json(
    api: &Client,
    url: &str,
    bearer: &str,
    body: &serde_json::Value,
) -> Result<(StatusCode, serde_json::Value)> {
    let payload = serde_json::to_vec(body).context("serializing request body")?;
    let resp = api
        .post(url)
        .bearer_auth(bearer)
        .header("Content-Type", "application/json")
        .body(payload)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let json = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    Ok((status, json))
}

/// POST an `application/x-www-form-urlencoded` body (the Google OAuth token
/// endpoint). Returns `(status, parsed-or-Null)`.
pub async fn post_form(
    api: &Client,
    url: &str,
    form: &[(&str, &str)],
) -> Result<(StatusCode, serde_json::Value)> {
    let resp = api
        .post(url)
        .form(form)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let json = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    Ok((status, json))
}
