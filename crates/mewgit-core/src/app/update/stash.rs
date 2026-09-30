//! Stash ペインのキー処理と一覧の受け取り（01 の「Stash ペイン」）。

use crate::git_exec::GitOp;
use crate::git_read::{GitReadError, StashEntry};

use super::branches::input;
use super::{confirm, focus};
use crate::app::{
    AppState, Cmd, DiffSource, InputIntent, JobKind, Key, KeyCode, NoticeLevel, Pane, TextBuffer,
};

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
        KeyCode::Char('z') => open_stash_push(state),
        KeyCode::Char('a') => on_selected(state, GitOp::StashApply),
        KeyCode::Char('Z') => on_selected(state, GitOp::StashPop),
        KeyCode::Char('d') => drop_stash(state),
        _ => Cmd::None,
    }
}

/// `z`（Worktree と Stash）。メッセージを聞く入力モーダルを開く（01 の「stash の
/// 書き込み」）。**一覧が空でも開く** —— 退避する相手は作業ツリーで、カーソル行ではない。
pub(super) fn open_stash_push(state: &mut AppState) -> Cmd {
    input(
        state,
        "stash（untracked を含む）".to_string(),
        "メッセージ（空なら git の既定）".to_string(),
        TextBuffer::default(),
        InputIntent::StashPush,
    )
}

/// `a` / `Z`。カーソル行の stash を `op` で書き込む。**鮮度は `issue` が問う**
/// （[`AppState::refuse_stale_stash`]）。一覧が空なら何もしない（`Enter` と同じ）。
fn on_selected(state: &AppState, op: fn(usize) -> GitOp) -> Cmd {
    state
        .stashes
        .selected()
        .map_or(Cmd::None, |stash| Cmd::Exec(op(stash.index)))
}

/// `d`。カーソル行の stash を消す確認を開く（不変ルール 5）。
///
/// **開く前に `refusal` 全体を問う**（01 の「stash の書き込み」）。git が走っている
/// あいだに開けると、その書き込み（autostash の `pull`）が終わって一覧がずれたあとに
/// `y` が通り、**確認に出したのとは別の stash を消す**。走っていなければ、確認を
/// 開いているあいだに始まる書き込みは無い（キーはモーダルが受ける）。
fn drop_stash(state: &mut AppState) -> Cmd {
    let Some(stash) = state.stashes.selected() else {
        return Cmd::None;
    };
    let op = GitOp::StashDrop(stash.index);
    let body = format!(
        "{}（{}）を削除します。取り消せません。",
        stash.name(),
        stash.message
    );
    if let Some(instead) = state.refusal(&Cmd::Exec(op.clone())) {
        return instead;
    }
    confirm(state, "stash を drop", body, op)
}

