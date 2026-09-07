//! `GraphRow.cells` を文字と色にする。設計は `docs/design/06-graph.md` の対応表と
//! `docs/design/07-ui.md` の Graph ペイン。
//!
//! 文字を持つのは `ui` だけ（不変ルール 2、06 の「責務」）。`graph` は線の向きしか
//! 知らない。

use mewgit_core::graph::{Cell, ColorId, GraphRow, Horizontal, Vertical};
use ratatui::style::{Color, Style};
use ratatui::text::Span;

use crate::ui::theme;

/// 1 レーンが占める桁数。罫線 1 桁 + 隣との継ぎ目 1 桁。
const LANE_WIDTH: usize = 2;

/// `[graph] colors` を ratatui の色にする。
///
/// 読めない色名は飛ばさずに [`Color::Reset`] で埋める。飛ばすと `ColorId` を
/// 割る周期が設定の見た目と変わり、「6 色書いたのに 5 色で回る」理由を追えなくなる。
pub fn palette(colors: &[String]) -> Vec<Color> {
    colors
        .iter()
        .map(|name| theme::color(name).unwrap_or(Color::Reset))
        .collect()
}

/// 罫線の桁数。`lanes` はそのフレームで描く行の最大レーン数。
pub const fn width(lanes: usize) -> usize {
    lanes * LANE_WIDTH
}

/// 行 1 本ぶんの罫線。`lanes` 本ぶんの桁に右を空白で詰める。
pub fn spans(row: &GraphRow, lanes: usize, palette: &[Color], ascii: bool) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(width(lanes));

    for index in 0..lanes {
        let cell = row.cells.get(index).copied().unwrap_or_default();
        spans.push(Span::styled(glyph(cell, ascii), fg(color(cell, palette))));

        // レーンの継ぎ目。両側がつながっているときだけ線を引く（07）。継ぎ目の色は
        // 左のセルの横線の色。
        let next = row.cells.get(index + 1).copied().unwrap_or_default();
        let joined = cell.horizontal.goes_right() && next.horizontal.goes_left();
        spans.push(if joined {
            let color = cell.horizontal.color().and_then(|id| pick(id, palette));
            Span::styled(pick_charset(HORIZONTAL, ascii), fg(color))
        } else {
            Span::raw(" ")
        });
    }

    spans
}

/// マスの色。縦線があればその色、無ければ横線の色（07）。
fn color(cell: Cell, palette: &[Color]) -> Option<Color> {
    cell.vertical
        .color()
        .or_else(|| cell.horizontal.color())
        .and_then(|id| pick(id, palette))
}

/// `ColorId` を palette に写す（06 の「色」）。palette が空なら端末の既定色。
fn pick(id: ColorId, palette: &[Color]) -> Option<Color> {
    if palette.is_empty() {
        return None;
    }
    Some(palette[id.0 as usize % palette.len()])
}

