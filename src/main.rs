//! Chrome プロファイルと CLI の OAuth 情報から、Claude / Codex / Antigravity /
//! PixelLab / Grok の利用枠とリセット時刻を表示する。

mod antigravity;
mod claude;
mod codex;
mod config;
mod cookies;
mod grok;
mod http;
mod jwt;
mod model;
mod pixellab;
mod profiles;
mod render;
mod report;
mod sort;

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use clap::{Parser, ValueEnum};

use config::BrowserWants;
use model::{AccountReport, Provider, UsageRow};
use profiles::Profile;
// render 層と共有する(`use crate::SortKey`)ため、クレートルートに再エクスポートする。
pub use sort::SortKey;

#[derive(Parser)]
#[command(
    name = "ai-usage",
    version,
    about = "Show Claude, Codex, Antigravity, PixelLab, and Grok usage limits"
)]
struct Cli {
    /// 対象を指定した Chrome profile 表示名に絞る(例: -p Work,Home)
    #[arg(short, long, value_delimiter = ',')]
    profile: Vec<String>,

    /// 対象を単一 provider に絞る
    #[arg(long, value_enum)]
    only: Option<ProviderArg>,

    /// table ではなく JSON を出力する
    #[arg(long)]
    json: bool,

    /// 対話式の端末画面で利用枠を表示する
    #[arg(long, conflicts_with_all = ["json", "statusline"])]
    tui: bool,

    /// compact な colored statusline を出力する(1 account 1 行)
    #[arg(long)]
    statusline: bool,

    /// statusline の provider ラベルを brand-logo glyph に置き換える
    /// (BrandLogos font が必要。github.com/owayo/brand-logo-font を参照)
    #[arg(long)]
    logos: bool,

    /// `--statusline` / `--tui` と併用し、network fetch の代わりにこの JSON file
    /// (cached `--json` output)から account を読む。statusline の高速描画で使う
    /// (`--statusline` / `--tui` なしでは無視され、通常どおり fetch する)。
    #[arg(long, value_name = "PATH")]
    input: Option<PathBuf>,

    /// statusline で active として highlight する account email
    /// (default: CLAUDE_CONFIG_DIR/.claude.json から読む)
    #[arg(long, value_name = "EMAIL")]
    active_email: Option<String>,

    /// statusline でこの profile 名に一致する account を active として highlight する
    /// `accounts[].profile` に対して case-insensitive に照合し、--active-email と
    /// .claude.json fallback より優先する。ログイン email ではなく profile で
    /// account を指定する tool 向け。
    #[arg(long, value_name = "NAME")]
    active_profile: Option<String>,

    /// statusline の --active-profile と併用し、highlight 対象をこの provider 行に限定する
    /// 未指定なら一致 profile の Claude 行を highlight する。
    #[arg(long, value_enum)]
    active_provider: Option<ProviderArg>,

    /// ANSI color を無効化する
    #[arg(long)]
    no_color: bool,

    /// ~/.config/ai-usage/config.toml の代わりにこの config file を使う
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// 現在 signed-in 済みの profile から starter config を生成して終了する
    #[arg(long)]
    init_config: bool,

    /// 検出した Chrome profile を一覧表示して終了する
    #[arg(long)]
    list_profiles: bool,

    /// statusline の active account 解決結果と行ごとの match 判定を stderr に出す
    /// (stdout は汚さない)。行が active になる/ならない理由の診断用。
    #[arg(long)]
    debug: bool,

    /// statusline の gauge 幅を狭い pane 向けに半分にする(16 ではなく 8)
    /// `--statusline` rendering にのみ影響する。
    #[arg(long)]
    compact: bool,

    /// statusline の長期枠 countdown 後に、local time の絶対リセット時刻
    /// `(MM/DD HH:MM)` を付ける。`--statusline` rendering にのみ影響する。
    #[arg(long)]
    reset_at: bool,

    /// 行の並び順。default は `provider`(プロバイダ順 — 既存挙動を維持)。
    /// `weekly-usage` は長期枠の使用率が高い順、`weekly-reset` は長期枠のリセット
    /// 時刻が近い順(オプション名は互換性のため維持)。データ無し/取得失敗の行は常に末尾。table/json/
    /// statusline すべてに適用される。
    #[arg(long, value_enum, default_value_t = SortKey::Provider)]
    sort: SortKey,

    /// `--statusline` 描画時に隠す provider(comma-separated: claude,codex,antigravity,
    /// pixellab,grok)。fetch と `--json` / table には影響しない。指定すると config の
    /// `[statusline] hide` を上書きする。
    #[arg(long, value_delimiter = ',', value_name = "PROVIDERS")]
    statusline_hide: Vec<ProviderArg>,
}

#[derive(ValueEnum, Clone, Copy)]
enum ProviderArg {
    Claude,
    Codex,
    Antigravity,
    Pixellab,
    Grok,
}

impl ProviderArg {
    fn to_provider(self) -> Provider {
        match self {
            ProviderArg::Claude => Provider::Claude,
            ProviderArg::Codex => Provider::Codex,
            ProviderArg::Antigravity => Provider::Antigravity,
            ProviderArg::Pixellab => Provider::PixelLab,
            ProviderArg::Grok => Provider::Grok,
        }
    }
}

/// provider と認証材料を 1 つの enum に閉じ込める。provider/auth を別々に保持して
/// 実行時に不整合を検出するのではなく、不正な組み合わせ自体を表現不能にする。
enum FetchSpec {
    Claude(HashMap<String, String>),
    Codex(HashMap<String, String>),
    PixelLab(HashMap<String, String>),
    Antigravity(Option<config::AntigravityCfg>),
    Grok(Option<config::GrokCfg>),
}

impl FetchSpec {
    fn provider(&self) -> Provider {
        match self {
            FetchSpec::Claude(_) => Provider::Claude,
            FetchSpec::Codex(_) => Provider::Codex,
            FetchSpec::PixelLab(_) => Provider::PixelLab,
            FetchSpec::Antigravity(_) => Provider::Antigravity,
            FetchSpec::Grok(_) => Provider::Grok,
        }
    }

    async fn fetch(&self, clients: &http::Clients) -> Result<Vec<UsageRow>> {
        match self {
            FetchSpec::Claude(cookies) => claude::fetch(&clients.browser, cookies).await,
            FetchSpec::Codex(cookies) => codex::fetch(&clients.browser, cookies).await,
            FetchSpec::PixelLab(cookies) => pixellab::fetch(&clients.browser, cookies).await,
            FetchSpec::Antigravity(cfg) => antigravity::fetch(&clients.api, cfg.as_ref()).await,
            // cli-chat-proxy.grok.com は Cloudflare 前面にあるが、`Authorization: Bearer` だけで
            // 通ることを実測で確認済み。Cookie 系プロバイダとは違って Chrome fingerprint は不要
            // なため、Antigravity と同じ plain `api` client を使う。
            FetchSpec::Grok(cfg) => grok::fetch(&clients.api, cfg.as_ref()).await,
        }
    }
}

/// fetch 結果の AccountReport 行に引き継ぐ job のメタデータ。auth と分離することで、
/// JoinSet の受け渡しをタプルのバラ撒きではなく構造体で行う。
struct JobMeta {
    profile_name: String,
    profile_email: Option<String>,
    label: Option<String>,
}

impl JobMeta {
    /// fetch 成功行 1 件を AccountReport にする。1 job が複数行(Antigravity の
    /// model group)を返すため、メタは行ごとに clone する。
    fn report_for(&self, provider: Provider, row: UsageRow) -> AccountReport {
        AccountReport {
            profile_name: self.profile_name.clone(),
            profile_email: self.profile_email.clone(),
            label: self.label.clone(),
            provider,
            group_label: row.group_label,
            usage: Ok(row.usage),
        }
    }

