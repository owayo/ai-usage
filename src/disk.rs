//! TUI の下部に表示する、この Mac のディスク容量。
//!
//! 容量は Finder と同じ基準で数える。`available` は重要な用途に使える容量で、
//! システムが必要に応じて消すパージ可能な領域を含む。今すぐ書ける空き (`free`) との
//! 差を、パージ可能な量の推定として扱う。
//!
//! パージ可能な量の問い合わせは 1 回 40〜120ms かかり、壊れかけのディスクでは戻らない
//! こともある。そのため取得は描画から切り離し、対象ごとに 1 本までの切り離しスレッドで
//! 行う。tokio の blocking pool は終了時に作業の完了を待つため、ここでは使わない。

#[cfg(target_os = "macos")]
mod macos;

use std::fmt;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

/// 取得が終わってから、同じボリュームの次の取得を始めるまでの間隔。
pub const INTERVAL: Duration = Duration::from_secs(30);
/// これより長く戻らない取得は「応答なし」とする。TUI は表示を切り替えるだけで取得を
/// 待ち続け、`--disk` はこの時間で待つのをやめて出力する。
pub const OVERDUE: Duration = Duration::from_secs(2);

/// 表示するボリューム。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// 起動ディスク (`/`)。
    Startup,
    /// 設定で追加したローカルボリュームのマウントポイント。
    Mount(PathBuf),
}

impl Target {
    /// 取得前、またはボリューム名が取れないときの表示名。
    pub fn label(&self) -> String {
        match self {
            Target::Startup => "/".to_string(),
            Target::Mount(path) => path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            ),
        }
    }
}

/// 起動ディスクに、設定で追加したマウントポイントを続ける。
///
/// 末尾の `/` や重複した区切りは取り除いて比べ、重複と `/` (起動ディスクとして表示済み)
/// は除く。
pub fn targets(mounts: &[String]) -> Vec<Target> {
    let mut targets = vec![Target::Startup];
    push_mounts(&mut targets, mounts);
    targets
}

/// `--disk --mount` で指定したボリュームだけを、指定した順に並べる。`/` は起動ディスクとし、
/// 正規化した後で重複するものは最初の 1 つだけを残す。
pub fn listed_targets(mounts: &[String]) -> Vec<Target> {
    let mut targets = Vec::new();
    push_mounts(&mut targets, mounts);
    targets
}

/// マウントポイントを `targets` に加える。先頭の `~/` を展開し、末尾の `/` や重複した区切りを
/// 取り除いて比べる。`/` は起動ディスク、空のパスは無視し、既にある対象は加えない。
fn push_mounts(targets: &mut Vec<Target>, mounts: &[String]) {
    for mount in mounts {
        let path: PathBuf = crate::config::expand_home(mount).components().collect();
        let target = if path == Path::new("/") {
            Target::Startup
        } else if path.as_os_str().is_empty() {
            continue;
        } else {
            Target::Mount(path)
        };
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
}

/// 1 ボリュームの容量。値はバイト。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    /// macOS が返すボリューム名 (例: `Macintosh HD`)。
    pub name: String,
    pub total: u64,
    /// 今すぐ書き込める空き。
    pub free: u64,
    /// Finder の「利用可能」。パージ可能な領域を含み、`free` 以上。
    pub available: u64,
    /// `available - free`。重要な用途の容量を返さないボリュームでは不明。
    pub purgeable: Option<u64>,
}

impl Volume {
    /// 総容量・空き・重要な用途に使える容量から、表示に使う値を組み立てる。
    pub fn new(
        name: String,
        total: u64,
        free: u64,
        important: Option<u64>,
    ) -> Result<Self, String> {
        if total == 0 {
            return Err("no capacity reported".into());
        }
        if free > total {
            return Err("free space exceeds the capacity".into());
        }
        // 重要な用途の容量に対応しないボリューム (Time Machine の外付けなど) は 0 を返す。
        // 空きがあるのに 0、または総容量を超える値は、不明として通常の空きで代える。
        let important = important.filter(|&value| value <= total && (value > 0 || free == 0));
        let (available, purgeable) = match important {
            // 2 つの値は別々に測られるため、ずれて free を下回っても free に揃える。
            Some(value) => {
                let available = value.max(free);
                (available, Some(available - free))
            }
            None => (free, None),
        };
        Ok(Self {
            name,
            total,
            free,
            available,
            purgeable,
        })
    }

