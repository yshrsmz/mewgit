//! アプリ全体の状態。設計は `docs/design/01-app-core.md`。
//!
//! `ui` はここにある値を描画するだけで、更新は [`update`](crate::app::update) が行う。

use std::collections::VecDeque;
use std::path::PathBuf;

use crate::auth::{AccountState, GhLogin, RepoTarget};
use crate::git_exec::RebasePlan;
use crate::git_read::{
    Branch, Commit, CommitDetail, DiffTarget, FileDiff, HeadInfo, InProgressOp, RefIndex, Remote,
    StashEntry, WorktreeSection, WorktreeSnapshot,
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
    pub diff: Option<DiffView>,
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
            diff: None,
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
                        Cmd::None
                    }
                }
            },
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
#[derive(Debug, Default)]
pub struct WorktreeState {
    pub snapshot: WorktreeSnapshot,
    pub section: WorktreeSection,
    pub cursor: usize,
}

/// 表示中の diff。何の diff かは `target` が示す。
#[derive(Debug, PartialEq, Eq)]
pub struct DiffView {
    pub target: DiffTarget,
    /// ファイル単位で持つ。平坦化すると `GitOp::StageHunk` に渡す `path` を失う。
    pub files: Vec<FileDiff>,
    pub cursor: DiffCursor,
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
