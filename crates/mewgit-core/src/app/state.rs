//! アプリ全体の状態。設計は `docs/design/01-app-core.md`。
//!
//! `ui` はここにある値を描画するだけで、更新は [`update`](crate::app::update) が行う。

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use crate::auth::url::RemoteUrl;
use crate::auth::{AccountSource, AccountState, DegradedReason, GhLogin, RepoTarget};
use crate::git_exec::RebasePlan;
use crate::git_read::{
    Branch, Commit, CommitDetail, DiffTarget, FileDiff, FileEntry, HeadInfo, InProgressOp, Oid,
    RefIndex, Remote, StashEntry, WorktreeSection, WorktreeSnapshot,
};
use crate::github::{GithubError, PrDetail, PullRequest, RateLimit, RepoRef};
use crate::graph::{GraphRow, LaneState};

use crate::config::Config;

use super::Cmd;

/// アプリ全体の状態。
///
/// `Default` は付けない。リポジトリの無い `AppState` を表現可能にしないため。
#[derive(Debug)]
pub struct AppState {
    pub repo: RepoInfo,
    pub account: AccountState,
    pub graph: GraphState,
    pub worktree: WorktreeState,
    /// 表示中の diff。`worktree` の下ではなく最上位に置く。`Pane::Diff` は独立した
    /// ペインで、グラフで選んだコミットの diff もここに入るため。
    pub diff: DiffState,
    pub branches: BranchState,
    /// 直前の `P` が `NonFastForward` で断られたこと。次の `P` を確認モーダル経由の
    /// force push に上げる（01 の「force push」）。
    pub push_rejected: Option<PushRejection>,
    pub stashes: Vec<StashEntry>,
    pub prs: PrState,
    pub progress: Option<InProgressOp>,
    pub focus: Pane,
    /// 右列の上に出すタブ。
    ///
    /// **`focus` だけで決めない**（01 の「PR ペイン › 右上のタブ」）。左の領域
    /// （Graph / Diff）は `focus` で決まるが、右上で同じにすると**チェックの結果を
    /// 見ながら `j` でグラフを辿った瞬間に消える**。揃えるのは
    /// [`reseat_focus`](AppState::reseat_focus) の 1 箇所。
    pub right_tab: RightTab,
    pub modal: Option<Modal>,
    pub jobs: JobTracker,
    /// エラー・情報のトースト。積むのは [`AppState::push_notice`]。
    pub notices: VecDeque<Notice>,
    pub term: TermSize,
    /// [`Msg::Tick`] が来た回数。スピナーの駒を選ぶのに使う（07 のヘッダ）。
    ///
    /// **時刻ではなく回数を持つ。** `ui` は `AppState` を描くだけなので、駒を時刻から
    /// 決めると描画のたびに時計を読むことになり、`TestBackend` で「回った」ことを
    /// 見張れない。`wrapping_add` で回すので溢れても止まらない。
    ///
    /// [`Msg::Tick`]: crate::app::Msg::Tick
    pub tick: u64,
    /// 起動時に確定した設定。実行中は再読込しない（08）。
    pub config: Config,
}

/// push が non-fast-forward で断られたこと（01 の「force push」）。
///
/// **ブランチ名と、そのときのローカルの先端を両方持つ。** 名前だけで憶えると、
/// `↓n` で断られた直後に `p` で早送りしても force の確認が出る（behind だけの
/// ブランチも `! [rejected] (non-fast-forward)` で断られる。git 2.55 で確認）。
/// pull / commit / rebase はどれも先端を動かすので、`oid` まで見れば黙って失効する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRejection {
    pub branch: String,
    pub oid: Oid,
}

/// 保持する通知の上限。超えた分は古いものから捨てる。
const MAX_NOTICES: usize = 100;

impl AppState {
    /// 開いたリポジトリと設定から初期状態を作る。
    pub fn new(repo: RepoInfo, config: Config) -> Self {
        Self {
            repo,
            // **旗を読む場所はここ 1 つ**（01 の「GitHub 機能を切る」）。以後の読み手は
            // `AccountState` を網羅 `match` するので、腕が増えれば何を出すか選ばされる。
            account: if config.github.enabled {
                AccountState::default()
            } else {
                AccountState::Disabled
            },
            graph: GraphState::default(),
            worktree: WorktreeState::default(),
            diff: DiffState::default(),
            branches: BranchState::default(),
            push_rejected: None,
            stashes: Vec::new(),
            prs: PrState::default(),
            progress: None,
            focus: Pane::default(),
            right_tab: RightTab::default(),
            modal: None,
            jobs: JobTracker::default(),
            notices: VecDeque::new(),
            term: TermSize::default(),
            tick: 0,
            config,
        }
    }

    /// `update` が返す [`Cmd`] を発行してよい形にする。
    ///
    /// 非同期ジョブを生む `Cmd` は [`JobTracker`] に登録し、同種が既に走っていれば
    /// [`Cmd::None`] に落とす。判定をここ（`update` の出口）に置くのは、ランタイムの
    /// `dispatch` が `AppState` に書いてよいのが `notices` だけだから（01 のランタイム）。
    pub fn issue(&mut self, cmd: Cmd) -> Cmd {
        match cmd {
            // `Batch` の中身は 1 つずつ判定する。まとめて弾くと、同種が走っている
            // `Cmd` 1 つのために他の `Cmd` まで落ちる。
            Cmd::Batch(cmds) => {
                let issued: Vec<Cmd> = cmds.into_iter().map(|cmd| self.issue(cmd)).collect();
                Cmd::batch(issued)
            }
            cmd => match cmd.job() {
                None => cmd,
                Some((kind, label)) => {
                    let job = Job::new(kind, label);
                    if self.jobs.start(job) {
                        cmd
                    } else {
                        self.notify_dropped(kind);
                        Cmd::None
                    }
                }
            },
        }
    }

    /// 同種のジョブが走っていて発行できなかった `Cmd` を伝える。
    ///
    /// 読み取りは黙って落とす（同じものをもう一度読むだけで、結果は変わらない）。
    /// 書き込みだけは「押したのに何も起きない」に見えるので伝える。**キューには
    /// 積まないと決めた**（01 の「ランタイム」）。
    ///
    /// **`JobKind` ではなくここに置くのは、案内に「次に押すキー」が入るから。**
    /// それは状態で変わる —— **中断をもう頼んだあとの `Ctrl-C` は終了になる**ので
    /// （01 の「キャンセル」）、`Ctrl-C` で中断と案内し続けると、モーダルに打った
    /// 本文ごとアプリが終わる。`JobKind` は「もう頼んだか」を知らない。
    ///
    /// 網羅 `match` にしているのは、種別を足したときに黙って落とすかどうかを
    /// 選ばせるため。
    pub fn notify_dropped(&mut self, kind: JobKind) {
        let message = match kind {
            JobKind::Exec if self.jobs.cancel_requested(JobKind::Exec) => {
                "中断を頼んだ git を待っています（Ctrl-C で終了）"
            }
            JobKind::Exec => "ほかの git 実行が終わるまで待ってください（Ctrl-C で中断）",
            JobKind::LoadLog
            | JobKind::LoadCommitDetail
            | JobKind::LoadCommitFiles
            | JobKind::LoadHeadMessage
            | JobKind::LoadRepoInfo
            | JobKind::LoadStatus
            | JobKind::LoadBranches
            | JobKind::LoadDiff
            | JobKind::ResolveTarget
            | JobKind::ResolveAccount
            | JobKind::ListAccounts
            | JobKind::FetchPrList
            | JobKind::FetchPrDetails => return,
            // **黙って落とさない。** 落とすと `Modal::PrCreate` が閉じないまま
            // 何も起きず（`update::submit` は `Cmd` が落ちるなら閉じない）、
            // 打った本文を抱えた箱が無反応に見える。
            JobKind::CreatePr => "PR を作っています",
            // **黙って落とさない。** `gh browse` は**起動したブラウザが終わるまで
            // 返らない**（gh 2.100.0 で実測。`BROWSER` に 5 秒眠るスクリプトを
            // 置くと 5.06 秒）。端末ブラウザや自分で抜けないラッパーを `BROWSER` に
            // している環境では、そのあいだ `o` が無反応になる（#63 の「無反応より
            // まし」で止めない）。
            JobKind::OpenPr => "ブラウザの起動を待っています",
        };
        self.push_notice(NoticeLevel::Warn, message.to_string(), None);
    }

    /// 通知を積む。[`MAX_NOTICES`] を超えた分は古いものから捨てる。
    ///
    /// 組み立てをここに寄せているのは、上限を 1 箇所で効かせるため。生産者は
    /// `update` とランタイムの両方にいる。
    pub fn push_notice(&mut self, level: NoticeLevel, message: String, detail: Option<String>) {
        self.notices.push_back(Notice {
            level,
            message,
            detail,
        });
        while self.notices.len() > MAX_NOTICES {
            self.notices.pop_front();
        }
    }

    /// `Tab` / `Shift+Tab` の巡回に入るペイン。
    ///
    /// **並びは定数でも、どの並びを使うかはグラフの選択で変わる**（01 の
    /// 「Pane と Modal」）。右ペインの中身は選択で決まるので、候補を固定にすると
    /// `focus` と画面の中身が食い違う状態（コミットを見ているのにキーは Working tree が
    /// 受ける）を表現できてしまう。
    pub fn focus_candidates(&self) -> &'static [Pane] {
        match self.graph.selection() {
            // **絞るのはここだけ。** コミット側の「対象が無い」はファイル一覧が届くまでの
            // 一時的なもので、届いた時点で頼み直す経路が別にある（01 の「コミットの
            // ファイル一覧」）。**外すと、待っているあいだ候補が増減するうえ、その
            // 頼み直しへ入れなくなる。** WIP 行の側が Worktree ペインと同じ
            // スナップショットを読む（無い状態はペインが既に出している）のに対し、
            // コミット側は 2 つ目の遅延読み込みを待つ、という違い。
            GraphSelection::Commit(_) => self.pick(Pane::COMMIT_FOCUS, Pane::COMMIT_FOCUS_NO_PRS),
            // **差分で表せる行を選んでいないなら `Diff` は候補から外す。** 巡回に
            // 入れるのは「実装済みで、かついま画面に出ているペインだけ」（07）で、
            // `Diff` は左の領域を Graph と差し替えて出す。相手がいないまま入れると、
            // **グラフが空のペインに差し替わる**。
            //
            // **見るのは区画ではなくカーソルの行。** conflicted の行と変更のある行が
            // 並んでいるリポジトリでは、`j` / `k` で候補が増減する。**それでよい** ——
            // 出せるかどうかは行ごとに決まり、Worktree ペインが出している中身と揃う。
            // コミット側で同じことを避けたのは、あちらの増減が**同じ行のまま時間で
            // 起きる**（一覧が届く）ためで、こちらはユーザーが動かした結果。
            //
            // **`wanted_diff()` では判定できない。** あちらは `focus != Pane::Diff` で
            // `None` を返すので、入る前は必ず `None`（入れるかを問う相手にならない）。
            //
            // **末尾で照合する。** 添字で削ると、並びを変えたときに別のペインが落ちる。
            GraphSelection::Wip if self.wanted_worktree_diff().is_none() => {
                let all = self.pick(Pane::WIP_FOCUS, Pane::WIP_FOCUS_NO_PRS);
                all.strip_suffix(&[Pane::Diff]).unwrap_or(all)
            }
            GraphSelection::Wip => self.pick(Pane::WIP_FOCUS, Pane::WIP_FOCUS_NO_PRS),
        }
    }

    /// `Prs` を含む一覧と含まない一覧のどちらを使うか。
    ///
    /// **見るのは [`AccountState::Disabled`] で、`config` を直に読まない。** 旗を
    /// 読む場所は [`AppState::new`] の 1 つで、以後は `account` が答える（01 の
    /// 「GitHub 機能を切る」）。
    fn pick(&self, with_prs: &'static [Pane], without: &'static [Pane]) -> &'static [Pane] {
        if self.account.is_disabled() {
            without
        } else {
            with_prs
        }
    }

    /// 巡回の次。末尾からは先頭に戻る。
    pub fn next_focus(&self) -> Pane {
        self.focus.cycle(self.focus_candidates(), 1)
    }

    /// 巡回の前。先頭からは末尾に戻る。
    pub fn prev_focus(&self) -> Pane {
        self.focus.cycle(self.focus_candidates(), -1)
    }

    /// 候補から外れた `focus` を [`Pane::Graph`] に座り直させる。
    ///
    /// グラフのカーソルが WIP 行を出入りすると候補が入れ替わる。戻す先を `Graph` に
    /// するのは、**座り直しがユーザーの選んでいない移動**だから。どちらの選択でも
    /// 候補に残るペインはほかにもあるが（落ちることがあるのは `Diff` だけ）、
    /// `Branches` と `Prs` は右列の上なので「グラフの行を選び直したら右の一覧に飛ぶ」
    /// という説明の付かない動きになる。いま見ている左の領域に戻すのが説明の付く動き
    /// （07 の「ペインとフォーカス」）。
    ///
    /// **同じ出口で [`right_tab`](AppState::right_tab) も揃える。** `focus` が
    /// `Branches` か `Prs` に居るならタブはそれと同じでなければならず、書く場所を
    /// 2 つに割ると食い違った状態が残る（01 の「右上のタブ」）。
    ///
    /// **フォーカスを動かすキーが通る `update::focus` は経由しない。** ここは
    /// `update` の**出口**なので `Cmd` を返す先が無い。`update::focus` は
    /// 「見えるようになったものを頼む」ためのもので、移す先が `Graph` でも
    /// **PRs タブは残るので `prs::fan_out` は出うる**（`wanted_details` の門は
    /// `focus` ではなく見えているタブを見る）—— 通すと、その `Cmd` を捨てる形に
    /// なる。座り直しに要るのは `focus` の代入だけ。
    pub fn reseat_focus(&mut self) {
        if !self.focus_candidates().contains(&self.focus) {
            self.focus = Pane::Graph;
        }
        self.right_tab = self.visible_right_tab();
    }

    /// いま右列の上に出ているタブ。
    ///
    /// **`focus` が右列に居るならそちらが正で、居ないときだけ憶えている
    /// [`right_tab`](Self::right_tab) を読む。** 同じ式を
    /// [`reseat_focus`](Self::reseat_focus)（揃える側）と `wanted_details`（読む側）の
    /// 両方が使うので、**「`reseat_focus` を通ったあとは `right_tab` がこれと等しい」が
    /// 代入 1 本で読める** —— 読む側が `right_tab` を直に見る形だと、`reseat_focus` は
    /// `update` の**出口**なので `Tab` で入ったその回だけ答えがずれる。
    fn visible_right_tab(&self) -> RightTab {
        RightTab::of(self.focus).unwrap_or(self.right_tab)
    }

    /// いま出すべき diff の対象。出すものが無ければ `None`（01 の「diff の遅延取得」）。
    ///
    /// **判定はここ 1 つ。** 次に何を読むかを決める `update` と、中身を描くか
    /// 「読み込み中」を出すかを決める `ui` が同じものを使う
    /// ([`GraphState::fresh_detail`] と同じ形)。分けて書くと、片方だけ直したときに
    /// 別のファイルの diff を出したままにできてしまう。
    pub fn wanted_diff(&self) -> Option<DiffTarget> {
        // 左の領域を Graph と差し替えて出すので、見せているかは `focus` で判る（07）。
        if self.focus != Pane::Diff {
            return None;
        }

        // **見る一覧はグラフの選択で変わる。** WIP 行ならワーキングツリー、コミット
        // なら右ペインのファイル一覧（01 の「diff の遅延取得」）。
        match self.graph.selection() {
            GraphSelection::Wip => self.wanted_worktree_diff(),
            GraphSelection::Commit(line) => {
                // **`path: None`（コミット全体）は出さない。** 一覧を出すためだけに
                // 全ファイルのハンクを組むことになる。一覧は `Cmd::LoadCommitFiles`
                // の担当（01 の「コミットのファイル一覧」）。
                let path = Some(self.graph.selected_file()?.path.clone());
                Some(DiffTarget::Commit {
                    oid: line.commit.oid,
                    path,
                })
            }
        }
    }

    /// PR の一覧を引いてよいか（01 の「PR ペイン › 何を引くか」）。
    ///
    /// **読むのは `update` だけ。** `ui` は「何を描くか」を [`PrState::load`] と
    /// [`AccountState`] の腕から出す（07 の「PRs ペイン」）—— 材料が揃うことと、
    /// 揃わない理由をどう言うかは別の問いで、後者は述語では答えられない。
    ///
    /// **HEAD は材料ではない。** 一覧はブランチに紐づかないので、detached でも
    /// unborn でも同じものが出る。
    pub fn wanted_pr_list(&self) -> bool {
        self.github_block().is_none()
    }

    /// GitHub を叩けない理由。叩けるなら `None`（01 の「PR ペイン › 引き直す（`r`）」）。
    ///
    /// **[`wanted_pr_list`](Self::wanted_pr_list) の裏返しではなく、あちらの出どころ。**
    /// 述語だけを出すと、理由が要る側（`r` の通知）が**入力をもう一度読んで消去法で
    /// 組み直す**ことになり、状態が既に持っている区別（`Degraded` / `Unasked` /
    /// `Unavailable`）が潰れる。材料を 1 つ足したときに「残るのは◯◯だけ」の側が
    /// 黙って嘘になる形でもある（`CLAUDE.md`）。
    ///
    /// **`account_view` と同じ文言を出す**（あちらの `Err` もここから採る）。
    pub fn github_block(&self) -> Option<GithubBlock> {
        self.github_target().err()
    }

    /// GitHub を叩く先。叩けないなら理由（[`github_block`](Self::github_block) の
    /// 出どころ）。
    ///
    /// **叩けることと、叩く先を 1 つの答えで返す。** 理由だけを返すと、叩ける側
    /// （PR 作成の `head` を組む `update::prs`）が `target` をもう一度読み直すことに
    /// なり、**「叩けるのに `target` が無い」という到達しない腕**を書かされる
    /// （`CLAUDE.md` の「旗と値を割らない」）。
    ///
    /// **腕の順で答えが変わる組（`Degraded` かつ `Unavailable`）は到達しない** ——
    /// `target` を書くのは `update::account` の 3 箇所だけで、`Degraded` になる
    /// `Msg::AccountResolved` は `target` が `Resolved` になってからしか出ない
    /// （2026-09-19 に `/code-review` が腕を入れ替えて確かめ、workspace が緑のまま
    /// だった）。**順は読みやすさのためで、守りではない。**
    pub fn github_target(&self) -> Result<&RepoTarget, GithubBlock> {
        match (&self.account, &self.repo.target) {
            (AccountState::Disabled, _) => Err(GithubBlock::AccountDisabled),
            (AccountState::Degraded(_), _) => Err(GithubBlock::AccountDegraded),
            (_, TargetState::Unavailable) => Err(GithubBlock::TargetUnavailable),
            (AccountState::Resolved(_), TargetState::Resolved(target)) => Ok(target),
            (_, TargetState::Unasked) | (AccountState::Resolving, _) => {
                Err(GithubBlock::AccountResolving)
            }
        }
    }

    /// 集計をまだ引いていない行の `(番号, head sha)`。**PRs タブが見えていなければ
    /// 空**（[`wanted_diff`](Self::wanted_diff) と同じ「見えているぶんだけ引く」形。
    /// latch にしない理由と引き金は 01 の「PR ペイン › 集計」）。
    pub(super) fn wanted_details(&self) -> Vec<(u64, String)> {
        if self.visible_right_tab() != RightTab::Prs {
            return Vec::new();
        }
        self.prs.wanted_details()
    }

    /// 一覧のうち、いま居るブランチの PR（01 の「現ブランチの PR」）。
    ///
    /// **状態に持たない。** `prs.list` と `repo` から毎回導く —— 写しを置くと、
    /// どちらかが更新されたときに片方だけ古くなる。
    pub fn current_pr(&self) -> Option<&PullRequest> {
        current_row(&self.repo, &self.prs.list)
    }

    /// PR 作成の `head` の候補（01 の「head の組み立て」）。HEAD がブランチに
    /// 乗っていなければ空。
    ///
    /// **[`head_homes`] とは読み手が違う。** あちらは「PR の head が**在りうる**
    /// リポジトリ」を返す寛容な照合（座り直し）で、追跡先（何を基にしたか）も入れる。
    /// こちらは**送る値の候補**なので、「そこに在る」と言える材料だけを使う（#101）。
    ///
    /// **候補は 2 つの材料から組む。**
    /// - 同名の remote-tracking ref が在るリモート（`graph.refs` の
    ///   [`RefIndex::remote_oid`]）。在ることが「そのリモートにそのブランチが在った」の
    ///   証拠で、`branch.<name>.remote`（何を基にしたか）と違って push の結果そのものが
    ///   残る。**名前はリモートの一覧から組む**（ref 名から切り出すと `/` を含む
    ///   リモート名で割り方が決まらない。02）。索引は `repo.head` と同じ
    ///   `Msg::RepoInfoLoaded` で届くので、2 つのあいだに順序は無い
    /// - 追跡設定の URL（`branch.<name>.remote` が URL のとき）。`gh pr checkout` と
    ///   mewgit の取り出しは fork の URL を書き、**remote-tracking ref を作らない**（05）。
    ///   **ブランチ名は `merge` から取る** —— 取り出しは衝突を避けてローカルを
    ///   `bob/main` にし、`merge` に `refs/heads/main` を書く（05 の「ブランチ名」）ので、
    ///   ローカルの名前で組むと fork に無いブランチを指す。`refs/heads/` の下に無い
    ///   `merge`（`refs/pull/<n>/head`）はブランチではないので候補にしない
    ///
    /// **`branch.<name>.pushRemote` は読まない。** 取り出しは `remote` にも同じ URL を
    /// 書くので上の 2 つ目で当たり、名前の `pushRemote` へ push したなら 1 つ目の ref が
    /// できる。
    ///
    /// **並びは HEAD と同じ oid の ref が先**（gh が採るのと同じ「そこまで push 済み」の
    /// 証拠）、残りはリモート名の順、URL は最後。**送る値（owner とブランチ名）で
    /// 1 つに畳む** —— `<owner>:<branch>` までしか運べないので、同じ owner の 2 つの
    /// リポジトリは区別できない（[`PrHead`]）。**GitHub の形として読めない
    /// リモートは入れない**。host は比べない（[`head_homes`] と同じ理由）。
    pub(super) fn head_candidates(&self) -> Vec<PrHead> {
        let Some(HeadInfo::Branch {
            name,
            oid,
            upstream,
        }) = self.repo.head.as_ref()
        else {
            return Vec::new();
        };
        let owner_of = |url: &str| RemoteUrl::parse(url).ok().map(|url| url.owner);

        let mut pushed: Vec<(&Remote, Oid)> = self
            .repo
            .remotes
            .iter()
            .filter_map(|remote| {
                let at = self
                    .graph
                    .refs
                    .remote_oid(&format!("{}/{name}", remote.name))?;
                Some((remote, at))
            })
            .collect();
        // **安定ソート**なので、同じ側の中ではリモート名の順（`repo.remotes` の並び）が残る。
        pushed.sort_by_key(|(_, at)| at != oid);
        let from_refs = pushed
            .into_iter()
            .filter_map(|(remote, _)| Some((owner_of(&remote.url)?, name.clone())));
        let from_url = upstream.as_ref().and_then(|upstream| {
            let url = match self.repo.tracked(&upstream.remote) {
                Tracked::Url(url) => url,
                // 名前の追跡先は「何を基にしたか」で、在る証拠にならない。在るなら
                // 1 つ目の材料（ref）で拾えている。
                Tracked::Named(_) | Tracked::Local | Tracked::Missing(_) => return None,
            };
            let branch = upstream.merge.strip_prefix("refs/heads/")?;
            Some((owner_of(url)?, branch.to_string()))
        });

        let mut heads: Vec<PrHead> = Vec::new();
        for (owner, branch) in from_refs.chain(from_url) {
            let seen = heads
                .iter()
                .any(|head| head.owner.eq_ignore_ascii_case(&owner) && head.branch == branch);
            if !seen {
                heads.push(PrHead { owner, branch });
            }
        }
        heads
    }

    /// 出している diff を**直近の書き込みより後に**読めているか。
    ///
    /// [`DiffState::fresh_view`] が「対象が合っているか」だけを見るのに対し、こちらは
    /// 鮮度まで見る。**描くのは前者、キャッシュを短絡してよいかとハンクからパッチを
    /// 組んでよいかは後者**（01 の「diff のキャッシュと鮮度」）。描く側で鮮度まで見ると、
    /// 読み直しが届くまで「読み込み中…」が挟まる。
    ///
    /// **対象では絞らない** —— [`DiffTarget::Commit`] の中身は書き込みで変わらないので
    /// 1 通ぶん無駄に読むが、単位ごとに例外を作るとその表を保つことになる（01）。
    pub fn diff_is_current(&self) -> bool {
        let wanted = self.wanted_diff();
        self.diff
            .fresh_view(wanted.as_ref())
            .is_some_and(|view| self.jobs.is_fresh(JobKind::LoadDiff, Some(view.epoch)))
    }

    /// 持っている worktree の一覧を**直近の書き込みより後に**読めているか。
    ///
    /// 一度も読めていなければ偽（`snapshot_epoch` が `None`）。commit が「stage された
    /// 変更がありません」を出してよいかの根拠（01 の「commit」）。
    pub fn worktree_is_current(&self) -> bool {
        self.jobs
            .is_fresh(JobKind::LoadStatus, self.worktree.snapshot_epoch)
    }

    /// ワーキングツリーで選んでいる行の diff の対象。
    fn wanted_worktree_diff(&self) -> Option<DiffTarget> {
        let (section, entry) = self.worktree.selected()?;
        // rename は新しい名前で絞る。`git_read` が `old_path` の側でも一致を見るので、
        // 片方だけ渡せば rename の行は落ちない（02）。
        let path = Some(entry.path.clone());
        match section {
            WorktreeSection::Staged => Some(DiffTarget::HeadVsIndex(path)),
            WorktreeSection::Unstaged | WorktreeSection::Untracked => {
                Some(DiffTarget::IndexVsWorktree(path))
            }
            // 解消ビューは MVP 外。conflicted は 2 者の diff で表せない。
            WorktreeSection::Conflicted => None,
        }
    }
}

