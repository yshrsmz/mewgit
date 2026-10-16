//! モーダルの描画。`ui::render` が最後に呼び、下のペインを覆って描く。
//!
//! キー処理は `app::update` にある。`ui` は `Cmd` を作らない（不変ルール 2）。

pub mod account_picker;
pub mod commit_message;
pub mod confirm;
pub mod help;
pub mod input;
pub mod mainline_picker;
pub mod notice_log;
pub mod picker;
pub mod pr_create;
pub mod progress_menu;
pub mod rebase_editor;
pub mod remote_picker;

use mewgit_core::app::{AppState, Modal, RewordInput, TextBuffer};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::ui;

/// 表示中のモーダルを描く。
///
/// **`AppState` を受ける。** `NoticeLog` は一覧を写しで持たず `AppState.notices` を
/// 読む（01 の「通知」）。欄を 1 つずつ引数に足すと、`AppState` の別の値を読む
/// モーダルが来るたびに署名が伸びる。
///
/// `match` で `_` を使わないのは、`Modal` に変種を足したときに描画漏れを
/// コンパイルエラーにするため（`update` や `Cmd::job_kind` と同じ）。
pub fn render(frame: &mut Frame, area: Rect, modal: &Modal, state: &AppState) {
    // 枠と省略の字（`config.graph.ascii`。07 の「ペインとフォーカス」）。
    let ascii = state.config.graph.ascii;
    match modal {
        Modal::Confirm { title, body, .. } => confirm::render(frame, area, title, body, ascii),
        Modal::CommitMessage { buf, amend } => {
            commit_message::render(frame, area, buf, *amend, ascii);
        }
        Modal::Input {
            title, prompt, buf, ..
        } => input::render(frame, area, title, prompt, buf, ascii),
        Modal::RemotePicker {
            reason,
            remotes,
            cursor,
        } => remote_picker::render(frame, area, reason, remotes, *cursor, ascii),
        Modal::AccountPicker { view, cursor } => {
            account_picker::render(frame, area, view, *cursor, ascii);
        }
        Modal::PrCreate {
            title,
            body,
            base,
            heads,
            draft,
            field,
        } => pr_create::render(
            frame,
            area,
            pr_create::Fields {
                title,
                body,
                base,
                heads,
                draft: *draft,
                field: *field,
            },
            ascii,
        ),
        Modal::NoticeLog { selected } => notice_log::render(frame, area, state, *selected),
        Modal::ProgressMenu { menu, cursor } => {
            progress_menu::render(frame, area, menu, *cursor, ascii)
        }
        Modal::MainlinePicker {
            target,
            summary,
            parents,
            cursor,
        } => mainline_picker::render(frame, area, target, summary, parents, *cursor, ascii),
        Modal::Help { pane, cursor } => help::render(frame, area, *pane, *cursor, ascii),
        Modal::RebaseEditor {
            onto,
            plan,
            cursor,
            reword,
            ..
        } => match (reword, plan.entries.get(*cursor)) {
            // 入力のあいだは一覧を隠す（07 の「RebaseEditor」）。
            (RewordInput::Open(buf), Some(entry)) => {
                rebase_editor::render_reword(frame, area, &entry.oid, buf, ascii);
            }
            _ => rebase_editor::render(frame, area, onto, plan, *cursor, ascii),
        },
    }
}

/// モーダルの既定の幅。狭い端末では画面の幅まで縮む。
///
/// **`Confirm` と `Input` で同じ**。「同じ」をコメントで主張すると、片方を広げた
/// ときに主張だけが残る。広く取る理由があるモーダル（`CommitMessage` は 72 桁で
/// 折る慣習を目で測れるようにする）だけが自分の値を持つ。
pub const WIDTH: u16 = 60;

/// 本文とキーの案内のあいだに空ける行数。
const GAP_ROWS: u16 = 1;

/// キーの案内。外側が行（手で分けた切れ目）、内側が群（`[キー] 動詞`）。
///
/// **群の並びのまま受ける。** 1 本の文字列で受けて区切りの空白で割り直す形だと、
/// 区切りを守っているかを誰も見ない —— 空白 2 で並べた群は 1 つと読まれて折れずに
/// 切れ、群の中に空白 3 を書けばそこで割れる。群を繋ぐのは [`place`] だけ。
pub type Hint<'a> = &'a [&'a [&'a str]];

