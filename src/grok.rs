//! Grok (xAI) の使用量取得。
//!
//! grok CLI は `~/.grok/auth.json` に OAuth 認証情報 (access_token / refresh_token /
//! oidc_issuer / oidc_client_id) を保存する。Chrome の Cookie ではないため
//! `[grok]` config は Antigravity と同じく top-level(profile とは独立)。
//!
//!   GET https://cli-chat-proxy.grok.com/v1/user?include=subscription
//!     -> { email, subscriptionTier, hasGrokCodeAccess, ... }
//!   GET https://cli-chat-proxy.grok.com/v1/billing
//!     -> { config: { monthlyLimit, used, billingPeriodStart, billingPeriodEnd, history[] } }
//!
//! access token が期限切れなら `POST https://auth.x.ai/oauth2/token`
//! (grant_type=refresh_token, client_id=<auth.json.oidc_client_id>) で更新する。
//! grok CLI は public OAuth client のため client_secret は不要。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use chrono::{DateTime, Utc};
use serde_json::Value;
use wreq::{Client, StatusCode};

use crate::config::GrokCfg;
use crate::http::{get_json, post_form};
use crate::model::{Usage, UsageRow, Window, WindowKind};

/// CLI が読み書きする通信先。debug ログでも公開されているので固定で埋めてよい。
const CHAT_PROXY: &str = "https://cli-chat-proxy.grok.com/v1";
/// OAuth issuer(auth.x.ai)の token endpoint。`/.well-known/openid-configuration`
/// にも同じ値がある。issuer が別の deployment に切り替わる可能性は低いが、
/// auth.json 側の `oidc_issuer` を優先して読む。
const DEFAULT_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";

/// エンドポイント叩き分けを 1 経路にまとめた対外向け API。
pub async fn fetch(client: &Client, cfg: Option<&GrokCfg>) -> Result<Vec<UsageRow>> {
    let path = auth_path(cfg).context("no Grok auth.json found (grok CLI not signed in?)")?;
    let mut auth =
        load_auth(&path).with_context(|| format!("reading Grok auth file {}", path.display()))?;

    if auth.expires_in() < 60 {
        auth = refresh(client, &auth)
            .await
            .context("refreshing Grok OAuth token")?;
    }

    let user = match get_user(client, &auth.access).await {
        Ok(v) => v,
        Err(e) if is_auth_error(&e) => {
            auth = refresh(client, &auth)
                .await
                .context("refreshing after unauthorized user response")?;
            get_user(client, &auth.access).await?
        }
        Err(e) => return Err(e),
    };
    // billing endpoint は Free 相当のアカウントでも 200 を返す(monthlyLimit=0)。
    // 401/403 なら auth 側の問題なので surface する。取得失敗は long window を
    // None のままにする(rendering は "quota なし" として崩れず表示できる)。
    let billing = match get_billing(client, &auth.access).await {
        Ok(v) => Some(v),
        Err(e) if is_auth_error(&e) => return Err(e),
        Err(_) => None,
    };

    Ok(UsageRow::single(build_usage(&user, billing.as_ref())))
}

/// 認証情報がある = Grok を表示可能。Antigravity と同じく、`enabled = false` の
/// 明示指定を最優先する。
pub fn available(cfg: Option<&GrokCfg>) -> bool {
    if matches!(cfg, Some(c) if c.enabled == Some(false)) {
        return false;
    }
    auth_path(cfg).map(|p| p.exists()).unwrap_or(false)
}

// ================================ REST endpoints ================================

async fn get_user(client: &Client, access: &str) -> Result<Value> {
    // `?include=subscription` を渡すと `subscriptionTier` が入る。
    // Free アカウントでは null が返る。
    get_json(
        client,
        &format!("{CHAT_PROXY}/user?include=subscription"),
        "",
        Some(access),
        None,
    )
    .await
    .context("fetching /v1/user")
}

async fn get_billing(client: &Client, access: &str) -> Result<Value> {
    get_json(
        client,
        &format!("{CHAT_PROXY}/billing"),
        "",
        Some(access),
        None,
    )
    .await
    .context("fetching /v1/billing")
}

fn is_auth_error(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}");
    msg.contains("HTTP 401") || msg.contains("HTTP 403")
}

// ================================ Usage build =================================

