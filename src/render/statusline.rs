//! compact / colored statusline 出力(1 account 1 行)。

use chrono::{DateTime, Local, Utc};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::manual_resets::format_resets_compact;
use super::palette::{self, ACTIVE, ALERT, Fg, LABEL, Level, MUTED, Paint};
use super::reset::{FIVE_H_TH, legacy_long_window_thresholds, window_thresholds};
use super::sort::{sorted_refs, statusline_default_cmp};
use super::{
    ActiveTarget, display_name, legacy_long_window_label, limit_observation_age, parse_utc,
    preferred_email, resolve_active, window_label,
};
use crate::SortKey;
use crate::model::Provider;
use crate::report::{AccountOut, Report, WindowOut};

// BrandLogos font の PUA-B glyph。`--logos` で使う。
const CLAUDE_LOGO: &str = "\u{100002}"; // Claude の太陽マーク。
const CODEX_LOGO: &str = "\u{100000}"; // OpenAI のマーク。
const ANTIGRAVITY_LOGO: &str = "\u{100003}"; // Antigravity のマーク。
const GROK_LOGO: &str = "\u{100004}"; // Grok (xAI) のマーク。
const PIXELLAB_LOGO: &str = "\u{100400}"; // PixelLab のドラゴン。
/// account 名 / model-group 名の欄幅(末尾の区切り 1 桁を含む)。最長の想定値
/// "Claude&GPT"(10 桁)がちょうど収まり、その右に区切りが 1 桁残る。
const NAME_FIELD_WIDTH: usize = 11;
/// `--reset-at` が長期枠の後ろに付ける ` (MM/DD HH:MM)` の表示幅。
const RESET_AT_WIDTH: usize = 14;

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
) -> std::io::Result<()> {
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
    super::write_stdout(&lines.join("\n"))
}

