//! グラフペインの描画。設計は `docs/design/07-ui.md` と `docs/design/06-graph.md`。
//!
//! 1 行は `罫線  短縮 Oid  ラベル  summary`。罫線のセルを文字と色に写すのは
//! `ui::lanes`。

use mewgit_core::app::{AppState, GraphLine, GraphState, JobKind, Pane};
use mewgit_core::git_read::{CommitDetail, Oid, RefLabel, Signature, Timestamp};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::ui::{lanes, theme, time};

/// 短縮 Oid の桁数。`git log --oneline` と同じ。
const SHORT_OID: usize = 7;

/// 展開した詳細に割く行数（区切り線を含む）。
const DETAIL_ROWS: u16 = 10;

/// 詳細を出していてもグラフに残す行数。端末を縮めたときに詳細だけが残って
/// グラフが消えないようにする。
const MIN_GRAPH_ROWS: u16 = 3;

/// 詳細の領域を取るのに要る行数。区切り線 1 行 + 見出し 3 行。これを割るなら
/// 出さない。1 行だけ取ると区切り線で埋まり、グラフの行を 1 つ失って何も出ない。
const MIN_DETAIL_ROWS: u16 = 4;

/// ペインの枠。`focused` はフォーカスの強調だけに効き、内側の大きさは変えない。
fn block(focused: bool) -> Block<'static> {
    crate::ui::pane_block("Graph".to_string(), focused)
}

/// 詳細の領域を取るか。
///
/// 展開していても行が 1 つも無ければ出すものが無いので取らない。**判定はこの 1 つ。**
/// `ui::graph_rows` と [`render`] が別々に決めると、`app` に渡した行数と実際に描く
/// 行数がずれ、スクロール位置の計算が静かに狂う。
pub fn shows_detail(graph: &GraphState) -> bool {
    graph.expanded && !graph.lines.is_empty()
}

/// 枠の内側をコミットの行と詳細に割る。詳細を出さないときは 2 つ目が `None`。
///
/// `ui::graph_rows` が行数を出すのに使う。
pub fn areas(area: Rect, shows_detail: bool) -> (Rect, Option<Rect>) {
    split(block(false).inner(area), shows_detail)
}

/// 枠の内側を割る。`Layout` に任せず自分で計算するのは、レイアウトの解に幅を
/// 持たせないため。
fn split(inner: Rect, shows_detail: bool) -> (Rect, Option<Rect>) {
    let detail = if shows_detail {
        DETAIL_ROWS.min(inner.height.saturating_sub(MIN_GRAPH_ROWS))
    } else {
        0
    };
    if detail < MIN_DETAIL_ROWS {
        return (inner, None);
    }

    let rows = Rect {
        height: inner.height - detail,
        ..inner
    };
    let detail = Rect {
        y: inner.y + rows.height,
        height: detail,
        ..inner
    };
    (rows, Some(detail))
}

/// グラフペインを描く。
pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let block = block(state.focus == Pane::Graph);
    // `block()` はタイトルを確保するので 1 フレームに 1 回だけ作る。
    let (rows, detail) = split(block.inner(area), shows_detail(&state.graph));
    frame.render_widget(block, area);

    if state.graph.lines.is_empty() {
        // 行が無ければ詳細も無い（`shows_detail`）ので、`rows` が枠の内側そのまま。
        frame.render_widget(Paragraph::new(empty_message(state)), rows);
        return;
    }

    if let Some(detail) = detail {
        render_detail(frame, detail, state);
    }

    // 見えている行だけを切り出す。添字は `scroll` からの相対で戻せる。
    let start = state.graph.scroll.min(state.graph.lines.len());
    let end = (start + rows.height as usize).min(state.graph.lines.len());
    let visible = &state.graph.lines[start..end];

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

    let lines: Vec<Line> = visible
        .iter()
        .enumerate()
        .map(|(offset, line)| view.commit_line(start + offset, line))
        .collect();

    frame.render_widget(Paragraph::new(lines), rows);
}

/// 選択コミットの詳細。上辺の罫線でグラフと分ける。
fn render_detail(frame: &mut Frame, area: Rect, state: &AppState) {
    let block = Block::default().borders(Borders::TOP);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = match state.graph.fresh_detail() {
        Some(detail) => detail_lines(detail, inner.height as usize),
        // カーソルの指すコミットのものが無い。届いていないのか失敗したのかは
        // `JobTracker` が知っている（07 の「詳細の展開」）。
        None => vec![Line::raw(detail_placeholder(state))],
    };

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// 詳細がまだ無いときの文言。空のグラフの出し分けと同じ形。
fn detail_placeholder(state: &AppState) -> &'static str {
    if state.jobs.is_running(JobKind::LoadCommitDetail) {
        "読み込み中…"
    } else {
        "詳細を読み込めませんでした"
    }
}

