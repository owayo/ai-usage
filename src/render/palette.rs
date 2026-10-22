//! statusline と TUI で共有する色。
//!
//! どの要素を何色にするかはここで決め、statusline は ANSI SGR、TUI は ratatui の
//! `Style` へ変換するだけにする。両方が同じ関数を呼ぶので、同じ意味の要素の色がずれない。

use chrono::{DateTime, Utc};

use crate::model::Provider;

/// 前景色。256 色の番号か truecolor。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fg {
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// 1 区間の装飾。前景色と太字だけを扱う。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Paint {
    pub fg: Fg,
    pub bold: bool,
}

impl Paint {
    const fn indexed(index: u8) -> Self {
        Paint {
            fg: Fg::Indexed(index),
            bold: false,
        }
    }

    const fn rgb((r, g, b): (u8, u8, u8)) -> Self {
        Paint {
            fg: Fg::Rgb(r, g, b),
            bold: false,
        }
    }

    const fn bold(self) -> Self {
        Paint { bold: true, ..self }
    }
}

/// 枠のラベル、アカウント名、プラン、手動リセットの件数など、値に添える文字。
/// ディスク欄のパージ可能の部分 (`▒`) にも使う。
pub(super) const LABEL: Paint = Paint::indexed(245);
/// 値の無いところ (バーの消灯部や空き、`--`)、利用枠のリセット日時、値の古さ。
pub(super) const MUTED: Paint = Paint::indexed(242);
/// 24 時間枠の制限、7 日未満で切れる手動リセットの期限、取得の失敗。
pub(super) const ALERT: Paint = Paint::indexed(196);
/// statusline で、このセッションのアカウント名を示す。
pub(super) const ACTIVE: Paint = ALERT.bold();
const WHITE: Paint = Paint::rgb((255, 255, 255));

/// 4 段階の警告色。利用枠の使用率、ディスクの使用率、リセットまでの残り時間で同じ 4 色を使う。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Level {
    Normal,
    Caution,
    Warning,
    Critical,
}

/// 利用枠の使用率 (%) の段階の境界。
pub(super) const USAGE_LEVELS: [f64; 3] = [60.0, 80.0, 90.0];

impl Level {
    /// `value` が昇順の境界 `bounds` のどこにあるか。境界ちょうどは上の段階に入る。
    /// 比べられない値 (NaN) は Normal になる。
    pub(super) fn of(value: f64, bounds: [f64; 3]) -> Self {
        if value >= bounds[2] {
            Level::Critical
        } else if value >= bounds[1] {
            Level::Warning
        } else if value >= bounds[0] {
            Level::Caution
        } else {
            Level::Normal
        }
    }

    /// 利用枠の使用率の段階。
    pub(super) fn usage(percent: f64) -> Self {
        Self::of(percent, USAGE_LEVELS)
    }

    /// バーの点灯部と、リセットまでの残り時間の色。
    pub(super) const fn fill(self) -> Paint {
        Paint::indexed(match self {
            Level::Normal => 35,
            Level::Caution => 178,
            Level::Warning => 208,
            Level::Critical => 196,
        })
    }

    /// 使用率の数字の色。上の 2 段階は太字にして、バーより目立たせる。
    pub(super) const fn text(self) -> Paint {
        match self {
            Level::Warning | Level::Critical => self.fill().bold(),
            Level::Normal | Level::Caution => self.fill(),
        }
    }
}

/// リセットまでの残り時間の色。`thresholds` は周期ごとの境界 (秒、昇順) で、境界ちょうどは
/// 余裕のある側に入る。日時が不明なら MUTED、期限を過ぎていれば Normal にする。
pub(super) fn reset_countdown(seconds: Option<i64>, thresholds: [i64; 3]) -> Paint {
    let level = match seconds {
        None => return MUTED,
        Some(seconds) if seconds <= 0 => Level::Normal,
        Some(seconds) if seconds < thresholds[0] => Level::Critical,
        Some(seconds) if seconds < thresholds[1] => Level::Warning,
        Some(seconds) if seconds < thresholds[2] => Level::Caution,
        Some(_) => Level::Normal,
    };
    level.fill()
}