    /// fetch 失敗を error 行 1 件にする(メタは move で消費)。
    fn error_report(self, provider: Provider, e: anyhow::Error) -> AccountReport {
        AccountReport {
            profile_name: self.profile_name,
            profile_email: self.profile_email,
            label: self.label,
            provider,
            group_label: None,
            usage: Err(e),
        }
    }
}

struct Job {
    meta: JobMeta,
    fetch: FetchSpec,
}

impl Job {
    /// Chrome profile の表示メタデータと認証材料を 1 job にまとめる。
    fn chrome(target: &Target, fetch: FetchSpec) -> Self {
        Self {
            meta: JobMeta {
                profile_name: target.profile.name.clone(),
                profile_email: target.profile.email.clone(),
                label: target.label.clone(),
            },
            fetch,
        }
    }

    /// Chrome profile に属さない OAuth account 用の job を作る。
    fn oauth(profile_name: &str, label: Option<String>, fetch: FetchSpec) -> Self {
        Self {
            meta: JobMeta {
                profile_name: profile_name.to_string(),
                profile_email: None,
                label,
            },
            fetch,
        }
    }
}

/// 表示対象として解決済みの Chrome profile。label と表示 provider を持つ。
struct Target {
    profile: Profile,
    label: Option<String>,
    wants: BrowserWants,
}

/// 1 job(retry 込み)の全体 deadline。個々のリクエストは http.rs の
/// REQUEST_TIMEOUT で切れるが、retry × backoff の積み上げでも合計がここを
/// 超えないよう上限を張る。超過した job はその行だけ error 表示になり、
/// 他 provider の結果はそのまま出る。
const JOB_DEADLINE: Duration = Duration::from_secs(20);

/// transport error / 429 / 5xx / Cloudflare challenge のみ backoff 付きで retry する。
/// 認証不正や parse error は待っても回復しないため即座に返す。
async fn fetch_with_retry(clients: &http::Clients, fetch: &FetchSpec) -> Result<Vec<UsageRow>> {
    const BACKOFF_MS: [u64; 3] = [600, 1500, 2800];
    let mut attempt = 0;
    loop {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(BACKOFF_MS[attempt - 1])).await;
        }
        match fetch.fetch(clients).await {
            Ok(rows) => return Ok(rows),
            Err(e) if attempt < BACKOFF_MS.len() && http::is_retryable(&e) => attempt += 1,
            Err(e) => return Err(e),
        }
    }
}

/// 必要な Chrome profile だけ Cookie を読み、signed-in provider の job を組み立てる。
fn chrome_jobs(root: &std::path::Path, targets: &[Target]) -> Result<Vec<Job>> {
    if !targets.iter().any(|target| target.wants.any()) {
        return Ok(Vec::new());
    }

    let password = cookies::safe_storage_key("Chrome Safe Storage")?;
    let key = cookies::derive_key(&password);
    let mut jobs = Vec::new();
    for target in targets {
        let Some(db) = profiles::cookies_db(root, &target.profile.dir) else {
            continue;
        };
        let profile_cookies = match cookies::load(&db, &key) {
            Ok(profile_cookies) => profile_cookies,
            Err(error) => {
                eprintln!(
                    "ai-usage: skipping Chrome profile {}: {error:#}",
                    target.profile.name
                );
                continue;
            }
        };
        if target.wants.claude && claude::has_session(&profile_cookies.claude) {
            jobs.push(Job::chrome(
                target,
                FetchSpec::Claude(profile_cookies.claude),
            ));
        }
        if target.wants.codex && codex::has_session(&profile_cookies.chatgpt) {
            jobs.push(Job::chrome(
                target,
                FetchSpec::Codex(profile_cookies.chatgpt),
            ));
        }
        if target.wants.pixellab && pixellab::has_session(&profile_cookies.pixellab) {
            jobs.push(Job::chrome(
                target,
                FetchSpec::PixelLab(profile_cookies.pixellab),
            ));
        }
    }
    Ok(jobs)
}

/// Chrome 由来の job 取得失敗を、OAuth provider の有無で「縮退」と「致命的」に振り分ける。
///
/// Keychain の許可ダイアログを拒否した場合など、Cookie 復号鍵が取れないと `chrome_jobs`
/// 全体が失敗する。これをそのまま伝播させると、Chrome と無関係な Antigravity / Grok の
/// 行まで消える。Chrome profile の検出失敗を `run()` で縮退させているのと同じ方針で、
/// OAuth provider が取得対象に残っているなら Chrome 行だけを諦めて続行する。
/// 取得対象が Chrome だけのときは、原因の分かる元のエラー(「Keychain のダイアログを
/// 承認して再実行」)をそのまま返す。
fn chrome_jobs_or_degrade(jobs: Result<Vec<Job>>, has_oauth_targets: bool) -> Result<Vec<Job>> {
    match jobs {
        Ok(jobs) => Ok(jobs),
        Err(error) if has_oauth_targets => {
            eprintln!("ai-usage: skipping Chrome profiles: {error:#}");
            Ok(Vec::new())
        }
        Err(error) => Err(error),
    }
}

/// job を並行実行し、完了順ではなく入力順に AccountReport を返す。
async fn run_jobs(clients: http::Clients, jobs: Vec<Job>) -> Vec<AccountReport> {
    let mut set = tokio::task::JoinSet::new();
    for (idx, job) in jobs.into_iter().enumerate() {
        let clients = clients.clone();
        set.spawn(async move {
            if idx > 0 {
                tokio::time::sleep(Duration::from_millis(150 * idx as u64)).await;
            }
            let provider = job.fetch.provider();
            let rows =
                match tokio::time::timeout(JOB_DEADLINE, fetch_with_retry(&clients, &job.fetch))
                    .await
                {
                    Ok(rows) => rows,
                    Err(_) => Err(anyhow::anyhow!(
                        "fetch timed out after {}s (including retries)",
                        JOB_DEADLINE.as_secs()
                    )),
                };
            (idx, provider, job.meta, rows)
        });
    }

    // fetch 成功時は 1 行以上(Antigravity は model group ごと)、失敗時は error 行を
    // 1 行返す。job 順は維持する。
    let mut results: Vec<(usize, AccountReport)> = Vec::new();
    while let Some(joined) = set.join_next().await {
        let (idx, provider, meta, rows) = match joined {
            Ok(result) => result,
            Err(error) => {
                eprintln!("ai-usage: fetch task failed: {error}");
                continue;
            }
        };
        match rows {
            Ok(rows) => {
                results.extend(
                    rows.into_iter()
                        .map(|row| (idx, meta.report_for(provider, row))),
                );
            }
            Err(error) => results.push((idx, meta.error_report(provider, error))),
        }
    }
    results.sort_by_key(|(idx, _)| *idx);
    results.into_iter().map(|(_, report)| report).collect()
}

/// 対象 profile の Cookie を復号し、signed-in 済み account を並行 fetch する。
/// 結果は入力順を維持する。
async fn fetch_reports(
    root: &std::path::Path,
    targets: &[Target],
    want_antigravity: bool,
    antigravity_cfg: Option<&config::AntigravityCfg>,
    want_grok: bool,
    grok_cfg: Option<&config::GrokCfg>,
) -> Result<Vec<AccountReport>> {
    let clients = http::clients()?;
    // Chrome Cookie job(Claude/Codex/PixelLab)。必要な場合だけ Keychain に触るため、
    // `--only antigravity` では prompt 自体を避けられる。
    let mut jobs =
        chrome_jobs_or_degrade(chrome_jobs(root, targets), want_antigravity || want_grok)?;

    // Antigravity job は Chrome profile に紐づかない単一 OAuth/local account。
    if want_antigravity {
        jobs.push(Job::oauth(
            "Antigravity",
            antigravity_cfg.and_then(|config| config.label.clone()),
            FetchSpec::Antigravity(antigravity_cfg.cloned()),
        ));
    }

    // Grok job も Antigravity と同じく Chrome profile と独立した OAuth account。
    if want_grok {
        jobs.push(Job::oauth(
            "Grok",
            grok_cfg.and_then(|config| config.label.clone()),
            FetchSpec::Grok(grok_cfg.cloned()),
        ));
    }

    if jobs.is_empty() {
        bail!(
            "No signed-in Claude/Codex/PixelLab sessions or Antigravity/Grok token found. Sign in \
             via Chrome, open Antigravity, run `agy` / `grok login`, or adjust --profile / --only / your config. \
             (Try --list-profiles.)"
        );
    }

    Ok(run_jobs(clients, jobs).await)
}

