//! TUI の最下部 (キー操作の説明の上) に置くディスク欄。

use std::cmp::Reverse;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::super::statusline::pad_display;
use crate::disk::{Monitor, Row, Volume};

/// ディスク欄を出しても、アカウント一覧に残す行数 (枠を含む)。見出しと 2 つの利用枠が収まる。
const MIN_ACCOUNT_ROWS: u16 = 6;
/// ボリューム名の欄の最大幅。
const MAX_NAME_WIDTH: usize = 16;
const MIN_BAR: usize = 6;
/// 説明を足すために、これより短くはしないバーの幅。狭い端末では `MIN_BAR` まで縮める。
const COMFORT_BAR: usize = 16;
const MAX_BAR: usize = 40;
const LEGEND: &str = "Disk · █ used ▒ purgeable ░ free";

/// ディスク欄の置き方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Placement {
    Hidden,
    /// 枠なしの 1 行。先頭のボリューム (起動ディスク) だけを出す。
    Line,
    /// 枠付きの欄に、先頭から指定の行数を出す。
    Panel(u16),
}

impl Placement {
    pub(super) fn height(self) -> u16 {
        match self {
            Placement::Hidden => 0,
            Placement::Line => 1,
            Placement::Panel(rows) => rows + 2,
        }
    }
}

/// アカウント一覧に `MIN_ACCOUNT_ROWS` 行を残せる置き方を選ぶ。足りなければ一覧を優先する。
pub(super) fn placement(volumes: usize, height: u16, header: u16, footer: u16) -> Placement {
    if volumes == 0 {
        return Placement::Hidden;
    }
    let spare = height.saturating_sub(header + footer + MIN_ACCOUNT_ROWS);
    if spare >= 3 {
        Placement::Panel(u16::try_from(volumes).unwrap_or(u16::MAX).min(spare - 2))
    } else if spare >= 1 {
        Placement::Line
    } else {
        Placement::Hidden
    }
}

pub(super) fn render(
    frame: &mut Frame,
    area: Rect,
    monitor: &Monitor,
    placement: Placement,
    color: bool,
    now: Instant,
) {
    let rows = monitor.rows();
    match placement {
        Placement::Hidden => {}
        Placement::Line => {
            let prefix = "Disk  ";
            let width = usize::from(area.width).saturating_sub(prefix.len());
            let names = name_width(&rows[..1]);
            let bar = preferred_bar(&rows[0], names, width, now).unwrap_or(MIN_BAR);
            let mut spans = vec![Span::raw(prefix)];
            spans.extend(row_spans(&rows[0], names, width, bar, color, now));
            frame.render_widget(Paragraph::new(Line::from(spans)), area);
        }
        Placement::Panel(shown) => {
            let shown = &rows[..usize::from(shown).min(rows.len())];
            let hidden = rows.len() - shown.len();
            let title = if hidden > 0 {
                format!(" {LEGEND} · +{hidden} more ")
            } else {
                format!(" {LEGEND} ")
            };
            let width = usize::from(area.width.saturating_sub(2));
            let names = name_width(shown);
            // 行どうしを比べられるよう、バーの幅は最も説明の長い行に合わせて揃える。
            let bar = shown
                .iter()
                .filter_map(|row| preferred_bar(row, names, width, now))
                .min()
                .unwrap_or(MIN_BAR);
            let lines: Vec<Line> = shown
                .iter()
                .map(|row| Line::from(row_spans(row, names, width, bar, color, now)))
                .collect();
            frame.render_widget(
                Paragraph::new(lines).block(Block::bordered().title(title)),
                area,
            );
        }
    }
}

fn row_name(row: &Row) -> String {
    row.last
        .as_ref()
        .map_or_else(|| row.target.label(), |(volume, _)| volume.name.clone())
}

/// 並べたボリュームのバーの位置を揃えるため、最も長い名前に合わせる。
fn name_width(rows: &[Row]) -> usize {
    rows.iter()
        .map(|row| row_name(row).width())
        .max()
        .unwrap_or(0)
        .min(MAX_NAME_WIDTH)
}

/// 名前の欄 (末尾の区切りを含む)。
fn name_field(row: &Row, name_width: usize) -> String {
    format!("{} ", pad_display(&row_name(row), name_width + 1))
}