    /// 使用中の容量。パージ可能な領域は含めない (Finder と同じ)。
    pub fn used(&self) -> u64 {
        self.total - self.available
    }

    pub fn used_percent(&self) -> f64 {
        self.used() as f64 * 100.0 / self.total as f64
    }
}

/// 容量を取得できなかった理由。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// マウントポイントとして見つからない。抜いたディスクの古い値は表示しない。
    NotMounted,
    /// ネットワークボリューム。応答しない共有で止まらないよう、問い合わせない。
    NotLocal,
    /// 問い合わせの失敗。前回の値は経過時間を付けて残す。
    Failed(String),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::NotMounted => f.write_str("not mounted"),
            ReadError::NotLocal => f.write_str("not a local volume"),
            ReadError::Failed(message) => f.write_str(message),
        }
    }
}

/// 対象の容量を読む。ネットワークボリュームには問い合わせない。
#[cfg(target_os = "macos")]
pub fn read(target: &Target) -> Result<Volume, ReadError> {
    macos::read(target)
}

#[cfg(not(target_os = "macos"))]
pub fn read(_target: &Target) -> Result<Volume, ReadError> {
    Err(ReadError::Failed(
        "disk usage is only available on macOS".into(),
    ))
}

/// 1 ボリュームの表示状態。
#[derive(Debug)]
pub struct Row {
    pub target: Target,
    /// 最後に取得できた値と、その取得を反映した時刻。
    pub last: Option<(Volume, Instant)>,
    /// 直近の取得が失敗した理由。成功すると消える。
    pub error: Option<ReadError>,
    /// 取得中なら、その開始時刻。
    pub started: Option<Instant>,
    next: Instant,
}

impl Row {
    pub fn new(target: Target, now: Instant) -> Self {
        Self {
            target,
            last: None,
            error: None,
            started: None,
            next: now,
        }
    }

    /// 取得を始めてから `OVERDUE` を過ぎても戻らない。
    pub fn overdue(&self, now: Instant) -> bool {
        self.started
            .is_some_and(|started| now.saturating_duration_since(started) >= OVERDUE)
    }
}

type Reader = Arc<dyn Fn(&Target) -> Result<Volume, ReadError> + Send + Sync>;
type Outcome = (usize, Result<Volume, ReadError>);

/// `read` を切り離しスレッドで呼び、結果を添字付きで `sender` に送る。JoinHandle は捨てるので、
/// 呼び出し側が終わるときにこのスレッドを待たない。
///
/// `read` が panic しても失敗として送る。送らずに終わると、受け取る側からは戻らない取得と
/// 区別がつかない。
fn spawn_reader(
    index: usize,
    target: Target,
    read: Reader,
    sender: Sender<Outcome>,
) -> io::Result<()> {
    std::thread::Builder::new()
        .name("ai-usage-disk".into())
        .spawn(move || {
            // panic した後に `read` や `target` は使わず、スレッドはそのまま終わる。
            let result = panic::catch_unwind(AssertUnwindSafe(|| read(&target)))
                .unwrap_or_else(|_| Err(ReadError::Failed("the disk reader panicked".into())));
            // 受け取る側が先に終わっていれば、結果は捨てる。
            let _ = sender.send((index, result));
        })
        .map(drop)
}

/// 読み取りのスレッドを起動できなかったときの失敗。
fn spawn_failed(error: &io::Error) -> ReadError {
    ReadError::Failed(format!("cannot start the disk reader: {error}"))
}

/// 対象ごとに切り離しスレッドで容量を取得し、結果を行に反映する。
pub struct Monitor {
    rows: Vec<Row>,
    read: Reader,
    sender: Sender<Outcome>,
    receiver: Receiver<Outcome>,
}

impl Monitor {
    pub fn new(targets: Vec<Target>, now: Instant) -> Self {
        Self::with_reader(targets, now, Arc::new(read))
    }

