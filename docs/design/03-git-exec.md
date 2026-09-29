# 03. git_exec: git CLI 実行層

## 責務

すべての書き込み操作を `git` サブプロセスで実行する。ssh 設定、credential helper、hook、`GIT_*` 環境変数が自動で効くようにするため。

## GitOp

```rust
pub enum GitOp {
    Stage(Vec<PathBuf>), Unstage(Vec<PathBuf>),
    StageHunk { path, patch: Vec<u8> }, UnstageHunk { .. }, DiscardHunk { .. },
    Discard(Vec<PathBuf>), DiscardUntracked(Vec<PathBuf>), DiscardAdded(Vec<PathBuf>),
    Commit { message: String, amend: bool },
    BranchCreate { name, start: Option<String>, switch: bool },
    Switch(String), BranchDelete { name, force }, BranchRename { from, to },
    Fetch { remote: Option<String>, prune: bool },
    Pull { mode: PullMode },
    Push { remote, src, dst: Option<String>, set_upstream, lease: Option<Oid> },
    RebaseInteractive { onto: String, plan: RebasePlan },
    RebaseContinue, RebaseSkip, RebaseAbort,
    CherryPick(Vec<Oid>), CherryPickContinue, CherryPickSkip, CherryPickAbort,
    Revert { oids: Vec<Oid>, mainline: Option<u32> }, RevertContinue, RevertAbort,
    MergeAbort,
    StashPush { message, include_untracked, paths: Option<Vec<PathBuf>> },
    StashApply(usize), StashPop(usize), StashDrop(usize),
    SetAccount { login }, UnsetAccount,
    CheckoutPr(Box<PrCheckout>),   // 複数の手順（下の「複数の手順を持つ書き込み」）
}
```

- **`patch` は `Vec<u8>`。** `git apply --cached` に戻す入力なので、lossy 変換すると
  非 UTF-8 のファイルをステージした結果が元のファイルと変わる（02 の「ハンクの持ち方」と
  同じ理由）
- **ハンクの 3 操作は `--whitespace=nowarn` を必ず付ける。** 既定はユーザーの
  `apply.whitespace` を読む。`fix`（よく勧められる設定）だと git が行末の空白を
  落としてから index に入れるので、**画面に出したハンクと index の中身が静かに
  食い違い**、`git status` は `MM` のまま残る。`error` だと逆に操作ごと落ちる。
  どちらも「見えているハンクをそのまま index へ」を壊す（git 2.55 で確認）。
  argv の組み立ては 3 変種で 1 本の関数に通し、変種を足したときに付け忘れない
  ようにする。`apply.ignoreWhitespace` も同じ性質を持つが、**結果が変わるケースを
  作れなかったので旗は足していない**（作れたら足す）
- **ハンクの discard だけ `--cached` が付かない。** `StageHunk` / `UnstageHunk` は
  index を書き換えるが、`DiscardHunk` はワークツリーのファイルを書き換える。
  **index とワークツリーの両方に適用する変種は作らない。** `git apply` は 1 回で
  片方にしか効かず（`--index` は「両方」だが index とワークツリーが一致していることを
  要求するので、staged な変更があると `does not match index` で落ちる。git 2.55 で
  確認）、2 回に割ると
  片方だけ通った中途半端な状態が残る。staged なハンクの discard は「先に unstage して
  ください」で断る（07 の Diff ペイン）
- **discard は tracked / untracked / 「HEAD に無い」の 3 変種に分ける。** untracked な
  ファイルは `git restore` では消せず `git clean` が要る。HEAD に無いファイル
  （`FileStatus::Added`）は、コミットが 1 つも無いと `restore --source=HEAD` 自体が
  落ちるので `git rm` が要る。`to_commands` は純粋関数で index もファイルシステムも
  見られないので、どれを出すかは区画と変更種別を知っている呼び出し側
  （`WorktreeSnapshot` を持つ `app`）が決める

## argv への変換

`GitOp` から argv と stdin を作るのは純粋関数 `to_commands`。argv だけでなく stdin も
一緒に返すのは、「どの引数で何を標準入力に流すか」が 1 つの判断だから（`git apply --cached -`）。
2 つの関数に割ると、変種を足したときに片方だけ直る。

```rust
pub struct GitCommand { pub argv: Vec<String>, pub stdin: Option<Vec<u8>> }

pub fn to_commands(op: &GitOp) -> Result<Vec<GitCommand>, ArgvError>;
```

**返すのは呼び出しの列。** 下の表の変種は `CheckoutPr` を除いてどれも 1 本で、
`CheckoutPr` だけが複数を返す（下の「複数の手順を持つ書き込み」）。

`argv` に `git` 自身は含めない（`Runner` が付ける）。`Result` なのは非 UTF-8 のパスを
argv に載せられないため。`git_read` が非 UTF-8 のパスを一覧ごと落とすので実際には来ないが、
`GitOp` は公開型なのでここでも塞ぐ（黙って lossy 変換すると別のファイルを操作してしまう）。

| GitOp | argv |
|---|---|
| `Stage(paths)` | `add -- <paths>` |
| `Unstage(paths)` | `reset --quiet -- <paths>` |
| `StageHunk { patch }` | `apply --cached --whitespace=nowarn -`（patch を stdin） |
| `UnstageHunk { patch }` | `apply --cached --reverse --whitespace=nowarn -` |
| `DiscardHunk { patch }` | `apply --reverse --whitespace=nowarn -`（`--cached` は付けない） |
| `Discard(paths)` | `restore --source=HEAD --staged --worktree -- <paths>` |
| `DiscardUntracked(paths)` | `clean --force -- <paths>` |
| `DiscardAdded(paths)` | `rm --force --quiet -- <paths>` |
| `Commit { message, amend }` | `commit [--amend] --message <message>` |
| `BranchCreate { switch: true }` | `switch --create <name> [<start>]` |
| `BranchCreate { switch: false }` | `branch <name> [<start>]` |
| `Switch(name)` | `switch <name>` |
| `BranchDelete { name, force }` | `branch --delete [--force] <name>` |
| `BranchRename { from, to }` | `branch --move -- <from> <to>` |
| `Fetch { remote, prune }` | `fetch [--prune] [<remote>]` |
| `Pull { mode }` | `pull --no-rebase` / `--rebase` / `--ff-only` |
| `Push { .. }` | `push [--force-with-lease=<dst\|src>:<lease>] [--set-upstream] <remote> <src>[:<dst>]` |
| `RebaseInteractive { onto }` | `rebase --interactive <onto>` |
| `RebaseContinue` / `Skip` / `Abort` | `rebase --continue` / `--skip` / `--abort` |
| `CherryPick(oids)` | `cherry-pick <oids>` |
| `CherryPickContinue` / `Skip` / `Abort` | `cherry-pick --continue` / `--skip` / `--abort` |
| `Revert { oids, mainline }` | `revert [--mainline <n>] <oids>` |
| `RevertContinue` / `Abort` | `revert --continue` / `--abort` |
| `MergeAbort` | `merge --abort` |
| `StashPush { .. }` | `stash push [--include-untracked] [--message <m>] [-- <paths>]` |
| `StashApply(i)` / `Pop` / `Drop` | `stash apply` / `pop` / `drop` `stash@{i}` |
| `SetAccount { login }` | `config --local -- mewgit.user <login>` |
| `UnsetAccount` | `config --local --unset -- mewgit.user` |
| `CheckoutPr(plan)` | 05 の「mewgit の手順」の表（場面ごとに複数） |

