//! PixelLab の使用量取得。profile の `supabase-auth-token` Cookie(www.pixellab.ai)を
//! Bearer token として使い、必要なら Supabase の refresh flow で更新してから
//! Cloudflare-fronted な PixelLab API を叩く:
//!   GET api.pixellab.ai/get-account-data  -> { imageAmount, imageGenerated, credits, tier, token }
//!   GET api.pixellab.ai/get-subscription  -> { name, generation_reset_date, next_bill_date, ... }
//!
//! Cookie は Supabase Auth Helpers の legacy JSON array 形式で保存されている:
//!   `["<access_token JWT>","<refresh_token>",null,null,null]` を URL-encode したもの。
//! 大きい token は Codex と同じく `supabase-auth-token.0` / `.1` に分割される。
//!
//! 期限切れ access token は `POST supabase.pixellab.ai/auth/v1/token?grant_type=refresh_token`
//! で更新する。anon key は PixelLab の JS bundle に埋め込まれた公開値
//! (`NEXT_PUBLIC_SUPABASE_ANON_KEY` 相当)なのでそのまま埋めてよい。

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use base64::Engine;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use wreq::Client;

use crate::http::get_json;
use crate::model::{Usage, UsageRow, Window};

/// www.pixellab.ai の Supabase auth Cookie。大きい token は Next.js と同じ `…token.0` /
/// `…token.1` に分割される。名前を 1 箇所で定義して cookie_bundle / has_session を揃える。
const SESSION_COOKIE: &str = "supabase-auth-token";

/// PixelLab フロントの Supabase プロジェクト。auth endpoint は Cloudflare 経由。
const SUPABASE_ORIGIN: &str = "https://supabase.pixellab.ai";
const API_ORIGIN: &str = "https://api.pixellab.ai";

/// PixelLab の JS bundle に含まれる anon key(role=anon, iat=2023-01, exp=2033-01)。
/// 公開値のため embed する。差し替えたい場合は `ANTIGRAVITY_*` と同じく env override 可能に
/// できるが、現時点で用途がないので固定。
const SUPABASE_ANON_KEY: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.\
eyJpc3MiOiJzdXBhYmFzZSIsInJlZiI6InlhemJ4cGt5cWdmbmh4YnJrdGdiIiwicm9sZSI6ImFub24iLCJpYXQiOjE2NzQzMzI5NTUsImV4cCI6MTk4OTkwODk1NX0.\
5a8GUrRDP8hHUgW4Bv4qD3eB_t5m9ewDSrIpAIPurvo";

/// この profile が PixelLab session Cookie を持つかどうか。split `…token.0` 形式と suffix
/// なし形式の両方を許容する。
pub fn has_session(cookies: &HashMap<String, String>) -> bool {
    cookies.contains_key(&format!("{SESSION_COOKIE}.0")) || cookies.contains_key(SESSION_COOKIE)
}

/// 分割された `…token.0` / `…token.1` / ... を連結して 1 つの value 文字列にする。
/// Codex の chunk 処理と同じロジックで、連番が途切れた時点で打ち切る。
/// split 形式が優先で、無ければ suffix なしの単体 Cookie にフォールバックする。
fn joined_cookie_value(cookies: &HashMap<String, String>) -> Option<String> {
    let mut idx = 0_usize;
    let mut joined = String::new();
    while let Some(v) = cookies.get(&format!("{SESSION_COOKIE}.{idx}")) {
        joined.push_str(v);
        idx += 1;
    }
    if joined.is_empty() {
        return cookies.get(SESSION_COOKIE).cloned();
    }
    Some(joined)
}

/// 復号済み Cookie から access_token / refresh_token を取り出す。
fn parse_session(cookies: &HashMap<String, String>) -> Result<SessionTokens> {
    let raw =
        joined_cookie_value(cookies).context("not signed in to pixellab.ai in this profile")?;
    let decoded = percent_decode(&raw);
    // Supabase Auth Helpers の legacy 形式は JSON array:
    //   [access_token, refresh_token, provider_token, provider_refresh_token, ...]
    // 新形式(base64 プレフィックス付きの `base64-…` JSON object)も、要素が
    // `access_token` / `refresh_token` を持つ dict なら同じ経路で扱える。
    let (access, refresh) = if let Some(rest) = decoded.strip_prefix("base64-") {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(rest.trim())
            .context("decoding base64 supabase cookie payload")?;
        let v: Value = serde_json::from_slice(&bytes)
            .context("parsing base64 supabase cookie payload as JSON")?;
        session_from_object(&v)?
    } else {
        let v: Value =
            serde_json::from_str(&decoded).context("parsing supabase-auth-token cookie as JSON")?;
        match v {
            Value::Array(_) => session_from_array(&v)?,
            _ => session_from_object(&v)?,
        }
    };
    Ok(SessionTokens { access, refresh })
}