    fn with_reader(targets: Vec<Target>, now: Instant, read: Reader) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            rows: targets
                .into_iter()
                .map(|target| Row::new(target, now))
                .collect(),
            read,
            sender,
            receiver,
        }
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// 終わった取得を反映し、期限の来たボリュームの取得を始める。表示が変わり得るなら true。
    ///
    /// 取得中のボリュームは、期限を過ぎても次の取得を始めない。戻らない取得があっても、
    /// スレッドはボリュームごとに 1 本までしか増えない。
    pub fn poll(&mut self, now: Instant) -> bool {
        let mut changed = false;
        while let Ok((index, result)) = self.receiver.try_recv() {
            let row = &mut self.rows[index];
            row.started = None;
            row.next = now + INTERVAL;
            match result {
                Ok(volume) => {
                    row.last = Some((volume, now));
                    row.error = None;
                }
                Err(error) => {
                    if !matches!(error, ReadError::Failed(_)) {
                        row.last = None;
                    }
                    row.error = Some(error);
                }
            }
            changed = true;
        }
        for (index, row) in self.rows.iter_mut().enumerate() {
            if row.started.is_some() || now < row.next {
                continue;
            }
            // TUI の終了時に、このスレッドを待たない。
            let spawned = spawn_reader(
                index,
                row.target.clone(),
                Arc::clone(&self.read),
                self.sender.clone(),
            );
            match spawned {
                Ok(()) => row.started = Some(now),
                Err(error) => {
                    row.error = Some(spawn_failed(&error));
                    row.next = now + INTERVAL;
                }
            }
            changed = true;
        }
        changed
    }
}

/// `snapshot` で 1 回読んだ結果。
#[derive(Debug)]
pub struct Reading {
    pub target: Target,
    /// 読めた容量か、読めなかった理由。期限までに結果が届かなければ None。
    pub result: Option<Result<Volume, ReadError>>,
}

impl Reading {
    /// 表示名。容量を読めたらボリューム名、読めなければ対象の名前。
    pub fn name(&self) -> String {
        match &self.result {
            Some(Ok(volume)) => volume.name.clone(),
            _ => self.target.label(),
        }
    }
}

/// 各ボリュームを切り離しスレッドで並行して 1 回ずつ読み、`wait` の間だけ結果を待つ。
///
/// 期限までに戻らなかったボリュームは `result` が None のまま返す。そのスレッドは待たないので、
/// 戻らない I/O があってもプロセスは終われる。全部そろえば期限を待たずに返す。結果は
/// `targets` と同じ順に並べる。
pub fn snapshot(targets: Vec<Target>, wait: Duration) -> Vec<Reading> {
    snapshot_with(targets, wait, Arc::new(read))
}

fn snapshot_with(targets: Vec<Target>, wait: Duration, read: Reader) -> Vec<Reading> {
    // 期限は全ボリュームで 1 つ。1 件ずつ `wait` まで待つと、待ち時間が件数倍に延びる。
    let deadline = Instant::now() + wait;
    let (sender, receiver) = mpsc::channel();
    let mut readings: Vec<Reading> = targets
        .into_iter()
        .map(|target| Reading {
            target,
            result: None,
        })
        .collect();
    let mut pending = 0;
    for (index, reading) in readings.iter_mut().enumerate() {
        let spawned = spawn_reader(
            index,
            reading.target.clone(),
            Arc::clone(&read),
            sender.clone(),
        );
        match spawned {
            Ok(()) => pending += 1,
            Err(error) => reading.result = Some(Err(spawn_failed(&error))),
        }
    }
    // 送る側を読み取りスレッドだけにして、全員が終われば Disconnected で抜けられるようにする。
    drop(sender);
    while pending > 0 {
        let left = deadline.saturating_duration_since(Instant::now());
        // 期限を過ぎた (Timeout)、またはすべてのスレッドが送らずに終わった (Disconnected)。
        let Ok((index, result)) = receiver.recv_timeout(left) else {
            break;
        };
        readings[index].result = Some(result);
        pending -= 1;
    }
    readings
}

