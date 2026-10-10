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
fn cached_resets_show_count_and_expiry_without_credentials_or_network() {
    let cache = CacheDir::new();
    let path = cache.0.join("report.json");
    let mut report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    report["accounts"][1]["manual_resets"] = serde_json::json!([
        {"kind":"full","remaining":3,"expires_at":"2000-01-01T00:00:00Z"},
        {"kind":"full","remaining":3,"expires_at":"2099-07-30T00:00:00Z"},
        {"kind":"full","remaining":2,"expires_at":"2099-06-20T00:00:00Z"},
        {"kind":"five_hour","remaining":null,"expires_at":null}
    ]);
    std::fs::write(&path, report.to_string()).unwrap();
    let output = cache.render(&["--only", "codex"]);
    assert!(output.contains("R:full 5; 5h ? (2099/06/"), "{output}");
    assert_eq!(output.matches("2099/").count(), 1, "{output}");
    assert!(!output.contains('@'), "{output}");
    assert!(!output.contains("2000/"));
    assert!(!output.contains('\x1b'));
}

/// 読み手を先に閉じた pipe を stdout にして実行する。書き込みは必ず EPIPE になる。
fn run_with_closed_stdout(command: &mut Command) -> std::process::Output {
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    command.stdout(writer).output().unwrap()
}

#[test]
fn closed_stdout_pipe_ends_quietly_instead_of_panicking() {
    // `ai-usage ... | head` のように読み手が先に終わっても、panic せず正常終了する。
    let cache = CacheDir::new();
    let statusline = run_with_closed_stdout(
        Command::new(env!("CARGO_BIN_EXE_ai-usage"))
            .args(["--statusline", "--no-color", "--input"])
            .arg(cache.0.join("report.json"))
            .arg("--config")
            .arg(cache.0.join("config.toml"))
            .env("HOME", &cache.0)
            .env("CLAUDE_CONFIG_DIR", &cache.0),
    );
    assert!(statusline.status.success(), "{statusline:?}");
    assert!(statusline.stderr.is_empty(), "{statusline:?}");

    let chrome = cache.0.join("Library/Application Support/Google/Chrome");
    std::fs::create_dir_all(&chrome).unwrap();
    std::fs::write(
        chrome.join("Local State"),
        r#"{"profile":{"info_cache":{"Default":{"name":"Work"}}}}"#,
    )
    .unwrap();
    let profiles = run_with_closed_stdout(
        Command::new(env!("CARGO_BIN_EXE_ai-usage"))
            .arg("--list-profiles")
            .env("HOME", &cache.0),
    );
    assert!(profiles.status.success(), "{profiles:?}");
    assert!(profiles.stderr.is_empty(), "{profiles:?}");
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
fn init_config_lists_signed_in_profiles_with_their_providers() {
    let fixture = CacheDir::new();
    let chrome = fixture.0.join("Library/Application Support/Google/Chrome");
    std::fs::create_dir_all(&chrome).unwrap();
    std::fs::write(
        chrome.join("Local State"),
        r#"{"profile":{"info_cache":{
            "Default":{"name":"Work","user_name":"work.user@example.com"},
            "Profile 1":{"name":"Work","user_name":""},
            "Profile 2":{"name":"Home"},
            "Profile 3":{"name":"Empty"}
        }}}"#,
    )
    .unwrap();
    // 雛形の生成は Cookie の有無だけを見るため、値は空のままでよい(Keychain も使わない)。
    let cookie_db = |dir: &str, rows: &[(&str, &str)]| {
        let network = chrome.join(dir).join("Network");
        std::fs::create_dir_all(&network).unwrap();
        let conn = rusqlite::Connection::open(network.join("Cookies")).unwrap();
        conn.execute_batch(
            "CREATE TABLE cookies (
                host_key TEXT NOT NULL,
                name TEXT NOT NULL,
                encrypted_value BLOB NOT NULL DEFAULT x''
            )",
        )
        .unwrap();
        for (host, name) in rows {
            conn.execute(
                "INSERT INTO cookies (host_key, name) VALUES (?1, ?2)",
                rusqlite::params![host, name],
            )
            .unwrap();
        }
    };
    cookie_db("Default", &[(".claude.ai", "sessionKey")]);
    cookie_db(
        "Profile 1",
        &[
            (".claude.ai", "sessionKey"),
            (".chatgpt.com", "__Secure-next-auth.session-token.0"),
            ("www.pixellab.ai", "supabase-auth-token"),
        ],
    );
    cookie_db(
        "Profile 2",
        &[(".chatgpt.com", "__Secure-next-auth.session-token")],
    );
    cookie_db("Profile 3", &[]);
    std::fs::write(
        fixture.0.join(".claude.json"),
        r#"{"oauthAccount":{"emailAddress":"active@example.com"}}"#,
    )
    .unwrap();

    let path = fixture.0.join("generated.toml");
    let output = Command::new(env!("CARGO_BIN_EXE_ai-usage"))
        .arg("--init-config")
        .arg("--config")
        .arg(&path)
        .env("HOME", &fixture.0)
        .env("CLAUDE_CONFIG_DIR", &fixture.0)
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);

    // 生成物は TOML として読み戻せ、ログイン済みの profile だけを並べる。
    let text = std::fs::read_to_string(&path).unwrap();
    let config: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(config["active_email"].as_str(), Some("active@example.com"));
    let profiles: Vec<_> = config["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|profile| {
            (
                profile["match"].as_str().unwrap(),
                profile["label"].as_str().unwrap(),
                profile.get("providers").map(|providers| {
                    providers
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|provider| provider.as_str().unwrap())
                        .collect::<Vec<_>>()
                }),
            )
        })
        .collect();
    assert_eq!(
        profiles,
        vec![
            // 一部の provider だけにログインしている profile は providers を列挙する。
            ("Home", "Home", Some(vec!["codex"])),
            // 表示名が重複する profile は一意なディレクトリ名で照合し、label は email の local part。
            ("Default", "work.user", Some(vec!["claude"])),
            // 全 provider にログイン済みなら providers は省略する。
            ("Profile 1", "Work", None),
        ],
        "{text}"
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