/// 案内の群のあいだに置く空白。
const HINT_GAP: &str = "   ";

/// モーダルの枠。
///
/// **右に余白を持たない。** 折り返しが 1 セルはみ出す件の余白は `ui::wrapped` が
/// 持つ（07 の「モーダル」）。ここにも載せていたころは本文の余白が 2 重にかかっていた。
///
/// 字はペインの細線と同じ組（`ui::border_set`）。`ascii` で `+-|` に倒れる。
pub fn block(title: &str, ascii: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_set(ui::border_set(false, ascii))
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
/// 本文が伸びたときに案内だけが枠の外へ押し出される。**案内は [`fold_hint`] で
/// 箱の内側の幅に折った行数だけ取る**（07 の「モーダルのキーの案内」）—— 1 行に
/// 収まらないキーを本文へ押し出さず、右端で切りもしない。
pub fn place(
    frame: &mut Frame,
    area: Rect,
    block: Block<'_>,
    want: u16,
    body_rows: u16,
    hint: Hint<'_>,
) -> Rect {
    let probe = probe(area, want);
    let probe_inner = block.inner(probe);
    let border_rows = probe.height - probe_inner.height;
    let hint = fold_hint(hint, probe_inner.width);
    // 高さの見積もりと `Layout` で同じものを使う。
    let hint_rows = u16::try_from(hint.len()).unwrap_or(u16::MAX);

    // 本文 + 空行 + キーの案内 + 枠。
    let height = body_rows.saturating_add(GAP_ROWS + hint_rows + border_rows);
    let area = centered(area, probe.width, height);
    let inner = block.inner(area);

    // 下のペインを `Clear` で消してから描く。覆わずに重ねると下の文字が透けて読めない。
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let [body, hint_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(hint_rows)]).areas(inner);
    let hint: Vec<Line<'_>> = hint.into_iter().map(Line::from).collect();
    frame.render_widget(Paragraph::new(hint), hint_area);
    body
}

/// キーの案内を幅 `width` に折る。
///
/// **群の途中では割らない。** 前から詰めて、入らない群を次の行へ送る。**手で分けた
/// 行はそのまま行の切れ目**（RebaseEditor は動作のキーと移動・実行のキーを意味で
/// 分けている）。収まるかは組んだ字から数える（[`ui::fits`]）。
///
/// 群 1 つが `width` より長ければ、その群だけで 1 行にする（描くときに右端で切れる）。
/// `ratatui` の `Wrap` に任せないのは、あれが単語（空白 1）で割るので群が割れるから。
fn fold_hint(hint: Hint<'_>, width: u16) -> Vec<String> {
    let mut rows = Vec::new();
    for line in hint {
        let mut row: Vec<&str> = Vec::new();
        for &group in *line {
            row.push(group);
            if row.len() > 1 && !ui::fits(&hint_row(&row), width) {
                row.pop();
                rows.push(hint_row(&row));
                row = vec![group];
            }
        }
        rows.push(hint_row(&row));
    }
    rows
}

