//! モーダルの描画。`ui::render` が最後に呼び、下のペインを覆って描く。
//!
//! キー処理は `app::update` にある。`ui` は `Cmd` を作らない（不変ルール 2）。

pub mod commit_message;
pub mod confirm;
pub mod input;

use mewgit_core::app::{Modal, TextBuffer};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

/// 表示中のモーダルを描く。
///
/// `match` で `_` を使わないのは、`Modal` に変種を足したときに描画漏れを
/// コンパイルエラーにするため（`update` や `Cmd::job_kind` と同じ）。
pub fn render(frame: &mut Frame, area: Rect, modal: &Modal) {
    match modal {
        Modal::Confirm { title, body, .. } => confirm::render(frame, area, title, body),
        Modal::CommitMessage { buf, amend } => commit_message::render(frame, area, buf, *amend),
        Modal::Input {
            title, prompt, buf, ..
        } => input::render(frame, area, title, prompt, buf),
        // 描画はそれぞれのモーダルのタスクで足す。キー処理も同じタスクで入る。
        Modal::RebaseEditor { .. }
        | Modal::PrCreate { .. }
        | Modal::AccountPicker { .. }
        | Modal::Help => {}
    }
}

/// 本文とキーの案内のあいだに空ける行数。
const GAP_ROWS: u16 = 1;

/// キーの案内に取る行数。高さの見積もりと `Layout` で同じものを使う。
const HINT_ROWS: u16 = 1;

/// モーダルの枠。
pub fn block(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(title.to_string())
        .border_style(Style::default().add_modifier(Modifier::BOLD))
}

/// 枠の内側に使える幅。
///
/// **枠が食う幅は `Block` 自身から出す。** `- 2` と手で書くと、あとで `Padding` を
/// 足したときに折り返し幅だけが古いまま残り、本文の末尾が黙って切れる。高さは
/// 折り返してからでないと決まらないので、幅の判る当て木を [`Block::inner`] に通す。
pub fn inner_width(area: Rect, want: u16, block: &Block<'_>) -> u16 {
    block.inner(probe(area, want)).width
}

/// 箱を画面の中央に描き、本文に使える領域を返す。
///
/// **2 つ目のモーダルが出た時点でここへ移した。** 各モーダルで違うのは本文の作り方と
/// 幅と案内の文言だけで、枠の測り方・高さの合算・`Clear`・縦割り・案内の置き場は
/// 同じ。写すと、余白や案内の位置を直したときに片方だけ古くなる。`contains` を見る
/// テストはどちらでも通るので気付けない（07 の「モーダル」）。
///
/// `body_rows` は本文が要求する行数。画面に収まらない分は [`centered`] が削る。
///
/// **キーの案内は本文とは別に最下行へ描く。** 1 つの `Paragraph` にまとめると、
/// 本文が伸びたときに案内だけが枠の外へ押し出される。
pub fn place(
    frame: &mut Frame,
    area: Rect,
    block: Block<'_>,
    want: u16,
    body_rows: u16,
    hint: &str,
) -> Rect {
    let probe = probe(area, want);
    let border_rows = probe.height - block.inner(probe).height;

    // 本文 + 空行 + キーの案内 + 枠。
    let height = body_rows.saturating_add(GAP_ROWS + HINT_ROWS + border_rows);
    let area = centered(area, probe.width, height);
    let inner = block.inner(area);

    // 下のペインを `Clear` で消してから描く。覆わずに重ねると下の文字が透けて読めない。
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let [body, hint_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(HINT_ROWS)]).areas(inner);
    frame.render_widget(Paragraph::new(Line::from(hint)), hint_area);
    body
}

/// 枠を測るための当て木。幅は画面に収まるところまで縮め、高さは測る側に任せる。
fn probe(area: Rect, want: u16) -> Rect {
    Rect::new(0, 0, want.min(area.width), u16::MAX)
}

/// 画面の中央に `width` x `height` を取る。画面が小さければそのぶん縮める。
///
/// 中央寄せの算術は書かず ratatui の [`Rect::centered`] に任せる。ratatui 0.30 で
/// 入った API で、中身は `Layout::…flex(Flex::Center)` を横と縦に掛ける、ここに
/// 書いてあったのと同じ合成。
///
/// **`Max` と `Length` で結果は変わらない。** ここには「`Length` だとはみ出す」と
/// 書いてあったが、実際に両方を通すとどちらも領域に丸められた（10x5 の領域に
/// 60x12 を頼むと両方とも 10x5）。ソルバが領域を超える解を出さないため。`Max` を
/// 残すのは「これ以下」という意図がそのまま読めるからで、はみ出しを防ぐためでは
/// ない。差が無いので、この 2 つを見分けるテストは書けない。
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    area.centered(Constraint::Max(width), Constraint::Max(height))
}

/// カーソルの桁と、それを枠に収めるための横のずらし幅。
///
/// **列からバイト位置への変換は [`TextBuffer::before_cursor`] に任せる。** 「列は
/// 文字数」という約束を 2 箇所で解くと、単位を変えたときに core だけ直して端末の
/// カーソルが黙ってずれる（01 の `TextBuffer`）。桁の数え方は ratatui の
/// [`Line::width`] に合わせ、描画と同じにする。
///
/// **ずらし幅を一緒に返すのは、桁だけを共有すると使う側が同じ引き算を書くから。**
/// 編集中のモーダルはどちらも折り返さないので、カーソルが右端を越えたぶんだけ横に
/// ずらす（`CommitMessage` と `Input` の 2 人目が出た時点でここへ移した）。
pub fn cursor_col(buf: &TextBuffer, width: u16) -> (u16, u16) {
    let col = u16::try_from(Line::from(buf.before_cursor()).width()).unwrap_or(u16::MAX);
    (col, col.saturating_sub(width.saturating_sub(1)))
}
