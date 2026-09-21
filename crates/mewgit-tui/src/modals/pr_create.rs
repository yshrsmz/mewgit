//! PR 作成モーダルの描画。設計は `docs/design/07-ui.md` の「PrCreate モーダル」。

use mewgit_core::app::{PrField, TextBuffer};
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::modals;
use crate::ui;

/// キーの案内。`update::prs::pr_create_key` が受けるキーと揃える。
///
/// **群のあいだは空白 2**（`CommitMessage` は 3）。キーが 1 つ多いので、3 だと
/// 枠の内側に入らない（07。#73 の「置かれる場所の予算が違うなら別の文言でよい」の
/// 空白版）。
///
/// **`Shift+Enter` は出さない。** `CommitMessage` は `Ctrl-J` と両方出しているが、
/// ここは予算が無い。**落とすのは `Shift+Enter` のほう** —— `Ctrl-J` はどの端末でも
/// 届くので、出す側を落としても**打つ手が無くなる人は出ない**（逆にすると、
/// 区別して送れない端末の人が行き止まる）。`Shift+Enter` 自体は効く。
const HINT: &str = "[Enter] 作成  [Tab] 次の欄  [Space] draft  [Ctrl-J] 改行  [Esc] やめる";

/// モーダルの幅。本文を書く箱なので `Confirm` より広い（`CommitMessage` と同じ）。
const WIDTH: u16 = 76;

/// 枠のタイトル。
const TITLE: &str = "Create pull request";

/// ラベルの桁（`Title  ` / `Body   ` / `Base   ` / `Draft  `）。
const LABEL_WIDTH: u16 = 7;

/// `Body` 以外の欄が取る行数（`Title` / `Base` / `Draft`）。
const SINGLE_ROWS: u16 = 3;

/// PR 作成モーダルを描く。
pub fn render(
    frame: &mut Frame,
    area: Rect,
    title: &TextBuffer,
    body: &TextBuffer,
    base: &TextBuffer,
    draft: bool,
    field: PrField,
) {
    let want = SINGLE_ROWS.saturating_add(u16::try_from(body.lines.len()).unwrap_or(u16::MAX));
    let area = modals::place(frame, area, modals::block(TITLE), WIDTH, want, HINT);

    // **算術で割る**（`ui::split` と同じ形）。`Layout` の `Length` に絶対優先を
    // 与えると、狭い端末で `Body` が 0 行に潰れる（#65 で一覧を潰した形）。
    let body_rows = area.height.saturating_sub(SINGLE_ROWS);
    let row = |index: u16, height: u16| -> Option<Rect> {
        let height = height.min(area.height.checked_sub(index)?);
        (height > 0).then(|| Rect::new(area.x, area.y + index, area.width, height))
    };

    let mut cursor = None;
    if let Some(at) = row(0, 1) {
        cursor = cursor.or(text_field(
            frame,
            at,
            "Title",
            title,
            field == PrField::Title,
        ));
    }
    if let Some(at) = row(1, body_rows) {
        cursor = cursor.or(body_field(frame, at, body, field == PrField::Body));
    }
    if let Some(at) = row(1 + body_rows, 1) {
        cursor = cursor.or(text_field(frame, at, "Base", base, field == PrField::Base));
    }
    if let Some(at) = row(2 + body_rows, 1) {
        // **`Draft` には端末カーソルを置かない。** 文字を打つ欄ではないので列が無い。
        // どの欄に居るかはラベルの反転が出す（07）。
        let value = if draft { "[x]" } else { "[ ]" };
        let (label, value_area) = split(at);
        label_at(frame, label, "Draft", field == PrField::Draft);
        frame.render_widget(Paragraph::new(Line::from(value)), value_area);
    }

    // **端末のカーソルは編集中の欄に置く**（`CommitMessage` と同じ）。潰れた端末で
    // その欄の行が残らなければ置かない。
    if let Some(position) = cursor {
        frame.set_cursor_position(position);
    }
}

/// 1 行の欄（`Title` / `Base`）。編集中ならカーソルの位置を返す。
fn text_field(
    frame: &mut Frame,
    at: Rect,
    label: &str,
    buf: &TextBuffer,
    focused: bool,
) -> Option<Position> {
    let (label_area, value_area) = split(at);
    label_at(frame, label_area, label, focused);

    // **折り返さない**（07）。折り返すと `ui` が出す桁とバッファの列が食い違い、
    // カーソルの位置を出せなくなる。**ずらす幅はその欄の幅** —— 箱の幅で数えると、
    // ラベルのぶんだけカーソルが右にずれる。
    let (col, scroll_x) = modals::cursor_col(buf, value_area.width);
    let line = Line::from(buf.lines[buf.cursor.0].as_str());
    frame.render_widget(Paragraph::new(line).scroll((0, scroll_x)), value_area);

    (focused && value_area.width > 0).then(|| Position {
        x: value_area.x + col,
        y: value_area.y,
    })
}