/// 群を 1 行に繋ぐ。**描く行と同じ字**なので、テストが行を丸ごと突き合わせるのにも使う。
pub fn hint_row(groups: &[&str]) -> String {
    groups.join(HINT_GAP)
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

/// 端末カーソルを置く**領域内の**桁と、本文を横にずらす幅。
///
/// **列からバイト位置への変換は [`TextBuffer::before_cursor`] に任せる。** 「列は
/// 文字数」という約束を 2 箇所で解くと、単位を変えたときに core だけ直して端末の
/// カーソルが黙ってずれる（01 の `TextBuffer`）。桁の数え方は ratatui の
/// [`Span::width`] に合わせ、描画と同じにする。
///
/// **返すのはバッファ上の桁ではなく、ずらしたあとの見える桁。** 生の桁を返すと
/// 使う側が `area.x + col - scroll_x` を書くことになり、その式が利用者のぶんだけ
/// 増える（`CommitMessage` と `Input` の 2 人目が出た時点でここへ移した）。
/// 編集中のモーダルはどちらも折り返さないので、カーソルが右端を越えたぶんだけ横に
/// ずらすのが両方に共通の約束になる。
/// 編集中のバッファを `area` に描き、端末カーソルを置く位置を返す。
///
/// **配線ごと共有する。** [`cursor_col`] は「どの桁か」を答える純粋関数で、その戻り値を
/// [`Paragraph::scroll`] と `set_cursor_position` に**渡すところ**は各 `render` に
/// 残っていた。#82 で「共有ヘルパが試されていることは、その戻り値を使う配線が
/// 試されていることではない」と書いたのは**配線が 2 箇所だったとき**の結論で、
/// `Modal::PrCreate` が 2 つの欄を足して 4 箇所になったので寄せた
/// （`.claude/rules/rust-coding.md` の「2 つ目の利用者が出た時点で共有先を作る」）。
///
/// **折り返さない**（07）。折り返すと `ui` が出す桁とバッファの列が食い違い、
/// カーソルの位置を出せなくなる。**ずらす幅は渡された `area` の幅** —— 欄にラベルが
/// 付くモーダル（`PrCreate`）では箱の幅ではないので、測る側が決める。
///
/// **高さが足りなければカーソルを置かない**（`None`）。リサイズの途中では
/// 本文の行が 1 行も残らない領域が来る。**カーソルの行が窓の外**のときも置かない ——
/// 窓は [`ui::window`] がカーソルの行を入れるように切るが、高さ 0 では切れない。
pub fn edit_area(frame: &mut Frame, area: Rect, buf: &TextBuffer) -> Option<Position> {
    let row = buf.cursor.0;
    // 本文が領域より高ければ、カーソルの行が入るところまでずらす。
    let shown = ui::window(buf.lines.len(), row..row + 1, area.height as usize);
    let (col, scroll_x) = cursor_col(buf, area.width);

    let lines: Vec<Line<'_>> = buf.lines[shown.clone()]
        .iter()
        .map(|line| Line::from(line.as_str()))
        .collect();
    frame.render_widget(Paragraph::new(lines).scroll((0, scroll_x)), area);

    // **端末のカーソルを本文の位置に置く。** 反転の升目を自前で描かない。編集中は
    // 端末側のカーソルが出ているのが自然で、行を選ぶ `Confirm` とは別物（07）。
    (area.width > 0 && shown.contains(&row)).then(|| Position {
        x: area.x + col,
        y: area.y + u16::try_from(row - shown.start).unwrap_or(u16::MAX),
    })
}

pub fn cursor_col(buf: &TextBuffer, width: u16) -> (u16, u16) {
    // **`Line::from` を通さない。** あちらは幅を測るためだけに `Vec<Span>` を
    // 1 本確保する。`Span` は `&str` を借りるだけで、`width()` の中身は同じ。
    let col = u16::try_from(Span::raw(buf.before_cursor()).width()).unwrap_or(u16::MAX);
    let scroll_x = col.saturating_sub(width.saturating_sub(1));
    (col - scroll_x, scroll_x)
}

#[cfg(test)]
mod tests {
    use super::{cursor_col, fold_hint, Hint};
    use crate::test_util::{draw, modal_text, row_of, state_with_modal, terminal_with};
    use mewgit_core::app::{Cmd, Modal, TermSize, TextBuffer};

    const SIZE: TermSize = TermSize {
        width: 100,
        height: 24,
    };

    #[test]
    fn the_box_keeps_its_border_when_a_line_just_overflows() {
        // **ratatui の折り返しは 1 セルはみ出す**（`ui::WRAP_MARGIN`）。はみ出した
        // 全角 1 文字が枠の縦線を上書きするので、**箱の右辺がその行だけ消える**。
        //
        // **踏むには幅がぴったり `内側 + 1` セルで、最後が全角の本文が要る。**
        // 下は 10 + 1 + 25 + 1 + 22 = 59 セル（枠の内側は 58）。1 セル短いと通る。
        // `Confirm` から在った形だが、実機で出たのは `gh` の `message`（全角と
        // 識別子が混ざる）を出す `AccountPicker` を入れてから。**枠の余白
        // （`RIGHT_MARGIN`）を外したあとも、`ui::WRAP_MARGIN` を 0 にすると落ちる**
        // （実測 2026-10-04）。
        let state = state_with_modal(Modal::Confirm {
            title: "確認".to_string(),
            body: "設定が指す nobody-does-not-exist-xyz のトークンがありません".to_string(),
            on_yes: Cmd::None,
        });
        let lines = draw(&state, SIZE);
        let body = row_of(&lines, "設定が指す");
        // キーの案内は ASCII だけなので、はみ出しようがない = 正しい枠の位置。
        let hint = row_of(&lines, "[y] 実行");

        let terminal = terminal_with(&state, SIZE);
        let buffer = terminal.backend().buffer();
        let borders = |row: u16| -> Vec<u16> {
            (0..SIZE.width)
                .filter(|&x| buffer[(x, row)].symbol() == "│")
                .collect()
        };

        assert_eq!(borders(body), borders(hint), "本文が枠の縦線を食っている");
    }

    #[test]
    fn a_hint_wider_than_the_box_breaks_between_groups() {
        // **群の途中では割らない**（07 の「モーダルのキーの案内」）。`[Shift+Enter /
        // Ctrl-J] 改行` は中に空白を持つので、単語で割る `Wrap` に任せると割れる。
        // 46 セル = 16 + 3 + 27。
        let hint: Hint<'_> = &[&[
            "[Enter] コミット",
            "[Shift+Enter / Ctrl-J] 改行",
            "[Esc] やめる",
        ]];

        assert_eq!(
            fold_hint(hint, 46),
            [
                "[Enter] コミット   [Shift+Enter / Ctrl-J] 改行",
                "[Esc] やめる"
            ]
        );
        // 1 セル狭いと 2 つ目の群も送る。
        assert_eq!(
            fold_hint(hint, 45),
            [
                "[Enter] コミット",
                "[Shift+Enter / Ctrl-J] 改行   [Esc] やめる"
            ]
        );
    }

    #[test]
    fn a_line_break_written_by_hand_is_kept() {
        // RebaseEditor は動作のキーと移動・実行のキーを意味で分けて折っている。幅が
        // 足りても混ぜない。
        assert_eq!(
            fold_hint(&[&["[p] pick"], &["[Esc] やめる"]], 80),
            ["[p] pick", "[Esc] やめる"]
        );
    }

    #[test]
    fn a_group_wider_than_the_box_takes_a_row_of_its_own() {
        // 割れないものは割らない。描くときに右端で切れる（07 の「残る限界」）。
        assert_eq!(
            fold_hint(&[&["[a] x", "[Shift+Enter / Ctrl-J] 改行", "[b] y"]], 10),
            ["[a] x", "[Shift+Enter / Ctrl-J] 改行", "[b] y"]
        );
    }

    #[test]
    fn a_folded_hint_grows_the_box_and_keeps_every_key() {
        // **右端で切らない。** 折る前は箱が端末の幅まで縮むと末尾の `[Esc]` から
        // 消えた（PrCreate は 73 桁未満。07）。折ったぶん箱を伸ばし、本文を削らない。
        let state = state_with_modal(Modal::CommitMessage {
            buf: TextBuffer::from_text("subject"),
            amend: false,
        });
        let narrow = TermSize { width: 50, ..SIZE };

        let wide = modal_text(&state, SIZE);
        let text = modal_text(&state, narrow);

        assert!(text.contains("[Esc] やめる"), "{text}");
        assert!(text.contains("subject"), "{text}");
        assert_eq!(
            text.lines().count(),
            wide.lines().count() + 1,
            "案内が 2 行にならない:\n{text}"
        );
    }

    #[test]
    fn a_line_wider_than_the_area_scrolls_until_the_cursor_is_at_the_right_edge() {
        // 折り返さないので、横にずらさないとカーソルが領域の外に出る。
        let buf = TextBuffer::from_text(&"x".repeat(30));

        let (col, scroll_x) = cursor_col(&buf, 10);

        assert_eq!(col, 9, "カーソルが領域の右端に来ていません");
        assert_eq!(scroll_x, 21);
    }
}