/// 開いているリポジトリ。
///
/// `git_read`（パス・リモート）と `auth`（ホスト名の正規化）の両方から組み立てるため、
/// どちらか一方の層ではなく `app` が持つ。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RepoInfo {
    /// ワークツリーのルート。
    pub path: PathBuf,
    pub remotes: Vec<Remote>,
    pub head: Option<HeadInfo>,
    /// 既定リモートから解析・正規化した host / port / owner / repo。
    pub target: TargetState,
}

impl RepoInfo {
    /// 名前で引いたリモート。無ければ `None`（`.`（ローカル追跡）もここに落ちる）。
    ///
    /// **引き方を 1 本にする。** `update::default_remote`（`branch.<HEAD>.remote` で
    /// 引く）と [`head_homes`] が同じ走査を書いていた。
    pub(super) fn remote(&self, name: &str) -> Option<&Remote> {
        self.remotes.iter().find(|remote| remote.name == name)
    }

    /// `branch.<name>.remote` の値をどう読むか。**git の仕様で値は名前でも URL でも
    /// よい**（`gh pr checkout` と PR の取り出しは fork の URL を書く。05）。
    ///
    /// **読み方を 1 本にする。** [`head_homes`] と `update::default_remote` が別々に
    /// 「名前で引けなければ URL」を書いていた。
    ///
    /// **URL かは GitHub の形かで決めない。** git の値として URL（`:` を含む /
    /// `/` 始まり）かで決める —— `RemoteUrl::parse` で決めると、GitHub の形でない
    /// URL（`/srv/git/x.git`）が「消えたリモート名」に化ける。リモート名は ref の
    /// 部品として検査されるので `:` を含められない。
    pub(super) fn tracked<'a>(&'a self, value: &'a str) -> Tracked<'a> {
        if value == "." {
            Tracked::Local
        } else if let Some(remote) = self.remote(value) {
            Tracked::Named(remote)
        } else if value.contains(':') || value.starts_with('/') {
            Tracked::Url(value)
        } else {
            Tracked::Missing(value)
        }
    }
}

/// [`RepoInfo::tracked`] の答え。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tracked<'a> {
    /// `.`（ローカル追跡）。
    Local,
    /// 登録されているリモート。
    Named(&'a Remote),
    /// URL。
    Url(&'a str),
    /// 名前に見えるが、登録されていない（消えたリモート）。
    Missing(&'a str),
}

/// [`RepoInfo::target`] の状態。
///
/// **`Option<RepoTarget>` にしない。** 「まだ頼んでいない」と「決められなかった」を
/// 同じ `None` にすると、[`Msg::RepoInfoLoaded`](super::Msg::RepoInfoLoaded) が届く
/// たびに `ssh -G` を頼み直す —— あれは ref を動かす書き込みのたびに走るので、
/// commit / checkout / fetch のたびにサブプロセスが 1 つ増える。#31 の
/// 「`None` が 2 つのことを指していた」と同じ数え方。
///
/// **`Asked` を持たない。** 返事待ちは [`JobTracker`] が [`JobKind::ResolveTarget`] で
/// 持っており、`AppState::issue` が走っているあいだの 2 本目を落とす。ここに置くと
/// 同じ時点を 2 人が見ることになる（#31 の `DiffState.stale`）。
///
/// **リモートは起動中に変わらない前提。** mewgit に remote を足すキーが無いので、
/// 外で `git remote add` したぶんは再起動まで効かない（`core.sshCommand` を起動時に
/// 1 回だけ読むのと同じ判断。02）。
#[derive(Debug, Default, PartialEq, Eq)]
pub enum TargetState {
    /// まだ既定リモートを見ていない。
    #[default]
    Unasked,
    Resolved(RepoTarget),
    /// 既定リモートが決まらなかったか、その URL が GitHub のものではなかった。
    /// **頼み直さない。**
    ///
    /// **理由は持たない。** 出す先がまだ無く（ヘッダの degraded 表示は M4 の別タスク）、
    /// 持たせても読む腕が無い。表示を入れるときに [`DegradedReason`] へ畳むかを決める
    /// （04）。
    ///
    /// [`DegradedReason`]: crate::auth::DegradedReason
    Unavailable,
}

impl TargetState {
    /// 決まっているなら中身。`github` を叩く経路はこれを通す。
    pub fn resolved(&self) -> Option<&RepoTarget> {
        match self {
            TargetState::Resolved(target) => Some(target),
            TargetState::Unasked | TargetState::Unavailable => None,
        }
    }
}

/// GitHub を叩けない理由（[`AppState::github_block`]）。
///
/// **理由の字までは持たない。** [`notice`](Self::notice) が出すのは通知 1 行ぶんで、
/// [`DegradedReason`] を字にする `ui::degraded` は `mewgit-tui` にしかない
/// （不変ルール 6）。`AccountDegraded` が中身を運ばないのはそのため —— 運んでも
/// core では展開できず、「`A` で見られる」までしか書けない。
///
/// **読み手は `r` の通知と `A` の通知（`update::account::account_view` の `Err`）。**
/// どちらも 1 行で予算が同じなので文言を分けない（#73 の「置かれる場所の予算が違うなら
/// 別の文言でよい」が当たらない側）。**枠の内側 30 セルの PRs ペイン**（07）は別で、
/// あちらが読むのは M6 のタスク（`docs/plan.md`）。
///
/// [`DegradedReason`]: crate::auth::DegradedReason
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GithubBlock {
    /// 既定リモートが決まらなかったか、GitHub のものではなかった。**待っても変わらない。**
    TargetUnavailable,
    /// まだ決まっていないだけ —— 既定リモートを見ている最中か、`gh auth status` の
    /// 返事を待っている。**待てば変わる。**
    ///
    /// **「対象をまだ見ていない」を別の腕にしない。** [`TargetState::Unasked`] で
    /// アカウントが `Resolved` になる経路は無い（`Cmd::ResolveAccount` が出るのは
    /// 対象が決まってから）ので、割っても**テストで踏めない腕**が増えるだけ
    /// （`CLAUDE.md`）。割れているのは「待てば変わる」と「変わらない」で、それは
    /// [`TargetUnavailable`](Self::TargetUnavailable) との境目のほう。
    AccountResolving,
    /// `gh` が無い / 古い / どのアカウントも届かない。**`A` に内訳がある。**
    AccountDegraded,
    /// `config.github.enabled = false`。
    AccountDisabled,
}

impl GithubBlock {
    /// 通知 1 行ぶんの文言。
    pub fn notice(self) -> String {
        match self {
            Self::TargetUnavailable => {
                "既定リモートから GitHub のリポジトリを特定できません".to_string()
            }
            Self::AccountResolving => "アカウントを解決しています".to_string(),
            // **直し方を書いてよい。** `A` は mewgit が持っている操作で、内訳もそこに
            // 出る（#63 の「指示を足すのはその操作を持たない側」の素直な側）。
            Self::AccountDegraded => "GitHub に接続できません（A で内訳を見られます）".to_string(),
            // **直す先は設定ファイルなので、外でやってくださいと書いてよい。**
            // **再起動が要ることも書く** —— 実行中に設定は読み直さない（08）。
            Self::AccountDisabled => {
                "GitHub 機能は config.toml の github.enabled で切ってあります（要再起動）"
                    .to_string()
            }
        }
    }
}

/// 端末サイズ。幅でレイアウトを分岐する（幅 < 100 で右ペインを隠す）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermSize {
    pub width: u16,
    pub height: u16,
}

impl Default for TermSize {
    fn default() -> Self {
        Self {
            width: 80,
            height: 24,
        }
    }
}

/// 右列の上に出せるタブ（01 の「PR ペイン › 右上のタブ」）。
///
/// **[`Pane`] をそのまま持たない。** 入りうる値は 2 つだけなので、`Pane` で持つと
/// 読む側が「ほかの値は来ない」という注釈付きの `_ =>` を置くことになり、
/// **M5 で `Stash` が 3 枚目に入ったときにコンパイラが何も言わない**（`Branches` が
/// 黙って描かれる）。写像は [`of`](Self::of) の 1 本だけ。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RightTab {
    #[default]
    Branches,
    Prs,
}

impl RightTab {
    /// そのペインにフォーカスが来たとき、追随するタブ。右列の上でなければ `None`。
    pub fn of(pane: Pane) -> Option<RightTab> {
        match pane {
            Pane::Branches => Some(RightTab::Branches),
            Pane::Prs => Some(RightTab::Prs),
            Pane::Graph | Pane::Stash | Pane::Worktree | Pane::CommitDetail | Pane::Diff => None,
        }
    }

    /// このタブを出しているときにキーを受けるペイン。
    pub fn pane(self) -> Pane {
        match self {
            RightTab::Branches => Pane::Branches,
            RightTab::Prs => Pane::Prs,
        }
    }
}

/// フォーカスできるペイン。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Pane {
    #[default]
    Graph,
    Branches,
    Stash,
    Prs,
    Worktree,
    CommitDetail,
    Diff,
}

impl Pane {
    /// WIP 行を選んでいるあいだの巡回先。右は [`GraphSelection::right_pane`] が決める。
    ///
    /// **まだ実装していないペインは入れない**（07）。移れてしまうと、キーを受ける先の
    /// 無い画面に入り込む。ペインを実装したらここに足す。並びは `Pane` の宣言順。
    const WIP_FOCUS: &'static [Pane] = &[
        Pane::Graph,
        Pane::Branches,
        Pane::Prs,
        Pane::Worktree,
        Pane::Diff,
    ];

    /// `config.github.enabled = false` のときの [`WIP_FOCUS`](Self::WIP_FOCUS)。
    ///
    /// **`Prs` は真ん中にあるので末尾で落とせない。** `Diff` のように
    /// `strip_suffix` で削る形にすると並びの意味が 2 通りになるので、一覧をもう 1 本
    /// 置く（`Prs` が落ちる条件は起動時に固まるので、揺れるのは `Diff` だけ）。
    const WIP_FOCUS_NO_PRS: &'static [Pane] =
        &[Pane::Graph, Pane::Branches, Pane::Worktree, Pane::Diff];

    /// コミットを選んでいるあいだの巡回先。右は [`GraphSelection::right_pane`] が決める。
    ///
    /// **`Worktree` を入れない。** 入れると、コミットを選んだまま
    /// `focus == Pane::Worktree` でいられてしまい、`s` / `u` / `x` が画面に出ていない
    /// ファイルを stage する（01 の「Pane と Modal」）。
    ///
    /// **`Diff` は両方に入るが、出る diff は違う。** ワーキングツリーのファイルか、
    /// コミットのファイルか。決めるのは [`AppState::wanted_diff`] の 1 つ。
    ///
    /// **`WIP_FOCUS` は `Diff` を末尾に置く。** 差分を出せる行が無いときに
    /// [`AppState::focus_candidates`] が末尾を落として候補から外すため。こちらの
    /// 一覧は落とさないので、並びの制約は無い。
    ///
    /// **`Branches` と `Prs` も両方に入る。** どちらも右列の上に出ていて、左の選択で
    /// 中身が変わらないため（07 の「レイアウト」）。`Prs` が落ちるのは
    /// `config.github.enabled = false` のときだけで、**`AccountState` では変えない**
    /// —— 変えると起動直後に解決が返った瞬間に `Tab` の行き先が増える（01 の
    /// 「右上のタブ」）。
    const COMMIT_FOCUS: &'static [Pane] = &[
        Pane::Graph,
        Pane::Branches,
        Pane::Prs,
        Pane::CommitDetail,
        Pane::Diff,
    ];

    /// `config.github.enabled = false` のときの [`COMMIT_FOCUS`](Self::COMMIT_FOCUS)。
    const COMMIT_FOCUS_NO_PRS: &'static [Pane] =
        &[Pane::Graph, Pane::Branches, Pane::CommitDetail, Pane::Diff];

    /// `panes` の並びのうち `delta` 個先。端では反対側に回る。
    ///
    /// **一覧に無いペインは先頭から数え直すが、そこへ来る経路は無い。**
    /// [`AppState::reseat_focus`] が `update` の出口で候補から外れた `focus` を潰すので、
    /// ここに入る `self` は必ず `panes` にいる。`unwrap_or(0)` はその保険で、
    /// 座り直す先を変えたときにここも直す必要はない（先頭は常に `Graph`）。
    fn cycle(self, panes: &[Pane], delta: isize) -> Pane {
        let current = panes.iter().position(|&pane| pane == self).unwrap_or(0);
        let len = panes.len() as isize;
        let next = (current as isize + delta).rem_euclid(len) as usize;
        panes[next]
    }
}

