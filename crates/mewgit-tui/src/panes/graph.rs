//! グラフペインの描画。設計は `docs/design/07-ui.md` と `docs/design/06-graph.md`。
//!
//! 1 行は `罫線  短縮 Oid  ラベル  summary`。先頭にワーキングツリーを表す WIP 行が
//! 1 つ入る。罫線のセルを文字と色に写すのは `ui::lanes`。
//!
//! **コミットの詳細はここに描かない。** 右ペインに出す（`panes::commit`）。M1 では
//! `Enter` でペインの下部に展開していたので、グラフの高さが `expanded` で変わり、
//! `ui::graph_rows` と `Msg::Layout` が「詳細を出すか」を運んでいた。差し替えではなく
//! 分割だったのが原因で、右へ移すとその引数ごと消えた（07 の「CommitDetail ペイン」）。

use mewgit_core::app::{AppState, GraphLine, JobKind, Pane};
use mewgit_core::git_read::RefLabel;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::ui::{self, lanes, theme};

/// ペインの枠。`focused` はフォーカスの強調だけに効き、内側の大きさは変えない。
fn block(focused: bool) -> Block<'static> {
    ui::pane_block("Graph".to_string(), focused)
}

/// 行を描ける領域。`ui::graph_rows` が行数を出すのに使う。
///
/// **枠の内側そのまま。** 高さは端末サイズだけで決まる（07 の「CommitDetail ペイン」）。
pub fn area(area: Rect) -> Rect {
    block(false).inner(area)
}

/// グラフペインを描く。
pub fn render(frame: &mut Frame, outer: Rect, state: &AppState) {
    let block = block(state.focus == Pane::Graph);
    // `block()` はタイトルを確保するので 1 フレームに 1 回だけ作る。
    let rows = block.inner(outer);
    frame.render_widget(block, outer);

    let graph = &state.graph;
    // 見えている表示行だけを切り出す。表示行 0 は WIP 行なので、コミットの行は
    // 1 つずらして取る（`GraphState::selection` と同じ対応）。
    let start = graph.scroll.min(graph.rows());
    let end = (start + rows.height as usize).min(graph.rows());
    let visible = &graph.lines[start.saturating_sub(1)..end.saturating_sub(1)];

    let view = View {
        state,
        head: theme::style(&state.config.theme.graph_head),
        // 罫線の桁は、いま描く行の最大レーン数に揃える（07）。読み込み済みの
        // 全行で最大を取ると、複雑な履歴で桁が育ち続ける。
        lanes: visible
            .iter()
            .map(|line| line.row.cells.len())
            .max()
            .unwrap_or(0),
        palette: lanes::palette(&state.config.graph.colors),
    };

    let mut lines: Vec<Line> = Vec::with_capacity(end.saturating_sub(start) + 1);
    if start == 0 {
        lines.push(view.wip_line());
        // コミットが 1 つも無いときは、読み込み中なのか本当に空なのかを WIP 行の下に
        // 出す。ペインごと差し替えないのは、WIP 行だけは常に選べるため。
        if graph.lines.is_empty() {
            lines.push(Line::from(Span::styled(
                empty_message(state),
                theme::muted(),
            )));
        }
    }
    // 先頭が WIP 行なので、コミットの表示行は 1 から始まる。
    let first = start.max(1);
    lines.extend(
        visible
            .iter()
            .enumerate()
            .map(|(offset, line)| view.commit_line(first + offset, line)),
    );

    frame.render_widget(Paragraph::new(lines), rows);
}

/// 1 フレームのあいだ変わらない描画条件。行ごとに引き回す値をまとめる。
struct View<'a> {
    state: &'a AppState,
    /// HEAD のブランチラベルのスタイル。
    head: Style,
    /// 罫線に使うレーン数。
    lanes: usize,
    palette: Vec<Color>,
}

/// コミットが 1 つも無いときに出す文言。
///
/// 読み込みが失敗したときに「読み込み中…」のままにしない。エラーの通知は
/// ステータスバーに出るが、次の通知で上書きされる。
fn empty_message(state: &AppState) -> &'static str {
    if state.jobs.is_running(JobKind::LoadLog) {
        "読み込み中…"
    } else if state.graph.load_failed {
        "コミットを読み込めませんでした"
    } else {
        "コミットがありません"
    }
}