/// Claude Code 設定 file の path。`$CLAUDE_CONFIG_DIR/.claude.json`、未設定なら
/// home 直下の `~/.claude.json`。
fn claude_config_path() -> PathBuf {
    resolve_claude_config_path(std::env::var_os("CLAUDE_CONFIG_DIR"), dirs::home_dir())
}

/// `claude_config_path` の解決規則。環境に触れずテストできるよう入力を引数で受ける。
///
/// 空文字の `CLAUDE_CONFIG_DIR` は未設定と同じ扱いにする。Unix の environ は `FOO=` を
/// 「値が空の存在する変数」として持つため、`export CLAUDE_CONFIG_DIR="$未定義変数"` を
/// 書いた shell から起動すると `var_os` が `Some("")` を返し、join の結果が相対 path
/// `.claude.json` になって cwd 直下の無関係な file を読んでしまう
/// (`profiles.rs` / `pixellab.rs` の「空文字は未設定」と規約を揃える)。
fn resolve_claude_config_path(env_dir: Option<OsString>, home: Option<PathBuf>) -> PathBuf {
    env_dir
        .filter(|d| !d.is_empty())
        .map(|d| PathBuf::from(d).join(".claude.json"))
        .unwrap_or_else(|| home.unwrap_or_default().join(".claude.json"))
}

/// Claude Code 設定 file から signed-in account email(`oauthAccount.emailAddress`)を読む。
/// file がない、parse できない、一部 auth method のように field 自体がない場合は `None`。
fn read_claude_email(path: &std::path::Path) -> Option<String> {
    let data = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    v.pointer("/oauthAccount/emailAddress")
        .and_then(|e| e.as_str())
        .map(str::to_string)
}

/// この session が signed-in している account email(active row highlight 用)。
fn active_claude_email() -> Option<String> {
    read_claude_email(&claude_config_path())
}

/// highlight 対象 account を解決する。`--active-profile`(必要なら `--active-provider` で
/// provider を限定)が最優先で、profile 名で照合する。未指定なら email chain
/// (`--active-email` → config → `.claude.json`)に fallback し、一致する Claude 行を
/// highlight する。`--debug` では判定を JSONL で stderr に出す。
fn active_target(cli: &Cli, cfg: &config::Config) -> Option<render::ActiveTarget> {
    if let Some(profile) = cli.active_profile.clone() {
        let provider = cli.active_provider.map(ProviderArg::to_provider);
        if cli.debug {
            eprintln!(
                "{}",
                serde_json::json!({
                    "event": "active",
                    "source": "active_profile_flag",
                    "profile": profile,
                    "provider": provider.map(|p| p.label()),
                })
            );
        }
        return Some(render::ActiveTarget {
            email: None,
            profile: Some(profile),
            provider,
        });
    }
    let (email, source, path) = resolve_active_email(cli, cfg);
    if cli.debug {
        eprintln!(
            "{}",
            serde_json::json!({
                "event": "active",
                "source": source,
                "path": path,
                "email": email,
            })
        );
    }
    email.map(|e| render::ActiveTarget {
        email: Some(e),
        profile: None,
        provider: None,
    })
}

/// email base の active 解決 chain。`--debug` 用に、選ばれた email と source
/// (参照した場合は `.claude.json` path)を返す。
fn resolve_active_email(
    cli: &Cli,
    cfg: &config::Config,
) -> (Option<String>, &'static str, Option<String>) {
    if let Some(e) = cli.active_email.clone() {
        return (Some(e), "active_email_flag", None);
    }
    if let Some(e) = cfg.active_email.clone() {
        return (Some(e), "config", None);
    }
    let path = claude_config_path();
    let email = read_claude_email(&path);
    (
        email,
        "claude_config",
        Some(path.to_string_lossy().into_owned()),
    )
}

fn color_enabled(no_color_flag: bool) -> bool {
    resolve_color_enabled(
        no_color_flag,
        std::env::var_os("NO_COLOR"),
        std::env::var("TERM").ok(),
    )
}

/// `color_enabled` の判定規則。環境に触れずテストできるよう入力を引数で受ける。
///
/// NO_COLOR の仕様(no-color.org)は「存在し、かつ空文字列でない」ときだけ無効化と
/// 定める。`is_some()` だけで見ると `export NO_COLOR="$未定義変数"` のような空値でも
/// 色が落ちるため、明示的に空文字を除外する。
fn resolve_color_enabled(
    no_color_flag: bool,
    no_color: Option<OsString>,
    term: Option<String>,
) -> bool {
    if no_color_flag || no_color.is_some_and(|v| !v.is_empty()) {
        return false;
    }
    term.map(|t| t != "dumb").unwrap_or(true)
}

/// TOML の basic string としてシリアライズした文字列を返す(クオート込み)。
/// プロファイル名やラベルが `"` や `\` を含んでも生成された TOML が壊れないよう、
/// すべての値出力でこれを通す。
fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// 現在 signed-in 済みの profile から starter `config.toml` を組み立てる。
/// Cookie の存在だけで判定し、network や Keychain は使わない。
fn generate_config(root: &std::path::Path, all: &[Profile]) -> String {
    let mut out = String::from(
        "# `ai-usage --init-config` で生成しました。自由に編集できます。\n\
         # 並び替え、不要 profile の削除、label の変更を自由に行えます。\n\n",
    );
    if let Some(active) = active_claude_email() {
        out += &format!("active_email = {}\n\n", toml_str(&active));
    }
    for p in all {
        let Some(db) = profiles::cookies_db(root, &p.dir) else {
            continue;
        };
        let sessions = cookies::detect_sessions(&db);
        if !sessions.claude && !sessions.codex && !sessions.pixellab {
            continue;
        }
        let email = p.email.as_deref().unwrap_or("");
        let label = email
            .split('@')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(&p.name);
        out += "[[profiles]]\n";
        out += &format!("match = {}", toml_str(profile_matcher(p, all)));
        if !email.is_empty() {
            out += &format!("   # {email}");
        }
        out += "\n";
        out += &format!("label = {}\n", toml_str(label));
        // 検出された provider の subset だけ出す。全 provider 揃っているときは省略。
        let all_wanted = sessions.claude && sessions.codex && sessions.pixellab;
        if !all_wanted {
            let mut wanted: Vec<&str> = Vec::new();
            if sessions.claude {
                wanted.push("claude");
            }
            if sessions.codex {
                wanted.push("codex");
            }
            if sessions.pixellab {
                wanted.push("pixellab");
            }
            let items: Vec<String> = wanted.iter().map(|s| toml_str(s)).collect();
            out += &format!("providers = [{}]\n", items.join(", "));
        }
        out += "\n";
    }
    out += "# 全表示モードから除外し、取得も省くプロバイダ。--only で一時的に表示できます。\n";
    out += "# [providers]\n# exclude = [\"antigravity\"]\n";
    out
}

/// 表示名が別のプロファイル名やディレクトリ名と衝突するときは、一意なディレクトリ名を使う。
fn profile_matcher<'a>(profile: &'a Profile, all: &[Profile]) -> &'a str {
    if all.iter().any(|other| {
        other.dir != profile.dir
            && (other.name.eq_ignore_ascii_case(&profile.name)
                || other.dir.eq_ignore_ascii_case(&profile.name))
    }) {
        &profile.dir
    } else {
        &profile.name
    }
}