/// グラフの表示行が指しているもの。
///
/// **先頭の WIP 行はコミットではない**ので `GraphState::lines` に入れず、表示行の
/// 添字を解くここで場合分けする（01 の「グラフの行」）。
///
/// コミット側が [`GraphLine`]（コミット + 罫線）なのは、`ui` が罫線を描くのに要るため。
/// [`GraphState::cursor_commit`] がコミットだけを取り出す薄い包み。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphSelection<'a> {
    /// ワーキングツリーの変更（グラフ先頭の `◇ WIP` 行）。
    Wip,
    Commit(&'a GraphLine),
}

impl GraphSelection<'_> {
    /// 右の領域に出るペイン。
    ///
    /// **選択とペインの対応はここ 1 つ。** `Enter` の行き先（`update::graph_key`）と
    /// 巡回の候補（[`AppState::focus_candidates`]）が別々に決めると、画面はコミット詳細
    /// なのにキーは Working tree が受ける状態を作れてしまう。`ui::render` だけは
    /// 中身（`&GraphLine`）を取り出す必要があるので変種で分岐するが、そちらが
    /// この対応と食い違っていないことはテストで見張る。
    pub fn right_pane(self) -> Pane {
        match self {
            GraphSelection::Wip => Pane::Worktree,
            GraphSelection::Commit(_) => Pane::CommitDetail,
        }
    }
}

/// グラフペインの状態。
#[derive(Debug, Default)]
pub struct GraphState {
    /// 表示行。コミットとレーン情報を 1 本にまとめ、添字のずれを型で消す。
    pub lines: Vec<GraphLine>,
    /// 末尾行を処理した直後のレーン状態。追加ページはここから計算を再開する（06 参照）。
    /// 行ごとに持たないのは、途中の行の `LaneState` を読む場面が無いため。
    pub lanes: LaneState,
    /// 行の右に並べる ref ラベル（02）。引くのはグラフペインだけなのでここに置く。
    /// fetch 後の読み直しは M3 で決める。
    pub refs: RefIndex,
    /// カーソルが指す**表示行**の添字。`0` は先頭の WIP 行、`1..` が `lines`。
    ///
    /// 解くのは [`GraphState::selection`] の 1 つ。各所で `lines.get(cursor - 1)` と
    /// 書くと、`- 1` の書き忘れが 1 行ずれとして静かに通る（01 の「グラフの行」）。
    pub cursor: usize,
    /// 画面最上部に出す行の添字。カーソルだけからは決まらない（カーソルを上に
    /// 動かしたとき、見えているうちは view を動かさない）ので状態として持つ。
    pub scroll: usize,
    /// グラフに使える行数。`ui` がレイアウトから出して [`Msg::Layout`] で運んでくる
    /// （`crate::app::Msg::Layout`）。core はヘッダや枠の高さを知らない。
    pub viewport: usize,
    /// 遅延取得した詳細（02）。どのコミットのものかは `detail.oid` で判る。
    /// カーソルの指すコミットと食い違っていれば、まだ届いていないか失敗したという
    /// こと。「読み込み中」のフラグを別に持たない（01 の「コミット詳細の遅延取得」）。
    pub detail: Option<CommitDetail>,
    /// 遅延取得したファイル一覧（02 の「コミットのファイル一覧」）。
    ///
    /// **`Option` で持つ。** `Vec` 1 本にすると、まだ届いていないのと
    /// `--allow-empty` のコミット（変更が 1 つも無い）が同じ見た目になる
    /// （`WorktreeState::loaded` と同じ理由。01 の「コミットのファイル一覧」）。
    pub files: Option<CommitFiles>,
    /// 直前に詳細の読み込みが失敗したコミット。**同じコミットには出し直さない。**
    ///
    /// 出すと、リポジトリが壊れているあいだ「失敗 → 再読込 → 失敗」を回す。
    /// **`Option<Oid>` なのは、対象が違えば一度も試していないから**（`bool` にすると、
    /// 別のコミットの失敗のあとに乗ったコミットまで塞ぐ）。`DiffState::failed` と
    /// 同じ形。落とすのは読み込みの成功とカーソル移動（`scrolled`）で、
    /// **移動して戻ってくれば 1 回だけ試し直せる**。
    pub detail_failed: Option<Oid>,
    /// 直前にファイル一覧の読み込みが失敗したコミット。[`detail_failed`] と同じ形。
    ///
    /// **詳細と別に持つ。** 片方だけが失敗することがあり（一覧はツリーの比較で、
    /// 本文の読み出しとは別の失敗をする）、1 本にすると成功したほうまで塞ぐ。
    ///
    /// [`detail_failed`]: Self::detail_failed
    pub files_failed: Option<Oid>,
    /// これ以上読むコミットが無い。
    pub loaded_all: bool,
    /// 直近のログ読み込みが失敗した。
    ///
    /// 立っているあいだ追加読み込みを出さない。失敗すると `lines` を捨てるので、
    /// 「残りが 1 画面以下」の条件が必ず真になり、以後のレイアウト変化のたびに
    /// 読み直しを出してしまう。次の読み込みはユーザーの操作から始める
    /// （01 の「追加ページの読み込み」）。
    pub load_failed: bool,
}

impl GraphState {
    /// 積んだ行を捨てて、先頭から読み直せる状態にする。
    ///
    /// ログ読み込みの失敗（02 の契約により、失敗したセッションは捨てられ次は先頭から
    /// 来る）と、commit のあとの読み直しで同じことをする。**1 本にまとめてあるのは、
    /// 片方に足し忘れると `lines` に無いコミットの詳細やレーンの状態が残るため。**
    /// `load_failed` はここでは触らない（呼び出し元で値が違う。失敗の側だけが立てる）。
    pub fn restart(&mut self) {
        self.lines.clear();
        // もう `lines` に無いコミットを指したままにしない。
        self.lanes = LaneState::default();
        self.detail = None;
        self.files = None;
        self.detail_failed = None;
        self.files_failed = None;
        self.cursor = 0;
        self.scroll = 0;
        self.loaded_all = false;
    }

    /// 表示行の数。先頭の WIP 行を含むので必ず 1 以上。
    ///
    /// **WIP 行は変更が無くても出す**（07 の「Graph ペイン」）。件数で出し入れすると
    /// status が届くたびに表示行数が変わり、カーソルが指すコミットを oid で座り直させる
    /// 必要が出る。常に 1 行あるなら `1 + lines.len()` の定数関係で済む。
    pub fn rows(&self) -> usize {
        self.lines.len() + 1
    }

    /// `index` 番目の表示行。[`rows`](Self::rows) の外なら `None`。
    ///
    /// **表示行から `lines` の添字への変換はここだけ。** `ui` が窓を切り出すときも
    /// ここを通す。各所で `lines.get(index - 1)` と書くと、`- 1` の書き忘れが 1 行ずれ
    /// として静かに通り、反転する行と右ペインの中身が食い違う（01 の「グラフの行」）。
    pub fn row(&self, index: usize) -> Option<GraphSelection<'_>> {
        match index.checked_sub(1) {
            Some(at) => self.lines.get(at).map(GraphSelection::Commit),
            None => Some(GraphSelection::Wip),
        }
    }

    /// カーソルが指しているもの。
    ///
    /// カーソルは常に表示行の中にいる（[`move_cursor`](Self::move_cursor) が丸める）ので
    /// `Wip` に落ちるのは本当に WIP 行を指しているときだけ。
    pub fn selection(&self) -> GraphSelection<'_> {
        self.row(self.cursor).unwrap_or(GraphSelection::Wip)
    }

    /// カーソルが指しているコミット。WIP 行を指していれば `None`。
    pub fn cursor_commit(&self) -> Option<&Commit> {
        match self.selection() {
            GraphSelection::Commit(line) => Some(&line.commit),
            GraphSelection::Wip => None,
        }
    }

    /// カーソルの指すコミットの詳細。別のコミットのものしか無ければ `None`。
    ///
    /// 「読み込み中」のフラグを持たず oid の一致で鮮度を見る（01 の「コミット詳細の
    /// 遅延取得」）。その判定をここに置くのは、`update`（次に何を読むか）と
    /// `ui`（何を描くか）が同じものを使うため。両方に書くと、片方だけ直したときに
    /// 古い詳細を出したままになってもコンパイルは通る。
    pub fn fresh_detail(&self) -> Option<&CommitDetail> {
        let oid = self.cursor_commit()?.oid;
        self.detail.as_ref().filter(|detail| detail.oid == oid)
    }

    /// カーソルの指すコミットのファイル一覧。別のコミットのものしか無ければ `None`。
    ///
    /// [`fresh_detail`](Self::fresh_detail) と同じ形。`update`（次に何を読むか）と
    /// `ui`（何を描くか）が同じものを使う。
    pub fn fresh_files(&self) -> Option<&CommitFiles> {
        let oid = self.cursor_commit()?.oid;
        self.files.as_ref().filter(|files| files.oid == oid)
    }

    /// [`fresh_files`](Self::fresh_files) の可変版。カーソルを動かすのに使う。
    ///
    /// **見えていない一覧のカーソルを動かさない。** 描くのも `wanted_diff` が対象を
    /// 決めるのも `fresh_files` を通したものだけなので、濾さずに動かすと画面に出て
    /// いない一覧のカーソルが黙って動く（[`DiffState::fresh_view_mut`] と同じ形）。
    pub fn fresh_files_mut(&mut self) -> Option<&mut CommitFiles> {
        let oid = self.cursor_commit()?.oid;
        self.files.as_mut().filter(|files| files.oid == oid)
    }

    /// カーソルの指すコミットの詳細を読みに行って失敗したままか。
    ///
    /// 「読み込み中」と「読み込めなかった」の出し分けに使う。**`ui` は
    /// `jobs.is_running` の否定から消去法で導かない。** `Cmd` の発行経路を変えた
    /// ときに片方だけ直しても通ってしまう（[`DiffState::has_failed`] と同じ形）。
    pub fn has_detail_failed(&self) -> bool {
        self.failed_here(self.detail_failed)
    }

    /// カーソルの指すコミットのファイル一覧を読みに行って失敗したままか。
    /// [`has_detail_failed`](Self::has_detail_failed) と同じ形。
    pub fn has_files_failed(&self) -> bool {
        self.failed_here(self.files_failed)
    }

    /// 失敗の印がカーソルの指すコミットのものか。
    fn failed_here(&self, failed: Option<Oid>) -> bool {
        failed.is_some() && self.cursor_commit().map(|commit| commit.oid) == failed
    }

    /// カーソルの指すコミットの一覧で選んでいるファイル。
    ///
    /// 一覧がまだ届いていないときと、変更が 1 つも無いコミットのときは `None`。
    /// [`AppState::wanted_diff`] と `ui` の反転がここを通る。
    pub fn selected_file(&self) -> Option<&FileEntry> {
        let files = self.fresh_files()?;
        files.entries.get(files.cursor)
    }

    /// カーソルを `delta` 行動かす。端では止まる。
    pub fn move_cursor(&mut self, delta: isize) {
        // 行が 1 つも積まれていなくても WIP 行はあるので、`rows()` は 1 以上。
        let last = self.rows() - 1;
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
        self.scroll_into_view();
    }

    /// カーソルが見える位置までスクロールを寄せる。
    ///
    /// カーソルが窓の中にいる限り動かさない。カーソルと `viewport` から毎回導くと、
    /// 下端から上へ戻るときにカーソルが最下行に貼り付いたまま中身だけが流れる。
    pub fn scroll_into_view(&mut self) {
        if self.viewport == 0 {
            return;
        }
        // 窓が広がったときに先に詰める。末尾に貼り付いたまま端末を大きくすると、
        // カーソルは窓の中にいるので下の分岐が動かず、上に行があるのに空白が残る。
        self.scroll = self.scroll.min(self.rows().saturating_sub(self.viewport));
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + self.viewport {
            self.scroll = self.cursor - self.viewport + 1;
        }
    }
}

/// 遅延取得したコミットのファイル一覧と、その中の選択位置。
///
/// どのコミットのものかは `oid` で判る（`CommitDetail` と同じ形）。「読み込み中」の
/// フラグを別に持たない（01 の「コミットのファイル一覧」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitFiles {
    pub oid: Oid,
    /// 02 の `commit_files()` の戻り値そのまま。空なら変更の無いコミット。
    pub entries: Vec<FileEntry>,
    /// 一覧の選択位置。左の Diff ペインの対象を決める。
    pub cursor: usize,
}

impl CommitFiles {
    /// カーソルを `delta` 行動かす。端では止まる。
    ///
    /// 行が 1 つも無ければ据え置く（選べるものが無い）。
    pub fn move_cursor(&mut self, delta: isize) {
        self.cursor = stepped_index(self.cursor, self.entries.len(), delta);
    }
}

/// グラフの 1 行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphLine {
    pub commit: Commit,
    pub row: GraphRow,
}

/// ワーキングツリーペインの状態。diff は [`AppState::diff`] にある。
///
/// カーソルは「区画 + 区画内の添字」で持つ。平坦な添字 1 つにすると、`snapshot` が
/// 入れ替わったときにどの区画のどのファイルを指していたのかを復元できない（01 の
/// 「ワーキングツリー」）。
#[derive(Debug, Default)]
pub struct WorktreeState {
    pub snapshot: WorktreeSnapshot,
    pub section: WorktreeSection,
    pub cursor: usize,
    /// 持っている [`snapshot`](Self::snapshot) を読んだ時点の [`Region::Worktree`] の世代。
    ///
    /// **旗 2 本（`stale` / `loaded`）を 1 本に畳んだもの**（01 の
    /// 「実行中ジョブ › 世代」）。`None` は「一度も読めていない」で、
    /// `Some(e)` は「世代 `e` の時点の一覧」。[`JobTracker::is_fresh`] に通すと
    /// 「直近の書き込みより後に読んだものか」が出る。
    ///
    /// **空の `snapshot` には 2 つの意味がある** — 「変更が無い」と「まだ読めていない」。
    /// commit の「stage された変更がありません」を 2 つ目で出すと、実際には stage
    /// されているのに断ることになる（01 の「commit」）。`None` と `Some` の区別が
    /// そのまま前者の根拠になる。
    pub snapshot_epoch: Option<u64>,
}

impl WorktreeState {
    /// 区画を並べる順。カーソルが進む順でもあり、`ui` が描く順でもある。
    ///
    /// Conflicted が先頭なのは、他の操作より先に解消が要るため。**並びはここだけ。**
    /// 描画側で別に並べると、見えている順とカーソルの動く順がずれる。
    pub const SECTIONS: [WorktreeSection; 4] = [
        WorktreeSection::Conflicted,
        WorktreeSection::Staged,
        WorktreeSection::Unstaged,
        WorktreeSection::Untracked,
    ];

    /// 選べる行を [`SECTIONS`](Self::SECTIONS) の順に並べたもの。空の区画は入らない。
    ///
    /// 「次に選べる行」の定義をここ 1 つにするための土台。移動
    /// （[`move_cursor`](Self::move_cursor)）と詰め直し（[`clamp`](Self::clamp)）が
    /// 同じ並びを見る。
    fn positions(&self) -> Vec<(WorktreeSection, usize)> {
        Self::SECTIONS
            .iter()
            .flat_map(|&section| {
                (0..self.snapshot.section(section).len()).map(move |index| (section, index))
            })
            .collect()
    }

    /// カーソルの指すファイル。行が無ければ `None`。
    pub fn selected(&self) -> Option<(WorktreeSection, &FileEntry)> {
        let entry = self.snapshot.section(self.section).get(self.cursor)?;
        Some((self.section, entry))
    }

    /// 全区画の合計件数。
    pub fn len(&self) -> usize {
        Self::SECTIONS
            .iter()
            .map(|&section| self.snapshot.section(section).len())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// [`SECTIONS`](Self::SECTIONS) の何番目の区画か。
    fn rank(section: WorktreeSection) -> usize {
        Self::SECTIONS
            .iter()
            .position(|&candidate| candidate == section)
            .unwrap_or(Self::SECTIONS.len())
    }

    /// `positions` の `index` 番目へカーソルを据える。
    ///
    /// 行が足りなければ最後の行、1 行も無ければ何も選べない位置に戻す。**端の扱いは
    /// ここ 1 箇所。** 移動と詰め直しで別に書くと、片方だけ端から落ちる。
    fn seat(&mut self, positions: &[(WorktreeSection, usize)], index: usize) {
        let Some(&(section, cursor)) = positions.get(index).or(positions.last()) else {
            self.section = WorktreeSection::default();
            self.cursor = 0;
            return;
        };
        self.section = section;
        self.cursor = cursor;
    }

    /// カーソルを `delta` 行動かす。区画をまたぎ、端では止まる。
    ///
    /// 区画の移動を別のキーに割り当てないのは、空の区画に入って何も選べない状態を
    /// 作らないため（01）。
    pub fn move_cursor(&mut self, delta: isize) {
        let positions = self.positions();
        match stepped(&positions, (self.section, self.cursor), delta) {
            Some((section, cursor)) => {
                self.section = section;
                self.cursor = cursor;
            }
            // 1 行も選べない。既定の位置に戻す。
            None => self.seat(&positions, 0),
        }
    }

    /// `snapshot` を入れ替えたあとに、選べる行へ寄せ直す。
    ///
    /// 選んでいたファイルが別の区画へ移る（stage / unstage）か消える（discard）ので、
    /// 元の位置がそのまま残るとは限らない。寄せ先は**元の位置以降で最初に選べる行**、
    /// 無ければ最後の行。`positions` は区画の順に並んでいるので、カーソルがまだ
    /// 有効なら「最初の位置以降」はその行自身に当たり、この呼び出しは何も変えない。
    pub fn clamp(&mut self) {
        let positions = self.positions();
        let here = (Self::rank(self.section), self.cursor);
        let next = positions
            .iter()
            .position(|&(section, index)| (Self::rank(section), index) >= here)
            .unwrap_or(positions.len());
        self.seat(&positions, next);
    }
}

/// diff ペインの状態。
///
/// **`failed` を [`DiffView`] の中に置かない。** 失敗したときは `view` が `None` なので、
/// 中に置くと印ごと消える（01 の「diff の遅延取得」）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DiffState {
    /// 届いている diff。まだ読んでいない / 失敗したときは `None`。
    ///
    /// **描くときは直に見ず [`fresh_view`](Self::fresh_view) を通す。** 別の対象の
    /// ものが残っていることがある。
    pub view: Option<DiffView>,
    /// 直前に失敗した対象。**同じ対象には出し直さない**（「失敗 → 再読込 → 失敗」を
    /// 回さないため）。対象が違えば一度も試していないので頼む。落とすのは読み込みの
    /// 成功と、ワーキングツリーのカーソル移動。
    pub failed: Option<DiffTarget>,
}