#[cfg(test)]
impl Monitor {
    /// スレッドを起動せずに、描画テスト用の状態を持たせる。
    pub fn from_rows(rows: Vec<Row>) -> Self {
        let mut monitor = Self::with_reader(
            Vec::new(),
            Instant::now(),
            Arc::new(|_: &Target| -> Result<Volume, ReadError> { unreachable!() }),
        );
        monitor.rows = rows;
        monitor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const GB: u64 = 1_000_000_000;

    #[test]
    fn capacity_uses_important_usage_and_estimates_purgeable_space() {
        // 実機の起動ディスクで測った値。Finder 基準で 86% 使用、57.7GB がパージ可能。
        let volume = Volume::new(
            "Macintosh HD".into(),
            494_384_795_648,
            13_080_645_632,
            Some(70_754_759_808),
        )
        .unwrap();
        assert_eq!(volume.available, 70_754_759_808);
        assert_eq!(volume.purgeable, Some(57_674_114_176));
        assert_eq!(volume.used(), 423_630_035_840);
        assert!((volume.used_percent() - 85.69).abs() < 0.01);
    }

    #[test]
    fn capacity_falls_back_to_free_space_when_important_usage_is_unusable() {
        // 未対応 (0)、未取得、総容量超えは不明として扱い、0 GB のパージ可能量にしない。
        for important in [Some(0), None, Some(600 * GB)] {
            let volume = Volume::new("Backup".into(), 500 * GB, 52 * GB, important).unwrap();
            assert_eq!(volume.available, 52 * GB, "{important:?}");
            assert_eq!(volume.purgeable, None, "{important:?}");
        }
        // 満杯のボリュームが返す 0 は、そのまま使ってよい。
        let full = Volume::new("Full".into(), 500 * GB, 0, Some(0)).unwrap();
        assert_eq!((full.available, full.purgeable), (0, Some(0)));
        assert_eq!(full.used_percent(), 100.0);
        // 別々に測った値がずれて free を下回っても、利用可能は free 以上に揃える。
        let skewed = Volume::new("Skewed".into(), 500 * GB, 20 * GB, Some(19 * GB)).unwrap();
        assert_eq!((skewed.available, skewed.purgeable), (20 * GB, Some(0)));
    }

    #[test]
    fn capacity_rejects_inconsistent_values() {
        assert!(Volume::new("Zero".into(), 0, 0, None).is_err());
        assert!(Volume::new("Over".into(), 10 * GB, 11 * GB, None).is_err());
    }

    #[test]
    fn targets_put_the_startup_disk_first_and_drop_duplicates() {
        let mounts = [
            "/Volumes/Archive/",
            "/",
            "/Volumes//Archive",
            "",
            "/Volumes/Photos",
        ]
        .map(String::from);
        assert_eq!(
            targets(&mounts),
            [
                Target::Startup,
                Target::Mount("/Volumes/Archive".into()),
                Target::Mount("/Volumes/Photos".into()),
            ]
        );
        assert_eq!(targets(&[]), [Target::Startup]);
        assert_eq!(Target::Startup.label(), "/");
        assert_eq!(Target::Mount("/Volumes/Archive".into()).label(), "Archive");
    }

    #[test]
    fn listed_targets_keep_the_given_order_and_treat_root_as_the_startup_disk() {
        let mounts = [
            "/Volumes/Archive/",
            "//",
            "/Volumes//Archive",
            "/Volumes/Photos",
            "/.",
        ]
        .map(String::from);
        // 起動ディスクを先頭に足さず、書いた順のまま、正規化して重複を除く。
        assert_eq!(
            listed_targets(&mounts),
            [
                Target::Mount("/Volumes/Archive".into()),
                Target::Startup,
                Target::Mount("/Volumes/Photos".into()),
            ]
        );
        assert_eq!(listed_targets(&["/".into()]), [Target::Startup]);
        assert!(listed_targets(&[]).is_empty());
        assert!(listed_targets(&[String::new()]).is_empty());
        // `~/` は設定と同じく展開する。
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            listed_targets(&["~/Mounted/".into()]),
            [Target::Mount(home.join("Mounted"))]
        );
    }

    fn fixed_volume(target: &Target) -> Result<Volume, ReadError> {
        let name = match target {
            Target::Startup => "Macintosh HD".to_string(),
            Target::Mount(_) => target.label(),
        };
        Volume::new(name, 100, 40, Some(60)).map_err(ReadError::Failed)
    }

