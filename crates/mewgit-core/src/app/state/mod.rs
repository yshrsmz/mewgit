//! アプリ全体の状態。設計は `docs/design/01-app-core.md`。
//!
//! `ui` はここにある値を描画するだけで、更新は [`update`](crate::app::update) が行う。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use crate::auth::url::RemoteUrl;
use crate::auth::{AccountSource, AccountState, DegradedReason, GhLogin, RepoTarget};
use crate::git_exec::{GitOp, RebasePlan};
use crate::git_read::{
    Branch, DiffTarget, FileDiff, FileEntry, Oid, PathStamp, Remote, RepoFingerprint, Timestamp,
    WorktreeSection, WorktreeSnapshot,
};
use crate::github::{PullRequest, RepoRef};

use crate::config::Config;

use super::keys::{self, Action, CommonAction};
use super::{Cmd, Keymap, ProgressMenu};

mod branches;
mod graph;
mod jobs;
mod prs;
mod stash;

pub use branches::BranchState;
pub use graph::{CommitFiles, GraphLine, GraphSelection, GraphState};
pub use jobs::{GithubJob, Job, JobKind, JobTracker, Region};
pub use prs::{PrHead, PrHeads, PrLoad, PrState};
pub use stash::StashState;

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
    pub stashes: StashState,
    pub prs: PrState,
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
    /// 外の変更の見張り（01 の「外部の変更の検知」）。
    pub watch: WatchState,
    /// エラー・情報のトースト。積むのは [`AppState::push_notice`]。
    pub notices: VecDeque<Notice>,
    /// キーの表。既定の表に `config.keymap` を当てたもの（08 の「キーマップの解決」）。
    /// 案内（フッター・`?` の一覧）も dispatch もここを引く。
    pub keymap: Keymap,
    pub term: TermSize,
    /// [`Msg::Tick`] が来た回数。スピナーの駒を選ぶのに使う（07 のヘッダ）。
    ///
    /// **時刻ではなく回数を持つ。** `ui` は `AppState` を描くだけなので、駒を時刻から
    /// 決めると描画のたびに時計を読むことになり、`TestBackend` で「回った」ことを
    /// 見張れない。`wrapping_add` で回すので溢れても止まらない。
    ///
    /// [`Msg::Tick`]: crate::app::Msg::Tick
    pub tick: u64,
    /// いまの時刻。通知に刻む（01 の「通知」）。
    ///
    /// **`update` は時計を読まない。** 読むのはランタイムで、起動時に
    /// [`AppState::new`] へ渡し、以後は [`Msg::Tick`] に載せて運ぶ。そのぶん刻まれる
    /// 時刻は最大 1 周期（250ms）古い。
    ///
    /// [`Msg::Tick`]: crate::app::Msg::Tick
    pub now: Timestamp,
    /// 起動時に確定した設定。実行中は再読込しない（08）。
    pub config: Config,
}

/// 外の変更の見張り（01 の「外部の変更の検知」）。
///
/// **期限は `update` が [`Msg::Tick`] の時刻で決める。** ランタイムに別のタイマーを
/// 持たせると、期限の判断がテストできない側に出る。
///
/// [`Msg::Tick`]: crate::app::Msg::Tick
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchState {
    /// 比べる基準の指紋。**`None` は「次の返りを基準にするだけで、比べない」** ——
    /// 起動直後と、mewgit の書き込みのあと（書き込みで指紋は必ず変わるので、比べると
    /// 自分の書き込みを外の変更と読む）。
    pub fingerprint: Option<RepoFingerprint>,
    /// probe が失敗したまま、まだ成功していない。通知を最初の 1 回に抑える
    /// （01 の「失敗の記憶」の形）。
    pub failed: bool,
    /// 次に probe を出す時刻（Unix 秒）。
    pub next_probe: i64,
    /// 次に定期の status を出す時刻（Unix 秒）。
    pub next_status: i64,
}

impl WatchState {
    /// probe の間隔（秒）。lazygit の `externalChangeCheckInterval` と同じ（01）。
    pub const PROBE_INTERVAL: i64 = 2;

    /// 起動時の期限。**どちらも 1 周期先から数える** —— 起動時は `init` が status と
    /// probe を自分で頼むので、最初の `Tick` で同じものをもう一度頼まない。
    fn starting_at(now: Timestamp, config: &Config) -> Self {
        let mut watch = Self::default();
        watch.restart_probe(now);
        watch.restart_status(now, config);
        watch
    }

    /// 定期の status の間隔（秒）。`general.refresh_interval` が 0 なら止める（08）。
    pub(crate) fn status_interval(config: &Config) -> Option<i64> {
        match config.general.refresh_interval {
            0 => None,
            seconds => Some(i64::try_from(seconds).unwrap_or(i64::MAX)),
        }
    }

    /// 次の probe を `now` から 1 周期先にする。
    pub(crate) fn restart_probe(&mut self, now: Timestamp) {
        self.next_probe = now.seconds.saturating_add(Self::PROBE_INTERVAL);
    }

    /// 期限が `now` から 1 周期より先にあれば、1 周期先へ引き戻す。**時計が後ろへ
    /// 跳んだとき**、期限が跳んだぶんだけ先に残って見張りが止まるのを塞ぐ。
    pub(crate) fn clamp_to(&mut self, now: Timestamp, config: &Config) {
        let probe = now.seconds.saturating_add(Self::PROBE_INTERVAL);
        self.next_probe = self.next_probe.min(probe);
        if let Some(interval) = Self::status_interval(config) {
            self.next_status = self.next_status.min(now.seconds.saturating_add(interval));
        }
    }

    /// 次の定期の status を `now` から 1 周期先にする。止めてあれば何もしない。
    pub(crate) fn restart_status(&mut self, now: Timestamp, config: &Config) {
        if let Some(interval) = Self::status_interval(config) {
            self.next_status = now.seconds.saturating_add(interval);
        }
    }
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
pub(super) const MAX_NOTICES: usize = 100;

impl AppState {
    /// 開いたリポジトリと設定から初期状態を作る。
    /// **`now` を取るのは、最初の `Tick` より前にも通知が積まれるから**（`main.rs` の
    /// 設定の警告）。時計の無い状態を作れると、その通知だけが時刻を持たない。
    pub fn new(repo: RepoInfo, config: Config, now: Timestamp) -> Self {
        // **解くのはここ**（08）。`config` は最下段（00）なので `app::Key` を名指しできない。
        let (keymap, warnings) = Keymap::new(&config.keymap);
        let mut state = Self {
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
            stashes: StashState::default(),
            prs: PrState::default(),
            focus: Pane::default(),
            right_tab: RightTab::default(),
            modal: None,
            jobs: JobTracker::default(),
            watch: WatchState::starting_at(now, &config),
            notices: VecDeque::new(),
            keymap,
            term: TermSize::default(),
            tick: 0,
            now,
            config,
        };
        for warning in warnings {
            state.push_notice(NoticeLevel::Warn, warning.to_string(), None);
        }
        state
    }