impl DiffState {
    /// `wanted` の diff。別の対象のものしか無ければ `None`。
    ///
    /// [`GraphState::fresh_detail`] と同じ形。**`view` を消して回らない。**
    /// フォーカスの移動やカーソル移動のたびに消すと、`Cmd::LoadDiff` の発行で避けた
    /// 「腕ごとの特殊ケース」を今度は消す側で作ることになる。鮮度は読むときに見る。
    ///
    /// 消さないことには利点もあって、同じファイルを開き直したときは読み直しが届くまで
    /// 前の中身をそのまま出せる（「読み込み中…」が挟まらない）。
    pub fn fresh_view(&self, wanted: Option<&DiffTarget>) -> Option<&DiffView> {
        let wanted = wanted?;
        self.view.as_ref().filter(|view| &view.target == wanted)
    }

    /// [`fresh_view`](Self::fresh_view) の可変版。カーソルを動かすのに使う。
    ///
    /// **見えていない diff のカーソルを動かさない。** 描くのは `fresh_view` を通した
    /// ものだけなので、濾さずに動かすと画面に出ていない `view` のカーソルが黙って
    /// 動く。次のタスクのハンク操作はこの `view` を読む。
    pub fn fresh_view_mut(&mut self, wanted: Option<&DiffTarget>) -> Option<&mut DiffView> {
        let wanted = wanted?;
        self.view.as_mut().filter(|view| &view.target == wanted)
    }

    /// `wanted` を読みに行って失敗したままか。
    ///
    /// 「読み込み中」と「読み込めなかった」の出し分けに使う。`is_running` の否定から
    /// 消去法で導くと、`Cmd` の発行経路を変えたときに片方だけ直しても通ってしまう。
    pub fn has_failed(&self, wanted: Option<&DiffTarget>) -> bool {
        wanted.is_some() && self.failed.as_ref() == wanted
    }
}

/// 表示中の diff。何の diff かは `target` が示す。
#[derive(Debug, PartialEq, Eq)]
pub struct DiffView {
    pub target: DiffTarget,
    /// ファイル単位で持つ。平坦化すると `GitOp::StageHunk` に渡す `path` を失う。
    pub files: Vec<FileDiff>,
    pub cursor: DiffCursor,
    /// これを読んだ時点の [`Region::Worktree`] の世代（01 の「実行中ジョブ › 世代」）。
    ///
    /// **中身は対象が同じままでも古くなる** —— ハンク単位で stage すると
    /// `IndexVsWorktree(a.txt)` のまま中身が減る。対象では鮮度を判じられないので、
    /// 読めた世代を値に添える。[`DiffState::is_current`] が判定に使う。
    pub epoch: u64,
}

impl DiffView {
    /// ハンクを (ファイル, ハンク) の順に並べたもの。カーソルが進む順。
    ///
    /// ハンクの無いファイル（バイナリ、mode だけの変更）は入らない。**選べない行を
    /// カーソルの並びに入れない**のは `WorktreeState::positions` と同じ理由で、
    /// 入れると `j` を押しても何も起きない行ができる。
    fn positions(&self) -> Vec<DiffCursor> {
        self.files
            .iter()
            .enumerate()
            .flat_map(|(file, diff)| {
                (0..diff.hunks.len()).map(move |hunk| DiffCursor { file, hunk })
            })
            .collect()
    }

    /// カーソルを `delta` 個動かす。ファイルをまたぎ、端では止まる。
    ///
    /// 1 つも選べる位置が無ければ据え置く（描く中身も無い）。
    pub fn move_cursor(&mut self, delta: isize) {
        if let Some(next) = stepped(&self.positions(), self.cursor, delta) {
            self.cursor = next;
        }
    }

    /// カーソルの指すハンクが `positions` の何番目か。無ければ `None`。
    pub fn hunk_index(&self) -> Option<usize> {
        self.positions()
            .iter()
            .position(|&position| position == self.cursor)
    }
}

/// `positions` の中で `current` から `delta` 個進んだ位置。端では止まる。
///
/// **「選べる位置の並び」を持つカーソルの端の扱いはここ 1 箇所。**
/// [`WorktreeState::move_cursor`] と [`DiffView::move_cursor`] が同じ規則を使う。
/// 並べる中身は違っても「進めて端で止める」は 1 つ。要素が無ければ `None` を返し、
/// そのときどうするかは呼び出し側が決める（Worktree は何も選べない位置に戻し、
/// Diff は据え置く）。
///
/// **平坦な添字のカーソルはここを通らない。** `0..len` が全部選べるので並びを組む
/// 必要が無く、組ませるとカーソルを 1 行動かすたびに `Vec` を確保することになる。
/// そちらは [`stepped_index`] が持つ。
fn stepped<T: Copy + PartialEq>(positions: &[T], current: T, delta: isize) -> Option<T> {
    let index = positions
        .iter()
        .position(|&position| position == current)
        .unwrap_or(0);
    positions
        .get(index.saturating_add_signed(delta))
        .or(positions.last())
        .copied()
}

/// `0..len` の添字を `delta` 動かした位置。端では止まり、要素が無ければ据え置く。
///
/// **平坦な添字のカーソルの端の扱いはここ 1 箇所。** [`CommitFiles::move_cursor`] と
/// [`BranchState::move_cursor`]、それに [`Modal::RemotePicker`] のカーソルが同じ規則を
/// 使う（Stash / PRs / [`Modal::AccountPicker`] も同じ形になる）。**呼ぶのは所有する型の
/// `move_cursor` だけ** —— 直に呼ぶ利用者を作ると、そのぶん可視性を広げることになる。
///
/// [`GraphState::move_cursor`] は通らない。WIP 行があるので行数が 0 にならず、
/// カーソルを動かしたらスクロールも寄せる必要があるため。
fn stepped_index(cursor: usize, len: usize, delta: isize) -> usize {
    let Some(last) = len.checked_sub(1) else {
        return cursor;
    };
    cursor.saturating_add_signed(delta).min(last)
}

/// diff ペインのカーソル。ファイルとその中のハンクを指す。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffCursor {
    pub file: usize,
    pub hunk: usize,
}

/// ブランチペインの状態（01 の「ブランチ一覧」）。
#[derive(Debug, Default)]
pub struct BranchState {
    /// 名前順。`git_read::branches` の戻り値そのまま。
    pub branches: Vec<Branch>,
    pub cursor: usize,
    /// 読み込みの段階。空の一覧と「まだ届いていない」を出し分けるのに要る。
    load: BranchLoad,
}

/// ブランチ一覧の読み込みがどこまで進んだか。
///
/// **`bool` を 2 つ（`loaded` / `failed`）にしない。** 4 通りのうち「まだ届いて
/// いないのに失敗した」が表現でき、どのテストからも踏めない腕になる（01）。
///
/// **`ui` には述語ではなくこの enum を見せる**（[`BranchState::load`]）。`has_failed`
/// のような述語を並べると、段階を足したときに `ui` の else に黙って落ちる。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum BranchLoad {
    /// まだ届いていない。
    #[default]
    Pending,
    Loaded,
    Failed,
}

impl BranchState {
    /// 届いた一覧で置き換える。
    ///
    /// **カーソルは選んでいたブランチの名前で座り直す**（01 の「ブランチ一覧」）。
    /// 読み直しは commit のたびに来るので、HEAD へ戻すだけだと別のブランチを見ながら
    /// commit した人のカーソルが毎回飛ぶ。**添字では座り直さない** — 一覧は名前順
    /// なので、ブランチが 1 本増えるだけで同じ添字が別のブランチを指す。
    pub(super) fn loaded(&mut self, branches: Vec<Branch>) {
        let selected = self.selected().map(|branch| branch.name.clone());
        self.cursor = selected
            .and_then(|name| branches.iter().position(|branch| branch.name == name))
            // 選んでいたものが消えた（削除 / リネーム）か、まだ何も選んでいない。
            .or_else(|| branches.iter().position(|branch| branch.is_head))
            // detached / unborn ではどの行にも印が付かないので先頭に座る。
            .unwrap_or(0);
        self.branches = branches;
        self.load = BranchLoad::Loaded;
    }

    /// 読み込みが失敗した。**読めていた一覧は捨てない**（`StatusLoaded` と同じ理由。
    /// 捨てると画面から消えるだけになる）。
    pub(super) fn failed(&mut self) {
        self.load = BranchLoad::Failed;
    }

    /// 読み込みの段階。一覧が空のときに `ui` が出す文言をここから決める
    /// （07 の「Branches ペイン」）。
    ///
    /// **`jobs.is_running` の否定から消去法で導かない**（01 の「失敗の記憶」）。
    pub fn load(&self) -> BranchLoad {
        self.load
    }

    /// カーソルの指すブランチ。一覧が空なら `None`。
    ///
    /// **切替・削除の対象はここからしか取らない。** `branches[cursor]` を呼び出し側で
    /// 書くと、一覧が空のとき（読み込み中・失敗・`git init` の直後）に添字が外れる腕が
    /// 増える。
    pub fn selected(&self) -> Option<&Branch> {
        self.branches.get(self.cursor)
    }

    /// HEAD が指しているブランチ。detached / unborn では `None`。
    ///
    /// **push の行き先も、断られたことの記憶も、HEAD はここからしか取らない。**
    /// あちらの材料（`branch.<name>.merge`、追跡 ref の位置、ahead / behind）を運ぶのは
    /// [`Branch`] の行だけで、`RepoInfo.head` が持つのは名前と oid と
    /// `branch.<name>.remote` だけ（#22 の「キャッシュした状態から破壊的操作を
    /// 選ばない」）。**鮮度の差ではない** —— `Reload::Refs` は `Cmd::LoadRepoInfo` と
    /// `Cmd::LoadBranches` を同じバッチで出すので、どちらも ref を動かす書き込みの
    /// たびに読み直される。
    pub fn head(&self) -> Option<&Branch> {
        self.branches.iter().find(|branch| branch.is_head)
    }

    /// カーソルを `delta` 行動かす。端では止まる。
    ///
    /// 行が 1 つも無ければ据え置く（[`stepped_index`]）。
    pub(super) fn move_cursor(&mut self, delta: isize) {
        self.cursor = stepped_index(self.cursor, self.branches.len(), delta);
    }
}

/// 一覧のうち、いま居るブランチの PR（01 の「現ブランチの PR」）。
///
/// **[`AppState::current_pr`] と [`PrState::loaded`] の両方がここを通る。** 座り直す
/// 先を決める側と描く側で照合の規則が割れると、カーソルが座っていない行を
/// 「現ブランチの PR」として扱える。
///
/// **fork のブランチも当てる。** 照合する相手は [`head_homes`] が確からしい順に
/// 出す（`branch.<HEAD>.remote` の URL → `target`）。
///
/// **到着順に依らない。** 材料の `head` と `remotes` はどちらも
/// [`Msg::RepoInfoLoaded`](super::Msg::RepoInfoLoaded) に乗って**同時に**届き、
/// `target` はその返信から始まる鎖（`Cmd::ResolveTarget` → `Cmd::ResolveAccount` →
/// 一覧）の途中なので、**一覧が届いた時点でどれも揃っている**。`BranchState` から
/// 追跡設定を取るとこれが崩れる（01 の「現ブランチの PR」）ので、**順序を回す
/// テストは要らない**。
pub(super) fn current_row<'a>(repo: &RepoInfo, list: &'a [PullRequest]) -> Option<&'a PullRequest> {
    let HeadInfo::Branch { name, .. } = repo.head.as_ref()? else {
        // **`Detached` も `Unborn` も `None`。** 切り離された HEAD には名前が無く、
        // コミットが 1 つも無いブランチから出した PR は無い。
        return None;
    };
    // **リポジトリ名まで見る**（`RepoRef` 丸ごとの比較）。owner だけだと、同じ owner の
    // 下に作った fork（`acme/app` → `acme/app-fork`）から出た他人の PR が
    // 「現ブランチの PR」に当たる。
    head_homes(repo).into_iter().find_map(|home| {
        list.iter().find(|pr| {
            pr.head.branch == *name
                && pr
                    .head
                    .repo
                    .as_ref()
                    .is_some_and(|repo| repo.same_as(&home))
        })
    })
}

/// HEAD のブランチから出した PR の head が在りうるリポジトリを、**確からしい順**に
/// 返す（01 の「現ブランチの PR」）。
///
/// **候補が 1 つに決まらないのは、git が「どこへ push したか」を憶えていないから。**
/// `branch.<name>.remote` が答えるのは**何を基にしたか**で、実測（git 2.55、
/// 一時リポジトリ。表は 01）では `git checkout -b fix bob/fix` のあと origin へ
/// push しても `bob` のまま。だから**追跡先で当たらなければ `target` でも当てる** ——
/// 追跡先を先に見るのは、fork 運用で upstream 側の同名ブランチから出た**他人の PR**
/// より、自分の fork の PR を優先するため。
///
/// **`branch.<name>.remote` はリモート名でも URL でもよい**（git の仕様）。
/// **`gh pr checkout` が書くのは URL のほう**（実測 2026-09-20、gh 2.101.0:
/// `branch.<b>.remote=https://github.com/<fork>/<repo>.git`）なので、名前で引けない
/// 値はそのまま URL として読む。ここを落とすと、**fork の PR をローカルに出す
/// いちばんふつうの経路**が当たらない。
///
/// **材料は引数に取らない。** 追跡設定は `repo.head` から導けるので、
/// 別々に渡すと対応の取れない組（`Detached` な `head` に `Some("origin")`）が型で
/// 組める（`CLAUDE.md` の「導出できる値を引数に取らせない」）。
///
/// **読めない追跡設定は候補にしない**（設定が無い / `.`（ローカル追跡）/ リモートが
/// 消えている / URL が GitHub の形でない）。**理由は分けない**：呼び出し側の扱いは
/// どれも同じで、**出す先も無い**（座る行が変わるだけで通知は出さない）。
///
/// **host は見ない。** [`RepoTarget::host`] は `ssh -G` で正規化済みだが、こちらは
/// URL に書かれたままなので、生のまま比べると **fork だけ別のエイリアスを使う
/// 多アカウント運用**（04）で逆に落ちる。残る穴は、別ホストの同名 `owner/repo` に
/// 同名のブランチがあり、その PR が一覧に居るとき（01）。
fn head_homes(repo: &RepoInfo) -> Vec<RepoRef> {
    let tracked = match repo.head.as_ref() {
        Some(HeadInfo::Branch {
            upstream: Some(upstream),
            ..
        }) => {
            // 名前で引ければその URL、URL ならそのもの。
            let url = match repo.tracked(&upstream.remote) {
                Tracked::Named(remote) => Some(remote.url.as_str()),
                Tracked::Url(url) => Some(url),
                Tracked::Local | Tracked::Missing(_) => None,
            };
            url.and_then(|url| RemoteUrl::parse(url).ok())
                .map(|url| RepoRef {
                    owner: url.owner,
                    repo: url.repo,
                })
        }
        _ => None,
    };
    let target = repo.target.resolved().map(|target| RepoRef {
        owner: target.owner.clone(),
        repo: target.repo.clone(),
    });
    tracked.into_iter().chain(target).collect()
}

/// PR ペインの状態（01 の「PR ペイン」）。
///
/// **中身はこのリポジトリで開いている PR の一覧**で、ブランチにもコミットにも
/// 紐づかない。**鍵を持たない**のはそのため —— 鍵が答えるのは「選択が動いたか」で、
/// 一覧には対応する問いが無い。追いつきは世代（[`JobKind::FetchPrList`] の
/// `region()`）だけで足りる。
///
/// **形は [`BranchState`] に揃えてある**（値 + 段階）。引けた値そのものを 3 状態の
/// enum に入れると、**読み直しが 1 回転けただけで読めていた一覧が画面から消える**
/// （`BranchState::failed` の doc が言う「捨てると画面から消えるだけになる」）。
/// 一覧を引き直すのは ref を動かす書き込みのたびなので、この差は毎コミット効く。
#[derive(Debug, Default)]
pub struct PrState {
    /// 開いている PR。API が返した順（更新の新しい順）そのまま。
    pub list: Vec<PullRequest>,
    /// 一覧のカーソル。
    pub cursor: usize,
    /// 読み込みの段階。空の一覧と「まだ届いていない」を出し分けるのに要る。
    load: PrLoad,
    /// PR ごとの集計。鍵は番号（01 の「PR ペイン › 集計」）。
    details: HashMap<u64, PrDetailEntry>,
    /// どのアカウントの一覧に対する集計か。[`PrState::forget`] が進める。
    ///
    /// **`(番号, head sha)` の鍵では足りない。** アカウントを切り替えても PR の番号も
    /// head も変わらないので、**前のトークンで飛んでいた扇形の答えが新しい一覧に
    /// そのまま当たる**（`forget` の doc が落とすと言っている守りを、走っている扇形が
    /// 外から破る）。鍵が答えるのは「どの行に当てるか」で、**「どのアカウントで
    /// 引いたか」には答えられない**。
    ///
    /// **[`JobTracker`] の世代とは別物。** あちらは git の書き込みが何を古くするかを
    /// 数えるもので、アカウントの切り替えでは進まない。
    epoch: u64,
    /// GitHub API の残量。ヘッダに出す（07 の「ヘッダ」）。
    ///
    /// **置き場所がここなのは、落とす引き金と突き合わせる世代が両方ここにあるから。**
    /// 残量は**そのトークンの予算**なので、アカウントが変われば嘘になる ——
    /// 落とすのは [`PrState::forget`]、飛んでいる扇形の答えを弾くのは同じ `epoch`。
    /// `AppState` の直下に置くと、落とす責務だけが別の場所に離れる。
    ///
    /// **ここへ届くのは PR の 2 本だけ**（一覧と扇形）。`auth::resolve` の probe が
    /// 使うクライアントも応答ヘッダから残量を憶えるが、**運ぶ経路が無い** ——
    /// `Msg::RateLimitSeen` を送るのはランタイムの PR の腕だけで、probe の
    /// クライアントは `auth` の中で使い捨てになる。**まだ誰のトークンか決まって
    /// いない候補の予算**を出さないため（05 の「残量の受け渡し」）。
    rate_limit: Option<RateLimit>,
    /// **いま作ったばかりで、まだ一覧に出ていない PR の番号**（01 の
    /// 「PR 作成（`N`）」）。届いた一覧でその行に座らせ、そこで落とす。
    ///
    /// **憶えないと座らない。** [`PrState::loaded`] は「いま選んでいる PR の番号」を
    /// 優先するので、作る前に別の行を選んでいると、新しい行が一覧の先頭に入っても
    /// カーソルはそのまま残る。
    ///
    /// **材料は `Msg::PrCreated` だけ。** 別のジョブから届く値を混ぜると、到着順で
    /// 答えが変わる（#63 の `RepoInfo.target`）。
    ///
    /// **当たったときだけ落とす**（[`PrState::loaded`]）。作成より前に頼まれた
    /// 一覧が先に届くことがあり（[`PrState::list_behind`]）、そこで落とすと
    /// **二度と座らない**。当たらないまま残るのは次の一覧までで、そこで
    /// [`PrState::forget_created`] が落とす。
    created: Option<u64>,
    /// **作った直後に頼んだ一覧が [`AppState::issue`] に落とされた。**
    ///
    /// `r` や書き込みで `FetchPrList` が飛んでいる最中に POST のほうが先に返ると、
    /// `pr_created` が返す `Cmd::FetchPrList` は同種実行中なので落ちる。そこへ届く
    /// 一覧は**作成より前に頼まれたもの**なので、作った PR は入っていない。
    ///
    /// **`JobTracker::is_fresh` では拾えない。** あれは領域の世代で「書き込みに
    /// 追い越されたか」を見るが、**PR 作成はローカルの ref を動かさない**ので
    /// 世代が進まない（`JobKind::CreatePr::region()` が `None`）。
    ///
    /// **1 回きり。** 当たらないたびに頼み直すと、作った PR が即座に閉じられた
    /// ときに**同じ往復を回し続ける**（01 の「`Err` から再発行してよいのは対象が
    /// 違うときだけ」と同じ形）。
    list_behind: bool,
}

