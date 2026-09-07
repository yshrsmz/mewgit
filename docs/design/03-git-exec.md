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
    Push { remote, branch, set_upstream, force, expected: Option<Oid> },
    RebaseInteractive { onto: String, plan: RebasePlan },
    RebaseContinue, RebaseSkip, RebaseAbort,
    CherryPick(Vec<Oid>), CherryPickContinue, CherryPickAbort,
    Revert { oids: Vec<Oid>, mainline: Option<u32> }, RevertContinue, RevertAbort,
    StashPush { message, include_untracked, paths: Option<Vec<PathBuf>> },
    StashApply(usize), StashPop(usize), StashDrop(usize),
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
  落ちるので `git rm` が要る。`to_command` は純粋関数で index もファイルシステムも
  見られないので、どれを出すかは区画と変更種別を知っている呼び出し側
  （`WorktreeSnapshot` を持つ `app`）が決める

## argv への変換

`GitOp` から argv と stdin を作るのは純粋関数 `to_command`。argv だけでなく stdin も
一緒に返すのは、「どの引数で何を標準入力に流すか」が 1 つの判断だから（`git apply --cached -`）。
2 つの関数に割ると、変種を足したときに片方だけ直る。

```rust
pub struct GitCommand { pub argv: Vec<String>, pub stdin: Option<Vec<u8>> }

pub fn to_command(op: &GitOp) -> Result<GitCommand, ArgvError>;
```

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
| `Push { .. }` | `push [--force-with-lease[=<branch>:<expected>]] [--set-upstream] <remote> <branch>` |
| `RebaseInteractive { onto }` | `rebase --interactive <onto>` |
| `RebaseContinue` / `Skip` / `Abort` | `rebase --continue` / `--skip` / `--abort` |
| `CherryPick(oids)` | `cherry-pick <oids>` |
| `CherryPickContinue` / `Abort` | `cherry-pick --continue` / `--abort` |
| `Revert { oids, mainline }` | `revert [--mainline <n>] <oids>` |
| `RevertContinue` / `Abort` | `revert --continue` / `--abort` |
| `StashPush { .. }` | `stash push [--include-untracked] [--message <m>] [-- <paths>]` |
| `StashApply(i)` / `Pop` / `Drop` | `stash apply` / `pop` / `drop` `stash@{i}` |

決めたことと理由:

- **パスの前には必ず `--` を置く。** `-n` や `HEAD` という名前のファイルがオプションや
  revision として解釈されるのを防ぐ
- **ユーザーが打った文字が positional に載るところにも `--` を置く。** いまは
  `BranchRename` の `to` だけ（`Modal::Input` が受けた文字がそのまま来る）。
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
  `to_command` のテストでだけ踏む）
- **force push は `--force-with-lease`。`--force` は他人のコミットを消せるので使わない。**
  **値（`=<branch>:<expected>`）を必ず添える。** 省くと lease は remote-tracking ref
  （`origin/main`）と比べるだけになり、`git fetch` を挟むと他人が進めた位置がそこに入って
  検査を素通りする（git 2.55 で確認）。「push が弾かれた → fetch して中身を見る →
  やはり force する」は UI が普通に通す道なので、素の lease は守りにならない。
  `expected` には確認モーダルで見せた位置をそのまま渡す
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
- 非 UTF-8 のパスは `to_command` と同じ `ArgvError::NonUtf8Path`。`git_read` が
  一覧ごと落とすので実際には来ない

## Runner

```rust
pub struct Runner { repo_root: PathBuf, git_bin: PathBuf, env: Vec<(OsString, OsString)> }

impl Runner {
    pub fn new(repo_root: impl Into<PathBuf>, git_bin: impl Into<PathBuf>) -> Self;
    /// 実行時に重ねる環境変数。テストの隔離と、M5 のエディタ差し替えに使う。
    pub fn with_env(self, vars: ...) -> Self;
    pub async fn run(&self, op: &GitOp) -> Result<ExecOutput, ExecError>;
}

pub struct ExecOutput { pub argv: Vec<String>, pub stdout: String, pub stderr: String }

pub enum ExecError {
    /// git の終了状態を得られなかった。argv を組めなかった / Runner が拒否した /
    /// 起動できなかった / 終了を待てなかった。
    NoResult { argv: Vec<String>, reason: String },
    /// git が非 0 で終わった。
    Failed { argv: Vec<String>, code: Option<i32>, stderr: String, kind: ExecErrorKind },
}
```

- argv と stdin の組み立ては純粋関数 `to_command()`（「argv への変換」）
- `git_bin` は `config` の `general.git_bin`（既定は `"git"`、絶対パスも書ける。08）から渡す。
  引数にしているのは、設定を読んだのに渡し忘れる経路を作らないため
- **`ExecError` を 2 つに割るのは、失敗の種類で埋まるフィールドが違うため。** 1 つの
  struct にすると「起動していないときは `code` が `None`、`stderr` に git ではない文言、
  `kind` は分類表に載らない値」という約束を、読む側が毎回思い出すことになる