fn session_from_array(v: &Value) -> Result<(String, String)> {
    let arr = v
        .as_array()
        .context("supabase cookie is not a JSON array")?;
    let access = arr
        .first()
        .and_then(Value::as_str)
        .context("supabase cookie array has no access token")?
        .to_string();
    let refresh = arr
        .get(1)
        .and_then(Value::as_str)
        .context("supabase cookie array has no refresh token")?
        .to_string();
    Ok((access, refresh))
}

fn session_from_object(v: &Value) -> Result<(String, String)> {
    let access = v
        .get("access_token")
        .and_then(Value::as_str)
        .context("supabase cookie has no access_token")?
        .to_string();
    let refresh = v
        .get("refresh_token")
        .and_then(Value::as_str)
        .context("supabase cookie has no refresh_token")?
        .to_string();
    Ok((access, refresh))
}

struct SessionTokens {
    access: String,
    refresh: String,
}

/// `%XX` エンコードを decode する最小実装。Cookie 値には ASCII / `%XX` しか入らない前提で、
/// 不正な `%XX` は元の文字列にそのまま残す(fail-soft、JSON parse 側でエラーにする)。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex_val(bytes[i + 1]);
            let lo = hex_val(bytes[i + 2]);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub async fn fetch(client: &Client, cookies: &HashMap<String, String>) -> Result<Vec<UsageRow>> {
    let mut session = parse_session(cookies)?;

    // access token が期限切れなら refresh してから叩く。期限内でも失敗したら
    // 401/403 fallback で refresh を試す(clock skew や revoke 対策)。
    if !access_token_fresh(&session.access) {
        session = refresh_session(client, &session.refresh)
            .await
            .context("refreshing PixelLab access token")?;
    }

    let account = match get_account_data(client, &session.access).await {
        Ok(v) => v,
        Err(e) if is_auth_error(&e) => {
            session = refresh_session(client, &session.refresh)
                .await
                .context("refreshing after unauthorized response")?;
            get_account_data(client, &session.access).await?
        }
        Err(e) => return Err(e),
    };
    // /get-subscription はサブスク未加入の profile では 200 で `{}` を返すことがある。
    // 生成枠の reset time は加入時のみ意味を持つ。取得失敗 = 未加入相当として無視する。
    let subscription = get_subscription(client, &session.access).await.ok();

    let usage = build_usage(&session.access, &account, subscription.as_ref())?;
    Ok(UsageRow::single(usage))
}

/// JWT payload の `exp` が近い(<= 60 秒)場合は再取得する。破損 JWT は 0 として扱う。
fn access_token_fresh(access: &str) -> bool {
    let Some(payload) = access.split('.').nth(1) else {
        return false;
    };
    let mut b64 = payload.replace('-', "+").replace('_', "/");
    while b64.len() % 4 != 0 {
        b64.push('=');
    }
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&b64) else {
        return false;
    };
    let Ok(claims) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    let Some(exp) = claims.get("exp").and_then(Value::as_i64) else {
        return false;
    };
    exp - Utc::now().timestamp() > 60
}

