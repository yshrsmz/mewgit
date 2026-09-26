//! 進行中操作（rebase / cherry-pick / revert / merge / am の途中）があるときの書き込み。
//!
//! どの `GitOp` を断るかは 03 の「進行中に通す書き込み」（実測の表）、いつ・どこで
//! 断るかは 01 の「進行中操作があるときの書き込み」。

use crate::git_exec::GitOp;
use crate::git_read::InProgressOp;

use crate::app::{AppState, NoticeLevel};

impl AppState {
    /// 進行中操作があって `op` を断るなら、理由を通知に積んで真を返す。
    ///
    /// **信じるのは鮮度のある snapshot だけ**。古いときは git に任せる（01 の
    /// 「進行中操作があるときの書き込み」）。
    pub(crate) fn refuse_in_progress(&mut self, op: &GitOp) -> bool {
        if !self.worktree_is_current() {
            return false;
        }
        let Some(message) = self
            .worktree
            .snapshot
            .progress
            .as_ref()
            .and_then(|progress| refusal(progress, op))
        else {
            return false;
        };
        self.push_notice(NoticeLevel::Warn, message, None);
        true
    }
}

/// `progress` の途中で `op` を断るなら、その案内文。通すなら `None`。
///
/// **断るのは HEAD か作業ツリー全体を動かす書き込み。** 解消に要るもの（stage、merge を
/// 結ぶ commit、`edit` で止まったところの amend）と、実測で無害なもの（fetch、push、
/// ブランチの改名・削除）は通す（03 の表）。
///
/// 網羅 `match` にしているのは、`GitOp` に変種を足したときに断るか通すかを選ばせるため。
fn refusal(progress: &InProgressOp, op: &GitOp) -> Option<String> {
    let passes = match op {
        GitOp::Stage(_)
        | GitOp::Unstage(_)
        | GitOp::StageHunk { .. }
        | GitOp::UnstageHunk { .. }
        | GitOp::DiscardHunk { .. }
        | GitOp::Discard(_)
        | GitOp::DiscardUntracked(_)
        | GitOp::DiscardAdded(_)
        // cherry-pick / merge の途中の amend は git が断る。`edit` で止まった rebase では
        // amend がその使い道なので、こちらでは分けない。
        | GitOp::Commit { .. }
        | GitOp::BranchCreate { switch: false, .. }
        | GitOp::BranchDelete { .. }
        | GitOp::BranchRename { .. }
        | GitOp::Fetch { .. }
        | GitOp::Push { .. }
        | GitOp::StashDrop(_)
        | GitOp::SetAccount { .. }
        | GitOp::UnsetAccount => true,
        // **stash だけは git が受けて進行中操作を壊す**（解消済みの cherry-pick / merge で
        // `*_HEAD` が消える）。ほかは git も断るが、何の途中かと出口を先に言う。
        GitOp::Switch(_)
        | GitOp::BranchCreate { switch: true, .. }
        | GitOp::Pull { .. }
        | GitOp::CheckoutPr(_)
        | GitOp::RebaseInteractive { .. }
        | GitOp::CherryPick(_)
        | GitOp::Revert { .. }
        | GitOp::StashPush { .. }
        | GitOp::StashApply(_)
        | GitOp::StashPop(_) => false,
        // 進行中の操作と種類が合うときだけ。
        GitOp::RebaseContinue | GitOp::RebaseSkip | GitOp::RebaseAbort => {
            matches!(progress, InProgressOp::Rebase { .. })
        }
        GitOp::CherryPickContinue | GitOp::CherryPickAbort => {
            matches!(progress, InProgressOp::CherryPick { .. })
        }
        GitOp::RevertContinue | GitOp::RevertAbort => {
            matches!(progress, InProgressOp::Revert { .. })
        }
    };
    (!passes).then(|| advice(progress))
}

