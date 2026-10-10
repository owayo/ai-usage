# CLI リファレンス

`ai-usage` の全コマンドと全オプションです。よく使う例は [README](../README.ja.md#使い方) にあります。

## コマンド

| コマンド | 説明 |
|---------|------|
| `ai-usage` | サインイン済みの全プロファイル・プロバイダの使用量を表示 |
| `ai-usage --init-config` | 現在サインイン済みのプロファイルから設定ファイルの雛形を生成。`--config <PATH>` で出力先を指定 |
| `ai-usage --list-profiles` | 検出した Chrome プロファイル一覧を表示 |

## オプション

### フィルタリング

| オプション | 短縮 | 説明 |
|-----------|------|------|
| `--profile <NAMES>` | `-p` | プロファイル名をカンマ区切りで指定 (Chrome 表示名または on-disk ディレクトリ名) |
| `--only <PROVIDER>` | | `claude` / `codex` / `antigravity` / `pixellab` / `grok` のみを表示。キャッシュからの statusline / TUI 描画にも適用 |

### 出力

| オプション | 説明 |
|-----------|------|
| `--json` | 機械可読な JSON で出力 |
| `--statusline` | 1 行/アカウントのコンパクト表示 (ステータスバー向け) |
| `--tui` | 対話式の端末画面。使用量を1分ごとに更新し、手動リセットの有効期限も表示。`--json` / `--statusline` との併用不可 |
| `--statusline --logos` | ブランドロゴ字形で表示 (BrandLogos フォントが必要) |
| `--statusline --compact` | 狭いペイン向けにゲージ幅を半分にする |
| `--statusline --reset-at` | 長期枠リセットの絶対時刻 (例: `(06/18 01:10)`) を末尾に併記 |
| `--statusline-hide <PROVIDERS>` | statusline でのみ非表示にする provider (comma 区切り)。`--json` / table には影響なし。例: `--statusline-hide antigravity,codex` |
| `--sort weekly-usage` | 長期枠の使用率が高い順 (リミットに近いアカウントを上に) |
| `--sort weekly-reset` | 長期枠のリセット時刻が近い順 (リセット待ちが短いアカウントを上に) |
| `--no-color` | ANSI カラーを無効化 (`NO_COLOR` 環境変数に空でない値が入っている場合 ([no-color.org](https://no-color.org/) の仕様) または `TERM=dumb` でも無効になります) |
| `--input <PATH>` | `--statusline` または `--tui` と併用し、キャッシュ済み `--json` ファイルから描画。TUIでは1分ごとに読み直す。Chrome・Keychain・ネットワークには触れない |

### アクティブ行の選択

| オプション | 説明 |
|-----------|------|
| `--active-email <EMAIL>` | statusline の Claude 行のサインイン済みメールと照合 (既定: `$CLAUDE_CONFIG_DIR/.claude.json`。環境変数が未設定または空なら `~/.claude.json`) |
| `--active-profile <NAME>` | statusline の行をプロファイル名で照合 |
| `--active-provider <NAME>` | statusline の照合対象を 1 プロバイダに固定: `claude` / `codex` / `antigravity` / `pixellab` / `grok` |

### 設定・デバッグ・情報

| オプション | 説明 |
|-----------|------|
| `--config <PATH>` | `~/.config/ai-usage/config.toml` の代わりにこの設定ファイルを使う。`--init-config` と併用すると、既存ファイルを上書きせず指定先に新規作成 |
| `--debug` | statusline の行ごとの判定結果を stderr に JSONL で出力 (stdout はクリーンなまま) |
| `--help` | ヘルプを表示 |
| `--version` | バージョンを表示 |

`--init-config` の出力先が既に存在する場合、雛形は stdout に表示します。キャッシュ描画は JSON に記録されたアカウントとラベルを使います。プロファイル選択とラベルはキャッシュ生成時に設定してください。`[providers].exclude` は通常表示、JSON、statusline、TUI、およびキャッシュ描画に適用します。`--only` で除外したプロバイダを一時的に表示できます。statusline 限定の非表示設定も引き続き使えます。

`ai-usage --json | head -n 5` のように stdout の読み手が途中で終了した場合は、残りの出力を捨てて終了コード 0 で終わります。

## 手動リセットの残回数と有効期限

Claude と Codex の行には、手動リセットの残回数と有効期限も表示します。テーブルは **Manual resets** 列で、期限が異なる付与分を `full 2 (1@10/23 05:27, 1@10/30 03:57)` のように個別に表示します。`1@…` はその日時に期限が切れる1回分です。statusline は末尾の `R:full 2 (10/23 05:27)` に残回数の合計と直近の有効期限だけを示します。有効期限まで7日未満になると日時部分だけが赤くなります。ちょうど7日なら通常色で、`--no-color` などの色抑制設定にも従います。`full` は完全リセット、`5h` はセッション枠、`1w` は週間枠のリセットです。日時はローカル時間です。

`0` は利用可能な残回数なし、`?` は残回数または期限を取得できなかった状態です。一時停止中の付与分は `full paused 1` のように別に表示します。statusline の期限は利用可能な付与分を優先し、それがなければ一時停止中のものから選びます。取得できた日時のうち最も近いものを表示し、期限がすべて不明なら `(?)`、残回数が0または不明だけなら期限を省略します。キャッシュ描画時にも期限切れの付与分を除外します。情報の表示だけを行い、リセットを使用する操作は行いません。

`--reset-at` を付けると、長期枠のリセット日時を出せない行 (時刻不明・リセット済み・長期枠なし) も日時と同じ幅を空けるため、`R:` はすべての行で同じ列から始まります。

JSON の各アカウントには、省略可能な `manual_resets` 配列を追加しています。各要素は `kind`、`remaining` (整数、不明なら `null`)、`expires_at` (RFC 3339、取得できなければ `null`)、`paused` (省略時は `false`) を持ちます。この項目がない旧キャッシュは、リセット表示なしで引き続き描画できます。