/// 値のある行の使用率と、バーの後ろに置ける説明 (詳しい順、最後は空)。
fn texts(row: &Row, now: Instant) -> Option<(&Volume, String, Vec<String>)> {
    let (volume, read_at) = row.last.as_ref()?;
    // 直近の取得が失敗した、または戻らないときは、表示中の値がいつのものかを添える。
    let stale = (row.error.is_some() || row.overdue(now))
        .then(|| format!("{} ago", age(now.saturating_duration_since(*read_at))));
    let details = details(volume)
        .into_iter()
        .map(|detail| match (&stale, detail.is_empty()) {
            (Some(stale), true) => stale.clone(),
            (Some(stale), false) => format!("{detail} · {stale}"),
            (None, _) => detail,
        })
        .collect();
    Some((volume, format!("  {}", percent_label(volume)), details))
}

/// 説明の前に空ける 2 桁を含めた、説明に要る幅。
fn detail_width(detail: &str) -> usize {
    if detail.is_empty() {
        0
    } else {
        2 + detail.width()
    }
}

/// 最も詳しい説明を出せるバーの幅。値のない行は None。
fn preferred_bar(row: &Row, name_width: usize, width: usize, now: Instant) -> Option<usize> {
    let (_, percent, details) = texts(row, now)?;
    let budget = width.saturating_sub(name_field(row, name_width).width() + percent.width());
    let needs: Vec<usize> = details.iter().map(|detail| detail_width(detail)).collect();
    // バーを COMFORT_BAR 以上に保てる中で最も詳しい説明を選ぶ。どれも入らない幅では
    // バーを MIN_BAR まで縮めて説明を残し、それも無理ならバーと使用率だけを出す。
    let chosen = needs
        .iter()
        .copied()
        .filter(|&need| need > 0)
        .find(|&need| budget >= need + COMFORT_BAR)
        .or_else(|| needs.iter().copied().find(|&need| budget >= need + MIN_BAR));
    Some(chosen.map_or_else(
        || budget.clamp(1, MAX_BAR),
        |need| (budget - need).min(MAX_BAR),
    ))
}

/// 1 ボリュームの行。バーの後ろの説明は、`bar` の幅で収まる最も詳しいものを選ぶ。
fn row_spans(
    row: &Row,
    name_width: usize,
    width: usize,
    bar: usize,
    color: bool,
    now: Instant,
) -> Vec<Span<'static>> {
    let paint = |color_name: Color| {
        if color {
            Style::default().fg(color_name)
        } else {
            Style::default()
        }
    };
    let name = name_field(row, name_width);
    let Some((volume, percent, details)) = texts(row, now) else {
        let status = if let Some(error) = &row.error {
            Span::styled(format!("unavailable: {error}"), paint(Color::Red))
        } else if row.overdue(now) {
            Span::styled("no response", paint(Color::Yellow))
        } else {
            Span::raw("reading…")
        };
        return vec![Span::raw(name), status];
    };
    let budget = width.saturating_sub(name.width() + percent.width());
    let bar_width = bar.min(budget).max(1);
    let detail = details
        .into_iter()
        .find(|detail| budget >= detail_width(detail) + bar_width)
        .unwrap_or_default();

    let [used, purgeable, free] = bar_cells(volume, bar_width);
    let bar = format!(
        "{}{}{}",
        "█".repeat(used),
        "▒".repeat(purgeable),
        "░".repeat(free)
    );
    let mut spans = vec![
        Span::raw(name),
        Span::styled(bar, paint(disk_color(volume.used_percent()))),
        Span::raw(percent),
    ];
    if !detail.is_empty() {
        spans.push(Span::raw(format!("  {detail}")));
    }
    spans
}

/// バーの後ろに置く説明を、詳しいものから順に並べる。最後は説明なし。
fn details(volume: &Volume) -> Vec<String> {
    let available = format_bytes(volume.available);
    let total = format_bytes(volume.total);
    let mut details = Vec::new();
    if let Some(purgeable) = volume.purgeable.filter(|&purgeable| purgeable > 0) {
        let purgeable = format_bytes(purgeable);
        details.push(format!(
            "{available} available of {total} · {} free · {purgeable} purgeable",
            format_bytes(volume.free)
        ));
        details.push(format!(
            "{available} available of {total} · {purgeable} purgeable"
        ));
    }
    details.push(format!("{available} available of {total}"));
    details.push(format!("{available} available"));
    details.push(String::new());
    details
}