/// PR 1 行ぶんの集計と、どの head sha について引いたか。
///
/// **外へは出さない**（[`PrState::detail`] が `result` だけを貸す）。`sha` を持つのは
/// 一覧と突き合わせるためで、読むのは [`in_list`] を呼ぶところだけ。一覧は
/// `sort=updated` で並びが変わり、PR の head も push で動くので、番号だけを鍵に
/// すると push で sha が進んだ PR に**前のコミットのチェック結果**が残る。
#[derive(Debug, Clone, PartialEq, Eq)]
struct PrDetailEntry {
    sha: String,
    /// **`Result` のまま持つ。** 失敗の理由は PR ごとに違う（404 は fork の削除、
    /// 403 はスコープ、レート制限は待てば直る）ので、畳むと `ui` が案内を選べない。
    result: Result<PrDetail, GithubError>,
}

/// その `(番号, head sha)` がこの一覧に居るか。
///
/// **集計の鍵の定義はここ 1 つ。** 憶える側（[`PrState::detail_loaded`]）と捨てる側
/// （[`PrState::loaded`]）が別々に書くと、片方に条件を足したときに**食い違ったまま
/// 両方のテストが緑になる**（`CLAUDE.md` の「同じ判定を 2 箇所に書かない」）。
fn in_list(list: &[PullRequest], number: u64, sha: &str) -> bool {
    list.iter()
        .any(|pr| pr.number == number && pr.head.sha == sha)
}

/// PR 一覧の読み込みがどこまで進んだか。
///
/// **`bool` を 2 つにしない**（[`BranchLoad`] と同じ理由）。
///
/// **[`BranchLoad`] と違って `Failed` が理由を持つ。** 案内に出すのが「読み込めません
/// でした」だけでは足りない —— 401 / 403 / レート制限は**次に何をするかが違う**ので、
/// [`GithubError`] をそのまま運んで `ui` に出させる（07 の「PRs ペイン」）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum PrLoad {
    /// まだ届いていない。
    #[default]
    Pending,
    Loaded,
    Failed(GithubError),
}

impl PrState {
    /// カーソルを動かす。行が 1 つも無ければ据え置く。
    pub(super) fn move_cursor(&mut self, delta: isize) {
        self.cursor = stepped_index(self.cursor, self.list.len(), delta);
    }

    /// いまカーソルが指している PR。
    pub fn selected(&self) -> Option<&PullRequest> {
        self.list.get(self.cursor)
    }

    /// 読み込みの段階。一覧が空のときに `ui` が出す文言をここから決める
    /// （07 の「PRs ペイン」）。
    ///
    /// **`ui` が見るのは一覧が空のときだけ。** 中身があるならそれを描く ——
    /// 読み直しが失敗しても読めていた一覧は捨てない（[`BranchState::load`] と同じ）。
    pub fn load(&self) -> &PrLoad {
        &self.load
    }

    /// 届いた一覧で置き換える。
    ///
    /// **カーソルは PR の番号で座り直す。** 一覧は `sort=updated` で引くので、
    /// **ref を動かす書き込みのたびに並びが変わる** —— 添字を保つと、自分が commit
    /// した瞬間にカーソルが別の PR を指す（`BranchState::loaded` が名前で座り直すのと
    /// 同じ理由）。
    ///
    /// **座り直す先は 3 つで、強い順に**：いま作った PR（[`PrState::created`]）、
    /// いま選んでいる PR の番号、`fallback` が新しい一覧から選ぶ番号（起動直後は
    /// 「いま居るブランチの PR」。01 の「現ブランチの PR」）。どれも見つからなければ
    /// 先頭。**`fallback` が新しい一覧を受け取るのは、その答えが一覧の中身から
    /// 決まるから** —— 呼ぶ側に座り直しの手順を出すと、添字のまま座る形が外から
    /// 書けるようになる。
    pub(super) fn loaded(
        &mut self,
        list: Vec<PullRequest>,
        fallback: impl FnOnce(&[PullRequest]) -> Option<u64>,
    ) {
        // **作ったばかりの PR がいちばん強い**（01 の「PR 作成（`N`）」）。人が
        // いま出したものなので、直前に選んでいた行より先に見せる。
        //
        // **当たったときだけ落とす。** 作成より前に頼まれた一覧が先に届くことが
        // あり（[`PrState::list_behind`]）、そこで無条件に落とすと**作った PR に
        // 二度と座らない**。当たらないまま残るのは次の一覧までで、そこで
        // [`PrState::forget_created`] が落とす。
        let wanted = self
            .created
            .take_if(|number| list.iter().any(|pr| pr.number == *number))
            .or_else(|| self.selected().map(|pr| pr.number))
            .or_else(|| fallback(&list));
        self.cursor = wanted
            .and_then(|number| list.iter().position(|pr| pr.number == number))
            // 選んでいたものが open から消えた（マージ / クローズ）か、まだ何も
            // 選んでいない。
            .unwrap_or(0);
        // **残すのは「引けた」集計のうち、`(番号, head sha)` が一致する行だけ。**
        // push で head が進んだ行のチェックは前のコミットのもので、一覧から消えた行の
        // ものは誰も読まない（05 の「扇形は毎回引き直さない」）。
        //
        // **引けなかった行（`Err`）はここで落とす。** 残すと `wanted_details` が
        // 二度と拾わず、**回線が 1 回切れただけでセッション中ずっと `!` のまま**に
        // なる。一覧を引き直すのは `f` と書き込みのあとなので、ここが
        // 「失敗からの出口」（01 の「何を引くか」が `reload` について言っているのと
        // 同じ形。**人が `r` を押しても落ちる** —— `forget_detail`）。
        // **輪にはならない** —— 1 回の扇形の中では答えが入って
        // `wanted_details` が空になり、次に頼むのは新しい一覧が届いたときだけ。
        self.details
            .retain(|number, entry| entry.result.is_ok() && in_list(&list, *number, &entry.sha));
        self.list = list;
        self.load = PrLoad::Loaded;
    }

    /// 読み込みが失敗した。**読めていた一覧は捨てない**（`BranchState::failed` と
    /// 同じ理由。捨てると画面から消えるだけになる）。
    pub(super) fn failed(&mut self, err: GithubError) {
        self.load = PrLoad::Failed(err);
    }

    /// この PR の集計。
    ///
    /// **`None` は「まだ引いていない」。** 表に**居ないこと**がそれで、`Err` は
    /// 「引いたが読めなかった」。**同じ `None` に畳まない**（`CLAUDE.md` の
    /// 「`None` の理由を 2 つ以上持たせない」）—— 畳むと `ui` が「CI の無い PR」と
    /// 「CI を読めなかった PR」を区別できない。
    ///
    /// 居ない行に扇形が飛ぶ条件は `AppState::wanted_details`（01 の
    /// 「PR ペイン › 集計」）。**どの sha について引いたかは外へ出さない** ——
    /// 突き合わせるのは `detail_loaded` と `loaded` の中だけで、出すと「表示している
    /// 行と違う sha の集計」を読む形が外から書ける。
    pub fn detail(&self, number: u64) -> Option<&Result<PrDetail, GithubError>> {
        self.details.get(&number).map(|entry| &entry.result)
    }

    /// まだ集計を持っていない行の `(番号, head sha)`。
    ///
    /// **持っている行は引き直さない。** 1 件ぶんの往復が PR の数だけ積むので、
    /// ref を動かす書き込みのたびに全件引くと `core` の予算に対して重すぎる
    /// （数は 05 の「扇形は毎回引き直さない」）。憶えた集計を落とすのは
    /// [`loaded`](Self::loaded)（head sha が動いた行）、[`forget`](Self::forget)
    /// （アカウントが変わった）、[`forget_detail`](Self::forget_detail)（`r` で
    /// 名指しされた行）。
    ///
    /// **`app` へは出さない。** タブの門を通さずに扇形を組む形が書けてしまうので、
    /// 外から呼べるのは [`AppState::wanted_details`] だけにしてある。
    fn wanted_details(&self) -> Vec<(u64, String)> {
        self.list
            .iter()
            .filter(|pr| !self.details.contains_key(&pr.number))
            .map(|pr| (pr.number, pr.head.sha.clone()))
            .collect()
    }

    /// 扇形の答えが 1 件届いた。
    ///
    /// **鍵は `(番号, sha)`。** 飛んでいるあいだに push で head が進んだ PR の答えは
    /// **前のコミットのもの**なので捨てる（捨てた行は集計を持たないままなので、
    /// 次の [`wanted_details`](Self::wanted_details) が拾い直す）。
    /// **一覧から消えた PR の答えも捨てる** —— 入れると、居ない行の集計が
    /// `details` に溜まる。
    pub(super) fn detail_loaded(
        &mut self,
        number: u64,
        sha: String,
        result: Result<PrDetail, GithubError>,
    ) {
        if !in_list(&self.list, number, &sha) {
            return;
        }
        self.details.insert(number, PrDetailEntry { sha, result });
    }

    /// 扇形が打ち切られた。**頼んだのに答えの返らなかった行を理由ごと埋める。**
    ///
    /// 埋めないと、その行は「まだ引いていない」のまま
    /// [`wanted_details`](Self::wanted_details) に拾われ、**同じ失敗を買い直す輪**に
    /// なる（01 の「`Err` から再発行してよいのは対象が違うときだけ」）。
    ///
    /// **埋める相手は `targets` であって、いまの一覧ではない。** 一覧のほうを舐めると、
    /// **その扇形が一度も対象にしていない行まで塞ぐ** —— 扇形が走っているあいだに
    /// commit などで一覧が増えると、増えた行は `AppState::issue` に落とされて頼まれて
    /// おらず、そこへ打ち切りの理由を書くと次の `fan_out` も空になって二度と引かれない。
    ///
    /// **いまの一覧に居ない対象は捨てる**（[`in_list`]）。頼んだあとに head が動いた
    /// 行を埋めても、その集計は次の [`loaded`](Self::loaded) が落とす。
    pub(super) fn details_aborted(&mut self, targets: &[(u64, String)], err: &GithubError) {
        for (number, sha) in targets {
            if !in_list(&self.list, *number, sha) {
                continue;
            }
            self.details
                .entry(*number)
                .or_insert_with(|| PrDetailEntry {
                    sha: sha.clone(),
                    result: Err(err.clone()),
                });
        }
    }

    /// いまの世代。扇形を頼むときに刻み、返信で突き合わせる。
    ///
    /// **`ui` は読まない。** 読むのは `update::prs`（頼むとき）と `test_util`
    /// （頼む `Cmd` を組むとき）。
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// その返信は、いまのアカウントの一覧に対するものか。
    pub(super) fn is_current(&self, epoch: u64) -> bool {
        self.epoch == epoch
    }

    /// いま判っている残量。ヘッダが読む（07）。
    pub fn rate_limit(&self) -> Option<RateLimit> {
        self.rate_limit
    }

    /// 応答が言った残量を憶える。
    ///
    /// **新しいほうだけ残す**（[`RateLimit::keep_newer`]）。ジョブは同時に走り、`Msg` が
    /// 届く順は応答の新しさの順ではない —— 一覧の答えが扇形の最中に返れば、扇形が
    /// もっと減らしたあとの数を上書きしてしまう。
    pub(super) fn rate_limit_seen(&mut self, seen: RateLimit) {
        RateLimit::keep_newer(&mut self.rate_limit, seen);
    }

    /// この行の集計を落とす。`Err` も落とす。
    ///
    /// **1 行だけなこと・世代を進めないことの理由は、呼ぶ側の 1 本**
    /// （`update::prs::refresh`。01 の「引き直す（`r`）」）。
    pub(super) fn forget_detail(&mut self, number: u64) {
        self.details.remove(&number);
    }

    /// アカウントが決まっていない状態に戻す。
    ///
    /// **一覧はどのアカウントで引いたかに紐づく。** `A` で選び直したり degraded に
    /// 落ちたりした後に前のトークンで引いた一覧を出したままにすると、`*` / `R` の印
    /// （login で決まる）だけが黙って消えた一覧が残る。**`Pending` に戻すのは、
    /// `AccountState` が `Resolved` に戻れば `prs::reload` が引き直すから**
    /// （01 の「何を引くか」）。
    pub(super) fn forget(&mut self) {
        self.list.clear();
        self.cursor = 0;
        self.load = PrLoad::Pending;
        // **集計も落とす。** 残すと、次のアカウントで引き直した一覧に**前の
        // トークンで見えた集計**が番号だけで当たる。
        self.details.clear();
        // **飛んでいる扇形の答えも当たらなくする。** 落とすだけでは足りない ——
        // 番号も head も変わらないので、前のトークンで頼んだ答えが新しい一覧に
        // そのまま入る（上の `epoch`）。
        self.epoch = self.epoch.wrapping_add(1);
        // **残量も落とす。** あれは**そのトークンの予算**で、次のアカウントの残量では
        // ない。残すと、ヘッダの `[gh: …]` だけが変わって数が前のまま残る。
        self.rate_limit = None;
        // **座らせる約束も落とす。** 一覧を捨てたので当てる先が無い。
        // **害が出るかは数えていない**（`target` は latch するので一覧は同じ
        // リポジトリのもので、同じ番号は同じ PR）—— 一覧を捨てるなら、それを
        // 指していた記憶も捨てる、という形を揃えているだけ。
        self.created = None;
        self.list_behind = false;
    }

    /// 作った PR の番号を憶える。届いた一覧でその行に座る（01 の「PR 作成（`N`）」）。
    ///
    /// **一覧に足さない。** 作った PR を手で差し込むと、並び（`sort=updated`）も
    /// 集計の鍵も `ui` の出し分けも、**一覧を引いたときとは別の経路**で作られる。
    /// 番号だけ憶えて引き直すほうが、産出元が 1 本で済む。
    pub(super) fn seat_on_created(&mut self, number: u64, list_behind: bool) {
        self.created = Some(number);
        self.list_behind = list_behind;
    }

    /// 作った直後の一覧が落とされていたか。**読んだら落とす**（1 回きり）。
    pub(super) fn take_list_behind(&mut self) -> bool {
        std::mem::take(&mut self.list_behind)
    }

    /// まだ座れていない作成の記憶があるか。
    pub(super) fn has_unseated_creation(&self) -> bool {
        self.created.is_some()
    }

    /// 座り先の記憶を落とす。**作成より後に頼んだ一覧に居なければ、もう出ない。**
    pub(super) fn forget_created(&mut self) {
        self.created = None;
    }
}

/// 世代を数える領域。**書き込みが何を古くするか**で分ける。
///
/// **1 本のカウンタにしない。** index だけを触る `stage` でもログの世代が進み、
/// 飛んでいる `LoadLog` を毎回捨てて `LogSession` の作り直し（履歴全体を辿り直す。
/// 02 の「セッションは作り直さず使い回す」）を払うことになる。どの書き込みがどこを
/// 古くするかは `update::Reload` が持っていて、その 3 値がそのままこの 3 領域に写る
/// （01 の「実行中ジョブ › 世代」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// index とワークツリー。[`JobKind::LoadStatus`] と [`JobKind::LoadDiff`]。
    Worktree,
    /// ref の索引（ラベル・HEAD・ブランチ一覧・HEAD のメッセージ）。
    Refs,
    /// 積んだログの行。[`JobKind::LoadLog`]。
    Log,
}

impl Region {
    /// 世代を持つ領域すべて。**宣言順に並べる** —— [`JobTracker`] は
    /// `region as usize` でカウンタを引き、その配列の幅をここの長さで取る。
    ///
    /// 腕を足してここに足し忘れると添字が範囲外になるので、`region_all_lists_every_arm`
    /// が網羅 `match` でコンパイルエラーにする。
    pub const ALL: [Region; 3] = [Region::Worktree, Region::Refs, Region::Log];
}

/// 実行中の非同期タスク。スピナー表示と `Cmd` の重複発行防止に使う。
///
/// **世代（epoch）も持つ。** 走らせた時点の世代をジョブに刻み、[`finish`](Self::finish)
/// が返すので、届いた返信が直近の書き込みより後に頼まれたものかを受け取り側で判じ
/// られる（01 の「実行中ジョブ › 世代」）。リーダースレッドには何も足さない
/// （`Cmd` / `Msg` は世代を運ばない）。
#[derive(Debug, Default)]
pub struct JobTracker {
    running: Vec<Job>,
    /// 領域ごとの世代。添字は [`Region::ALL`] の位置。
    epochs: [u64; Region::ALL.len()],
}

impl JobTracker {
    /// この種別のタスクが動いているか。同種の `Cmd` を二重に投げないための判定。
    pub fn is_running(&self, kind: JobKind) -> bool {
        self.running.iter().any(|job| job.kind == kind)
    }