impl AppState {
    /// stash の書き込みを、持っている値が古くて断るなら、理由を通知に積んで代わりに
    /// 出す `Cmd` を返す（01 の「stash の書き込み」）。
    ///
    /// - **push は鮮度のある snapshot でしか通さない。** 解消済みの cherry-pick / merge の
    ///   途中の `stash push` は git が受けて `*_HEAD` を消す（03 の表）。進行中操作の
    ///   門（[`refuse_in_progress`](Self::refuse_in_progress)）は古い snapshot を
    ///   git に任せるので、書き込み直後の窓でそれを踏む。鮮度があって空なら断る ——
    ///   git は `No local changes to save` を exit 0 で返し、何も起きないように見える
    /// - **apply / pop / drop は鮮度のある一覧でしか通さない。** 渡すのは
    ///   `stash@{index}` で、古い一覧の index は別の stash を指す（oid は drop / pop が
    ///   受けない。`is not a stash reference`）
    ///
    /// 網羅 `match` にしているのは、`GitOp` に変種を足したときに、持っている値の鮮度に
    /// 頼る書き込みかを選ばせるため。
    pub(crate) fn refuse_stale_stash(&mut self, op: &GitOp) -> Option<Cmd> {
        let (message, instead) = match op {
            GitOp::StashPush { .. } if !self.worktree_is_current() => {
                // 続きを頼む: 古い snapshot で断った push の代わりの status の読み直し。
                ("状態を読み直しています。もう一度押して", Cmd::LoadStatus)
            }
            GitOp::StashPush {
                include_untracked, ..
            } if nothing_to_stash(self, *include_untracked) => {
                ("退避する変更がありません", Cmd::None)
            }
            GitOp::StashApply(_) | GitOp::StashPop(_) | GitOp::StashDrop(_)
                if !self.stashes_are_current() =>
            {
                // 続きを頼む: 古い一覧で断った書き込みの代わりの stash 一覧の読み直し。
                (
                    "stash の一覧を読み直しています。もう一度押して",
                    Cmd::LoadStashes,
                )
            }
            GitOp::StashPush { .. }
            | GitOp::StashApply(_)
            | GitOp::StashPop(_)
            | GitOp::StashDrop(_)
            | GitOp::Stage(_)
            | GitOp::Unstage(_)
            | GitOp::StageHunk { .. }
            | GitOp::UnstageHunk { .. }
            | GitOp::DiscardHunk { .. }
            | GitOp::Discard(_)
            | GitOp::DiscardUntracked(_)
            | GitOp::DiscardAdded(_)
            | GitOp::Commit { .. }
            | GitOp::BranchCreate { .. }
            | GitOp::Switch(_)
            | GitOp::BranchDelete { .. }
            | GitOp::BranchRename { .. }
            | GitOp::Fetch { .. }
            | GitOp::Pull { .. }
            | GitOp::Push { .. }
            | GitOp::RebaseInteractive { .. }
            | GitOp::RebaseContinue
            | GitOp::RebaseSkip
            | GitOp::RebaseAbort
            | GitOp::CherryPick(_)
            | GitOp::CherryPickContinue
            | GitOp::CherryPickSkip
            | GitOp::CherryPickAbort
            | GitOp::Revert { .. }
            | GitOp::RevertContinue
            | GitOp::RevertSkip
            | GitOp::RevertAbort
            | GitOp::MergeAbort
            | GitOp::SetAccount { .. }
            | GitOp::UnsetAccount
            | GitOp::CheckoutPr(_) => return None,
        };
        self.push_notice(NoticeLevel::Warn, message.to_string(), None);
        Some(instead)
    }
}

