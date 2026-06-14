//! ai-usage — show Claude & Codex usage limits (5-hour / weekly windows and
//! their reset times) across the Chrome profiles you're signed into.

mod antigravity;
mod claude;
mod codex;
mod config;
mod cookies;
mod http;
mod model;
mod profiles;
mod render;
mod report;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use clap::{Parser, ValueEnum};

use model::{AccountReport, Provider, UsageRow};
use profiles::Profile;

#[derive(Parser)]
#[command(
    name = "ai-usage",
    version,
    about = "Show Claude & Codex usage limits across Chrome profiles"
)]
struct Cli {
    /// Limit to these Chrome profile display names (e.g. -p Work,Home)
    #[arg(short, long, value_delimiter = ',')]
    profile: Vec<String>,

    /// Limit to a single provider
    #[arg(long, value_enum)]
    only: Option<ProviderArg>,

    /// Emit JSON instead of a table
    #[arg(long)]
    json: bool,

    /// Emit the compact, colored statusline (one account per line)
    #[arg(long)]
    statusline: bool,

    /// In the statusline, replace the "Claude"/"Codex" labels with brand-logo
    /// glyphs (requires the BrandLogos font; see github.com/owayo/brand-logo-font).
    #[arg(long)]
    logos: bool,

    /// Read accounts from this JSON file (a cached `--json` output) instead of
    /// fetching over the network. Used by the statusline for fast rendering.
    #[arg(long, value_name = "PATH")]
    input: Option<PathBuf>,

    /// Email of the account to highlight as active (default: read from
    /// CLAUDE_CONFIG_DIR/.claude.json).
    #[arg(long, value_name = "EMAIL")]
    active_email: Option<String>,

    /// Disable ANSI color.
    #[arg(long)]
    no_color: bool,

    /// Use this config file instead of ~/.config/ai-usage/config.toml
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Generate a starter config from the currently signed-in profiles, then exit
    #[arg(long)]
    init_config: bool,

    /// List discovered Chrome profiles and exit
    #[arg(long)]
    list_profiles: bool,
}

#[derive(ValueEnum, Clone, Copy)]
enum ProviderArg {
    Claude,
    Codex,
    Antigravity,
}

/// What a job authenticates with: Chrome cookies (Claude/Codex) or the local
/// Google OAuth token / running `agy` (Antigravity).
enum AuthMaterial {
    BrowserCookies(HashMap<String, String>),
    GoogleOAuth(Option<config::AntigravityCfg>),
}

struct Job {
    profile_name: String,
    profile_email: Option<String>,
    label: Option<String>,
    provider: Provider,
    auth: AuthMaterial,
}

/// A Chrome profile resolved for display: its label and which providers to show.
struct Target {
    profile: Profile,
    label: Option<String>,
    want_claude: bool,
    want_codex: bool,
}

/// Cloudflare occasionally challenges an otherwise-valid request; retry a few
/// times with backoff before giving up on an account.
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