pub(super) fn render_row(
    a: &AccountOut,
    row_email: Option<&str>,
    active: bool,
    opts: &StatuslineOpts,
    now: DateTime<Utc>,
) -> String {
    let mut rendered = render_identity(a, row_email, active, opts);
    rendered += &render_windows(a, opts, now);
    if let Some(observation) = a.limit_observation.as_ref()
        && let Some(age) = limit_observation_age(observation, now)
    {
        rendered += &paint(
            opts.color,
            ALERT,
            &format!("  24h LIMIT {} ({age} ago)", observation.model),
        );
    }
    if a.ok
        && let Some(resets) = &a.manual_resets
    {
        let resets = format_resets_compact(resets, now);
        rendered += &paint(opts.color, LABEL, &format!("  R:{}", resets.counts));
        if let Some(deadline) = resets.deadline {
            rendered += &paint(opts.color, LABEL, " (");
            rendered += &paint(
                opts.color,
                palette::manual_reset_deadline(deadline.expires_at, now),
                &deadline.text,
            );
            rendered += &paint(opts.color, LABEL, ")");
        }
    }
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
    // 色はどちらのモードでも palette::marker が決める (Codex / Grok は白)。
    let marker = palette::marker(account.provider);
    if opts.logos {
        let logo = match account.provider {
            Provider::Claude => CLAUDE_LOGO,
            Provider::Codex => CODEX_LOGO,
            Provider::Antigravity => ANTIGRAVITY_LOGO,
            Provider::PixelLab => PIXELLAB_LOGO,
            Provider::Grok => GROK_LOGO,
        };
        rendered += &paint(opts.color, marker, &format!("{logo}  "));
    } else {
        rendered += &paint(opts.color, marker, &format!("{provider_label:<6} "));
    }
    // Antigravity 行は単一 token で account name が冗長なため model-group を表示する。
    // それ以外は account name を表示する。"Claude&GPT" が入る幅で pad し、全行の gauge を揃える。
    let display = account.group_label.as_deref().unwrap_or(&name);
    rendered += &paint(
        opts.color,
        if active { ACTIVE } else { LABEL },
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
pub(super) fn pad_display(s: &str, field_width: usize) -> String {
    // 1 桁は区切り用に確保するので、本文が使えるのは field_width - 1 桁まで。
    let budget = field_width.saturating_sub(1);
    let width = s.width();
    if width <= budget {
        return format!("{s}{}", " ".repeat(field_width - width));
    }
    let mut out = String::new();
    let mut used = 0usize;
    // ZWJ 絵文字・国旗・結合文字を途中で切らず、連なった文字列全体の幅で判断する。
    // 文字ごとの幅の合計では、絵文字や一部の合字の表示幅を過大に数えてしまう。
    for (start, grapheme) in s.grapheme_indices(true) {
        let prefix = &s[..start + grapheme.len()];
        let width = prefix.width();
        if width > budget {
            break;
        }
        out.push_str(grapheme);
        used = width;
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
        // 短期枠だけの行は `--reset-at` の日時を付けないが、同じ幅を空けて後続の列を揃える。
        if opts.reset_at && !show_reset_at {
            rendered += &" ".repeat(RESET_AT_WIDTH);
        }
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

fn window_seg(
    opts: &StatuslineOpts,
    label: &str,
    w: Option<&WindowOut>,
    now: DateTime<Utc>,
    th: [i64; 3],
    show_reset_at: bool,
    gauge_width: usize,
) -> String {
    let mut s = paint(opts.color, LABEL, &format!("{label} "));
    let mut reset_at_shown = false;
    match w {
        None => {
            // データ無し: 空 gauge + "--"(% なし) + "--"(残り時間)。
            // Some 分岐と同じ桁幅(gauge_width + 1 + 4 + 2 + 6)に揃える。
            s += &paint(opts.color, MUTED, &"░".repeat(gauge_width));
            s += " ";
            s += &paint(opts.color, MUTED, &format!("{:>4}", "--"));
            s += "  ";
            s += &paint(opts.color, MUTED, &format!("{:<6}", "--"));
        }
        Some(w) => {
            match w.used_percent {
                Some(percent) => {
                    s += &gauge(opts.color, percent, gauge_width);
                    s += " ";
                    s += &paint(
                        opts.color,
                        Level::usage(percent).text(),
                        &format!("{:>3}%", percent.round() as i64),
                    );
                }
                None => {
                    s += &paint(opts.color, MUTED, &"░".repeat(gauge_width));
                    s += " ";
                    s += &paint(opts.color, MUTED, " --%");
                }
            }
            s += "  ";
            let reset = w.resets_at.as_deref().and_then(parse_utc);
            let rem = reset.map(|r| (r - now).num_seconds());
            let countdown = match rem {
                Some(sec) if sec > 0 => compact_dur(sec),
                Some(_) => "now".to_string(),
                None => "--".to_string(),
            };
            // 残り時間・リセット済み ("now")・時刻不明 ("--") のどれも、色は残り秒数から
            // palette::reset_countdown が決める。
            s += &paint(
                opts.color,
                palette::reset_countdown(rem, th),
                &format!("{countdown:<6}"),
            );
            // --reset-at: 長期枠の残り時間の後ろに (MM/DD HH:MM) を local 時刻で併記する。
            // 短期枠は呼び出し元で show_reset_at=false 固定。リセット済みの枠には付けない。
            if show_reset_at
                && rem.is_some_and(|sec| sec > 0)
                && let Some(r) = reset
            {
                s += &paint(
                    opts.color,
                    MUTED,
                    &format!(" ({})", r.with_timezone(&Local).format("%m/%d %H:%M")),
                );
                reset_at_shown = true;
            }
        }
    }
    // データ無し・リセット済み・時刻不明で日時を出せない行も同じ幅を空け、
    // 後ろに続く手動リセットの列を行間で揃える。
    if show_reset_at && !reset_at_shown {
        s += &" ".repeat(RESET_AT_WIDTH);
    }
    s
}

/// `s` を `p` の SGR で囲む。色は palette が決め、ここでは ANSI に直すだけにする。
///
/// Claude Code は statusline をキャッシュから描き直すので、出力はバイト単位で保つ。
/// 区間ごとに必ず `\x1b[0m` で閉じ、隣り合う同色の区間もまとめない。空文字列も囲む
/// (使用率 0% の gauge は点灯部が空でも SGR を出す)。色を使わないときは何も足さない。
/// `--disk` の人向け出力も、同じ変換で色を付ける。
pub(super) fn paint(color: bool, p: Paint, s: &str) -> String {
    if color {
        format!("\x1b[{}m{s}\x1b[0m", sgr(p))
    } else {
        s.to_string()
    }
}

/// ANSI SGR のパラメータ部。太字は `1;` を前に置く (`1;38;5;196`)。
fn sgr(p: Paint) -> String {
    let fg = match p.fg {
        Fg::Indexed(index) => format!("38;5;{index}"),
        Fg::Rgb(r, g, b) => format!("38;2;{r};{g};{b}"),
    };
    if p.bold { format!("1;{fg}") } else { fg }
}

fn gauge(color: bool, pct: f64, width: usize) -> String {
    let mut filled = ((pct / 100.0) * width as f64).round() as i64;
    if pct > 0.0 && filled < 1 {
        filled = 1;
    }
    let filled = filled.clamp(0, width as i64) as usize;
    let mut s = paint(color, Level::usage(pct).fill(), &"█".repeat(filled));
    s += &paint(color, MUTED, &"░".repeat(width - filled));
    s
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
    use super::super::reset::{MONTHLY_TH, WEEK_TH};
    use super::*;
    use crate::model::{ResetKind, WindowKind};
    use crate::report::{LimitObservationOut, ManualResetOut};

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

    fn manual_reset_account(expires_at: Option<DateTime<Utc>>) -> AccountOut {
        AccountOut {
            profile: "Work".into(),
            provider: Provider::Codex,
            ok: true,
            plan: None,
            email: None,
            profile_email: None,
            label: None,
            group_label: None,
            short: None,
            long: None,
            manual_resets: Some(vec![ManualResetOut {
                kind: ResetKind::Full,
                remaining: Some(1),
                expires_at: expires_at.map(|expiry| expiry.to_rfc3339()),
                paused: false,
            }]),
            limit_observation: None,
            error: None,
        }
    }

    #[test]
    fn unknown_percent_keeps_weekly_reset_in_statusline() {
        let now = fixed_utc("2026-10-10T00:00:00Z");
        let window = WindowOut {
            kind: Some(WindowKind::Weekly),
            used_percent: None,
            resets_at: Some((now + chrono::Duration::days(5)).to_rfc3339()),
            resets_in_seconds: None,
        };
        let rendered = window_seg(&plain_opts(), "1w", Some(&window), now, WEEK_TH, false, 8);
        assert!(
            rendered.contains("--%") && rendered.contains("5d"),
            "{rendered}"
        );
        assert!(!rendered.contains("0%"), "{rendered}");
    }

    #[test]
    fn manual_reset_deadline_is_red_only_below_one_week() {
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let week = chrono::Duration::weeks(1);
        let tick = chrono::Duration::nanoseconds(1);
        let mut opts = plain_opts();
        opts.color = true;
        for (remaining, red) in [
            (Some(week - tick), true),
            (Some(week), false),
            (Some(week + tick), false),
            (Some(tick), true),
            (Some(chrono::Duration::zero()), false),
            (Some(-tick), false),
            (None, false),
        ] {
            let expiry = remaining.map(|remaining| now + remaining);
            let account = manual_reset_account(expiry);
            let line = render_row(&account, None, false, &opts, now);
            let suffix = line.rsplit_once("R:").unwrap().1;
            assert_eq!(
                suffix.matches("\x1b[38;5;196m").count(),
                usize::from(red),
                "remaining={remaining:?}: {suffix:?}"
            );
            if red {
                let date = expiry
                    .unwrap()
                    .with_timezone(&Local)
                    .format("%m/%d %H:%M")
                    .to_string();
                assert!(
                    suffix.contains(&format!("\x1b[38;5;196m{date}\x1b[0m")),
                    "{suffix:?}"
                );
                assert!(suffix.ends_with("\x1b[38;5;245m)\x1b[0m"), "{suffix:?}");
                assert!(suffix.starts_with("full 1\x1b[0m"), "{suffix:?}");
            }
        }
    }

    #[test]
    fn manual_resets_start_in_one_column_whether_or_not_a_reset_date_is_shown() {
        // `--reset-at` の日時を出せない行(時刻不明・リセット済み・長期枠なし)も同じ幅を空け、
        // 手動リセットの列が行ごとにずれないようにする。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let window = |kind, resets_at: Option<&str>| {
            Some(WindowOut {
                kind: Some(kind),
                used_percent: Some(20.0),
                resets_at: resets_at.map(str::to_string),
                resets_in_seconds: None,
            })
        };
        let column = |short, long, reset_at| {
            let mut account = manual_reset_account(None);
            account.short = short;
            account.long = long;
            let mut opts = plain_opts();
            opts.reset_at = reset_at;
            let line = render_row(&account, None, false, &opts, now);
            line.split_once("R:").unwrap().0.width()
        };
        let five_hour = || window(WindowKind::FiveHour, Some("2026-06-15T03:00:00Z"));
        let dated = || window(WindowKind::Weekly, Some("2026-06-17T00:00:00Z"));
        let undated = || window(WindowKind::Weekly, None);
        let expected = column(five_hour(), dated(), true);
        for (case, short, long) in [
            ("時刻不明", five_hour(), undated()),
            (
                "リセット済み",
                five_hour(),
                window(WindowKind::Weekly, Some("2026-06-10T00:00:00Z")),
            ),
            ("短期枠だけ", five_hour(), None),
            ("長期枠だけ", None, dated()),
            ("長期枠だけで時刻不明", None, undated()),
        ] {
            assert_eq!(column(short, long, true), expected, "{case}");
        }
        // `--reset-at` が無いときは日時の幅を空けない。
        assert_eq!(
            column(five_hour(), dated(), false),
            expected - RESET_AT_WIDTH
        );
        assert_eq!(column(five_hour(), None, false), expected - RESET_AT_WIDTH);
    }

    #[test]
    fn manual_resets_are_omitted_for_failed_rows_and_rows_without_them() {
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let mut failed = manual_reset_account(Some(now + chrono::Duration::days(1)));
        failed.ok = false;
        failed.error = Some("fetch failed".into());
        let line = render_row(&failed, None, false, &plain_opts(), now);
        assert!(!line.contains("R:"), "{line:?}");

        let mut without = manual_reset_account(None);
        without.manual_resets = None;
        let line = render_row(&without, None, false, &plain_opts(), now);
        assert!(!line.contains("R:"), "{line:?}");
    }

    #[test]
    fn manual_reset_deadline_respects_color_suppression() {
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let account = manual_reset_account(Some(now + chrono::Duration::days(1)));
        let line = render_row(&account, None, false, &plain_opts(), now);
        assert!(!line.contains('\x1b'), "{line:?}");
        assert!(line.contains("R:full 1 ("), "{line}");
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
    fn window_seg_appends_reset_at_only_when_enabled() {
        // 未来のリセット時刻 + show_reset_at=true → 末尾に "(MM/DD HH:MM)" が付く。
        // ローカル TZ 依存の具体値は検証せず、括弧の有無で機能を確認。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let opts = plain_opts();
        let w = WindowOut {
            kind: Some(WindowKind::Weekly),
            used_percent: Some(54.0),
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
            used_percent: Some(100.0),
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
            used_percent: Some(50.0),
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
                used_percent: Some(50.0),
                resets_at: Some("2026-06-16T00:00:00Z".to_string()),
                resets_in_seconds: Some(86400),
            }),
            long: None,
            manual_resets: None,
            limit_observation: None,
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
                used_percent: Some(10.0),
                resets_at: Some("2026-06-17T00:00:00Z".to_string()),
                resets_in_seconds: Some(2 * 86400),
            }),
            error: None,
            manual_resets: None,
            limit_observation: None,
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

    // 以下の golden テストは色付きの行をバイト単位で固定する。Claude Code はキャッシュから
    // statusline を描き直すので、TUI の都合で palette を変えて statusline の出力まで
    // 変わったら、ここで気付けるようにする。期待値は palette の関数から組み立てず SGR を
    // そのまま書く。日時だけは表示と同じく local 時刻で組み立てる。

    #[test]
    fn colored_claude_row_matches_golden_bytes() {
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let weekly_reset = fixed_utc("2026-06-17T05:00:00Z");
        let reset_expiry = fixed_utc("2026-06-16T03:00:00Z");
        let account = AccountOut {
            profile: "Home".into(),
            provider: Provider::Claude,
            ok: true,
            plan: Some("Max".into()),
            email: None,
            profile_email: None,
            label: Some("home".into()),
            group_label: None,
            short: Some(WindowOut {
                kind: Some(WindowKind::FiveHour),
                used_percent: Some(62.0),
                resets_at: Some("2026-06-15T04:40:00Z".into()),
                resets_in_seconds: None,
            }),
            long: Some(WindowOut {
                kind: Some(WindowKind::Weekly),
                used_percent: Some(85.0),
                resets_at: Some(weekly_reset.to_rfc3339()),
                resets_in_seconds: None,
            }),
            manual_resets: Some(vec![
                ManualResetOut {
                    kind: ResetKind::Full,
                    remaining: Some(1),
                    expires_at: Some(reset_expiry.to_rfc3339()),
                    paused: false,
                },
                ManualResetOut {
                    kind: ResetKind::FiveHour,
                    remaining: Some(0),
                    expires_at: None,
                    paused: false,
                },
            ]),
            // 経過時間は now から数えるので、now を固定すれば "3h ago" で決まる。
            limit_observation: Some(LimitObservationOut {
                model: "test-model".into(),
                observed_at: "2026-06-14T20:50:00Z".into(),
                used_tokens: 120,
                limit_tokens: 100,
            }),
            error: None,
        };
        let mut opts = plain_opts();
        opts.color = true;
        opts.reset_at = true;
        let local =
            |time: DateTime<Utc>| time.with_timezone(&Local).format("%m/%d %H:%M").to_string();
        let expected = format!(
            concat!(
                "  \x1b[38;2;217;119;87mClaude \x1b[0m",
                "\x1b[38;5;245mhome       \x1b[0m",
                // 5h: 62% は gauge も数字も黄。残り 4h40m は緑。
                "\x1b[38;5;245m5h \x1b[0m",
                "\x1b[38;5;178m██████████\x1b[0m\x1b[38;5;242m░░░░░░\x1b[0m",
                " \x1b[38;5;178m 62%\x1b[0m",
                "  \x1b[38;5;35m4h40m \x1b[0m",
                // 1w: 85% は数字だけ太字。残り 2d05h は黄、--reset-at の日時は MUTED。
                "   \x1b[38;5;245m1w \x1b[0m",
                "\x1b[38;5;208m██████████████\x1b[0m\x1b[38;5;242m░░\x1b[0m",
                " \x1b[1;38;5;208m 85%\x1b[0m",
                "  \x1b[38;5;178m2d05h \x1b[0m",
                "\x1b[38;5;242m ({weekly_reset})\x1b[0m",
                "\x1b[38;5;196m  24h LIMIT test-model (3h ago)\x1b[0m",
                // 手動リセット: 件数と括弧は LABEL、7 日未満で切れる期限だけ赤。
                "\x1b[38;5;245m  R:full 1; 5h 0\x1b[0m",
                "\x1b[38;5;245m (\x1b[0m",
                "\x1b[38;5;196m{reset_expiry}\x1b[0m",
                "\x1b[38;5;245m)\x1b[0m",
            ),
            weekly_reset = local(weekly_reset),
            reset_expiry = local(reset_expiry),
        );
        assert_eq!(render_row(&account, None, false, &opts, now), expected);
    }

    #[test]
    fn colored_active_codex_row_matches_golden_bytes() {
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let reset_expiry = fixed_utc("2026-06-25T00:00:00Z");
        let mut account = manual_reset_account(Some(reset_expiry));
        // リセット時刻を過ぎた 100% の短期枠と、リセット時刻の無い 0% の長期枠。
        account.short = Some(WindowOut {
            kind: Some(WindowKind::FiveHour),
            used_percent: Some(100.0),
            resets_at: Some("2026-06-14T23:00:00Z".into()),
            resets_in_seconds: Some(0),
        });
        account.long = Some(WindowOut {
            kind: Some(WindowKind::Weekly),
            used_percent: Some(0.0),
            resets_at: None,
            resets_in_seconds: None,
        });
        let mut opts = plain_opts();
        opts.color = true;
        opts.logos = true;
        opts.compact = true;
        let expected = format!(
            concat!(
                // Codex の印は白、このセッションのアカウント名は太字の赤。
                "  \x1b[38;2;255;255;255m\u{100000}  \x1b[0m",
                "\x1b[1;38;5;196mWork       \x1b[0m",
                // 100% は消灯部が空でも SGR で囲む。リセット済みの now は緑。
                "\x1b[38;5;245m5h \x1b[0m",
                "\x1b[38;5;196m████████\x1b[0m\x1b[38;5;242m\x1b[0m",
                " \x1b[1;38;5;196m100%\x1b[0m",
                "  \x1b[38;5;35mnow   \x1b[0m",
                // 0% は点灯部が空でも SGR で囲む。時刻不明の -- は MUTED。
                "   \x1b[38;5;245m1w \x1b[0m",
                "\x1b[38;5;35m\x1b[0m\x1b[38;5;242m░░░░░░░░\x1b[0m",
                " \x1b[38;5;35m  0%\x1b[0m",
                "  \x1b[38;5;242m--    \x1b[0m",
                // 7 日以上先の期限は LABEL のまま。同じ色が続いても区間ごとに閉じる。
                "\x1b[38;5;245m  R:full 1\x1b[0m",
                "\x1b[38;5;245m (\x1b[0m",
                "\x1b[38;5;245m{reset_expiry}\x1b[0m",
                "\x1b[38;5;245m)\x1b[0m",
            ),
            reset_expiry = reset_expiry.with_timezone(&Local).format("%m/%d %H:%M"),
        );
        assert_eq!(render_row(&account, None, true, &opts, now), expected);
    }

    #[test]
    fn colored_placeholders_match_golden_bytes() {
        // 取得に失敗した行と使用率が不明な枠は、値の無いところを MUTED で描く。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let mut opts = plain_opts();
        opts.color = true;
        opts.compact = true;

        let mut failed = manual_reset_account(None);
        failed.profile = "Broken".into();
        failed.ok = false;
        failed.error = Some("fetch failed".into());
        assert_eq!(
            render_row(&failed, None, false, &opts, now),
            concat!(
                "  \x1b[38;2;255;255;255mCodex  \x1b[0m",
                "\x1b[38;5;245mBroken     \x1b[0m",
                "\x1b[38;5;245m5h \x1b[0m\x1b[38;5;242m░░░░░░░░\x1b[0m",
                " \x1b[38;5;242m  --\x1b[0m  \x1b[38;5;242m--    \x1b[0m",
                "   \x1b[38;5;245m1w \x1b[0m\x1b[38;5;242m░░░░░░░░\x1b[0m",
                " \x1b[38;5;242m  --\x1b[0m  \x1b[38;5;242m--    \x1b[0m",
            )
        );

        let unknown = AccountOut {
            profile: "Grok".into(),
            provider: Provider::Grok,
            ok: true,
            plan: Some("Free".into()),
            email: None,
            profile_email: None,
            label: None,
            group_label: None,
            short: None,
            long: Some(WindowOut {
                kind: Some(WindowKind::Monthly),
                used_percent: None,
                resets_at: Some("2026-07-05T03:00:00Z".into()),
                resets_in_seconds: None,
            }),
            manual_resets: None,
            limit_observation: None,
            error: None,
        };
        // 長期枠だけの行は 2 枠ぶんの幅 (2 * 8 + 19 = 35) の gauge にまとめる。
        assert_eq!(
            render_row(&unknown, None, false, &opts, now),
            concat!(
                "  \x1b[38;2;255;255;255mGrok   \x1b[0m",
                "\x1b[38;5;245mGrok       \x1b[0m",
                "\x1b[38;5;245m1m \x1b[0m",
                "\x1b[38;5;242m░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░\x1b[0m",
                " \x1b[38;5;242m --%\x1b[0m",
                "  \x1b[38;5;35m20d03h\x1b[0m",
            )
        );
    }

    #[test]
    fn colored_rows_without_a_reset_date_match_golden_bytes() {
        // `--reset-at` で日時を出せない行 (取得の失敗・短期枠だけ) も 14 桁を空け、
        // 後ろの `R:` の列を揃える。期限が不明な手動リセットは `(?)` を LABEL で出す。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let mut opts = plain_opts();
        opts.color = true;
        opts.compact = true;
        opts.reset_at = true;
        let pad = " ".repeat(RESET_AT_WIDTH);

        let mut failed = manual_reset_account(None);
        failed.ok = false;
        failed.error = Some("fetch failed".into());
        assert_eq!(
            render_row(&failed, None, false, &opts, now),
            format!(
                concat!(
                    "  \x1b[38;2;255;255;255mCodex  \x1b[0m",
                    "\x1b[38;5;245mWork       \x1b[0m",
                    "\x1b[38;5;245m5h \x1b[0m\x1b[38;5;242m░░░░░░░░\x1b[0m",
                    " \x1b[38;5;242m  --\x1b[0m  \x1b[38;5;242m--    \x1b[0m",
                    "   \x1b[38;5;245m1w \x1b[0m\x1b[38;5;242m░░░░░░░░\x1b[0m",
                    " \x1b[38;5;242m  --\x1b[0m  \x1b[38;5;242m--    \x1b[0m{pad}",
                ),
                pad = pad,
            )
        );

        let mut short_only = manual_reset_account(None);
        short_only.short = Some(WindowOut {
            kind: Some(WindowKind::FiveHour),
            used_percent: Some(20.0),
            resets_at: Some("2026-06-15T02:00:00Z".into()),
            resets_in_seconds: None,
        });
        // 短期枠だけの行は 2 枠ぶんの幅 (2 * 8 + 19 = 35) の gauge にまとめる。
        assert_eq!(
            render_row(&short_only, None, false, &opts, now),
            format!(
                concat!(
                    "  \x1b[38;2;255;255;255mCodex  \x1b[0m",
                    "\x1b[38;5;245mWork       \x1b[0m",
                    "\x1b[38;5;245m5h \x1b[0m",
                    "\x1b[38;5;35m███████\x1b[0m\x1b[38;5;242m{free}\x1b[0m",
                    " \x1b[38;5;35m 20%\x1b[0m",
                    "  \x1b[38;5;178m2h00m \x1b[0m{pad}",
                    "\x1b[38;5;245m  R:full 1\x1b[0m",
                    "\x1b[38;5;245m (\x1b[0m",
                    "\x1b[38;5;245m?\x1b[0m",
                    "\x1b[38;5;245m)\x1b[0m",
                ),
                free = "░".repeat(28),
                pad = pad,
            )
        );
    }

    #[test]
    fn sgr_puts_bold_first_and_keeps_truecolor() {
        // statusline の出力をバイト単位で保つため、SGR の並び (太字が先) を固定する。
        assert_eq!(sgr(LABEL), "38;5;245");
        assert_eq!(sgr(MUTED), "38;5;242");
        assert_eq!(sgr(ALERT), "38;5;196");
        assert_eq!(sgr(ACTIVE), "1;38;5;196");
        assert_eq!(sgr(palette::marker(Provider::Codex)), "38;2;255;255;255");
        assert_eq!(sgr(palette::marker(Provider::Claude)), "38;2;217;119;87");
    }

    /// 表示幅を東アジア文字幅で数える(テスト側の期待値を組み立てるための補助)。
    fn display_width(s: &str) -> usize {
        s.width()
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
    fn pad_display_measures_emoji_sequences_as_strings() {
        for name in ["👩‍🔬work", "🇯🇵home", "❤️work", "👨‍👩‍👧‍👦home"]
        {
            let padded = pad_display(name, NAME_FIELD_WIDTH);
            assert!(padded.starts_with(name), "名前が途中で切れた: {name:?}");
            assert_eq!(display_width(&padded), NAME_FIELD_WIDTH, "{name:?}");
        }
    }

    #[test]
    fn pad_display_keeps_graphemes_intact_at_the_boundary() {
        assert_eq!(pad_display("123456789👩‍🔬", NAME_FIELD_WIDTH), "123456789  ");
        assert_eq!(pad_display("12345678👩‍🔬", NAME_FIELD_WIDTH), "12345678👩‍🔬 ");
        assert_eq!(pad_display("e\u{301}x", 2), "e\u{301} ");
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
