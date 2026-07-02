//! `--sort` の行ソート。`AccountOut`(statusline/json)と `AccountReport`(table)の
//! 双方を `SortableRow` で抽象化し、比較ロジックを 1 箇所に集約する。

use std::cmp::Ordering;

use chrono::{DateTime, Utc};

use super::parse_utc;
use crate::SortKey;
use crate::model::AccountReport;
use crate::report::AccountOut;

/// `--sort` の比較ロジックを 1 箇所に集約する。`SortKey::Provider` の場合は
/// 「データ無し行を末尾に」というルールは適用せず、各レンダラーが自身の
/// デフォルト順序(table/json はジョブ順、statusline は provider.rank())を
/// 維持できるようにする。
fn cmp_optional_asc<T: PartialOrd>(a: Option<T>, b: Option<T>) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
        // Some(値あり) を上に、None(欠損)を末尾に固定。
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn cmp_optional_desc<T: PartialOrd>(a: Option<T>, b: Option<T>) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => y.partial_cmp(&x).unwrap_or(Ordering::Equal),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// 1 行分のソートキーを抽出する小さなビュー。`AccountOut` / `AccountReport`
/// 双方に実装することで、ソートロジック本体を 1 つで共有できる。
pub(super) trait SortableRow {
    fn profile(&self) -> &str;
    fn weekly_used_percent(&self) -> Option<f64>;
    /// 週枠リセットまでの残秒数。`now` を渡すのは `AccountReport` 側で
    /// `DateTime<Utc>` から計算するため。`AccountOut` 側はキャッシュ JSON に
    /// 入った `resets_in_seconds` をそのまま返すと「キャッシュ作成時点」の
    /// 残秒数になるため、`resets_at` を再パースして `now` 起点で計算し直す。
    fn weekly_resets_in_seconds(&self, now: DateTime<Utc>) -> Option<i64>;
}

impl SortableRow for AccountOut {
    fn profile(&self) -> &str {
        &self.profile
    }
    fn weekly_used_percent(&self) -> Option<f64> {
        self.weekly.as_ref().map(|w| w.used_percent)
    }
    fn weekly_resets_in_seconds(&self, now: DateTime<Utc>) -> Option<i64> {
        self.weekly
            .as_ref()
            .and_then(|w| w.resets_at.as_deref())
            .and_then(parse_utc)
            .map(|r| (r - now).num_seconds().max(0))
    }
}

impl SortableRow for AccountReport {
    fn profile(&self) -> &str {
        &self.profile_name
    }
    fn weekly_used_percent(&self) -> Option<f64> {
        self.usage
            .as_ref()
            .ok()
            .and_then(|u| u.weekly.as_ref())
            .map(|w| w.used_percent)
    }
    fn weekly_resets_in_seconds(&self, now: DateTime<Utc>) -> Option<i64> {
        self.usage
            .as_ref()
            .ok()
            .and_then(|u| u.weekly.as_ref())
            .and_then(|w| w.resets_at)
            .map(|r| (r - now).num_seconds().max(0))
    }
}

/// 入力スライスを参照のまま並び替える。`SortKey::Provider` のときは挙動を
/// 呼び出し側に委ねるため、引数の `default_cmp` を使ってソートする
/// (statusline は provider.rank() ベース、table/json は維持なので
/// `None` を渡してそのまま出力する)。
pub(super) fn sorted_refs<T: SortableRow>(
    items: &[T],
    sort: SortKey,
    now: DateTime<Utc>,
    default_cmp: Option<fn(&T, &T) -> Ordering>,
) -> Vec<&T> {
    let mut v: Vec<&T> = items.iter().collect();
    match sort {
        SortKey::Provider => {
            if let Some(cmp) = default_cmp {
                v.sort_by(|a, b| cmp(a, b));
            }
        }
        SortKey::WeeklyUsage => v.sort_by(|a, b| {
            cmp_optional_desc(a.weekly_used_percent(), b.weekly_used_percent())
                .then_with(|| a.profile().cmp(b.profile()))
        }),
        SortKey::WeeklyReset => v.sort_by(|a, b| {
            cmp_optional_asc(
                a.weekly_resets_in_seconds(now),
                b.weekly_resets_in_seconds(now),
            )
            .then_with(|| a.profile().cmp(b.profile()))
        }),
    }
    v
}

/// statusline の default 順序: provider.rank() → profile 名。
pub(super) fn statusline_default_cmp(a: &AccountOut, b: &AccountOut) -> Ordering {
    a.provider
        .rank()
        .cmp(&b.provider.rank())
        .then_with(|| a.profile.cmp(&b.profile))
}

