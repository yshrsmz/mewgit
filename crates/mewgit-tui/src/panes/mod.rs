//! ペインごとの描画。`ui::render` から呼ばれる。
//!
//! 各ペインは `AppState` を読むだけで、`Cmd` を作らない（不変ルール 2）。

use std::borrow::Cow;

use mewgit_core::app::{AppState, RightTab};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Block;
use ratatui::Frame;

use crate::ui::{self, theme};

pub mod branches;
pub mod commit;
pub mod diff;
pub mod graph;
pub mod prs;
pub mod stash;
pub mod worktree;

/// 右列の上に出すタブの中身。
///
/// **タブ全部で共有する。** `Branches` / `Stash` / `Prs` は同じ領域を差し替えで使い、枠も
/// 高さの規則も同じなので、ペインごとに持つと 1 枚だけ変えられる（`.claude/rules/rust-coding.md` の「2 つ目の
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
/// （`.claude/rules/rust-coding.md` の「`None` の理由を 2 つ以上持たせない」）。
pub struct Rows {
    pub lines: Vec<Line<'static>>,
    pub cursor: Option<usize>,
}

impl TopPane {
    /// いま出すタブの行を組む。`width` は描く領域の幅（枠を含む）。
    ///
    /// **幅を受けるのは PRs の集計の上限のため**（`prs::lines`。07 の「PRs ペイン」）。
    /// 枠の内側の幅は描くときと同じ [`block`](Self::block) から出す。
    pub fn build(state: &AppState, width: u16) -> Self {
        let tab = state.right_tab;
        let inner = Self::block(state, tab)
            .inner(Rect::new(0, 0, width, 1))
            .width;
        let rows = match tab {
            RightTab::Branches => branches::lines(state),
            RightTab::Stash => stash::lines(state),
            RightTab::Prs => prs::lines(state, inner),
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
        let border_rows = probe.height - Self::block(state, self.tab).inner(probe).height;
        ui::capped_rows(self.rows.lines.len() + usize::from(border_rows), available)
    }

    /// 枠。行数を測るときと描くときで同じものを組む。
    ///
    /// **`focus` はタブと突き合わせる。** `focus == Pane::Branches` のように片方の
    /// 名前で見ると、揃っていない状態（`AppState::reseat_focus` が揃える）で枠が細く
    /// 描ける。
    ///
    /// **`self` を取らない** —— 組む前（[`build`](Self::build)）にも内側の幅を測る。
    fn block(state: &AppState, tab: RightTab) -> Block<'static> {
        ui::pane_block(
            right_tabs(state),
            state.focus == tab.pane(),
            state.config.graph.ascii,
        )
    }

    /// 描く。領域の高さは [`height`](Self::height) が決める。
    ///
    /// **カーソル行の反転はここ 1 箇所。** どのタブも同じ [`Rows`] を出すので、
    /// ペインごとに `theme::selected()` を当てると 1 枚だけ落ちる
    /// （実際に落ちた —— PRs ペインを一覧に書き直したときに反転が消え、`j` / `k` を
    /// 押しても画面が変わらないまま `o` がカーソル行を開いていた）。
    pub fn render(self, frame: &mut Frame, area: Rect, state: &AppState) {
        let block = Self::block(state, self.tab);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Rows { mut lines, cursor } = self.rows;
        if let Some(line) = cursor.and_then(|at| lines.get_mut(at)) {
            line.style = theme::selected();
        }
        // 選べる行が無いときは先頭を見せる（文言 1 行しか無い）。
        let at = cursor.unwrap_or(0);
        ui::render_built(frame, inner, lines, at..at + 1);
    }
}

/// 右列の上のタブ見出し（07 の「右上のタブ」）。
///
/// **どのタブも同じ関数が組む。** どのペインを描いていても見出しは同じものでなければ
/// ならず、ペインごとに組むと 1 枚だけ古くなる。
///
/// **出す見出しは巡回候補から導く。** `Tab` で行けないペインの見出しを並べないため
/// （07）。`config.github.enabled = false` のとき `Prs` が落ちるのはそちら
/// （`AppState::focus_candidates`）の判断で、ここでは数え直さない —— 数え直すと
/// 「出ている見出し」と「行ける先」が食い違っても何も落ちない。
///
/// **タブは常に 2 枚以上**（`Stash` は GitHub を切っても残る）ので、どれか 1 枚を
/// 強調する。
fn right_tabs(state: &AppState) -> Line<'static> {
    let mut spans = Vec::new();
    // **並びも候補から導く**（`Pane` の宣言順）。見出しは網羅 `match` で引くので、
    // タブを足せばここがコンパイルエラーになる。
    let tabs = state
        .focus_candidates()
        .iter()
        .filter_map(|&pane| RightTab::of(pane));
    for right_tab in tabs {
        let title: Cow<'static, str> = match right_tab {
            RightTab::Branches => branches::title(state),
            RightTab::Stash => stash::title(state),
            RightTab::Prs => prs::title().into(),
        };
        if !spans.is_empty() {
            spans.push(Span::styled("|", theme::muted()));
        }
        spans.extend(tab(state, right_tab, title));
    }
    Line::from(spans)
}

/// タブ見出しの名前と件数。**0 件なら名前だけ** —— 読み込み中や失敗のときに `(0)` を
/// 出すと、1 件も無いリポジトリと同じ見た目になる（本文で出し分けているのと同じ理由）。
/// Branches と Stash が同じ規則で、片方だけ直すと見出しの規則がずれる。
fn counted(name: &'static str, len: usize) -> Cow<'static, str> {
    match len {
        0 => name.into(),
        len => format!("{name} ({len})").into(),
    }
}

/// タブ 1 枚の見出し。いま出ているほうは前後 1 空白ごと反転し、出ていないほうは
/// 弱く出す（07 の「右上のタブ」）。
///
/// **空白は両方に付ける。** 出ているほうにだけ付けると、切り替えるたびに見出しの
/// 位置がずれる。空白を別の `Span` にするのは、見出しを `format!` で組み直さないため
/// （枠は毎フレーム組む）。
fn tab(state: &AppState, tab: RightTab, title: impl Into<Cow<'static, str>>) -> [Span<'static>; 3] {
    let style = if state.right_tab == tab {
        theme::current_tab()
    } else {
        theme::muted()
    };
    [
        Span::styled(" ", style),
        Span::styled(title, style),
        Span::styled(" ", style),
    ]
}