    /// `update` が返す [`Cmd`] を発行してよい形にする。
    ///
    /// 非同期ジョブを生む `Cmd` は [`JobTracker`] に登録し、[`refusal`](Self::refusal) が
    /// 断るものは代わりの `Cmd` に差し替える。判定をここ（`update` の出口）に置くのは、
    /// ランタイムの `dispatch` が `AppState` に書いてよいのが `notices` だけだから
    /// （01 のランタイム）。
    pub fn issue(&mut self, cmd: Cmd) -> Cmd {
        match cmd {
            // `Batch` の中身は 1 つずつ判定する。まとめて弾くと、同種が走っている
            // `Cmd` 1 つのために他の `Cmd` まで落ちる。
            Cmd::Batch(cmds) => {
                let issued: Vec<Cmd> = cmds.into_iter().map(|cmd| self.issue(cmd)).collect();
                Cmd::batch(issued)
            }
            cmd => match self.refusal(&cmd) {
                Some(instead) => self.issue(instead),
                None => {
                    if let Some((kind, label)) = cmd.job() {
                        // 走っていないことは `refusal` が確かめた。
                        let _ = self.jobs.start(Job::new(kind, label, self.tick));
                    }
                    cmd
                }
            },
        }
    }

    /// `op` の前提（進行中操作が無い・持っている値が新しい・stash できる中身がある）が
    /// 崩れていて断るなら、
    /// 理由を通知に積んで代わりに出す `Cmd` を返す。[`refusal`](Self::refusal) のうち
    /// **git が走っているか以外の全部**。
    ///
    /// **入力と確認を開く前にもこれを問う**（`update::input`）。門を足すときはここに
    /// 足せば両方に効く —— 開く前の判定を別に並べると、片方にだけ足した門で打った
    /// 本文が消える。
    pub(crate) fn refuse_write(&mut self, op: &GitOp) -> Option<Cmd> {
        if self.refuse_in_progress(op) {
            // 続きを頼む: 進行中操作で断った書き込みの代わりの status の読み直し。
            return Some(Cmd::LoadStatus);
        }
        // 持っている値の鮮度と中身に頼る書き込み（stash の index、push の守り、
        // unmerged / rename の断り）。
        self.refuse_by_held_values(op)
    }

