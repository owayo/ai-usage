//! ai-usage — ログイン済み Chrome profile ごとの Claude / Codex 使用量上限
//! (5時間枠 / 週次枠とリセット時刻)を表示する。

mod antigravity;
mod claude;
mod codex;
mod config;
mod cookies;
mod http;
mod model;
mod pixellab;
mod profiles;
mod render;
mod report;
mod sort;

use std::collections::HashMap;
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
    about = "Show Claude & Codex usage limits across Chrome profiles"
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

    /// compact な colored statusline を出力する(1 account 1 行)
    #[arg(long)]
    statusline: bool,

    /// statusline の "Claude"/"Codex" ラベルを brand-logo glyph に置き換える
    /// (BrandLogos font が必要。github.com/owayo/brand-logo-font を参照)
    #[arg(long)]
    logos: bool,

    /// network fetch の代わりに、この JSON file(cached `--json` output)から account を読む
    /// statusline の高速描画で使う。
    #[arg(long, value_name = "PATH")]
    input: Option<PathBuf>,

    /// active として highlight する account email
    /// (default: CLAUDE_CONFIG_DIR/.claude.json から読む)
    #[arg(long, value_name = "EMAIL")]
    active_email: Option<String>,

    /// この profile 名に一致する account を active として highlight する
    /// `accounts[].profile` に対して case-insensitive に照合し、--active-email と
    /// .claude.json fallback より優先する。ログイン email ではなく profile で
    /// account を指定する tool 向け。
    #[arg(long, value_name = "NAME")]
    active_profile: Option<String>,

    /// --active-profile と併用し、highlight 対象をこの provider 行に限定する
    /// (claude/codex/antigravity)。未指定なら一致 profile の Claude 行を highlight する。
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

    /// active account の解決結果と行ごとの match 判定を JSONL で stderr に出す
    /// (stdout は汚さない)。行が active になる/ならない理由の診断用。
    #[arg(long)]
    debug: bool,

    /// statusline の gauge 幅を狭い pane 向けに半分にする(16 ではなく 8)
    /// `--statusline` rendering にのみ影響する。
    #[arg(long)]
    compact: bool,

    /// statusline の weekly countdown 後に、local time の絶対リセット時刻
    /// `(MM/DD HH:MM)` を付ける。`--statusline` rendering にのみ影響する。
    #[arg(long)]
    reset_at: bool,

    /// 行の並び順。default は `provider`(プロバイダ順 — 既存挙動を維持)。
    /// `weekly-usage` は週枠の使用率が高い順、`weekly-reset` は週枠のリセット
    /// 時刻が近い順。データ無し/取得失敗の行は常に末尾に並ぶ。table/json/
    /// statusline すべてに適用される。
    #[arg(long, value_enum, default_value_t = SortKey::Provider)]
    sort: SortKey,

    /// `--statusline` 描画時に隠す provider(comma-separated: claude,codex,antigravity,
    /// pixellab)。fetch と `--json` / table には影響しない。指定すると config の
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
}

impl ProviderArg {
    fn to_provider(self) -> Provider {
        match self {
            ProviderArg::Claude => Provider::Claude,
            ProviderArg::Codex => Provider::Codex,
            ProviderArg::Antigravity => Provider::Antigravity,
            ProviderArg::Pixellab => Provider::PixelLab,
        }
    }
}

/// job の認証材料。Claude/Codex は Chrome Cookie、Antigravity は local Google OAuth
/// token または実行中の `agy` を使う。
enum AuthMaterial {
    BrowserCookies(HashMap<String, String>),
    GoogleOAuth(Option<config::AntigravityCfg>),
}

/// fetch 結果の AccountReport 行に引き継ぐ job のメタデータ。auth と分離することで、
/// JoinSet の受け渡しをタプルのバラ撒きではなく構造体で行う。
struct JobMeta {
    profile_name: String,
    profile_email: Option<String>,
    label: Option<String>,
    provider: Provider,
}

impl JobMeta {
    /// fetch 成功行 1 件を AccountReport にする。1 job が複数行(Antigravity の
    /// model group)を返すため、メタは行ごとに clone する。
    fn report_for(&self, row: UsageRow) -> AccountReport {
        AccountReport {
            profile_name: self.profile_name.clone(),
            profile_email: self.profile_email.clone(),
            label: self.label.clone(),
            provider: self.provider,
            group_label: row.group_label,
            usage: Ok(row.usage),
        }
    }

    /// fetch 失敗を error 行 1 件にする(メタは move で消費)。
    fn error_report(self, e: anyhow::Error) -> AccountReport {
        AccountReport {
            profile_name: self.profile_name,
            profile_email: self.profile_email,
            label: self.label,
            provider: self.provider,
            group_label: None,
            usage: Err(e),
        }
    }
}

