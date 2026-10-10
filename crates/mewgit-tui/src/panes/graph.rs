//! グラフペインの描画。設計は `docs/design/07-ui.md` と `docs/design/06-graph.md`。
//!
//! 1 行は `罫線  短縮 Oid  ラベル  summary`。先頭にワーキングツリーを表す WIP 行が
//! 1 つ入る。罫線のセルを文字と色に写すのは `ui::lanes`。
//!
//! **コミットの詳細はここに描かない。** 右ペインに出す（`panes::commit`）。M1 では
//! `Enter` でペインの下部に展開していたので、グラフの高さが `expanded` で変わり、
//! `ui::graph_rows` と `Msg::Layout` が「詳細を出すか」を運んでいた。差し替えではなく
//! 分割だったのが原因で、右へ移すとその引数ごと消えた（07 の「CommitDetail ペイン」）。

use mewgit_core::app::{AppState, GraphLine, GraphSelection, JobKind, Pane};
use mewgit_core::git_read::{short_oid, RefLabel, SHORT_OID};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::ui::{self, lanes, theme};

/// ペインの枠。`focused` はフォーカスの強調だけに効き、内側の大きさは変えない。
fn block(focused: bool) -> Block<'static> {
    ui::pane_block("Graph", focused)
}

/// 描くときの枠。**印があればタイトルに本数を出す**（`Graph (印 3)`。07）—— 画面外の
/// 印も数えるので、スクロールで印の行が見えなくなっても付いていることが判る。
/// 内側の大きさは [`block`] と変わらないので、[`area`] は印を見ない。
fn titled_block(state: &AppState) -> Block<'static> {
    let focused = state.focus == Pane::Graph;
    match state.graph.marks.len() {
        0 => block(focused),
        marks => ui::pane_block(format!("Graph (印 {marks})"), focused),
    }
}

/// 行を描ける領域。`ui::graph_rows` が行数を出すのに使う。
///
/// **枠の内側そのまま。** 高さは端末サイズだけで決まる（07 の「CommitDetail ペイン」）。
pub fn area(area: Rect) -> Rect {
    block(false).inner(area)
}

/// グラフペインを描く。
pub fn render(frame: &mut Frame, outer: Rect, state: &AppState) {
    let block = titled_block(state);
    // `block()` はタイトルを確保するので 1 フレームに 1 回だけ作る。
    let rows = block.inner(outer);
    frame.render_widget(block, outer);

    let graph = &state.graph;
    // 見えている表示行の範囲。**添字を `lines` に読み替えるのは `GraphState::row`**
    // で、ここには `- 1` を書かない（01 の「グラフの行」）。
    let start = graph.scroll.min(graph.rows());
    let end = (start + rows.height as usize).min(graph.rows());
    let shown = || (start..end).filter_map(|index| Some((index, graph.row(index)?)));

    let view = View {
        state,
        head: theme::style(&state.config.theme.graph_head),
        head_name: state.branches.head_branch().map(|b| b.name.as_str()),
        // 罫線の桁は、いま描く行の最大レーン数に揃える（07）。読み込み済みの
        // 全行で最大を取ると、複雑な履歴で桁が育ち続ける。
        lanes: shown()
            .filter_map(|(_, row)| match row {
                GraphSelection::Commit(line) => Some(line.row.cells.len()),
                GraphSelection::Wip => None,
            })
            .max()
            .unwrap_or(0),
        palette: lanes::palette(&state.config.graph.colors),
    };

    let mut lines: Vec<Line> = shown()
        .map(|(index, row)| view.row_line(index, row))
        .collect();
    // コミットが 1 つも無いときは、読み込み中なのか本当に空なのかを WIP 行の下に出す。
    // **これは表示行ではない**（選べないので `rows()` には数えない）。行が無いときは
    // `rows()` が 1 なので、ペインの高さには必ず収まる。
    if graph.lines.is_empty() {
        lines.push(Line::from(Span::styled(
            empty_message(state),
            theme::muted(),
        )));
    }

    frame.render_widget(Paragraph::new(lines), rows);
}

/// 1 フレームのあいだ変わらない描画条件。行ごとに引き回す値をまとめる。
struct View<'a> {
    state: &'a AppState,
    /// HEAD のブランチラベルのスタイル。
    head: Style,
    /// HEAD のブランチ名。detached / unborn / 一覧がまだ無いときは `None`。
    /// **ブランチ一覧の HEAD の行から引く** —— Branches ペインの `*` と同じ値なので、
    /// 強調する名前は食い違わない。ラベルの集合は `refs()` の別の列挙から来るので、
    /// その名前のラベルがまだ無いことはある（02 の「refs とラベル」）。
    head_name: Option<&'a str>,
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
        ui::LOADING
    } else if state.graph.load_failed {
        "コミットを読み込めませんでした"
    } else {
        "コミットがありません"
    }
}

