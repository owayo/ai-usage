//! statusline と TUI が、同じ文字を同じ色で描くことを確かめるテスト。
//!
//! 両レンダラーは palette の同じ関数で色を決めるが、どの要素にどの関数を使うかは
//! それぞれの描画コードが決める。ここでは同じアカウントを両方で描き、共通の文字の色を比べる。

use chrono::{DateTime, Local, Utc};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

use super::statusline::{StatuslineOpts, render_row};
use super::tui::account_lines;
use crate::report::AccountOut;

/// 色の付いた区間。`sgr` は SGR のパラメータ (`38;5;245`)、色の無い区間は None。
struct Segment {
    sgr: Option<String>,
    text: String,
}

/// statusline の 1 行を区間に分ける。`\x1b[0m` で色を外す。
fn statusline_segments(line: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut sgr = None;
    let mut rest = line;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("\x1b[") {
            let end = after.find('m').expect("SGR の終わり");
            sgr = Some(&after[..end])
                .filter(|code| *code != "0")
                .map(str::to_string);
            rest = &after[end + 1..];
        } else {
            let end = rest.find('\x1b').unwrap_or(rest.len());
            segments.push(Segment {
                sgr: sgr.clone(),
                text: rest[..end].to_string(),
            });
            rest = &rest[end..];
        }
    }
    segments
}

/// TUI の Style を、statusline と同じ SGR のパラメータに直す。
fn sgr(style: Style) -> Option<String> {
    let fg = match style.fg? {
        Color::Indexed(index) => format!("38;5;{index}"),
        Color::Rgb(r, g, b) => format!("38;2;{r};{g};{b}"),
        other => panic!("palette に無い色: {other:?}"),
    };
    Some(if style.add_modifier.contains(Modifier::BOLD) {
        format!("1;{fg}")
    } else {
        fg
    })
}

fn tui_segments(lines: &[Line<'static>]) -> Vec<Segment> {
    lines
        .iter()
        .flat_map(|line| &line.spans)
        .map(|span| Segment {
            sgr: sgr(span.style),
            text: span.content.to_string(),
        })
        .collect()
}

/// `text` を含む `nth` 番目の区間の色。
fn color_of(segments: &[Segment], text: &str, nth: usize) -> Option<String> {
    segments
        .iter()
        .filter(|segment| segment.text.contains(text))
        .nth(nth)
        .unwrap_or_else(|| {
            let texts: Vec<_> = segments.iter().map(|segment| &segment.text).collect();
            panic!("{nth} 番目の {text:?} が無い: {texts:?}")
        })
        .sgr
        .clone()
}

fn utc(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339)
        .unwrap()
        .with_timezone(&Utc)
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

/// 同じアカウントを statusline (`--reset-at` 付き) と TUI で描く。
fn render_both(account: &AccountOut, now: DateTime<Utc>) -> (Vec<Segment>, Vec<Segment>) {
    let opts = StatuslineOpts {
        color: true,
        logos: false,
        debug: false,
        compact: false,
        reset_at: true,
        hide: Vec::new(),
    };
    (
        statusline_segments(&render_row(account, None, false, &opts, now)),
        tui_segments(&account_lines(account, true, 100, now)),
    )
}

#[test]
fn shared_text_has_the_same_color_in_the_statusline_and_the_tui() {
    let now = utc("2026-10-11T00:00:00Z");
    let at = |delta: chrono::Duration| (now + delta).to_rfc3339();
    let local = |delta: chrono::Duration| {
        (now + delta)
            .with_timezone(&Local)
            .format("%m/%d %H:%M")
            .to_string()
    };
    let weekly_reset = chrono::Duration::days(2) + chrono::Duration::hours(5);
    let manual_expiry = chrono::Duration::days(1) + chrono::Duration::hours(3);
    let claude = account(serde_json::json!({
        "label": "home",
        "five_hour": {"kind":"five_hour","used_percent":62.0,
                      "resets_at":at(chrono::Duration::minutes(40)),"resets_in_seconds":null},
        "weekly": {"kind":"weekly","used_percent":85.0,
                   "resets_at":at(weekly_reset),"resets_in_seconds":null},
        "manual_resets": [
            {"kind":"full","remaining":1,"expires_at":at(manual_expiry)},
            {"kind":"five_hour","remaining":0,"expires_at":null}
        ]
    }));
    let (statusline, tui) = render_both(&claude, now);
    let weekly_date = local(weekly_reset);
    let manual_date = local(manual_expiry);
    for (text, nth) in [
        ("Claude", 0),
        ("home", 0),
        ("5h", 0),
        ("1w", 0),
        // 5h と 1w のバーの点灯部・消灯部。
        ("█", 0),
        ("░", 0),
        ("█", 1),
        ("░", 1),
        (" 62%", 0),
        (" 85%", 0),
        (weekly_date.as_str(), 0),
        ("full 1", 0),
        (manual_date.as_str(), 0),
    ] {
        assert_eq!(
            color_of(&statusline, text, nth),
            color_of(&tui, text, nth),
            "{text:?} ({nth})"
        );
    }
    // 残り時間は表記が違う (`40m` / `40m`、`2d05h` / `2d 5h`) ので、それぞれの表記で比べる。
    assert_eq!(color_of(&statusline, "40m", 0), color_of(&tui, "40m", 0));
    assert_eq!(
        color_of(&statusline, "2d05h", 0),
        color_of(&tui, "2d 5h", 0)
    );

    // 比べた色が、どちらも色なしで一致しているだけではないことを確かめる。
    assert_eq!(color_of(&tui, &manual_date, 0).as_deref(), Some("38;5;196"));
    assert_eq!(color_of(&tui, " 85%", 0).as_deref(), Some("1;38;5;208"));

    // Codex の白い印、使用率が不明な枠、24 時間枠の制限。
    let grok = account(serde_json::json!({
        "provider": "grok",
        "weekly": {"kind":"monthly","used_percent":null,
                   "resets_at":at(chrono::Duration::days(20)),"resets_in_seconds":null},
        "limit_observation": {"model":"grok-test","observed_at":at(-chrono::Duration::minutes(5)),
                              "used_tokens":120,"limit_tokens":100}
    }));
    let (statusline, tui) = render_both(&grok, now);
    for text in ["Grok", " --%", "LIMIT"] {
        assert_eq!(
            color_of(&statusline, text, 0),
            color_of(&tui, text, 0),
            "{text:?}"
        );
    }
    let codex = account(serde_json::json!({"provider": "codex", "label": "work"}));
    let (statusline, tui) = render_both(&codex, now);
    assert_eq!(
        color_of(&statusline, "Codex", 0),
        color_of(&tui, "Codex", 0)
    );
    assert_eq!(color_of(&statusline, "work", 0), color_of(&tui, "work", 0));
}