- 環境変数: `GIT_TERMINAL_PROMPT=0`、`LC_ALL=C`（エラー文の検出を安定させる）、`GIT_EDITOR=true`（意図しないエディタ起動を防ぐ）。`with_env` の指定はこの後に重ねるので、テストは上書きできる
- **`GIT_TERMINAL_PROMPT=0` では ssh のパスフレーズ入力を止められない。** `ssh` は
  stdin ではなく `/dev/tty` を直接読むので、agent に載っていない鍵だと raw mode の画面に
  プロンプトが出てそのまま止まる（タイムアウトは設けない方針、キャンセルは M3）。
  `GIT_SSH_COMMAND="ssh -o BatchMode=yes"` を既定に置けば `Permission denied (publickey)`
  で即座に失敗して分類できるが、ユーザーの `GIT_SSH_COMMAND` や `core.sshCommand` を
  黙って上書きする。**どう避けるかは M3（fetch / pull / push）で決める。**
  それまでこの経路は UI から到達しない
- stdin は閉じる。例外はハンクの 3 操作（`StageHunk` / `UnstageHunk` / `DiscardHunk`）
  だけで、どれも `git apply … -` に patch を stdin で渡す
- stdout / stderr は表示専用なので lossy で `String` にする
- タイムアウトは設けない（fetch の長さは予測不能）。代わりに UI からキャンセル可能にし、子プロセスを kill する（`kill_on_drop`）
- **`RebaseInteractive` は `Runner` が実行せずに拒否する**（M5 まで）。todo を差し替えずに `git rebase -i` を走らせると、`GIT_EDITOR=true` により全 `pick` の rebase が実行され、`plan` を黙って捨てた破壊的操作になる。M5 で `--sequence-editor` を配線したら外す
- **進行中操作（`InProgressOp`）の再検出は M5。** `git_read::in_progress` がまだ無いので、`ExecDone.progress` はランタイムが `None` を入れる

終了状態を得られない経路は 4 つ（`to_command` の失敗、`RebaseInteractive` の拒否、
起動の失敗、終了待ちの失敗）。どれも `ExecError::NoResult` で返し、`reason` には git の
stderr ではなく mewgit の説明文を入れる。

## エラー分類（ExecErrorKind）

出力のパターンで分類し、UI が案内文を出せるようにする。**判定は上から順**で、
先に当たったものを採る。`rejected` は広いので、認証系より後ろに置く。

**見るのは stderr だけではなく stdout も。** `git merge`（`pull --no-rebase` の経路）は
コンフリクトのとき `CONFLICT (content): ...` と `Automatic merge failed` を **stdout** に
出し、stderr には何も書かない。stderr だけで分類すると `Other` に落ちる
（git 2.55 で確認）。cherry-pick / rebase は `CONFLICT` が stdout、
`error: could not apply` が stderr と分かれる。

| パターン | kind | UI の案内 |
|---|---|---|
| `SAML SSO` / `organization has enabled or enforced SAML` | `SamlSsoRequired` | SSH 鍵の SSO 承認手順 |
| `Permission denied (publickey)` | `SshAuthFailed` | `ssh -T <host>` での確認を促す |
| `Authentication failed` | `HttpsAuthFailed` | `gh auth setup-git` を案内 |
| `CONFLICT` / `could not apply` | `Conflict` | 進行中モードへ遷移 |
| `non-fast-forward` / `rejected` | `NonFastForward` | pull または force push の選択 |
| その他 | `Other` | stderr をそのまま表示 |

分類するのは git の出力があるとき、つまり `ExecError::Failed` だけ。`NoResult` には
`kind` が無い（分類関数を通らない）。

## Interactive rebase の実装

`git rebase -i` は todo ファイルをエディタで開く。これを自前スクリプトで置き換える。

1. UI で `RebasePlan { entries: Vec<(Action, Oid, summary)> }` を作る
2. todo ファイルの内容を `plan.to_todo()` で生成し、一時ファイル `T` に書く
3. `GIT_SEQUENCE_EDITOR="cp T"` 相当のスクリプトを渡して `git rebase -i <onto>` を実行
   - 実際には `sh -c 'cat "$1" > "$2"'` 形式のヘルパーではなく、自バイナリを `--sequence-editor <T>` サブコマンド付きで指定する（外部依存ゼロ）
4. `reword` は `GIT_EDITOR` にも自バイナリを指定し、あらかじめ集めたメッセージを書き込む
5. 停止したら `ExecFinished` に添えた `progress` で `Rebase` モードに入り、continue / skip / abort を提供

自バイナリをエディタとして使う場合、`mewgit --sequence-editor <path>` / `mewgit --commit-editor <path>` の隠しサブコマンドを用意する。

## 進行中操作との整合

- `Runner` は `GitOp` 実行後に必ず `InProgressOp`（`git_read::in_progress`）を再検出し、
  `ExecFinished` の `progress` として一緒に返す。実行結果と進行中操作を別々の `Msg` に
  割ると `update` が受け取る順序に依存してしまうため、1 通にまとめる
  （M5。`git_read::in_progress` ができるまではランタイムが `None` を入れる）
- 進行中操作があるときは、continue / skip / abort 以外の書き込み `GitOp` を拒否する（UI 側でも無効化）