/// Decrypt cookies for the target profiles and fetch every signed-in account
/// concurrently. Results preserve the input order.
async fn fetch_reports(
    root: &std::path::Path,
    targets: &[Target],
    want_antigravity: bool,
    antigravity_cfg: Option<&config::AntigravityCfg>,
) -> Result<Vec<AccountReport>> {
    let clients = http::clients()?;
    let mut jobs: Vec<Job> = Vec::new();

    // Chrome cookie jobs (Claude/Codex). Touch the Keychain only if something
    // actually wants them — `--only antigravity` skips the prompt entirely.
    let wants_chrome = targets.iter().any(|t| t.want_claude || t.want_codex);
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
            if t.want_claude && claude::has_session(&pc.claude) {
                jobs.push(Job {
                    profile_name: t.profile.name.clone(),
                    profile_email: t.profile.email.clone(),
                    label: t.label.clone(),
                    provider: Provider::Claude,
                    auth: AuthMaterial::BrowserCookies(pc.claude),
                });
            }
            if t.want_codex && codex::has_session(&pc.chatgpt) {
                jobs.push(Job {
                    profile_name: t.profile.name.clone(),
                    profile_email: t.profile.email.clone(),
                    label: t.label.clone(),
                    provider: Provider::Codex,
                    auth: AuthMaterial::BrowserCookies(pc.chatgpt),
                });
            }
        }
    }

    // Antigravity job — a single OAuth/local account, not tied to a Chrome profile.
    if want_antigravity {
        jobs.push(Job {
            profile_name: "Antigravity".to_string(),
            profile_email: None,
            label: antigravity_cfg.and_then(|c| c.label.clone()),
            provider: Provider::Antigravity,
            auth: AuthMaterial::GoogleOAuth(antigravity_cfg.cloned()),
        });
    }

    if jobs.is_empty() {
        bail!(
            "No signed-in Claude/Codex sessions or Antigravity token found. Sign in via Chrome, \
             run `agy`, or adjust --profile / --only / your config. (Try --list-profiles.)"
        );
    }

    let mut set = tokio::task::JoinSet::new();
    for (idx, job) in jobs.into_iter().enumerate() {
        let clients = clients.clone();
        set.spawn(async move {
            if idx > 0 {
                tokio::time::sleep(Duration::from_millis(150 * idx as u64)).await;
            }
            let rows = fetch_with_retry(&clients, job.provider, &job.auth).await;
            (
                idx,
                job.profile_name,
                job.profile_email,
                job.label,
                job.provider,
                rows,
            )
        });
    }

    // A successful fetch yields one or more rows (Antigravity: one per model
    // group); a failure yields a single error row. Preserve job order.
    let mut results: Vec<(usize, AccountReport)> = Vec::new();
    while let Some(joined) = set.join_next().await {
        let Ok((idx, pname, pemail, label, provider, rows)) = joined else {
            continue;
        };
        match rows {
            Ok(rows) => {
                for row in rows {
                    results.push((
                        idx,
                        AccountReport {
                            profile_name: pname.clone(),
                            profile_email: pemail.clone(),
                            label: label.clone(),
                            provider,
                            group_label: row.group_label,
                            usage: Ok(row.usage),
                        },
                    ));
                }
            }
            Err(e) => results.push((
                idx,
                AccountReport {
                    profile_name: pname,
                    profile_email: pemail,
                    label,
                    provider,
                    group_label: None,
                    usage: Err(e),
                },
            )),
        }
    }
    results.sort_by_key(|(idx, _)| *idx);
    Ok(results.into_iter().map(|(_, r)| r).collect())
}

/// The account email this session is signed in as (for active-row highlighting).
fn active_claude_email() -> Option<String> {
    // Claude Code stores settings in `$CLAUDE_CONFIG_DIR/.claude.json`, or
    // `~/.claude.json` at the home root when that variable is unset.
    let path = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(|d| PathBuf::from(d).join(".claude.json"))
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".claude.json"));
    let data = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    v.pointer("/oauthAccount/emailAddress")
        .and_then(|e| e.as_str())
        .map(str::to_string)
}

fn color_enabled(no_color_flag: bool) -> bool {
    if no_color_flag || std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true)
}