struct Job {
    meta: JobMeta,
    auth: AuthMaterial,
}

/// 表示対象として解決済みの Chrome profile。label と表示 provider を持つ。
struct Target {
    profile: Profile,
    label: Option<String>,
    wants: BrowserWants,
}

/// Cloudflare は有効な request にも challenge を返すことがあるため、account を失敗扱いに
/// する前に backoff 付きで数回 retry する。
async fn fetch_with_retry(
    clients: &http::Clients,
    provider: Provider,
    auth: &AuthMaterial,
) -> Result<Vec<UsageRow>> {
    const BACKOFF_MS: [u64; 3] = [600, 1500, 2800];
    let mut last: Option<anyhow::Error> = None;
    for attempt in 0..=BACKOFF_MS.len() {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(BACKOFF_MS[attempt - 1])).await;
        }
        let result = match (provider, auth) {
            (Provider::Claude, AuthMaterial::BrowserCookies(c)) => {
                claude::fetch(&clients.browser, c).await
            }
            (Provider::Codex, AuthMaterial::BrowserCookies(c)) => {
                codex::fetch(&clients.browser, c).await
            }
            (Provider::PixelLab, AuthMaterial::BrowserCookies(c)) => {
                pixellab::fetch(&clients.browser, c).await
            }
            (Provider::Antigravity, AuthMaterial::GoogleOAuth(cfg)) => {
                antigravity::fetch(&clients.api, cfg.as_ref()).await
            }
            _ => return Err(anyhow::anyhow!("provider/auth mismatch")),
        };
        match result {
            Ok(rows) => return Ok(rows),
            Err(e) => last = Some(e),
        }
    }
    Err(last.expect("at least one attempt was made"))
}

/// 対象 profile の Cookie を復号し、signed-in 済み account を並行 fetch する。
/// 結果は入力順を維持する。
async fn fetch_reports(
    root: &std::path::Path,
    targets: &[Target],
    want_antigravity: bool,
    antigravity_cfg: Option<&config::AntigravityCfg>,
) -> Result<Vec<AccountReport>> {
    let clients = http::clients()?;
    let mut jobs: Vec<Job> = Vec::new();

    // Chrome Cookie job(Claude/Codex)。必要な場合だけ Keychain に触る。
    // `--only antigravity` では prompt 自体を避ける。
    let wants_chrome = targets.iter().any(|t| t.wants.any());
    if wants_chrome {
        let password = cookies::safe_storage_key("Chrome Safe Storage")?;
        let key = cookies::derive_key(&password);
        for t in targets {
            let Some(db) = profiles::cookies_db(root, &t.profile.dir) else {
                continue;
            };
            let Ok(pc) = cookies::load(&db, &key) else {
                continue;
            };
            if t.wants.claude && claude::has_session(&pc.claude) {
                jobs.push(Job {
                    meta: JobMeta {
                        profile_name: t.profile.name.clone(),
                        profile_email: t.profile.email.clone(),
                        label: t.label.clone(),
                        provider: Provider::Claude,
                    },
                    auth: AuthMaterial::BrowserCookies(pc.claude),
                });
            }
            if t.wants.codex && codex::has_session(&pc.chatgpt) {
                jobs.push(Job {
                    meta: JobMeta {
                        profile_name: t.profile.name.clone(),
                        profile_email: t.profile.email.clone(),
                        label: t.label.clone(),
                        provider: Provider::Codex,
                    },
                    auth: AuthMaterial::BrowserCookies(pc.chatgpt),
                });
            }
            if t.wants.pixellab && pixellab::has_session(&pc.pixellab) {
                jobs.push(Job {
                    meta: JobMeta {
                        profile_name: t.profile.name.clone(),
                        profile_email: t.profile.email.clone(),
                        label: t.label.clone(),
                        provider: Provider::PixelLab,
                    },
                    auth: AuthMaterial::BrowserCookies(pc.pixellab),
                });
            }
        }
    }

    // Antigravity job は Chrome profile に紐づかない単一 OAuth/local account。
    if want_antigravity {
        jobs.push(Job {
            meta: JobMeta {
                profile_name: "Antigravity".to_string(),
                profile_email: None,
                label: antigravity_cfg.and_then(|c| c.label.clone()),
                provider: Provider::Antigravity,
            },
            auth: AuthMaterial::GoogleOAuth(antigravity_cfg.cloned()),
        });
    }

    if jobs.is_empty() {
        bail!(
            "No signed-in Claude/Codex/PixelLab sessions or Antigravity token found. Sign in via \
             Chrome, run `agy`, or adjust --profile / --only / your config. (Try --list-profiles.)"
        );
    }

    let mut set = tokio::task::JoinSet::new();
    for (idx, job) in jobs.into_iter().enumerate() {
        let clients = clients.clone();
        set.spawn(async move {
            if idx > 0 {
                tokio::time::sleep(Duration::from_millis(150 * idx as u64)).await;
            }
            let rows = fetch_with_retry(&clients, job.meta.provider, &job.auth).await;
            (idx, job.meta, rows)
        });
    }

    // fetch 成功時は 1 行以上(Antigravity は model group ごと)、失敗時は error 行を
    // 1 行返す。job 順は維持する。
    let mut results: Vec<(usize, AccountReport)> = Vec::new();
    while let Some(joined) = set.join_next().await {
        let Ok((idx, meta, rows)) = joined else {
            continue;
        };
        match rows {
            Ok(rows) => {
                results.extend(rows.into_iter().map(|row| (idx, meta.report_for(row))));
            }
            Err(e) => results.push((idx, meta.error_report(e))),
        }
    }
    results.sort_by_key(|(idx, _)| *idx);
    Ok(results.into_iter().map(|(_, r)| r).collect())
}

