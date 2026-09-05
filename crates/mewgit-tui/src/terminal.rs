//! 端末の初期化と復元。
//!
//! raw mode と代替画面の切替はここに閉じ込める。実端末が必要で単体テストできないため、
//! ここだけはテストを書かない（CLAUDE.md の例外）。

use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// raw mode に入り、代替画面に切り替えて `Terminal` を返す。
///
/// パニックフックも仕掛ける。復元してから既定のフックに渡すので、パニックメッセージは
/// 通常画面に出る。
pub fn init() -> Result<Tui> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    install_panic_hook();

    let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    Ok(terminal)
}

/// 代替画面を抜けて raw mode を解除する。
///
/// イベントループが `Err` で抜けたときも必ず通す。
pub fn restore() -> Result<()> {
    execute!(io::stdout(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}

/// パニック時に端末を戻してから既定のフックに渡す。
///
/// 順序が逆だと、パニックメッセージが raw mode の代替画面に出て読めない。
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // 復元に失敗しても、パニックの内容を出すほうを優先する。
        let _ = restore();
        previous(info);
    }));
}
