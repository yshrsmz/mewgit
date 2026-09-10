//! `git_exec` モジュール。設計は `docs/design/03-git-exec.md` を参照。
//!
//! 書き込みはすべて `git` サブプロセスで行う（不変ルール 1）。ssh 設定・credential
//! helper・hook・`GIT_*` 環境変数を自動で効かせるため。

mod argv;
mod patch;
mod runner;

use std::path::PathBuf;

use crate::git_read::Oid;

pub use argv::{push_remote_ref, to_command, ArgvError, GitCommand};
pub use patch::{hunk_patch, supports_hunk_patch};
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
    /// ハンク単位の discard。**ワークツリーだけを書き換える**（`--cached` は付かない）。
    ///
    /// index とワークツリーの両方に当てる変種は作らない。`git apply --index` は
    /// 2 つが一致していることを要求するので、staged な変更があるファイルでは
    /// `does not match index` で落ちる。2 回に割ると片方だけ通った中途半端な状態が
    /// 残るので、staged なハンクの discard は UI が「先に unstage してください」と
    /// 断る（03）。
    DiscardHunk {
        path: PathBuf,
        patch: Vec<u8>,
    },
    /// tracked なファイルの変更を HEAD の内容に戻す。
    ///
    /// untracked なファイルは `git restore` では消せないので
    /// [`GitOp::DiscardUntracked`] に、HEAD に無いファイルは HEAD を解決できない
    /// ことがあるので [`GitOp::DiscardAdded`] に分ける。どれを出すかは区画と
    /// 変更種別を知っている呼び出し側が決める（`to_command` は純粋関数で
    /// ファイルシステムも index も見られない）。
    Discard(Vec<PathBuf>),
    /// untracked なファイルを削除する（`git clean`）。
    DiscardUntracked(Vec<PathBuf>),
    /// **HEAD に無い** staged なファイル（[`FileStatus::Added`]）を捨てる。
    ///
    /// [`GitOp::Discard`] は `restore --source=HEAD` なので、コミットが 1 つも無い
    /// リポジトリでは HEAD を解決できずに落ちる。`git rm --force` は index からも
    /// ワークツリーからも消すので、HEAD に無いファイルに対しては `Discard` と同じ結果に
    /// なり、しかも HEAD の有無に依らない（03）。
    ///
    /// **HEAD にあるファイルへ掛けてはいけない。** 変更ではなくファイルそのものが消える。
    /// 「HEAD に無いか」は `FileStatus::Added` で判る。`RepoInfo.head` は起動時に読んだ
    /// きりで古くなりうるので、そちらでは判断しない。
    ///
    /// [`FileStatus::Added`]: crate::git_read::FileStatus::Added
    DiscardAdded(Vec<PathBuf>),
    Commit {
        message: String,
        amend: bool,
    },
    BranchCreate {
        name: String,
        start: Option<String>,
        /// 作ったブランチに切り替えるか（`switch --create` か `branch`）。
        ///
        /// **UI から出るのは常に `true`。** `false` を組む経路は無く（01 の
        /// `InputIntent` は真偽値を持たない）、`branch <name>` の側は
        /// [`to_command`](argv::to_command) のテストでだけ踏む。
        switch: bool,
    },
    /// ブランチに切り替える（`switch <name>`）。
    ///
    /// **受けるのはブランチ名だけ。** commit を detached で取り出すには
    /// `switch --detach` が要るので、そのときは別の変種を足す（M5 の cherry-pick /
    /// revert で「このコミットを取り出す」が要るならそこで決める）。ここに Oid を
    /// 渡すと `fatal: invalid reference` で落ちる（git 2.55 で確認）。
    Switch(String),
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
        /// push するローカルの ref 名。refspec の左辺。
        src: String,
        /// リモート側の ref 名。**ローカル名と違うときだけ `Some`**（refspec は
        /// `<src>:<dst>` になる）。`refs/heads/` の下に無い追跡先はここに完全な ref 名が
        /// 入る（01 の「push の行き先」）。
        ///
        /// **`src` と 1 本の文字列に畳まない。** refspec は右辺、`--force-with-lease` は
        /// 先頭に同じ名前を要求するので、畳むと lease が `<src>:<dst>:<oid>` に化ける。
        dst: Option<String>,
        set_upstream: bool,
        /// `Some` なら force push（`--force-with-lease=<dst|src>:<lease>`）。値は
        /// 「リモートはまだここにいるはず」と主張する Oid で、UI が確認モーダルで
        /// 見せた位置をそのまま渡す。
        ///
        /// **旗と値を割らない。** `force: bool` と別々に持つと `force: true` で値が
        /// 無い形が組めてしまい、それは値の無い `--force-with-lease` —— remote-tracking
        /// ref と比べるだけなので、`git fetch` を挟むと他人が進めた位置がそこに入り、
        /// 検査を素通りして他人のコミットを消す。1 つにすればその形は型として作れない。
        lease: Option<Oid>,
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
    /// このリポジトリで使う GitHub アカウントを `.git/config` に憶える
    /// （`Modal::AccountPicker` の `Enter`。01 の「アカウントの選び直し」）。
    ///
    /// **書き込みなので git2 ではなく CLI**（不変ルール 1）。**キーの名前は
    /// [`auth::MEWGIT_USER`] の 1 本**で、読む側（`auth::resolve` の git2）と
    /// 別々に書かない。
    ///
    /// [`auth::MEWGIT_USER`]: crate::auth::MEWGIT_USER
    SetAccount {
        login: String,
    },
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
/// **「git が走って非 0 で終わった」と「終了状態を得られなかった」を型で分ける。**
/// 1 つの struct にまとめると、後者では `code` が常に `None`、`stderr` に git では
/// なく mewgit の文言が入る、`kind` に分類表へ載らない値が入る、という約束を
/// 読む側が毎回思い出すことになる。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecError {
    /// `git` の終了状態を得られなかった。argv を組めなかった、[`Runner`] が実行を
    /// 拒否した、起動できなかった、終了を待てなかった、のいずれか。
    #[error("git を実行できませんでした: {reason}")]
    NoResult {
        /// 実行しようとした argv。`git` 自身は含まない。組めなかったときだけ空。
        argv: Vec<String>,
        /// mewgit 側の説明文。git の stderr ではない。
        reason: String,
    },
    /// ユーザーが中断した（01 の「実行中ジョブのキャンセル」）。
    ///
    /// **`Failed` と分けるのは、これが失敗ではないから。** git の exit code と
    /// stderr は signal で落としたことの言い換えにしかならず、[`ExecErrorKind`] の
    /// 分類表にも載らない。読む側は「押した本人が止めた」ものとして扱う
    /// （通知は `Error` ではなく `Info`）。
    #[error("git の実行を中断しました")]
    Canceled {
        /// 中断した argv。`git` 自身は含まない。
        argv: Vec<String>,
    },
    /// `git` が非 0 で終わった。
    #[error("git の実行に失敗しました: git {}", .argv.join(" "))]
    Failed {
        /// 実行した argv。`git` 自身は含まない。
        argv: Vec<String>,
        code: Option<i32>,
        stderr: String,
        kind: ExecErrorKind,
    },
}

