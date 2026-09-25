# CLI reference

Every command and option of `ai-usage`. The everyday examples are in the [README](../README.md#usage).

## Commands

| Command | Description |
|---------|-------------|
| `ai-usage` | Show usage for all signed-in profiles and providers |
| `ai-usage --init-config` | Generate a starter config from currently signed-in sessions |
| `ai-usage --list-profiles` | List discovered Chrome profiles |

## Options

### Filtering

| Option | Short | Description |
|--------|-------|-------------|
| `--profile <NAMES>` | `-p` | Comma-separated profile names (Chrome display name or on-disk dir) |
| `--only <PROVIDER>` | | Show only `claude`, `codex`, `antigravity`, `pixellab`, or `grok` |

### Output

| Option | Description |
|--------|-------------|
| `--json` | Machine-readable JSON output |
| `--statusline` | Compact one-line-per-account output for status bars |
| `--statusline --logos` | With brand-logo glyphs (requires the BrandLogos font) |
| `--statusline --compact` | Half-width gauge for narrow panes |
| `--statusline --reset-at` | Append the long-window reset clock-time, e.g. `(06/18 01:10)` |
| `--statusline-hide <PROVIDERS>` | Comma-separated providers to skip in statusline only (`--json` / table unaffected). E.g. `--statusline-hide antigravity,codex` |
| `--sort weekly-usage` | Rank rows by long-window utilization (closest to the cap first) |
| `--sort weekly-reset` | Rank rows by long-window reset time (soonest first) |
| `--no-color` | Disable ANSI colors. Colors are also suppressed when `NO_COLOR` holds a non-empty value (per [no-color.org](https://no-color.org/)) or `TERM=dumb` |
| `--input <PATH>` | Render the statusline from a cached `--json` file instead of fetching. Touches neither Chrome, Keychain, nor the network — used for fast status-bar redraws |

### Active row selection

| Option | Description |
|--------|-------------|
| `--active-email <EMAIL>` | Match the signed-in email of a Claude row (default: `$CLAUDE_CONFIG_DIR/.claude.json`, falling back to `~/.claude.json` when that variable is unset or empty) |
| `--active-profile <NAME>` | Match a profile by name |
| `--active-provider <NAME>` | Pin to a single provider: `claude`, `codex`, `antigravity`, `pixellab`, or `grok` |

### Config, debug and info

| Option | Description |
|--------|-------------|
| `--config <PATH>` | Use this config file instead of `~/.config/ai-usage/config.toml` |
| `--debug` | Print per-row match decisions to stderr as JSONL (stdout stays clean for pipes) |
| `--help` | Print help |
| `--version` | Print version |
