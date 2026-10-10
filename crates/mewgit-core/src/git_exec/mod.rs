//! `git_exec` モジュール。設計は `docs/design/03-git-exec.md` を参照。
//!
//! 書き込みはすべて `git` サブプロセスで行う（不変ルール 1）。ssh 設定・credential
//! helper・hook・`GIT_*` 環境変数を自動で効かせるため。

mod argv;
mod patch;
mod rebase;
mod runner;
mod version;

use std::path::PathBuf;

use crate::git_read::{Oid, PathStamp, RefusedReword};

pub use argv::{push_remote_ref, to_commands, ArgvError, GitCommand};
pub use patch::{hunk_patch, supports_hunk_patch};
pub use rebase::{
    write_commit_message, write_todo, EditorError, RebaseAction, RebaseEntry, RebasePlan,
    RebasePlanError, EDITOR_STDERR_PREFIX,
};
pub use runner::Runner;
pub use version::{require_minimum, VersionError};

/// 書き込み操作。[`to_commands`] で `git` の argv と標準入力に変換する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitOp {
    Stage(Vec<PathBuf>),
    Unstage(Vec<PathBuf>),
    /// ハンク単位の stage（`git apply --cached`）。
    StageHunk(Box<HunkApply>),
    /// ハンク単位の unstage（`git apply --cached --reverse`）。**守りは index 側と HEAD 側を
    /// 比べる**（[`GitOp::stale_at`]）。
    UnstageHunk(Box<HunkApply>),
    /// ハンク単位の discard。**ワークツリーだけを書き換える**（`--cached` は付かない）。
    ///
    /// index とワークツリーの両方に当てる変種は作らない。`git apply --index` は
    /// 2 つが一致していることを要求するので、staged な変更があるファイルでは
    /// `does not match index` で落ちる。2 回に割ると片方だけ通った中途半端な状態が
    /// 残るので、staged なハンクの discard は UI が「先に unstage してください」と
    /// 断る（03）。
    DiscardHunk(Box<HunkApply>),
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
    /// 「HEAD に無いか」は `FileStatus::Added` で判る。ブランチ一覧の HEAD は読み直しが
    /// 返るまで古くなりうるので、そちらでは判断しない。
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
        /// [`to_commands`] のテストでだけ踏む。
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
        /// plan を組んだ時点の HEAD。sequence editor が git の `orig-head` と比べ、
        /// 違えば rebase を始めさせない（03 の「Interactive rebase の実装」）。
        head: Oid,
        plan: RebasePlan,
    },
    /// `reword` が `Some` なのは、mewgit が渡した reword を hook が断って止まった形
    /// （`InProgressOp::Rebase::refused_reword` を写したもの）。HEAD を照合し、新しい
    /// メッセージを amend してから continue する 3 本になる（03 の「Interactive rebase の
    /// 実装」）。`None` は素の `rebase --continue`。
    RebaseContinue {
        reword: Option<RefusedReword>,
    },
    RebaseSkip,
    RebaseAbort,
    CherryPick(Vec<Oid>),
    CherryPickContinue,
    /// 止まった pick を捨てて次へ進む（`cherry-pick --skip`）。**変更が空で止まった
    /// pick から出る手**で、continue は同じ理由で断られる（03 の「continue / skip /
    /// abort の終わり方」）。
    CherryPickSkip,
    CherryPickAbort,
    Revert {
        oids: Vec<Oid>,
        mainline: Option<u32>,
    },
    RevertContinue,
    /// 止まった revert を捨てて次へ進む（`revert --skip`）。**解消した結果が空で continue が
    /// 断られた形から出る手**（03 の「revert の開始」）。
    RevertSkip,
    RevertAbort,
    /// merge を始める前に戻す（`merge --abort`）。**continue に当たる変種は無い** ——
    /// merge を結ぶのは `Commit`（03 の「進行中に通す書き込み」）。
    MergeAbort,
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
    /// **CLI を通す理由に不変ルール 1 を引かない**（03）。あのルールの理由は hook が
    /// 走らないことで、`git config` は hook を 1 つも走らせない。ここで CLI なのは
    /// 「読みは git2、書きは CLI」の分担と、失敗の argv と git の出力が [`ExecError`] で
    /// UI まで届く経路（不変ルール 4）に乗せるため。**キーの名前は
    /// [`auth::MEWGIT_USER`] の 1 本**で、読む側（`auth::resolve` の git2）と
    /// 別々に書かない。
    ///
    /// [`auth::MEWGIT_USER`]: crate::auth::MEWGIT_USER
    SetAccount {
        login: String,
    },
    /// 憶えたアカウントを `.git/config` から外す（`Modal::AccountPicker` の `u`。
    /// 01 の「アカウントの選び直し」）。
    ///
    /// **外すのは `.git/config` の [`auth::MEWGIT_USER`] だけ。** 設定ファイルの
    /// `[[repos]]` は `git config` の手が届かないので、`u` 自体が
    /// [`AccountSource::is_unsettable`] のときしか出ない。
    ///
    /// **設定されていないキーに当てると exit 5 を stderr 空で返す**（03 の実測表）。
    /// 成否から「いま何が設定されているか」は読めないので、書き込みのあとは結果に
    /// 関わらず解決をやり直す（01）。
    ///
    /// [`auth::MEWGIT_USER`]: crate::auth::MEWGIT_USER
    /// [`AccountSource::is_unsettable`]: crate::auth::AccountSource::is_unsettable
    UnsetAccount,
    /// PR をローカルのブランチに取り出す（PRs ペインの `Enter`。05 の
    /// 「PR チェックアウト」）。
    ///
    /// **git を続けて叩く唯一の変種**（03 の「複数の手順を持つ書き込み」）。
    /// どの手順を組むかは [`PrCheckout`] の値で決まり、その値を決めるのは `app`
    /// （ブランチ一覧と `target` を知っている側）。
    ///
    /// **`Box` に載せる。** 中身は文字列を 4 本持つので、裸で置くと `GitOp` が
    /// 膨らみ、`Cmd` と `Msg` が 1 打鍵ごとにその大きさで作られる。
    CheckoutPr(Box<PrCheckout>),
}

