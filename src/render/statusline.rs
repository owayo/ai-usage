//! compact / colored statusline 出力(1 account 1 行)。

use chrono::{DateTime, Local, Utc};

use super::sort::{sorted_refs, statusline_default_cmp};
use super::{ActiveTarget, brand_rgb, display_name, parse_utc, resolve_active};
use crate::SortKey;
use crate::model::Provider;
use crate::report::{AccountOut, Report, WindowOut};

// 256-color ANSI code(SGR wrapper なしの parameter 部分)。
const CODEX_LOGO_COLOR: &str = "38;2;255;255;255"; // white。Codex logo glyph 用。
// BrandLogos font の PUA-B glyph。`--logos` で使う。
const CLAUDE_LOGO: &str = "\u{100002}"; // Claude sunburst。
const CODEX_LOGO: &str = "\u{100000}"; // OpenAI mark。
const ANTIGRAVITY_LOGO: &str = "\u{100003}"; // Antigravity mark。
const GRAY: &str = "38;5;245";
const DIM: &str = "38;5;242";
const GREEN: &str = "38;5;35";
const BOLD_RED: &str = "1;38;5;196"; // active account
const FIVE_H_TH: [i64; 3] = [3600, 7200, 10800];
const WEEK_TH: [i64; 3] = [86400, 172800, 259200];

/// brand color を ANSI SGR truecolor parameter として返す(statusline 用)。
fn brand_sgr(p: Provider) -> String {
    let (r, g, b) = brand_rgb(p);
    format!("38;2;{r};{g};{b}")
}

/// statusline レンダリングの表示オプション。CLI flag 群を 1 つに束ね、
/// レンダラー間の引数爆発(clippy::too_many_arguments)を避ける。
pub struct StatuslineOpts {
    pub color: bool,
    pub logos: bool,
    pub debug: bool,
    pub compact: bool,
    pub reset_at: bool,
}

/// account ごとに 1 行を render する(Claude → Codex の group 順)。
/// 各行は 5h / 1w の gauge、percentage、reset countdown を持つ。
/// `active_email`(この session の account)に一致する行は赤 bold で表示する。
pub fn statusline(
    report: &Report,
    active: Option<&ActiveTarget>,
    sort: SortKey,
    opts: &StatuslineOpts,
) {
    let now = Utc::now();
    // SortKey::Provider のときは従来どおり provider.rank() → profile 名で並べる。
    // weekly-usage / weekly-reset のときは sorted_refs 側のロジックで上書き。
    let rows = sorted_refs(&report.accounts, sort, now, Some(statusline_default_cmp));
    let lines: Vec<String> = rows
        .iter()
        .map(|a| {
            let row_email = a.email.as_deref().or(a.profile_email.as_deref());
            // profile targeting は任意 provider 行を highlight できる。
            // email targeting は従来の Claude-only 挙動を保つ。`--debug` で各行の理由を出す。
            let is_active = resolve_active(active, a.provider, &a.profile, row_email, opts.debug);
            render_row(a, row_email, is_active, opts, now)
        })
        .collect();
    print!("{}", lines.join("\n"));
}

fn render_row(
    a: &AccountOut,
    row_email: Option<&str>,
    active: bool,
    opts: &StatuslineOpts,
    now: DateTime<Utc>,
) -> String {
    let prov = match a.provider {
        Provider::Claude => "Claude",
        Provider::Codex => "Codex",
        Provider::Antigravity => "AGY",
        Provider::PixelLab => "Pixel",
    };
    let name = display_name(
        a.label.as_deref(),
        row_email,
        a.profile_email.as_deref(),
        &a.profile,
    );
    let mut s = String::from("  ");
    // provider marker は `--logos` なら brand-logo glyph、そうでなければ text label。
    // PixelLab は BrandLogos font に glyph が無いため logos モードでも text にフォールバック。
    if opts.logos {
        let logo_form = match a.provider {
            Provider::Claude => Some((CLAUDE_LOGO, brand_sgr(a.provider))),
            // Codex mark は teal の brand color より white の方が読みやすい。
            Provider::Codex => Some((CODEX_LOGO, CODEX_LOGO_COLOR.to_string())),
            Provider::Antigravity => Some((ANTIGRAVITY_LOGO, brand_sgr(a.provider))),
            Provider::PixelLab => None,
        };
        if let Some((logo, logo_color)) = logo_form {
            s += &paint(opts.color, &logo_color, &format!("{logo}  "));
        } else {
            s += &paint(opts.color, &brand_sgr(a.provider), &format!("{prov:<6} "));
        }
    } else {
        s += &paint(opts.color, &brand_sgr(a.provider), &format!("{prov:<6} "));
    }
    // Antigravity 行は単一 token で account name が冗長なため model-group を表示する。
    // それ以外は account name を表示する。"Claude&GPT" が入る幅で pad し、全行の gauge を揃える。
    let display = a.group_label.as_deref().unwrap_or(&name);
    s += &paint(
        opts.color,
        if active { BOLD_RED } else { GRAY },
        &format!("{display:<11}"),
    );
    if !a.ok {
        // データ取得に失敗したアカウントも、データ有り行と桁位置を揃える。
        // window_seg の None 分岐(空ゲージ + "--")を 5h / 1w 双方で再利用する。
        // 5h には reset_at を伝搬しない(1w 限定のため false 固定)。
        s += &window_seg(opts, "5h", None, now, FIVE_H_TH, false);
        s += "   ";
        s += &window_seg(opts, "1w", None, now, WEEK_TH, opts.reset_at);
        return s;
    }
    s += &window_seg(opts, "5h", a.five_hour.as_ref(), now, FIVE_H_TH, false);
    s += "   ";
    s += &window_seg(opts, "1w", a.weekly.as_ref(), now, WEEK_TH, opts.reset_at);
    s
}

