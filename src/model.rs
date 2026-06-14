//! Shared data model for usage reporting.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
    Antigravity,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::Claude => "Claude",
            Provider::Codex => "Codex",
            Provider::Antigravity => "Antigravity",
        }
    }

    /// Sort rank for the statusline: Claude first, then Codex, then others.
    pub fn rank(self) -> u8 {
        match self {
            Provider::Claude => 0,
            Provider::Codex => 1,
            Provider::Antigravity => 2,
        }
    }
}

/// A single rate-limit window (e.g. the rolling 5-hour or the weekly window).
#[derive(Clone, Debug)]
pub struct Window {
    /// Utilization as a percentage in `0..=100`.
    pub used_percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Default)]
pub struct Usage {
    pub email: Option<String>,
    pub plan: Option<String>,
    pub five_hour: Option<Window>,
    pub weekly: Option<Window>,
}

/// One displayable row of usage. A provider fetch returns one or more rows: most
/// providers return a single ungrouped row (`group_label = None`), but Antigravity
/// reports one row per model group ("Gemini", "Claude & GPT"), each tagged.
#[derive(Clone, Debug)]
pub struct UsageRow {
    /// Model-group label, for providers that split usage across groups.
    pub group_label: Option<String>,
    pub usage: Usage,
}

impl UsageRow {
    /// Wrap a single `Usage` as one ungrouped row (Claude/Codex).
    pub fn single(usage: Usage) -> Vec<UsageRow> {
        vec![UsageRow {
            group_label: None,
            usage,
        }]
    }
}

/// The result of querying one account row: one provider — and, for Antigravity,
/// one model group within it — within one Chrome profile or OAuth token.
pub struct AccountReport {
    pub profile_name: String,
    /// The Chrome profile's account email (from Local State), used to match the
    /// currently-active session account for highlighting.
    pub profile_email: Option<String>,
    /// Configured display label (from config.toml), if any.
    pub label: Option<String>,
    pub provider: Provider,
    /// Model-group label for multi-group providers (Antigravity); `None` otherwise.
    pub group_label: Option<String>,
    pub usage: anyhow::Result<Usage>,
}