決めたことと理由:

- **`SetAccount` はアカウント解決の結果を `.git/config` に憶える**（`Modal::AccountPicker`
  の `Enter`。01 の「アカウントの選び直し」）。**`git` CLI を通す理由に不変ルール 1 を
  引かない** —— あのルールの理由は **hook が走らないこと**で、名指しされているのは
  commit / checkout / rebase / stash。`git config --local` は hook を 1 つも走らせない
  ので、引くと「config の書き込みは `GitOp` でなければならない」と読めてしまう。
  ここで CLI なのは**「読みは git2、書きは CLI」の分担**と、失敗の argv と stderr が
  `ExecError` で UI まで届く経路（不変ルール 4）に乗せるため。
  **キーの名前は `auth::MEWGIT_USER` の 1 本**から取る —— 読む側
  （`auth::resolve` の git2）と別々に書くと、片方を直したときに黙って別のキーを
  読み書きする。実測（git 2.55、`env -i` の一時リポジトリ）:

  | 確かめたこと | 結果 |
  |---|---|
  | `config --local mewgit.user <値>`（`--` 無し） | 値が `--global` / `--unset` / `--list` / `-4` でも**そのまま値として書かれる**（exit 0） |
  | `config --local -- mewgit.user <値>` | **同じ結果。** `git config` はキーより後ろをオプションとして読まない |
  | linked worktree での `--local` | 共有の `.git/config` に書く（`config.worktree` は作られない）= 読み側の git2 `ConfigLevel::Local` と同じ段 |
  | `mewgit.user` に値が 2 つあるとき `config --local -- mewgit.user <値>` | **exit 5**（`cannot overwrite multiple values with a single value`）。mewgit は `--add` しないので手で書いたときだけ。**畳まない**（`--replace-all` にすると、手で書いた別の値を黙って消す） |

  **`--` は守りになっていない**（上の 2 行目）。それでも置くのは、外したときの根拠が
  「いまの git はそう読まない」だけになるから —— `git branch --move` はまさにそれで
  足をすくわれた（下の項）。**「守っている」と書かない**のが `--` を置くことより大事で、
  書くと `-` 始まりの login をここで弾いたつもりになる
- **`UnsetAccount` は `.git/config` から `mewgit.user` を外す**（`Modal::AccountPicker`
  の `u`。01 の「アカウントの選び直し」）。**`--` は `--unset` より後ろ**に置く。
  実測（git 2.55、`env -i` の一時リポジトリ）:

  | 確かめたこと | 結果 |
  |---|---|
  | `config --local --unset -- mewgit.user` | exit 0。値が 1 つだけ残っていた `[mewgit]` は**節ごと消える** |
  | `config --local -- --unset mewgit.user` | `error: key does not contain a section: --unset` で **exit 2**。`--` より後ろは全部キーと値として読まれる |
  | 設定されていないキーに `--unset` | **exit 5、stderr は空** |
  | 値が 2 つあるキーに `--unset` | exit 5（`warning: mewgit.user has multiple values`）。`--unset-all` にはしない —— 手で書いた別の値を黙って消す |

  **3 行目が 01 の「成否で分岐しない」の根拠。** 断りの文言が出ないので、失敗から
  「いま何が設定されているか」は読めない —— 読み直して `.git/config` の実物に
  答えさせる。**`--` は `SetAccount` と形を揃えて置くだけ**で、ここでも守っていない
  （`UnsetAccount` は値を取らず、人が打った文字も外から来た名前も positional に載らない）
- **パスの前には必ず `--` を置く。** `-n` や `HEAD` という名前のファイルがオプションや
  revision として解釈されるのを防ぐ
- **人が打った文字や外から来た名前が positional に載るところにも `--` を置く。**
  いまは `BranchRename` の `to`（`Modal::Input` が受けた文字がそのまま来る）と
  `SetAccount` の `login`（上。**こちらは実測では効いていない**）の 2 つ。
  `git branch` は parse-options なので、`git branch --move topic --force` は
  「`--force` へ改名」ではなく**「HEAD を `topic` へ強制改名」**と読まれ、元の
  `topic` が黙って消えて **exit 0 で返る**（git 2.55 で確認）。`--` があれば
  `fatal: '--force' is not a valid branch name` で断られる。
  **`BranchCreate` には置かない** — `switch --create` は次の argv を無条件に名前として
  食うので `-` 始まりでも fatal になり、逆に `--create --` と書くと `--` 自体が名前に
  なって `fatal: invalid reference` で壊れる（git 2.55 で確認）
- **unstage は `restore --staged` ではなく `reset`。** コミットが 1 つも無いリポジトリ
  （unborn HEAD）で `git restore --staged` は `fatal: could not resolve 'HEAD'` で落ちる。
  `git reset -- <paths>` は同じ状態でも通る（git 2.55 で確認）。`--quiet` は
  「Unstaged changes after reset:」の一覧を出させないため
- **ブランチの切替は `checkout` ではなく `switch`。** `git checkout <名前>` は同名の
  ブランチとファイルがあるとブランチを優先するが、**ファイルだけが存在する名前**では
  「そのファイルを index から取り出す」と解釈してワークツリーの変更を黙って捨てる。
  `switch` は pathspec を取らないので `fatal: invalid reference: <名前>` で止まる
  （git 2.55 で確認）。`--` を置いて `checkout` を守る形と違い、`--` を落としたときに
  黙って危なくなる余地が無い。**`Checkout` が受けるのはブランチ名だけ**で、commit を
  detached で取り出すには `switch --detach` が要る（要るようになったら別の変種にする）
- **変種名は `Checkout` から `Switch` に改めた**（ブランチの作成とリネームのタスクで
  一緒に決めた）。argv が `switch` なのに変種名が `checkout` だと、同じ「作ってから
  切り替える」を 2 つの語彙で書くことになる。`BranchCreate { checkout: bool }` も
  `switch: bool` に揃えた。**`InputIntent`（01）は真偽値を持たない** — UI からの作成は
  常に切り替えるので、`switch: false` を組む経路が無い（`branch <name>` の側は
  `to_commands` のテストでだけ踏む）
