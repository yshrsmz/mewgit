//! Stash ペインのキー処理と一覧の受け取り（01 の「Stash ペイン」）。

use crate::git_read::{GitReadError, StashEntry};

use super::focus;
use crate::app::{AppState, Cmd, DiffSource, JobKind, Key, KeyCode, NoticeLevel, Pane};

/// Stash ペインのキー処理。
pub(super) fn stash_key(state: &mut AppState, key: Key) -> Cmd {
    match key.code() {
        KeyCode::Char('j') | KeyCode::Down => {
            state.stashes.move_cursor(1);
            Cmd::None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            state.stashes.move_cursor(-1);
            Cmd::None
        }
        KeyCode::Enter => open_stash_diff(state),
        // **`Tab` でしか入れないペインにも出口を用意する**（Branches と同じ。07）。
        KeyCode::Esc => focus(state, Pane::Graph),
        _ => Cmd::None,
    }
}

/// `Enter`。カーソルの指す stash の diff を左に出す（07 の「Diff ペイン」）。
///
/// **どの stash かは oid で憶える**（[`DiffSource`]）。`Esc` で戻ってくるのもここ。
fn open_stash_diff(state: &mut AppState) -> Cmd {
    // 一覧が空（読み込み中・失敗・stash が 1 つも無い）。カーソル行が描かれていないので、
    // 通知を出す相手もいない（Branches の `Enter` と同じ）。
    let Some(stash) = state.stashes.selected() else {
        return Cmd::None;
    };
    state.diff.source = DiffSource::Stash(stash.oid);
    focus(state, Pane::Diff)
}

