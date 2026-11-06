//! human-readable table 出力(default の表示形式)。

use chrono::{DateTime, Duration, Local, Utc};
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, Color, ContentArrangement, Table};

use super::manual_resets::format_resets;
use super::palette::brand_rgb;
use super::sort::sorted_refs;
use super::{UNKNOWN_USAGE, display_name, limit_observation_age};
use crate::SortKey;
use crate::model::{AccountReport, Provider, Usage, Window};
use crate::report::{LimitObservationOut, ManualResetOut};

/// provider の brand color を comfy-table truecolor として返す(table 用)。
fn provider_color(p: Provider) -> Color {
    let (r, g, b) = brand_rgb(p);
    Color::Rgb { r, g, b }
}

/// 手動リセットの列に出す値。失敗した行と、表示する種類が無い行は None。
fn manual_reset_text(report: &AccountReport, now: DateTime<Utc>) -> Option<String> {
    let resets: Vec<_> = report
        .usage
        .as_ref()
        .ok()?
        .manual_resets
        .as_ref()?
        .iter()
        .map(ManualResetOut::from)
        .collect();
    format_resets(&resets, now, true)
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
    // SortKey::Provider のときは入力(=ジョブ順)をそのまま保持。
    let ordered = sorted_refs(reports, sort, now, None);
    // 列の有無とセルに同じ値を使うため、手動リセットの表示は行ごとに 1 回だけ組み立てる。
    let resets: Vec<_> = ordered.iter().map(|r| manual_reset_text(r, now)).collect();
    let show_resets = resets.iter().any(Option::is_some);
    let mut header = vec!["Account", "Service", "Plan", "Short window", "Long window"];
    if show_resets {
        header.push("Manual resets");
    }
    table
        .load_style(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(header);

    for (r, reset_text) in ordered.into_iter().zip(resets) {
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
                    short_window_cell(u, now, short_bar_width, color),
                    window_cell(&u.long, now, long_bar_width, color),
                ];
                if show_resets {
                    cells.push(Cell::new(reset_text.as_deref().unwrap_or("—")));
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

/// 短期枠の列。Grok のモデル別 24 時間枠で拒否された記録があれば、使用率は不明と書いて
/// 拒否の経過時間を添える。拒否は過去の出来事なので、「いま制限中」と読める赤ではなく黄
/// (テーブルの使用率 60〜85% と同じ名前付きの黄) にする。
/// 短期枠そのものがある行では枠を隠さず、記録を 2 行目に足す。comfy-table はセル全体に
/// 1 色しか付けられない (行ごとの色には custom_styling 機能と追加の依存が要る) ので、
/// このときはセルに色を付けない。付けると、記録が短期枠の段階色 (90% なら赤) になる。
fn short_window_cell(u: &Usage, now: DateTime<Utc>, bar_width: usize, color: bool) -> Cell {
    let record = u
        .limit_observation
        .as_ref()
        .map(LimitObservationOut::from)
        .and_then(|observation| {
            let age = limit_observation_age(&observation, now)?;
            Some(format!(
                "24h {} · {UNKNOWN_USAGE} · refused {age} ago",
                observation.model
            ))
        });
    match (record, &u.short) {
        (None, short) => window_cell(short, now, bar_width, color),
        (Some(record), None) => tint(Cell::new(record), color, Color::Yellow),
        (Some(record), Some(short)) => {
            let (text, _) = window_text(short, now, bar_width);
            Cell::new(format!("{text}\n{record}"))
        }
    }
}

fn window_cell(w: &Option<Window>, now: DateTime<Utc>, bar_width: usize, color: bool) -> Cell {
    match w {
        // データ無し: 色なしのプレースホルダ("—")。
        None => Cell::new("—"),
        Some(w) => {
            let (text, color_code) = window_text(w, now, bar_width);
            tint(Cell::new(text), color, color_code)
        }
    }
}

/// 1 枠ぶんの文字と色。使用率が不明な枠は、消灯したバーと `--%` (0% と見分けにくい) の
/// 代わりに `usage unknown` と書く。
fn window_text(w: &Window, now: DateTime<Utc>, bar_width: usize) -> (String, Color) {
    let reset = w
        .resets_at
        .map(|r| humanize(r - now))
        .unwrap_or_else(|| "—".to_string());
    let (usage, color_code) = match w.used_percent {
        Some(percent) => (
            format!(
                "{}  {:>3}%",
                tbar(percent, bar_width),
                percent.round() as i64
            ),
            tlevel(percent),
        ),
        None => (UNKNOWN_USAGE.to_string(), Color::DarkGrey),
    };
    let text = format!("{} {usage}  · {reset}", w.kind.label());
    (text.trim_start().to_string(), color_code)
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
                    used_percent: Some(90.0),
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
    fn manual_resets_column_appears_only_when_a_row_has_resets_to_show() {
        use crate::model::{ManualReset, ResetKind, Usage};
        let now = fixed_utc("2026-06-15T00:00:00Z");
        // 残りが 0 回の付与と、描画の時点で失効した付与だけを持つ行。
        let zero = || Usage {
            manual_resets: Some(vec![
                ManualReset {
                    kind: ResetKind::Full,
                    remaining: Some(0),
                    expires_at: None,
                    paused: false,
                },
                ManualReset {
                    kind: ResetKind::Full,
                    remaining: Some(3),
                    expires_at: Some(now),
                    paused: false,
                },
            ]),
            ..Default::default()
        };
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
        // 出す手動リセットを持つ行が無ければ、0 回だけの行があっても列自体を出さない。
        let without = render(&[
            report(Provider::PixelLab, Ok(Usage::default())),
            report(Provider::Claude, Ok(zero())),
            report(Provider::Claude, Err(anyhow::anyhow!("test failure"))),
        ]);
        assert!(
            !without.iter().any(|line| line.contains("Manual resets")),
            "{without:#?}"
        );

        // 取得できなかった回数は 0 回と区別して出すため、それだけでも列を足す。
        let unknown = render(&[report(
            Provider::Claude,
            Ok(Usage {
                manual_resets: Some(vec![ManualReset::unknown(ResetKind::Full)]),
                ..Default::default()
            }),
        )]);
        line(&unknown, "Manual resets");
        assert!(line(&unknown, "Claude").contains("full ?"), "{unknown:#?}");

        // 1 行でも出すものがあれば列を足し、ほかの行は "—" で埋める。
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
            report(Provider::Claude, Ok(zero())),
            report(Provider::PixelLab, Ok(Usage::default())),
            report(Provider::Claude, Err(anyhow::anyhow!("test failure"))),
        ]);
        line(&with, "Manual resets");
        let date = expiry.with_timezone(&Local).format("%m/%d %H:%M");
        assert!(
            line(&with, "Codex").contains(&format!("full 2 (2@{date})")),
            "{with:#?}"
        );
        // 0 回だけの行・手動リセットを持たない行・取得に失敗した行。
        let rows: Vec<_> = with.iter().filter(|line| line.contains("Work")).collect();
        assert_eq!(rows.len(), 4, "{with:#?}");
        for row in rows.iter().filter(|line| !line.contains("Codex")) {
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
            used_percent: Some(46.0),
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
    fn unknown_percent_keeps_period_and_reset_without_claiming_zero() {
        let now = fixed_utc("2026-10-10T00:00:00Z");
        let window = Some(Window {
            kind: crate::model::WindowKind::Weekly,
            used_percent: None,
            resets_at: Some(now + Duration::days(5)),
        });
        let cell = window_cell(&window, now, NORMAL_BAR_WIDTH, false)
            .content()
            .to_string();
        // 消灯したバーと `--%` は 0% と見分けにくいので、不明と文字で書く。
        assert_eq!(cell, "1w usage unknown  · in 5d 0h");
        assert!(!cell.contains('%') && !cell.contains('░'), "{cell}");
    }

    #[test]
    fn grok_refusal_is_history_next_to_unknown_usage() {
        let now = fixed_utc("2026-11-06T01:30:00Z");
        let refusal = |minutes_ago| crate::model::LimitObservation {
            model: "grok-test".into(),
            observed_at: now - Duration::minutes(minutes_ago),
            used_tokens: 615_636,
            limit_tokens: 600_000,
        };
        let mut usage = Usage {
            plan: Some("Free".into()),
            limit_observation: Some(refusal(141)),
            ..Usage::default()
        };
        let cell = short_window_cell(&usage, now, NORMAL_BAR_WIDTH, false)
            .content()
            .to_string();
        assert_eq!(cell, "24h grok-test · usage unknown · refused 2h ago");
        assert!(!cell.contains("LIMIT"), "{cell}");

        // 短期枠がある行では枠を隠さず、記録を 2 行目に足す。
        usage.short = Some(Window {
            kind: crate::model::WindowKind::Daily,
            used_percent: Some(30.0),
            resets_at: Some(now + Duration::hours(3)),
        });
        let cell = short_window_cell(&usage, now, NORMAL_BAR_WIDTH, false)
            .content()
            .to_string();
        let (window, record) = cell.split_once('\n').expect("2 行");
        assert!(
            window.starts_with("1d ") && window.contains("30%"),
            "{cell}"
        );
        assert_eq!(record, "24h grok-test · usage unknown · refused 2h ago");

        // 色を付けて描いたとき、記録は黄。短期枠と同じセルに並ぶときはセルに色を付けず、
        // 記録が短期枠の段階色 (90% の赤) にならない。
        let render = |short: Option<Window>| {
            let usage = Usage {
                plan: Some("Free".into()),
                short,
                limit_observation: Some(refusal(141)),
                ..Usage::default()
            };
            let row = AccountReport {
                profile_name: "Grok".into(),
                profile_email: None,
                label: None,
                provider: Provider::Grok,
                group_label: None,
                usage: Ok(usage),
            };
            let mut table = build_table(&[row], SortKey::Provider, true, now);
            table.enforce_styling();
            table.to_string()
        };
        // comfy-table はセルの左右の余白ごと色で囲む。
        let alone = render(None);
        assert!(
            alone.contains("\x1b[38;5;11m 24h grok-test · usage unknown"),
            "{alone:?}"
        );
        let combined = render(Some(Window {
            kind: crate::model::WindowKind::Daily,
            used_percent: Some(90.0),
            resets_at: Some(now + Duration::hours(3)),
        }));
        assert!(
            combined.contains("24h grok-test · usage unknown"),
            "{combined:?}"
        );
        assert!(!combined.contains("m 24h grok-test"), "{combined:?}");
        assert!(!combined.contains("m 1d "), "{combined:?}");
        assert!(!combined.contains("\x1b[38;5;9m"), "{combined:?}");

        // 24 時間を過ぎた記録は出さない (使えるようになったとは書かない)。
        usage.short = None;
        usage.limit_observation = Some(refusal(24 * 60 + 1));
        assert_eq!(
            short_window_cell(&usage, now, NORMAL_BAR_WIDTH, false).content(),
            "—"
        );
    }

    #[test]
    fn window_cell_wide_bar_matches_requested_width() {
        // merged 行(5h 無し)向けに、gauge 文字数が WIDE_BAR_WIDTH と一致する。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let w = Some(Window {
            kind: crate::model::WindowKind::Monthly,
            used_percent: Some(50.0),
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
