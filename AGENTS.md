# AGENTS.md

Working notes for `ai-usage`. See `README.md` for the user-facing overview.

## What it does

macOS CLI that decrypts each Chrome profile's cookies (via the "Chrome Safe
Storage" Keychain key) and reports Claude, Codex, and PixelLab usage limits —
typed 5-hour / daily / weekly / monthly windows plus reset times — for every
signed-in profile. Antigravity (Google's `agy`) and Grok (xAI's `grok` CLI)
are fetched via OAuth alongside them.

## Source map

| File | Role |
|------|------|
| `src/profiles.rs` | Chrome profile discovery (`Local State`). |
| `src/cookies.rs`  | macOS `v10` cookie decryption. |
| `src/http.rs`     | `wreq` client with Chrome TLS/HTTP2 emulation (Cloudflare), deadlines, and shared transient-failure classification. |
| `src/claude.rs` / `src/codex.rs` / `src/antigravity.rs` / `src/pixellab.rs` / `src/grok.rs` | Per-provider usage fetchers. |
| `src/model.rs`    | `Provider` / `Usage` / `Window` / `WindowKind` data model. |
| `src/config.rs`   | `~/.config/ai-usage/config.toml` (profiles + Antigravity/Grok tables) + `BrowserWants`. |
| `src/sort.rs`     | `SortKey` (`--sort`), shared by CLI and renderers. |
| `src/report.rs`   | JSON DTO (shared by `--json` output and `--input` cache). |
| `src/render.rs`   | Shared row resolution (display name, active highlight, brand colors) + JSON output; re-exports the renderers. |
| `src/render/sort.rs` / `src/render/table.rs` / `src/render/statusline.rs` | Row sorting (`SortableRow`), human table, compact statusline. |
| `src/main.rs`     | CLI, profile/provider resolution, concurrent fetch. |

## Transient failures vs auth failures

`http.rs` marks recoverable failures by wrapping them in a private
`RetryableHttpError` and exposes `is_retryable()`; `main.rs` retries only those.
Provider modules that refresh a token on 401/403 (`pixellab.rs`, `grok.rs`) must
consult `is_retryable()` **before** matching on the message text. The Cloudflare
challenge string is literally `Cloudflare challenge (HTTP 403). …`, so a naive
`contains("HTTP 403")` classifies a transient block as an expired session: it
burns a rotation-tracked refresh token, replaces the retryable marker with a
non-retryable refresh error (killing the backoff retries), and reports
"Re-run `grok login`" for what was a temporary block. Keep the marker check
first in any new `is_auth_error`.

Related: the whole tool is read-only with respect to credentials — it never
writes a rotated refresh token back to the Chrome cookie or `auth.json`. Each
`fetch` therefore refreshes **at most once**, so the stored token stays at most
one generation behind.

## Degrading instead of failing

Chrome discovery failure is not fatal outside the Chrome-centric information
modes. `--list-profiles` / `--init-config` still return the error, but a normal
run reports `skipping Chrome profiles: …` on stderr and continues with zero
browser profiles, so the OAuth-only providers (Antigravity, Grok) still render.
Before this, a machine without Chrome produced output only when `--only
antigravity` / `--only grok` was passed — the `needs_profile_discovery()` bypass
existed but auto mode failed hard.

`config::load` degrades to auto mode on any unreadable config, but only stays
silent for "the *default* path does not exist". An explicitly passed `--config`
that is missing, a directory, or unreadable is reported on stderr, so a typo
does not masquerade as "my config is being ignored".

## Build / check

`make build` · `make release` · `make install` · `make check` (clippy
`-D warnings` + rustfmt) · `make test`.

