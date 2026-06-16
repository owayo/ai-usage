//! Rendering: human table, JSON, and the compact colored statusline.

use chrono::{DateTime, Duration, Local, Utc};
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Attribute, Cell, Color, ContentArrangement, Table};

use crate::model::{AccountReport, Provider, Window};
use crate::report::{AccountOut, Report, WindowOut};

/// Account label shown to the user: the configured label if set, else the
/// username part of the provider account email (e.g. `work@example.com` → `work`),
/// falling back to the Chrome profile email's username, then the profile name.
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

/// How the highlighted ("active") account is specified. The default path
/// resolves a signed-in email (from `.claude.json`); callers that drive a
/// specific account instead pin it by profile name (+ optional provider),
/// independent of which account the host tool is currently signed in as.
pub struct ActiveTarget {
    pub email: Option<String>,
    pub profile: Option<String>,
    pub provider: Option<Provider>,
}

/// Decide whether a row is the active one, returning `(matched, reason)`. The
/// reason explains a non-match for `--debug`. Profile targeting takes priority
/// and can address any provider; email targeting keeps the original behaviour
/// of highlighting only the matching Claude row.
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

/// Emit one JSONL diagnostic line per row to stderr for `--debug`. stdout is
/// reserved for rendered output, so this never corrupts a piped statusline/JSON.
/// Only non-secret fields (provider/profile/email/decision) are logged.
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

/// Brand RGB per provider — the single source for both the table (comfy-table
/// `Color::Rgb`) and the statusline (`brand_sgr`'s ANSI truecolor).
fn brand_rgb(p: Provider) -> (u8, u8, u8) {
    match p {
        Provider::Claude => (217, 119, 87), // Anthropic coral #D97757
        Provider::Codex => (16, 163, 127),  // OpenAI teal #10A37F
        Provider::Antigravity => (66, 133, 244), // Google blue #4285F4
    }
}

/// Brand color for a provider, as a comfy-table truecolor (table use).
fn provider_color(p: Provider) -> Color {
    let (r, g, b) = brand_rgb(p);
    Color::Rgb { r, g, b }
}

/// Brand color as an ANSI SGR truecolor parameter (statusline use).
fn brand_sgr(p: Provider) -> String {
    let (r, g, b) = brand_rgb(p);
    format!("38;2;{r};{g};{b}")
}

/// Service-column text: the provider, plus the model-group for Antigravity rows.
fn service_label(p: Provider, group: Option<&str>) -> String {
    match group {
        Some(g) => format!("{} · {}", p.label(), g),
        None => p.label().to_string(),
    }
}

// ===== Human-readable table =================================================

pub fn table(reports: &[AccountReport], active: Option<&ActiveTarget>, debug: bool) {
    let now = Utc::now();
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            "Account",
            "Service",
            "Plan",
            "5-hour",
            "Weekly (7-day)",
        ]);

    for r in reports {
        let row_email = match &r.usage {
            Ok(u) => u.email.as_deref(),
            Err(_) => None,
        }
        .or(r.profile_email.as_deref());
        let name = display_name(
            r.label.as_deref(),
            row_email,
            r.profile_email.as_deref(),
            &r.profile_name,
        );
        let (is_active, reason) = match active {
            Some(t) => is_active_row(t, r.provider, &r.profile_name, row_email),
            None => (false, "no_active_target"),
        };
        if debug {
            debug_row(r.provider, &r.profile_name, row_email, is_active, reason);
        }
        let mut name_cell = Cell::new(&name);
        if is_active {
            name_cell = name_cell.fg(Color::Red).add_attribute(Attribute::Bold);
        }

        match &r.usage {
            Ok(u) => {
                let (c5, col5) = window_cell(&u.five_hour, now);
                let (cw, colw) = window_cell(&u.weekly, now);
                let mut cell5 = Cell::new(c5);
                if let Some(c) = col5 {
                    cell5 = cell5.fg(c);
                }
                let mut cellw = Cell::new(cw);
                if let Some(c) = colw {
                    cellw = cellw.fg(c);
                }
                table.add_row(vec![
                    name_cell,
                    Cell::new(service_label(r.provider, r.group_label.as_deref()))
                        .fg(provider_color(r.provider)),
                    Cell::new(u.plan.as_deref().unwrap_or("—")),
                    cell5,
                    cellw,
                ]);
            }
            Err(e) => {
                let msg: String = format!("{e:#}").chars().take(150).collect();
                table.add_row(vec![
                    name_cell,
                    Cell::new(service_label(r.provider, r.group_label.as_deref()))
                        .fg(provider_color(r.provider)),
                    Cell::new("—"),
                    Cell::new(format!("⚠ {msg}")).fg(Color::DarkGrey),
                    Cell::new(""),
                ]);
            }
        }
    }

    println!("{table}");
    println!(
        "  updated {} · bars = usage, time = until reset",
        now.with_timezone(&Local).format("%H:%M")
    );
}

