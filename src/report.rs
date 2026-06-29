//! serialize 可能な report DTO。`--json`(serialize)と `--statusline`
//! (cached file から deserialize して render)で共有する。
//! schema を 1 つにすることで、statusline は `--json` の出力をそのまま描画する。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{AccountReport, Provider, Window};

#[derive(Serialize, Deserialize)]
pub struct WindowOut {
    /// 使用率 percentage。0-100。
    pub used_percent: f64,
    /// 絶対 reset time(RFC 3339)。statusline はここから countdown を再計算するため、
    /// cache が古くても正しい "reset までの時間" を表示できる。
    pub resets_at: Option<String>,
    pub resets_in_seconds: Option<i64>,
}

impl WindowOut {
    fn new(w: &Window, now: DateTime<Utc>) -> Self {
        WindowOut {
            used_percent: w.used_percent,
            resets_at: w.resets_at.map(|r| r.to_rfc3339()),
            resets_in_seconds: w.resets_at.map(|r| (r - now).num_seconds().max(0)),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct AccountOut {
    pub profile: String,
    pub provider: Provider,
    pub ok: bool,
    pub plan: Option<String>,
    pub email: Option<String>,
    pub profile_email: Option<String>,
    /// config.toml 由来の display label。未設定なら None。
    pub label: Option<String>,
    /// multi-group provider(Antigravity)の model-group label。それ以外は `None`。
    pub group_label: Option<String>,
    pub five_hour: Option<WindowOut>,
    pub weekly: Option<WindowOut>,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Report {
    pub generated_at: String,
    pub accounts: Vec<AccountOut>,
}

impl Report {
    pub fn build(reports: &[AccountReport]) -> Self {
        let now = Utc::now();
        let accounts = reports
            .iter()
            .map(|r| match &r.usage {
                Ok(u) => AccountOut {
                    profile: r.profile_name.clone(),
                    provider: r.provider,
                    ok: true,
                    plan: u.plan.clone(),
                    email: u.email.clone(),
                    profile_email: r.profile_email.clone(),
                    label: r.label.clone(),
                    group_label: r.group_label.clone(),
                    five_hour: u.five_hour.as_ref().map(|w| WindowOut::new(w, now)),
                    weekly: u.weekly.as_ref().map(|w| WindowOut::new(w, now)),
                    error: None,
                },
                Err(e) => AccountOut {
                    profile: r.profile_name.clone(),
                    provider: r.provider,
                    ok: false,
                    plan: None,
                    email: None,
                    profile_email: r.profile_email.clone(),
                    label: r.label.clone(),
                    group_label: r.group_label.clone(),
                    five_hour: None,
                    weekly: None,
                    error: Some(format!("{e:#}")),
                },
            })
            .collect();
        Report {
            generated_at: now.to_rfc3339(),
            accounts,
        }
    }
}
