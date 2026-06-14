//! Codex/ChatGPT usage. The profile's `__Secure-next-auth.session-token` cookie
//! is exchanged for a Bearer token, which then calls the Codex usage endpoint:
//!   GET /api/auth/session         -> { accessToken, user }
//!   GET /backend-api/wham/usage   -> { rate_limit: { primary_window, ... } }

use std::collections::HashMap;

use anyhow::{Context, Result};
use base64::Engine;
use chrono::{TimeZone, Utc};
use wreq::Client;

use crate::http::get_json;
use crate::model::{Usage, UsageRow, Window};

/// chatgpt.com's session-token cookie. Large tokens are split across `…token.0`
/// and `…token.1`; a small one stays in the unsuffixed `…session-token`. Defined
/// once so `cookie_header` (sends it) and `has_session` (detects it) can't drift.
const SESSION_TOKEN: &str = "__Secure-next-auth.session-token";

fn cookie_header(cookies: &HashMap<String, String>) -> Result<String> {
    let mut header = if let Some(t0) = cookies.get(&format!("{SESSION_TOKEN}.0")) {
        format!("{SESSION_TOKEN}.0={t0}")
    } else if let Some(t) = cookies.get(SESSION_TOKEN) {
        format!("{SESSION_TOKEN}={t}")
    } else {
        anyhow::bail!("not signed in to chatgpt.com in this profile");
    };
    if let Some(t1) = cookies.get(&format!("{SESSION_TOKEN}.1")) {
        header.push_str(&format!("; {SESSION_TOKEN}.1={t1}"));
    }
    for name in ["cf_clearance", "__cf_bm", "_puid"] {
        if let Some(v) = cookies.get(name) {
            header.push_str(&format!("; {name}={v}"));
        }
    }
    Ok(header)
}

/// Whether this profile carries a chatgpt.com session-token cookie (either the
/// split `…token.0` form or the unsuffixed one). Mirrors `cookie_header`'s
/// requirement so the caller never has to know the cookie names.
pub fn has_session(cookies: &HashMap<String, String>) -> bool {
    cookies.contains_key(&format!("{SESSION_TOKEN}.0")) || cookies.contains_key(SESSION_TOKEN)
}

pub async fn fetch(client: &Client, cookies: &HashMap<String, String>) -> Result<Vec<UsageRow>> {
    let cookie = cookie_header(cookies)?;

    let session = get_json(
        client,
        "https://chatgpt.com/api/auth/session",
        &cookie,
        None,
        None,
    )
    .await
    .context("reading chatgpt session")?;
    let access = session
        .get("accessToken")
        .and_then(|a| a.as_str())
        .context("chatgpt session has no accessToken (signed out?)")?;
    let email = session
        .pointer("/user/email")
        .and_then(|e| e.as_str())
        .map(str::to_string);
    let account_id = jwt_account_id(access);

    let usage = get_json(
        client,
        "https://chatgpt.com/backend-api/wham/usage",
        &cookie,
        Some(access),
        account_id.as_deref(),
    )
    .await
    .context("fetching wham/usage")?;

    let plan = usage
        .get("plan_type")
        .and_then(|p| p.as_str())
        .map(str::to_string);
    let rate = usage.get("rate_limit");

    let mut five = None;
    let mut weekly = None;
    // Classify by window duration, not by primary/secondary position.
    for key in ["primary_window", "secondary_window"] {
        let Some(w) = rate.and_then(|r| r.get(key)) else {
            continue;
        };
        if w.is_null() {
            continue;
        }
        let secs = w
            .get("limit_window_seconds")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        let window = parse_window(w);
        if secs <= 8 * 3600 {
            five = window;
        } else {
            weekly = window;
        }
    }

    Ok(UsageRow::single(Usage {
        email,
        plan,
        five_hour: five,
        weekly,
    }))
}

fn parse_window(w: &serde_json::Value) -> Option<Window> {
    let used = w.get("used_percent").and_then(serde_json::Value::as_f64)?;
    let resets_at = w
        .get("reset_at")
        .and_then(serde_json::Value::as_i64)
        .and_then(|e| Utc.timestamp_opt(e, 0).single());
    Some(Window {
        used_percent: used,
        resets_at,
    })
}

/// Extract `chatgpt_account_id` from the access token's JWT claims.
fn jwt_account_id(jwt: &str) -> Option<String> {
    let payload = jwt.split('.').nth(1)?;
    let mut b64 = payload.replace('-', "+").replace('_', "/");
    while b64.len() % 4 != 0 {
        b64.push('=');
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .get("https://api.openai.com/auth")
        .and_then(|a| a.get("chatgpt_account_id"))
        .and_then(|s| s.as_str())
        .map(str::to_string)
}
