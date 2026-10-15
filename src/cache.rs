//! プロバイダの取得結果をプロセス間で共有する。認証材料や表示ラベルは保存しない。

use std::fs::{self, File, OpenOptions, TryLockError};
use std::future::Future;
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use crate::diagnostics::Diagnostics;
use crate::model::{Provider, UsageRow};

/// 手元の認証情報を読めなかった場合は、次の起動で読み直せるようキャッシュしない。
#[derive(Debug)]
pub(crate) struct CredentialError(pub String);

impl std::fmt::Display for CredentialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CredentialError {}

const ERROR_TTL: Duration = Duration::from_secs(15);
const LOCK_TIMEOUT: Duration = Duration::from_secs(35);

#[derive(Clone)]
pub struct Cache {
    pub diagnostics: Diagnostics,
    root: PathBuf,
    ttl: Duration,
    refresh: bool,
    requested_at: SystemTime,
}

/// パスの表記差を吸収し、取得元ごとに安定したキーを作る。
/// SHA-1 は秘密の保護ではなくファイル名の生成にのみ使う。
pub fn key(provider: Provider, source: &Path) -> String {
    let source = source
        .canonicalize()
        .or_else(|_| std::path::absolute(source))
        .unwrap_or_else(|_| source.to_path_buf());
    let mut hash = Sha1::new();
    hash.update(provider.label().as_bytes());
    hash.update([0]);
    hash.update(source.as_os_str().as_encoded_bytes());
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn file_fingerprint(path: &Path) -> String {
    match fs::metadata(path) {
        Ok(meta) => format!(
            "{}:{}:{}:{}:{}",
            meta.dev(),
            meta.ino(),
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec()
        ),
        Err(_) => "missing".to_string(),
    }
}

#[derive(Serialize, Deserialize)]
struct Entry {
    version: u8,
    updated_at: SystemTime,
    fingerprint: String,
    result: std::result::Result<Vec<UsageRow>, String>,
}

impl Entry {
    fn usable(&self, cache: &Cache, now: SystemTime) -> bool {
        let Ok(age) = now.duration_since(self.updated_at) else {
            return false; // 時計が戻った場合も古い結果を新着と見なさない。
        };
        let ttl = if self.result.is_ok() {
            cache.ttl
        } else {
            cache.ttl.min(ERROR_TTL)
        };
        let updated = chrono::DateTime::<chrono::Utc>::from(self.updated_at);
        let now_utc = chrono::DateTime::<chrono::Utc>::from(now);
        let reset_passed = self.result.as_ref().is_ok_and(|rows| {
            rows.iter().any(|row| {
                [row.usage.short.as_ref(), row.usage.long.as_ref()]
                    .into_iter()
                    .flatten()
                    .filter_map(|window| window.resets_at)
                    .any(|reset| updated < reset && reset <= now_utc)
            })
        });
        self.version == 1
            && !reset_passed
            && ((!cache.refresh && age < ttl) || self.updated_at >= cache.requested_at)
    }

    fn into_result(self) -> Result<Vec<UsageRow>> {
        self.result.map_err(anyhow::Error::msg)
    }
}

// unlock を明示して、fork した子が exec 前に同じ open file description を持つ場合も
// 親の終了後までロックを引き延ばさない。ロックファイル自体は削除しない。
struct Guard(File);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl Cache {
    pub fn new(ttl: Duration, refresh: bool, diagnostics: Diagnostics) -> Result<Self> {
        let root = std::env::var_os("AI_USAGE_CACHE_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| dirs::cache_dir().map(|root| root.join("ai-usage")))
            .context("could not determine the usage cache directory")?;
        Ok(Self {
            diagnostics,
            ..Self::at(root.join("v1"), ttl, refresh)
        })
    }

    pub(crate) fn at(root: PathBuf, ttl: Duration, refresh: bool) -> Self {
        Self {
            diagnostics: Diagnostics::default(),
            root,
            ttl,
            refresh,
            requested_at: SystemTime::now(),
        }
    }

    /// TUI の各更新にも独立した要求時刻を持たせる。
    pub fn request(&self) -> Self {
        Self {
            requested_at: SystemTime::now(),
            ..self.clone()
        }
    }

    fn read(&self, key: &str, fingerprint: &str) -> Option<Entry> {
        let data = fs::read(self.root.join(format!("{key}.json"))).ok()?;
        let entry: Entry = serde_json::from_slice(&data).ok()?;
        (entry.fingerprint == fingerprint && entry.usable(self, SystemTime::now())).then_some(entry)
    }

    async fn lock(&self, key: &str) -> Result<Guard> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.root)
            .context("creating usage cache directory")?;
        anyhow::ensure!(
            !fs::symlink_metadata(&self.root)?.file_type().is_symlink(),
            "usage cache directory must not be a symbolic link"
        );
        fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(self.root.join(format!("{key}.lock")))
            .context("opening usage cache lock")?;
        let deadline = Instant::now() + LOCK_TIMEOUT;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Guard(file)),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(TryLockError::WouldBlock) => bail!("timed out waiting for usage cache update"),
                Err(TryLockError::Error(error)) => return Err(error.into()),
            }
        }
    }

    fn write(&self, key: &str, entry: &Entry) -> Result<()> {
        // 同一キーの writer はロック下に1つだけ。前回の異常終了で残った一時ファイルは
        // 削除して作り直し、完成するまで読者へ見せない。
        let temporary = self.root.join(format!("{key}.tmp"));
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&serde_json::to_vec(entry)?)?;
            fs::rename(&temporary, self.root.join(format!("{key}.json")))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    /// ロック待ちの後にも読み直す。待機中に先行プロセスが取得した結果は再取得しない。
    /// fetch の期限は呼び出し側が設定し、ロック待ちとは別に予算を確保する。
    pub async fn fetch(
        &self,
        key: &str,
        fingerprint: &str,
        fetch: impl Future<Output = Result<Vec<UsageRow>>>,
    ) -> Result<Vec<UsageRow>> {
        if let Some(entry) = self.read(key, fingerprint) {
            return entry.into_result();
        }
        let _guard = self.lock(key).await?;
        if let Some(entry) = self.read(key, fingerprint) {
            return entry.into_result();
        }
        let result = fetch.await;
        if result
            .as_ref()
            .is_err_and(|error| error.is::<CredentialError>())
        {
            return result;
        }
        let entry = Entry {
            version: 1,
            updated_at: SystemTime::now(),
            fingerprint: fingerprint.to_string(),
            result: result.map_err(|error| format!("{error:#}")),
        };
        if let Err(error) = self.write(key, &entry) {
            self.diagnostics
                .warn(format!("ai-usage: could not save usage cache: {error:#}"));
        }
        entry.into_result()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Usage, Window, WindowKind};
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "ai-usage-shared-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn cache(&self, refresh: bool) -> Cache {
            Cache::at(self.0.join("cache"), Duration::from_secs(120), refresh)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn rows() -> Vec<UsageRow> {
        UsageRow::single(Usage {
            plan: Some("Test plan".to_string()),
            ..Usage::default()
        })
    }

    fn entry(updated_at: SystemTime, error: bool) -> Entry {
        Entry {
            version: 1,
            fingerprint: "auth".to_string(),
            updated_at,
            result: if error {
                Err("temporarily unavailable".to_string())
            } else {
                Ok(rows())
            },
        }
    }

    #[test]
    fn freshness_boundaries_future_times_and_refresh_are_explicit() {
        let dir = TestDir::new();
        let mut cache = dir.cache(false);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        cache.requested_at = now;
        assert!(entry(now - Duration::from_secs(119), false).usable(&cache, now));
        assert!(!entry(now - Duration::from_secs(120), false).usable(&cache, now));
        assert!(entry(now - Duration::from_secs(14), true).usable(&cache, now));
        assert!(!entry(now - Duration::from_secs(15), true).usable(&cache, now));
        assert!(!entry(now + Duration::from_secs(1), false).usable(&cache, now));
        cache.refresh = true;
        assert!(!entry(now - Duration::from_nanos(1), false).usable(&cache, now));
        assert!(entry(now, false).usable(&cache, now));
        cache.ttl = Duration::ZERO;
        cache.refresh = false;
        assert!(!entry(now - Duration::from_nanos(1), false).usable(&cache, now));
        assert!(entry(now, false).usable(&cache, now));
    }

    #[test]
    fn passing_a_quota_reset_invalidates_a_fresh_entry() {
        let dir = TestDir::new();
        let mut cache = dir.cache(false);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        cache.requested_at = now;
        let mut data = entry(now - Duration::from_secs(10), false);
        data.result.as_mut().unwrap()[0].usage.long = Some(Window {
            kind: WindowKind::Weekly,
            used_percent: Some(100.0),
            resets_at: Some((now - Duration::from_secs(1)).into()),
        });
        assert!(!data.usable(&cache, now));
    }

    #[tokio::test]
    async fn cache_hits_do_not_poll_the_fetch_and_auth_changes_invalidate() {
        let dir = TestDir::new();
        let cache = dir.cache(false);
        cache
            .fetch("source", "auth", async { Ok(rows()) })
            .await
            .unwrap();
        let hit = cache
            .request()
            .fetch("source", "auth", async { panic!("cache hit fetched") })
            .await
            .unwrap();
        assert_eq!(hit[0].usage.plan.as_deref(), Some("Test plan"));
        let error = cache
            .request()
            .fetch("source", "new-auth", async { bail!("auth changed") })
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "auth changed");
        assert_eq!(
            cache
                .request()
                .fetch("source", "new-auth", async {
                    panic!("error cache fetched")
                })
                .await
                .unwrap_err()
                .to_string(),
            "auth changed"
        );
        let persisted = fs::read_to_string(cache.root.join("source.json")).unwrap();
        assert!(!persisted.contains("Test plan"));
        assert_eq!(
            fs::metadata(&cache.root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for name in ["source.json", "source.lock"] {
            assert_eq!(
                fs::metadata(cache.root.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn expired_corrupt_and_unknown_version_entries_are_replaced() {
        let dir = TestDir::new();
        let cache = dir.cache(false);
        let _guard = cache.lock("source").await.unwrap();
        drop(_guard);
        for content in [
            String::new(),
            "{broken".to_string(),
            {
                let mut old = entry(SystemTime::now() - Duration::from_secs(120), false);
                old.version = 1;
                serde_json::to_string(&old).unwrap()
            },
            {
                let mut future = entry(SystemTime::now(), false);
                future.version = 2;
                serde_json::to_string(&future).unwrap()
            },
        ] {
            fs::write(cache.root.join("source.json"), content).unwrap();
            cache
                .request()
                .fetch("source", "auth", async { Ok(rows()) })
                .await
                .unwrap();
            assert!(cache.read("source", "auth").is_some());
        }
    }

    #[tokio::test]
    async fn cache_write_failures_keep_the_result_and_capture_a_warning() {
        let dir = TestDir::new();
        let mut cache = dir.cache(false);
        cache.diagnostics = Diagnostics::new(true);
        drop(cache.lock("source").await.unwrap());
        fs::create_dir(cache.root.join("source.json")).unwrap();
        let result = cache
            .fetch("source", "auth", async { Ok(rows()) })
            .await
            .unwrap();
        assert_eq!(result[0].usage.plan.as_deref(), Some("Test plan"));
        let warnings = cache.diagnostics.take();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("could not save usage cache"));
        assert!(!cache.root.join("source.tmp").exists());
    }

    #[tokio::test]
    async fn credential_errors_are_not_cached_even_through_context() {
        let dir = TestDir::new();
        let cache = dir.cache(false);
        let error = cache
            .fetch("source", "auth", async {
                Err(
                    anyhow::Error::new(CredentialError("keychain denied".into()))
                        .context("loading browser credentials"),
                )
            })
            .await
            .unwrap_err();
        assert!(error.is::<CredentialError>());
        assert!(!cache.root.join("source.json").exists());
        let result = cache
            .request()
            .fetch("source", "auth", async { Ok(rows()) })
            .await
            .unwrap();
        assert_eq!(result[0].usage.plan.as_deref(), Some("Test plan"));
    }

    #[tokio::test]
    async fn unusable_cache_directories_and_locks_never_fetch_without_a_lock() {
        let dir = TestDir::new();
        let file_root = dir.0.join("file");
        fs::write(&file_root, "existing file").unwrap();
        let linked_root = dir.0.join("link");
        let link_target = dir.0.join("target");
        fs::create_dir(&link_target).unwrap();
        std::os::unix::fs::symlink(&link_target, &linked_root).unwrap();
        let blocked_lock = dir.0.join("blocked");
        fs::create_dir_all(blocked_lock.join("source.lock")).unwrap();

        for root in [file_root, linked_root, blocked_lock] {
            let cache = Cache::at(root, Duration::from_secs(120), false);
            assert!(
                cache
                    .fetch("source", "auth", async {
                        panic!("lock failure must not trigger a fetch")
                    })
                    .await
                    .is_err()
            );
        }
        assert_eq!(
            fs::read_to_string(dir.0.join("file")).unwrap(),
            "existing file"
        );
        assert!(fs::read_dir(link_target).unwrap().next().is_none());
    }

    fn spawn_worker(dir: &TestDir, mode: &str, refresh: bool) -> Child {
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cache::tests::process_worker", "--nocapture"])
            .env("AI_USAGE_TEST_CACHE", &dir.0)
            .env("AI_USAGE_TEST_MODE", mode)
            .env("AI_USAGE_TEST_REFRESH", if refresh { "1" } else { "0" })
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap()
    }

    fn wait_for(path: &Path) {
        let until = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            assert!(Instant::now() < until, "waiting for {}", path.display());
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    // 通常のテスト実行では何もしない。同じテストバイナリを子として起動し、
    // 本物の別プロセスから同じロックとキャッシュを使う。
    #[tokio::test]
    async fn process_worker() {
        let Some(root) = std::env::var_os("AI_USAGE_TEST_CACHE") else {
            return;
        };
        let root = PathBuf::from(root);
        let refresh = std::env::var("AI_USAGE_TEST_REFRESH").unwrap() == "1";
        let cache = Cache::at(root.join("cache"), Duration::from_secs(120), refresh);
        fs::write(root.join(format!("ready-{}", std::process::id())), "ready").unwrap();
        while !root.join("go").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mode = std::env::var("AI_USAGE_TEST_MODE").unwrap();
        let result = cache
            .fetch("source", "auth", async {
                let mut count = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(root.join("calls"))
                    .unwrap();
                writeln!(count, "fetch").unwrap();
                fs::write(root.join("entered"), "entered").unwrap();
                if mode == "hang" {
                    std::future::pending::<()>().await;
                }
                tokio::time::sleep(Duration::from_millis(150)).await;
                if mode == "error" {
                    bail!("shared failure");
                }
                Ok(rows())
            })
            .await;
        if mode == "error" {
            assert_eq!(result.unwrap_err().to_string(), "shared failure");
        } else {
            assert_eq!(result.unwrap()[0].usage.plan.as_deref(), Some("Test plan"));
        }
    }

    #[test]
    fn multiple_processes_share_cold_expired_refresh_and_failed_fetches() {
        for (refresh, mode, seeded) in [
            (false, "ok", false),
            (false, "ok", true),
            (true, "ok", true),
            (false, "error", false),
        ] {
            let dir = TestDir::new();
            if seeded {
                let cache = dir.cache(false);
                let runtime = tokio::runtime::Runtime::new().unwrap();
                runtime.block_on(async {
                    let _guard = cache.lock("source").await.unwrap();
                    let when = if refresh {
                        SystemTime::now()
                    } else {
                        SystemTime::now() - Duration::from_secs(120)
                    };
                    cache.write("source", &entry(when, false)).unwrap();
                });
            }
            let mut children: Vec<_> = (0..6).map(|_| spawn_worker(&dir, mode, refresh)).collect();
            for child in &children {
                wait_for(&dir.0.join(format!("ready-{}", child.id())));
            }
            fs::write(dir.0.join("go"), "go").unwrap();
            for child in &mut children {
                assert!(child.wait().unwrap().success());
            }
            assert_eq!(
                fs::read_to_string(dir.0.join("calls"))
                    .unwrap()
                    .lines()
                    .count(),
                1,
                "{refresh} {mode} {seeded}"
            );
        }
    }

    #[tokio::test]
    async fn killed_updater_releases_its_lock() {
        let dir = TestDir::new();
        fs::write(dir.0.join("go"), "go").unwrap();
        let mut child = spawn_worker(&dir, "hang", false);
        wait_for(&dir.0.join("entered"));
        child.kill().unwrap();
        child.wait().unwrap();
        let cache = dir.cache(false);
        tokio::time::timeout(
            Duration::from_secs(5),
            cache.fetch("source", "auth", async { Ok(rows()) }),
        )
        .await
        .unwrap()
        .unwrap();
    }

    #[test]
    fn source_keys_and_fingerprints_are_stable_and_separate() {
        let dir = TestDir::new();
        let source = dir.0.join("auth.json");
        fs::write(&source, "test credentials").unwrap();
        let link = dir.0.join("link.json");
        std::os::unix::fs::symlink(&source, &link).unwrap();
        assert_eq!(key(Provider::Grok, &source), key(Provider::Grok, &link));
        assert_ne!(
            key(Provider::Grok, &source),
            key(Provider::Antigravity, &source)
        );
        let old = file_fingerprint(&source);
        fs::write(&source, "changed credentials").unwrap();
        assert_ne!(old, file_fingerprint(&source));
    }
}
