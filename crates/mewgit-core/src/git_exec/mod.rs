//! `git_exec` モジュール。設計は `docs/design/03-git-exec.md` を参照。
//!
//! 書き込みはすべて `git` サブプロセスで行う（不変ルール 1）。ssh 設定・credential
//! helper・hook・`GIT_*` 環境変数を自動で効かせるため。
//!
//! 現時点では `app` の `Msg` / `Cmd` が参照する型だけを定義している。`Runner` と
//! `to_argv` は M2 以降で実装する。

use std::path::PathBuf;

use crate::git_read::Oid;

/// 書き込み操作。`to_argv` で `git` の argv に変換する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitOp {
    Stage(Vec<PathBuf>),
    Unstage(Vec<PathBuf>),
    StageHunk {
        path: PathBuf,
        patch: String,
    },
    UnstageHunk {
        path: PathBuf,
        patch: String,
    },
    Discard(Vec<PathBuf>),
    Commit {
        message: String,
        amend: bool,
    },
    BranchCreate {
        name: String,
        start: Option<String>,
        checkout: bool,
    },
    Checkout(String),
    BranchDelete {
        name: String,
        force: bool,
    },
    BranchRename {
        from: String,
        to: String,
    },
    Fetch {
        remote: Option<String>,
        prune: bool,
    },
    Pull {
        mode: PullMode,
    },
    Push {
        remote: String,
        branch: String,
        set_upstream: bool,
        force: bool,
    },
    RebaseInteractive {
        onto: String,
        plan: RebasePlan,
    },
    RebaseContinue,
    RebaseSkip,
    RebaseAbort,
    CherryPick(Vec<Oid>),
    CherryPickContinue,
    CherryPickAbort,
    Revert {
        oids: Vec<Oid>,
        mainline: Option<u32>,
    },
    RevertContinue,
    RevertAbort,
    StashPush {
        message: Option<String>,
        include_untracked: bool,
        paths: Option<Vec<PathBuf>>,
    },
    StashApply(usize),
    StashPop(usize),
    StashDrop(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullMode {
    Merge,
    Rebase,
    FastForwardOnly,
}

/// interactive rebase の todo。`to_todo` で todo ファイルの内容にする。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebasePlan {
    pub entries: Vec<RebaseEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebaseEntry {
    pub action: RebaseAction,
    pub oid: Oid,
    pub summary: String,
    /// `reword` / `squash` で使う書き換え後のメッセージ。
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebaseAction {
    Pick,
    Reword,
    Edit,
    Squash,
    Fixup,
    Drop,
}

/// 成功した `git` 実行の結果。`argv` は UI に見せるために必ず保持する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub argv: Vec<String>,
    pub stdout: String,
    pub stderr: String,
}

/// 失敗した `git` 実行。argv と stderr を握りつぶさない（不変ルール 4）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("git の実行に失敗しました: {}", .argv.join(" "))]
pub struct ExecError {
    pub argv: Vec<String>,
    pub code: Option<i32>,
    pub stderr: String,
    pub kind: ExecErrorKind,
}

/// stderr のパターンから分類したエラー種別。UI の案内文を切り替えるのに使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecErrorKind {
    SamlSsoRequired,
    SshAuthFailed,
    HttpsAuthFailed,
    Conflict,
    NonFastForward,
    Other,
}
