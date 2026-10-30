//! 全アカウントの使用枠を同時に比較できる対話式の端末表示。

mod disk;

use std::future::Future;
use std::io::{self, IsTerminal};
use std::pin::Pin;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use chrono::{DateTime, Datelike, Local, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use super::manual_resets::{TuiResetLine, format_resets_tui};
use super::palette::{self, Fg, Level, Paint};
use super::reset::{FIVE_H_TH, legacy_long_window_thresholds, window_thresholds};
use super::sort::sorted_refs;
use super::{display_name, limit_observation_age, parse_utc};
use crate::SortKey;
use crate::diagnostics::Diagnostics;
use crate::disk::{Monitor as DiskMonitor, Target as DiskTarget};
use crate::model::{Provider, WindowKind};
use crate::report::{AccountOut, Report, WindowOut};

type FetchFuture<'a> = Pin<Box<dyn Future<Output = Result<Report>> + 'a>>;
pub type Fetch<'a> = Box<dyn Fn() -> FetchFuture<'a> + 'a>;
const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

struct RestoreTerminal(Diagnostics);

impl Drop for RestoreTerminal {
    fn drop(&mut self) {
        ratatui::restore();
        for warning in self.0.take() {
            eprintln!("{warning}");
        }
    }
}

struct App {
    warnings: Vec<String>,
    report: Option<Report>,
    error: Option<String>,
    loading: bool,
    scroll: usize,
    sort: SortKey,
    color: bool,
    /// 最下部のディスク欄。`--no-disk` などで無効なら None。
    disk: Option<DiskMonitor>,
}

impl App {
    /// 描画する順の行。`--sort weekly-reset` は残り時間で並べるので、色と同じ時刻を渡す。
    fn rows(&self, now: DateTime<Utc>) -> Vec<&AccountOut> {
        self.report
            .as_ref()
            .map(|report| sorted_refs(&report.accounts, self.sort, now, None))
            .unwrap_or_default()
    }

    /// アカウントの数。並べ替えは数を変えないので、並べずに数える。
    fn len(&self) -> usize {
        self.report
            .as_ref()
            .map_or(0, |report| report.accounts.len())
    }

    fn move_scroll(&mut self, delta: isize) {
        let last = self.len().saturating_sub(1);
        self.scroll = self.scroll.min(last).saturating_add_signed(delta).min(last);
    }
}

/// `--tui` のメインループ。通信が終わる前にも取得中画面を表示する。
///
/// `disk` が空でなければ、最下部にそのボリュームの容量を表示する。利用枠を保存済みの
/// JSON から読む場合も、ディスクの値はこの Mac から読む。
pub async fn run(
    initial: Option<Report>,
    fetch: Option<Fetch<'_>>,
    sort: SortKey,
    color: bool,
    diagnostics: Diagnostics,
    disk: Vec<DiskTarget>,
) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("--tui requires an interactive terminal (use --json for redirected output)");
    }

    let mut terminal = ratatui::try_init()?;
    let _restore = RestoreTerminal(diagnostics.clone());
    let mut pending = if initial.is_none() {
        fetch.as_ref().map(|start| start())
    } else {
        None
    };
    let mut next_refresh = Instant::now() + REFRESH_INTERVAL;
    let initial_warnings = diagnostics.take();
    let mut app = App {
        warnings: initial_warnings.clone(),
        report: initial,
        error: None,
        loading: pending.is_some(),
        scroll: 0,
        sort,
        color,
        disk: (!disk.is_empty()).then(|| DiskMonitor::new(disk, Instant::now())),
    };

    let mut redraw = true;
    let mut last_draw = Instant::now();
    loop {
        // ディスクの取得は別スレッドで進む。ここでは結果の反映と次の取得の開始だけを行う。
        if let Some(monitor) = app.disk.as_mut()
            && monitor.poll(Instant::now())
        {
            redraw = true;
        }
        if pending.is_none() && Instant::now() >= next_refresh {
            if let Some(start) = &fetch {
                pending = Some(start());
                app.loading = true;
                redraw = true;
            }
            next_refresh = Instant::now() + REFRESH_INTERVAL;
        }
        if redraw || last_draw.elapsed() >= Duration::from_secs(1) {
            terminal.draw(|frame| draw(frame, &app))?;
            last_draw = Instant::now();
            redraw = false;
        }

        tokio::select! {
            result = async { pending.as_mut().expect("fetch guard").as_mut().await }, if pending.is_some() => {
                pending = None;
                app.loading = false;
                match result {
                    Ok(report) => {
                        app.report = Some(report);
                        app.move_scroll(0);
                        app.error = None;
                    }
                    Err(error) => app.error = Some(format!("{error:#}")),
                }
                app.warnings = initial_warnings.clone();
                app.warnings.extend(diagnostics.take());
                redraw = true;
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }

        while event::poll(Duration::ZERO)? {
            redraw = true;
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(());
                    }
                    KeyCode::Up | KeyCode::Char('k') => app.move_scroll(-1),
                    KeyCode::Down | KeyCode::Char('j') => app.move_scroll(1),
                    KeyCode::Home => app.scroll = 0,
                    KeyCode::End => app.scroll = app.len().saturating_sub(1),
                    KeyCode::PageUp => app.move_scroll(-10),
                    KeyCode::PageDown => app.move_scroll(10),
                    _ => {}
                },
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
}

fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < 48 || area.height < 9 {
        frame.render_widget(
            Paragraph::new(format!(
                "Terminal too small ({}×{}). Need 48×9. q: quit",
                area.width, area.height
            )),
            area,
        );
        return;
    }

    let header = if app.warnings.is_empty() { 3 } else { 4 };
    let footer = 2;
    // ディスク欄はアカウント一覧の下、キー操作の説明の上に置く。高さが足りなければ一覧を優先する。
    let disk = disk::placement(
        app.disk.as_ref().map_or(0, |monitor| monitor.rows().len()),
        area.height,
        header,
        footer,
    );
    let chunks = Layout::vertical([
        Constraint::Length(header),
        Constraint::Min(3),
        Constraint::Length(disk.height()),
        Constraint::Length(footer),
    ])
    .split(area);

    let state = if app.loading {
        if app.report.is_some() {
            "Refreshing usage…".to_string()
        } else {
            "Fetching usage…".to_string()
        }
    } else if let Some(error) = &app.error {
        format!("Fetch failed: {error}")
    } else if let Some(report) = &app.report {
        let updated = parse_utc(&report.generated_at)
            .map(|date| {
                date.with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_else(|| report.generated_at.clone());
        format!("Updated {updated}  ·  {} accounts", report.accounts.len())
    } else {
        "No data".to_string()
    };
    let mut status = vec![Line::from(state)];
    if !app.warnings.is_empty() {
        status.push(Line::from(format!("Warning: {}", app.warnings.join("; "))));
    }
    frame.render_widget(
        Paragraph::new(status).block(Block::bordered().title(" ai-usage · usage dashboard ")),
        chunks[0],
    );

    // 期限の境界で並び順と行ごとの判定がずれないよう、現在時刻は 1 回の描画で 1 度だけ取る。
    let now = Utc::now();
    let rows = app.rows(now);
    // 更新で行数が減った場合も、リストと位置表示には同じ有効な位置を使う。
    let scroll = app.scroll.min(rows.len().saturating_sub(1));
    // List は表示領域より高い項目を丸ごと省くため、行へ展開して見える部分を描く。
    let lines: Vec<_> = rows
        .iter()
        .skip(scroll)
        .flat_map(|account| account_lines(account, app.color, chunks[1].width, now))
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(format!(
            " Accounts · from {}/{} ",
            if rows.is_empty() { 0 } else { scroll + 1 },
            rows.len()
        ))),
        chunks[1],
    );
    if rows.is_empty() {
        let message = if app.loading {
            "Loading accounts…".to_string()
        } else if let Some(error) = &app.error {
            format!("Could not fetch usage: {error}")
        } else {
            "No accounts to display".to_string()
        };
        frame.render_widget(
            Paragraph::new(message).wrap(Wrap { trim: true }),
            inner(chunks[1]),
        );
    }

    if let Some(monitor) = &app.disk {
        disk::render(frame, chunks[2], monitor, disk, app.color, Instant::now());
    }

    frame.render_widget(
        Paragraph::new("↑↓ / j k: scroll   Home/End/PgUp/PgDn: move   q/Esc/Ctrl-C: quit\nBars = used quota · reset = time left · auto-refresh every 1m"),
        chunks[3],
    );
}

/// palette の色を ratatui の Style にする。色を使わないときは、statusline と同じく太字も付けない。
fn style(color: bool, paint: Paint) -> Style {
    if !color {
        return Style::default();
    }
    let style = Style::default().fg(match paint.fg {
        Fg::Indexed(index) => Color::Indexed(index),
        Fg::Rgb(r, g, b) => Color::Rgb(r, g, b),
    });
    if paint.bold {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

fn inner(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

/// 1 アカウントの行。statusline と共通の要素は palette の同じ色で塗り、太字や色は
/// Span ごとに付ける (親の Line に置くと、色を付けない Span にも引き継がれる)。
pub(super) fn account_lines(
    account: &AccountOut,
    color: bool,
    area_width: u16,
    now: DateTime<Utc>,
) -> Vec<Line<'static>> {
    let label = style(color, palette::LABEL);
    let name = display_name(
        account.label.as_deref(),
        account.email.as_deref(),
        account.profile_email.as_deref(),
        &account.profile,
    );
    let provider = account.provider.label();
    let group = account
        .group_label
        .as_deref()
        .map(|s| format!(" · {s}"))
        .unwrap_or_default();
    // provider 名は statusline 左端の印と同じ色。名前・モデルグループ・プランは名前の欄と同じ LABEL。
    let mut heading = vec![Span::styled(
        provider,
        style(color, palette::marker(account.provider)),
    )];
    if name != provider || !group.is_empty() {
        heading.push(Span::styled(format!("{group}  {name}"), label));
    }
    if let Some(plan) = account.plan.as_deref().filter(|plan| !plan.is_empty()) {
        heading.push(Span::styled(format!("  · {plan}"), label));
    }
    let mut lines = vec![Line::from(heading)];
    if !account.ok {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("Error: ", style(color, palette::ALERT)),
            Span::styled(
                account
                    .error
                    .as_deref()
                    .unwrap_or("Unknown error")
                    .to_string(),
                label,
            ),
        ]));
    } else {
        for (window, short) in [
            (account.short.as_ref(), true),
            (account.long.as_ref(), false),
        ] {
            if let Some(window) = window {
                lines.push(Line::from(window_spans(
                    window,
                    account.provider,
                    short,
                    color,
                    area_width,
                    now,
                )));
            }
        }
        if account.short.is_none()
            && account.long.is_none()
            && !account
                .limit_observation
                .as_ref()
                .is_some_and(|observation| limit_observation_age(observation, now).is_some())
        {
            lines.push(Line::from(Span::styled(
                "  No usage quota reported",
                style(color, palette::MUTED),
            )));
        }
        if let Some(observation) = account.limit_observation.as_ref()
            && let Some(age) = limit_observation_age(observation, now)
        {
            // statusline の `24h LIMIT` と同じく、観測の記録全体を ALERT で塗る。
            let alert = style(color, palette::ALERT);
            lines.push(Line::from(Span::styled(
                format!("  24h  LIMIT HIT  {} · seen {age} ago", observation.model),
                alert,
            )));
            lines.push(Line::from(Span::styled(
                format!(
                    "       {}/{} tokens at refusal",
                    observation.used_tokens, observation.limit_tokens
                ),
                alert,
            )));
        }
    }
    if let Some(resets) = &account.manual_resets {
        for (index, line) in format_resets_tui(resets, now).into_iter().enumerate() {
            let lead = if index == 0 {
                "  Resets: "
            } else {
                "          "
            };
            let mut spans = vec![Span::styled(lead, label)];
            match line {
                TuiResetLine::Summary(text) => spans.push(Span::styled(text, label)),
                // 期限の日時だけを、statusline と同じく 7 日未満で ALERT にする。
                TuiResetLine::Deadline { prefix, deadline } => {
                    let paint = palette::manual_reset_deadline(deadline.expires_at, now);
                    spans.push(Span::styled(prefix, label));
                    spans.push(Span::styled(deadline.text, style(color, paint)));
                }
            }
            lines.push(Line::from(spans));
        }
    }
    lines.push(Line::default());
    lines
}