/// `--only` で選んだ provider を Chrome 系 provider の選択に直す。Antigravity / Grok は
/// Chrome Cookie を使わない(別経路で取得する)ため、どの Chrome provider も選ばない。
fn only_wants(only: ProviderArg) -> BrowserWants {
    let none = BrowserWants::none();
    match only {
        ProviderArg::Claude => BrowserWants {
            claude: true,
            ..none
        },
        ProviderArg::Codex => BrowserWants {
            codex: true,
            ..none
        },
        ProviderArg::Pixellab => BrowserWants {
            pixellab: true,
            ..none
        },
        ProviderArg::Antigravity | ProviderArg::Grok => none,
    }
}

/// config 行の無い profile に表示する provider。global `--only` があればその provider だけ、
/// 無ければ Chrome provider の全て。
fn resolve_wants(cli: &Cli) -> BrowserWants {
    cli.only.map_or(BrowserWants::all(), only_wants)
}

/// config の各 `[[profiles]]` 行を検出済み profile に割り当て、`(行, all の添字)` を config 順に
/// 返す。ディレクトリ名の一致を優先し、表示名の一致はまだ割り当てていない profile を優先する
/// (表示名が重複しても、別々の行を同じ profile にまとめない)。一致する profile の無い行は落とす。
fn bind_config_rows<'a>(
    all: &[Profile],
    cfg: &'a config::Config,
) -> Vec<(&'a config::ProfileCfg, usize)> {
    let mut bound = Vec::new();
    let mut used = HashSet::new();
    for row in &cfg.profiles {
        let Some(index) = all
            .iter()
            .position(|p| p.dir.eq_ignore_ascii_case(&row.matcher))
            .or_else(|| {
                (0..all.len())
                    .find(|&i| !used.contains(&i) && row.matches(&all[i].name, &all[i].dir))
            })
            .or_else(|| all.iter().position(|p| row.matches(&p.name, &p.dir)))
        else {
            continue;
        };
        used.insert(index);
        bound.push((row, index));
    }
    bound
}

/// 割り当て済みの config 行ごとに、表示する provider を決める(戻り値は `rows` と同じ順)。
///
/// 同じ profile に複数の行があるときは、config の `providers` を先に書いた行がその provider を
/// 受け持ち、同じ Cookie で同じ provider を二重に取得しない。`--only` は受け持ちを決めた後で
/// 当てる。先に当てると全行が同じ provider を受け持つように見え、別の provider 用に書いた
/// 先頭行の label で表示されてしまう。`--only` は config の `providers` より優先するため、
/// どの行も受け持たない provider は、その profile の先頭行で表示する。
fn config_row_wants(
    rows: &[(&config::ProfileCfg, usize)],
    only: Option<ProviderArg>,
) -> Vec<BrowserWants> {
    let mut owned = HashMap::<usize, BrowserWants>::new();
    let mut wants: Vec<BrowserWants> = rows
        .iter()
        .map(|(row, profile)| {
            let owned = owned.entry(*profile).or_insert_with(BrowserWants::none);
            let mine = row.wants().without(*owned);
            *owned = owned.union(mine);
            mine
        })
        .collect();
    if let Some(only) = only.map(only_wants) {
        let mut first_rows = HashSet::new();
        for ((_, profile), wants) in rows.iter().zip(&mut wants) {
            let unowned = if first_rows.insert(*profile) {
                only.without(owned[profile])
            } else {
                BrowserWants::none()
            };
            *wants = wants.union(unowned).intersect(only);
        }
    }
    wants
}

/// 表示する profile を label / provider filter 付きで解決する。
/// 優先順は `--profile` > config `[[profiles]]` > 全 auto-discover。
fn build_targets(all: Vec<Profile>, cli: &Cli, cfg: &config::Config) -> Vec<Target> {
    let rows = bind_config_rows(&all, cfg);
    let wants = config_row_wants(&rows, cli.only);

    if !cli.profile.is_empty() {
        // --profile: discovery 順を保ち、指定 profile だけに絞る。label と providers は
        // 指定なしの実行と同じ config 行から決め、config に無い profile は既定値で表示する。
        let mut targets = Vec::new();
        for (index, profile) in all.iter().enumerate() {
            if !cli.profile.iter().any(|w| {
                w.eq_ignore_ascii_case(&profile.name) || w.eq_ignore_ascii_case(&profile.dir)
            }) {
                continue;
            }
            let mut configured = false;
            for ((row, _), wants) in rows.iter().zip(&wants).filter(|((_, i), _)| *i == index) {
                configured = true;
                if wants.any() {
                    targets.push(Target {
                        profile: profile.clone(),
                        label: row.label.clone(),
                        wants: *wants,
                    });
                }
            }
            if !configured {
                targets.push(Target {
                    profile: profile.clone(),
                    label: None,
                    wants: resolve_wants(cli),
                });
            }
        }
        targets
    } else if !cfg.profiles.is_empty() {
        // config 順: 各 [[profiles]] 行を割り当てた profile で表示する。
        rows.iter()
            .zip(wants)
            .filter(|(_, wants)| wants.any())
            .map(|((row, index), wants)| Target {
                profile: all[*index].clone(),
                label: row.label.clone(),
                wants,
            })
            .collect()
    } else {
        // auto: 検出済み profile を discovery 順ですべて使う。
        all.into_iter()
            .map(|profile| Target {
                profile,
                label: None,
                wants: resolve_wants(cli),
            })
            .collect()
    }
}

/// `--list-profiles`: 検出 profile と Cookie store の有無を出力する。
fn list_profiles(root: &std::path::Path, all: &[Profile]) -> std::io::Result<()> {
    let mut text = String::new();
    for p in all {
        let note = if profiles::cookies_db(root, &p.dir).is_some() {
            ""
        } else {
            "  (no cookie store)"
        };
        text += &format!(
            "{:<18} dir={:<12} {}{}\n",
            p.name,
            p.dir,
            p.email.as_deref().unwrap_or(""),
            note
        );
    }
    render::write_stdout(&text)
}

/// `--init-config`: starter config を書き込む。既に存在する場合は stdout に出す。
fn write_init_config(
    root: &std::path::Path,
    all: &[Profile],
    explicit: Option<&std::path::Path>,
) -> Result<()> {
    let text = generate_config(root, all);
    let path = explicit
        .map(std::path::Path::to_path_buf)
        .or_else(config::default_path);
    match path {
        Some(p) if create_config(&p, &text)? => {
            eprintln!("Wrote starter config to {}", p.display());
        }
        Some(p) => {
            eprintln!(
                "Config already exists at {} — printing a fresh one to stdout (redirect to overwrite).",
                p.display()
            );
            render::write_stdout(&text)?;
        }
        None => render::write_stdout(&text)?,
    }
    Ok(())
}

/// 既存ファイルを上書きせず、設定ファイルを排他的に新規作成する。
fn create_config(path: &std::path::Path, text: &str) -> Result<bool> {
    use std::io::Write;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    file.write_all(text.as_bytes())?;
    Ok(true)
}

/// CLI flag 群から statusline の表示オプションを組み立てる。cached / fresh 双方の
/// 描画経路で同じ引数展開を繰り返さないための共有ヘルパ。
fn statusline_opts(cli: &Cli, cfg: &config::Config) -> render::StatuslineOpts {
    render::StatuslineOpts {
        color: color_enabled(cli.no_color),
        logos: cli.logos,
        debug: cli.debug,
        compact: cli.compact,
        reset_at: cli.reset_at,
        hide: resolve_statusline_hide(cli, cfg),
    }
}

/// `[providers] exclude` を全表示モード共通の除外対象に解決する。
/// 明示的な `--only` は設定より優先し、その provider だけは表示する。
fn excluded_providers(cli: &Cli, cfg: &config::Config) -> Vec<model::Provider> {
    let only = cli.only.map(ProviderArg::to_provider);
    let mut excluded = Vec::new();
    if let Some(providers) = &cfg.providers {
        for provider in providers
            .exclude
            .iter()
            .filter_map(|name| parse_provider(name))
        {
            if Some(provider) != only && !excluded.contains(&provider) {
                excluded.push(provider);
            }
        }
    }
    excluded
}

