//! Codex/ChatGPT 使用量取得。profile の `__Secure-next-auth.session-token` Cookie を
//! Bearer token に交換し、その token で Codex usage endpoint を呼ぶ:
//!   GET /api/auth/session         -> { accessToken, user }
//!   GET /backend-api/wham/usage   -> { rate_limit: { primary_window, ... } }

use std::collections::HashMap;

use anyhow::{Context, Result};
use base64::Engine;
use chrono::{TimeZone, Utc};
use wreq::Client;

use crate::http::get_json;
use crate::model::{Usage, UsageRow, Window, WindowKind};

/// chatgpt.com の session-token Cookie。大きい token は `…token.0` と `…token.1` に
/// 分割され、小さい token は suffix なしの `…session-token` に入る。
/// 送信側の `cookie_header` と検出側の `has_session` がずれないよう、ここで一元化する。
const SESSION_TOKEN: &str = "__Secure-next-auth.session-token";

fn cookie_header(cookies: &HashMap<String, String>) -> Result<String> {
    // Next.js の cookie chunking は `.0`, `.1`, `.2`, ... と連番で分割する(各 chunk が
    // 4kB 以下)。`.1` までしか送らないとトークンが大きいときに途中で切れ、サーバ側で
    // 無効セッション扱いになる。連番が途切れるまで全 chunk を結合する。
    let mut header = String::new();
    let mut idx = 0_usize;
    while let Some(v) = cookies.get(&format!("{SESSION_TOKEN}.{idx}")) {
        if !header.is_empty() {
            header.push_str("; ");
        }
        header.push_str(&format!("{SESSION_TOKEN}.{idx}={v}"));
        idx += 1;
    }
    if header.is_empty() {
        if let Some(t) = cookies.get(SESSION_TOKEN) {
            header.push_str(&format!("{SESSION_TOKEN}={t}"));
        } else {
            anyhow::bail!("not signed in to chatgpt.com in this profile");
        }
    }
    for name in ["cf_clearance", "__cf_bm", "_puid"] {
        if let Some(v) = cookies.get(name) {
            header.push_str(&format!("; {name}={v}"));
        }
    }
    Ok(header)
}

/// この profile が chatgpt.com session-token Cookie を持つかどうか。split `…token.0` 形式と
/// suffix なし形式の両方を見る。`cookie_header` の要件と揃え、caller が Cookie 名を知らずに済む。
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
    let (short, long) = classify_windows(usage.get("rate_limit"));

    Ok(UsageRow::single(Usage {
        email,
        plan,
        short,
        long,
    }))
}

/// 短期スロットとみなす window duration の上限。5 時間枠に多少の余裕を見た値で、
/// これを超える window は長期(週次)スロットへ回す。
const SHORT_WINDOW_MAX_SECONDS: i64 = 8 * 3600;

/// `rate_limit` の primary / secondary window を、**JSON 上の位置ではなく window duration**
/// で短期 / 長期スロットに振り分ける。どちらが 5 時間枠かは応答の順序に依存しないため、
/// primary/secondary が入れ替わっても表示が崩れない。
fn classify_windows(rate: Option<&serde_json::Value>) -> (Option<Window>, Option<Window>) {
    let mut short = None;
    let mut long = None;
    for key in ["primary_window", "secondary_window"] {
        let Some(w) = rate.and_then(|r| r.get(key)) else {
            continue;
        };
        if w.is_null() {
            continue;
        }
        // duration が欠落 / null の window を「0 秒 = 短期」と見なさない。0 に倒すと
        // 週次枠が 5h スロットへ入り、使用率が誤ったラベルで表示される。duration が
        // 読めないときだけ、従来どおり JSON 上の位置へ fallback する。
        let is_short = match w
            .get("limit_window_seconds")
            .and_then(serde_json::Value::as_i64)
        {
            Some(secs) => secs <= SHORT_WINDOW_MAX_SECONDS,
            None => key == "primary_window",
        };
        let kind = if is_short {
            WindowKind::FiveHour
        } else {
            WindowKind::Weekly
        };
        // used_percent が読めない window は、すでに埋まったスロットを壊さずに読み飛ばす。
        // 無条件代入だと、後続 window の None が先に解析済みの window を消してしまう。
        let Some(window) = parse_window(w, kind) else {
            continue;
        };
        if is_short {
            short = Some(window);
        } else {
            long = Some(window);
        }
    }
    (short, long)
}

fn parse_window(w: &serde_json::Value, kind: WindowKind) -> Option<Window> {
    let used = w.get("used_percent").and_then(serde_json::Value::as_f64)?;
    let resets_at = w
        .get("reset_at")
        .and_then(serde_json::Value::as_i64)
        .and_then(|e| Utc.timestamp_opt(e, 0).single());
    Some(Window {
        kind,
        used_percent: used,
        resets_at,
    })
}

