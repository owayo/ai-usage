//! compact / colored statusline 出力(1 account 1 行)。

use chrono::{DateTime, Local, Utc};
use unicode_width::UnicodeWidthChar;

use super::sort::{sorted_refs, statusline_default_cmp};
use super::{
    ActiveTarget, brand_rgb, display_name, legacy_long_window_label, parse_utc, preferred_email,
    resolve_active, window_label,
};
use crate::SortKey;
use crate::model::{Provider, WindowKind};
use crate::report::{AccountOut, Report, WindowOut};

// 256-color ANSI code(SGR wrapper なしの parameter 部分)。
// Codex / Grok の marker 色。Codex の teal brand color は暗背景で沈み、Grok(xAI)は
// 白黒基調ブランドのため、logo glyph・text label とも白で表示する。
const WHITE_MARKER_COLOR: &str = "38;2;255;255;255";
// BrandLogos font の PUA-B glyph。`--logos` で使う。
const CLAUDE_LOGO: &str = "\u{100002}"; // Claude sunburst。
const CODEX_LOGO: &str = "\u{100000}"; // OpenAI mark。
const ANTIGRAVITY_LOGO: &str = "\u{100003}"; // Antigravity mark。
const GROK_LOGO: &str = "\u{100004}"; // Grok (xAI) mark。
const PIXELLAB_LOGO: &str = "\u{100400}"; // PixelLab dragon。
const GRAY: &str = "38;5;245";
const DIM: &str = "38;5;242";
const GREEN: &str = "38;5;35";
const BOLD_RED: &str = "1;38;5;196"; // アクティブなアカウント。
/// account 名 / model-group 名の欄幅(末尾の区切り 1 桁を含む)。最長の想定値
/// "Claude&GPT"(10 桁)がちょうど収まり、その右に区切りが 1 桁残る。
const NAME_FIELD_WIDTH: usize = 11;
const FIVE_H_TH: [i64; 3] = [3600, 7200, 10800];
const DAILY_TH: [i64; 3] = [4 * 3600, 8 * 3600, 12 * 3600];
const WEEK_TH: [i64; 3] = [86400, 172800, 259200];
const MONTHLY_TH: [i64; 3] = [3 * 86400, 7 * 86400, 14 * 86400];

/// brand color を ANSI SGR truecolor parameter として返す(statusline 用)。
fn brand_sgr(p: Provider) -> String {
    let (r, g, b) = brand_rgb(p);
    format!("38;2;{r};{g};{b}")
}

/// provider marker(左端の logo glyph またはテキストラベル)に使う ANSI color。
/// Codex は brand teal よりも白の方が明るいターミナル背景でも視認しやすく、
/// Grok は xAI の白黒ブランドに合わせるため、logos モードと text モードの
/// どちらでも `WHITE_MARKER_COLOR` を使う。
fn marker_color(p: Provider) -> String {
    match p {
        Provider::Codex | Provider::Grok => WHITE_MARKER_COLOR.to_string(),
        _ => brand_sgr(p),
    }
}

/// statusline レンダリングの表示オプション。CLI flag 群を 1 つに束ね、
/// レンダラー間の引数爆発(clippy::too_many_arguments)を避ける。
pub struct StatuslineOpts {
    pub color: bool,
    pub logos: bool,
    pub debug: bool,
    pub compact: bool,
    pub reset_at: bool,
    /// statusline で非表示にする provider。fetch と `--json` / table には無関係。
    pub hide: Vec<Provider>,
}

