//! 手動リセットの残回数と有効期限を両レンダラーで同じ意味で表示する。
//! 残りが 0 回の種類は出さず、何も残っていない行は手動リセットの表示ごと省く。
//! 取得できなかった回数 (`?`) は、0 回と区別して出す。

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Local, Utc};

use super::parse_utc;
use crate::model::ResetKind;
use crate::report::ManualResetOut;

struct ResetGroup {
    kind: ResetKind,
    paused: bool,
    total: Option<u64>,
    deadlines: BTreeMap<Option<DateTime<Utc>>, u64>,
}

impl ResetGroup {
    fn label_count(&self, omit_full: bool) -> String {
        let count = self
            .total
            .map_or_else(|| "?".into(), |count| count.to_string());
        let paused = if self.paused { "paused " } else { "" };
        if omit_full && self.kind == ResetKind::Full {
            format!("{paused}{count}")
        } else {
            format!("{} {paused}{count}", self.kind.label())
        }
    }
}

pub(super) struct CompactResets {
    pub counts: String,
    pub deadline: Option<ResetDeadline>,
}

pub(super) struct ResetDeadline {
    pub expires_at: Option<DateTime<Utc>>,
    pub text: String,
}

impl std::fmt::Display for CompactResets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.counts)?;
        if let Some(deadline) = &self.deadline {
            write!(f, " ({})", deadline.text)?;
        }
        Ok(())
    }
}

/// テーブルの手動リセットの列。表示する種類が無ければ None を返す。
pub(super) fn format_resets(
    resets: &[ManualResetOut],
    now: DateTime<Utc>,
    multiline: bool,
) -> Option<String> {
    let groups: Vec<_> = summarize(resets, now)
        .into_iter()
        .map(|group| {
            let mut text = group.label_count(false);
            if group.total.is_some_and(|count| count > 0) {
                let deadlines: Vec<_> = group
                    .deadlines
                    .into_iter()
                    .map(|(expiry, count)| format!("{count}@{}", format_deadline(expiry, now)))
                    .collect();
                text += &format!(" ({})", deadlines.join(", "));
            }
            text
        })
        .collect();
    (!groups.is_empty()).then(|| groups.join(if multiline { "\n" } else { "; " }))
}

/// TUI の手動リセットの 1 行。期限の日時は statusline と同じ規則で色を付けるため、
/// 前に置く文字とは分けて持つ。
pub(super) enum TuiResetLine {
    /// 種類ごとの残回数 (`full 1`、`5h ?`、`full paused 1`)。
    Summary(String),
    /// 付与分ごとの有効期限。`prefix` は `  1@` の部分。
    Deadline {
        prefix: String,
        deadline: ResetDeadline,
    },
}

/// TUI では期限を付与分ごとの行に分け、狭い端末でも切れないようにする。
/// 表示する種類が無ければ空を返す。
pub(super) fn format_resets_tui(
    resets: &[ManualResetOut],
    now: DateTime<Utc>,
) -> Vec<TuiResetLine> {
    let mut lines = Vec::new();
    for group in summarize(resets, now) {
        lines.push(TuiResetLine::Summary(group.label_count(false)));
        if group.total.is_some_and(|count| count > 0) {
            for (expiry, count) in group.deadlines {
                lines.push(TuiResetLine::Deadline {
                    prefix: format!("  {count}@"),
                    deadline: ResetDeadline {
                        expires_at: expiry,
                        text: format_deadline(expiry, now),
                    },
                });
            }
        }
    }
    lines
}

