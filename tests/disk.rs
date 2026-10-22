//! `--disk` を実際の CLI で検証する。起動ディスクの容量はこの Mac から読むため macOS 専用。
//! Chrome・Keychain・ネットワーク・共有キャッシュには触れないことも確かめる。
#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

/// マウントされていないことが確かなマウントポイント。
const MISSING: &str = "/Volumes/ai-usage-missing";

/// HOME と共有キャッシュの置き場所を、テストごとの空のディレクトリにする。
struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ai-usage-disk-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ai-usage"));
        command
            .args(args)
            .env("HOME", &self.0)
            .env("CLAUDE_CONFIG_DIR", &self.0)
            .env("AI_USAGE_CACHE_DIR", self.0.join("cache"))
            .env("TERM", "xterm-256color")
            .env_remove("NO_COLOR")
            .env_remove("XDG_CONFIG_HOME");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    /// 正常終了し、stderr に何も出さなかった出力。
    fn stdout(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn disk_json_reports_the_requested_volumes_only() {
    let fixture = Fixture::new();
    let missing_config = fixture.0.join("missing.toml");
    // 使用量向けの指定が混ざっていても動き、--mount があれば --config の誤りにも触れない。
    let body = fixture.stdout(&[
        "--json",
        "--only",
        "codex",
        "--refresh",
        "--cache-ttl",
        "0",
        "--no-disk",
        "--config",
        missing_config.to_str().unwrap(),
        "--disk",
        "--mount",
        "/",
        "--mount",
        MISSING,
    ]);
    let report: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert!(report["generated_at"].is_string());
    let volumes = report["volumes"].as_array().unwrap();
    assert_eq!(volumes.len(), 2, "{body}");

    let startup = &volumes[0];
    assert_eq!(startup["target"], "/");
    assert_eq!(startup["status"], "ok", "{body}");
    assert!(!startup["name"].as_str().unwrap().is_empty());
    let bytes = |key: &str| startup[key].as_u64().unwrap();
    assert!(bytes("total_bytes") > 0);
    assert!(bytes("free_bytes") <= bytes("available_bytes"));
    assert!(bytes("available_bytes") <= bytes("total_bytes"));
    let percent = startup["used_percent"].as_f64().unwrap();
    assert!((0.0..=100.0).contains(&percent), "{percent}");
    assert!(startup["capacity_basis"].is_string());
    assert!(startup["error"].is_null());

    assert_eq!(volumes[1]["target"], MISSING);
    assert_eq!(volumes[1]["status"], "not_mounted");
    assert!(volumes[1]["total_bytes"].is_null());

    // 利用枠の取得を一切しないので、共有キャッシュも作らない。
    assert!(!fixture.0.join("cache").exists());
}

#[test]
fn disk_text_reads_the_tui_volumes_from_the_config() {
    let fixture = Fixture::new();
    let config = fixture.0.join("config.toml");
    // [tui] disk = false は TUI の欄の表示設定なので、--disk の対象は変えない。
    std::fs::write(
        &config,
        format!("[tui]\ndisk = false\ndisk_mounts = [\"{MISSING}\"]\n"),
    )
    .unwrap();
    let config = config.to_str().unwrap();

    let plain = fixture.stdout(&["--disk", "--no-color", "--config", config]);
    let lines: Vec<&str> = plain.lines().collect();
    assert_eq!(lines.len(), 2, "{plain}");
    assert!(
        lines[0].contains('%') && lines[0].contains("available"),
        "{plain}"
    );
    assert!(lines[1].starts_with("ai-usage-missing"), "{plain}");
    assert!(lines[1].ends_with("  unavailable: not mounted"), "{plain}");
    assert!(!plain.contains('\x1b'), "{plain}");

    // 色は TUI と同じ判定に従う。NO_COLOR でも消え、指定が無ければ付く。起動ディスクの空きは
    // 実行のたびに変わり得るので、出力全体ではなく、値に依存しない行と色の有無で比べる。
    let output = fixture
        .command(&["--disk", "--config", config])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let no_color = String::from_utf8(output.stdout).unwrap();
    assert!(!no_color.contains('\x1b'), "{no_color:?}");
    assert_eq!(no_color.lines().count(), 2, "{no_color}");
    assert_eq!(no_color.lines().nth(1), Some(lines[1]), "{no_color}");
    let colored = fixture.stdout(&["--disk", "--config", config]);
    assert!(
        colored.contains("\x1b[38;5;196munavailable: not mounted\x1b[0m"),
        "{colored:?}"
    );
}

#[test]
fn disk_mode_rejects_other_modes_with_a_usage_error() {
    let fixture = Fixture::new();
    for args in [
        &["--disk", "--tui"][..],
        &["--disk", "--statusline"],
        &["--mount", "/"],
    ] {
        let output = fixture.run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
    }
}

#[test]
fn disk_output_to_a_closed_pipe_ends_quietly() {
    // `ai-usage --disk | head` のように読み手が先に終わっても、正常終了する。
    let fixture = Fixture::new();
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let output = fixture
        .command(&["--disk", "--mount", "/"])
        .stdout(writer)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}
