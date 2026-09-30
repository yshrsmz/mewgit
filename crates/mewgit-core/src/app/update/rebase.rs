//! interactive rebase の開始（Graph の `i`）と、plan を編む [`Modal::RebaseEditor`]。
//!
//! 範囲と断る形は 01 の「interactive rebase の開始（`i`）」、エディタのキーと描き方は
//! 07 の「RebaseEditor」、git への渡し方は 03 の「Interactive rebase の実装」。

use std::collections::HashMap;

use super::graph::picked_commit;
use super::modal::picker_key;
use super::{confirm, submit_exec};
use crate::app::state::stepped_index;
use crate::app::{AppState, Cmd, Key, KeyCode, Modal, NoticeLevel};
use crate::git_exec::{GitOp, RebaseAction, RebaseEntry, RebasePlan};
use crate::git_read::{short_oid, Commit, HeadInfo, Oid};

/// `i`。カーソルの指すコミットから HEAD までを [`Modal::RebaseEditor`] に並べる。
///
/// **断るのは開く前**（01）。エディタで並べ替えた plan は、閉じたあとで断ると失われる。
/// 進行中操作と作業ツリーの断りは [`AppState::refuse_write`] の 1 本で、確定のときの
/// `issue` にも同じものが効く。
pub(super) fn open_rebase_editor(state: &mut AppState) -> Cmd {
    let Some(selected) = picked_commit(state, "rebase") else {
        return Cmd::None;
    };
    let (head, plan) = match plan_to(state, &selected) {
        Ok(found) => found,
        Err(message) => {
            state.push_notice(NoticeLevel::Warn, message.to_string(), None);
            return Cmd::None;
        }
    };
    // `plan_to` が root コミットを断っているので、親は必ずある。
    let onto = selected.parents[0];
    let op = rebase_op(onto, head, plan);
    if let Some(instead) = state.refuse_write(&op) {
        return instead;
    }
    let GitOp::RebaseInteractive { plan, .. } = op else {
        unreachable!("rebase_op は RebaseInteractive を組む");
    };
    state.modal = Some(Modal::RebaseEditor {
        onto,
        head,
        plan,
        cursor: 0,
    });
    Cmd::None
}

/// HEAD から `selected` までを第 1 親で辿り、古い順（git の todo の順）に並べた plan と、
/// 辿り始めた HEAD。組めなければ断りの文言。
///
/// **読み込み済みのグラフだけを辿る**（読み足さない。01）。表示行の並びでは切らない ——
/// 行はレーンをまたいだ topo-order なので、あいだの行が HEAD の祖先とは限らない。
fn plan_to(state: &AppState, selected: &Commit) -> Result<(Oid, RebasePlan), &'static str> {
    if selected.parents.is_empty() {
        return Err("root コミットからは rebase できません");
    }
    let head = match &state.repo.head {
        Some(HeadInfo::Branch { oid, .. } | HeadInfo::Detached(oid)) => *oid,
        Some(HeadInfo::Unborn { .. }) | None => return Err("HEAD のコミットがまだありません"),
    };
    let loaded: HashMap<Oid, &Commit> = state
        .graph
        .lines
        .iter()
        .map(|line| (line.commit.oid, &line.commit))
        .collect();
    let mut entries = Vec::new();
    let mut at = head;
    loop {
        let Some(commit) = loaded.get(&at) else {
            return Err(UNREACHABLE);
        };
        // git は既定の todo で merge を落とし、topic 側を 1 列に混ぜる（01 の実測）。
        // 第 1 親だけの plan を渡すと topic 側のコミットが消える。
        if commit.parents.len() > 1 {
            return Err("範囲に merge コミットがあるので rebase できません");
        }
        entries.push(RebaseEntry {
            action: RebaseAction::Pick,
            oid: commit.oid,
            summary: commit.summary.clone(),
        });
        if at == selected.oid {
            break;
        }
        // 親が無い = root まで辿って届かなかった。
        let Some(&parent) = commit.parents.first() else {
            return Err(UNREACHABLE);
        };
        at = parent;
    }
    entries.reverse();
    Ok((head, RebasePlan { entries }))
}

/// HEAD から第 1 親で辿って選択に届かないときの断り。**祖先でないのか、まだ読み込んで
/// いないのかは分けない** —— 分けるには残りのページを読むことになる（01）。
const UNREACHABLE: &str =
    "HEAD から辿れないコミットです（HEAD の祖先でないか、まだ読み込んでいない）";

fn rebase_op(onto: Oid, head: Oid, plan: RebasePlan) -> GitOp {
    GitOp::RebaseInteractive {
        onto: onto.to_string(),
        head,
        plan,
    }
}

