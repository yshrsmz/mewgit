//! ペインごとの描画。`ui::render` から呼ばれる。
//!
//! 各ペインは `AppState` を読むだけで、`Cmd` を作らない（不変ルール 2）。

use mewgit_core::app::{AppState, Pane};
use mewgit_core::auth::AccountState;
use ratatui::text::{Line, Span};

use crate::ui::theme;

pub mod branches;
pub mod commit;
pub mod diff;
pub mod graph;
pub mod prs;
pub mod worktree;

/// 右列の上のタブ見出し（07 の「右上のタブ」）。
///
/// **2 枚とも同じ関数が組む。** どちらのペインを描いていても見出しは同じものでなければ
/// ならず、ペインごとに組むと片方だけ古くなる。
///
/// **`config.github.enabled = false` のときは `Branches` だけ。** `Prs` が巡回候補から
/// 落ちるので、押しても切り替わらない見出しを並べないため（07）。判定は
/// [`AccountState::Disabled`] —— 旗を読む場所は `AppState::new` の 1 つ（01）。
///
/// **`Stash` は入らない。** M5 のペインなので、いま並べると 3 枚目が押せない見出しに
/// なる。
pub fn right_tabs(state: &AppState) -> Line<'static> {
    let branches = Span::styled(branches::title(state), tab_style(state, Pane::Branches));
    if matches!(state.account, AccountState::Disabled) {
        return Line::from(branches);
    }
    Line::from(vec![
        branches,
        Span::styled(" | ", theme::muted()),
        Span::styled(prs::title(), tab_style(state, Pane::Prs)),
    ])
}

/// いま出ていないタブは弱く出す。
fn tab_style(state: &AppState, tab: Pane) -> ratatui::style::Style {
    if state.right_tab == tab {
        ratatui::style::Style::default()
    } else {
        theme::muted()
    }
}
