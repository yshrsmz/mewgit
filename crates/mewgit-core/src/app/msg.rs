//! `update` に入力されるイベント。設計は `docs/design/01-app-core.md`。

use crate::auth::{Account, AuthError, DegradedReason, GhLogin, ResolvedTarget};
use crate::git_exec::{ExecError, ExecOutput, GitOp};
use crate::git_read::{
    Branch, CommitDetail, DiffTarget, FileDiff, FileEntry, GitReadError, HeadInfo, InProgressOp,
    LogPage, Oid, RefIndex, Remote, WorktreeSnapshot,
};
use crate::github::{GithubError, PrDetail, PullRequest, RateLimit};

use super::{Key, TermSize};

/// 状態を動かすイベント。端末入力と非同期タスクの完了の両方が入る。
///
/// `Clone` は要求しない。[`Account`](crate::auth::Account) がトークン
/// （`SecretString`）を持つため。
///
/// 大きい payload は `Box` に入れる。入れないと `Msg` が 336 バイトになり、1 打鍵ごとに
/// 作られる [`Msg::Key`]（実データ 12 バイト）まで同じ大きさで動く。
#[derive(Debug)]
pub enum Msg {
    Key(Key),
    /// 端末のサイズが変わった。
    Resize {
        term: TermSize,
    },
    /// グラフペインに使える行数が変わった。
    ///
    /// 行数は `ui` がレイアウトから出す。core がヘッダやステータスバーの高さを
    /// 知っていると、07 のレイアウトを変えたときに黙ってずれる（01 の「グラフの行」）。
    ///
    /// [`Msg::Resize`] と分けているのは、行数が変わる原因が端末サイズだけでは
    /// ないため。詳細の展開（`Enter`）でもグラフは縮む。1 つにまとめると、
    /// `Enter` を押しただけなのに「端末サイズが変わった」という嘘の `Msg` になる。
    Layout {
        graph_rows: usize,
    },
    /// 端末からの貼り付け（bracketed paste）。
    ///
    /// **これが無いと、貼り付けた文字は 1 文字ずつ [`Msg::Key`] として届く。**
    /// コミットメッセージを貼ると最初の改行で確定してしまい、残りがペインのキーとして
    /// 解釈される（`x` で discard の確認が開き、あとから来た `y` がそれを実行する）。
    /// 端末に bracketed paste を要求するのは `main.rs`。
    Paste(String),
    /// 250ms 周期。スピナー更新と外部変更検知に使う。
    Tick,

