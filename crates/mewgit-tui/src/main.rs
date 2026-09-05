//! mewgit エントリポイント。
//!
//! 隠しサブコマンド `--sequence-editor <path>` / `--commit-editor <path>` は
//! interactive rebase 用（`docs/design/03-git-exec.md`）。M5 で実装する。
//!
//! ここは配線だけで、実端末が必要なためテストを書かない（CLAUDE.md の例外）。

mod input;
mod modals;
mod panes;
mod runtime;
mod ui;

use std::path::PathBuf;

use anyhow::{bail, Result};
use mewgit_core::app::{AppState, NoticeLevel, RepoInfo, TermSize};
use mewgit_core::config::Config;
use ratatui::crossterm::event::EventStream;

#[tokio::main]
async fn main() -> Result<()> {
    let (config, warnings) = Config::load();

    let mut state = AppState::new(
        RepoInfo {
            path: repo_path()?,
            ..RepoInfo::default()
        },
        config,
    );

    // 設定の警告は起動時に見せる。読み込み自体は失敗しない（08 参照）。
    for warning in warnings {
        state.push_notice(NoticeLevel::Warn, warning.to_string(), None);
    }

    // raw mode / 代替画面 / パニックフックは ratatui が持っている。
    let mut terminal = ratatui::try_init()?;

    // 起動時に Resize は飛んでこないので、初期サイズは自分で取る。
    if let Ok(size) = terminal.size() {
        state.term = TermSize {
            width: size.width,
            height: size.height,
        };
    }

    let result = runtime::run(&mut terminal, EventStream::new(), &mut state).await;
    // restore は () を返すので、ループのエラーを後始末のエラーで踏み潰さない。
    ratatui::restore();
    result
}

/// 開くリポジトリ。引数が無ければカレントディレクトリ。
///
/// 絶対パスに直すのは、`.` や `..` のままだと `file_name()` が `None` になり、
/// ヘッダに `.` と出てしまうため。
fn repo_path() -> Result<PathBuf> {
    let raw = match std::env::args_os().nth(1) {
        Some(arg) => {
            // まだオプションを解釈しないので、`--help` という名前のリポジトリを
            // 開きにいかないよう弾く。引数解析は M5 の隠しサブコマンドと一緒に入れる。
            if arg.to_string_lossy().starts_with('-') {
                bail!(
                    "オプションはまだ対応していません: {}",
                    arg.to_string_lossy()
                );
            }
            PathBuf::from(arg)
        }
        None => PathBuf::from("."),
    };

    Ok(std::fs::canonicalize(&raw).unwrap_or(raw))
}