impl<'a> View<'a> {
    /// 先頭の WIP 行。`◇  WIP (n)`。
    ///
    /// **罫線を繋がない**（06 の「責務」）。`◇` を罫線の桁の先頭に置き、残りは空白で
    /// 埋める。埋める幅に短縮 Oid のぶんも含めるのは、`WIP (n)` を下の行の summary と
    /// 同じ桁に揃えるため。
    fn wip_line(&self) -> Line<'static> {
        let ascii = self.state.config.graph.ascii;
        let glyph = lanes::wip_glyph(ascii);
        let pad = lanes::width(self.lanes) + ui::SHORT_OID + 1 - glyph.chars().count();

        let line = Line::from(vec![
            Span::raw(glyph),
            Span::raw(" ".repeat(pad)),
            Span::raw(format!("WIP ({})", self.state.worktree.len())),
        ]);
        if self.state.graph.cursor == 0 {
            line.style(theme::selected())
        } else {
            line
        }
    }

    /// 1 行分。`罫線  短縮 Oid  ラベル  summary`。`index` は表示行の添字。
    fn commit_line(&self, index: usize, line: &'a GraphLine) -> Line<'a> {
        // `&self` より長い借用が要る。ラベルは `state` から借りて行に載せるため。
        let state = self.state;
        let labels = state.graph.refs.labels(line.commit.oid);
        // 罫線をそのまま土台にする。Oid・空白・ラベル 1 件あたり 4 つ・summary の
        // ぶんを足して、詰め直しを起こさない大きさにする。
        let mut spans: Vec<Span<'a>> = lanes::spans(
            &line.row,
            self.lanes,
            &self.palette,
            state.config.graph.ascii,
        );
        spans.reserve(3 + labels.len() * 4);

        spans.push(Span::styled(
            ui::short_oid(&line.commit.oid),
            theme::muted(),
        ));
        spans.push(Span::raw(" "));

        for label in labels {
            // 飾りは組み立てずに 3 つのスパンに分ける。毎フレーム全行ぶん `format!` する
            // 必要が無く、HEAD の強調も括弧まで掛かる。
            let style = label_style(label, self.head);
            let (open, close) = brackets(label);
            spans.push(Span::styled(open, style));
            spans.push(Span::styled(label.name(), style));
            spans.push(Span::styled(close, style));
            spans.push(Span::raw(" "));
        }

        spans.push(Span::raw(line.commit.summary.as_str()));

        let line = Line::from(spans);
        if index == state.graph.cursor {
            // 選択行は反転（`theme::selected`。worktree ペインと同じ見た目）。
            line.style(theme::selected())
        } else {
            line
        }
    }
}

/// 種別ごとの飾り（06 の「ラベル」）。
fn brackets(label: &RefLabel) -> (&'static str, &'static str) {
    match label {
        RefLabel::Local { .. } => ("[", "]"),
        RefLabel::Remote { .. } => ("{", "}"),
        RefLabel::Tag { .. } => ("<", ">"),
    }
}