fn window_cell(w: &Option<Window>, now: DateTime<Utc>) -> (String, Option<Color>) {
    match w {
        None => ("—".to_string(), None),
        Some(w) => {
            let reset = w
                .resets_at
                .map(|r| humanize(r - now))
                .unwrap_or_else(|| "—".to_string());
            let text = format!(
                "{}  {:>3}%  · {}",
                tbar(w.used_percent),
                w.used_percent.round() as i64,
                reset
            );
            (text, Some(tlevel(w.used_percent)))
        }
    }
}

fn tbar(pct: f64) -> String {
    let filled = ((pct / 100.0) * 10.0).round().clamp(0.0, 10.0) as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled))
}

fn tlevel(pct: f64) -> Color {
    if pct >= 85.0 {
        Color::Red
    } else if pct >= 60.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn humanize(d: Duration) -> String {
    let s = d.num_seconds();
    if s <= 0 {
        return "now".to_string();
    }
    let (days, hours, mins) = (s / 86400, (s % 86400) / 3600, (s % 3600) / 60);
    if days > 0 {
        format!("in {days}d {hours}h")
    } else if hours > 0 {
        format!("in {hours}h {mins}m")
    } else {
        format!("in {mins}m")
    }
}

// ===== JSON =================================================================

pub fn json(report: &Report) {
    println!("{}", serde_json::to_string_pretty(report).unwrap());
}

// ===== Statusline (compact, colored, one account per line) ==================

// 256-color ANSI codes (parameter portion, no SGR wrapper).
const CODEX_LOGO_COLOR: &str = "38;2;255;255;255"; // white — Codex logo glyph
// Brand-logo glyphs in PUA-B (BrandLogos font), used with `--logos`.
const CLAUDE_LOGO: &str = "\u{100002}"; // Claude sunburst
const CODEX_LOGO: &str = "\u{100000}"; // OpenAI mark
const ANTIGRAVITY_LOGO: &str = "\u{100003}"; // Antigravity mark
const GRAY: &str = "38;5;245";
const DIM: &str = "38;5;242";
const GREEN: &str = "38;5;35";
const BOLD_RED: &str = "1;38;5;196"; // active account
const FIVE_H_TH: [i64; 3] = [3600, 7200, 10800];
const WEEK_TH: [i64; 3] = [86400, 172800, 259200];

/// Render one row per account (grouped Claude-then-Codex), each with 5h and 1w
/// gauges + percentage + reset countdown. The account matching `active_email`
/// (this session's account) is shown in red+bold.
pub fn statusline(
    report: &Report,
    active: Option<&ActiveTarget>,
    color: bool,
    logos: bool,
    debug: bool,
    compact: bool,
    reset_at: bool,
) {
    let now = Utc::now();
    let mut rows: Vec<&AccountOut> = report.accounts.iter().collect();
    rows.sort_by(|a, b| {
        a.provider
            .rank()
            .cmp(&b.provider.rank())
            .then_with(|| a.profile.cmp(&b.profile))
    });
    let lines: Vec<String> = rows
        .iter()
        .map(|a| {
            let row_email = a.email.as_deref().or(a.profile_email.as_deref());
            // Profile targeting can highlight any provider's row; email targeting
            // keeps the original Claude-only behaviour. `--debug` explains each row.
            let (is_active, reason) = match active {
                Some(t) => is_active_row(t, a.provider, &a.profile, row_email),
                None => (false, "no_active_target"),
            };
            if debug {
                debug_row(a.provider, &a.profile, row_email, is_active, reason);
            }
            render_row(
                a, row_email, is_active, color, logos, now, compact, reset_at,
            )
        })
        .collect();
    print!("{}", lines.join("\n"));
}

#[allow(clippy::too_many_arguments)]
fn render_row(
    a: &AccountOut,
    row_email: Option<&str>,
    active: bool,
    color: bool,
    logos: bool,
    now: DateTime<Utc>,
    compact: bool,
    reset_at: bool,
) -> String {
    let prov = match a.provider {
        Provider::Claude => "Claude",
        Provider::Codex => "Codex",
        Provider::Antigravity => "AGY",
    };
    let name = display_name(
        a.label.as_deref(),
        row_email,
        a.profile_email.as_deref(),
        &a.profile,
    );
    let mut s = String::from("  ");
    // Provider marker: a brand-logo glyph with `--logos`, otherwise the text label.
    if logos {
        let (logo, logo_color) = match a.provider {
            Provider::Claude => (CLAUDE_LOGO, brand_sgr(a.provider)),
            // Codex's mark reads better in white than its teal brand color.
            Provider::Codex => (CODEX_LOGO, CODEX_LOGO_COLOR.to_string()),
            Provider::Antigravity => (ANTIGRAVITY_LOGO, brand_sgr(a.provider)),
        };
        s += &paint(color, &logo_color, &format!("{logo}  "));
    } else {
        s += &paint(color, &brand_sgr(a.provider), &format!("{prov:<6} "));
    }
    // Antigravity rows show their model-group (the account name is redundant for
    // a single token); others show the account name. Pad to a width that fits
    // "Claude&GPT" so every row's gauges line up.
    let display = a.group_label.as_deref().unwrap_or(&name);
    s += &paint(
        color,
        if active { BOLD_RED } else { GRAY },
        &format!("{display:<11}"),
    );
    if !a.ok {
        // データ取得に失敗したアカウントも、データ有り行と桁位置を揃える。
        // window_seg の None 分岐(空ゲージ + "--")を 5h / 1w 双方で再利用する。
        // 5h には reset_at を伝搬しない(1w 限定のため false 固定)。
        s += &window_seg(color, "5h", None, now, FIVE_H_TH, compact, false);
        s += "   ";
        s += &window_seg(color, "1w", None, now, WEEK_TH, compact, reset_at);
        return s;
    }
    s += &window_seg(
        color,
        "5h",
        a.five_hour.as_ref(),
        now,
        FIVE_H_TH,
        compact,
        false,
    );
    s += "   ";
    s += &window_seg(
        color,
        "1w",
        a.weekly.as_ref(),
        now,
        WEEK_TH,
        compact,
        reset_at,
    );
    s
}

fn window_seg(
    color: bool,
    label: &str,
    w: Option<&WindowOut>,
    now: DateTime<Utc>,
    th: [i64; 3],
    compact: bool,
    show_reset_at: bool,
) -> String {
    // --compact 時はゲージ幅を半分(8)にする。空ゲージ・実ゲージとも同じ幅で揃える。
    let gauge_width = if compact { 8 } else { 16 };
    let mut s = paint(color, GRAY, &format!("{label} "));
    match w {
        None => {
            // データ無し: 空ゲージ + "--"(% なし) + "--"(残り時間)。
            // Some 分岐と同じ桁幅(gauge_width + 1 + 4 + 2 + 5)に揃える。
            s += &paint(color, DIM, &"░".repeat(gauge_width));
            s += " ";
            s += &paint(color, DIM, &format!("{:>4}", "--"));
            s += "  ";
            s += &paint(color, DIM, &format!("{:<5}", "--"));
        }
        Some(w) => {
            s += &gauge(color, w.used_percent, gauge_width);
            s += " ";
            s += &paint(
                color,
                pct_code(w.used_percent),
                &format!("{:>3}%", w.used_percent.round() as i64),
            );
            s += "  ";
            let reset = w.resets_at.as_deref().and_then(parse_utc);
            let rem = reset.map(|r| (r - now).num_seconds());
            match rem {
                Some(sec) if sec > 0 => {
                    s += &paint(
                        color,
                        reset_code(sec, th),
                        &format!("{:<5}", compact_dur(sec)),
                    );
                    // --reset-at: 1w 行の残り時間の後ろに (MM/DD HH:MM) をローカル時刻で併記。
                    // 5h 側は呼び出し元で show_reset_at=false 固定。
                    if show_reset_at && let Some(r) = reset {
                        s += &paint(
                            color,
                            DIM,
                            &format!(" ({})", r.with_timezone(&Local).format("%m/%d %H:%M")),
                        );
                    }
                }
                Some(_) => s += &paint(color, GREEN, &format!("{:<5}", "now")),
                None => s += &paint(color, DIM, &format!("{:<5}", "--")),
            }
        }
    }
    s
}

fn paint(color: bool, code: &str, s: &str) -> String {
    if color && !code.is_empty() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

fn gauge(color: bool, pct: f64, width: usize) -> String {
    let mut filled = ((pct / 100.0) * width as f64).round() as i64;
    if pct > 0.0 && filled < 1 {
        filled = 1;
    }
    let filled = filled.clamp(0, width as i64) as usize;
    let mut s = paint(color, level_code(pct), &"█".repeat(filled));
    s += &paint(color, DIM, &"░".repeat(width - filled));
    s
}

fn level_code(pct: f64) -> &'static str {
    if pct >= 90.0 {
        "38;5;196"
    } else if pct >= 80.0 {
        "38;5;208"
    } else if pct >= 60.0 {
        "38;5;178"
    } else {
        "38;5;35"
    }
}

fn pct_code(pct: f64) -> &'static str {
    if pct >= 90.0 {
        "1;38;5;196"
    } else if pct >= 80.0 {
        "1;38;5;208"
    } else if pct >= 60.0 {
        "38;5;178"
    } else {
        "38;5;35"
    }
}

