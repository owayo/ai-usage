//! statusline と TUI で共有する、リセット日時の表示と残り時間の警告のしきい値。
//! 色そのものは `palette::reset_countdown` が決める。

use chrono::{DateTime, Datelike, Local};

use crate::model::{Provider, WindowKind};
use crate::report::WindowOut;

/// ローカル日付に対応する日本語の曜日を付ける。TUI は別年の日時だけ年も出す。
pub(super) fn format_reset_at(local: DateTime<Local>, show_year: bool) -> String {
    let date_pattern = if show_year { "%Y/%m/%d" } else { "%m/%d" };
    let weekday =
        ["月", "火", "水", "木", "金", "土", "日"][local.weekday().num_days_from_monday() as usize];
    format!(
        "{} {weekday} {}",
        local.format(date_pattern),
        local.format("%H:%M")
    )
}

pub(super) const FIVE_H_TH: [i64; 3] = [3600, 7200, 10800];
const DAILY_TH: [i64; 3] = [4 * 3600, 8 * 3600, 12 * 3600];
pub(super) const WEEK_TH: [i64; 3] = [86400, 172800, 4 * 86400];
pub(super) const MONTHLY_TH: [i64; 3] = [3 * 86400, 7 * 86400, 14 * 86400];

pub(super) fn window_thresholds(w: Option<&WindowOut>, legacy: [i64; 3]) -> [i64; 3] {
    match w.and_then(|w| w.kind) {
        Some(WindowKind::FiveHour) => FIVE_H_TH,
        Some(WindowKind::Daily) => DAILY_TH,
        Some(WindowKind::Weekly) => WEEK_TH,
        Some(WindowKind::Monthly) => MONTHLY_TH,
        None => legacy,
    }
}

/// `kind` を持たない旧キャッシュ用の長期枠しきい値。
/// ラベルと同じく PixelLab / Grok は月次、それ以外は週次として扱う。
pub(super) fn legacy_long_window_thresholds(provider: Provider) -> [i64; 3] {
    match provider {
        Provider::PixelLab | Provider::Grok => MONTHLY_TH,
        _ => WEEK_TH,
    }
}