/// ラベルの見た目。HEAD のブランチだけ強調する。
fn label_style(label: &RefLabel, head_style: Style) -> Style {
    match label {
        RefLabel::Local { is_head: true, .. } => head_style,
        _ => Style::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{
        draw, graph_rows, state_at, state_with_commits, state_with_history, terminal_with,
    };
    use mewgit_core::app::{init, update, GraphSelection, Msg, TermSize};
    use mewgit_core::git_read::{GitReadError, Oid, RefIndex, RefLabel};
    use mewgit_core::test_util::fake_commit;
    use ratatui::buffer::Buffer;
    use ratatui::style::Modifier;

    /// WIP 行が出る画面の行。ヘッダ 1 行 + ペインの枠の上辺 1 行。
    const WIP_ROW: usize = 2;

    /// 1 件目のコミットが出る画面の行。WIP 行の 1 つ下（07 の「Graph ペイン」）。
    const FIRST_ROW: usize = WIP_ROW + 1;

    /// その行で最初に `symbol` が出る桁。
    ///
    /// 桁を直に書かない。罫線が入ると行頭からの位置が動くうえ、`●` のように
    /// 幅が環境で変わりうる文字が前に来る。
    fn column_of(buffer: &Buffer, y: u16, symbol: &str) -> Option<u16> {
        (0..buffer.area.width).find(|&x| buffer[(x, y)].symbol() == symbol)
    }

    #[test]
    fn each_commit_gets_a_line_with_its_short_oid_and_summary() {
        let state = state_with_commits(&["newest", "oldest"]);

        let lines = draw(&state, TermSize::default());

        // 枠の中の 1 行目は WIP 行。コミットはその下から、`git_read` が決めた順のまま。
        assert!(
            lines[FIRST_ROW].contains("newest"),
            "{:?}",
            lines[FIRST_ROW]
        );
        assert!(lines[FIRST_ROW + 1].contains("oldest"), "{:?}", lines[3]);
        let oid = state.graph.lines[0].commit.oid.to_string();
        assert!(
            lines[FIRST_ROW].contains(&format!("{} newest", &oid[..ui::SHORT_OID])),
            "短縮 Oid が summary の前にありません: {:?}",
            lines[FIRST_ROW]
        );
        assert!(
            !lines[FIRST_ROW].contains(&oid[..ui::SHORT_OID + 1]),
            "Oid を切り詰めていません: {:?}",
            lines[FIRST_ROW]
        );
    }

    #[test]
    fn the_first_row_is_the_wip_row_and_counts_the_changes() {
        let mut state = state_with_commits(&["first"]);
        state.worktree.snapshot =
            mewgit_core::test_util::fake_snapshot(&["a.txt"], &["b.txt"], &[]);

        let lines = draw(&state, TermSize::default());

        assert!(lines[WIP_ROW].contains("◇"), "{:?}", lines[WIP_ROW]);
        assert!(lines[WIP_ROW].contains("WIP (2)"), "{:?}", lines[WIP_ROW]);
    }

    #[test]
    fn the_wip_row_is_there_even_with_no_changes_and_no_commits() {
        // 件数で出し入れすると status が届くたびに表示行数が変わり、カーソルの
        // 座り直しが要る（01 の「グラフの行」）。空のリポジトリでも 1 行目は WIP。
        let state = state_with_commits(&[]);

        let lines = draw(&state, TermSize::default());

        assert!(lines[WIP_ROW].contains("WIP (0)"), "{:?}", lines[WIP_ROW]);
    }

    #[test]
    fn the_wip_row_has_no_lane_art_but_lines_up_with_the_summaries() {
        // 罫線は繋がない（06 の「責務」）。それでも本文の桁は下の行と揃える。
        let state = state_with_history(MERGE);

        let lines = draw(&state, TermSize::default());

        assert_eq!(art(&lines, WIP_ROW), "◇   ");
        // 桁で比べる。`str::find` はバイト位置を返すので、`◇` や枠の `│` のぶんずれる。
        let summary = lines[FIRST_ROW]
            .chars()
            .position(|c| c.is_ascii_hexdigit())
            .expect("Oid がありません")
            + ui::SHORT_OID
            + 1;
        let wip = lines[WIP_ROW]
            .chars()
            .position(|c| c == 'W')
            .expect("WIP 行がありません");
        assert_eq!(
            wip, summary,
            "{:?} / {:?}",
            lines[WIP_ROW], lines[FIRST_ROW]
        );
    }

    #[test]
    fn ascii_replaces_the_wip_glyph_too() {
        // `◇` は罫線と同じ East Asian Ambiguous 幅。逃げ道は罫線と同じ `ascii`（06）。
        let mut state = state_with_commits(&["first"]);
        state.config.graph.ascii = true;

        let lines = draw(&state, TermSize::default());

        assert!(!lines[WIP_ROW].contains("◇"), "{:?}", lines[WIP_ROW]);
        assert!(lines[WIP_ROW].contains("@ "), "{:?}", lines[WIP_ROW]);
    }

    #[test]
    fn ref_labels_are_decorated_by_kind() {
        let mut state = state_with_commits(&["first"]);
        let oid = state.graph.lines[0].commit.oid;
        state.graph.refs = RefIndex::from_iter([
            (
                oid,
                RefLabel::Local {
                    name: "main".to_string(),
                    is_head: true,
                },
            ),
            (
                oid,
                RefLabel::Remote {
                    name: "origin/main".to_string(),
                },
            ),
            (
                oid,
                RefLabel::Tag {
                    name: "v1.0".to_string(),
                },
            ),
        ]);

        let lines = draw(
            &state,
            TermSize {
                width: 100,
                height: 24,
            },
        );

        // 種別ごとの飾りは 06 の「ラベル」。並びは Local → Remote → Tag。
        assert!(
            lines[FIRST_ROW].contains("[main] {origin/main} <v1.0>"),
            "{:?}",
            lines[FIRST_ROW]
        );
    }

    #[test]
    fn the_head_branch_label_is_emphasised() {
        // 既定の `theme.graph_head` は "bold"。色として読むと強調がまるごと消える。
        let mut state = state_with_commits(&["first"]);
        let oid = state.graph.lines[0].commit.oid;
        state.graph.refs = RefIndex::from_iter([(
            oid,
            RefLabel::Local {
                name: "main".to_string(),
                is_head: true,
            },
        )]);

        let terminal = terminal_with(&state, TermSize::default());
        let buffer = terminal.backend().buffer();

        let y = FIRST_ROW as u16;
        let x = column_of(buffer, y, "[").expect("ラベルが描かれていません");
        assert!(buffer[(x, y)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn a_label_that_is_not_head_is_not_emphasised() {
        let mut state = state_with_commits(&["first"]);
        let oid = state.graph.lines[0].commit.oid;
        state.graph.refs = RefIndex::from_iter([(
            oid,
            RefLabel::Remote {
                name: "origin/main".to_string(),
            },
        )]);

        let terminal = terminal_with(&state, TermSize::default());
        let buffer = terminal.backend().buffer();

        let y = FIRST_ROW as u16;
        let x = column_of(buffer, y, "{").expect("ラベルが描かれていません");
        assert!(!buffer[(x, y)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn the_cursor_line_is_highlighted() {
        // `cursor` は表示行の添字。`1` は WIP 行の 1 つ下、つまり 1 件目のコミット。
        let mut state = state_with_commits(&["first", "second"]);
        state.graph.cursor = 1;

        let terminal = terminal_with(&state, TermSize::default());
        let buffer = terminal.backend().buffer();

        assert!(!buffer[(1, WIP_ROW as u16)]
            .modifier
            .contains(Modifier::REVERSED));
        assert!(buffer[(1, FIRST_ROW as u16)]
            .modifier
            .contains(Modifier::REVERSED));
    }

    #[test]
    fn the_wip_row_is_highlighted_when_it_is_selected() {
        // 既定のカーソルは WIP 行（`cursor == 0`）。
        let state = state_with_commits(&["first"]);

        let terminal = terminal_with(&state, TermSize::default());
        let buffer = terminal.backend().buffer();

        assert!(buffer[(1, WIP_ROW as u16)]
            .modifier
            .contains(Modifier::REVERSED));
    }

    #[test]
    fn scrolling_moves_the_first_visible_line() {
        let summaries: Vec<String> = (0..5).map(|i| format!("commit {i}")).collect();
        let refs: Vec<&str> = summaries.iter().map(String::as_str).collect();
        let mut state = state_with_commits(&refs);
        state.graph.scroll = 2;

        let lines = draw(&state, TermSize::default());

        // 表示行 0 は WIP 行なので、2 行スクロールすると `lines[1]` が先頭に来る。
        assert!(lines[WIP_ROW].contains("commit 1"), "{:?}", lines[WIP_ROW]);
        assert!(!lines.iter().any(|line| line.contains("commit 0 ")));
        assert!(!lines.iter().any(|line| line.contains("WIP")));
    }

    #[test]
    fn only_the_visible_rows_are_drawn() {
        let summaries: Vec<String> = (0..30).map(|i| format!("commit {i:02}")).collect();
        let refs: Vec<&str> = summaries.iter().map(String::as_str).collect();
        let state = state_with_commits(&refs);

        let lines = draw(&state, TermSize::default());

        let drawn = lines.iter().filter(|line| line.contains("commit ")).count();
        // `ui::graph_rows` が `app` に渡す行数と、実際に描く行数は同じでなければならない。
        // ずれるとカーソルが画面の外に出る。先頭の WIP 行は "commit " を含まないので
        // 1 を足して比べる。
        assert_eq!(drawn + 1, graph_rows(&state));
    }

    #[test]
    fn the_graph_height_does_not_depend_on_what_is_selected() {
        // コミット詳細は右ペインに出るので、選択が変わってもグラフの高さは動かない
        // （07 の「CommitDetail ペイン」）。動くと `Msg::Layout` が毎回流れる。
        let mut state = state_with_commits(&["first", "second"]);
        assert_eq!(state.graph.selection(), GraphSelection::Wip);
        let on_wip = graph_rows(&state);

        state.graph.cursor = 1;
        assert!(matches!(state.graph.selection(), GraphSelection::Commit(_)));

        assert_eq!(graph_rows(&state), on_wip);
    }

    #[test]
    fn an_empty_repository_says_so_under_the_wip_row() {
        let state = state_with_commits(&[]);

        let lines = draw(&state, TermSize::default());

        assert!(
            lines[FIRST_ROW].contains("コミットがありません"),
            "{:?}",
            lines[FIRST_ROW]
        );
    }

    #[test]
    fn before_the_first_page_arrives_it_says_loading() {
        let mut state = state_at("/repo");
        let _ = init(&mut state);

        let lines = draw(&state, TermSize::default());

        assert!(
            lines[FIRST_ROW].contains("読み込み中"),
            "{:?}",
            lines[FIRST_ROW]
        );
    }

    #[test]
    fn a_failed_load_does_not_keep_saying_loading() {
        // 失敗の通知はステータスバーに出るが、次の通知で上書きされる。
        // ペインが「読み込み中…」のままだと、何も起きていないように見える。
        let mut state = state_at("/repo");
        let _ = init(&mut state);
        let _ = update(
            &mut state,
            Msg::LogLoaded {
                restart: false,
                result: Err(GitReadError::NonUtf8Head),
            },
        );

        let lines = draw(&state, TermSize::default());

        assert!(
            lines[FIRST_ROW].contains("読み込めませんでした"),
            "{:?}",
            lines[FIRST_ROW]
        );
    }

    #[test]
    fn a_wide_character_summary_is_not_split() {
        let state = state_with_commits(&["日本語のコミットメッセージ"]);

        let lines = draw(&state, TermSize::default());

        assert!(
            lines[FIRST_ROW].contains("日本語のコミットメッセージ"),
            "{:?}",
            lines[FIRST_ROW]
        );
    }

    /// テストで描く履歴のレーン数。どのケースも 2 本に収まる。
    const LANES: usize = 2;

    /// 罫線の桁だけを切り出す。行頭の 1 桁はペインの枠。
    fn art(lines: &[String], row: usize) -> String {
        lines[row]
            .chars()
            .skip(1)
            .take(lanes::width(LANES))
            .collect()
    }

    /// 枠 1 桁 + 罫線のうしろから本文が始まる。
    const TEXT_COLUMN: usize = 1 + lanes::width(LANES);

    /// マージのあるコミット列。`graph::lane::tests` の
    /// `a_merge_opens_a_lane_for_its_second_parent` と同じ形。
    const MERGE: &[(u8, &[u8])] = &[(1, &[2, 3]), (2, &[4]), (3, &[4]), (4, &[])];

    #[test]
    fn lanes_are_drawn_as_the_same_art_that_graph_lane_tests_fix() {
        // **`graph::lane::tests` の期待値と同じ文字列**。片方を変えたら両方直す
        // （対応表が core のテスト用レンダラと `ui::lanes` の 2 箇所にあるため）。
        let state = state_with_history(MERGE);

        let lines = draw(&state, TermSize::default());

        assert_eq!(art(&lines, FIRST_ROW), "○─╮ ");
        assert_eq!(art(&lines, FIRST_ROW + 1), "● │ ");
        assert_eq!(art(&lines, FIRST_ROW + 2), "│ ● ");
        assert_eq!(art(&lines, FIRST_ROW + 3), "●─╯ ");
    }

    #[test]
    fn a_second_parent_waiting_on_the_left_draws_a_line_back_to_it() {
        // `├`（`Through` + `Right`）を通す唯一のケース。`MERGE` では出ない。
        // 形は `graph::lane::tests` の
        // `a_second_parent_that_is_already_waiting_is_reached_by_a_line` と同じ。
        let state = state_with_history(&[(1, &[3]), (2, &[4, 3]), (4, &[3]), (3, &[])]);

        let lines = draw(&state, TermSize::default());

        assert_eq!(art(&lines, FIRST_ROW), "●   ");
        assert_eq!(art(&lines, FIRST_ROW + 1), "├─○ ");
        assert_eq!(art(&lines, FIRST_ROW + 2), "│ ● ");
        assert_eq!(art(&lines, FIRST_ROW + 3), "●─╯ ");
    }

    #[test]
    fn ascii_replaces_the_box_drawing_characters() {
        let mut state = state_with_history(MERGE);
        state.config.graph.ascii = true;

        let lines = draw(&state, TermSize::default());

        assert_eq!(art(&lines, FIRST_ROW), "o-\\ ");
        assert_eq!(art(&lines, FIRST_ROW + 1), "* | ");
        assert_eq!(art(&lines, FIRST_ROW + 3), "*-/ ");
    }

    #[test]
    fn each_lane_takes_a_colour_from_the_palette() {
        let state = state_with_history(MERGE);

        let terminal = terminal_with(&state, TermSize::default());
        let buffer = terminal.backend().buffer();

        // 既定の palette は blue, magenta, …（08）。第 1 親のレーンは 0 番、
        // 第 2 親に開いたレーンは 1 番。
        let y = FIRST_ROW as u16;
        assert_eq!(buffer[(1, y)].symbol(), "○");
        assert_eq!(buffer[(1, y)].fg, Color::Blue);
        assert_eq!(buffer[(3, y)].symbol(), "╮");
        assert_eq!(buffer[(3, y)].fg, Color::Magenta);
    }

    #[test]
    fn an_empty_palette_falls_back_to_the_terminal_colour() {
        // `colors = []` でも割り算で落ちない。
        let mut state = state_with_history(MERGE);
        state.config.graph.colors = Vec::new();

        let terminal = terminal_with(&state, TermSize::default());
        let buffer = terminal.backend().buffer();

        let y = FIRST_ROW as u16;
        assert_eq!(buffer[(1, y)].symbol(), "○");
        assert_eq!(buffer[(1, y)].fg, Color::Reset);
    }

    #[test]
    fn an_unreadable_colour_name_keeps_the_place_of_the_others() {
        // 読めない名前を飛ばすと `ColorId` を割る周期が設定とずれ、
        // 「2 色書いたのに 1 色で回る」理由を追えなくなる。
        let mut state = state_with_history(MERGE);
        state.config.graph.colors = vec!["nonsense".to_string(), "red".to_string()];

        let terminal = terminal_with(&state, TermSize::default());
        let buffer = terminal.backend().buffer();

        let y = FIRST_ROW as u16;
        assert_eq!(buffer[(1, y)].fg, Color::Reset);
        assert_eq!(buffer[(3, y)].fg, Color::Red);
    }

    #[test]
    fn the_text_starts_at_the_same_column_on_every_row() {
        // 罫線の桁は行ごとのレーン数ではなく、いま描く行の最大に揃える（07）。
        // 揃えないと Oid の桁が行ごとに動く。
        let state = state_with_history(MERGE);

        let lines = draw(&state, TermSize::default());

        let starts: Vec<Option<usize>> = (FIRST_ROW..FIRST_ROW + 4)
            .map(|row| lines[row].chars().position(|c| c.is_ascii_hexdigit()))
            .collect();
        assert_eq!(starts, vec![Some(TEXT_COLUMN); 4], "{starts:?}");
    }

    #[test]
    fn an_unknown_oid_has_no_labels() {
        let state = state_with_commits(&["first"]);
        let other = fake_commit(99, "other").oid;
        assert_ne!(other, Oid::zero());

        assert!(state.graph.refs.labels(other).is_empty());
    }
}
