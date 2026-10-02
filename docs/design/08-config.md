# 08. config: 設定ファイルとキーバインド

## 配置

- 設定: `$XDG_CONFIG_HOME/mewgit/config.toml`（既定 `~/.config/mewgit/config.toml`）
- 状態・ログ: `$XDG_STATE_HOME/mewgit/`
- リポジトリ固有: `.git/config` の `[mewgit]` セクション

macOS でも XDG のパスを使う。`directories` クレートの `config_dir()` は macOS で
`~/Library/Application Support` を返すため使わない。`XDG_CONFIG_HOME` → `$HOME/.config` の
順で自前に組み立て、ホームディレクトリは `std::env::home_dir()` で取る。どちらも
絶対パスのときだけ採用する。相対パスを許すと cwd 基準で解決され、開いているリポジトリが
置いた `.config/mewgit/config.toml` を読んでしまう（`general.git_bin` と `general.gh_bin` があるので任意コード実行になる）。

## config.toml

```toml
[general]
git_bin = "git"          # PATH から探す。絶対パス指定可
gh_bin = "gh"
editor_fallback = false  # true なら reword 等で $EDITOR を使う
refresh_interval = 10    # 秒。この間隔で status を読み直す（外での編集を拾う）。0 で止める

[graph]
ascii = false
page_size = 300                                                  # 0 なら既定値に戻す
colors = ["blue", "magenta", "cyan", "green", "yellow", "red"]   # 読めない色名は端末既定色

[github]
enabled = true    # false で GitHub 機能を丸ごと切る（gh を起動しない）
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

### スタイル指定

`[theme]` の値は空白区切りのトークン。色名（`green`、`#ff00aa` など ratatui が読める
表記）と修飾（`bold` `dim` `italic` `underlined` `reversed`）を混ぜられる。既定の
`graph_head` が `"bold"` であるとおり、値は色とは限らない。読めないトークンは黙って
無視する（設定の 1 語のミスで起動を止めない）。解釈は `ui::theme::style`（07）。

`[graph] colors` は色名だけの並びで、こちらは**読めない名前を飛ばさずに端末既定色として
並びに残す**。飛ばすと `ColorId % colors.len()` の周期が設定の見た目と変わり、
「6 色書いたのに 5 色で回る」理由をユーザーが追えない。色名の解釈は
`ui::theme::color` を共有する。

### `general.refresh_interval`

外でファイルを編集したのを拾うための、status の定期的な読み直しの間隔（01 の「外部の
変更の検知」）。既定の 10 は lazygit の `refresher.refreshInterval` と同じ。**0 で止める**
（大きなリポジトリで status が重いとき）。止めても、端末が前面に戻ったときと `R` では
読み直す。**ref と進行中操作の見張り（2 秒の probe）はこの値に関わらず走る** —— git を
起動せず、列挙のコストは ref 1 万本で 70ms（01）

### `github.enabled`

**`false` は「GitHub 機能を丸ごと切る」**（01 の「GitHub 機能を切る」に、どの読み手が
何をやめるかがある）。**`AppState::new` が `AccountState::Disabled` に写す**ので、
`config` を読む場所はここ 1 箇所。

- **起動時に固まる。** ほかの設定と同じく実行中は読み直さない（下の「読み込み順」）
- **`ssh -G` は走る。** ヘッダの `owner/repo` は `RepoInfo.target` から出ていて、
  GitHub API も `gh` も通らない（01）。**「GitHub を切ったのにリポジトリ名が出る」は
  意図した形**で、あれはリモート URL の話
- **`concurrency` はここでは効かない。** 同時リクエスト数の上限は `Semaphore` を
  入れる 2/2 のタスクで読む（05）

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

`Config` は起動時に確定し、実行中の再読込は MVP では行わない。読み込んだ `Config` は
`AppState` が持つ（01）。`ui` は `AppState` 経由でしか値を読めないため（不変ルール 2）、
`theme` を届けるにはここに置くしかない。

**「実行中の再読込は行わない」が画面に出る場所が 1 つある。** `Modal::AccountPicker` の
`r`（解決し直す）は `gh` と `.git/config` を読み直すが、**ここの `general.gh_bin` と
`[[repos]]` は写しを使うので拾えない**（01 の「外で直したあとの再解決」）。そちらを
指す案内には再起動が要ることも書く（07）—— **書かないと、押しても変わらない直し方を
案内することになる。** 再読込を入れるなら、その案内も一緒に落とす。

1・2 は `config` モジュールの責務。3 はリポジトリのパスを必要とし、値としても
`mewgit.user` しか読まないため `auth::resolve`（04）が `Config` とは別に読む。
4 は `Config::load()` が 2 の直後に適用する。`Config` 自身は `.git/config` を見ない。
MVP で見る環境変数は `MEWGIT_GIT_BIN` と `MEWGIT_GH_BIN` の 2 つ。M6 で
`LC_ALL` / `LC_CTYPE` / `LANG` を足し、UTF-8 でなければ `graph.ascii` の既定を
真にする（06 の「ASCII フォールバック」）。設定ファイルに書いてあればそちらが勝つ。

## 構造体

