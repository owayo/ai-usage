//! human table、JSON、compact colored statusline の rendering。
//!
//! table / statusline / 行ソートはサブモジュールに分かれる。このファイルは
//! レンダラー間で共有する行の解決(表示名・active 判定・brand color)と、
//! 15 行で足りる JSON 出力を持つ。

mod sort;
mod statusline;
mod table;

pub use statusline::{StatuslineOpts, statusline};
pub use table::table;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::SortKey;
use crate::model::Provider;
use crate::report::{AccountOut, Report};

/// user に表示する account label。config label があればそれを使い、なければ provider account
/// email の username 部(例: `work@example.com` → `work`)に fallback する。さらに Chrome
/// profile email の username、profile 名の順で fallback する。
fn display_name<'a>(
    label: Option<&'a str>,
    email: Option<&'a str>,
    profile_email: Option<&'a str>,
    profile: &'a str,
) -> String {
    if let Some(l) = label.filter(|s| !s.is_empty()) {
        return l.to_string();
    }
    email
        .or(profile_email)
        .and_then(|e| e.split('@').next())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| profile.to_string())
}

/// highlight する "active" account の指定方法。default path は `.claude.json` から
/// signed-in email を解決する。特定 account を操作する caller は、host tool が現在
/// signed-in している account と独立に、profile 名(+任意 provider)で pin できる。
pub struct ActiveTarget {
    pub email: Option<String>,
    pub profile: Option<String>,
    pub provider: Option<Provider>,
}

/// 行が active かどうかを判定し、`(matched, reason)` を返す。reason は `--debug` で
/// non-match の理由を説明する。profile targeting は優先度が高く任意 provider を指せる。
/// email targeting は従来どおり、一致する Claude 行だけを highlight する。
fn is_active_row(
    target: &ActiveTarget,
    provider: Provider,
    profile: &str,
    row_email: Option<&str>,
) -> (bool, &'static str) {
    if let Some(want) = target.profile.as_deref() {
        if !profile.eq_ignore_ascii_case(want) {
            return (false, "profile_mismatch");
        }
        return match target.provider {
            Some(pv) if provider != pv => (false, "provider_mismatch"),
            Some(_) => (true, "profile_provider_match"),
            None if provider == Provider::Claude => (true, "profile_match_claude"),
            None => (false, "provider_not_claude"),
        };
    }
    if let Some(want) = target.email.as_deref() {
        if provider != Provider::Claude {
            return (false, "provider_not_claude");
        }
        return match row_email {
            Some(got) if got.eq_ignore_ascii_case(want) => (true, "email_match"),
            Some(_) => (false, "email_mismatch"),
            None => (false, "no_row_email"),
        };
    }
    (false, "no_active_target")
}

/// `--debug` 用に、行ごとの診断情報を JSONL で stderr に出す。stdout は rendered output 専用の
/// ため、pipe された statusline/JSON を壊さない。secret ではない field
/// (provider/profile/email/decision)だけを log する。
fn debug_row(
    provider: Provider,
    profile: &str,
    row_email: Option<&str>,
    matched: bool,
    reason: &str,
) {
    eprintln!(
        "{}",
        serde_json::json!({
            "event": "row_match",
            "provider": provider.label(),
            "profile": profile,
            "row_email": row_email,
            "matched": matched,
            "reason": reason,
        })
    );
}

/// 行の active 判定と `--debug` 診断出力をまとめて行う。table / statusline の
/// 両レンダラーで同じ判定・同じ JSONL を出すための共有入口。
fn resolve_active(
    active: Option<&ActiveTarget>,
    provider: Provider,
    profile: &str,
    row_email: Option<&str>,
    debug: bool,
) -> bool {
    let (is_active, reason) = match active {
        Some(t) => is_active_row(t, provider, profile, row_email),
        None => (false, "no_active_target"),
    };
    if debug {
        debug_row(provider, profile, row_email, is_active, reason);
    }
    is_active
}

/// 長期(right)スロットの表示 label。統一 model は `Usage::weekly` を長期スロットとして
/// 使い回しているが、実際の reset サイクル(週次 / 月次)は provider ごとに違うため
/// render 時に解決する。table / statusline の両方で使う。
fn long_window_label(p: Provider) -> &'static str {
    match p {
        Provider::PixelLab => "1m",
        _ => "1w",
    }
}

/// provider ごとの brand RGB。table(comfy-table `Color::Rgb`)と statusline
/// (`brand_sgr` の ANSI truecolor)で共有する単一 source。
fn brand_rgb(p: Provider) -> (u8, u8, u8) {
    match p {
        Provider::Claude => (217, 119, 87), // Anthropic coral #D97757
        Provider::Codex => (16, 163, 127),  // OpenAI teal #10A37F
        Provider::Antigravity => (66, 133, 244), // Google blue #4285F4
        Provider::PixelLab => (234, 179, 8), // pixel-art amber #EAB308
    }
}