    // git_read
    /// ログの 1 ページ。`Result` にしているのは、失敗しても必ずここに来るようにするため。
    /// 失敗の通り道が無いと `JobKind::LoadLog` が走ったまま残り、以後の `Cmd::LoadLog` が
    /// 黙って捨てられる。
    ///
    /// `restart` は**要求した内容**。届いたページが先頭からのものか続きかは
    /// [`LogPage`] からは判らない（位置を持たない）。`CommitDetailLoaded` の `oid` や
    /// `DiffLoaded` の `target` と同じで、要求した内容を返信に載せる（01 の
    /// 「ログの読み直し」）。
    LogLoaded {
        restart: bool,
        result: Result<LogPage, GitReadError>,
    },
    /// 選択コミットの詳細。`LogLoaded` と同じ理由で `Result` を運ぶ。失敗の通り道が
    /// 無いと `JobKind::LoadCommitDetail` が走ったまま残り、以後の要求が黙って落ちる。
    ///
    /// `oid` は**要求したコミット**。`Err` のときにどれが失敗したのかを知る手掛かりは
    /// これしかない。無いと、別のコミットの失敗といま見ているコミットの区別が付かず、
    /// 一度も試していないコミットを「読み込めませんでした」のまま放置してしまう。
    CommitDetailLoaded {
        oid: Oid,
        result: Box<Result<CommitDetail, GitReadError>>,
    },
    /// 選択コミットが変更したファイルの一覧。`CommitDetailLoaded` と同じ形
    /// （`Result` + 要求した Oid）で、理由も同じ。**別の `Msg` なのは `Cmd` を
    /// 分けているから**で、1 通にまとめると本文だけ見えていれば足りる時点でも
    /// 一覧を待つことになる（01 の「コミットのファイル一覧」）。
    CommitFilesLoaded {
        oid: Oid,
        result: Box<Result<Vec<FileEntry>, GitReadError>>,
    },
    /// amend の初期値にする HEAD のメッセージ全文。`Ok(None)` はコミットが 1 つも
    /// 無い（unborn HEAD）。「読めなかった」と「amend する相手がいない」で案内が
    /// 違うので、`Err` に畳まない。
    HeadMessageLoaded(Result<Option<String>, GitReadError>),
    /// リポジトリの概要。起動時と、ref が変わったあと（M3）に読む。
    ///
    /// 3 つを 1 通にまとめるのは、どれもリポジトリの ref から導かれる値で、
    /// fetch の後には揃って古くなるため。`Box` は `Msg` を小さく保つため。
    RepoInfoLoaded(Box<Result<RepoSnapshot, GitReadError>>),
    /// ワーキングツリーの状態。`LogLoaded` と同じ理由で `Result` を運ぶ。失敗の
    /// 通り道が無いと `JobKind::LoadStatus` が走ったまま残り、以後の
    /// `Cmd::LoadStatus` が黙って落ちる。書き込みのたびに `ExecFinished` が
    /// `LoadStatus` を出すので、詰まると「stage したのに一覧が変わらない」になる。
    StatusLoaded(Box<Result<WorktreeSnapshot, GitReadError>>),
    /// ローカルブランチの一覧。`StatusLoaded` と同じ理由で `Result` を運ぶ。失敗の
    /// 通り道が無いと `JobKind::LoadBranches` が走ったまま残り、以後の
    /// `Cmd::LoadBranches` が黙って落ちる。
    ///
    /// **要求した内容は運ばない。** 一覧はリポジトリ全体で 1 つなので、
    /// `CommitDetailLoaded` の `oid` に当たる「どれを頼んだか」が無い。
    BranchesLoaded(Box<Result<Vec<Branch>, GitReadError>>),
    /// 対象の diff。`LogLoaded` と同じ理由で `Result` を運ぶ。失敗の通り道が無いと
    /// `JobKind::LoadDiff` が走ったまま残り、以後の `Cmd::LoadDiff` が黙って落ちる。
    ///
    /// `target` は**要求した対象**。`Err` のときにどれが失敗したのかを知る手掛かりは
    /// これしかない（`CommitDetailLoaded` の `oid` と同じ）。`Ok` でも、届いた diff が
    /// いま選んでいるファイルのものかの判定に要る。**`FileDiff.path` からは導けない。**
    /// Staged と Unstaged で同じファイルを選んだときはパスが同じで `DiffTarget` の
    /// 変種だけが違う。
    DiffLoaded {
        target: DiffTarget,
        /// ファイル単位。平坦化すると `GitOp::StageHunk` に渡す `path` を失う。
        result: Box<Result<Vec<FileDiff>, GitReadError>>,
    },

    // git_exec
    /// `git` の実行が終わった。`Runner` が実行直後に再検出した進行中操作を一緒に運ぶ（03）。
    ExecFinished(Box<ExecDone>),
    /// `.git/HEAD` の監視など、`GitOp` 以外の経路で進行中操作が変わった（M6）。
    ProgressDetected(Option<InProgressOp>),