/// [`GitOp::CheckoutPr`] の中身。**場面（どこから取るか × ローカルにどう在るか）を
/// 値で持ち、手順はそこから [`to_commands`] が導く**（05 の「mewgit の手順」の表）。
///
/// 手順の列そのもの（`Vec<GitOp>`）を持たないのは、**組み合わせてはいけない列が
/// 型で組める**から —— たとえば「いま居るブランチへ `<src>:<b>` で fetch する」は
/// git が exit 128 で断る形（05）で、列を渡す形だと呼び出し側がそれを避ける責任を持つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrCheckout {
    /// fetch 元のリモート名（`RepoInfo.target` を決めたリモート。05）。
    pub remote: String,
    /// 取り出す先のローカルのブランチ名（05 の「ブランチ名」）。
    pub branch: String,
    pub source: PrSource,
    pub local: LocalBranch,
}

/// PR の head がどこに在るか。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrSource {
    /// base と同じリポジトリのブランチ。**名前は [`PrCheckout::branch`] と同じ**
    /// （`--track` が `<remote>/<branch>` を追うので、違う名前にすると追跡先と
    /// 食い違う。05）。
    SameRepo,
    /// fork（消えた fork を含む）。中身は base 側の `refs/pull/<number>/head` から取る。
    Fork {
        number: u64,
        /// **新しく作るときだけ書く**追跡設定（[`LocalBranch::Absent`]）。
        upstream: ForkUpstream,
    },
}

/// fork から取り出したブランチに書く追跡設定（05 の「追跡先」）。
///
/// **`remote` と `merge` を 1 つにまとめる。** どちらか片方だけを書くと、git は
/// 追跡設定として読まない（`branch.<b>.merge` が無ければ上流が無い）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkUpstream {
    /// `branch.<b>.remote` と `branch.<b>.pushRemote` に書く値。fork の URL か、
    /// 組めないときは fetch 元のリモート名。
    pub remote: String,
    /// `branch.<b>.merge` に書く値。`refs/heads/<head>` か `refs/pull/<n>/head`。
    pub merge: String,
}

/// 取り出す先のブランチがローカルに在るか（05）。
///
/// **「いま居る」を持たない。** 一覧の HEAD の行は外で `git switch` されると古く、
/// それを信じて切替を省くと別のブランチを早送りする。在るなら**いつも切り替えてから**
/// 早送りする（いま居れば `switch` は何もしない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalBranch {
    /// 無い。作る（在れば git が断る形で）。
    Absent,
    /// 在る。切り替えて `merge --ff-only` で早送りする。
    Present,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullMode {
    Merge,
    Rebase,
    FastForwardOnly,
}