/// [`Modal::RebaseEditor`] のキー処理（07 の「RebaseEditor」）。`J` / `K` と動作のキーを
/// 受け、残り（`j` / `k` / `Enter` / `Esc`）は `picker_key` に委ねる（`account_key` と
/// 同じ形）。
pub(super) fn rebase_editor_key(state: &mut AppState, key: Key) -> Cmd {
    let action = match key.code() {
        KeyCode::Char('J') => return swap(state, 1),
        KeyCode::Char('K') => return swap(state, -1),
        // `r`（reword）はまだ無い（`docs/plan.md` の M5 で次のタスクに割った）。
        KeyCode::Char('p') => RebaseAction::Pick,
        KeyCode::Char('e') => RebaseAction::Edit,
        KeyCode::Char('s') => RebaseAction::Squash,
        KeyCode::Char('f') => RebaseAction::Fixup,
        KeyCode::Char('d') => RebaseAction::Drop,
        _ => return picker_key(state, key, submit),
    };
    if let Some(Modal::RebaseEditor { plan, cursor, .. }) = state.modal.as_mut() {
        if let Some(entry) = plan.entries.get_mut(*cursor) {
            entry.action = action;
        }
    }
    Cmd::None
}

/// カーソルの行を `delta` の向きの隣と入れ替える。**カーソルは行について動く。**
/// 端の扱いは `j` / `k` と同じ（[`stepped_index`]。回り込まない）。
fn swap(state: &mut AppState, delta: isize) -> Cmd {
    if let Some(Modal::RebaseEditor { plan, cursor, .. }) = state.modal.as_mut() {
        let to = stepped_index(*cursor, plan.entries.len(), delta);
        plan.entries.swap(*cursor, to);
        *cursor = to;
    }
    Cmd::None
}