    // auth / github
    /// [`Cmd::ResolveTarget`](super::Cmd::ResolveTarget) の結果（01 の
    /// 「`RepoInfo.target` を埋める」）。
    ///
    /// **`Result` にしない。** `ssh -G` が失敗しても `host_raw` を使って `target` は
    /// 埋まる（04 の「解決失敗なら `host_raw` をそのまま使い、通知に出す」）。
    /// `Result` にすると呼び出し側が `Err` で `target` を捨て、**degraded ではなく
    /// 正規化だけを諦める**という 04 の経路が消える。
    /// **`Box` に載せる。** `Msg` のサイズは最大変種で決まり、1 打鍵ごとに作られる
    /// `Msg::Key` まで大きくなる（`msg_stays_small`）。
    TargetResolved(Box<ResolvedTarget>),
    /// `auth::resolve` の結果。`Result<Account, AuthError>` にしないのは、`AuthError` と
    /// `DegradedReason` が同じ失敗を二重に表現してしまうため（04）。`AuthError` は
    /// `gh` / `ssh` 実行の内部エラーに留め、`resolve` の中で `DegradedReason` に畳む。
    ///
    /// **[`AccountState`](crate::auth::AccountState) では運ばない。** あの型には
    /// 「まだ頼んでいない」（`Resolving`）があり、`resolve` は返さない —— 運ぶ型に
    /// すると、その約束を見張るのは doc の 1 文だけになる（01 の `AccountResolved`）。
    /// [`update`](super::update) が `Ok` を `Resolved`、`Err` を `Degraded` に写す。
    AccountResolved(Result<Account, DegradedReason>),
    /// 選び直しの候補（01 の「候補の取り直し」）。`auth::Backend::logins` の戻り値
    /// そのまま。
    ///
    /// **`Err` を [`DegradedReason`] に畳まない。** 畳むと `auth::resolve` の外に
    /// [`AccountState`](crate::auth::AccountState) の産出元ができ、04 が決めた境目が
    /// 消える。受けるのは [`Modal::AccountPicker`](crate::app::Modal::AccountPicker)
    /// だけで、`state.account` は動かさない。
    AccountsListed(Result<Vec<GhLogin>, AuthError>),
    /// 開いている PR の一覧（01 の「PR ペイン › 何を引くか」）。
    ///
    /// **鍵を運ばない。** 一覧は選択に紐づかないので突き合わせる相手が無く、
    /// 追いつきは世代が見る（01 の「実行中ジョブ › 世代」）。
    PrListLoaded(Result<Vec<PullRequest>, GithubError>),
    /// 扇形の答えが 1 件（01 の「PR ペイン › 集計」）。
    ///
    /// **1 ジョブから何度も届く。** 60 件を同時 4 本で引くと全部揃うまで数秒かかる
    /// ので、揃うのを待たずに届いた行から出す。ジョブを畳むのは
    /// [`Msg::PrDetailsFinished`] のほう。
    ///
    /// **鍵は `(number, sha)`。** 飛んでいるあいだに push で head が進んだ PR の
    /// 答えは前のコミットのものなので、受け取り側が捨てる。
    PrDetailLoaded {
        /// 要求に刻んだ世代（`AppState::prs` の `epoch`）。アカウントが切り替わって
        /// いれば捨てる —— `(number, sha)` は「どの行に当てるか」しか答えない。
        epoch: u64,
        number: u64,
        sha: String,
        result: Result<PrDetail, GithubError>,
    },
    /// 扇形が終わった。**ジョブを畳むのはここ。**
    ///
    /// `aborted` が `Some` なら**打ち切った**という意味で、理由と**頼んだ対象**を
    /// 運ぶ。答えの返らなかった対象はその理由で埋まる（01 の「PR ペイン › 集計」）。
    /// **対象を運ぶのは、いまの一覧を舐めると頼んでいない行まで塞ぐから** ——
    /// 扇形が走っているあいだに増えた行がそれ。
    PrDetailsFinished {
        epoch: u64,
        aborted: Option<(GithubError, Vec<(u64, String)>)>,
    },
    /// 応答が言ったレートリミット残量（01 の「PR ペイン › 残量」）。
    ///
    /// **別の `Msg` にする。** 残量は答えそのものではなく、**同じ応答の副産物** ——
    /// [`Msg::PrListLoaded`] や [`Msg::PrDetailLoaded`] に載せると、答えが無い回
    /// （組み立てに失敗した扇形）から残量だけを出す形が作れない。
    ///
    /// **ジョブは持たない。** 走っているのは一覧か扇形のジョブで、これはその途中に
    /// 何度でも流れる。畳むのはあちらの終端。
    ///
    /// **世代を運ぶ。** 残量は**そのトークンの予算**なので、アカウントを切り替えた
    /// あとに前のトークンの扇形が返す数は嘘になる（`(reset, used)` は「どちらが
    /// 新しいか」しか答えない。#89 の「鍵が答えられる問いを数える」）。
    RateLimitSeen {
        epoch: u64,
        seen: RateLimit,
    },
    /// `N` で頼んだ PR 作成の結果 —— 作った PR の**番号**（01 の「PR 作成（`N`）」）。
    ///
    /// **`()` に落とさない。** 番号が要る —— 通知に `#<番号>` を出し、届いた一覧で
    /// その行にカーソルを座らせる（`PrState::created`）。
    ///
    /// **[`PullRequest`] を丸ごと運ばない。** 残りのフィールドを読む場所が無く、
    /// 載せると `String` 5 本と `Vec` のぶんで `Msg` の上限（下の `msg_stays_small`）に
    /// 当たって `Box` が要る。**`GithubClient::create_pr` は `PullRequest` を返す
    /// まま**（05）で、番号に落とすのはランタイムの腕。
    PrCreated(Result<u64, GithubError>),
    /// `gh browse` の結果。`Err` は通知に出す（01 の「`o`（ブラウザで開く）」）。
    ///
    /// **`Ok` でも何も出さない。** ブラウザが前に出てくるので、画面に足す言葉が無い。
    OpenPrFinished(Result<(), AuthError>),
}

/// [`Msg::RepoInfoLoaded`] の中身。
#[derive(Debug)]
pub struct RepoSnapshot {
    pub remotes: Vec<Remote>,
    pub head: HeadInfo,
    pub refs: RefIndex,
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