ファイル上の形（TOML そのまま）と、アプリが使う形（glob コンパイル済み）を型で分ける。
`Deserialize` を持つのは private な `ConfigFile` 側だけで、公開する `Config` は
`from_toml_str` / `load_from` / `load` / `default` からしか作れない。

```rust
/// TOML の生の形。private。
#[derive(Default, Deserialize)]
#[serde(default)]
struct ConfigFile {
    general: General,
    graph: Graph,
    github: Github,
    repos: Vec<RepoRuleSpec>,
    keymap: HashMap<String, String>,
    theme: Theme,
}

#[derive(Deserialize)]
struct RepoRuleSpec {
    #[serde(rename = "match")]
    pattern: String,
    user: String,
}

/// 検証済みの設定。`Deserialize` は持たない。
#[derive(Default)]
pub struct Config {
    pub general: General,
    pub graph: Graph,
    pub github: Github,
    pub repos: Vec<RepoRule>,
    pub keymap: HashMap<String, String>,
    pub theme: Theme,
}

/// glob をコンパイル済みの `[[repos]]` エントリ。不正な glob のものは存在しない。
pub struct RepoRule {
    pub pattern: String,
    pub user: String,
    matcher: GlobMatcher,
}

impl Config {
    /// 既定のパスから読む。ファイルが無ければ既定値。
    pub fn load() -> (Config, Vec<ConfigWarning>);
    /// 指定パスから読む。テスト用の seam。
    pub fn load_from(path: &Path) -> (Config, Vec<ConfigWarning>);
    /// TOML 文字列から読む。純粋関数。glob のコンパイルまで済ませる。
    pub fn from_toml_str(s: &str) -> (Config, Vec<ConfigWarning>);

    pub fn account_for(&self, host: &str, owner: &str, repo: &str) -> Option<&str>;
}

/// 設定ファイルのパスを組み立てる純粋関数。どちらも決まらなければ `None`。
pub fn resolve_config_path(xdg_config_home: Option<&Path>, home: Option<&Path>) -> Option<PathBuf>;
```

`matcher` を `Option` にせず必須にすることで、「コンパイルし忘れた `Config` では
`account_for` が黙って `None` を返す」という状態を型で排除する。`ConfigFile` から
`Config` への変換のときに glob をコンパイルし、失敗したルールはそこで捨てる。

`Default` は `General` / `Graph` / `Github` / `Theme` に手で実装する（`Config` と
`ConfigFile` はサブ構造体の `Default` を呼ぶだけなので derive でよい）。`derive(Default)`
ではゼロ値になり、上記の既定値（`git_bin = "git"`、`page_size = 300`、
`github.enabled = true` など）と食い違うため。各フィールドに `#[serde(default)]` を付け、
部分的な TOML でも書かれていない項目が既定値のまま残るようにする。

## エラーとしない扱い

`Config` の読み込みは失敗しない。問題は `ConfigWarning` として返し、起動時に
notice として表示したうえで既定値で継続する。

| 状況 | 挙動 |
|---|---|
| ファイルが無い | 既定値。警告なし |
| 読み取り失敗（権限など） | 既定値 + `Io` |
| TOML パースエラー | 既定値 + `Parse`（他の項目も採用しない） |
| `[[repos]]` の glob 不正 | そのルールだけ捨てて `InvalidGlob` |
| ホームディレクトリ不明 | 既定値 + `NoConfigDir` |

## アカウント解決（`[[repos]]`）

- マッチ対象の文字列は `"{host}/{owner}/{repo}"`
- glob は `globset`。`*` は `/` を跨がない（`literal_separator = true`）
- 大文字小文字は区別しない（`case_insensitive = true`）。GitHub の host / owner / repo は
  case-insensitive だが、リモート URL はユーザーが打った綴りを保持するため
- **先に書いたルールが勝つ**。複数マッチしても最初の 1 件を返す
- glob は `ConfigFile` → `Config` の変換時に 1 度だけコンパイルして `RepoRule` に保持する
  （`account_for` では再コンパイルしない）
- 知らないキーはエラーにせず無視する。`deny_unknown_fields` は使わない（設定ファイルの
  1 行のタイポで全項目が既定値に落ちるのを避ける）

## キーマップの解決

- `config` は `keymap` を `HashMap<String, String>` のまま保持する。解析結果の型は
  crossterm の `KeyEvent` ではなく core の `app::Key`（01）。パーサをどこに置くかは
  キーマップのタスク（MVP 後。`docs/plan.md` の「MVP 後」）で決める
- `keymap` の値は `app::Key` に変換（`"ctrl-d"`, `"shift-tab"`, `"P"` などを解析）
- **上書きは `app::keys` の表と dispatch の両方に効かせる**（01 の「案内に出すキーの
  表」）。片方だけだと、**ヒントが既定の字を出したまま別のキーが効く**
- **先に表へアクションの同一性を足す。** いまの表が持っているのはキーと表示の文言だけで、
  TOML のアクション名（`fetch` / `stage`）と突き合わせる鍵が無い（**`label` は鍵にならない**
  —— `移動` や `破棄` はどのアクション名とも一致しない）。足せば「両方に効かせる」は
  「表 1 本を上書きする」に縮む
- 同一ペイン内で衝突があれば起動時に警告し、後勝ちにする
- 未定義のアクション名も警告のみ
