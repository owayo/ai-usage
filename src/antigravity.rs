//! Antigravity(Google の agentic IDE + `agy` CLI)の使用量取得。
//!
//! CodexBar に合わせて、以下の 2 経路を順に試す:
//!   1. **Local language_server** — `agy`(またはアプリ)実行中に localhost HTTPS の
//!      Connect-RPC endpoint を提供する。`RetrieveUserQuotaSummary` は `agy /usage`
//!      に表示される "Gemini Models" / "Claude and GPT models" の週次グループを返し、
//!      `GetUserStatus` はアカウント email と plan を補う。
//!   2. **OAuth remote** — `~/.gemini` の OAuth token(必要なら自動 refresh)で
//!      `cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota` を呼ぶ。実行中の
//!      プロセスなしで使えるが、Gemini の per-model 日次 quota に限られる。
//!
//! Claude/Codex と違って Chrome Cookie ではない。詳細は CLAUDE.md と
//! <https://github.com/steipete/CodexBar/blob/main/docs/antigravity.md>.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Value, json};
use wreq::{Client, StatusCode};

use crate::config::AntigravityCfg;
use crate::http::{post_form, post_json};
use crate::model::{Usage, UsageRow, Window};

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const CODE_ASSIST: &str = "https://cloudcode-pa.googleapis.com/v1internal";

/// 情報量の多い local 経路を優先し、失敗時は OAuth remote にフォールバックする。
/// 戻り値は model group ごとに 1 行。
pub async fn fetch(api: &Client, cfg: Option<&AntigravityCfg>) -> Result<Vec<UsageRow>> {
    if let Ok(rows) = local_fetch().await
        && !rows.is_empty()
    {
        return Ok(rows);
    }
    oauth_fetch(api, cfg).await
}

/// Antigravity を表示可能かどうか。token ファイルがあるか、`agy` が起動中なら true。
/// `enabled = false` が明示されている場合は常に false。
pub fn available(cfg: Option<&AntigravityCfg>) -> bool {
    if matches!(cfg, Some(c) if c.enabled == Some(false)) {
        return false;
    }
    token_path(cfg).map(|p| p.exists()).unwrap_or(false) || !agy_listen_ports().is_empty()
}

// ============================ local language_server ============================

async fn local_fetch() -> Result<Vec<UsageRow>> {
    let ports = agy_listen_ports();
    if ports.is_empty() {
        bail!("agy/Antigravity not running");
    }
    let local = Client::builder()
        .cert_verification(false)
        .verify_hostname(false)
        .build()
        .context("building localhost client")?;
    for port in ports {
        if let Ok(rows) = local_quota(&local, port).await
            && !rows.is_empty()
        {
            return Ok(rows);
        }
    }
    bail!("no quota from local agy server")
}

async fn local_quota(local: &Client, port: u16) -> Result<Vec<UsageRow>> {
    let base = format!("https://127.0.0.1:{port}/exa.language_server_pb.LanguageServerService");
    let summary = local_post(local, &format!("{base}/RetrieveUserQuotaSummary")).await?;
    let (email, plan) = match local_post(local, &format!("{base}/GetUserStatus")).await {
        Ok(s) => user_identity(&s),
        Err(_) => (None, None),
    };
    parse_summary(&summary, email, plan)
}

async fn local_post(local: &Client, url: &str) -> Result<Value> {
    let resp = local
        .post(url)
        .header("Connect-Protocol-Version", "1")
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let text = resp.text().await.unwrap_or_default();
    serde_json::from_str(&text).with_context(|| format!("parsing {url}"))
}

