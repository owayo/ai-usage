//! TUI の最下部 (キー操作の説明の上) に置くディスク欄。
//!
//! 1 行の中身 (バーのセル数・使用率・内訳・状態の文言と色) は `render/disk.rs` が組み立てる。
//! ここでは欄の置き方と、端末の幅に合わせてバーの幅と説明の詳しさを選ぶことを受け持つ。

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::super::disk::{
    Segment, bar_segments, breakdown_segment, breakdowns, name_segment, name_width,
    no_response_segment, percent_segment, unavailable_segment,
};
use super::super::palette::MUTED;
use super::style;
use crate::disk::{Monitor, Row, Volume};

/// ディスク欄を出しても、アカウント一覧に残す行数 (枠を含む)。見出しと 2 つの利用枠が収まる。
const MIN_ACCOUNT_ROWS: u16 = 6;
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
            let names = rows_name_width(&rows[..1]);
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
            let names = rows_name_width(shown);
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
fn rows_name_width(rows: &[Row]) -> usize {
    name_width(rows.iter().map(row_name))
}

/// バーの後ろに置く説明。内訳と経過時間は色を分けるため、別々に持つ。
#[derive(Default)]
struct Detail {
    /// 容量の内訳。最も短い候補では空。
    breakdown: String,
    /// 表示中の値が古いときの経過時間 (`3m ago`)。
    stale: Option<String>,
}

impl Detail {
    /// 説明の前に空ける 2 桁を含めた区間と色。内訳と経過時間の両方があれば ` · ` でつなぐ。
    /// 幅の計算と描画の両方がこれを使うので、測った幅と描いた幅はずれない。
    fn parts(&self) -> Vec<Segment> {
        let mut parts = Vec::new();
        if !self.breakdown.is_empty() {
            parts.push(breakdown_segment(&self.breakdown));
        }
        if let Some(stale) = &self.stale {
            let separator = if parts.is_empty() { "  " } else { " · " };
            parts.push((format!("{separator}{stale}"), MUTED));
        }
        parts
    }

    /// 説明に要る幅。説明がなければ 0。
    fn width(&self) -> usize {
        self.parts().iter().map(|(text, _)| text.width()).sum()
    }
}

/// 値のある行の使用率と、バーの後ろに置ける説明 (詳しい順。最後は内訳なし)。
fn texts(row: &Row, now: Instant) -> Option<(&Volume, Segment, Vec<Detail>)> {
    let (volume, read_at) = row.last.as_ref()?;
    // 直近の取得が失敗した、または戻らないときは、表示中の値がいつのものかを添える。
    // 内訳を出せない幅でも経過時間だけは残るよう、どの候補にも付ける。
    let stale = (row.error.is_some() || row.overdue(now))
        .then(|| format!("{} ago", age(now.saturating_duration_since(*read_at))));
    let details = breakdowns(volume)
        .into_iter()
        .map(|breakdown| Detail {
            breakdown,
            stale: stale.clone(),
        })
        .collect();
    Some((volume, percent_segment(volume), details))
}

/// 最も詳しい説明を出せるバーの幅。値のない行は None。
fn preferred_bar(row: &Row, name_width: usize, width: usize, now: Instant) -> Option<usize> {
    let (_, (percent, _), details) = texts(row, now)?;
    let (name, _) = name_segment(&row_name(row), name_width);
    let budget = width.saturating_sub(name.width() + percent.width());
    let needs: Vec<usize> = details.iter().map(Detail::width).collect();
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
    let name = name_segment(&row_name(row), name_width);
    let Some((volume, percent, details)) = texts(row, now) else {
        let status = if let Some(error) = &row.error {
            unavailable_segment(error)
        } else if row.overdue(now) {
            no_response_segment()
        } else {
            ("reading…".into(), MUTED)
        };
        return spans([name, status], color);
    };
    let budget = width.saturating_sub(name.0.width() + percent.0.width());
    let bar_width = bar.min(budget).max(1);
    let detail = details
        .into_iter()
        .find(|detail| budget >= detail.width() + bar_width)
        .unwrap_or_default();

    let mut segments = vec![name];
    segments.extend(bar_segments(volume, bar_width));
    segments.push(percent);
    segments.extend(detail.parts());
    spans(segments, color)
}

