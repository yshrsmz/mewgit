//! グラフペインの描画。設計は `docs/design/07-ui.md` と `docs/design/06-graph.md`。
//!
//! レーンの罫線はまだ描かない（`GraphRow.cells` が空のあいだは何も出さない）。
//! `graph::lane` を実装したら、行の左に `cells` を写した列を足す。

use mewgit_core::app::AppState;
use mewgit_core::config::Theme;
use mewgit_core::git_read::RefLabel;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// 短縮 Oid の桁数。`git log --oneline` と同じ。
const SHORT_OID: usize = 7;

/// グラフペインを描く。
pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let block = Block::default().borders(Borders::ALL).title("Graph");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.graph.lines.is_empty() {
        let message = if state.graph.loaded_all {
            "コミットがありません"
        } else {
            "読み込み中…"
        };
        frame.render_widget(Paragraph::new(message), inner);
        return;
    }

    let height = inner.height as usize;
    let lines: Vec<Line> = state
        .graph
        .lines
        .iter()
        .enumerate()
        .skip(state.graph.scroll)
        .take(height)
        .map(|(index, line)| commit_line(state, index, line))
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

/// 1 行分。`短縮 Oid  ラベル  summary`。
fn commit_line<'a>(
    state: &'a AppState,
    index: usize,
    line: &'a mewgit_core::app::GraphLine,
) -> Line<'a> {
    let mut spans = vec![
        Span::styled(
            short_oid(&line.commit.oid),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(" "),
    ];

    for label in state.graph.refs.labels(line.commit.oid) {
        spans.push(Span::styled(
            decorate(label),
            label_style(&state.config.theme, label),
        ));
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
fn decorate(label: &RefLabel) -> String {
    match label {
        RefLabel::Local { name, .. } => format!("[{name}]"),
        RefLabel::Remote { name } => format!("{{{name}}}"),
        RefLabel::Tag { name } => format!("<{name}>"),
    }
}

/// ラベルの見た目。HEAD のブランチだけ強調する。
fn label_style(theme: &Theme, label: &RefLabel) -> Style {
    match label {
        RefLabel::Local { is_head: true, .. } => Style::default()
            .fg(color(&theme.graph_head))
            .add_modifier(Modifier::BOLD),
        _ => Style::default(),
    }
}

/// 設定の色名を ratatui の色にする。読めない名前は端末の既定色に落とす。
///
/// 設定は起動を止めない（08）ので、ここでもエラーにしない。
fn color(name: &str) -> Color {
    name.parse().unwrap_or(Color::Reset)
}

/// 先頭 [`SHORT_OID`] 桁。
fn short_oid(oid: &mewgit_core::git_read::Oid) -> String {
    let mut text = oid.to_string();
    text.truncate(SHORT_OID);
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime;
    use mewgit_core::app::{update, Msg, TermSize};
    use mewgit_core::config::Config;
    use mewgit_core::test_util::TestRepo;
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;
    use ratatui::Terminal;

    /// リポジトリを開いてログを 1 ページ積んだ状態。
    async fn state_with_log(repo: &TestRepo) -> AppState {
        let (mut state, reader) = runtime::open(repo.path(), Config::default())
            .await
            .expect("リポジトリを開けませんでした");
        let page = reader.log_page(100).await.expect("ログを読めませんでした");
        update(&mut state, Msg::LogLoaded(Ok(page)));
        state
    }

    /// 描画結果を行ごとの文字列にする。
    ///
    /// `TestBackend` の `Display` を使う。自前でセルを走査すると全角文字の 2 セル目が
    /// 空白として混ざる。
    fn draw(state: &AppState, size: TermSize) -> Vec<String> {
        terminal_with(state, size)
            .backend()
            .to_string()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn terminal_with(state: &AppState, size: TermSize) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(size.width, size.height)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, state))
            .unwrap();
        terminal
    }

    #[tokio::test]
    async fn each_commit_gets_a_line_with_its_short_oid_and_summary() {
        let repo = TestRepo::init();
        repo.commit("first commit");
        let oid = repo.commit("second commit");
        let state = state_with_log(&repo).await;

        let lines = draw(&state, TermSize::default());

        // 枠の中の 1 行目が最新のコミット。
        assert!(lines[2].contains("second commit"), "{:?}", lines[2]);
        assert!(lines[3].contains("first commit"), "{:?}", lines[3]);
        assert!(
            lines[2].contains(&oid.to_string()[..SHORT_OID]),
            "短縮 Oid が出ていません: {:?}",
            lines[2]
        );
    }

    #[tokio::test]
    async fn ref_labels_are_decorated_by_kind() {
        let repo = TestRepo::init();
        repo.commit("first");
        repo.git(&["tag", "v1.0"]);
        repo.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let state = state_with_log(&repo).await;

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

    #[tokio::test]
    async fn the_cursor_line_is_highlighted() {
        let repo = TestRepo::init();
        repo.commit("first");
        repo.commit("second");
        let mut state = state_with_log(&repo).await;
        state.graph.cursor = 1;

        let terminal = terminal_with(&state, TermSize::default());
        let buffer = terminal.backend().buffer();

        // 枠の中は y=2 から。カーソルは 2 行目（y=3）。
        assert!(!buffer[(1, 2)].modifier.contains(Modifier::REVERSED));
        assert!(buffer[(1, 3)].modifier.contains(Modifier::REVERSED));
    }

    #[tokio::test]
    async fn scrolling_moves_the_first_visible_line() {
        let repo = TestRepo::init();
        for i in 0..5 {
            repo.commit(&format!("commit {i}"));
        }
        let mut state = state_with_log(&repo).await;
        state.graph.scroll = 2;

        let lines = draw(&state, TermSize::default());

        // 新しい順に commit 4, 3, 2… なので 2 行飛ばすと commit 2 が先頭。
        assert!(lines[2].contains("commit 2"), "{:?}", lines[2]);
        assert!(!lines.iter().any(|l| l.contains("commit 4")));
    }

    #[tokio::test]
    async fn only_the_visible_rows_are_drawn() {
        // 端末 24 行 = ヘッダ 1 + 枠 2 + ステータス 1 を除いて 20 行。
        // `app` 側がスクロール位置を決めるのに使う値と一致していないと、
        // カーソルが画面の外に出る。
        let repo = TestRepo::init();
        for i in 0..30 {
            repo.commit(&format!("commit {i}"));
        }
        let state = state_with_log(&repo).await;

        let lines = draw(&state, TermSize::default());

        let drawn = lines.iter().filter(|line| line.contains("commit ")).count();
        assert_eq!(drawn, 20, "描いた行数が違います");
    }

    #[tokio::test]
    async fn an_empty_repository_says_so_instead_of_showing_nothing() {
        let repo = TestRepo::init();
        let state = state_with_log(&repo).await;

        let lines = draw(&state, TermSize::default());

        assert!(lines[2].contains("コミットがありません"), "{:?}", lines[2]);
    }

    #[test]
    fn before_the_first_page_arrives_it_says_loading() {
        let state = AppState::new(Default::default(), Config::default());

        let lines = draw(&state, TermSize::default());

        assert!(lines[2].contains("読み込み中"), "{:?}", lines[2]);
    }

    #[tokio::test]
    async fn a_wide_character_summary_is_not_split() {
        let repo = TestRepo::init();
        repo.commit("日本語のコミットメッセージ");
        let state = state_with_log(&repo).await;

        let lines = draw(&state, TermSize::default());

        assert!(
            lines[2].contains("日本語のコミットメッセージ"),
            "{:?}",
            lines[2]
        );
    }
}
