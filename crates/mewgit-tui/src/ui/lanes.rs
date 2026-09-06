//! `GraphRow.cells` を文字と色にする。設計は `docs/design/06-graph.md` の対応表と
//! `docs/design/07-ui.md` の Graph ペイン。
//!
//! 文字を持つのは `ui` だけ（不変ルール 2、06 の「責務」）。`graph` は線の向きしか
//! 知らない。

use mewgit_core::graph::{Cell, ColorId, GraphRow, Horizontal, Vertical};
use ratatui::style::{Color, Style};
use ratatui::text::Span;

/// 1 レーンが占める桁数。罫線 1 桁 + 隣との継ぎ目 1 桁。
const LANE_WIDTH: usize = 2;

/// `[graph] colors` を ratatui の色にする。
///
/// 読めない色名は飛ばさずに [`Color::Reset`] で埋める。飛ばすと `ColorId` を
/// 割る周期が設定の見た目と変わり、「6 色書いたのに 5 色で回る」理由を追えなくなる。
pub fn palette(colors: &[String]) -> Vec<Color> {
    colors
        .iter()
        .map(|name| name.parse().unwrap_or(Color::Reset))
        .collect()
}

/// 罫線の桁数。`lanes` はそのフレームで描く行の最大レーン数。
pub fn width(lanes: usize) -> usize {
    lanes * LANE_WIDTH
}

/// 行 1 本ぶんの罫線。`lanes` 本ぶんの桁に右を空白で詰める。
pub fn spans(row: &GraphRow, lanes: usize, palette: &[Color], ascii: bool) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(width(lanes));

    for index in 0..lanes {
        let cell = row.cells.get(index).copied().unwrap_or_default();
        spans.push(Span::styled(
            glyph(cell, ascii),
            style(color(cell, palette)),
        ));

        // レーンの継ぎ目。両側がつながっているときだけ線を引く（07）。
        let next = row.cells.get(index + 1).copied().unwrap_or_default();
        let joined = cell.horizontal.goes_right() && next.horizontal.goes_left();
        let fill = if joined {
            pick_charset(HORIZONTAL, ascii)
        } else {
            " "
        };
        spans.push(Span::styled(
            fill,
            style(cell.horizontal.color().and_then(|id| pick(id, palette))),
        ));
    }

    spans
}

/// マスの色。縦線があればその色、無ければ横線の色。
fn color(cell: Cell, palette: &[Color]) -> Option<Color> {
    let id = match cell.vertical {
        Vertical::None => cell.horizontal.color(),
        Vertical::Through(id) | Vertical::Up(id) | Vertical::Down(id) => Some(id),
        Vertical::Commit { color, .. } => Some(color),
    };
    id.and_then(|id| pick(id, palette))
}

/// `ColorId` を palette に写す（06 の「色」）。palette が空なら端末の既定色。
fn pick(id: ColorId, palette: &[Color]) -> Option<Color> {
    if palette.is_empty() {
        return None;
    }
    Some(palette[id.0 as usize % palette.len()])
}

fn style(color: Option<Color>) -> Style {
    match color {
        Some(color) => Style::default().fg(color),
        None => Style::default(),
    }
}

/// (縦, 横) から 1 文字。対応表は 06。
///
/// `&'static str` を返すのは、毎フレーム 行 × レーン数ぶん `Span` を作るため。
/// `char` から `String` を起こすと、そのぶん確保が要る。
fn glyph(cell: Cell, ascii: bool) -> &'static str {
    use Horizontal as H;
    use Vertical as V;

    // (UTF-8, ASCII)。ASCII の置換表は 06 の「ASCII フォールバック」。
    let (utf8, plain) = match (cell.vertical, cell.horizontal) {
        (V::Commit { merge: true, .. }, _) => ("○", "o"),
        (V::Commit { merge: false, .. }, _) => ("●", "*"),
        (V::None, H::None) => (" ", " "),
        (V::None, _) => HORIZONTAL,
        (V::Through(_), H::None) => ("│", "|"),
        (V::Through(_), H::Left(_)) => ("┤", "+"),
        (V::Through(_), H::Right(_)) => ("├", "+"),
        (V::Through(_), H::Both(_)) => ("┼", "+"),
        (V::Up(_), H::None) => ("╵", "|"),
        (V::Up(_), H::Left(_)) => ("╯", "/"),
        (V::Up(_), H::Right(_)) => ("╰", "\\"),
        (V::Up(_), H::Both(_)) => ("┴", "+"),
        (V::Down(_), H::None) => ("╷", "|"),
        (V::Down(_), H::Left(_)) => ("╮", "\\"),
        (V::Down(_), H::Right(_)) => ("╭", "/"),
        (V::Down(_), H::Both(_)) => ("┬", "+"),
    };

    pick_charset((utf8, plain), ascii)
}

/// 横線。レーンの継ぎ目にも使う。
const HORIZONTAL: (&str, &str) = ("─", "-");

fn pick_charset(glyphs: (&'static str, &'static str), ascii: bool) -> &'static str {
    if ascii {
        glyphs.1
    } else {
        glyphs.0
    }
}