/// statusline は種類ごとの残回数と、行全体で最も近い有効期限だけを表示する。
/// 完全リセットは回数だけにし、それ以外は種類を残す。
/// 表示する種類が無ければ None を返し、`↻` ごと省く。
pub(super) fn format_resets_compact(
    resets: &[ManualResetOut],
    now: DateTime<Utc>,
) -> Option<CompactResets> {
    let groups = summarize(resets, now);
    if groups.is_empty() {
        return None;
    }
    let counts = groups
        .iter()
        .map(|group| group.label_count(true))
        .collect::<Vec<_>>()
        .join("; ");
    // 利用可能な付与分を優先し、それがなければ一時停止中の期限を残す。
    let use_paused = !groups
        .iter()
        .any(|group| !group.paused && !group.deadlines.is_empty());
    let deadlines: Vec<_> = groups
        .iter()
        .filter(|group| group.paused == use_paused)
        .flat_map(|group| group.deadlines.keys().copied())
        .collect();
    let deadline = if deadlines.is_empty() {
        None
    } else {
        // Option の順序では None が先頭になるため、既知の日時だけで最小値を取る。
        let expiry = deadlines.into_iter().flatten().min();
        Some(ResetDeadline {
            expires_at: expiry,
            text: format_deadline(expiry, now),
        })
    };
    Some(CompactResets { counts, deadline })
}

fn format_deadline(expiry: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    expiry
        .map(|expiry| {
            let local = expiry.with_timezone(&Local);
            let pattern = if local.year() == now.with_timezone(&Local).year() {
                "%m/%d %H:%M"
            } else {
                "%Y/%m/%d %H:%M"
            };
            local.format(pattern).to_string()
        })
        .unwrap_or_else(|| "?".into())
}

