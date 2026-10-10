# CLI reference

Every command and option of `ai-usage`. The everyday examples are in the [README](../README.md#usage).

## Commands

| Command | Description |
|---------|-------------|
| `ai-usage` | Show usage for all signed-in profiles and providers |
| `ai-usage --init-config` | Generate a starter config from currently signed-in sessions; `--config <PATH>` chooses its destination |
| `ai-usage --list-profiles` | List discovered Chrome profiles |

## Options

### Filtering

| Option | Short | Description |
|--------|-------|-------------|
| `--profile <NAMES>` | `-p` | Comma-separated profile names (Chrome display name or on-disk dir) |
| `--only <PROVIDER>` | | Show only `claude`, `codex`, `antigravity`, `pixellab`, or `grok`; also applies to cached statusline and TUI output |

### Output

| Option | Description |
|--------|-------------|
| `--json` | Machine-readable JSON output |
| `--statusline` | Compact one-line-per-account output for status bars |
| `--tui` | Interactive terminal dashboard; cannot be combined with `--json` or `--statusline` |
| `--statusline --logos` | With brand-logo glyphs (requires the BrandLogos font) |
| `--statusline --compact` | Half-width gauge for narrow panes |
| `--statusline --reset-at` | Append the long-window reset clock-time, e.g. `(06/18 01:10)` |
| `--statusline-hide <PROVIDERS>` | Comma-separated providers to skip in statusline only (`--json` / table unaffected). E.g. `--statusline-hide antigravity,codex` |
| `--sort weekly-usage` | Rank rows by long-window utilization (closest to the cap first) |
| `--sort weekly-reset` | Rank rows by long-window reset time (soonest first) |
| `--no-color` | Disable ANSI colors. Colors are also suppressed when `NO_COLOR` holds a non-empty value (per [no-color.org](https://no-color.org/)) or `TERM=dumb` |
| `--input <PATH>` | With `--statusline` or `--tui`, render a cached `--json` file without accessing Chrome, Keychain, or the network |

### Active row selection

| Option | Description |
|--------|-------------|
| `--active-email <EMAIL>` | Match the signed-in email of a statusline Claude row (default: `$CLAUDE_CONFIG_DIR/.claude.json`, falling back to `~/.claude.json` when that variable is unset or empty) |
| `--active-profile <NAME>` | Match a statusline row by profile name |
| `--active-provider <NAME>` | Pin statusline matching to one provider: `claude`, `codex`, `antigravity`, `pixellab`, or `grok` |

### Config, debug and info

| Option | Description |
|--------|-------------|
| `--config <PATH>` | Use this config file instead of `~/.config/ai-usage/config.toml`; with `--init-config`, create it without overwriting an existing file |
| `--debug` | Print statusline per-row match decisions to stderr as JSONL (stdout stays clean for pipes) |
| `--help` | Print help |
| `--version` | Print version |

If the `--init-config` destination already exists, the template is printed to stdout instead. Cached output uses the accounts and labels recorded in the JSON file; set profile selection and labels when generating the cache. `--only` and statusline hiding still apply when rendering it.

When the reader of stdout stops early, as in `ai-usage --json | head -n 5`, the remaining output is discarded and the command exits with status 0.

## Manual usage resets

Claude and Codex rows also show the remaining manual usage resets and their expiry times. The table adds a **Manual resets** column with separate dates, such as `full 2 (1@10/23 05:27, 1@10/30 03:57)`; `1@…` means one reset expires at that date and time. The statusline appends only the total counts and nearest expiry: `R:full 2 (10/23 05:27)`. Only the date text turns red when fewer than seven days remain. Exactly seven days keeps the normal color, and color suppression settings such as `--no-color` still apply. `full` restores both usage windows, `5h` restores the session window, and `1w` restores the weekly window. All displayed dates use local time.

`0` means no available resets remain; `?` means the count or expiry could not be obtained. Temporarily paused grants retain a separate count, such as `full paused 1`. The statusline prefers available grants when selecting an expiry and falls back to paused grants only when no available grants remain. It shows the nearest known date, `(?)` when all candidate dates are unknown, and no date when counts are all zero or unknown. Expired resets are excluded even when rendering an older cache. This reports availability only and never uses a reset.

With `--reset-at`, rows that cannot show a long-window reset time (unknown, already passed, or no long window) leave the same width blank, so `R:` starts in the same column on every row.

JSON accounts carry an optional `manual_resets` array with `kind`, `remaining` (integer or `null` for unknown), `expires_at` (RFC 3339 or `null`), and `paused` (defaults to `false` when absent). Older caches without this field continue to render without the reset suffix.
