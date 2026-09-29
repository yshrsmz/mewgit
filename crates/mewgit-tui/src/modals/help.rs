//! キーの一覧モーダルの描画。設計は `docs/design/07-ui.md` の「Help モーダル」。
//!
//! **行は core が組む**（`keys::help`）。ここは字にして `modals::picker` に並べるだけ。

use mewgit_core::app::{keys, HelpRow, Pane};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::modals::picker::{self, Picker};
use crate::ui::{self, theme};

const TITLE: &str = "キー";

/// キーの案内。`update::picker_key` が受けるキーと揃える（`Enter` は何もしない）。
const HINT: &str = "[j/k] 移動   [Esc] 閉じる";

/// キーの字の桁。**いちばん長い `Tab/Shift+Tab` に 2 桁足す**（説明の頭を揃える）。
const KEY_CELLS: usize = "Tab/Shift+Tab".len() + 2;

/// `pane` のキーとどこでも効くキーを並べる。
pub fn render(frame: &mut Frame, area: Rect, pane: Pane, cursor: usize) {
    picker::render(
        frame,
        area,
        Picker {
            title: TITLE,
            body: "".into(),
            rows: keys::help(pane).into_iter().map(row).collect(),
            cursor,
            hint: HINT,
        },
    );
}

/// 1 行。見出しは淡色（`theme::muted`）、キーは `  j/k          移動`。
fn row(row: HelpRow) -> Line<'static> {
    match row {
        HelpRow::Heading(title) => Line::from(Span::styled(title, theme::muted())),
        HelpRow::Key(hint) => {
            let mut keys = String::new();
            ui::push_keys(&mut keys, hint);
            Line::from(vec![
                Span::raw(format!("  {keys:<KEY_CELLS$}")),
                Span::raw(hint.label()),
            ])
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::test_util::{draw, row_is_reversed, state_with_modal};
    use mewgit_core::app::{AppState, Modal, Pane, TermSize};

    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    fn state_with_help(pane: Pane, cursor: usize) -> AppState {
        let mut state = state_with_modal(Modal::Help { cursor });
        state.focus = pane;
        state
    }

    #[test]
    fn the_help_lists_the_pane_keys_and_the_keys_that_work_anywhere() {
        let state = state_with_help(Pane::Graph, 0);

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("このペイン"), "{text}");
        assert!(text.contains("ブランチ作成"), "{text}");
        assert!(text.contains("どこでも"), "{text}");
        // フッターから外した共通キーと、表に足した `m` / `Tab` / `Ctrl-C`。
        for key in [
            "fetch",
            "account",
            "通知",
            "進行中操作のメニュー",
            "Tab/Shift+Tab",
        ] {
            assert!(text.contains(key), "{key} がありません: {text}");
        }
        assert!(text.contains("[Esc] 閉じる"), "{text}");
    }

    #[test]
    fn the_whole_list_fits_at_80x24_even_for_the_longest_pane() {
        // Worktree はペインのキーがいちばん多い。本文を持たないので空行を取らず、80x24 で
        // 末尾の `q` まで入る（07 の「Help モーダル」）。
        let text = draw(&state_with_help(Pane::Worktree, 0), SIZE).join("\n");
        assert!(text.contains("中断 / 終了"), "{text}");
        assert!(text.contains("quit"), "{text}");
    }

    #[test]
    fn the_bottom_rows_come_into_view_as_the_cursor_moves_down() {
        // 低い端末では全部が 1 画面に入らない。窓がカーソルに付いてこないと、末尾の `q` と
        // `Ctrl-C` が見えない。
        let short = TermSize {
            width: 80,
            height: 16,
        };
        let top = draw(&state_with_help(Pane::Worktree, 0), short).join("\n");
        assert!(!top.contains("中断 / 終了"), "最初から入っています: {top}");

        let last = mewgit_core::app::keys::help(Pane::Worktree).len() - 1;
        let state = state_with_help(Pane::Worktree, last);
        let bottom = draw(&state, short).join("\n");

        assert!(bottom.contains("中断 / 終了"), "{bottom}");
        assert!(bottom.contains("Ctrl-C"), "{bottom}");
        assert!(
            row_is_reversed(&state, short, "quit"),
            "カーソル行が反転していない"
        );
    }
}
