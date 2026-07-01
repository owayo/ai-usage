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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Usage;

    fn report_with(usage: anyhow::Result<Usage>) -> AccountReport {
        AccountReport {
            profile_name: "Work".to_string(),
            profile_email: Some("p@x.test".to_string()),
            label: Some("work".to_string()),
            provider: Provider::Claude,
            group_label: None,
            usage,
        }
    }

    #[test]
    fn build_maps_ok_usage_into_account_out() {
        // 正常系: ok=true、plan/email/window が引き継がれる。
        let usage = Usage {
            email: Some("a@x.test".to_string()),
            plan: Some("Max".to_string()),
            five_hour: Some(Window {
                used_percent: 42.0,
                resets_at: None,
            }),
            weekly: None,
        };
        let report = Report::build(&[report_with(Ok(usage))]);
        assert_eq!(report.accounts.len(), 1);
        let a = &report.accounts[0];
        assert!(a.ok);
        assert_eq!(a.plan.as_deref(), Some("Max"));
        assert_eq!(a.email.as_deref(), Some("a@x.test"));
        assert_eq!(a.profile_email.as_deref(), Some("p@x.test"));
        assert_eq!(a.label.as_deref(), Some("work"));
        assert!(a.error.is_none());
        let w = a.five_hour.as_ref().unwrap();
        assert_eq!(w.used_percent, 42.0);
        // resets_at 無しなら reset 情報も無い。
        assert!(w.resets_at.is_none());
        assert!(w.resets_in_seconds.is_none());
    }

    #[test]
    fn build_maps_err_into_error_row() {
        // 異常系: ok=false、error 文が入り、window は空、機微情報は持ち越さない。
        let report = Report::build(&[report_with(Err(anyhow::anyhow!("boom")))]);
        let a = &report.accounts[0];
        assert!(!a.ok);
        assert!(a.plan.is_none());
        assert!(a.email.is_none());
        assert!(a.five_hour.is_none());
        assert!(a.weekly.is_none());
        assert!(a.error.as_deref().unwrap().contains("boom"));
        // profile_email / label はエラー行でも保持される(表示名の解決に使うため)。
        assert_eq!(a.profile_email.as_deref(), Some("p@x.test"));
        assert_eq!(a.label.as_deref(), Some("work"));
    }

    #[test]
    fn build_clamps_past_reset_to_zero_and_keeps_future_positive() {
        // resets_in_seconds は `.max(0)` で負値にならない(過去リセット→0)。
        let past = Utc::now() - chrono::Duration::hours(3);
        let future = Utc::now() + chrono::Duration::hours(3);
        let usage = Usage {
            email: None,
            plan: None,
            five_hour: Some(Window {
                used_percent: 10.0,
                resets_at: Some(past),
            }),
            weekly: Some(Window {
                used_percent: 20.0,
                resets_at: Some(future),
            }),
        };
        let report = Report::build(&[report_with(Ok(usage))]);
        let a = &report.accounts[0];
        // 過去のリセットは 0 に丸められる(負にならない)。
        assert_eq!(a.five_hour.as_ref().unwrap().resets_in_seconds, Some(0));
        // 未来のリセットは正の残秒数。約 3 時間(誤差 60 秒許容)。
        let secs = a.weekly.as_ref().unwrap().resets_in_seconds.unwrap();
        assert!(
            (secs - 3 * 3600).abs() <= 60,
            "expected ~10800s, got {secs}"
        );
    }
}
