//! テスト用のヘルパ。`mewgit-tui` は 1 つのバイナリクレートなので、単体テストは
//! すべて同じコンパイル単位に入る。feature で切り分ける必要はない。

use mewgit_core::app::{update, AppState, Modal, Msg, RepoInfo, TermSize};
use mewgit_core::config::Config;
use mewgit_core::git_read::{Commit, LogPage};
use mewgit_core::test_util::{fake_commit, fake_commit_with_parents};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

use crate::ui;

/// 描画結果を行ごとの文字列にする。
///
/// `TestBackend` の `Display` を使う。自前でセルを走査すると全角文字の 2 セル目が
/// 空白として混ざる（「新しい通知」→「新 し い 通 知」）が、`Display` は幅を見て
/// 正しく詰めてくれる。スタイルを見たいときは [`terminal_with`] を使う。
///
/// `Display` は 1 行を `"` で囲むので外す。付いたままだと文字列上の桁が
/// バッファの x 座標と 1 つずれ、桁を見るテストが読みにくくなる。
pub fn draw(state: &AppState, size: TermSize) -> Vec<String> {
    terminal_with(state, size)
        .backend()
        .to_string()
        .lines()
        .map(|line| unquote(line).to_string())
        .collect()
}

/// 囲みの `"` と、そのうしろの注記を落とす。
///
/// 全角文字があると `TestBackend` は隠れたセルの一覧を閉じ `"` のあとに足す
/// （`"…" Hidden by multi-width symbols: [(1, " ")]`）。注記の中にも `"` が
/// 入るので、末尾から探さずに注記の開始で切る。
fn unquote(line: &str) -> &str {
    let body = line.strip_prefix('"').unwrap_or(line);
    match body.split_once(HIDDEN_CELLS) {
        Some((head, _)) => head,
        None => body.strip_suffix('"').unwrap_or(body),
    }
}

/// `TestBackend` が隠れたセルを報告するときの区切り。閉じ `"` を含む。
const HIDDEN_CELLS: &str = "\" Hidden by multi-width symbols:";

/// `needle` を含む行が何行目か。無ければ panic する。
///
/// 桁や行を直に書かない。レイアウトを変えたときにテストだけ古い座標を持つのを避ける
/// （[`graph_rows`] と同じ理由）。
pub fn row_of(lines: &[String], needle: &str) -> u16 {
    lines
        .iter()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle} を含む行がありません: {lines:?}")) as u16
}

/// 1 フレーム描いた `TestBackend` を返す。バッファのスタイルを見るとき用。
pub fn terminal_with(state: &AppState, size: TermSize) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(size.width, size.height)).unwrap();
    terminal.draw(|frame| ui::render(frame, state)).unwrap();
    terminal
}

/// `runtime::layout` がその状態に入れるのと同じグラフの行数。
///
/// ランタイムのループを回さないテストが `viewport` を用意するのに使う。判定を
/// 手で書くと、レイアウトの決め方が変わったときにテストだけ古い値を持つ。
pub fn graph_rows(state: &AppState) -> usize {
    ui::graph_rows(state.term, crate::panes::graph::shows_detail(&state.graph))
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

/// 与えた summary のコミットを新しい順に積んだ状態。親は持たない。
pub fn state_with_commits(summaries: &[&str]) -> AppState {
    let commits = summaries
        .iter()
        .enumerate()
        .map(|(index, summary)| fake_commit(index as u8, summary))
        .collect();
    state_with_log(commits)
}

/// モーダルを出した状態。下に何か描かれていること（覆いのテスト）が要るので、
/// コミットを 2 件積んである。
pub fn state_with_modal(modal: Modal) -> AppState {
    let mut state = state_with_commits(&["first", "second"]);
    state.modal = Some(modal);
    state
}

/// `(索引, 親の索引)` の並びからコミットを組んで積んだ状態。並びは新しい順。
///
/// レーンのある行を描くのに使う。oid は索引から作る（`fake_oid`）。
pub fn state_with_history(history: &[(u8, &[u8])]) -> AppState {
    let commits = history
        .iter()
        .map(|(index, parents)| fake_commit_with_parents(*index, parents, "summary"))
        .collect();
    state_with_log(commits)
}

/// コミット列を 1 ページとして積んだ状態。
///
/// 実リポジトリを作らない。描画のテストが `git` の挙動で落ちないようにするため。
/// `viewport` は 24 行の端末で描くときのグラフの行数に合わせる。
fn state_with_log(commits: Vec<Commit>) -> AppState {
    let mut state = state_at("/repo");
    state.graph.viewport = graph_rows(&state);

    let page = LogPage {
        commits,
        done: true,
    };
    let _ = update(
        &mut state,
        Msg::LogLoaded {
            restart: false,
            result: Ok(page),
        },
    );
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_with_wide_characters_comes_back_without_the_quotes() {
        // 全角があると `TestBackend` は閉じ `"` のうしろに隠れたセルの一覧を足す。
        // 末尾から `"` を探すと、その注記の中の `"` に当たって外れない。
        let state = state_with_commits(&["日本語のコミットメッセージ"]);

        let lines = draw(&state, TermSize::default());

        assert!(!lines[2].contains('"'), "{:?}", lines[2]);
        assert!(!lines[2].contains("Hidden by"), "{:?}", lines[2]);
        assert!(lines[2].contains("日本語のコミットメッセージ"));
    }
}
