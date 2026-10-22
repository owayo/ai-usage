# 設定

`ai-usage` は **設定なしでも動作** します。Claude / Codex / PixelLab セッションを持つ Chrome プロファイルに加え、利用可能な Antigravity / Grok の OAuth 情報も自動検出します。表示対象のプロファイルを固定したい、表示名を変更したい、プロバイダを絞り込みたい場合は **`~/.config/ai-usage/config.toml`** (または `$XDG_CONFIG_HOME/ai-usage/config.toml`) を置いてください。`--config <PATH>` を付けると、代わりにそのファイルを読みます。

## 初期設定

現在のセッションから雛形を生成できます。

```bash
ai-usage --init-config
ai-usage --init-config --config ./config.toml
```

2つ目のコマンドは出力先を明示します。必要な親ディレクトリも作成します。シンボリックリンクを含む既存ファイルは上書きせず、雛形を stdout に出します。Chrome 表示名が曖昧な場合、生成する設定には一意なプロファイルディレクトリ名を使います。

雛形は [`config.example.toml`](../config.example.toml) にもあります。

## 設定例

```toml
# 任意: アクティブとしてハイライトするアカウント
# (既定: CLAUDE_CONFIG_DIR/.claude.json から自動検出 = Claude Code セッションのアカウント)
# active_email = "home@example.com"

# [[profiles]] を1つでも書くと、ここに列挙したものだけが、この順番で表示されます。
[[profiles]]
match = "Work"                    # Chrome の表示名、またはディスク上のディレクトリ名 (例: "Default")
label = "work"                    # 任意: アカウントメール username の代わりに表示
# providers = ["claude", "codex"] # 任意: Chrome provider のサブセット。省略時は全て

[[profiles]]
match = "Home"
label = "home"

# Antigravity (Google `agy`) 使用量。~/.gemini OAuth トークン、Antigravity.app、
# または実行中の `agy` があれば自動検出されるため、設定は任意です。ラベル変更、非既定トークンの
# 指定、またはオフにしたい場合だけ追加します。
[antigravity]
# enabled = true                    # false なら検出されても非表示
label = "antigravity"               # 任意: 行に表示するラベル
# token_path = "~/.gemini/antigravity-cli/antigravity-oauth-token"

# Grok (xAI `grok` CLI) 使用量。~/.grok/auth.json (`grok login` が書き出す) が
# あれば自動検出されるため、設定は任意です。フィールドは [antigravity] と同構造。
[grok]
# enabled = true                    # false なら検出されても非表示
label = "grok"                      # 任意: 行に表示するラベル
# auth_path = "~/.grok/auth.json"

# table / JSON / statusline / TUI / キャッシュ描画のすべてから除外します。
# 除外したプロバイダは取得もしません。--only で一時的に表示できます。
[providers]
exclude = ["antigravity"]           # claude / codex / antigravity / pixellab / grok

# --tui のディスク欄と、`ai-usage --disk` が既定で読むボリューム。起動ディスクは常に
# 先頭です。disk_mounts には追加するローカルボリュームのマウントポイントを正確に書き、
# 書いた順に表示します。
[tui]
# disk = true                       # false ならディスク欄を非表示 (--no-disk と同じ)
disk_mounts = ["/Volumes/Archive"]  # 起動ディスクの後に表示するローカルボリューム
```

## 設定オプション

