//! `git_exec` モジュール。設計は `docs/design/03-git-exec.md` を参照。
//!
//! 書き込みはすべて `git` サブプロセスで行う（不変ルール 1）。ssh 設定・credential
//! helper・hook・`GIT_*` 環境変数を自動で効かせるため。

mod argv;
mod runner;

use std::fmt;
use std::path::PathBuf;

use crate::git_read::Oid;

pub use argv::{to_command, ArgvError, GitCommand};
pub use runner::Runner;

/// 書き込み操作。[`to_command`] で `git` の argv と標準入力に変換する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitOp {
    Stage(Vec<PathBuf>),
    Unstage(Vec<PathBuf>),
    /// ハンク単位の stage。`patch` は `git apply --cached` に渡すパッチそのもの。
    ///
    /// `path` は argv に載らない（パッチのファイルヘッダが対象を持つ）。UI が
    /// 「どのファイルを stage したか」を通知に出すために持つ。
    StageHunk {
        path: PathBuf,
        /// **`String` ではなく `Vec<u8>`。** lossy 変換すると、非 UTF-8 のファイルを
        /// ステージした結果が元のファイルと変わる（02 の「ハンクの持ち方」）。
        patch: Vec<u8>,
    },
    UnstageHunk {
        path: PathBuf,
        patch: Vec<u8>,
    },
    /// tracked なファイルの変更を HEAD の内容に戻す。
    ///
    /// untracked なファイルは `git restore` では消せないので
    /// [`GitOp::DiscardUntracked`] に分ける。どちらを出すかは区画を知っている
    /// 呼び出し側が決める（`to_command` は純粋関数でファイルシステムを見られない）。
    Discard(Vec<PathBuf>),
    /// untracked なファイルを削除する（`git clean`）。
    DiscardUntracked(Vec<PathBuf>),
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
///
/// `thiserror` を使わず `Display` を手で書いているのは、[`ExecErrorKind::NotAttempted`]
/// のときだけ文面を変えるため。「失敗しました: <argv>」は、そもそも起動していない
/// ときには嘘になる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecError {
    /// 実行した（あるいは実行しようとした）argv。`git` 自身は含まない。
    /// argv を組めずに失敗したときだけ空になる。
    pub argv: Vec<String>,
    pub code: Option<i32>,
    pub stderr: String,
    pub kind: ExecErrorKind,
}

impl fmt::Display for ExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.kind == ExecErrorKind::NotAttempted {
            write!(f, "git を実行しませんでした: {}", self.stderr)
        } else {
            write!(f, "git の実行に失敗しました: git {}", self.argv.join(" "))
        }
    }
}

impl std::error::Error for ExecError {}

/// 出力のパターンから分類したエラー種別。UI の案内文を切り替えるのに使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecErrorKind {
    SamlSsoRequired,
    SshAuthFailed,
    HttpsAuthFailed,
    Conflict,
    NonFastForward,
    Other,
    /// `git` を起動していない。argv を組めなかったか、`Runner` が実行を拒否した。
    /// プロセスが無いので `code` は `None`、`stderr` には mewgit の説明文が入る。
    NotAttempted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_of_exec_error_shows_the_argv() {
        let err = ExecError {
            argv: vec!["add".to_string(), "--".to_string(), "a.txt".to_string()],
            code: Some(128),
            stderr: "fatal: pathspec".to_string(),
            kind: ExecErrorKind::Other,
        };
        assert_eq!(
            err.to_string(),
            "git の実行に失敗しました: git add -- a.txt"
        );
    }

    #[test]
    fn display_of_not_attempted_shows_the_reason() {
        // 起動していないので argv を並べても意味が無い。理由を出す。
        let err = ExecError {
            argv: Vec::new(),
            code: None,
            stderr: "パスが UTF-8 ではありません".to_string(),
            kind: ExecErrorKind::NotAttempted,
        };
        assert_eq!(
            err.to_string(),
            "git を実行しませんでした: パスが UTF-8 ではありません"
        );
    }
}
