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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cookie_header_includes_required_session_key() {
        let mut c = HashMap::new();
        c.insert("sessionKey".to_string(), "abc".to_string());
        let h = cookie_header(&c).unwrap();
        assert!(h.starts_with("sessionKey=abc"));
    }

    #[test]
    fn cookie_header_appends_cloudflare_cookies() {
        // cf_clearance / __cf_bm / _cfuvid は付加される(順序は登場順)。
        let mut c = HashMap::new();
        c.insert("sessionKey".to_string(), "abc".to_string());
        c.insert("cf_clearance".to_string(), "x".to_string());
        c.insert("__cf_bm".to_string(), "y".to_string());
        c.insert("_cfuvid".to_string(), "z".to_string());
        let h = cookie_header(&c).unwrap();
        assert!(h.contains("; cf_clearance=x"));
        assert!(h.contains("; __cf_bm=y"));
        assert!(h.contains("; _cfuvid=z"));
    }

    #[test]
    fn cookie_header_errors_without_session() {
        // sessionKey が無いと未ログインと判定されてエラーになる。
        let c = HashMap::<String, String>::new();
        assert!(cookie_header(&c).is_err());
    }

    #[test]
    fn has_session_checks_key() {
        let mut c = HashMap::new();
        assert!(!has_session(&c));
        c.insert("sessionKey".to_string(), "x".to_string());
        assert!(has_session(&c));
    }

    #[test]
    fn pick_org_prefers_chat_capability() {
        // capabilities に "chat" を含む組織を最優先で選ぶ。
        let v = json!([
            {"uuid": "u1", "capabilities": ["admin"]},
            {"uuid": "u2", "capabilities": ["chat", "admin"]},
        ]);
        assert_eq!(pick_org(&v).as_deref(), Some("u2"));
    }

    #[test]
    fn pick_org_falls_back_to_first() {
        // 該当なしなら先頭を返す。
        let v = json!([
            {"uuid": "u1", "capabilities": ["admin"]},
            {"uuid": "u2", "capabilities": []},
        ]);
        assert_eq!(pick_org(&v).as_deref(), Some("u1"));
    }

    #[test]
    fn pick_org_returns_none_for_empty_or_invalid() {
        assert_eq!(pick_org(&json!([])), None);
        assert_eq!(pick_org(&json!({})), None);
    }

    #[test]
    fn account_email_reads_known_shapes() {
        // /api/account の応答は形が複数あるためそれぞれカバー。
        assert_eq!(
            account_email(&json!({"email_address": "a@x.test"})).as_deref(),
            Some("a@x.test")
        );
        assert_eq!(
            account_email(&json!({"email": "b@x.test"})).as_deref(),
            Some("b@x.test")
        );
        assert_eq!(
            account_email(&json!({"account": {"email_address": "c@x.test"}})).as_deref(),
            Some("c@x.test")
        );
        assert_eq!(account_email(&json!({})), None);
    }

    #[test]
    fn parse_window_reads_utilization_and_reset() {
        let v = json!({"utilization": 42.5, "resets_at": "2026-06-15T06:28:32Z"});
        let w = parse_window(Some(&v)).unwrap();
        assert_eq!(w.used_percent, 42.5);
        assert!(w.resets_at.is_some());
    }

    #[test]
    fn parse_window_handles_null_and_missing() {
        // None / Null / utilization 欠落 は None を返す。
        assert!(parse_window(None).is_none());
        assert!(parse_window(Some(&json!(null))).is_none());
        assert!(parse_window(Some(&json!({}))).is_none());
    }

    #[test]
    fn parse_window_tolerates_invalid_reset_time() {
        // 不正な resets_at は None で握りつぶす(used_percent は維持)。
        let v = json!({"utilization": 10.0, "resets_at": "not a date"});
        let w = parse_window(Some(&v)).unwrap();
        assert_eq!(w.used_percent, 10.0);
        assert!(w.resets_at.is_none());
    }
}
