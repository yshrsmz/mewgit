//! グラフペインの描画。設計は `docs/design/07-ui.md` と `docs/design/06-graph.md`。
//!
//! 1 行は `罫線  短縮 Oid  ラベル  summary`。罫線のセルを文字と色に写すのは
//! `ui::lanes`。

use mewgit_core::app::{AppState, GraphLine, JobKind};
use mewgit_core::git_read::RefLabel;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::ui::{lanes, theme};

/// 短縮 Oid の桁数。`git log --oneline` と同じ。
const SHORT_OID: usize = 7;

/// ペインの枠。
fn block() -> Block<'static> {
    Block::default().borders(Borders::ALL).title("Graph")
}

/// 枠の内側。`ui::graph_rows` が行数を出すのに使う。
pub fn inner(area: Rect) -> Rect {
    block().inner(area)
}

/// グラフペインを描く。
pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let block = block();
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.graph.lines.is_empty() {
        frame.render_widget(Paragraph::new(empty_message(state)), inner);
        return;
    }

    let visible: Vec<(usize, &GraphLine)> = state
        .graph
        .lines
        .iter()
        .enumerate()
        .skip(state.graph.scroll)
        .take(inner.height as usize)
        .collect();

    let view = View {
        state,
        head: theme::style(&state.config.theme.graph_head),
        // 罫線の桁は、いま描く行の最大レーン数に揃える（07）。読み込み済みの
        // 全行で最大を取ると、複雑な履歴で桁が育ち続ける。
        lanes: visible
            .iter()
            .map(|(_, line)| line.row.cells.len())
            .max()
            .unwrap_or(0),
        palette: lanes::palette(&state.config.graph.colors),
        ascii: state.config.graph.ascii,
    };

    let lines: Vec<Line> = visible
        .into_iter()
        .map(|(index, line)| view.commit_line(index, line))
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

/// 1 フレームのあいだ変わらない描画条件。行ごとに引き回す値をまとめる。
struct View<'a> {
    state: &'a AppState,
    /// HEAD のブランチラベルのスタイル。
    head: Style,
    /// 罫線に使うレーン数。
    lanes: usize,
    palette: Vec<Color>,
    ascii: bool,
}

/// 行が 1 つも無いときに出す文言。
///
/// 読み込みが失敗したときに「読み込み中…」のままにしない。エラーの通知は
/// ステータスバーに出るが、次の通知で上書きされる。読み込み中かどうかは
/// `JobTracker` が知っているので、状態を増やさずに区別できる。
fn empty_message(state: &AppState) -> &'static str {
    if state.jobs.is_running(JobKind::LoadLog) {
        "読み込み中…"
    } else if state.graph.loaded_all {
        "コミットがありません"
    } else {
        "コミットを読み込めませんでした"
    }
}

