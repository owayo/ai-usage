//! キャッシュ描画を実際の CLI で検証する。認証情報や Chrome には依存しない。

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

struct CacheDir(PathBuf);

impl CacheDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ai-usage-cache-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("config.toml"), "").unwrap();
        let accounts: Vec<_> = ["claude", "codex", "grok"]
            .into_iter()
            .map(|provider| {
                serde_json::json!({
                    "profile": "Work",
                    "provider": provider,
                    "ok": true,
                    "label": "work",
                    "five_hour": null,
                    "weekly": {
                        "kind": "monthly",
                        "used_percent": 25.0,
                        "resets_at": null,
                        "resets_in_seconds": null,
                    },
                })
            })
            .collect();
        let report = serde_json::json!({
            "generated_at": "2026-07-01T00:00:00Z",
            "accounts": accounts,
        });
        std::fs::write(path.join("report.json"), report.to_string()).unwrap();
        Self(path)
    }

    fn render(&self, args: &[&str]) -> String {
        let output = Command::new(env!("CARGO_BIN_EXE_ai-usage"))
            .args(["--statusline", "--no-color", "--input"])
            .arg(self.0.join("report.json"))
            .args(["--config"])
            .arg(self.0.join("config.toml"))
            .args(args)
            .env("HOME", &self.0)
            .env("CLAUDE_CONFIG_DIR", &self.0)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        assert!(output.stderr.is_empty(), "{:?}", output.stderr);
        String::from_utf8(output.stdout).unwrap()
    }
}

impl Drop for CacheDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn cached_statusline_respects_only_provider() {
    let cache = CacheDir::new();
    let output = cache.render(&["--only", "codex"]);
    assert_eq!(output.lines().count(), 1, "{output}");
    assert!(output.contains("Codex"));
}

#[test]
fn cached_statusline_hide_still_applies_after_only() {
    let cache = CacheDir::new();
    assert!(
        cache
            .render(&["--only", "codex", "--statusline-hide", "codex"])
            .is_empty()
    );
}

#[test]
fn cached_statusline_without_filter_keeps_all_providers() {
    let cache = CacheDir::new();
    assert_eq!(cache.render(&[]).lines().count(), 3);
}

#[test]
fn init_config_uses_explicit_path_and_never_overwrites_it() {
    let fixture = CacheDir::new();
    let chrome = fixture.0.join("Library/Application Support/Google/Chrome");
    std::fs::create_dir_all(&chrome).unwrap();
    std::fs::write(
        chrome.join("Local State"),
        r#"{"profile":{"info_cache":{}}}"#,
    )
    .unwrap();
    let path = fixture.0.join("nested/alternate.toml");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_ai-usage"))
            .arg("--init-config")
            .arg("--config")
            .arg(&path)
            .env("HOME", &fixture.0)
            .env("CLAUDE_CONFIG_DIR", &fixture.0)
            .output()
            .unwrap()
    };
    let first = run();
    assert!(first.status.success(), "{:?}", first.stderr);
    assert!(first.stdout.is_empty());
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("ai-usage --init-config")
    );
    assert!(!fixture.0.join(".config/ai-usage/config.toml").exists());
    std::fs::write(&path, "# existing config\n").unwrap();
    let second = run();
    assert!(second.status.success(), "{:?}", second.stderr);
    assert!(
        String::from_utf8(second.stdout)
            .unwrap()
            .contains("ai-usage --init-config")
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# existing config\n"
    );
}

#[test]
fn init_config_preserves_a_dangling_symlink() {
    let fixture = CacheDir::new();
    let chrome = fixture.0.join("Library/Application Support/Google/Chrome");
    std::fs::create_dir_all(&chrome).unwrap();
    std::fs::write(
        chrome.join("Local State"),
        r#"{"profile":{"info_cache":{}}}"#,
    )
    .unwrap();
    let destination = fixture.0.join("linked-config.toml");
    let missing_target = fixture.0.join("missing-config.toml");
    std::os::unix::fs::symlink(&missing_target, &destination).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ai-usage"))
        .arg("--init-config")
        .arg("--config")
        .arg(&destination)
        .env("HOME", &fixture.0)
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("ai-usage --init-config")
    );
    assert_eq!(std::fs::read_link(destination).unwrap(), missing_target);
    assert!(!missing_target.exists());
}