- **force push は `--force-with-lease`。`--force` は他人のコミットを消せるので使わない。**
  **値（`=<dst>:<lease>`）を必ず添える。** 省くと lease は remote-tracking ref
  （`origin/main`）と比べるだけになり、`git fetch` を挟むと他人が進めた位置がそこに入って
  検査を素通りする（git 2.55 で確認）。「push が弾かれた → fetch して中身を見る →
  やはり force する」は UI が普通に通す道なので、素の lease は守りにならない。
  `lease` には確認モーダルで見せた位置をそのまま渡す
- **`force: bool` と `expected: Option<Oid>` に割らず、`lease: Option<Oid>` 1 つで表す。**
  2 つに割ると `force: true, expected: None` が組めてしまい、それがまさに上で
  「守りにならない」と書いた裸の `--force-with-lease` になる。**旗と値を 1 つにすれば
  その組み合わせは型として存在しない。** 前の形は `force` を出す経路が無いあいだだけ
  安全で、force push を配線した時点で踏める腕になっていた
- **行き先は `src` と `dst` に割る。** refspec（`<src>[:<dst>]`）と lease の refname は
  **同じ「リモート側の ref 名」を要求するが、置き場所が違う** —— refspec では右辺、lease
  では先頭。1 本の文字列に畳むと、`x` が `refs/heads/y` を追跡している形で
  `--force-with-lease=x:y:<oid>` という壊れた値になる。`dst` が `None`（ローカル名と
  同じ）なら refspec は `src` だけ、lease の refname も `src`。**lease の refname は
  短縮名（`main`）でも完全な ref 名（`refs/heads/main`）でも効く**ので、refspec の右辺に
  渡すのと同じ文字列をそのまま使う（git 2.55 で確認）
- **pathspec が空の `GitOp` は `ArgvError::EmptyPathspec` で拒否する。** `--` だけを
  置くと git は「指定なし」と読み、対象がリポジトリ全体に広がる。`reset --quiet --` は
  index 全体を戻し、`clean --force --` は untracked を全部消す（git 2.55 で確認）。
  「0 件を discard します」と確認して全部消えることになる。`StashPush { paths: None }` は
  「全部」を表す正当な指定なので、`Some(vec![])` とは区別する
- **HEAD に無いファイルの discard は `rm --force`。** `restore --source=HEAD --staged
  --worktree` は HEAD が無いと `fatal: could not resolve 'HEAD'` で落ちる。
  `git rm --force -- <paths>` は unborn でも通り、index からもワークツリーからも消える。
  これは HEAD があるときに staged な `Added` へ `restore` を掛けた結果（ファイルごと
  消える）と同じで、ワークツリー側が既に消えている staged 削除でも通る（git 2.55 で確認）。
  **`Discard` の代わりに無条件で使ってはいけない。** HEAD にあるファイルへ掛けると、
  変更ではなくファイルそのものが消える。**「HEAD に無いか」は `RepoInfo.head` では
  判断しない。** あれは起動時に 1 回読むきりで、外部でコミットされると古くなる。
  `FileStatus::Added` は `LoadStatus` が毎回運んでくるので、そちらで決める
- **untracked の discard に `-d` は付けない。** `git_read::status` が
  `recurse_untracked_dirs(true)` で列挙するので、渡すのは常にファイル。ディレクトリごと
  消す余地を残さない
- `switch` / `restore` を使うので git 2.23 以降が要る

## ハンクからパッチを組む

`GitOp` の `patch` を作るのも純粋関数。`git_read` が持つ 1 つの `Hunk`（02 の
「ハンクの持ち方」）を `git apply` に渡せるパッチにする。

```rust
pub fn hunk_patch(path: &Path, hunk: &Hunk) -> Result<Vec<u8>, ArgvError>;
```

出力はこの 4 つを並べたもの。

```
diff --git a/<path> b/<path>
--- a/<path>
+++ b/<path>
<hunk.header>
<行>...
```

- **`index <blob>..<blob> <mode>` の行は出さない。** blob の hash を `git_read` が
  持っておらず、無くても `git apply` は通る（git 2.55 で確認）。3-way merge
  （`--3way`）は使わないので落ちる経路も無い
- **ハンクのヘッダ（`@@ -a,b +c,d @@`）はそのまま載せる。** 数を組み直さない。
  `git apply` はヘッダの行数と実際の行数が食い違うと `corrupt patch` で落ちるので、
  数え直しは「元は正しかったものを壊す」側にしか働かない。開始行のほうは
  3 方向とも当てたい側の位置が入っている（stage は `-` 側 = index、unstage は
  `+` 側 = index、discard は `+` 側 = ワークツリー）ので、2 つ目以降のハンクだけを
  送っても探索なしで当たる
- **行は `<記号><content>` に `\n` を足したもの。** `content` は改行を落として
  持っているので（02）、`\r` はそのまま残り CRLF のファイルが壊れない
- **`*NoEol` の行の後ろにだけ `\ No newline at end of file` を足す。** マーカーは
  ヘッダの行数に数えない（git の出力もそうなっている。git 2.55 で確認）
- **パスは quote しない。** 非 ASCII も空白も、そのままの UTF-8 バイト列で
  `git apply` が受ける（git 2.55 で確認）。`git diff` の出力は
  `core.quotePath` の既定で非 ASCII を `"a/\346\227\245.txt"` と quote するが、
  受け側は両方を読める。`diff --git a/x b/y.txt b/x b/y.txt` のように `diff --git`
  行だけでは割り切れない名前でも、`---` / `+++` の 2 行から決め直すので当たる
- **ただし制御文字を含むパスと `"` で始まるパスは `ArgvError::UnquotablePath`
  で拒否する。** quote しない以上、tab は名前の終わりと読まれ（`---` の行は tab で
  区切る決まり）、`\r` は CRLF のパッチとして落とされ、`"` で始まる名前は quote
  されたものとして unescape される。どれも**別のファイルに patch を当てる**形の
  失敗なので、静かに通さない。禁じる文字を数え上げるのではなく「制御文字か」で
  弾くのは、受け側の細かい規則を写し取らずに済ませるため。
  ファイル単位の stage / unstage / discard はパスを argv に載せるだけなので、
  こういう名前でも通る
- 非 UTF-8 のパスは `to_commands` と同じ `ArgvError::NonUtf8Path`。`git_read` が
  一覧ごと落とすので実際には来ない

## Runner

