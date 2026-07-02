# CLAUDE.md

Working notes for `ai-usage`. See `README.md` for the user-facing overview.

## What it does

macOS CLI that decrypts each Chrome profile's cookies (via the "Chrome Safe
Storage" Keychain key) and reports Claude, Codex, and PixelLab usage limits —
the rolling 5-hour and weekly windows plus reset times — for every signed-in
profile. Antigravity (Google's `agy`) is fetched via OAuth alongside them.

## Source map

| File | Role |
|------|------|
| `src/profiles.rs` | Chrome profile discovery (`Local State`). |
| `src/cookies.rs`  | macOS `v10` cookie decryption. |
| `src/http.rs`     | `wreq` client with Chrome TLS/HTTP2 emulation (Cloudflare). |
| `src/claude.rs` / `src/codex.rs` / `src/antigravity.rs` / `src/pixellab.rs` | Per-provider usage fetchers. |
| `src/model.rs`    | `Provider` / `Usage` / `Window` data model. |
| `src/config.rs`   | `~/.config/ai-usage/config.toml` (profiles + Antigravity table) + `BrowserWants`. |
| `src/sort.rs`     | `SortKey` (`--sort`), shared by CLI and renderers. |
| `src/report.rs`   | JSON DTO (shared by `--json` output and `--input` cache). |
| `src/render.rs`   | Shared row resolution (display name, active highlight, brand colors) + JSON output; re-exports the renderers. |
| `src/render/sort.rs` / `src/render/table.rs` / `src/render/statusline.rs` | Row sorting (`SortableRow`), human table, compact statusline. |
| `src/main.rs`     | CLI, profile/provider resolution, concurrent fetch. |

## Build / check

`make build` · `make release` · `make install` · `make check` (clippy
`-D warnings` + rustfmt) · `make test`.

Each module ships unit tests next to its source (`#[cfg(test)] mod tests`),
covering pure logic: cookie decryption round-trips, exact provider-domain
filtering, and session-cookie name matching (`cookies.rs`), org/window parsing
(`claude.rs`/`codex.rs`), TOML config and `BrowserWants` (`config.rs`),
display-name and active-row resolution (`render.rs`), row sorting
(`render/sort.rs`), table bar/humanize formatting (`render/table.rs`),
statusline gauge/duration formatting (`render/statusline.rs`), Antigravity
quota parsing including ISO-8601 and epoch-second `resetTime` plus wrapped/flat
`GetUserStatus` shapes (`antigravity.rs`), PixelLab Supabase cookie parsing
(legacy JSON-array + `base64-…` object forms + `.0/.1` chunk join), JWT `exp` /
`email` extraction, `/get-account-data` + `/get-subscription` folding into the
`weekly` slot with `generation_reset_date` (`pixellab.rs`), report-DTO building
with reset-countdown clamping (`report.rs`), and TOML-value escaping plus
provider resolution (`main.rs`).
Network code (`http.rs`) is not unit-tested — drive it via `make build` + a real
run.

## Adding a provider

Add a `Provider` variant in `model.rs`, a `fetch()` module returning `Usage`,
and wire it into `main.rs` (`fetch_with_retry`) plus the `report.rs` /
`render.rs` mappers.

## PixelLab

Implemented in `src/pixellab.rs` — auto-discovered when the Chrome profile has a
`supabase-auth-token` Cookie on `www.pixellab.ai`. Uses the browser-emulating
`wreq` client (both the Supabase auth endpoint and the PixelLab API sit behind
Cloudflare, and reject plain HTTP clients).

Auth flow:

1. Read the `supabase-auth-token` Cookie (`.0/.1/...` chunks are joined if
   split). It is URL-encoded and decodes to either:
   - the **legacy** Auth Helpers JSON array
     `[access_token, refresh_token, provider_token, provider_refresh_token, ...]`,
     or
   - the **new** `base64-<base64_of_json>` form containing
     `{access_token, refresh_token, expires_at, ...}`.
2. If the JWT's `exp` is within 60 s (or missing), refresh via
   `POST https://supabase.pixellab.ai/auth/v1/token?grant_type=refresh_token`
   with the PixelLab public **anon key** in both the `apikey` and `Authorization`
   headers (Supabase requires both). The anon key is a JWT baked into the JS
   bundle (`NEXT_PUBLIC_SUPABASE_ANON_KEY`), so it lives as a constant in
   `pixellab.rs`.
3. Call `GET https://api.pixellab.ai/get-account-data` for
   `{ imageAmount, imageGenerated, credits, tier }`; retry once via refresh on
   401/403/`Invalid token`.
4. Call `GET https://api.pixellab.ai/get-subscription` (best-effort; free users
   get an empty body) for `{ name, generation_reset_date, next_bill_date }`.

Map to `Usage`:

- `weekly` = monthly generation window:
  `used_percent = imageGenerated / imageAmount * 100` (clamped to `[0, 100]`),
  `resets_at = generation_reset_date` (falls back to `next_bill_date`, then
  `expiry_date`). The unified model has no dedicated "monthly" slot, so we
  reuse the long-window column. Rendering keeps this honest with two
  overrides: the per-row badge in both `render/table.rs` and
  `render/statusline.rs` reads `1m` (not `1w`), and because
  `five_hour == None`, the short slot is dropped and the long slot expands
  into a merged bar (`render/statusline.rs` uses `wide_gauge = 2 * gauge + 19`
  so the right edge still aligns with dual-slot rows; `render/table.rs` swaps
  in `WIDE_BAR_WIDTH = 24` for the same effect). This branch is data-driven
  ("5h missing → merged"), so Antigravity local-server groups (which also
  return only weekly buckets) benefit automatically.
- `five_hour` = `None` (PixelLab has no rolling short-window quota).
- `plan` = subscription `name` (e.g. `Tier 1: Pixel Apprentice`) with
  `+ $X.XX credits` appended when the pay-as-you-go USD balance is > 0. Free
  accounts get `Tier <N>` / `Free`.
- `email` = the `email` claim in the JWT payload.

## Antigravity

Implemented in `src/antigravity.rs` — auto-discovered when a `~/.gemini` token or
a running `agy` is found; configurable via the top-level `[antigravity]` table.
Each model group is one row (`UsageRow { group_label, usage }`), shown as
`Antigravity · Gemini` / `Antigravity · Claude&GPT`.

Antigravity (Google's agentic IDE + the `agy` CLI) reports per-model-group quota
— the "Gemini Models" and "Claude and GPT models" weekly / 5-hour windows shown
by `agy`'s `/usage`. Unlike Claude/Codex this is **not** behind a browser cookie:
Antigravity authenticates with a Google OAuth token, not a web session, so it
needs an auth path separate from `cookies.rs`.

**Follow [CodexBar](https://github.com/steipete/CodexBar)'s Antigravity
provider** — the most complete reverse-engineering of these sources:
<https://github.com/steipete/CodexBar/blob/main/docs/antigravity.md>.

Quota sources, in CodexBar's preference order:

1. **Local `language_server`** (Antigravity app or `agy` CLI) — a localhost
   HTTPS server. `POST https://127.0.0.1:<port>/exa.language_server_pb.LanguageServerService/RetrieveUserQuotaSummary`
   returns the richest payload (both groups, weekly + 5-hour). Find `<port>` with
   `lsof` on the running process; the `agy` CLI path needs no CSRF token (app/IDE
   paths read `--csrf_token` from process args). Only works while the app / `agy`
   is running.
2. **OAuth remote** — `POST https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota`
   with `Authorization: Bearer <token>`, returning Gemini per-model **daily**
   buckets (`{buckets:[{modelId, remainingFraction, resetTime}]}`; body `{}`).
   Note `retrieveUserQuotaSummary` returns **403** over OAuth — the grouped
   weekly view is local-only, so the OAuth fallback collapses to a single
   representative Gemini row. The token is under `~/.gemini/` (the `agy` CLI token
   store / `oauth_creds.json`); refresh via `oauth2.googleapis.com/token` using
   Antigravity's **own** OAuth client (its `gemini-cli` client is rejected with
   `unauthorized_client`). Discover the client id/secret from `Antigravity.app`,
   or override with `ANTIGRAVITY_OAUTH_CLIENT_ID` / `ANTIGRAVITY_OAUTH_CLIENT_SECRET`.
   Needs no running process.

Map to `Window`: `groups[].buckets[].remaining.remainingFraction` →
`used_percent = (1 - remainingFraction) * 100`; bucket reset metadata /
`resetTime` (ISO-8601, epoch-seconds fallback) → `resets_at`. Use the most
constrained (lowest-remaining) bucket per group for the bar. Handle both the
`groups[]` summary shape and the `buckets[]` / `quotaInfo` model shape
defensively — `v1internal` is an undocumented contract and shifts between
Antigravity releases.

As of 2026-07 the local `language_server` `RetrieveUserQuotaSummary` response
returns **only `window: "weekly"` buckets** per group (its own description
line says *"Within each group, models share a weekly limit"*). So both
groups' `five_hour` slots are `None` and the render layer collapses them
into the same merged wide bar as PixelLab. Keep the `is_weekly` /
`five_hour` split code — if Antigravity restores 5-hour buckets in a future
release, `parse_summary` will populate `five_hour` again and rows revert to
the dual-slot layout automatically.

## Statusline cache

`~/.claude/statusline.sh` drives the second-line usage display: it calls
`ai-usage --json`, caches the result at `/tmp/claude-statusline-<uid>/ai-usage.json`
(TTL ~120s, refreshed asynchronously so drawing never blocks), then renders it
with `ai-usage --statusline --input <cache>`. The cache filename tracks the
binary name.

**Gotcha after a rename or reinstall:** when the binary name (or cache key)
changes, the new cache file doesn't exist yet, so every usage row — Antigravity
included — is briefly empty until the first draw spawns the background
`ai-usage --json` that repopulates it. That's a cache-warmup delay, not a fetch
failure. Confirm with `ai-usage --only antigravity --json` before digging into a
provider: a populated `Claude&GPT` row means the local `agy` language_server
path is working (the OAuth fallback collapses to a single Gemini row).

## Repo hygiene

The GitHub repo is public. CI runs `cargo test` + clippy + rustfmt on macOS
(`.github/workflows/ci.yml`); the release workflow (`release.yml`) is
`workflow_dispatch` — it bumps `Cargo.toml` to a `YY.M.NNN` version, tags it,
builds `x86_64` / `aarch64` Apple Darwin binaries, attaches the tarballs to a
GitHub Release, then rebuilds `arm64_sonoma` / `sonoma` Homebrew bottles from
those binaries, uploads them alongside the release, and rewrites the
`Formula/ai-usage.rb` file in the `owayo/homebrew-ai-usage` tap. The tap push
uses a GitHub App token from the `APP_ID` / `PRIVATE_KEY` repo secrets — the
App must be installed on `homebrew-ai-usage` for the `update-homebrew` job to
succeed. Never commit personal paths, emails, org names, or live tokens /
secrets — use `~` / `$HOME` placeholders in docs and examples.
