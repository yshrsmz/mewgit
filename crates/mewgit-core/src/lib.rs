//! mewgit core: UI に依存しないロジック。
//!
//! レイヤー構成は `docs/design/00-overview.md` を参照。
//! - 読み取りは [`git_read`]（git2）、書き込みは [`git_exec`]（git CLI）。
//! - [`ui`] 相当は `mewgit-tui` クレート側にあり、ここには含めない。

pub mod app;
pub mod auth;
pub mod config;
pub mod git_exec;
pub mod git_read;
pub mod github;
pub mod graph;
