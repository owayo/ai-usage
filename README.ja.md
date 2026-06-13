# ai-usage

サインイン済みの各 Chrome プロファイルについて、**Claude** と **OpenAI Codex (ChatGPT)** の使用量
— ローリング **5時間** 枠と **週次** 枠、そしてそれぞれのリセット時刻 — を1コマンドでまとめて表示する
macOS 向け CLI です。

各 Chrome プロファイルのセッションをブラウザから直接読み取るため、ログインし直すことなく
**複数アカウントを同時に**確認できます(例: `Work` と `Home` の2プロファイル × Claude/Codex の
サブスク = 4アカウント)。

```
┌─────────┬─────────┬──────────┬──────────────────────────┬──────────────────────────┐
│ Account ┆ Service ┆ Plan     ┆ 5-hour                   ┆ Weekly (7-day)           │
╞═════════╪═════════╪══════════╪══════════════════════════╪══════════════════════════╡
│ work    ┆ Claude  ┆ max      ┆ █░░░░░░░░░    4%  · in 2h ┆ █░░░░░░░░░    3%  · in 4d │
│ work    ┆ Codex   ┆ team     ┆ █░░░░░░░░░    1%  · in 5h ┆ ░░░░░░░░░░    0%  · in 7d │
│ home    ┆ Claude  ┆ max      ┆ █░░░░░░░░░   12%  · in 1h ┆ █░░░░░░░░░    3%  · in 5d │
│ home    ┆ Codex   ┆ prolite  ┆ █░░░░░░░░░   10%  · in 4h ┆ ███░░░░░░░   31%  · in 4d │
└─────────┴─────────┴──────────┴──────────────────────────┴──────────────────────────┘
  updated 21:46 · bars = usage, time = until reset
```

## なぜ

Claude Code (`/usage`) と Codex (`/status`) は、ログイン中の **1アカウント** しか表示しません。
別々の Chrome プロファイルで業務アカウントを切り替えていると、全アカウントの残量を一覧できる場所が
ありません。`ai-usage` がその一覧です。

## 仕組み

検出した各 Chrome プロファイルについて:

1. `~/Library/Application Support/Google/Chrome/<profile>/Cookies` の Cookie を、macOS Keychain の
   **Chrome Safe Storage** キーで復号(標準の `v10` AES‑128‑CBC 方式)。
2. **Claude** — `sessionKey` Cookie で `claude.ai/api/organizations/{org}/usage` を呼び
   `five_hour` / `seven_day` の `{utilization, resets_at}` を取得。
3. **Codex** — `__Secure-next-auth.session-token` Cookie を `chatgpt.com/api/auth/session` で
   Bearer トークンに交換し、`chatgpt.com/backend-api/wham/usage` を呼んで
   `rate_limit.primary_window` / `secondary_window` を取得。

**Antigravity**(Google の `agy` CLI / IDE)は Chrome Cookie を使いません。
`ai-usage` は `~/.gemini` の OAuth トークンを読み(必要に応じて refresh)、`agy`
起動中はよりリッチな localhost の quota サーバーを優先します。**Gemini** と
**Claude & GPT** のモデルグループ別 週次上限(`agy /usage` と同じ数値)を表示します。

`claude.ai` と `chatgpt.com` はいずれも Cloudflare の背後にあるため、HTTP クライアント
([`wreq`](https://crates.io/crates/wreq))が Chrome の TLS/HTTP2 フィンガープリントをエミュレートし、
プロファイルの `cf_clearance` Cookie を再送します(素の HTTP クライアントは `403` になります)。
散発的なチャレンジに備えてリトライ + バックオフも行います。

ブラウザが Anthropic / OpenAI に対して普段行うのと同じ認証付きリクエスト以外、データは外部に
出ません。トークンや Cookie を出力・保存することもありません。

## 動作環境

- **macOS + Google Chrome 専用**(Chrome は macOS で `v10` 方式の Cookie を使用。Windows の `v20`
  app-bound 暗号化には未対応)。

## インストール

Rust ツールチェインと、[`wreq`](https://crates.io/crates/wreq) の BoringSSL ビルドに必要な
**cmake** が前提です:

```sh
brew install cmake        # または: make deps
cargo install --path .    # または: make install (~/.local/bin へ)
```

## 使い方

```sh
ai-usage                      # サインイン済みの全プロファイル・両サービス
ai-usage -p Work,Home        # プロファイル指定
ai-usage --only claude        # サービス指定(claude / codex / antigravity)
ai-usage --json               # JSON で出力(機械可読)
ai-usage --statusline         # 1行/アカウントのコンパクト表示(ステータスバー向け)
ai-usage --list-profiles      # 検出した Chrome プロファイル一覧
```

**初回実行時**は macOS の Keychain ダイアログ(*「"Chrome Safe Storage" キーを使用しようとしています」*)
が出るので **「常に許可」** を選んでください。

## 開発

```sh
make build      # デバッグビルド
make release    # 最適化リリースビルド
make install    # ビルドして ~/.local/bin へインストール
make check      # clippy(-D warnings)+ rustfmt チェック
make test       # テスト実行
make deps       # ビルド前提(cmake)を導入
```

リリースは GitHub Actions の **Release** ワークフロー(`workflow_dispatch`)から実行します。
CI は macOS(Intel + ARM)で `clippy` / `fmt` / `test` / build を回します。

## 注意・制限

- `cf_clearance` Cookie が失効していると、その1アカウントだけ *Cloudflare challenge* エラーになります。
  該当サイトをその Chrome プロファイルで一度開いて更新し、再実行してください(他アカウントには影響しません)。
- 使用量エンドポイントは **非公式 / リバースエンジニアリング** によるもので、変更される可能性があります。
- 依存の `wreq-util` が **GPL‑3.0** のため、本プロジェクトも GPL‑3.0 ライセンスです。

## 謝辞

**Antigravity**(Google の `agy` CLI / IDE)の使用量対応は、
[CodexBar](https://github.com/steipete/CodexBar) の Antigravity プロバイダ実装
([実装メモ](https://github.com/steipete/CodexBar/blob/main/docs/antigravity.md))を
参考にしています。