    /// この領域のいまの世代。持っている値がこれと同じなら最新。
    pub fn epoch(&self, region: Region) -> u64 {
        self.epochs[region as usize]
    }

    /// 書き込みが 1 つ終わり、この領域の値が古くなった。
    ///
    /// **[`AppState::issue`] より先に呼ぶ。** 進めたあとに発行した `Cmd` が新しい世代で
    /// 刻まれる。逆にすると、書き込みの直後に頼んだ読み直しまで「古い要求」になり、
    /// 届いた時点で捨てて頼み直す往復を延々と繰り返す。
    ///
    /// **失敗した書き込みでも進める。** `git apply` が途中で落ちても index は動いて
    /// いることがある。どこを進めるかは `update::Reload` が成否ごと決める。
    pub fn advance(&mut self, region: Region) {
        self.epochs[region as usize] += 1;
    }

    /// 開始を記録する。同種が既に動いていれば何もせず `false` を返す。
    pub fn start(&mut self, mut job: Job) -> bool {
        if self.is_running(job.kind) {
            return false;
        }
        job.epoch = job.kind.region().map(|region| self.epoch(region));
        self.running.push(job);
        true
    }

    /// 完了を記録し、**そのジョブを頼んだ時点の世代**を返す。
    ///
    /// 走っていなければ `None`。領域を持たない種別（[`JobKind::region`] が `None`）も
    /// `None` で、そちらは返り値を読まない。
    pub fn finish(&mut self, kind: JobKind) -> Option<u64> {
        let at = self.running.iter().position(|job| job.kind == kind)?;
        self.running.remove(at).epoch
    }

    /// `asked` の刻印が、`kind` が読む領域のいまの世代と同じか。
    ///
    /// **領域は [`JobKind::region`] から引く。** 呼び出し側に渡させると、
    /// `finish(LoadStatus)` の刻印を `Region::Log` と比べる形が組めてしまい、
    /// 常に「古い」になって頼み直しを回す（`CLAUDE.md` の「旗と値を割らない」が
    /// 1 段上がった形）。
    ///
    /// **`None` は「刻印が無い」**（走っていなかった）。領域を持つ種別では「古い」に
    /// 倒す —— 一度だけ頼み直して、その返信は必ず刻印を持つ。領域を持たない種別は
    /// 書き込みで古くならないので真。
    ///
    /// 引数に取るのは**届いた返信の刻印**か、[`WorktreeState::snapshot_epoch`] のような
    /// **持っている値の刻印**。追いつき（頼み直すか）を決めるときは前者で判じる ——
    /// 後者で判じると、読み取りが失敗し続けたときに「古い → 頼み直す → 失敗」を回す
    /// （失敗は値を入れ替えないので刻印が古いまま残る）。
    pub fn is_fresh(&self, kind: JobKind, asked: Option<u64>) -> bool {
        match kind.region() {
            None => true,
            Some(region) => asked == Some(self.epoch(region)),
        }
    }

    /// 走っているこの種別のジョブに「中断を頼んだ」と刻む。
    ///
    /// **まだ頼んでいなければ真。** 走っていないときと、既に頼んだあとは偽 ——
    /// `Ctrl-C` はそこで終了に落ちる（01 の「キャンセル」）。刻みを持たないと、
    /// SIGTERM で死なない git のあいだ `Ctrl-C` が中断を返し続け、モーダル表示中は
    /// 脱出口が 1 つも無くなる。
    ///
    /// **刻みはジョブと一緒に消える。** [`finish`](Self::finish) が `Job` ごと
    /// 落とすので、次の書き込みは頼んでいない状態から始まる。
    pub fn request_cancel(&mut self, kind: JobKind) -> bool {
        let Some(job) = self.running.iter_mut().find(|job| job.kind == kind) else {
            return false;
        };
        if job.canceling {
            return false;
        }
        job.canceling = true;
        true
    }

    /// この種別のジョブに中断を頼んであるか。走っていなければ偽。
    ///
    /// 読むのは案内文の側（[`AppState::notify_dropped`]）。頼んだかどうかで
    /// 「次に押すキー」が変わる。
    pub fn cancel_requested(&self, kind: JobKind) -> bool {
        self.running
            .iter()
            .any(|job| job.kind == kind && job.canceling)
    }

    /// 表示用。実行中のタスクを登録順に返す。
    pub fn iter(&self) -> impl Iterator<Item = &Job> {
        self.running.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.running.is_empty()
    }
}

/// 実行中のタスク 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub kind: JobKind,
    /// ヘッダのスピナーに添える短い説明。どの `Cmd` から来たかで決まるので、
    /// [`Cmd::job`] が種別と一緒に返す（`Exec` は `fetch` / `push` のように
    /// 走らせるサブコマンド）。
    pub label: &'static str,
    /// 走らせた時点の、この種別の領域の世代。
    ///
    /// **非公開で、[`JobTracker::start`] だけが入れる。** 組み立てる側に持たせると、
    /// 刻む場所が `start` の外に増える（`issue` の外で `Cmd` を発行する経路ができた
    /// ときに、世代の無いジョブが黙って混ざる）。領域を持たない種別
    /// （[`JobKind::region`] が `None`）では `None` のまま。
    epoch: Option<u64>,
    /// 中断を頼んだか（[`JobTracker::request_cancel`]）。**2 回目の `Ctrl-C` を
    /// 終了に落とすために持つ。** 持たないと、SIGTERM で死なない git のあいだ
    /// `Ctrl-C` が中断を返し続け、モーダル表示中は脱出口が 1 つも無くなる
    /// （`q` は入力文字。01 の「キャンセル」）。
    canceling: bool,
}

impl Job {
    /// スピナーの説明を添えて組む。世代は [`JobTracker::start`] が刻む。
    pub fn new(kind: JobKind, label: &'static str) -> Self {
        Self {
            kind,
            label,
            epoch: None,
            canceling: false,
        }
    }
}

/// タスクの種類。`Cmd` の変種と 1 対 1 で、対応は [`Cmd::job_kind`] が持つ。
///
/// **粒度は種別単位で、対象（Oid やパス）は持たない。** `LoadCommitDetail` では、
/// カーソルが動いたぶんの追いつきを `Msg::CommitDetailLoaded` の側で行うことにした
/// （01 の「コミット詳細の遅延取得」）。Oid まで含めて同一性を取ると、スクロール中の
/// 全コミットぶんの要求が同時に走る。`LoadDiff` も同じ方針で M2 に持ち越す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    LoadLog,
    LoadCommitDetail,
    LoadCommitFiles,
    LoadHeadMessage,
    LoadRepoInfo,
    LoadStatus,
    LoadBranches,
    LoadDiff,
    Exec,
    ResolveTarget,
    ResolveAccount,
    ListAccounts,
    FetchPrList,
    FetchPrDetails,
    CreatePr,
    OpenPr,
}

impl JobKind {
    /// この種別が読む値が、どの領域の世代で古くなるか。
    ///
    /// `None` は「書き込みでは古くならない」か、**値そのものが鍵を持っていて世代より
    /// 強い**もの（01 の「実行中ジョブ › 世代」）。コミットの詳細とファイル一覧は
    /// `Msg` が運ぶ Oid で追いつくので、世代では判じない（amend で Oid が変わった
    /// 返信は、鮮度を見るまでもなく別のコミットのものとして捨てられる）。
    ///
    /// 網羅 `match` にしているのは、種別を足したときにどの領域なのかを選ばせるため。
    pub fn region(self) -> Option<Region> {
        match self {
            // index とワークツリー。どの書き込みでも古くなる。
            JobKind::LoadStatus | JobKind::LoadDiff => Some(Region::Worktree),
            // ref から導かれる値。`LoadHeadMessage` もここ —— commit / amend が
            // 走っているあいだに `C` を押すと、書き込み前の本文で amend の入力欄が
            // 埋まる。
            // **PR の一覧もここ。** 一覧は選択に紐づかないので鍵が無く、**世代だけ**が
            // 「書き込みで中身が変わった」を言う（commit / push で head sha が進み、
            // CI が回り直し、マージすれば open から消える）。世代を見ないと、書き込みの
            // 最中に飛んでいた要求の答え（書き込み前の GitHub の状態）が採られ、
            // そのあとの引き直しは `AppState::issue` に落とされたまま誰も拾わない。
            JobKind::LoadRepoInfo
            | JobKind::LoadBranches
            | JobKind::LoadHeadMessage
            | JobKind::FetchPrList => Some(Region::Refs),
            // 積んだログの行。ref が動いて walk に入る集合が変わったときだけ古くなる。
            JobKind::LoadLog => Some(Region::Log),
            // **PR ごとの集計は鍵のほうが強い。** 答えは `(番号, head sha)` を
            // 運び、受け取り側が**いまの一覧**と突き合わせる（`PrState::detail_loaded`）
            // ので、書き込みの最中に飛んでいた答えは head sha が進んだ時点で捨てられる。
            // **世代を足しても増える守りが無い** —— 世代で「古い」と判じたところで、
            // 集計を憶えている行は `PrState::wanted_details` が拾わないので頼み直しが
            // 出ない。**sha が変わらないまま中身が変わる**（レビューが付く / CI が
            // 回り直す）のは世代でも鍵でも拾えないので、そこは人が `r` を押して
            // 記憶を落とす（01 の「引き直す（`r`）」）。
            JobKind::FetchPrDetails
            // 書き込みそのものと、Oid で追いつくもの、リポジトリの外から来るもの。
            | JobKind::Exec
            | JobKind::LoadCommitDetail
            | JobKind::LoadCommitFiles
            // `ssh -G` はリポジトリの ref を読まない。読むのは `.git/config` の
            // リモート URL と `~/.ssh/config` で、どちらも git の書き込みで動かない。
            | JobKind::ResolveTarget
            | JobKind::ResolveAccount
            // `gh auth status` が読むのは gh 自身の設定で、git の書き込みで動かない。
            | JobKind::ListAccounts
            // **`OpenPr` は値を読まない**（ブラウザに渡すだけ）。
            | JobKind::OpenPr
            // **`CreatePr` が返すのは作った PR そのもの。** 書き込みに追い越されて
            // 古くなるという問いが無い（番号は変わらない）。一覧のほうは
            // `FetchPrList` が世代で追いつく。
            | JobKind::CreatePr => None,
        }
    }
}

/// トースト通知 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub level: NoticeLevel,
    pub message: String,
    /// 実行した argv や stderr など、展開して見せる詳細（不変ルール 4）。
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warn,
    Error,
}

/// モーダル。表示中はキー入力をすべてモーダルが受ける。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    /// 破壊的操作の確認。
    ///
    /// `on_yes` が `Msg` ではなく `Cmd` なのは、破壊的操作がすべて
    /// `Cmd::Exec(GitOp)` で表せるため。`Msg` を入力イベントと非同期完了だけの
    /// 語彙に保つ。
    Confirm {
        title: String,
        body: String,
        on_yes: Cmd,
    },
    /// 1 行入力。確定後に何をするかは [`InputIntent`] がデータとして持つ。
    Input {
        title: String,
        prompt: String,
        buf: TextBuffer,
        intent: InputIntent,
    },
    CommitMessage {
        buf: TextBuffer,
        amend: bool,
    },
    RebaseEditor {
        plan: RebasePlan,
        cursor: usize,
    },
    /// PR を作る（01 の「PR 作成（`N`）」）。`N` で開く。
    ///
    /// **`base` も [`TextBuffer`]。** 要件がベースブランチの指定を求めており、
    /// 編集できる欄はカーソルを持てないといけない（設計にはずっと `String` と
    /// 書いてあった）。
    ///
    /// **どの欄を編集しているかは [`PrField`] が持つ。** 欄が複数あるので、
    /// カーソルの位置だけでは決まらない。
    ///
    /// **`head` は選ばせる**（01 の「head の組み立て」）。`heads` は `N` を押した時点の
    /// 候補と、そのどれを選んでいるか。**確定のときに候補を組み直す**ので、ここに
    /// あるのは画面に出すための写し。
    PrCreate {
        title: TextBuffer,
        body: TextBuffer,
        base: TextBuffer,
        heads: PrHeads,
        draft: bool,
        field: PrField,
    },
    /// アカウント解決の内訳を出し、選べるなら選ばせる（01 の
    /// 「アカウントの選び直し」）。`A` で開く。中身は [`AccountView`] が割る。
    ///
    /// **`Resolving` では開かない**（通知に落とす。01 の表）。
    AccountPicker {
        view: AccountView,
        cursor: usize,
    },
    /// fetch するリモートを一覧から選ぶ（01 の「決まらないときの逃げ道」）。
    ///
    /// **`f` が既定リモートを決められないときだけ開く。** 決められないのは
    /// 「黙って選ぶと危ない」形（`origin` が無い / 追跡先がローカル / 追跡先が一覧に
    /// 無い）で、**危ないのは mewgit が選ぶこと**。人が選ぶなら危なくないので、
    /// **候補が 1 本でも素通しにしない**。
    ///
    /// **確定後に何をするかを持たない**（`InputIntent` に当たるものが無い）。
    /// 出口は `GitOp::Fetch` 1 つだけで、`p` / `P` はここを通らない（01）。
    /// 2 つ目の出口が要るときに、そのタスクで `InputIntent` と同じ形を足す。
    RemotePicker {
        /// なぜ選ばされているか。`update::default_remote` が返した理由をそのまま出す。
        /// **無いと、毎回出るのか設定が壊れているのかが判らない。**
        reason: String,
        remotes: Vec<String>,
        cursor: usize,
    },
    Help,
}

impl Modal {
    /// 一覧から 1 つ選ぶモーダルのカーソルを動かす。カーソルを持たないモーダルでは
    /// 何もしない。
    ///
    /// **端の扱いは [`stepped_index`]**（ペインのカーソルと同じ規則）。ここに置くのは、
    /// 平坦な添字のカーソルが**どれも所有する型の `move_cursor` に包まれている**ため
    /// （[`BranchState::move_cursor`] / [`CommitFiles::move_cursor`]）。`update` から
    /// [`stepped_index`] を直に呼ぶと、そのために可視性を広げることになる。
    ///
    /// [`Modal::RebaseEditor`] が入るときは、ここに腕を足す。
    pub(super) fn move_cursor(&mut self, delta: isize) {
        match self {
            Modal::RemotePicker {
                remotes, cursor, ..
            } => *cursor = stepped_index(*cursor, remotes.len(), delta),
            // **選べない degraded では候補が空**なので、`stepped_index` が 0 を返して
            // 動かない（`NoAccess` の内訳は本文に出る。07）。
            Modal::AccountPicker { view, cursor } => {
                *cursor = stepped_index(*cursor, view.candidates().len(), delta);
            }
            Modal::Confirm { .. }
            | Modal::Input { .. }
            | Modal::CommitMessage { .. }
            | Modal::RebaseEditor { .. }
            | Modal::PrCreate { .. }
            | Modal::Help => {}
        }
    }
}

/// [`Modal::PrCreate`] のどの欄を編集しているか（01 の「PR 作成（`N`）」）。
///
/// **巡回の順は宣言順**（`Tab` が次へ = [`PrField::next`]、`Shift+Tab` が前へ）で、
/// 07 が描く縦の並びと同じ。**画面の並びと巡回の順を割らない** —— 割ると `Tab` を
/// 押した先が画面のどこかを目で追えなくなる。
///
/// **`Draft` だけが文字を打つ欄ではない。** 端末カーソルを置ける場所が無いので、
/// どの欄に居るかの印はラベルの反転が出す（07）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PrField {
    #[default]
    Title,
    Body,
    Base,
    /// **文字を打つ欄ではない**（`Draft` と同じ）。`←` / `→` で候補を選び直す。
    Head,
    Draft,
}

impl PrField {
    /// `Tab` の行き先。端では巻く。
    ///
    /// **[`stepped_index`] を使わない。** あちらは一覧のカーソルで、**端で止まる**
    /// （行が増減するものを行き過ぎないため）。欄は増えないので、`Tab` を押し続けて
    /// 最後の欄で止まるほうが驚く。
    ///
    /// **並びの配列を持たない。** 網羅 `match` で書くと、**変種を足したときに
    /// コンパイルエラーになる** —— 配列にすると長さの辻褄が合ってしまい、足した欄が
    /// 巡回から黙って漏れる（`CLAUDE.md` の「離れた場所に、コードの『いま N ある』を
    /// 書かない」の、並びの版）。
    pub(super) fn next(self) -> Self {
        match self {
            PrField::Title => PrField::Body,
            PrField::Body => PrField::Base,
            PrField::Base => PrField::Head,
            PrField::Head => PrField::Draft,
            PrField::Draft => PrField::Title,
        }
    }

    /// `Shift+Tab` の行き先。[`PrField::next`] の逆。
    pub(super) fn prev(self) -> Self {
        match self {
            PrField::Title => PrField::Draft,
            PrField::Body => PrField::Title,
            PrField::Base => PrField::Body,
            PrField::Head => PrField::Base,
            PrField::Draft => PrField::Head,
        }
    }
}

/// PR 作成の `head` の候補 1 つ（01 の「head の組み立て」）。
///
/// **owner だけを持ち、リポジトリ名は持たない。** `POST /pulls` の `head` が運べるのは
/// `<owner>:<branch>` までなので、同じ owner の 2 つのリポジトリは送る値の上で
/// 区別できない —— 持つと、区別できない 2 つを別の候補として並べることになる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrHead {
    pub owner: String,
    pub branch: String,
}

impl PrHead {
    /// 欄に出す字。**常に `<owner>:<branch>`** —— どのリポジトリから出すかを
    /// 人に確かめさせる欄なので、送る値（`PrHead::api_value`）と違って owner を
    /// 落とさない。
    pub fn label(&self) -> String {
        format!("{}:{}", self.owner, self.branch)
    }

    /// `POST /pulls` の `head`。**`target` と同じ owner ならブランチ名だけ**、違えば
    /// `<owner>:<branch>`（gh の `QualifiedHeadRef` と同じ形）。GitHub の login は
    /// 大文字小文字を区別しないので、比べるときもそうする。
    pub(super) fn api_value(&self, target_owner: &str) -> String {
        if self.owner.eq_ignore_ascii_case(target_owner) {
            self.branch.clone()
        } else {
            self.label()
        }
    }
}

/// [`Modal::PrCreate`] の `head` の候補と、選んでいる位置（01 の「head の組み立て」）。
///
/// **カーソルを持ち主の型に包む**（`BranchState::move_cursor` /
/// `CommitFiles::move_cursor` と同じ）。一覧とカーソルを `Modal` のフィールドに
/// 並べると、端の扱い（`stepped_index`）を `Modal` に置くことになり、
/// **どの欄に居るときだけ動かすか**の判断と割れる。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrHeads {
    list: Vec<PrHead>,
    cursor: usize,
}