/// 成功した `git` 実行の結果。`argv` は UI に見せるために必ず保持する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub argv: Vec<String>,
    pub stdout: String,
    pub stderr: String,
}

/// 失敗した `git` 実行。argv と git の出力を握りつぶさない（不変ルール 4）。
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
    /// ハンクを読んだあとに、対象のファイルか index が変わっていた
    /// （[`GitOp::stale_at`]）。**`git` は起動していない。**
    ///
    /// 古いハンクを当てると、`s` は HEAD にもワークツリーにも無い内容を index に
    /// 入れる（03 の「古いハンクの守り」）。`Failed` と分けるのは、何も壊れておらず、
    /// 読み直した差分でもう一度押せば済むから（通知は `Warn`）。
    ///
    /// **要約行にパスを入れない。** 長さに上限が無く、通知の予算（01 の「失敗の案内文」）を
    /// 食って案内の側を押し出す。パスは通知の詳細に出す（`app` の `exec_detail`）。
    #[error("外で変更されていました。読み直した差分で押し直してください")]
    Stale {
        /// 起こすはずだった argv。`git` 自身は含まない。
        argv: Vec<String>,
        path: PathBuf,
    },
    /// `git` が非 0 で終わった。
    #[error("git の実行に失敗しました: git {}", .argv.join(" "))]
    Failed {
        /// 実行した argv。`git` 自身は含まない。
        argv: Vec<String>,
        code: Option<i32>,
        /// **理由が stdout にだけ出る失敗がある**（何も stage していない `commit`、
        /// 未解消の `rebase --continue`。03 の「エラー分類」）ので、分類のあとも捨てない。
        stdout: String,
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
            | ExecError::Stale { argv, .. }
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

    /// git の出力から選んだ失敗の理由の 1 行（`error:` / `fatal:` の接頭辞は剥ぐ）。
    /// `Failed` 以外と、選べる行が無いときは `None`。選び方は 03 の「エラー分類」の
    /// 「理由の行」。
    pub fn reason(&self) -> Option<&str> {
        match self {
            ExecError::Failed { stdout, stderr, .. } => runner::reason(stdout, stderr),
            ExecError::NoResult { .. } | ExecError::Canceled { .. } | ExecError::Stale { .. } => {
                None
            }
        }
    }
}

/// ハンク操作 1 つぶんの中身（[`GitOp::StageHunk`] / [`GitOp::UnstageHunk`] /
/// [`GitOp::DiscardHunk`]）。
///
/// **3 変種で同じものを持つので 1 つにまとめ、丸ごと箱に入れる。** 裸で並べると
/// `GitOp` が 96 から 104 バイトに伸び、`Cmd` の大きさの規則（01 の「`Cmd`」。
/// `cmd_stays_small`）が数え直しになる（実測 2026-10-03）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkApply {
    /// argv に載らない（パッチのファイルヘッダが対象を持つ）。UI が「どのファイルか」を
    /// 通知に出すのと、守りが指紋を採り直すのに使う。
    pub path: PathBuf,
    /// `git apply` に渡すパッチそのもの。**`String` ではなく `Vec<u8>`。** lossy 変換すると、
    /// 非 UTF-8 のファイルをステージした結果が元のファイルと変わる（02 の「ハンクの持ち方」）。
    pub patch: Vec<u8>,
    /// パッチを読んだ時点の `path` の指紋（`FileDiff::stamp`）。[`Runner`] が `git apply` を
    /// 起こす直前に採り直し、違えば起こさずに [`ExecError::Stale`] で断る（03 の
    /// 「古いハンクの守り」）。argv には載らない。
    pub expect: PathStamp,
}