fn jwt_email(access: &str) -> Option<String> {
    let payload = access.split('.').nth(1)?;
    let mut b64 = payload.replace('-', "+").replace('_', "/");
    while b64.len() % 4 != 0 {
        b64.push('=');
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .get("email")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Cloudflare の challenge / 401 / 403(auth 期限切れ)を detect する。
fn is_auth_error(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}");
    msg.contains("HTTP 401") || msg.contains("HTTP 403") || msg.contains("Invalid token")
}

async fn get_account_data(client: &Client, access: &str) -> Result<Value> {
    // Cookie header は不要。api.pixellab.ai は Bearer だけで通る。
    get_json(
        client,
        &format!("{API_ORIGIN}/get-account-data"),
        "",
        Some(access),
        None,
    )
    .await
    .context("fetching /get-account-data")
}

async fn get_subscription(client: &Client, access: &str) -> Result<Value> {
    get_json(
        client,
        &format!("{API_ORIGIN}/get-subscription"),
        "",
        Some(access),
        None,
    )
    .await
    .context("fetching /get-subscription")
}

async fn refresh_session(client: &Client, refresh: &str) -> Result<SessionTokens> {
    let url = format!("{SUPABASE_ORIGIN}/auth/v1/token?grant_type=refresh_token");
    // Supabase の auth endpoint は anon key を apikey / Authorization ヘッダの両方で要求する。
    // wreq の bearer_auth は Authorization を上書きするだけなので、apikey は body 直下で追加する。
    let body = json!({ "refresh_token": refresh });
    // apikey ヘッダは post_json では設定できないため、custom request を組む。
    let payload = serde_json::to_vec(&body).context("serializing refresh body")?;
    let resp = client
        .post(&url)
        .header("apikey", SUPABASE_ANON_KEY)
        .bearer_auth(SUPABASE_ANON_KEY)
        .header("Content-Type", "application/json")
        .header("Origin", "https://www.pixellab.ai")
        .header("Referer", "https://www.pixellab.ai/")
        .body(payload)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let snippet: String = text.chars().take(160).collect();
        bail!(
            "Supabase refresh failed (HTTP {}): {snippet}",
            status.as_u16()
        );
    }
    let v: Value =
        serde_json::from_str(&text).context("parsing Supabase refresh response as JSON")?;
    let access = v
        .get("access_token")
        .and_then(Value::as_str)
        .context("refresh response has no access_token")?
        .to_string();
    let refresh = v
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or(refresh)
        .to_string();
    Ok(SessionTokens { access, refresh })
}

/// `/get-account-data` + optional `/get-subscription` を Usage に畳み込む。
/// - `imageGenerated / imageAmount` を monthly generation window として `weekly` に入れる
///   (統一 model の long-window スロットを再利用)。resets_at は subscription の
///   `generation_reset_date`、なければ `next_bill_date` を使う。
/// - `credits`(USD pay-as-you-go)は plan ラベルに `+ $X.XX credits` として付ける。
fn build_usage(access: &str, account: &Value, subscription: Option<&Value>) -> Result<Usage> {
    let image_amount = account
        .get("imageAmount")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let image_generated = account
        .get("imageGenerated")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let credits = account
        .get("credits")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let tier = account.get("tier").and_then(Value::as_i64).unwrap_or(0);

    let plan_name = subscription
        .and_then(|s| s.get("name"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if tier >= 1 {
                format!("Tier {tier}")
            } else {
                "Free".to_string()
            }
        });
    let plan = if credits > 0.0 {
        Some(format!("{plan_name} + ${credits:.2} credits"))
    } else {
        Some(plan_name)
    };

    let weekly = if image_amount > 0.0 {
        let used_percent = (image_generated / image_amount * 100.0).clamp(0.0, 100.0);
        let resets_at = subscription.and_then(subscription_reset);
        Some(Window {
            used_percent,
            resets_at,
        })
    } else {
        None
    };

    Ok(Usage {
        email: jwt_email(access),
        plan,
        five_hour: None,
        weekly,
    })
}