/// `RetrieveUserQuotaSummary` を model group ごとの `UsageRow` に変換する。
fn parse_summary(v: &Value, email: Option<String>, plan: Option<String>) -> Result<Vec<UsageRow>> {
    let groups = v
        .pointer("/response/groups")
        .or_else(|| v.get("groups"))
        .and_then(|g| g.as_array())
        .context("RetrieveUserQuotaSummary has no groups")?;
    let mut rows = Vec::new();
    for g in groups {
        let label = g
            .get("displayName")
            .and_then(|d| d.as_str())
            .map(short_group_label);
        let mut usage = Usage {
            email: email.clone(),
            plan: plan.clone(),
            five_hour: None,
            weekly: None,
        };
        for b in g
            .get("buckets")
            .and_then(|b| b.as_array())
            .into_iter()
            .flatten()
        {
            let Some(window) = bucket_to_window(b) else {
                continue;
            };
            // 同じ window 種別の bucket が複数返る場合は、最も残量の少ない(used_percent が
            // 高い)= 最も制約の厳しいものを採用する。先勝ち / 後勝ちだと API のレスポンス
            // 順序次第で過小表示が起きる。
            let target = if is_weekly(b) {
                &mut usage.weekly
            } else {
                &mut usage.five_hour
            };
            let should_replace = target
                .as_ref()
                .map(|cur| window.used_percent > cur.used_percent)
                .unwrap_or(true);
            if should_replace {
                *target = Some(window);
            }
        }
        rows.push(UsageRow {
            group_label: label,
            usage,
        });
    }
    if rows.is_empty() {
        bail!("no groups in quota summary");
    }
    Ok(rows)
}

fn user_identity(v: &Value) -> (Option<String>, Option<String>) {
    // GetUserStatus は RetrieveUserQuotaSummary と同じ Connect-RPC のため、
    // `response` で包まれた形 ({"response":{"userStatus":{...}}}) と包まない形の
    // 両方があり得る。parse_summary が `/response/groups` を許容するのに揃え、
    // ここでも wrapper 付き → top-level → 全体、の順に userStatus を解決する。
    let us = v
        .pointer("/response/userStatus")
        .or_else(|| v.get("userStatus"))
        .unwrap_or(v);
    let email = us.get("email").and_then(|e| e.as_str()).map(str::to_string);
    let plan = us
        .pointer("/planStatus/planInfo/planName")
        .and_then(|p| p.as_str())
        .map(str::to_string);
    (email, plan)
}

fn is_weekly(b: &Value) -> bool {
    if let Some(w) = b.get("window").and_then(|w| w.as_str()) {
        return w.to_ascii_lowercase().contains("week");
    }
    if let Some(id) = b.get("bucketId").and_then(|i| i.as_str()) {
        if id.contains("week") {
            return true;
        }
        if id.contains("5h") || id.contains("five") || id.contains("hour") {
            return false;
        }
    }
    if let Some(name) = b.get("displayName").and_then(|n| n.as_str())
        && name.to_ascii_lowercase().contains("week")
    {
        return true;
    }
    // 最後は reset までの時間で判定する。8h 超なら週次 window とみなす。
    bucket_reset(b)
        .map(|r| (r - Utc::now()).num_seconds() > 8 * 3600)
        .unwrap_or(true)
}

fn bucket_to_window(b: &Value) -> Option<Window> {
    // language_server は `{remaining:{remainingFraction}}`、OAuth 経路はトップレベルの
    // `remainingFraction` を返すことがあるため両形式を許容する。
    let rf = b
        .pointer("/remaining/remainingFraction")
        .or_else(|| b.get("remainingFraction"))
        .and_then(Value::as_f64)
        .unwrap_or(1.0);
    let used = ((1.0 - rf) * 100.0).clamp(0.0, 100.0);
    Some(Window {
        used_percent: used,
        resets_at: bucket_reset(b),
    })
}

fn bucket_reset(b: &Value) -> Option<DateTime<Utc>> {
    let reset = b.get("resetTime")?;
    if let Some(s) = reset.as_str() {
        return DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| d.with_timezone(&Utc))
            .or_else(|| s.parse::<i64>().ok().and_then(epoch_seconds));
    }
    reset.as_i64().and_then(epoch_seconds)
}