impl PrHeads {
    /// 先頭を選んで開く。**既定が先頭でよいのは、欄に出て人が確定するから**
    /// （#101 の「候補の一覧の先頭を断定値に使わない」を破らない理由。01）。
    pub fn new(list: Vec<PrHead>) -> Self {
        Self { list, cursor: 0 }
    }

    pub fn list(&self) -> &[PrHead] {
        &self.list
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// 選んでいる候補。候補が空なら `None`（開いているあいだに消えた形）。
    pub fn selected(&self) -> Option<&PrHead> {
        self.list.get(self.cursor)
    }

    /// `←` / `→`。**端で止まる**（ペインのカーソルと同じ `stepped_index`）。
    pub fn move_cursor(&mut self, delta: isize) {
        self.cursor = stepped_index(self.cursor, self.list.len(), delta);
    }
}

/// [`Modal::AccountPicker`] が出す 2 つの中身（01 の「アカウントの選び直し」）。
///
/// **`A` は `Resolved` でも `Degraded` でも開く**ので、出すものが 2 つある ——
/// 解決に失敗した内訳と、決まっているアカウントの選び直し。どちらも「一覧から
/// 1 つ選ぶ」骨格を共有するが、候補の出どころが違う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountView {
    /// degraded の内訳。
    ///
    /// **持つのは [`DegradedReason`] そのもの。** 選べる候補
    /// （[`DegradedReason::candidates`]）も内訳（`NoAccess` の `attempts`、
    /// `Unusable` の `message`）もそこから引けるので、`Vec<GhLogin>` を写すと
    /// 同じ候補が [`AppState::account`] と 2 箇所にでき、`ui` と `update` が
    /// どちらを読むかで答えが割れうる。
    Degraded(DegradedReason),
    /// 決まっているアカウントの選び直し。
    Repick(Repick),
}

/// 決まっているアカウントを選び直す画面の中身（01 の「候補の取り直し」）。
///
/// **`login` / `host` / `source` は [`AppState::account`] の写し。** `ui` の
/// モーダル描画は `Modal` しか受け取らない（`modals::render`）ので、ここに無い値は
/// 画面に出せない —— [`Modal::RemotePicker`] が `remotes` を写しているのと同じ形。
///
/// **写しが古くなる瞬間は在る。** 「開いているあいだ [`AppState::account`] は
/// 動かない」と書いていたが**嘘だった**（`/code-review` が拾った）——
/// `update::submit_exec` は `git` を走らせる**前に**モーダルを閉じ、`A` には
/// 実行中の断りが無いので、走っているあいだに**書き込み前の写しを持ったモーダルを
/// 開き直せる**。そこで `update::account::resolve_again` が
/// **`account` を動かすときに開いている `AccountPicker` を閉じる** ——
/// 写しを禁じるのではなく、**写しの寿命を切る**（01 の「アカウントの選び直し」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repick {
    pub login: String,
    pub host: String,
    /// いまの決め方。**`u` を出すかどうかもここから引く**
    /// （[`AccountSource::is_unsettable`]）。
    pub source: AccountSource,
    pub logins: AccountListing,
}

/// 取り直した候補一覧（01 の「候補の取り直し」）。
///
/// **3 値にする。** `Option<Vec<GhLogin>>` だと「待っている」と「引けなかった」が
/// 同じ `None` になり、本文に理由を出せない（「`None` の理由を 2 つ以上持たせない」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountListing {
    /// `Cmd::ListAccounts` の返事待ち。**開いた直後は必ずここ** —— `gh auth status` は
    /// 0.95 秒かかるので（#61 の実測）、待ってから開くと `A` が 1 秒無反応になる。
    Waiting,
    Listed(Vec<GhLogin>),
    /// `gh` を呼べなかった理由。
    ///
    /// **[`DegradedReason`] に畳まない。** 畳むと `auth::resolve` の外に
    /// [`AccountState`] の産出元ができ、04 が決めた境目が消える（01）。
    Failed(String),
}

impl AccountView {
    /// `Enter` で選べる候補。
    ///
    /// **読み手が散っているので 1 本にする** —— `ui`（一覧の行・タイトル・キーの
    /// 案内）、`Modal::move_cursor`、`Enter` の確定。別々に `matches!` を書くと、
    /// 選ばせないはずの画面で `Enter` だけが効く。
    ///
    /// **`Waiting` と `Failed` が空なのは意図。** 候補が届いていない画面で `Enter` を
    /// 効かせる相手がいない。
    pub fn candidates(&self) -> &[GhLogin] {
        match self {
            AccountView::Degraded(reason) => reason.candidates(),
            AccountView::Repick(repick) => match &repick.logins {
                AccountListing::Listed(logins) => logins,
                AccountListing::Waiting | AccountListing::Failed(_) => &[],
            },
        }
    }

    /// `u`（指定の解除）が効くか。
    ///
    /// **キーの案内（`[u] 指定を解除`）もこれを読む。** 別々に見ると、効かない画面に
    /// 案内だけが出る（#53）。
    ///
    /// **degraded でも真になりうる。** [`DegradedReason::ConfiguredUnavailable`] は
    /// 明示指定のトークンが取れなかった形で、**外す対象は在る** —— ここを偽に
    /// すると、壊れた `mewgit.user` を憶えたまま `A` が案内しか出せない行き止まりが
    /// 残る（塞ぎたかった `Resolved` 側の穴の、degraded に落ちた版）。
    ///
    /// **どの degraded が出どころを持つかは [`DegradedReason::configured_source`] に
    /// 聞く。** ここで変種を `match` すると、`_ =>` の受け皿が
    /// [`DegradedReason`] とは別のモジュールに座り、出どころを運ぶ変種が増えても
    /// 黙って偽を返す。
    pub fn is_unsettable(&self) -> bool {
        self.source().is_some_and(AccountSource::is_unsettable)
    }

    /// `r`（解決し直す）が効くか。**キーの案内（`[r] 解決し直す`）もこれを読む**
    /// （`is_unsettable` と同じ理由。#53）。
    ///
    /// **degraded だけ。** `Repick` を外してある理由は 01 の「外で直したあとの
    /// 再解決」にある。
    pub fn is_reresolvable(&self) -> bool {
        matches!(self, AccountView::Degraded(_))
    }

    /// この画面が指している明示指定の出どころ。無ければ `None`。
    fn source(&self) -> Option<AccountSource> {
        match self {
            AccountView::Degraded(reason) => reason.configured_source(),
            AccountView::Repick(repick) => Some(repick.source),
        }
    }
}

/// [`Modal::Input`] の確定後に何をするか。
///
/// コールバック（`fn(String) -> Msg`）にしないのは、`fn` ポインタが環境を
/// キャプチャできず、`BranchRename` の `from` のような文脈を運べないため。
/// データで持つことで [`Modal`] が `PartialEq` を保て、遷移をテストできる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputIntent {
    /// ブランチを作る。作ったら切り替える（`switch --create`）。
    ///
    /// **`GitOp::BranchCreate` の `switch: bool` はここに持たない**（03 の
    /// 「argv への変換」に理由がある）。
    BranchCreate {
        /// 起点。`None` が HEAD（Branches ペインの `n`）、`Some` がグラフで選んで
        /// いるコミットの Oid（Graph ペインの `n`）。
        ///
        /// タグを起点にしたいときは、そのタグのラベルが乗った行をグラフで選ぶ
        /// （タグ一覧のペインは無い。07 の「Graph ペイン」）。
        start: Option<String>,
    },
    /// ブランチの名前を変える（`branch --move`）。`from` は元の名前。
    BranchRename { from: String },
}

/// 複数行の編集バッファ。
///
/// ratatui のウィジェットや `tui-textarea` は状態に持たない（core が UI に依存するため）。
/// 編集操作は M2 の commit タスクで実装する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBuffer {
    pub lines: Vec<String>,
    /// (行, 列)。どちらも 0 始まり。
    pub cursor: (usize, usize),
}

impl Default for TextBuffer {
    /// 空でも 1 行はある。cursor が指す行が常に存在するようにするため。
    fn default() -> Self {
        Self::from_text("")
    }
}

/// [`TextBuffer::move_cursor`] に渡す移動。
///
/// `KeyCode` をそのまま渡さないのは、文字キーまで移動として受けられる形になるため。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

impl TextBuffer {
    /// 文字列から作る。改行で分割する。
    pub fn from_text(text: &str) -> Self {
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        let row = lines.len() - 1;
        let col = lines[row].chars().count();
        Self {
            lines,
            cursor: (row, col),
        }
    }

    /// 改行で連結して 1 つの文字列にする。
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// カーソルのいる行の、カーソルより左。
    ///
    /// 描画側がカーソルの桁を出すのに使う。**列からバイト位置への変換をここで済ませる**
    /// ため、`ui` は「列は文字数」という約束を知らずに済む。2 箇所で解くと、単位を
    /// 変えたときに core だけ直して端末のカーソルが黙ってずれる。
    pub fn before_cursor(&self) -> &str {
        let (row, col) = self.cursor;
        &self.lines[row][..byte_at(&self.lines[row], col)]
    }

    /// 空白しか無いか。コミットメッセージを断る判定に使う（01 の
    /// 「コミットメッセージの編集」）。
    pub fn is_blank(&self) -> bool {
        self.lines.iter().all(|line| line.trim().is_empty())
    }

    /// カーソル位置に 1 文字入れる。
    pub fn insert_char(&mut self, c: char) {
        let (row, col) = self.cursor;
        let at = byte_at(&self.lines[row], col);
        self.lines[row].insert(at, c);
        self.cursor.1 = col + 1;
    }

    /// 文字列をカーソル位置に入れる。改行は行を割る。
    ///
    /// 貼り付けの入口。**1 文字ずつ [`TextBuffer::insert_char`] を呼ぶ形にしない**の
    /// は、改行と制御文字の扱いをここで 1 回だけ決めるため。
    ///
    /// **制御文字は落とす**（`\r` を含む）。CRLF のまま入れると行末に制御文字が残り、
    /// タブは `ui` が数える桁とずれてカーソルの位置が合わなくなる（07 の
    /// 「CommitMessage モーダル」は折り返さない代わりに桁を自分で出している）。
    pub fn insert_text(&mut self, text: &str) {
        for (n, line) in text.split('\n').enumerate() {
            if n > 0 {
                self.insert_newline();
            }
            self.insert_line(line);
        }
    }

    /// カーソル位置に**行を増やさずに**入れる。
    ///
    /// 制御文字を落とすので改行も消える。1 行入力（`Modal::Input`）の貼り付けが
    /// これを呼ぶ（01 の「1 行入力」）。**呼び出し側で改行を除いてから
    /// [`TextBuffer::insert_text`] に渡さない** — 改行をどう扱うかの判断が 2 箇所に
    /// 割れるうえ、貼り付け全体のコピーを 1 本余計に確保することになる。
    pub fn insert_line(&mut self, text: &str) {
        for c in text.chars().filter(|c| !c.is_control()) {
            self.insert_char(c);
        }
    }

    /// カーソル位置で行を割る。
    pub fn insert_newline(&mut self) {
        let (row, col) = self.cursor;
        let at = byte_at(&self.lines[row], col);
        let tail = self.lines[row].split_off(at);
        self.lines.insert(row + 1, tail);
        self.cursor = (row + 1, 0);
    }

    /// カーソルの左を 1 文字消す。行頭なら前の行とつなぐ。
    ///
    /// **最後の 1 行は消さない。** `lines` が空になると `cursor` の指す行が無くなる。
    pub fn backspace(&mut self) {
        let (row, col) = self.cursor;
        if col > 0 {
            let at = byte_at(&self.lines[row], col - 1);
            self.lines[row].remove(at);
            self.cursor.1 = col - 1;
        } else if row > 0 {
            let tail = self.lines.remove(row);
            let joined_at = self.len(row - 1);
            self.lines[row - 1].push_str(&tail);
            self.cursor = (row - 1, joined_at);
        }
    }

    /// カーソルの位置を 1 文字消す。行末なら次の行をつなぐ。
    pub fn delete(&mut self) {
        let (row, col) = self.cursor;
        if col < self.len(row) {
            let at = byte_at(&self.lines[row], col);
            self.lines[row].remove(at);
        } else if row + 1 < self.lines.len() {
            let next = self.lines.remove(row + 1);
            self.lines[row].push_str(&next);
        }
    }

    /// カーソルを動かす。
    ///
    /// **行末より右・最終行より下へは出ない。** 上下移動で短い行に移ったときは行末へ
    /// 寄せる。「元の桁を覚えて長い行で戻す」形にはしない（覚えた桁は状態が増える
    /// うえ、編集で無効になる時点を決められない）。左右は行をまたぐ。
    pub fn move_cursor(&mut self, to: Motion) {
        let (row, col) = self.cursor;
        let last = self.lines.len() - 1;
        self.cursor = match to {
            Motion::Left if col > 0 => (row, col - 1),
            Motion::Left if row > 0 => (row - 1, self.len(row - 1)),
            Motion::Right if col < self.len(row) => (row, col + 1),
            Motion::Right if row < last => (row + 1, 0),
            Motion::Up if row > 0 => (row - 1, col.min(self.len(row - 1))),
            Motion::Down if row < last => (row + 1, col.min(self.len(row + 1))),
            Motion::Home => (row, 0),
            Motion::End => (row, self.len(row)),
            // 端では動かない。
            Motion::Left | Motion::Right | Motion::Up | Motion::Down => (row, col),
        };
    }

    /// `row` 行目の長さ（文字数）。
    fn len(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }
}