/// `/v1/user` と optional `/v1/billing` を Usage に畳み込む。
///
/// - plan = `subscriptionTier`(null は "Free"、null で `hasGrokCodeAccess=true` の
///   場合は "Free (Grok Build)" として grok CLI 利用可否も示す)。
/// - long = billing の月次サイクル。`used / monthlyLimit * 100` を Monthly window に
///   入れる。`monthlyLimit == 0`(Free / まだ subscription を有効化していない)場合は、
///   billing period だけを 0% として表示し、reset 時刻の視認性は保つ。
/// - short = None(grok CLI は 5h window を REST では露出していない)。
fn build_usage(user: &Value, billing: Option<&Value>) -> Usage {
    let email = user
        .get("email")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let plan = plan_label(user);
    let long = billing.and_then(billing_to_window);

    Usage {
        email,
        plan,
        short: None,
        long,
    }
}

fn plan_label(user: &Value) -> Option<String> {
    let tier = user
        .get("subscriptionTier")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let has_build = user
        .get("hasGrokCodeAccess")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match tier {
        Some(t) => Some(t.to_string()),
        None if has_build => Some("Free".to_string()),
        None => Some("Free".to_string()),
    }
}

fn billing_to_window(v: &Value) -> Option<Window> {
    let config = v.get("config").unwrap_or(v);
    // API は `{"val": <number>}` の wrapper で数値を返す(通貨/枚数の混在を許容するため)。
    let limit = number_val(config.get("monthlyLimit"))?;
    let used = number_val(config.get("used")).unwrap_or(0.0);
    // `monthlyLimit = 0` は Free / 未設定サブスクの signal。credit 枠が存在しない
    // ため used_percent = 0 として、reset 時刻(billing period 末尾)だけを見せる。
    let used_percent = if limit > 0.0 {
        (used / limit * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let resets_at = config
        .get("billingPeriodEnd")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));
    Some(Window {
        kind: WindowKind::Monthly,
        used_percent,
        resets_at,
    })
}

fn number_val(v: Option<&Value>) -> Option<f64> {
    let v = v?;
    // `{val: N}` の nested と、そのままの N の両方に対応する。
    v.get("val").and_then(Value::as_f64).or_else(|| v.as_f64())
}

// ================================ Auth loading + refresh ================================

#[derive(Clone)]
struct Auth {
    access: String,
    refresh: String,
    /// Unix 秒。未知の場合は 0。
    expiry: i64,
    /// refresh 先の token endpoint。auth.json の oidc_issuer から組み立てる。
    token_url: String,
    /// refresh 時の client_id。auth.json の oidc_client_id 相当。
    client_id: String,
}

impl Auth {
    fn expires_in(&self) -> i64 {
        if self.expiry == 0 {
            return 0;
        }
        self.expiry - Utc::now().timestamp()
    }
}

fn auth_path(cfg: Option<&GrokCfg>) -> Option<PathBuf> {
    if let Some(p) = cfg.and_then(|c| c.auth_path.as_ref()) {
        return Some(expand(p));
    }
    dirs::home_dir().map(|h| h.join(".grok").join("auth.json"))
}

fn expand(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(p)
}

fn load_auth(path: &Path) -> Result<Auth> {
    let data = std::fs::read_to_string(path)?;
    parse_auth_document(&data)
}

/// `~/.grok/auth.json` の JSON 文字列を `Auth` に変換する。file I/O とは分離し、
/// unit test で tempfile なしで parse 分岐(動的キー wrapper / flat / 複数 entry)を
/// 網羅できるようにしてある。
fn parse_auth_document(data: &str) -> Result<Auth> {
    let v: Value = serde_json::from_str(data)?;
    // トップレベルは動的キー(`"<issuer>::<client_id>": {...}`)。最も新しい `create_time`
    // の entry を採用する。key を持たなければ file 全体を entry として扱う互換 fallback。
    let entry = select_entry(&v).context("Grok auth.json has no usable entry")?;

    let access = entry
        .get("key")
        .and_then(Value::as_str)
        .context("auth entry has no `key` (access token)")?
        .to_string();
    let refresh = entry
        .get("refresh_token")
        .and_then(Value::as_str)
        .context("auth entry has no `refresh_token`")?
        .to_string();
    let expiry = parse_expiry(entry).unwrap_or_else(|| jwt_exp(&access).unwrap_or(0));

    let issuer = entry
        .get("oidc_issuer")
        .and_then(Value::as_str)
        .unwrap_or("https://auth.x.ai")
        .trim_end_matches('/')
        .to_string();
    let token_url = if issuer.is_empty() {
        DEFAULT_TOKEN_URL.to_string()
    } else {
        format!("{issuer}/oauth2/token")
    };
    let client_id = entry
        .get("oidc_client_id")
        .and_then(Value::as_str)
        .context("auth entry has no `oidc_client_id`")?
        .to_string();

    Ok(Auth {
        access,
        refresh,
        expiry,
        token_url,
        client_id,
    })
}

