# CLAUDE.md

Working notes for `ai-usage`. See `README.md` for the user-facing overview.

## What it does

macOS CLI that decrypts each Chrome profile's cookies (via the "Chrome Safe
Storage" Keychain key) and reports Claude and Codex usage limits — the rolling
5-hour and weekly windows plus reset times — for every signed-in profile.

## Source map

| File | Role |
|------|------|
| `src/profiles.rs` | Chrome profile discovery (`Local State`). |
| `src/cookies.rs`  | macOS `v10` cookie decryption. |
| `src/http.rs`     | `wreq` client with Chrome TLS/HTTP2 emulation (Cloudflare). |
| `src/claude.rs` / `src/codex.rs` | Per-provider usage fetchers. |
| `src/model.rs`    | `Provider` / `Usage` / `Window` data model. |
| `src/report.rs` / `src/render.rs` | JSON DTO + table / statusline rendering. |
| `src/main.rs`     | CLI, profile/provider resolution, concurrent fetch. |

## Build / check

`make build` · `make release` · `make install` · `make check` (clippy
`-D warnings` + rustfmt) · `make test`.

## Adding a provider

Add a `Provider` variant in `model.rs`, a `fetch()` module returning `Usage`,
and wire it into `main.rs` (`fetch_with_retry`) plus the `report.rs` /
`render.rs` mappers.

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

Public GitHub repo: never commit personal paths, emails, org names, or live
tokens / secrets. Use `~` / `$HOME` placeholders in docs and examples.
