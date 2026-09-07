//! コミットメッセージのモーダルの描画。
//! 設計は `docs/design/07-ui.md` の「CommitMessage モーダル」。

use mewgit_core::app::TextBuffer;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::modals::centered;
use crate::ui;

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

/// 本文とキーの案内のあいだに空ける行数。
const GAP_ROWS: u16 = 1;

/// キーの案内に取る行数。高さの見積もりと `Layout` で同じものを使う。
const HINT_ROWS: u16 = 1;

/// コミットメッセージの編集を描く。
///
/// 下のペインを `Clear` で消してから描く。覆わずに重ねると下の文字が透けて読めなくなる。
pub fn render(frame: &mut Frame, area: Rect, buf: &TextBuffer, amend: bool) {
    // **どちらの操作かを枠に出す。** amend は HEAD を書き換えるので、新規コミットの
    // つもりで確定させない（07）。
    let title = if amend {
        "Commit message (amend)"
    } else {
        "Commit message"
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().add_modifier(Modifier::BOLD));

    let width = WIDTH.min(area.width);
    // 枠が食う行数は `Block` 自身から出す（`confirm` と同じ理由）。
    let probe = Rect::new(0, 0, width, u16::MAX);
    let border_rows = probe.height - block.inner(probe).height;

    // 本文 + 空行 + キーの案内 + 枠。画面に収まらない分は `centered` が削る。
    let body_rows = u16::try_from(buf.lines.len()).unwrap_or(u16::MAX);
    let height = body_rows.saturating_add(GAP_ROWS + HINT_ROWS + border_rows);
    let area = centered(area, width, height);
    let inner = block.inner(area);

    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    // キーの案内は本文とは別に最下行へ描く（`confirm` と同じ理由。1 つの
    // `Paragraph` にまとめると、本文が伸びたときに案内だけが枠の外へ出る）。
    let [body_area, hint_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(HINT_ROWS)]).areas(inner);
    frame.render_widget(Paragraph::new(Line::from(HINT)), hint_area);

    let (row, col) = buf.cursor;
    // 本文が枠より高ければ、カーソルの行が入るところまでずらす。
    let shown = ui::window(buf.lines.len(), row..row + 1, body_area.height as usize);

    // **`ui::render_rows` は使わない。** あちらは横位置を持たず、diff と一覧は
    // それでよい（長い行は右で切れるだけ）。編集中はカーソルが右端を越えるので、
    // 越えたぶんだけ横にずらす必要がある。
    let cursor_col = display_col(&buf.lines[row], col);
    let scroll_x = cursor_col.saturating_sub(body_area.width.saturating_sub(1));

    let lines: Vec<Line<'_>> = buf.lines[shown.clone()]
        .iter()
        .map(|line| Line::from(line.as_str()))
        .collect();
    // **折り返さない**（07）。折り返すと `ui` が出す桁とバッファの列が食い違い、
    // カーソルの位置を出せなくなる。
    frame.render_widget(Paragraph::new(lines).scroll((0, scroll_x)), body_area);

    // **端末のカーソルを本文の位置に置く。** 反転の升目を自前で描かない。編集中は
    // 端末側のカーソルが出ているのが自然で、行を選ぶ `Confirm` とは別物（07）。
    if let Some(offset) = row
        .checked_sub(shown.start)
        .filter(|_| shown.contains(&row))
    {
        frame.set_cursor_position(Position {
            x: body_area.x + cursor_col - scroll_x,
            y: body_area.y + u16::try_from(offset).unwrap_or(u16::MAX),
        });
    }
}

/// カーソルの左にある本文の表示桁数。
///
/// `cursor` の列は文字数なので、全角を含む行ではそのまま桁にできない（01 の
/// `TextBuffer`）。桁の計算は ratatui の [`Line::width`] に任せ、描画と同じ数え方に
/// する。
fn display_col(line: &str, col: usize) -> u16 {
    let prefix: String = line.chars().take(col).collect();
    u16::try_from(Line::from(prefix).width()).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use crate::test_util::{draw, row_of, state_with_commits, terminal_with};
    use mewgit_core::app::{AppState, Modal, TermSize, TextBuffer};
    use ratatui::layout::Position;
    use ratatui::text::Line;

    /// 既定の 80x24。
    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    /// コミットメッセージのモーダルを出した状態。
    fn state_with_message(text: &str, amend: bool) -> AppState {
        let mut state = state_with_commits(&["first", "second"]);
        state.modal = Some(Modal::CommitMessage {
            buf: TextBuffer::from_text(text),
            amend,
        });
        state
    }

    /// 端末カーソルの位置。
    fn cursor(state: &AppState) -> Position {
        terminal_with(state, SIZE)
            .get_cursor_position()
            .expect("カーソル位置を取れませんでした")
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
        assert_eq!(cursor(&state).y, row_of(&lines, "line 39"));
    }

    #[test]
    fn the_terminal_cursor_sits_after_the_wide_characters_already_typed() {
        // `cursor` の列は文字数なので、全角を含む行ではそのまま桁にできない
        // （01 の `TextBuffer`）。桁で数えないと、全角のぶんだけ左にずれる。
        let state = state_with_message("あいう", false);

        let lines = draw(&state, SIZE);
        let x = cursor(&state).x;
        let row = &lines[row_of(&lines, "あいう") as usize];

        // 「あいう」の直後。**バイト位置ではなく桁で測る**（枠の `│` は 3 バイト）。
        let head = &row[..row.find("あいう").unwrap()];
        assert_eq!(
            x as usize,
            Line::from(head).width() + Line::from("あいう").width()
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
        assert_eq!(cursor(&state).y, row_of(&lines, "END"));
    }

    #[test]
    fn the_modal_covers_what_is_under_it() {
        let state = state_with_message("本文", false);

        let lines = draw(&state, SIZE);
        let body_row = &lines[row_of(&lines, "本文") as usize];

        assert!(!body_row.contains("first"), "{body_row}");
        assert!(!body_row.contains("second"), "{body_row}");
    }

    #[test]
    fn a_tiny_terminal_does_not_panic() {
        // リサイズの途中では枠より小さい端末が来る。
        for (width, height) in [(1, 1), (10, 3), (20, 5)] {
            let state = state_with_message("本文\n2 行目", false);
            let lines = draw(&state, TermSize { width, height });
            assert_eq!(lines.len(), height as usize);
        }
    }
}