```rust
pub struct Runner { repo_root: PathBuf, git_bin: PathBuf, env: Vec<(OsString, OsString)> }

impl Runner {
    pub fn new(repo_root: impl Into<PathBuf>, git_bin: impl Into<PathBuf>) -> Self;
    /// 実行時に重ねる環境変数。テストの隔離と、M5 のエディタ差し替えに使う。
    pub fn with_env(self, vars: ...) -> Self;
    pub async fn run(&self, op: &GitOp) -> Result<ExecOutput, ExecError>;
    /// 中断できる形（下の「キャンセル」）。
    pub async fn run_cancelable(&self, op: &GitOp, cancel: oneshot::Receiver<()>)
        -> Result<ExecOutput, ExecError>;
}

pub struct ExecOutput { pub argv: Vec<String>, pub stdout: String, pub stderr: String }

pub enum ExecError {
    /// git の終了状態を得られなかった。argv を組めなかった / Runner が拒否した /
    /// 起動できなかった / 終了を待てなかった。
    NoResult { argv: Vec<String>, reason: String },
    /// ユーザーが中断した（下の「キャンセル」）。
    Canceled { argv: Vec<String> },
    /// git が非 0 で終わった。
    Failed { argv: Vec<String>, code: Option<i32>, stderr: String, kind: ExecErrorKind },
}
```

- argv と stdin の組み立ては純粋関数 `to_commands()`（「argv への変換」）
- `git_bin` は `config` の `general.git_bin`（既定は `"git"`、絶対パスも書ける。08）から渡す。
  引数にしているのは、設定を読んだのに渡し忘れる経路を作らないため
- **`ExecError` を割るのは、失敗の種類で埋まるフィールドが違うため。** 1 つの
  struct にすると「起動していないときは `code` が `None`、`stderr` に git ではない文言、
  `kind` は分類表に載らない値」という約束を、読む側が毎回思い出すことになる。
  `Canceled` も同じ理由で 3 つ目にした（中断した git の `code` と `stderr` は
  signal の言い換えでしかない）
- 環境変数: `GIT_TERMINAL_PROMPT=0`、`LC_ALL=C`（エラー文の検出を安定させる）、`GIT_EDITOR=true`（意図しないエディタ起動を防ぐ）。`with_env` の指定はこの後に重ねるので、テストは上書きできる
- **`GIT_TERMINAL_PROMPT=0` では ssh のパスフレーズ入力を止められない。** `ssh` は
  stdin ではなく `/dev/tty` を直接読むので、agent に載っていない鍵だと止まる
  （タイムアウトは設けない方針。抜けるのは下の「キャンセル」）。**画面は壊れない** ——
  git を自分のプロセスグループで起こしている（下の「キャンセル」）ため、そこから
  `/dev/tty` を読む子孫は SIGTTIN で停止する（別グループの子が `/dev/tty` を読むと
  `stat` が `T`、同じグループなら `S+` で入力待ちになることを実測）。**プロンプトが
  出ないまま止まる**ので、押した本人にはなお何が起きているか判らない。
  **実効 ssh の後ろに `-o BatchMode=yes` を重ねて塞ぐ**（下の「BatchMode の重ね方」）

### キャンセル

```rust
pub async fn run(&self, op: &GitOp) -> Result<ExecOutput, ExecError>;
pub async fn run_cancelable(&self, op: &GitOp, cancel: oneshot::Receiver<()>)
    -> Result<ExecOutput, ExecError>;
```

**`git` は自分のプロセスグループの長として起動し（`process_group(0)`）、中断は
そのグループ宛てに SIGTERM を送る。** 実測で決めた形（macOS の git 2.55、`index.lock` を
握ったまま止まる clean filter で確認）。

| 送り方 | `index.lock` | git の子（ssh / hook / filter） |
|---|---|---|
| SIGKILL（`kill_on_drop`） | **残る**。以後の書き込みが全部落ちる | 孤児として生き残る |
| git 本体だけに SIGTERM | 消える | 孤児として生き残る |
| **グループ宛てに SIGTERM** | 消える | 一緒に死ぬ |

- **`kill_on_drop` は付けない。** あれが送るのは SIGKILL で、git は sigchain の後始末を
  せずに死ぬ。future を捨てるのは mewgit 自身が終わるときだけなので、そのまま走らせて
  git に自分で終わらせるほうが安全
- **殺したあと、終了を待ってから返す。** 待たずに捨てると、lock の削除が済む前に
  呼び出し側が次の書き込みを出せてしまう
- **待つのは「git が終わるまで」ではなく「pipe が閉じるまで」**（`wait_with_output` は
  `wait` と両方の `read_to_end` を join する。tokio 1.53 の実装で確認）。グループ宛ての
  signal は送った瞬間のメンバーに届くので、**直後に fork された子孫を取り逃がすと、
  その子孫が git の pipe を握っているぶん `ExecFinished` が遅れる**。短く打ち切らない
  のは、lock の削除より先に次の書き込みを出さないため。**その間の脱出口は 2 回目の
  `Ctrl-C`（終了）**（01 の「キャンセル」）
- **中断は失敗と型で分ける**（`ExecError::Canceled { argv }`）。git の exit code と stderr は
  signal で落としたことの言い換えにしかならず、`ExecErrorKind` の分類表にも載らない。
  UI は「押した本人が止めた」ものとして扱う（01 の「キャンセル」）。**出力は読まずに
  捨てる** —— fetch は progress を数十 KB 吐いてから殺されるので、使わないものを UTF-8
  検証に通さない
- **合図が届いたあとでも、git が成功していれば `Ok` を返す。** signal より先に終わって
  いた形で、書き込みの結果はリポジトリに残っている。`Canceled` にすると、済んだ commit を
  「中断した」と案内したうえに読み直しの経路まで変わる（`update::Reload` は成否で分かれる）
- `run` は送り手を作らずに `run_cancelable` へ渡す。受け側の `Ok(())` パターンが外れた枝は
  `select!` が無効にするので、その呼び方では中断の腕が二度と選ばれない

### 複数の手順を持つ書き込み

`GitOp::CheckoutPr` は git を続けて叩く（05 の「mewgit の手順」）。
**`Runner` が順に叩き、1 本でも非 0 で終わればそこで止めて、その 1 本の
`ExecError` を返す**（残りは叩かない）。成功なら**最後の 1 本の `ExecOutput`** を返す。

- **1 つの `GitOp` にまとめる理由は 05 の「一連の手順を 1 つの書き込みにする」。**
  `app` から見ると書き込みは 1 回で、`JobKind::Exec` も `Reload` も 1 回
- **止まった手順が何だったかは `argv` で判る。** `ExecError` はどの変種も
  `argv` を持っているので、通知の `detail` にはその 1 本の argv と stderr が出る
  （不変ルール 4）。**どこまで済んだかは運ばない** —— 済んだ手順の結果は
  リポジトリに残っており、読み直しがそれを拾う（`update::Reload` は
  `CheckoutPr` を `RefsAndLog` に置く）