impl ExecError {
    /// 実行した（しようとした）argv。UI はどちらの失敗でもこれを見せる。
    pub fn argv(&self) -> &[String] {
        match self {
            ExecError::NoResult { argv, .. }
            | ExecError::Canceled { argv }
            | ExecError::Failed { argv, .. } => argv,
        }
    }

    /// 実行した（しようとした）コマンドの表示。`git` を頭に付けた 1 行。
    ///
    /// 見せ方をここ 1 つにする。呼び出し側で `argv.join(" ")` を書くと、`git` を
    /// 付ける／付けないが箇所ごとにずれる。
    pub fn command_line(&self) -> String {
        format!("git {}", self.argv().join(" "))
    }
}

/// git の出力のパターンから分類したエラー種別。UI の案内文を切り替えるのに使う。
///
/// 出力があるとき、つまり [`ExecError::Failed`] にしか付かない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecErrorKind {
    SamlSsoRequired,
    SshAuthFailed,
    HttpsAuthFailed,
    Conflict,
    NonFastForward,
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_of_a_failed_run_shows_the_argv() {
        let err = ExecError::Failed {
            argv: vec!["add".to_string(), "--".to_string(), "a.txt".to_string()],
            code: Some(128),
            stderr: "fatal: pathspec".to_string(),
            kind: ExecErrorKind::Other,
        };
        assert_eq!(
            err.to_string(),
            "git の実行に失敗しました: git add -- a.txt"
        );
        assert_eq!(err.argv(), ["add", "--", "a.txt"]);
    }

    #[test]
    fn display_of_a_run_without_a_result_shows_the_reason() {
        // 起動していないので argv を並べても意味が無い。理由を出す。
        let err = ExecError::NoResult {
            argv: Vec::new(),
            reason: "パスが UTF-8 ではありません".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "git を実行できませんでした: パスが UTF-8 ではありません"
        );
        assert!(err.argv().is_empty());
    }
}
