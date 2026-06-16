# ai-usage

[![CI](https://github.com/owayo/ai-usage/actions/workflows/ci.yml/badge.svg)](https://github.com/owayo/ai-usage/actions/workflows/ci.yml)
&nbsp;[日本語](README.ja.md)

One command to see your **Claude** and **OpenAI Codex (ChatGPT)** usage limits — the
rolling **5-hour** window and the **weekly** window, plus when each resets —
across every Chrome profile you're signed into.

It reads each Chrome profile's session straight from the browser, so it can report
**multiple accounts at once** (e.g. a `Work` and an `Home` profile, each with both a
Claude and a Codex subscription = four accounts) without you logging anything in or out.

```
┌─────────┬─────────┬──────────┬──────────────────────────┬──────────────────────────┐
│ Account ┆ Service ┆ Plan     ┆ 5-hour                   ┆ Weekly (7-day)           │
╞═════════╪═════════╪══════════╪══════════════════════════╪══════════════════════════╡
│ work    ┆ Claude  ┆ max      ┆ █░░░░░░░░░    4%  · in 2h ┆ █░░░░░░░░░    3%  · in 4d │
│ work    ┆ Codex   ┆ team     ┆ █░░░░░░░░░    1%  · in 5h ┆ ░░░░░░░░░░    0%  · in 7d │
│ home    ┆ Claude  ┆ max      ┆ █░░░░░░░░░   12%  · in 1h ┆ █░░░░░░░░░    3%  · in 5d │
│ home    ┆ Codex   ┆ prolite  ┆ █░░░░░░░░░   10%  · in 4h ┆ ███░░░░░░░   31%  · in 4d │
└─────────┴─────────┴──────────┴──────────────────────────┴──────────────────────────┘
  updated 21:46 · bars = usage, time = until reset
```

## Why

Claude Code (`/usage`) and Codex (`/status`) each only show the **one** account they're
logged into. If you switch between work accounts in different Chrome profiles, there's no
single place to see where you stand on all of them. `ai-usage` is that place.

## How it works

For each Chrome profile it finds, `ai-usage`:

1. Decrypts the profile's cookies from `~/Library/Application Support/Google/Chrome/<profile>/Cookies`
   using the **Chrome Safe Storage** key from your macOS Keychain (the standard `v10`
   AES‑128‑CBC scheme).
2. **Claude** — uses the `sessionKey` cookie to call
   `claude.ai/api/organizations/{org}/usage` → `five_hour` / `seven_day` `{utilization, resets_at}`.
3. **Codex** — uses the `__Secure-next-auth.session-token` cookie to exchange for a Bearer
   token via `chatgpt.com/api/auth/session`, then calls `chatgpt.com/backend-api/wham/usage`
   → `rate_limit.primary_window` / `secondary_window`.

**Antigravity** (Google's `agy` CLI / IDE) has no Chrome cookie: `ai-usage` reads
the OAuth token from `~/.gemini` (refreshing it as needed), and when `agy` is
running it prefers the richer localhost quota server. It reports the **Gemini** and
**Claude & GPT** model-group weekly limits — the same numbers as `agy`'s `/usage`.

Both `claude.ai` and `chatgpt.com` sit behind Cloudflare, so the HTTP client
([`wreq`](https://crates.io/crates/wreq)) emulates Chrome's TLS/HTTP2 fingerprint and
replays the profile's `cf_clearance` cookie — a plain HTTP client just gets a `403`.

Nothing leaves your machine except the same authenticated requests your browser already
makes to Anthropic and OpenAI. No tokens or cookies are printed or stored.

## Install

Requires the Rust toolchain and (to build [`wreq`](https://crates.io/crates/wreq)'s
BoringSSL) **cmake**:

```sh
brew install cmake
cargo install --path .
# or: cargo build --release  →  ./target/release/ai-usage
```

## Usage

```sh
ai-usage                      # all signed-in profiles, both services
ai-usage -p Work,Home         # only these profiles
ai-usage --only claude        # only Claude (or: --only codex / antigravity)
ai-usage --json               # machine-readable output
ai-usage --statusline         # compact one-line-per-account output (for status bars)
ai-usage --statusline --logos # … with brand-logo glyphs (needs the BrandLogos font)
ai-usage --statusline --compact   # … with a half-width gauge for narrow panes
ai-usage --statusline --reset-at  # … and append the weekly reset clock-time, e.g. (06/18 01:10)
ai-usage --list-profiles      # show discovered Chrome profiles
```

Pick which row is shown as "active" (highlighted in red) with one of:

- `--active-email <EMAIL>` — match the signed-in email of a Claude row (default
  source: `$CLAUDE_CONFIG_DIR/.claude.json`, the Claude Code session account)
- `--active-profile <NAME>` — match a profile by name; pin to a single provider
  with `--active-provider claude|codex|antigravity`
- `--debug` — print per-row match decisions to stderr as JSONL (stdout is left
  clean so a piped statusline keeps rendering)

The **first run** triggers a macOS Keychain prompt
(*"… wants to use the 'Chrome Safe Storage' key"*) — choose **Always Allow**.

## Configuration

`ai-usage` needs **no configuration** — it auto-discovers every Chrome profile that has a
Claude or Codex session and shows them all. To pin *which* profiles appear, rename them, or
limit providers, drop a file at **`~/.config/ai-usage/config.toml`**
(or `$XDG_CONFIG_HOME/ai-usage/config.toml`):

```toml
# Optional: highlight this account as active (default: auto-detected from
# CLAUDE_CONFIG_DIR/.claude.json — the Claude Code session's account).
# active_email = "home@example.com"

# Listing any [[profiles]] shows ONLY those, in this order.
[[profiles]]
match = "Work"                  # Chrome display name, or on-disk dir e.g. "Default"
label = "work"                     # optional: shown instead of the account email username
# providers = ["claude", "codex"] # optional: subset to show; default = both

[[profiles]]
match = "Home"
label = "home"
```

Or generate one from your current sessions: **`ai-usage --init-config`**.
Precedence: **CLI flags > config file > auto-detection**. A starter also lives at
[`config.example.toml`](config.example.toml).

## Development

```sh
make build      # debug build
make release    # optimized release build
make install    # build + install to ~/.local/bin
make check      # clippy (-D warnings) + rustfmt check
make test       # run tests
make deps       # install build prerequisites (cmake)
```

Releases are cut from the GitHub Actions **Release** workflow (`workflow_dispatch`); CI runs
`clippy` / `fmt` / `test` / build on macOS (Intel + ARM).

## Notes & limitations

- **macOS + Google Chrome only**, for now. (Chrome stores cookies with the `v10` scheme on
  macOS; Windows' `v20` app-bound encryption is not handled.)
- If a `cf_clearance` cookie has gone stale you'll see a *Cloudflare challenge* error for
  that one account — open the relevant site once in that Chrome profile to refresh it, then
  re-run. Other accounts are unaffected.
- The usage endpoints are **undocumented / reverse-engineered** and may change.
- This tool depends on `wreq-util`, which is **GPL‑3.0**; this project is therefore licensed
  GPL‑3.0.

## Acknowledgements

**Antigravity** (Google's `agy` CLI / IDE) usage support follows the
reverse-engineering in [CodexBar](https://github.com/steipete/CodexBar)'s
Antigravity provider — see its
[implementation notes](https://github.com/steipete/CodexBar/blob/main/docs/antigravity.md).
