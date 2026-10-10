//! human-readable table 出力(default の表示形式)。

use chrono::{DateTime, Duration, Local, Utc};
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, Color, ContentArrangement, Table};

use super::manual_resets::format_resets;
use super::sort::sorted_refs;
use super::{brand_rgb, display_name};
use crate::SortKey;
use crate::model::{AccountReport, Provider, Window};
use crate::report::ManualResetOut;

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

pub fn table(reports: &[AccountReport], sort: SortKey, color: bool) -> std::io::Result<()> {
    let now = Utc::now();
    let table = build_table(reports, sort, color, now);
    super::write_stdout(&format!(
        "{table}\n  updated {} · bars = usage, time = until reset\n",
        now.with_timezone(&Local).format("%H:%M")
    ))
}

/// 端末幅による自動調整を保ったまま、色の要否に応じてセルを組み立てる。
fn build_table(reports: &[AccountReport], sort: SortKey, color: bool, now: DateTime<Utc>) -> Table {
    let mut table = Table::new();
    let show_resets = reports
        .iter()
        .any(|r| r.usage.as_ref().is_ok_and(|u| u.manual_resets.is_some()));
    let mut header = vec!["Account", "Service", "Plan", "Short window", "Long window"];
    if show_resets {
        header.push("Manual resets");
    }
    table
        .load_style(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(header);

    // SortKey::Provider のときは入力(=ジョブ順)をそのまま保持。
    let ordered = sorted_refs(reports, sort, now, None);
    for r in ordered {
        let provider_email = match &r.usage {
            Ok(u) => u.email.as_deref(),
            Err(_) => None,
        };
        let name = display_name(
            r.label.as_deref(),
            provider_email,
            r.profile_email.as_deref(),
            &r.profile_name,
        );
        let name_cell = Cell::new(&name);

        match &r.usage {
            Ok(u) => {
                // 短期 / 長期のどちらか一方しかない行は、存在する側のバーを横長にする。
                // comfy-table は colspan を持たないため、空側セルは "—" のまま。
                let (short_bar_width, long_bar_width) =
                    bar_widths(u.short.is_some(), u.long.is_some());
                let mut cells = vec![
                    name_cell,
                    tint(
                        Cell::new(service_label(r.provider, r.group_label.as_deref())),
                        color,
                        provider_color(r.provider),
                    ),
                    Cell::new(u.plan.as_deref().unwrap_or("—")),
                    window_cell(&u.short, now, short_bar_width, color),
                    window_cell(&u.long, now, long_bar_width, color),
                ];
                if show_resets {
                    let text = u
                        .manual_resets
                        .as_ref()
                        .map(|resets| {
                            let resets: Vec<_> = resets.iter().map(ManualResetOut::from).collect();
                            format_resets(&resets, now, true)
                        })
                        .unwrap_or_else(|| "—".into());
                    cells.push(Cell::new(text));
                }
                table.add_row(cells);
            }
            Err(e) => {
                let msg: String = format!("{e:#}").chars().take(150).collect();
                let mut cells = vec![
                    name_cell,
                    tint(
                        Cell::new(service_label(r.provider, r.group_label.as_deref())),
                        color,
                        provider_color(r.provider),
                    ),
                    Cell::new("—"),
                    tint(Cell::new(format!("⚠ {msg}")), color, Color::DarkGrey),
                    Cell::new(""),
                ];
                if show_resets {
                    cells.push(Cell::new("—"));
                }
                table.add_row(cells);
            }
        }
    }

    table
}

fn tint(cell: Cell, color: bool, fg: Color) -> Cell {
    if color { cell.fg(fg) } else { cell }
}

/// 通常セルの gauge 幅(旧 tbar と同じ)。
const NORMAL_BAR_WIDTH: usize = 10;
/// quota が 1 枠だけの行に使う横長 gauge 幅。
const WIDE_BAR_WIDTH: usize = 24;

fn bar_widths(has_short: bool, has_long: bool) -> (usize, usize) {
    match (has_short, has_long) {
        (true, false) => (WIDE_BAR_WIDTH, NORMAL_BAR_WIDTH),
        (false, true) => (NORMAL_BAR_WIDTH, WIDE_BAR_WIDTH),
        _ => (NORMAL_BAR_WIDTH, NORMAL_BAR_WIDTH),
    }
}

fn window_cell(w: &Option<Window>, now: DateTime<Utc>, bar_width: usize, color: bool) -> Cell {
    match w {
        // データ無し: 色なしのプレースホルダ("—")。
        None => Cell::new("—"),
        Some(w) => {
            let reset = w
                .resets_at
                .map(|r| humanize(r - now))
                .unwrap_or_else(|| "—".to_string());
            let text = format!(
                "{} {}  {:>3}%  · {}",
                w.kind.label(),
                tbar(w.used_percent, bar_width),
                w.used_percent.round() as i64,
                reset
            );
            tint(Cell::new(text.trim_start()), color, tlevel(w.used_percent))
        }
    }
}

fn tbar(pct: f64, width: usize) -> String {
    let filled = ((pct / 100.0) * width as f64)
        .round()
        .clamp(0.0, width as f64) as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
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
    // 1〜59 秒だけを最低1分として扱い、以降の従来の切り捨て表示は維持する。
    let display_seconds = s.max(60);
    let (days, hours, mins) = (
        display_seconds / 86400,
        (display_seconds % 86400) / 3600,
        (display_seconds % 3600) / 60,
    );
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
    fn table_respects_color_for_success_and_error_cells() {
        let now = Utc::now();
        let report = |usage| AccountReport {
            profile_name: "Work".into(),
            profile_email: None,
            label: None,
            provider: Provider::Claude,
            group_label: None,
            usage,
        };
        let rows = vec![
            report(Ok(crate::model::Usage {
                short: Some(Window {
                    kind: crate::model::WindowKind::FiveHour,
                    used_percent: 90.0,
                    resets_at: None,
                }),
                ..Default::default()
            })),
            report(Err(anyhow::anyhow!("test failure"))),
        ];
        for color in [false, true] {
            let mut table = build_table(&rows, SortKey::Provider, color, now);
            // 強制描画で、テスト実行時の TTY の有無に左右されず ANSI の有無を検証する。
            table.enforce_styling();
            assert_eq!(table.to_string().contains('\x1b'), color);
        }
    }

    #[test]
    fn manual_resets_column_appears_only_when_a_row_reports_resets() {
        use crate::model::{ManualReset, ResetKind, Usage};
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let report = |provider, usage| AccountReport {
            profile_name: "Work".into(),
            profile_email: None,
            label: None,
            provider,
            group_label: None,
            usage,
        };
        let render = |rows: &[AccountReport]| {
            let mut table = build_table(rows, SortKey::Provider, false, now);
            // テストを実行する端末の幅に左右されず、セルを折り返さずに描画する。
            table.set_width(500);
            table.lines().collect::<Vec<_>>()
        };
        let line = |lines: &[String], text: &str| {
            lines
                .iter()
                .find(|line| line.contains(text))
                .cloned()
                .unwrap_or_else(|| panic!("{text:?} が無い: {lines:#?}"))
        };
        // 手動リセットを持つ行が無ければ列自体を出さない。
        let without = render(&[
            report(Provider::PixelLab, Ok(Usage::default())),
            report(Provider::Claude, Err(anyhow::anyhow!("test failure"))),
        ]);
        assert!(
            !without.iter().any(|line| line.contains("Manual resets")),
            "{without:#?}"
        );

        // 1 行でも持てば列を足し、持たない行と失敗行は "—" で埋める。
        let expiry = fixed_utc("2026-06-20T00:00:00Z");
        let with = render(&[
            report(
                Provider::Codex,
                Ok(Usage {
                    manual_resets: Some(vec![ManualReset {
                        kind: ResetKind::Full,
                        remaining: Some(2),
                        expires_at: Some(expiry),
                        paused: false,
                    }]),
                    ..Default::default()
                }),
            ),
            report(Provider::PixelLab, Ok(Usage::default())),
            report(Provider::Claude, Err(anyhow::anyhow!("test failure"))),
        ]);
        line(&with, "Manual resets");
        let date = expiry.with_timezone(&Local).format("%m/%d %H:%M");
        assert!(
            line(&with, "Codex").contains(&format!("full 2 (2@{date})")),
            "{with:#?}"
        );
        for text in ["PixelLab", "test failure"] {
            let row = line(&with, text);
            let last_cell = row.trim_end_matches('│').rsplit('┆').next().unwrap();
            assert_eq!(last_cell.trim(), "—", "{with:#?}");
        }
    }

    #[test]
    fn humanize_rounds_appropriately() {
        // 0以下 は "now"、それ以上は単位ごとに丸める。
        assert_eq!(humanize(Duration::seconds(0)), "now");
        assert_eq!(humanize(Duration::seconds(-100)), "now");
        assert_eq!(humanize(Duration::seconds(1)), "in 1m");
        assert_eq!(humanize(Duration::seconds(59)), "in 1m");
        assert_eq!(humanize(Duration::seconds(60)), "in 1m");
        assert_eq!(humanize(Duration::seconds(61)), "in 1m");
        assert_eq!(humanize(Duration::seconds(3599)), "in 59m");
        assert_eq!(humanize(Duration::minutes(5)), "in 5m");
        assert_eq!(humanize(Duration::minutes(125)), "in 2h 5m");
        assert_eq!(humanize(Duration::hours(25)), "in 1d 1h");
    }

    #[test]
    fn tbar_clamps_extremes() {
        // 0% は全 ░、100% は全 █、超過/欠損もパニックせず clamp される。標準 10 幅。
        assert_eq!(tbar(0.0, 10), "░".repeat(10));
        assert_eq!(tbar(100.0, 10), "█".repeat(10));
        assert_eq!(tbar(150.0, 10), "█".repeat(10));
        assert_eq!(tbar(-10.0, 10), "░".repeat(10));
    }

    #[test]
    fn tbar_width_controls_length() {
        // WIDE_BAR_WIDTH (merged 用) でも同じ挙動。
        assert_eq!(tbar(0.0, WIDE_BAR_WIDTH), "░".repeat(WIDE_BAR_WIDTH));
        assert_eq!(tbar(100.0, WIDE_BAR_WIDTH), "█".repeat(WIDE_BAR_WIDTH));
        // 50% は width/2 の █ を返す(width が偶数のとき厳密に半々)。
        let half = tbar(50.0, 24);
        assert_eq!(half.chars().filter(|c| *c == '█').count(), 12);
        assert_eq!(half.chars().filter(|c| *c == '░').count(), 12);
    }

    #[test]
    fn single_window_expands_on_its_own_side() {
        assert_eq!(bar_widths(true, false), (WIDE_BAR_WIDTH, NORMAL_BAR_WIDTH));
        assert_eq!(bar_widths(false, true), (NORMAL_BAR_WIDTH, WIDE_BAR_WIDTH));
        assert_eq!(bar_widths(true, true), (NORMAL_BAR_WIDTH, NORMAL_BAR_WIDTH));
    }

    #[test]
    fn service_label_appends_group() {
        assert_eq!(service_label(Provider::Claude, None), "Claude");
        assert_eq!(
            service_label(Provider::Antigravity, Some("Gemini")),
            "Antigravity · Gemini"
        );
    }

    #[test]
    fn window_cell_uses_kind_badge_and_omits_when_none() {
        // WindowKind の badge が text 先頭に付く。空データは "—" のまま。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let w = Some(Window {
            kind: crate::model::WindowKind::Monthly,
            used_percent: 46.0,
            resets_at: Some(fixed_utc("2026-06-20T15:00:00Z")),
        });
        let cell = window_cell(&w, now, NORMAL_BAR_WIDTH, true).content();
        assert!(cell.starts_with("1m"), "expected 1m badge in {cell:?}");
        assert!(cell.contains("46%"));

        // データ無しは幅指定に関わらず "—" のまま(桁ズレさせない)。
        assert_eq!(
            window_cell(&None, now, NORMAL_BAR_WIDTH, true).content(),
            "—"
        );
        assert_eq!(window_cell(&None, now, WIDE_BAR_WIDTH, true).content(), "—");
    }

    #[test]
    fn window_cell_wide_bar_matches_requested_width() {
        // merged 行(5h 無し)向けに、gauge 文字数が WIDE_BAR_WIDTH と一致する。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let w = Some(Window {
            kind: crate::model::WindowKind::Monthly,
            used_percent: 50.0,
            resets_at: Some(fixed_utc("2026-06-20T00:00:00Z")),
        });
        let cell = window_cell(&w, now, WIDE_BAR_WIDTH, true)
            .content()
            .to_string();
        let gauge_glyphs = cell.chars().filter(|c| *c == '█' || *c == '░').count();
        assert_eq!(gauge_glyphs, WIDE_BAR_WIDTH);
    }

    fn fixed_utc(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }
}
