//! 状態遷移。設計は `docs/design/01-app-core.md`。
//!
//! `update` は同期・純粋（`AppState` の変更を除く）。I/O は呼ばず、必要な副作用は
//! [`Cmd`] として返す。
//!
//! ペインごとのキー処理は、対応するペインのタスクと一緒に足す。いまあるのは
//! グラフだけ。

use crate::git_read::LogPage;
use crate::graph::GraphRow;

use super::{AppState, Cmd, GraphLine, JobKind, Key, KeyCode, Msg, NoticeLevel, Pane, TermSize};

/// 起動直後に実行してほしい副作用。
///
/// `runtime::run` がループに入る前に呼ぶ。`main.rs` から呼ぶとテストの外に出てしまう。
/// status / アカウント解決は、それぞれのタスクでここに足す。
pub fn init(state: &mut AppState) -> Cmd {
    let cmd = Cmd::LoadLog {
        count: state.config.graph.page_size,
    };
    state.issue(cmd)
}

/// 1 つの [`Msg`] を状態に適用し、実行してほしい副作用を返す。
///
/// 返す `Cmd` は [`AppState::issue`] を通す。同種のジョブが走っているあいだ二重に
/// 発行しないため（01 の「update の原則」）。
pub fn update(state: &mut AppState, msg: Msg) -> Cmd {
    let cmd = update_inner(state, msg);
    state.issue(cmd)
}

fn update_inner(state: &mut AppState, msg: Msg) -> Cmd {
    match msg {
        Msg::Key(key) => update_key(state, key),
        Msg::Resize(size) => {
            state.term = size;
            Cmd::None
        }
        Msg::Tick => Cmd::None,

        Msg::LogLoaded(result) => log_loaded(state, result),

        // 以下は対応するマイルストーンのタスクで実装する。ここで `_` を使わないのは、
        // `Msg` に変種を足したときに実装漏れをコンパイルエラーにするため。
        Msg::StatusLoaded(_) => Cmd::None,
        Msg::DiffLoaded { .. } => Cmd::None,
        Msg::ExecFinished(_) => Cmd::None,
        Msg::ProgressDetected(_) => Cmd::None,
        Msg::AccountResolved(_) => Cmd::None,
        Msg::PrLoaded(_) => Cmd::None,
        Msg::ChecksLoaded(_) => Cmd::None,
        Msg::PrListLoaded(_) => Cmd::None,
    }
}

/// キー入力を `modal` → `focus` の順に振り分ける。
fn update_key(state: &mut AppState, key: Key) -> Cmd {
    // Ctrl-C はモーダルより先に見る。raw mode では SIGINT が届かず、モーダル表示中は
    // `q` が入力文字になる（M2 の CommitMessage）ので、常に効く脱出口が要る。
    // M3 で実行中ジョブのキャンセルを入れたら、ジョブがあるときはそちらを優先する。
    if key == Key::ctrl('c') {
        return Cmd::Quit;
    }

    // モーダル表示中はすべてモーダルが受ける。個々のモーダルの処理は各マイルストーンで。
    if state.modal.is_some() {
        if key == Key::new(KeyCode::Esc) {
            state.modal = None;
        }
        return Cmd::None;
    }

    if key == Key::char('q') {
        return Cmd::Quit;
    }

    match state.focus {
        Pane::Graph => graph_key(state, key),
        // 各ペインのキー処理は対応するマイルストーンで足す。
        Pane::Branches | Pane::Stash | Pane::Prs | Pane::Worktree | Pane::Diff => Cmd::None,
    }
}

/// グラフペインのキー処理。
///
/// いまは上下移動だけ。`Enter` の詳細展開・`g` / `G`・スクロール追随の追加読み込みは
/// それぞれ別のタスクで足す。
fn graph_key(state: &mut AppState, key: Key) -> Cmd {
    let height = graph_height(state.term);
    match key.code() {
        KeyCode::Char('j') | KeyCode::Down => state.graph.move_cursor(1, height),
        KeyCode::Char('k') | KeyCode::Up => state.graph.move_cursor(-1, height),
        _ => {}
    }
    Cmd::None
}

/// グラフペインに使える行数。
///
/// 07 のレイアウト（ヘッダ 1 行・ステータスバー 1 行・枠の上下 2 行）から導く。
/// `ui` は `AppState` を書けないので、スクロール位置を決めるこの値は `app` が持つ。
/// **レイアウトを変えたらここも直す。** ずれると `ui::panes::graph` のテストが落ちる。
fn graph_height(term: TermSize) -> usize {
    (term.height as usize).saturating_sub(4)
}