Each module ships unit tests next to its source (`#[cfg(test)] mod tests`),
covering pure logic: cookie decryption round-trips and malformed schema-v24
prefix rejection, live WAL visibility through read-only Cookie DB access, exact provider-domain
filtering, numeric session-cookie chunk name matching (`.0`, `.1`, ...)
(`cookies.rs`), Chrome profile discovery / cookie-store precedence
(`profiles.rs`), org/window parsing (`claude.rs`/`codex.rs`), TOML config
loading including explicit-path and invalid-file fallbacks, and
`BrowserWants` (`config.rs`), display-name and active-row resolution including
malformed provider-email fallback (missing/empty/duplicate `@` separators)
(`render.rs`), row sorting (`render/sort.rs`), table bar/humanize formatting
(`render/table.rs`), statusline gauge/duration formatting, provider-aware
monthly reset thresholds for legacy caches, and display-width name padding
(over-long / exactly-fitting / full-width names) (`render/statusline.rs`),
Antigravity quota parsing including nested/flat
`remainingFraction`, missing-quota rejection, ISO-8601 and epoch-second
`resetTime`, app/IDE CSRF process-argument extraction, overflow-safe token expiry,
the local-path timeout budget that keeps the OAuth fallback reachable,
plus wrapped/flat
`GetUserStatus` shapes (`antigravity.rs`), PixelLab Supabase cookie parsing
(legacy JSON-array + `base64-…` unpadded Base64URL object forms + standard-Base64
compatibility + `.0/.1` chunk join), overflow-safe JWT `exp` /
`email` extraction, `/get-account-data` + `/get-subscription` folding into the
typed monthly long slot with `generation_reset_date` (`pixellab.rs`), overflow-safe
Grok token expiry and newest-usable multi-entry auth selection (`grok.rs`),
auth-vs-retryable classification driven by the real Cloudflare-challenge string
(`pixellab.rs` / `grok.rs`), report-DTO
building with reset-countdown clamping and old-cache compatibility (`report.rs`),
retryable HTTP marker/status classification including response-body failures and
GET/POST `408` / `429` / `5xx` handling
(`http.rs`), TOML-value escaping, provider resolution, and Chrome-discovery
bypass for cached / OAuth-only modes (`main.rs`). Drive the network paths via
`make build` + a real run.

## Dependency safety

