//! human-readable table 出力(default の表示形式)。

use chrono::{DateTime, Duration, Local, Utc};
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Attribute, Cell, Color, ContentArrangement, Table};

use super::sort::sorted_refs;
use super::{ActiveTarget, brand_rgb, display_name, resolve_active};
use crate::SortKey;
use crate::model::{AccountReport, Provider, Window};

/// provider の brand color を comfy-table truecolor として返す(table 用)。
fn provider_color(p: Provider) -> Color {
    let (r, g, b) = brand_rgb(p);
    Color::Rgb { r, g, b }
}

/// service column の text。provider に、Antigravity 行では model-group を付ける。
fn service_label(p: Provider, group: Option<&str>) -> String {
    match group {
        Some(g) => format!("{} · {}", p.label(), g),
        None => p.label().to_string(),
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn service_label_appends_group() {
        assert_eq!(service_label(Provider::Claude, None), "Claude");
        assert_eq!(
            service_label(Provider::Antigravity, Some("Gemini")),
            "Antigravity · Gemini"
        );
    }
}
