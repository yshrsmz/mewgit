//! ペインごとの描画。`ui::render` から呼ばれる。
//!
//! 各ペインは `AppState` を読むだけで、`Cmd` を作らない（不変ルール 2）。

use mewgit_core::app::{AppState, Pane, RightTab};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Block;
use ratatui::Frame;

use crate::ui::{self, theme};

pub mod branches;
pub mod commit;
pub mod diff;
pub mod graph;
pub mod prs;
pub mod worktree;

/// 右列の上に出すタブの中身。
///
/// **2 枚で共有する。** `Branches` と `Prs` は同じ領域を差し替えで使い、枠も高さの
/// 規則も同じなので、ペインごとに持つと片方だけ変えられる（`CLAUDE.md` の「2 つ目の
/// 利用者が出た時点で共有先を作る」）。各ペインが持つのは [`Rows`] を組む関数だけ。
///
/// **組むのは 1 フレームに 1 回。** `ui::render` が組んだものを高さの決定
/// （[`height`](Self::height)）と描画（[`render`](Self::render)）の両方に回す。
pub struct TopPane {
    tab: RightTab,
    rows: Rows,
}

/// 組んだ行と、カーソルが何行目か。
///
/// **`cursor` が `Option` なのは、選べる行が無い状態があるから。** 一覧が空のときに
/// 出すのは文言 1 行で、そこは選択肢ではない —— `0` で表すと「先頭の行を選んでいる」と
/// 同じ値になり、反転させる側が「文言を反転してよいか」を答えられなくなる
/// （`CLAUDE.md` の「`None` の理由を 2 つ以上持たせない」）。
pub struct Rows {
    pub lines: Vec<Line<'static>>,
    pub cursor: Option<usize>,
}

impl TopPane {
    /// いま出すタブの行を組む。
    pub fn build(state: &AppState) -> Self {
        let tab = state.right_tab;
        let rows = match tab {
            RightTab::Branches => branches::lines(state),
            RightTab::Prs => prs::lines(state),
        };
        TopPane { tab, rows }
    }

    /// 右列のうちこのタブに割く行数。枠の上下を含む。
    ///
    /// **上限は右列の半分**（07）。無いと一覧の長いリポジトリで下のペインが潰れる。
    /// **行数は組んだ行から数える** —— 別の式で数えると、文言や見出しの入れ方を
    /// 変えたときに片方だけ直しても通り、枠からはみ出すか余白が残る（#22）。
    ///
    /// **枠が食う行数を `+ 2` と手で書かない**（`modals::place` と同じ理由）。あとで
    /// `Padding` を足すと、割く高さだけが古いまま残って最後の 1 行が黙って消える。
    pub fn height(&self, state: &AppState, available: u16) -> u16 {
        let probe = Rect::new(0, 0, 0, u16::MAX);
        let border_rows = probe.height - self.block(state).inner(probe).height;
        ui::capped_rows(self.rows.lines.len() + usize::from(border_rows), available)
    }

    /// 枠。行数を測るときと描くときで同じものを組む。
    ///
    /// **`focus` はタブと突き合わせる。** `focus == Pane::Branches` のように片方の
    /// 名前で見ると、揃っていない状態（`AppState::reseat_focus` が揃える）で枠が細く
    /// 描ける。
    fn block(&self, state: &AppState) -> Block<'static> {
        ui::pane_block(right_tabs(state), state.focus == self.tab.pane())
    }

    /// 描く。領域の高さは [`height`](Self::height) が決める。
    ///
    /// **カーソル行の反転はここ 1 箇所。** `Branches` と `Prs` は同じ [`Rows`] を
    /// 出すので、ペインごとに `theme::selected()` を当てると片方だけ落ちる
    /// （実際に落ちた —— PRs ペインを一覧に書き直したときに反転が消え、`j` / `k` を
    /// 押しても画面が変わらないまま `o` がカーソル行を開いていた）。
    pub fn render(self, frame: &mut Frame, area: Rect, state: &AppState) {
        let block = self.block(state);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Rows { mut lines, cursor } = self.rows;
        if let Some(line) = cursor.and_then(|at| lines.get_mut(at)) {
            line.style = theme::selected();
        }
        // 選べる行が無いときは先頭を見せる（文言 1 行しか無い）。
        let at = cursor.unwrap_or(0);
        ui::render_rows(frame, inner, lines, at..at + 1);
    }
}

/// 右列の上のタブ見出し（07 の「右上のタブ」）。
///
/// **2 枚とも同じ関数が組む。** どちらのペインを描いていても見出しは同じものでなければ
/// ならず、ペインごとに組むと片方だけ古くなる。
///
/// **出す見出しは巡回候補から導く。** `Tab` で行けないペインの見出しを並べないため
/// （07）。`config.github.enabled = false` のとき `Prs` が落ちるのはそちら
/// （`AppState::focus_candidates`）の判断で、ここでは数え直さない —— 数え直すと
/// 「出ている見出し」と「行ける先」が食い違っても何も落ちない。
///
/// **`Stash` は入らない。** M5 のペインで、まだ候補に入っていない。
fn right_tabs(state: &AppState) -> Line<'static> {
    let branches = Span::styled(branches::title(state), tab_style(state, RightTab::Branches));
    if !state.focus_candidates().contains(&Pane::Prs) {
        return Line::from(branches);
    }
    Line::from(vec![
        branches,
        Span::styled(" | ", theme::muted()),
        Span::styled(prs::title(), tab_style(state, RightTab::Prs)),
    ])
}

/// いま出ていないタブは弱く出す。
fn tab_style(state: &AppState, tab: RightTab) -> Style {
    if state.right_tab == tab {
        Style::default()
    } else {
        theme::muted()
    }
}
