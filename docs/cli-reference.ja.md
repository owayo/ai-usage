# CLI リファレンス

`ai-usage` の全コマンドと全オプションです。よく使う例は [README](../README.ja.md#使い方) にあります。

## コマンド

| コマンド | 説明 |
|---------|------|
| `ai-usage` | サインイン済みの全プロファイル・プロバイダの使用量を表示 |
| `ai-usage --init-config` | 現在サインイン済みのプロファイルから設定ファイルの雛形を生成 |
| `ai-usage --list-profiles` | 検出した Chrome プロファイル一覧を表示 |

## オプション

### フィルタリング

| オプション | 短縮 | 説明 |
|-----------|------|------|
| `--profile <NAMES>` | `-p` | プロファイル名をカンマ区切りで指定 (Chrome 表示名または on-disk ディレクトリ名) |
| `--only <PROVIDER>` | | `claude` / `codex` / `antigravity` / `pixellab` / `grok` のみを表示 |

### 出力

| オプション | 説明 |
|-----------|------|
| `--json` | 機械可読な JSON で出力 |
| `--statusline` | 1 行/アカウントのコンパクト表示 (ステータスバー向け) |
| `--statusline --logos` | ブランドロゴ字形で表示 (BrandLogos フォントが必要) |
| `--statusline --compact` | 狭いペイン向けにゲージ幅を半分にする |
| `--statusline --reset-at` | 長期枠リセットの絶対時刻 (例: `(06/18 01:10)`) を末尾に併記 |
| `--statusline-hide <PROVIDERS>` | statusline でのみ非表示にする provider (comma 区切り)。`--json` / table には影響なし。例: `--statusline-hide antigravity,codex` |
| `--sort weekly-usage` | 長期枠の使用率が高い順 (リミットに近いアカウントを上に) |
| `--sort weekly-reset` | 長期枠のリセット時刻が近い順 (リセット待ちが短いアカウントを上に) |
| `--no-color` | ANSI カラーを無効化 (`NO_COLOR` 環境変数に空でない値が入っている場合 ([no-color.org](https://no-color.org/) の仕様) または `TERM=dumb` でも無効になります) |
| `--input <PATH>` | フェッチせず、キャッシュ済み `--json` ファイルから statusline を描画。Chrome・Keychain・ネットワークのいずれにも触れないため、ステータスバーの再描画が高速 |

### アクティブ行の選択

| オプション | 説明 |
|-----------|------|
| `--active-email <EMAIL>` | Claude 行のサインイン済みメールと照合 (既定: `$CLAUDE_CONFIG_DIR/.claude.json`。環境変数が未設定または空なら `~/.claude.json`) |
| `--active-profile <NAME>` | プロファイル名で照合 |
| `--active-provider <NAME>` | 1 プロバイダに固定: `claude` / `codex` / `antigravity` / `pixellab` / `grok` |

### 設定・デバッグ・情報

| オプション | 説明 |
|-----------|------|
| `--config <PATH>` | `~/.config/ai-usage/config.toml` の代わりにこの設定ファイルを使う |
| `--debug` | 行ごとの判定結果を stderr に JSONL で出力 (stdout はクリーンなまま) |
| `--help` | ヘルプを表示 |
| `--version` | バージョンを表示 |
