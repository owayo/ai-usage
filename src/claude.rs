//! profile の `sessionKey` Cookie で認証し、claude.ai web API から Claude 使用量を取得する:
//!   GET /api/organizations            -> 組織 UUID
//!   GET /api/organizations/{id}/usage -> { five_hour, seven_day, ... }

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use wreq::Client;

use crate::http::get_json;
use crate::model::{ManualReset, ResetKind, Usage, UsageRow, Window, WindowKind};

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

/// この profile が claude.ai session Cookie を持つかどうか。既に読み込み済みの Cookie に対する
/// 安価な存在確認で、network は使わない。Cookie 名の知識を caller ではなくここに閉じ込める。
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
        // 設定の使用量ページと同じ opt-in で、手動リセットの付与情報も取得する。
        &format!("https://claude.ai/api/organizations/{org_id}/usage?cedar_ember=1"),
        &cookie,
        None,
        None,
    )
    .await
    .context("fetching usage")?;

    // usage endpoint は account email を持たないため、/api/account から補う。
    let email = get_json(client, "https://claude.ai/api/account", &cookie, None, None)
        .await
        .ok()
        .and_then(|v| account_email(&v));

    Ok(UsageRow::single(Usage {
        email,
        plan: None,
        short: parse_window(usage.get("five_hour"), WindowKind::FiveHour),
        long: parse_window(usage.get("seven_day"), WindowKind::Weekly),
        manual_resets: Some(
            parse_manual_resets(usage.get("cedar_ember"), Utc::now()).unwrap_or_else(|| {
                vec![
                    ManualReset::unknown(ResetKind::Full),
                    ManualReset::unknown(ResetKind::FiveHour),
                ]
            }),
        ),
        limit_observation: None,
    }))
}

fn parse_manual_resets(
    v: Option<&serde_json::Value>,
    now: DateTime<Utc>,
) -> Option<Vec<ManualReset>> {
    let v = v?;
    let eligible = v.get("eligible")?.as_bool()?;
    let mut resets = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if eligible {
        for grant in v.get("grants")?.as_array()? {
            let paused = grant.get("paused").and_then(serde_json::Value::as_bool) == Some(true);
            let expires_at = grant
                .get("ends_at")
                .and_then(serde_json::Value::as_str)
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc));
            if expires_at.is_some_and(|end| end <= now) {
                continue;
            }
            let id = grant.get("id")?.as_str()?;
            if !seen.insert(id) {
                continue;
            }
            let remaining = grant.get("resets_left")?.as_u64()?;
            if remaining == 0 {
                continue;
            }
            let clears = grant.get("clears")?.as_array()?;
            let short = clears.iter().any(|v| v.as_str() == Some("five_hour"));
            let long = clears.iter().any(|v| v.as_str() == Some("seven_day"));
            let kind = match (short, long) {
                (true, true) => ResetKind::Full,
                (true, false) => ResetKind::FiveHour,
                (false, true) => ResetKind::Weekly,
                _ => ResetKind::Other,
            };
            resets.push(ManualReset {
                kind,
                remaining: Some(remaining),
                expires_at,
                paused,
            });
        }
    }
    // 設定ページは未付与の完全 / 5 時間リセットも 0 回として区別する。
    for kind in [ResetKind::Full, ResetKind::FiveHour] {
        if !resets.iter().any(|r| r.kind == kind) {
            resets.push(ManualReset {
                kind,
                remaining: Some(0),
                expires_at: None,
                paused: false,
            });
        }
    }
    Some(resets)
}

/// claude.ai の /api/account response から signed-in account email を取り出す。
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

/// chat capability を持つ organization を優先し、なければ先頭を使う。
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

