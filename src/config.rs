//! 任意 config file: `~/.config/ai-usage/config.toml`
//! (または `$XDG_CONFIG_HOME/ai-usage/config.toml`)。
//!
//! config がない場合、ai-usage は Claude または Codex session を持つ Chrome profile を
//! 自動検出する。config がある場合は、表示 profile、順序、label、profile ごとの provider、
//! "active" account を選べる。優先順は CLI flags > config file > auto-detection。

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Config {
    /// active として highlight する account email(CLAUDE_CONFIG_DIR detection を上書き)。
    pub active_email: Option<String>,

    /// 明示的な profile 選択。空でなければ、この profile だけをこの順序で表示する。
    /// 空または未指定なら、signed-in 済み profile をすべて auto-discover する。
    pub profiles: Vec<ProfileCfg>,

    /// Antigravity(Google `agy`)使用量。ここで disabled にしない限り、`~/.gemini` または
    /// 実行中の `agy` から auto-discover する。Chrome profile ではないため、
    /// `[[profiles]]` 配下ではなく top-level に置く。
    pub antigravity: Option<AntigravityCfg>,

    /// Grok(xAI `grok` CLI)使用量。Antigravity と同じく OAuth token
    /// (`~/.grok/auth.json`)ベースで、Chrome profile とは無関係な top-level 設定。
    pub grok: Option<GrokCfg>,

    /// `--statusline` 描画のみに効く設定。`--json` / table 出力には影響しない。
    pub statusline: Option<StatuslineCfg>,
}

/// `--statusline` 出力にだけ効く設定(top-level `[statusline]`)。
#[derive(Deserialize, Clone, Default)]
#[serde(default)]
pub struct StatuslineCfg {
    /// statusline に出さない provider の list(小文字: "claude" / "codex" / "antigravity"
    /// / "pixellab")。fetch と `--json` / table には影響しない。CLI `--statusline-hide`
    /// が指定されるとそちらで上書きされる。
    pub hide: Vec<String>,
}

/// Antigravity provider config(top-level `[antigravity]`)。
#[derive(Deserialize, Clone, Default)]
#[serde(default)]
pub struct AntigravityCfg {
    /// `None` = auto(token または実行中 `agy` があれば表示)、`Some(false)` = off。
    pub enabled: Option<bool>,
    /// OAuth token path を上書きする(default: `~/.gemini/...`)。
    pub token_path: Option<String>,
    /// 表示 label(default: account email の username)。
    pub label: Option<String>,
}

/// Grok provider config(top-level `[grok]`)。
#[derive(Deserialize, Clone, Default)]
#[serde(default)]
pub struct GrokCfg {
    /// `None` = auto(`~/.grok/auth.json` があれば表示)、`Some(false)` = off。
    pub enabled: Option<bool>,
    /// OAuth auth file path を上書きする(default: `~/.grok/auth.json`)。
    pub auth_path: Option<String>,
    /// 表示 label(default: account email の username)。
    pub label: Option<String>,
}

#[derive(Deserialize)]
pub struct ProfileCfg {
    /// Chrome profile 表示名(例: "Work")または on-disk dir(例: "Default")。
    #[serde(rename = "match")]
    pub matcher: String,

    /// account email の username の代わりに表示する label(例: "work")。
    pub label: Option<String>,

    /// この profile で表示する provider(`["claude", "codex"]` の subset)。
    /// 省略時は両方。
    pub providers: Option<Vec<String>>,
}

/// profile ごとに表示する Chrome 系 provider(Claude / Codex / PixelLab)の選択。
/// タプルの位置取り違えを避けるための小さな型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrowserWants {
    pub claude: bool,
    pub codex: bool,
    pub pixellab: bool,
}

impl BrowserWants {
    /// 全 provider を表示する既定値。
    pub fn all() -> Self {
        Self {
            claude: true,
            codex: true,
            pixellab: true,
        }
    }

    /// いずれかでも表示対象があるか(Chrome Cookie 復号が必要か)。
    pub fn any(self) -> bool {
        self.claude || self.codex || self.pixellab
    }
}

