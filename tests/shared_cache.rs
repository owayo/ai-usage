//! 本物のCLIを複数起動し、偽のOAuthエンドポイントへのHTTP要求数を数える。

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ai-usage-http-cache-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ai-usage"));
        command
            .args(["--only", "grok", "--no-color", "--config"])
            .arg(self.0.join("config.toml"))
            .env("HOME", &self.0)
            .env("AI_USAGE_CACHE_DIR", self.0.join("cache"))
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .env("HTTP_PROXY", "")
            .env("HTTPS_PROXY", "")
            .env("ALL_PROXY", "")
            .env("http_proxy", "")
            .env("https_proxy", "")
            .env("all_proxy", "");
        command
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn concurrent_cli_instances_make_one_http_request_and_share_errors_across_formats() {
    let dir = TestDir::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let stopped = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(AtomicUsize::new(0));
    let stop = Arc::clone(&stopped);
    let count = Arc::clone(&requests);
    let server = std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            let (mut stream, _) = match listener.accept() {
                Ok(accepted) => accepted,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut received = Vec::new();
            let mut buffer = [0_u8; 2048];
            loop {
                let size = stream.read(&mut buffer).unwrap();
                assert_ne!(size, 0);
                received.extend_from_slice(&buffer[..size]);
                if let Some(end) = received.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&received[..end]);
                    let length = header
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if received.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            assert!(received.starts_with(b"POST /oauth2/token HTTP/1.1\r\n"));
            count.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(150));
            let body = r#"{"error":"fixture refresh rejected"}"#;
            write!(stream, "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    let auth_path = dir.0.join("auth.json");
    fs::write(
        &auth_path,
        serde_json::json!({
            "key":"fixture-access", "refresh_token":"fixture-refresh",
            "oidc_client_id":"fixture-client", "oidc_issuer":format!("http://{address}"),
            "expires_at":"2000-01-01T00:00:00Z",
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.0.join("config.toml"),
        format!(
            "[grok]\nauth_path = {}\nlabel = \"first\"\n",
            toml::Value::String(auth_path.display().to_string())
        ),
    )
    .unwrap();
    let children: Vec<_> = (0..4)
        .map(|_| {
            dir.command()
                .arg("--json")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["accounts"][0]["ok"], false);
        assert_eq!(report["accounts"][0]["label"], "first");
        assert!(
            report["accounts"][0]["error"]
                .as_str()
                .unwrap()
                .contains("fixture refresh rejected")
        );
    }
    fs::write(
        dir.0.join("config.toml"),
        format!(
            "[grok]\nauth_path = {}\nlabel = \"second\"\n",
            toml::Value::String(auth_path.display().to_string())
        ),
    )
    .unwrap();
    for args in [&[][..], &["--statusline"][..]] {
        let output = dir.command().args(args).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("second"), "{text}");
        if args.is_empty() {
            assert!(text.contains("fixture refresh rejected"), "{text}");
        } else {
            assert!(text.contains("--"), "{text}");
        }
    }
    stopped.store(true, Ordering::Relaxed);
    server.join().unwrap();
    assert_eq!(requests.load(Ordering::Relaxed), 1);
}

#[test]
fn declining_keychain_is_retried_on_the_next_cli_run() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TestDir::new();
    let chrome = dir.0.join("Library/Application Support/Google/Chrome");
    fs::create_dir_all(chrome.join("Default")).unwrap();
    fs::write(
        chrome.join("Local State"),
        r#"{"profile":{"info_cache":{"Default":{"name":"Home"}}}}"#,
    )
    .unwrap();
    let db = rusqlite::Connection::open(chrome.join("Default/Cookies")).unwrap();
    db.execute_batch(
        "CREATE TABLE cookies (host_key TEXT, name TEXT, encrypted_value BLOB);
        INSERT INTO cookies VALUES ('.claude.ai', 'sessionKey', x'01');",
    )
    .unwrap();
    drop(db);
    fs::write(dir.0.join("config.toml"), "").unwrap();

    // macOS のダイアログを開かず、拒否された鍵取得と再実行時の呼び出し回数を検証する。
    let bin = dir.0.join("bin");
    fs::create_dir(&bin).unwrap();
    let security = bin.join("security");
    fs::write(
        &security,
        "#!/bin/sh\nprintf 'called\\n' >> \"$AI_USAGE_TEST_KEYCHAIN_CALLS\"\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&security, fs::Permissions::from_mode(0o755)).unwrap();
    let calls = dir.0.join("calls");
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(paths).unwrap();
    for _ in 0..2 {
        let output = Command::new(env!("CARGO_BIN_EXE_ai-usage"))
            .args(["--only", "claude", "--config"])
            .arg(dir.0.join("config.toml"))
            .env("HOME", &dir.0)
            .env("AI_USAGE_CACHE_DIR", dir.0.join("cache"))
            .env("AI_USAGE_TEST_KEYCHAIN_CALLS", &calls)
            .env("PATH", &path)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("approve the macOS prompt"),
            "{output:?}"
        );
    }
    assert_eq!(fs::read_to_string(calls).unwrap(), "called\ncalled\n");
}
