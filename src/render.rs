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

fn matches_active(active: Option<&str>, row_email: Option<&str>) -> bool {
    active
        .zip(row_email)
        .is_some_and(|(want, got)| want.eq_ignore_ascii_case(got))
}

/// Brand color for a provider, as a comfy-table truecolor (table use).
fn provider_color(p: Provider) -> Color {
    match p {
        Provider::Claude => Color::Rgb {
            r: 217,
            g: 119,
            b: 87,
        }, // Anthropic coral #D97757
        Provider::Codex => Color::Rgb {
            r: 16,
            g: 163,
            b: 127,
        }, // OpenAI teal #10A37F
        Provider::Antigravity => Color::Rgb {
            r: 66,
            g: 133,
            b: 244,
        }, // Google blue #4285F4
    }
}

/// Service-column text: the provider, plus the model-group for Antigravity rows.
fn service_label(p: Provider, group: Option<&str>) -> String {
    match group {
        Some(g) => format!("{} · {}", p.label(), g),
        None => p.label().to_string(),
    }
}

// ===== Human-readable table =================================================

pub fn table(reports: &[AccountReport], active_email: Option<&str>) {
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
        let active =
            matches!(r.provider, Provider::Claude) && matches_active(active_email, row_email);
        let mut name_cell = Cell::new(&name);
        if active {
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
const ACCOUNT: &str = "38;5;147"; // lavender — fallback provider label
const CLAUDE_BRAND: &str = "38;2;217;119;87"; // Anthropic coral #D97757
const CODEX_BRAND: &str = "38;2;16;163;127"; // OpenAI teal #10A37F
const ANTIGRAVITY_BRAND: &str = "38;2;66;133;244"; // Google blue #4285F4
const CODEX_LOGO_COLOR: &str = "38;2;255;255;255"; // white — Codex brand logo
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
pub fn statusline(report: &Report, active_email: Option<&str>, color: bool, logos: bool) {
    let now = Utc::now();
    let mut rows: Vec<&AccountOut> = report.accounts.iter().collect();
    rows.sort_by(|a, b| {
        prov_rank(&a.provider)
            .cmp(&prov_rank(&b.provider))
            .then_with(|| a.profile.cmp(&b.profile))
    });
    let lines: Vec<String> = rows
        .iter()
        .map(|a| {
            let row_email = a.email.as_deref().or(a.profile_email.as_deref());
            // This is the Claude-side statusline, so only the active Claude
            // account is highlighted; the Codex row of the same email stays neutral.
            let active = a.provider == "claude" && matches_active(active_email, row_email);
            render_row(a, row_email, active, color, logos, now)
        })
        .collect();
    print!("{}", lines.join("\n"));
}

fn prov_rank(p: &str) -> u8 {
    match p {
        "claude" => 0,
        "codex" => 1,
        _ => 2,
    }
}

fn render_row(
    a: &AccountOut,
    row_email: Option<&str>,
    active: bool,
    color: bool,
    logos: bool,
    now: DateTime<Utc>,
) -> String {
    let prov = match a.provider.as_str() {
        "claude" => "Claude",
        "codex" => "Codex",
        "antigravity" => "AGY",
        other => other,
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
        let (logo, logo_color) = match a.provider.as_str() {
            "claude" => (CLAUDE_LOGO, CLAUDE_BRAND),
            "codex" => (CODEX_LOGO, CODEX_LOGO_COLOR),
            "antigravity" => (ANTIGRAVITY_LOGO, ANTIGRAVITY_BRAND),
            _ => ("", ACCOUNT),
        };
        s += &paint(color, logo_color, &format!("{logo}  "));
    } else {
        let brand = match a.provider.as_str() {
            "claude" => CLAUDE_BRAND,
            "codex" => CODEX_BRAND,
            "antigravity" => ANTIGRAVITY_BRAND,
            _ => ACCOUNT,
        };
        s += &paint(color, brand, &format!("{prov:<6} "));
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
        s += &window_seg(color, "5h", None, now, FIVE_H_TH);
        s += "   ";
        s += &window_seg(color, "1w", None, now, WEEK_TH);
        return s;
    }
    s += &window_seg(color, "5h", a.five_hour.as_ref(), now, FIVE_H_TH);
    s += "   ";
    s += &window_seg(color, "1w", a.weekly.as_ref(), now, WEEK_TH);
    s
}

fn window_seg(
    color: bool,
    label: &str,
    w: Option<&WindowOut>,
    now: DateTime<Utc>,
    th: [i64; 3],
) -> String {
    let mut s = paint(color, GRAY, &format!("{label} "));
    match w {
        None => {
            // データ無し: 空ゲージ + "--"(% なし) + "--"(残り時間)。
            // Some 分岐と同じ桁幅(16 + 1 + 4 + 2 + 5 = 28)に揃える。
            s += &paint(color, DIM, &"░".repeat(16));
            s += " ";
            s += &paint(color, DIM, &format!("{:>4}", "--"));
            s += "  ";
            s += &paint(color, DIM, &format!("{:<5}", "--"));
        }
        Some(w) => {
            s += &gauge(color, w.used_percent, 16);
            s += " ";
            s += &paint(
                color,
                pct_code(w.used_percent),
                &format!("{:>3}%", w.used_percent.round() as i64),
            );
            s += "  ";
            let rem = w
                .resets_at
                .as_deref()
                .and_then(parse_utc)
                .map(|r| (r - now).num_seconds());
            match rem {
                Some(sec) if sec > 0 => {
                    s += &paint(
                        color,
                        reset_code(sec, th),
                        &format!("{:<5}", compact_dur(sec)),
                    )
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
