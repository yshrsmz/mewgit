//! Diff ペインのキー処理と、diff の遅延取得（01 の「diff の遅延取得」）。
//!
//! **ハンクに効く `s` / `u` / `x` の本体は [`super::worktree`] にある** ——
//! 区画と変更種別から `GitOp` を選ぶ表がファイル単位と共通だから（[`super::worktree::plan`]）。

use crate::git_read::{DiffTarget, FileDiff, GitReadError};

use super::focus;
use super::worktree::{act, Action};
use crate::app::{AppState, Cmd, DiffCursor, DiffView, JobKind, Key, KeyCode, NoticeLevel};

/// 見せている diff が要るなら頼む。
///
/// **同じ対象で一度失敗しているなら出さない。** 出すと「失敗 → 再読込 → 失敗」を回す。
/// 対象が違えば一度も試していないので頼む（01）。
///
/// **同じ対象を既に持っていて、それが直前の書き込みより後のものなら出さない**
/// （01 の「diff のキャッシュと鮮度」）。ワーキングツリーの diff は
/// 1 ファイルぶんでもワークツリー全体を走査してから絞るので、`Esc` / `Enter` の
/// たびに走らせるとリーダースレッドが `LoadLog` を待たせる。
pub(super) fn load_diff(state: &mut AppState) -> Cmd {
    let Some(wanted) = state.wanted_diff() else {
        return Cmd::None;
    };
    if state.diff.failed.as_ref() == Some(&wanted) {
        return Cmd::None;
    }
    // 書き込みより後の diff を既に持っているなら読み直さない。
    //
    // **`LoadDiff` が走っているかは見ない。** 走っていれば `issue` が落とし、いま
    // 飛んでいる要求は書き込みより前のものなので、その返信は `diff_loaded` で
    // 捨てられて同じここから頼み直される（01 の「diff のキャッシュと鮮度」）。
    if state.diff_is_current() {
        return Cmd::None;
    }

    Cmd::LoadDiff(wanted)
}

/// Diff ペインのキー処理。
///
/// カーソルはハンク単位で、`s` / `u` / `x` はカーソルの指すハンクに効く（07）。
/// 可否の表は Worktree ペインと共通（[`plan`]）。
pub(super) fn diff_key(state: &mut AppState, key: Key) -> Cmd {
    match key.code() {
        KeyCode::Char('j') | KeyCode::Down => move_hunk(state, 1),
        KeyCode::Char('k') | KeyCode::Up => move_hunk(state, -1),
        KeyCode::Char('s') => act(state, Action::Stage),
        KeyCode::Char('u') => act(state, Action::Unstage),
        KeyCode::Char('x') => act(state, Action::Discard),
        // **戻り先は `Enter` の行き先と同じ 1 つの関数から出す**
        // （`GraphSelection::right_pane`）。WIP 行なら Working tree、コミットなら
        // コミット詳細。別々に書くと、来た側と違うペインに戻る腕ができる。
        KeyCode::Esc => focus(state, state.graph.selection().right_pane()),
        _ => Cmd::None,
    }
}

/// diff のカーソルを `delta` ハンク動かす。
///
/// **いま見えている diff だけを動かす。** 別の対象のものが `view` に残っていることが
/// あり（読み込み中・失敗中）、濾さずに動かすと画面に出ていない diff のカーソルが
/// 黙って動く。
fn move_hunk(state: &mut AppState, delta: isize) -> Cmd {
    let wanted = state.wanted_diff();
    if let Some(view) = state.diff.fresh_view_mut(wanted.as_ref()) {
        view.move_cursor(delta);
    }
    Cmd::None
}

