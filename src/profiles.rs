//! macOS の Chrome profile と Cookie database を検出する。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub fn chrome_root() -> Result<PathBuf> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    Ok(home.join("Library/Application Support/Google/Chrome"))
}

#[derive(Debug, Clone)]
pub struct Profile {
    /// on-disk directory 名。例: `"Default"` / `"Profile 7"`。
    pub dir: String,
    /// user-visible な表示名。例: `"Work"` / `"Home"`。
    pub name: String,
    pub email: Option<String>,
}

/// Chrome の `Local State` を読み、on-disk profile directory と表示名を対応付ける。
/// 両者は意図的に異なることがあるため、この file を正とする。
pub fn discover(root: &Path) -> Result<Vec<Profile>> {
    let local_state = root.join("Local State");
    let data = std::fs::read_to_string(&local_state)
        .with_context(|| format!("reading {}", local_state.display()))?;
    let local_state: serde_json::Value = serde_json::from_str(&data)?;
    let cache = local_state
        .pointer("/profile/info_cache")
        .and_then(|v| v.as_object())
        .context("Local State has no profile.info_cache")?;

    let mut profiles: Vec<Profile> = cache
        .iter()
        .map(|(dir, info)| Profile {
            dir: dir.clone(),
            name: info
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or(dir)
                .to_string(),
            email: info
                .get("user_name")
                .and_then(|n| n.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        })
        .collect();
    profiles.sort_by_key(|p| p.name.to_lowercase());
    Ok(profiles)
}

/// profile の Cookies SQLite DB を探す。新しい Chrome は `Network/` subdirectory に移動済み。
pub fn cookies_db(root: &Path, dir: &str) -> Option<PathBuf> {
    for sub in ["Network/Cookies", "Cookies"] {
        let p = root.join(dir).join(sub);
        if p.exists() {
            return Some(p);
        }
    }
    None
}