impl GitOp {
    /// ハンク操作の中身。ハンク操作でなければ `None`。
    ///
    /// 網羅 `match` にしているのは、パッチを当てる変種を足したときに、古いハンクの守り
    /// （03）を通すかどうかを選ばせるため。
    pub fn hunk(&self) -> Option<&HunkApply> {
        match self {
            GitOp::StageHunk(hunk) | GitOp::UnstageHunk(hunk) | GitOp::DiscardHunk(hunk) => {
                Some(hunk)
            }
            GitOp::Stage(_)
            | GitOp::Unstage(_)
            | GitOp::Discard(_)
            | GitOp::DiscardUntracked(_)
            | GitOp::DiscardAdded(_)
            | GitOp::Commit { .. }
            | GitOp::BranchCreate { .. }
            | GitOp::Switch(_)
            | GitOp::BranchDelete { .. }
            | GitOp::BranchRename { .. }
            | GitOp::Fetch { .. }
            | GitOp::Pull { .. }
            | GitOp::Push { .. }
            | GitOp::RebaseInteractive { .. }
            | GitOp::RebaseContinue { .. }
            | GitOp::RebaseSkip
            | GitOp::RebaseAbort
            | GitOp::CherryPick(_)
            | GitOp::CherryPickContinue
            | GitOp::CherryPickSkip
            | GitOp::CherryPickAbort
            | GitOp::Revert { .. }
            | GitOp::RevertContinue
            | GitOp::RevertSkip
            | GitOp::RevertAbort
            | GitOp::MergeAbort
            | GitOp::StashPush { .. }
            | GitOp::StashApply(_)
            | GitOp::StashPop(_)
            | GitOp::StashDrop(_)
            | GitOp::SetAccount { .. }
            | GitOp::UnsetAccount
            | GitOp::CheckoutPr(_) => None,
        }
    }

    /// `now`（いまの指紋）が、この操作が前提にした指紋から外れているか。前提の無い
    /// 操作では偽。
    ///
    /// **比べるのは、パッチの基準と当てる先だけ**（03 の「古いハンクの守り」）。
    /// - stage / discard: パッチは index → ワークツリー（`IndexVsWorktree`）。ワークツリーと
    ///   index。HEAD は見ない —— 外で commit しても、このパッチは変わらない
    /// - unstage: パッチは HEAD → index（`HeadVsIndex`）で、当てる先は index。index と HEAD。
    ///   ワークツリーは見ない —— エディタで編集を続けながら staged なハンクを外す形を、
    ///   毎回断らないため。**HEAD を見ないと、外で commit したあとの古いハンクが exit 0 で
    ///   通る**（`/code-review` が拾った）
    pub fn stale_at(&self, now: &PathStamp) -> bool {
        let Some(hunk) = self.hunk() else {
            return false;
        };
        let (expect, now) = (&hunk.expect, now);
        let worktree_side = (expect.worktree, expect.index) != (now.worktree, now.index);
        let head_side = (expect.index, expect.head) != (now.index, now.head);
        match self {
            GitOp::UnstageHunk(_) => head_side,
            _ => worktree_side,
        }
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
    /// cherry-pick の結果が空で止まった（`The previous cherry-pick is now empty`）。
    /// **進行中操作は残り、continue は同じ理由で断られる** —— 出る手は skip か abort
    /// （03 の「continue / skip / abort の終わり方」）。
    Empty,
    /// 積む変更が無かった（stdout の `nothing to commit` の類。03 の「エラー分類」）。
    /// **文言で分けた種別で、進行中操作が残ったかは言わない** —— 1 本だけの revert では
    /// 何も残らないが、複数の revert の途中では `sequencer/` が残る（03 の「revert の
    /// 開始」）。どちらかを決めるのは op を見る `app` の側。
    NoChanges,
    NonFastForward,
    /// plan を組んだあとに HEAD が動き、sequence editor が断った（`EditorError::HeadMoved`）。
    /// git は rebase を始めず、進行中操作は残らない（03 の「Interactive rebase の実装」）。
    HeadMoved,
    /// 未解消のファイルが残っている（stdout の `<path>: needs merge` か、stderr の
    /// `Committing is not possible because you have unmerged files`）。何も書き込まれて
    /// いない（03 の「エラー分類」）。
    Unmerged,
    /// git がエディタ（自バイナリ）を起こせず、そのパスがもう無い —— 起動中に mewgit の
    /// バイナリが消えた。**出力からは決めない**（shell で文言が割れる）。[`Runner`] が
    /// 失敗のあとにパスを見て決める（03 の「エラー分類」）。
    EditorGone,
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
            stdout: String::new(),
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