fn epoch_seconds(seconds: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_opt(seconds, 0).single()
}

fn short_group_label(display: &str) -> String {
    let d = display.to_ascii_lowercase();
    if d.contains("gemini") {
        "Gemini".to_string()
    } else if d.contains("claude") || d.contains("gpt") {
        "Claude&GPT".to_string()
    } else {
        display
            .trim_end_matches(" Models")
            .trim_end_matches(" models")
            .to_string()
    }
}

// ============================ agy process discovery ============================

fn agy_listen_ports() -> Vec<u16> {
    let mut ports = Vec::new();
    for pid in agy_pids() {
        ports.extend(listen_ports(pid));
    }
    ports.sort_unstable();
    ports.dedup();
    ports
}

fn agy_pids() -> Vec<u32> {
    let Ok(out) = Command::new("ps")
        .args(["-ax", "-o", "pid=,comm="])
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut pids = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((pid, comm)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let name = comm.trim().rsplit('/').next().unwrap_or("").trim();
        if (name == "agy" || name.starts_with("language_server"))
            && let Ok(p) = pid.trim().parse()
        {
            pids.push(p);
        }
    }
    pids
}

fn listen_ports(pid: u32) -> Vec<u16> {
    let Ok(out) = Command::new("lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-a", "-p", &pid.to_string()])
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut ports = Vec::new();
    for line in text.lines().skip(1) {
        if !(line.contains("127.0.0.1") || line.contains("[::1]")) {
            continue;
        }
        if let Some(idx) = line.rfind(':') {
            let num: String = line[idx + 1..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if let Ok(p) = num.parse() {
                ports.push(p);
            }
        }
    }
    ports
}

// ============================ OAuth remote ============================

async fn oauth_fetch(api: &Client, cfg: Option<&AntigravityCfg>) -> Result<Vec<UsageRow>> {
    let path = token_path(cfg).context("no ~/.gemini Antigravity token found")?;
    let mut tok = load_token(&path)
        .with_context(|| format!("reading Antigravity token {}", path.display()))?;

    if tok.expires_in() < 300 {
        tok = refresh(api, &tok)
            .await
            .context("refreshing Antigravity OAuth token")?;
    }

    let url = format!("{CODE_ASSIST}:retrieveUserQuota");
    let (mut status, mut body) = post_json(api, &url, &tok.access, &json!({})).await?;
    if status == StatusCode::UNAUTHORIZED {
        tok = refresh(api, &tok).await.context("refreshing after 401")?;
        let r = post_json(api, &url, &tok.access, &json!({})).await?;
        status = r.0;
        body = r.1;
    }
    if status == StatusCode::FORBIDDEN {
        bail!("OAuth token lacks quota permission — open `agy` for full data");
    }
    if !status.is_success() {
        bail!("retrieveUserQuota HTTP {}", status.as_u16());
    }
    parse_buckets(&body)
}

/// `retrieveUserQuota` の bucket(Gemini per-model 日次)を代表 1 行に畳み込む。
fn parse_buckets(v: &Value) -> Result<Vec<UsageRow>> {
    let buckets = v
        .get("buckets")
        .and_then(|b| b.as_array())
        .context("retrieveUserQuota has no buckets")?;
    // 代表値は最も制約の厳しい(remainingFraction が最小の)bucket。
    let mut worst: Option<&Value> = None;
    let mut worst_rf = 2.0_f64;
    for b in buckets {
        let rf = b
            .get("remainingFraction")
            .and_then(Value::as_f64)
            .unwrap_or(1.0);
        if rf <= worst_rf {
            worst_rf = rf;
            worst = Some(b);
        }
    }
    let b = worst.context("retrieveUserQuota returned no buckets")?;
    // REQUESTS bucket は 1 日以内に reset されるため短期 window として出す。
    let usage = Usage {
        email: None,
        plan: None,
        five_hour: bucket_to_window(b),
        weekly: None,
    };
    Ok(vec![UsageRow {
        group_label: Some("Gemini".to_string()),
        usage,
    }])
}

