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
ascii = false            # 書かなければロケールから決まる（UTF-8 でなければ true）
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

**`graph.ascii` だけは 1 の既定値が環境変数（ロケール）から決まる。** 2 に書いてあれば
そちらが勝つので、4 の上書きとは向きが逆になる。

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
上書きに使う環境変数は `MEWGIT_GIT_BIN` と `MEWGIT_GH_BIN`。これとは別に
`LC_ALL` / `LC_CTYPE` / `LANG` を見て、UTF-8 でなければ `graph.ascii` の既定を
真にする（判定の規則と実測は 06 の「ASCII フォールバック」）。
**`load_from` / `from_toml_str` はどちらの環境変数も見ない**（テストの入口なので、
`graph.ascii` の既定は偽のまま）。

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
    graph: GraphFile,
    github: Github,
    repos: Vec<RepoRuleSpec>,
    keymap: HashMap<String, String>,
    theme: Theme,
}

/// TOML 上の `[graph]`。`ascii` は書いていなければロケールに任せるので `Option`。
#[derive(Deserialize)]
#[serde(default)]
struct GraphFile {
    ascii: Option<bool>,
    page_size: usize,
    colors: Vec<String>,
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

`Default` は `General` / `Graph` / `GraphFile` / `Github` / `Theme` に手で実装する（`Config` と
`ConfigFile` はサブ構造体の `Default` を呼ぶだけなので derive でよい）。`derive(Default)`
ではゼロ値になり、上記の既定値（`git_bin = "git"`、`page_size = 300`、
`github.enabled = true` など）と食い違うため。各フィールドに `#[serde(default)]` を付け、
部分的な TOML でも書かれていない項目が既定値のまま残るようにする。

**`GraphFile` を `Graph` と分けるのは `ascii` のため。** `bool` + `#[serde(default)]` では
`ascii = false` と書いたのと書いていないのが同じ値になり、「書いてあればロケールより
勝つ」が成り立たない（06 の「ASCII フォールバック」）。`Config` の `graph.ascii` は
`bool` のままで、`ConfigFile` → `Config` の変換で `None` をロケールの値に解く。
**読めなかった経路（ファイルが無い・読めない・パースできない・場所が決まらない）も
既定の `ConfigFile` からこの変換を通る**ので、書いていない値を解く場所は 1 つ。

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

- `config` は `keymap` を `HashMap<String, String>` のまま保持する。解析結果の型は crossterm の `KeyEvent` ではなく core の `app::Key`（01）
- **パーサは `app` に置く**（`Key::parse`、`app/key.rs`）。`config` は最下段（00）なので `app::Key` を名指しできない。上書きを当てた表（`app::Keymap`）は `AppState::new` が組んで `AppState.keymap` に持つ
- **上書きは `app::keys` の表 1 本に効かせる**（01 の「案内に出すキーの表 › 表が dispatch を兼ねる」）。表が打鍵ごとに `Action` を持ち、dispatch も同じ表を引くので、ヒントだけが既定の字を出す形は作れない
- **上書きはキーの差し替え**で、足すのではない。`stage = "S"` を書くと `s` は効かなくなる。表に出さない別名（矢印の `Down` / `Up`）は残る

### 値の書き方

`Key::parse` が読む形。

- 修飾は `ctrl-` / `alt-` / `shift-` を前に重ねる（大文字小文字を問わない。`Ctrl-d` も読む）
- 残りは名前か 1 文字。名前は `enter` `esc` `tab` `backtab` `backspace` `delete` `insert` `left` `right` `up` `down` `home` `end` `pageup` `pagedown` `space` `f1`〜（大文字小文字を問わない）
- **1 文字は書いたまま**。`P` は `Char('P')` で、`p` とは別のキー。`-` もキーになる（`"-"`、`"ctrl--"`）
- 正規化は `Key::with_mods` に任せる（`shift-tab` は `backtab` と同じ。01 の「キー入力」）
- **押しても届かない形は断る**（警告して捨てる）。黙って正規化すると、書いたキーと効くキーが割れる
  - `shift-` + 文字（`shift-a`）—— `with_mods` が shift を落とすので `a` として効いてしまう。大文字で書く
  - `ctrl-` + 大文字の英字（`ctrl-D`）—— 端末は `Ctrl` + 英字を小文字で送る。crossterm 0.29 の `parse_event` は 0x01〜0x1A を小文字の `Char` に、CSI u（mewgit は `DISAMBIGUATE_ESCAPE_CODES` を押す）は shift を外した字で返す（ソースで確認。実機では叩いていない）。`alt-` + 大文字は断らない —— 古い形式では `ESC P` が `Alt` + `P` で届く。**ただし CSI u を話す端末では `Alt+Shift+P` が `alt-p` として届く**（shift を外した字に `with_mods` が shift を落とす）ので、`alt-P` は古い形式の端末でしか効かない（受け入れた穴）
  - `ctrl-i` / `ctrl-m` / `ctrl-[` —— CSI u を話さない端末では `Tab` / `Enter` / `Esc` と同じバイト（0x09 / 0x0D / 0x1B）で届き、crossterm はそちらのキーとして返す（`parse_event`。ソースで確認）。`ctrl-j`（0x0A）は raw mode では `Enter` に化けないので受ける

### 動作の名前

**名前は平坦**（`[keymap] stage = "S"`。2026-10-18 にユーザーと決めた）。同じ意味の動作は複数のペインで名前を共有し、上書きはその全部に効く。意味が違うものは名前を分ける（PRs の `r` は `refresh`、Branches の `r` は `rename`、Graph の `Esc` は `unmark`、右列の `Esc` は `back`）。**ペインごとに別のキーを当てることはできない** —— `[keymap.worktree]` の形は採らなかった。名前は `Action::name()` の網羅 `match` 1 本が持ち、snake_case で書く。

| 名前 | 既定 | どこで |
|---|---|---|
| `down` / `up` | `j` / `k` | 全ペイン |
| `help` / `quit` | `?` / `q` | どこでも |
| `fetch` / `pull` / `push` | `f` / `p` / `P` | どこでも |
| `account` / `notice_log` / `progress_menu` / `reload` | `A` / `L` / `m` / `R` | どこでも |
| `next_pane` / `prev_pane` | `Tab` / `Shift+Tab` | どこでも |
| `focus_graph` / `focus_branches` / `focus_stash` / `focus_prs` / `focus_right` | `1`〜`5` | どこでも |
| `detail` / `cherry_pick` / `revert` / `rebase` / `mark` / `unmark` | `Enter` / `y` / `v` / `i` / `Space` / `Esc` | Graph |
| `branch` | `n` | Graph と Branches |
| `checkout` | `Enter` | Branches と PRs |
| `rename` / `delete` / `force_delete` | `r` / `d` / `D` | Branches |
| `diff` | `Enter` | Stash と Worktree とコミット詳細 |
| `stash` | `z` | Stash と Worktree |
| `apply` / `pop` / `drop` | `a` / `Z` / `d` | Stash |
| `browse` / `refresh` / `create_pr` | `o` / `r` / `N` | PRs |
| `stage` / `unstage` / `discard` | `s` / `u` / `x` | Worktree と Diff |
| `commit` / `amend` / `stash_file` | `c` / `C` / `Z` | Worktree |
| `back` | `Esc` | Graph 以外 |

- **同じ画面で 2 つの動作が名前を共有しない**（テストが見る）。共有すると 1 つの上書きが同じ画面に同じキーを 2 本置く
- **stage / unstage / discard は共有の enum にしない**（`WorktreeAction` と `DiffAction` が別々に持つ）。名前の字が 2 箇所に出るだけで、共有の enum にすると `all_variants!` の網羅のテストが `missing` の形になる。共有の組はテストが固定しているので、字の打ち間違いで割れれば落ちる

### 断る上書き

- **`Ctrl-C` は上書きさせない**（2026-10-18 に決めた）。モーダル表示中も効く唯一の脱出口で、モーダルが受けるキーに付け替えると抜ける手が無くなる。**脱出口が消えないことは置き場所で守る** —— `update_key` は表を引かずに `keys::INTERRUPT` と比べる（01）ので、上書きが表をどう書き換えても `Ctrl-C` は効く。**守れるのはそこまで**で、案内が嘘をつかないことは守らない。だから上書きの処理でも**名前と値の両方を断る** —— `interrupt` の名前を受け付けないだけでは足りず、値に `"ctrl-c"` を書いた上書き（`fetch = "ctrl-c"`）も警告して捨てる。捨てないと、`?` の一覧が `Ctrl-C:fetch` と案内し、押すと先頭の `INTERRUPT` が受けて終了する（01）。`q` / `?` / `Tab` / 番号は上書きできる
- 知らない名前・読めない値も警告して捨てる。ほかの上書きは当たる

### 衝突

- **衝突はペインごとに「共通キー ∪ そのペインのキー ∪ 別名」で見る。** 判定は `PaneKeys::build`（production）が持つ。**警告するのは上書きが勝ったときだけ**で、既定の表どうしが重ならないことはテストが「組んだ表に既定の打鍵が全部残っている」で見る（警告では見られない）
- **後勝ち。** 上書きは既定の打鍵（と別名）より強い。**上書き同士は名前の昇順で後ろが勝つ** —— `keymap` は `HashMap` で、ファイルに書いた順は残っていない。`fetch = "X"` と `pull = "X"` なら `pull`
- **負けた打鍵はその画面の表から外す** —— 案内（フッター・`?` の一覧・文言に書いたキーの字）からも dispatch からも。外さないと、`fetch = "s"` で Worktree の `?` の一覧が `s:stage` と案内しながら `s` で fetch が走る。打鍵が 1 つも残らない行は行ごと落ちる。**外すのは重なった画面だけ** —— 共通キーの `f` を `stage` に当てても、`stage` の無い Graph では `f:fetch` は残る
- 衝突は警告する。共通キーの衝突は全部のペインで起きるので、同じ組（勝った名前と負けた名前）は 1 つに畳む。**別名（矢印）を奪った上書きは別の文言**で、`j` / `k` は残り矢印だけが奪われたことを言う

### 警告

**`AppState::new` が `Warn` の通知として積む**（`KeymapWarning`）。`ConfigWarning` は `config` の型なので、`app::Key` の解析の失敗を運べない。文言は `[keymap]` で始まり、値を読めなかったもの（`<名前> = "<値>": <理由>`）・知らない名前・`interrupt`・衝突でそれぞれ書き方が違う。

### 文言に書いたキーの字

**通知や画面の文言に書いたキーの字（「`N` で作成」「`m` から continue」「`A` で選ぶ」など）も上書きに追従する。** 文言は字ではなく動作でキーを引き、`Key` の `Display`（core の `app/key.rs`）で字にする。2/2 までは追従しない穴として受け入れていた（2026-10-18）。キーを字にする関数が `ui` にしか無く、core の通知から引けなかったためで、字にする処理を core へ移して塞いだ。

- **引く表は動作が決める**（`keys::key`）。ペインの動作（`i` の rebase、`N` の PR 作成、`z` の stash）は**そのペインの表**、どこでも効く動作（`f` / `p` / `P` / `m` / `A` / `L`）は**フォーカスのペインの表**。**モーダルは見ない**。
  - **ペインの動作をフォーカスの表で引かない** —— 通知は非同期に届くので、`i` を押して Worktree へ移ったあとに rebase の失敗が届くと、衝突が無くても `i` が見つからずキーが消える
  - **モーダルを見ない** —— push の失敗のような通知はモーダル表示中にも届き、閉じたあとも残る。ヘッダは 1 行の予算で段を選ぶ（07 の「幅が足りないとき」）ので、モーダルの開閉でキーが出入りすると段が入れ替わる。`Modal::AccountPicker` の「閉じてから `A` で引き直せます」は、描く時点のフォーカス（閉じたあとに戻るペイン）の表を引く。モーダルが開いたときのペインを写して持つ形（`Modal::Help` の `pane`）は採らない —— 聞いているのは閉じたあとに押せるキーだから
  - **例外は PRs ペインの「（N）」だけ**（`keys::live_key`）。いま押せるときだけ添えると決めてある（07 の「PRs ペイン」）。同じ PRs ペインの degraded の「（A）」はヘッダと同じ `keys::key` で、モーダルを開いても消えない
- **キーが 1 つでも引けなければ、キーを書かない形で出す**（2026-10-19 にユーザーと決めた）。衝突に負けて、その画面の表から外れたときに起きる（上の「衝突」）。文言ごとにキーを書かない形を 1 つ持ち、キーを 2 つ書く文言（`f` と `P`）も、どちらかが欠ければその形にする。**動作の名前（`create_pr`）を代わりに書かない** —— `?` の一覧に出るのはラベルで名前ではないので、名前を書いても画面から辿れない
- **引くのは `keys::key`、組むのは `keys::spell` の 1 本。** `spell` には動作の並び・キーを差し込む文言・キーを書かない形を渡す。表の選び方とキーが欠けたときの扱いを呼ぶ側ごとに書かない。キーの有無で形を変える ui（ヘッダの段）は `keys::key` の `Option` を読む
- **キーを書く文言の予算（通知の 71 セル、バッジの隣の 48 セル。01 の「失敗の案内文」）は既定のキーで測る**（受け入れた穴）。`Alt-PageDown` のような長いキーに上書きすると予算を超え、末尾が `…` で切られることがある。上限の無い入力なので、いちばん長い形を見張りでは作れない
- **追従しない字。** `Ctrl-C`（上書きできない。上の「断る上書き」）、モーダルのキー（`Esc` や `[y]`。モーダルは自前の案内を持ち、表には入っていない。07）、ペインの名前（「Branches で改名」）、git のサブコマンド（「commit で結ぶ」）、`mewgit --help` の「`?` で…、`q` で終了」（設定を読む前に出す固定の文。受け入れた穴）
