# CLI reference

Every command and option of `ai-usage`. The everyday examples are in the [README](../README.md#usage).

## Commands

| Command | Description |
|---------|-------------|
| `ai-usage` | Show usage for all signed-in profiles and providers |
| `ai-usage --disk` | Print this Mac's disk usage, one volume per line, without fetching usage; see [Disk usage output](#disk-usage-output) |
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

### Disk usage

| Option | Description |
|--------|-------------|
| `--disk` | Print the disk usage of the startup disk and the volumes in `[tui].disk_mounts`, then exit. Cannot be combined with `--tui`, `--statusline`, `--input`, `--list-profiles`, or `--init-config` |
| `--disk --json` | Print the same values as JSON, in bytes. The fields are listed in [Disk usage output](#disk-usage-output) |
| `--mount <PATH>` | With `--disk`, read only the volumes given with `--mount`, in the given order, instead of the startup disk and `[tui].disk_mounts`. Repeat it for more volumes; `/` is the startup disk. Requires `--disk` |

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

## Disk usage output

`ai-usage --disk` prints this Mac's disk usage on its own and exits. It reads the startup disk first, followed by the volumes listed in `[tui].disk_mounts`. With `--mount`, it reads only the volumes given with `--mount`, in that order, and does not read the config file at all. A leading `~/`, trailing slashes, and repeated separators are normalized as in the config, and a volume listed twice is read once. `/` is the startup disk.

Each line has the same layout as a row of the `--tui` disk panel. The bar is a fixed 20 cells, followed by the used percentage and the most detailed size breakdown:

```text
Macintosh HD  █████████████████▒▒░   86%  70.8 GB available of 494.4 GB · 13.1 GB free · 57.7 GB purgeable
External      ████████░░░░░░░░░░░░   40%  1.2 TB available of 2.0 TB
Archive       unavailable: not mounted
Backup        no response
```

Colors also match the panel ([Disk usage](architecture.md#disk-usage)). `--no-color`, a non-empty `NO_COLOR`, and `TERM=dumb` turn them off.

Each volume is read once, all in parallel, and the command waits at most 2 seconds for all of them together. It prints as soon as every volume has answered. A volume that has not answered by then is shown as `no response` (`"status": "timeout"` in JSON), and the command exits without waiting for that read.

`--disk` never accesses Chrome, Keychain, the network, or the shared usage cache. Options that only affect usage output, such as `--profile`, `--only`, `--sort`, `--cache-ttl`, `--refresh`, and the statusline options, are accepted and ignored, so `--disk` can be appended to an existing `ai-usage` command line. `--no-disk` and `[tui].disk = false` only hide the TUI panel and do not change what `--disk` reads.

The exit status is 0 whenever the output is written, even when some volumes could not be read; each volume's `status` tells what happened to it. Invalid arguments exit with status 2. That includes `--mount` without `--disk`, an empty `--mount` value, and `--disk` combined with `--tui` or another mode. A failure to write the output exits with a nonzero status, except that a reader that closes stdout early (`ai-usage --disk | head -n 1`) still ends with status 0.

`--disk --json` prints the same values in bytes:

```json
{
  "schema_version": 1,
  "generated_at": "2026-10-22T13:19:01.175468+00:00",
  "volumes": [
    {
      "target": "/",
      "status": "ok",
      "name": "Macintosh HD",
      "total_bytes": 494384795648,
      "free_bytes": 13080645632,
      "available_bytes": 70754759808,
      "purgeable_bytes": 57674114176,
      "used_percent": 85.68832204573356,
      "capacity_basis": "important_usage",
      "error": null
    },
    {
      "target": "/Volumes/Archive",
      "status": "not_mounted",
      "name": null,
      "total_bytes": null,
      "free_bytes": null,
      "available_bytes": null,
      "purgeable_bytes": null,
      "used_percent": null,
      "capacity_basis": null,
      "error": null
    }
  ]
}
```

Every volume entry has every key, and a value that is not known is `null`. Volumes appear in the order they were read.

| Field | Meaning |
|-------|---------|
| `schema_version` | `1`. Raised when a field is removed or its meaning changes |
| `generated_at` | When the output was built, in RFC 3339 (UTC) |
| `target` | `/` for the startup disk; otherwise the normalized mount point |
| `status` | `ok`, `not_mounted` (not currently a mount point, such as an unplugged disk), `not_local` (a network volume, which is never queried), `timeout` (no answer within 2 seconds), or `error` (the query failed) |
| `name` | Volume name reported by macOS, such as `Macintosh HD` |
| `total_bytes` | Total capacity |
| `free_bytes` | Space that is free right now |
| `available_bytes` | Capacity available for important usage, which Finder reports as available and which includes purgeable space |
| `purgeable_bytes` | `available_bytes − free_bytes`, an estimate. `null` when the volume does not report capacity for important usage; `0` is a measured value |
| `used_percent` | `(total_bytes − available_bytes) / total_bytes × 100`, not rounded |
| `capacity_basis` | `important_usage` when `available_bytes` is the capacity for important usage, or `free_space` when the volume does not report it and `available_bytes` equals `free_bytes` |
| `error` | The failure message when `status` is `error`; otherwise `null` |

Every field except `target` and `status` (and `error` for the `error` status) is `null` unless `status` is `ok`. Byte values are exact and are not rounded to decimal units. Disk usage is never included in the usage `--json`, the table, `--statusline`, or the shared usage cache.
