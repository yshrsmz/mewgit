//! グラフペインの描画。設計は `docs/design/07-ui.md` と `docs/design/06-graph.md`。
//!
//! レーンの罫線はまだ描かない。`graph::lane` が `GraphRow.cells` を埋めるように
//! なったが、ここではまだ読んでいない。行の左に `cells` を写した列を足すのは
//! `docs/plan.md` M1 の `ui::panes::graph` のタスク。

use mewgit_core::app::{AppState, GraphLine, JobKind};
use mewgit_core::git_read::RefLabel;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::ui::theme;

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

    let head_style = theme::style(&state.config.theme.graph_head);
    let lines: Vec<Line> = state
        .graph
        .lines
        .iter()
        .enumerate()
        .skip(state.graph.scroll)
        .take(inner.height as usize)
        .map(|(index, line)| commit_line(state, index, line, head_style))
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
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

/// 1 行分。`短縮 Oid  ラベル  summary`。
fn commit_line<'a>(
    state: &'a AppState,
    index: usize,
    line: &'a GraphLine,
    head_style: Style,
) -> Line<'a> {
    let labels = state.graph.refs.labels(line.commit.oid);
    // Oid・空白・ラベル 1 件あたり 4 つ・summary。詰め直しを起こさない大きさで取る。
    let mut spans = Vec::with_capacity(3 + labels.len() * 4);

    spans.push(Span::styled(
        short_oid(line),
        Style::default().add_modifier(Modifier::DIM),
    ));
    spans.push(Span::raw(" "));

    for label in labels {
        // 飾りは組み立てずに 3 つのスパンに分ける。毎フレーム全行ぶん `format!` する
        // 必要が無く、HEAD の強調も括弧まで掛かる。
        let style = label_style(label, head_style);
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
    use crate::test_util::{draw, state_at, state_with_commits, terminal_with};
    use mewgit_core::app::{init, update, Msg, TermSize};
    use mewgit_core::git_read::{GitReadError, Oid, RefIndex, RefLabel};
    use mewgit_core::test_util::fake_commit;

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

        // `1234567 [main] …` の `[` の位置。
        assert_eq!(buffer[(9, 2)].symbol(), "[");
        assert!(buffer[(9, 2)].modifier.contains(Modifier::BOLD));
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

        assert_eq!(buffer[(9, 2)].symbol(), "{");
        assert!(!buffer[(9, 2)].modifier.contains(Modifier::BOLD));
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

    #[test]
    fn a_row_without_lanes_draws_no_graph_column() {
        // `cells` が空の行（`GraphRow` を組まずに積んだ行）でも落ちない、罫線も出ない。
        let mut state = state_with_commits(&["first"]);
        state.graph.lines[0].row.cells.clear();

        let lines = draw(&state, TermSize::default());

        // 両端は枠なので、内側だけを見る。
        let inside: String = lines[2].chars().skip(1).take_while(|c| *c != '│').collect();
        assert!(!inside.contains('│'), "{inside:?}");
    }

    #[test]
    fn an_unknown_oid_has_no_labels() {
        let state = state_with_commits(&["first"]);
        let other = fake_commit(99, "other").oid;
        assert_ne!(other, Oid::zero());

        assert!(state.graph.refs.labels(other).is_empty());
    }
}