- **中断は手順のあいだでも効く。** 合図は 1 本の `oneshot` で、叩く前に
  「もう届いているか」を見る（届いていれば次を起こさずに `Canceled` を返す。
  `argv` は起こさなかった 1 本のもの）。走っている最中に届けば 1 本だけの
  書き込みと同じ（上の「キャンセル」）。**届いたことは憶えておく** —— 走っている
  1 本が合図より先に成功していた回（`canceled_by` が偽）に受け側を「もう来ない」と
  同じに畳むと、次の手順の前の確認が「届いていない」と答え、止めたはずの手順が
  走る（`/code-review` が拾った。`CancelSignal` の 3 状態）
- **`verb()` は最初の 1 本の `argv[0]`**（`CheckoutPr` は `fetch`）。スピナーに
  出る語で、時間のほとんどは fetch が使う。見張りは `verb_matches_the_argv`
  （最初の 1 本と比べる）

### BatchMode の重ね方

素の `GIT_SSH_COMMAND="ssh -o BatchMode=yes"` を置くと、ユーザーの
`GIT_SSH_COMMAND` / `core.sshCommand` を**捨てて**しまう。実効値を先に求めてから、
その後ろにオプションを足す。

```
実効 = $GIT_SSH_COMMAND ?? core.sshCommand ?? ($GIT_SSH があれば「触らない」) ?? "ssh"
GIT_SSH_COMMAND = "<実効> -o BatchMode=yes"
```

- **git は ssh コマンドをシェル経由で起動し、引数を後ろに足す**ので、末尾に書いた
  `-o` は host より前に並ぶ。`core.sshCommand='sh dump.sh -o BatchMode=yes'` で argv を
  ダンプして確認した（git 2.55）。優先順は `GIT_SSH_COMMAND` > `core.sshCommand` で、
  こちらが環境変数に書けば config 側は読まれない（同じ方法で確認）
- **`GIT_SSH` だけが設定されている環境では何も足さない。** `GIT_SSH` は引数の渡し方が
  違い（シェルを通さない）、`GIT_SSH_COMMAND` を置くと git はそちらを優先するので、
  重ねるつもりが差し替えになる。**その環境では守りが無く、しかもユーザーには何も
  出ない** —— 止まって初めて分かる（プロンプトすら出ない。上の `GIT_TERMINAL_PROMPT` の
  項）。抜ける手は実行中ジョブのキャンセル（下の「キャンセル」）で、**グループごと殺すので
  停止した ssh も一緒に死ぬ**（停止中のプロセスにも SIGTERM は届く。実測）。
  **通知を出す形にはしない** — 起動時に「あなたの環境は守られていません」と出しても、
  fetch を押すまで意味が無い
- **ユーザーが明示した `BatchMode=no` は勝つ。** ssh は同じ項目を**先に現れた値**で
  決めるので（`ssh -o BatchMode=no -o BatchMode=yes -G` が `batchmode no` を返す。
  OpenSSH 10.3 で確認）、実効値の中に書かれた設定はこちらの追記より前に来る。
  **黙って上書きしない方針と向きが揃っている**
- **`core.sshCommand` を読むのは `git_read`**（不変ルール 1。読み取りは git2）。
  `ReaderHandle` が `workdir` と同じく起動時に 1 回だけ読む。実行のたびには読み直さない
  ので、mewgit を起動したまま設定を変えても効かない
- **`Runner` が受け取るのは実効値（`SshCommand`）で、生の設定値ではない。** 決めたのに
  渡し忘れる経路を作らないよう `git_bin` と同じくコンストラクタの引数にする。
  **環境変数を読むのは `auth::ssh::effective_ssh_from_env` の 1 箇所だけ** —
  実効値を決める `effective_ssh` を純粋関数のままにしておくと、実行環境に
  `GIT_SSH_COMMAND` が置かれていてもテストの結果が動かない
- 実効値を決める関数は `auth::ssh` に置く。「git が実際に使う ssh はどれか」は
  `resolve_ssh_host` と同じ問いで、`git_exec` から `auth` への依存は層の向きに沿う（00）。
  **`resolve_ssh_host` をその実効値に向けるのは M4。** いまは呼び出す経路が無く
  （`RepoInfo.target` を埋めるのが M4）、実効値はシェル文字列なので `sh -c` 経由の
  起動に変える必要がある。踏めない腕を先に入れない（04 の「SSH エイリアスの正規化」）
- stdin は閉じる。例外はハンクの 3 操作（`StageHunk` / `UnstageHunk` / `DiscardHunk`）
  だけで、どれも `git apply … -` に patch を stdin で渡す
- stdout / stderr は表示専用なので lossy で `String` にする
- タイムアウトは設けない（fetch の長さは予測不能）。代わりに UI から中断できるようにする
  （下の「キャンセル」）
- **`RebaseInteractive` は `Runner` が実行せずに拒否する**（M5 まで）。todo を差し替えずに `git rebase -i` を走らせると、`GIT_EDITOR=true` により全 `pick` の rebase が実行され、`plan` を黙って捨てた破壊的操作になる。M5 で `--sequence-editor` を配線したら外す

終了状態を得られない経路は 4 つ（`to_commands` の失敗、`RebaseInteractive` の拒否、
起動の失敗、終了待ちの失敗）。どれも `ExecError::NoResult` で返し、`reason` には git の
stderr ではなく mewgit の説明文を入れる。**中断はここに入れない** —— 終了状態は
得られており（signal で落ちた）、区別したいのは「失敗か、止めたか」だから
（`ExecError::Canceled`。上の「キャンセル」）。

## エラー分類（ExecErrorKind）

出力のパターンで分類し、UI が案内文を出せるようにする。**判定は上から順**で、
先に当たったものを採る。非 FF は語が広くなりやすいので、認証系より後ろに置く。

**`rejected` だけで非 FF と読まない。** hook や保護ブランチが断った push も
`! [remote rejected] main -> main (pre-receive hook declined)` と書く（git 2.55 で確認）。
`NonFastForward` にすると `app` が「次の `P` は force push」と憶える（01 の
「force push」）ので、履歴と関係の無い拒否に「リモートを上書きします」の確認が出る。
git が非 FF を表すのは 4 つ —— push が断る `(non-fast-forward)`、`(fetch first)`、
lease が古いときの `(stale info)` と、`pull --ff-only` が断る
`fatal: Not possible to fast-forward, aborting.`。

**4 つ目は `rejected` を含まない。** `pull --ff-only` は履歴が分かれているとこの
`fatal:` だけを書き、上の 3 語をどれも出さない（git 2.55 で確認）。**拾わないと
`p` の失敗が `Other` に落ち、案内文の側から消える** —— まさに案内が要る場面なのに
「git の実行に失敗しました」しか出ない。`hint:` の行は `advice.diverging false` で
消えるので、見るのは `fatal:` の側。

**広げたので読み手を数え直す。** `NonFastForward` を見るのは 2 人。`push_rejection`
（force を arm する。01 の「force push」）は `GitOp::Push` で門番しているので
`Pull` の失敗では立たない。もう 1 人は案内文（01 の「失敗の案内文」）で、そちらも
op で分けている。**#49 の「分類の読み手が増えたら偽陽性が何を引くかを数える」の
裏返し** —— 語を増やす側でも同じ数え方が要る。