/// `{"<issuer>::<client_id>": {...}}` 形式から最も新しい entry を選ぶ。
/// 単一 entry しか無い一般的ケースで hashmap 順序に依存させないための helper。
fn select_entry(v: &Value) -> Option<&Value> {
    let obj = v.as_object()?;
    // すでに `key` を持つトップレベルなら wrapper 無し形として扱う。
    if obj.contains_key("key") && obj.contains_key("refresh_token") {
        return Some(v);
    }
    // 動的キーの中身から entry を選ぶ。create_time が新しい方を優先。
    let mut best: Option<(&Value, i64)> = None;
    for (_, val) in obj {
        if !val.is_object() {
            continue;
        }
        let ts = val
            .get("create_time")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.timestamp())
            .unwrap_or(0);
        best = match best {
            None => Some((val, ts)),
            Some((_, cur)) if ts > cur => Some((val, ts)),
            other => other,
        };
    }
    best.map(|(v, _)| v)
}

fn parse_expiry(v: &Value) -> Option<i64> {
    // auth.json は ISO-8601 の `expires_at` を持つ。旧 fields には `expiry` / `expiry_date`
    // も観測されるので併せて許容する(pixellab.rs / antigravity.rs と同じ寛容ポリシー)。
    for key in ["expires_at", "expiry"] {
        if let Some(s) = v.get(key).and_then(Value::as_str)
            && let Ok(d) = DateTime::parse_from_rfc3339(s)
        {
            return Some(d.timestamp());
        }
    }
    if let Some(ms) = v.get("expiry_date").and_then(Value::as_i64) {
        return Some(ms / 1000);
    }
    None
}

