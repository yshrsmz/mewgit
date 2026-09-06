//! アプリ全体の状態。設計は `docs/design/01-app-core.md`。
//!
//! `ui` はここにある値を描画するだけで、更新は [`update`](crate::app::update) が行う。

use std::collections::VecDeque;
use std::path::PathBuf;

use crate::auth::{AccountState, GhLogin, RepoTarget};
use crate::git_exec::RebasePlan;
use crate::git_read::{
    Branch, Commit, CommitDetail, DiffTarget, FileDiff, FileEntry, HeadInfo, InProgressOp,
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
    Diff,
}

impl Pane {
    /// `Tab` / `Shift+Tab` の巡回に入るペイン。
    ///
    /// **まだ実装していないペインは入れない**（07）。移れてしまうと、キーを受ける先の
    /// 無い画面に入り込む。ペインを実装したらここに足す。並びは表示順。
    pub const FOCUSABLE: &'static [Pane] = &[Pane::Graph, Pane::Worktree, Pane::Diff];

    /// 巡回の次。末尾からは先頭に戻る。
    pub fn next(self) -> Pane {
        self.cycle(1)
    }

    /// 巡回の前。先頭からは末尾に戻る。
    pub fn prev(self) -> Pane {
        self.cycle(-1)
    }

    /// 巡回を `delta` 個進める。一覧に無いペイン（実装前のものが `focus` に
    /// 入っている場合）は先頭から数え直す。
    fn cycle(self, delta: isize) -> Pane {
        let panes = Pane::FOCUSABLE;
        let current = panes.iter().position(|&pane| pane == self).unwrap_or(0);
        let len = panes.len() as isize;
        let next = (current as isize + delta).rem_euclid(len) as usize;
        panes[next]
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
    pub cursor: usize,
    /// 画面最上部に出す行の添字。カーソルだけからは決まらない（カーソルを上に
    /// 動かしたとき、見えているうちは view を動かさない）ので状態として持つ。
    pub scroll: usize,
    /// グラフに使える行数。`ui` がレイアウトから出して [`Msg::Layout`] で運んでくる
    /// （`crate::app::Msg::Layout`）。core はヘッダや枠の高さを知らない。
    pub viewport: usize,
    /// 選択コミットの詳細を展開しているか。
    pub expanded: bool,
    /// 遅延取得した詳細（02）。どのコミットのものかは `detail.oid` で判る。
    /// カーソルの指すコミットと食い違っていれば、まだ届いていないか失敗したという
    /// こと。「読み込み中」のフラグを別に持たない（01 の「コミット詳細の遅延取得」）。
    pub detail: Option<CommitDetail>,
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
    /// カーソルが指しているコミット。行が無ければ `None`。
    pub fn cursor_commit(&self) -> Option<&Commit> {
        self.lines.get(self.cursor).map(|line| &line.commit)
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

    /// カーソルを `delta` 行動かす。端では止まる。
    pub fn move_cursor(&mut self, delta: isize) {
        if self.lines.is_empty() {
            self.cursor = 0;
            self.scroll = 0;
            return;
        }

        let last = self.lines.len() - 1;
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
        self.scroll = self
            .scroll
            .min(self.lines.len().saturating_sub(self.viewport));
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + self.viewport {
            self.scroll = self.cursor - self.viewport + 1;
        }
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
    /// 持っている [`view`](Self::view)、または飛んでいる要求が、直前の書き込みより
    /// 前のもの。
    ///
    /// [`view`](Self::view) をキャッシュに使うための無効化の印（01 の
    /// 「diff のキャッシュと `DiffState.stale`」）。対象は変わらないまま中身だけが
    /// 変わる経路があるため要る（ハンク単位で stage すると `IndexVsWorktree(a.txt)` の
    /// まま中身が減る）。立てるのは `exec_finished`、落とすのは `load_diff` が
    /// `Cmd::LoadDiff` を実際に出せたときだけ。
    ///
    /// **対象は持たない。** 書き込みの時点では `clamp` がまだで、そのとき選ばれて
    /// いるファイルが読み直しの対象になるとは限らない。
    pub stale: bool,
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
/// **端の扱いはここ 1 箇所。** [`WorktreeState::move_cursor`] と
/// [`DiffView::move_cursor`] が同じ規則を使う。並べる中身は違っても「進めて端で
/// 止める」は 1 つ。要素が無ければ `None` を返し、そのときどうするかは呼び出し側が決める
/// （Worktree は何も選べない位置に戻し、Diff は据え置く）。
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

/// diff ペインのカーソル。ファイルとその中のハンクを指す。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffCursor {
    pub file: usize,
    pub hunk: usize,
}

/// ブランチペインの状態。
#[derive(Debug, Default)]
pub struct BranchState {
    pub branches: Vec<Branch>,
    pub cursor: usize,
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
    LoadRepoInfo,
    LoadStatus,
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
            | JobKind::LoadRepoInfo
            | JobKind::LoadStatus
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
            JobKind::LoadRepoInfo => "repo",
            JobKind::LoadStatus => "status",
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
    BranchCreate {
        start: Option<String>,
        checkout: bool,
    },
    BranchRename {
        from: String,
    },
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
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_util::fake_snapshot;

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
    fn tab_cycles_only_through_the_implemented_panes() {
        // 未実装のペインに移れると、キーを受ける先の無い画面に入り込む。
        let mut pane = Pane::default();
        for _ in 0..Pane::FOCUSABLE.len() {
            assert!(Pane::FOCUSABLE.contains(&pane), "{pane:?}");
            pane = pane.next();
        }
        // 一周して戻る。
        assert_eq!(pane, Pane::default());
        assert_eq!(Pane::Graph.prev(), *Pane::FOCUSABLE.last().unwrap());
    }

    #[test]
    fn a_pane_outside_the_cycle_falls_back_to_the_first() {
        // 実装前のペインが `focus` に入っていても、`Tab` で巡回に戻れる。
        assert_eq!(Pane::Prs.next(), Pane::FOCUSABLE[1]);
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