`wreq` 5.x and `wreq-util` 2.x were yanked in 2026-08 when upstream reset its
version numbering (`wreq` 5.3.0 → 0.15.3, `wreq-util` 2.2.6 → 0.1.0; the
`6.0.0-rc.*` / `3.0.0-rc.*` lines are frozen — see
<https://github.com/0x676e67/wreq/issues/1254>). This project tracks the current
`wreq` 0.16.x + `wreq-util` 0.2.x line. Upstream ships breaking changes as new
minor versions (0.16 → 0.17), so bump both crates together and re-check the API
surface used in `http.rs` / `antigravity.rs` (`emulation`,
`tls_cert_verification`, `tls_verify_hostname`, `RequestBuilder::form`).

`wreq` 0.16 depends on `lru` ≥ 0.18.2, which fixes RUSTSEC-2026-0002
(`iter_mut`) and RUSTSEC-2026-0253 (`pop`). The former
`pool_max_idle_per_host(0)` workaround that disabled connection pooling was
therefore removed; run `cargo audit` after every dependency update.

Feature flags on `wreq` 0.16: `form` is required for `RequestBuilder::form`
(used by `post_form`), `system-proxy` keeps the macOS system-proxy detection
that 5.x enabled by default (`macos-system-configuration`), and `charset` stays
off because every endpoint returns UTF-8 JSON (`Response::text` then decodes as
UTF-8 only). The browser client uses `Emulation::Chrome149` so the TLS/HTTP2
fingerprint and `sec-ch-ua` brand version match the pinned `UA` constant.
`ClientBuilder::emulation` overwrites the TLS / HTTP1 / HTTP2 / default-header
sets in one call, so it must come **before** `user_agent` — the current order is
correct and swapping it would silently drop the pinned `UA`.

`system-proxy` pulls `system-configuration`, and that crate's version matters:
0.6.x panics (`Attempted to create a NULL object`, `dynamic_store.rs`) when
`SCDynamicStoreCreateWithOptions` returns NULL because `configd` is unreachable,
which crashes the whole binary in sandboxed environments. 0.7.0 returns `None`
instead, and `wreq` 0.16 handles that. The 0.16 line therefore also fixed a
reachable panic — confirm `system-configuration` stays at ≥ 0.7 after dependency
bumps (`cargo tree -p wreq -i system-configuration`).

Note that `wreq` enables the system proxy by default (`auto_sys_proxy = true`)
and applies **no loopback exclusion**, so any client that must not leave the
machine has to call `no_proxy()` explicitly — see the Antigravity localhost
client below.

## Adding a provider

Add a `Provider` variant in `model.rs`, a `fetch()` module returning `Usage`,
and a matching `FetchSpec` variant in `main.rs`, plus provider-specific render
metadata. Every returned `Window` must carry its real `WindowKind`; renderers
must not infer a period from the provider.

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
   - the **new** `base64-<unpadded_base64url_of_json>` form containing
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

- `long` = monthly generation window (`WindowKind::Monthly`):
  `used_percent = imageGenerated / imageAmount * 100` (clamped to `[0, 100]`),
  `resets_at = generation_reset_date` (falls back to `next_bill_date`, then
  `expiry_date`). The long-window column accepts typed weekly/monthly windows;
  the per-row badge comes from `WindowKind` and therefore reads `1m`. Because
  `short == None`, the short slot is dropped and the long slot expands
  into a merged bar (`render/statusline.rs` uses `wide_gauge = 2 * gauge + 19`
  so the right edge still aligns with dual-slot rows; `render/table.rs` swaps
  in `WIDE_BAR_WIDTH = 24` for the same effect). This branch is data-driven
  ("only one quota window → merged"), so Antigravity local-server groups and
  the daily-only OAuth fallback benefit automatically.
- `short` = `None` (PixelLab has no rolling short-window quota).
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

The localhost client is built separately from `http.rs`'s pair because it must
disable TLS certificate **and** hostname verification for the self-signed
`language_server`. Three properties are load-bearing and must survive edits:

- `no_proxy()` — `wreq` applies the system/env proxy to `127.0.0.1` as well, and
  with verification disabled a proxy could terminate TLS and read the
  `X-Codeium-Csrf-Token`.
- `timeout` / `connect_timeout` — `wreq`'s defaults are "no timeout", and its
  only built-in fallback (`tcp_user_timeout`) is Linux-only.
- An overall cap on the whole local path (`LOCAL_FETCH_TIMEOUT`), because
  `local_endpoints()` returns the product of processes × listening ports and
  issues two requests per endpoint. Without the cap, one hung language_server
  consumes `main.rs`'s `JOB_DEADLINE` and the OAuth fallback never runs — the
  row fails even with a valid token. `local_timeouts_leave_budget_for_the_oauth_fallback`
  locks this relationship in.

Map to `Window`: `groups[].buckets[].remaining.remainingFraction` or flat
`remainingFraction` → `used_percent = (1 - remainingFraction) * 100`; bucket
reset metadata / `resetTime` (ISO-8601, epoch-seconds fallback) → `resets_at`.
Buckets without a numeric `remainingFraction` are skipped instead of being
rendered as fabricated `0%` usage; a response with no usable quota is an error.
Local summary buckets get `WindowKind::Weekly` / `WindowKind::FiveHour`; the
OAuth fallback's per-model bucket gets `WindowKind::Daily`, so its badge is
`1d` rather than being mislabeled as `5h`.
Use the most constrained (lowest-remaining) bucket per group or OAuth fallback
row for the bar. Handle both the `groups[]` summary shape and the `buckets[]` /
`quotaInfo` model shape defensively — `v1internal` is an undocumented contract
and shifts between Antigravity releases.

As of 2026-07 the local `language_server` `RetrieveUserQuotaSummary` response
returns **only `window: "weekly"` buckets** per group (its own description
line says *"Within each group, models share a weekly limit"*). So both
groups' `short` slots are `None` and the render layer collapses them
into the same merged wide bar as PixelLab. Keep the `is_weekly` /
`short` split code — if Antigravity restores 5-hour buckets in a future
release, `parse_summary` will populate `short` again and rows revert to
the dual-slot layout automatically.

## Grok

Implemented in `src/grok.rs` — auto-discovered when `~/.grok/auth.json` exists
(written by `grok login`); configurable via the top-level `[grok]` table.
Chrome-independent single OAuth account, presented as one row labeled `Grok`.

Auth flow:

1. Read `~/.grok/auth.json`. The top-level key is a dynamic
   `"<oidc_issuer>::<oidc_client_id>"` string; the entry value carries
   `key` (access-token JWT), `refresh_token`, `expires_at`, `oidc_issuer`,
   `oidc_client_id`, `email`. If several entries are present the newest complete
   entry by `create_time` wins; incomplete entries are skipped. A flat document
   (no wrapper key) is also accepted.
2. Refresh via `POST <oidc_issuer>/oauth2/token` with
   `grant_type=refresh_token`, `refresh_token=…`, `client_id=…` when
   `expires_at` is within 60 s. `grok` CLI is a public OAuth client so
   `client_secret` is not needed. A refresh-token 401 surfaces a
   "Re-run `grok login`" message.
3. `GET https://cli-chat-proxy.grok.com/v1/user?include=subscription` for
   `{ email, subscriptionTier, hasGrokCodeAccess, teamId, … }`. 401/403
   triggers one refresh + retry.
4. `GET https://cli-chat-proxy.grok.com/v1/billing` for
   `{ config: { monthlyLimit, used, billingPeriodStart, billingPeriodEnd,
   history[] } }`. Non-auth failures leave `long = None` (the row still
   shows plan / email as "quota なし"). Uses the plain `api` HTTP client:
   the endpoint is Cloudflare-fronted but accepts a plain `Authorization:
   Bearer` without Chrome fingerprint emulation.

Map to `Usage`:

- `long` = monthly billing cycle (`WindowKind::Monthly`):
  `used_percent = used / monthlyLimit * 100` (clamped to `[0, 100]`).
  When `monthlyLimit == 0` (Free / no active subscription) render as `0%`
  and keep `resets_at = billingPeriodEnd` so the reset countdown still
  informs the user. `short == None`, so the render layer merges into the
  same wide long-only bar used by PixelLab and Antigravity summaries.
- `short` = `None` (grok CLI does not expose a rolling short-window quota
  over REST; the 5-hour / weekly buckets that surface in the WS
  `authenticate` response are not attempted here).
- `plan` = `subscriptionTier` (e.g. `SuperGrok`); `null` / missing → `Free`.
- `email` = `email` from `/v1/user`.

The endpoints were confirmed against `grok --debug --debug-file=…` traces
(gateway `authenticate` / `session/prompt` are WS-only; the REST surface is
`/v1/{user,billing,models,responses}`). If Grok ships a public REST for the
grouped 5-hour quota, extend `fetch` to populate `short` — the render layer
will revert to the dual-slot layout automatically (see the same fallback in
`src/antigravity.rs`).

## Statusline cache

`~/.claude/statusline.sh` drives the second-line usage display: it calls
`ai-usage --json`, caches the result at `/tmp/claude-statusline-<uid>/ai-usage.json`
(TTL ~120s, refreshed asynchronously so drawing never blocks), then renders it
with `ai-usage --statusline --input <cache>`. The cache filename tracks the
binary name.

Unlike the table (where comfy-table measures widths), the statusline builds its
columns by hand, so every field must be padded by **display width**, not
`char` count. `format!("{s:<11}")` counts `char`s and never truncates: a
full-width label takes two columns per `char`, a label of exactly the field
width leaves no separator (`development5h ███…`), and a longer one shifts the
whole row. `pad_display()` measures with `unicode-width`, truncates what does
not fit, and always keeps at least one separator column. Labels come from
user-supplied config, email local-parts, and Chrome profile names, so all three
cases occur in practice.

The serialized account keys remain `five_hour` / `weekly` for external and
cache compatibility, while each non-null window now includes a `kind`
(`five_hour` / `daily` / `weekly` / `monthly`). `kind` is optional during
deserialization so caches written by older binaries still render with the
legacy slot/provider label fallback. Legacy PixelLab and Grok rows also use
monthly reset-warning thresholds, matching their `1m` labels instead of the
weekly defaults.

`--statusline --input <cache>` is a cache-only render path: it must not discover
Chrome profiles, access Keychain, or call the network. `--list-profiles` and
`--init-config` remain higher-priority information modes when combined with
cache flags.

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