/// Build a starter `config.toml` from the profiles currently signed in (detected
/// by cookie presence — no network, no Keychain).
fn generate_config(root: &std::path::Path, all: &[Profile]) -> String {
    let mut out = String::from(
        "# Generated by `ai-usage --init-config`. Edit freely:\n\
         # reorder, drop profiles you don't want, or change labels.\n\n",
    );
    if let Some(active) = active_claude_email() {
        out += &format!("active_email = \"{active}\"\n\n");
    }
    for p in all {
        let Some(db) = profiles::cookies_db(root, &p.dir) else {
            continue;
        };
        let (claude, codex) = cookies::detect_sessions(&db);
        if !claude && !codex {
            continue;
        }
        let email = p.email.as_deref().unwrap_or("");
        let label = email
            .split('@')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(&p.name);
        out += "[[profiles]]\n";
        out += &format!("match = \"{}\"", p.name);
        if !email.is_empty() {
            out += &format!("   # {email}");
        }
        out += "\n";
        out += &format!("label = \"{label}\"\n");
        if !(claude && codex) {
            let only = if claude { "claude" } else { "codex" };
            out += &format!("providers = [\"{only}\"]\n");
        }
        out += "\n";
    }
    out
}

/// Which providers to show for a profile: a global `--only` flag wins; otherwise
/// the profile's config `providers`; otherwise both.
fn resolve_wants(cli: &Cli, cfg: Option<&config::ProfileCfg>) -> (bool, bool) {
    if cli.only.is_some() {
        (
            matches!(cli.only, Some(ProviderArg::Claude)),
            matches!(cli.only, Some(ProviderArg::Codex)),
        )
    } else if let Some(c) = cfg {
        c.wants()
    } else {
        (true, true)
    }
}

/// Resolve which profiles to show (with labels + provider filters).
/// Precedence: `--profile` > config `[[profiles]]` > auto-discover all.
fn build_targets(all: Vec<Profile>, cli: &Cli, cfg: &config::Config) -> Vec<Target> {
    // Construction is identical across the three selection strategies; only which
    // profiles are chosen, in what order, and with which config row differs.
    let make = |profile: Profile, c: Option<&config::ProfileCfg>| {
        let (want_claude, want_codex) = resolve_wants(cli, c);
        Target {
            label: c.and_then(|c| c.label.clone()),
            want_claude,
            want_codex,
            profile,
        }
    };

    if !cli.profile.is_empty() {
        // --profile: discovery order, filtered to the named profiles.
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
        // Config order: each [[profiles]] row, matched against a discovered profile.
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
        // Auto: every discovered profile, in discovery order.
        all.into_iter().map(|p| make(p, None)).collect()
    }
}

/// `--list-profiles`: print discovered profiles and whether each has a cookie store.
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

/// `--init-config`: write a starter config, or print it to stdout if one exists.
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

/// Render the statusline from a cached `--json` file. A missing or invalid cache
/// prints nothing — the next draw repopulates it.
fn render_cached_statusline(path: &std::path::Path, cli: &Cli, active: Option<&str>) {
    let Ok(data) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(report) = serde_json::from_str::<report::Report>(&data) else {
        return;
    };
    render::statusline(&report, active, color_enabled(cli.no_color), cli.logos);
}

/// Render freshly fetched reports in the format selected by the CLI flags.
fn render_reports(cli: &Cli, reports: &[AccountReport], active: Option<&str>) {
    if cli.statusline {
        render::statusline(
            &report::Report::build(reports),
            active,
            color_enabled(cli.no_color),
            cli.logos,
        );
    } else if cli.json {
        render::json(&report::Report::build(reports));
    } else {
        render::table(reports, active);
    }
}

/// Discover profiles, dispatch the info-only flags, then fetch and render usage.
async fn run(cli: Cli) -> Result<()> {
    let cfg = config::load(cli.config.as_deref());
    let root = profiles::chrome_root()?;
    // `--only antigravity` never touches Chrome — skip profile discovery (and,
    // in fetch_reports, the Keychain prompt).
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

    let active = cli
        .active_email
        .clone()
        .or_else(|| cfg.active_email.clone())
        .or_else(active_claude_email);

    // Statusline rendered from a cached file: no network, no Keychain.
    if cli.statusline
        && let Some(path) = cli.input.as_deref()
    {
        render_cached_statusline(path, &cli, active.as_deref());
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

    render_reports(&cli, &reports, active.as_deref());
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    run(Cli::parse()).await
}