impl<'a> View<'a> {
    /// 1 行分。`罫線  短縮 Oid  ラベル  summary`。
    fn commit_line(&self, index: usize, line: &'a GraphLine) -> Line<'a> {
        // `&self` より長い借用が要る。ラベルは `state` から借りて行に載せるため。
        let state = self.state;
        let labels = state.graph.refs.labels(line.commit.oid);
        // 罫線・Oid・空白・ラベル 1 件あたり 4 つ・summary。詰め直しを起こさない
        // 大きさで取る。
        let mut spans = Vec::with_capacity(lanes::width(self.lanes) + 3 + labels.len() * 4);

        spans.extend(lanes::spans(
            &line.row,
            self.lanes,
            &self.palette,
            self.ascii,
        ));
        spans.push(Span::styled(
            short_oid(line),
            Style::default().add_modifier(Modifier::DIM),
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
            // 選択行は反転。フォーカスの無いペインとの描き分けは M6 のレイアウトで。
            line.style(Style::default().add_modifier(Modifier::REVERSED))
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

/// 先頭 [`SHORT_OID`] 桁。
fn short_oid(line: &GraphLine) -> String {
    let mut text = line.commit.oid.to_string();
    text.truncate(SHORT_OID);
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{draw, state_at, state_with_commits, state_with_history, terminal_with};
    use mewgit_core::app::{init, update, Msg, TermSize};
    use mewgit_core::git_read::{GitReadError, Oid, RefIndex, RefLabel};
    use mewgit_core::test_util::fake_commit;
    use ratatui::buffer::Buffer;

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

        // 枠の中の 1 行目が 1 件目。行の並びは `git_read` が決めた順のまま。
        assert!(lines[2].contains("newest"), "{:?}", lines[2]);
        assert!(lines[3].contains("oldest"), "{:?}", lines[3]);
        let oid = state.graph.lines[0].commit.oid.to_string();
        assert!(
            lines[2].contains(&format!("{} newest", &oid[..SHORT_OID])),
            "短縮 Oid が summary の前にありません: {:?}",
            lines[2]
        );
        assert!(
            !lines[2].contains(&oid[..SHORT_OID + 1]),
            "Oid を切り詰めていません: {:?}",
            lines[2]
        );
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
            lines[2].contains("[main] {origin/main} <v1.0>"),
            "{:?}",
            lines[2]
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

        let x = column_of(buffer, 2, "[").expect("ラベルが描かれていません");
        assert!(buffer[(x, 2)].modifier.contains(Modifier::BOLD));
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

        let x = column_of(buffer, 2, "{").expect("ラベルが描かれていません");
        assert!(!buffer[(x, 2)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn the_cursor_line_is_highlighted() {
        let mut state = state_with_commits(&["first", "second"]);
        state.graph.cursor = 1;

        let terminal = terminal_with(&state, TermSize::default());
        let buffer = terminal.backend().buffer();

        // 枠の中は y=2 から。カーソルは 2 行目（y=3）。
        assert!(!buffer[(1, 2)].modifier.contains(Modifier::REVERSED));
        assert!(buffer[(1, 3)].modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn scrolling_moves_the_first_visible_line() {
        let summaries: Vec<String> = (0..5).map(|i| format!("commit {i}")).collect();
        let refs: Vec<&str> = summaries.iter().map(String::as_str).collect();
        let mut state = state_with_commits(&refs);
        state.graph.scroll = 2;

        let lines = draw(&state, TermSize::default());

        assert!(lines[2].contains("commit 2"), "{:?}", lines[2]);
        assert!(!lines.iter().any(|line| line.contains("commit 0 ")));
    }

    #[test]
    fn only_the_visible_rows_are_drawn() {
        let summaries: Vec<String> = (0..30).map(|i| format!("commit {i:02}")).collect();
        let refs: Vec<&str> = summaries.iter().map(String::as_str).collect();
        let state = state_with_commits(&refs);

        let lines = draw(&state, TermSize::default());

        let drawn = lines.iter().filter(|line| line.contains("commit ")).count();
        // `ui::graph_rows` が `app` に渡す行数と、実際に描く行数は同じでなければならない。
        // ずれるとカーソルが画面の外に出る。
        assert_eq!(drawn, crate::ui::graph_rows(TermSize::default()));
    }

    #[test]
    fn an_empty_repository_says_so_instead_of_showing_nothing() {
        let state = state_with_commits(&[]);

        let lines = draw(&state, TermSize::default());

        assert!(lines[2].contains("コミットがありません"), "{:?}", lines[2]);
    }

    #[test]
    fn before_the_first_page_arrives_it_says_loading() {
        let mut state = state_at("/repo");
        init(&mut state);

        let lines = draw(&state, TermSize::default());

        assert!(lines[2].contains("読み込み中"), "{:?}", lines[2]);
    }

    #[test]
    fn a_failed_load_does_not_keep_saying_loading() {
        // 失敗の通知はステータスバーに出るが、次の通知で上書きされる。
        // ペインが「読み込み中…」のままだと、何も起きていないように見える。
        let mut state = state_at("/repo");
        init(&mut state);
        update(&mut state, Msg::LogLoaded(Err(GitReadError::NonUtf8Head)));

        let lines = draw(&state, TermSize::default());

        assert!(lines[2].contains("読み込めませんでした"), "{:?}", lines[2]);
    }

    #[test]
    fn a_wide_character_summary_is_not_split() {
        let state = state_with_commits(&["日本語のコミットメッセージ"]);

        let lines = draw(&state, TermSize::default());

        assert!(
            lines[2].contains("日本語のコミットメッセージ"),
            "{:?}",
            lines[2]
        );
    }

    /// 罫線の桁だけを切り出す。行頭の 1 桁はペインの枠。
    fn art(lines: &[String], row: usize, lanes: usize) -> String {
        lines[row].chars().skip(1).take(lanes * 2).collect()
    }

    /// 枠 1 桁 + 罫線 4 桁のうしろから本文が始まる。
    const TEXT_COLUMN: usize = 5;

    /// マージのあるコミット列。`graph::lane::tests` の
    /// `a_merge_opens_a_lane_for_its_second_parent` と同じ形。
    const MERGE: &[(u8, &[u8])] = &[(1, &[2, 3]), (2, &[4]), (3, &[4]), (4, &[])];

    #[test]
    fn lanes_are_drawn_as_the_same_art_that_graph_lane_tests_fix() {
        // **`graph::lane::tests` の期待値と同じ文字列**。片方を変えたら両方直す
        // （対応表が core のテスト用レンダラと `ui::lanes` の 2 箇所にあるため）。
        let state = state_with_history(MERGE);

        let lines = draw(&state, TermSize::default());

        assert_eq!(art(&lines, 2, 2), "○─╮ ");
        assert_eq!(art(&lines, 3, 2), "● │ ");
        assert_eq!(art(&lines, 4, 2), "│ ● ");
        assert_eq!(art(&lines, 5, 2), "●─╯ ");
    }

    #[test]
    fn a_second_parent_waiting_on_the_left_draws_a_line_back_to_it() {
        // `graph::lane::tests` の
        // `a_second_parent_that_is_already_waiting_is_reached_by_a_line` と同じ形。
        let state = state_with_history(&[(1, &[3]), (2, &[4, 3]), (4, &[3]), (3, &[])]);

        let lines = draw(&state, TermSize::default());

        assert_eq!(art(&lines, 2, 2), "●   ");
        assert_eq!(art(&lines, 3, 2), "├─○ ");
        assert_eq!(art(&lines, 4, 2), "│ ● ");
        assert_eq!(art(&lines, 5, 2), "●─╯ ");
    }

    #[test]
    fn ascii_replaces_the_box_drawing_characters() {
        let mut state = state_with_history(MERGE);
        state.config.graph.ascii = true;

        let lines = draw(&state, TermSize::default());

        assert_eq!(art(&lines, 2, 2), "o-\\ ");
        assert_eq!(art(&lines, 3, 2), "* | ");
        assert_eq!(art(&lines, 5, 2), "*-/ ");
    }

    #[test]
    fn each_lane_takes_a_colour_from_the_palette() {
        let state = state_with_history(MERGE);

        let terminal = terminal_with(&state, TermSize::default());
        let buffer = terminal.backend().buffer();

        // 既定の palette は blue, magenta, …（08）。第 1 親のレーンは 0 番、
        // 第 2 親に開いたレーンは 1 番。
        assert_eq!(buffer[(1, 2)].symbol(), "○");
        assert_eq!(buffer[(1, 2)].fg, Color::Blue);
        assert_eq!(buffer[(3, 2)].symbol(), "╮");
        assert_eq!(buffer[(3, 2)].fg, Color::Magenta);
    }

    #[test]
    fn an_empty_palette_falls_back_to_the_terminal_colour() {
        // `colors = []` でも割り算で落ちない。
        let mut state = state_with_history(MERGE);
        state.config.graph.colors = Vec::new();

        let terminal = terminal_with(&state, TermSize::default());
        let buffer = terminal.backend().buffer();

        assert_eq!(buffer[(1, 2)].symbol(), "○");
        assert_eq!(buffer[(1, 2)].fg, Color::Reset);
    }

    #[test]
    fn an_unreadable_colour_name_keeps_the_place_of_the_others() {
        // 読めない名前を飛ばすと `ColorId` を割る周期が設定とずれ、
        // 「2 色書いたのに 1 色で回る」理由を追えなくなる。
        let mut state = state_with_history(MERGE);
        state.config.graph.colors = vec!["nonsense".to_string(), "red".to_string()];

        let terminal = terminal_with(&state, TermSize::default());
        let buffer = terminal.backend().buffer();

        assert_eq!(buffer[(1, 2)].fg, Color::Reset);
        assert_eq!(buffer[(3, 2)].fg, Color::Red);
    }

    #[test]
    fn the_text_starts_at_the_same_column_on_every_row() {
        // 罫線の桁は行ごとのレーン数ではなく、いま描く行の最大に揃える（07）。
        // 揃えないと Oid の桁が行ごとに動く。
        let state = state_with_history(MERGE);

        let lines = draw(&state, TermSize::default());

        let starts: Vec<Option<usize>> = (2..6)
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