**見るのは stderr だけではなく stdout も。** `git merge`（`pull --no-rebase` の経路）は
コンフリクトのとき `CONFLICT (content): ...` と `Automatic merge failed` を **stdout** に
出し、stderr には何も書かない。stderr だけで分類すると `Other` に落ちる
（git 2.55 で確認）。cherry-pick / rebase は `CONFLICT` が stdout、
`error: could not apply` が stderr と分かれる。

| パターン | kind |
|---|---|
| `SAML SSO` / `organization has enabled or enforced SAML` | `SamlSsoRequired` |
| `Permission denied (publickey)` | `SshAuthFailed` |
| `Authentication failed` | `HttpsAuthFailed` |
| `CONFLICT` / `could not apply` | `Conflict` |
| `is now empty` | `Empty` |
| `nothing to commit` / `nothing added to commit` / `no changes added to commit` | `NoChanges` |
| `non-fast-forward` / `fetch first` / `stale info` / `Not possible to fast-forward` | `NonFastForward` |
| その他 | `Other` |

**`NoChanges` は `Empty` より後ろ。** 空で止まった cherry-pick の stdout の末尾にも
`nothing to commit, working tree clean` が出る（git 2.55.0 で実測）ので、先に見ると
進行中操作が残っているのに「何も始まらなかった」と読む。**3 つの語は作業ツリーの状態で
割れる同じ結び**（clean / untracked がある / tracked が dirty。下の「revert の開始」）。
**`git commit` も同じ語を出す**（stage したものが無いとき）ので `Commit` の失敗も
`NoChanges` になるが、案内は op で分けてあり、commit には出さない（01 の「失敗の案内文」）。

**案内文は 01 の「失敗の案内文」が持つ。** ここに文言まで並べると、同じ kind でも
op で変わる分（非 FF は 4 通り）を表に書けないうえ、「次に何のキーを押すか」は
`git_exec` が答えられる問いではない（`moves_refs()` を `app` へ移したのと同じ数え方。
`.claude/rules/rust-coding.md` の「層をまたぐ述語」）。

**`SshAuthFailed` の案内にホストは入れていない。** `ssh -T <host>` を出すには
「どのリモートで失敗したか」が要るが、`GitOp::Pull` はリモート名を運ばず、
`RemoteUrl` は `user@` を捨てる（04）ので `git@` を書き足すと推測になる。
**M4 で `RepoInfo.target` が埋まったので、材料は届いている**（04 の「既定リモート」）。
ただし `target.host` は**既定リモートのホスト**で、失敗した `GitOp` が使ったリモートとは
限らない（`Push` は自分でリモート名を運ぶ）。**案内にホストを入れるなら、`GitOp` 側から
辿る**（#49 の「完了時に `state` から取る値は、発行時と同じである保証を数える」）。
**入れないと決めた**（2026-09-26、通知の履歴のタスク）。stderr は `Notice.detail` に
残って `L` の履歴で読め、**ssh は認証の失敗で自分からホストを名乗る** ——
`git@github.com: Permission denied (publickey).`（鍵を渡さずに `git ls-remote
git@github.com:octocat/Hello-World.git` を叩いて実測。git 2.55.0 / OpenSSH 10.3p1）。
失敗した `GitOp` から辿る配線を足さなくても、どのホストで断られたかは画面に出る。

分類するのは git の出力があるとき、つまり `ExecError::Failed` だけ。`NoResult` には
`kind` が無い（分類関数を通らない）。

## Interactive rebase の実装

`git rebase -i` は todo ファイルをエディタで開く。これを自前スクリプトで置き換える。

1. UI で `RebasePlan { entries: Vec<(Action, Oid, summary)> }` を作る
2. todo ファイルの内容を `plan.to_todo()` で生成し、一時ファイル `T` に書く
3. `GIT_SEQUENCE_EDITOR="cp T"` 相当のスクリプトを渡して `git rebase -i <onto>` を実行
   - 実際には `sh -c 'cat "$1" > "$2"'` 形式のヘルパーではなく、自バイナリを `--sequence-editor <T>` サブコマンド付きで指定する（外部依存ゼロ）
4. `reword` は `GIT_EDITOR` にも自バイナリを指定し、あらかじめ集めたメッセージを書き込む
5. 停止したら、書き込みのあとの `LoadStatus` が運ぶ進行中操作（`WorktreeSnapshot.progress`）がバッジに出て、`m` のメニューから continue / skip / abort を打つ（01 の「進行中操作のメニュー（`m`）」。専用のモードは持たない）

自バイナリをエディタとして使う場合、`mewgit --sequence-editor <path>` / `mewgit --commit-editor <path>` の隠しサブコマンドを用意する。

## 進行中操作との整合

- **`ExecFinished` は進行中操作を運ばない。** `exec_finished` が成功・失敗・中断を
  問わず出す `LoadStatus` の返り（`WorktreeSnapshot.progress`。02）で入れ替わる
  （`GitOp::SetAccount` / `UnsetAccount`（`.git/config` の 1 行だけを書く）は出さない —— 進行中操作は動かない）。
  **以前は「実行結果と 1 通に束ねる」と決めていた**（別々の `Msg` に割ると受け取る順序に
  依存するため）が、束ねると起動時・書き込みのあと・M6 の外部変更の検知で入口が 3 本に
  なり、それぞれに到着順の議論が要った。status に載せれば、古い返りを捨てるのは既存の
  世代の 1 本で済む（01 の「起動時の読み込み」）。**代わりにバッジは status が届くまで
  1 往復遅れる**が、そのあいだは `conflicted` の一覧も同じだけ遅れており、世代の門が
  その窓を「まだ信じない」と読むので食い違いにはならない（2026-09-26、`/simplify` の
  指摘を受けてユーザーと決めた）
- `Runner` は gitdir を知らない。中断した git の後始末（`CHERRY_PICK_HEAD` の削除など）は
  `run_cancelable` が終了を待ってから返るので、`LoadStatus` はそのあとを読む
- 進行中操作があるときは、**HEAD か作業ツリー全体を動かす書き込み**を `app` が
  git を起動する前に断る（下の「進行中に通す書き込み」。判定の置き場所は 01 の
  「進行中操作があるときの書き込み」）

### 進行中に通す書き込み

**以前は「continue / skip / abort 以外の書き込み `GitOp` を拒否する（UI 側でも無効化）」と
書いていたが、そのとおりに作るとコンフリクトから抜けられない。** 解消には stage
（`git add`）が要り、merge を結ぶのは `commit` で（merge の continue に当たる `GitOp` は
無い）、rebase の `edit` で止まったところで打つのは `commit --amend` だから。