/// 届いた diff を積む。
///
/// `JobTracker` の粒度は種別単位なので、要求が飛んでいるあいだの選択の移動は
/// 落とされる。ここで追いつく（`commit_detail_loaded` と同じ形）。
pub(super) fn diff_loaded(
    state: &mut AppState,
    target: DiffTarget,
    result: Result<Vec<FileDiff>, GitReadError>,
) -> Cmd {
    // **この返信が書き込みより後に頼んだものか。** 見るのは届いた返信の刻印であって、
    // 持っている `view` の鮮度ではない（01 の「実行中ジョブ › 世代」）。
    let asked = state.jobs.finish(JobKind::LoadDiff);
    let fresh = state.jobs.is_fresh(JobKind::LoadDiff, asked);

    match result {
        Ok(files) => {
            state.diff.failed = None;
            // **採るのは「いま選んでいるファイルのもの」かつ「書き込みより後のもの」
            // だけ。** 対象を見ないと正しく出ている画面が別のファイルの diff に化け、
            // 鮮度を見ないと書き込み前の中身に戻る。
            //
            // **刻印は届いた返信のものをそのまま持ち越す。** `Region::Worktree` から
            // 取り直すと、刻印の出どころが読む側（`diff_is_current` は
            // `JobKind::LoadDiff` から領域を引く）と 2 本になる。
            let wanted = state.wanted_diff();
            let read_at = asked.filter(|_| fresh && wanted.as_ref() == Some(&target));
            if let Some(read_at) = read_at {
                // **同じ対象を読み直したときはカーソルを持ち越す。** 書き込みのあとは
                // 同じファイルの diff がもう一度届く。既定に戻すと、選んでいたハンクが
                // 先頭へ飛んでから操作が効くことになる。
                let cursor = state
                    .diff
                    .view
                    .as_ref()
                    .filter(|view| view.target == target)
                    .map_or_else(DiffCursor::default, |view| view.cursor);
                let mut view = DiffView {
                    target,
                    files,
                    cursor,
                    epoch: read_at,
                };
                // 読み直しでハンクが減っていることがある。選べる位置へ寄せ直す。
                view.move_cursor(0);
                state.diff.view = Some(view);
            }
        }
        Err(err) => {
            state.push_notice(NoticeLevel::Error, err.to_string(), None);
            state.diff.failed = Some(target);
        }
    }

    // **採ったときもここを通る。** 対象が違っていた（選択が動いた / 別の対象が
    // 失敗した）ときは追いつきの再発行、まだ書き込みより前の diff しか無ければ
    // 読み直し。経路を 1 本にするため出口は 1 つ（01）。
    load_diff(state)
}

#[cfg(test)]
mod tests {
    use super::super::graph::NO_DIFF;
    use super::super::test_support::{
        diff_ok, exec_done, fake_diff, fake_output, only_staged, press, press_code, worktree_state,
    };
    use super::super::update;
    use super::super::worktree::NOT_LATEST;
    use super::*;
    use crate::app::{Job, Modal, Msg, Pane};

    use crate::git_exec::{hunk_patch, GitOp};
    use crate::git_read::{FileStatus, Hunk, LineOrigin, WorktreeSection, WorktreeSnapshot};

    use crate::test_util::{fake_entry, fake_file_diff, fake_hunk, fake_snapshot, reply};

    /// ワーキングツリー側の diff の対象。`assert_eq!` を短くするため。
    fn unstaged_diff(path: &str) -> DiffTarget {
        DiffTarget::IndexVsWorktree(Some(path.into()))
    }

    fn diff_err(target: DiffTarget) -> Msg {
        Msg::DiffLoaded {
            target,
            result: Box::new(Err(GitReadError::NonUtf8Head)),
        }
    }

    /// Diff ペインを開き、`paths[0]` の diff が `hunks` 個のハンクで届いた状態。
    fn diff_state(paths: &[&str], hunks: usize) -> AppState {
        diff_pane(WorktreeSection::Unstaged, paths, fake_diff(paths[0], hunks))
    }

