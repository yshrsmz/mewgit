//! PR 作成モーダルの描画。設計は `docs/design/07-ui.md` の「PrCreate モーダル」。

use mewgit_core::app::{PrField, PrHead, TextBuffer};
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

/// ラベルの桁（`Title  ` / `Body   ` / `Base   ` / `Head   ` / `Draft  `）。
const LABEL_WIDTH: u16 = 7;

/// 描く欄の値。`Modal::PrCreate` を分解したもの。
///
/// **束ねるのは引数の数のため**（clippy の `too_many_arguments`）。`&Modal` を
/// 受けて中で分解すると、`PrCreate` 以外が来たときの到達しない腕が要る。
pub struct Fields<'a> {
    pub title: &'a TextBuffer,
    pub body: &'a TextBuffer,
    pub base: &'a TextBuffer,
    pub heads: &'a [PrHead],
    pub head_cursor: usize,
    pub draft: bool,
    pub field: PrField,
}

/// PR 作成モーダルを描く。
///
/// **縦は算術で割る**（`ui::split` と同じ形）。`Layout` の `Length` に絶対優先を
/// 与えると、狭い端末で `Body` が 0 行に潰れる（#65 で一覧を潰した形）。
pub fn render(frame: &mut Frame, area: Rect, fields: &Fields) {
    let Fields {
        title,
        body,
        base,
        heads,
        head_cursor,
        draft,
        field,
    } = *fields;
    // 1 行の欄（`Title` / `Base` / `Head` / `Draft`）と、残りを取る `Body`。
    let singles: u16 = 4;
    let want = singles.saturating_add(u16::try_from(body.lines.len()).unwrap_or(u16::MAX));
    let area = modals::place(frame, area, modals::block(TITLE), WIDTH, want, HINT);
    let body_rows = area.height.saturating_sub(singles);

    // **上から順に積む。** 各欄の y を `1 + body_rows` のように手で組むと、欄を
    // 足したときにその式が全部ずれる。
    let mut y = area.y;
    let mut take = |height: u16| -> Option<Rect> {
        let left = area.height.checked_sub(y - area.y)?;
        let height = height.min(left);
        let at = (height > 0).then(|| Rect::new(area.x, y, area.width, height));
        y += height;
        at
    };

    let mut cursor = None;
    if let Some(at) = take(1) {
        cursor = cursor.or(text_field(frame, at, "Title", title, field));
    }
    if let Some(at) = take(body_rows) {
        cursor = cursor.or(text_field(frame, at, "Body", body, field));
    }
    if let Some(at) = take(1) {
        cursor = cursor.or(text_field(frame, at, "Base", base, field));
    }
    if let Some(at) = take(1) {
        // **`Head` にも端末カーソルを置かない**（`Draft` と同じく打つ欄ではない）。
        let (label, value) = split(at);
        label_at(frame, label, "Head", field == PrField::Head);
        frame.render_widget(
            Paragraph::new(Line::from(head_value(heads, head_cursor))),
            value,
        );
    }
    if let Some(at) = take(1) {
        // **`Draft` には端末カーソルを置かない。** 文字を打つ欄ではないので列が無い。
        // どの欄に居るかはラベルの反転が出す（07）。
        let (label, value) = split(at);
        label_at(frame, label, "Draft", field == PrField::Draft);
        frame.render_widget(Paragraph::new(Line::from(draft_box(draft))), value);
    }

    // **端末のカーソルは編集中の欄に置く**（`CommitMessage` と同じ）。潰れた端末で
    // その欄の行が残らなければ置かない。
    if let Some(position) = cursor {
        frame.set_cursor_position(position);
    }
}

/// 文字を打つ欄（`Title` / `Body` / `Base`）。編集中ならカーソルの位置を返す。
///
/// **1 行の欄と複数行の欄を分けない。** 違うのは渡す `height` だけで、窓の切り出しも
/// 横スクロールも `modals::edit_area` が受け持つ。
fn text_field(
    frame: &mut Frame,
    at: Rect,
    label: &str,
    buf: &TextBuffer,
    field: PrField,
) -> Option<Position> {
    let focused = label_field(label) == field;
    let (label_area, value_area) = split(at);
    // ラベルは塊の先頭行にだけ置く（`Body` は下へ伸びる）。
    label_at(
        frame,
        Rect::new(label_area.x, label_area.y, label_area.width, 1),
        label,
        focused,
    );
    // **ずらす幅はその欄の幅**（ラベルのぶん狭い）で、箱の幅ではない。
    let position = modals::edit_area(frame, value_area, buf);
    focused.then_some(position).flatten()
}

/// ラベルの字が指す欄。**この写像はここ 1 本**で、描く順（`render`）と
/// フォーカスの判定が同じ字から出る。
fn label_field(label: &str) -> PrField {
    match label {
        "Title" => PrField::Title,
        "Body" => PrField::Body,
        "Base" => PrField::Base,
        // 打つ欄ではないので `text_field` からは来ない。
        _ => PrField::Draft,
    }
}