**実測（git 2.55.0、2026-09-27）。** 状態 × 操作の組ごとに一時リポジトリを作り直して叩いた
（1 つのリポジトリで続けて叩くと、前の操作が状態を変える —— `stash push` が
`CHERRY_PICK_HEAD` を消したあとの `commit` は「cherry-pick の途中の commit」ではない）。
「解消済み」はコンフリクトしたファイルを `git add` したあと。

| 操作 | コンフリクト中（3 種とも） | 解消済みの rebase | 解消済みの cherry-pick / merge |
|---|---|---|---|
| `switch` | 断る（`cannot switch branch while rebasing` など） | 断る | 断る |
| `pull --ff-only` | 断る（unmerged files） | 断る（detached HEAD） | 断る（`You have not concluded your …`） |
| `cherry-pick` / `revert` | 断る（unmerged files） | 断る（local changes would be overwritten） | 断る |
| `rebase` の開始 | 断る | 断る（`already a rebase-merge directory`） | 断る（index contains uncommitted changes） |
| `stash push` | 断る（`could not write index`） | **通る**（解消した内容を stash が持っていく。`rebase-merge/` は残る） | **通り、`CHERRY_PICK_HEAD` / `MERGE_HEAD` が消える** |
| `commit` | 断る（unmerged files） | 通る（detached HEAD に積む） | 通り、**操作を結ぶ**（`*_HEAD` が消える） |
| `commit --amend` | 断る | 通る（`edit` の使い道そのもの） | 断る（`You are in the middle of a cherry-pick -- cannot amend`） |
| `fetch` / `push` | 通る | 通る | 通る |
| `branch -m` / `branch -D` | 通る（**rebase 中の対象ブランチは両方とも断る** —— `-m` は `is being rebased`、`-D` は `used by worktree`） | 同左 | 通る |

ここから決めたこと ——

- **断るのは HEAD か作業ツリー全体を動かす書き込み**（switch、pull、PR の取り出し、
  cherry-pick / revert / rebase の開始、stash の push / apply / pop など）。**git が自分で
  断るものも断る** —— 押すたびに赤い通知と argv を出すより、何の途中かと出口を先に言う
  ほうが読める。**`stash` だけは git が受けて進行中操作を壊す**ので、これは案内の都合では
  なく守り
- **通すのは解消に要るもの**（stage / unstage / ハンク / discard、`Commit`。amend も ——
  cherry-pick / merge の途中の amend は git が断る）**と、実測で無害なもの**（fetch、push、
  ブランチの改名・削除 —— **ただし rebase 中の対象ブランチは断る**。git も断るが、`r` は
  打った名前が消えるので入力を開く前に断りたい。対象ブランチは `rebase-*/head-name` から
  読む（02 の「進行中操作の検出」））。**変種ごとの振り分けは `app::update::progress::refusal` の
  網羅 `match` が正**で、ここには写さない
- **continue / skip / abort は、進行中の操作と種類が合うときだけ通す**（cherry-pick の途中の
  `RebaseContinue` は断る）。**merge は `MergeAbort` だけ** —— merge の continue に当たる
  `GitOp` は無く、結ぶのは `Commit`。switch / pull を断るので、`MergeAbort` が無いと
  merge の途中から mewgit で抜ける手が commit しか無い。`rebase -r` の merge で止まった形は
  rebase として検出する（02）ので、`MergeAbort` は断り、rebase の abort を案内する。**am には通すものが無い** —— mewgit から am は始めず、
  `GitOp` にも無い。端末で `git am --continue` / `--abort` を打つよう案内する
- **UI 側でキーを無効化しない**（フッターのキーヒントも消さない）。押せば断りの理由が
  通知に出るので、消えたキーを探させるより読める。ヒントを状態で出し分けると、
  `app::keys` の表が 1 本でなくなる（07 の「ステータスバー」）

### continue / skip / abort の終わり方

**結果は 3 通り**（`.claude/rules/rust-coding.md` の「二値だった結果に 3 つ目を足したら」）。
`Runner` と同じ `GIT_EDITOR=true LC_ALL=C` で、一時リポジトリを作り直して叩いた
（git 2.55.0、2026-09-27）。**`GIT_EDITOR=true` なので、continue がコミットメッセージの
エディタで止まることは無い**（解消したコミットは元のメッセージのまま積まれる）。

| 操作 | exit | stdout / stderr | 分類 |
|---|---|---|---|
| 解消して `rebase --continue` → 最後まで | 0 | stderr `Successfully rebased and updated refs/heads/<b>.` | 成功 |
| `edit` で止まったあと `rebase --continue` | 0 | 同上 | 成功 |
| 解消して `rebase --continue` / `rebase --skip` → **次のコミットでまたコンフリクト** | 1 | stdout `CONFLICT (content)`、stderr `could not apply` | `Conflict` |
| 解消して `cherry-pick --continue` → 次の pick でコンフリクト | 1 | 同上 | `Conflict` |
| 解消せずに `rebase --continue` | 1 | stdout `a: needs merge` / `You must edit all merge conflicts` | `Other` |
| 解消せずに `cherry-pick --continue` | 128 | stderr `Committing is not possible because you have unmerged files.` | `Other` |
| `rebase --abort` / `merge --abort` | 0 | 何も出さない | 成功 |
| 変更が空で止まった pick のあと `cherry-pick --continue` | 1 | stderr `The previous cherry-pick is now empty`（止まったときと同じ文） | `Empty` |
| 変更が空で止まった pick のあと `cherry-pick --skip` / `--abort` | 0 | 何も出さない | 成功 |

**空で止まった pick は continue では抜けられない**（上の表の 2 行。2026-09-29 に測った）。
`GIT_EDITOR=true` でも同じで、git は `commit --allow-empty` か `--skip` を求める。
**出口は skip か abort** なので、`m` のメニューの cherry-pick に skip を入れた（01 の
「進行中操作のメニュー（`m`）」）。**revert にも入れた** —— 解消した結果が空だと continue が
同じように断られる（下の「revert の開始」の表。stderr は空で、`Empty` ではなく
`NoChanges` に落ちる）。

**`Conflict` は失敗ではなく「次で止まった」** —— 進行中操作は残り、バッジの数が進む。
そこで赤を出すと壊れたように読めるので、`app` は `Warn` で次の手を案内する（01 の
「失敗の案内文」）。**解消していない `Other` は赤のまま** —— git の文言（`needs merge`）が
そのまま理由になる。

### cherry-pick の開始

**`GitOp::CherryPick` は 1 本だけの形で使う**（01 の「cherry-pick の開始（`y`）」。複数は
`docs/plan.md` で別の行に割った）。一時リポジトリを作り直して叩いた（git 2.55.0、
`GIT_EDITOR=true LC_ALL=C`、2026-09-29）。