/// Claude Code 設定 file の path。`$CLAUDE_CONFIG_DIR/.claude.json`、未設定なら
/// home 直下の `~/.claude.json`。
fn claude_config_path() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(|d| PathBuf::from(d).join(".claude.json"))
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".claude.json"))
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
    if no_color_flag || std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true)
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
        "# Generated by `ai-usage --init-config`. Edit freely:\n\
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
        out += &format!("match = {}", toml_str(&p.name));
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
    out
}

/// profile ごとに表示する provider を決める。global `--only` flag が最優先で、
/// 次に profile config の `providers`、未指定なら両方。
fn resolve_wants(cli: &Cli, cfg: Option<&config::ProfileCfg>) -> BrowserWants {
    // global `--only` が最優先。未指定(None)のときだけ config の providers、
    // それも無ければ全 provider true(既定)に fallback する。
    // Antigravity は Chrome provider をすべて false にする(別経路で取得)。
    match cli.only {
        Some(ProviderArg::Claude) => BrowserWants {
            claude: true,
            codex: false,
            pixellab: false,
        },
        Some(ProviderArg::Codex) => BrowserWants {
            claude: false,
            codex: true,
            pixellab: false,
        },
        Some(ProviderArg::Pixellab) => BrowserWants {
            claude: false,
            codex: false,
            pixellab: true,
        },
        Some(ProviderArg::Antigravity) => BrowserWants {
            claude: false,
            codex: false,
            pixellab: false,
        },
        None => cfg.map_or(BrowserWants::all(), |c| c.wants()),
    }
}

/// 表示する profile を label / provider filter 付きで解決する。
/// 優先順は `--profile` > config `[[profiles]]` > 全 auto-discover。
fn build_targets(all: Vec<Profile>, cli: &Cli, cfg: &config::Config) -> Vec<Target> {
    // 3 つの選択戦略で構築処理は同じ。選ばれる profile、順序、対応 config row だけが違う。
    let make = |profile: Profile, c: Option<&config::ProfileCfg>| Target {
        label: c.and_then(|c| c.label.clone()),
        wants: resolve_wants(cli, c),
        profile,
    };

    if !cli.profile.is_empty() {
        // --profile: discovery 順を保ち、指定 profile だけに絞る。
        all.into_iter()
            .filter(|p| {
                cli.profile
                    .iter()
                    .any(|w| w.eq_ignore_ascii_case(&p.name) || w.eq_ignore_ascii_case(&p.dir))
            })
            .map(|p| {
                let c = cfg.profiles.iter().find(|c| c.matches(&p.name, &p.dir));
                make(p, c)
            })
            .collect()
    } else if !cfg.profiles.is_empty() {
        // config 順: 各 [[profiles]] row を検出済み profile に照合する。
        cfg.profiles
            .iter()
            .filter_map(|c| {
                all.iter()
                    .find(|p| c.matches(&p.name, &p.dir))
                    .cloned()
                    .map(|p| make(p, Some(c)))
            })
            .collect()
    } else {
        // auto: 検出済み profile を discovery 順ですべて使う。
        all.into_iter().map(|p| make(p, None)).collect()
    }
}

/// `--list-profiles`: 検出 profile と Cookie store の有無を出力する。
fn list_profiles(root: &std::path::Path, all: &[Profile]) {
    for p in all {
        let note = if profiles::cookies_db(root, &p.dir).is_some() {
            ""
        } else {
            "  (no cookie store)"
        };
        println!(
            "{:<18} dir={:<12} {}{}",
            p.name,
            p.dir,
            p.email.as_deref().unwrap_or(""),
            note
        );
    }
}

