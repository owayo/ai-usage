# Maintenance review record

Each section records a dependency update and code review. Changes address reproduced defects or behavior demonstrably incorrect from the code; suspected issues without that evidence are listed as unconfirmed observations and left unchanged.

## 2026-10-08 review

### Confirmed defects

| Reproduction | Evidence | Fix and regression coverage |
|---|---|---|
| Pipe output into a reader that stops early, such as `ai-usage --json \| head -n 5` or `ai-usage --statusline --input <PATH> \| true` | `print!` / `println!` panic when stdout reports `EPIPE`, so the command printed `failed printing to stdout: Broken pipe` and exited with status 101 | Write rendered output through `render::write_stdout` and return I/O errors to `main`, which ends quietly with status 0 on a broken pipe. A CLI test hands the binary a pipe whose reader is already closed, for both the cached statusline and `--list-profiles` |
| Render the statusline with `--reset-at` when a Claude or Codex row has no future long-window reset time (unknown, already passed, or no long window) | The manual-reset `R:` segment follows the optional ` (MM/DD HH:MM)` date, so it started 14 columns further left on rows without that date. The statusline otherwise keeps its columns aligned, including the `--reset-at` date itself | Reserve the date's width on those rows when `--reset-at` is set. A unit test compares the `R:` column across dated, undated, passed, short-only, and long-only rows |
| Configure two `[[profiles]]` entries for one directory with different `providers` (a `claude` entry labeled `claude-label` and a `codex` entry labeled `codex-label`), then run with `--profile <dir>` or `--only codex` | `--profile` used only the first matching entry, so the Codex row disappeared. `--only` was applied before the entries divided the providers, so the Codex row was labeled `claude-label`. Both contradict the documented per-provider entries | Bind each entry to one profile, give each provider to the first entry that lists it, and apply `--only` afterwards (it still overrides `providers`). `--profile` reuses the same bindings. A unit test covers no filter, `--profile`, `--only`, both together, a provider no entry lists, and an unconfigured profile |

Fixes and unit tests are in `src/main.rs`, `src/render.rs`, `src/render/statusline.rs`, and `src/render/table.rs`; CLI coverage is in `tests/statusline_cache.rs`. The previous binary was confirmed to panic with status 101 on the closed pipe and to misalign `R:` by 14 columns; the per-provider test fails on the previous `build_targets` for both `--profile` and `--only codex`.

### Refactoring

JWT payload decoding was repeated in four functions across `codex.rs`, `grok.rs`, and `pixellab.rs`; it now lives in `src/jwt.rs`. The identical `~/` expansion in `antigravity.rs` and `grok.rs` moved to `config::expand_home`. Behavior is unchanged: the existing tests still pass, and new tests cover padded and unpadded payloads, malformed tokens, and path expansion. The highest cyclomatic complexity (`run`, 15) comes from dispatching the CLI modes and was left as is.

### Added test coverage

A local HTTP/1.1 server now checks how `get_json`, `post_json`, and `post_form` classify responses (retryable 503, 429, a connection closed before responding, and the Cloudflare challenge; non-retryable 401 and malformed JSON) and which headers they send. Cookie loading is tested end to end: schema v24 hash stripping, older schemas, unrelated hosts, and a path containing a space, `?`, and `#`. `--init-config` generation is tested through the CLI. Manual-reset parsing and rendering gained boundary tests: empty grants, unknown scopes, duplicate credit IDs, a missing status, year display, failed rows, and the table column.

### Dependencies

`depup --install --include-pinned` found no direct dependency updates within its two-week release-age policy. Transitive crates published at least two weeks earlier were updated in `Cargo.lock`: libredox 0.1.25, lru 0.18.5, and thiserror / thiserror-impl 2.0.21. Newer candidates such as tokio 1.53.2 and toml 1.1.7 wait for a later update under the same policy.

### Unconfirmed observations

- When the Codex reset-credit list is shorter than `available_count`, the count reads `?` instead of `available_count`. A third-party document says the backend may cap the list; showing an unknown count is the intended conservative behavior and is fixed by a test.
- Codex waits for the reset-credit side request (up to five seconds) even after the usage request has failed, so repeated retryable failures use more of the 20-second job deadline.
- Accounts without the manual-reset feature may show `full ?` indefinitely if Claude omits `cedar_ember` or the Codex endpoint returns 403 or 404.

These need evidence from real responses and were left unchanged.

### Validation

