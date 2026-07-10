//! usage report 用の共有 data model。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
    Antigravity,
    PixelLab,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::Claude => "Claude",
            Provider::Codex => "Codex",
            Provider::Antigravity => "Antigravity",
            Provider::PixelLab => "PixelLab",
        }
    }

    /// statusline の sort rank。Claude、Codex、その他の順。
    pub fn rank(self) -> u8 {
        match self {
            Provider::Claude => 0,
            Provider::Codex => 1,
            Provider::Antigravity => 2,
            Provider::PixelLab => 3,
        }
    }
}

/// rate-limit window の実際の周期。表示側が provider から周期を推測せず、
/// provider が取得時点で正しい意味を付与する。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    FiveHour,
    Daily,
    Weekly,
    Monthly,
}

impl WindowKind {
    pub fn label(self) -> &'static str {
        match self {
            WindowKind::FiveHour => "5h",
            WindowKind::Daily => "1d",
            WindowKind::Weekly => "1w",
            WindowKind::Monthly => "1m",
        }
    }
}

/// 単一の rate-limit window。
#[derive(Clone, Debug)]
pub struct Window {
    pub kind: WindowKind,
    /// 使用率。percentage で `0..=100`。
    pub used_percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Default)]
pub struct Usage {
    pub email: Option<String>,
    pub plan: Option<String>,
    /// 左側の短期スロット。通常は 5 時間だが、provider によっては日次。
    pub short: Option<Window>,
    /// 右側の長期スロット。週次または月次。
    pub long: Option<Window>,
}

/// 表示可能な usage 1 行。provider fetch は 1 行以上を返す。多くの provider は
/// group なしの単一行(`group_label = None`)だが、Antigravity は model group
/// ("Gemini", "Claude & GPT")ごとに tag 付きの行を返す。
#[derive(Clone, Debug)]
pub struct UsageRow {
    /// usage を group 分割する provider の model-group label。
    pub group_label: Option<String>,
    pub usage: Usage,
}

impl UsageRow {
    /// 単一の `Usage` を group なしの 1 行として包む(Claude/Codex)。
    pub fn single(usage: Usage) -> Vec<UsageRow> {
        vec![UsageRow {
            group_label: None,
            usage,
        }]
    }
}

/// account 行 1 件の query 結果。1 つの Chrome profile または OAuth token の中の
/// 1 provider、Antigravity ではさらにその中の 1 model group を表す。
pub struct AccountReport {
    pub profile_name: String,
    /// Chrome profile の account email(Local State 由来)。現在 active な session account の
    /// highlight 照合に使う。
    pub profile_email: Option<String>,
    /// config.toml 由来の display label。未設定なら None。
    pub label: Option<String>,
    pub provider: Provider,
    /// multi-group provider(Antigravity)の model-group label。それ以外は `None`。
    pub group_label: Option<String>,
    pub usage: anyhow::Result<Usage>,
}