fn subscription_reset(v: &Value) -> Option<DateTime<Utc>> {
    for key in ["generation_reset_date", "next_bill_date", "expiry_date"] {
        if let Some(s) = v.get(key).and_then(Value::as_str)
            && let Ok(d) = DateTime::parse_from_rfc3339(s)
        {
            return Some(d.with_timezone(&Utc));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde_json::json;

    fn put(c: &mut HashMap<String, String>, k: &str, v: &str) {
        c.insert(k.to_string(), v.to_string());
    }

    fn make_jwt(claims: &Value) -> String {
        let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
        format!("hdr.{body}.sig")
    }

    fn short_lived_jwt(email: &str, exp_offset: i64) -> String {
        let exp = Utc::now().timestamp() + exp_offset;
        make_jwt(&json!({ "email": email, "exp": exp }))
    }

    #[test]
    fn percent_decode_basic() {
        assert_eq!(percent_decode("hello"), "hello");
        assert_eq!(percent_decode("%5B%22a%22%5D"), "[\"a\"]");
        // 不正な `%X` は fail-soft で残す。
        assert_eq!(percent_decode("50%off"), "50%off");
    }

    #[test]
    fn has_session_recognizes_split_and_flat_forms() {
        let mut c = HashMap::new();
        assert!(!has_session(&c));
        put(&mut c, SESSION_COOKIE, "x");
        assert!(has_session(&c));

        let mut c2 = HashMap::new();
        put(&mut c2, &format!("{SESSION_COOKIE}.0"), "x");
        assert!(has_session(&c2));
    }

    #[test]
    fn joined_cookie_value_combines_chunks_in_order() {
        let mut c = HashMap::new();
        put(&mut c, &format!("{SESSION_COOKIE}.0"), "aaa");
        put(&mut c, &format!("{SESSION_COOKIE}.1"), "bbb");
        put(&mut c, &format!("{SESSION_COOKIE}.2"), "ccc");
        // .4 は連番が途切れた後なので採用しない(現実には起きないが防御的挙動を確認)。
        put(&mut c, &format!("{SESSION_COOKIE}.4"), "skip");
        assert_eq!(joined_cookie_value(&c).as_deref(), Some("aaabbbccc"));
    }

    #[test]
    fn parse_session_reads_legacy_array_format() {
        // %5B%22JWT%22%2C%22REFRESH%22%2Cnull%2Cnull%2Cnull%5D = ["JWT","REFRESH",null,null,null]
        let value = "%5B%22JWT%22%2C%22REFRESH%22%2Cnull%2Cnull%2Cnull%5D";
        let mut c = HashMap::new();
        put(&mut c, SESSION_COOKIE, value);
        let s = parse_session(&c).unwrap();
        assert_eq!(s.access, "JWT");
        assert_eq!(s.refresh, "REFRESH");
    }

    #[test]
    fn parse_session_reads_object_form_with_base64_prefix() {
        // 新形式: base64-<base64({"access_token","refresh_token",...})>
        let payload = json!({
            "access_token": "AAA",
            "refresh_token": "BBB",
        });
        let encoded = base64::engine::general_purpose::STANDARD.encode(payload.to_string());
        let cookie = format!("base64-{encoded}");
        let mut c = HashMap::new();
        put(&mut c, SESSION_COOKIE, &cookie);
        let s = parse_session(&c).unwrap();
        assert_eq!(s.access, "AAA");
        assert_eq!(s.refresh, "BBB");
    }

    #[test]
    fn parse_session_errors_without_cookie() {
        let c = HashMap::new();
        assert!(parse_session(&c).is_err());
    }

    #[test]
    fn access_token_fresh_uses_exp_claim() {
        // 期限切れ / 期限内を JWT の `exp` から判定する。
        let expired = short_lived_jwt("x@x.test", -60);
        let valid = short_lived_jwt("x@x.test", 600);
        assert!(!access_token_fresh(&expired));
        assert!(access_token_fresh(&valid));
        // 破損 JWT は不新鮮扱い(=refresh を試みる)。
        assert!(!access_token_fresh("garbage"));
    }

    #[test]
    fn jwt_email_reads_email_claim() {
        let jwt = short_lived_jwt("yohei@example.com", 600);
        assert_eq!(jwt_email(&jwt).as_deref(), Some("yohei@example.com"));
        // 空 email は None。
        let no_email = make_jwt(&json!({ "email": "" }));
        assert_eq!(jwt_email(&no_email), None);
        // 破損 JWT は None。
        assert_eq!(jwt_email("garbage"), None);
    }

    #[test]
    fn build_usage_maps_monthly_generation_to_weekly_slot() {
        let jwt = short_lived_jwt("yohei@example.com", 600);
        let account = json!({
            "imageAmount": 2000.0,
            "imageGenerated": 919.0,
            "credits": 0.0,
            "tier": 1,
            "token": "api-token",
        });
        let sub = json!({
            "name": "Tier 1: Pixel Apprentice",
            "generation_reset_date": "2026-07-08T00:00:00+00:00",
            "status": true,
        });
        let u = build_usage(&jwt, &account, Some(&sub)).unwrap();
        assert_eq!(u.email.as_deref(), Some("yohei@example.com"));
        assert_eq!(u.plan.as_deref(), Some("Tier 1: Pixel Apprentice"));
        assert!(u.five_hour.is_none());
        let w = u.weekly.as_ref().unwrap();
        assert!(
            (w.used_percent - 45.95).abs() < 0.05,
            "expected ~45.95%, got {}",
            w.used_percent
        );
        // resets_at は generation_reset_date の RFC 3339 が UTC で入る。
        let r = w.resets_at.unwrap();
        assert_eq!(r.to_rfc3339(), "2026-07-08T00:00:00+00:00");
    }

    #[test]
    fn build_usage_appends_credits_to_plan_when_positive() {
        let jwt = short_lived_jwt("y@x.test", 600);
        let account = json!({
            "imageAmount": 2000.0,
            "imageGenerated": 0.0,
            "credits": 3.5,
            "tier": 1,
        });
        let sub = json!({ "name": "Tier 1: Pixel Apprentice" });
        let u = build_usage(&jwt, &account, Some(&sub)).unwrap();
        assert_eq!(
            u.plan.as_deref(),
            Some("Tier 1: Pixel Apprentice + $3.50 credits")
        );
    }

    #[test]
    fn build_usage_without_subscription_names_by_tier() {
        // Tier だけ判る場合は "Tier N" / "Free" にフォールバック。
        let jwt = short_lived_jwt("y@x.test", 600);
        let free = build_usage(
            &jwt,
            &json!({ "imageAmount": 0.0, "imageGenerated": 0.0, "credits": 0.0, "tier": 0 }),
            None,
        )
        .unwrap();
        assert_eq!(free.plan.as_deref(), Some("Free"));
        // imageAmount = 0 は generation quota なしとして weekly も None。
        assert!(free.weekly.is_none());

        let paid = build_usage(
            &jwt,
            &json!({ "imageAmount": 500.0, "imageGenerated": 100.0, "credits": 0.0, "tier": 2 }),
            None,
        )
        .unwrap();
        assert_eq!(paid.plan.as_deref(), Some("Tier 2"));
        let w = paid.weekly.as_ref().unwrap();
        assert!((w.used_percent - 20.0).abs() < 0.01);
        // subscription が無いと reset date も無い(bar だけ表示)。
        assert!(w.resets_at.is_none());
    }

    #[test]
    fn subscription_reset_prefers_generation_reset_over_next_bill() {
        // 月次サイクル: generation_reset_date が正しい reset 時刻。
        // 未設定なら next_bill_date、それも無ければ expiry_date にフォールバック。
        let s = json!({
            "generation_reset_date": "2026-07-08T00:00:00+00:00",
            "next_bill_date": "2026-08-08T00:00:00+00:00",
            "expiry_date": "2027-01-01T00:00:00+00:00",
        });
        let r = subscription_reset(&s).unwrap();
        assert_eq!(r.to_rfc3339(), "2026-07-08T00:00:00+00:00");

        let s2 = json!({ "next_bill_date": "2026-08-08T00:00:00+00:00" });
        assert_eq!(
            subscription_reset(&s2).unwrap().to_rfc3339(),
            "2026-08-08T00:00:00+00:00"
        );

        let s3 = json!({ "expiry_date": "2027-01-01T00:00:00+00:00" });
        assert_eq!(
            subscription_reset(&s3).unwrap().to_rfc3339(),
            "2027-01-01T00:00:00+00:00"
        );

        assert!(subscription_reset(&json!({})).is_none());
    }

    #[test]
    fn build_usage_clamps_over_generated_to_100_percent() {
        // 超過が API から返るケース(サイクル境界での race)でも 100% でクランプする。
        let jwt = short_lived_jwt("y@x.test", 600);
        let u = build_usage(
            &jwt,
            &json!({
                "imageAmount": 100.0,
                "imageGenerated": 150.0,
                "credits": 0.0,
                "tier": 1,
            }),
            Some(&json!({ "name": "Pro" })),
        )
        .unwrap();
        assert_eq!(u.weekly.as_ref().unwrap().used_percent, 100.0);
    }

    #[test]
    fn is_auth_error_detects_401_403_and_invalid_token() {
        // http::get_json の error 文言に含まれる pattern を検出する。
        assert!(is_auth_error(&anyhow!(
            "fetching /get-account-data: HTTP 401 from https://api.pixellab.ai/x: {{\"detail\":\"nope\"}}"
        )));
        assert!(is_auth_error(&anyhow!(
            "HTTP 403 from url: {{\"detail\":\"Invalid token\"}}"
        )));
        assert!(is_auth_error(&anyhow!(
            "some wrapper: {{\"detail\":\"Invalid token\"}}"
        )));
        // 通常のネットワークエラーは false。
        assert!(!is_auth_error(&anyhow!(
            "GET https://api.pixellab.ai/x: connection refused"
        )));
        assert!(!is_auth_error(&anyhow!("HTTP 500 from url")));
    }
}