/// ログのページを積む。
fn log_loaded(state: &mut AppState, result: Result<LogPage, crate::git_read::GitReadError>) -> Cmd {
    state.jobs.finish(JobKind::LoadLog);

    let page = match result {
        Ok(page) => page,
        Err(err) => {
            state.push_notice(NoticeLevel::Error, err.to_string(), None);
            // 失敗したセッションは捨てられ、次の読み込みは先頭から来る（02）。
            // 積んだ行を残すと同じコミットが二重に並ぶ。
            state.graph.lines.clear();
            state.graph.cursor = 0;
            state.graph.scroll = 0;
            state.graph.loaded_all = false;
            return Cmd::None;
        }
    };

    state
        .graph
        .lines
        .extend(page.commits.into_iter().map(|commit| GraphLine {
            row: GraphRow::unlaned(commit.oid),
            commit,
        }));
    state.graph.loaded_all = page.done;
    Cmd::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Modal, RepoInfo};
    use crate::config::Config;
    use crate::git_read::{Commit, GitReadError, Oid, Signature};

    fn state() -> AppState {
        AppState::new(RepoInfo::default(), Config::default())
    }

    /// `summary` だけが違うコミット。Oid は連番から作る。
    fn commit(n: u8) -> Commit {
        let mut bytes = [0u8; 20];
        bytes[19] = n;
        Commit {
            oid: Oid::from_bytes(&bytes).unwrap(),
            parents: Vec::new(),
            summary: format!("commit {n}"),
            author: Signature {
                name: "test".to_string(),
                email: "test@example.invalid".to_string(),
            },
            time: 0,
        }
    }

    /// `n` 件のコミットを積んだ状態。端末は既定（24 行 = グラフ 20 行）。
    fn state_with_commits(n: u8) -> AppState {
        let mut state = state();
        let page = LogPage {
            commits: (0..n).map(commit).collect(),
            done: true,
        };
        update(&mut state, Msg::LogLoaded(Ok(page)));
        state
    }

    #[test]
    fn init_asks_for_the_first_page_and_registers_the_job() {
        let mut state = state();

        let cmd = init(&mut state);

        assert_eq!(
            cmd,
            Cmd::LoadLog {
                count: state.config.graph.page_size
            }
        );
        assert!(state.jobs.is_running(JobKind::LoadLog));
    }

    #[test]
    fn a_second_load_while_one_is_running_is_dropped() {
        // 二重に投げると同じページを 2 回積む。
        let mut state = state();
        init(&mut state);

        let again = state.issue(Cmd::LoadLog { count: 10 });

        assert_eq!(again, Cmd::None);
    }

    #[test]
    fn batch_only_drops_the_duplicated_cmd() {
        let mut state = state();
        init(&mut state);

        let cmd = state.issue(Cmd::Batch(vec![
            Cmd::LoadLog { count: 10 },
            Cmd::LoadStatus,
        ]));

        // 走っているのは LoadLog だけなので、LoadStatus は残る。
        assert_eq!(cmd, Cmd::LoadStatus);
    }

    #[test]
    fn log_loaded_appends_lines_and_finishes_the_job() {
        let mut state = state();
        init(&mut state);

        let page = LogPage {
            commits: vec![commit(1), commit(2)],
            done: false,
        };
        assert_eq!(update(&mut state, Msg::LogLoaded(Ok(page))), Cmd::None);

        let summaries: Vec<&str> = state
            .graph
            .lines
            .iter()
            .map(|line| line.commit.summary.as_str())
            .collect();
        assert_eq!(summaries, ["commit 1", "commit 2"]);
        assert!(!state.graph.loaded_all);
        // ジョブを片付けないと、以後の LoadLog が黙って捨てられる。
        assert!(!state.jobs.is_running(JobKind::LoadLog));
    }

    #[test]
    fn a_second_page_is_appended_after_the_first() {
        let mut state = state();
        let first = LogPage {
            commits: vec![commit(1)],
            done: false,
        };
        update(&mut state, Msg::LogLoaded(Ok(first)));

        let second = LogPage {
            commits: vec![commit(2)],
            done: true,
        };
        update(&mut state, Msg::LogLoaded(Ok(second)));

        assert_eq!(state.graph.lines.len(), 2);
        assert!(state.graph.loaded_all);
    }

    #[test]
    fn a_failed_load_notices_and_drops_what_was_stacked() {
        // 失敗したセッションは捨てられ、次の読み込みは先頭から来る（02）。
        // 積んだ行を残すと同じコミットが二重に並ぶ。
        let mut state = state_with_commits(3);
        init(&mut state);

        update(&mut state, Msg::LogLoaded(Err(GitReadError::NonUtf8Head)));

        assert!(state.graph.lines.is_empty());
        assert_eq!(state.graph.cursor, 0);
        assert!(!state.graph.loaded_all);
        assert!(!state.jobs.is_running(JobKind::LoadLog));
        assert_eq!(state.notices.back().unwrap().level, NoticeLevel::Error);
    }

    #[test]
    fn j_and_k_move_the_graph_cursor() {
        let mut state = state_with_commits(3);

        update(&mut state, Msg::Key(Key::char('j')));
        assert_eq!(state.graph.cursor, 1);

        update(&mut state, Msg::Key(Key::new(KeyCode::Down)));
        assert_eq!(state.graph.cursor, 2);

        update(&mut state, Msg::Key(Key::char('k')));
        assert_eq!(state.graph.cursor, 1);

        update(&mut state, Msg::Key(Key::new(KeyCode::Up)));
        assert_eq!(state.graph.cursor, 0);
    }

    #[test]
    fn the_cursor_stops_at_both_ends() {
        let mut state = state_with_commits(2);

        update(&mut state, Msg::Key(Key::char('k')));
        assert_eq!(state.graph.cursor, 0);

        for _ in 0..5 {
            update(&mut state, Msg::Key(Key::char('j')));
        }
        assert_eq!(state.graph.cursor, 1);
    }

    #[test]
    fn moving_the_cursor_on_an_empty_graph_does_nothing() {
        let mut state = state();

        update(&mut state, Msg::Key(Key::char('j')));

        assert_eq!(state.graph.cursor, 0);
        assert_eq!(state.graph.scroll, 0);
    }

    #[test]
    fn the_view_follows_the_cursor_past_the_bottom() {
        // 端末 24 行 = グラフ 20 行。21 行目に降りるとスクロールが 1 進む。
        let mut state = state_with_commits(30);

        for _ in 0..20 {
            update(&mut state, Msg::Key(Key::char('j')));
        }

        assert_eq!(state.graph.cursor, 20);
        assert_eq!(state.graph.scroll, 1);
    }

    #[test]
    fn the_view_stays_put_while_the_cursor_is_visible() {
        // 下端から 1 つ戻ってもスクロールは動かない。動かすとカーソルが最下行に
        // 貼り付いたまま中身だけが流れる。
        let mut state = state_with_commits(30);
        for _ in 0..20 {
            update(&mut state, Msg::Key(Key::char('j')));
        }

        update(&mut state, Msg::Key(Key::char('k')));

        assert_eq!(state.graph.cursor, 19);
        assert_eq!(state.graph.scroll, 1);
    }

    #[test]
    fn q_quits() {
        let mut state = state();
        assert_eq!(update(&mut state, Msg::Key(Key::char('q'))), Cmd::Quit);
    }

    #[test]
    fn ctrl_q_does_not_quit() {
        let mut state = state();
        // 終了は修飾なしの `q` だけ。Ctrl-q で落ちると事故になる。
        assert_eq!(update(&mut state, Msg::Key(Key::ctrl('q'))), Cmd::None);
    }

    #[test]
    fn ctrl_c_quits() {
        let mut state = state();
        assert_eq!(update(&mut state, Msg::Key(Key::ctrl('c'))), Cmd::Quit);
    }

    #[test]
    fn ctrl_c_quits_even_with_a_modal_open() {
        // raw mode では SIGINT が届かず、モーダル中は `q` が入力文字になる。
        // 常に効く脱出口でないと閉じ込められる。
        let mut state = AppState {
            modal: Some(Modal::Help),
            ..state()
        };
        assert_eq!(update(&mut state, Msg::Key(Key::ctrl('c'))), Cmd::Quit);
    }

    #[test]
    fn unhandled_key_does_nothing() {
        let mut state = state();
        assert_eq!(update(&mut state, Msg::Key(Key::char('z'))), Cmd::None);
        assert_eq!(state.focus, Pane::Graph);
        assert!(state.modal.is_none());
    }

    #[test]
    fn esc_closes_the_modal() {
        let mut state = AppState {
            modal: Some(Modal::Help),
            ..state()
        };

        assert_eq!(
            update(&mut state, Msg::Key(Key::new(KeyCode::Esc))),
            Cmd::None
        );
        assert!(state.modal.is_none());
    }

    #[test]
    fn modal_swallows_other_keys() {
        let mut state = AppState {
            modal: Some(Modal::Help),
            ..state()
        };

        // モーダル表示中の `q` は終了ではない。入力中に閉じられると困る。
        assert_eq!(update(&mut state, Msg::Key(Key::char('q'))), Cmd::None);
        assert_eq!(state.modal, Some(Modal::Help));
    }

    #[test]
    fn resize_updates_the_terminal_size() {
        let mut state = state();
        let size = TermSize {
            width: 120,
            height: 40,
        };

        assert_eq!(update(&mut state, Msg::Resize(size)), Cmd::None);
        assert_eq!(state.term, size);
    }

    #[test]
    fn tick_does_nothing_yet() {
        let mut state = state();
        assert_eq!(update(&mut state, Msg::Tick), Cmd::None);
    }
}