| 選んだコミット・状態 | exit | stdout / stderr | 残るもの | 分類 |
|---|---|---|---|---|
| 別のブランチのコミット | 0 | stdout `[main c708635] t1` | 無し（HEAD が進む） | 成功 |
| 同上、detached HEAD | 0 | stdout `[detached HEAD d4b7235] t1` | 無し | 成功 |
| 同上、**別のファイル**が dirty | 0 | 同上 | dirty のまま | 成功 |
| 同上、**同じファイル**が dirty | 128 | stderr `Your local changes to the following files would be overwritten by merge` | 無し | `Other` |
| HEAD 自身・HEAD の祖先・merge で取り込み済みのコミット（後の履歴が同じ箇所を変えていない） | 1 | stderr `The previous cherry-pick is now empty` | **`CHERRY_PICK_HEAD`** | `Empty` |
| HEAD の祖先で、**後のコミットが同じ箇所を変えている** | 1 | stdout `CONFLICT (content)`、stderr `could not apply` | `CHERRY_PICK_HEAD`、`UU` | `Conflict` |
| コンフリクトするコミット | 1 | stdout `CONFLICT (content)`、stderr `could not apply` | `CHERRY_PICK_HEAD`、`UU` | `Conflict` |
| merge コミット | 128 | stderr `is a merge but no -m option was given` | 無し | `Other` |

- **どの失敗でも HEAD は動かない**（1 本だけのとき）。書き込みのあとの読み直し
  （01 の「ログの読み直し」）は、成功したときだけログまで読み直す。複数では途中まで
  積んでから止まるので、成否を見ずに読み直す
- **空で止まった形は `Empty` に分ける。** `Other` のままだと赤い `Display` に落ち、
  stdout の `all conflicts fixed: run "git cherry-pick --continue"` に従うと同じ理由で
  断られる。**`CONFLICT` は大文字で見る** —— この形の stdout にも小文字の `conflicts` が
  出る
- **`--empty=drop` は付けない。** 空の pick を落として exit 0 で返る（stderr に
  `dropping … patch contents already upstream`）が、**成功は通知に出さないので、押しても
  何も起きなかったように見える**
- **merge コミットは `app` が手前で断る**（`GitOp::CherryPick` は mainline を運ばない）。
  mainline を選ぶのは revert の開始（下）

### revert の開始

**`GitOp::Revert` は 1 本だけの形で使う**（01 の「revert の開始（`v`）」）。一時リポジトリを
作り直して叩いた（git 2.55.0、`GIT_EDITOR=true LC_ALL=C`、2026-09-29）。fixture は
`c1`（`a`）→ `c2`（`b`）→ `c3`（`a` を書き換え）の main に、`c1` から分けた topic（`t`）を
`--no-ff` で merge した形。

| 選んだコミット・状態 | exit | stdout / stderr | 残るもの | 分類 |
|---|---|---|---|---|
| 親が 1 つのコミット | 0 | stdout `[main 18dbc69] Revert "c2"` | 無し（HEAD が進む） | 成功 |
| 同上、**別のファイル**が dirty | 0 | 同上 | dirty のまま | 成功 |
| 同上、**同じファイル**が dirty | 128 | stderr `Your local changes to the following files would be overwritten by merge` | 無し | `Other` |
| merge コミット、`-m` 無し | 128 | stderr `is a merge but no -m option was given` | 無し | `Other` |
| merge コミット、`-m 1` | 0 | stdout `Revert "merge"`、**topic 側の `t` を消す** | 無し | 成功 |
| merge コミット、`-m 2` | 0 | stdout `Revert "merge"`、**main 側の `b` を消し `a` を戻す** | 無し | 成功 |
| merge コミット、`-m 3`（無い親） | 128 | stderr `does not have parent 3` | 無し | `Other` |
| 親が 1 つのコミットに `-m 1` | 0 | 成功と同じ | 無し | 成功 |
| 後の履歴が同じ箇所を変えている（`c1`） | 1 | stdout `CONFLICT (modify/delete)`、stderr `could not revert` | `REVERT_HEAD`、`UD`（**`sequencer/` は無し**） | `Conflict` |
| 取り消し済みのコミット | 1 | stdout `nothing to commit, working tree clean`、**stderr 空** | **無し**（`REVERT_HEAD` も `sequencer/` も残らない） | `NoChanges` |
| 同上、untracked がある | 1 | stdout `nothing added to commit but untracked files present` | 無し | `NoChanges` |
| 同上、tracked が dirty | 1 | stdout `no changes added to commit` | 無し | `NoChanges` |
| コンフリクトのあと `revert --skip` | 0 | 何も出さない | 無し（HEAD は元のまま） | 成功 |
| コンフリクトを **HEAD 側の内容で解いて** `revert --continue` | 1 | stdout `You are currently reverting commit …` + `nothing to commit`、**stderr 空** | **`REVERT_HEAD`**（進行中のまま） | `NoChanges` |
| ↑ のあと `revert --skip` | 0 | 何も出さない | 無し（HEAD は元のまま） | 成功 |
| （参考）複数の revert で skip した先が空 | 1 | stdout `nothing to commit`、stderr 空 | **`sequencer/` だけ**（`todo` の先頭行は `revert …`） | `NoChanges` |
| （参考）複数の revert で途中が空 | 1 | stdout `Revert currently in progress.` + 上の 3 つの結びのどれか | **`sequencer/` だけ**（`REVERT_HEAD` は無い） | `NoChanges` |

- **どの失敗でも HEAD は動かない**（1 本だけのとき）。cherry-pick と同じく、成功したときだけ
  ログまで読み直す（01 の「ログの読み直し」）
- **`--mainline N` は N 番目の親を残す。** `-m 1` と `-m 2` で消えるファイルが逆になる
  （表の 2 行）。画面では「残す側の親」と言う（01、07）
- **取り消す変更が無い形は `NoChanges` に分ける**（上の「エラー分類」）。stderr が空なので、
  `Other` のままだと argv だけの赤い通知になる
- **1 本だけでも continue で空になる。** コンフリクトを HEAD 側の内容で解くと、開始と
  同じ文言で断られるが **`REVERT_HEAD` が残る**。**`NoChanges` は進行中操作が残ったかを
  言わない**ので、案内は op で分ける（開始なら「取り消し済み」、continue / skip なら
  「m から skip」。01 の「失敗の案内文」）。skip の先が空でも `sequencer/` が残って止まり、
  `todo` の先頭行が `revert` なので 02 の検出が `Revert` と読む（`m` の skip が届く）
- **複数の revert で途中が空だと止まる**（参考行）。`sequencer/` だけが残る形で、
  `REVERT_HEAD` が無いので skip で抜ける。これを拾うのは複数の revert のタスク ——
  02 の検出表は `sequencer/` だけの形を `sequencer/todo` の先頭行で見るので、そこで
  `Revert` と読めるかも一緒に確かめる