fn parse_window(v: Option<&serde_json::Value>, kind: WindowKind) -> Option<Window> {
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
        kind,
        used_percent: Some(used),
        resets_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn manual_resets_preserve_scope_count_and_each_expiry() {
        let now = DateTime::parse_from_rfc3339("2026-06-15T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let v = json!({"eligible":true,"grants":[
            {"id":"full-a","resets_left":1,"clears":["five_hour","seven_day"],"ends_at":"2026-06-20T00:00:00Z"},
            {"id":"full-b","resets_left":2,"clears":["five_hour","seven_day"],"ends_at":"2026-06-25T00:00:00Z"},
            {"id":"session","resets_left":3,"clears":["five_hour"],"ends_at":"2026-06-22T00:00:00Z"},
            {"id":"weekly","resets_left":1,"clears":["seven_day"],"ends_at":null}
        ]});
        let resets = parse_manual_resets(Some(&v), now).unwrap();
        assert_eq!(resets.len(), 4);
        assert_eq!(resets[0].kind, ResetKind::Full);
        assert_eq!(resets[1].remaining, Some(2));
        assert_ne!(resets[0].expires_at, resets[1].expires_at);
        assert_eq!(resets[2].kind, ResetKind::FiveHour);
        assert_eq!(resets[2].remaining, Some(3));
        assert_eq!(resets[3].kind, ResetKind::Weekly);
        assert!(resets[3].expires_at.is_none());
    }

    #[test]
    fn manual_resets_keep_paused_but_exclude_expired_spent_and_duplicate_grants() {
        let now = DateTime::parse_from_rfc3339("2026-06-15T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let active = json!({"id":"active","resets_left":1,"clears":["five_hour","seven_day"],"ends_at":"2026-06-20T00:00:00Z"});
        let v = json!({"eligible":true,"grants":[active, active,
            {"id":"paused","resets_left":5,"paused":true,"clears":["five_hour","seven_day"],"ends_at":"2026-06-21T00:00:00Z"},
            {"id":"expired","resets_left":5,"ends_at":"2026-06-15T00:00:00Z"},
            {"id":"spent","resets_left":0}
        ]});
        let resets = parse_manual_resets(Some(&v), now).unwrap();
        assert_eq!(resets.len(), 3);
        assert_eq!(resets[0].remaining, Some(1));
        assert_eq!(resets[1].remaining, Some(5));
        assert!(resets[1].paused);
        assert!(resets[1].expires_at.is_some());
        assert_eq!(resets[2].kind, ResetKind::FiveHour);
        assert_eq!(resets[2].remaining, Some(0));
    }

    #[test]
    fn manual_resets_cover_empty_and_unknown_scopes() {
        let now = DateTime::parse_from_rfc3339("2026-06-15T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let summary = |resets: Vec<ManualReset>| {
            resets
                .iter()
                .map(|reset| (reset.kind, reset.remaining))
                .collect::<Vec<_>>()
        };
        // 付与が 1 件も無いアカウントは、完全 / 5 時間とも取得済みの 0 回として表示する。
        let empty = parse_manual_resets(Some(&json!({"eligible":true,"grants":[]})), now).unwrap();
        assert_eq!(
            summary(empty),
            vec![(ResetKind::Full, Some(0)), (ResetKind::FiveHour, Some(0))]
        );
        // 回復対象の枠を判別できない付与は other として数え、0 回の表示は残す。
        let other = json!({"eligible":true,"grants":[
            {"id":"opus","resets_left":2,"clears":["opus_weekly"]}
        ]});
        assert_eq!(
            summary(parse_manual_resets(Some(&other), now).unwrap()),
            vec![
                (ResetKind::Other, Some(2)),
                (ResetKind::Full, Some(0)),
                (ResetKind::FiveHour, Some(0)),
            ]
        );
        // 有効な付与なのに clears が無い応答は範囲が分からないため、全体を不明にする。
        let unscoped = json!({"eligible":true,"grants":[{"id":"x","resets_left":1}]});
        assert!(parse_manual_resets(Some(&unscoped), now).is_none());
    }

    #[test]
    fn missing_or_malformed_reset_data_is_unknown_instead_of_zero() {
        let now = Utc::now();
        for v in [
            json!(null),
            json!({}),
            json!({"eligible":true}),
            json!({"eligible":true,"grants":[{"id":"bad","resets_left":-1}]}),
        ] {
            assert!(parse_manual_resets(Some(&v), now).is_none());
        }
        assert!(parse_manual_resets(None, now).is_none());
        let none = parse_manual_resets(Some(&json!({"eligible":false})), now).unwrap();
        assert!(none.iter().all(|r| r.remaining == Some(0)));
    }

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
        let w = parse_window(Some(&v), WindowKind::Weekly).unwrap();
        assert_eq!(w.kind, WindowKind::Weekly);
        assert_eq!(w.used_percent, Some(42.5));
        assert!(w.resets_at.is_some());
    }

    #[test]
    fn parse_window_handles_null_and_missing() {
        // None / Null / utilization 欠落 は None を返す。
        assert!(parse_window(None, WindowKind::Weekly).is_none());
        assert!(parse_window(Some(&json!(null)), WindowKind::Weekly).is_none());
        assert!(parse_window(Some(&json!({})), WindowKind::Weekly).is_none());
    }

    #[test]
    fn parse_window_tolerates_invalid_reset_time() {
        // 不正な resets_at は None で握りつぶす(used_percent は維持)。
        let v = json!({"utilization": 10.0, "resets_at": "not a date"});
        let w = parse_window(Some(&v), WindowKind::Weekly).unwrap();
        assert_eq!(w.used_percent, Some(10.0));
        assert!(w.resets_at.is_none());
    }
}