/// account ごとに 1 行を render する(Claude → Codex の group 順)。
/// 各行は周期付きの短期 / 長期 gauge、percentage、reset countdown を持つ。
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
        // 表示前に hide list で除外する。fetch は変えないので `--json` cache と
        // 同居しても、`--input` から読んだ report をそのまま filter するだけで済む。
        .filter(|a| !opts.hide.contains(&a.provider))
        .map(|a| {
            let row_email = preferred_email(a.email.as_deref(), a.profile_email.as_deref());
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
    let mut rendered = render_identity(a, row_email, active, opts);
    rendered += &render_windows(a, opts, now);
    rendered
}

/// provider marker と表示名を、quota 部分に依存せず組み立てる。
fn render_identity(
    account: &AccountOut,
    row_email: Option<&str>,
    active: bool,
    opts: &StatuslineOpts,
) -> String {
    let provider_label = match account.provider {
        Provider::Claude => "Claude",
        Provider::Codex => "Codex",
        Provider::Antigravity => "AGY",
        Provider::PixelLab => "Pixel",
        Provider::Grok => "Grok",
    };
    let name = display_name(
        account.label.as_deref(),
        row_email,
        account.profile_email.as_deref(),
        &account.profile,
    );
    let mut rendered = String::from("  ");
    // provider marker は `--logos` なら brand-logo glyph、そうでなければ text label。
    // どちらのモードでも Codex は teal brand color より white の方が読みやすいので
    // マーカー用の色は marker_color() に集約する。
    let marker_color = marker_color(account.provider);
    if opts.logos {
        let logo = match account.provider {
            Provider::Claude => CLAUDE_LOGO,
            Provider::Codex => CODEX_LOGO,
            Provider::Antigravity => ANTIGRAVITY_LOGO,
            Provider::PixelLab => PIXELLAB_LOGO,
            Provider::Grok => GROK_LOGO,
        };
        rendered += &paint(opts.color, &marker_color, &format!("{logo}  "));
    } else {
        rendered += &paint(opts.color, &marker_color, &format!("{provider_label:<6} "));
    }
    // Antigravity 行は単一 token で account name が冗長なため model-group を表示する。
    // それ以外は account name を表示する。"Claude&GPT" が入る幅で pad し、全行の gauge を揃える。
    let display = account.group_label.as_deref().unwrap_or(&name);
    rendered += &paint(
        opts.color,
        if active { BOLD_RED } else { GRAY },
        &pad_display(display, NAME_FIELD_WIDTH),
    );
    rendered
}

/// 表示名を欄幅ちょうどに整える。
///
/// `format!("{display:<11}")` は幅を **char 数**で数え、超過分を切り詰めない。そのため
/// 全角を含む label は 1 文字 2 桁ぶん右へずれ、11 文字以上の label は gauge 位置がずれ、
/// ちょうど 11 文字だと padding が 0 になって次の window ラベルと地続きになる
/// (`development5h ███…`)。label は config のユーザー入力・email の local part・
/// Chrome profile 名から来るため、いずれも現実に起こる。
///
/// ここでは端末上の表示幅で数え、溢れる分は切り詰め、右端に必ず 1 桁以上の区切りを残す。
fn pad_display(s: &str, field_width: usize) -> String {
    // 1 桁は区切り用に確保するので、本文が使えるのは field_width - 1 桁まで。
    let budget = field_width.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0usize;
    for c in s.chars() {
        let width = c.width().unwrap_or(0);
        if used + width > budget {
            break;
        }
        out.push(c);
        used += width;
    }
    out.push_str(&" ".repeat(field_width - used));
    out
}

/// quota の空表示・単一枠・二枠表示を選び、statusline 後半を組み立てる。
fn render_windows(account: &AccountOut, opts: &StatuslineOpts, now: DateTime<Utc>) -> String {
    // 長期(right)スロットのラベルはプロバイダーごとのリセット周期に合わせる。
    // PixelLab / Grok は月次枠なので "1m"、それ以外は従来どおり "1w"。
    let short_label = window_label(account.short.as_ref().and_then(|window| window.kind), "5h");
    let long_label = window_label(
        account.long.as_ref().and_then(|window| window.kind),
        legacy_long_window_label(account.provider),
    );
    let gauge_width = if opts.compact { 8 } else { 16 };
    let mut rendered = String::new();
    if !account.ok {
        // データ取得に失敗したアカウントも、データ有り行と桁位置を揃える。
        // window_seg の None 分岐(空ゲージ + "--")を短期 / 長期スロット双方で再利用する。
        // 短期スロットには reset_at を伝搬しない(長期限定のため false 固定)。
        rendered += &window_seg(opts, "5h", None, now, FIVE_H_TH, false, gauge_width);
        rendered += "   ";
        rendered += &window_seg(
            opts,
            long_label,
            None,
            now,
            legacy_long_window_thresholds(account.provider),
            opts.reset_at,
            gauge_width,
        );
        return rendered;
    }
    // quota が短期 / 長期どちらか 1 枠だけなら、空スロットを巻き取って横長表示する。
    // PixelLab / local Antigravity は長期のみ、OAuth Antigravity は日次の短期のみ。
    // 横幅 = 2 スロット + 区切り 3 文字 と等しくなるよう wide_gauge = 2*gauge + 19。
    let single_window = match (account.short.as_ref(), account.long.as_ref()) {
        (Some(w), None) => Some((w, short_label, FIVE_H_TH, false)),
        (None, Some(w)) => Some((
            w,
            long_label,
            legacy_long_window_thresholds(account.provider),
            opts.reset_at,
        )),
        _ => None,
    };
    if let Some((window, label, legacy_thresholds, show_reset_at)) = single_window {
        let wide_gauge = 2 * gauge_width + 19;
        rendered += &window_seg(
            opts,
            label,
            Some(window),
            now,
            window_thresholds(Some(window), legacy_thresholds),
            show_reset_at,
            wide_gauge,
        );
    } else {
        rendered += &window_seg(
            opts,
            short_label,
            account.short.as_ref(),
            now,
            window_thresholds(account.short.as_ref(), FIVE_H_TH),
            false,
            gauge_width,
        );
        rendered += "   ";
        rendered += &window_seg(
            opts,
            long_label,
            account.long.as_ref(),
            now,
            window_thresholds(
                account.long.as_ref(),
                legacy_long_window_thresholds(account.provider),
            ),
            opts.reset_at,
            gauge_width,
        );
    }
    rendered
}

fn window_thresholds(w: Option<&WindowOut>, legacy: [i64; 3]) -> [i64; 3] {
    match w.and_then(|w| w.kind) {
        Some(WindowKind::FiveHour) => FIVE_H_TH,
        Some(WindowKind::Daily) => DAILY_TH,
        Some(WindowKind::Weekly) => WEEK_TH,
        Some(WindowKind::Monthly) => MONTHLY_TH,
        None => legacy,
    }
}

/// `kind` を持たない旧キャッシュ用の長期枠しきい値。
/// ラベルと同じく PixelLab / Grok は月次、それ以外は週次として扱う。
fn legacy_long_window_thresholds(provider: Provider) -> [i64; 3] {
    match provider {
        Provider::PixelLab | Provider::Grok => MONTHLY_TH,
        _ => WEEK_TH,
    }
}

fn window_seg(
    opts: &StatuslineOpts,
    label: &str,
    w: Option<&WindowOut>,
    now: DateTime<Utc>,
    th: [i64; 3],
    show_reset_at: bool,
    gauge_width: usize,
) -> String {
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
            hide: Vec::new(),
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
    fn gauge_lights_one_block_for_any_nonzero_usage() {
        let filled = |pct| gauge(false, pct, 16).chars().filter(|c| *c == '█').count();
        let empty = |pct| gauge(false, pct, 16).chars().filter(|c| *c == '░').count();

        // 0% は空のまま。ごく僅かな使用は四捨五入で 0 ブロックになるが、
        // 「使っているのに空バー」に見せないため必ず 1 ブロックは点灯させる。
        assert_eq!(filled(0.0), 0);
        assert_eq!(filled(0.1), 1);
        assert_eq!(filled(50.0), 8);
        assert_eq!(filled(100.0), 16);

        // 範囲外の値(API 側の race / 破損データ)でも panic せず幅を超えない。
        assert_eq!(filled(150.0), 16);
        assert_eq!(filled(-10.0), 0);
        // 桁揃えの不変条件: 点灯 + 空白の合計は常に gauge 幅と一致する。
        for pct in [-10.0, 0.0, 0.1, 42.0, 99.9, 100.0, 150.0] {
            assert_eq!(filled(pct) + empty(pct), 16, "pct={pct}");
        }
    }

    #[test]
    fn usage_colors_escalate_at_60_80_90_percent() {
        // gauge とパーセント表示は同じ境界で段階的に警告色へ上げる。
        for code in [level_code, pct_code] {
            assert_eq!(code(0.0), code(59.9), "60% 未満は同じ色");
            assert_ne!(code(60.0), code(59.9), "60% で 1 段上がる");
            assert_ne!(code(80.0), code(79.9), "80% で 1 段上がる");
            assert_ne!(code(90.0), code(89.9), "90% で 1 段上がる");
        }
    }

    #[test]
    fn reset_colors_use_exclusive_thresholds() {
        // しきい値ちょうどは「まだ余裕がある側」に入る(`<` 判定)。境界がずれると
        // リセット直前の赤が 1 段早く/遅く出る。
        let th = WEEK_TH;
        assert_eq!(reset_code(th[0] - 1, th), "38;5;196");
        assert_eq!(reset_code(th[0], th), "38;5;208");
        assert_eq!(reset_code(th[1], th), "38;5;178");
        assert_eq!(reset_code(th[2], th), "38;5;35");
        // 月次枠は同じ関数をより長いしきい値で使う(2 日後は月次なら危険域)。
        assert_eq!(reset_code(2 * 86400, MONTHLY_TH), "38;5;196");
        assert_eq!(reset_code(2 * 86400, WEEK_TH), "38;5;178");
    }

    #[test]
    fn window_seg_appends_reset_at_only_when_enabled() {
        // 未来のリセット時刻 + show_reset_at=true → 末尾に "(MM/DD HH:MM)" が付く。
        // ローカル TZ 依存の具体値は検証せず、括弧の有無で機能を確認。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let opts = plain_opts();
        let w = WindowOut {
            kind: Some(WindowKind::Weekly),
            used_percent: 54.0,
            resets_at: Some("2026-06-17T16:10:00Z".to_string()),
            resets_in_seconds: Some(2 * 86400),
        };
        let with_date = window_seg(&opts, "1w", Some(&w), now, WEEK_TH, true, 16);
        assert!(
            with_date.contains('(') && with_date.ends_with(')'),
            "expected date suffix in {with_date:?}"
        );
        let without = window_seg(&opts, "1w", Some(&w), now, WEEK_TH, false, 16);
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
        let none_out = window_seg(&opts, "1w", None, now, WEEK_TH, true, 16);
        assert!(
            !none_out.contains('('),
            "no date for None window: {none_out:?}"
        );

        let expired = WindowOut {
            kind: Some(WindowKind::Weekly),
            used_percent: 100.0,
            resets_at: Some("2026-06-10T00:00:00Z".to_string()),
            resets_in_seconds: Some(0),
        };
        let expired_out = window_seg(&opts, "1w", Some(&expired), now, WEEK_TH, true, 16);
        assert!(
            !expired_out.contains('('),
            "no date when already reset: {expired_out:?}"
        );
        assert!(expired_out.contains("now"));
    }

    #[test]
    fn window_seg_gauge_width_controls_bar_length() {
        // 明示的に渡した gauge_width で bar 長が決まる。マージ時の横長スロット検証。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let opts = plain_opts();
        let w = WindowOut {
            kind: Some(WindowKind::Weekly),
            used_percent: 50.0,
            resets_at: Some("2026-06-17T00:00:00Z".to_string()),
            resets_in_seconds: Some(2 * 86400),
        };
        // 標準幅 16 → gauge 部は 16 文字。
        let normal = window_seg(&opts, "1w", Some(&w), now, WEEK_TH, false, 16);
        // マージ時の 51 → gauge 部は 51 文字。gauge 部分だけ純増する。
        let wide = window_seg(&opts, "1m", Some(&w), now, WEEK_TH, false, 51);
        // gauge 文字 (█/░) の総数で比較。
        let count_glyphs = |s: &str| s.chars().filter(|c| *c == '█' || *c == '░').count();
        assert_eq!(count_glyphs(&normal), 16);
        assert_eq!(count_glyphs(&wide), 51);
    }

    #[test]
    fn single_daily_window_uses_typed_label_and_merged_width() {
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let account = AccountOut {
            profile: "Antigravity".to_string(),
            provider: Provider::Antigravity,
            ok: true,
            plan: None,
            email: None,
            profile_email: None,
            label: None,
            group_label: Some("Gemini".to_string()),
            short: Some(WindowOut {
                kind: Some(WindowKind::Daily),
                used_percent: 50.0,
                resets_at: Some("2026-06-16T00:00:00Z".to_string()),
                resets_in_seconds: Some(86400),
            }),
            long: None,
            error: None,
        };
        let mut opts = plain_opts();
        opts.reset_at = true;
        let line = render_row(&account, None, false, &opts, now);
        let gauge_glyphs = line.chars().filter(|c| *c == '█' || *c == '░').count();
        assert!(line.contains("1d "), "daily label missing: {line:?}");
        assert!(
            !line.contains('('),
            "short-only window must not show absolute reset time: {line:?}"
        );
        assert_eq!(gauge_glyphs, 51);
    }

    #[test]
    fn legacy_monthly_window_uses_monthly_reset_thresholds() {
        // 旧キャッシュでは kind が欠落する。PixelLab の 2 日後リセットは、
        // 週次の黄色ではなく月次の危険域(3 日未満)として赤で表示する。
        assert_eq!(legacy_long_window_thresholds(Provider::Grok), MONTHLY_TH);
        assert_eq!(legacy_long_window_thresholds(Provider::Claude), WEEK_TH);
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let account = AccountOut {
            profile: "PixelLab".to_string(),
            provider: Provider::PixelLab,
            ok: true,
            plan: None,
            email: None,
            profile_email: None,
            label: None,
            group_label: None,
            short: None,
            long: Some(WindowOut {
                kind: None,
                used_percent: 10.0,
                resets_at: Some("2026-06-17T00:00:00Z".to_string()),
                resets_in_seconds: Some(2 * 86400),
            }),
            error: None,
        };
        let mut opts = plain_opts();
        opts.color = true;

        let rendered = render_windows(&account, &opts, now);
        assert!(rendered.contains("1m "), "月次ラベルがない: {rendered:?}");
        assert!(
            rendered.contains("\x1b[38;5;196m2d00h "),
            "月次しきい値の赤色になっていない: {rendered:?}"
        );
    }

    /// 表示幅を東アジア文字幅で数える(テスト側の期待値を組み立てるための補助)。
    fn display_width(s: &str) -> usize {
        s.chars().map(|c| c.width().unwrap_or(0)).sum()
    }

    #[test]
    fn pad_display_keeps_short_names_byte_identical_to_plain_padding() {
        // 欄に収まる ASCII 名は従来の `{:<11}` と 1 バイトも変わってはいけない。
        // ここが変わると既存 statusline の桁揃えが丸ごとずれる。
        for name in ["", "a", "owa", "home", "work", "Claude&GPT"] {
            assert_eq!(
                pad_display(name, NAME_FIELD_WIDTH),
                format!("{name:<11}"),
                "収まる名前で従来出力と差が出た: {name:?}"
            );
        }
    }

    #[test]
    fn pad_display_always_leaves_a_separator_column() {
        // ちょうど欄幅と同じ 11 文字は、従来 padding が 0 になり
        // 次の window ラベルと地続きになっていた("development5h ...")。
        // 切り詰めて必ず 1 桁以上の空白を残す。
        for name in [
            "development",          // ちょうど 11 文字
            "christopher.anderson", // 11 文字超
            "antigravity",          // README のサンプル label
        ] {
            let padded = pad_display(name, NAME_FIELD_WIDTH);
            assert_eq!(
                display_width(&padded),
                NAME_FIELD_WIDTH,
                "欄幅に揃っていない: {padded:?}"
            );
            assert!(
                padded.ends_with(' '),
                "区切りの空白が残っていない: {padded:?}"
            );
        }
    }

    #[test]
    fn pad_display_counts_full_width_characters_as_two_columns() {
        // 全角は 1 文字 = 2 桁。char 数で数えると 1 文字ごとに 1 桁ずつ溢れる。
        let padded = pad_display("業務用アカウント", NAME_FIELD_WIDTH);
        assert_eq!(display_width(&padded), NAME_FIELD_WIDTH);
        // 10 桁ぶん = 全角 5 文字だけ入り、残り 1 桁が区切りになる。
        assert_eq!(padded, "業務用アカ ");
    }

    #[test]
    fn pad_display_does_not_split_a_character_across_the_boundary() {
        // 全角は 2 桁なので、残り 1 桁の位置では入れずに打ち切る(半端な桁を作らない)。
        // "a" (1 桁) + 全角 4 文字 (8 桁) = 9 桁。次の全角は 11 桁目に食い込むため入らない。
        let padded = pad_display("a業務用ア業", NAME_FIELD_WIDTH);
        assert_eq!(padded, "a業務用ア  ");
        assert_eq!(display_width(&padded), NAME_FIELD_WIDTH);
    }
}