`make ci` passed rustfmt, clippy with `-D warnings`, 213 unit tests, and eight CLI tests. `cargo audit` reported no vulnerabilities across 241 dependencies, and wreq still uses system-configuration 0.7.0.

## 2026-10-04 review

Confirmed defects from the October 2026 dependency update and code review. The entire source tree was inspected through AST analysis. Changes address reproduced defects or behavior demonstrably incorrect from the code; complexity alone did not justify refactoring.

### Confirmed defects

| Reproduction | Evidence | Fix and regression coverage |
|---|---|---|
| Render a cache containing multiple providers with `--statusline --input <PATH> --only codex` | The cache path never inspected `--only`, so the same filter behaved differently from a fresh fetch | Filter accounts before rendering. Real CLI tests cover filtering, hiding the selected provider, and no filter |
| Use scientist/family emoji, flags, or combining characters in a statusline label | Summing character widths overcounts sequences displayed in two columns. Truncation can split a grapheme and change the rendered text | Measure whole-string display width and truncate only at grapheme boundaries. Tests cover emoji sequences, boundaries, and combining characters |
| Store complete Grok entries created at `.100` and `.900` within the same second | Converting creation times to Unix seconds erased their ordering, allowing an older entry to win by key order | Compare full timestamps with fractional seconds. Test with the older entry sorted first |
| Render a terminal table with `--no-color`, nonempty `NO_COLOR`, or `TERM=dumb` | Color control reached only the statusline; service, quota, error, and active cells were always styled | Pass the shared color decision to the table. Force styling in tests to check ANSI output independently of TTY detection |
| Antigravity discovery encounters a stalled `ps` or `lsof` | Synchronous `.output()` blocked the async runtime thread, preventing an outer async timeout from interrupting discovery or reaching OAuth fallback | Use asynchronous subprocesses, individual one-second deadlines, and termination on cancellation. Tests cover output, failure, missing executable, and a long sleep interrupted by a short deadline |
| Run `--init-config --config ./alternate.toml`, or create the destination between its existence check and write | Initialization ignored the specified path. A file created after the check was overwritten; a dangling symlink was followed and its target written | Use the specified path with exclusive `create_new(true)` creation. Print the template when it exists. CLI tests cover parent creation and preservation of files and dangling symlinks |
| Chrome profiles share a display name and multiple config entries use that name | Each entry selected the same first profile, omitting other profiles and creating concurrent fetches of the same provider with the same cookies | Prefer unused matching directories and deduplicate by directory/provider. Preserve distinct provider settings and labels; generate unique directory matches. Tests cover duplicate names/settings, name-directory collisions, and separate provider settings |

Fixes and unit tests are in `src/main.rs`, `src/render/statusline.rs`, `src/render/table.rs`, `src/grok.rs`, and `src/antigravity.rs`; CLI coverage is in `tests/statusline_cache.rs`. Cache filtering, emoji width, and subsecond Grok ordering were also verified to fail before their fixes.

### Dependencies and build tools

`depup --install --include-pinned` updated clap to 4.6.7 and toml to 1.1.6. No major library update was required. unicode-segmentation 1.13.3, already a transitive dependency, became a direct dependency for grapheme boundaries. Tokio's `process` feature supports asynchronous discovery.

CMake is now managed by `mise.toml` and was updated through depup to 4.4.3. Local setup and CI use that same pinned version. Rust remains at 1.98.1.

### Unconfirmed observations

The long-term effects of refreshing without saving tokens, login guidance for OAuth HTTP 400, terminal control characters in labels/API errors, and compatibility with future cache window kinds need additional specification or input evidence and were left unchanged. Cloudflare body classification and OAuth client extraction were also left unchanged without concrete misclassification or extraction failures.

### Validation

`make ci` passed rustfmt, clippy with `-D warnings`, 183 unit tests, and five CLI tests. `cargo audit` reported no vulnerabilities across 242 dependencies. wreq still uses system-configuration 0.7.0. `make build` and real-account Claude/Codex API fetches also succeeded. Live checks of other providers require credentials and local setup; these checks do not establish success for every provider. Personal execution data was not copied into public documentation.

actionlint 1.7.12 retained outdated input definitions for `actions/create-github-app-token@v3`, incorrectly rejecting `client-id`. The [official action.yml](https://github.com/actions/create-github-app-token/blob/v3/action.yml) accepts `client-id`, while `app-id` is deprecated and optional. Rerunning with only those two diagnostics excluded passed; no permanent exclusion was added to the workflows or lint configuration.
