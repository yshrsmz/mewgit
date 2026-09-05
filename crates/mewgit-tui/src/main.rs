//! mewgit エントリポイント。
//!
//! 隠しサブコマンド `--sequence-editor <path>` / `--commit-editor <path>` は
//! interactive rebase 用（`docs/design/03-git-exec.md`）。M5 で実装する。
//!
//! ここは配線だけで、実端末が必要なためテストを書かない（CLAUDE.md の例外）。

mod keymap;
mod modals;
mod panes;
mod runtime;
mod terminal;
mod ui;

use std::path::PathBuf;

use anyhow::Result;
use mewgit_core::app::{AppState, Notice, NoticeLevel, RepoInfo, TermSize};
use mewgit_core::config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    let (config, warnings) = Config::load();
    let _ = config; // M1 以降で使う。

    let mut state = AppState::new(RepoInfo {
        path: repo_path(),
        ..RepoInfo::default()
    });

    // 設定の警告は起動時に見せる。読み込み自体は失敗しない（08 参照）。
    for warning in warnings {
        state.notices.push_back(Notice {
            level: NoticeLevel::Warn,
            message: warning.to_string(),
            detail: None,
        });
    }

    // 起動時に Resize は飛んでこないので、初期サイズは自分で取る。
    if let Ok((width, height)) = crossterm::terminal::size() {
        state.term = TermSize { width, height };
    }

    let mut term = terminal::init()?;
    let result = runtime::run(&mut term, &mut state).await;
    // ループが Err で抜けても端末は必ず戻す。
    terminal::restore()?;
    result
}

/// 開くリポジトリ。引数が無ければカレントディレクトリ。
fn repo_path() -> PathBuf {
    std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
