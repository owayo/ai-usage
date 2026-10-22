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
| `--tui` | Interactive terminal dashboard; refreshes usage every minute and shows manual-reset expiries and a [disk usage](architecture.md#disk-usage) panel. Cannot be combined with `--json` or `--statusline` |
| `--no-disk` | Hide the disk usage panel in `--tui`. Takes precedence over `[tui].disk` in the config |
| `--statusline --logos` | With brand-logo glyphs (requires the BrandLogos font) |
| `--statusline --compact` | Half-width gauge for narrow panes |
| `--statusline --reset-at` | Append the long-window reset clock-time, e.g. `(06/18 01:10)` |
| `--statusline-hide <PROVIDERS>` | Comma-separated providers to skip in statusline only (`--json` / table unaffected). E.g. `--statusline-hide antigravity,codex` |
| `--sort weekly-usage` | Rank rows by long-window utilization (closest to the cap first) |
| `--sort weekly-reset` | Rank rows by long-window reset time (soonest first) |
| `--no-color` | Disable ANSI colors. Colors are also suppressed when `NO_COLOR` holds a non-empty value (per [no-color.org](https://no-color.org/)) or `TERM=dumb` |
| `--input <PATH>` | With `--statusline` or `--tui`, render a cached `--json` file without accessing Chrome, Keychain, or the network. TUI rereads it every minute; its disk panel still reads this Mac's volumes live |

### Shared usage cache

| Option | Description |
|--------|-------------|
| `--cache-ttl <SECONDS>` | Reuse each source's usage for this many seconds (default: `120`). `0` skips reuse of an earlier result while still coalescing overlapping fetches |
| `--refresh` | Ignore an earlier cached result. An update completed after this request began is shared with concurrent requests |

The table, JSON, statusline, and live TUI share results per provider and normalized credential-source path. Output settings, profile labels, and `--only` do not create separate caches. Only selected sources are fetched; a broad request can reuse sources previously fetched with `--only`. The TUI checks for updates every minute and applies the same cache lifetime. Chrome cache hits read session-cookie fingerprints without decrypting cookies or accessing Keychain. Changing session cookies, the active Claude organization, or an OAuth credential file invalidates that source's result. Crossing a known quota reset also expires it.

Errors are shared for up to 15 seconds (bounded by `--cache-ttl`); an expired success is replaced by the current error rather than displayed as current usage. An updater holds an OS file lock until its result has been saved atomically. Other processes wait up to 35 seconds and reuse its result. A crashed updater releases its lock automatically. If the lock cannot be acquired, the row reports an error without making an additional API call. A save failure warns on stderr; that result is still displayed, but later runs may need to fetch again.

On macOS the cache is in `~/Library/Caches/ai-usage/v1/`. Set `AI_USAGE_CACHE_DIR` to change the parent of `v1/`. The directory is private (`0700`), and entries and lock files are `0600`. Saved entries contain usage, timestamps, credential fingerprints, and error messages; cookies, access tokens, and refresh tokens are not saved. Do not delete lock files while instances are running.

`--statusline --input` and `--tui --input` remain file-only render modes; `--refresh` and `--cache-ttl` do not make them fetch. Profile listing and config generation also bypass the shared usage cache.

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

If the `--init-config` destination already exists, the template is printed to stdout instead. Cached output uses the accounts and labels recorded in the JSON file; set profile selection and labels when generating the cache. `[providers].exclude` filters all output modes, including cached statusline and TUI; `--only` can temporarily include an excluded provider. Statusline-specific hiding also applies when rendering that mode.

With `--statusline --input <PATH>`, a missing, unreadable, empty, or invalid JSON cache produces no stdout, reports the path and cause on stderr, and exits with a nonzero status. A valid report with zero accounts exits with status 0 and no output. Callers must handle errors even before the first cache is generated; this mode does not regenerate the cache.

When the reader of stdout stops early, as in `ai-usage --json | head -n 5`, the remaining output is discarded and the command exits with status 0.

## Manual usage resets

Claude and Codex rows also show the remaining manual usage resets and their expiry times. The table adds a **Manual resets** column with separate dates, such as `full 2 (1@10/23 05:27, 1@10/30 03:57)`; `1@…` means one reset expires at that date and time. The statusline appends only the total counts and nearest expiry: `R:full 2 (10/23 05:27)`. Only the date text turns red when fewer than seven days remain. Exactly seven days keeps the normal color, and color suppression settings such as `--no-color` still apply. The TUI lists every expiry and applies the same rule to each date. `full` restores both usage windows, `5h` restores the session window, and `1w` restores the weekly window. All displayed dates use local time.

`0` means no available resets remain; `?` means the count or expiry could not be obtained. Temporarily paused grants retain a separate count, such as `full paused 1`. The statusline prefers available grants when selecting an expiry and falls back to paused grants only when no available grants remain. It shows the nearest known date, `(?)` when all candidate dates are unknown, and no date when counts are all zero or unknown. Expired resets are excluded even when rendering an older cache. This reports availability only and never uses a reset.

With `--reset-at`, rows that cannot show a long-window reset time (unknown, already passed, or no long window) leave the same width blank, so `R:` starts in the same column on every row.

JSON accounts carry an optional `manual_resets` array with `kind`, `remaining` (integer or `null` for unknown), `expires_at` (RFC 3339 or `null`), and `paused` (defaults to `false` when absent). Older caches without this field continue to render without the reset suffix.
