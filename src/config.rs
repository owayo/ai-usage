//! Optional config file: `~/.config/ai-usage/config.toml`
//! (or `$XDG_CONFIG_HOME/ai-usage/config.toml`).
//!
//! Without it, ai-usage auto-discovers every Chrome profile that has a Claude
//! or Codex session. With it, you choose which profiles to show, their order,
//! their labels, the providers per profile, and which account is "active".
//! Precedence: CLI flags > config file > auto-detection.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Config {
    /// Account email to highlight as active (overrides CLAUDE_CONFIG_DIR detection).
    pub active_email: Option<String>,

    /// Explicit profile selection. When non-empty, ONLY these profiles are shown,
    /// in this order. When empty/absent, all signed-in profiles are auto-discovered.
    pub profiles: Vec<ProfileCfg>,

    /// Antigravity (Google `agy`) usage. Auto-discovered from `~/.gemini` or a
    /// running `agy` unless disabled here. Not a Chrome profile, so it lives at
    /// the top level rather than under `[[profiles]]`.
    pub antigravity: Option<AntigravityCfg>,
}

/// Antigravity provider config (top-level `[antigravity]`).
#[derive(Deserialize, Clone, Default)]
#[serde(default)]
pub struct AntigravityCfg {
    /// `None` = auto (show when a token or running `agy` is found); `Some(false)` = off.
    pub enabled: Option<bool>,
    /// Override the OAuth token path (default: `~/.gemini/...`).
    pub token_path: Option<String>,
    /// Display label (default: the account email username).
    pub label: Option<String>,
}

#[derive(Deserialize)]
pub struct ProfileCfg {
    /// Chrome profile display name (e.g. "Work") or on-disk dir (e.g. "Default").
    #[serde(rename = "match")]
    pub matcher: String,

    /// Label shown instead of the account email username (e.g. "work").
    pub label: Option<String>,

    /// Providers to show for this profile (subset of `["claude", "codex"]`).
    /// Omitted = both.
    pub providers: Option<Vec<String>>,
}

impl ProfileCfg {
    pub fn matches(&self, name: &str, dir: &str) -> bool {
        self.matcher.eq_ignore_ascii_case(name) || self.matcher.eq_ignore_ascii_case(dir)
    }

    /// `(want_claude, want_codex)` from this profile's `providers` list.
    pub fn wants(&self) -> (bool, bool) {
        match &self.providers {
            None => (true, true),
            Some(list) => (
                list.iter().any(|s| s.eq_ignore_ascii_case("claude")),
                list.iter().any(|s| s.eq_ignore_ascii_case("codex")),
            ),
        }
    }
}

/// Resolve the config path: `$XDG_CONFIG_HOME/ai-usage/config.toml`, else
/// `~/.config/ai-usage/config.toml`. (Not `dirs::config_dir()`, which is
/// `~/Library/Application Support` on macOS.)
pub fn default_path() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(xdg).join("ai-usage").join("config.toml"));
    }
    dirs::home_dir().map(|h| h.join(".config").join("ai-usage").join("config.toml"))
}

/// Load the config, falling back to defaults (auto mode) when absent. An invalid
/// file is reported on stderr and treated as defaults rather than aborting.
/// Unknown fields are ignored so newer configs stay readable by older binaries.
pub fn load(explicit: Option<&Path>) -> Config {
    let path = match explicit {
        Some(p) => Some(p.to_path_buf()),
        None => default_path(),
    };
    let Some(path) = path else {
        return Config::default();
    };
    let Ok(text) = fs::read_to_string(&path) else {
        return Config::default(); // no file → auto mode
    };
    toml::from_str(&text).unwrap_or_else(|e| {
        eprintln!("ai-usage: ignoring invalid config {}: {e}", path.display());
        Config::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(matcher: &str, providers: Option<&[&str]>) -> ProfileCfg {
        ProfileCfg {
            matcher: matcher.to_string(),
            label: None,
            providers: providers.map(|s| s.iter().map(|x| x.to_string()).collect()),
        }
    }

    #[test]
    fn matches_is_case_insensitive_for_name_and_dir() {
        let c = cfg("Work", None);
        assert!(c.matches("work", "Default"));
        assert!(c.matches("Other", "WORK"));
        assert!(!c.matches("Home", "Profile 1"));
    }

    #[test]
    fn wants_defaults_to_both() {
        // providers が未指定なら Claude/Codex 両方を表示対象にする。
        let c = cfg("Work", None);
        assert_eq!(c.wants(), (true, true));
    }

    #[test]
    fn wants_filters_to_listed_providers() {
        // providers リストに名前があるものだけ表示する(順序・大小無関係)。
        assert_eq!(cfg("W", Some(&["claude"])).wants(), (true, false));
        assert_eq!(cfg("W", Some(&["CODEX"])).wants(), (false, true));
        assert_eq!(cfg("W", Some(&["claude", "codex"])).wants(), (true, true));
        assert_eq!(cfg("W", Some(&[])).wants(), (false, false));
    }

    #[test]
    fn parses_toml_with_profiles_and_antigravity() {
        let text = r#"
            active_email = "alice@example.com"

            [[profiles]]
            match = "Work"
            label = "work"
            providers = ["claude"]

            [antigravity]
            enabled = true
            label = "agy"
        "#;
        let parsed: Config = toml::from_str(text).unwrap();
        assert_eq!(parsed.active_email.as_deref(), Some("alice@example.com"));
        assert_eq!(parsed.profiles.len(), 1);
        assert_eq!(parsed.profiles[0].matcher, "Work");
        assert_eq!(parsed.profiles[0].label.as_deref(), Some("work"));
        assert_eq!(parsed.profiles[0].wants(), (true, false));
        let agy = parsed.antigravity.as_ref().unwrap();
        assert_eq!(agy.enabled, Some(true));
        assert_eq!(agy.label.as_deref(), Some("agy"));
    }

    #[test]
    fn parses_unknown_fields_gracefully() {
        // 未知のフィールドはデシリアライズエラーにせず無視する。
        let text = r#"
            unknown_field = 1

            [[profiles]]
            match = "Work"
            extra = "ignored"
        "#;
        let parsed: Config = toml::from_str(text).unwrap();
        assert_eq!(parsed.profiles.len(), 1);
    }

    #[test]
    fn empty_config_yields_defaults() {
        let parsed: Config = toml::from_str("").unwrap();
        assert!(parsed.active_email.is_none());
        assert!(parsed.profiles.is_empty());
        assert!(parsed.antigravity.is_none());
    }
}
