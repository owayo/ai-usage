# CLI リファレンス

`ai-usage` の全コマンドと全オプションです。よく使う例は [README](../README.ja.md#使い方) にあります。

## コマンド

| コマンド | 説明 |
|---------|------|
| `ai-usage` | サインイン済みの全プロファイル・プロバイダの使用量を表示 |
| `ai-usage --disk` | この Mac のディスク容量を、使用量を取得せずに1ボリューム1行で表示。[ディスク容量の出力](#ディスク容量の出力) を参照 |
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
| `--tui` | 対話式の端末画面。使用量を1分ごとに更新し、手動リセットの有効期限と[ディスク使用量](architecture.ja.md#ディスク使用量)の欄も表示。`--json` / `--statusline` との併用不可 |
| `--no-disk` | `--tui` のディスク使用量の欄を隠す。設定ファイルの `[tui].disk` より優先 |
| `--statusline --logos` | ブランドロゴ字形で表示 (BrandLogos フォントが必要) |
| `--statusline --compact` | 狭いペイン向けにゲージ幅を半分にする |
| `--statusline --reset-at` | 長期枠リセットの絶対時刻 (例: `(06/18 01:10)`) を末尾に併記 |
| `--statusline-hide <PROVIDERS>` | statusline でのみ非表示にする provider (comma 区切り)。`--json` / table には影響なし。例: `--statusline-hide antigravity,codex` |
| `--sort weekly-usage` | 長期枠の使用率が高い順 (リミットに近いアカウントを上に) |
| `--sort weekly-reset` | 長期枠のリセット時刻が近い順 (リセット待ちが短いアカウントを上に) |
| `--no-color` | ANSI カラーを無効化 (`NO_COLOR` 環境変数に空でない値が入っている場合 ([no-color.org](https://no-color.org/) の仕様) または `TERM=dumb` でも無効になります) |
| `--input <PATH>` | `--statusline` または `--tui` と併用し、キャッシュ済み `--json` ファイルから描画。TUIでは1分ごとに読み直す。Chrome・Keychain・ネットワークには触れない。TUI のディスク欄だけは、この Mac のボリュームを直接読む |

### ディスク容量

| オプション | 説明 |
|-----------|------|
| `--disk` | 起動ディスクと `[tui].disk_mounts` のボリュームの容量を表示して終了。`--tui` / `--statusline` / `--input` / `--list-profiles` / `--init-config` との併用不可 |
| `--disk --json` | 同じ値をバイト単位の JSON で出力。項目は [ディスク容量の出力](#ディスク容量の出力) を参照 |
| `--mount <PATH>` | `--disk` と併用し、起動ディスクと `[tui].disk_mounts` の代わりに、指定したボリュームだけを指定した順に読む。繰り返し指定でき、`/` は起動ディスク。`--disk` なしでは使えない |

### 共有キャッシュ

| オプション | 説明 |
|-----------|------|
| `--cache-ttl <SECONDS>` | 取得結果を再利用する秒数。既定は `120`。`0` では以前の結果を再利用せず、同時に進む取得だけを共有 |
| `--refresh` | 以前のキャッシュを使わず更新。この要求の開始後に完了した取得は、同時に実行したコマンドと共有 |

テーブル、JSON、statusline、通常のTUIで取得結果を共有します。共有の単位はプロバイダと認証元のパスです。出力形式、表示ラベル、`--only` が違っても、同じ取得元なら結果を使えます。指定した取得元だけに問い合わせるため、`--only` で取得した結果を後の全件表示でも再利用できます。TUIは1分ごとに更新を確認し、同じ保存期間を適用します。

Chromeのキャッシュ確認では、セッションCookieの暗号文から変更を検知します。キャッシュが有効なら復号とKeychain参照を省きます。セッションCookie、Claudeの選択組織、OAuth認証ファイルが変わった場合や、取得後に利用枠のリセット時刻を過ぎた場合は、保存期間内でも更新します。

取得失敗も最大15秒間共有します。ただし、`--cache-ttl` が15秒未満なら、その秒数が上限です。更新に失敗した場合はエラーを表示します。期限切れの成功結果を最新の使用量として表示することはありません。

更新するプロセスはファイルロックを保持し、完成した結果を保存してから解放します。他のプロセスは最大35秒待ち、その結果を使います。更新元が異常終了してもOSがロックを解放します。ロックを取得できない場合は行にエラーを表示し、追加のAPI通信は行いません。保存に失敗した場合はstderrに警告して取得結果を表示します。その場合、後の起動では再取得が必要になることがあります。

macOSでの保存先は `~/Library/Caches/ai-usage/v1/` です。`AI_USAGE_CACHE_DIR` で `v1/` の親ディレクトリを変更できます。ディレクトリの権限は `0700`、結果とロックファイルは `0600` です。使用量、取得時刻、認証元の変更を検知する値、エラー文を保存します。Cookieやアクセストークン、リフレッシュトークンは保存しません。起動中はロックファイルを削除しないでください。

`--statusline --input` と `--tui --input` は、指定ファイルだけを読む従来の動作を保ちます。`--refresh` を付けても通信しません。プロファイル一覧と設定生成も共有キャッシュを使いません。

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

`--statusline --input <PATH>` でキャッシュが未存在・読み込み不可・空・不正な JSON の場合、stdout には何も出しません。パスと原因を stderr に出し、0 以外の終了コードで終わります。アカウントが0件の正常なレポートは、何も出さず終了コード 0 で終わります。初回のキャッシュ生成前もエラーになるため、呼び出し側で対処してください。このモードはキャッシュを再生成しません。

`ai-usage --json | head -n 5` のように stdout の読み手が途中で終了した場合は、残りの出力を捨てて終了コード 0 で終わります。

## 手動リセットの残回数と有効期限

Claude と Codex の行には、手動リセットの残回数と有効期限も表示します。テーブルは **Manual resets** 列で、期限が異なる付与分を `full 2 (1@10/23 05:27, 1@10/30 03:57)` のように個別に表示します。`1@…` はその日時に期限が切れる1回分です。statusline は末尾の `R:full 2 (10/23 05:27)` に残回数の合計と直近の有効期限だけを示します。有効期限まで7日未満になると日時部分だけが赤くなります。ちょうど7日なら通常色で、`--no-color` などの色抑制設定にも従います。TUI は期限をすべて表示し、日時ごとに同じ規則で色を付けます。`full` は完全リセット、`5h` はセッション枠、`1w` は週間枠のリセットです。日時はローカル時間です。

`0` は利用可能な残回数なし、`?` は残回数または期限を取得できなかった状態です。一時停止中の付与分は `full paused 1` のように別に表示します。statusline の期限は利用可能な付与分を優先し、それがなければ一時停止中のものから選びます。取得できた日時のうち最も近いものを表示し、期限がすべて不明なら `(?)`、残回数が0または不明だけなら期限を省略します。キャッシュ描画時にも期限切れの付与分を除外します。情報の表示だけを行い、リセットを使用する操作は行いません。

`--reset-at` を付けると、長期枠のリセット日時を出せない行 (時刻不明・リセット済み・長期枠なし) も日時と同じ幅を空けるため、`R:` はすべての行で同じ列から始まります。

JSON の各アカウントには、省略可能な `manual_resets` 配列を追加しています。各要素は `kind`、`remaining` (整数、不明なら `null`)、`expires_at` (RFC 3339、取得できなければ `null`)、`paused` (省略時は `false`) を持ちます。この項目がない旧キャッシュは、リセット表示なしで引き続き描画できます。

## ディスク容量の出力

`ai-usage --disk` は、この Mac のディスク容量だけを表示して終了します。起動ディスクを先頭に、`[tui].disk_mounts` に書いたボリュームを続けて読みます。`--mount` を付けると、指定したボリュームだけを指定した順に読み、設定ファイルは読みません。先頭の `~/`、末尾の `/`、重複した区切りは設定と同じく正規化し、同じボリュームを2回指定しても読むのは1回です。`/` は起動ディスクを表します。

各行の形は `--tui` のディスク欄の行と同じです。バーは20セルに固定し、その後に使用率と、最も詳しい内訳を続けます。

```text
Macintosh HD  █████████████████▒▒░   86%  70.8 GB available of 494.4 GB · 13.1 GB free · 57.7 GB purgeable
External      ████████░░░░░░░░░░░░   40%  1.2 TB available of 2.0 TB
Archive       unavailable: not mounted
Backup        no response
```

色もディスク欄と同じです ([ディスク使用量](architecture.ja.md#ディスク使用量))。`--no-color`、空でない `NO_COLOR`、`TERM=dumb` のいずれかがあれば色を付けません。

各ボリュームは並行して1回ずつ読み、全体で最大2秒待ちます。全ボリュームの結果がそろえば、その時点で表示します。2秒たっても結果が返らないボリュームは `no response` (JSON では `"status": "timeout"`) と表示し、その読み取りの終了を待たずに終了します。

`--disk` は Chrome・Keychain・ネットワーク・共有キャッシュに触れません。使用量の表示だけに効くオプション (`--profile`・`--only`・`--sort`・`--cache-ttl`・`--refresh`・statusline 用のオプションなど) は、受け付けたうえで無視します。そのため、既存の `ai-usage` のコマンド列に `--disk` を足して呼べます。`--no-disk` と `[tui].disk = false` は TUI の欄を隠すだけで、`--disk` が読むボリュームは変えません。

出力を書けたら、読めないボリュームがあっても終了コードは0です。各ボリュームで何が起きたかは `status` で分かります。引数の誤りは終了コード2で終わります。`--disk` なしの `--mount`、空の `--mount`、`--tui` など別のモードとの併用が該当します。出力を書けなかった場合は0以外で終わります。ただし、`ai-usage --disk | head -n 1` のように読み手が stdout を先に閉じた場合は0で終わります。

`--disk --json` は、同じ値をバイト単位で出力します。

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

どのボリュームの要素にも、すべてのキーがあります。分からない値は `null` です。要素は読んだ順に並びます。

| 項目 | 意味 |
|------|------|
| `schema_version` | `1`。項目を削除したり意味を変えたりしたときに上げる |
| `generated_at` | 出力を組み立てた時刻 (RFC 3339、UTC) |
| `target` | 起動ディスクは `/`、それ以外は正規化したマウントポイント |
| `status` | `ok`、`not_mounted` (取り外したディスクなど、現在マウントポイントでない)、`not_local` (ネットワークボリューム。問い合わせない)、`timeout` (2秒以内に結果が返らない)、`error` (問い合わせに失敗) のいずれか |
| `name` | macOS が返すボリューム名 (例: `Macintosh HD`) |
| `total_bytes` | 総容量 |
| `free_bytes` | 今この時点で空いている領域 |
| `available_bytes` | 重要な用途に使える容量。Finder が「利用可能」と表示する値で、パージ可能な領域を含む |
| `purgeable_bytes` | `available_bytes − free_bytes` の推定値。重要な用途の容量を返さないボリュームでは `null`。`0` は測った値 |
| `used_percent` | `(total_bytes − available_bytes) / total_bytes × 100`。丸めない |
| `capacity_basis` | `available_bytes` が重要な用途の容量なら `important_usage`。それを返さないボリュームで、`available_bytes` を `free_bytes` と同じ値にしたなら `free_space` |
| `error` | `status` が `error` のときの失敗の内容。それ以外は `null` |

`status` が `ok` でない要素は、`target` と `status` (`error` の状態では `error` も) のほかがすべて `null` です。バイト数は正確な値で、10進の単位に丸めません。使用量の `--json`・テーブル・`--statusline`・共有キャッシュには、ディスク容量を含めません。
