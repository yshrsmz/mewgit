# 08. config: 設定ファイルとキーバインド

## 配置

- 設定: `$XDG_CONFIG_HOME/mewgit/config.toml`（既定 `~/.config/mewgit/config.toml`）
- 状態・ログ: `$XDG_STATE_HOME/mewgit/`
- リポジトリ固有: `.git/config` の `[mewgit]` セクション

## config.toml

```toml
[general]
git_bin = "git"          # PATH から探す。絶対パス指定可
gh_bin = "gh"
editor_fallback = false  # true なら reword 等で $EDITOR を使う

[graph]
ascii = false
page_size = 300
colors = ["blue", "magenta", "cyan", "green", "yellow", "red"]

[github]
enabled = true
concurrency = 4

# リポジトリごとのアカウント指定（アカウント解決の最優先）
[[repos]]
match = "github.com/acme-*/*"     # glob。host/owner/repo に対して
user = "taro_acme"

[[repos]]
match = "github.com/codingfeline/*"
user = "codingfeline"

[keymap]
fetch = "f"
push = "P"
"stage" = "s"

[theme]
graph_head = "bold"
pr_open = "green"
pr_merged = "magenta"
pr_closed = "red"
```

## リポジトリ固有設定

```
git config mewgit.user taro_acme
```

- `[[repos]]` より優先。`AccountPicker` で選んだ結果はここに保存する
- 読み取りは git2 の `Config`、書き込みは `git config` CLI

## 読み込み順

1. 組み込み既定値
2. `config.toml`（存在すれば。パースエラーは起動時に表示し既定値で継続）
3. `.git/config` の `[mewgit]`
4. 環境変数 `MEWGIT_*`（`_GIT_BIN` など、デバッグ用）

`Config` は起動時に確定し、実行中の再読込は MVP では行わない。

## 構造体

```rust
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub graph: Graph,
    pub github: Github,
    pub repos: Vec<RepoRule>,
    pub keymap: HashMap<String, String>,
    pub theme: Theme,
}

impl Config {
    pub fn load() -> (Config, Vec<ConfigWarning>);
    pub fn account_for(&self, host: &str, owner: &str, repo: &str) -> Option<&str>;
}
```

## キーマップの解決

- `keymap` の値は `crossterm` の `KeyEvent` に変換（`"ctrl-d"`, `"shift-tab"`, `"P"` などを解析）
- 同一ペイン内で衝突があれば起動時に警告し、後勝ちにする
- 未定義のアクション名も警告のみ