/// 複数行の欄（`Body`）。ラベルは 1 行目だけに出す。
fn body_field(frame: &mut Frame, at: Rect, buf: &TextBuffer, focused: bool) -> Option<Position> {
    let (label_area, value_area) = split(at);
    // ラベルは塊の先頭行にだけ置く。
    label_at(
        frame,
        Rect::new(label_area.x, label_area.y, label_area.width, 1),
        "Body",
        focused,
    );

    // 本文が欄より高ければ、カーソルの行が入るところまでずらす（`CommitMessage` と
    // 同じ形）。
    let cursor_row = buf.cursor.0;
    let shown = ui::window(
        buf.lines.len(),
        cursor_row..cursor_row + 1,
        value_area.height as usize,
    );
    let (col, scroll_x) = modals::cursor_col(buf, value_area.width);
    let lines: Vec<Line<'_>> = buf.lines[shown.clone()]
        .iter()
        .map(|line| Line::from(line.as_str()))
        .collect();
    frame.render_widget(Paragraph::new(lines).scroll((0, scroll_x)), value_area);

    (focused && value_area.width > 0 && shown.contains(&cursor_row)).then(|| Position {
        x: value_area.x + col,
        y: value_area.y + u16::try_from(cursor_row - shown.start).unwrap_or(u16::MAX),
    })
}

/// ラベルの桁と値の桁に割る。**幅が足りなければ値の桁が 0 になる。**
fn split(at: Rect) -> (Rect, Rect) {
    let label = LABEL_WIDTH.min(at.width);
    (
        Rect::new(at.x, at.y, label, at.height),
        Rect::new(at.x + label, at.y, at.width - label, at.height),
    )
}

/// ラベルを描く。**編集中の欄は反転**（07）。
fn label_at(frame: &mut Frame, at: Rect, label: &str, focused: bool) {
    let mut line = Line::from(label);
    if focused {
        line = line.style(ui::theme::selected());
    }
    frame.render_widget(Paragraph::new(line), at);
}

#[cfg(test)]
mod tests {
    use crate::test_util::{cursor, draw, row_of, state_with_modal, terminal_with};
    use mewgit_core::app::{AppState, Modal, PrField, TermSize, TextBuffer};
    use ratatui::style::Modifier;
    use ratatui::text::Line;

    /// 既定の 80x24。
    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    fn state_with_pr(title: &str, body: &str, base: &str, draft: bool, field: PrField) -> AppState {
        state_with_modal(Modal::PrCreate {
            title: TextBuffer::from_text(title),
            body: TextBuffer::from_text(body),
            base: TextBuffer::from_text(base),
            draft,
            field,
        })
    }

    /// その行が反転しているか（ラベルの桁だけを見る）。
    ///
    /// **行の全幅で見ない**（#86）。値の桁は反転していないので、行のどこかが
    /// 反転していればよい形にすると、**隣の欄が反転していても真になる**。
    fn label_is_reversed(state: &AppState, label: &str) -> bool {
        let lines = draw(state, SIZE);
        let row = row_of(&lines, label);
        let line = &lines[row as usize];
        // **バイト位置ではなく桁で測る**（枠の `│` は 3 バイト。`CommitMessage` の
        // カーソルのテストが同じ罠を踏んでいる）。
        let head = &line[..line.find(label).expect("ラベルの桁")];
        let start = Line::from(head).width() as u16;
        let width = Line::from(label).width() as u16;
        let terminal = terminal_with(state, SIZE);
        let buffer = terminal.backend().buffer();
        (start..start + width).all(|x| {
            buffer[(x, row)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        })
    }

    #[test]
    fn the_modal_shows_every_field_and_the_keys() {
        let state = state_with_pr("feat: x", "本文", "main", false, PrField::Title);

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("Create pull request"), "{text}");
        assert!(text.contains("Title"), "{text}");
        assert!(text.contains("feat: x"), "{text}");
        assert!(text.contains("Body"), "{text}");
        assert!(text.contains("本文"), "{text}");
        assert!(text.contains("Base"), "{text}");
        assert!(text.contains("main"), "{text}");
        // **`[ ]` / `[x]` で出す**（07）。`false` / `true` は画面の語ではない。
        assert!(text.contains("Draft  [ ]"), "{text}");
        assert!(text.contains("[Enter] 作成"), "{text}");
        assert!(text.contains("[Tab] 次の欄"), "{text}");
        assert!(text.contains("[Space] draft"), "{text}");
        assert!(text.contains("[Ctrl-J] 改行"), "{text}");
        // 確認を挟まずに本文が消えるので、押す前に見えている必要がある。
        assert!(text.contains("[Esc] やめる"), "{text}");
    }

    #[test]
    fn the_draft_box_is_checked_when_draft_is_on() {
        // **肯定まで 1 本で見る**（`CLAUDE.md` の「否定を見る assert は…」）。
        // `[ ]` だけを見る assert は、`draft` を読まない実装でも通る。
        let off = draw(&state_with_pr("t", "", "main", false, PrField::Draft), SIZE).join("\n");
        let on = draw(&state_with_pr("t", "", "main", true, PrField::Draft), SIZE).join("\n");

        assert!(off.contains("Draft  [ ]"), "{off}");
        assert!(on.contains("Draft  [x]"), "{on}");
    }