// sorted_refs は AccountOut / AccountReport 双方の SortableRow 実装で
// 共有しているため、テストは AccountOut ベースで挙動を確認しつつ、
// AccountReport 側のキー抽出が正しいことも 1 件だけクロスチェックする。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Provider;
    use crate::report::WindowOut;

    fn fixed_utc(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn out(
        profile: &str,
        provider: Provider,
        weekly_pct: Option<f64>,
        weekly_resets_at: Option<&str>,
    ) -> AccountOut {
        let resets_in = weekly_resets_at.and_then(parse_utc).map(|r| {
            let now = fixed_utc("2026-06-15T00:00:00Z");
            (r - now).num_seconds().max(0)
        });
        AccountOut {
            profile: profile.to_string(),
            provider,
            ok: true,
            plan: None,
            email: None,
            profile_email: None,
            label: None,
            group_label: None,
            five_hour: None,
            weekly: weekly_pct.map(|p| WindowOut {
                used_percent: p,
                resets_at: weekly_resets_at.map(str::to_string),
                resets_in_seconds: resets_in,
            }),
            error: None,
        }
    }

    fn profile_order<'a>(rows: &[&'a AccountOut]) -> Vec<&'a str> {
        rows.iter().map(|x| x.profile.as_str()).collect()
    }

    #[test]
    fn sort_weekly_usage_orders_descending_by_used_percent() {
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let items = vec![
            out("a", Provider::Claude, Some(10.0), None),
            out("b", Provider::Codex, Some(80.0), None),
            out("c", Provider::Antigravity, Some(50.0), None),
        ];
        let sorted = sorted_refs(&items, SortKey::WeeklyUsage, now, None);
        assert_eq!(profile_order(&sorted), vec!["b", "c", "a"]);
    }

    #[test]
    fn sort_weekly_usage_puts_missing_rows_last() {
        // weekly 無し(取得失敗 / プラン未開示など) の行は末尾に固定。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let items = vec![
            out("a", Provider::Claude, Some(80.0), None),
            out("b", Provider::Codex, None, None),
            out("c", Provider::Antigravity, Some(20.0), None),
        ];
        let sorted = sorted_refs(&items, SortKey::WeeklyUsage, now, None);
        assert_eq!(profile_order(&sorted), vec!["a", "c", "b"]);
    }

    #[test]
    fn sort_weekly_reset_orders_ascending_by_resets_at() {
        // resets_at から `now` を起点に残秒数を逆算するため、キャッシュ
        // JSON の resets_in_seconds(古い値) ではなく resets_at が正解。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let items = vec![
            out(
                "a",
                Provider::Claude,
                Some(50.0),
                Some("2026-06-17T00:00:00Z"),
            ),
            out(
                "b",
                Provider::Codex,
                Some(50.0),
                Some("2026-06-16T00:00:00Z"),
            ),
            out(
                "c",
                Provider::Antigravity,
                Some(50.0),
                Some("2026-06-20T00:00:00Z"),
            ),
        ];
        let sorted = sorted_refs(&items, SortKey::WeeklyReset, now, None);
        assert_eq!(profile_order(&sorted), vec!["b", "a", "c"]);
    }

    #[test]
    fn sort_weekly_reset_puts_missing_rows_last() {
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let items = vec![
            out("a", Provider::Claude, Some(50.0), None), // resets_at 無し
            out(
                "b",
                Provider::Codex,
                Some(50.0),
                Some("2026-06-16T00:00:00Z"),
            ),
            out(
                "c",
                Provider::Antigravity,
                Some(50.0),
                Some("2026-06-20T00:00:00Z"),
            ),
        ];
        let sorted = sorted_refs(&items, SortKey::WeeklyReset, now, None);
        assert_eq!(profile_order(&sorted), vec!["b", "c", "a"]);
    }

    #[test]
    fn sort_provider_preserves_input_order_without_default_cmp() {
        // SortKey::Provider + default_cmp None → 入力順そのまま(table/json 用)。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let items = vec![
            out("alpha", Provider::Codex, None, None),
            out("beta", Provider::Claude, None, None),
            out("gamma", Provider::Antigravity, None, None),
        ];
        let sorted = sorted_refs(&items, SortKey::Provider, now, None);
        assert_eq!(profile_order(&sorted), vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn sort_provider_applies_default_cmp_for_statusline() {
        // SortKey::Provider + statusline_default_cmp → Claude → Codex → AGY。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let items = vec![
            out("alpha", Provider::Codex, None, None),
            out("beta", Provider::Claude, None, None),
            out("gamma", Provider::Antigravity, None, None),
        ];
        let sorted = sorted_refs(&items, SortKey::Provider, now, Some(statusline_default_cmp));
        assert_eq!(profile_order(&sorted), vec!["beta", "alpha", "gamma"]);
    }

    #[test]
    fn sort_tiebreaks_on_profile_name() {
        // 使用率が同じ → profile 名昇順で安定。
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let items = vec![
            out("zeta", Provider::Claude, Some(50.0), None),
            out("alpha", Provider::Codex, Some(50.0), None),
        ];
        let sorted = sorted_refs(&items, SortKey::WeeklyUsage, now, None);
        assert_eq!(profile_order(&sorted), vec!["alpha", "zeta"]);
    }

    #[test]
    fn account_report_sortable_pulls_weekly_from_usage() {
        // AccountReport 側の SortableRow 実装も同じ順序を返すか確認。
        use crate::model::{Usage, Window};
        let now = fixed_utc("2026-06-15T00:00:00Z");
        let mk = |name: &str, prov: Provider, pct: Option<f64>, resets_at: Option<&str>| {
            let weekly = pct.map(|p| Window {
                used_percent: p,
                resets_at: resets_at.and_then(parse_utc),
            });
            AccountReport {
                profile_name: name.to_string(),
                profile_email: None,
                label: None,
                provider: prov,
                group_label: None,
                usage: Ok(Usage {
                    weekly,
                    ..Usage::default()
                }),
            }
        };
        let items = vec![
            mk(
                "a",
                Provider::Claude,
                Some(10.0),
                Some("2026-06-20T00:00:00Z"),
            ),
            mk(
                "b",
                Provider::Codex,
                Some(80.0),
                Some("2026-06-16T00:00:00Z"),
            ),
            mk("c", Provider::Antigravity, None, None),
        ];
        // 週枠使用率降順 → b, a, c。
        let usage_sorted = sorted_refs(&items, SortKey::WeeklyUsage, now, None);
        assert_eq!(
            usage_sorted
                .iter()
                .map(|r| r.profile_name.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a", "c"]
        );
        // リセット時刻昇順 → b(1日後), a(5日後), c(欠損)。
        let reset_sorted = sorted_refs(&items, SortKey::WeeklyReset, now, None);
        assert_eq!(
            reset_sorted
                .iter()
                .map(|r| r.profile_name.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a", "c"]
        );
    }
}