    #[test]
    fn snapshot_returns_at_one_shared_deadline_without_waiting_for_stuck_reads() {
        const WAIT: Duration = Duration::from_millis(250);
        let (release, gate) = mpsc::channel::<()>();
        let gate = Arc::new(Mutex::new(gate));
        let stuck = |n: u8| Target::Mount(format!("/Volumes/Stuck{n}").into());
        let is_stuck = |target: &Target| target.label().starts_with("Stuck");
        let targets = vec![
            stuck(1),
            Target::Startup,
            stuck(2),
            stuck(3),
            Target::Mount("/Volumes/Archive".into()),
            stuck(4),
        ];
        let start = Instant::now();
        let readings = snapshot_with(
            targets.clone(),
            WAIT,
            Arc::new(move |target: &Target| {
                if is_stuck(target) {
                    // 応答しないディスクを、テストが解放するまで戻らない取得で再現する。
                    let _ = gate.lock().unwrap().recv();
                    return Err(ReadError::Failed("released".into()));
                }
                fixed_volume(target)
            }),
        );
        let elapsed = start.elapsed();
        // 期限まで待ち、戻らない取得を待たずに返る。期限は全体で 1 つなので、戻らない取得が
        // 4 件あっても待ち時間は 4 倍に延びない。
        assert!(elapsed >= WAIT, "{elapsed:?}");
        assert!(elapsed < WAIT * 4, "{elapsed:?}");
        // 結果は完了順ではなく、対象の順に並ぶ。
        let order: Vec<_> = readings.iter().map(|reading| &reading.target).collect();
        assert_eq!(order, targets.iter().collect::<Vec<_>>());
        for reading in readings.iter().filter(|reading| is_stuck(&reading.target)) {
            assert_eq!(reading.result, None, "{reading:?}");
        }
        assert_eq!(readings[0].name(), "Stuck1");
        assert_eq!(readings[1].result, Some(fixed_volume(&Target::Startup)));
        assert_eq!(readings[1].name(), "Macintosh HD");
        assert_eq!(readings[4].name(), "Archive");
        assert!(matches!(readings[4].result, Some(Ok(_))));
        // 戻らなかった取得を終わらせる。
        drop(release);
    }

    #[test]
    fn snapshot_returns_as_soon_as_every_volume_has_answered() {
        let start = Instant::now();
        let readings = snapshot_with(
            vec![Target::Startup, Target::Mount("/Volumes/Archive".into())],
            Duration::from_secs(60),
            Arc::new(|target: &Target| match target {
                Target::Startup => fixed_volume(target),
                Target::Mount(_) => Err(ReadError::NotMounted),
            }),
        );
        assert!(start.elapsed() < Duration::from_secs(10));
        assert_eq!(readings[0].result, Some(fixed_volume(&Target::Startup)));
        assert_eq!(readings[1].result, Some(Err(ReadError::NotMounted)));
        assert_eq!(readings[1].name(), "Archive");

        // 対象が無ければ、何も待たずに返る。
        let start = Instant::now();
        assert!(snapshot_with(Vec::new(), Duration::from_secs(60), Arc::new(read)).is_empty());
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn snapshot_reports_a_panicking_reader_as_a_failure_instead_of_a_timeout() {
        let readings = snapshot_with(
            vec![Target::Startup, Target::Mount("/Volumes/Archive".into())],
            Duration::from_secs(60),
            Arc::new(|target: &Target| match target {
                Target::Startup => fixed_volume(target),
                Target::Mount(_) => panic!("reader bug"),
            }),
        );
        assert!(matches!(readings[0].result, Some(Ok(_))));
        assert_eq!(
            readings[1].result,
            Some(Err(ReadError::Failed("the disk reader panicked".into())))
        );
    }

    fn wait_for(monitor: &mut Monitor, done: impl Fn(&Monitor) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(monitor) {
            assert!(
                Instant::now() < deadline,
                "the reader thread did not report back"
            );
            std::thread::sleep(Duration::from_millis(1));
            monitor.poll(Instant::now());
        }
    }

    #[test]
    fn monitor_reads_each_target_and_waits_the_interval_after_completion() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let start = Instant::now();
        let mut monitor = Monitor::with_reader(
            vec![Target::Startup, Target::Mount("/Volumes/Archive".into())],
            start,
            Arc::new(move |target: &Target| {
                counter.fetch_add(1, Ordering::SeqCst);
                match target {
                    Target::Startup => Volume::new("Macintosh HD".into(), 100, 40, Some(60)),
                    Target::Mount(_) => return Err(ReadError::NotMounted),
                }
                .map_err(ReadError::Failed)
            }),
        );
        assert!(monitor.poll(start));
        assert!(monitor.rows().iter().all(|row| row.started.is_some()));
        wait_for(&mut monitor, |monitor| {
            monitor.rows().iter().all(|row| row.started.is_none())
        });
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let rows = monitor.rows();
        assert_eq!(rows[0].last.as_ref().unwrap().0.available, 60);
        assert_eq!(rows[0].error, None);
        assert!(rows[1].last.is_none());
        assert_eq!(rows[1].error, Some(ReadError::NotMounted));

        // 次の取得は、完了を反映してから INTERVAL 後まで始めない。
        assert!(rows.iter().all(|row| row.next >= start + INTERVAL));
        let earliest = rows.iter().map(|row| row.next).min().unwrap();
        let latest = rows.iter().map(|row| row.next).max().unwrap();
        assert!(!monitor.poll(earliest - Duration::from_millis(1)));
        assert!(monitor.rows().iter().all(|row| row.started.is_none()));
        assert!(monitor.poll(latest));
        assert!(monitor.rows().iter().all(|row| row.started.is_some()));
    }