    /// Diff ペインを開き、`files` が届いた状態。
    ///
    /// **Diff ペインの立ち上げはここ 1 本。** 「worktree の状態を作る → `Enter` →
    /// `diff_ok`」を 2 箇所に書くと、片方だけ直したときに差が出る。
    fn diff_pane(section: WorktreeSection, paths: &[&str], files: Vec<FileDiff>) -> AppState {
        let snapshot = match section {
            WorktreeSection::Staged => fake_snapshot(paths, &[], &[]),
            WorktreeSection::Unstaged => fake_snapshot(&[], paths, &[]),
            WorktreeSection::Untracked => fake_snapshot(&[], &[], paths),
            WorktreeSection::Conflicted => panic!("conflicted は diff を出さない"),
        };
        let mut state = worktree_state(snapshot, section);
        let _ = press_code(&mut state, KeyCode::Enter);
        let target = state.wanted_diff().expect("対象が決まっていません");
        let _ = update(&mut state, diff_ok(target, files));
        state
    }

    #[test]
    fn enter_opens_the_diff_pane_and_asks_for_the_selected_file() {
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt", "b.txt"], &[]),
            WorktreeSection::Unstaged,
        );

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(state.focus, Pane::Diff);
        assert_eq!(cmd, Cmd::LoadDiff(unstaged_diff("a.txt")));
    }

    #[test]
    fn a_staged_row_is_diffed_against_head() {
        // 同じファイルでも区画で対象が変わる。パスだけでは決まらない。
        let mut state = only_staged();

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(
            cmd,
            Cmd::LoadDiff(DiffTarget::HeadVsIndex(Some("a.txt".into())))
        );
    }

    #[test]
    fn a_conflicted_row_refuses_to_open_the_diff() {
        // 解消ビューは MVP 外。2 者の diff では表せない（02）ので対象が決まらない。
        // **入れてしまうと、左のグラフが空の Diff に差し替わる**（01 の
        // 「Pane と Modal」）。`Tab` も同じ判定で候補から外す。
        let snapshot = WorktreeSnapshot {
            conflicted: vec![fake_entry("c.txt", FileStatus::Conflicted)],
            ..WorktreeSnapshot::default()
        };
        let mut state = worktree_state(snapshot, WorktreeSection::Conflicted);

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.focus, Pane::Worktree);
        assert!(!state.focus_candidates().contains(&Pane::Diff));
        // 黙って何も起きないと、キーが効いていないのか対象が悪いのか判らない。
        assert_eq!(
            state.notices.back().map(|notice| notice.message.as_str()),
            Some(NO_DIFF)
        );
    }

    #[test]
    fn tab_into_the_diff_pane_asks_for_it_too() {
        // `Tab` の腕に `LoadDiff` を書かず、フォーカスの移動が 1 箇所を通ることの確認。
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );

        let cmd = press_code(&mut state, KeyCode::Tab);

        assert_eq!(state.focus, Pane::Diff);
        assert_eq!(cmd, Cmd::LoadDiff(unstaged_diff("a.txt")));
    }

    #[test]
    fn esc_goes_back_to_the_worktree_without_asking_for_anything() {
        let mut state = diff_state(&["a.txt"], 1);

        let cmd = press_code(&mut state, KeyCode::Esc);

        assert_eq!(state.focus, Pane::Worktree);
        // 見せていないものは読まない。`wanted_diff` が `None` になる。
        assert_eq!(cmd, Cmd::None);
    }

    #[test]
    fn a_diff_for_another_file_is_dropped_and_the_right_one_is_asked_again() {
        // `JobKind` の粒度は種別単位なので、要求が飛んでいるあいだの選択の移動は
        // 落とされる。届いた側で追いつく。
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        let _ = press_code(&mut state, KeyCode::Enter);

        let cmd = update(
            &mut state,
            diff_ok(unstaged_diff("b.txt"), fake_diff("b.txt", 1)),
        );

        assert_eq!(state.diff.view, None);
        assert_eq!(cmd, Cmd::LoadDiff(unstaged_diff("a.txt")));
    }

    #[test]
    fn a_failed_diff_is_not_asked_again_for_the_same_file() {
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        let _ = press_code(&mut state, KeyCode::Enter);

        let cmd = update(&mut state, diff_err(unstaged_diff("a.txt")));

        // 出すと「失敗 → 再読込 → 失敗」を回す。
        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.notices.len(), 1);
        // 出入りしても同じ。
        let _ = press_code(&mut state, KeyCode::Esc);
        assert_eq!(press_code(&mut state, KeyCode::Enter), Cmd::None);
    }

    #[test]
    fn moving_the_cursor_lets_a_failed_file_be_tried_again() {
        // 一律に止めると、失敗した対象が二度と読めなくなる。`GraphState.load_failed` を
        // カーソル移動で落とすのと同じ形。
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt", "b.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        let _ = press_code(&mut state, KeyCode::Enter);
        let _ = update(&mut state, diff_err(unstaged_diff("a.txt")));
        let _ = press_code(&mut state, KeyCode::Esc);

        // 動かして戻る。対象は同じ a.txt。
        let _ = press(&mut state, 'j');
        let _ = press(&mut state, 'k');

        assert_eq!(
            press_code(&mut state, KeyCode::Enter),
            Cmd::LoadDiff(unstaged_diff("a.txt"))
        );
    }

    #[test]
    fn a_failure_for_another_file_does_not_block_the_current_one() {
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        let _ = press_code(&mut state, KeyCode::Enter);

        let cmd = update(&mut state, diff_err(unstaged_diff("b.txt")));

        // a.txt はまだ一度も試していない。塞ぐと「読み込めませんでした」のまま残る。
        assert_eq!(cmd, Cmd::LoadDiff(unstaged_diff("a.txt")));
    }

    #[test]
    fn a_write_that_moves_the_file_to_another_section_reloads_the_diff() {
        // stage すると対象が `IndexVsWorktree` から `HeadVsIndex` に変わる。
        // 判定は `clamp` のあとでないと、古い区画で対象を決めてしまう。
        let mut state = diff_state(&["a.txt"], 1);

        let cmd = reply(
            &mut state,
            Cmd::LoadStatus,
            Msg::StatusLoaded(Box::new(Ok(fake_snapshot(&["a.txt"], &[], &[])))),
        );

        assert_eq!(
            cmd,
            Cmd::LoadDiff(DiffTarget::HeadVsIndex(Some("a.txt".into())))
        );
    }

    #[test]
    fn a_reload_of_the_same_file_keeps_the_hunk_cursor() {
        // 同じ対象の diff がもう一度届くのは書き込みのあと。既定に戻すと、選んで
        // いたハンクが先頭へ飛んでから次の操作が効くことになる。
        let mut state = diff_state(&["a.txt"], 3);
        let _ = press(&mut state, 'j');
        let _ = press(&mut state, 'j');
        assert_eq!(state.diff.view.as_ref().unwrap().cursor.hunk, 2);

        let _ = update(&mut state, exec_done(Ok(fake_output())));
        let _ = update(
            &mut state,
            Msg::StatusLoaded(Box::new(Ok(fake_snapshot(&[], &["a.txt"], &[])))),
        );
        let _ = update(
            &mut state,
            diff_ok(unstaged_diff("a.txt"), fake_diff("a.txt", 3)),
        );

        assert_eq!(state.diff.view.as_ref().unwrap().cursor.hunk, 2);
    }

    #[test]
    fn a_reload_with_fewer_hunks_seats_the_cursor_again() {
        let mut state = diff_state(&["a.txt"], 3);
        let _ = press(&mut state, 'j');
        let _ = press(&mut state, 'j');

        // ハンクが 1 つに減って届く。
        let target = unstaged_diff("a.txt");
        let _ = reply(
            &mut state,
            Cmd::LoadDiff(target.clone()),
            diff_ok(target, fake_diff("a.txt", 1)),
        );

        assert_eq!(state.diff.view.as_ref().unwrap().cursor.hunk, 0);
    }

    #[test]
    fn keys_do_not_move_the_cursor_of_a_diff_that_is_not_shown() {
        // 別の対象のものが `view` に残っているあいだ（読み込み中・失敗中）に `j` を
        // 押しても、画面に出ていない diff のカーソルは動かさない。
        let mut state = diff_state(&["a.txt", "b.txt"], 3);
        let _ = press_code(&mut state, KeyCode::Esc);
        let _ = press(&mut state, 'j');
        let _ = press_code(&mut state, KeyCode::Enter);

        // b.txt はまだ届いていない。`view` には a.txt が残っている。
        let _ = press(&mut state, 'j');

        assert_eq!(state.diff.view.as_ref().unwrap().cursor.hunk, 0);
    }

    #[test]
    fn j_and_k_move_the_hunk_cursor_and_stop_at_the_ends() {
        let mut state = diff_state(&["a.txt"], 3);
        assert_eq!(state.diff.view.as_ref().unwrap().cursor.hunk, 0);

        let hunk = |state: &AppState| state.diff.view.as_ref().unwrap().cursor.hunk;
        let _ = press(&mut state, 'j');
        assert_eq!(hunk(&state), 1);
        let _ = press(&mut state, 'j');
        let _ = press(&mut state, 'j');
        assert_eq!(hunk(&state), 2, "下端で止まる");
        let _ = press(&mut state, 'k');
        assert_eq!(hunk(&state), 1);
        let _ = press(&mut state, 'k');
        let _ = press(&mut state, 'k');
        assert_eq!(hunk(&state), 0, "上端で止まる");
    }

    // ---- ハンク単位の stage / unstage / discard -------------------------------

    /// 中身のあるハンク。パッチに戻したときに行が並ぶ。
    fn real_hunk(start: usize) -> Hunk {
        fake_hunk(
            &format!("@@ -{start},2 +{start},2 @@"),
            &[
                (LineOrigin::Context, "ctx"),
                (LineOrigin::Deletion, "old"),
                (LineOrigin::Addition, "new"),
            ],
        )
    }

    /// ハンク 1 つから組んだパッチ。**実装と同じ関数を通す。**
    /// 期待値を手で書くと、パッチの形を変えたときにテストの側だけが古くなる
    /// （形そのものは `git_exec::patch` のユニットテストと `tests/hunk.rs` が見張る）。
    fn patch_of(path: &str, hunk: &Hunk) -> Vec<u8> {
        hunk_patch(std::path::Path::new(path), hunk).expect("パッチを組めませんでした")
    }

    #[test]
    fn s_in_the_diff_pane_stages_only_the_hunk_under_the_cursor() {
        let hunks = vec![real_hunk(1), real_hunk(9)];
        let mut state = diff_pane(
            WorktreeSection::Unstaged,
            &["a.txt"],
            vec![fake_file_diff("a.txt", hunks.clone())],
        );
        // 2 番目のハンクへ移る。1 番目のパッチが出たら、選んでいる場所を見ていない。
        let _ = press(&mut state, 'j');

        let cmd = press(&mut state, 's');

        assert_eq!(
            cmd,
            Cmd::Exec(GitOp::StageHunk {
                path: "a.txt".into(),
                patch: patch_of("a.txt", &hunks[1]),
            })
        );
    }

    #[test]
    fn u_in_the_diff_pane_unstages_only_the_hunk_under_the_cursor() {
        let hunks = vec![real_hunk(1)];
        let mut state = diff_pane(
            WorktreeSection::Staged,
            &["a.txt"],
            vec![fake_file_diff("a.txt", hunks.clone())],
        );

        let cmd = press(&mut state, 'u');

        assert_eq!(
            cmd,
            Cmd::Exec(GitOp::UnstageHunk {
                path: "a.txt".into(),
                patch: patch_of("a.txt", &hunks[0]),
            })
        );
    }

    #[test]
    fn x_in_the_diff_pane_confirms_and_names_the_hunk() {
        let hunks = vec![real_hunk(1)];
        let mut state = diff_pane(
            WorktreeSection::Unstaged,
            &["a.txt"],
            vec![fake_file_diff("a.txt", hunks.clone())],
        );

        assert_eq!(press(&mut state, 'x'), Cmd::None);

        let Some(Modal::Confirm { body, on_yes, .. }) = &state.modal else {
            panic!("確認モーダルが出ていません: {:?}", state.modal);
        };
        // **ファイル名だけでは足りない。** ファイル単位の discard と同じ本文になり、
        // どちらを押したのか画面から判らなくなる。
        assert!(body.contains("a.txt"), "本文に対象が無い: {body}");
        assert!(
            body.contains(&hunks[0].header),
            "本文にハンクが無い: {body}"
        );
        assert_eq!(
            on_yes,
            &Cmd::Exec(GitOp::DiscardHunk {
                path: "a.txt".into(),
                patch: patch_of("a.txt", &hunks[0]),
            })
        );
    }

    #[test]
    fn x_on_a_staged_hunk_is_refused_with_a_reason() {
        let mut state = diff_pane(
            WorktreeSection::Staged,
            &["a.txt"],
            vec![fake_file_diff("a.txt", vec![real_hunk(1)])],
        );

        // index とワークツリーの両方に当てることになるので断る（03）。
        assert_eq!(press(&mut state, 'x'), Cmd::None);
        assert!(state.modal.is_none(), "{:?}", state.modal);
        let notice = state.notices.back().expect("通知が出ていません");
        assert_eq!(notice.level, NoticeLevel::Warn);
        assert!(notice.message.contains("unstage"), "{}", notice.message);
    }

    #[test]
    fn a_file_that_hunks_cannot_describe_falls_back_to_the_whole_file() {
        // ハンク = ファイル全体になるもの。パッチには `--- /dev/null` や
        // `rename from` のヘッダが要るので、ハンク単位の `GitOp` は出さない（07）。
        let untracked = FileDiff {
            status: FileStatus::Untracked,
            ..fake_file_diff("n.txt", vec![real_hunk(1)])
        };
        let mut state = diff_pane(WorktreeSection::Untracked, &["n.txt"], vec![untracked]);

        assert_eq!(
            press(&mut state, 's'),
            Cmd::Exec(GitOp::Stage(vec!["n.txt".into()]))
        );
    }

    #[test]
    fn a_binary_file_falls_back_to_the_whole_file() {
        // `binary` が立つとハンクは空になるが、`hunks` を見るだけでは
        // 「mode だけの変更」と区別が付かない。旗のほうで落とす。
        let binary = FileDiff {
            binary: true,
            ..fake_file_diff("a.png", vec![real_hunk(1)])
        };
        let mut state = diff_pane(WorktreeSection::Unstaged, &["a.png"], vec![binary]);

        assert_eq!(
            press(&mut state, 's'),
            Cmd::Exec(GitOp::Stage(vec!["a.png".into()]))
        );
    }

    #[test]
    fn a_file_level_fallback_is_not_refused_by_the_freshness_guard() {
        // **断るのはハンク単位のときだけ**（07）。ハンクにできないファイルへの `s` は
        // 古いパッチを一切使わないので、読み直しを待たせる理由が無い。ペイン名で
        // ガードを書くとここまで塞ぐ。
        let binary = FileDiff {
            binary: true,
            ..fake_file_diff("a.png", vec![real_hunk(1)])
        };
        let mut state = diff_pane(WorktreeSection::Unstaged, &["a.png"], vec![binary]);
        let _ = update(&mut state, exec_done(Ok(fake_output())));
        assert!(!state.diff_is_current());

        assert_eq!(
            press(&mut state, 's'),
            Cmd::Exec(GitOp::Stage(vec!["a.png".into()]))
        );
    }

    #[test]
    fn a_path_that_cannot_go_into_a_patch_is_refused_instead_of_falling_back() {
        // tab を含むパスは `git apply` が名前の終わりと読む（03）。ファイル単位に
        // 落とすと、ハンク 1 つのつもりでファイル全体を stage することになる。
        let mut state = diff_pane(
            WorktreeSection::Unstaged,
            &["a\tb.txt"],
            vec![fake_file_diff("a\tb.txt", vec![real_hunk(1)])],
        );

        assert_eq!(press(&mut state, 's'), Cmd::None);
        assert_eq!(
            state.notices.back().map(|notice| notice.level),
            Some(NoticeLevel::Warn)
        );
    }

    #[test]
    fn keys_are_refused_while_the_shown_diff_is_older_than_the_last_write() {
        // **区画で断られる組み合わせを混ぜない。** Unstaged で `u` は鮮度に関わらず
        // 「stage されていません」で断られるので、その腕は見張りを踏まない。
        for (section, key) in [
            (WorktreeSection::Unstaged, 's'),
            (WorktreeSection::Unstaged, 'x'),
            (WorktreeSection::Staged, 'u'),
        ] {
            let mut state = diff_pane(
                section,
                &["a.txt"],
                vec![fake_file_diff("a.txt", vec![real_hunk(1)])],
            );
            // 書き込みが終わった直後。読み直しはまだ届いていない。
            let _ = update(&mut state, exec_done(Ok(fake_output())));
            assert!(!state.diff_is_current(), "{key}");

            // 古いハンクから組んだパッチは、同じ内容のハンクが 2 つあると別の場所に当たる。
            assert_eq!(press(&mut state, key), Cmd::None, "{key}");
            assert!(state.modal.is_none(), "{key}");
            assert_eq!(
                state.notices.back().map(|notice| notice.message.as_str()),
                Some(NOT_LATEST),
                "{key}"
            );
        }
    }

    #[test]
    fn keys_are_refused_while_the_reload_has_been_asked_for_but_has_not_arrived() {
        // **`Cmd::LoadDiff` を出しただけでは新しくならない。** 返信が届くまで、
        // 出している view は書き込みより前のものそのもの。
        let mut state = diff_pane(
            WorktreeSection::Unstaged,
            &["a.txt"],
            vec![fake_file_diff("a.txt", vec![real_hunk(1)])],
        );
        let _ = update(&mut state, exec_done(Ok(fake_output())));
        let asked = update(
            &mut state,
            Msg::StatusLoaded(Box::new(Ok(fake_snapshot(&[], &["a.txt"], &[])))),
        );
        assert_eq!(asked, Cmd::LoadDiff(unstaged_diff("a.txt")), "頼んではいる");

        assert_eq!(press(&mut state, 's'), Cmd::None);
        assert_eq!(
            state.notices.back().map(|notice| notice.level),
            Some(NoticeLevel::Warn)
        );
    }

    #[test]
    fn keys_do_nothing_while_the_diff_pane_has_nothing_to_show() {
        // 「読み込み中…」や「差分を読み込めませんでした」を出しているあいだ、
        // **ファイル単位に落とすとハンク 1 つのつもりの `s` がファイル全体を stage する。**
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        let _ = press_code(&mut state, KeyCode::Enter);
        assert!(state.diff.view.is_none(), "まだ届いていない");

        for key in ['s', 'u', 'x'] {
            assert_eq!(press(&mut state, key), Cmd::None, "{key}");
            assert!(state.modal.is_none(), "{key}");
            assert_eq!(
                state.notices.back().map(|notice| notice.message.as_str()),
                Some(NOT_LATEST),
                "{key}"
            );
        }

        // 読み込めなかったあとも同じ。
        let _ = update(&mut state, diff_err(unstaged_diff("a.txt")));
        assert_eq!(press(&mut state, 's'), Cmd::None);
        assert_eq!(
            state.notices.back().map(|notice| notice.message.as_str()),
            Some(NOT_LATEST)
        );
    }

    #[test]
    fn a_reload_that_fails_leaves_the_shown_diff_marked_as_old() {
        // 頼んだ読み直しが失敗したら、出しているのは書き込み前の diff のまま。
        // **失敗を「新しくなった」と読まない** —— 採れたときだけ刻印が新しくなる。
        let mut state = diff_pane(
            WorktreeSection::Unstaged,
            &["a.txt"],
            vec![fake_file_diff("a.txt", vec![real_hunk(1)])],
        );
        let _ = update(&mut state, exec_done(Ok(fake_output())));
        let _ = update(
            &mut state,
            Msg::StatusLoaded(Box::new(Ok(fake_snapshot(&[], &["a.txt"], &[])))),
        );

        let cmd = update(&mut state, diff_err(unstaged_diff("a.txt")));

        // 同じ対象には出し直さない（「失敗 → 再読込 → 失敗」を回さない）。
        assert_eq!(cmd, Cmd::None);
        assert!(!state.diff_is_current());
        assert_eq!(press(&mut state, 's'), Cmd::None);
    }

    #[test]
    fn reopening_the_same_file_uses_the_diff_already_in_hand() {
        // 1 ファイルぶんの diff でもワークツリー全体を走査するので、開き直すたびに
        // 読むとリーダーが `LoadLog` を待たせる（01）。
        let mut state = diff_state(&["a.txt"], 1);

        let _ = press_code(&mut state, KeyCode::Esc);
        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::None);
    }

    #[test]
    fn a_write_makes_the_same_target_be_read_again() {
        // ハンク 1 つを stage しても残りが unstaged なら区画は変わらない。**対象が
        // 同じままなのでキャッシュが効いてしまう**のを世代が止める。
        let mut state = diff_state(&["a.txt"], 2);
        let _ = update(&mut state, exec_done(Ok(fake_output())));

        let cmd = update(
            &mut state,
            Msg::StatusLoaded(Box::new(Ok(fake_snapshot(&[], &["a.txt"], &[])))),
        );

        assert_eq!(cmd, Cmd::LoadDiff(unstaged_diff("a.txt")));
        // **まだ新しくなっていない。** 頼んだだけで、出している diff は書き込みより前。
        assert!(!state.diff_is_current());
    }

    #[test]
    fn a_reply_that_was_asked_for_before_the_write_is_asked_for_again() {
        // 書き込みの時点で `LoadDiff` が走っていると、`status_loaded` が出す
        // `Cmd::LoadDiff` は `issue` に落とされる。飛んでいた返信は書き込みより前の
        // ものなので、それを採ったまま黙って終わると古い diff が残る（01）。
        let mut state = diff_state(&["a.txt"], 2);
        assert!(state.jobs.start(Job::new(JobKind::LoadDiff, "diff",)));

        let _ = update(&mut state, exec_done(Ok(fake_output())));
        let dropped = update(
            &mut state,
            Msg::StatusLoaded(Box::new(Ok(fake_snapshot(&[], &["a.txt"], &[])))),
        );
        assert_eq!(dropped, Cmd::None, "走っているので落とされる");
        assert!(!state.diff_is_current(), "落とされたので、まだ頼めていない");

        // 書き込みより前に頼まれていた返信が届く（中身が違うと判るように 5 ハンク）。
        let cmd = update(
            &mut state,
            diff_ok(unstaged_diff("a.txt"), fake_diff("a.txt", 5)),
        );

        assert_eq!(cmd, Cmd::LoadDiff(unstaged_diff("a.txt")));
        assert!(!state.diff_is_current());
        // **書き込みより前の返信は採らない。** 採ると、頼み直しが届くまでのあいだ
        // 書き込み前の中身を新しいものとして出すことになる。
        assert_eq!(state.diff.view.as_ref().unwrap().files[0].hunks.len(), 2);
    }
}
