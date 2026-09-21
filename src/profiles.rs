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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ai-usage-profiles-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn discover_reads_email_falls_back_to_dir_and_sorts_by_name() {
        let root = temp_root("discover");
        std::fs::write(
            root.join("Local State"),
            r#"{"profile":{"info_cache":{
                "Profile 2":{"name":"Work","user_name":"work@example.com"},
                "Default":{"name":"home","user_name":""},
                "Profile 1":{}
            }}}"#,
        )
        .unwrap();

        let profiles = discover(&root).unwrap();
        assert_eq!(
            profiles
                .iter()
                .map(|p| (p.dir.as_str(), p.name.as_str(), p.email.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("Default", "home", None),
                ("Profile 1", "Profile 1", None),
                ("Profile 2", "Work", Some("work@example.com")),
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discover_fails_on_missing_unreadable_and_shapeless_local_state() {
        let root = temp_root("discover-errors");

        // Local State が無い → path 付きのエラー(利用者が原因を特定できること)。
        let err = discover(&root).unwrap_err().to_string();
        assert!(err.contains("Local State"), "path が message に無い: {err}");

        // JSON として壊れている。
        std::fs::write(root.join("Local State"), "{ not json").unwrap();
        assert!(discover(&root).is_err());

        // JSON ではあるが profile.info_cache が無い。
        std::fs::write(root.join("Local State"), r#"{"profile":{}}"#).unwrap();
        let err = discover(&root).unwrap_err().to_string();
        assert!(err.contains("info_cache"), "原因が伝わらない: {err}");

        // info_cache が object でない(配列)場合も同じ経路で弾く。
        std::fs::write(root.join("Local State"), r#"{"profile":{"info_cache":[]}}"#).unwrap();
        assert!(discover(&root).is_err());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discover_falls_back_to_dir_when_name_is_not_a_string() {
        // Chrome が name を文字列以外で書いた場合でも profile を落とさず、
        // directory 名を表示名として採用する(行が消える方が困る)。
        let root = temp_root("discover-name-type");
        std::fs::write(
            root.join("Local State"),
            r#"{"profile":{"info_cache":{
                "Profile 3":{"name":42,"user_name":"a@example.com"},
                "Profile 4":{"name":null}
            }}}"#,
        )
        .unwrap();

        let profiles = discover(&root).unwrap();
        assert_eq!(
            profiles
                .iter()
                .map(|p| (p.dir.as_str(), p.name.as_str()))
                .collect::<Vec<_>>(),
            vec![("Profile 3", "Profile 3"), ("Profile 4", "Profile 4")]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cookies_db_returns_none_when_neither_store_exists() {
        let root = temp_root("cookies-db-none");
        // profile directory 自体が無い場合も、存在するが Cookie DB が無い場合も None。
        assert_eq!(cookies_db(&root, "Default"), None);
        std::fs::create_dir_all(root.join("Default/Network")).unwrap();
        assert_eq!(cookies_db(&root, "Default"), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cookies_db_prefers_network_store_and_falls_back_to_legacy_store() {
        let root = temp_root("cookies-db");
        let profile = root.join("Default");
        std::fs::create_dir_all(profile.join("Network")).unwrap();
        std::fs::write(profile.join("Cookies"), []).unwrap();
        assert_eq!(cookies_db(&root, "Default"), Some(profile.join("Cookies")));

        std::fs::write(profile.join("Network/Cookies"), []).unwrap();
        assert_eq!(
            cookies_db(&root, "Default"),
            Some(profile.join("Network/Cookies"))
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