/// 詳細の中身。`git show` の見出しに近い並びにする。
///
/// `rows` は描ける行数。メッセージはそこまでで打ち切る。1 行が折り返しで 2 行以上に
/// なることはあっても 0 行になることはないので、見える行を落とすことはない。マージや
/// revert の本文は数百行になることがあり、全部組むと毎フレームその数だけ `Line` を
/// 作ることになる。
fn detail_lines(detail: &CommitDetail, rows: usize) -> Vec<Line<'_>> {
    let mut lines = vec![
        Line::from(Span::styled(
            format!("commit {}", detail.oid),
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::raw(who("Author", &detail.author, detail.author_time)),
        Line::raw(who("Commit", &detail.committer, detail.committer_time)),
    ];

    if !detail.parents.is_empty() {
        let parents: Vec<String> = detail.parents.iter().map(short).collect();
        lines.push(Line::raw(format!("Parent: {}", parents.join(" "))));
    }

    lines.push(Line::raw(""));
    lines.extend(
        detail
            .message
            .lines()
            .take(rows.saturating_sub(lines.len()))
            .map(Line::raw),
    );
    lines
}

/// `Author: name <mail>  2026-09-06 15:35:55 +0900`。
fn who(label: &str, sig: &Signature, time: Timestamp) -> String {
    format!(
        "{label}: {} <{}>  {}",
        sig.name,
        sig.email,
        time::format(time)
    )
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

/// 行が 1 つも無いときに出す文言。
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
    /// 1 行分。`罫線  短縮 Oid  ラベル  summary`。
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
            short(&line.commit.oid),
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
fn short(oid: &Oid) -> String {
    let mut text = oid.to_string();
    text.truncate(SHORT_OID);
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{
        draw, graph_rows, state_at, state_with_commits, state_with_history, terminal_with,
    };
    use mewgit_core::app::{init, update, JobKind, Key, KeyCode, Msg, TermSize};
    use mewgit_core::git_read::{GitReadError, Oid, RefIndex, RefLabel};
    use mewgit_core::test_util::{fake_commit, fake_detail, fake_oid};
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
        assert_eq!(drawn, graph_rows(&state));
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
        let _ = init(&mut state);

        let lines = draw(&state, TermSize::default());

        assert!(lines[2].contains("読み込み中"), "{:?}", lines[2]);
    }

    #[test]
    fn a_failed_load_does_not_keep_saying_loading() {
        // 失敗の通知はステータスバーに出るが、次の通知で上書きされる。
        // ペインが「読み込み中…」のままだと、何も起きていないように見える。
        let mut state = state_at("/repo");
        let _ = init(&mut state);
        let _ = update(&mut state, Msg::LogLoaded(Err(GitReadError::NonUtf8Head)));

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

        assert_eq!(art(&lines, 2), "○─╮ ");
        assert_eq!(art(&lines, 3), "● │ ");
        assert_eq!(art(&lines, 4), "│ ● ");
        assert_eq!(art(&lines, 5), "●─╯ ");
    }

    #[test]
    fn a_second_parent_waiting_on_the_left_draws_a_line_back_to_it() {
        // `├`（`Through` + `Right`）を通す唯一のケース。`MERGE` では出ない。
        // 形は `graph::lane::tests` の
        // `a_second_parent_that_is_already_waiting_is_reached_by_a_line` と同じ。
        let state = state_with_history(&[(1, &[3]), (2, &[4, 3]), (4, &[3]), (3, &[])]);

        let lines = draw(&state, TermSize::default());

        assert_eq!(art(&lines, 2), "●   ");
        assert_eq!(art(&lines, 3), "├─○ ");
        assert_eq!(art(&lines, 4), "│ ● ");
        assert_eq!(art(&lines, 5), "●─╯ ");
    }

    #[test]
    fn ascii_replaces_the_box_drawing_characters() {
        let mut state = state_with_history(MERGE);
        state.config.graph.ascii = true;

        let lines = draw(&state, TermSize::default());

        assert_eq!(art(&lines, 2), "o-\\ ");
        assert_eq!(art(&lines, 3), "* | ");
        assert_eq!(art(&lines, 5), "*-/ ");
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

    /// 詳細を展開し、届いた状態にする。`update` を通すので `viewport` も本番と同じ。
    fn expanded_with(summaries: &[&str], detail: mewgit_core::git_read::CommitDetail) -> AppState {
        let mut state = state_with_commits(summaries);
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));
        // 展開でグラフが縮む。ランタイムなら `Msg::Layout` が入れる値（01）。
        state.graph.viewport = graph_rows(&state);
        let _ = update(
            &mut state,
            Msg::CommitDetailLoaded {
                oid: detail.oid,
                result: Box::new(Ok(detail)),
            },
        );
        state
    }

    #[test]
    fn expanding_takes_rows_away_from_the_graph() {
        let mut state = state_with_commits(&["first"]);
        let collapsed = graph_rows(&state);

        state.graph.expanded = true;
        let expanded = graph_rows(&state);

        assert!(
            expanded < collapsed,
            "詳細を出しても行数が減っていません: {collapsed} -> {expanded}"
        );
        assert_eq!(collapsed - expanded, DETAIL_ROWS as usize);
    }

    #[test]
    fn the_detail_shows_the_message_signatures_and_parents() {
        let mut detail = fake_detail(0, "subject\n\nbody line");
        detail.parents = vec![fake_oid(1)];
        let state = expanded_with(&["subject"], detail);

        // 80 桁だと右ペインのぶんグラフが 46 桁になり、見出しが折り返して
        // メッセージが詳細の高さから押し出される。中身を見るテストは広い端末で描く
        // （狭いレイアウトは M6）。
        let lines = draw(
            &state,
            TermSize {
                width: 120,
                height: 24,
            },
        );
        let text = lines.join("\n");

        assert!(text.contains("subject"), "{text}");
        assert!(text.contains("body line"), "{text}");
        assert!(text.contains("Author: mewgit test"), "{text}");
        assert!(text.contains("Commit: mewgit test"), "{text}");
        // 日時はコミットのタイムゾーンで出す（`fake_detail` は epoch の +0000）。
        assert!(text.contains("1970-01-01 00:00:00 +0000"), "{text}");
        assert!(
            text.contains(&format!(
                "Parent: {}",
                &fake_oid(1).to_string()[..SHORT_OID]
            )),
            "{text}"
        );
    }

    #[test]
    fn the_graph_rows_are_still_drawn_under_the_detail() {
        let state = expanded_with(&["first", "second"], fake_detail(0, "first"));

        let lines = draw(&state, TermSize::default());

        assert!(lines[2].contains("first"), "{:?}", lines[2]);
        assert!(lines[3].contains("second"), "{:?}", lines[3]);
    }

    #[test]
    fn a_detail_for_another_commit_says_it_is_loading() {
        // カーソルを動かした直後は、まだ前のコミットの詳細しか無い。
        let mut state = expanded_with(&["first", "second"], fake_detail(0, "first"));
        let _ = update(&mut state, Msg::Key(Key::char('j')));
        assert!(state.jobs.is_running(JobKind::LoadCommitDetail));

        let text = draw(&state, TermSize::default()).join("\n");

        assert!(text.contains("読み込み中"), "{text}");
        // 前のコミットの本文を出したままにしない。
        assert!(!text.contains("Author: mewgit test"), "{text}");
    }

    #[test]
    fn a_detail_that_failed_says_so_instead_of_loading() {
        let mut state = expanded_with(&["first", "second"], fake_detail(0, "first"));
        let _ = update(&mut state, Msg::Key(Key::char('j')));
        let oid = state.graph.cursor_commit().unwrap().oid;
        let _ = update(
            &mut state,
            Msg::CommitDetailLoaded {
                oid,
                result: Box::new(Err(GitReadError::NonUtf8Head)),
            },
        );

        let text = draw(&state, TermSize::default()).join("\n");

        assert!(text.contains("詳細を読み込めませんでした"), "{text}");
    }

    #[test]
    fn an_empty_graph_uses_the_whole_pane_even_when_expanded() {
        // 行が 1 つも無ければ詳細に出すものも無い。描く高さと `graph_rows` が
        // 返す行数は、この場合も一致していなければならない。
        let mut state = state_with_commits(&[]);
        state.graph.expanded = true;

        let lines = draw(&state, TermSize::default());

        assert!(lines[2].contains("コミットがありません"), "{:?}", lines[2]);
        assert!(!lines.join("\n").contains("読み込み中"));
        // 描く高さと `graph_rows` が返す行数は、この場合も一致していること。
        let expanded = graph_rows(&state);
        state.graph.expanded = false;
        assert_eq!(expanded, graph_rows(&state));
    }

    #[test]
    fn a_detail_that_would_be_only_a_border_is_not_opened() {
        // 1 行しか取れないと区切り線で埋まる。グラフの行を 1 つ失って何も出ない
        // という状態を作らない。
        let mut state = state_with_commits(&["first"]);

        for height in 3..24u16 {
            state.term = TermSize { width: 80, height };
            state.graph.expanded = false;
            let collapsed = graph_rows(&state);
            state.graph.expanded = true;
            let expanded = graph_rows(&state);

            let detail = collapsed - expanded;
            assert!(
                detail == 0 || detail >= MIN_DETAIL_ROWS as usize,
                "height {height}: 詳細に {detail} 行しか取れていません"
            );
        }
    }

    #[test]
    fn a_short_pane_keeps_the_graph_visible() {
        // 端末を縮めても、詳細だけが残ってグラフが消えることはない。詳細に割けるのは
        // グラフに `MIN_GRAPH_ROWS` 行を残せる範囲だけで、それも取れなければ出さない。
        let mut state = state_with_commits(&["first"]);

        for height in 3..16u16 {
            state.term = TermSize { width: 80, height };
            state.graph.expanded = false;
            let collapsed = graph_rows(&state);
            state.graph.expanded = true;
            let expanded = graph_rows(&state);

            assert!(
                expanded >= collapsed.min(MIN_GRAPH_ROWS as usize),
                "height {height}: 展開すると {collapsed} 行が {expanded} 行になります"
            );
        }
    }
}
