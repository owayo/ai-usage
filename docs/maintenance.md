# Maintenance review record

Confirmed defects from the October 2026 dependency update and code review. The entire source tree was inspected through AST analysis. Changes address reproduced defects or behavior demonstrably incorrect from the code; complexity alone did not justify refactoring.

## Confirmed defects

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

## Dependencies and build tools

`depup --install --include-pinned` updated clap to 4.6.7 and toml to 1.1.6. No major library update was required. unicode-segmentation 1.13.3, already a transitive dependency, became a direct dependency for grapheme boundaries. Tokio's `process` feature supports asynchronous discovery.

CMake is now managed by `mise.toml` and was updated through depup to 4.4.3. Local setup and CI use that same pinned version. Rust remains at 1.98.1.

## Unconfirmed observations

The long-term effects of refreshing without saving tokens, login guidance for OAuth HTTP 400, terminal control characters in labels/API errors, and compatibility with future cache window kinds need additional specification or input evidence and were left unchanged. Cloudflare body classification and OAuth client extraction were also left unchanged without concrete misclassification or extraction failures.

## Validation

`make ci` passed rustfmt, clippy with `-D warnings`, 183 unit tests, and five CLI tests. `cargo audit` reported no vulnerabilities across 242 dependencies. wreq still uses system-configuration 0.7.0. `make build` and real-account Claude/Codex API fetches also succeeded. Live checks of other providers require credentials and local setup; these checks do not establish success for every provider. Personal execution data was not copied into public documentation.

actionlint 1.7.12 retained outdated input definitions for `actions/create-github-app-token@v3`, incorrectly rejecting `client-id`. The [official action.yml](https://github.com/actions/create-github-app-token/blob/v3/action.yml) accepts `client-id`, while `app-id` is deprecated and optional. Rerunning with only those two diagnostics excluded passed; no permanent exclusion was added to the workflows or lint configuration.