/// `--init-config`: starter config を書き込む。既に存在する場合は stdout に出す。
fn write_init_config(root: &std::path::Path, all: &[Profile]) -> Result<()> {
    let text = generate_config(root, all);
    match config::default_path() {
        Some(p) if !p.exists() => {
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(&p, &text)?;
            eprintln!("Wrote starter config to {}", p.display());
        }
        Some(p) => {
            eprintln!(
                "Config already exists at {} — printing a fresh one to stdout (redirect to overwrite).",
                p.display()
            );
            print!("{text}");
        }
        None => print!("{text}"),
    }
    Ok(())
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
        _ => None,
    }
}

/// cached `--json` file から statusline を描画する。cache がない/不正な場合は何も出さず、
/// 次の描画で再生成される。
fn render_cached_statusline(
    path: &std::path::Path,
    cli: &Cli,
    cfg: &config::Config,
    active: Option<&render::ActiveTarget>,
) {
    let Ok(data) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(report) = serde_json::from_str::<report::Report>(&data) else {
        return;
    };
    render::statusline(&report, active, cli.sort, &statusline_opts(cli, cfg));
}

/// fresh に fetch した report を CLI flag に応じた format で描画する。
fn render_reports(
    cli: &Cli,
    cfg: &config::Config,
    reports: &[AccountReport],
    active: Option<&render::ActiveTarget>,
) {
    if cli.statusline {
        render::statusline(
            &report::Report::build(reports),
            active,
            cli.sort,
            &statusline_opts(cli, cfg),
        );
    } else if cli.json {
        render::json(&report::Report::build(reports), cli.sort);
    } else {
        render::table(reports, active, cli.sort, cli.debug);
    }
}

/// profile を検出し、info-only flag を処理してから usage を fetch/render する。
async fn run(cli: Cli) -> Result<()> {
    let cfg = config::load(cli.config.as_deref());
    let root = profiles::chrome_root()?;
    // `--only antigravity` は Chrome に触らないため、profile discovery と
    // fetch_reports 内の Keychain prompt を避ける。
    let all = if matches!(cli.only, Some(ProviderArg::Antigravity)) {
        Vec::new()
    } else {
        profiles::discover(&root)?
    };

    if cli.list_profiles {
        list_profiles(&root, &all);
        return Ok(());
    }
    if cli.init_config {
        return write_init_config(&root, &all);
    }

    let active = active_target(&cli, &cfg);

    // cached file から statusline を描画する場合は network も Keychain も使わない。
    if cli.statusline
        && let Some(path) = cli.input.as_deref()
    {
        render_cached_statusline(path, &cli, &cfg, active.as_ref());
        return Ok(());
    }

    let targets = build_targets(all, &cli, &cfg);
    let want_antigravity = match cli.only {
        Some(ProviderArg::Antigravity) => true,
        Some(_) => false,
        None => antigravity::available(cfg.antigravity.as_ref()),
    };
    let reports =
        fetch_reports(&root, &targets, want_antigravity, cfg.antigravity.as_ref()).await?;

    render_reports(&cli, &cfg, &reports, active.as_ref());
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    run(Cli::parse()).await
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let cli = Cli::parse_from(["ai-usage", "--only", "codex"]);
        // config で 3 種全て true でも、--only codex なら codex だけ。
        assert_eq!(resolve_wants(&cli, Some(&cfg)), bw(false, true, false));

        let cli = Cli::parse_from(["ai-usage", "--only", "claude"]);
        assert_eq!(resolve_wants(&cli, Some(&cfg)), bw(true, false, false));

        let cli = Cli::parse_from(["ai-usage", "--only", "pixellab"]);
        assert_eq!(resolve_wants(&cli, Some(&cfg)), bw(false, false, true));

        // --only antigravity は Chrome 系 provider をすべて false にする(Antigravity は別経路)。
        let cli = Cli::parse_from(["ai-usage", "--only", "antigravity"]);
        assert_eq!(resolve_wants(&cli, Some(&cfg)), bw(false, false, false));
    }

    #[test]
    fn resolve_wants_falls_back_to_config_then_all() {
        // --only 無し → config の providers。
        let cfg = config::ProfileCfg {
            matcher: "Work".to_string(),
            label: None,
            providers: Some(vec!["claude".to_string(), "pixellab".to_string()]),
        };
        let cli = Cli::parse_from(["ai-usage"]);
        assert_eq!(resolve_wants(&cli, Some(&cfg)), bw(true, false, true));

        // config も無ければ全 provider true(既定)。
        assert_eq!(resolve_wants(&cli, None), BrowserWants::all());
    }
}