fn window_seg(
    opts: &StatuslineOpts,
    label: &str,
    w: Option<&WindowOut>,
    now: DateTime<Utc>,
    th: [i64; 3],
    show_reset_at: bool,
) -> String {
    // --compact 時は gauge 幅を半分(8)にする。空 gauge / 実 gauge とも同じ幅で揃える。
    let gauge_width = if opts.compact { 8 } else { 16 };
    let mut s = paint(opts.color, GRAY, &format!("{label} "));
    match w {
        None => {
            // データ無し: 空 gauge + "--"(% なし) + "--"(残り時間)。
            // Some 分岐と同じ桁幅(gauge_width + 1 + 4 + 2 + 5)に揃える。
            s += &paint(opts.color, DIM, &"░".repeat(gauge_width));
            s += " ";
            s += &paint(opts.color, DIM, &format!("{:>4}", "--"));
            s += "  ";
            s += &paint(opts.color, DIM, &format!("{:<6}", "--"));
        }
        Some(w) => {
            s += &gauge(opts.color, w.used_percent, gauge_width);
            s += " ";
            s += &paint(
                opts.color,
                pct_code(w.used_percent),
                &format!("{:>3}%", w.used_percent.round() as i64),
            );
            s += "  ";
            let reset = w.resets_at.as_deref().and_then(parse_utc);
            let rem = reset.map(|r| (r - now).num_seconds());
            match rem {
                Some(sec) if sec > 0 => {
                    s += &paint(
                        opts.color,
                        reset_code(sec, th),
                        &format!("{:<6}", compact_dur(sec)),
                    );
                    // --reset-at: 1w 行の残り時間の後ろに (MM/DD HH:MM) を local 時刻で併記。
                    // 5h 側は呼び出し元で show_reset_at=false 固定。
                    if show_reset_at && let Some(r) = reset {
                        s += &paint(
                            opts.color,
                            DIM,
                            &format!(" ({})", r.with_timezone(&Local).format("%m/%d %H:%M")),
                        );
                    }
                }
                Some(_) => s += &paint(opts.color, GREEN, &format!("{:<6}", "now")),
                None => s += &paint(opts.color, DIM, &format!("{:<6}", "--")),
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
    // 59s left は 0m ではなく 1m と表示するため切り上げる。下位 unit は zero-pad し、
    // digit position を揃える(3h07m / 4d03h)。window_seg は結果を幅 6 に pad する。
    // 1w window の `XXhYYm`(例: `12h18m`)に十分な幅で、`--reset-at` の trailing
    // `(MM/DD HH:MM)` も行間で揃う。
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_utc(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    /// テスト用に、color/compact を無効化した最小構成の StatuslineOpts を作る。
    /// window_seg は color と compact しか参照しないため、他 flag は false 固定。
    fn plain_opts() -> StatuslineOpts {
        StatuslineOpts {
            color: false,
            logos: false,
            debug: false,
            compact: false,
            reset_at: false,
        }
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
    fn window_seg_appends_reset_at_only_when_enabled() {
        // 未来のリセット時刻 + show_reset_at=true → 末尾に "(MM/DD HH:MM)" が付く。
        // ローカル TZ 依存の具体値は検証せず、括弧の有無で機能を確認。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let opts = plain_opts();
        let w = WindowOut {
            used_percent: 54.0,
            resets_at: Some("2026-06-17T16:10:00Z".to_string()),
            resets_in_seconds: Some(2 * 86400),
        };
        let with_date = window_seg(&opts, "1w", Some(&w), now, WEEK_TH, true);
        assert!(
            with_date.contains('(') && with_date.ends_with(')'),
            "expected date suffix in {with_date:?}"
        );
        let without = window_seg(&opts, "1w", Some(&w), now, WEEK_TH, false);
        assert!(
            !without.contains('('),
            "did not expect date suffix in {without:?}"
        );
    }

    #[test]
    fn window_seg_reset_at_skips_when_no_window_or_expired() {
        // データ無し or 既にリセット済み(now 表示)では、show_reset_at=true でも日時は出さない。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let opts = plain_opts();
        let none_out = window_seg(&opts, "1w", None, now, WEEK_TH, true);
        assert!(
            !none_out.contains('('),
            "no date for None window: {none_out:?}"
        );

        let expired = WindowOut {
            used_percent: 100.0,
            resets_at: Some("2026-06-10T00:00:00Z".to_string()),
            resets_in_seconds: Some(0),
        };
        let expired_out = window_seg(&opts, "1w", Some(&expired), now, WEEK_TH, true);
        assert!(
            !expired_out.contains('('),
            "no date when already reset: {expired_out:?}"
        );
        assert!(expired_out.contains("now"));
    }
}