/// `Enter`。**`RebasePlan::validate()` を門にする**（エディタの赤い行も同じ関数を読む）。
/// 通らなければ閉じずに理由を通知に出す。**全部 `drop` は確認を挟む**（03）。
fn submit(state: &mut AppState) -> Cmd {
    let Some(Modal::RebaseEditor {
        onto, head, plan, ..
    }) = &state.modal
    else {
        return Cmd::None;
    };
    if let Err(err) = plan.validate() {
        state.push_notice(NoticeLevel::Warn, err.to_string(), None);
        return Cmd::None;
    }
    let onto = *onto;
    let dropped = plan
        .entries
        .iter()
        .all(|entry| entry.action == RebaseAction::Drop)
        .then_some(plan.entries.len());
    let op = rebase_op(onto, *head, plan.clone());
    let Some(count) = dropped else {
        return submit_exec(state, op);
    };
    // `confirm` はモーダルを差し替える。断られる書き込みなら開く前に断る
    // （`submit_exec` と同じ判定）。
    if let Some(instead) = state.refusal(&Cmd::Exec(op.clone())) {
        return instead;
    }
    let body = format!(
        "{count} 本を全部 drop し、ブランチを {} まで戻します。",
        short_oid(&onto)
    );
    confirm(state, "rebase", body, op)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{open_confirm, press, press_code, state_with_commits};
    use super::*;
    use crate::app::Msg;
    use crate::git_read::{InProgressOp, WorktreeSnapshot};
    use crate::test_util::{fake_oid, fake_snapshot, reply};

    /// `n` 件の 1 列の履歴（`commit i` の親は `commit i+1`、最後が root）。HEAD は
    /// `commit 0` のブランチで、作業ツリーは鮮度のある空の snapshot。
    fn linear(n: usize) -> AppState {
        let mut state = state_with_commits(n);
        for (i, line) in state.graph.lines.iter_mut().enumerate() {
            line.commit.parents = if i + 1 < n {
                vec![fake_oid(i as u8 + 1)]
            } else {
                Vec::new()
            };
        }
        state.repo.head = Some(HeadInfo::Branch {
            name: "main".to_string(),
            oid: fake_oid(0),
            upstream: None,
        });
        status(&mut state, fake_snapshot(&[], &[], &[]));
        state
    }

    fn status(state: &mut AppState, snapshot: WorktreeSnapshot) {
        let _ = reply(
            state,
            Cmd::LoadStatus,
            Msg::StatusLoaded(Box::new(Ok(snapshot))),
        );
    }

    /// カーソルを `commit index` の行に乗せて `i` を押す（0 行目は WIP 行）。
    fn press_i_on(state: &mut AppState, index: usize) -> Cmd {
        for _ in 0..=index {
            let _ = press(state, 'j');
        }
        press(state, 'i')
    }

    fn editor(state: &AppState) -> (Oid, &RebasePlan, usize) {
        match &state.modal {
            Some(Modal::RebaseEditor {
                onto, plan, cursor, ..
            }) => (*onto, plan, *cursor),
            other => panic!("RebaseEditor が開いていません: {other:?}"),
        }
    }

    fn actions(plan: &RebasePlan) -> Vec<(&'static str, Oid)> {
        plan.entries
            .iter()
            .map(|entry| (entry.action.keyword(), entry.oid))
            .collect()
    }

    /// エディタを開かずに、`level` の通知で断った。
    fn refused(state: &AppState, level: NoticeLevel, needle: &str) {
        assert_eq!(state.modal, None);
        let notice = state.shown_notice().expect("通知が出ていません");
        assert_eq!(notice.level, level);
        assert!(notice.message.contains(needle), "{}", notice.message);
    }

    #[test]
    fn i_lists_the_selected_commit_up_to_head_oldest_first_onto_its_parent() {
        let mut state = linear(4);

        assert_eq!(press_i_on(&mut state, 2), Cmd::None);

        let Some(Modal::RebaseEditor { head, .. }) = &state.modal else {
            panic!("RebaseEditor が開いていません: {:?}", state.modal);
        };
        // plan を組んだ HEAD を運ぶ（sequence editor が `orig-head` と比べる。03）。
        assert_eq!(*head, fake_oid(0));
        let (onto, plan, cursor) = editor(&state);
        // 選択コミットも範囲に入る（`git rebase -i <選択>^` と同じ。01）。
        assert_eq!(onto, fake_oid(3));
        assert_eq!(
            actions(plan),
            [
                ("pick", fake_oid(2)),
                ("pick", fake_oid(1)),
                ("pick", fake_oid(0))
            ]
        );
        assert_eq!(plan.entries[0].summary, "commit 2");
        assert_eq!(cursor, 0);
    }

    #[test]
    fn i_walks_first_parents_not_the_rows_between() {
        // HEAD（commit 0）の親は commit 2。あいだの行（commit 1）は別の枝で、
        // 表示行の並びで切ると混ざる（01）。
        let mut state = linear(4);
        state.graph.lines[0].commit.parents = vec![fake_oid(2)];

        let _ = press_i_on(&mut state, 2);

        let (_, plan, _) = editor(&state);
        assert_eq!(
            actions(plan),
            [("pick", fake_oid(2)), ("pick", fake_oid(0))]
        );
    }

    #[test]
    fn i_on_the_wip_row_has_nothing_to_rebase() {
        let mut state = linear(3);

        assert_eq!(press(&mut state, 'i'), Cmd::None);

        refused(&state, NoticeLevel::Info, "コミットの行");
    }

    #[test]
    fn i_on_the_root_commit_is_refused() {
        // `GitOp::RebaseInteractive` は `--root` を表せない（01）。
        let mut state = linear(3);

        let _ = press_i_on(&mut state, 2);

        refused(&state, NoticeLevel::Warn, "root");
    }

    #[test]
    fn a_merge_commit_in_the_range_is_refused() {
        // git は merge を落として topic 側を 1 列に混ぜる（01 の実測）。
        let mut state = linear(4);
        state.graph.lines[1].commit.parents = vec![fake_oid(2), fake_oid(42)];

        let _ = press_i_on(&mut state, 2);

        refused(&state, NoticeLevel::Warn, "merge");
    }

    #[test]
    fn a_commit_head_does_not_reach_is_refused() {
        // HEAD の親がまだ読み込んでいないコミット。祖先でない形と区別しない（01）。
        let mut state = linear(4);
        state.graph.lines[0].commit.parents = vec![fake_oid(99)];

        let _ = press_i_on(&mut state, 2);

        refused(&state, NoticeLevel::Warn, "HEAD から辿れない");
    }

    #[test]
    fn an_unborn_head_is_refused() {
        let mut state = linear(3);
        state.repo.head = Some(HeadInfo::Unborn {
            name: "main".to_string(),
        });

        let _ = press_i_on(&mut state, 1);

        refused(&state, NoticeLevel::Warn, "HEAD");
    }

    #[test]
    fn tracked_changes_are_refused_before_the_editor_opens_but_untracked_ones_are_not() {
        // 開いてから断ると、並べ替えた plan が消える（01）。
        let mut state = linear(3);
        status(&mut state, fake_snapshot(&[], &["a.txt"], &[]));

        let _ = press_i_on(&mut state, 1);

        refused(&state, NoticeLevel::Warn, "commit か stash");

        // untracked だけなら git は通す（01 の実測）。
        let mut state = linear(3);
        status(&mut state, fake_snapshot(&[], &[], &["new.txt"]));

        let _ = press_i_on(&mut state, 1);

        let _ = editor(&state);
    }

    #[test]
    fn a_stale_snapshot_asks_for_the_status_again_instead_of_opening() {
        let mut state = linear(3);
        state.worktree.snapshot_epoch = None;

        assert_eq!(press_i_on(&mut state, 1), Cmd::LoadStatus);

        refused(&state, NoticeLevel::Warn, "読み直しています");
    }

    #[test]
    fn an_operation_in_progress_is_refused_before_the_editor_opens() {
        let mut state = linear(3);
        let mut snapshot = fake_snapshot(&[], &[], &[]);
        snapshot.progress = Some(InProgressOp::CherryPick {
            oid: Some(fake_oid(9)),
        });
        status(&mut state, snapshot);

        let _ = press_i_on(&mut state, 1);

        refused(&state, NoticeLevel::Warn, "cherry-pick");
    }

    #[test]
    fn the_editor_changes_actions_and_moves_rows_with_the_cursor() {
        let mut state = linear(4);
        let _ = press_i_on(&mut state, 2);

        // 先頭（commit 2）を下へ。カーソルは行について動く。
        let _ = press(&mut state, 'J');
        let (_, plan, cursor) = editor(&state);
        assert_eq!(cursor, 1);
        assert_eq!(plan.entries[1].oid, fake_oid(2));
        // 下端では動かない。
        let _ = press(&mut state, 'j');
        let _ = press(&mut state, 'J');
        assert_eq!(editor(&state).2, 2);
        // 動作を変える。
        let _ = press(&mut state, 'f');
        let _ = press(&mut state, 'k');
        let _ = press(&mut state, 'K');
        // 上端では動かない。
        let _ = press(&mut state, 'K');
        assert_eq!(editor(&state).2, 0);
        let _ = press(&mut state, 'd');
        let _ = press(&mut state, 'p');
        let _ = press(&mut state, 's');
        let _ = press(&mut state, 'j');
        let _ = press(&mut state, 'e');

        let (_, plan, cursor) = editor(&state);
        assert_eq!(cursor, 1);
        assert_eq!(
            actions(plan),
            [
                ("squash", fake_oid(2)),
                ("edit", fake_oid(1)),
                ("fixup", fake_oid(0))
            ]
        );
    }

    #[test]
    fn enter_refuses_a_plan_git_would_stop_on_and_keeps_the_editor_open() {
        let mut state = linear(3);
        let _ = press_i_on(&mut state, 1);
        let _ = press(&mut state, 's');

        assert_eq!(press_code(&mut state, KeyCode::Enter), Cmd::None);

        let _ = editor(&state);
        let notice = state.shown_notice().expect("通知が出ていません");
        assert!(notice.message.contains("squash"), "{}", notice.message);
    }

    #[test]
    fn enter_runs_the_plan_and_closes_the_editor() {
        let mut state = linear(3);
        let _ = press_i_on(&mut state, 1);
        let _ = press(&mut state, 'j');
        let _ = press(&mut state, 'f');
        let plan = editor(&state).1.clone();

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(
            cmd,
            Cmd::Exec(GitOp::RebaseInteractive {
                onto: fake_oid(2).to_string(),
                head: fake_oid(0),
                plan,
            })
        );
        assert_eq!(state.modal, None);
    }

    #[test]
    fn dropping_every_row_asks_before_running() {
        let mut state = linear(3);
        let _ = press_i_on(&mut state, 1);
        let _ = press(&mut state, 'd');
        let _ = press(&mut state, 'j');
        let _ = press(&mut state, 'd');
        let plan = editor(&state).1.clone();

        assert_eq!(press_code(&mut state, KeyCode::Enter), Cmd::None);

        let (body, on_yes) = open_confirm(&state);
        assert!(body.contains(&short_oid(&fake_oid(2))), "{body}");
        assert_eq!(
            on_yes,
            &Cmd::Exec(GitOp::RebaseInteractive {
                onto: fake_oid(2).to_string(),
                head: fake_oid(0),
                plan,
            })
        );
    }

    #[test]
    fn esc_closes_the_editor_without_running() {
        let mut state = linear(3);
        let _ = press_i_on(&mut state, 1);

        assert_eq!(press_code(&mut state, KeyCode::Esc), Cmd::None);

        assert_eq!(state.modal, None);
    }
}