fn reset_code(sec: i64, th: [i64; 3]) -> &'static str {
    if sec < th[0] {
        "38;5;196"
    } else if sec < th[1] {
        "38;5;208"
    } else if sec < th[2] {
        "38;5;178"
    } else {
        "38;5;35"
    }
}

fn compact_dur(sec: i64) -> String {
    // Round up so 59s left shows as 1m (not 0m). Zero-pad the lower unit so digit
    // positions stay aligned (3h07m / 4d03h). window_seg pads the result to width 5.
    let minutes = (sec + 59) / 60;
    if minutes < 60 {
        format!("{minutes}m")
    } else {
        let hours = minutes / 60;
        let mins = minutes % 60;
        if hours < 24 {
            format!("{hours}h{mins:02}m")
        } else {
            format!("{}d{:02}h", hours / 24, hours % 24)
        }
    }
}

fn parse_utc(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
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
    fn compact_dur_units() {
        // 単位の境界(分→時間→日)を確認。1分未満は1m に丸める。
        assert_eq!(compact_dur(0), "0m");
        assert_eq!(compact_dur(1), "1m");
        assert_eq!(compact_dur(59), "1m");
        assert_eq!(compact_dur(60), "1m");
        assert_eq!(compact_dur(61), "2m");
        assert_eq!(compact_dur(59 * 60), "59m");
        assert_eq!(compact_dur(60 * 60), "1h00m");
        assert_eq!(compact_dur(3 * 3600 + 7 * 60), "3h07m");
        assert_eq!(compact_dur(24 * 3600), "1d00h");
        assert_eq!(compact_dur(4 * 86400 + 3 * 3600), "4d03h");
    }

    #[test]
    fn humanize_rounds_appropriately() {
        // 0以下 は "now"、それ以上は単位ごとに丸める。
        assert_eq!(humanize(Duration::seconds(0)), "now");
        assert_eq!(humanize(Duration::seconds(-100)), "now");
        assert_eq!(humanize(Duration::minutes(5)), "in 5m");
        assert_eq!(humanize(Duration::minutes(125)), "in 2h 5m");
        assert_eq!(humanize(Duration::hours(25)), "in 1d 1h");
    }

    #[test]
    fn tbar_clamps_extremes() {
        // 0% は全 ░、100% は全 █、超過/欠損もパニックせず clamp される。
        assert_eq!(tbar(0.0), "░".repeat(10));
        assert_eq!(tbar(100.0), "█".repeat(10));
        assert_eq!(tbar(150.0), "█".repeat(10));
        assert_eq!(tbar(-10.0), "░".repeat(10));
    }

    #[test]
    fn parse_utc_accepts_rfc3339() {
        assert!(parse_utc("2026-06-15T06:28:32Z").is_some());
        assert!(parse_utc("2026-06-12T15:06:32.244+09:00").is_some());
        assert!(parse_utc("not a date").is_none());
    }

    #[test]
    fn service_label_appends_group() {
        assert_eq!(service_label(Provider::Claude, None), "Claude");
        assert_eq!(
            service_label(Provider::Antigravity, Some("Gemini")),
            "Antigravity · Gemini"
        );
    }

    fn fixed_utc(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn window_seg_appends_reset_at_only_when_enabled() {
        // 未来のリセット時刻 + show_reset_at=true → 末尾に "(MM/DD HH:MM)" が付く。
        // ローカル TZ 依存の具体値は検証せず、括弧の有無で機能を確認。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let w = WindowOut {
            used_percent: 54.0,
            resets_at: Some("2026-06-17T16:10:00Z".to_string()),
            resets_in_seconds: Some(2 * 86400),
        };
        let with_date = window_seg(false, "1w", Some(&w), now, WEEK_TH, false, true);
        assert!(
            with_date.contains('(') && with_date.ends_with(')'),
            "expected date suffix in {with_date:?}"
        );
        let without = window_seg(false, "1w", Some(&w), now, WEEK_TH, false, false);
        assert!(
            !without.contains('('),
            "did not expect date suffix in {without:?}"
        );
    }

    #[test]
    fn window_seg_reset_at_skips_when_no_window_or_expired() {
        // データ無し or 既にリセット済み(now 表示)では、show_reset_at=true でも日時は出さない。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let none_out = window_seg(false, "1w", None, now, WEEK_TH, false, true);
        assert!(
            !none_out.contains('('),
            "no date for None window: {none_out:?}"
        );

        let expired = WindowOut {
            used_percent: 100.0,
            resets_at: Some("2026-06-10T00:00:00Z".to_string()),
            resets_in_seconds: Some(0),
        };
        let expired_out = window_seg(false, "1w", Some(&expired), now, WEEK_TH, false, true);
        assert!(
            !expired_out.contains('('),
            "no date when already reset: {expired_out:?}"
        );
        assert!(expired_out.contains("now"));
    }
}