/// 区間を、palette の色を付けた Span にする。
fn spans(segments: impl IntoIterator<Item = Segment>, color: bool) -> Vec<Span<'static>> {
    segments
        .into_iter()
        .map(|(text, paint)| Span::styled(text, style(color, paint)))
        .collect()
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

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier, Style};

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

    /// 期待する装飾。palette を通さず、256 色の番号で書く。
    fn fg(index: u8) -> Style {
        Style::default().fg(Color::Indexed(index))
    }

    fn bold(index: u8) -> Style {
        fg(index).add_modifier(Modifier::BOLD)
    }

    /// `needle` を含む区間。区分や項目ごとに区間を分けているので、含む区間は 1 つに限る。
    fn span_with<'a>(spans: &'a [Span<'static>], needle: &str) -> &'a Span<'static> {
        let mut found = spans.iter().filter(|span| span.content.contains(needle));
        let span = found
            .next()
            .unwrap_or_else(|| panic!("no span contains {needle:?}: {spans:?}"));
        assert!(
            found.next().is_none(),
            "several spans contain {needle:?}: {spans:?}"
        );
        span
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
    fn row_detail_shortens_with_the_width_and_keeps_the_bar() {
        let now = base();
        let row = sampled(startup(), now);
        let names = rows_name_width(std::slice::from_ref(&row));
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
        // 値のない行は、名前と状態の 2 区間。状態の色は取得中・応答なし・失敗で変える。
        let status = |row: &Row| {
            let spans = spans_for(row, names, 80, true, now);
            assert_eq!(spans.len(), 2, "{spans:?}");
            assert_eq!(spans[0].style, fg(245), "{spans:?}");
            spans[1].style
        };
        let mut row = Row::new(Target::Mount("/Volumes/Archive".into()), now);
        assert_eq!(
            text(&spans_for(&row, names, 80, false, now)).trim_end(),
            "Archive       reading…"
        );
        assert_eq!(status(&row), fg(242));

        row.started = Some(now - OVERDUE_FOR_TEST);
        assert!(text(&spans_for(&row, names, 80, false, now)).ends_with("no response"));
        assert_eq!(status(&row), fg(178));

        row.started = None;
        row.error = Some(ReadError::NotMounted);
        assert!(
            text(&spans_for(&row, names, 80, false, now)).ends_with("unavailable: not mounted")
        );
        assert_eq!(status(&row), fg(196));

        // 失敗しても前回の値は残し、いつの値かを添える。
        let mut stale = sampled(startup(), now - Duration::from_secs(180));
        stale.error = Some(ReadError::Failed("I/O error".into()));
        let line = text(&spans_for(&stale, names, 118, false, now));
        assert!(line.ends_with("57.7 GB purgeable · 3m ago"), "{line:?}");
        // 内訳は値に添える色、経過時間は値の古さを示す色で、区間を分ける。
        let spans = spans_for(&stale, names, 118, true, now);
        let [.., breakdown, age] = spans.as_slice() else {
            panic!("{spans:?}");
        };
        assert!(
            breakdown.content.ends_with("57.7 GB purgeable"),
            "{spans:?}"
        );
        assert_eq!(breakdown.style, fg(245));
        assert_eq!(age.content, " · 3m ago");
        assert_eq!(age.style, fg(242));
        // 説明が入らない幅でも、経過時間だけは残す。
        let narrow = text(&spans_for(&stale, names, 34, false, now));
        assert!(narrow.ends_with("86%  3m ago"), "{narrow:?}");
        let spans = spans_for(&stale, names, 34, true, now);
        let [.., percent, age] = spans.as_slice() else {
            panic!("{spans:?}");
        };
        assert_eq!(percent.content, "   86%");
        assert_eq!(age.content, "  3m ago");
        assert_eq!(age.style, fg(242));

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
        // 段階の境界 (80% / 90% / 95%) そのものは render/disk.rs のテストで確かめる。
        let now = base();
        // 起動ディスク (86%): 使用中の区分と使用率は 2 段目の色。パージ可能分と空きは灰色。
        let spans = spans_for(&sampled(startup(), now), 12, 80, true, now);
        assert_eq!(span_with(&spans, "Macintosh HD").style, fg(245));
        assert_eq!(span_with(&spans, "█").style, fg(178));
        assert_eq!(span_with(&spans, "▒").style, fg(245));
        assert_eq!(span_with(&spans, "░").style, fg(242));
        assert_eq!(span_with(&spans, "86%").style, fg(178));
        assert_eq!(span_with(&spans, "available").style, fg(245));

        // 90% 以上は使用率の数字だけを太字にし、バーは太字にしない。
        let full = |free: u64| {
            let volume = Volume::new("V".into(), 1_000 * GB, free * GB, Some(free * GB));
            sampled(volume.unwrap(), now)
        };
        for (row, percent, used, expected) in [
            (full(80), "92%", fg(208), bold(208)),
            (full(30), "97%", fg(196), bold(196)),
        ] {
            let spans = spans_for(&row, 12, 80, true, now);
            assert_eq!(span_with(&spans, "█").style, used, "{percent}");
            assert_eq!(span_with(&spans, percent).style, expected, "{percent}");
            assert_eq!(span_with(&spans, "░").style, fg(242), "{percent}");
        }

        // 0 セルの区分は区間を作らない。
        let spans = spans_for(&full(0), 12, 80, true, now);
        assert!(
            spans.iter().all(|span| !span.content.is_empty()),
            "{spans:?}"
        );
        assert!(!text(&spans).contains(['▒', '░']), "{spans:?}");

        // 色を使わないときは、どの区間にも色も太字も付けず、文字は色付きのときと同じにする。
        let archive = || Row::new(Target::Mount("/Volumes/Archive".into()), now);
        let mut stale = sampled(startup(), now - Duration::from_secs(180));
        stale.error = Some(ReadError::Failed("I/O error".into()));
        let mut silent = archive();
        silent.started = Some(now - OVERDUE_FOR_TEST);
        let mut unavailable = archive();
        unavailable.error = Some(ReadError::NotMounted);
        for row in [
            sampled(startup(), now),
            full(30),
            stale,
            archive(),
            silent,
            unavailable,
        ] {
            for width in [34, 80, 118] {
                let colored = spans_for(&row, 12, width, true, now);
                let plain = spans_for(&row, 12, width, false, now);
                assert!(
                    plain.iter().all(|span| span.style == Style::default()),
                    "{plain:?}"
                );
                assert_eq!(text(&colored), text(&plain));
            }
        }
    }

    #[test]
    fn long_names_are_cut_at_the_name_column() {
        use super::super::super::disk::MAX_NAME_WIDTH;

        let now = base();
        let mut volume = startup();
        volume.name = "とても長いボリュームの名前です".into();
        let row = sampled(volume, now);
        let names = rows_name_width(std::slice::from_ref(&row));
        assert_eq!(names, MAX_NAME_WIDTH);
        let line = text(&spans_for(&row, names, 80, false, now));
        let bar = line.find('█').unwrap();
        assert_eq!(line[..bar].width(), MAX_NAME_WIDTH + 2, "{line:?}");
    }

    #[test]
    fn rows_match_the_disk_command_output() {
        use crate::disk::Reading;

        /// statusline と同じ書式の SGR に直す (色を付けない区間はそのまま)。
        fn ansi(spans: &[Span]) -> String {
            spans
                .iter()
                .map(|span| match span.style.fg {
                    Some(Color::Indexed(index)) => {
                        let bold = if span.style.add_modifier.contains(Modifier::BOLD) {
                            "1;"
                        } else {
                            ""
                        };
                        format!("\x1b[{bold}38;5;{index}m{}\x1b[0m", span.content)
                    }
                    _ => span.content.to_string(),
                })
                .collect()
        }

        // `--disk` の行は、十分に広い端末の行を幅 20 のバーで描いたものと同じになる。
        let now = base();
        let external = Volume::new("External".into(), 2_000 * GB, 1_200 * GB, None).unwrap();
        let full = Volume::new("Full".into(), 1_000 * GB, 30 * GB, Some(30 * GB)).unwrap();
        let mut unplugged = Row::new(Target::Mount("/Volumes/Archive".into()), now);
        unplugged.error = Some(ReadError::NotMounted);
        let mut silent = Row::new(Target::Mount("/Volumes/Backup".into()), now);
        silent.started = Some(now - OVERDUE_FOR_TEST);
        let rows = [
            sampled(startup(), now),
            sampled(external.clone(), now),
            sampled(full.clone(), now),
            unplugged,
            silent,
        ];
        let readings = [
            Reading {
                target: Target::Startup,
                result: Some(Ok(startup())),
            },
            Reading {
                target: Target::Mount("/Volumes/External".into()),
                result: Some(Ok(external)),
            },
            Reading {
                target: Target::Mount("/Volumes/Full".into()),
                result: Some(Ok(full)),
            },
            Reading {
                target: Target::Mount("/Volumes/Archive".into()),
                result: Some(Err(ReadError::NotMounted)),
            },
            Reading {
                target: Target::Mount("/Volumes/Backup".into()),
                result: None,
            },
        ];
        let names = rows_name_width(&rows);
        for color in [false, true] {
            let tui: Vec<String> = rows
                .iter()
                .map(|row| format!("{}\n", ansi(&row_spans(row, names, 200, 20, color, now))))
                .collect();
            assert_eq!(
                super::super::super::disk::text(&readings, color),
                tui.concat(),
                "color={color}"
            );
        }
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

    #[test]
    fn rendered_cells_use_the_palette_colors() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::buffer::Cell;

        /// 字形 `symbol` の最初のセルの色と装飾。
        fn first(cells: &[Cell], symbol: &str) -> (Color, Modifier) {
            let cell = cells
                .iter()
                .find(|cell| cell.symbol() == symbol)
                .unwrap_or_else(|| panic!("no {symbol:?} cell"));
            (cell.fg, cell.modifier)
        }
        /// 使用率の数字と `%` のセルの文字と、それぞれの色と装飾。
        fn percent(cells: &[Cell]) -> (String, Vec<(Color, Modifier)>) {
            let end = cells.iter().position(|cell| cell.symbol() == "%").unwrap();
            let cells = &cells[end - 2..=end];
            (
                cells.iter().map(Cell::symbol).collect(),
                cells.iter().map(|cell| (cell.fg, cell.modifier)).collect(),
            )
        }
        let plain = (Color::Reset, Modifier::empty());
        let indexed = |index: u8| (Color::Indexed(index), Modifier::empty());

        let now = base();
        let critical = Volume::new("Full".into(), 1_000 * GB, 30 * GB, Some(30 * GB)).unwrap();
        // 起動ディスク (86%) と 97% のボリュームを色付きで描き、各行のセルを返す。
        let draw = |placement: Placement| -> Vec<Vec<Cell>> {
            let monitor = Monitor::from_rows(vec![
                sampled(startup(), now),
                sampled(critical.clone(), now),
            ]);
            let mut terminal = Terminal::new(TestBackend::new(100, placement.height())).unwrap();
            terminal
                .draw(|frame| render(frame, frame.area(), &monitor, placement, true, now))
                .unwrap();
            let buffer = terminal.backend().buffer();
            (0..placement.height())
                .map(|y| {
                    (0..100)
                        .map(|x| buffer.cell((x, y)).unwrap().clone())
                        .collect()
                })
                .collect()
        };

        let lines = draw(Placement::Panel(2));
        // 枠と見出し (凡例の字形も) には色を付けない。
        assert_eq!(first(&lines[0], "┌"), plain);
        assert_eq!(first(&lines[0], "D"), plain);
        assert_eq!(first(&lines[0], "█"), plain);
        assert_eq!(first(&lines[1], "│"), plain);
        // 起動ディスク (86%) は 2 段目の色。パージ可能分と空きは段階によらず灰色。
        assert_eq!(first(&lines[1], "M"), indexed(245));
        assert_eq!(first(&lines[1], "█"), indexed(178));
        assert_eq!(first(&lines[1], "▒"), indexed(245));
        assert_eq!(first(&lines[1], "░"), indexed(242));
        assert_eq!(percent(&lines[1]), ("86%".into(), vec![indexed(178); 3]));
        // 97% は最上段。使用率の数字だけを太字にする。
        assert_eq!(first(&lines[2], "█"), indexed(196));
        assert_eq!(first(&lines[2], "░"), indexed(242));
        assert_eq!(
            percent(&lines[2]),
            ("97%".into(), vec![(Color::Indexed(196), Modifier::BOLD); 3])
        );

        // 1 行の置き方でも同じ色。先頭の `Disk` には色を付けない。
        let line = &draw(Placement::Line)[0];
        assert_eq!(first(line, "D"), plain);
        assert_eq!(first(line, "M"), indexed(245));
        assert_eq!(first(line, "█"), indexed(178));
        assert_eq!(percent(line), ("86%".into(), vec![indexed(178); 3]));
    }
}