// ============================ token + refresh ============================

struct Token {
    access: String,
    refresh: String,
    /// Unix 秒。未知の場合は 0。
    expiry: i64,
}

impl Token {
    fn expires_in(&self) -> i64 {
        if self.expiry == 0 {
            return 0;
        }
        self.expiry - now_secs()
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn token_path(cfg: Option<&AntigravityCfg>) -> Option<PathBuf> {
    if let Some(p) = cfg.and_then(|c| c.token_path.as_ref()) {
        return Some(expand(p));
    }
    let home = dirs::home_dir()?;
    for rel in [
        ".gemini/antigravity-cli/antigravity-oauth-token",
        ".gemini/oauth_creds.json",
    ] {
        let p = home.join(rel);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn expand(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(p)
}

fn load_token(path: &Path) -> Result<Token> {
    let data = std::fs::read_to_string(path)?;
    let v: Value = serde_json::from_str(&data)?;
    // 形は 2 種類: antigravity-cli の {token:{access_token,refresh_token,expiry}} と、
    // oauth_creds.json の flat {access_token,refresh_token,expiry_date}。
    let inner = v.get("token").unwrap_or(&v);
    let access = inner
        .get("access_token")
        .and_then(|a| a.as_str())
        .context("token has no access_token")?
        .to_string();
    let refresh = inner
        .get("refresh_token")
        .and_then(|r| r.as_str())
        .context("token has no refresh_token")?
        .to_string();
    Ok(Token {
        access,
        refresh,
        expiry: parse_expiry(inner),
    })
}

fn parse_expiry(v: &Value) -> i64 {
    // antigravity-cli は ISO-8601 の "expiry"、oauth_creds は epoch-ms の "expiry_date"。
    if let Some(s) = v.get("expiry").and_then(|e| e.as_str())
        && let Ok(d) = DateTime::parse_from_rfc3339(s)
    {
        return d.timestamp();
    }
    if let Some(ms) = v.get("expiry_date").and_then(Value::as_i64) {
        return ms / 1000;
    }
    0
}

async fn refresh(api: &Client, tok: &Token) -> Result<Token> {
    let (id, secret) = oauth_client().context(
        "Antigravity OAuth client unknown — set ANTIGRAVITY_OAUTH_CLIENT_ID/SECRET, or install Antigravity.app",
    )?;
    let form = [
        ("client_id", id.as_str()),
        ("client_secret", secret.as_str()),
        ("refresh_token", tok.refresh.as_str()),
        ("grant_type", "refresh_token"),
    ];
    let (status, body) = post_form(api, TOKEN_URL, &form).await?;
    if !status.is_success() {
        let msg = body
            .get("error_description")
            .or_else(|| body.get("error"))
            .and_then(|e| e.as_str())
            .unwrap_or("unknown");
        bail!("OAuth refresh failed (HTTP {}): {msg}", status.as_u16());
    }
    let access = body
        .get("access_token")
        .and_then(|a| a.as_str())
        .context("refresh response has no access_token")?
        .to_string();
    let expiry = body
        .get("expires_in")
        .and_then(Value::as_i64)
        .map(|s| now_secs() + s)
        .unwrap_or(0);
    Ok(Token {
        access,
        refresh: tok.refresh.clone(),
        expiry,
    })
}

/// Antigravity の OAuth client id/secret。env override を優先し、なければ
/// インストール済み `Antigravity.app` から抽出する。リポジトリには埋め込まない。
fn oauth_client() -> Option<(String, String)> {
    if let (Ok(id), Ok(secret)) = (
        std::env::var("ANTIGRAVITY_OAUTH_CLIENT_ID"),
        std::env::var("ANTIGRAVITY_OAUTH_CLIENT_SECRET"),
    ) && !id.is_empty()
        && !secret.is_empty()
    {
        return Some((id, secret));
    }
    discover_from_app()
}

fn discover_from_app() -> Option<(String, String)> {
    let js =
        std::fs::read_to_string("/Applications/Antigravity.app/Contents/Resources/app/out/main.js")
            .ok()?;
    Some((find_client_id(&js)?, find_secret(&js)?))
}

fn find_client_id(s: &str) -> Option<String> {
    const MARKER: &str = ".apps.googleusercontent.com";
    let end = s.find(MARKER)?;
    let bytes = s.as_bytes();
    let mut start = end;
    while start > 0 && {
        let c = bytes[start - 1];
        c.is_ascii_alphanumeric() || c == b'-'
    } {
        start -= 1;
    }
    Some(s[start..end + MARKER.len()].to_string())
}

fn find_secret(s: &str) -> Option<String> {
    let i = s.find("GOCSPX-")?;
    let secret: String = s[i..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(35)
        .collect();
    (secret.len() >= 20).then_some(secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_summary_into_groups() {
        let v = json!({"response": {"groups": [
            {"displayName": "Gemini Models", "buckets": [
                {"bucketId": "gemini-weekly", "window": "weekly",
                 "remainingFraction": 0.9637, "resetTime": "2026-06-19T05:06:39Z"}]},
            {"displayName": "Claude and GPT models", "buckets": [
                {"bucketId": "3p-weekly", "window": "weekly",
                 "remainingFraction": 1.0, "resetTime": "2026-06-21T06:34:44Z"}]}
        ]}});
        let rows = parse_summary(&v, Some("e@x.test".into()), Some("Pro".into())).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].group_label.as_deref(), Some("Gemini"));
        let w = rows[0].usage.weekly.as_ref().unwrap();
        assert!(
            (w.used_percent - 3.63).abs() < 0.05,
            "got {}",
            w.used_percent
        );
        assert_eq!(rows[0].usage.email.as_deref(), Some("e@x.test"));
        assert_eq!(rows[0].usage.plan.as_deref(), Some("Pro"));
        assert_eq!(rows[1].group_label.as_deref(), Some("Claude&GPT"));
        assert_eq!(rows[1].usage.weekly.as_ref().unwrap().used_percent, 0.0);
    }

    #[test]
    fn oauth_buckets_pick_most_constrained() {
        let v = json!({"buckets": [
            {"modelId": "gemini-2.5-pro", "remainingFraction": 1.0,
             "resetTime": "2026-06-15T06:28:32Z"},
            {"modelId": "gemini-2.5-flash", "remainingFraction": 0.4,
             "resetTime": "2026-06-15T06:28:32Z"}
        ]});
        let rows = parse_buckets(&v).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].group_label.as_deref(), Some("Gemini"));
        let w = rows[0].usage.five_hour.as_ref().unwrap();
        assert!(
            (w.used_percent - 60.0).abs() < 0.01,
            "got {}",
            w.used_percent
        );
    }

    #[test]
    fn weekly_vs_short_window() {
        assert!(is_weekly(&json!({"window": "weekly"})));
        assert!(is_weekly(&json!({"bucketId": "gemini-weekly"})));
        assert!(!is_weekly(&json!({"bucketId": "gemini-5h"})));
    }

    #[test]
    fn bucket_to_window_reads_nested_remaining_shape() {
        // CLAUDE.md にある `{remaining:{remainingFraction}}` 形を読めること。
        let b = json!({
            "remaining": {"remainingFraction": 0.2},
            "resetTime": "2026-06-15T06:28:32Z"
        });
        let w = bucket_to_window(&b).unwrap();
        assert!(
            (w.used_percent - 80.0).abs() < 0.01,
            "used should be 80% got {}",
            w.used_percent
        );
    }

    #[test]
    fn bucket_reset_accepts_epoch_seconds() {
        let numeric = bucket_reset(&json!({"resetTime": 1_700_000_000_i64})).unwrap();
        assert_eq!(numeric.timestamp(), 1_700_000_000);

        let string = bucket_reset(&json!({"resetTime": "1700000001"})).unwrap();
        assert_eq!(string.timestamp(), 1_700_000_001);
    }

    #[test]
    fn parse_summary_picks_most_constrained_when_same_window_repeats() {
        // 同一 group 内に weekly bucket が複数あるとき、最も制約の厳しい
        // (= remainingFraction の小さい)bucket が採用されること。
        let v = json!({"response": {"groups": [
            {"displayName": "Gemini Models", "buckets": [
                {"bucketId": "loose-weekly", "window": "weekly",
                 "remainingFraction": 0.9, "resetTime": "2026-06-19T05:06:39Z"},
                {"bucketId": "tight-weekly", "window": "weekly",
                 "remainingFraction": 0.2, "resetTime": "2026-06-19T05:06:39Z"}
            ]}
        ]}});
        let rows = parse_summary(&v, None, None).unwrap();
        let w = rows[0].usage.weekly.as_ref().unwrap();
        // 0.2 = 80% used が採用される(0.9 = 10% used が後勝ちで上書きされる旧バグの再現確認)。
        assert!(
            (w.used_percent - 80.0).abs() < 0.01,
            "expected 80% (most constrained), got {}",
            w.used_percent
        );
    }

    #[test]
    fn group_label_shortening() {
        assert_eq!(short_group_label("Gemini Models"), "Gemini");
        assert_eq!(short_group_label("Claude and GPT models"), "Claude&GPT");
        assert_eq!(short_group_label("Other Models"), "Other");
    }

    #[test]
    fn expiry_both_shapes() {
        assert_eq!(
            parse_expiry(&json!({"expiry_date": 1_700_000_000_000_i64})),
            1_700_000_000
        );
        assert!(parse_expiry(&json!({"expiry": "2026-06-12T15:06:32.244434+09:00"})) > 0);
        assert_eq!(parse_expiry(&json!({})), 0);
    }

    #[test]
    fn extracts_oauth_client_from_js() {
        let js = r#"x({clientId:"1071006060591-abc123.apps.googleusercontent.com",clientSecret:"GOCSPX-0123456789012345678901234567"})y"#;
        assert_eq!(
            find_client_id(js).as_deref(),
            Some("1071006060591-abc123.apps.googleusercontent.com")
        );
        assert_eq!(
            find_secret(js).as_deref(),
            Some("GOCSPX-0123456789012345678901234567")
        );
    }

    #[test]
    fn user_identity_reads_wrapped_and_flat_shapes() {
        // Connect-RPC の wrapper 付き ({"response":{"userStatus":{...}}}) を読める。
        let wrapped = json!({"response": {"userStatus": {
            "email": "u@x.test",
            "planStatus": {"planInfo": {"planName": "Pro"}}
        }}});
        assert_eq!(
            user_identity(&wrapped),
            (Some("u@x.test".into()), Some("Pro".into()))
        );

        // wrapper 無しの top-level `userStatus` 形も従来どおり読める。
        let flat = json!({"userStatus": {
            "email": "v@x.test",
            "planStatus": {"planInfo": {"planName": "Team"}}
        }});
        assert_eq!(
            user_identity(&flat),
            (Some("v@x.test".into()), Some("Team".into()))
        );

        // userStatus キーが無い最外殻フラット形は email/plan を直接読む(全体に fallback)。
        let bare = json!({"email": "w@x.test"});
        assert_eq!(user_identity(&bare), (Some("w@x.test".into()), None));

        // email も plan も無ければ両方 None。
        assert_eq!(user_identity(&json!({})), (None, None));
    }
}