impl<'a> View<'a> {
    /// 表示行 1 本。`index` は表示行の添字。
    ///
    /// **カーソル行の反転はここでまとめて掛ける。** 行の種類ごとに書くと、種類を
    /// 足したときに反転だけ落ちる（選択行は worktree ペインと同じ `theme::selected`）。
    fn row_line(&self, index: usize, row: GraphSelection<'a>) -> Line<'a> {
        let line = match row {
            GraphSelection::Wip => self.wip_line(),
            GraphSelection::Commit(line) => self.commit_line(line),
        };
        if index == self.state.graph.cursor {
            line.style(theme::selected())
        } else {
            line
        }
    }

    /// 先頭の WIP 行。`◇  WIP (n)`。
    ///
    /// **罫線を繋がない**（06 の「責務」）。`◇` を罫線の桁の先頭に置き、残りは空白で
    /// 埋める。埋める幅に短縮 Oid のぶんも含めるのは、`WIP (n)` を下の行の summary と
    /// 同じ桁に揃えるため。
    fn wip_line(&self) -> Line<'static> {
        let glyph = lanes::wip_glyph(self.state.config.graph.ascii);
        let pad = lanes::width(self.lanes) + SHORT_OID + 1 - glyph.chars().count();

        Line::from(vec![
            Span::raw(glyph),
            // 毎フレーム組み直さない。詰めるのは罫線 + Oid の桁ぶんだけなので、
            // 定数の空白から切り出せば足りる。
            Span::raw(&SPACES[..pad.min(SPACES.len())]),
            Span::raw(format!("WIP ({})", self.state.worktree.len())),
        ])
    }