    /// `cmd` を [`issue`](Self::issue) が断るなら、理由を通知に積んで**代わりに出す
    /// `Cmd`** を返す。通すなら `None`。
    ///
    /// **打った本文を持つモーダルの確定（`update::submit`）も同じ関数で問う** ——
    /// 断られると分かっていれば閉じずに断れる。判定を写すと、片方にだけ足した
    /// 断りで本文が消える。**進行中操作を先に診る** —— git の実行が終わっても
    /// 断られるので、待てと言うより出口を言うほうが先に要る。
    ///
    /// **進行中操作で断ったら status を読み直す。** 世代を進めるのは mewgit の書き込み
    /// だけなので、端末で rebase を終えても snapshot は「鮮度あり・途中」のまま残る。
    /// 断ると git を起動しないので、読み直さないと断り続ける（01 の「進行中操作が
    /// あるときの書き込み」）。
    pub(crate) fn refusal(&mut self, cmd: &Cmd) -> Option<Cmd> {
        if let Cmd::Exec(op) = cmd {
            if let Some(instead) = self.refuse_write(op) {
                return Some(instead);
            }
        }
        match cmd.job_kind() {
            Some(kind) if self.jobs.is_running(kind) => {
                self.notify_dropped(kind);
                Some(Cmd::None)
            }
            _ => None,
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
            | JobKind::LoadCommitMessage
            | JobKind::LoadRepoInfo
            | JobKind::LoadStatus
            | JobKind::LoadStashes
            | JobKind::LoadDiff
            | JobKind::ResolveTarget
            | JobKind::ResolveAccount
            | JobKind::ListAccounts
            | JobKind::FetchPrList
            | JobKind::FetchPrDetails
            | JobKind::Probe => return,
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
        // 上限で捨てても最新は残るので、最新の次の番号で重ならない。
        let seq = self.notices.back().map_or(0, |last| last.seq + 1);
        self.notices.push_back(Notice {
            seq,
            level,
            message,
            detail,
            at: self.now,
            dismissed: false,
        });
        while self.notices.len() > MAX_NOTICES {
            self.notices.pop_front();
        }
    }

    /// 履歴で `selected`（[`Notice::seq`]）が描かれる行。新しい順で 0 が最新。
    ///
    /// **捨てられた通知を選んでいたら最古の行に寄せる**（01 の「通知」）。`None` は
    /// 通知が 1 件も無いときだけ。**描く側（`ui`）と `j` / `k` がこの 1 本を読む** ——
    /// 寄せ方を 2 箇所に書くと、描いている行と動かす起点が食い違う。
    pub fn notice_log_row(&self, selected: u64) -> Option<usize> {
        let last = self.notices.len().checked_sub(1)?;
        let row = self
            .notices
            .iter()
            .rev()
            .position(|notice| notice.seq == selected);
        Some(row.unwrap_or(last))
    }

    /// ステータスバーに出ている通知。
    ///
    /// 出すのは最新の 1 件で、**キー入力で下ろしたら出さない**（01 の「通知」）。
    /// 下ろした旗は通知ごとに持つので、新しい通知が積まれればまた出る。
    pub fn shown_notice(&self) -> Option<&Notice> {
        self.notices.back().filter(|notice| !notice.dismissed)
    }

    /// 出ている通知を下ろす。キー入力と貼り付けの入口で呼ぶ（01 の「通知」）。
    ///
    /// **そのキーが出す通知より先に呼ぶ。** 後に呼ぶと、押した結果の警告
    /// （「変更のないファイルは stage できません」）がその場で消えて読めない。
    pub fn dismiss_notice(&mut self) {
        if let Some(notice) = self.notices.back_mut() {
            notice.dismissed = true;
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
            // **見るのは `diff_candidate()` で、`wanted_diff()` ではない。** あちらは
            // `focus != Pane::Diff` で `None` を返すので、入る前は必ず `None`（入れるかを
            // 問う相手にならない）。**stash の diff を出しているあいだは `Some`** なので
            // 外れない（01 の「Stash ペイン」）—— 外すと、変更の無いワーキングツリーで
            // stash を開いた瞬間に `reseat_focus` が Graph へ戻す。
            //
            // **末尾で照合する。** 添字で削ると、並びを変えたときに別のペインが落ちる。
            GraphSelection::Wip if self.diff_candidate().is_none() => {
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

    /// `pane` へ移れない理由。候補に入っていれば `None`（07 の「ペインとフォーカス」の
    /// `1`〜`5`）。
    ///
    /// **候補を決める規則の隣に置く。** 落とす条件（[`pick`](Self::pick) と `Diff` の
    /// 絞り）を足す人がここも直せるように —— 呼ぶ側が理由を決め打ちすると、条件が
    /// 増えたときに別の理由へ「GitHub 機能が無効」と言う。
    pub fn focus_refusal(&self, pane: Pane) -> Option<String> {
        if self.focus_candidates().contains(&pane) {
            return None;
        }
        Some(match pane {
            // `pick` が `Prs` を落とすのは GitHub 機能を切ったときだけ。
            Pane::Prs => GithubBlock::AccountDisabled.notice(self),
            // 選択に合わない右下と、対象の無い `Diff`。どちらも番号では選べない
            // （`5` は `right_pane` が候補と同じ選択から決め、`Diff` に番号は無い）。
            _ => "そのペインはいま出ていません".to_string(),
        })
    }

    /// 巡回の次。末尾からは先頭に戻る。
    ///
    /// **候補に無い `focus` は先頭から数え直すが、そこへ来る経路は無い。**
    /// [`AppState::reseat_focus`] が `update` の出口で候補から外れた `focus` を潰すので、
    /// ここで読む `focus` は必ず候補にいる。`cycled` の数え直しはその保険で、
    /// 座り直す先を変えたときにここも直す必要はない（先頭は常に `Graph`）。
    pub fn next_focus(&self) -> Pane {
        cycled(self.focus_candidates(), self.focus, 1)
    }

    /// 巡回の前。先頭からは末尾に戻る（候補に無い `focus` は [`AppState::next_focus`] と同じ）。
    pub fn prev_focus(&self) -> Pane {
        cycled(self.focus_candidates(), self.focus, -1)
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
    ///
    /// **Diff ペインを離れたら [`DiffState::source`] も戻す**（01 の「Stash ペイン」）。
    /// 離れる経路は `Esc` / `Tab` / `Shift+Tab` / 座り直しと複数あり、経路ごとに書くと
    /// 足し忘れが静かに通る —— 残ると、`Tab` で入り直したときにグラフの選択ではなく
    /// 前に開いた stash の diff が出る。**座り直しより後に見る**（座り直しも離れる経路）。
    pub fn reseat_focus(&mut self) {
        if !self.focus_candidates().contains(&self.focus) {
            self.focus = Pane::Graph;
        }
        if self.focus != Pane::Diff {
            self.diff.source = DiffSource::Selection;
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
        self.diff_candidate()
    }

    /// Diff ペインに入ったら出すもの。`focus` を問わない（[`wanted_diff`](Self::wanted_diff)
    /// の中身で、巡回の候補もここを見る）。
    fn diff_candidate(&self) -> Option<DiffTarget> {
        match self.diff_origin() {
            DiffOrigin::Worktree => self.wanted_worktree_diff(),
            // **`path: None`（コミット全体）は出さない。** 一覧を出すためだけに
            // 全ファイルのハンクを組むことになる。一覧は `Cmd::LoadCommitFiles`
            // の担当（01 の「コミットのファイル一覧」）。
            DiffOrigin::Commit(oid) => Some(DiffTarget::Commit {
                oid,
                path: Some(self.graph.selected_file()?.path.clone()),
            }),
            // **stash は全体を出す**（01 の「Stash ペイン」）。stash のファイル一覧を
            // 別に出す場所が無く、Diff ペインが全ファイルを見出し付きで並べる（07）。
            DiffOrigin::Stash(oid) => Some(DiffTarget::Stash(oid)),
        }
    }

    /// Diff ペインの出どころ（01 の「Stash ペイン」）。
    ///
    /// **[`DiffSource`] と グラフの選択を 1 つに解く。** Diff ペインを読む側
    /// （出す diff・巡回の候補・`Esc` の戻り先・`s` / `u` / `x` の可否）はどれもこれを
    /// 網羅 `match` する —— 各所で `DiffSource` とグラフの選択を 2 段で見る形にすると、
    /// 出どころを足したときに**コンパイラが何も言わず**、段の順序（stash を先に見る）も
    /// 各所で守ることになる。
    pub fn diff_origin(&self) -> DiffOrigin {
        match (self.diff.source, self.graph.selection()) {
            (DiffSource::Stash(oid), _) => DiffOrigin::Stash(oid),
            (DiffSource::Selection, GraphSelection::Wip) => DiffOrigin::Worktree,
            (DiffSource::Selection, GraphSelection::Commit(line)) => {
                DiffOrigin::Commit(line.commit.oid)
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
    /// 黙って嘘になる形でもある（`.claude/rules/rust-coding.md`）。
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
    /// （`.claude/rules/rust-coding.md` の「旗と値を割らない」）。
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
    /// **状態に持たない。** `prs.list()` と `repo`、ブランチ一覧の HEAD の行から毎回導く ——
    /// 写しを置くと、
    /// どちらかが更新されたときに片方だけ古くなる。
    pub fn current_pr(&self) -> Option<&PullRequest> {
        current_row(&self.repo, &self.branches, self.prs.list())
    }

    /// PR 作成の `head` の候補（01 の「head の組み立て」）。ブランチ一覧に HEAD の行が
    /// 無ければ（detached / unborn / まだ届いていない）空。
    ///
    /// **[`head_homes`] とは読み手が違う。** あちらは「PR の head が**在りうる**
    /// リポジトリ」を返す寛容な照合（座り直し）で、追跡先（何を基にしたか）も入れる。
    /// こちらは**送る値の候補**なので、「そこに在る」と言える材料だけを使う（#101）。
    ///
    /// **候補は 2 つの材料から組む。**
    /// - 同名の remote-tracking ref が在るリモート（`graph.refs` の
    ///   [`RefIndex::remote_oid`](crate::git_read::RefIndex::remote_oid)）。在ることが「そのリモートにそのブランチが在った」の
    ///   証拠で、`branch.<name>.remote`（何を基にしたか）と違って push の結果そのものが
    ///   残る。**名前はリモートの一覧から組む**（ref 名から切り出すと `/` を含む
    ///   リモート名で割り方が決まらない。02）。索引は HEAD の行と同じ
    ///   `Msg::RepoInfoLoaded` で届くので、2 つのあいだに順序は無い
    /// - 追跡設定の URL（`branch.<name>.remote` が URL のとき）。`gh pr checkout` と
    ///   mewgit の取り出しは fork の URL を書き、**remote-tracking ref を作らない**（05）。
    ///   **ブランチ名はローカルの名前ではない**（[`RepoInfo::url_tracked`]）
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
        let Some(Branch { name, oid, .. }) = self.branches.head_branch() else {
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
        // 名前の追跡先は「何を基にしたか」で、在る証拠にならない。在るなら 1 つ目の
        // 材料（ref）で拾えている。
        let from_url = self
            .repo
            .url_tracked(&self.branches)
            .and_then(|(url, branch)| Some((owner_of(url)?, branch.to_string())));

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
    ///
    /// **世代に加えて、ファイルの `(mtime, len)` も比べる**（[`DiffView::stamp`]）。
    /// 外の編集は世代を進めないので、刻印だけでは古い diff を最新と読む。
    pub fn diff_is_current(&self) -> bool {
        let wanted = self.wanted_diff();
        self.diff.fresh_view(wanted.as_ref()).is_some_and(|view| {
            self.jobs.is_fresh(JobKind::LoadDiff, Some(view.epoch))
                && view.stamp == self.diff_stamp(&view.target)
        })
    }

    /// `target` が読むファイルの、持っている snapshot の指紋（`WorktreeSnapshot.stamps`）。
    ///
    /// **ワークツリーか index を読む対象だけ。** コミットと stash の中身はオブジェクトで、
    /// 外の編集では変わらない。`HeadVsIndex` はワークツリーを読まないが、指紋を丸ごと
    /// 比べる —— ワークツリーだけの編集で 1 回余分に読み直すが、index 側だけを比べる
    /// 関数を別に持つより安い。
    pub fn diff_stamp(&self, target: &DiffTarget) -> Option<PathStamp> {
        self.worktree
            .snapshot
            .stamps
            .get(target.live_path()?)
            .copied()
    }

    /// 持っている worktree の一覧を**直近の書き込みより後に**読めているか。
    ///
    /// 一度も読めていなければ偽（`snapshot_epoch` が `None`）。commit が「stage された
    /// 変更がありません」を出してよいかの根拠（01 の「commit」）。
    pub fn worktree_is_current(&self) -> bool {
        self.jobs
            .is_fresh(JobKind::LoadStatus, self.worktree.snapshot_epoch)
    }

    /// 持っている stash の一覧を**直近の書き込みより後に**読めているか
    /// （[`worktree_is_current`](Self::worktree_is_current) の stash 版）。
    ///
    /// apply / pop / drop は `stash@{index}` で渡すので、古い一覧の index は別の stash を
    /// 指しうる（01 の「stash の書き込み」）。
    pub(crate) fn stashes_are_current(&self) -> bool {
        self.jobs
            .is_fresh(JobKind::LoadStashes, self.stashes.epoch())
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

    /// HEAD のブランチの追跡先が URL のとき、その URL と **fork でのブランチ名**
    /// （`merge` から `refs/heads/` を外したもの）。
    ///
    /// **追跡設定は `branches` の HEAD の行から読む**（理由は `head_homes` の doc）。
    ///
    /// **名前は `merge` から取る。** URL を書くのは PR の取り出し（mewgit と
    /// `gh pr checkout`）と `git push -u <URL> <local>:<remote>` で、どれもローカルと
    /// 違う名前を `merge` に書きうる（取り出しは衝突を避けてローカルを `bob/main` に
    /// する。01 の「現ブランチの PR」の「比べる名前」）。**読み方を 1 本にする** ——
    /// 座り直し（[`head_homes`]）と送る側（[`AppState::head_candidates`]）が別々に
    /// 書くと、片方だけ直ったときに「取り出した PR に座らない」と「fork に無い
    /// ブランチを送る」が片側だけで起きる。`refs/heads/` の下に無い `merge` は
    /// ブランチではないので `None`（取り出しは URL と一緒には書かない。05）。
    pub(super) fn url_tracked<'a>(
        &'a self,
        branches: &'a BranchState,
    ) -> Option<(&'a str, &'a str)> {
        let upstream = branches.head_branch()?.upstream_config.as_ref()?;
        let Tracked::Url(url) = self.tracked(&upstream.remote) else {
            return None;
        };
        Some((url, upstream.merge.strip_prefix("refs/heads/")?))
    }
}

/// 一覧のうち、いま居るブランチの PR（01 の「現ブランチの PR」）。
///
/// **[`AppState::current_pr`] と [`PrState::loaded`] の両方がここを通る。** 座り直す
/// 先を決める側と描く側で照合の規則が割れると、カーソルが座っていない行を
/// 「現ブランチの PR」として扱える。
///
/// **fork のブランチも当てる。** 照合する相手（リポジトリと、そこでのブランチ名）は
/// [`head_homes`] が確からしい順に出す（`branch.<HEAD>.remote` の URL → `target`）。
///
/// **到着順に依らない。** 材料の HEAD の行（`branches`）と `remotes` はどちらも
/// [`Msg::RepoInfoLoaded`](super::Msg::RepoInfoLoaded) に乗って**同時に**届き、
/// `target` はその返信から始まる鎖（`Cmd::ResolveTarget` → `Cmd::ResolveAccount` →
/// 一覧）の途中なので、**一覧が届いた時点でどれも揃っている**（経緯は 02 の「head」）。
/// **順序を回すテストは要らない**。
pub(super) fn current_row<'a>(
    repo: &RepoInfo,
    branches: &BranchState,
    list: &'a [PullRequest],
) -> Option<&'a PullRequest> {
    // **リポジトリ名まで見る**（`RepoRef` 丸ごとの比較）。owner だけだと、同じ owner の
    // 下に作った fork（`acme/app` → `acme/app-fork`）から出た他人の PR が
    // 「現ブランチの PR」に当たる。
    head_homes(repo, branches)
        .into_iter()
        .find_map(|(home, branch)| {
            list.iter().find(|pr| {
                pr.head.branch == branch
                    && pr
                        .head
                        .repo
                        .as_ref()
                        .is_some_and(|repo| repo.same_as(&home))
            })
        })
}

/// HEAD のブランチから出した PR の head が在りうるリポジトリと、そこでのブランチ名を、
/// **確からしい順**に返す（01 の「現ブランチの PR」）。ブランチ一覧に HEAD の行が
/// 無ければ空（detached にも unborn にも PR は無い）。
///
/// **名前は、追跡先が URL の腕だけ `merge` から取る**（[`RepoInfo::url_tracked`]）。
/// 名前の追跡先と `target` の腕はローカルの名前のまま —— 理由は 01 の「比べる名前」。
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
/// **HEAD の行は引数に取らない。** 名前も追跡設定も `branches` の HEAD の行
/// （[`BranchState::head_branch`]）から導けるので、呼ぶ側に選ばせると対応の取れない組
/// （HEAD でない行）が型で組める（`.claude/rules/rust-coding.md` の「導出できる値を
/// 引数に取らせない」）。
///
/// **読めない追跡設定は候補にしない**（設定が無い / `.`（ローカル追跡）/ リモートが
/// 消えている / URL が GitHub の形でない / URL の `merge` がブランチでない）。
/// **理由は分けない**：呼び出し側の扱いは
/// どれも同じで、**出す先も無い**（座る行が変わるだけで通知は出さない）。
///
/// **host は見ない。** [`RepoTarget::host`] は `ssh -G` で正規化済みだが、こちらは
/// URL に書かれたままなので、生のまま比べると **fork だけ別のエイリアスを使う
/// 多アカウント運用**（04）で逆に落ちる。残る穴は、別ホストの同名 `owner/repo` に
/// 同名のブランチがあり、その PR が一覧に居るとき（01）。
fn head_homes<'a>(repo: &'a RepoInfo, branches: &'a BranchState) -> Vec<(RepoRef, &'a str)> {
    let Some(Branch {
        name,
        upstream_config,
        ..
    }) = branches.head_branch()
    else {
        return Vec::new();
    };
    // 名前で引ければその URL、URL ならそのもの。
    let tracked = match upstream_config
        .as_ref()
        .map(|upstream| repo.tracked(&upstream.remote))
    {
        Some(Tracked::Named(remote)) => Some((remote.url.as_str(), name.as_str())),
        _ => repo.url_tracked(branches),
    };
    let tracked = tracked.and_then(|(url, branch)| {
        let url = RemoteUrl::parse(url).ok()?;
        Some((
            RepoRef {
                owner: url.owner,
                repo: url.repo,
            },
            branch,
        ))
    });
    let target = repo.target.resolved().map(|target| {
        (
            RepoRef {
                owner: target.owner.clone(),
                repo: target.repo.clone(),
            },
            name.as_str(),
        )
    });
    tracked.into_iter().chain(target).collect()
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
    /// **理由は持たない。** 読み手（`A` と `r` の通知、PRs ペイン）はどれも 2 つを分けずに
    /// 同じ事実を出すので（[`GithubBlock::TargetUnavailable`]）、持たせても読む腕が無い。
    /// 分けて出すときに [`DegradedReason`] へ畳むかを決める（04）。
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
/// 別の文言でよい」が当たらない側）。**枠の狭い PRs ペイン**（07）も
/// `TargetUnavailable` を読むが、文言は枠の内側に合わせて別に持つ。
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
    /// （`.claude/rules/testing.md`）。割れているのは「待てば変わる」と「変わらない」で、それは
    /// [`TargetUnavailable`](Self::TargetUnavailable) との境目のほう。
    AccountResolving,
    /// `gh` が無い / 古い / どのアカウントも届かない。**`A` に内訳がある。**
    AccountDegraded,
    /// `config.github.enabled = false`。
    AccountDisabled,
}

impl GithubBlock {
    /// 通知 1 行ぶんの文言。`state` は文言に書くキーを引くため（[`keys::spell`]。08 の
    /// 「文言に書いたキーの字」）。
    pub fn notice(self, state: &AppState) -> String {
        match self {
            Self::TargetUnavailable => {
                "既定リモートから GitHub のリポジトリを特定できません".to_string()
            }
            Self::AccountResolving => "アカウントを解決しています".to_string(),
            // **直し方を書いてよい。** `A` は mewgit が持っている操作で、内訳もそこに
            // 出る（#63 の「指示を足すのはその操作を持たない側」の素直な側）。
            Self::AccountDegraded => keys::spell(
                state,
                [Action::Common(CommonAction::Account)],
                |[account]| format!("GitHub に接続できません（{account} で内訳を見られます）"),
                "GitHub に接続できません",
            ),
            // **直す先は設定ファイルなので、外でやってくださいと書いてよい。**
            // **再起動が要ることも書く** —— 実行中に設定は読み直さない（08）。
            Self::AccountDisabled => {
                "GitHub 機能は config.toml の github.enabled で無効です（要再起動）".to_string()
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
/// **[`Pane`] をそのまま持たない。** 入りうる値は右列の上のペインだけなので、`Pane` で
/// 持つと読む側が「ほかの値は来ない」という注釈付きの `_ =>` を置くことになり、
/// タブを足したときにコンパイラが何も言わない（`Branches` が黙って描かれる）。
/// 3 枚目の `Stash` を足したときは、読む側の網羅 `match` が全部コンパイルエラーに
/// なった。写像は [`of`](Self::of) の 1 本だけ。
///
/// 並びは `Pane` の宣言順（`Branches | Stash | Prs`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RightTab {
    #[default]
    Branches,
    Stash,
    Prs,
}

impl RightTab {
    /// そのペインにフォーカスが来たとき、追随するタブ。右列の上でなければ `None`。
    pub fn of(pane: Pane) -> Option<RightTab> {
        match pane {
            Pane::Branches => Some(RightTab::Branches),
            Pane::Stash => Some(RightTab::Stash),
            Pane::Prs => Some(RightTab::Prs),
            Pane::Graph | Pane::Worktree | Pane::CommitDetail | Pane::Diff => None,
        }
    }

    /// このタブを出しているときにキーを受けるペイン。
    pub fn pane(self) -> Pane {
        match self {
            RightTab::Branches => Pane::Branches,
            RightTab::Stash => Pane::Stash,
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
    /// 全ペイン。巡回の候補ではない（それは下の `*_FOCUS`）—— ペインごとに回すテストが、
    /// 足したペインを見落とさないための一覧。
    pub const ALL: &[Pane] = &all_variants!(Pane {
        Graph,
        Branches,
        Stash,
        Prs,
        Worktree,
        CommitDetail,
        Diff,
    });

    /// WIP 行を選んでいるあいだの巡回先。右は [`GraphSelection::right_pane`] が決める。
    ///
    /// **まだ実装していないペインは入れない**（07）。移れてしまうと、キーを受ける先の
    /// 無い画面に入り込む。ペインを実装したらここに足す。並びは `Pane` の宣言順。
    const WIP_FOCUS: &'static [Pane] = &[
        Pane::Graph,
        Pane::Branches,
        Pane::Stash,
        Pane::Prs,
        Pane::Worktree,
        Pane::Diff,
    ];

    /// `config.github.enabled = false` のときの [`WIP_FOCUS`](Self::WIP_FOCUS)。
    ///
    /// **`Prs` は真ん中にあるので末尾で落とせない。** `Diff` のように
    /// `strip_suffix` で削る形にすると並びの意味が 2 通りになるので、別の一覧として
    /// 持つ（`Prs` が落ちる条件は起動時に固まるので、揺れるのは `Diff` だけ）。
    /// **手で写さずにコンパイル時に導く**（[`without_prs`]）—— 写すと、ペインを足した
    /// ときに片方だけ足して `github.enabled = false` のときだけ巡回から漏れうる。
    const WIP_FOCUS_NO_PRS: &'static [Pane] =
        &without_prs::<{ count_without_prs(Pane::WIP_FOCUS) }>(Pane::WIP_FOCUS);

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
    /// **`Branches` と `Stash` と `Prs` も両方に入る。** どれも右列の上に出ていて、左の
    /// 選択で中身が変わらないため（07 の「レイアウト」）。`Prs` が落ちるのは
    /// `config.github.enabled = false` のときだけで、**`AccountState` では変えない**
    /// —— 変えると起動直後に解決が返った瞬間に `Tab` の行き先が増える（01 の
    /// 「右上のタブ」）。
    const COMMIT_FOCUS: &'static [Pane] = &[
        Pane::Graph,
        Pane::Branches,
        Pane::Stash,
        Pane::Prs,
        Pane::CommitDetail,
        Pane::Diff,
    ];

    /// `config.github.enabled = false` のときの [`COMMIT_FOCUS`](Self::COMMIT_FOCUS)。
    const COMMIT_FOCUS_NO_PRS: &'static [Pane] =
        &without_prs::<{ count_without_prs(Pane::COMMIT_FOCUS) }>(Pane::COMMIT_FOCUS);
}

/// `panes` のうち `Pane::Prs` でないものの数。[`without_prs`] の長さに渡す。
const fn count_without_prs(panes: &[Pane]) -> usize {
    let mut count = 0;
    let mut i = 0;
    while i < panes.len() {
        if !matches!(panes[i], Pane::Prs) {
            count += 1;
        }
        i += 1;
    }
    count
}

/// `panes` から `Pane::Prs` を抜いた並び。巡回の一覧を `const` のまま導くためのもの
/// （`Pane::WIP_FOCUS_NO_PRS` / `Pane::COMMIT_FOCUS_NO_PRS`。理由は前者の doc）。
///
/// **長さ `N` は [`count_without_prs`] から渡す。** 手で書いた数と食い違うと、短ければ
/// 添字の範囲外、長ければ末尾の assert が const 評価で落ちる（コンパイルエラー）。
const fn without_prs<const N: usize>(panes: &[Pane]) -> [Pane; N] {
    let mut out = [Pane::Graph; N];
    let mut filled = 0;
    let mut i = 0;
    while i < panes.len() {
        if !matches!(panes[i], Pane::Prs) {
            out[filled] = panes[i];
            filled += 1;
        }
        i += 1;
    }
    assert!(filled == N, "長さが Prs を抜いた数と合いません");
    out
}

/// `items` の並びのうち、`current` から `delta` 個先。端では反対側に回る。`current` が
/// 並びに無ければ先頭から数える（呼ぶ側が、そこへ来る経路が無いことを doc に書く）。
///
/// **一覧のカーソル（[`stepped_index`]）とは別物。** あちらは端で止まる。
fn cycled<T: Copy + PartialEq>(items: &[T], current: T, delta: isize) -> T {
    let at = items.iter().position(|&item| item == current).unwrap_or(0);
    let len = items.len() as isize;
    items[(at as isize + delta).rem_euclid(len) as usize]
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
    /// 描画側で別に並べると、見えている順とカーソルの動く順がずれる。区画を足して
    /// ここに足し忘れると、その区画は描かれずカーソルも届かないので、`all_variants!` の
    /// 網羅 `match` でコンパイルエラーにする。
    pub const SECTIONS: &[WorktreeSection] = &all_variants!(WorktreeSection {
        Conflicted,
        Staged,
        Unstaged,
        Untracked,
    });

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

    /// `path` に触れる行を、区画と一緒に [`SECTIONS`](Self::SECTIONS) の順で拾う。
    /// staged と unstaged の両方に出るファイルは 2 つ返る。**rename は元のパスでも当たる**
    /// —— `git mv a a2` のあとに作った `?? a` を退避すると、git は rename を割る（03 の
    /// 「stash push のパス指定」）。**パス単位の書き込み**（部分 stash）が「そのパスに
    /// 何があるか」を問うためのもの。
    pub fn entries_at<'a>(
        &'a self,
        path: &'a Path,
    ) -> impl Iterator<Item = (WorktreeSection, &'a FileEntry)> {
        Self::SECTIONS
            .iter()
            .flat_map(|&section| {
                self.snapshot
                    .section(section)
                    .iter()
                    .map(move |entry| (section, entry))
            })
            .filter(move |(_, entry)| entry.path == path || entry.old_path.as_deref() == Some(path))
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// tracked のファイルに変更があるか（staged / unstaged / conflicted）。**untracked は
    /// 数えない** —— git は untracked だけなら rebase を通す（01 の「interactive rebase の
    /// 開始（`i`）」の実測）。
    pub(crate) fn has_tracked_changes(&self) -> bool {
        let snapshot = &self.snapshot;
        !(snapshot.staged.is_empty()
            && snapshot.unstaged.is_empty()
            && snapshot.conflicted.is_empty())
    }

    /// [`SECTIONS`](Self::SECTIONS) の何番目の区画か。
    fn rank(section: WorktreeSection) -> usize {
        Self::SECTIONS
            .iter()
            .position(|&candidate| candidate == section)
            .expect("SECTIONS は all_variants! で全区画を持つ")
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
    /// Diff ペインに何の diff を出しているか（01 の「Stash ペイン」）。
    ///
    /// **Diff ペインに居るあいだだけ意味を持つ。** 離れたら
    /// [`AppState::reseat_focus`] が `Selection` に戻すので、`Tab` で入り直したときは
    /// グラフの選択の diff が出る。
    pub source: DiffSource,
}

/// Diff ペインに出す diff の出どころ（01 の「Stash ペイン」）。
///
/// **`Enter` で来た側を憶える。** グラフの選択（WIP 行かコミット）は Diff ペインに
/// 居るあいだも画面の状態として残っているので、そちらからは導けるが、Stash ペインの
/// どの行から来たかは残らない。`Esc` の戻り先と [`AppState::wanted_diff`] がこれを読む。
///
/// **stash は index ではなく oid で持つ。** index は push / drop で指す相手が変わる
/// （[`StashState::loaded`]）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DiffSource {
    /// グラフの選択。WIP 行ならワーキングツリー、コミットならそのファイル一覧。
    #[default]
    Selection,
    /// Stash ペインの `Enter`。
    Stash(Oid),
}

/// [`DiffSource`] とグラフの選択を解いた、Diff ペインの出どころ
/// （[`AppState::diff_origin`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffOrigin {
    /// WIP 行の選択。ワーキングツリーのファイル。
    Worktree,
    /// コミット行の選択。そのコミットのファイル一覧で選んだもの。
    Commit(Oid),
    /// Stash ペインの `Enter`。
    Stash(Oid),
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
    /// 採った時点の、対象のファイルの指紋（ワークツリーの `(mtime, len)` と index・HEAD の
    /// blob。[`AppState::diff_stamp`]）。**外でファイルや index を書き換えたのを拾うため**
    /// （01 の「外部の変更の検知 › 定期の status」）。世代は mewgit の書き込みでしか
    /// 進まないので、外の編集では刻印が最新のまま残る。
    pub stamp: Option<PathStamp>,
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
pub(crate) fn stepped_index(cursor: usize, len: usize, delta: isize) -> usize {
    seated_index(cursor, len, cursor.saturating_add_signed(delta))
}

/// `0..len` の添字を `row` へ跳ばした位置。外なら最後の要素に丸め、要素が無ければ
/// `cursor` のまま据え置く（[`stepped_index`] と同じ端の扱い）。
///
/// **呼ぶのは所有する型の `set_cursor` と [`stepped_index`] だけ**（[`stepped_index`] と
/// 同じ理由）。
pub(crate) fn seated_index(cursor: usize, len: usize, row: usize) -> usize {
    let Some(last) = len.checked_sub(1) else {
        return cursor;
    };
    row.min(last)
}

/// diff ペインのカーソル。ファイルとその中のハンクを指す。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffCursor {
    pub file: usize,
    pub hunk: usize,
}

/// 一覧（ブランチ・stash）の読み込みがどこまで進んだか。
///
/// **`bool` を 2 つ（`loaded` / `failed`）にしない。** 4 通りのうち「まだ届いて
/// いないのに失敗した」が表現でき、どのテストからも踏めない腕になる（01）。
///
/// **`ui` には述語ではなくこの enum を見せる**（[`BranchState::load`]）。`has_failed`
/// のような述語を並べると、段階を足したときに `ui` の else に黙って落ちる。
///
/// **2 つ目の利用者（[`StashState`]）が出た時点で `BranchLoad` から改名した。**
/// 写すと、段階を足すときに片方だけ古くなる。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ListLoad {
    /// まだ届いていない。
    #[default]
    Pending,
    Loaded,
    Failed,
}

/// トースト通知 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// 積まれた順の番号（[`AppState::push_notice`] が振る）。[`Modal::NoticeLog`] が
    /// 選んでいる通知を指すのに使う。
    pub seq: u64,
    pub level: NoticeLevel,
    pub message: String,
    /// 実行した argv や stderr など、展開して見せる詳細（不変ルール 4）。
    pub detail: Option<String>,
    /// 積まれた時刻（[`AppState::now`] の写し）。
    pub at: Timestamp,
    /// ステータスバーから下ろしたか（[`AppState::dismiss_notice`]）。
    pub dismissed: bool,
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
    /// interactive rebase の plan を編む（01 の「interactive rebase の開始（`i`）」、07 の
    /// 「RebaseEditor」）。Graph の `i` で開く。
    ///
    /// **`plan` は古い順**（git の todo と同じ）。`onto` は選択コミットの第 1 親、`head` は
    /// plan を組んだ時点の HEAD（`GitOp::RebaseInteractive` の `head`）。
    /// **検証の結果は持たない** —— 描く側も `Enter` も `RebasePlan::validate()` を
    /// その場で呼ぶ。持つと、行を動かすたびに更新を忘れる形が生まれる。
    ///
    /// **`reword` は `r` の入力**（[`RewordInput`]。01）。別のモーダルにしないのは、戻る
    /// ときに plan を丸ごと運ぶことになるため（`PrCreate` の `field` と同じく、1 つの
    /// モーダルの中でモードを切る）。
    RebaseEditor {
        onto: Oid,
        head: Oid,
        plan: RebasePlan,
        cursor: usize,
        reword: RewordInput,
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
    /// 通知の履歴（01 の「通知」）。`L` で開く。**ステータスバーでは読めない
    /// `detail`（argv と git の出力）と日付を出すのはここだけ。**
    ///
    /// **一覧は持たない。** 読むのは [`AppState::notices`] で、写しを持つと開いている
    /// あいだに積まれた通知が出ない。
    NoticeLog {
        /// 選んでいる通知の [`Notice::seq`]。**添字で持たない** —— 添字だと、開いて
        /// いるあいだに積まれた 1 件でカーソルの下の通知が入れ替わり、`push_notice` が
        /// モーダルを知って補正することになる（01 の「通知」）。描く行は
        /// [`AppState::notice_log_row`]。
        selected: u64,
    },
    /// 進行中操作から出る手（continue / skip / abort）を選ぶ（01 の「進行中操作の
    /// メニュー（`m`）」）。`m` で開く。
    ///
    /// **開いた時点で組んだメニューを持つ**（`RemotePicker` の `remotes` と同じ）。
    /// 開いているあいだに status が届いて進行中操作が入れ替わっても、**カーソルの下の
    /// 項目が動かない**。
    ProgressMenu {
        menu: ProgressMenu,
        cursor: usize,
    },
    /// merge コミットを revert するとき、**残す側の親**（`--mainline`）を選ぶ（01 の
    /// 「revert の開始（`v`）」）。Graph の `v` が merge コミットで開く。
    ///
    /// **開いた時点のコミットと親を写して持つ**（`RemotePicker` の `remotes` と同じ）。
    /// 開いているあいだにログが読み直されても、選んだ番号が別のコミットの親を指さない。
    /// `cursor` の添字 + 1 が `--mainline` の番号。
    MainlinePicker {
        target: Oid,
        summary: String,
        parents: Vec<ParentChoice>,
        cursor: usize,
    },
    /// キーの一覧（07 の「Help モーダル」）。`?` で開く。
    ///
    /// **開いたときのペインを持つ。** [`AppState::focus`] を描くたびに読むと、開いている
    /// あいだに届いた status で `reseat_focus`（`update` の出口で毎回通る）がフォーカスを
    /// 移したとき、一覧が別のペインのものに入れ替わる。行は持たず、描く行は
    /// [`PaneKeys::help`](super::PaneKeys::help) が `pane` の表から組む。`cursor` は窓を動かすためだけ。
    Help {
        pane: Pane,
        cursor: usize,
    },
}

/// [`Modal::MainlinePicker`] の 1 行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentChoice {
    pub oid: Oid,
    /// **読み込み済みのグラフに在るときだけ**（01 の「revert の開始（`v`）」）。
    /// 無いときに読み足さない —— oid だけでも選べ、読み足しを待たせるほうが重い。
    pub summary: Option<String>,
}

impl AppState {
    /// 一覧から 1 つ選ぶモーダルのカーソルを動かす。カーソルを持たないモーダルでは
    /// 何もしない。
    ///
    /// **端の扱いは [`stepped_index`]**（ペインのカーソルと同じ規則）。ここに置くのは、
    /// 平坦な添字のカーソルが**どれも所有する型の `move_cursor` に包まれている**ため
    /// （[`BranchState::move_cursor`] / [`CommitFiles::move_cursor`]）。`update` から
    /// [`stepped_index`] を直に呼ぶと、そのために可視性を広げることになる。
    ///
    /// **`Modal` ではなく `AppState` に置く。** [`Modal::NoticeLog`] の一覧は
    /// [`AppState::notices`] で、モーダル自身は中身を知らない。
    pub(super) fn move_modal_cursor(&mut self, delta: isize) {
        // 一覧を読むので、`modal` を可変で借りる前に行き先を決めておく。
        let notice_to = |selected: u64| {
            let row = self.notice_log_row(selected)?;
            let row = stepped_index(row, self.notices.len(), delta);
            Some(self.notices[self.notices.len() - 1 - row].seq)
        };
        let notice = match self.modal {
            Some(Modal::NoticeLog { selected }) => notice_to(selected),
            _ => None,
        };
        match self.modal.as_mut() {
            Some(Modal::RemotePicker {
                remotes, cursor, ..
            }) => *cursor = stepped_index(*cursor, remotes.len(), delta),
            // **選べない degraded では候補が空**なので、`stepped_index` が 0 を返して
            // 動かない（`NoAccess` の内訳は本文に出る。07）。
            Some(Modal::AccountPicker { view, cursor }) => {
                *cursor = stepped_index(*cursor, view.candidates().len(), delta);
            }
            Some(Modal::ProgressMenu { menu, cursor }) => {
                *cursor = stepped_index(*cursor, menu.items.len(), delta);
            }
            Some(Modal::MainlinePicker {
                parents, cursor, ..
            }) => *cursor = stepped_index(*cursor, parents.len(), delta),
            Some(Modal::RebaseEditor { plan, cursor, .. }) => {
                *cursor = stepped_index(*cursor, plan.entries.len(), delta);
            }
            Some(Modal::Help { pane, cursor }) => {
                *cursor = stepped_index(*cursor, self.keymap.pane(*pane).help().len(), delta);
            }
            // **通知が無ければ動かない**（`notice_log_row` が `None`）。
            Some(Modal::NoticeLog { selected }) => {
                if let Some(seq) = notice {
                    *selected = seq;
                }
            }
            Some(
                Modal::Confirm { .. }
                | Modal::Input { .. }
                | Modal::CommitMessage { .. }
                | Modal::PrCreate { .. },
            )
            | None => {}
        }
    }
}

/// [`Modal::PrCreate`] のどの欄を編集しているか（01 の「PR 作成（`N`）」）。
///
/// **巡回の順は [`PrField::ALL`] の並び**（`Tab` が次へ = [`PrField::next`]、
/// `Shift+Tab` が前へ）で、07 が描く縦の並びと同じ。**画面の並びと巡回の順を割らない**
/// —— 割ると `Tab` を押した先が画面のどこかを目で追えなくなる。tui の
/// `modals::pr_create::render` も `ALL` を回して欄を積む。
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
    /// 巡回の並び。**`next` と `prev` はこの 1 本から両方向を導く** —— 2 つの網羅
    /// `match` に別々に書くと、食い違ってもコンパイラは何も言わない。`all_variants!` が
    /// 網羅 `match` と同じ並びから作るので、欄を足すと並べるまでコンパイルエラーになる。
    pub const ALL: &[PrField] = &all_variants!(PrField {
        Title,
        Body,
        Base,
        Head,
        Draft,
    });

    /// `Tab` の行き先。端では巻く。
    ///
    /// **[`stepped_index`] を使わない。** あちらは一覧のカーソルで、**端で止まる**
    /// （行が増減するものを行き過ぎないため）。欄は増えないので、`Tab` を押し続けて
    /// 最後の欄で止まるほうが驚く。
    pub(super) fn next(self) -> Self {
        cycled(Self::ALL, self, 1)
    }

    /// `Shift+Tab` の行き先。[`PrField::next`] の逆。
    pub(super) fn prev(self) -> Self {
        cycled(Self::ALL, self, -1)
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
/// **写しが古くなる瞬間は在る**ので、`account` を `Resolving` に落とす 1 箇所
/// （`update::account::leave_account`）が開いている `AccountPicker` を閉じる
/// （01 の「写しの寿命」）。
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
/// 同じ `None` になり、本文に理由を出せない（`.claude/rules/rust-coding.md` の
/// 「`None` の理由を 2 つ以上持たせない」）。
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
    /// 案内）、`AppState::move_modal_cursor`、`Enter` の確定。別々に `matches!` を書くと、
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
    /// 作業ツリーを stash する（`stash push --include-untracked`）。打つのは
    /// メッセージで、**空なら `--message` を付けない**（git の既定。01 の「stash の書き込み」）。
    StashPush {
        /// `None` が全体（`z`）、`Some` が Worktree のカーソルの 1 ファイル（`Z`）。
        /// **開いた時点のパスで持つ** —— 確定までにカーソルが動いても相手は変わらない。
        path: Option<PathBuf>,
    },
}

impl InputIntent {
    /// 打った `name` で確定したときに出す書き込み。
    ///
    /// **開く前の判定もこれで問う**（名前は空のまま）。進行中操作で断るかは変種と
    /// `switch` だけで決まるので、確定と同じ `GitOp` を組めば判定が割れない（01 の
    /// 「進行中操作があるときの書き込み」）。
    pub fn op(&self, name: String) -> GitOp {
        match self {
            InputIntent::BranchCreate { start } => GitOp::BranchCreate {
                name,
                start: start.clone(),
                switch: true,
            },
            InputIntent::BranchRename { from } => GitOp::BranchRename {
                from: from.clone(),
                to: name,
            },
            InputIntent::StashPush { path } => GitOp::StashPush {
                message: (!name.trim().is_empty()).then_some(name),
                // 「全体」は Worktree ペインに出ているもの全部で、untracked もそこに
                // 出る。**1 ファイルでも付ける** —— 無いと untracked のパスは
                // `pathspec did not match` で落ち、パスで絞ればほかの untracked は
                // 退避されない（01 の「stash の書き込み」）。
                include_untracked: true,
                paths: path.clone().map(|path| vec![path]),
            },
        }
    }

    /// 空で確定したら断るか。**名前は空を断り、stash のメッセージは空を通す**
    /// （空なら git の既定のメッセージ）。
    pub fn requires_text(&self) -> bool {
        match self {
            InputIntent::BranchCreate { .. } | InputIntent::BranchRename { .. } => true,
            InputIntent::StashPush { .. } => false,
        }
    }
}

/// [`Modal::RebaseEditor`] の reword の入力（01 の「interactive rebase の開始（`i`）」）。
///
/// **読んでいるあいだを状態に持つ。** 持たないと、返事が届いたときに「誰が頼んだか」を
/// エディタの形（開いているか、plan にその Oid があるか）から推し量ることになり、`r` →
/// エディタを閉じる → 同じ範囲で `i` と打つと頼んでいない入力が開く（`/simplify` が拾った）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RewordInput {
    /// 一覧を出している。
    #[default]
    Closed,
    /// `r` を押し、そのコミットのメッセージ全文を読んでいる。返事はこの Oid のものだけを採る。
    Loading(Oid),
    /// カーソルの行のメッセージを打っている。キーはすべて入力が受ける。
    Open(TextBuffer),
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
    use crate::test_util::{fake_snapshot, FAKE_NOW};

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

    #[test]
    fn the_lists_without_prs_keep_every_other_pane_in_order() {
        // `without_prs` が `Prs` だけを抜き、並びを崩さないこと。
        for (all, without) in [
            (Pane::WIP_FOCUS, Pane::WIP_FOCUS_NO_PRS),
            (Pane::COMMIT_FOCUS, Pane::COMMIT_FOCUS_NO_PRS),
        ] {
            // **抜く相手が入っていることを先に見る**（入っていなければ自明に通る）。
            assert!(all.contains(&Pane::Prs), "{all:?}");
            let expected: Vec<Pane> = all.iter().copied().filter(|&p| p != Pane::Prs).collect();
            assert_eq!(without, expected.as_slice());
        }
    }

    /// コミットを 1 件積んだ状態。カーソルは WIP 行のまま。
    ///
    /// **`GraphLine` を手で組まない。** `LaneState::extend` を通らない `GraphRow` は
    /// `lane` も `cells` も実際には出てこない値になり、レーンや色を見るテストを
    /// 足した瞬間に「通るが現実と違う」状態になる。
    fn state_with_a_commit() -> AppState {
        let mut state = AppState::new(RepoInfo::default(), Config::default(), FAKE_NOW);
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
        let mut state = AppState::new(RepoInfo::default(), Config::default(), FAKE_NOW);
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
            state.graph.set_cursor(cursor);
            let right = state.graph.selection().right_pane();
            assert!(
                state.focus_candidates().contains(&right),
                "cursor={cursor} right={right:?} candidates={:?}",
                state.focus_candidates()
            );
        }
    }

    #[test]
    fn a_pane_out_of_the_candidates_is_refused_with_a_reason_that_is_not_github() {
        // `Prs` 以外の理由で落ちたペインに「GitHub 機能が無効」と言わない
        // （`focus_refusal` の doc）。コミットを選んでいるあいだ Working tree は候補の外。
        let mut state = state_with_a_commit();
        state.graph.set_cursor(1);
        assert_eq!(state.focus_refusal(Pane::CommitDetail), None);
        let reason = state
            .focus_refusal(Pane::Worktree)
            .expect("候補に入っています");
        assert_ne!(reason, GithubBlock::AccountDisabled.notice(&state));
    }

    #[test]
    fn the_branches_pane_is_a_candidate_whatever_the_graph_selects() {
        // 右列の上に常に出ていて、左の選択で中身が変わらない（07 の「レイアウト」）。
        // 候補から外れると、画面に出ているペインへ `Tab` で行けない。
        let mut state = state_with_a_commit();
        for cursor in [0, 1] {
            state.graph.set_cursor(cursor);
            assert!(
                state.focus_candidates().contains(&Pane::Branches),
                "cursor={cursor} candidates={:?}",
                state.focus_candidates()
            );
        }
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

        state.graph.set_cursor(1);

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

        state.graph.set_cursor(1);
        state.reseat_focus();

        assert_eq!(state.focus, Pane::Graph);
    }

    #[test]
    fn the_wip_row_is_a_display_row_of_its_own() {
        // 表示行 0 は WIP 行、1.. が `lines`（01 の「グラフの行」）。
        let mut state = state_with_a_commit();
        assert_eq!(state.graph.rows(), 2);
        assert_eq!(state.graph.selection(), GraphSelection::Wip);
        assert!(state.graph.cursor_commit().is_none());

        state.graph.set_cursor(1);

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
        assert_eq!(state.graph.cursor(), 0);

        state.graph.move_cursor(5);
        assert_eq!(state.graph.cursor(), 1);
    }

    #[test]
    fn an_empty_graph_still_has_the_wip_row() {
        // 行が 1 つも無くても WIP 行は選べる（07 の「Graph ペイン」）。
        let mut state = AppState::new(RepoInfo::default(), Config::default(), FAKE_NOW);
        assert_eq!(state.graph.rows(), 1);

        state.graph.move_cursor(3);

        assert_eq!(state.graph.cursor(), 0);
        assert_eq!(state.graph.selection(), GraphSelection::Wip);
    }

    #[test]
    fn push_notice_drops_the_oldest_beyond_the_cap() {
        let mut state = AppState::new(RepoInfo::default(), Config::default(), FAKE_NOW);
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
        // **捨てたあとも番号は重ならない**（`Modal::NoticeLog` が番号で選ぶ）。上限を
        // 2 件以上越えて積まないと、件数で振る形でも重ならずに通る。
        let seqs: Vec<u64> = state.notices.iter().map(|n| n.seq).collect();
        assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
    }

    #[test]
    fn text_buffer_round_trips() {
        let buf = TextBuffer::from_text("first\nsecond");
        assert_eq!(buf.lines, ["first", "second"]);
        assert_eq!(buf.cursor, (1, 6));
        assert_eq!(buf.text(), "first\nsecond");
    }
}
