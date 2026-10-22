//! ディスク容量の 1 行の中身と、`--disk` の出力。
//!
//! バーのセル数・使用率・内訳・状態の文言と、それぞれの色をここで決める。TUI のディスク欄と
//! `--disk` の人向け出力は、この区間 (文字と色) を自分の形式に変換するだけにして、表示どうしで
//! 計算が食い違わないようにする。TUI は区間を ratatui の `Span` に、`--disk` は statusline と
//! 同じ ANSI SGR に変換する。

use std::cmp::Reverse;
use std::io;

use chrono::{DateTime, Utc};
use serde::Serialize;
use unicode_width::UnicodeWidthStr;

use super::palette::{ALERT, LABEL, Level, MUTED, Paint};
use super::statusline::{pad_display, paint};
use crate::disk::{ReadError, Reading, Target, Volume};

/// 使用率 (%) の段階の境界。ディスクは利用枠より満杯に近い状態で使われるため、
/// 利用枠の境界 (`USAGE_LEVELS`) より高めに置く。
pub(super) const DISK_LEVELS: [f64; 3] = [80.0, 90.0, 95.0];
/// ボリューム名の欄の最大幅。
pub(super) const MAX_NAME_WIDTH: usize = 16;
/// `--disk` のバーの幅。出力先の端末の幅は分からないので固定する。
const BAR_WIDTH: usize = 20;
/// `--disk --json` の形式の版。読み手が頼るキーを消したり意味を変えたりしたら上げる。
const SCHEMA_VERSION: u32 = 1;

/// 1 区間の文字と色。
pub(super) type Segment = (String, Paint);

/// 改行や ESC などの制御文字を空白にする。マウントポイントの名前やエラーの文に混ざっていても、
/// 1 ボリューム 1 行を保ち、端末の表示を崩さない。
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// 並べたボリュームのバーの位置を揃えるため、名前の欄を最も長い名前に合わせる。
pub(super) fn name_width<S: AsRef<str>>(names: impl IntoIterator<Item = S>) -> usize {
    names
        .into_iter()
        .map(|name| printable(name.as_ref()).width())
        .max()
        .unwrap_or(0)
        .min(MAX_NAME_WIDTH)
}