/// 手動リセットの有効期限の日時。7 日未満で切れるものだけを ALERT にする。
/// ちょうど 7 日、期限切れ、日時が不明なものは LABEL。
pub(super) fn manual_reset_deadline(expiry: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Paint {
    if expiry.is_some_and(|expiry| expiry > now && expiry - now < chrono::Duration::weeks(1)) {
        ALERT
    } else {
        LABEL
    }
}

/// provider の印 (statusline 左端のラベルかロゴ、TUI の見出しの provider 名) の色。
/// Codex のティールは暗い背景に沈み、Grok (xAI) は白黒のブランドなので、この 2 つは白にする。
pub(super) fn marker(provider: Provider) -> Paint {
    match provider {
        Provider::Codex | Provider::Grok => WHITE,
        _ => Paint::rgb(brand_rgb(provider)),
    }
}

/// provider ごとの brand RGB。table(comfy-table `Color::Rgb`)と `marker` で共有する。
pub(super) fn brand_rgb(p: Provider) -> (u8, u8, u8) {
    match p {
        Provider::Claude => (217, 119, 87), // Anthropic のコーラル #D97757
        Provider::Codex => (16, 163, 127),  // OpenAI のティール #10A37F
        Provider::Antigravity => (66, 133, 244), // Google のブルー #4285F4
        Provider::PixelLab => (132, 204, 22), // ピクセルアート調の黄緑 #84CC16
        // xAI brand は白/黒基調で dark 背景では黒が沈むため、識別性の高い
        // 明るい cyan(Tailwind cyan-400 #22D3EE)を採用する。
        Provider::Grok => (34, 211, 238),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::reset::{MONTHLY_TH, WEEK_TH};

    fn utc(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn named_colors_keep_their_palette_numbers() {
        // statusline の出力をバイト単位で保つため、番号と太字の有無を固定する。
        assert_eq!(LABEL, Paint::indexed(245));
        assert_eq!(MUTED, Paint::indexed(242));
        assert_eq!(ALERT, Paint::indexed(196));
        assert_eq!(
            ACTIVE,
            Paint {
                fg: Fg::Indexed(196),
                bold: true
            }
        );
    }

    #[test]
    fn usage_levels_escalate_at_60_80_90_percent() {
        for (percent, fill, bold) in [
            (0.0, 35, false),
            (59.9, 35, false),
            (60.0, 178, false),
            (79.9, 178, false),
            (80.0, 208, true),
            (89.9, 208, true),
            (90.0, 196, true),
            (150.0, 196, true),
            (f64::NAN, 35, false),
        ] {
            let level = Level::usage(percent);
            assert_eq!(level.fill(), Paint::indexed(fill), "{percent}");
            assert_eq!(
                level.text(),
                Paint {
                    fg: Fg::Indexed(fill),
                    bold
                },
                "{percent}"
            );
            // バーの点灯部は太字にしない。
            assert!(!level.fill().bold, "{percent}");
        }
    }

    #[test]
    fn levels_accept_other_bounds() {
        let disk = [80.0, 90.0, 95.0];
        assert_eq!(Level::of(79.9, disk), Level::Normal);
        assert_eq!(Level::of(80.0, disk), Level::Caution);
        assert_eq!(Level::of(90.0, disk), Level::Warning);
        assert_eq!(Level::of(95.0, disk), Level::Critical);
    }

    #[test]
    fn reset_countdown_uses_exclusive_thresholds() {
        // しきい値ちょうどは「まだ余裕がある側」に入る(`<` 判定)。境界がずれると
        // リセット直前の赤が 1 段早く/遅く出る。
        let th = WEEK_TH;
        assert_eq!(reset_countdown(Some(1), th), Paint::indexed(196));
        assert_eq!(reset_countdown(Some(th[0] - 1), th), Paint::indexed(196));
        assert_eq!(reset_countdown(Some(th[0]), th), Paint::indexed(208));
        assert_eq!(reset_countdown(Some(th[1]), th), Paint::indexed(178));
        assert_eq!(reset_countdown(Some(th[2]), th), Paint::indexed(35));
        // 期限到来は緑、日時不明は MUTED。
        assert_eq!(reset_countdown(Some(0), th), Paint::indexed(35));
        assert_eq!(reset_countdown(Some(-1), th), Paint::indexed(35));
        assert_eq!(reset_countdown(None, th), Paint::indexed(242));
        // 月次枠は同じ関数をより長いしきい値で使う(2 日後は月次なら危険域)。
        assert_eq!(
            reset_countdown(Some(2 * 86400), MONTHLY_TH),
            Paint::indexed(196)
        );
        assert_eq!(
            reset_countdown(Some(2 * 86400), WEEK_TH),
            Paint::indexed(178)
        );
        // 残り時間の色は太字にしない。
        assert!(!reset_countdown(Some(1), th).bold);
    }

    #[test]
    fn manual_reset_deadline_is_alert_only_below_one_week() {
        let now = utc("2026-06-15T00:00:00Z");
        let week = chrono::Duration::weeks(1);
        let tick = chrono::Duration::nanoseconds(1);
        for (remaining, expected) in [
            (Some(tick), ALERT),
            (Some(week - tick), ALERT),
            (Some(week), LABEL),
            (Some(week + tick), LABEL),
            (Some(chrono::Duration::zero()), LABEL),
            (Some(-tick), LABEL),
            (None, LABEL),
        ] {
            let expiry = remaining.map(|remaining| now + remaining);
            assert_eq!(
                manual_reset_deadline(expiry, now),
                expected,
                "remaining={remaining:?}"
            );
        }
    }

    #[test]
    fn markers_use_brand_colors_except_codex_and_grok() {
        for provider in [Provider::Claude, Provider::Antigravity, Provider::PixelLab] {
            assert_eq!(marker(provider), Paint::rgb(brand_rgb(provider)));
        }
        for provider in [Provider::Codex, Provider::Grok] {
            assert_eq!(marker(provider), Paint::rgb((255, 255, 255)));
        }
    }
}
