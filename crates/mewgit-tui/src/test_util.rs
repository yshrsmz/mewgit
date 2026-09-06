//! テスト用のヘルパ。`mewgit-tui` は 1 つのバイナリクレートなので、単体テストは
//! すべて同じコンパイル単位に入る。feature で切り分ける必要はない。

use mewgit_core::app::{update, AppState, Msg, RepoInfo, TermSize};
use mewgit_core::config::Config;
use mewgit_core::git_read::LogPage;
use mewgit_core::test_util::fake_commit;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

use crate::ui;

/// 描画結果を行ごとの文字列にする。
///
/// `TestBackend` の `Display` を使う。自前でセルを走査すると全角文字の 2 セル目が
/// 空白として混ざる（「新しい通知」→「新 し い 通 知」）が、`Display` は幅を見て
/// 正しく詰めてくれる。スタイルを見たいときは [`terminal_with`] を使う。
pub fn draw(state: &AppState, size: TermSize) -> Vec<String> {
    terminal_with(state, size)
        .backend()
        .to_string()
        .lines()
        .map(str::to_string)
        .collect()
}

/// 1 フレーム描いた `TestBackend` を返す。バッファのスタイルを見るとき用。
pub fn terminal_with(state: &AppState, size: TermSize) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(size.width, size.height)).unwrap();
    terminal.draw(|frame| ui::render(frame, state)).unwrap();
    terminal
}

/// パスだけ入った状態。
pub fn state_at(path: &str) -> AppState {
    AppState::new(
        RepoInfo {
            path: path.into(),
            ..RepoInfo::default()
        },
        Config::default(),
    )
}

/// 与えた summary のコミットを新しい順に積んだ状態。
///
/// 実リポジトリを作らない。描画のテストが `git` の挙動で落ちないようにするため。
/// `viewport` は 24 行の端末で描くときのグラフの行数に合わせる。
pub fn state_with_commits(summaries: &[&str]) -> AppState {
    let mut state = state_at("/repo");
    state.graph.viewport = ui::graph_rows(TermSize::default());

    let page = LogPage {
        commits: summaries
            .iter()
            .enumerate()
            .map(|(index, summary)| fake_commit(index as u8, summary))
            .collect(),
        done: true,
    };
    update(&mut state, Msg::LogLoaded(Ok(page)));
    state
}
