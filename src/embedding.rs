//! Structured, non-printing entry point for applications. The caller owns Tokio.
#![deny(clippy::print_stdout, clippy::print_stderr, clippy::exit)]

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use serde::Serialize;

use crate::{BrowserWants, Provider, cache, config, diagnostics, profiles, report};

#[derive(Clone, Debug)]
pub struct CollectOptions {
    pub config_path: Option<PathBuf>,
    pub cache_ttl: Duration,
    pub refresh: bool,
}

impl Default for CollectOptions {
    fn default() -> Self {
        Self {
            config_path: None,
            cache_ttl: Duration::from_secs(120),
            refresh: false,
        }
    }
}

#[derive(Serialize)]
pub struct Snapshot {
    pub report: report::Report,
    pub warnings: Vec<String>,
}

/// Uses the CLI's configuration, credential handling, cache and provider fetchers.
/// Does not parse host arguments, print, install global handlers or create a runtime.
/// May prompt for Keychain access; call only after an explicit user action.
pub async fn collect(options: CollectOptions) -> Result<Snapshot> {
    // Parsing a fixed argument vector shares CLI defaults without reading host argv
    // or using Parser::parse (which can exit the hosting application).
    let cli = crate::Cli::try_parse_from(["ai-usage"])?;
    let diagnostics = diagnostics::Diagnostics::new(true);
    let cfg = config::load_with_diagnostics(options.config_path.as_deref(), &diagnostics);
    let excluded = crate::excluded_providers(&cli, &cfg);
    let all_excluded = [
        Provider::Claude,
        Provider::Codex,
        Provider::Antigravity,
        Provider::PixelLab,
        Provider::Grok,
    ]
    .iter()
    .all(|p| excluded.contains(p));
    if all_excluded {
        return Ok(Snapshot {
            report: report::Report::build(&[]),
            warnings: diagnostics.take(),
        });
    }
    let root = profiles::chrome_root()?;
    let all = if BrowserWants::all()
        .without(crate::excluded_browser_wants(&excluded))
        .any()
    {
        profiles::discover(&root).unwrap_or_else(|error| {
            diagnostics.warn(format!("ai-usage: skipping Chrome profiles: {error:#}"));
            Vec::new()
        })
    } else {
        Vec::new()
    };
    let targets = crate::filter_targets(crate::build_targets(all, &cli, &cfg), &excluded);
    let antigravity = !excluded.contains(&Provider::Antigravity)
        && crate::antigravity::available(cfg.antigravity.as_ref()).await;
    let grok = !excluded.contains(&Provider::Grok) && crate::grok::available(cfg.grok.as_ref());
    let cache = cache::Cache::new(options.cache_ttl, options.refresh, diagnostics.clone())?;
    let reports = crate::fetch_reports(
        &root,
        &targets,
        antigravity,
        cfg.antigravity.as_ref(),
        grok,
        cfg.grok.as_ref(),
        &cache,
    )
    .await?;
    Ok(Snapshot {
        report: report::Report::build(&reports),
        warnings: diagnostics.take(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn excluded_providers_do_not_touch_credentials_or_network() {
        let path = std::env::temp_dir().join(format!("ai-usage-embed-{}.toml", std::process::id()));
        std::fs::write(
            &path,
            "[providers]\nexclude = ['claude', 'codex', 'antigravity', 'pixellab', 'grok']\n",
        )
        .unwrap();
        let result = collect(CollectOptions {
            config_path: Some(path.clone()),
            ..Default::default()
        })
        .await;
        std::fs::remove_file(path).unwrap();
        let snapshot = result.unwrap();
        assert!(snapshot.report.accounts.is_empty());
        assert!(snapshot.warnings.is_empty());
        assert!(serde_json::to_value(snapshot).unwrap()["report"]["accounts"].is_array());
    }
}