/// stash の一覧が届いた。
///
/// **成功も失敗も `jobs.finish` を通る**（`branches_loaded` と同じ）。
pub(super) fn stashes_loaded(
    state: &mut AppState,
    result: Result<Vec<StashEntry>, GitReadError>,
) -> Cmd {
    let asked = state.jobs.finish(JobKind::LoadStashes);

    match result {
        Ok(entries) => state.stashes.loaded(entries),
        // 読めていた一覧は捨てない（`branches_loaded` と同じ理由）。
        Err(err) => {
            state.stashes.failed();
            state.push_notice(NoticeLevel::Error, err.to_string(), None);
        }
    }

    // 続きを頼む: 落とされたぶんの stash 一覧の追いつき。
    if state.jobs.is_fresh(JobKind::LoadStashes, asked) {
        Cmd::None
    } else {
        Cmd::LoadStashes
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        diff_ok, exec_done_of, fake_diff, fake_output, issued, press, press_code, state,
        state_on_stashes, tab_to, with_stashes, worktree_state,
    };
    use super::super::worktree::STASH_DIFF;
    use super::*;
    use crate::app::{update, ListLoad, Msg, Region};
    use crate::git_exec::{GitOp, PullMode};
    use crate::git_read::{DiffTarget, WorktreeSection};
    use crate::test_util::{fake_snapshot, fake_stash};

    #[test]
    fn stashes_loaded_fills_in_the_list_and_ends_the_job() {
        let mut state = state();

        let cmd = with_stashes(&mut state, Ok(vec![fake_stash(0), fake_stash(1)]));

        assert_eq!(cmd, Cmd::None);
        assert!(!state.jobs.is_running(JobKind::LoadStashes));
        assert_eq!(state.stashes.entries, [fake_stash(0), fake_stash(1)]);
        assert_eq!(state.stashes.load(), ListLoad::Loaded);
    }

    #[test]
    fn an_empty_list_is_loaded_not_pending() {
        // stash を 1 度もしていないリポジトリ。「読み込み中…」と同じ見た目にしない。
        let mut state = state();
        assert_eq!(state.stashes.load(), ListLoad::Pending);

        let _ = with_stashes(&mut state, Ok(vec![]));

        assert_eq!(state.stashes.load(), ListLoad::Loaded);
    }

    #[test]
    fn a_failed_load_keeps_the_list_it_had_and_says_so() {
        let mut state = state();
        let _ = with_stashes(&mut state, Ok(vec![fake_stash(0)]));

        let cmd = with_stashes(&mut state, Err(GitReadError::ReaderGone));

        // 同じものを頼み直さない（「失敗 → 再読込 → 失敗」を回さない）。
        assert_eq!(cmd, Cmd::None);
        assert!(!state.jobs.is_running(JobKind::LoadStashes));
        assert_eq!(state.stashes.entries, [fake_stash(0)]);
        assert_eq!(state.stashes.load(), ListLoad::Failed);
        assert!(state.shown_notice().is_some());
    }

    #[test]
    fn a_list_asked_for_before_a_refs_write_is_asked_for_again() {
        // `refs/stash` は ref なので `Region::Refs`（01 の「Stash ペイン」）。書き込みで
        // 世代が進んだあとに届いた返信は、書き込み前の一覧かもしれない。
        let mut state = state();
        let _ = state.issue(Cmd::LoadStashes);
        state.jobs.advance(Region::Refs);

        let cmd = update(
            &mut state,
            Msg::StashesLoaded(Box::new(Ok(vec![fake_stash(0)]))),
        );

        assert_eq!(cmd, Cmd::LoadStashes);
    }

    #[test]
    fn a_pull_reads_the_stashes_again_for_an_autostash() {
        // `merge.autoStash` を設定していると、`pull --ff-only` が exit 0 のまま
        // `stash@{0}: autostash` を積む（01 の「Stash ペイン」に実測）。stash の書き込み
        // ではないが、一覧を読み直さないと退避された変更が画面に出ない。
        let mut state = state_on_stashes(0);
        let pull = GitOp::Pull {
            mode: PullMode::FastForwardOnly,
        };
        let _ = state.issue(Cmd::Exec(pull.clone()));

        let cmd = update(&mut state, exec_done_of(pull, Ok(fake_output())));

        assert!(issued(&cmd, &Cmd::LoadStashes), "{cmd:?}");
        let _ = update(
            &mut state,
            Msg::StashesLoaded(Box::new(Ok(vec![fake_stash(0)]))),
        );
        assert_eq!(state.stashes.entries, [fake_stash(0)]);
    }

    #[test]
    fn the_cursor_stays_on_the_stash_it_was_on_when_one_is_pushed() {
        // push は先頭に 1 つ足すので、同じ index は 1 つ古い stash を指す。
        let mut state = state_on_stashes(3);
        let _ = press(&mut state, 'j');
        let selected = state.stashes.selected().unwrap().oid;

        let _ = with_stashes(
            &mut state,
            Ok(vec![
                fake_stash(9),
                fake_stash(0),
                fake_stash(1),
                fake_stash(2),
            ]),
        );

        assert_eq!(state.stashes.selected().unwrap().oid, selected);
        assert_eq!(state.stashes.cursor, 2);
    }

    #[test]
    fn the_cursor_stays_in_place_when_its_stash_is_gone() {
        // drop / pop で選んでいた stash が消えた。次の stash が詰めてくる位置に留まる。
        let mut state = state_on_stashes(3);
        let _ = press(&mut state, 'j');

        let _ = with_stashes(&mut state, Ok(vec![fake_stash(0), fake_stash(2)]));
        assert_eq!(state.stashes.cursor, 1);
        assert_eq!(state.stashes.selected(), Some(&fake_stash(2)));

        // 末尾が消えたら、新しい末尾へ。
        let _ = with_stashes(&mut state, Ok(vec![fake_stash(0)]));
        assert_eq!(state.stashes.cursor, 0);
    }

    #[test]
    fn j_and_k_move_the_cursor_and_stop_at_the_ends() {
        let mut state = state_on_stashes(2);

        let _ = press(&mut state, 'k');
        assert_eq!(state.stashes.cursor, 0);
        let _ = press(&mut state, 'j');
        let _ = press(&mut state, 'j');
        assert_eq!(state.stashes.cursor, 1);
    }

    #[test]
    fn esc_goes_back_to_the_graph() {
        let mut state = state_on_stashes(1);

        let _ = press_code(&mut state, KeyCode::Esc);

        assert_eq!(state.focus, Pane::Graph);
    }

    #[test]
    fn enter_opens_the_whole_stash_on_the_left_and_asks_for_it() {
        let mut state = state_on_stashes(2);
        let _ = press(&mut state, 'j');

        let cmd = press_code(&mut state, KeyCode::Enter);

        let wanted = DiffTarget::Stash(fake_stash(1).oid);
        // **ワーキングツリーに差分を出せる行が無くても開く**（`state_on_stashes` は WIP
        // 側の `Diff` が候補から外れている形）。外れたままだと `reseat_focus` が Graph へ
        // 戻す。
        assert_eq!(state.focus, Pane::Diff);
        assert_eq!(state.wanted_diff(), Some(wanted.clone()));
        assert_eq!(cmd, Cmd::LoadDiff(wanted));
    }

    #[test]
    fn enter_on_an_empty_list_does_nothing() {
        let mut state = state_on_stashes(0);

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.focus, Pane::Stash);
    }

    #[test]
    fn esc_from_a_stash_diff_comes_back_to_the_stash_pane() {
        let mut state = state_on_stashes(1);
        let _ = press_code(&mut state, KeyCode::Enter);

        let _ = press_code(&mut state, KeyCode::Esc);

        assert_eq!(state.focus, Pane::Stash);
        // 離れたので出どころは戻っている。
        assert_eq!(state.diff.source, DiffSource::Selection);
    }

    #[test]
    fn leaving_a_stash_diff_by_tab_forgets_where_it_came_from() {
        // 残ると、`Tab` で Diff に入り直したときに前に開いた stash が出る。
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        let _ = with_stashes(&mut state, Ok(vec![fake_stash(0)]));
        let _ = tab_to(&mut state, Pane::Stash);
        let _ = press_code(&mut state, KeyCode::Enter);
        assert_eq!(state.focus, Pane::Diff);

        let _ = press_code(&mut state, KeyCode::Tab);
        assert_ne!(state.focus, Pane::Diff);
        assert_eq!(state.diff.source, DiffSource::Selection);

        let _ = tab_to(&mut state, Pane::Diff);
        assert_eq!(
            state.wanted_diff(),
            Some(DiffTarget::IndexVsWorktree(Some("a.txt".into())))
        );
    }

    #[test]
    fn s_u_x_on_a_stash_diff_are_refused_and_touch_nothing() {
        // **WIP 行を選び、ワーキングツリーに stage できる行がある形で見る。** グラフの
        // 選択だけを見る守り（コミットの diff 用）はここを通してしまい、stash の
        // ハンクを見ながらワーキングツリーの `a.txt` を stage する。
        for key in ['s', 'u', 'x'] {
            let mut state = worktree_state(
                fake_snapshot(&["a.txt"], &["a.txt"], &[]),
                WorktreeSection::Unstaged,
            );
            let _ = with_stashes(&mut state, Ok(vec![fake_stash(0)]));
            let _ = tab_to(&mut state, Pane::Stash);
            let target = DiffTarget::Stash(fake_stash(0).oid);
            let _ = press_code(&mut state, KeyCode::Enter);
            let _ = update(&mut state, diff_ok(target, fake_diff("a.txt", 1)));

            let cmd = press(&mut state, key);

            assert_eq!(cmd, Cmd::None, "{key}");
            assert_eq!(state.modal, None, "{key}");
            assert_eq!(
                state.shown_notice().map(|notice| notice.message.as_str()),
                Some(STASH_DIFF),
                "{key}"
            );
        }
    }
}