    /// 1 行分。`罫線  短縮 Oid  ラベル  summary`。
    fn commit_line(&self, line: &'a GraphLine) -> Line<'a> {
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

        // **印は oid の見た目だけで出し、桁を足さない**（07）—— 印の桁を足すと、印の
        // 有無で summary の桁がずれる。
        let oid_style = if state.graph.is_marked(line.commit.oid) {
            theme::marked()
        } else {
            theme::muted()
        };
        spans.push(Span::styled(short_oid(&line.commit.oid), oid_style));
        spans.push(Span::raw(" "));

        for label in labels {
            // 飾りは組み立てずに 3 つのスパンに分ける。毎フレーム全行ぶん `format!` する
            // 必要が無く、HEAD の強調も括弧まで掛かる。
            let style = self.label_style(label);
            let (open, close) = brackets(label);
            spans.push(Span::styled(open, style));
            spans.push(Span::styled(label.name(), style));
            spans.push(Span::styled(close, style));
            spans.push(Span::raw(" "));
        }

        spans.push(Span::raw(line.commit.summary.as_str()));

        Line::from(spans)
    }

    /// ラベルの見た目。HEAD のブランチだけ強調する。
    fn label_style(&self, label: &RefLabel) -> Style {
        match label {
            RefLabel::Local { name } if Some(name.as_str()) == self.head_name => self.head,
            _ => Style::default(),
        }
    }
}

/// WIP 行の桁を詰める空白。詰めるのは罫線 + 短縮 Oid の桁ぶん（`2 * lanes + 8`）なので、
/// レーンが 30 本を超えると足りなくなる。そのときは WIP の見出しが summary の桁より
/// 左に寄るだけで、切れはしない（`min` で止める）。
const SPACES: &str = "                                                                    ";

/// 種別ごとの飾り（06 の「ラベル」）。
fn brackets(label: &RefLabel) -> (&'static str, &'static str) {
    match label {
        RefLabel::Local { .. } => ("[", "]"),
        RefLabel::Remote { .. } => ("{", "}"),
        RefLabel::Tag { .. } => ("<", ">"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{
        col_of, draw, graph_rows, state_at, state_with_commits, state_with_history, terminal_with,
    };
    use mewgit_core::app::{init, update, Msg, TermSize};
    use mewgit_core::git_read::{GitReadError, RefIndex, RefLabel};
    use mewgit_core::test_util::{deliver_branches, fake_branch, fake_branch_list};
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
        assert!(
            lines[FIRST_ROW + 1].contains("oldest"),
            "{:?}",
            lines[FIRST_ROW + 1]
        );
        let oid = state.graph.lines[0].commit.oid.to_string();
        assert!(
            lines[FIRST_ROW].contains(&format!("{} newest", &oid[..SHORT_OID])),
            "短縮 Oid が summary の前にありません: {:?}",
            lines[FIRST_ROW]
        );
        assert!(
            !lines[FIRST_ROW].contains(&oid[..SHORT_OID + 1]),
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
            + SHORT_OID
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

    /// 同じコミットに `a-first` と `main` のラベルを載せ、ブランチ一覧を `head` で届けた状態。
    ///
    /// **HEAD でない `a-first` を一覧の先頭（= 選択中の行）とラベルの先頭に置く**
    /// （`.claude/rules/testing.md` の「読み元を移すリファクタの見張り」）—— HEAD の名前を
    /// 一覧の先頭や選択中の行から取る形、最初の Local ラベルを強調する形を落とす。
    fn state_with_two_local_labels(head: Option<&str>) -> AppState {
        let mut state = state_with_commits(&["first"]);
        let oid = state.graph.lines[0].commit.oid;
        state.graph.refs = RefIndex::from_iter(["main", "a-first"].map(|name| {
            (
                oid,
                RefLabel::Local {
                    name: name.to_string(),
                },
            )
        }));
        let branches = fake_branch_list(vec![fake_branch("a-first"), fake_branch("main")], head);
        let _ = deliver_branches(&mut state, Ok(branches));
        // 読み込みはカーソルを HEAD の行へ座らせるので、先頭へ戻して HEAD でない行を選ぶ。
        state.branches.cursor = 0;
        state
    }

    #[test]
    fn the_head_branch_label_is_emphasised() {
        // 既定の `theme.graph_head` は "bold"。色として読むと強調がまるごと消える。
        // **HEAD はブランチ一覧の HEAD の行から引く**（02 の「refs とラベル」）。
        let state = state_with_two_local_labels(Some("main"));

        let line = &draw(&state, TermSize::default())[FIRST_ROW];
        let terminal = terminal_with(&state, TermSize::default());
        let buffer = terminal.backend().buffer();

        let y = FIRST_ROW as u16;
        let head = col_of(line, "[main]");
        let other = col_of(line, "[a-first]");
        assert!(other < head, "並びが Local の名前順ではありません");
        assert!(buffer[(head, y)].modifier.contains(Modifier::BOLD));
        assert!(!buffer[(other, y)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn no_label_is_emphasised_when_head_is_detached() {
        // 否定だけだと「強調を一切しない」実装でも通るので、同じラベルで HEAD を
        // `main` に置いた側が強調に変わるところまで 1 本で見る。
        let bold = |head: Option<&str>| {
            let state = state_with_two_local_labels(head);
            let line = &draw(&state, TermSize::default())[FIRST_ROW];
            let terminal = terminal_with(&state, TermSize::default());
            let buffer = terminal.backend().buffer();
            let y = FIRST_ROW as u16;
            ["[main]", "[a-first]"].map(|label| {
                buffer[(col_of(line, label), y)]
                    .modifier
                    .contains(Modifier::BOLD)
            })
        };

        assert_eq!(bold(None), [false, false]);
        assert_eq!(bold(Some("main")), [true, false]);
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
    fn a_marked_commit_underlines_its_oid_without_moving_the_summary() {
        // 07 の「Graph ペイン」—— 印は oid の見た目だけで出し、桁は足さない。
        let mut state = state_with_commits(&["first", "second"]);
        let marked = state.graph.lines[0].commit.oid;
        state.graph.toggle_mark(marked);

        let terminal = terminal_with(&state, TermSize::default());
        let buffer = terminal.backend().buffer();

        let oid = |row: usize| {
            let text = state.graph.lines[row - FIRST_ROW].commit.oid.to_string();
            let y = row as u16;
            let x = column_of(buffer, y, &text[..1]).expect("oid が描かれていません");
            &buffer[(x, y)]
        };
        assert!(oid(FIRST_ROW).modifier.contains(Modifier::UNDERLINED));
        assert_eq!(oid(FIRST_ROW).fg, theme::marked().fg.unwrap());
        assert!(!oid(FIRST_ROW + 1).modifier.contains(Modifier::UNDERLINED));
        let lines = draw(&state, TermSize::default());
        assert_eq!(
            lines[FIRST_ROW].find("first"),
            lines[FIRST_ROW + 1].find("second"),
            "印の有無で summary の桁がずれています"
        );
    }

    #[test]
    fn the_title_counts_the_marks() {
        let mut state = state_with_commits(&["first", "second"]);
        let title = |state: &AppState| draw(state, TermSize::default())[WIP_ROW - 1].clone();
        assert!(!title(&state).contains("印"), "{}", title(&state));

        for line in 0..2 {
            let oid = state.graph.lines[line].commit.oid;
            state.graph.toggle_mark(oid);
        }

        assert!(title(&state).contains("Graph (印 2)"), "{}", title(&state));
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
}