/// JWT の `exp` claim を取り出す。auth.json に expires_at が無いときの二次 fallback。
fn jwt_exp(jwt: &str) -> Option<i64> {
    let payload = jwt.split('.').nth(1)?;
    let mut b64 = payload.replace('-', "+").replace('_', "/");
    while b64.len() % 4 != 0 {
        b64.push('=');
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims.get("exp").and_then(Value::as_i64)
}

async fn refresh(client: &Client, auth: &Auth) -> Result<Auth> {
    let form = [
        ("client_id", auth.client_id.as_str()),
        ("grant_type", "refresh_token"),
        ("refresh_token", auth.refresh.as_str()),
    ];
    let (status, body) = post_form(client, &auth.token_url, &form).await?;
    if !status.is_success() {
        let msg = body
            .get("error_description")
            .or_else(|| body.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        // 401 は refresh token が失効している(サインアウトされた等)。ユーザ操作を促す。
        if status == StatusCode::UNAUTHORIZED {
            bail!(
                "Grok OAuth refresh rejected (HTTP 401): {msg}. Re-run `grok login` to \
                 refresh ~/.grok/auth.json."
            );
        }
        bail!(
            "Grok OAuth refresh failed (HTTP {}): {msg}",
            status.as_u16()
        );
    }
    let access = body
        .get("access_token")
        .and_then(Value::as_str)
        .context("refresh response has no access_token")?
        .to_string();
    let refresh = body
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or(&auth.refresh)
        .to_string();
    let expiry = body
        .get("expires_in")
        .and_then(Value::as_i64)
        .map(|s| Utc::now().timestamp() + s)
        .or_else(|| jwt_exp(&access))
        .unwrap_or(0);
    Ok(Auth {
        access,
        refresh,
        expiry,
        token_url: auth.token_url.clone(),
        client_id: auth.client_id.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde_json::json;

    fn make_jwt(claims: &Value) -> String {
        let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
        format!("hdr.{body}.sig")
    }

    #[test]
    fn number_val_reads_wrapped_and_flat_numbers() {
        // `{val: N}` wrapper と、そのままの N の両方を許容する。
        assert_eq!(number_val(Some(&json!({"val": 1234.0}))), Some(1234.0));
        assert_eq!(number_val(Some(&json!(42))), Some(42.0));
        // wrapper 内部が数値でなければ None(呼び出し側は long window を作らない)。
        assert_eq!(number_val(Some(&json!({"val": "no"}))), None);
        assert_eq!(number_val(None), None);
    }

    #[test]
    fn billing_to_window_reads_period_and_percent() {
        // monthlyLimit > 0 の Pro 相当ユーザを再現。
        let b = json!({"config": {
            "monthlyLimit": {"val": 100.0},
            "used": {"val": 25.0},
            "billingPeriodStart": "2026-07-01T00:00:00+00:00",
            "billingPeriodEnd": "2026-08-01T00:00:00+00:00",
        }});
        let w = billing_to_window(&b).unwrap();
        assert_eq!(w.kind, WindowKind::Monthly);
        assert!((w.used_percent - 25.0).abs() < 0.01);
        assert_eq!(
            w.resets_at.unwrap().to_rfc3339(),
            "2026-08-01T00:00:00+00:00"
        );
    }

    #[test]
    fn billing_to_window_free_tier_shows_zero_percent_with_reset_time() {
        // Free / 未設定サブスクは monthlyLimit=0 で降ってくる。0% + 有効な reset 時刻。
        let b = json!({"config": {
            "monthlyLimit": {"val": 0.0},
            "used": {"val": 0.0},
            "billingPeriodStart": "2026-07-01T00:00:00+00:00",
            "billingPeriodEnd": "2026-08-01T00:00:00+00:00",
        }});
        let w = billing_to_window(&b).unwrap();
        assert_eq!(w.used_percent, 0.0);
        assert!(w.resets_at.is_some());
    }

    #[test]
    fn billing_to_window_clamps_over_used() {
        // race 境界などで used > limit が返っても 100% に丸める。
        let b = json!({"config": {
            "monthlyLimit": {"val": 100.0},
            "used": {"val": 150.0},
            "billingPeriodEnd": "2026-08-01T00:00:00+00:00",
        }});
        let w = billing_to_window(&b).unwrap();
        assert_eq!(w.used_percent, 100.0);
    }

    #[test]
    fn plan_label_prefers_subscription_then_free() {
        // subscriptionTier が入っていればそれをそのまま plan として表示。
        let paid = json!({"subscriptionTier": "SuperGrok"});
        assert_eq!(plan_label(&paid).as_deref(), Some("SuperGrok"));

        // Free tier(subscriptionTier=null)は "Free"。hasGrokCodeAccess で分岐しない。
        let free_with_build = json!({"subscriptionTier": null, "hasGrokCodeAccess": true});
        assert_eq!(plan_label(&free_with_build).as_deref(), Some("Free"));
        let free_no_build = json!({"subscriptionTier": null, "hasGrokCodeAccess": false});
        assert_eq!(plan_label(&free_no_build).as_deref(), Some("Free"));
    }

    #[test]
    fn build_usage_pulls_email_and_long_from_billing() {
        let user = json!({
            "email": "yohei@example.com",
            "subscriptionTier": "SuperGrok",
            "hasGrokCodeAccess": true,
        });
        let billing = json!({"config": {
            "monthlyLimit": {"val": 200.0},
            "used": {"val": 40.0},
            "billingPeriodEnd": "2026-08-01T00:00:00+00:00",
        }});
        let u = build_usage(&user, Some(&billing));
        assert_eq!(u.email.as_deref(), Some("yohei@example.com"));
        assert_eq!(u.plan.as_deref(), Some("SuperGrok"));
        assert!(u.short.is_none());
        let w = u.long.as_ref().unwrap();
        assert_eq!(w.kind, WindowKind::Monthly);
        assert!((w.used_percent - 20.0).abs() < 0.01);
    }

    #[test]
    fn build_usage_without_billing_leaves_long_none() {
        // billing 取得に失敗しても plan / email だけは出る(row は "quota なし" 表示)。
        let user = json!({"email": "y@x.test", "subscriptionTier": null});
        let u = build_usage(&user, None);
        assert_eq!(u.email.as_deref(), Some("y@x.test"));
        assert_eq!(u.plan.as_deref(), Some("Free"));
        assert!(u.short.is_none());
        assert!(u.long.is_none());
    }

    #[test]
    fn select_entry_picks_newest_by_create_time() {
        // 実 auth.json は `"<issuer>::<client_id>": {...}` の 1 entry だが、
        // 複数 entry が混じっても create_time で最新を選ぶ。
        let older = json!({
            "key": "old-token",
            "refresh_token": "old-refresh",
            "oidc_issuer": "https://auth.x.ai",
            "oidc_client_id": "client-1",
            "create_time": "2026-06-01T00:00:00Z",
        });
        let newer = json!({
            "key": "new-token",
            "refresh_token": "new-refresh",
            "oidc_issuer": "https://auth.x.ai",
            "oidc_client_id": "client-2",
            "create_time": "2026-07-01T00:00:00Z",
        });
        let doc = json!({
            "https://auth.x.ai::client-1": older,
            "https://auth.x.ai::client-2": newer,
        });
        let entry = select_entry(&doc).unwrap();
        assert_eq!(entry.get("key").and_then(Value::as_str), Some("new-token"));
    }

    #[test]
    fn select_entry_reads_flat_document_as_entry() {
        // 動的キー wrapper が無く、直下に key / refresh_token がある形も許容する。
        let doc = json!({
            "key": "flat-token",
            "refresh_token": "flat-refresh",
            "oidc_client_id": "client",
        });
        let entry = select_entry(&doc).unwrap();
        assert_eq!(entry.get("key").and_then(Value::as_str), Some("flat-token"));
    }

    #[test]
    fn parse_expiry_prefers_expires_at_iso() {
        let entry = json!({"expires_at": "2026-07-15T12:09:17.346427Z"});
        let ts = parse_expiry(&entry).unwrap();
        assert!(ts > 1_784_000_000);
    }

    #[test]
    fn parse_expiry_falls_back_to_expiry_date_ms() {
        // millisecond epoch も許容(pixellab / antigravity と同じ)。
        let entry = json!({"expiry_date": 1_784_000_000_000_i64});
        assert_eq!(parse_expiry(&entry), Some(1_784_000_000));
    }

    #[test]
    fn jwt_exp_extracts_claim() {
        let jwt = make_jwt(&json!({"exp": 1_784_117_357_i64, "sub": "u"}));
        assert_eq!(jwt_exp(&jwt), Some(1_784_117_357));
        assert_eq!(jwt_exp("garbage"), None);
    }

    #[test]
    fn parse_auth_document_reads_real_shape() {
        // 動的キー wrapper + `key`(access JWT)の 実 auth.json 形をパースできる。
        let jwt = make_jwt(&json!({"exp": Utc::now().timestamp() + 3600}));
        let doc = json!({
            "https://auth.x.ai::abc": {
                "key": jwt,
                "refresh_token": "REFRESH",
                "auth_mode": "oidc",
                "create_time": "2026-07-15T06:09:17Z",
                "expires_at": "2026-07-15T12:09:17Z",
                "oidc_issuer": "https://auth.x.ai",
                "oidc_client_id": "abc",
            }
        });
        let auth = parse_auth_document(&doc.to_string()).unwrap();
        assert_eq!(auth.refresh, "REFRESH");
        assert_eq!(auth.client_id, "abc");
        // oidc_issuer を優先し、末尾スラッシュなしで token endpoint を組み立てる。
        assert_eq!(auth.token_url, "https://auth.x.ai/oauth2/token");
    }

    #[test]
    fn parse_auth_document_defaults_issuer_when_missing() {
        // 動的キー wrapper 無し + oidc_issuer 未指定 → default (auth.x.ai) を採用する。
        let doc = json!({
            "key": "flat-token",
            "refresh_token": "flat-refresh",
            "oidc_client_id": "cid",
        });
        let auth = parse_auth_document(&doc.to_string()).unwrap();
        assert_eq!(auth.access, "flat-token");
        assert_eq!(auth.token_url, "https://auth.x.ai/oauth2/token");
    }

    #[test]
    fn parse_auth_document_errors_without_refresh_token() {
        // access token だけあっても、refresh 経路が張れないので拒否する。
        let doc = json!({"key": "only-access", "oidc_client_id": "cid"});
        assert!(parse_auth_document(&doc.to_string()).is_err());
    }

    #[test]
    fn is_auth_error_detects_401_403() {
        assert!(is_auth_error(&anyhow::anyhow!(
            "fetching /v1/user: HTTP 401 from ..."
        )));
        assert!(is_auth_error(&anyhow::anyhow!("HTTP 403 from ...")));
        assert!(!is_auth_error(&anyhow::anyhow!("connection refused")));
    }

    #[test]
    fn available_respects_explicit_disable() {
        // 明示的な enabled=false は他条件を無視して常に false。
        let cfg = GrokCfg {
            enabled: Some(false),
            auth_path: Some("/does/not/exist".to_string()),
            label: None,
        };
        assert!(!available(Some(&cfg)));
    }
}