fn window_spans(
    window: &WindowOut,
    provider: Provider,
    short: bool,
    color: bool,
    area_width: u16,
    now: DateTime<Utc>,
) -> Vec<Span<'static>> {
    let width = if area_width < 64 {
        5
    } else if area_width < 80 {
        8
    } else {
        12
    };
    let percent = window
        .used_percent
        .filter(|percent| percent.is_finite())
        .map(|percent| percent.clamp(0.0, 100.0));
    let filled = percent.map_or(0, |percent| {
        if percent > 0.0 {
            ((percent * width as f64 / 100.0).ceil() as usize).min(width)
        } else {
            0
        }
    });
    // 点灯部と数字は使用率の段階色、消灯部と不明な使用率は statusline と同じ MUTED。
    let level = percent.map(Level::usage);
    let muted = style(color, palette::MUTED);
    let label = style(color, palette::LABEL);
    let reset = window.resets_at.as_deref().and_then(parse_utc);
    let legacy = if short {
        FIVE_H_TH
    } else {
        legacy_long_window_thresholds(provider)
    };
    let reset_style = style(
        color,
        palette::reset_countdown(
            reset.map(|time| (time - now).num_seconds()),
            window_thresholds(Some(window), legacy),
        ),
    );
    let reset_at = reset
        .map(|time| {
            let local = time.with_timezone(&Local);
            let pattern = if local.year() == now.with_timezone(&Local).year() {
                "%m/%d %H:%M"
            } else {
                "%Y/%m/%d %H:%M"
            };
            format!(" ({})", local.format(pattern))
        })
        .unwrap_or_default();
    let mut spans = vec![
        Span::styled(
            format!("  {:<2}", kind_label(window, provider, short)),
            label,
        ),
        Span::raw("  "),
        Span::styled(
            "█".repeat(filled),
            level.map_or(muted, |level| style(color, level.fill())),
        ),
        Span::styled("░".repeat(width - filled), muted),
        Span::raw("  "),
        Span::styled(
            percent.map_or(" --%".to_string(), |p| format!("{p:>3.0}%")),
            level.map_or(muted, |level| style(color, level.text())),
        ),
        Span::styled("  reset ", label),
        Span::styled(reset_text(window, now), reset_style),
    ];
    // リセットの日時は statusline の `--reset-at` と同じく MUTED。
    if !reset_at.is_empty() {
        spans.push(Span::styled(reset_at, muted));
    }
    spans
}

fn kind_label(window: &WindowOut, provider: Provider, short: bool) -> &'static str {
    window.kind.map(WindowKind::label).unwrap_or(if short {
        "5h"
    } else {
        match provider {
            Provider::PixelLab | Provider::Grok => "1m",
            _ => "1w",
        }
    })
}