/// access token の JWT claims から `chatgpt_account_id` を取り出す。
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

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde_json::json;

    fn put(c: &mut HashMap<String, String>, k: &str, v: &str) {
        c.insert(k.to_string(), v.to_string());
    }

    #[test]
    fn cookie_header_uses_unsuffixed_when_split_absent() {
        // 小さいトークンは分割されず、そのままヘッダに乗る。
        let mut c = HashMap::new();
        put(&mut c, SESSION_TOKEN, "short");
        let h = cookie_header(&c).unwrap();
        assert!(h.starts_with(&format!("{SESSION_TOKEN}=short")));
    }

    #[test]
    fn cookie_header_combines_split_tokens() {
        // 大きいトークンは .0/.1 に分割されるので両方を結合する。
        let mut c = HashMap::new();
        put(&mut c, &format!("{SESSION_TOKEN}.0"), "head");
        put(&mut c, &format!("{SESSION_TOKEN}.1"), "tail");
        let h = cookie_header(&c).unwrap();
        assert!(h.contains(&format!("{SESSION_TOKEN}.0=head")));
        assert!(h.contains(&format!("{SESSION_TOKEN}.1=tail")));
    }

    #[test]
    fn cookie_header_combines_three_or_more_chunks() {
        // 3 分割以上に対応(Next.js は chunk 数に上限なし)。
        // .0/.1/.2 すべてが結合されること、連番途切れの後ろの番号は無視されることを確認。
        let mut c = HashMap::new();
        put(&mut c, &format!("{SESSION_TOKEN}.0"), "a");
        put(&mut c, &format!("{SESSION_TOKEN}.1"), "b");
        put(&mut c, &format!("{SESSION_TOKEN}.2"), "c");
        // 連番が一度切れた後の chunk は採用しない(現実には起きない異常データの防御)。
        put(&mut c, &format!("{SESSION_TOKEN}.4"), "skip");
        let h = cookie_header(&c).unwrap();
        assert!(h.contains(&format!("{SESSION_TOKEN}.0=a")));
        assert!(h.contains(&format!("{SESSION_TOKEN}.1=b")));
        assert!(h.contains(&format!("{SESSION_TOKEN}.2=c")));
        assert!(!h.contains("skip"));
    }

    #[test]
    fn cookie_header_attaches_cf_and_puid() {
        let mut c = HashMap::new();
        put(&mut c, SESSION_TOKEN, "x");
        put(&mut c, "cf_clearance", "cf");
        put(&mut c, "__cf_bm", "bm");
        put(&mut c, "_puid", "p");
        let h = cookie_header(&c).unwrap();
        assert!(h.contains("; cf_clearance=cf"));
        assert!(h.contains("; __cf_bm=bm"));
        assert!(h.contains("; _puid=p"));
    }

    #[test]
    fn cookie_header_errors_without_session() {
        let c = HashMap::<String, String>::new();
        assert!(cookie_header(&c).is_err());
    }

    #[test]
    fn has_session_recognizes_both_forms() {
        // split form / 単一 form どちらでも検出する。
        let mut c = HashMap::new();
        assert!(!has_session(&c));
        put(&mut c, &format!("{SESSION_TOKEN}.0"), "x");
        assert!(has_session(&c));
        let mut c2 = HashMap::new();
        put(&mut c2, SESSION_TOKEN, "x");
        assert!(has_session(&c2));
    }

    #[test]
    fn parse_window_reads_used_percent_and_reset() {
        let v = json!({"used_percent": 23.5, "reset_at": 1_700_000_000_i64});
        let w = parse_window(&v, WindowKind::Weekly).unwrap();
        assert_eq!(w.kind, WindowKind::Weekly);
        assert_eq!(w.used_percent, 23.5);
        assert!(w.resets_at.is_some());
    }

    #[test]
    fn parse_window_missing_percent_returns_none() {
        assert!(parse_window(&json!({}), WindowKind::Weekly).is_none());
    }

    #[test]
    fn classify_windows_splits_by_duration_not_position() {
        // primary が週次・secondary が 5 時間、という順序で返っても duration で振り分ける。
        // 位置で決めると、応答の順序が変わっただけで 5h と 1w が入れ替わって表示される。
        let rate = json!({
            "primary_window": {"limit_window_seconds": 604_800, "used_percent": 12.0},
            "secondary_window": {"limit_window_seconds": 18_000, "used_percent": 34.0},
        });
        let (short, long) = classify_windows(Some(&rate));
        let short = short.expect("5 時間枠が短期スロットに入る");
        assert_eq!(short.kind, WindowKind::FiveHour);
        assert_eq!(short.used_percent, 34.0);
        let long = long.expect("週次枠が長期スロットに入る");
        assert_eq!(long.kind, WindowKind::Weekly);
        assert_eq!(long.used_percent, 12.0);
    }

    #[test]
    fn classify_windows_boundary_is_eight_hours_inclusive() {
        // 8 時間ちょうどは短期、1 秒でも超えれば長期。境界値そのものを固定したいので、
        // 実装側の定数ではなくリテラルで書く(定数を動かしたらこのテストが落ちる)。
        let boundary = json!({
            "primary_window": {"limit_window_seconds": 28_800, "used_percent": 1.0},
            "secondary_window": {"limit_window_seconds": 28_801, "used_percent": 2.0},
        });
        let (short, long) = classify_windows(Some(&boundary));
        assert_eq!(short.unwrap().kind, WindowKind::FiveHour);
        assert_eq!(long.unwrap().kind, WindowKind::Weekly);
    }

    #[test]
    fn classify_windows_skips_missing_and_null_entries() {
        // rate_limit 自体が無い / null の window は、そのスロットを空のままにする。
        let (short, long) = classify_windows(None);
        assert!(short.is_none() && long.is_none());

        let partial = json!({
            "primary_window": {"limit_window_seconds": 18_000, "used_percent": 7.0},
            "secondary_window": null,
        });
        let (short, long) = classify_windows(Some(&partial));
        assert_eq!(short.unwrap().used_percent, 7.0);
        assert!(long.is_none(), "null の window は長期スロットを埋めない");

        // used_percent が読めない window はスロットを埋めない。
        let unusable = json!({"primary_window": {"limit_window_seconds": 604_800}});
        let (short, long) = classify_windows(Some(&unusable));
        assert!(short.is_none() && long.is_none());
    }

    #[test]
    fn classify_windows_keeps_a_parsed_window_when_the_other_is_unusable() {
        // used_percent を持たない window が、すでに埋まったスロットを上書きして消さない。
        // 無条件代入だと 5h の 7.0% が secondary の None で消え、行ごと空になる。
        let rate = json!({
            "primary_window": {"limit_window_seconds": 18_000, "used_percent": 7.0},
            "secondary_window": {},
        });
        let (short, long) = classify_windows(Some(&rate));
        assert_eq!(short.expect("5h 枠が残る").used_percent, 7.0);
        assert!(long.is_none());

        // 順序が逆(先に長期が埋まる)でも同じ。
        let reversed = json!({
            "primary_window": {"limit_window_seconds": 604_800, "used_percent": 61.0},
            "secondary_window": {"limit_window_seconds": 18_000},
        });
        let (short, long) = classify_windows(Some(&reversed));
        assert!(short.is_none());
        assert_eq!(long.expect("週次枠が残る").used_percent, 61.0);
    }

    #[test]
    fn classify_windows_falls_back_to_position_when_duration_is_missing() {
        // limit_window_seconds が欠落 / null のとき、0 秒(=短期)に倒さず位置で振り分ける。
        // 0 に倒すと週次の使用率が 5h ラベルで表示され、長期スロットが空になる。
        let rate = json!({
            "primary_window": {"limit_window_seconds": 18_000, "used_percent": 7.0},
            "secondary_window": {"limit_window_seconds": null, "used_percent": 61.0},
        });
        let (short, long) = classify_windows(Some(&rate));
        let short = short.expect("5h 枠は短期スロットに残る");
        assert_eq!(short.kind, WindowKind::FiveHour);
        assert_eq!(short.used_percent, 7.0);
        let long = long.expect("duration 不明の secondary は長期スロットへ");
        assert_eq!(long.kind, WindowKind::Weekly);
        assert_eq!(long.used_percent, 61.0);

        // フィールドごと無い場合も同じ扱い。
        let absent = json!({
            "primary_window": {"used_percent": 7.0},
            "secondary_window": {"used_percent": 61.0},
        });
        let (short, long) = classify_windows(Some(&absent));
        assert_eq!(short.expect("primary は短期").used_percent, 7.0);
        assert_eq!(long.expect("secondary は長期").used_percent, 61.0);
    }

    #[test]
    fn jwt_account_id_extracts_from_claims() {
        // ペイロード JSON を URL-safe base64 で組み立てて JWT を再現する。
        let claims = json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "acc-123"}
        });
        let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let jwt = format!("hdr.{body}.sig");
        assert_eq!(jwt_account_id(&jwt).as_deref(), Some("acc-123"));
    }

    #[test]
    fn jwt_account_id_returns_none_for_malformed() {
        assert_eq!(jwt_account_id(""), None);
        assert_eq!(jwt_account_id("only_one_segment"), None);
        assert_eq!(jwt_account_id("a.@@@.c"), None);
    }
}