    #[test]
    fn monitor_keeps_one_reader_per_target_while_a_read_hangs() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (release, gate) = mpsc::channel::<()>();
        let gate = Arc::new(Mutex::new(gate));
        let counter = Arc::clone(&calls);
        let start = Instant::now();
        let mut monitor = Monitor::with_reader(
            vec![Target::Startup],
            start,
            Arc::new(move |_: &Target| {
                counter.fetch_add(1, Ordering::SeqCst);
                // 応答しないディスクを、テストが解放するまで戻らない取得で再現する。
                let _ = gate.lock().unwrap().recv();
                Err(ReadError::Failed("I/O error".into()))
            }),
        );
        monitor.poll(start);
        for minutes in 1..=5 {
            monitor.poll(start + Duration::from_secs(60 * minutes));
        }
        let row = &monitor.rows()[0];
        assert!(row.overdue(start + OVERDUE));
        assert!(!row.overdue(start + OVERDUE - Duration::from_millis(1)));
        release.send(()).unwrap();
        wait_for(&mut monitor, |monitor| monitor.rows()[0].started.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            monitor.rows()[0].error,
            Some(ReadError::Failed("I/O error".into()))
        );
    }

    #[test]
    fn a_panicking_reader_counts_as_a_failed_read_and_is_retried() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let start = Instant::now();
        let mut monitor = Monitor::with_reader(
            vec![Target::Startup],
            start,
            Arc::new(move |_: &Target| -> Result<Volume, ReadError> {
                counter.fetch_add(1, Ordering::SeqCst);
                panic!("reader bug");
            }),
        );
        monitor.poll(start);
        wait_for(&mut monitor, |monitor| monitor.rows()[0].started.is_none());
        // 取得中のまま残らず、ほかの失敗と同じく INTERVAL 後に読み直す。
        let row = &monitor.rows()[0];
        assert_eq!(
            row.error,
            Some(ReadError::Failed("the disk reader panicked".into()))
        );
        let next = row.next;
        assert!(monitor.poll(next));
        assert!(monitor.rows()[0].started.is_some());
        wait_for(&mut monitor, |monitor| monitor.rows()[0].started.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failed_reads_keep_the_last_values_but_unmounted_disks_drop_them() {
        let start = Instant::now();
        let volume = Volume::new("Archive".into(), 100, 50, None).unwrap();
        for (error, keeps) in [
            (ReadError::Failed("timeout".into()), true),
            (ReadError::NotMounted, false),
            (ReadError::NotLocal, false),
        ] {
            let mut monitor = Monitor::from_rows(vec![Row {
                target: Target::Mount("/Volumes/Archive".into()),
                last: Some((volume.clone(), start)),
                error: None,
                started: Some(start),
                next: start,
            }]);
            monitor.sender.send((0, Err(error.clone()))).unwrap();
            assert!(monitor.poll(start));
            let row = &monitor.rows()[0];
            assert_eq!(row.last.is_some(), keeps, "{error:?}");
            assert_eq!(row.error.as_ref(), Some(&error));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reads_the_startup_disk() {
        let volume = read(&Target::Startup).unwrap();
        assert!(!volume.name.is_empty());
        assert!(volume.total > 0);
        assert!(volume.free <= volume.available && volume.available <= volume.total);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unmounted_paths_are_not_queried() {
        let path = std::env::temp_dir().join(format!("ai-usage-disk-{}", std::process::id()));
        assert_eq!(read(&Target::Mount(path)), Err(ReadError::NotMounted));
        // 起動ディスク上の普通のディレクトリも、マウントポイントではない。
        assert_eq!(
            read(&Target::Mount("/usr/bin".into())),
            Err(ReadError::NotMounted)
        );
    }
}
