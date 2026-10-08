//! `app` モジュール。設計は `docs/design/01-app-core.md` を参照。
//!
//! Elm アーキテクチャ。端末入力と非同期タスクの完了が [`Msg`] になり、[`update`] が
//! [`AppState`] を変更して [`Cmd`] を返す。ランタイムが [`Cmd`] を tokio タスクに
//! 変換し、結果をまた [`Msg`] で戻す。
//!
//! ここにある型は ratatui / crossterm に依存しない（不変ルール 6）。

mod cmd;
mod key;
pub mod keys;
mod msg;
mod state;
mod update;

pub use cmd::Cmd;
pub use key::{Key, KeyCode, Modifiers};
pub use keys::{HelpRow, HelpSection, KeyHint};
pub use msg::{ExecDone, Msg, RepoSnapshot};
pub use state::{
    AccountListing, AccountView, AppState, BranchState, CommitFiles, DiffCursor, DiffOrigin,
    DiffSource, DiffState, DiffView, GithubBlock, GithubJob, GraphLine, GraphSelection, GraphState,
    InputIntent, Job, JobKind, JobTracker, ListLoad, Modal, Motion, Notice, NoticeLevel, Pane,
    ParentChoice, PrField, PrHead, PrHeads, PrLoad, PrState, PushRejection, Region, Repick,
    RepoInfo, RewordInput, RightTab, StashState, TargetState, TermSize, TextBuffer, WatchState,
    WorktreeState,
};
pub use update::{init, progress_menu, update, MenuItem, ProgressMenu};