/// 文字数で数えた `col` を、`line` のバイト位置に直す。
///
/// **列を文字数で持つのは、非 ASCII の 1 文字を消すのに何バイト戻るかを呼び出し側に
/// 知らせないため**（01 の `TextBuffer`）。変換はここ 1 箇所で行う。行末の次は
/// `line.len()`。
fn byte_at(line: &str, col: usize) -> usize {
    line.char_indices()
        .nth(col)
        .map_or(line.len(), |(at, _)| at)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::app::Msg;
    use crate::git_read::LogPage;
    use crate::test_util::{fake_branch, fake_snapshot};

    /// 指定した区画にファイルを並べ、staged の先頭を選んだ状態。
    fn worktree(staged: &[&str], unstaged: &[&str], untracked: &[&str]) -> WorktreeState {
        WorktreeState {
            snapshot: fake_snapshot(staged, unstaged, untracked),
            section: WorktreeSection::Staged,
            ..WorktreeState::default()
        }
    }

    /// カーソルの指すパス。
    fn selected(worktree: &WorktreeState) -> Option<String> {
        worktree
            .selected()
            .map(|(_, entry)| entry.path.display().to_string())
    }

    #[test]
    fn typing_inserts_at_the_cursor_and_keeps_multibyte_characters_whole() {
        // 列は文字数で数える。バイト位置に直すのを間違えると、非 ASCII の途中に
        // 文字を差し込んで文字列が壊れる。
        let mut buf = TextBuffer::from_text("あい");
        buf.move_cursor(Motion::Left);
        buf.insert_char('X');

        assert_eq!(buf.text(), "あXい");
        assert_eq!(buf.cursor, (0, 2));
    }

    #[test]
    fn enter_splits_the_line_at_the_cursor() {
        let mut buf = TextBuffer::from_text("summary body");
        buf.cursor = (0, 7);
        buf.insert_newline();

        assert_eq!(buf.lines, vec!["summary".to_string(), " body".to_string()]);
        assert_eq!(buf.cursor, (1, 0));
    }

    #[test]
    fn backspace_at_the_start_of_a_line_joins_it_to_the_one_above() {
        let mut buf = TextBuffer::from_text("あい\nう");
        buf.cursor = (1, 0);
        buf.backspace();

        assert_eq!(buf.text(), "あいう");
        // つないだ位置に来る。バイト長で数えると全角のぶんずれる。
        assert_eq!(buf.cursor, (0, 2));
    }

    #[test]
    fn backspace_never_removes_the_last_line() {
        // `lines` が空になると `cursor` の指す行が無くなる。
        let mut buf = TextBuffer::default();
        buf.backspace();

        assert_eq!(buf.lines, vec![String::new()]);
        assert_eq!(buf.cursor, (0, 0));
    }

    #[test]
    fn delete_at_the_end_of_a_line_pulls_up_the_next_one() {
        let mut buf = TextBuffer::from_text("a\nb");
        buf.cursor = (0, 1);
        buf.delete();

        assert_eq!(buf.text(), "ab");

        // 最終行の末尾では何も起きない。
        buf.cursor = (0, 2);
        buf.delete();
        assert_eq!(buf.text(), "ab");
    }

    #[test]
    fn moving_down_to_a_shorter_line_clamps_the_column() {
        let mut buf = TextBuffer::from_text("long line\nab");
        buf.cursor = (0, 9);
        buf.move_cursor(Motion::Down);

        assert_eq!(buf.cursor, (1, 2));

        // **戻しても桁は覚えていない。** 覚える形にすると、編集でその桁が無効に
        // なる時点を決められない。
        buf.move_cursor(Motion::Up);
        assert_eq!(buf.cursor, (0, 2));
    }

    #[test]
    fn the_cursor_stops_at_the_ends_and_crosses_lines_sideways() {
        let mut buf = TextBuffer::from_text("ab\ncd");
        buf.cursor = (0, 0);

        // 先頭より前へは出ない。
        buf.move_cursor(Motion::Left);
        assert_eq!(buf.cursor, (0, 0));
        buf.move_cursor(Motion::Up);
        assert_eq!(buf.cursor, (0, 0));

        // 行末から右は次の行の先頭へ。
        buf.move_cursor(Motion::End);
        buf.move_cursor(Motion::Right);
        assert_eq!(buf.cursor, (1, 0));
        // 行頭から左は前の行の末尾へ。
        buf.move_cursor(Motion::Left);
        assert_eq!(buf.cursor, (0, 2));

        // 末尾より後ろへは出ない。
        buf.cursor = (1, 2);
        buf.move_cursor(Motion::Right);
        assert_eq!(buf.cursor, (1, 2));
        buf.move_cursor(Motion::Down);
        assert_eq!(buf.cursor, (1, 2));
        buf.move_cursor(Motion::Home);
        assert_eq!(buf.cursor, (1, 0));
    }

    #[test]
    fn pasted_text_goes_in_whole_with_its_line_breaks() {
        // **貼り付けは 1 つの `Msg` で来る。** キーに割り直すと、最初の改行で確定して
        // しまい、残りがペインのキーとして解釈される。
        let mut buf = TextBuffer::from_text("fix: ");
        buf.insert_text("直す\n\n本文 1\n本文 2");

        assert_eq!(buf.text(), "fix: 直す\n\n本文 1\n本文 2");
        // カーソルは貼り付けた末尾。
        assert_eq!(buf.cursor, (3, 4));
    }

    #[test]
    fn pasted_control_characters_are_dropped() {
        // CRLF のまま入れると行末に制御文字が残り、タブは `ui` が数える桁とずれて
        // カーソルの位置が合わなくなる（07 の「CommitMessage モーダル」）。
        let mut buf = TextBuffer::default();
        buf.insert_text("a\r\nb\tc");

        assert_eq!(buf.text(), "a\nbc");
    }

    #[test]
    fn pasting_in_the_middle_of_a_line_splits_it() {
        let mut buf = TextBuffer::from_text("ab");
        buf.cursor = (0, 1);
        buf.insert_text("X\nY");

        assert_eq!(buf.lines, vec!["aX".to_string(), "Yb".to_string()]);
        assert_eq!(buf.cursor, (1, 1));
    }

    #[test]
    fn a_buffer_of_only_whitespace_counts_as_blank() {
        // 空メッセージを断る判定に使う。`text()` が空かどうかでは、空行と空白だけの
        // 行を通してしまう。
        assert!(TextBuffer::default().is_blank());
        assert!(TextBuffer::from_text("  \n\t\n").is_blank());
        assert!(!TextBuffer::from_text("\n\nfix").is_blank());
    }

    #[test]
    fn the_worktree_cursor_crosses_sections_as_one_list() {
        let mut worktree = worktree(&["s.txt"], &["u.txt"], &["n.txt"]);

        assert_eq!(selected(&worktree).as_deref(), Some("s.txt"));
        worktree.move_cursor(1);
        assert_eq!(worktree.section, WorktreeSection::Unstaged);
        assert_eq!(selected(&worktree).as_deref(), Some("u.txt"));
        worktree.move_cursor(1);
        assert_eq!(worktree.section, WorktreeSection::Untracked);
        // 端では止まる。
        worktree.move_cursor(1);
        assert_eq!(selected(&worktree).as_deref(), Some("n.txt"));

        worktree.move_cursor(-1);
        assert_eq!(selected(&worktree).as_deref(), Some("u.txt"));
    }

    #[test]
    fn the_worktree_cursor_skips_empty_sections() {
        // 空の区画に入ると、何も選べない位置にカーソルが座る。
        let mut worktree = worktree(&["s.txt"], &[], &["n.txt"]);

        worktree.move_cursor(1);

        assert_eq!(worktree.section, WorktreeSection::Untracked);
        assert_eq!(selected(&worktree).as_deref(), Some("n.txt"));
    }

    #[test]
    fn a_worktree_without_files_has_nothing_selected() {
        let mut worktree = worktree(&[], &[], &[]);

        worktree.move_cursor(1);

        assert!(worktree.is_empty());
        assert_eq!(selected(&worktree), None);
    }

    #[test]
    fn clamp_moves_to_the_next_row_when_the_selected_file_leaves_the_section() {
        // unstaged の 2 件目を選んでいる状態で、その行が stage されて消えた。
        let mut worktree = worktree(&[], &["a.txt", "b.txt", "c.txt"], &[]);
        worktree.section = WorktreeSection::Unstaged;
        worktree.cursor = 1;

        worktree.snapshot.staged = vec![worktree.snapshot.unstaged.remove(1)];
        worktree.clamp();

        // 元の位置以降で最初に選べる行。添字は同じで中身が繰り上がる。
        assert_eq!(worktree.section, WorktreeSection::Unstaged);
        assert_eq!(selected(&worktree).as_deref(), Some("c.txt"));
    }

    #[test]
    fn clamp_falls_back_to_the_last_row_when_the_tail_is_gone() {
        let mut worktree = worktree(&[], &["a.txt", "b.txt"], &[]);
        worktree.section = WorktreeSection::Untracked;
        worktree.cursor = 3;

        worktree.clamp();

        assert_eq!(worktree.section, WorktreeSection::Unstaged);
        assert_eq!(selected(&worktree).as_deref(), Some("b.txt"));
    }

    #[test]
    fn clamp_keeps_a_still_valid_cursor() {
        let mut worktree = worktree(&["s.txt"], &["u.txt"], &[]);
        worktree.section = WorktreeSection::Unstaged;

        worktree.clamp();

        assert_eq!(selected(&worktree).as_deref(), Some("u.txt"));
    }

    #[test]
    fn clamp_resets_when_every_section_is_empty() {
        let mut worktree = worktree(&["s.txt"], &[], &[]);
        worktree.snapshot.staged.clear();

        worktree.clamp();

        assert_eq!(worktree.cursor, 0);
        assert_eq!(selected(&worktree), None);
    }

    /// コミットを 1 件積んだ状態。カーソルは WIP 行のまま。
    ///
    /// **`GraphLine` を手で組まない。** `LaneState::extend` を通らない `GraphRow` は
    /// `lane` も `cells` も実際には出てこない値になり、レーンや色を見るテストを
    /// 足した瞬間に「通るが現実と違う」状態になる。
    fn state_with_a_commit() -> AppState {
        let mut state = AppState::new(RepoInfo::default(), Config::default());
        let page = LogPage {
            commits: vec![crate::test_util::fake_commit(1, "summary")],
            done: true,
        };
        let count = state.config.graph.page_size;
        let _ = crate::test_util::reply(
            &mut state,
            Cmd::LoadLog {
                count,
                restart: false,
            },
            Msg::LogLoaded {
                restart: false,
                result: Ok(page),
            },
        );
        state
    }

    #[test]
    fn tab_cycles_only_through_the_implemented_panes() {
        // 未実装のペインに移れると、キーを受ける先の無い画面に入り込む。
        let mut state = AppState::new(RepoInfo::default(), Config::default());
        let candidates = state.focus_candidates();
        for _ in 0..candidates.len() {
            assert!(candidates.contains(&state.focus), "{:?}", state.focus);
            state.focus = state.next_focus();
        }
        // 一周して戻る。
        assert_eq!(state.focus, Pane::default());
        assert_eq!(state.prev_focus(), *candidates.last().unwrap());
    }

    #[test]
    fn the_right_pane_is_always_one_of_the_candidates() {
        // 候補は 2 本の定数、右ペインは `GraphSelection::right_pane` が決める。
        // 食い違うと、画面に出ているペインへ `Tab` で行けない（またはその逆）。
        let mut state = state_with_a_commit();
        for cursor in [0, 1] {
            state.graph.cursor = cursor;
            let right = state.graph.selection().right_pane();
            assert!(
                state.focus_candidates().contains(&right),
                "cursor={cursor} right={right:?} candidates={:?}",
                state.focus_candidates()
            );
        }
    }

    #[test]
    fn the_branches_pane_is_a_candidate_whatever_the_graph_selects() {
        // 右列の上に常に出ていて、左の選択で中身が変わらない（07 の「レイアウト」）。
        // 候補から外れると、画面に出ているペインへ `Tab` で行けない。
        let mut state = state_with_a_commit();
        for cursor in [0, 1] {
            state.graph.cursor = cursor;
            assert!(
                state.focus_candidates().contains(&Pane::Branches),
                "cursor={cursor} candidates={:?}",
                state.focus_candidates()
            );
        }
    }

    /// カーソルの指すブランチ名。
    fn cursor_name(branches: &BranchState) -> Option<&str> {
        branches.selected().map(|branch| branch.name.as_str())
    }

    #[test]
    fn a_loaded_branch_list_seats_the_cursor_on_the_head() {
        let mut branches = BranchState::default();

        branches.loaded(vec![
            fake_branch("another", false),
            fake_branch("main", true),
            fake_branch("topic", false),
        ]);

        assert_eq!(cursor_name(&branches), Some("main"));
        assert_eq!(branches.load(), BranchLoad::Loaded);
    }

    #[test]
    fn a_detached_head_seats_the_branch_cursor_on_the_first_row() {
        // detached / unborn ではどの行にも印が付かない（02 の「ブランチ一覧」）。
        let mut branches = BranchState::default();

        branches.loaded(vec![
            fake_branch("another", false),
            fake_branch("main", false),
        ]);

        assert_eq!(cursor_name(&branches), Some("another"));
    }

    #[test]
    fn a_reload_keeps_the_cursor_on_the_branch_by_name() {
        // 読み直しは commit のたびに来る（01 の `update::Reload`）。HEAD へ戻すだけだと、
        // 別のブランチを見ながら commit した人のカーソルが毎回飛ぶ。
        let mut branches = BranchState::default();
        branches.loaded(vec![fake_branch("main", true), fake_branch("topic", false)]);
        branches.move_cursor(1);
        assert_eq!(cursor_name(&branches), Some("topic"));

        // ブランチが 1 本増えて添字がずれる（一覧は名前順）。
        branches.loaded(vec![
            fake_branch("alpha", false),
            fake_branch("main", true),
            fake_branch("topic", false),
        ]);

        assert_eq!(cursor_name(&branches), Some("topic"));
    }

    #[test]
    fn a_reload_that_loses_the_selected_branch_falls_back_to_the_head() {
        // 消えた（削除 / リネーム）ときの行き先。
        let mut branches = BranchState::default();
        branches.loaded(vec![fake_branch("main", true), fake_branch("topic", false)]);
        branches.move_cursor(1);

        branches.loaded(vec![fake_branch("main", true)]);

        assert_eq!(cursor_name(&branches), Some("main"));
    }

    #[test]
    fn the_branch_cursor_stops_at_both_ends() {
        let mut branches = BranchState::default();
        branches.loaded(vec![fake_branch("a", false), fake_branch("b", false)]);

        branches.move_cursor(-1);
        assert_eq!(branches.cursor, 0);
        branches.move_cursor(1);
        branches.move_cursor(1);
        assert_eq!(branches.cursor, 1);
    }

    #[test]
    fn an_empty_branch_list_leaves_the_cursor_where_it_is() {
        // 選べるものが無い。`CommitFiles::move_cursor` と同じ。
        let mut branches = BranchState::default();

        branches.move_cursor(1);

        assert_eq!(branches.cursor, 0);
        assert_eq!(cursor_name(&branches), None);
    }

    #[test]
    fn a_failed_branch_list_keeps_what_was_already_there() {
        // 読めていた一覧は捨てない（`status_loaded` と同じ理由）。
        let mut branches = BranchState::default();
        branches.loaded(vec![fake_branch("main", true)]);

        branches.failed();

        assert_eq!(branches.load(), BranchLoad::Failed);
        assert_eq!(branches.branches.len(), 1);
    }

    #[test]
    fn a_display_row_past_the_end_is_none() {
        // 窓の切り出し（`panes::graph`）が末尾を跨ぐので、範囲外は `None` で返す。
        let state = state_with_a_commit();

        assert_eq!(state.graph.row(0), Some(GraphSelection::Wip));
        assert!(matches!(
            state.graph.row(1),
            Some(GraphSelection::Commit(_))
        ));
        assert_eq!(state.graph.row(2), None);
    }

    #[test]
    fn selecting_a_commit_takes_the_worktree_panes_out_of_the_cycle() {
        // コミットを選んだまま `focus == Pane::Worktree` でいられると、`s` / `u` / `x` が
        // 画面に出ていないファイルを stage する（01 の「Pane と Modal」）。
        let mut state = state_with_a_commit();
        assert!(state.focus_candidates().contains(&Pane::Worktree));

        state.graph.cursor = 1;

        assert!(!state.focus_candidates().contains(&Pane::Worktree));
        assert!(state.focus_candidates().contains(&Pane::CommitDetail));
        // `Diff` は残る。コミットのファイル一覧で選んだ 1 件を左に出すため。
        assert!(state.focus_candidates().contains(&Pane::Diff));
    }

    #[test]
    fn an_unchanged_worktree_takes_the_diff_out_of_the_cycle() {
        // **報告された形。** 変更が 1 つも無いと差分を出す相手がいないのに `Diff` が
        // 候補に残っていて、`Tab` でグラフが空の Diff に差し替わっていた。
        let mut state = state_with_a_commit();
        state.worktree.snapshot = fake_snapshot(&[], &[], &[]);

        assert!(!state.focus_candidates().contains(&Pane::Diff));
        // ほかの候補は残る。巡回そのものを止めるわけではない。
        assert!(state.focus_candidates().contains(&Pane::Worktree));

        // 1 件でも差分で表せる行があれば戻る。
        state.worktree.snapshot = fake_snapshot(&[], &["a.txt"], &[]);
        assert!(state.focus_candidates().contains(&Pane::Diff));
    }

    #[test]
    fn a_focus_that_leaves_the_cycle_is_reseated_on_the_graph() {
        let mut state = state_with_a_commit();
        state.focus = Pane::Worktree;

        state.graph.cursor = 1;
        state.reseat_focus();

        assert_eq!(state.focus, Pane::Graph);
    }

    #[test]
    fn a_focus_that_is_still_a_candidate_is_left_alone() {
        // WIP 行に戻るだけで右ペインのフォーカスを奪わない。
        let mut state = state_with_a_commit();
        state.focus = Pane::Worktree;

        state.reseat_focus();

        assert_eq!(state.focus, Pane::Worktree);
    }

    #[test]
    fn the_wip_row_is_a_display_row_of_its_own() {
        // 表示行 0 は WIP 行、1.. が `lines`（01 の「グラフの行」）。
        let mut state = state_with_a_commit();
        assert_eq!(state.graph.rows(), 2);
        assert_eq!(state.graph.selection(), GraphSelection::Wip);
        assert!(state.graph.cursor_commit().is_none());

        state.graph.cursor = 1;

        let commit = state
            .graph
            .cursor_commit()
            .expect("コミットを指していません");
        assert_eq!(commit.summary, "summary");
    }

    #[test]
    fn the_cursor_stops_at_the_wip_row_and_at_the_last_commit() {
        let mut state = state_with_a_commit();

        state.graph.move_cursor(-1);
        assert_eq!(state.graph.cursor, 0);

        state.graph.move_cursor(5);
        assert_eq!(state.graph.cursor, 1);
    }

    #[test]
    fn an_empty_graph_still_has_the_wip_row() {
        // 行が 1 つも無くても WIP 行は選べる（07 の「Graph ペイン」）。
        let mut state = AppState::new(RepoInfo::default(), Config::default());
        assert_eq!(state.graph.rows(), 1);

        state.graph.move_cursor(3);

        assert_eq!(state.graph.cursor, 0);
        assert_eq!(state.graph.selection(), GraphSelection::Wip);
    }

    #[test]
    fn push_notice_drops_the_oldest_beyond_the_cap() {
        let mut state = AppState::new(RepoInfo::default(), Config::default());
        for i in 0..MAX_NOTICES + 5 {
            state.push_notice(NoticeLevel::Info, format!("notice {i}"), None);
        }

        assert_eq!(state.notices.len(), MAX_NOTICES);
        // 最新は残り、最古から捨てられる。
        assert_eq!(
            state.notices.back().map(|n| n.message.as_str()),
            Some(format!("notice {}", MAX_NOTICES + 4).as_str())
        );
        assert_eq!(
            state.notices.front().map(|n| n.message.as_str()),
            Some("notice 5")
        );
    }

    #[test]
    fn push_notice_keeps_level_and_detail() {
        let mut state = AppState::new(RepoInfo::default(), Config::default());
        state.push_notice(
            NoticeLevel::Error,
            "失敗".to_string(),
            Some("git push origin main".to_string()),
        );

        let notice = state.notices.back().unwrap();
        assert_eq!(notice.level, NoticeLevel::Error);
        assert_eq!(notice.detail.as_deref(), Some("git push origin main"));
    }

    #[test]
    fn text_buffer_round_trips() {
        let buf = TextBuffer::from_text("first\nsecond");
        assert_eq!(buf.lines, ["first", "second"]);
        assert_eq!(buf.cursor, (1, 6));
        assert_eq!(buf.text(), "first\nsecond");
    }

    #[test]
    fn default_text_buffer_has_one_empty_line() {
        // cursor が指す行が必ず存在する必要がある。
        let buf = TextBuffer::default();
        assert_eq!(buf.lines, [""]);
        assert_eq!(buf.cursor, (0, 0));
        assert_eq!(buf.text(), "");
    }

    fn job(kind: JobKind) -> Job {
        Job::new(kind, "test")
    }

    #[test]
    fn job_tracker_rejects_a_second_job_of_the_same_kind() {
        let mut jobs = JobTracker::default();
        assert!(jobs.is_empty());

        assert!(jobs.start(job(JobKind::LoadStatus)));
        assert!(jobs.is_running(JobKind::LoadStatus));
        // 同種の Cmd を二重に投げないための判定。
        assert!(!jobs.start(job(JobKind::LoadStatus)));
        assert_eq!(jobs.iter().count(), 1);
    }

    #[test]
    fn job_tracker_allows_different_kinds() {
        let mut jobs = JobTracker::default();
        assert!(jobs.start(job(JobKind::LoadStatus)));
        assert!(jobs.start(job(JobKind::FetchPrList)));
        assert_eq!(jobs.iter().count(), 2);
    }

    #[test]
    fn job_tracker_finish_makes_room_again() {
        let mut jobs = JobTracker::default();
        jobs.start(job(JobKind::LoadLog));

        jobs.finish(JobKind::LoadLog);
        assert!(!jobs.is_running(JobKind::LoadLog));
        assert!(jobs.is_empty());
        assert!(jobs.start(job(JobKind::LoadLog)));
    }

    #[test]
    fn region_all_lists_every_arm() {
        // **`JobTracker.epochs` の幅は `Region::ALL` の長さ。** 腕を足してここに
        // 足し忘れると、その領域の `advance` / `epoch` が添字の範囲外で落ちる。
        // 網羅 `match` にしてあるので、腕を足すとこの関数がコンパイルエラーになる
        // （そのとき `ALL` にも足す）。
        fn listed(region: Region) -> bool {
            let name = match region {
                Region::Worktree => "worktree",
                Region::Refs => "refs",
                Region::Log => "log",
            };
            !name.is_empty()
        }

        let mut jobs = JobTracker::default();
        for (index, region) in Region::ALL.into_iter().enumerate() {
            assert!(listed(region));
            assert_eq!(region as usize, index, "{region:?} の添字がずれています");
            jobs.advance(region);
            assert_eq!(jobs.epoch(region), 1);
        }
    }

    #[test]
    fn a_kind_without_a_region_is_always_fresh() {
        // `Exec` やコミット詳細は書き込みで古くならない（詳細は Oid で追いつく）。
        // ここが偽だと、追いつきを持たない種別に頼み直しを回させる腕ができる。
        let mut jobs = JobTracker::default();
        assert!(jobs.is_fresh(JobKind::Exec, None));
        jobs.advance(Region::Worktree);
        assert!(jobs.is_fresh(JobKind::LoadCommitDetail, None));
        // 領域を持つ種別は、刻印が無ければ「古い」。
        assert!(!jobs.is_fresh(JobKind::LoadStatus, None));
    }

    #[test]
    fn finishing_an_idle_kind_is_a_no_op() {
        let mut jobs = JobTracker::default();
        jobs.start(job(JobKind::LoadLog));

        jobs.finish(JobKind::FetchPrList);
        assert!(jobs.is_running(JobKind::LoadLog));
    }
}
