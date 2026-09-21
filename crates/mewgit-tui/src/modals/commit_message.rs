//! コミットメッセージのモーダルの描画。
//! 設計は `docs/design/07-ui.md` の「CommitMessage モーダル」。

use mewgit_core::app::TextBuffer;
use ratatui::layout::Rect;
use ratatui::Frame;

use crate::modals;

/// キーの案内。`update::commit_message_key` が受けるキーと揃える。
///
/// **`Shift+Enter` と `Ctrl-J` の両方を出す。** 前者は端末が `Enter` と区別して
/// 送れるときだけ届き、後者はどの端末でも届く（01 の「コミットメッセージの編集」）。
/// 片方だけ書くと、届かない端末の人に打つ手が無いか、届く端末の人が慣れたキーを
/// 見つけられない。
const HINT: &str = "[Enter] コミット   [Shift+Enter / Ctrl-J] 改行   [Esc] やめる";

/// モーダルの幅。狭い端末では画面の幅まで縮む。
///
/// `Confirm` より広く取るのは、git のコミットメッセージが本文を 72 桁で折る慣習だから。
/// そこが見えない箱だと折る位置を目で測れない。
const WIDTH: u16 = 76;

/// コミットメッセージの編集を描く。
pub fn render(frame: &mut Frame, area: Rect, buf: &TextBuffer, amend: bool) {
    // **どちらの操作かを枠に出す。** amend は HEAD を書き換えるので、新規コミットの
    // つもりで確定させない（07）。
    let title = if amend {
        "Commit message (amend)"
    } else {
        "Commit message"
    };
    // 折り返さないので、本文の行数はそのままバッファの行数（下の `Paragraph`）。
    let body_rows = u16::try_from(buf.lines.len()).unwrap_or(u16::MAX);
    let body_area = modals::place(frame, area, modals::block(title), WIDTH, body_rows, HINT);

    // **`ui::render_rows` は使わない。** あちらは横位置を持たず、diff と一覧は
    // それでよい（長い行は右で切れるだけ）。編集中はカーソルが右端を越えるので、
    // 越えたぶんだけ横にずらす必要がある。窓の切り出しと配線は
    // `modals::edit_area`（`Input` と `PrCreate` が同じものを使う）。
    if let Some(position) = modals::edit_area(frame, body_area, buf) {
        frame.set_cursor_position(position);
    }
}

#[cfg(test)]
mod tests {
    use crate::test_util::{col_of, cursor, draw, row_of, state_with_modal};
    use mewgit_core::app::{AppState, Modal, TermSize, TextBuffer};
    use ratatui::text::Line;

    /// 既定の 80x24。
    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    /// コミットメッセージのモーダルを出した状態。
    fn state_with_message(text: &str, amend: bool) -> AppState {
        state_with_modal(Modal::CommitMessage {
            buf: TextBuffer::from_text(text),
            amend,
        })
    }

    #[test]
    fn the_modal_shows_the_message_and_the_keys() {
        let state = state_with_message("fix(app): 直す\n\n本文", false);

        let lines = draw(&state, SIZE);
        let text = lines.join("\n");

        assert!(text.contains("Commit message"), "{text}");
        assert!(text.contains("fix(app): 直す"), "{text}");
        assert!(text.contains("本文"), "{text}");
        // 届くキーは端末で違うので、両方を出す（07）。
        assert!(text.contains("[Enter] コミット"), "{text}");
        assert!(text.contains("Shift+Enter / Ctrl-J"), "{text}");
        // 確認を挟まずに本文が消えるので、押す前に見えている必要がある。
        assert!(text.contains("[Esc] やめる"), "{text}");
    }

    #[test]
    fn amend_says_so_in_the_title() {
        // amend は HEAD を書き換えるので、新規コミットのつもりで確定させない（07）。
        let plain = draw(&state_with_message("x", false), SIZE).join("\n");
        let amend = draw(&state_with_message("x", true), SIZE).join("\n");

        assert!(!plain.contains("amend"), "{plain}");
        assert!(amend.contains("Commit message (amend)"), "{amend}");
    }

    #[test]
    fn the_body_does_not_push_the_keys_out_of_the_box() {
        // 本文とキーの案内を 1 つの Paragraph にまとめると、本文が伸びたぶんだけ
        // 案内が枠の外へ出る（`confirm` と同じ）。
        let body = (0..40)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let state = state_with_message(&body, false);

        let lines = draw(&state, SIZE);
        let text = lines.join("\n");

        assert!(text.contains("[Enter] コミット"), "{text}");
        // 枠の下辺より上にあること。`"└"` だけを探すと、下のグラフペインの左下角が
        // 同じ行にあって当たる（枠が画面いっぱいのときに重なる）。
        assert!(
            row_of(&lines, "[Enter] コミット") < row_of(&lines, "└────────"),
            "{text}"
        );
    }

    #[test]
    fn a_body_taller_than_the_box_scrolls_to_the_cursor() {
        // 見えない行にカーソルがあると、何を打っているのか判らない。
        let body = (0..40)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut state = state_with_message(&body, false);
        let Some(Modal::CommitMessage { buf, .. }) = &mut state.modal else {
            unreachable!()
        };
        buf.cursor = (39, 0);

        let lines = draw(&state, SIZE);
        let text = lines.join("\n");

        assert!(text.contains("line 39"), "{text}");
        assert_eq!(cursor(&state, SIZE).y, row_of(&lines, "line 39"));
    }

    #[test]
    fn the_terminal_cursor_sits_after_the_wide_characters_already_typed() {
        // `cursor` の列は文字数なので、全角を含む行ではそのまま桁にできない
        // （01 の `TextBuffer`）。桁で数えないと、全角のぶんだけ左にずれる。
        let state = state_with_message("あいう", false);

        let lines = draw(&state, SIZE);
        let x = cursor(&state, SIZE).x;
        let row = &lines[row_of(&lines, "あいう") as usize];

        // 「あいう」の直後。**バイト位置ではなく桁で測る**（枠の `│` は 3 バイト。
        // 測り方は `test_util::col_of` の 1 本）。
        assert_eq!(
            x,
            col_of(row, "あいう") + Line::from("あいう").width() as u16
        );
    }

    #[test]
    fn a_line_longer_than_the_box_scrolls_sideways_to_keep_the_cursor_visible() {
        // 折り返さないので、横にずらさないとカーソルが枠の外に出る。打っている
        // 文字が見えないまま入力を続けることになる。
        let long = format!("{}END", "x".repeat(120));
        let state = state_with_message(&long, false);

        let lines = draw(&state, SIZE);
        let text = lines.join("\n");

        assert!(text.contains("END"), "{text}");
        assert_eq!(cursor(&state, SIZE).y, row_of(&lines, "END"));
    }

    #[test]
    fn a_tiny_terminal_does_not_panic() {
        // リサイズの途中では枠より小さい端末が来る。**編集中のモーダルごとに
        // 1 本ずつ見る。** 窓が 0 行になったときにカーソルを置かない分岐は
        // モーダルごとに形が違い（こちらは窓が空か、`Input` は入力欄の高さが 0 か）、
        // 下を覆うことと中央寄せだけが `modals::place` の担当（`confirm` が見ている）。
        for (width, height) in [(1, 1), (10, 3), (20, 5)] {
            let state = state_with_message("本文\n2 行目", false);
            let lines = draw(&state, TermSize { width, height });
            assert_eq!(lines.len(), height as usize);
        }
    }
}