/// 何の途中かと、そこから出る手。**71 セルに収める**（01 の「失敗の案内文」）。
fn advice(progress: &InProgressOp) -> String {
    let verb = match progress {
        InProgressOp::Rebase { .. } => "rebase",
        InProgressOp::CherryPick { .. } => "cherry-pick",
        InProgressOp::Revert { .. } => "revert",
        // merge の continue に当たる `GitOp` は無い。結ぶのは `c`（03 の表）。
        InProgressOp::Merge { .. } => {
            return "merge の途中です。先に commit で結んでください".to_string();
        }
        // mewgit から am は始めず、`GitOp` にも無い（02 の「進行中操作の検出」）。
        InProgressOp::Am { .. } => {
            return "am の途中です。端末で git am --continue か --abort を打ってください"
                .to_string();
        }
    };
    format!("{verb} の途中です。先に continue か abort で終えてください")
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        cells, fake_output, finish_exec, open_input, press, press_code, state_on_branches,
        type_text, worktree_state, NOTICE_CELLS,
    };
    use super::*;
    use crate::app::{Cmd, KeyCode, Modal, Msg};
    use crate::git_exec::{LocalBranch, PrSource, PullMode};
    use crate::git_read::{Step, WorktreeSection};
    use crate::test_util::{fake_oid, fake_pr_checkout, fake_snapshot, reply};

    fn rebase() -> InProgressOp {
        InProgressOp::Rebase {
            step: Some(Step {
                current: 1,
                total: 2,
            }),
        }
    }

    fn cherry_pick() -> InProgressOp {
        InProgressOp::CherryPick {
            oid: Some(fake_oid(1)),
        }
    }

    fn every_progress() -> [InProgressOp; 5] {
        [
            rebase(),
            InProgressOp::Am { step: None },
            cherry_pick(),
            InProgressOp::Revert { oid: None },
            InProgressOp::Merge { oid: fake_oid(1) },
        ]
    }

    /// 進行中操作を載せた status を届ける（鮮度のある snapshot になる）。
    fn arrive(state: &mut AppState, progress: Option<InProgressOp>) {
        let mut snapshot = fake_snapshot(&["a.txt"], &[], &[]);
        snapshot.progress = progress;
        let _ = reply(
            state,
            Cmd::LoadStatus,
            Msg::StatusLoaded(Box::new(Ok(snapshot))),
        );
    }

    /// ステータスバーに出ている通知の文言。
    fn shown(state: &AppState) -> &str {
        &state.shown_notice().expect("通知が出ていません").message
    }

    #[test]
    fn resolving_a_conflict_and_concluding_a_merge_pass() {
        // **解消に要るものを断ると、コンフリクトから抜けられない**（03 の表）。
        for progress in every_progress() {
            for op in [
                GitOp::Stage(vec!["a.txt".into()]),
                GitOp::Discard(vec!["a.txt".into()]),
                GitOp::Commit {
                    message: "m".to_string(),
                    amend: false,
                },
                GitOp::Commit {
                    message: "m".to_string(),
                    amend: true,
                },
                GitOp::Fetch {
                    remote: None,
                    prune: false,
                },
                GitOp::BranchRename {
                    from: "a".to_string(),
                    to: "b".to_string(),
                },
            ] {
                assert_eq!(refusal(&progress, &op), None, "{progress:?} {op:?}");
            }
        }
    }

    #[test]
    fn moving_head_or_the_whole_worktree_is_refused() {
        for progress in every_progress() {
            for op in [
                GitOp::Switch("main".to_string()),
                GitOp::BranchCreate {
                    name: "b".to_string(),
                    start: None,
                    switch: true,
                },
                GitOp::Pull {
                    mode: PullMode::FastForwardOnly,
                },
                fake_pr_checkout(PrSource::SameRepo, LocalBranch::Absent),
                GitOp::CherryPick(vec![]),
                // **git が受けて進行中操作を壊す**唯一の形（03 の表）。
                GitOp::StashPush {
                    message: None,
                    include_untracked: false,
                    paths: None,
                },
            ] {
                assert!(refusal(&progress, &op).is_some(), "{progress:?} {op:?}");
            }
        }
    }

    #[test]
    fn continue_and_abort_pass_only_for_the_operation_in_progress() {
        assert_eq!(refusal(&rebase(), &GitOp::RebaseContinue), None);
        assert_eq!(refusal(&rebase(), &GitOp::RebaseSkip), None);
        assert_eq!(refusal(&cherry_pick(), &GitOp::CherryPickAbort), None);
        // cherry-pick の途中の `rebase --continue` は git が別の操作として扱う。
        assert!(refusal(&cherry_pick(), &GitOp::RebaseContinue).is_some());
        assert!(refusal(&rebase(), &GitOp::RevertAbort).is_some());
        // am には mewgit から打てる出口が無い。
        assert!(refusal(&InProgressOp::Am { step: None }, &GitOp::RebaseContinue).is_some());
    }

    #[test]
    fn every_advice_fits_the_notice_line() {
        for progress in every_progress() {
            let message = advice(&progress);
            assert!(cells(&message) <= NOTICE_CELLS, "{message}");
        }
    }

    #[test]
    fn switching_branches_in_the_middle_of_a_rebase_is_refused_before_git_runs() {
        let mut state = state_on_branches(&[("another", false), ("main", true)]);
        arrive(&mut state, Some(rebase()));
        let _ = press(&mut state, 'k');

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::None);
        assert!(shown(&state).starts_with("rebase の途中です"));
    }

    #[test]
    fn staging_in_the_middle_of_a_rebase_still_runs() {
        let mut snapshot = fake_snapshot(&["a.txt"], &[], &[]);
        snapshot.progress = Some(rebase());
        let mut state = worktree_state(snapshot, WorktreeSection::Staged);
        // 同じ状態で切り替えは断られる —— 通ったのが進行中操作を見落としたからではない。
        assert!(state.refuse_in_progress(&GitOp::Switch("main".to_string())));

        let cmd = press(&mut state, 'u');

        assert!(matches!(cmd, Cmd::Exec(GitOp::Unstage(_))), "{cmd:?}");
    }

    #[test]
    fn a_stale_snapshot_leaves_the_decision_to_git() {
        // abort した直後は status が届くまで古い。持っている「rebase の途中」を信じると、
        // 終わった rebase を理由に断る（01）。
        let mut state = state_on_branches(&[("another", false), ("main", true)]);
        arrive(&mut state, Some(rebase()));
        assert!(state.refuse_in_progress(&GitOp::Switch("another".to_string())));
        finish_exec(&mut state, GitOp::RebaseAbort, Ok(fake_output()));
        assert!(!state.worktree_is_current());
        let _ = press(&mut state, 'k');

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::Exec(GitOp::Switch("another".to_string())));
    }

    #[test]
    fn nothing_in_progress_refuses_nothing() {
        let mut state = state_on_branches(&[("another", false), ("main", true)]);
        arrive(&mut state, None);
        // 鮮度のある snapshot で「無い」と読めている（古くて git に任せた形ではない）。
        assert!(state.worktree_is_current());
        let _ = press(&mut state, 'k');

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::Exec(GitOp::Switch("another".to_string())));
    }

    #[test]
    fn n_is_refused_before_the_name_is_typed() {
        // 打ち終えてから断ると、打った名前が消える（01）。
        let mut state = state_on_branches(&[("main", true)]);
        arrive(&mut state, Some(rebase()));

        let cmd = press(&mut state, 'n');

        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.modal, None);
        assert!(shown(&state).starts_with("rebase の途中です"));
    }

    #[test]
    fn r_still_opens_because_renaming_passes() {
        let mut state = state_on_branches(&[("main", true)]);
        arrive(&mut state, Some(rebase()));

        let _ = press(&mut state, 'r');

        assert!(matches!(state.modal, Some(Modal::Input { .. })));
    }

    #[test]
    fn submitting_a_name_after_a_rebase_started_keeps_the_input_open() {
        // 打っているあいだに status が届いて rebase の途中と判った。閉じると打った名前が
        // 消えるので、走っている git で断るときと同じく閉じずに断る。
        let mut state = state_on_branches(&[("main", true)]);
        let _ = press(&mut state, 'n');
        type_text(&mut state, "topic");
        arrive(&mut state, Some(rebase()));

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::None);
        let (_, _, buf, _) = open_input(&state);
        assert_eq!(buf.text(), "topic");
        assert!(shown(&state).starts_with("rebase の途中です"));
    }
}