    #[test]
    fn the_label_of_the_field_being_edited_is_reversed() {
        // **どの欄に居るかはラベルの反転が出す**（07）。`Draft` には端末カーソルを
        // 置けないので、カーソルだけを手掛かりにすると**そこで印が消える**。
        let state = state_with_pr("t", "b", "main", false, PrField::Base);

        assert!(label_is_reversed(&state, "Base"), "Base が反転していない");
        assert!(
            !label_is_reversed(&state, "Title"),
            "Title まで反転している"
        );
        assert!(
            !label_is_reversed(&state, "Draft"),
            "Draft まで反転している"
        );

        // 欄が移れば印も移る。**`Draft` でも出る**（カーソルが置けない唯一の欄）。
        let state = state_with_pr("t", "b", "main", false, PrField::Draft);
        assert!(label_is_reversed(&state, "Draft"), "Draft が反転していない");
        assert!(!label_is_reversed(&state, "Base"), "Base が残っている");
    }

    #[test]
    fn the_terminal_cursor_sits_in_the_field_being_edited() {
        // 編集中の欄の値の桁に置く。**ラベルのぶん右**（`split`）。
        let state = state_with_pr("feat", "b", "main", false, PrField::Base);
        let lines = draw(&state, SIZE);
        let at = cursor(&state, SIZE);

        assert_eq!(at.y, row_of(&lines, "Base"), "Base の行にない");
        let row = &lines[at.y as usize];
        let head = &row[..row.find("main").expect("値の桁")];
        assert_eq!(
            at.x as usize,
            Line::from(head).width() + Line::from("main").width(),
            "打った字の直後にない"
        );
    }

    #[test]
    fn the_cursor_follows_the_body_line_being_edited() {
        // 複数行の欄では行も追う。**ラベルの行が先頭行**（`body_field`）。
        let mut state = state_with_pr("t", "1 行目\n2 行目", "main", false, PrField::Body);
        let Some(Modal::PrCreate { body, .. }) = &mut state.modal else {
            unreachable!()
        };
        body.cursor = (1, 0);

        let lines = draw(&state, SIZE);
        assert_eq!(cursor(&state, SIZE).y, row_of(&lines, "2 行目"));
    }

    #[test]
    fn a_value_longer_than_its_field_scrolls_sideways_to_keep_the_cursor_visible() {
        // **ずらす幅はその欄の幅**（ラベルのぶん狭い）。箱の幅で数えると、
        // ラベルの桁ぶんだけカーソルが枠の外へ出る。
        let long = format!("{}END", "x".repeat(120));
        let state = state_with_pr(&long, "b", "main", false, PrField::Title);

        let lines = draw(&state, SIZE);
        let at = cursor(&state, SIZE);

        assert!(lines.join("\n").contains("END"), "{lines:?}");
        assert_eq!(at.y, row_of(&lines, "END"));
        // 枠の中に収まっている（枠の右辺より左）。
        let row = &lines[at.y as usize];
        let right = row.rfind('│').expect("枠の右辺") as u16;
        assert!(at.x < right, "カーソルが枠の外: x={} right={right}", at.x);
    }

    #[test]
    fn a_body_taller_than_the_box_scrolls_to_the_cursor() {
        // 見えない行にカーソルがあると、何を打っているのか判らない
        // （`CommitMessage` と同じ）。
        let body = (0..40)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut state = state_with_pr("t", &body, "main", false, PrField::Body);
        let Some(Modal::PrCreate { body, .. }) = &mut state.modal else {
            unreachable!()
        };
        body.cursor = (39, 0);

        let lines = draw(&state, SIZE);
        assert!(lines.join("\n").contains("line 39"), "{lines:?}");
        assert_eq!(cursor(&state, SIZE).y, row_of(&lines, "line 39"));
        // **本文がキーの案内を押し出さない**（`modals::place` が別に描く）。
        assert!(
            row_of(&lines, "[Enter] 作成") < row_of(&lines, "└────────"),
            "{lines:?}"
        );
    }

    #[test]
    fn a_tiny_terminal_does_not_panic() {
        // リサイズの途中では枠より小さい端末が来る。**編集中のモーダルごとに
        // 1 本ずつ見る**（`CommitMessage` の同名テストが理由を書いている）。
        // こちらの形は**欄を算術で割る**ところで、`Body` が 0 行になる
        // （#65 で一覧を潰したのと同じ形）。
        for (width, height) in [(1, 1), (5, 4), (10, 3), (20, 5), (40, 8)] {
            let state = state_with_pr("t", "本文\n2 行目", "main", false, PrField::Body);
            let lines = draw(&state, TermSize { width, height });
            assert_eq!(lines.len(), height as usize);
        }
    }
}
