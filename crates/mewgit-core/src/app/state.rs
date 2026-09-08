//! アプリ全体の状態。設計は `docs/design/01-app-core.md`。
//!
//! `ui` はここにある値を描画するだけで、更新は [`update`](crate::app::update) が行う。

use std::collections::VecDeque;
use std::path::PathBuf;

use crate::auth::{AccountState, GhLogin, RepoTarget};
use crate::git_exec::RebasePlan;
use crate::git_read::{
    Branch, Commit, CommitDetail, DiffTarget, FileDiff, FileEntry, HeadInfo, InProgressOp, Oid,
    RefIndex, Remote, StashEntry, WorktreeSection, WorktreeSnapshot,
};
use crate::github::{Check, PullRequest};
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
    pub modal: Option<Modal>,
    pub jobs: JobTracker,
    /// エラー・情報のトースト。積むのは [`AppState::push_notice`]。
    pub notices: VecDeque<Notice>,
    pub term: TermSize,
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
            account: AccountState::default(),
            graph: GraphState::default(),
            worktree: WorktreeState::default(),
            diff: DiffState::default(),
            branches: BranchState::default(),
            push_rejected: None,
            stashes: Vec::new(),
            prs: PrState::default(),
            progress: None,
            focus: Pane::default(),
            modal: None,
            jobs: JobTracker::default(),
            notices: VecDeque::new(),
            term: TermSize::default(),
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
            cmd => match cmd.job_kind() {
                None => cmd,
                Some(kind) => {
                    let job = Job {
                        kind,
                        label: kind.label().to_string(),
                    };
                    if self.jobs.start(job) {
                        cmd
                    } else {
                        self.dropped(kind);
                        Cmd::None
                    }
                }
            },
        }
    }

    /// 同種のジョブが走っていて発行できなかった `Cmd` を伝える。
    ///
    /// 何を伝えるかは [`JobKind::drop_notice`] が種別ごとに持つ。
    fn dropped(&mut self, kind: JobKind) {
        if let Some(message) = kind.drop_notice() {
            self.push_notice(NoticeLevel::Warn, message.to_string(), None);
        }
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
            GraphSelection::Wip => Pane::WIP_FOCUS,
            GraphSelection::Commit(_) => Pane::COMMIT_FOCUS,
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
    /// 候補にあるペインは `Graph` / `Branches` / `Diff` の 3 つあるが、いま見ている
    /// 左の領域に戻すのが説明の付く動き（07 の「ペインとフォーカス」）。
    ///
    /// **フォーカスを動かすキーが通る `update::focus` は経由しない。** あちらは
    /// 「見えるようになった diff を頼む」ためのもので、ここが移す先は必ず `Graph`
    /// （`wanted_diff()` が `None` になる側）なので頼むものが無い。通しても必ず
    /// `Cmd::None` が返る腕が 1 つ増えるだけになる。
    pub fn reseat_focus(&mut self) {
        if !self.focus_candidates().contains(&self.focus) {
            self.focus = Pane::Graph;
        }
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
    /// 既定リモートから解析・正規化した host / owner / repo。未解決なら `None`。
    pub target: Option<RepoTarget>,
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
    const WIP_FOCUS: &'static [Pane] = &[Pane::Graph, Pane::Branches, Pane::Worktree, Pane::Diff];

    /// コミットを選んでいるあいだの巡回先。右は [`GraphSelection::right_pane`] が決める。
    ///
    /// **`Worktree` を入れない。** 入れると、コミットを選んだまま
    /// `focus == Pane::Worktree` でいられてしまい、`s` / `u` / `x` が画面に出ていない
    /// ファイルを stage する（01 の「Pane と Modal」）。
    ///
    /// **`Diff` は両方に入るが、出る diff は違う。** ワーキングツリーのファイルか、
    /// コミットのファイルか。決めるのは [`AppState::wanted_diff`] の 1 つ。
    ///
    /// **`Branches` も両方に入る。** 右列の上に常に出ていて、左の選択で中身が
    /// 変わらないため（07 の「レイアウト」）。
    const COMMIT_FOCUS: &'static [Pane] =
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
    /// 頼んだ読み直しが [`AppState::issue`] に落とされた。
    ///
    /// 書き込みで ref が動くと積んだ行が丸ごと古くなるが、そのとき続きの読み込みが
    /// 走っていると同種の読み直しは落ちる。落ちたぶんをここに覚えておき、走っていた
    /// ページが届いた時点で頼み直す。**[`WorktreeState::stale`] と同じ形**で、立てるのも
    /// 同じ「書き込みの時点で読み取りが走っていたか」（01 の「ログの読み直し」）。
    ///
    /// 読む場所は 2 つ（届いたページを採るか・頼み直すか）で、**どちらも「読み直しが
    /// 落とされたまま」という同じ時点を見ている**ので `bool` で足りる。
    /// [`DiffState::freshness`] が 3 状態なのは、「短絡してよいか」と「パッチを組んで
    /// よいか」で見る時点が違ったため。
    ///
    /// **立っているあいだは必ず `JobKind::LoadLog` が走っている**（落とした相手が
    /// それ）。続きの読み込みをここで止める必要が無いのはこの不変条件による。
    ///
    /// [`DiffState::freshness`]: DiffState
    pub reload_pending: bool,
}

impl GraphState {
    /// 積んだ行を捨てて、先頭から読み直せる状態にする。
    ///
    /// ログ読み込みの失敗（02 の契約により、失敗したセッションは捨てられ次は先頭から
    /// 来る）と、commit のあとの読み直しで同じことをする。**1 本にまとめてあるのは、
    /// 片方に足し忘れると `lines` に無いコミットの詳細やレーンの状態が残るため。**
    /// `load_failed` はここでは触らない（呼び出し元で値が違う。失敗の側だけが立てる）。
    /// `reload_pending` は落とす。どちらの呼び出し元でも「先頭から積み直したので
    /// 借りは無い」で一致し、**足し忘れると届かない読み直しを待ち続ける**。
    pub fn restart(&mut self) {
        self.reload_pending = false;
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
    /// 届く予定の status が、直近の書き込みより前に頼まれたもの。
    ///
    /// `JobKind` の粒度は種別単位なので、読み取りが走っているあいだの
    /// `Cmd::LoadStatus` は [`AppState::issue`] に落とされる。落ちたぶんを届いた
    /// 時点で頼み直すための印（01 の「ワーキングツリー」）。立てるのは
    /// `exec_finished`、落とすのは `status_loaded`。
    pub stale: bool,
    /// 一度でも status を読めた。
    ///
    /// **空の `snapshot` には 2 つの意味がある** — 「変更が無い」と「まだ読めていない」。
    /// 起動直後や `Cmd::LoadStatus` が失敗したあとは後者で、`stale` では区別できない
    /// （あれは書き込みでしか立たない）。commit の「stage された変更がありません」を
    /// この 2 つ目で出すと、実際には stage されているのに断ることになる
    /// （01 の「commit」）。
    pub loaded: bool,
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
    /// 持っている [`view`](Self::view) と、飛んでいる `LoadDiff` の鮮度。
    ///
    /// [`view`](Self::view) をキャッシュに使うための無効化の印（01 の
    /// 「diff のキャッシュと鮮度」）。対象は変わらないまま中身だけが変わる経路が
    /// あるため要る（ハンク単位で stage すると `IndexVsWorktree(a.txt)` のまま
    /// 中身が減る）。
    pub freshness: Freshness,
}

/// 持っている [`DiffView`] と、飛んでいる `LoadDiff` の鮮度。
///
/// **`bool` では足りない。** キャッシュを短絡してよいか（= 読み直しを**頼んだ**か）と、
/// 出しているハンクからパッチを組んでよいか（= 読み直しが**届いた**か）は切り替わる
/// 時点が違う。1 つの `bool` はどちらか片方の時点でしか倒れないので、もう片方が
/// 「頼んだだけで届いていない diff」を新しいものとして扱う（01 の
/// 「diff のキャッシュと鮮度」）。
///
/// **対象は持たない。** 書き込みの時点では `clamp` がまだで、そのとき選ばれている
/// ファイルが読み直しの対象になるとは限らない。
///
/// `JobTracker` に世代を持たせれば [`WorktreeState::stale`] ごと畳める。**それは
/// M3 の fetch で入れる**（01 の「実行中ジョブ › 世代」）。ここはその局所版。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// 書き込みより後の diff を持っている（書き込みがまだ無いときも含む）。
    #[default]
    Fresh,
    /// 書き込みより前の diff しか無く、読み直しをまだ頼めていない。
    ///
    /// 書き込みの瞬間と、`LoadDiff` が走っていて `issue` に落とされたときがこれ。
    Stale,
    /// 書き込みより前の diff しか無いが、読み直しは頼んである。
    ///
    /// **次に届く 1 通が書き込みより後のもの。** 逆に言うと、この状態のあいだは
    /// `LoadDiff` がちょうど 1 つ飛んでいる。届いた時点で必ず `Fresh` か `Stale` に
    /// 倒れるので、`Esc` で対象が消えても取り残されない。
    Asked,
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
/// [`BranchState::move_cursor`] が同じ規則を使う（Stash / PRs も同じ形になる）。
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

    /// カーソルを `delta` 行動かす。端では止まる。
    ///
    /// 行が 1 つも無ければ据え置く（[`stepped_index`]）。
    pub(super) fn move_cursor(&mut self, delta: isize) {
        self.cursor = stepped_index(self.cursor, self.branches.len(), delta);
    }
}

/// PR ペインの状態。
#[derive(Debug, Default)]
pub struct PrState {
    /// 現在のブランチに対応する PR。
    pub current: Option<PullRequest>,
    /// 一覧タブの中身。各行のバッジには `PullRequest.checks`（集計）を使う。
    pub list: Vec<PullRequest>,
    /// `current` のチェック一覧。空でなければ `current.checks`（集計）より優先する。
    /// 一覧が取れているならそちらが新しいため。
    pub checks: Vec<Check>,
    pub cursor: usize,
}

/// 実行中の非同期タスク。スピナー表示と `Cmd` の重複発行防止に使う。
#[derive(Debug, Default)]
pub struct JobTracker {
    running: Vec<Job>,
}

impl JobTracker {
    /// この種別のタスクが動いているか。同種の `Cmd` を二重に投げないための判定。
    pub fn is_running(&self, kind: JobKind) -> bool {
        self.running.iter().any(|job| job.kind == kind)
    }

    /// 開始を記録する。同種が既に動いていれば何もせず `false` を返す。
    pub fn start(&mut self, job: Job) -> bool {
        if self.is_running(job.kind) {
            return false;
        }
        self.running.push(job);
        true
    }

    /// 完了を記録する。
    pub fn finish(&mut self, kind: JobKind) {
        self.running.retain(|job| job.kind != kind);
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
    /// ステータスバーに出す短い説明。`Exec` 以外は種別から決まるが、`Exec` だけは
    /// `GitOp` によって変わる（`fetch` / `push` など）ので値として持つ。
    pub label: String,
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
    ResolveAccount,
    FetchPr,
    FetchChecks,
    FetchPrList,
}

impl JobKind {
    /// 同種が走っていて発行できなかったときに出す通知。黙って落としてよければ `None`。
    ///
    /// 読み取りは黙って落とす（同じものをもう一度読むだけで、結果は変わらない）。
    /// 書き込みだけは「押したのに何も起きない」に見えるので伝える。キューに積んで
    /// 順に流すのは M3（01 の「ランタイム」）。網羅 `match` にしているのは、
    /// 種別を足したときにどちらなのかを選ばせるため。
    pub fn drop_notice(self) -> Option<&'static str> {
        match self {
            JobKind::Exec => Some("ほかの git 実行が終わるまで待ってください"),
            JobKind::LoadLog
            | JobKind::LoadCommitDetail
            | JobKind::LoadCommitFiles
            | JobKind::LoadHeadMessage
            | JobKind::LoadRepoInfo
            | JobKind::LoadStatus
            | JobKind::LoadBranches
            | JobKind::LoadDiff
            | JobKind::ResolveAccount
            | JobKind::FetchPr
            | JobKind::FetchChecks
            | JobKind::FetchPrList => None,
        }
    }

    /// ステータスバーに出す既定の説明。
    ///
    /// `Exec` だけは `GitOp` によって `fetch` / `push` と変わるので、[`Job::label`] は
    /// `String` のまま。実行時に差し替えるのは M3 の `Cmd::Exec` のタスク。
    pub fn label(self) -> &'static str {
        match self {
            JobKind::LoadLog => "log",
            JobKind::LoadCommitDetail => "commit",
            JobKind::LoadCommitFiles => "files",
            JobKind::LoadHeadMessage => "head",
            JobKind::LoadRepoInfo => "repo",
            JobKind::LoadStatus => "status",
            JobKind::LoadBranches => "branches",
            JobKind::LoadDiff => "diff",
            JobKind::Exec => "git",
            JobKind::ResolveAccount => "account",
            JobKind::FetchPr => "pr",
            JobKind::FetchChecks => "checks",
            JobKind::FetchPrList => "pr list",
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
    PrCreate {
        title: TextBuffer,
        body: TextBuffer,
        base: String,
        draft: bool,
    },
    AccountPicker {
        candidates: Vec<GhLogin>,
        cursor: usize,
    },
    Help,
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
        let _ = crate::app::update(
            &mut state,
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
        Job {
            kind,
            label: "test".to_string(),
        }
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
        assert!(jobs.start(job(JobKind::FetchPr)));
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
    fn finishing_an_idle_kind_is_a_no_op() {
        let mut jobs = JobTracker::default();
        jobs.start(job(JobKind::LoadLog));

        jobs.finish(JobKind::FetchPr);
        assert!(jobs.is_running(JobKind::LoadLog));
    }
}