fn summarize(resets: &[ManualResetOut], now: DateTime<Utc>) -> Vec<ResetGroup> {
    let mut groups = Vec::new();
    for kind in [
        ResetKind::Full,
        ResetKind::FiveHour,
        ResetKind::Weekly,
        ResetKind::Other,
    ] {
        for paused in [false, true] {
            let mut total = Some(0_u64);
            let mut deadlines = BTreeMap::<Option<DateTime<Utc>>, u64>::new();
            for reset in resets
                .iter()
                .filter(|r| r.kind == kind && r.paused == paused)
            {
                let expiry = reset.expires_at.as_deref().and_then(parse_utc);
                // JSON cache を読むたびに判定し、キャッシュ更新前でも失効分を数えない。
                if expiry.is_some_and(|end| end <= now) {
                    continue;
                }
                total = total
                    .zip(reset.remaining)
                    .and_then(|(sum, count)| sum.checked_add(count));
                if let Some(count) = reset.remaining.filter(|count| *count > 0) {
                    let slot = deadlines.entry(expiry).or_default();
                    *slot = slot.saturating_add(count);
                }
            }
            // 残りが 0 回の種類は出さない (失効しただけの付与も含む)。不明は 0 回と区別して残す。
            if total == Some(0) {
                continue;
            }
            groups.push(ResetGroup {
                kind,
                paused,
                total,
                deadlines,
            });
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset(kind: ResetKind, remaining: Option<u64>, expires_at: Option<&str>) -> ManualResetOut {
        ManualResetOut {
            kind,
            remaining,
            expires_at: expires_at.map(str::to_string),
            paused: false,
        }
    }

    /// statusline に出す文字列。手動リセットの表示を省く行は None。
    fn compact(resets: &[ManualResetOut], now: DateTime<Utc>) -> Option<String> {
        format_resets_compact(resets, now).map(|resets| resets.to_string())
    }

    #[test]
    fn nothing_is_shown_when_no_reset_remains() {
        let now = parse_utc("2026-06-15T00:00:00Z").unwrap();
        let mut held = reset(ResetKind::Full, Some(0), None);
        held.paused = true;
        // 0 回・描画の時点で失効した付与 (期限ちょうどを含む)・一時停止中の 0 回だけなら、
        // どのレンダラーにも何も渡さない。
        for resets in [
            vec![],
            vec![reset(ResetKind::Full, Some(0), None)],
            vec![
                reset(ResetKind::Full, Some(0), None),
                reset(ResetKind::FiveHour, Some(0), None),
                reset(ResetKind::Other, Some(2), Some("2026-06-15T00:00:00Z")),
                held,
            ],
        ] {
            assert_eq!(format_resets(&resets, now, false), None);
            assert_eq!(compact(&resets, now), None);
            assert!(format_resets_tui(&resets, now).is_empty());
        }
        // 0 回の種類だけを省き、残っている種類と回数が不明な種類は出す。
        let mixed = [
            reset(ResetKind::Full, Some(0), None),
            reset(ResetKind::Weekly, Some(1), None),
            reset(ResetKind::Other, None, None),
        ];
        assert_eq!(compact(&mixed, now).as_deref(), Some("1w 1; other ? (?)"));
    }

    #[test]
    fn cache_expiry_removes_only_expired_grants_and_keeps_each_deadline() {
        let now = parse_utc("2026-06-15T00:00:00Z").unwrap();
        let expiry = "2026-06-16T04:00:00Z";
        let later = "2026-06-20T05:00:00Z";
        let resets = [
            reset(ResetKind::Full, Some(9), Some("2026-06-15T00:00:00Z")),
            reset(ResetKind::Full, Some(1), Some(expiry)),
            reset(ResetKind::Full, Some(2), Some(later)),
            reset(ResetKind::FiveHour, Some(0), None),
        ];
        let text = format_resets(&resets, now, false);
        let local = |date| {
            parse_utc(date)
                .unwrap()
                .with_timezone(&Local)
                .format("%m/%d %H:%M")
                .to_string()
        };
        // 残りが 0 回の 5h は出さない。
        assert_eq!(
            text,
            Some(format!("full 3 (1@{}, 2@{})", local(expiry), local(later)))
        );
        // すべて失効したら、手動リセットの表示ごと省く。
        let after = parse_utc("2026-06-21T00:00:00Z").unwrap();
        assert_eq!(format_resets(&resets, after, true), None);
    }

    #[test]
    fn tui_lines_keep_each_expiry_for_coloring() {
        let now = parse_utc("2026-06-15T00:00:00Z").unwrap();
        let soon = parse_utc("2026-06-16T04:00:00Z").unwrap();
        let resets = [
            reset(ResetKind::Full, Some(1), Some("2026-06-16T04:00:00Z")),
            reset(ResetKind::Full, Some(2), None),
            reset(ResetKind::FiveHour, Some(0), None),
        ];
        let lines = format_resets_tui(&resets, now);
        let text: Vec<String> = lines
            .iter()
            .map(|line| match line {
                TuiResetLine::Summary(text) => text.clone(),
                TuiResetLine::Deadline { prefix, deadline } => {
                    format!("{prefix}{}", deadline.text)
                }
            })
            .collect();
        let dated = format!("  1@{}", format_deadline(Some(soon), now));
        assert_eq!(text, ["full 3", "  2@?", dated.as_str()]);
        // TUI が statusline と同じ規則で日時を塗れるよう、期限は日時のまま渡す (不明は None)。
        let expiries: Vec<_> = lines
            .iter()
            .filter_map(|line| match line {
                TuiResetLine::Deadline { deadline, .. } => Some(deadline.expires_at),
                TuiResetLine::Summary(_) => None,
            })
            .collect();
        assert_eq!(expiries, [None, Some(soon)]);
    }

    #[test]
    fn deadline_shows_the_year_only_outside_the_current_year() {
        let now = parse_utc("2026-06-15T00:00:00Z").unwrap();
        let same_year = parse_utc("2026-08-01T12:00:00Z").unwrap();
        let next_year = parse_utc("2027-08-01T12:00:00Z").unwrap();
        let local =
            |time: DateTime<Utc>, pattern| time.with_timezone(&Local).format(pattern).to_string();
        assert_eq!(
            format_deadline(Some(same_year), now),
            local(same_year, "%m/%d %H:%M")
        );
        assert_eq!(
            format_deadline(Some(next_year), now),
            local(next_year, "%Y/%m/%d %H:%M")
        );
        assert_eq!(format_deadline(None, now), "?");
    }

    #[test]
    fn unknown_count_and_unknown_expiry_do_not_mean_zero() {
        let now = parse_utc("2026-06-15T00:00:00Z").unwrap();
        let resets = [
            reset(ResetKind::Full, None, None),
            reset(ResetKind::FiveHour, Some(2), Some("invalid")),
        ];
        assert_eq!(
            format_resets(&resets, now, false).as_deref(),
            Some("full ?; 5h 2 (2@?)")
        );
    }

    #[test]
    fn paused_grants_keep_count_and_expiry_separate_from_available_resets() {
        let now = parse_utc("2026-06-15T00:00:00Z").unwrap();
        let expiry = "2026-06-20T04:00:00Z";
        let mut held = reset(ResetKind::Full, Some(1), Some(expiry));
        held.paused = true;
        let date = parse_utc(expiry)
            .unwrap()
            .with_timezone(&Local)
            .format("%m/%d %H:%M");
        // 利用できる付与が無くても、一時停止中の付与は別に数えて出す。
        assert_eq!(
            format_resets(&[held], now, false),
            Some(format!("full paused 1 (1@{date})"))
        );
    }

    #[test]
    fn compact_keeps_all_counts_and_only_the_earliest_expiry_across_types() {
        let now = parse_utc("2026-06-15T00:00:00Z").unwrap();
        let earliest = parse_utc("2026-06-16T04:00:00Z").unwrap();
        let resets = [
            reset(ResetKind::Full, Some(2), Some("2026-06-20T04:00:00Z")),
            reset(ResetKind::FiveHour, Some(1), Some("2026-06-16T04:00:00Z")),
            reset(ResetKind::Full, Some(1), Some("2026-06-18T04:00:00Z")),
            reset(ResetKind::Full, Some(9), Some("2026-06-15T00:00:00Z")),
        ];
        assert_eq!(
            compact(&resets, now),
            Some(format!(
                "3; 5h 1 ({})",
                format_deadline(Some(earliest), now)
            ))
        );
        // 5h の付与が失効したら 5h ごと消え、期限は残った付与から選ぶ。
        let after = parse_utc("2026-06-17T00:00:00Z").unwrap();
        let next = parse_utc("2026-06-18T04:00:00Z").unwrap();
        assert_eq!(
            compact(&resets, after),
            Some(format!("3 ({})", format_deadline(Some(next), after)))
        );
    }

    #[test]
    fn compact_distinguishes_known_unknown_and_absent_expiries() {
        let now = parse_utc("2026-06-15T00:00:00Z").unwrap();
        let expiry = parse_utc("2026-06-20T04:00:00Z").unwrap();
        let resets = [
            reset(ResetKind::Full, Some(1), None),
            reset(ResetKind::Full, Some(1), Some("2026-06-20T04:00:00Z")),
            reset(ResetKind::FiveHour, None, None),
        ];
        assert_eq!(
            compact(&resets, now),
            Some(format!("2; 5h ? ({})", format_deadline(Some(expiry), now)))
        );
        assert_eq!(
            compact(&[reset(ResetKind::Full, Some(2), Some("invalid"))], now).as_deref(),
            Some("2 (?)")
        );
        assert_eq!(
            compact(&[reset(ResetKind::Full, None, None)], now).as_deref(),
            Some("?")
        );
    }

    #[test]
    fn compact_uses_paused_expiry_only_without_available_grants() {
        let now = parse_utc("2026-06-15T00:00:00Z").unwrap();
        let expiry = parse_utc("2026-06-20T04:00:00Z").unwrap();
        let held_expiry = parse_utc("2026-06-16T04:00:00Z").unwrap();
        let mut held = reset(ResetKind::Full, Some(1), Some("2026-06-16T04:00:00Z"));
        held.paused = true;
        let available = reset(ResetKind::FiveHour, Some(2), Some("2026-06-20T04:00:00Z"));
        assert_eq!(
            compact(&[held.clone(), available], now),
            Some(format!(
                "paused 1; 5h 2 ({})",
                format_deadline(Some(expiry), now)
            ))
        );
        assert_eq!(
            compact(&[held.clone()], now),
            Some(format!(
                "paused 1 ({})",
                format_deadline(Some(held_expiry), now)
            ))
        );
        assert_eq!(
            compact(&[held, reset(ResetKind::FiveHour, Some(1), None)], now).as_deref(),
            Some("paused 1; 5h 1 (?)")
        );
    }
}
