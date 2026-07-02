//! human table、JSON、compact colored statusline の rendering。

use std::cmp::Ordering;

use chrono::{DateTime, Duration, Local, Utc};
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Attribute, Cell, Color, ContentArrangement, Table};
use serde::Serialize;

use crate::SortKey;
use crate::model::{AccountReport, Provider, Window};
use crate::report::{AccountOut, Report, WindowOut};

// ===== Sorting ==============================================================

/// `--sort` の比較ロジックを 1 箇所に集約する。`SortKey::Provider` の場合は
/// 「データ無し行を末尾に」というルールは適用せず、各レンダラーが自身の
/// デフォルト順序(table/json はジョブ順、statusline は provider.rank())を
/// 維持できるようにする。
fn cmp_optional_asc<T: PartialOrd>(a: Option<T>, b: Option<T>) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
        // Some(値あり) を上に、None(欠損)を末尾に固定。
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn cmp_optional_desc<T: PartialOrd>(a: Option<T>, b: Option<T>) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => y.partial_cmp(&x).unwrap_or(Ordering::Equal),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// 1 行分のソートキーを抽出する小さなビュー。`AccountOut` / `AccountReport`
/// 双方に実装することで、ソートロジック本体を 1 つで共有できる。
trait SortableRow {
    fn profile(&self) -> &str;
    fn weekly_used_percent(&self) -> Option<f64>;
    /// 週枠リセットまでの残秒数。`now` を渡すのは `AccountReport` 側で
    /// `DateTime<Utc>` から計算するため。`AccountOut` 側はキャッシュ JSON に
    /// 入った `resets_in_seconds` をそのまま返すと「キャッシュ作成時点」の
    /// 残秒数になるため、`resets_at` を再パースして `now` 起点で計算し直す。
    fn weekly_resets_in_seconds(&self, now: DateTime<Utc>) -> Option<i64>;
}

impl SortableRow for AccountOut {
    fn profile(&self) -> &str {
        &self.profile
    }
    fn weekly_used_percent(&self) -> Option<f64> {
        self.weekly.as_ref().map(|w| w.used_percent)
    }
    fn weekly_resets_in_seconds(&self, now: DateTime<Utc>) -> Option<i64> {
        self.weekly
            .as_ref()
            .and_then(|w| w.resets_at.as_deref())
            .and_then(parse_utc)
            .map(|r| (r - now).num_seconds().max(0))
    }
}

impl SortableRow for AccountReport {
    fn profile(&self) -> &str {
        &self.profile_name
    }
    fn weekly_used_percent(&self) -> Option<f64> {
        self.usage
            .as_ref()
            .ok()
            .and_then(|u| u.weekly.as_ref())
            .map(|w| w.used_percent)
    }
    fn weekly_resets_in_seconds(&self, now: DateTime<Utc>) -> Option<i64> {
        self.usage
            .as_ref()
            .ok()
            .and_then(|u| u.weekly.as_ref())
            .and_then(|w| w.resets_at)
            .map(|r| (r - now).num_seconds().max(0))
    }
}

/// 入力スライスを参照のまま並び替える。`SortKey::Provider` のときは挙動を
/// 呼び出し側に委ねるため、引数の `default_cmp` を使ってソートする
/// (statusline は provider.rank() ベース、table/json は維持なので
/// `None` を渡してそのまま出力する)。
fn sorted_refs<T: SortableRow>(
    items: &[T],
    sort: SortKey,
    now: DateTime<Utc>,
    default_cmp: Option<fn(&T, &T) -> Ordering>,
) -> Vec<&T> {
    let mut v: Vec<&T> = items.iter().collect();
    match sort {
        SortKey::Provider => {
            if let Some(cmp) = default_cmp {
                v.sort_by(|a, b| cmp(a, b));
            }
        }
        SortKey::WeeklyUsage => v.sort_by(|a, b| {
            cmp_optional_desc(a.weekly_used_percent(), b.weekly_used_percent())
                .then_with(|| a.profile().cmp(b.profile()))
        }),
        SortKey::WeeklyReset => v.sort_by(|a, b| {
            cmp_optional_asc(
                a.weekly_resets_in_seconds(now),
                b.weekly_resets_in_seconds(now),
            )
            .then_with(|| a.profile().cmp(b.profile()))
        }),
    }
    v
}

/// statusline の default 順序: provider.rank() → profile 名。
fn statusline_default_cmp(a: &AccountOut, b: &AccountOut) -> Ordering {
    a.provider
        .rank()
        .cmp(&b.provider.rank())
        .then_with(|| a.profile.cmp(&b.profile))
}

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

/// provider ごとの brand RGB。table(comfy-table `Color::Rgb`)と statusline
/// (`brand_sgr` の ANSI truecolor)で共有する単一 source。
fn brand_rgb(p: Provider) -> (u8, u8, u8) {
    match p {
        Provider::Claude => (217, 119, 87), // Anthropic coral #D97757
        Provider::Codex => (16, 163, 127),  // OpenAI teal #10A37F
        Provider::Antigravity => (66, 133, 244), // Google blue #4285F4
    }
}