fn excluded_browser_wants(excluded: &[model::Provider]) -> BrowserWants {
    BrowserWants {
        claude: excluded.contains(&model::Provider::Claude),
        codex: excluded.contains(&model::Provider::Codex),
        pixellab: excluded.contains(&model::Provider::PixelLab),
    }
}

/// config 行ごとの label と provider の受け持ちを決めた後で全体の除外を適用する。
fn filter_targets(targets: Vec<Target>, excluded: &[model::Provider]) -> Vec<Target> {
    let omitted = excluded_browser_wants(excluded);
    targets
        .into_iter()
        .filter_map(|mut target| {
            target.wants = target.wants.without(omitted);
            target.wants.any().then_some(target)
        })
        .collect()
}

/// 古い JSON キャッシュにも同じ除外を適用する。`--only` を先に解決する。
fn filter_cached_report(report: &mut report::Report, cli: &Cli, excluded: &[model::Provider]) {
    if let Some(only) = cli.only {
        report
            .accounts
            .retain(|account| account.provider == only.to_provider());
    }
    report
        .accounts
        .retain(|account| !excluded.contains(&account.provider));
}

/// `--statusline-hide`(CLI)と `[statusline] hide`(config)を Provider 集合に解決する。
/// CLI 指定があればそちらを最優先。未知の文字列は wrap 無しで無視する
/// (config を古い binary で読めるようにするのと同じ寛容ポリシー)。
fn resolve_statusline_hide(cli: &Cli, cfg: &config::Config) -> Vec<model::Provider> {
    if !cli.statusline_hide.is_empty() {
        return cli
            .statusline_hide
            .iter()
            .map(|p| p.to_provider())
            .collect();
    }
    let Some(sl) = cfg.statusline.as_ref() else {
        return Vec::new();
    };
    sl.hide.iter().filter_map(|s| parse_provider(s)).collect()
}

/// config で渡された provider 名文字列を Provider に解決する。case-insensitive。
fn parse_provider(s: &str) -> Option<model::Provider> {
    match s.to_ascii_lowercase().as_str() {
        "claude" => Some(model::Provider::Claude),
        "codex" => Some(model::Provider::Codex),
        "antigravity" => Some(model::Provider::Antigravity),
        "pixellab" => Some(model::Provider::PixelLab),
        "grok" => Some(model::Provider::Grok),
        _ => None,
    }
}

/// cached `--json` file から statusline を描画する。cache がない/不正な場合は何も出さず、
/// 次の描画で再生成される。
fn render_cached_statusline(
    path: &std::path::Path,
    cli: &Cli,
    cfg: &config::Config,
    excluded: &[model::Provider],
    active: Option<&render::ActiveTarget>,
) -> std::io::Result<()> {
    let Ok(data) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    let Ok(mut report) = serde_json::from_str::<report::Report>(&data) else {
        return Ok(());
    };
    filter_cached_report(&mut report, cli, excluded);
    render::statusline(&report, active, cli.sort, &statusline_opts(cli, cfg))
}

/// fresh に fetch した report を CLI flag に応じた format で描画する。
fn render_reports(
    cli: &Cli,
    cfg: &config::Config,
    reports: &[AccountReport],
    active: Option<&render::ActiveTarget>,
) -> std::io::Result<()> {
    if cli.statusline {
        render::statusline(
            &report::Report::build(reports),
            active,
            cli.sort,
            &statusline_opts(cli, cfg),
        )
    } else if cli.json {
        render::json(&report::Report::build(reports), cli.sort)
    } else {
        render::table(reports, cli.sort, color_enabled(cli.no_color))
    }
}

/// Chrome や network に触れず、入力済み cache だけを描画するモードか判定する。
fn uses_cached_statusline(cli: &Cli) -> bool {
    cli.statusline && cli.input.is_some() && !cli.list_profiles && !cli.init_config
}

fn uses_cached_tui(cli: &Cli) -> bool {
    cli.tui && cli.input.is_some() && !cli.list_profiles && !cli.init_config
}

/// profile を検出し、info-only flag を処理してから usage を fetch/render する。
fn needs_profile_discovery(cli: &Cli, excluded: &[model::Provider]) -> bool {
    // 一覧と設定生成は provider filter にかかわらず Chrome profile が必要。
    // OAuth provider 単独取得とキャッシュ描画では Local State を読まない。
    cli.list_profiles
        || cli.init_config
        || !(uses_cached_statusline(cli) || uses_cached_tui(cli))
            && cli
                .only
                .map_or(BrowserWants::all(), only_wants)
                .without(excluded_browser_wants(excluded))
                .any()
}