fn reset_text(window: &WindowOut, now: DateTime<Utc>) -> String {
    let Some(reset) = window.resets_at.as_deref().and_then(parse_utc) else {
        return "?".into();
    };
    let seconds = (reset - now).num_seconds();
    if seconds <= 0 {
        return "due".into();
    }
    // 期限前の1〜59秒は、表と同じく最低1分として表示する。
    let seconds = seconds.max(60);
    let days = seconds / 86_400;
    let hours = seconds % 86_400 / 3_600;
    let minutes = seconds % 3_600 / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn scroll_stays_in_range() {
        let report: Report = serde_json::from_value(serde_json::json!({
            "generated_at": "2026-10-10T00:00:00Z",
            "accounts": [
                {"profile":"Home", "provider":"claude", "ok":true, "plan":null,
                 "email":null, "profile_email":null, "label":null, "group_label":null,
                 "five_hour":null, "weekly":null, "error":null},
                {"profile":"Work", "provider":"codex", "ok":true, "plan":null,
                 "email":null, "profile_email":null, "label":null, "group_label":null,
                 "five_hour":null, "weekly":null, "error":null}
            ]
        }))
        .unwrap();
        let mut app = App {
            warnings: Vec::new(),
            report: Some(report),
            error: None,
            loading: false,
            scroll: 0,
            sort: SortKey::Provider,
            color: false,
            disk: None,
        };
        app.move_scroll(10);
        assert_eq!(app.scroll, 1);
        app.move_scroll(-10);
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn refreshed_shorter_reports_keep_the_displayed_scroll_in_range() {
        let report: Report = serde_json::from_value(serde_json::json!({
            "generated_at": "2026-10-10T00:00:00Z",
            "accounts": [
                {"profile":"Home", "provider":"claude", "ok":true, "plan":null,
                 "email":null, "profile_email":null, "label":null, "group_label":null,
                 "five_hour":null, "weekly":null, "error":null}
            ]
        }))
        .unwrap();
        // 更新前の3行目までスクロールした状態で、取得結果が1行、続いて0行になる。
        let mut app = App {
            warnings: Vec::new(),
            report: Some(report),
            error: None,
            loading: false,
            scroll: 2,
            sort: SortKey::Provider,
            color: false,
            disk: None,
        };
        for (count, expected) in [(1, "from 1/1"), (0, "from 0/0")] {
            app.report.as_mut().unwrap().accounts.truncate(count);
            let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
            terminal.draw(|frame| draw(frame, &app)).unwrap();
            let buffer = terminal.backend().buffer();
            let screen = (0..12)
                .map(|y| {
                    (0..80)
                        .map(|x| buffer.cell((x, y)).unwrap().symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(screen.contains(expected), "{screen}");
            assert!(
                screen.contains(if count == 0 {
                    "No accounts to display"
                } else {
                    "Claude"
                }),
                "{screen}"
            );
        }
    }

    #[test]
    fn captured_warnings_render_inside_the_dashboard() {
        let app = App {
            warnings: vec!["skipping Chrome profiles: keychain denied".into()],
            report: None,
            error: None,
            loading: false,
            scroll: 0,
            sort: SortKey::Provider,
            color: false,
            disk: None,
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            screen.contains("Warning: skipping Chrome profiles: keychain denied"),
            "{screen}"
        );
    }

    #[test]
    fn reset_text_ignores_stale_cached_countdown() {
        let window = WindowOut {
            kind: Some(WindowKind::Weekly),
            used_percent: Some(12.0),
            resets_at: Some((Utc::now() + chrono::Duration::hours(2)).to_rfc3339()),
            resets_in_seconds: Some(1),
        };
        assert!(reset_text(&window, Utc::now()).contains("1h"));
        let absolute = window
            .resets_at
            .as_deref()
            .and_then(parse_utc)
            .unwrap()
            .with_timezone(&Local)
            .format("%m/%d %H:%M")
            .to_string();
        let line = Line::from(window_spans(
            &window,
            Provider::Grok,
            false,
            false,
            80,
            Utc::now(),
        ));
        assert!(
            line.spans
                .iter()
                .any(|span| span.content.contains(&absolute))
        );
    }

    /// 「reset 」の後ろに置く、リセットまでの残り時間の Span。
    fn countdown<'a>(spans: &'a [Span<'static>]) -> &'a Span<'static> {
        let label = spans
            .iter()
            .position(|span| span.content == "  reset ")
            .expect("reset label");
        &spans[label + 1]
    }

    #[test]
    fn reset_countdown_uses_period_colors_and_the_date_is_muted() {
        let now = parse_utc("2026-10-11T00:00:00Z").unwrap();
        for (kind, thresholds) in [
            (WindowKind::FiveHour, [3600, 7200, 10800]),
            (WindowKind::Daily, [14400, 28800, 43200]),
            (WindowKind::Weekly, [86400, 172800, 345600]),
            (WindowKind::Monthly, [259200, 604800, 1209600]),
        ] {
            for (seconds, index) in [
                (1, 196),
                (thresholds[0] - 1, 196),
                (thresholds[0], 208),
                (thresholds[1] - 1, 208),
                (thresholds[1], 178),
                (thresholds[2] - 1, 178),
                (thresholds[2], 35),
            ] {
                let reset = now + chrono::Duration::seconds(seconds);
                let window = WindowOut {
                    kind: Some(kind),
                    used_percent: Some(12.0),
                    resets_at: Some(reset.to_rfc3339()),
                    resets_in_seconds: Some(1),
                };
                // 明示 kind は、プロバイダーや格納先の旧周期より優先する。
                for short in [true, false] {
                    let spans = window_spans(&window, Provider::Grok, short, true, 80, now);
                    assert_eq!(
                        countdown(&spans).style.fg,
                        Some(Color::Indexed(index)),
                        "{kind:?}, {seconds}s, short={short}"
                    );
                    // 日時は statusline の `--reset-at` と同じく、残り時間に関係なく MUTED。
                    let date = reset.with_timezone(&Local).format("%m/%d %H:%M");
                    let last = spans.last().unwrap();
                    assert_eq!(last.content, format!(" ({date})"));
                    assert_eq!(last.style.fg, Some(Color::Indexed(242)));
                    assert_eq!(spans[2].style.fg, Some(Color::Indexed(35)));
                }
            }
        }
    }

    #[test]
    fn reset_colors_use_legacy_periods_without_trusting_cached_seconds() {
        let now = parse_utc("2026-10-11T00:00:00Z").unwrap();
        let window = WindowOut {
            kind: None,
            used_percent: Some(12.0),
            resets_at: Some((now + chrono::Duration::days(2)).to_rfc3339()),
            resets_in_seconds: Some(1),
        };
        for (provider, short, index) in [
            (Provider::Claude, false, 178),
            (Provider::Codex, false, 178),
            (Provider::Antigravity, false, 178),
            (Provider::PixelLab, false, 196),
            (Provider::Grok, false, 196),
            (Provider::Grok, true, 35),
        ] {
            let spans = window_spans(&window, provider, short, true, 80, now);
            let countdown = countdown(&spans);
            assert_eq!(countdown.style.fg, Some(Color::Indexed(index)));
            assert_eq!(countdown.content, "2d 0h");
        }
    }

    #[test]
    fn reset_colors_handle_unknown_due_and_color_suppression() {
        let now = parse_utc("2026-10-11T00:00:00Z").unwrap();
        let due = now.to_rfc3339();
        let past = (now - chrono::Duration::days(1)).to_rfc3339();
        let future = (now + chrono::Duration::hours(1)).to_rfc3339();
        for (reset, index, text) in [
            (None, 242, "?"),
            (Some("invalid"), 242, "?"),
            (Some(due.as_str()), 35, "due"),
            (Some(past.as_str()), 35, "due"),
            (Some(future.as_str()), 196, "1h 0m"),
        ] {
            let window = WindowOut {
                kind: Some(WindowKind::Weekly),
                used_percent: Some(12.0),
                resets_at: reset.map(str::to_string),
                resets_in_seconds: Some(1),
            };
            let colored = window_spans(&window, Provider::Claude, false, true, 80, now);
            let plain = window_spans(&window, Provider::Claude, false, false, 80, now);
            assert_eq!(countdown(&colored).style.fg, Some(Color::Indexed(index)));
            assert_eq!(countdown(&colored).content, text);
            // 色を使わないときは、statusline と同じく色も太字も付けない。
            assert!(plain.iter().all(|span| span.style == Style::default()));
            assert_eq!(
                Line::from(colored).to_string(),
                Line::from(plain).to_string()
            );
        }
    }

    #[test]
    fn window_line_uses_the_statusline_colors() {
        let now = parse_utc("2026-10-11T00:00:00Z").unwrap();
        let window = |percent| WindowOut {
            kind: Some(WindowKind::Weekly),
            used_percent: percent,
            resets_at: Some((now + chrono::Duration::days(2)).to_rfc3339()),
            resets_in_seconds: None,
        };
        let fg = |span: &Span| span.style.fg;
        let bold = |span: &Span| span.style.add_modifier.contains(Modifier::BOLD);
        // (使用率, 点灯部と数字の色, 数字の太字)。数字は 80% から太字。
        for (percent, index, strong) in [
            (12.0, 35, false),
            (62.0, 178, false),
            (85.0, 208, true),
            (95.0, 196, true),
        ] {
            let spans = window_spans(
                &window(Some(percent)),
                Provider::Claude,
                false,
                true,
                80,
                now,
            );
            let [
                label,
                _,
                filled,
                empty,
                _,
                number,
                reset_label,
                countdown,
                date,
            ] = &spans[..]
            else {
                panic!("{spans:?}");
            };
            assert_eq!(label.content, "  1w");
            assert_eq!(fg(label), Some(Color::Indexed(245)));
            assert_eq!(fg(filled), Some(Color::Indexed(index)), "{percent}");
            assert_eq!(fg(empty), Some(Color::Indexed(242)), "{percent}");
            assert_eq!(fg(number), Some(Color::Indexed(index)), "{percent}");
            assert_eq!(bold(number), strong, "{percent}");
            assert_eq!(fg(reset_label), Some(Color::Indexed(245)));
            assert_eq!(fg(countdown), Some(Color::Indexed(178)));
            assert_eq!(fg(date), Some(Color::Indexed(242)));
            // 太字は数字だけ。枠のラベルやバーには付けない。
            for span in [label, filled, empty, reset_label, countdown, date] {
                assert!(!bold(span), "{span:?}");
            }
        }
        // 使用率が不明なら、バー全体と `--%` を MUTED にする。
        let spans = window_spans(&window(None), Provider::Claude, false, true, 80, now);
        assert_eq!(spans[2].content, "");
        assert_eq!(spans[3].content, "░".repeat(12));
        assert_eq!(spans[3].style.fg, Some(Color::Indexed(242)));
        assert_eq!(spans[5].content, " --%");
        assert_eq!(spans[5].style.fg, Some(Color::Indexed(242)));
        assert!(!spans[5].style.add_modifier.contains(Modifier::BOLD));
    }

    fn account(value: serde_json::Value) -> AccountOut {
        let mut base = serde_json::json!({
            "profile":"Home", "provider":"claude", "ok":true, "plan":null,
            "email":null, "profile_email":null, "label":null, "group_label":null,
            "five_hour":null, "weekly":null, "error":null
        });
        base.as_object_mut()
            .unwrap()
            .extend(value.as_object().unwrap().clone());
        serde_json::from_value(base).unwrap()
    }

    /// `text` と同じ内容の Span。
    fn span<'a>(lines: &'a [Line<'static>], text: &str) -> &'a Span<'static> {
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content == text)
            .unwrap_or_else(|| panic!("{text:?} が無い: {lines:#?}"))
    }

    #[test]
    fn account_lines_use_the_statusline_colors() {
        let now = parse_utc("2026-10-11T00:00:00Z").unwrap();
        let at = |delta: chrono::Duration| (now + delta).to_rfc3339();
        let date = |delta: chrono::Duration| {
            (now + delta)
                .with_timezone(&Local)
                .format("%m/%d %H:%M")
                .to_string()
        };
        let claude = account(serde_json::json!({
            "label": "home", "plan": "Max",
            "weekly": {"kind":"weekly","used_percent":85.0,"resets_at":at(chrono::Duration::days(2)),"resets_in_seconds":null},
            "manual_resets": [
                {"kind":"full","remaining":1,"expires_at":at(chrono::Duration::days(1))},
                {"kind":"full","remaining":1,"expires_at":at(chrono::Duration::days(10))},
                {"kind":"five_hour","remaining":0,"expires_at":null}
            ]
        }));
        let lines = account_lines(&claude, true, 80, now);
        let fg = |text: &str| span(&lines, text).style.fg;
        // 見出し: provider 名は statusline の印と同じブランド色、名前とプランは LABEL。太字にしない。
        assert_eq!(fg("Claude"), Some(Color::Rgb(217, 119, 87)));
        assert_eq!(fg("  home"), Some(Color::Indexed(245)));
        assert_eq!(fg("  · Max"), Some(Color::Indexed(245)));
        // 手動リセット: 件数と `1@` は LABEL、7 日未満で切れる日時だけ ALERT。
        assert_eq!(fg("  Resets: "), Some(Color::Indexed(245)));
        assert_eq!(fg("full 2"), Some(Color::Indexed(245)));
        assert_eq!(fg("5h 0"), Some(Color::Indexed(245)));
        assert_eq!(fg("  1@"), Some(Color::Indexed(245)));
        assert_eq!(
            fg(&date(chrono::Duration::days(1))),
            Some(Color::Indexed(196))
        );
        assert_eq!(
            fg(&date(chrono::Duration::days(10))),
            Some(Color::Indexed(245))
        );
        // 太字は 80% 以上の数字だけ。
        let bold: Vec<_> = lines
            .iter()
            .flat_map(|line| &line.spans)
            .filter(|span| span.style.add_modifier.contains(Modifier::BOLD))
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(bold, [" 85%"]);
        // 親の Line に色や太字を置かない (色を付けない Span に引き継がれるため)。
        assert!(lines.iter().all(|line| line.style == Style::default()));

        // Codex と Grok の provider 名は statusline の印と同じく白。
        for provider in ["codex", "grok"] {
            let lines = account_lines(
                &account(serde_json::json!({"provider": provider})),
                true,
                80,
                now,
            );
            assert_eq!(
                lines[0].spans[0].style.fg,
                Some(Color::Rgb(255, 255, 255)),
                "{provider}"
            );
        }

        // 取得の失敗: `Error:` は ALERT、メッセージは LABEL。
        let failed = account(serde_json::json!({"ok": false, "error": "fetch failed"}));
        let lines = account_lines(&failed, true, 80, now);
        assert_eq!(fg_of(&lines, "Error: "), Some(Color::Indexed(196)));
        assert_eq!(fg_of(&lines, "fetch failed"), Some(Color::Indexed(245)));

        // 枠が無い行は MUTED。
        let empty = account(serde_json::json!({}));
        let lines = account_lines(&empty, true, 80, now);
        assert_eq!(
            fg_of(&lines, "  No usage quota reported"),
            Some(Color::Indexed(242))
        );

        // 24 時間枠の制限は、statusline の `24h LIMIT` と同じく記録全体を ALERT にする。
        let limited = account(serde_json::json!({
            "provider": "grok",
            "limit_observation": {"model":"grok-test","observed_at":at(-chrono::Duration::minutes(5)),
                                  "used_tokens":120,"limit_tokens":100}
        }));
        let lines = account_lines(&limited, true, 80, now);
        for text in [
            "  24h  LIMIT HIT  grok-test · seen 5m ago",
            "       120/100 tokens at refusal",
        ] {
            let limit = span(&lines, text);
            assert_eq!(limit.style.fg, Some(Color::Indexed(196)), "{text}");
            assert!(!limit.style.add_modifier.contains(Modifier::BOLD), "{text}");
        }
    }

    fn fg_of(lines: &[Line<'static>], text: &str) -> Option<Color> {
        span(lines, text).style.fg
    }

    /// `text` が始まるセル。1 文字が 1 セルを占める文字列だけを探す。
    fn cell_at<'a>(buffer: &'a ratatui::buffer::Buffer, text: &str) -> &'a ratatui::buffer::Cell {
        let chars: Vec<String> = text.chars().map(String::from).collect();
        let area = buffer.area;
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let found = chars.iter().enumerate().all(|(offset, ch)| {
                    u16::try_from(offset)
                        .ok()
                        .and_then(|offset| buffer.cell((x.checked_add(offset)?, y)))
                        .is_some_and(|cell| cell.symbol() == ch)
                });
                if found {
                    return buffer.cell((x, y)).unwrap();
                }
            }
        }
        panic!("{text:?} が画面に無い");
    }

    #[test]
    fn redrawing_the_same_terminal_replaces_colors_and_bold() {
        let now = Utc::now();
        let at = |delta: chrono::Duration| (now + delta).to_rfc3339();
        let date = |delta: chrono::Duration| {
            (now + delta)
                .with_timezone(&Local)
                .format("%m/%d %H:%M")
                .to_string()
        };
        let report = |percent: f64, expiry: chrono::Duration| Report {
            generated_at: now.to_rfc3339(),
            accounts: vec![account(serde_json::json!({
                "label": "home",
                "five_hour": {"kind":"five_hour","used_percent":percent,
                              "resets_at":at(chrono::Duration::hours(4)),"resets_in_seconds":null},
                "manual_resets": [{"kind":"full","remaining":1,"expires_at":at(expiry)}]
            }))],
        };
        let mut app = App {
            warnings: Vec::new(),
            report: Some(report(95.0, chrono::Duration::days(1))),
            error: None,
            loading: false,
            scroll: 0,
            sort: SortKey::Provider,
            color: true,
            disk: None,
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let percent = cell_at(buffer, "95%");
        assert_eq!(percent.fg, Color::Indexed(196));
        assert!(percent.modifier.contains(Modifier::BOLD));
        let heading = cell_at(buffer, "Claude");
        assert_eq!(heading.fg, Color::Rgb(217, 119, 87));
        assert!(!heading.modifier.contains(Modifier::BOLD));
        let deadline = date(chrono::Duration::days(1));
        assert_eq!(cell_at(buffer, &deadline).fg, Color::Indexed(196));

        // 同じ端末へ描き直すと、前の描画の色や太字はセルに残らない。
        app.report = Some(report(20.0, chrono::Duration::days(10)));
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let percent = cell_at(buffer, "20%");
        assert_eq!(percent.fg, Color::Indexed(35));
        let deadline = date(chrono::Duration::days(10));
        assert_eq!(cell_at(buffer, &deadline).fg, Color::Indexed(245));
        assert!(
            buffer
                .content()
                .iter()
                .all(|cell| !cell.modifier.contains(Modifier::BOLD))
        );
    }

    #[test]
    fn account_lines_without_color_have_no_style_and_the_same_text() {
        let now = parse_utc("2026-10-11T00:00:00Z").unwrap();
        let soon = (now + chrono::Duration::days(1)).to_rfc3339();
        for value in [
            serde_json::json!({
                "label": "home", "plan": "Max",
                "five_hour": {"kind":"five_hour","used_percent":95.0,"resets_at":soon,"resets_in_seconds":null},
                "manual_resets": [{"kind":"full","remaining":1,"expires_at":soon}]
            }),
            serde_json::json!({"ok": false, "error": "fetch failed"}),
            serde_json::json!({
                "provider": "grok",
                "limit_observation": {"model":"grok-test","observed_at":now.to_rfc3339(),
                                      "used_tokens":120,"limit_tokens":100}
            }),
        ] {
            let account = account(value);
            let colored = account_lines(&account, true, 80, now);
            let plain = account_lines(&account, false, 80, now);
            assert!(
                plain
                    .iter()
                    .flat_map(|line| &line.spans)
                    .all(|span| span.style == Style::default()),
                "{plain:#?}"
            );
            let text = |lines: &[Line]| lines.iter().map(Line::to_string).collect::<Vec<_>>();
            assert_eq!(text(&colored), text(&plain));
        }
    }

    #[test]
    fn legacy_short_and_long_windows_have_distinct_badges() {
        let window = WindowOut {
            kind: None,
            used_percent: Some(20.0),
            resets_at: None,
            resets_in_seconds: None,
        };
        assert_eq!(kind_label(&window, Provider::Claude, true), "5h");
        assert_eq!(kind_label(&window, Provider::Claude, false), "1w");
        assert_eq!(kind_label(&window, Provider::Grok, false), "1m");
    }

    #[test]
    fn unknown_percent_is_not_shown_as_zero_in_tui() {
        let window = WindowOut {
            kind: Some(WindowKind::Weekly),
            used_percent: None,
            resets_at: None,
            resets_in_seconds: None,
        };
        let line = Line::from(window_spans(
            &window,
            Provider::Grok,
            false,
            false,
            60,
            Utc::now(),
        ));
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("1w") && text.contains("--%"), "{text}");
        assert!(!text.contains("0%"), "{text}");
    }

    #[test]
    fn tui_shows_each_manual_reset_expiry_and_recent_grok_limit() {
        let expiry_a = (Utc::now() + chrono::Duration::days(1)).to_rfc3339();
        let expiry_b = (Utc::now() + chrono::Duration::days(2)).to_rfc3339();
        let report: Report = serde_json::from_value(serde_json::json!({
            "generated_at": Utc::now().to_rfc3339(),
            "accounts": [
                {"profile":"Home","provider":"claude","ok":true,"plan":null,
                 "email":null,"profile_email":null,"label":null,"group_label":null,
                 "five_hour":null,"weekly":null,"error":null,
                 "manual_resets":[
                    {"kind":"full","remaining":1,"expires_at":expiry_a},
                    {"kind":"full","remaining":1,"expires_at":expiry_b}
                 ]},
                {"profile":"Home","provider":"grok","ok":true,"plan":"Free",
                 "email":null,"profile_email":null,"label":null,"group_label":null,
                 "five_hour":null,"weekly":null,"error":null,
                 "limit_observation":{"model":"grok-test","observed_at":Utc::now().to_rfc3339(),
                                      "used_tokens":120,"limit_tokens":100}}
            ]
        }))
        .unwrap();
        let app = App {
            warnings: Vec::new(),
            report: Some(report),
            error: None,
            loading: false,
            scroll: 0,
            sort: SortKey::Provider,
            color: false,
            disk: None,
        };
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let screen = (0..20)
            .map(|y| {
                (0..100)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("Resets: full 2"), "{screen}");
        assert_eq!(screen.matches("1@").count(), 2, "{screen}");
        assert!(screen.contains("24h  LIMIT HIT"), "{screen}");
        assert!(screen.contains("grok-test · seen"), "{screen}");
        assert!(screen.contains("120/100 tokens at refusal"), "{screen}");
    }

    #[test]
    fn reset_text_handles_minute_hour_and_day_boundaries() {
        let now = parse_utc("2026-10-19T00:00:00Z").unwrap();
        let mut window = WindowOut {
            kind: Some(WindowKind::FiveHour),
            used_percent: Some(10.0),
            resets_at: None,
            resets_in_seconds: Some(999),
        };
        for (seconds, expected) in [
            (-1, "due"),
            (0, "due"),
            (1, "1m"),
            (59, "1m"),
            (60, "1m"),
            (61, "1m"),
            (3599, "59m"),
            (3600, "1h 0m"),
            (86399, "23h 59m"),
            (86400, "1d 0h"),
        ] {
            window.resets_at = Some((now + chrono::Duration::seconds(seconds)).to_rfc3339());
            assert_eq!(reset_text(&window, now), expected, "{seconds} seconds");
        }
        for reset in [None, Some("invalid".to_string())] {
            window.resets_at = reset;
            assert_eq!(reset_text(&window, now), "?");
        }
    }

    #[test]
    fn short_terminal_keeps_an_oversized_account_visible() {
        let report: Report = serde_json::from_value(serde_json::json!({
            "generated_at": "2026-10-10T00:00:00Z",
            "accounts": [{
                "profile":"Home", "provider":"claude", "ok":true,
                "plan":null, "email":null, "profile_email":null, "label":null,
                "group_label":null, "error":null,
                "five_hour":{"used_percent":62.0,"resets_at":null,"resets_in_seconds":null},
                "weekly":{"used_percent":85.0,"resets_at":null,"resets_in_seconds":null}
            }]
        }))
        .unwrap();
        let app = App {
            warnings: Vec::new(),
            report: Some(report),
            error: None,
            loading: false,
            scroll: 0,
            sort: SortKey::Provider,
            color: false,
            disk: None,
        };
        let mut terminal = Terminal::new(TestBackend::new(48, 9)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let screen = (0..9)
            .map(|y| {
                (0..48)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("Claude"), "{screen}");
        assert!(screen.contains("62%"), "{screen}");
        assert!(screen.contains("from 1/1"), "{screen}");
    }

    #[test]
    fn overview_groups_periods_under_each_account_without_active_marker() {
        let report: Report = serde_json::from_value(serde_json::json!({
            "generated_at": "2026-10-10T00:00:00Z",
            "accounts": [
                {"profile":"Home", "provider":"claude", "ok":true, "plan":"Max",
                 "email":null, "profile_email":null, "label":"home", "group_label":null,
                 "five_hour":{"kind":"five_hour","used_percent":62.0,"resets_at":null,"resets_in_seconds":null},
                 "weekly":{"kind":"weekly","used_percent":85.0,"resets_at":null,"resets_in_seconds":null},
                 "error":null},
                {"profile":"Work", "provider":"codex", "ok":true, "plan":"Pro",
                 "email":null, "profile_email":null, "label":"work", "group_label":null,
                 "five_hour":{"kind":"five_hour","used_percent":40.0,"resets_at":null,"resets_in_seconds":null},
                 "weekly":{"kind":"weekly","used_percent":80.0,"resets_at":null,"resets_in_seconds":null},
                 "error":null},
                {"profile":"Other", "provider":"pixellab", "ok":true, "plan":"Tier 1",
                 "email":null, "profile_email":null, "label":"other", "group_label":null,
                 "five_hour":null,
                 "weekly":{"kind":"monthly","used_percent":46.0,"resets_at":null,"resets_in_seconds":null},
                 "error":null}
            ]
        })).unwrap();
        let mut app = App {
            warnings: Vec::new(),
            report: Some(report),
            error: None,
            loading: false,
            scroll: 0,
            sort: SortKey::Provider,
            color: false,
            disk: None,
        };
        for width in [48, 60, 80] {
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal.draw(|frame| draw(frame, &app)).unwrap();
            let buffer = terminal.backend().buffer();
            let screen = (0..24)
                .map(|y| {
                    (0..width)
                        .map(|x| buffer.cell((x, y)).unwrap().symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            for expected in [
                "Claude", "62%", "85%", "Codex", "40%", "80%", "PixelLab", "46%",
            ] {
                assert!(
                    screen.contains(expected),
                    "missing {expected} at {width} columns in:\n{screen}"
                );
            }
            let lines: Vec<_> = screen.lines().collect();
            let claude = lines
                .iter()
                .position(|line| line.contains("Claude"))
                .unwrap();
            let codex = lines
                .iter()
                .position(|line| line.contains("Codex"))
                .unwrap();
            let pixellab = lines
                .iter()
                .position(|line| line.contains("PixelLab"))
                .unwrap();
            assert!(claude < codex && codex < pixellab, "{screen}");
            assert!(
                lines[claude + 1].contains("5h") && lines[claude + 1].contains("62%"),
                "{screen}"
            );
            assert!(
                lines[claude + 2].contains("1w") && lines[claude + 2].contains("85%"),
                "{screen}"
            );
            assert!(
                lines[codex + 1].contains("5h") && lines[codex + 1].contains("40%"),
                "{screen}"
            );
            assert!(
                lines[codex + 2].contains("1w") && lines[codex + 2].contains("80%"),
                "{screen}"
            );
            assert!(
                lines[pixellab + 1].contains("1m") && lines[pixellab + 1].contains("46%"),
                "{screen}"
            );
            assert!(!screen.contains('●') && !screen.contains('❯'), "{screen}");
        }
        app.move_scroll(1);
        let mut terminal = Terminal::new(TestBackend::new(48, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let screen = (0..24)
            .map(|y| {
                (0..48)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !screen.contains("Claude") && screen.contains("Codex"),
            "{screen}"
        );
    }

    fn screen_lines(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect()
            })
            .collect()
    }

    fn app_with_disk(warnings: Vec<String>) -> App {
        let report: Report = serde_json::from_value(serde_json::json!({
            "generated_at": "2026-10-10T00:00:00Z",
            "accounts": [{
                "profile":"Home", "provider":"claude", "ok":true,
                "plan":null, "email":null, "profile_email":null, "label":null,
                "group_label":null, "error":null,
                "five_hour":{"used_percent":62.0,"resets_at":null,"resets_in_seconds":null},
                "weekly":{"used_percent":85.0,"resets_at":null,"resets_in_seconds":null}
            }]
        }))
        .unwrap();
        let now = Instant::now();
        let mut row = crate::disk::Row::new(DiskTarget::Startup, now);
        let volume = crate::disk::Volume::new(
            "Macintosh HD".into(),
            494_384_795_648,
            13_080_645_632,
            Some(70_754_759_808),
        )
        .unwrap();
        row.last = Some((volume, now));
        App {
            warnings,
            report: Some(report),
            error: None,
            loading: false,
            scroll: 0,
            sort: SortKey::Provider,
            color: false,
            disk: Some(DiskMonitor::from_rows(vec![row])),
        }
    }

    #[test]
    fn disk_panel_sits_above_the_key_help_and_yields_to_accounts() {
        let app = app_with_disk(Vec::new());
        let lines = screen_lines(&app, 80, 24);
        let legend = lines
            .iter()
            .position(|line| line.contains("Disk · █ used ▒ purgeable ░ free"))
            .unwrap();
        // 枠付きの欄 (3 行) の下に、キー操作の説明 (2 行) が残る。
        assert_eq!(legend, 24 - 2 - 3, "{lines:#?}");
        let row = &lines[legend + 1];
        assert!(row.contains("Macintosh HD  █"), "{row}");
        assert!(row.contains("86%  70.8 GB available of 494.4 GB"), "{row}");
        assert!(lines[22].contains("q/Esc/Ctrl-C: quit"), "{lines:#?}");
        assert!(lines.iter().any(|line| line.contains("62%")));
        assert!(lines.iter().any(|line| line.contains("85%")));

        // 最小幅でもバーと利用可能な容量は残す。
        let narrow = screen_lines(&app, 48, 14);
        assert!(
            narrow[10].contains("86%  70.8 GB available") && !narrow[10].contains("of 494.4"),
            "{narrow:#?}"
        );

        // 12〜13 行では枠なしの 1 行にし、それより低い端末ではアカウント一覧に譲る。
        for height in [12, 13] {
            let lines = screen_lines(&app, 80, height);
            assert!(
                lines[usize::from(height) - 3].starts_with("Disk  Macintosh HD  █"),
                "{height}: {lines:#?}"
            );
            assert!(!lines.iter().any(|line| line.contains("purgeable ░ free")));
        }
        for height in [9, 11] {
            let lines = screen_lines(&app, 80, height);
            assert!(
                !lines.iter().any(|line| line.contains("Macintosh HD")),
                "{height}: {lines:#?}"
            );
            assert!(lines.iter().any(|line| line.contains("Claude")));
        }

        // ヘッダーに警告の行が増えると、同じ置き方に 1 行多く要る。
        let warned = app_with_disk(vec!["skipping Chrome profiles: keychain denied".into()]);
        let lines = screen_lines(&warned, 80, 14);
        assert!(lines[11].starts_with("Disk  Macintosh HD"), "{lines:#?}");
        let lines = screen_lines(&warned, 80, 15);
        assert!(lines.iter().any(|line| line.contains("purgeable ░ free")));
    }

    #[test]
    fn disk_panel_lists_extra_volumes_and_summarizes_rows_that_do_not_fit() {
        let mut app = app_with_disk(Vec::new());
        let now = Instant::now();
        let mut unplugged =
            crate::disk::Row::new(DiskTarget::Mount("/Volumes/Archive".into()), now);
        unplugged.error = Some(crate::disk::ReadError::NotMounted);
        let photos = crate::disk::Row::new(DiskTarget::Mount("/Volumes/Photos".into()), now);
        let startup = app.disk.take().unwrap().rows()[0].last.clone().unwrap();
        let mut first = crate::disk::Row::new(DiskTarget::Startup, now);
        first.last = Some(startup);
        app.disk = Some(DiskMonitor::from_rows(vec![first, unplugged, photos]));

        let lines = screen_lines(&app, 80, 24);
        let legend = lines
            .iter()
            .position(|line| line.contains("purgeable ░ free"))
            .unwrap();
        assert_eq!(legend, 24 - 2 - 5, "{lines:#?}");
        // バーの位置を揃えるため、名前の欄は最も長い名前に合わせる。
        assert!(lines[legend + 1].contains("Macintosh HD  █"), "{lines:#?}");
        assert!(
            lines[legend + 2].contains("Archive       unavailable: not mounted"),
            "{lines:#?}"
        );
        assert!(
            lines[legend + 3].contains("Photos        reading…"),
            "{lines:#?}"
        );

        // 一覧の 6 行を残すと 2 行分しか入らない高さでは、残りを見出しにまとめる。
        let lines = screen_lines(&app, 80, 15);
        assert!(
            lines.iter().any(|line| line.contains("free · +1 more")),
            "{lines:#?}"
        );
        assert!(
            !lines.iter().any(|line| line.contains("Photos")),
            "{lines:#?}"
        );
    }
}
