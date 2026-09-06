# 03. git_exec: git CLI 実行層

## 責務

すべての書き込み操作を `git` サブプロセスで実行する。ssh 設定、credential helper、hook、`GIT_*` 環境変数が自動で効くようにするため。

## GitOp

```rust
pub enum GitOp {
    Stage(Vec<PathBuf>), Unstage(Vec<PathBuf>),
    StageHunk { path, patch: Vec<u8> }, UnstageHunk { .. },
    Discard(Vec<PathBuf>), DiscardUntracked(Vec<PathBuf>),
    Commit { message: String, amend: bool },
    BranchCreate { name, start: Option<String>, checkout: bool },
    Checkout(String), BranchDelete { name, force }, BranchRename { from, to },
    Fetch { remote: Option<String>, prune: bool },
    Pull { mode: PullMode }, Push { remote, branch, set_upstream, force },
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
- **discard は tracked と untracked で変種を分ける。** untracked なファイルは
  `git restore` では消せず `git clean` が要る。`to_command` は純粋関数でファイルシステムを
  見られないので、どちらを出すかは区画を知っている呼び出し側（`WorktreeSnapshot` を持つ
  `app`）が決める

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
| `StageHunk { patch }` | `apply --cached -`（patch を stdin） |
| `UnstageHunk { patch }` | `apply --cached --reverse -`（patch を stdin） |
| `Discard(paths)` | `restore --source=HEAD --staged --worktree -- <paths>` |
| `DiscardUntracked(paths)` | `clean --force -- <paths>` |
| `Commit { message, amend }` | `commit [--amend] --message <message>` |
| `BranchCreate { checkout: true }` | `switch --create <name> [<start>]` |
| `BranchCreate { checkout: false }` | `branch <name> [<start>]` |
| `Checkout(target)` | `checkout <target> --` |
| `BranchDelete { name, force }` | `branch --delete [--force] <name>` |
| `BranchRename { from, to }` | `branch --move <from> <to>` |
| `Fetch { remote, prune }` | `fetch [--prune] [<remote>]` |
| `Pull { mode }` | `pull --no-rebase` / `--rebase` / `--ff-only` |
| `Push { .. }` | `push [--force-with-lease] [--set-upstream] <remote> <branch>` |
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
- **unstage は `restore --staged` ではなく `reset`。** コミットが 1 つも無いリポジトリ
  （unborn HEAD）で `git restore --staged` は `fatal: could not resolve 'HEAD'` で落ちる。
  `git reset -- <paths>` は同じ状態でも通る（git 2.55 で確認）。`--quiet` は
  「Unstaged changes after reset:」の一覧を出させないため
- **`checkout <target> --` の `--` は revision として読ませるため。** 同名のファイルが
  あるときに「そのファイルを取り出す」と解釈されるのを防ぐ
- **force push は `--force-with-lease`。** リモートが自分の知らない位置に進んでいたら
  弾かれる。`--force` は他人のコミットを消せるので使わない
- **untracked の discard に `-d` は付けない。** `git_read::status` が
  `recurse_untracked_dirs(true)` で列挙するので、渡すのは常にファイル。ディレクトリごと
  消す余地を残さない
- `switch` / `restore` を使うので git 2.23 以降が要る

## Runner

```rust
pub struct Runner { repo_root: PathBuf, env: Vec<(OsString, OsString)> }

impl Runner {
    pub fn new(repo_root: impl Into<PathBuf>) -> Self;
    /// 実行時に追加する環境変数。テストが実行環境の設定を持ち込まないために使う。
    pub fn with_env(self, vars: ...) -> Self;
    pub async fn run(&self, op: &GitOp) -> Result<ExecOutput, ExecError>;
}

pub struct ExecOutput { pub argv: Vec<String>, pub stdout: String, pub stderr: String }
pub struct ExecError { pub argv: Vec<String>, pub code: Option<i32>, pub stderr: String, pub kind: ExecErrorKind }
```

- argv と stdin の組み立ては純粋関数 `to_command()`（「argv への変換」）
- `git` は PATH から引く。実行するバイナリを設定で差し替える要求が無いので、パスは持たない
- 環境変数: `GIT_TERMINAL_PROMPT=0`、`LC_ALL=C`（エラー文の検出を安定させる）、`GIT_EDITOR=true`（意図しないエディタ起動を防ぐ）。`with_env` の指定はこの後に重ねるので、テストは上書きできる
- stdin は閉じる。ハンク適用は `git apply --cached -` に patch を stdin で渡す例外
- stdout / stderr は表示専用なので lossy で `String` にする
- タイムアウトは設けない（fetch の長さは予測不能）。代わりに UI からキャンセル可能にし、子プロセスを kill する（`kill_on_drop`）
- **`RebaseInteractive` は `Runner` が実行せずに拒否する**（M5 まで）。todo を差し替えずに `git rebase -i` を走らせると、`GIT_EDITOR=true` により全 `pick` の rebase が実行され、`plan` を黙って捨てた破壊的操作になる。M5 で `--sequence-editor` を配線したら外す
- **進行中操作（`InProgressOp`）の再検出は M5。** `git_read::in_progress` がまだ無いので、`ExecDone.progress` はランタイムが `None` を入れる

`git` を起動しないまま失敗する経路が 2 つある（上の `RebaseInteractive` と、非 UTF-8 の
パスで `to_command` が失敗する場合）。どちらも `ExecErrorKind::NotAttempted` で返す。
プロセスを起動していないので `code` は `None`、`stderr` には git ではなく mewgit の
説明文が入る。

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

`NotAttempted` はこの表に載らない。stderr のパターンではなく、git を起動しなかった
ことを表すため（分類関数は通らない）。

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
