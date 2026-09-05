//! アプリ全体の状態。設計は `docs/design/01-app-core.md`。
//!
//! `ui` はここにある値を描画するだけで、更新は [`update`](crate::app::update) が行う。

use std::collections::VecDeque;
use std::path::PathBuf;

use crate::auth::{AccountState, GhLogin, RepoTarget};
use crate::git_exec::RebasePlan;
use crate::git_read::{
    Branch, Commit, DiffTarget, HeadInfo, Hunk, InProgressOp, Remote, StashEntry, WorktreeSnapshot,
};
use crate::github::{Check, PullRequest};
use crate::graph::{GraphRow, LaneState};

use super::Msg;

/// アプリ全体の状態。
#[derive(Debug, Default)]
pub struct AppState {
    pub repo: RepoInfo,
    pub account: AccountState,
    pub graph: GraphState,
    pub worktree: WorktreeState,
    pub branches: BranchState,
    pub stashes: Vec<StashEntry>,
    pub prs: PrState,
    pub progress: Option<InProgressOp>,
    pub focus: Pane,
    pub modal: Option<Modal>,
    pub jobs: JobTracker,
    pub notices: VecDeque<Notice>,
    pub term: TermSize,
}

/// 開いているリポジトリ。
///
/// `git_read`（パス・リモート）と `auth`（ホスト名の正規化）の両方から組み立てるため、
/// どちらか一方の層ではなく `app` が持つ。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    pub commits: Vec<Commit>,
    /// 行ごとのレーン情報と、その行を処理した直後の `LaneState`。
    /// 追加ページは末尾の `LaneState` から計算を再開する（06 参照）。
    pub rows: Vec<(GraphRow, LaneState)>,
    pub cursor: usize,
    pub scroll: usize,
    /// 選択コミットの詳細を展開しているか。
    pub expanded: bool,
    /// これ以上読むコミットが無い。
    pub loaded_all: bool,
}

/// ワーキングツリーペインと diff ペインの状態。
#[derive(Debug, Default)]
pub struct WorktreeState {
    pub snapshot: WorktreeSnapshot,
    pub section: WorktreeSection,
    pub cursor: usize,
    /// 選択中ファイルの diff。未取得なら `None`。
    pub diff: Option<DiffView>,
}

/// ワーキングツリーペインのどの区画を見ているか。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WorktreeSection {
    Staged,
    #[default]
    Unstaged,
    Untracked,
    Conflicted,
}

/// 表示中の diff。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffView {
    pub target: DiffTarget,
    pub hunks: Vec<Hunk>,
    /// ハンク単位のカーソル。
    pub cursor: usize,
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
    /// 一覧タブの中身。
    pub list: Vec<PullRequest>,
    /// `current` のチェック一覧。
    pub checks: Vec<Check>,
    pub cursor: usize,
}

/// 実行中の非同期タスク。スピナー表示と `Cmd` の重複発行防止に使う。
#[derive(Debug, Default)]
pub struct JobTracker {
    pub running: Vec<Job>,
}

/// 実行中のタスク 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub kind: JobKind,
    /// ステータスバーに出す短い説明（例: `fetch`）。
    pub label: String,
}

/// タスクの種類。同種のものを二重に投げないための識別子。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    LoadLog,
    LoadStatus,
    LoadDiff,
    Exec,
    ResolveAccount,
    FetchPr,
    FetchChecks,
    FetchPrList,
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
#[derive(Debug)]
pub enum Modal {
    /// 破壊的操作の確認。`on_yes` は `state.modal.take()` でムーブして取り出す。
    Confirm {
        title: String,
        body: String,
        on_yes: Box<Msg>,
    },
    Input {
        title: String,
        prompt: String,
        buf: TextBuffer,
        on_submit: fn(String) -> Msg,
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
    fn default() -> Self {
        // 空でも 1 行はある。cursor が指す行が常に存在するようにするため。
        Self {
            lines: vec![String::new()],
            cursor: (0, 0),
        }
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
