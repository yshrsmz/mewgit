//! mewgit エントリポイント。
//!
//! 隠しサブコマンド `--sequence-editor <path>` / `--commit-editor <path>` は
//! interactive rebase 用（`docs/design/03-git-exec.md`）。M5 で実装する。
//!
//! ここは配線だけで、実端末が必要なためテストを書かない（CLAUDE.md の例外）。
//! 分岐のある処理は `runtime` 側に置く。

mod input;
mod modals;
mod panes;
mod runtime;
#[cfg(test)]
mod test_util;
mod ui;

use std::path::PathBuf;

use anyhow::{bail, Result};
use mewgit_core::app::NoticeLevel;
use mewgit_core::config::Config;
use ratatui::crossterm::event::{
    EventStream, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;

#[tokio::main]
async fn main() -> Result<()> {
    let (config, warnings) = Config::load();
    let (mut state, reader) = runtime::open(&repo_path()?, config)?;

    // 設定の警告は起動時に見せる。読み込み自体は失敗しない（08 参照）。
    for warning in warnings {
        state.push_notice(NoticeLevel::Warn, warning.to_string(), None);
    }

    // raw mode / 代替画面 / パニックフックは ratatui が持っている。
    let mut terminal = ratatui::try_init()?;
    push_keyboard_enhancement();

    let result = runtime::run(&mut terminal, EventStream::new(), &mut state, reader).await;

    let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    // restore は () を返すので、ループのエラーを後始末のエラーで踏み潰さない。
    ratatui::restore();
    result
}

/// kitty keyboard protocol を要求する。
///
/// 有効になると `Shift+Enter` が `Enter` と区別して届く（`CSI 13;2u`）。コミット
/// メッセージの改行に使う（01 の「コミットメッセージの編集」）。対応しない端末では
/// 何も起きず、そこでは `Alt+Enter`（`ESC CR`）と `Ctrl-J` が改行になる。
///
/// **対応するかを問い合わせない。** `crossterm::terminal::supports_keyboard_enhancement`
/// は端末に `CSI ?u` を投げて応答を待ち、**応答しない端末では 2 秒待ってから諦める**
/// （crossterm 0.28.1 の `terminal/sys/unix.rs`）。実測で起動が 0.82 秒 → 2.04 秒に
/// なった。対応しない端末ほど黒い画面を長く見ることになる。`CSI > 1 u` は private な
/// CSI なので、解さない端末は読み飛ばす。押せたかを知る必要も無い（`Enter` の意味は
/// どちらでも変わらず、増えるのは受けられる改行キーだけ）。
///
/// 押す旗は `DISAMBIGUATE_ESCAPE_CODES` だけにする。`REPORT_EVENT_TYPES` まで押すと
/// 離鍵も届き、1 打鍵が 2 つの `Msg::Key` になる。
fn push_keyboard_enhancement() {
    let _ = execute!(
        std::io::stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
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
