//! 進行中操作のメニューの描画。設計は `docs/design/07-ui.md` の
//! 「ProgressMenu モーダル」。
//!
//! **箱の中の割り方は `modals::picker` と共有する**（`RemotePicker` / `AccountPicker` と
//! 3 人）。何を並べるかは core の `progress_menu` が決め、ここは並べるだけ。

use mewgit_core::app::{MenuItem, ProgressMenu};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::modals::picker::{self, Picker};

/// キーの案内。`update::picker_key` と `update::progress::progress_picked` が受けるキーと揃える。
const HINT: &str = "[j/k] 選ぶ   [Enter] 実行   [Esc] やめる";

/// git の語の桁。**いちばん長い `continue` に 2 桁足す**（説明の頭を揃える）。
const WORD_CELLS: usize = "continue".len() + 2;

/// 進行中操作から出る手を並べる。
pub fn render(frame: &mut Frame, area: Rect, menu: &ProgressMenu, cursor: usize) {
    picker::render(
        frame,
        area,
        Picker {
            title: &menu.title,
            body: menu.body.into(),
            rows: menu.items.iter().map(row).collect(),
            cursor,
            hint: HINT,
        },
    );
}

/// 1 項目の行。`continue  解消した内容で続ける`。
fn row(item: &MenuItem) -> Line<'static> {
    Line::from(vec![
        Span::raw(format!("{:<WORD_CELLS$}", item.word)),
        Span::raw(item.about),
    ])
}

#[cfg(test)]
mod tests {
    use crate::test_util::{draw, row_is_reversed, state_with_modal};
    use mewgit_core::app::{progress_menu, AppState, Modal, TermSize};
    use mewgit_core::git_read::{InProgressOp, Step};
    use mewgit_core::test_util::fake_oid;

    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    fn rebase() -> InProgressOp {
        InProgressOp::Rebase {
            step: Some(Step {
                current: 1,
                total: 2,
            }),
            branch: Some("topic".to_string()),
        }
    }

    fn state_with_menu(progress: &InProgressOp, cursor: usize) -> AppState {
        state_with_modal(Modal::ProgressMenu {
            menu: progress_menu(progress).expect("項目がありません"),
            cursor,
        })
    }

    #[test]
    fn the_menu_shows_what_is_in_progress_and_every_way_out() {
        let state = state_with_menu(&rebase(), 0);

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("rebase の途中"), "{text}");
        assert!(text.contains("git add してから continue"), "{text}");
        for line in [
            "continue  解消した内容で続ける",
            "skip      止まったコミットを捨てて続ける",
            "abort     rebase を始める前に戻す",
        ] {
            assert!(text.contains(line), "{line}\n{text}");
        }
        assert!(text.contains("[Enter] 実行"), "{text}");
    }

    #[test]
    fn a_merge_says_how_to_conclude_it() {
        // continue に当たる項目が無いので、結び方を言わないと出口が abort だけに読める（07）。
        let state = state_with_menu(&InProgressOp::Merge { oid: fake_oid(1) }, 0);

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("merge の途中"), "{text}");
        assert!(text.contains("結ぶときは commit してください"), "{text}");
        assert!(text.contains("abort     merge を始める前に戻す"), "{text}");
        assert!(!text.contains("continue"), "{text}");
    }

    #[test]
    fn the_cursor_row_is_highlighted() {
        let state = state_with_menu(&rebase(), 1);

        assert!(row_is_reversed(&state, SIZE, "skip"));
        assert!(!row_is_reversed(&state, SIZE, "continue  解消"));
    }
}
