//! 共有 HTTP client。
//!
//! `browser` は Chrome TLS/HTTP2 emulation 付きの `wreq` を使い、replay した
//! `cf_clearance` Cookie を Cloudflare に受け入れさせる。plain client は fingerprint で
//! 403 "Just a moment" challenge になるため、claude.ai と chatgpt.com では使えない。
//! `api` は Cloudflare 配下ではない Google `cloudcode-pa` endpoint 用の plain client。

use anyhow::{Context, Result, anyhow};
use wreq::{Client, Response, StatusCode};
use wreq_util::Emulation;

/// installed Chrome と合わせた User-Agent。その Chrome で発行された `cf_clearance` Cookie を
/// Cloudflare に有効と判定させる。
pub const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36";

/// Cloudflare-fronted site 用の browser-emulating client と Google API 用 plain client。
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

/// `url` を GET して JSON body を parse する。Cloudflare challenge や HTTP error は
/// 分かりやすい message に変換する。browser(Chrome-emulating) client 用。
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

/// response body を lenient に JSON へ parse し `(status, parsed-or-Null)` を返す。
/// post_json / post_form 共通の後段処理。
async fn status_and_json(resp: Response) -> (StatusCode, serde_json::Value) {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let json = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// JSON body を POST し、`(status, parsed-or-Null)` を返す。401 → refresh+retry、
/// 403 → この token では endpoint 不許可、などの判定は caller が行う。
/// この project では `wreq` の `.json()` が必要とする `json` feature を有効化していないため、
/// body は手動 serialize する。`get_json` の手動 parse 方針にも揃えている。
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
    Ok(status_and_json(resp).await)
}

/// `application/x-www-form-urlencoded` body を POST する(Google OAuth token endpoint)。
/// 戻り値は `(status, parsed-or-Null)`。
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
    Ok(status_and_json(resp).await)
}
