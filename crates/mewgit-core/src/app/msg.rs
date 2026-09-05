//! `update` に入力されるイベント。設計は `docs/design/01-app-core.md`。

use crate::auth::AccountState;
use crate::git_exec::{ExecError, ExecOutput, GitOp};
use crate::git_read::{Commit, DiffTarget, FileDiff, InProgressOp, WorktreeSnapshot};
use crate::github::{Check, GithubError, PullRequest};

use super::{Key, TermSize};

/// 状態を動かすイベント。端末入力と非同期タスクの完了の両方が入る。
///
/// `Clone` は要求しない。[`Account`](crate::auth::Account) がトークン
/// （`SecretString`）を持つため。
///
/// 大きい payload は `Box` に入れる。入れないと `Msg` が 336 バイトになり、1 打鍵ごとに
/// 作られる [`Msg::Key`]（実データ 12 バイト）まで同じ大きさで動く。[`Msg::ExecFinished`]
/// と [`Msg::PrLoaded`] の 2 つを箱に入れると 96 バイトになる。
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
        /// ファイル単位。平坦化すると `GitOp::StageHunk` に渡す `path` を失う。
        files: Vec<FileDiff>,
    },

    // git_exec
    /// `git` の実行が終わった。`Runner` が実行直後に再検出した進行中操作を一緒に運ぶ（03）。
    ExecFinished(Box<ExecDone>),
    /// `.git/HEAD` の監視など、`GitOp` 以外の経路で進行中操作が変わった（M6）。
    ProgressDetected(Option<InProgressOp>),

    // auth / github
    /// `auth::resolve` の結果。`Result<Account, AuthError>` にしないのは、`AuthError` と
    /// `DegradedReason` が同じ失敗を二重に表現してしまうため（04）。`AuthError` は
    /// `gh` / `ssh` 実行の内部エラーに留め、`resolve` の中で `DegradedReason` に畳む。
    AccountResolved(AccountState),
    PrLoaded(Box<Result<Option<PullRequest>, GithubError>>),
    ChecksLoaded(Result<Vec<Check>, GithubError>),
    PrListLoaded(Result<Vec<PullRequest>, GithubError>),
}

/// [`Msg::ExecFinished`] の中身。
#[derive(Debug)]
pub struct ExecDone {
    pub op: GitOp,
    pub result: Result<ExecOutput, ExecError>,
    /// 実行直後に再検出した進行中操作。実行結果と別の `Msg` に割ると `update` が
    /// 受け取る順序に依存するので、1 通にまとめる（03）。
    pub progress: Option<InProgressOp>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msg_stays_small() {
        // `Msg::Key` は 1 打鍵ごとに作られるが、enum のサイズは最大変種で決まる。
        // Box を外したり大きい payload を直に持たせると 300 バイト超になるので、
        // 上限で見張る（実測値は 64bit で 96）。
        let size = std::mem::size_of::<Msg>();
        assert!(size <= 128, "Msg が大きすぎます: {size} バイト");
    }
}