| オプション | 説明 | 既定値 |
|-----------|------|--------|
| `active_email` | このアカウントの Claude 行をアクティブとしてハイライト | `CLAUDE_CONFIG_DIR/.claude.json` (未設定なら `~/.claude.json`) から自動検出 |
| `[[profiles]]` | 表示するプロファイル一覧 (空なら自動検出) | `[]` (自動) |
| `profiles[].match` | Chrome 表示名または on-disk ディレクトリ名 (例: `Default`) | 必須 |
| `profiles[].label` | アカウントメール username の代わりに表示するラベル | メール username |
| `profiles[].providers` | Chrome プロファイルで表示するプロバイダのサブセット | Claude / Codex / PixelLab |
| `[antigravity].enabled` | 検出時に Antigravity 行を表示 | `true` |
| `[antigravity].label` | Antigravity 行のラベル | `antigravity` |
| `[antigravity].token_path` | 非既定の OAuth トークンパス | `~/.gemini/…` |
| `[grok].enabled` | 検出時に Grok 行を表示 | `true` |
| `[grok].label` | Grok 行のラベル | `grok` |
| `[grok].auth_path` | 非既定の `auth.json` パス | `~/.grok/auth.json` |
| `[providers].exclude` | 全表示モードから除外し、取得も省くプロバイダ | `[]` |
| `[statusline].hide` | `--statusline` で非表示にするプロバイダ (`--json` / table には表示) | `[]` |
| `[tui].disk` | `--tui` の画面下部にディスク使用量の欄を表示 (`--no-disk` が優先)。`--disk` には影響しない | `true` |
| `[tui].disk_mounts` | 起動ディスクの後に表示する、追加のローカルボリュームのマウントポイント。`--tui` の欄と、`--mount` なしの `--disk` で使う | `[]` |

優先順位は **CLI フラグ > 設定ファイル > 自動検出** です。

`[providers].exclude` は古い `--input` キャッシュにも適用され、TUI の再読込時も同じプロバイダを隠します。`--only <PROVIDER>` を指定すると、そのプロバイダだけは一時的に表示できます。従来の `[statusline].hide` は statusline 限定のままです。`--statusline-hide` はその一覧を上書きしますが、全体の除外は解除しません。`--list-profiles` と `--init-config` には適用しません。

照合時は大文字・小文字を区別せず、ディレクトリ名を表示名より優先します。同じ Chrome ディレクトリの各プロバイダからの取得は1回だけです。別プロバイダを別の設定行に書けば、個別のラベルを保持できます。同じプロバイダの重複指定は先の設定を優先します。同じ表示名を繰り返し指定した場合、検出順に未使用の一致プロファイルを優先します。名前が重複する場合はディレクトリ名で対象を明示してください。

`--profile` と `--only` を付けても、プロバイダ別の設定行は保たれます。`--profile` は、指定なしで実行したときと同じ設定行 (ラベル) で選んだプロファイルを表示します。`--only codex` は、`codex` を挙げた設定行のラベルで Codex の行を表示します。`--only` は `providers` より優先するため、そのディレクトリのどの設定行にも挙げていないプロバイダも、そのディレクトリの最初の設定行で表示します。設定に無いプロファイルを `--profile` で選んだ場合は既定値で表示します。

`[tui].disk_mounts` に書いたローカルボリュームは、`--tui` のディスク欄で起動ディスクの後に、書いた順で表示します。各項目には `/Volumes/Archive` のようにマウントポイントを正確に書きます (先頭の `~/` はホームディレクトリに展開)。各項目は、容量を問い合わせる前にシステムのマウント表と照合します。現在マウントポイントでないパスは、取り外したディスクも含め、古い値を残さず `unavailable: not mounted` と表示します。ネットワークボリュームは `unavailable: not a local volume` と表示して一度も問い合わせないため、応答しないネットワーク共有で TUI が止まることはありません。重複した項目と `/` (常に先頭に表示する起動ディスク) は無視します。`--no-disk` を付けると、`[tui].disk` が `true` でも欄を隠します。値の測り方は [ディスク使用量](architecture.ja.md#ディスク使用量) を参照してください。

`ai-usage --disk` も同じボリュームを読みます。起動ディスクに `[tui].disk_mounts` が続きます。`--mount` を付けると、その実行に限ってこの一覧の代わりに指定したボリュームを読み、設定ファイルは読みません。`[tui].disk = false` と `--no-disk` は TUI の欄を隠すだけで、`--disk` が読むボリュームは変えません。詳しくは [ディスク容量の出力](cli-reference.ja.md#ディスク容量の出力) を参照してください。