async fn run(cli: Cli) -> Result<()> {
    if cli.tui
        && !cli.list_profiles
        && !cli.init_config
        && (!std::io::stdin().is_terminal() || !std::io::stdout().is_terminal())
    {
        bail!("--tui requires an interactive terminal (use --json for redirected output)");
    }
    let cfg = if cli.list_profiles || cli.init_config {
        config::Config::default()
    } else {
        config::load(cli.config.as_deref())
    };
    let excluded = excluded_providers(&cli, &cfg);

    // キャッシュ描画は Chrome の Local State、network、Keychain のいずれにも依存させない。
    if uses_cached_statusline(&cli) {
        let path = cli
            .input
            .as_deref()
            .expect("キャッシュ描画モードでは入力パスが存在する");
        let active = active_target(&cli, &cfg);
        render_cached_statusline(path, &cli, &cfg, &excluded, active.as_ref())?;
        return Ok(());
    }

    if uses_cached_tui(&cli) {
        let path = cli.input.as_ref().expect("checked input");
        let read = || -> Result<report::Report> {
            let data = std::fs::read_to_string(path)?;
            let mut report: report::Report = serde_json::from_str(&data)?;
            filter_cached_report(&mut report, &cli, &excluded);
            Ok(report)
        };
        let initial = read()?;
        let refresh: render::TuiFetch<'_> = Box::new(|| Box::pin(async { read() }));
        return render::tui(
            Some(initial),
            Some(refresh),
            cli.sort,
            color_enabled(cli.no_color),
        )
        .await;
    }

    if [
        Provider::Claude,
        Provider::Codex,
        Provider::Antigravity,
        Provider::PixelLab,
        Provider::Grok,
    ]
    .iter()
    .all(|provider| excluded.contains(provider))
    {
        if cli.tui {
            return render::tui(
                Some(report::Report::build(&[])),
                None,
                cli.sort,
                color_enabled(cli.no_color),
            )
            .await;
        }
        render_reports(&cli, &cfg, &[], None)?;
        return Ok(());
    }

    let root = profiles::chrome_root()?;
    let all = if needs_profile_discovery(&cli, &excluded) {
        match profiles::discover(&root) {
            Ok(all) => all,
            // profile 一覧と設定生成は Chrome そのものが目的なので、従来どおり失敗させる。
            Err(error) if cli.list_profiles || cli.init_config => return Err(error),
            // それ以外は Chrome を「provider 0 件」に縮退させる。Antigravity / Grok は
            // Chrome ではなく OAuth token を見るため、Chrome 未導入や Local State 破損で
            // これらまで道連れにすると、`--only` を付けたときだけ結果が出るという
            // 説明のつかない非対称が残る。個々の profile 読み取り失敗を握りつぶして
            // 続行する既存方針(fetch_reports)とも揃う。
            Err(error) => {
                eprintln!("ai-usage: skipping Chrome profiles: {error:#}");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    if cli.list_profiles {
        list_profiles(&root, &all)?;
        return Ok(());
    }
    if cli.init_config {
        return write_init_config(&root, &all, cli.config.as_deref());
    }

    let targets = filter_targets(build_targets(all, &cli, &cfg), &excluded);
    let want_antigravity = !excluded.contains(&Provider::Antigravity)
        && match cli.only {
            Some(ProviderArg::Antigravity) => true,
            Some(_) => false,
            None => antigravity::available(cfg.antigravity.as_ref()).await,
        };
    let want_grok = !excluded.contains(&Provider::Grok)
        && match cli.only {
            Some(ProviderArg::Grok) => true,
            Some(_) => false,
            None => grok::available(cfg.grok.as_ref()),
        };
    if cli.tui {
        let refresh: render::TuiFetch<'_> = Box::new(|| {
            Box::pin(async {
                let reports = fetch_reports(
                    &root,
                    &targets,
                    want_antigravity,
                    cfg.antigravity.as_ref(),
                    want_grok,
                    cfg.grok.as_ref(),
                )
                .await?;
                Ok(report::Report::build(&reports))
            })
        });
        return render::tui(None, Some(refresh), cli.sort, color_enabled(cli.no_color)).await;
    }
    let reports = fetch_reports(
        &root,
        &targets,
        want_antigravity,
        cfg.antigravity.as_ref(),
        want_grok,
        cfg.grok.as_ref(),
    )
    .await?;

    let active = cli.statusline.then(|| active_target(&cli, &cfg)).flatten();
    render_reports(&cli, &cfg, &reports, active.as_ref())?;
    Ok(())
}

/// stdout の読み手が先に pipe を閉じたことによる書き込み失敗かどうか。
fn is_broken_pipe(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::BrokenPipe)
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    match run(Cli::parse()).await {
        // `ai-usage --json | head` のように読み手が出力を最後まで読まないのは異常ではないため、
        // 残りの出力を捨てて正常終了する。
        Err(error) if is_broken_pipe(&error) => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_config_path_treats_empty_env_as_unset() {
        let home = PathBuf::from("/home/u");

        // 通常: $CLAUDE_CONFIG_DIR/.claude.json。
        assert_eq!(
            resolve_claude_config_path(Some(OsString::from("/cfg")), Some(home.clone())),
            PathBuf::from("/cfg/.claude.json")
        );
        // 未設定: home 直下。
        assert_eq!(
            resolve_claude_config_path(None, Some(home.clone())),
            PathBuf::from("/home/u/.claude.json")
        );

        // 空文字は未設定と同じ。`export CLAUDE_CONFIG_DIR="$未定義変数"` を書いた shell から
        // 起動しても、相対 path `.claude.json` に落ちて cwd の無関係な file を読まない。
        let from_empty = resolve_claude_config_path(Some(OsString::new()), Some(home));
        assert_eq!(from_empty, PathBuf::from("/home/u/.claude.json"));
        assert!(
            from_empty.is_absolute(),
            "cwd 相対に落ちてはいけない: {}",
            from_empty.display()
        );
    }

    #[test]
    fn color_is_disabled_only_by_flag_non_empty_no_color_or_dumb_term() {
        let xterm = || Some("xterm-256color".to_string());

        // --no-color が最優先。
        assert!(!resolve_color_enabled(true, None, xterm()));
        // NO_COLOR は値の中身を問わず無効化する(仕様どおり "0" でも無効)。
        assert!(!resolve_color_enabled(
            false,
            Some(OsString::from("1")),
            xterm()
        ));
        assert!(!resolve_color_enabled(
            false,
            Some(OsString::from("0")),
            xterm()
        ));
        // 空の NO_COLOR は「未設定」と同じ(no-color.org: "present and not an empty string")。
        assert!(resolve_color_enabled(false, Some(OsString::new()), xterm()));
        // TERM=dumb は色なし、TERM 未設定 / 非 UTF-8 は色あり。
        assert!(!resolve_color_enabled(
            false,
            None,
            Some("dumb".to_string())
        ));
        assert!(resolve_color_enabled(false, None, None));
    }

    #[test]
    fn toml_str_roundtrips_through_toml_parser() {
        // 重要な不変条件は「生成した config.toml が壊れず、元の値へ round-trip する」こと。
        // toml クレートは `"` を含む値を single-quote literal にするなど形式は選ぶため、
        // 出力の見た目ではなく、TOML として parse し直して元の文字列に戻るかを検証する。
        for s in [
            "work",
            "a\"b",
            "a\\b",
            "quote\"and\\slash",
            "日本語プロファイル",
            "",
        ] {
            let line = format!("v = {}", toml_str(s));
            let map: HashMap<String, String> = toml::from_str(&line)
                .unwrap_or_else(|e| panic!("toml_str({s:?}) produced invalid TOML {line:?}: {e}"));
            assert_eq!(
                map.get("v").map(String::as_str),
                Some(s),
                "round-trip mismatch for {s:?} via {line:?}"
            );
        }
        // bare 値ではなく必ずクオートで囲まれる(TOML の key/value を壊さない)。
        assert!(toml_str("work").starts_with(['"', '\'']));
    }

    fn bw(claude: bool, codex: bool, pixellab: bool) -> BrowserWants {
        BrowserWants {
            claude,
            codex,
            pixellab,
        }
    }

    #[test]
    fn resolve_wants_only_flag_takes_precedence() {
        // --only は config より優先され、その provider だけ true になる。
        let cfg = config::ProfileCfg {
            matcher: "Work".to_string(),
            label: None,
            providers: Some(vec![
                "claude".to_string(),
                "codex".to_string(),
                "pixellab".to_string(),
            ]),
        };
        let rows = [(&cfg, 0)];
        // config で 3 種全て true でも、--only codex なら codex だけ。
        assert_eq!(
            config_row_wants(&rows, Some(ProviderArg::Codex)),
            vec![bw(false, true, false)]
        );
        assert_eq!(
            config_row_wants(&rows, Some(ProviderArg::Claude)),
            vec![bw(true, false, false)]
        );
        assert_eq!(
            config_row_wants(&rows, Some(ProviderArg::Pixellab)),
            vec![bw(false, false, true)]
        );

        // --only antigravity / grok は Chrome 系 provider をすべて false にする(別経路で取得)。
        for only in [ProviderArg::Antigravity, ProviderArg::Grok] {
            assert_eq!(
                config_row_wants(&rows, Some(only)),
                vec![bw(false, false, false)]
            );
        }

        // config の providers に無い provider でも、--only で指定すれば表示する。
        let claude_only = config::ProfileCfg {
            providers: Some(vec!["claude".to_string()]),
            ..cfg
        };
        assert_eq!(
            config_row_wants(&[(&claude_only, 0)], Some(ProviderArg::Codex)),
            vec![bw(false, true, false)]
        );
        // config の行が無い profile も同じ。
        let cli = Cli::parse_from(["ai-usage", "--only", "codex"]);
        assert_eq!(resolve_wants(&cli), bw(false, true, false));
    }

    #[test]
    fn resolve_wants_falls_back_to_config_then_all() {
        // --only 無し → config の providers。
        let cfg = config::ProfileCfg {
            matcher: "Work".to_string(),
            label: None,
            providers: Some(vec!["claude".to_string(), "pixellab".to_string()]),
        };
        assert_eq!(
            config_row_wants(&[(&cfg, 0)], None),
            vec![bw(true, false, true)]
        );

        // config も無ければ全 provider true(既定)。
        assert_eq!(
            resolve_wants(&Cli::parse_from(["ai-usage"])),
            BrowserWants::all()
        );
    }

    #[test]
    fn per_provider_rows_keep_their_labels_with_profile_and_only_filters() {
        // 同じディレクトリに provider 別の行を書いた設定は、--profile / --only を付けても
        // 各 provider をその行の label で表示する。
        let cfg = config::Config {
            profiles: vec![
                profile_cfg("Default", Some("claude-label"), Some(&["claude"])),
                profile_cfg("Default", Some("codex-label"), Some(&["codex"])),
            ],
            ..Default::default()
        };
        let rows = |args: &[&str]| {
            let cli = Cli::parse_from(std::iter::once("ai-usage").chain(args.iter().copied()));
            build_targets(
                vec![profile("Default", "Work"), profile("Profile 2", "Home")],
                &cli,
                &cfg,
            )
            .into_iter()
            .map(|target| (target.profile.dir, target.label, target.wants))
            .collect::<Vec<_>>()
        };
        let labeled = |label: &str, wants| ("Default".to_string(), Some(label.to_string()), wants);
        let both = vec![
            labeled("claude-label", bw(true, false, false)),
            labeled("codex-label", bw(false, true, false)),
        ];
        assert_eq!(rows(&[]), both);
        assert_eq!(rows(&["--profile", "Work"]), both);
        let codex = vec![labeled("codex-label", bw(false, true, false))];
        assert_eq!(rows(&["--only", "codex"]), codex);
        assert_eq!(rows(&["--profile", "Default", "--only", "codex"]), codex);
        // どの行も受け持たない provider は、--only の優先により先頭行で表示する。
        assert_eq!(
            rows(&["--only", "pixellab"]),
            vec![labeled("claude-label", bw(false, false, true))]
        );
        // config に無い profile を --profile で選んだときは既定値で表示する。
        assert_eq!(
            rows(&["--profile", "Home"]),
            vec![("Profile 2".to_string(), None, BrowserWants::all())]
        );
    }

    #[test]
    fn fetch_spec_owns_its_provider_identity() {
        assert_eq!(
            FetchSpec::Claude(HashMap::new()).provider(),
            Provider::Claude
        );
        assert_eq!(FetchSpec::Codex(HashMap::new()).provider(), Provider::Codex);
        assert_eq!(
            FetchSpec::PixelLab(HashMap::new()).provider(),
            Provider::PixelLab
        );
        assert_eq!(
            FetchSpec::Antigravity(None).provider(),
            Provider::Antigravity
        );
        assert_eq!(FetchSpec::Grok(None).provider(), Provider::Grok);
    }

    #[test]
    fn chrome_failure_degrades_only_when_oauth_targets_remain() {
        // Keychain 拒否などで Chrome 系 job が取れなくても、Antigravity / Grok が対象に
        // 残っているなら Chrome 行だけを諦めて続行する(OAuth provider を巻き添えにしない)。
        let degraded =
            chrome_jobs_or_degrade(Err(anyhow::anyhow!("keychain denied")), true).unwrap();
        assert!(degraded.is_empty());

        // Chrome だけが対象なら、原因の分かる元のエラーをそのまま返す。
        let fatal = chrome_jobs_or_degrade(Err(anyhow::anyhow!("keychain denied")), false)
            .err()
            .expect("Chrome だけが対象なら失敗を握り潰さない");
        assert_eq!(fatal.to_string(), "keychain denied");

        // 成功時は job をそのまま通す(OAuth 対象の有無で中身を変えない)。
        for has_oauth in [true, false] {
            let jobs = chrome_jobs_or_degrade(
                Ok(vec![Job::oauth("Grok", None, FetchSpec::Grok(None))]),
                has_oauth,
            )
            .unwrap();
            assert_eq!(jobs.len(), 1);
            assert_eq!(jobs[0].fetch.provider(), Provider::Grok);
        }
    }

    fn profile(dir: &str, name: &str) -> Profile {
        Profile {
            dir: dir.to_string(),
            name: name.to_string(),
            email: None,
        }
    }

    fn profile_cfg(
        matcher: &str,
        label: Option<&str>,
        providers: Option<&[&str]>,
    ) -> config::ProfileCfg {
        config::ProfileCfg {
            matcher: matcher.to_string(),
            label: label.map(str::to_string),
            providers: providers.map(|list| list.iter().map(|s| s.to_string()).collect()),
        }
    }

    fn target_names(targets: &[Target]) -> Vec<&str> {
        targets.iter().map(|t| t.profile.name.as_str()).collect()
    }

    #[test]
    fn build_targets_filters_by_cli_profile_using_name_or_dir() {
        // --profile は表示名と on-disk dir のどちらでも照合し、discovery 順を保つ。
        let all = || vec![profile("Default", "home"), profile("Profile 2", "Work")];
        let cfg = config::Config::default();

        let by_name = Cli::parse_from(["ai-usage", "--profile", "work"]);
        assert_eq!(
            target_names(&build_targets(all(), &by_name, &cfg)),
            vec!["Work"]
        );

        let by_dir = Cli::parse_from(["ai-usage", "--profile", "default"]);
        assert_eq!(
            target_names(&build_targets(all(), &by_dir, &cfg)),
            vec!["home"]
        );

        // 複数指定は discovery 順(config 順ではない)で返る。
        let both = Cli::parse_from(["ai-usage", "--profile", "Work,home"]);
        assert_eq!(
            target_names(&build_targets(all(), &both, &cfg)),
            vec!["home", "Work"]
        );

        // 一致しない指定は空になる(存在しない profile を勝手に補完しない)。
        let missing = Cli::parse_from(["ai-usage", "--profile", "nope"]);
        assert!(build_targets(all(), &missing, &cfg).is_empty());
    }

    #[test]
    fn build_targets_does_not_fetch_a_profile_twice() {
        let all = vec![profile("Default", "Work"), profile("Profile 2", "work")];
        let cfg = config::Config {
            profiles: vec![
                profile_cfg("Work", None, None),
                profile_cfg("Work", None, None),
                profile_cfg("Default", None, None),
            ],
            ..Default::default()
        };
        let targets = build_targets(all, &Cli::parse_from(["ai-usage"]), &cfg);
        let dirs: Vec<_> = targets.iter().map(|t| t.profile.dir.as_str()).collect();
        assert_eq!(dirs, ["Default", "Profile 2"]);
    }

    #[test]
    fn build_targets_preserves_disjoint_provider_settings_for_one_directory() {
        let cfg = config::Config {
            profiles: vec![
                profile_cfg("Default", Some("claude-label"), Some(&["claude"])),
                profile_cfg("Default", Some("codex-label"), Some(&["codex"])),
                profile_cfg("Default", Some("duplicate"), Some(&["claude", "pixellab"])),
            ],
            ..Default::default()
        };
        let targets = build_targets(
            vec![profile("Default", "Work")],
            &Cli::parse_from(["ai-usage"]),
            &cfg,
        );
        assert_eq!(targets.len(), 3);
        assert_eq!(targets[0].label.as_deref(), Some("claude-label"));
        assert_eq!(targets[1].label.as_deref(), Some("codex-label"));
        assert!(targets[0].wants.claude);
        assert!(targets[1].wants.codex);
        assert!(!targets[2].wants.claude);
        assert!(targets[2].wants.pixellab);
    }

    #[test]
    fn profile_matcher_uses_unique_directories_for_ambiguous_names() {
        let all = vec![
            profile("Default", "Work"),
            profile("Profile 2", "work"),
            profile("Profile 3", "Home"),
        ];
        assert_eq!(profile_matcher(&all[0], &all), "Default");
        assert_eq!(profile_matcher(&all[1], &all), "Profile 2");
        assert_eq!(profile_matcher(&all[2], &all), "Home");
        let collision = vec![profile("Default", "Home"), profile("Profile 2", "Default")];
        assert_eq!(profile_matcher(&collision[1], &collision), "Profile 2");
        let cfg = config::Config {
            profiles: vec![profile_cfg("Default", None, None)],
            ..Default::default()
        };
        assert_eq!(
            build_targets(collision, &Cli::parse_from(["ai-usage"]), &cfg)[0]
                .profile
                .dir,
            "Default"
        );
    }

    #[test]
    fn build_targets_uses_config_order_then_falls_back_to_auto_discovery() {
        let all = || vec![profile("Default", "home"), profile("Profile 2", "Work")];
        let cli = Cli::parse_from(["ai-usage"]);

        // config [[profiles]] があれば、その記述順で並べ label / providers も反映する。
        let cfg = config::Config {
            profiles: vec![
                profile_cfg("Work", Some("work"), None),
                profile_cfg("home", None, Some(&["claude"])),
                // 検出されなかった profile 行は黙って落とす。
                profile_cfg("Missing", None, None),
            ],
            ..Default::default()
        };
        let targets = build_targets(all(), &cli, &cfg);
        assert_eq!(target_names(&targets), vec!["Work", "home"]);
        assert_eq!(targets[0].label.as_deref(), Some("work"));
        assert_eq!(targets[0].wants, BrowserWants::all());
        assert!(targets[1].label.is_none());
        assert_eq!(targets[1].wants, bw(true, false, false));

        // config が空なら auto-discover。discovery 順のまま label 無し・全 provider。
        let auto = build_targets(all(), &cli, &config::Config::default());
        assert_eq!(target_names(&auto), vec!["home", "Work"]);
        assert!(auto.iter().all(|t| t.label.is_none()));
        assert!(auto.iter().all(|t| t.wants == BrowserWants::all()));

        // --profile は config 選択より優先される(config 順ではなく discovery 順になる)。
        let pinned = Cli::parse_from(["ai-usage", "--profile", "home"]);
        assert_eq!(
            target_names(&build_targets(all(), &pinned, &cfg)),
            vec!["home"]
        );
    }

    #[test]
    fn statusline_hide_prefers_cli_over_config_and_drops_unknown_names() {
        let cfg = config::Config {
            statusline: Some(config::StatuslineCfg {
                hide: vec![
                    "claude".to_string(),
                    "GROK".to_string(),
                    "unknown".to_string(),
                ],
            }),
            ..Default::default()
        };

        // CLI 指定があれば config を完全に置き換える(部分マージはしない)。
        let cli = Cli::parse_from(["ai-usage", "--statusline-hide", "codex,pixellab"]);
        assert_eq!(
            resolve_statusline_hide(&cli, &cfg),
            vec![Provider::Codex, Provider::PixelLab]
        );

        // CLI 未指定なら config を使う。大文字小文字は無視し、未知の名前は黙って捨てる
        // (新しい provider 名が書かれた config を古い binary が読んでも落とさないため)。
        let cli = Cli::parse_from(["ai-usage"]);
        assert_eq!(
            resolve_statusline_hide(&cli, &cfg),
            vec![Provider::Claude, Provider::Grok]
        );

        // [statusline] が無ければ何も隠さない。
        assert!(resolve_statusline_hide(&cli, &config::Config::default()).is_empty());
    }

    #[test]
    fn global_exclusion_filters_fetch_targets_and_cached_accounts() {
        let cfg = config::Config {
            profiles: vec![
                profile_cfg("Default", Some("claude-label"), Some(&["claude"])),
                profile_cfg("Default", Some("codex-label"), Some(&["codex"])),
                profile_cfg("Default", Some("pixel-label"), Some(&["pixellab"])),
            ],
            providers: Some(config::ProvidersCfg {
                exclude: vec!["CoDeX".into(), "GROK".into(), "unknown".into()],
            }),
            ..Default::default()
        };
        let cli = Cli::parse_from(["ai-usage"]);
        let excluded = excluded_providers(&cli, &cfg);
        assert_eq!(excluded, [Provider::Codex, Provider::Grok]);

        let targets = filter_targets(
            build_targets(vec![profile("Default", "Work")], &cli, &cfg),
            &excluded,
        );
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].label.as_deref(), Some("claude-label"));
        assert_eq!(targets[1].label.as_deref(), Some("pixel-label"));
        assert!(targets[0].wants.claude);
        assert!(targets[1].wants.pixellab);

        let reports =
            [Provider::Claude, Provider::Codex, Provider::Grok].map(|provider| AccountReport {
                profile_name: "Work".into(),
                profile_email: None,
                label: None,
                provider,
                group_label: None,
                usage: Ok(Default::default()),
            });
        let mut cached = report::Report::build(&reports);
        filter_cached_report(&mut cached, &cli, &excluded);
        assert_eq!(cached.accounts.len(), 1);
        assert_eq!(cached.accounts[0].provider, Provider::Claude);

        let explicit = Cli::parse_from(["ai-usage", "--only", "grok"]);
        let excluded = excluded_providers(&explicit, &cfg);
        assert_eq!(excluded, [Provider::Codex]);
        let mut cached = report::Report::build(&reports);
        filter_cached_report(&mut cached, &explicit, &excluded);
        assert_eq!(cached.accounts.len(), 1);
        assert_eq!(cached.accounts[0].provider, Provider::Grok);
    }

    #[test]
    fn only_broken_pipe_write_errors_end_quietly() {
        let broken = || anyhow::Error::from(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        assert!(is_broken_pipe(&broken()));
        // context で包まれても chain の io::Error から判定する。
        assert!(is_broken_pipe(&broken().context("writing the table")));
        // 他の I/O 失敗や、文面に "Broken pipe" を含むだけのエラーは従来どおり失敗にする。
        assert!(!is_broken_pipe(&anyhow::Error::from(std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        ))));
        assert!(!is_broken_pipe(&anyhow::anyhow!(
            "GET https://example.test: Broken pipe"
        )));
    }

    #[test]
    fn profile_discovery_respects_info_oauth_and_cache_modes() {
        // OAuth provider 単独取得では Chrome の Local State を読まない。
        let grok = Cli::parse_from(["ai-usage", "--only", "grok"]);
        assert!(!needs_profile_discovery(&grok, &[]));
        let antigravity = Cli::parse_from(["ai-usage", "--only", "antigravity"]);
        assert!(!needs_profile_discovery(&antigravity, &[]));

        // 一覧・設定生成は --only と併用しても Chrome profile を対象にする。
        let list = Cli::parse_from(["ai-usage", "--only", "grok", "--list-profiles"]);
        assert!(needs_profile_discovery(&list, &[]));
        let init = Cli::parse_from(["ai-usage", "--only", "antigravity", "--init-config"]);
        assert!(needs_profile_discovery(&init, &[]));

        // キャッシュ描画は通常モードでも Chrome に依存しない。
        let cached = Cli::parse_from([
            "ai-usage",
            "--statusline",
            "--input",
            "/tmp/ai-usage-cache.json",
        ]);
        assert!(!needs_profile_discovery(&cached, &[]));

        let cached_tui =
            Cli::parse_from(["ai-usage", "--tui", "--input", "/tmp/ai-usage-cache.json"]);
        assert!(uses_cached_tui(&cached_tui));
        assert!(!needs_profile_discovery(&cached_tui, &[]));

        // 情報表示 flag は cache 指定より優先し、従来どおり profile を検出する。
        let cached_list = Cli::parse_from([
            "ai-usage",
            "--statusline",
            "--input",
            "/tmp/ai-usage-cache.json",
            "--list-profiles",
        ]);
        assert!(needs_profile_discovery(&cached_list, &[]));
        assert!(!uses_cached_statusline(&cached_list));

        let cached_init = Cli::parse_from([
            "ai-usage",
            "--statusline",
            "--input",
            "/tmp/ai-usage-cache.json",
            "--init-config",
        ]);
        assert!(needs_profile_discovery(&cached_init, &[]));
        assert!(!uses_cached_statusline(&cached_init));
        assert!(needs_profile_discovery(&Cli::parse_from(["ai-usage"]), &[]));

        let all_browser_hidden = [Provider::Claude, Provider::Codex, Provider::PixelLab];
        assert!(!needs_profile_discovery(
            &Cli::parse_from(["ai-usage"]),
            &all_browser_hidden,
        ));
        assert!(needs_profile_discovery(&list, &all_browser_hidden));
        assert!(needs_profile_discovery(
            &Cli::parse_from(["ai-usage", "--only", "claude"]),
            &[],
        ));
    }
}
