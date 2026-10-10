//! 全アカウントの使用枠を同時に比較できる対話式の端末表示。

use std::future::Future;
use std::io::{self, IsTerminal};
use std::pin::Pin;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use chrono::{DateTime, Local, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};

use super::manual_resets::format_resets_compact;
use super::sort::sorted_refs;
use super::{brand_rgb, display_name, parse_utc};
use crate::SortKey;
use crate::model::{Provider, WindowKind};
use crate::report::{AccountOut, Report, WindowOut};

type Fetch<'a> = Pin<Box<dyn Future<Output = Result<Report>> + 'a>>;

struct RestoreTerminal;

impl Drop for RestoreTerminal {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

struct App {
    report: Option<Report>,
    error: Option<String>,
    loading: bool,
    scroll: usize,
    sort: SortKey,
    color: bool,
}

impl App {
    fn rows(&self) -> Vec<&AccountOut> {
        self.report
            .as_ref()
            .map(|report| sorted_refs(&report.accounts, self.sort, Utc::now(), None))
            .unwrap_or_default()
    }

    fn move_scroll(&mut self, delta: isize) {
        let len = self.rows().len();
        if len > 0 {
            self.scroll = self.scroll.saturating_add_signed(delta).min(len - 1);
        }
    }
}

/// `--tui` のメインループ。通信が終わる前にも取得中画面を表示する。
pub async fn run(
    initial: Option<Report>,
    mut fetch: Option<Fetch<'_>>,
    sort: SortKey,
    color: bool,
) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("--tui requires an interactive terminal (use --json for redirected output)");
    }

    let mut terminal = ratatui::try_init()?;
    let _restore = RestoreTerminal;
    let mut app = App {
        report: initial,
        error: None,
        loading: fetch.is_some(),
        scroll: 0,
        sort,
        color,
    };

    let mut redraw = true;
    let mut last_draw = Instant::now();
    loop {
        if redraw || last_draw.elapsed() >= Duration::from_secs(1) {
            terminal.draw(|frame| draw(frame, &app))?;
            last_draw = Instant::now();
            redraw = false;
        }

        tokio::select! {
            result = async { fetch.as_mut().expect("fetch guard").as_mut().await }, if fetch.is_some() => {
                fetch = None;
                app.loading = false;
                match result {
                    Ok(report) => app.report = Some(report),
                    Err(error) => app.error = Some(format!("{error:#}")),
                }
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
                    KeyCode::End => app.scroll = app.rows().len().saturating_sub(1),
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

    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .split(area);

    let state = if app.loading {
        "Fetching usage…".to_string()
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
    frame.render_widget(
        Paragraph::new(state).block(Block::bordered().title(" ai-usage · usage dashboard ")),
        chunks[0],
    );

    let rows = app.rows();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|account| account_item(account, app.color, chunks[1].width))
        .collect();
    let mut position = ListState::default().with_offset(app.scroll);
    frame.render_stateful_widget(
        List::new(items).block(Block::bordered().title(format!(
            " Accounts · from {}/{} ",
            if rows.is_empty() { 0 } else { app.scroll + 1 },
            rows.len()
        ))),
        chunks[1],
        &mut position,
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

    frame.render_widget(
        Paragraph::new("↑↓ / j k: scroll   Home/End/PgUp/PgDn: move   q/Esc/Ctrl-C: quit\nBars = used quota · reset = time left · restart to refresh"),
        chunks[2],
    );
}

fn inner(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

fn account_item<'a>(account: &AccountOut, color: bool, area_width: u16) -> ListItem<'a> {
    let (r, g, b) = brand_rgb(account.provider);
    let brand = if color {
        Style::default().fg(Color::Rgb(r, g, b))
    } else {
        Style::default()
    };
    let name = display_name(
        account.label.as_deref(),
        account.email.as_deref(),
        account.profile_email.as_deref(),
        &account.profile,
    );
    let group = account
        .group_label
        .as_deref()
        .map(|s| format!(" · {s}"))
        .unwrap_or_default();
    let identity = if name == account.provider.label() && group.is_empty() {
        account.provider.label().to_string()
    } else {
        format!("{}{}  {name}", account.provider.label(), group)
    };
    let mut heading = vec![Span::styled(identity, brand.add_modifier(Modifier::BOLD))];
    if let Some(plan) = account.plan.as_deref().filter(|plan| !plan.is_empty()) {
        heading.push(Span::raw(format!("  · {plan}")));
    }
    let mut lines = vec![Line::from(heading)];
    if !account.ok {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "Error: ",
                if color {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default()
                },
            ),
            Span::raw(
                account
                    .error
                    .as_deref()
                    .unwrap_or("Unknown error")
                    .to_string(),
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
                )));
            }
        }
        if account.short.is_none() && account.long.is_none() {
            lines.push(Line::from("  No usage quota reported"));
        }
    }
    if let Some(resets) = &account.manual_resets {
        let summary = format_resets_compact(resets, Utc::now());
        lines.push(Line::from(format!("  Resets: {}", summary.counts)));
    }
    lines.push(Line::default());
    ListItem::new(lines)
}

fn window_spans(
    window: &WindowOut,
    provider: Provider,
    short: bool,
    color: bool,
    area_width: u16,
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
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(width - filled));
    let bar_style = if color {
        Style::default().fg(percent.map_or(Color::DarkGray, usage_color))
    } else {
        Style::default()
    };
    vec![
        Span::styled(
            format!("  {:<2}  ", kind_label(window, provider, short)),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(bar, bar_style),
        Span::raw(format!(
            "  {}  reset {}",
            percent.map_or(" --%".to_string(), |p| format!("{p:>3.0}%")),
            reset_text(window, Utc::now())
        )),
    ]
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

fn usage_color(percent: f64) -> Color {
    if percent >= 90.0 {
        Color::Red
    } else if percent >= 80.0 {
        Color::LightRed
    } else if percent >= 60.0 {
        Color::Yellow
    } else {
        Color::Green
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
            report: Some(report),
            error: None,
            loading: false,
            scroll: 0,
            sort: SortKey::Provider,
            color: false,
        };
        app.move_scroll(10);
        assert_eq!(app.scroll, 1);
        app.move_scroll(-10);
        assert_eq!(app.scroll, 0);
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
        let line = Line::from(window_spans(&window, Provider::Grok, false, false, 60));
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("1w") && text.contains("--%"), "{text}");
        assert!(!text.contains("0%"), "{text}");
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
            report: Some(report),
            error: None,
            loading: false,
            scroll: 0,
            sort: SortKey::Provider,
            color: false,
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
}
