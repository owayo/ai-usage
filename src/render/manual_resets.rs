//! 手動リセットの残回数と有効期限を、テーブル・TUI・statusline で同じ意味で表示する。
//!
//! 表示するのは、通常の週間枠そのものをリセットする付与 (`ResetKind::restores_weekly_window`)
//! だけにする。完全リセットと週間枠だけのリセットは、種類名を付けずに 1 つの回数にまとめる。
//! Claude は同じ付与でも、回復する枠 (`clears`) に今使っている枠しか挙げない。5 時間枠を
//! 使っていないアカウントでは完全リセットが週間枠だけのリセットに見えるので、分けると
//! アカウントによって表記が変わる。Fable のリセットなど、ほかの種類は `--json` にだけ残す。
//!
//! 残りが 0 回なら手動リセットの表示ごと省く。取得できなかった回数 (`?`) は 0 回と区別して出す。

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Local, Utc};

use super::parse_utc;
use super::reset::format_reset_at;
use crate::report::ManualResetOut;

/// 表示する付与を、利用できるものと一時停止中のものに分けて数えた結果。
struct ResetGroup {
    paused: bool,
    total: Option<u64>,
    deadlines: BTreeMap<Option<DateTime<Utc>>, u64>,
}

impl ResetGroup {
    /// 残回数 (`2`、`?`、`paused 1`)。週間枠をリセットする付与だけを数えるので、種類名は付けない。
    fn count_text(&self) -> String {
        let count = self
            .total
            .map_or_else(|| "?".into(), |count| count.to_string());
        if self.paused {
            format!("paused {count}")
        } else {
            count
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

/// テーブルの手動リセットの列。表示する付与が無ければ None を返す。
pub(super) fn format_resets(
    resets: &[ManualResetOut],
    now: DateTime<Utc>,
    multiline: bool,
) -> Option<String> {
    let groups: Vec<_> = summarize(resets, now)
        .into_iter()
        .map(|group| {
            let mut text = group.count_text();
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
    /// 残回数 (`1`、`?`、`paused 1`)。
    Summary(String),
    /// 付与分ごとの有効期限。`prefix` は `  1@` の部分。
    Deadline {
        prefix: String,
        deadline: ResetDeadline,
    },
}

/// TUI では期限を付与分ごとの行に分け、狭い端末でも切れないようにする。
/// 表示する付与が無ければ空を返す。
pub(super) fn format_resets_tui(
    resets: &[ManualResetOut],
    now: DateTime<Utc>,
) -> Vec<TuiResetLine> {
    let mut lines = Vec::new();
    for group in summarize(resets, now) {
        lines.push(TuiResetLine::Summary(group.count_text()));
        if group.total.is_some_and(|count| count > 0) {
            for (expiry, count) in group.deadlines {
                lines.push(TuiResetLine::Deadline {
                    prefix: format!("  {count}@"),
                    deadline: ResetDeadline {
                        expires_at: expiry,
                        text: format_tui_deadline(expiry, now),
                    },
                });
            }
        }
    }
    lines
}

/// statusline は残回数と、行全体で最も近い有効期限だけを表示する。
/// 表示する付与が無ければ None を返し、`↻` ごと省く。
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
        .map(ResetGroup::count_text)
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

fn format_tui_deadline(expiry: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    expiry
        .map(|expiry| {
            let local = expiry.with_timezone(&Local);
            let show_year = local.year() != now.with_timezone(&Local).year();
            format_reset_at(local, show_year)
        })
        .unwrap_or_else(|| "?".into())
}

/// 週間枠をリセットする付与だけを、利用できるもの・一時停止中のものの順に数える。
/// 表示に含める種類は `ResetKind::restores_weekly_window` の 1 か所で決める。
fn summarize(resets: &[ManualResetOut], now: DateTime<Utc>) -> Vec<ResetGroup> {
    let mut groups = Vec::new();
    for paused in [false, true] {
        let mut total = Some(0_u64);
        let mut deadlines = BTreeMap::<Option<DateTime<Utc>>, u64>::new();
        for reset in resets
            .iter()
            .filter(|r| r.kind.restores_weekly_window() && r.paused == paused)
        {
            let expiry = reset.expires_at.as_deref().and_then(parse_utc);
            // JSON cache を読むたびに判定し、キャッシュ更新前でも失効分を数えない。
            if expiry.is_some_and(|end| end <= now) {
                continue;
            }
            // 完全リセットと週間枠だけのリセットの回数を足す。どれか 1 つでも回数が不明なら、
            // 既知の分だけを合計にせず、合計も不明にする。
            total = total
                .zip(reset.remaining)
                .and_then(|(sum, count)| sum.checked_add(count));
            if let Some(count) = reset.remaining.filter(|count| *count > 0) {
                let slot = deadlines.entry(expiry).or_default();
                *slot = slot.saturating_add(count);
            }
        }
        // 残りが 0 回なら出さない (付与が無い・失効しただけの場合も含む)。不明は 0 回と区別して残す。
        if total == Some(0) {
            continue;
        }
        groups.push(ResetGroup {
            paused,
            total,
            deadlines,
        });
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ResetKind;
    use chrono::TimeZone;

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

    /// TUI の手動リセットの行を、期限の日時も文字にして並べる。
    fn tui_text(resets: &[ManualResetOut], now: DateTime<Utc>) -> Vec<String> {
        format_resets_tui(resets, now)
            .iter()
            .map(|line| match line {
                TuiResetLine::Summary(text) => text.clone(),
                TuiResetLine::Deadline { prefix, deadline } => {
                    format!("{prefix}{}", deadline.text)
                }
            })
            .collect()
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
                reset(ResetKind::Weekly, Some(0), None),
                reset(ResetKind::Weekly, Some(2), Some("2026-06-15T00:00:00Z")),
                held,
            ],
        ] {
            assert_eq!(format_resets(&resets, now, false), None);
            assert_eq!(compact(&resets, now), None);
            assert!(format_resets_tui(&resets, now).is_empty());
        }
        // 取得側が補う確定 0 回の full は、ほかの付与の回数を減らさない。
        let mixed = [
            reset(ResetKind::Full, Some(0), None),
            reset(ResetKind::Weekly, Some(1), None),
        ];
        assert_eq!(compact(&mixed, now).as_deref(), Some("1 (?)"));
        // 回数が不明なら、0 回と区別して出す。
        let unknown = [
            reset(ResetKind::Full, Some(0), None),
            reset(ResetKind::Weekly, None, None),
        ];
        assert_eq!(compact(&unknown, now).as_deref(), Some("?"));
    }

    #[test]
    fn only_resets_that_restore_the_weekly_window_are_shown() {
        let now = parse_utc("2026-06-15T00:00:00Z").unwrap();
        let end = "2026-06-20T00:00:00Z";
        // Fable・5 時間枠だけ・その他のリセットは、回数が残っていても不明でも表示しない。
        // 後の版が足した種類も other として読むので、同じく表示しない。
        for kind in [ResetKind::Fable, ResetKind::FiveHour, ResetKind::Other] {
            for remaining in [Some(3), None] {
                let mut held = reset(kind, remaining, Some(end));
                held.paused = true;
                let resets = [reset(kind, remaining, Some(end)), held];
                assert_eq!(format_resets(&resets, now, false), None, "{kind:?}");
                assert_eq!(compact(&resets, now), None, "{kind:?}");
                assert!(format_resets_tui(&resets, now).is_empty(), "{kind:?}");
            }
        }
        // 表示しない付与の期限は、statusline の直近の期限にも使わない。
        let later = parse_utc("2026-06-25T00:00:00Z").unwrap();
        let resets = [
            reset(ResetKind::Fable, Some(1), Some("2026-06-16T00:00:00Z")),
            reset(ResetKind::FiveHour, Some(1), Some("2026-06-17T00:00:00Z")),
            reset(ResetKind::Full, Some(1), Some("2026-06-25T00:00:00Z")),
        ];
        assert_eq!(
            compact(&resets, now),
            Some(format!("1 ({})", format_deadline(Some(later), now)))
        );
    }

    #[test]
    fn full_and_weekly_resets_read_the_same_without_kind_names() {
        // 2026-11-11 の実応答。同じ「One full limit reset」の付与が、5 時間枠を使っている
        // アカウントでは full、使っていないアカウントでは weekly になり、Fable の付与も並ぶ。
        let now = parse_utc("2026-11-11T02:00:00Z").unwrap();
        let end = "2026-12-10T22:00:00+00:00";
        let fable_end = "2027-03-09T07:59:00+00:00";
        let idle_five_hour = [
            reset(ResetKind::Weekly, Some(1), Some(end)),
            reset(ResetKind::Fable, Some(1), Some(fable_end)),
            reset(ResetKind::Full, Some(0), None),
        ];
        let active_five_hour = [
            reset(ResetKind::Full, Some(1), Some(end)),
            reset(ResetKind::Fable, Some(1), Some(fable_end)),
            reset(ResetKind::Fable, Some(1), Some(fable_end)),
            reset(ResetKind::Fable, Some(1), Some(fable_end)),
        ];
        let expiry = Some(parse_utc(end).unwrap());
        let date = format_deadline(expiry, now);
        let tui_date = format_tui_deadline(expiry, now);
        for resets in [&idle_five_hour[..], &active_five_hour[..]] {
            assert_eq!(compact(resets, now), Some(format!("1 ({date})")));
            assert_eq!(
                format_resets(resets, now, false),
                Some(format!("1 (1@{date})"))
            );
            assert_eq!(
                tui_text(resets, now),
                ["1".to_string(), format!("  1@{tui_date}")]
            );
        }
        // 期限が同じ完全リセットと週間枠だけのリセットは、1 つの期限にまとめて数える。
        let merged = [
            reset(ResetKind::Full, Some(1), Some(end)),
            reset(ResetKind::Weekly, Some(2), Some(end)),
        ];
        assert_eq!(
            format_resets(&merged, now, false),
            Some(format!("3 (3@{date})"))
        );
        // 一時停止中の付与は合算せず、利用できる付与の後に置く。
        let mut held = reset(ResetKind::Weekly, Some(2), None);
        held.paused = true;
        let mixed = [
            held,
            reset(ResetKind::Other, Some(1), None),
            reset(ResetKind::Fable, Some(2), None),
            reset(ResetKind::Full, Some(1), None),
            reset(ResetKind::FiveHour, Some(1), None),
        ];
        assert_eq!(compact(&mixed, now).as_deref(), Some("1; paused 2 (?)"));
        assert_eq!(
            tui_text(&mixed, now),
            ["1", "  1@?", "paused 2", "  2@?"].map(String::from)
        );
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
            reset(ResetKind::Weekly, Some(0), None),
        ];
        let text = format_resets(&resets, now, false);
        let local = |date| {
            parse_utc(date)
                .unwrap()
                .with_timezone(&Local)
                .format("%m/%d %H:%M")
                .to_string()
        };
        // 失効した 9 回と、残りが 0 回の付与は数えない。
        assert_eq!(
            text,
            Some(format!("3 (1@{}, 2@{})", local(expiry), local(later)))
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
            reset(ResetKind::Weekly, Some(2), None),
            reset(ResetKind::FiveHour, Some(0), None),
        ];
        let lines = format_resets_tui(&resets, now);
        let dated = format!("  1@{}", format_tui_deadline(Some(soon), now));
        assert_eq!(tui_text(&resets, now), ["3", "  2@?", dated.as_str()]);
        // TUI が statusline と同じ規則で日時を塗れるよう、期限は日時のまま渡す (不明は None)。
        let expiries: Vec<_> = lines
            .iter()
            .filter_map(|line| match line {
                TuiResetLine::Deadline { deadline, .. } => Some(deadline.expires_at),
                TuiResetLine::Summary(_) => None,
            })
            .collect();
        assert_eq!(expiries, [None, Some(soon)]);
        for (year, expected) in [(2026, "12/01 火 04:49"), (2027, "2027/12/01 水 04:49")] {
            let expiry = Local
                .with_ymd_and_hms(year, 12, 1, 4, 49, 0)
                .single()
                .unwrap()
                .with_timezone(&Utc);
            assert_eq!(format_tui_deadline(Some(expiry), now), expected);
        }
        assert_eq!(format_tui_deadline(None, now), "?");
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
        // 回数が不明な付与が 1 つでもあれば、既知の 2 回だけを合計にせず不明にする。
        // テーブルと TUI は合計が不明なら期限の内訳を出さず、statusline は既知の期限を出す。
        let expiry = parse_utc("2026-06-20T00:00:00Z").unwrap();
        let resets = [
            reset(ResetKind::Full, Some(2), Some("2026-06-20T00:00:00Z")),
            reset(ResetKind::Weekly, None, None),
        ];
        assert_eq!(format_resets(&resets, now, false).as_deref(), Some("?"));
        assert_eq!(tui_text(&resets, now), ["?"]);
        assert_eq!(
            compact(&resets, now),
            Some(format!("? ({})", format_deadline(Some(expiry), now)))
        );
        // 読めない期限は失効扱いにせず、回数を残して期限を `?` にする。
        assert_eq!(
            format_resets(
                &[reset(ResetKind::Weekly, Some(2), Some("invalid"))],
                now,
                false
            )
            .as_deref(),
            Some("2 (2@?)")
        );
        // 合計があふれたら、誤った回数を出さずに不明にする。
        let huge = [
            reset(ResetKind::Full, Some(u64::MAX), None),
            reset(ResetKind::Weekly, Some(1), None),
        ];
        assert_eq!(compact(&huge, now).as_deref(), Some("? (?)"));
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
            format_resets(std::slice::from_ref(&held), now, false),
            Some(format!("paused 1 (1@{date})"))
        );
        // 利用できる付与と並べるときは、合算せずに後ろへ置く。
        let soon = "2026-06-18T04:00:00Z";
        let soon_date = parse_utc(soon)
            .unwrap()
            .with_timezone(&Local)
            .format("%m/%d %H:%M");
        assert_eq!(
            format_resets(
                &[held, reset(ResetKind::Weekly, Some(1), Some(soon))],
                now,
                true
            ),
            Some(format!("1 (1@{soon_date})\npaused 1 (1@{date})"))
        );
    }

    #[test]
    fn compact_sums_full_and_weekly_and_keeps_only_the_earliest_expiry() {
        let now = parse_utc("2026-06-15T00:00:00Z").unwrap();
        let earliest = parse_utc("2026-06-16T04:00:00Z").unwrap();
        let resets = [
            reset(ResetKind::Full, Some(2), Some("2026-06-20T04:00:00Z")),
            reset(ResetKind::Weekly, Some(1), Some("2026-06-16T04:00:00Z")),
            reset(ResetKind::Full, Some(1), Some("2026-06-18T04:00:00Z")),
            reset(ResetKind::Full, Some(9), Some("2026-06-15T00:00:00Z")),
        ];
        assert_eq!(
            compact(&resets, now),
            Some(format!("4 ({})", format_deadline(Some(earliest), now)))
        );
        // 最も近い期限の付与が失効したら、残った付与だけを数えて期限を選び直す。
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
        // 期限が分からない付与があっても、分かっている期限を出す。
        let resets = [
            reset(ResetKind::Full, Some(1), None),
            reset(ResetKind::Weekly, Some(1), Some("2026-06-20T04:00:00Z")),
        ];
        assert_eq!(
            compact(&resets, now),
            Some(format!("2 ({})", format_deadline(Some(expiry), now)))
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
        let available = reset(ResetKind::Weekly, Some(2), Some("2026-06-20T04:00:00Z"));
        // 一時停止中の付与の期限が近くても、利用できる付与の期限を出す。
        assert_eq!(
            compact(&[held.clone(), available], now),
            Some(format!(
                "2; paused 1 ({})",
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
            compact(&[held, reset(ResetKind::Full, Some(1), None)], now).as_deref(),
            Some("1; paused 1 (?)")
        );
    }
}