/// `Head` の値（07 の「PrCreate モーダル」）。選んでいる候補の `<owner>:<branch>` と、
/// **候補が 2 つ以上あるときだけ**選び直せることの印。
///
/// **印は ASCII**（`<- ->`）。`ui::pick_charset` を通さずに済み、端末で化けない。
/// **空は「候補なし」** —— 開いているあいだに候補が消えた形（確定のときに
/// `update::prs` が欄を入れ替える）。
fn head_value(heads: &[PrHead], cursor: usize) -> String {
    match heads.get(cursor) {
        None => "(候補なし)".to_string(),
        Some(head) if heads.len() == 1 => head.label(),
        Some(head) => format!("{}  ({}/{} <- ->)", head.label(), cursor + 1, heads.len()),
    }
}

/// `Draft` の値。**`ui::pick_charset` を通さない** —— ASCII だけなので端末で
/// 化けようがない（07）。
fn draft_box(draft: bool) -> &'static str {
    if draft {
        "[x]"
    } else {
        "[ ]"
    }
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
    use crate::test_util::{col_of, cursor, draw, row_of, state_with_modal, terminal_with};
    use mewgit_core::app::{AppState, Modal, PrField, PrHead, TermSize, TextBuffer};
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
            heads: vec![head("taro")],
            head_cursor: 0,
            draft,
            field,
        })
    }

    fn head(owner: &str) -> PrHead {
        PrHead {
            owner: owner.to_string(),
            branch: "feat".to_string(),
        }
    }

    /// `head` の候補と選んでいる位置だけを差し替える。
    fn with_heads(mut state: AppState, owners: &[&str], cursor: usize) -> AppState {
        let Some(Modal::PrCreate {
            heads, head_cursor, ..
        }) = &mut state.modal
        else {
            unreachable!()
        };
        *heads = owners.iter().map(|owner| head(owner)).collect();
        *head_cursor = cursor;
        state
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
        // **候補が 1 つなら選び直しの印を出さない**（07）。
        assert!(text.contains("Head   taro:feat"), "{text}");
        assert!(!text.contains("<- ->"), "{text}");
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
    fn the_head_shows_the_chosen_candidate_and_the_count_only_when_there_is_a_choice() {
        let base = state_with_pr("t", "", "main", false, PrField::Head);

        let two = draw(&with_heads(base, &["alice", "taro"], 1), SIZE).join("\n");
        // **選んでいる側**を出す（先頭ではなく）。
        assert!(two.contains("Head   taro:feat  (2/2 <- ->)"), "{two}");
        assert!(!two.contains("alice"), "{two}");

        let base = state_with_pr("t", "", "main", false, PrField::Head);
        let none = draw(&with_heads(base, &[], 0), SIZE).join("\n");
        assert!(none.contains("Head   (候補なし)"), "{none}");
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
        // `Head` もカーソルを置けない欄なので、印は反転だけ。
        let state = state_with_pr("t", "b", "main", false, PrField::Head);
        assert!(label_is_reversed(&state, "Head"), "Head が反転していない");
        assert!(!label_is_reversed(&state, "Draft"), "Draft が残っている");
    }

    #[test]
    fn the_terminal_cursor_sits_in_the_field_being_edited() {
        // 編集中の欄の値の桁に置く。**ラベルのぶん右**（`split`）。
        let state = state_with_pr("feat", "b", "main", false, PrField::Base);
        let lines = draw(&state, SIZE);
        let at = cursor(&state, SIZE);

        assert_eq!(at.y, row_of(&lines, "Base"), "Base の行にない");
        assert_eq!(
            at.x,
            col_of(&lines[at.y as usize], "main") + Line::from("main").width() as u16,
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
    fn a_value_longer_than_its_field_scrolls_within_the_field_and_spares_the_label() {
        // **渡す領域もずらす幅も「その欄」**（ラベルのぶん狭い）。箱の領域を渡すと
        // **長い値がラベルの桁を上書きする** —— カーソルの桁は**どちらでも枠の内側の
        // 右端**（`欄.x + 欄.width == 箱.x + 箱.width`）なので、位置だけを見る
        // assert ではこの取り違えを踏めない。
        let long = format!("{}END", "x".repeat(120));
        let state = state_with_pr(&long, "b", "main", false, PrField::Title);

        let lines = draw(&state, SIZE);
        let at = cursor(&state, SIZE);
        let row = &lines[at.y as usize];

        assert!(row.contains("END"), "{row:?}");
        assert_eq!(at.y, row_of(&lines, "Title"), "Title の行にない");
        assert!(row.contains("Title"), "ラベルが値に潰された: {row:?}");
        // **枠の右辺より左に居る。桁で測る** —— `rfind` のバイト位置をそのまま桁に
        // すると、行の左にある枠 1 本につき 2 大きくなって見張りが緩む。
        let right = Line::from(&row[..row.rfind('│').expect("枠の右辺")]).width() as u16;
        assert!(at.x < right, "カーソルが枠の外: x={} right={right}", at.x);
        // 値が始まるのはラベルの桁のあと（`split`）。
        assert!(
            at.x > col_of(row, "Title"),
            "カーソルがラベルの桁に居る: {row:?}"
        );
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