/// 鮮度のある snapshot に退避するものが無いか。`-u` を付けるなら untracked も数える。
fn nothing_to_stash(state: &AppState, include_untracked: bool) -> bool {
    let snapshot = &state.worktree.snapshot;
    snapshot.staged.is_empty()
        && snapshot.unstaged.is_empty()
        && snapshot.conflicted.is_empty()
        && (!include_untracked || snapshot.untracked.is_empty())
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
        // **いつの一覧かを値に添える**（`status_loaded` と同じ）。apply / pop / drop に
        // 渡す index を信じてよいかは、この刻印で判る。
        Ok(entries) => state.stashes.loaded(entries, asked),
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
        diff_ok, exec_done_of, failed_with, fake_diff, fake_output, finish_exec, issued,
        open_confirm, open_input, press, press_code, state, state_on_stashes, tab_to, type_text,
        with_stashes, worktree_state,
    };
    use super::super::worktree::STASH_DIFF;
    use super::*;
    use crate::app::{update, ListLoad, Msg, NoticeLevel, Region};
    use crate::git_exec::{ExecErrorKind, PullMode};
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

    /// `n` 件の stash と `snapshot` が**鮮度のある状態で**届き、Stash ペインに居る。
    fn ready(n: usize, snapshot: crate::git_read::WorktreeSnapshot) -> AppState {
        let mut state = worktree_state(snapshot, WorktreeSection::Unstaged);
        let _ = with_stashes(&mut state, Ok((0..n).map(fake_stash).collect()));
        let _ = tab_to(&mut state, Pane::Stash);
        state
    }

    fn dirty() -> crate::git_read::WorktreeSnapshot {
        fake_snapshot(&[], &["a.txt"], &[])
    }

    fn message(state: &AppState) -> Option<&str> {
        state.shown_notice().map(|notice| notice.message.as_str())
    }

    fn push_op(message: Option<&str>) -> GitOp {
        GitOp::StashPush {
            message: message.map(str::to_string),
            include_untracked: true,
            paths: None,
        }
    }

    #[test]
    fn z_asks_for_a_message_and_a_blank_one_pushes_with_the_git_default() {
        // 空なら `--message` を付けない（01 の「stash の書き込み」）。名前の入力と違い
        // 空を断らない。
        let mut state = ready(1, dirty());

        let _ = press(&mut state, 'z');
        assert_eq!(*open_input(&state).3, InputIntent::StashPush);
        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::Exec(push_op(None)));
        assert_eq!(state.modal, None);
    }

    #[test]
    fn z_pushes_everything_including_untracked_with_the_typed_message() {
        // untracked だけの作業ツリーでも開く —— `-u` を付けるので退避するものがある。
        let mut state = ready(0, fake_snapshot(&[], &[], &["new.txt"]));

        let _ = press(&mut state, 'z');
        type_text(&mut state, "wip");
        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::Exec(push_op(Some("wip"))));
    }

    #[test]
    fn z_in_the_worktree_pane_opens_the_same_input() {
        let mut state = worktree_state(dirty(), WorktreeSection::Unstaged);

        let _ = press(&mut state, 'z');

        assert_eq!(*open_input(&state).3, InputIntent::StashPush);
    }

    #[test]
    fn z_on_a_clean_worktree_is_refused_before_the_input_opens() {
        // git は `No local changes to save` を exit 0 で返し、何も起きないように見える。
        let mut state = ready(1, fake_snapshot(&[], &[], &[]));

        let cmd = press(&mut state, 'z');

        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.modal, None);
        assert_eq!(message(&state), Some("退避する変更がありません"));
    }

    #[test]
    fn z_on_a_stale_snapshot_is_refused_and_reads_the_status_again() {
        // 書き込みの直後の status 待ち。解消済みの cherry-pick / merge の途中の push は
        // git が受けて `*_HEAD` を消す（03 の表）ので、進行中操作の門と違い git に
        // 任せない。
        let mut state = ready(1, dirty());
        finish_exec(
            &mut state,
            GitOp::Stage(vec!["a.txt".into()]),
            Ok(fake_output()),
        );
        state.jobs.finish(JobKind::LoadStatus);
        assert!(!state.worktree_is_current());

        let cmd = press(&mut state, 'z');

        assert_eq!(state.modal, None);
        assert_eq!(cmd, Cmd::LoadStatus);
        assert_eq!(
            message(&state),
            Some("状態を読み直しています。もう一度押して")
        );
    }

    #[test]
    fn a_push_typed_before_the_snapshot_went_stale_is_refused_without_closing_the_input() {
        // 入力の確定も同じ門（`AppState::refusal`）を通る。打ったメッセージは消えない。
        let mut state = ready(1, dirty());
        let _ = press(&mut state, 'z');
        type_text(&mut state, "wip");
        state.jobs.advance(Region::Worktree);

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert!(!issued(&cmd, &Cmd::Exec(push_op(Some("wip")))), "{cmd:?}");
        assert_eq!(open_input(&state).2.text(), "wip");
    }

    #[test]
    fn a_and_capital_z_apply_and_pop_the_stash_under_the_cursor() {
        let mut state = ready(2, dirty());
        let _ = press(&mut state, 'j');

        assert_eq!(press(&mut state, 'a'), Cmd::Exec(GitOp::StashApply(1)));
        state.jobs.finish(JobKind::Exec);
        assert_eq!(press(&mut state, 'Z'), Cmd::Exec(GitOp::StashPop(1)));
    }

    #[test]
    fn d_confirms_before_dropping_and_names_the_stash() {
        let mut state = ready(2, dirty());
        let _ = press(&mut state, 'j');

        let cmd = press(&mut state, 'd');

        assert_eq!(cmd, Cmd::None, "確認より先に git を走らせています");
        let (body, on_yes) = open_confirm(&state);
        assert!(body.contains("stash@{1}"), "{body}");
        assert!(body.contains("stash 1"), "{body}");
        assert_eq!(*on_yes, Cmd::Exec(GitOp::StashDrop(1)));
    }

    #[test]
    fn d_is_refused_while_git_is_running() {
        // 開けると、走っている書き込み（autostash の pull）が終わって一覧がずれたあとに
        // `y` が通り、確認に出したのとは別の stash を消す。
        let mut state = ready(2, dirty());
        let _ = state.issue(Cmd::Exec(GitOp::Fetch {
            remote: None,
            prune: false,
        }));

        let cmd = press(&mut state, 'd');

        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.modal, None);
        assert!(state.shown_notice().is_some());
    }

    #[test]
    fn the_stash_keys_do_nothing_while_the_list_is_empty() {
        for key in ['a', 'Z', 'd'] {
            let mut state = ready(0, dirty());

            assert_eq!(press(&mut state, key), Cmd::None, "{key}");
            assert_eq!(state.modal, None, "{key}");
        }
    }

    #[test]
    fn a_stash_write_reads_the_status_and_the_list_again_but_not_the_log() {
        let mut state = ready(1, dirty());
        let _ = state.issue(Cmd::Exec(GitOp::StashPop(0)));

        let cmd = update(
            &mut state,
            exec_done_of(GitOp::StashPop(0), Ok(fake_output())),
        );

        assert!(issued(&cmd, &Cmd::LoadStatus), "{cmd:?}");
        assert!(issued(&cmd, &Cmd::LoadStashes), "{cmd:?}");
        assert!(!issued(&cmd, &Cmd::LoadRepoInfo), "{cmd:?}");
        assert!(!state.stashes_are_current());
    }

    #[test]
    fn an_index_from_a_list_older_than_the_last_write_is_refused() {
        // push は先頭に 1 つ足すので、届く前の一覧の `stash@{1}` は別の stash を指す。
        let mut state = ready(2, dirty());
        let _ = press(&mut state, 'j');
        finish_exec(&mut state, push_op(None), Ok(fake_output()));

        for key in ['a', 'Z', 'd'] {
            let cmd = press(&mut state, key);

            assert!(!matches!(cmd, Cmd::Exec(_)), "{key}: {cmd:?}");
            assert_eq!(state.modal, None, "{key}");
            assert_eq!(
                message(&state),
                Some("stash の一覧を読み直しています。もう一度押して"),
                "{key}"
            );
        }

        // 一覧が届けば通る。
        let _ = update(
            &mut state,
            Msg::StashesLoaded(Box::new(Ok((0..3).map(fake_stash).collect()))),
        );
        assert!(matches!(
            press(&mut state, 'a'),
            Cmd::Exec(GitOp::StashApply(_))
        ));
    }

    #[test]
    fn a_conflicting_pop_says_the_stash_is_kept() {
        // git は `CONFLICT` を stdout に出して exit 1、stderr は空（01 に実測）。
        // 進行中操作にはならないので `m` は案内しない。
        let mut state = ready(1, dirty());

        finish_exec(
            &mut state,
            GitOp::StashPop(0),
            Err(failed_with(ExecErrorKind::Conflict)),
        );

        let notice = state.shown_notice().expect("通知がありません");
        assert_eq!(notice.level, NoticeLevel::Warn);
        assert!(
            notice.message.contains("stash は残してある"),
            "{}",
            notice.message
        );
        assert!(!notice.message.contains("m から"), "{}", notice.message);
    }
}