fn parse_utc(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

// ===== JSON =================================================================

/// Report を SortKey に従って並び替えてシリアライズする。元の `Report` を
/// clone せずに参照のみで並べ替えるため、`accounts` を `Vec<&AccountOut>` に
/// 差し替えた `OrderedReport` ラッパーを 1 回限りのシリアライズで使う。
pub fn json(report: &Report, sort: SortKey) {
    #[derive(Serialize)]
    struct OrderedReport<'a> {
        generated_at: &'a str,
        accounts: Vec<&'a AccountOut>,
    }
    let now = Utc::now();
    // JSON は table と同じく SortKey::Provider のときは入力順を維持する。
    let accounts = sort::sorted_refs(&report.accounts, sort, now, None);
    let out = OrderedReport {
        generated_at: &report.generated_at,
        accounts,
    };
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_uses_label_first() {
        // ラベルが空でなければラベルが最優先される。
        let n = display_name(Some("work"), Some("a@b.test"), Some("c@d.test"), "Work");
        assert_eq!(n, "work");
    }

    #[test]
    fn display_name_falls_back_to_email_username() {
        // ラベル無し → 行メール(プロバイダ側のメール) → username 部のみ。
        let n = display_name(None, Some("alice@example.com"), None, "Work");
        assert_eq!(n, "alice");
    }

    #[test]
    fn display_name_falls_back_to_profile_email() {
        // 行メール無し → プロファイルメール。
        let n = display_name(None, None, Some("bob@example.com"), "Work");
        assert_eq!(n, "bob");
    }

    #[test]
    fn display_name_falls_back_to_profile_name() {
        // どれも使えなければプロファイル名(@ 区切りで空になる場合も含む)。
        let n = display_name(None, Some("@example.com"), None, "Work");
        assert_eq!(n, "Work");
        let n = display_name(None, None, None, "Home");
        assert_eq!(n, "Home");
    }

    #[test]
    fn display_name_ignores_empty_label() {
        // 空文字ラベルは無視してフォールバックへ。
        let n = display_name(Some(""), Some("carol@example.com"), None, "Work");
        assert_eq!(n, "carol");
    }

    #[test]
    fn active_row_profile_match_targets_claude_by_default() {
        // プロファイル名一致 + provider 指定なし → Claude 行のみアクティブ。
        let t = ActiveTarget {
            email: None,
            profile: Some("Work".into()),
            provider: None,
        };
        let (m, r) = is_active_row(&t, Provider::Claude, "work", None);
        assert!(m);
        assert_eq!(r, "profile_match_claude");
        let (m, r) = is_active_row(&t, Provider::Codex, "work", None);
        assert!(!m);
        assert_eq!(r, "provider_not_claude");
    }

    #[test]
    fn active_row_profile_with_provider_pins_row() {
        // profile + provider 指定で対応する行のみアクティブ。
        let t = ActiveTarget {
            email: None,
            profile: Some("Work".into()),
            provider: Some(Provider::Codex),
        };
        assert_eq!(
            is_active_row(&t, Provider::Codex, "Work", None),
            (true, "profile_provider_match")
        );
        assert_eq!(
            is_active_row(&t, Provider::Claude, "Work", None),
            (false, "provider_mismatch")
        );
        assert_eq!(
            is_active_row(&t, Provider::Codex, "Home", None),
            (false, "profile_mismatch")
        );
    }

    #[test]
    fn active_row_email_targets_claude_only() {
        // email 指定は Claude 行のみマッチ対象。
        let t = ActiveTarget {
            email: Some("alice@example.com".into()),
            profile: None,
            provider: None,
        };
        assert_eq!(
            is_active_row(&t, Provider::Claude, "Work", Some("ALICE@example.com")),
            (true, "email_match")
        );
        assert_eq!(
            is_active_row(&t, Provider::Claude, "Work", Some("bob@example.com")),
            (false, "email_mismatch")
        );
        assert_eq!(
            is_active_row(&t, Provider::Claude, "Work", None),
            (false, "no_row_email")
        );
        assert_eq!(
            is_active_row(&t, Provider::Codex, "Work", Some("alice@example.com")),
            (false, "provider_not_claude")
        );
    }

    #[test]
    fn active_row_no_target() {
        let t = ActiveTarget {
            email: None,
            profile: None,
            provider: None,
        };
        assert_eq!(
            is_active_row(&t, Provider::Claude, "Work", None),
            (false, "no_active_target")
        );
    }

    #[test]
    fn parse_utc_accepts_rfc3339() {
        assert!(parse_utc("2026-06-15T06:28:32Z").is_some());
        assert!(parse_utc("2026-06-12T15:06:32.244+09:00").is_some());
        assert!(parse_utc("not a date").is_none());
    }

    #[test]
    fn long_window_label_by_provider() {
        // PixelLab は月次サイクル(1m)、それ以外は週次(1w)。
        assert_eq!(long_window_label(Provider::Claude), "1w");
        assert_eq!(long_window_label(Provider::Codex), "1w");
        assert_eq!(long_window_label(Provider::Antigravity), "1w");
        assert_eq!(long_window_label(Provider::PixelLab), "1m");
    }
}