impl ProfileCfg {
    pub fn matches(&self, name: &str, dir: &str) -> bool {
        self.matcher.eq_ignore_ascii_case(name) || self.matcher.eq_ignore_ascii_case(dir)
    }

    /// この profile の `providers` list から表示する Chrome provider(Claude / Codex / PixelLab)を得る。
    pub fn wants(&self) -> BrowserWants {
        match &self.providers {
            None => BrowserWants::all(),
            Some(list) => BrowserWants {
                claude: list.iter().any(|s| s.eq_ignore_ascii_case("claude")),
                codex: list.iter().any(|s| s.eq_ignore_ascii_case("codex")),
                pixellab: list.iter().any(|s| s.eq_ignore_ascii_case("pixellab")),
            },
        }
    }
}

/// config path を解決する。`$XDG_CONFIG_HOME/ai-usage/config.toml`、なければ
/// `~/.config/ai-usage/config.toml`。macOS では `dirs::config_dir()` が
/// `~/Library/Application Support` になるため使わない。
pub fn default_path() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(xdg).join("ai-usage").join("config.toml"));
    }
    dirs::home_dir().map(|h| h.join(".config").join("ai-usage").join("config.toml"))
}

/// config を読み込む。存在しない場合は default(auto mode)に fallback する。
/// 不正な file は stderr に報告し、abort せず default として扱う。
/// unknown field は無視し、新しい config を古い binary でも読めるようにする。
pub fn load(explicit: Option<&Path>) -> Config {
    let path = match explicit {
        Some(p) => Some(p.to_path_buf()),
        None => default_path(),
    };
    let Some(path) = path else {
        return Config::default();
    };
    let Ok(text) = fs::read_to_string(&path) else {
        return Config::default(); // file なし → auto mode
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
    fn wants_defaults_to_all() {
        // providers が未指定なら Claude/Codex/PixelLab を全て表示対象にする。
        let c = cfg("Work", None);
        assert_eq!(c.wants(), BrowserWants::all());
    }

    #[test]
    fn wants_filters_to_listed_providers() {
        // providers リストに名前があるものだけ表示する(順序・大小無関係)。
        assert_eq!(
            cfg("W", Some(&["claude"])).wants(),
            BrowserWants {
                claude: true,
                codex: false,
                pixellab: false,
            }
        );
        assert_eq!(
            cfg("W", Some(&["CODEX"])).wants(),
            BrowserWants {
                claude: false,
                codex: true,
                pixellab: false,
            }
        );
        assert_eq!(
            cfg("W", Some(&["claude", "codex"])).wants(),
            BrowserWants {
                claude: true,
                codex: true,
                pixellab: false,
            }
        );
        assert_eq!(
            cfg("W", Some(&["Pixellab"])).wants(),
            BrowserWants {
                claude: false,
                codex: false,
                pixellab: true,
            }
        );
        assert_eq!(
            cfg("W", Some(&[])).wants(),
            BrowserWants {
                claude: false,
                codex: false,
                pixellab: false,
            }
        );
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

            [statusline]
            hide = ["claude", "codex"]
        "#;
        let parsed: Config = toml::from_str(text).unwrap();
        assert_eq!(parsed.active_email.as_deref(), Some("alice@example.com"));
        assert_eq!(parsed.profiles.len(), 1);
        assert_eq!(parsed.profiles[0].matcher, "Work");
        assert_eq!(parsed.profiles[0].label.as_deref(), Some("work"));
        assert_eq!(
            parsed.profiles[0].wants(),
            BrowserWants {
                claude: true,
                codex: false,
                pixellab: false,
            }
        );
        let agy = parsed.antigravity.as_ref().unwrap();
        assert_eq!(agy.enabled, Some(true));
        assert_eq!(agy.label.as_deref(), Some("agy"));
        let sl = parsed.statusline.as_ref().unwrap();
        assert_eq!(sl.hide, vec!["claude".to_string(), "codex".to_string()]);
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
