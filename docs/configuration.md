# Configuration

`ai-usage` needs **no configuration**: it auto-discovers every Chrome profile that has a Claude, Codex, or PixelLab session, plus available Antigravity and Grok OAuth credentials. To pin *which* profiles appear, rename them, or limit providers, put a file at **`~/.config/ai-usage/config.toml`** (or `$XDG_CONFIG_HOME/ai-usage/config.toml`). `--config <PATH>` reads another file instead.

## Initial setup

Generate a starter config from your current sessions:

```bash
ai-usage --init-config
```

A template also lives at [`config.example.toml`](../config.example.toml).

## Example configuration

```toml
# Optional: highlight this account as active (default: auto-detected from
# CLAUDE_CONFIG_DIR/.claude.json — the Claude Code session's account).
# active_email = "home@example.com"

# Listing any [[profiles]] shows ONLY those, in this order.
[[profiles]]
match = "Work"                    # Chrome display name, or on-disk dir e.g. "Default"
label = "work"                    # optional: shown instead of the account email username
# providers = ["claude", "codex"] # optional Chrome-provider subset; default = all

[[profiles]]
match = "Home"
label = "home"

# Antigravity (Google's `agy`). Auto-discovered from ~/.gemini, Antigravity.app,
# or a running `agy` — config is optional. Use it only to relabel, pin a
# non-default token, or disable the row.
[antigravity]
# enabled = true                    # false to hide even when detected
label = "antigravity"               # optional row label
# token_path = "~/.gemini/antigravity-cli/antigravity-oauth-token"

# Grok (xAI's `grok` CLI). Auto-discovered when ~/.grok/auth.json exists
# (written by `grok login`). Config is optional and mirrors [antigravity].
[grok]
# enabled = true                    # false to hide even when detected
label = "grok"                      # optional row label
# auth_path = "~/.grok/auth.json"

# Statusline-only display filter. Hides rows from `--statusline` output while
# keeping them in `--json` / table (so scripts and manual checks still see them).
# Overridden by `--statusline-hide` on the CLI.
[statusline]
hide = ["antigravity"]              # subset of: claude / codex / antigravity / pixellab / grok
```

## Configuration options

| Option | Description | Default |
|--------|-------------|---------|
| `active_email` | Highlight this account's Claude row as active | Auto-detected from `CLAUDE_CONFIG_DIR/.claude.json` (or `~/.claude.json`) |
| `[[profiles]]` | Explicit list of profiles to show (empty = auto-discover all) | `[]` (auto) |
| `profiles[].match` | Chrome display name or on-disk directory (e.g. `Default`) | Required |
| `profiles[].label` | Display label instead of the account email username | Email username |
| `profiles[].providers` | Chrome-provider subset to show for this profile | Claude / Codex / PixelLab |
| `[antigravity].enabled` | Show the Antigravity row when detected | `true` |
| `[antigravity].label` | Row label for Antigravity | `antigravity` |
| `[antigravity].token_path` | Non-default OAuth token path | `~/.gemini/…` |
| `[grok].enabled` | Show the Grok row when detected | `true` |
| `[grok].label` | Row label for Grok | `grok` |
| `[grok].auth_path` | Non-default `auth.json` path | `~/.grok/auth.json` |
| `[statusline].hide` | Providers to omit from `--statusline` (still in `--json` / table) | `[]` |

Precedence: **CLI flags > config file > auto-detection**.