/// provider の brand color を comfy-table truecolor として返す(table 用)。
fn provider_color(p: Provider) -> Color {
    let (r, g, b) = brand_rgb(p);
    Color::Rgb { r, g, b }
}

/// brand color を ANSI SGR truecolor parameter として返す(statusline 用)。
fn brand_sgr(p: Provider) -> String {
    let (r, g, b) = brand_rgb(p);
    format!("38;2;{r};{g};{b}")
}

/// service column の text。provider に、Antigravity 行では model-group を付ける。
fn service_label(p: Provider, group: Option<&str>) -> String {
    match group {
        Some(g) => format!("{} · {}", p.label(), g),
        None => p.label().to_string(),
    }
}

// ===== Human-readable table =================================================

pub fn table(reports: &[AccountReport], active: Option<&ActiveTarget>, sort: SortKey, debug: bool) {
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

    // SortKey::Provider のときは入力(=ジョブ順)をそのまま保持。
    let ordered = sorted_refs(reports, sort, now, None);
    for r in ordered {
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
        let is_active = resolve_active(active, r.provider, &r.profile_name, row_email, debug);
        let mut name_cell = Cell::new(&name);
        if is_active {
            name_cell = name_cell.fg(Color::Red).add_attribute(Attribute::Bold);
        }

        match &r.usage {
            Ok(u) => {
                table.add_row(vec![
                    name_cell,
                    Cell::new(service_label(r.provider, r.group_label.as_deref()))
                        .fg(provider_color(r.provider)),
                    Cell::new(u.plan.as_deref().unwrap_or("—")),
                    window_cell(&u.five_hour, now),
                    window_cell(&u.weekly, now),
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

fn window_cell(w: &Option<Window>, now: DateTime<Utc>) -> Cell {
    match w {
        // データ無し: 色なしのプレースホルダ("—")。
        None => Cell::new("—"),
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
            Cell::new(text).fg(tlevel(w.used_percent))
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
    let accounts = sorted_refs(&report.accounts, sort, now, None);
    let out = OrderedReport {
        generated_at: &report.generated_at,
        accounts,
    };
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

// ===== Statusline(compact / colored / 1 account 1 line) ======================

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
    };
    let name = display_name(
        a.label.as_deref(),
        row_email,
        a.profile_email.as_deref(),
        &a.profile,
    );
    let mut s = String::from("  ");
    // provider marker は `--logos` なら brand-logo glyph、そうでなければ text label。
    if opts.logos {
        let (logo, logo_color) = match a.provider {
            Provider::Claude => (CLAUDE_LOGO, brand_sgr(a.provider)),
            // Codex mark は teal の brand color より white の方が読みやすい。
            Provider::Codex => (CODEX_LOGO, CODEX_LOGO_COLOR.to_string()),
            Provider::Antigravity => (ANTIGRAVITY_LOGO, brand_sgr(a.provider)),
        };
        s += &paint(opts.color, &logo_color, &format!("{logo}  "));
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

    // ===== SortKey =====
    //
    // sorted_refs は AccountOut / AccountReport 双方の SortableRow 実装で
    // 共有しているため、テストは AccountOut ベースで挙動を確認しつつ、
    // AccountReport 側のキー抽出が正しいことも 1 件だけクロスチェックする。

    fn out(
        profile: &str,
        provider: Provider,
        weekly_pct: Option<f64>,
        weekly_resets_at: Option<&str>,
    ) -> AccountOut {
        let resets_in = weekly_resets_at.and_then(parse_utc).map(|r| {
            let now = fixed_utc("2026-06-15T00:00:00Z");
            (r - now).num_seconds().max(0)
        });
        AccountOut {
            profile: profile.to_string(),
            provider,
            ok: true,
            plan: None,
            email: None,
            profile_email: None,
            label: None,
            group_label: None,
            five_hour: None,
            weekly: weekly_pct.map(|p| WindowOut {
                used_percent: p,
                resets_at: weekly_resets_at.map(str::to_string),
                resets_in_seconds: resets_in,
            }),
            error: None,
        }
    }

    fn profile_order<'a>(rows: &[&'a AccountOut]) -> Vec<&'a str> {
        rows.iter().map(|x| x.profile.as_str()).collect()
    }

    #[test]
    fn sort_weekly_usage_orders_descending_by_used_percent() {
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let items = vec![
            out("a", Provider::Claude, Some(10.0), None),
            out("b", Provider::Codex, Some(80.0), None),
            out("c", Provider::Antigravity, Some(50.0), None),
        ];
        let sorted = sorted_refs(&items, SortKey::WeeklyUsage, now, None);
        assert_eq!(profile_order(&sorted), vec!["b", "c", "a"]);
    }

    #[test]
    fn sort_weekly_usage_puts_missing_rows_last() {
        // weekly 無し(取得失敗 / プラン未開示など) の行は末尾に固定。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let items = vec![
            out("a", Provider::Claude, Some(80.0), None),
            out("b", Provider::Codex, None, None),
            out("c", Provider::Antigravity, Some(20.0), None),
        ];
        let sorted = sorted_refs(&items, SortKey::WeeklyUsage, now, None);
        assert_eq!(profile_order(&sorted), vec!["a", "c", "b"]);
    }

    #[test]
    fn sort_weekly_reset_orders_ascending_by_resets_at() {
        // resets_at から `now` を起点に残秒数を逆算するため、キャッシュ
        // JSON の resets_in_seconds(古い値) ではなく resets_at が正解。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let items = vec![
            out(
                "a",
                Provider::Claude,
                Some(50.0),
                Some("2026-06-17T00:00:00Z"),
            ),
            out(
                "b",
                Provider::Codex,
                Some(50.0),
                Some("2026-06-16T00:00:00Z"),
            ),
            out(
                "c",
                Provider::Antigravity,
                Some(50.0),
                Some("2026-06-20T00:00:00Z"),
            ),
        ];
        let sorted = sorted_refs(&items, SortKey::WeeklyReset, now, None);
        assert_eq!(profile_order(&sorted), vec!["b", "a", "c"]);
    }

    #[test]
    fn sort_weekly_reset_puts_missing_rows_last() {
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let items = vec![
            out("a", Provider::Claude, Some(50.0), None), // resets_at 無し
            out(
                "b",
                Provider::Codex,
                Some(50.0),
                Some("2026-06-16T00:00:00Z"),
            ),
            out(
                "c",
                Provider::Antigravity,
                Some(50.0),
                Some("2026-06-20T00:00:00Z"),
            ),
        ];
        let sorted = sorted_refs(&items, SortKey::WeeklyReset, now, None);
        assert_eq!(profile_order(&sorted), vec!["b", "c", "a"]);
    }

    #[test]
    fn sort_provider_preserves_input_order_without_default_cmp() {
        // SortKey::Provider + default_cmp None → 入力順そのまま(table/json 用)。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let items = vec![
            out("alpha", Provider::Codex, None, None),
            out("beta", Provider::Claude, None, None),
            out("gamma", Provider::Antigravity, None, None),
        ];
        let sorted = sorted_refs(&items, SortKey::Provider, now, None);
        assert_eq!(profile_order(&sorted), vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn sort_provider_applies_default_cmp_for_statusline() {
        // SortKey::Provider + statusline_default_cmp → Claude → Codex → AGY。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let items = vec![
            out("alpha", Provider::Codex, None, None),
            out("beta", Provider::Claude, None, None),
            out("gamma", Provider::Antigravity, None, None),
        ];
        let sorted = sorted_refs(&items, SortKey::Provider, now, Some(statusline_default_cmp));
        assert_eq!(profile_order(&sorted), vec!["beta", "alpha", "gamma"]);
    }

    #[test]
    fn sort_tiebreaks_on_profile_name() {
        // 使用率が同じ → profile 名昇順で安定。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let items = vec![
            out("zeta", Provider::Claude, Some(50.0), None),
            out("alpha", Provider::Codex, Some(50.0), None),
        ];
        let sorted = sorted_refs(&items, SortKey::WeeklyUsage, now, None);
        assert_eq!(profile_order(&sorted), vec!["alpha", "zeta"]);
    }

    #[test]
    fn account_report_sortable_pulls_weekly_from_usage() {
        // AccountReport 側の SortableRow 実装も同じ順序を返すか確認。
        use crate::model::{Usage, Window};
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let mk = |name: &str, prov: Provider, pct: Option<f64>, resets_at: Option<&str>| {
            let weekly = pct.map(|p| Window {
                used_percent: p,
                resets_at: resets_at.and_then(parse_utc),
            });
            AccountReport {
                profile_name: name.to_string(),
                profile_email: None,
                label: None,
                provider: prov,
                group_label: None,
                usage: Ok(Usage {
                    weekly,
                    ..Usage::default()
                }),
            }
        };
        let items = vec![
            mk(
                "a",
                Provider::Claude,
                Some(10.0),
                Some("2026-06-20T00:00:00Z"),
            ),
            mk(
                "b",
                Provider::Codex,
                Some(80.0),
                Some("2026-06-16T00:00:00Z"),
            ),
            mk("c", Provider::Antigravity, None, None),
        ];
        // 週枠使用率降順 → b, a, c。
        let usage_sorted = sorted_refs(&items, SortKey::WeeklyUsage, now, None);
        assert_eq!(
            usage_sorted
                .iter()
                .map(|r| r.profile_name.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a", "c"]
        );
        // リセット時刻昇順 → b(1日後), a(5日後), c(欠損)。
        let reset_sorted = sorted_refs(&items, SortKey::WeeklyReset, now, None);
        assert_eq!(
            reset_sorted
                .iter()
                .map(|r| r.profile_name.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a", "c"]
        );
    }
}
