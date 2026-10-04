//! 一覧から 1 つ選ぶモーダルの置き方。設計は `docs/design/07-ui.md` の
//! 「RemotePicker モーダル」。
//!
//! **2 つ目の利用者（`AccountPicker`）が出た時点でここへ移した。** 各モーダルで
//! 違うのはタイトル・本文の文字列・行の組み立て・キーの案内だけで、**本文を測って
//! 折り返す / 一覧に先に行を渡す / 窓を切り出す / カーソル行を反転する**はどちらも
//! 同じ。写すと、`a_long_reason_never_squeezes_the_list_to_nothing` が塞いだ形
//! （一覧が 0 行に潰れる）を片方だけが踏み直す。

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Text};
use ratatui::Frame;

use crate::modals;
use crate::ui::{self, theme};

/// 本文と一覧のあいだに空ける行数。
///
/// **`modals::GAP_ROWS` とは別のもの**（あちらは本文とキーの案内のあいだ）。
const LIST_GAP_ROWS: u16 = 1;

/// 一覧から 1 つ選ぶモーダルの中身。
pub struct Picker<'a> {
    /// 枠のタイトル。**何を選ぶのかを出す。**
    pub title: &'a str,
    /// 箱の幅。ほかと揃えるなら `modals::WIDTH`。
    pub width: u16,
    /// なぜこの画面が出ているか。**折り返す**ので長くてよい。**空なら行も区切りも
    /// 取らない**（`Help` は本文を持たない。一覧が 2 行ぶん広く使える）。
    ///
    /// **`String` で受けない。** 行に割ってあるものを `\n` で繋いで渡すと、
    /// `Text::from(&str)` が同じ位置で割り直す（描くのは毎フレーム）。
    pub body: Text<'a>,
    /// 選ばせる行。**空でよい** —— `AccountPicker` は選ばせない degraded でも開く
    /// （04）。そのとき空行も一覧の領域も取らない。
    pub rows: Vec<Line<'a>>,
    pub cursor: usize,
    /// キーの案内。**受けるキーと揃える**（`update` の `*_key`）。
    pub hint: &'a str,
}

/// 箱を描き、本文と一覧を置く。
pub fn render(frame: &mut Frame, area: Rect, picker: Picker<'_>) {
    let block = modals::block(picker.title);
    // **本文は折り返しうる。** 桁数を幅で割る見積もりだと単語境界で折れて増えたぶんを
    // 数え落とし、末尾が黙って切れる（`Confirm` と同じ理由。07）。
    let width = modals::inner_width(area, picker.width, &block);
    let no_body = picker.body.width() == 0;
    let (body_text, body_rows) = ui::wrapped(picker.body, width);
    let (body_rows, gap) = if no_body {
        (0, 0)
    } else {
        (body_rows, LIST_GAP_ROWS)
    };

    // 一覧は折り返さない。1 行に収まらない行は端で切れる。
    let rows = u16::try_from(picker.rows.len()).unwrap_or(u16::MAX);
    let list_rows = if picker.rows.is_empty() {
        0
    } else {
        rows.saturating_add(gap)
    };
    let inner = modals::place(
        frame,
        area,
        block,
        picker.width,
        body_rows.saturating_add(list_rows),
        picker.hint,
    );

    // **一覧が無ければ縦に割らない。** `Min(1)` のまま呼ぶと一覧が 1 行を取る ——
    // 箱に余裕があるうちは `place` が空ける余白から取るだけだが、**箱が端末の高さに
    // 丸められた回は本文の最後の行が消える**（実測の境目は 80x9 に内訳 5 件。
    // `a_body_that_fills_the_box_is_not_cut_by_the_empty_list`）。消えるのは
    // いちばん下 = 直し方の行なので、モーダルが在る理由が欠ける。
    if picker.rows.is_empty() {
        frame.render_widget(body_text, inner);
        return;
    }

    // **一覧に先に行を渡す。** 本文を `Length` にすると、`modals::place` が箱を端末の
    // 高さに丸めたときに**一覧が 0 行に潰れる** —— 行が 1 つも出ないのに `j` と
    // `Enter` は効くので、見えていない相手を選ぶ。`ui::render_rows` が守るのは
    // 「カーソルが一覧の外へ出ない」ことで、**一覧そのものが消えることは守らない**
    // （`/code-review` が拾った）。本文は候補を並べるので、本数が増えるほど長くなる
    // = 潰れやすくなる。
    //
    // **空行は本文の枠に含める。** 3 本に割って真ん中を捨てると、捨てる束縛と制約が
    // 1 つずつ増えるだけで結果は同じ（`Paragraph` は上詰め）。
    let [body_area, list] = Layout::vertical([
        Constraint::Max(body_rows.saturating_add(gap)),
        Constraint::Min(1),
    ])
    .areas(inner);

    frame.render_widget(body_text, body_area);

    // カーソル行はペインと同じ反転（`theme::selected`）。一覧から 1 つ選ぶ形が
    // 同じなので、見た目も揃える。**その場で塗る** —— 1 行のために `Vec` を
    // 組み直すと、描くたびに全行を積み替えることになる。
    let mut lines = picker.rows;
    if let Some(line) = lines.get_mut(picker.cursor) {
        line.style = theme::selected();
    }
    // **`ui::render_built` を通す。** 素の `Paragraph` で描くと、枠に入りきらない
    // 本数のときにカーソル行が画面の外へ出たまま `j` で動き続け、**見えていない
    // 相手を `Enter` で選ぶ** —— このモーダルが防ぐために在る「黙って選ばない」
    // そのもの。ペインの一覧が 4 つともここを通っている。
    ui::render_built(frame, list, lines, picker.cursor..picker.cursor + 1);
}
