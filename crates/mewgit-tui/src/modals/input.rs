//! 1 行入力モーダルの描画。設計は `docs/design/07-ui.md` の「Input モーダル」。

use mewgit_core::app::TextBuffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::Frame;

use crate::modals;
use crate::ui;

/// キーの案内。`update::input_key` が受けるキーと揃える。
///
/// **操作名（作成 / リネーム）は書かない。** 枠のタイトルが言っているので、`Modal::Input`
/// に案内の文言まで持たせると同じことを 2 箇所で言い、片方だけ直った状態が生まれる（07）。
const HINT: modals::Hint<'static> = &[&["[Enter] 決定", "[Esc] やめる"]];

/// 入力欄に取る行数。1 行しか打てない（01 の「1 行入力」）。
const INPUT_ROWS: u16 = 1;

/// 1 行入力を描く。
pub fn render(frame: &mut Frame, area: Rect, title: &str, prompt: &str, buf: &TextBuffer) {
    let block = modals::block(title);
    // **prompt は折り返してから行数を数える**（`Confirm` の本文と同じ）。`桁数 / 幅` の
    // 切り上げで見積もると、単語境界で折れて増えたぶんを数え落として末尾が切れる。
    let (prompt_text, prompt_rows) =
        ui::wrapped(prompt, modals::inner_width(area, modals::WIDTH, &block));

    let body_area = modals::place(
        frame,
        area,
        block,
        modals::WIDTH,
        prompt_rows.saturating_add(INPUT_ROWS),
        HINT,
    );
    let [prompt_area, input_area] = Layout::vertical([
        Constraint::Length(prompt_rows),
        Constraint::Length(INPUT_ROWS),
    ])
    .areas(body_area);
    frame.render_widget(prompt_text, prompt_area);

    // **折り返さない**（07）。折り返すと `ui` が出す桁とバッファの列が食い違い、
    // カーソルの位置を出せなくなる。長い名前はカーソルの見えるところまで横にずらす。
    // 配線は `modals::edit_area`（`CommitMessage` と `PrCreate` が同じものを使う）。
    // **行は 1 本しか無い**ので、あちらの窓の切り出しはここでは 1 行を返すだけ。
    if let Some(position) = modals::edit_area(frame, input_area, buf) {
        frame.set_cursor_position(position);
    }
}

#[cfg(test)]
mod tests {
    use crate::test_util::{col_of, cursor, draw, row_of, state_with_modal};
    use mewgit_core::app::{AppState, InputIntent, Modal, TermSize, TextBuffer};
    use ratatui::text::Line;

    /// 既定の 80x24。
    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    /// 入力モーダルを出した状態。
    fn state_with_input(title: &str, prompt: &str, text: &str) -> AppState {
        state_with_modal(Modal::Input {
            title: title.to_string(),
            prompt: prompt.to_string(),
            buf: TextBuffer::from_text(text),
            intent: InputIntent::BranchCreate { start: None },
        })
    }

    #[test]
    fn the_modal_shows_the_title_prompt_input_and_keys() {
        let state = state_with_input("ブランチを作成", "新しいブランチの名前", "feat/x");

        let lines = draw(&state, SIZE);
        let text = lines.join("\n");

        assert!(text.contains("ブランチを作成"), "{text}");
        assert!(text.contains("新しいブランチの名前"), "{text}");
        assert!(text.contains("feat/x"), "{text}");
        assert!(text.contains("[Enter] 決定"), "{text}");
        // 確認を挟まずに打った文字が消えるので、押す前に見えている必要がある。
        assert!(text.contains("[Esc] やめる"), "{text}");
    }

    #[test]
    fn the_prompt_sits_above_the_input() {
        // 逆に並ぶと、打つ文字の意味の説明が入力欄の下に来る。**`contains` では
        // 見張れない**（どちらも画面には出ている）。
        let state = state_with_input("ブランチをリネーム", "main の新しい名前", "main");

        let lines = draw(&state, SIZE);

        assert!(
            row_of(&lines, "main の新しい名前") < cursor(&state, SIZE).y,
            "{lines:?}"
        );
    }

    #[test]
    fn the_terminal_cursor_sits_after_the_text_already_typed() {
        // `cursor` の列は文字数なので、全角を含む行ではそのまま桁にできない
        // （01 の `TextBuffer`）。桁で数えないと、全角のぶんだけ左にずれる。
        let state = state_with_input("ブランチを作成", "名前", "あいう");

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
    fn a_name_longer_than_the_box_scrolls_sideways_to_keep_the_cursor_visible() {
        // 折り返さないので、横にずらさないとカーソルが枠の外に出る。打っている
        // 文字が見えないまま入力を続けることになる。
        let long = format!("{}END", "x".repeat(120));
        let state = state_with_input("ブランチを作成", "名前", &long);

        let lines = draw(&state, SIZE);
        let text = lines.join("\n");

        assert!(text.contains("END"), "{text}");
        assert_eq!(cursor(&state, SIZE).y, row_of(&lines, "END"));
    }

    #[test]
    fn a_wrapped_prompt_is_not_cut_off_by_the_input_below_it() {
        // **prompt の行数を折り返し前で数えると 2 行目が消える。** 入力欄は正しい
        // 位置に残るので、「入力欄が押し出されていない」を見ても気付けない
        // （長いブランチ名は普通にあり、`from` はそのまま prompt に載る）。
        let prompt = format!("{} の新しい名前", "feat/".repeat(24));
        let state = state_with_input("ブランチをリネーム", &prompt, "topic");

        let lines = draw(&state, SIZE);
        let text = lines.join("\n");

        assert!(text.contains("の新しい名前"), "{text}");
        // 入力欄は折り返した prompt の下、キーの案内より上。
        assert!(
            row_of(&lines, "の新しい名前") < row_of(&lines, "topic"),
            "{text}"
        );
        assert!(
            row_of(&lines, "topic") < row_of(&lines, "[Enter] 決定"),
            "{text}"
        );
    }

    #[test]
    fn a_tiny_terminal_does_not_panic() {
        // リサイズの途中では枠より小さい端末が来る。**prompt と入力欄の縦割りは
        // このモーダルにしかない**ので、ここで見る。
        for (width, height) in [(1, 1), (10, 3), (20, 5)] {
            let state = state_with_input("ブランチを作成", "名前", "topic");
            let lines = draw(&state, TermSize { width, height });
            assert_eq!(lines.len(), height as usize);
        }
    }
}
