//! ディスク容量の 1 行の中身。
//!
//! バーのセル数・使用率・内訳・状態の文言と、それぞれの色をここで決める。描く側は、この区間
//! (文字と色) を自分の形式に変換するだけにして、表示どうしで計算が食い違わないようにする。

use std::cmp::Reverse;

use unicode_width::UnicodeWidthStr;

use super::palette::{ALERT, LABEL, Level, MUTED, Paint};
use super::statusline::pad_display;
use crate::disk::{ReadError, Volume};

/// 使用率 (%) の段階の境界。ディスクは利用枠より満杯に近い状態で使われるため、
/// 利用枠の境界 (`USAGE_LEVELS`) より高めに置く。
pub(super) const DISK_LEVELS: [f64; 3] = [80.0, 90.0, 95.0];
/// ボリューム名の欄の最大幅。
pub(super) const MAX_NAME_WIDTH: usize = 16;

/// 1 区間の文字と色。
pub(super) type Segment = (String, Paint);

/// 並べたボリュームのバーの位置を揃えるため、名前の欄を最も長い名前に合わせる。
pub(super) fn name_width<S: AsRef<str>>(names: impl IntoIterator<Item = S>) -> usize {
    names
        .into_iter()
        .map(|name| name.as_ref().width())
        .max()
        .unwrap_or(0)
        .min(MAX_NAME_WIDTH)
}

/// 名前の欄 (末尾の区切りを含む)。
pub(super) fn name_segment(name: &str, name_width: usize) -> Segment {
    (format!("{} ", pad_display(name, name_width + 1)), LABEL)
}

/// バーの 3 区分。使用中の区分は使用率の段階の色にし、パージ可能分と空きは段階によらず
/// 灰色にする。0 セルの区分は出さない。
pub(super) fn bar_segments(volume: &Volume, width: usize) -> Vec<Segment> {
    let [used, purgeable, free] = bar_cells(volume, width);
    [
        (used, "█", level(volume).fill()),
        (purgeable, "▒", LABEL),
        (free, "░", MUTED),
    ]
    .into_iter()
    .filter(|&(cells, _, _)| cells > 0)
    .map(|(cells, symbol, paint)| (symbol.repeat(cells), paint))
    .collect()
}

/// バーの後ろの使用率 (前に 2 桁空ける)。数字の色は使用中の区分と同じ段階で決める。
pub(super) fn percent_segment(volume: &Volume) -> Segment {
    (format!("  {}", percent_label(volume)), level(volume).text())
}

/// 使用率の後ろに置く内訳 (前に 2 桁空ける)。
pub(super) fn breakdown_segment(breakdown: &str) -> Segment {
    (format!("  {breakdown}"), LABEL)
}

/// 容量を取得できなかったボリュームの状態。
pub(super) fn unavailable_segment(error: &ReadError) -> Segment {
    (format!("unavailable: {error}"), ALERT)
}

/// 取得が戻らないボリュームの状態。
pub(super) fn no_response_segment() -> Segment {
    ("no response".into(), Level::Caution.fill())
}

fn level(volume: &Volume) -> Level {
    Level::of(volume.used_percent(), DISK_LEVELS)
}

/// バーの後ろに置く容量の内訳を、詳しいものから順に並べる。最後は内訳なし。
pub(super) fn breakdowns(volume: &Volume) -> Vec<String> {
    let available = format_bytes(volume.available);
    let total = format_bytes(volume.total);
    let mut breakdowns = Vec::new();
    if let Some(purgeable) = volume.purgeable.filter(|&purgeable| purgeable > 0) {
        let purgeable = format_bytes(purgeable);
        breakdowns.push(format!(
            "{available} available of {total} · {} free · {purgeable} purgeable",
            format_bytes(volume.free)
        ));
        breakdowns.push(format!(
            "{available} available of {total} · {purgeable} purgeable"
        ));
    }
    breakdowns.push(format!("{available} available of {total}"));
    breakdowns.push(format!("{available} available"));
    breakdowns.push(String::new());
    breakdowns
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn bar_segments_follow_the_cells_and_skip_empty_parts() {
        let text = |segments: &[Segment]| -> String {
            segments.iter().map(|(text, _)| text.as_str()).collect()
        };
        assert_eq!(text(&bar_segments(&startup(), 12)), "█████████▒▒░");
        let full = Volume::new("Full".into(), 500 * GB, 0, Some(0)).unwrap();
        let segments = bar_segments(&full, 12);
        assert_eq!(segments.len(), 1, "{segments:?}");
        assert_eq!(text(&segments), "█".repeat(12));
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
    fn disk_levels_escalate_at_80_90_95_percent() {
        // ディスクの段階の境界は 80% / 90% / 95%。境界ちょうどは上の段階に入る。
        for (percent, expected) in [
            (0.0, Level::Normal),
            (79.9, Level::Normal),
            (80.0, Level::Caution),
            (89.9, Level::Caution),
            (90.0, Level::Warning),
            (95.0, Level::Critical),
            (100.0, Level::Critical),
        ] {
            assert_eq!(Level::of(percent, DISK_LEVELS), expected, "{percent}");
        }
    }

    #[test]
    fn name_width_caps_the_longest_name() {
        assert_eq!(name_width(["Archive", "Macintosh HD"]), 12);
        assert_eq!(name_width(Vec::<String>::new()), 0);
        // 全角は 1 文字 2 桁で数え、欄は MAX_NAME_WIDTH で頭打ちにする。
        assert_eq!(name_width(["写真"]), 4);
        assert_eq!(
            name_width(["とても長いボリュームの名前です"]),
            MAX_NAME_WIDTH
        );
        assert_eq!(name_segment("Archive", 12).0, "Archive       ");
    }
}
