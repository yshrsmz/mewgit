//! キーの一覧モーダルの描画。設計は `docs/design/07-ui.md` の「Help モーダル」。
//!
//! **行は core が組む**（`PaneKeys::help`）。ここは字にして `modals::picker` に並べるだけ。

use mewgit_core::app::{HelpRow, HelpSection, KeyHint, PaneKeys};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::modals;
use crate::modals::picker::{self, Picker};
use crate::ui::{self, theme};

const TITLE: &str = "キー";

/// キーの案内。`update::picker_key` が受けるキーと揃える（`Enter` は何もしない）。
const HINT: modals::Hint<'static> = &[&["[j/k] 移動", "[Esc] 閉じる"]];

/// 開いたペインのキーとどこでも効くキーを並べる。
pub fn render(frame: &mut Frame, area: Rect, keys: &PaneKeys, cursor: usize, charset: ui::Charset) {
    let rows = keys.help();
    // **キーの字の桁は並べる行から導く**（いちばん長いキーに 2 桁足して、説明の頭を揃える）。
    let key_cells = rows
        .iter()
        .filter_map(|row| match row {
            HelpRow::Key(hint) => Some(keys_text(hint).len()),
            HelpRow::Heading(_) => None,
        })
        .max()
        .unwrap_or(0)
        + 2;
    picker::render(
        frame,
        area,
        Picker {
            title: TITLE,
            width: modals::WIDTH,
            body: "".into(),
            rows: rows.into_iter().map(|row| line(row, key_cells)).collect(),
            cursor,
            hint: HINT,
        },
        charset,
    );
}

/// 1 行。見出しは淡色（`theme::muted`）、キーは `  j/k          移動`。
fn line(row: HelpRow<'_>, key_cells: usize) -> Line<'static> {
    match row {
        HelpRow::Heading(section) => Line::from(Span::styled(heading(section), theme::muted())),
        HelpRow::Key(hint) => Line::from(vec![
            Span::raw(format!("  {:<key_cells$}", keys_text(hint))),
            Span::raw(hint.label()),
        ]),
    }
}

/// 段の見出し。
fn heading(section: HelpSection) -> &'static str {
    match section {
        HelpSection::Pane => "このペイン",
        HelpSection::Anywhere => "どこでも",
    }
}

/// キーの字（`j/k`）。**ASCII だけ**なので、桁はバイト数で数えてよい。
fn keys_text(hint: &KeyHint) -> String {
    let mut out = String::new();
    ui::push_keys(&mut out, hint);
    out
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
        state_with_modal(Modal::Help { pane, cursor })
    }

    #[test]
    fn the_help_lists_the_pane_keys_and_the_keys_that_work_anywhere() {
        let state = state_with_help(Pane::Graph, 0);

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("cherry-pick"), "{text}");
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

        let last = state_with_help(Pane::Worktree, 0)
            .keymap
            .pane(Pane::Worktree)
            .help()
            .len()
            - 1;
        let state = state_with_help(Pane::Worktree, last);
        let bottom = draw(&state, short).join("\n");

        assert!(bottom.contains("中断 / 終了"), "{bottom}");
        assert!(bottom.contains("Ctrl-C"), "{bottom}");
        assert!(
            row_is_reversed(&state, short, "quit"),
            "カーソル行が反転していない"
        );
    }

    #[test]
    fn the_list_is_of_the_pane_it_was_opened_on_even_if_the_focus_moved() {
        // 開いているあいだに届いた status で `reseat_focus` がフォーカスを移しても、一覧は
        // 開いたときのペインのまま（`Modal::Help` の doc）。
        let mut state = state_with_help(Pane::Worktree, 0);
        state.focus = Pane::Graph;

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("stage"), "{text}");
        assert!(!text.contains("cherry-pick"), "{text}");
    }
}