/// 名前の欄 (末尾の区切りを含む)。
pub(super) fn name_segment(name: &str, name_width: usize) -> Segment {
    (
        format!("{} ", pad_display(&printable(name), name_width + 1)),
        LABEL,
    )
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
    (printable(&format!("unavailable: {error}")), ALERT)
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

// ===== --disk ================================================================

/// `--disk`: 1 ボリューム 1 行の人向け出力を stdout に書く。
pub fn disk_text(readings: &[Reading], color: bool) -> io::Result<()> {
    super::write_stdout(&text(readings, color))
}

/// `--disk --json`: 容量をバイトのまま JSON で stdout に書く。
pub fn disk_json(readings: &[Reading]) -> io::Result<()> {
    super::write_stdout(&json(readings, Utc::now())?)
}

/// TUI のディスク欄の行と同じ見た目で、1 ボリューム 1 行に並べる。端末の幅は分からないので、
/// バーは固定幅にし、内訳は最も詳しいものを出す。期限までに戻らなかったボリュームは
/// `no response` と表示する。
pub(super) fn text(readings: &[Reading], color: bool) -> String {
    let names: Vec<String> = readings.iter().map(Reading::name).collect();
    let width = name_width(&names);
    let mut out = String::new();
    for (reading, name) in readings.iter().zip(&names) {
        let mut segments = vec![name_segment(name, width)];
        match &reading.result {
            Some(Ok(volume)) => {
                segments.extend(bar_segments(volume, BAR_WIDTH));
                segments.push(percent_segment(volume));
                if let Some(breakdown) = breakdowns(volume).first() {
                    segments.push(breakdown_segment(breakdown));
                }
            }
            Some(Err(error)) => segments.push(unavailable_segment(error)),
            None => segments.push(no_response_segment()),
        }
        for (text, part) in &segments {
            out += &paint(color, *part, text);
        }
        out.push('\n');
    }
    out
}

/// `--disk --json` の全体。
#[derive(Serialize)]
struct DiskReport {
    schema_version: u32,
    generated_at: String,
    volumes: Vec<VolumeOut>,
}

/// 1 ボリューム。読み手が場合分けせずに済むよう、どの状態でもすべてのキーを出す (不明は null)。
#[derive(Serialize)]
struct VolumeOut {
    /// 起動ディスクは `/`、それ以外は正規化したマウントポイント。
    target: String,
    status: Status,
    name: Option<String>,
    total_bytes: Option<u64>,
    free_bytes: Option<u64>,
    available_bytes: Option<u64>,
    /// パージ可能な量が分からないボリュームでは null。0 にはしない。
    purgeable_bytes: Option<u64>,
    /// 使用 (total − available) の割合。表示用に丸めない。
    used_percent: Option<f64>,
    capacity_basis: Option<CapacityBasis>,
    /// `status` が `error` のときの理由。
    error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Ok,
    NotMounted,
    NotLocal,
    Timeout,
    Error,
}

/// 利用可能 (`available_bytes`) の出どころ。
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum CapacityBasis {
    /// 重要な用途に使える容量 (Finder の「利用可能」)。パージ可能な量も分かる。
    ImportantUsage,
    /// 重要な用途の容量を返さないボリュームで、今すぐ書ける空きで代えた。
    FreeSpace,
}

impl VolumeOut {
    fn new(reading: &Reading) -> Self {
        let target = match &reading.target {
            Target::Startup => "/".to_string(),
            Target::Mount(path) => path.to_string_lossy().into_owned(),
        };
        let (status, error) = match &reading.result {
            Some(Ok(volume)) => return Self::measured(target, volume),
            Some(Err(ReadError::NotMounted)) => (Status::NotMounted, None),
            Some(Err(ReadError::NotLocal)) => (Status::NotLocal, None),
            Some(Err(ReadError::Failed(message))) => (Status::Error, Some(message.clone())),
            None => (Status::Timeout, None),
        };
        VolumeOut {
            target,
            status,
            name: None,
            total_bytes: None,
            free_bytes: None,
            available_bytes: None,
            purgeable_bytes: None,
            used_percent: None,
            capacity_basis: None,
            error,
        }
    }

    /// 容量を読めたボリューム。
    fn measured(target: String, volume: &Volume) -> Self {
        VolumeOut {
            target,
            status: Status::Ok,
            name: Some(volume.name.clone()),
            total_bytes: Some(volume.total),
            free_bytes: Some(volume.free),
            available_bytes: Some(volume.available),
            purgeable_bytes: volume.purgeable,
            used_percent: Some(volume.used_percent()),
            capacity_basis: Some(if volume.purgeable.is_some() {
                CapacityBasis::ImportantUsage
            } else {
                CapacityBasis::FreeSpace
            }),
            error: None,
        }
    }
}

/// `--disk --json` の本文 (末尾に改行)。色は付けない。
fn json(readings: &[Reading], now: DateTime<Utc>) -> serde_json::Result<String> {
    let report = DiskReport {
        schema_version: SCHEMA_VERSION,
        generated_at: now.to_rfc3339(),
        volumes: readings.iter().map(VolumeOut::new).collect(),
    };
    Ok(format!("{}\n", serde_json::to_string_pretty(&report)?))
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

    fn mount(path: &str) -> Target {
        Target::Mount(path.into())
    }

    fn reading(target: Target, result: Option<Result<Volume, ReadError>>) -> Reading {
        Reading { target, result }
    }

    /// 重要な用途の容量を返さない外付けディスク。40% 使用で、パージ可能な量は不明。
    fn external() -> Volume {
        Volume::new("External".into(), 2_000 * GB, 1_200 * GB, None).unwrap()
    }

    /// 人向け出力の各状態をそろえた並び。
    fn every_state() -> Vec<Reading> {
        vec![
            reading(Target::Startup, Some(Ok(startup()))),
            reading(mount("/Volumes/External"), Some(Ok(external()))),
            reading(mount("/Volumes/Archive"), Some(Err(ReadError::NotMounted))),
            reading(mount("/Volumes/Share"), Some(Err(ReadError::NotLocal))),
            reading(mount("/Volumes/Backup"), None),
            reading(
                mount("/Volumes/Broken"),
                Some(Err(ReadError::Failed("I/O error".into()))),
            ),
        ]
    }

    #[test]
    fn disk_text_lists_one_volume_per_line() {
        assert_eq!(
            text(&every_state(), false),
            concat!(
                "Macintosh HD  █████████████████▒▒░   86%  ",
                "70.8 GB available of 494.4 GB · 13.1 GB free · 57.7 GB purgeable\n",
                "External      ████████░░░░░░░░░░░░   40%  1.2 TB available of 2.0 TB\n",
                "Archive       unavailable: not mounted\n",
                "Share         unavailable: not a local volume\n",
                "Backup        no response\n",
                "Broken        unavailable: I/O error\n",
            )
        );
        assert_eq!(text(&[], false), "");
    }

    #[test]
    fn disk_text_colors_match_the_tui_panel() {
        let critical = Volume::new("Full".into(), 1_000 * GB, 30 * GB, Some(30 * GB)).unwrap();
        let readings = [
            reading(Target::Startup, Some(Ok(startup()))),
            reading(mount("/Volumes/Full"), Some(Ok(critical))),
            reading(mount("/Volumes/Archive"), Some(Err(ReadError::NotMounted))),
            reading(mount("/Volumes/Backup"), None),
        ];
        assert_eq!(
            text(&readings, true),
            concat!(
                // 86%: 使用中と使用率は 2 段目の黄。パージ可能分は灰色、空きは暗い灰色。
                "\x1b[38;5;245mMacintosh HD  \x1b[0m",
                "\x1b[38;5;178m█████████████████\x1b[0m",
                "\x1b[38;5;245m▒▒\x1b[0m",
                "\x1b[38;5;242m░\x1b[0m",
                "\x1b[38;5;178m   86%\x1b[0m",
                "\x1b[38;5;245m  70.8 GB available of 494.4 GB · 13.1 GB free · 57.7 GB purgeable\x1b[0m\n",
                // 97%: 最上段の赤。使用率の数字だけを太字にする。
                "\x1b[38;5;245mFull          \x1b[0m",
                "\x1b[38;5;196m███████████████████\x1b[0m",
                "\x1b[38;5;242m░\x1b[0m",
                "\x1b[1;38;5;196m   97%\x1b[0m",
                "\x1b[38;5;245m  30.0 GB available of 1.0 TB\x1b[0m\n",
                "\x1b[38;5;245mArchive       \x1b[0m",
                "\x1b[38;5;196munavailable: not mounted\x1b[0m\n",
                "\x1b[38;5;245mBackup        \x1b[0m",
                "\x1b[38;5;178mno response\x1b[0m\n",
            )
        );
    }

    #[test]
    fn disk_text_keeps_control_characters_out_of_the_terminal() {
        let readings = [reading(
            mount("/Volumes/Two\nLines"),
            Some(Err(ReadError::Failed("bad\x1b[2Jthing".into()))),
        )];
        assert_eq!(
            text(&readings, false),
            "Two Lines  unavailable: bad [2Jthing\n"
        );
    }

    #[test]
    fn disk_json_keeps_every_key_and_reports_each_state() {
        let now = DateTime::parse_from_rfc3339("2026-10-22T13:19:01.175468+00:00")
            .unwrap()
            .with_timezone(&Utc);
        // 2 つの値を別々に測った結果、パージ可能な量が 0 と分かったディスク。
        let measured_zero =
            Volume::new("Scratch".into(), 500 * GB, 20 * GB, Some(19 * GB)).unwrap();
        let mut readings = every_state();
        readings.push(reading(mount("/Volumes/Scratch"), Some(Ok(measured_zero))));
        let body = json(&readings, now).unwrap();
        assert!(body.ends_with("}\n"), "{body}");
        assert!(!body.contains('\x1b'), "{body}");
        let report: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(report["schema_version"], 1);
        assert_eq!(report["generated_at"], "2026-10-22T13:19:01.175468+00:00");

        let volumes = report["volumes"].as_array().unwrap();
        assert_eq!(volumes.len(), 7);
        let mut keys = [
            "target",
            "status",
            "name",
            "total_bytes",
            "free_bytes",
            "available_bytes",
            "purgeable_bytes",
            "used_percent",
            "capacity_basis",
            "error",
        ];
        keys.sort_unstable();
        for volume in volumes {
            let mut present: Vec<&str> = volume
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            present.sort_unstable();
            assert_eq!(present, keys, "{volume}");
        }

        // 値はバイトのまま。使用率は表示用に丸めない。
        let startup_json = &volumes[0];
        assert_eq!(startup_json["target"], "/");
        assert_eq!(startup_json["status"], "ok");
        assert_eq!(startup_json["name"], "Macintosh HD");
        assert_eq!(startup_json["total_bytes"], 494_384_795_648_u64);
        assert_eq!(startup_json["free_bytes"], 13_080_645_632_u64);
        assert_eq!(startup_json["available_bytes"], 70_754_759_808_u64);
        assert_eq!(startup_json["purgeable_bytes"], 57_674_114_176_u64);
        let percent = startup_json["used_percent"].as_f64().unwrap();
        assert!(
            (percent - startup().used_percent()).abs() < 1e-9,
            "{percent}"
        );
        assert!((percent - 85.6883).abs() < 1e-4, "{percent}");
        assert_eq!(startup_json["capacity_basis"], "important_usage");
        assert!(startup_json["error"].is_null());

        // パージ可能な量が分からなければ null にし、通常の空きで代えたことを示す。
        let external_json = &volumes[1];
        assert_eq!(external_json["target"], "/Volumes/External");
        assert_eq!(external_json["status"], "ok");
        assert_eq!(external_json["available_bytes"], 1_200 * GB);
        assert!(external_json["purgeable_bytes"].is_null());
        assert_eq!(external_json["used_percent"], 40.0);
        assert_eq!(external_json["capacity_basis"], "free_space");

        // 0 と分かっているパージ可能な量は 0 のまま出す。
        let scratch_json = &volumes[6];
        assert_eq!(scratch_json["purgeable_bytes"], 0);
        assert_eq!(scratch_json["capacity_basis"], "important_usage");

        // 値の無い状態は、target・status・error のほかをすべて null にする。
        for (volume, target, status, error) in [
            (&volumes[2], "/Volumes/Archive", "not_mounted", None),
            (&volumes[3], "/Volumes/Share", "not_local", None),
            (&volumes[4], "/Volumes/Backup", "timeout", None),
            (&volumes[5], "/Volumes/Broken", "error", Some("I/O error")),
        ] {
            assert_eq!(volume["target"], target, "{volume}");
            assert_eq!(volume["status"], status, "{volume}");
            assert_eq!(volume["error"].as_str(), error, "{volume}");
            for key in keys
                .iter()
                .filter(|key| !matches!(**key, "target" | "status" | "error"))
            {
                assert!(volume[*key].is_null(), "{key}: {volume}");
            }
        }
    }
}