/// 使用・パージ可能・空きのセル数。合計は必ず `width` になる。
fn bar_cells(volume: &Volume, width: usize) -> [usize; 3] {
    // purgeable が不明なボリュームは available == free なので、3 区分の合計は常に総容量。
    let parts = [volume.used(), volume.purgeable.unwrap_or(0), volume.free];
    let total: u128 = parts.iter().map(|&part| u128::from(part)).sum();
    let width_wide = width as u128;
    let mut cells = parts.map(|part| (u128::from(part) * width_wide / total) as usize);
    let mut remainders = parts.map(|part| u128::from(part) * width_wide % total);
    // 切り捨てで余ったセルを、端数の大きい区分から 1 つずつ配る。
    for _ in cells.iter().sum::<usize>()..width {
        let index = (0..3)
            .max_by_key(|&index| (remainders[index], Reverse(index)))
            .expect("three parts");
        cells[index] += 1;
        remainders[index] = 0;
    }
    // 少しでも使っている・空いているディスクが、空・満杯に見えないよう、使用中と空きは
    // 0 でなければ最低 1 セル見せる (数値は説明の方が正確)。パージ可能分は補助の情報なので、
    // 1 セルに満たない量のために使用中のセルを削らない。
    if parts.iter().filter(|&&part| part > 0).count() <= width {
        for index in [0, 2] {
            if parts[index] > 0 && cells[index] == 0 {
                let donor = (0..3)
                    .max_by_key(|&other| cells[other])
                    .expect("three parts");
                if cells[donor] > 1 {
                    cells[donor] -= 1;
                    cells[index] += 1;
                }
            }
        }
    }
    cells
}

/// 使用率。丸めで空きや使用が消えて見えないよう、0% と 100% は本当に空・満杯のときだけ出す。
fn percent_label(volume: &Volume) -> String {
    let used = volume.used();
    let mut shown = volume.used_percent().round() as u64;
    if used > 0 {
        shown = shown.max(1);
    }
    if used < volume.total {
        shown = shown.min(99);
    }
    format!("{shown:>3}%")
}