/// 前景色だけのスタイル。`None` は端末の既定色（`Some(Color::Reset)` と違い、
/// 行やペインの色を上書きしない）。
fn fg(color: Option<Color>) -> Style {
    Style {
        fg: color,
        ..Style::default()
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

/// グラフ先頭の WIP 行の印。
///
/// **コミットの `●` / `○` と重ならない字を選ぶ。** WIP 行はコミットではなく、
/// `graph` 層を通らない（06 の「責務」）。ASCII 側も同じ理由で `*` / `o` を避ける。
pub fn wip_glyph(ascii: bool) -> &'static str {
    pick_charset(("◇", "@"), ascii)
}

fn pick_charset(glyphs: (&'static str, &'static str), ascii: bool) -> &'static str {
    if ascii {
        glyphs.1
    } else {
        glyphs.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mewgit_core::graph::GraphRow;

    fn cell(vertical: Vertical, horizontal: Horizontal) -> Cell {
        Cell {
            vertical,
            horizontal,
        }
    }

    fn row(cells: Vec<Cell>) -> GraphRow {
        GraphRow {
            oid: mewgit_core::test_util::fake_oid(1),
            lane: 0,
            cells,
            color: ColorId(0),
        }
    }

    /// 描かれた文字を並べる。
    fn text(spans: &[Span<'static>]) -> String {
        spans.iter().map(|span| span.content.as_ref()).collect()
    }

    #[test]
    fn every_combination_has_the_glyph_the_design_table_says() {
        // 06 の「なぜ『縦 × 横』なのか」の対応表をそのまま写したもの。
        // 表を変えるときは 06 と `graph::lane::tests` のレンダラも一緒に直す。
        let id = ColorId(0);
        let table: &[(Vertical, Horizontal, &str, &str)] = &[
            (Vertical::None, Horizontal::None, " ", " "),
            (Vertical::None, Horizontal::Left(id), "─", "-"),
            (Vertical::None, Horizontal::Right(id), "─", "-"),
            (Vertical::None, Horizontal::Both(id), "─", "-"),
            (Vertical::Through(id), Horizontal::None, "│", "|"),
            (Vertical::Through(id), Horizontal::Left(id), "┤", "+"),
            (Vertical::Through(id), Horizontal::Right(id), "├", "+"),
            (Vertical::Through(id), Horizontal::Both(id), "┼", "+"),
            (Vertical::Up(id), Horizontal::None, "╵", "|"),
            (Vertical::Up(id), Horizontal::Left(id), "╯", "/"),
            (Vertical::Up(id), Horizontal::Right(id), "╰", "\\"),
            (Vertical::Up(id), Horizontal::Both(id), "┴", "+"),
            (Vertical::Down(id), Horizontal::None, "╷", "|"),
            (Vertical::Down(id), Horizontal::Left(id), "╮", "\\"),
            (Vertical::Down(id), Horizontal::Right(id), "╭", "/"),
            (Vertical::Down(id), Horizontal::Both(id), "┬", "+"),
        ];

        for &(vertical, horizontal, utf8, plain) in table {
            let cell = cell(vertical, horizontal);
            assert_eq!(glyph(cell, false), utf8, "{vertical:?} {horizontal:?}");
            assert_eq!(glyph(cell, true), plain, "{vertical:?} {horizontal:?}");
        }
    }

    #[test]
    fn a_commit_is_a_disc_and_a_merge_is_hollow() {
        let plain = Vertical::Commit {
            color: ColorId(0),
            merge: false,
        };
        let merge = Vertical::Commit {
            color: ColorId(0),
            merge: true,
        };

        // 横線が付いていても本体の文字は変わらない。
        assert_eq!(glyph(cell(plain, Horizontal::None), false), "●");
        assert_eq!(
            glyph(cell(plain, Horizontal::Right(ColorId(0))), false),
            "●"
        );
        assert_eq!(glyph(cell(merge, Horizontal::None), false), "○");
        assert_eq!(glyph(cell(merge, Horizontal::None), true), "o");
        assert_eq!(glyph(cell(plain, Horizontal::None), true), "*");
    }

    #[test]
    fn a_row_is_padded_to_the_given_number_of_lanes() {
        // 桁を揃えるのは `ui` の仕事（07）。行のセル数が少なくても右を空白で埋める。
        let row = row(vec![cell(
            Vertical::Commit {
                color: ColorId(0),
                merge: false,
            },
            Horizontal::None,
        )]);

        let spans = spans(&row, 3, &[], false);

        assert_eq!(text(&spans), "●     ");
        assert_eq!(spans.len(), width(3));
    }

    #[test]
    fn the_gap_between_lanes_is_drawn_only_when_both_sides_reach_it() {
        let id = ColorId(0);
        // `●─╮`: 左は右へ、右は左へ伸びているので継ぎ目に線が入る。
        let joined = row(vec![
            cell(
                Vertical::Commit {
                    color: id,
                    merge: true,
                },
                Horizontal::Right(id),
            ),
            cell(Vertical::Down(id), Horizontal::Left(id)),
        ]);
        // `● │`: どちらも横へ伸びていない。
        let apart = row(vec![
            cell(
                Vertical::Commit {
                    color: id,
                    merge: false,
                },
                Horizontal::None,
            ),
            cell(Vertical::Through(id), Horizontal::None),
        ]);

        assert_eq!(text(&spans(&joined, 2, &[], false)), "○─╮ ");
        assert_eq!(text(&spans(&apart, 2, &[], false)), "● │ ");
    }

    #[test]
    fn colours_repeat_over_the_palette() {
        let palette = palette(&["red".to_string(), "blue".to_string()]);

        assert_eq!(pick(ColorId(0), &palette), Some(Color::Red));
        assert_eq!(pick(ColorId(1), &palette), Some(Color::Blue));
        // 3 本目のレーンは先頭に戻る（06 の `ColorId % palette.len()`）。
        assert_eq!(pick(ColorId(2), &palette), Some(Color::Red));
    }

    #[test]
    fn an_unreadable_colour_name_keeps_its_place_in_the_palette() {
        // 飛ばすと周期が設定の見た目と変わり、「2 色書いたのに 1 色で回る」理由を
        // 追えなくなる（08）。
        let palette = palette(&["nonsense".to_string(), "blue".to_string()]);

        assert_eq!(palette.len(), 2);
        assert_eq!(pick(ColorId(0), &palette), Some(Color::Reset));
        assert_eq!(pick(ColorId(1), &palette), Some(Color::Blue));
    }

    #[test]
    fn an_empty_palette_leaves_the_colour_to_the_terminal() {
        // `Some(Color::Reset)` ではなく `None`。行やペインの色を上書きしない。
        assert_eq!(pick(ColorId(0), &[]), None);
        assert_eq!(fg(None).fg, None);
    }

    #[test]
    fn a_cell_without_a_vertical_line_takes_the_colour_of_the_horizontal_one() {
        let palette = palette(&["red".to_string(), "blue".to_string()]);
        let horizontal_only = cell(Vertical::None, Horizontal::Both(ColorId(1)));
        let both = cell(Vertical::Through(ColorId(0)), Horizontal::Both(ColorId(1)));

        assert_eq!(color(horizontal_only, &palette), Some(Color::Blue));
        // 縦線があればそちらが勝つ（07）。
        assert_eq!(color(both, &palette), Some(Color::Red));
    }
}
