//! Claude usage via the claude.ai web API, authenticated with the profile's
//! `sessionKey` cookie:
//!   GET /api/organizations            -> organization uuid
//!   GET /api/organizations/{id}/usage -> { five_hour, seven_day, ... }

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use wreq::Client;

use crate::http::get_json;
use crate::model::{Usage, UsageRow, Window};

fn cookie_header(cookies: &HashMap<String, String>) -> Result<String> {
    let session = cookies
        .get("sessionKey")
        .context("not signed in to claude.ai in this profile")?;
    let mut header = format!("sessionKey={session}");
    for name in ["cf_clearance", "__cf_bm", "_cfuvid"] {
        if let Some(v) = cookies.get(name) {
            header.push_str(&format!("; {name}={v}"));
        }
    }
    Ok(header)
}

/// Whether this profile carries a claude.ai session cookie. A cheap presence
/// check (no decryption beyond what's already loaded, no network) used to decide
/// whether to spawn a Claude fetch — keeps the cookie-name knowledge here rather
/// than in the caller.
pub fn has_session(cookies: &HashMap<String, String>) -> bool {
    cookies.contains_key("sessionKey")
}

pub async fn fetch(client: &Client, cookies: &HashMap<String, String>) -> Result<Vec<UsageRow>> {
    let cookie = cookie_header(cookies)?;

    let orgs = get_json(
        client,
        "https://claude.ai/api/organizations",
        &cookie,
        None,
        None,
    )
    .await
    .context("listing organizations")?;
    let org_id = pick_org(&orgs)
        .or_else(|| cookies.get("lastActiveOrg").cloned())
        .context("no organization found for this account")?;

    let usage = get_json(
        client,
        &format!("https://claude.ai/api/organizations/{org_id}/usage"),
        &cookie,
        None,
        None,
    )
    .await
    .context("fetching usage")?;

    // The usage endpoint doesn't carry the account email; /api/account does.
    let email = get_json(client, "https://claude.ai/api/account", &cookie, None, None)
        .await
        .ok()
        .and_then(|v| account_email(&v));

    Ok(UsageRow::single(Usage {
        email,
        plan: None,
        five_hour: parse_window(usage.get("five_hour")),
        weekly: parse_window(usage.get("seven_day")),
    }))
}

/// Extract the signed-in account's email from claude.ai's /api/account response.
fn account_email(v: &serde_json::Value) -> Option<String> {
    for key in ["email_address", "email"] {
        if let Some(e) = v.get(key).and_then(|x| x.as_str()) {
            return Some(e.to_string());
        }
    }
    if let Some(acc) = v.get("account") {
        for key in ["email_address", "email"] {
            if let Some(e) = acc.get(key).and_then(|x| x.as_str()) {
                return Some(e.to_string());
            }
        }
    }
    None
}

/// Prefer a chat-capable organization, else the first one.
fn pick_org(orgs: &serde_json::Value) -> Option<String> {
    let arr = orgs.as_array()?;
    let chat = arr.iter().find(|o| {
        o.get("capabilities")
            .and_then(|c| c.as_array())
            .map(|caps| caps.iter().any(|v| v.as_str() == Some("chat")))
            .unwrap_or(false)
    });
    chat.or_else(|| arr.first())
        .and_then(|o| o.get("uuid"))
        .and_then(|u| u.as_str())
        .map(str::to_string)
}

fn parse_window(v: Option<&serde_json::Value>) -> Option<Window> {
    let v = v?;
    if v.is_null() {
        return None;
    }
    let used = v.get("utilization").and_then(serde_json::Value::as_f64)?;
    let resets_at = v
        .get("resets_at")
        .and_then(|r| r.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));
    Some(Window {
        used_percent: used,
        resets_at,
    })
}
