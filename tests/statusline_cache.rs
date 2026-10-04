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