/// Finder と同じ 10 進の単位 (1 GB = 1,000,000,000 バイト)。
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["KB", "MB", "GB", "TB", "PB", "EB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    // 小数 1 桁で 1000.0 と表示される値は、次の単位に上げる。
    while value >= 999.95 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn age(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        "<1m".into()
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

/// ディスクは利用枠より満杯に近い状態で使われるため、しきい値を高めに置く。
fn disk_color(percent: f64) -> Color {
    if percent >= 95.0 {
        Color::Red
    } else if percent >= 90.0 {
        Color::LightRed
    } else if percent >= 80.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::{ReadError, Target};

    const GB: u64 = 1_000_000_000;

    /// 実機の起動ディスクで測った値。Finder 基準で 86% 使用、57.7GB がパージ可能。
    fn startup() -> Volume {
        Volume::new(
            "Macintosh HD".into(),
            494_384_795_648,
            13_080_645_632,
            Some(70_754_759_808),
        )
        .unwrap()
    }

    /// 経過時間を引いても負にならないよう、基準時刻を未来に置く。
    fn base() -> Instant {
        Instant::now() + Duration::from_secs(3_600)
    }

    fn sampled(volume: Volume, read_at: Instant) -> Row {
        let mut row = Row::new(Target::Startup, read_at);
        row.last = Some((volume, read_at));
        row
    }

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|span| span.content.as_ref()).collect()
    }

    /// 1 行だけを描くときと同じく、その行に最適なバーの幅で組む。
    fn spans_for(
        row: &Row,
        names: usize,
        width: usize,
        color: bool,
        now: Instant,
    ) -> Vec<Span<'static>> {
        let bar = preferred_bar(row, names, width, now).unwrap_or(MIN_BAR);
        row_spans(row, names, width, bar, color, now)
    }

    #[test]
    fn placement_keeps_room_for_the_accounts_list() {
        for (volumes, height, header, expected) in [
            (1, 9, 3, Placement::Hidden),
            (1, 11, 3, Placement::Hidden),
            (1, 12, 3, Placement::Line),
            (1, 13, 3, Placement::Line),
            (1, 14, 3, Placement::Panel(1)),
            (1, 14, 4, Placement::Line),
            (1, 15, 4, Placement::Panel(1)),
            (3, 15, 3, Placement::Panel(2)),
            (3, 40, 3, Placement::Panel(3)),
            (0, 40, 3, Placement::Hidden),
        ] {
            assert_eq!(
                placement(volumes, height, header, 2),
                expected,
                "{volumes} volumes, {height} rows, header {header}"
            );
        }
        assert_eq!(Placement::Hidden.height(), 0);
        assert_eq!(Placement::Line.height(), 1);
        assert_eq!(Placement::Panel(2).height(), 4);
    }

    #[test]
    fn bar_cells_fill_the_width_and_keep_small_parts_visible() {
        let volume = startup();
        for width in 1..=40 {
            assert_eq!(bar_cells(&volume, width).iter().sum::<usize>(), width);
        }
        // 端数の大きいパージ可能分に余りを配り、0 セルの空きには使用中から 1 セル回す。
        assert_eq!(bar_cells(&volume, 12), [9, 2, 1]);
        // 1 セルしかなければ、最大の区分だけを出す。
        assert_eq!(bar_cells(&volume, 1), [1, 0, 0]);
        let full = Volume::new("Full".into(), 500 * GB, 0, Some(0)).unwrap();
        assert_eq!(bar_cells(&full, 12), [12, 0, 0]);
        let empty = Volume::new("Empty".into(), 500 * GB, 500 * GB, Some(500 * GB)).unwrap();
        assert_eq!(bar_cells(&empty, 12), [0, 0, 12]);
        // 重要な用途の容量が不明なら、パージ可能分のセルは出さない。
        let huge = Volume::new("Huge".into(), 10_000 * GB, GB, None).unwrap();
        assert_eq!(bar_cells(&huge, 12), [11, 0, 1]);
    }

    #[test]
    fn percent_label_reserves_0_and_100_for_exact_extremes() {
        let volume = |available: u64| Volume::new("V".into(), 1_000 * GB, 0, Some(available));
        assert_eq!(percent_label(&startup()), " 86%");
        // 1GB でも空いていれば 100% にせず、1GB でも使っていれば 0% にしない。
        assert_eq!(percent_label(&volume(GB).unwrap()), " 99%");
        assert_eq!(percent_label(&volume(999 * GB).unwrap()), "  1%");
        let full = Volume::new("Full".into(), 1_000 * GB, 0, Some(0)).unwrap();
        assert_eq!(percent_label(&full), "100%");
        let empty = Volume::new("Empty".into(), 1_000 * GB, 1_000 * GB, None).unwrap();
        assert_eq!(percent_label(&empty), "  0%");
    }

    #[test]
    fn format_bytes_uses_decimal_units_like_finder() {
        for (bytes, expected) in [
            (0, "0 B"),
            (999, "999 B"),
            (1_000, "1.0 KB"),
            (13_080_645_632, "13.1 GB"),
            (70_754_759_808, "70.8 GB"),
            (494_384_795_648, "494.4 GB"),
            (999_940_000_000, "999.9 GB"),
            (999_950_000_000, "1.0 TB"),
            (1_000_203_071_488, "1.0 TB"),
        ] {
            assert_eq!(format_bytes(bytes), expected, "{bytes}");
        }
    }

    #[test]
    fn row_detail_shortens_with_the_width_and_keeps_the_bar() {
        let now = base();
        let row = sampled(startup(), now);
        let names = name_width(std::slice::from_ref(&row));
        // 説明を足してもバーを 16 セル残せる範囲で最も詳しくし、狭い幅では 6 セルまで縮める。
        for (width, expected, bar) in [
            (
                118,
                "70.8 GB available of 494.4 GB · 13.1 GB free · 57.7 GB purgeable",
                32,
            ),
            (98, "70.8 GB available of 494.4 GB · 57.7 GB purgeable", 27),
            (78, "70.8 GB available of 494.4 GB", 27),
            (58, "70.8 GB available", 19),
            (46, "70.8 GB available", 7),
            (26, "", 6),
        ] {
            let line = text(&spans_for(&row, names, width, false, now));
            assert!(line.starts_with("Macintosh HD  █"), "{line:?}");
            assert!(line.contains("  86%"), "{line:?}");
            assert!(
                line.ends_with(&format!("86%  {expected}").trim_end().to_string()),
                "{width}: {line:?}"
            );
            assert_eq!(
                line.chars()
                    .filter(|c| matches!(c, '█' | '▒' | '░'))
                    .count(),
                bar,
                "{width}: {line:?}"
            );
            assert!(line.width() <= width, "{width}: {line:?}");
        }
    }

    #[test]
    fn rows_report_loading_unavailable_and_stale_values() {
        let now = base();
        let names = 12;
        let mut row = Row::new(Target::Mount("/Volumes/Archive".into()), now);
        assert_eq!(
            text(&spans_for(&row, names, 80, false, now)).trim_end(),
            "Archive       reading…"
        );

        row.started = Some(now - OVERDUE_FOR_TEST);
        assert!(text(&spans_for(&row, names, 80, false, now)).ends_with("no response"));

        row.started = None;
        row.error = Some(ReadError::NotMounted);
        let spans = spans_for(&row, names, 80, true, now);
        assert!(text(&spans).ends_with("unavailable: not mounted"));
        assert_eq!(spans.last().unwrap().style.fg, Some(Color::Red));

        // 失敗しても前回の値は残し、いつの値かを添える。
        let mut stale = sampled(startup(), now - Duration::from_secs(180));
        stale.error = Some(ReadError::Failed("I/O error".into()));
        let line = text(&spans_for(&stale, names, 118, false, now));
        assert!(line.ends_with("57.7 GB purgeable · 3m ago"), "{line:?}");
        // 説明が入らない幅でも、経過時間だけは残す。
        let narrow = text(&spans_for(&stale, names, 34, false, now));
        assert!(narrow.ends_with("86%  3m ago"), "{narrow:?}");

        // 取得が戻らない間も同じ。成功した値だけなら経過時間は出さない。
        let mut waiting = sampled(startup(), now - Duration::from_secs(33));
        waiting.started = Some(now - Duration::from_secs(3));
        assert!(text(&spans_for(&waiting, names, 80, false, now)).ends_with("· <1m ago"));
        waiting.started = Some(now - Duration::from_secs(1));
        assert!(!text(&spans_for(&waiting, names, 80, false, now)).contains("ago"));
    }

    const OVERDUE_FOR_TEST: Duration = crate::disk::OVERDUE;

    #[test]
    fn bar_color_follows_the_used_share_and_respects_no_color() {
        for (percent, expected) in [
            (0.0, Color::Green),
            (79.9, Color::Green),
            (80.0, Color::Yellow),
            (89.9, Color::Yellow),
            (90.0, Color::LightRed),
            (95.0, Color::Red),
            (100.0, Color::Red),
        ] {
            assert_eq!(disk_color(percent), expected, "{percent}");
        }
        let now = base();
        let row = sampled(startup(), now);
        let colored = spans_for(&row, 12, 80, true, now);
        assert_eq!(colored[1].style.fg, Some(Color::Yellow));
        let plain = spans_for(&row, 12, 80, false, now);
        assert!(plain.iter().all(|span| span.style.fg.is_none()));
        assert_eq!(text(&colored), text(&plain));
    }

    #[test]
    fn long_names_are_cut_at_the_name_column() {
        let now = base();
        let mut volume = startup();
        volume.name = "とても長いボリュームの名前です".into();
        let row = sampled(volume, now);
        let names = name_width(std::slice::from_ref(&row));
        assert_eq!(names, MAX_NAME_WIDTH);
        let line = text(&spans_for(&row, names, 80, false, now));
        let bar = line.find('█').unwrap();
        assert_eq!(line[..bar].width(), MAX_NAME_WIDTH + 2, "{line:?}");
    }

    #[test]
    fn panel_rows_share_one_bar_width() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let now = base();
        let mut archive = Row::new(Target::Mount("/Volumes/Archive".into()), now);
        archive.last = Some((
            Volume::new("Archive".into(), 1_000 * GB, 41 * GB, Some(41 * GB)).unwrap(),
            now,
        ));
        let monitor = Monitor::from_rows(vec![sampled(startup(), now), archive]);
        let mut terminal = Terminal::new(TestBackend::new(100, 4)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    frame.area(),
                    &monitor,
                    Placement::Panel(2),
                    false,
                    now,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let bars: Vec<usize> = (1..3)
            .map(|y| {
                (0..100)
                    .filter(|&x| matches!(buffer.cell((x, y)).unwrap().symbol(), "█" | "▒" | "░"))
                    .count()
            })
            .collect();
        // 説明の短い Archive の行も、起動ディスクの行と同じ幅のバーで描く。
        assert_eq!(bars[0], bars[1], "{bars:?}");
        assert!(bars[0] >= MIN_BAR, "{bars:?}");
    }
}
