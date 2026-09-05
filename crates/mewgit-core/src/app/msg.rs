//! `update` に入力されるイベント。設計は `docs/design/01-app-core.md`。

use crate::auth::{Account, AuthError};
use crate::git_exec::{ExecError, ExecOutput, GitOp};
use crate::git_read::{Commit, DiffTarget, Hunk, InProgressOp, WorktreeSnapshot};
use crate::github::{Check, GithubError, PullRequest};

use super::{Key, TermSize};

/// 状態を動かすイベント。端末入力と非同期タスクの完了の両方が入る。
///
/// `Clone` は要求しない。[`Account`] がトークン（`SecretString`）を持つため。
/// [`Modal::Confirm`](super::Modal::Confirm) は `on_yes` をムーブで取り出す。
#[derive(Debug)]
pub enum Msg {
    Key(Key),
    Resize(TermSize),
    /// 250ms 周期。スピナー更新と外部変更検知に使う。
    Tick,

    // git_read
    LogLoaded {
        commits: Vec<Commit>,
        /// これ以上読むコミットが無い。
        done: bool,
    },
    StatusLoaded(WorktreeSnapshot),
    DiffLoaded {
        target: DiffTarget,
        hunks: Vec<Hunk>,
    },

    // git_exec
    /// `git` の実行が終わった。`Runner` が実行直後に再検出した進行中操作を一緒に運ぶ（03）。
    ExecFinished {
        op: GitOp,
        result: Result<ExecOutput, ExecError>,
        progress: Option<InProgressOp>,
    },
    /// `.git/HEAD` の監視など、`GitOp` 以外の経路で進行中操作が変わった（M6）。
    ProgressDetected(Option<InProgressOp>),

    // auth / github
    AccountResolved(Result<Account, AuthError>),
    PrLoaded(Result<Option<PullRequest>, GithubError>),
    ChecksLoaded(Result<Vec<Check>, GithubError>),
    PrListLoaded(Result<Vec<PullRequest>, GithubError>),
}
