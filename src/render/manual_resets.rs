//! 手動リセットの残回数と有効期限を両レンダラーで同じ意味で表示する。

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
    fn label_count(&self) -> String {
        let label = format!(
            "{}{}",
            self.kind.label(),
            if self.paused { " paused" } else { "" }
        );
        match self.total {
            Some(count) => format!("{label} {count}"),
            None => format!("{label} ?"),
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

pub(super) fn format_resets(
    resets: &[ManualResetOut],
    now: DateTime<Utc>,
    multiline: bool,
) -> String {
    let groups: Vec<_> = summarize(resets, now)
        .into_iter()
        .map(|group| {
            let mut text = group.label_count();
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
    if groups.is_empty() {
        "0".into()
    } else {
        groups.join(if multiline { "\n" } else { "; " })
    }
}

/// statusline は種類ごとの残回数と、行全体で最も近い有効期限だけを表示する。
pub(super) fn format_resets_compact(
    resets: &[ManualResetOut],
    now: DateTime<Utc>,
) -> CompactResets {
    let groups = summarize(resets, now);
    if groups.is_empty() {
        return CompactResets {
            counts: "0".into(),
            deadline: None,
        };
    }
    let counts = groups
        .iter()
        .map(ResetGroup::label_count)
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
    CompactResets { counts, deadline }
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
        let entries: Vec<_> = resets.iter().filter(|r| r.kind == kind).collect();
        if entries.is_empty() {
            continue;
        }
        for paused in [false, true] {
            let entries: Vec<_> = entries.iter().filter(|r| r.paused == paused).collect();
            if paused && entries.is_empty() {
                continue;
            }
            let mut total = Some(0_u64);
            let mut deadlines = BTreeMap::<Option<DateTime<Utc>>, u64>::new();
            for reset in entries {
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
            if paused && total == Some(0) {
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
        assert_eq!(
            text,
            format!("full 3 (1@{}, 2@{}); 5h 0", local(expiry), local(later))
        );
        let after = parse_utc("2026-06-21T00:00:00Z").unwrap();
        assert_eq!(format_resets(&resets, after, true), "full 0\n5h 0");
    }

    #[test]
    fn unknown_count_and_unknown_expiry_do_not_mean_zero() {
        let now = parse_utc("2026-06-15T00:00:00Z").unwrap();
        let resets = [
            reset(ResetKind::Full, None, None),
            reset(ResetKind::FiveHour, Some(2), Some("invalid")),
        ];
        assert_eq!(format_resets(&resets, now, false), "full ?; 5h 2 (2@?)");
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
        assert_eq!(
            format_resets(&[held], now, false),
            format!("full 0; full paused 1 (1@{date})")
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
            format_resets_compact(&resets, now).to_string(),
            format!("full 3; 5h 1 ({})", format_deadline(Some(earliest), now))
        );
        let after = parse_utc("2026-06-17T00:00:00Z").unwrap();
        let next = parse_utc("2026-06-18T04:00:00Z").unwrap();
        assert_eq!(
            format_resets_compact(&resets, after).to_string(),
            format!("full 3; 5h 0 ({})", format_deadline(Some(next), after))
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
            format_resets_compact(&resets, now).to_string(),
            format!("full 2; 5h ? ({})", format_deadline(Some(expiry), now))
        );
        assert_eq!(
            format_resets_compact(&[reset(ResetKind::Full, Some(2), Some("invalid"))], now)
                .to_string(),
            "full 2 (?)"
        );
        assert_eq!(
            format_resets_compact(&[reset(ResetKind::Full, Some(0), None)], now).to_string(),
            "full 0"
        );
        assert_eq!(
            format_resets_compact(&[reset(ResetKind::Full, None, None)], now).to_string(),
            "full ?"
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
            format_resets_compact(&[held.clone(), available], now).to_string(),
            format!(
                "full 0; full paused 1; 5h 2 ({})",
                format_deadline(Some(expiry), now)
            )
        );
        assert_eq!(
            format_resets_compact(&[held.clone()], now).to_string(),
            format!(
                "full 0; full paused 1 ({})",
                format_deadline(Some(held_expiry), now)
            )
        );
        assert_eq!(
            format_resets_compact(&[held, reset(ResetKind::FiveHour, Some(1), None)], now)
                .to_string(),
            "full 0; full paused 1; 5h 1 (?)"
        );
    }
}
