# 03. git_exec: git CLI 実行層

## 責務

すべての書き込み操作を `git` サブプロセスで実行する。ssh 設定、credential helper、hook、`GIT_*` 環境変数が自動で効くようにするため。

## GitOp

```rust
pub enum GitOp {
    Stage(Vec<PathBuf>), Unstage(Vec<PathBuf>),
    StageHunk { path, patch: String }, UnstageHunk { .. },
    Discard(Vec<PathBuf>),
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

## Runner

```rust
pub struct Runner { repo_root: PathBuf, git_bin: PathBuf }

impl Runner {
    pub async fn run(&self, op: GitOp) -> Result<ExecOutput, ExecError>;
}

pub struct ExecOutput { pub argv: Vec<String>, pub stdout: String, pub stderr: String }
pub struct ExecError { pub argv: Vec<String>, pub code: Option<i32>, pub stderr: String, pub kind: ExecErrorKind }
```

- `GitOp → Vec<String>` の変換は純粋関数 `to_argv()` として分離し、テストする
- 環境変数: `GIT_TERMINAL_PROMPT=0`、`LC_ALL=C`（エラー文の検出を安定させる）、`GIT_EDITOR=true`（意図しないエディタ起動を防ぐ）
- stdin は閉じる。ハンク適用は `git apply --cached -` に patch を stdin で渡す例外
- タイムアウトは設けない（fetch の長さは予測不能）。代わりに UI からキャンセル可能にし、子プロセスを kill する

## エラー分類（ExecErrorKind）

stderr のパターンで分類し、UI が案内文を出せるようにする。

| パターン | kind | UI の案内 |
|---|---|---|
| `SAML SSO` / `organization has enabled or enforced SAML` | `SamlSsoRequired` | SSH 鍵の SSO 承認手順 |
| `Permission denied (publickey)` | `SshAuthFailed` | `ssh -T <host>` での確認を促す |
| `CONFLICT` / `could not apply` | `Conflict` | 進行中モードへ遷移 |
| `non-fast-forward` / `rejected` | `NonFastForward` | pull または force push の選択 |
| `Authentication failed` | `HttpsAuthFailed` | `gh auth setup-git` を案内 |
| その他 | `Other` | stderr をそのまま表示 |

## Interactive rebase の実装

`git rebase -i` は todo ファイルをエディタで開く。これを自前スクリプトで置き換える。

1. UI で `RebasePlan { entries: Vec<(Action, Oid, summary)> }` を作る
2. todo ファイルの内容を `plan.to_todo()` で生成し、一時ファイル `T` に書く
3. `GIT_SEQUENCE_EDITOR="cp T"` 相当のスクリプトを渡して `git rebase -i <onto>` を実行
   - 実際には `sh -c 'cat "$1" > "$2"'` 形式のヘルパーではなく、自バイナリを `--sequence-editor <T>` サブコマンド付きで指定する（外部依存ゼロ）
4. `reword` は `GIT_EDITOR` にも自バイナリを指定し、あらかじめ集めたメッセージを書き込む
5. 停止したら `ProgressDetected` で `Rebase` モードに入り、continue / skip / abort を提供

自バイナリをエディタとして使う場合、`mewgit --sequence-editor <path>` / `mewgit --commit-editor <path>` の隠しサブコマンドを用意する。

## 進行中操作との整合

- `Runner` は `GitOp` 実行後に必ず `InProgressOp` を再検出して `ExecFinished` と一緒に返す
- 進行中操作があるときは、continue / skip / abort 以外の書き込み `GitOp` を拒否する（UI 側でも無効化）
