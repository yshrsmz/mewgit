//! 進行中操作（rebase / cherry-pick / revert / merge / am の途中）があるときの書き込みと、
//! そこから出る手（`m` のメニュー）。
//!
//! どの `GitOp` を断るかは 03 の「進行中に通す書き込み」（実測の表）、いつ・どこで
//! 断るかは 01 の「進行中操作があるときの書き込み」、メニューは同じ節の
//! 「進行中操作のメニュー（`m`）」。

use crate::git_exec::GitOp;
use crate::git_read::InProgressOp;

use super::{confirm, submit_exec};
use crate::app::{AppState, Cmd, Modal, NoticeLevel};

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
    // 改名・削除は下で通すが、**rebase しているブランチだけは git が断る**（`-m` は
    // `is being rebased`、`-D` は `used by worktree`。03 の表）。`r` は入力を開く前に
    // ここで断られるので、打った名前が消えない（01）。
    if let GitOp::BranchDelete { name: target, .. } | GitOp::BranchRename { from: target, .. } = op
    {
        if matches!(progress, InProgressOp::Rebase { branch: Some(b), .. } if b == target) {
            return Some(REBASING_BRANCH.to_string());
        }
    }
    let passes = match op {
        GitOp::Stage(_)
        | GitOp::Unstage(_)
        | GitOp::StageHunk(_)
        | GitOp::UnstageHunk(_)
        | GitOp::DiscardHunk(_)
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
        GitOp::RebaseContinue { .. } | GitOp::RebaseSkip | GitOp::RebaseAbort => {
            matches!(progress, InProgressOp::Rebase { .. })
        }
        GitOp::CherryPickContinue | GitOp::CherryPickSkip | GitOp::CherryPickAbort => {
            matches!(progress, InProgressOp::CherryPick { .. })
        }
        GitOp::RevertContinue | GitOp::RevertSkip | GitOp::RevertAbort => {
            matches!(progress, InProgressOp::Revert { .. })
        }
        // `rebase -r` の merge で止まった形は `Rebase` として検出する（02）ので、
        // そこでは断って rebase の abort を案内する。
        GitOp::MergeAbort => matches!(progress, InProgressOp::Merge { .. }),
    };
    (!passes).then(|| advice(progress))
}

/// rebase しているブランチを改名・削除しようとしたときの断り文句。
const REBASING_BRANCH: &str = "rebase 中は改名・削除できません。m から終えて";

/// 何の途中か（git のサブコマンドの字）。
fn verb(progress: &InProgressOp) -> &'static str {
    match progress {
        InProgressOp::Rebase { .. } => "rebase",
        InProgressOp::Am { .. } => "am",
        InProgressOp::CherryPick { .. } => "cherry-pick",
        InProgressOp::Revert { .. } => "revert",
        InProgressOp::Merge { .. } => "merge",
    }
}

/// 何の途中かと、そこから出る手。**必ずバッジと並ぶ**ので、71 セルではなくバッジの隣の
/// 幅に収める（01 の「失敗の案内文」）。
///
/// **`m` を案内するのはここだけ**（フッターには出さない。01 の「進行中操作の
/// メニュー（`m`）」）。
fn advice(progress: &InProgressOp) -> String {
    match progress {
        InProgressOp::Rebase { .. }
        | InProgressOp::CherryPick { .. }
        | InProgressOp::Revert { .. } => {
            format!("{} の途中。m から continue か abort", verb(progress))
        }
        // merge の continue に当たる `GitOp` は無い。結ぶのは commit（03 の表）。
        InProgressOp::Merge { .. } => "merge の途中。commit で結ぶか m から abort".to_string(),
        // mewgit から am は始めず、`GitOp` にも無い（02 の「進行中操作の検出」）。
        InProgressOp::Am { .. } => "am の途中。端末で git am --continue か --abort".to_string(),
    }
}

/// `m` のメニュー（[`Modal::ProgressMenu`]）の中身。開くときに 1 回だけ組む。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressMenu {
    /// 何の途中か（枠のタイトル）。
    pub title: String,
    /// 一覧の上に出す 1 行。**merge だけ違う** —— continue に当たる項目が無いので、
    /// 結び方を言わないと出口が abort しか無く読める（07 の「ProgressMenu モーダル」）。
    pub body: &'static str,
    pub items: Vec<MenuItem>,
}

/// メニューの 1 項目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    /// 表示する語（`continue` / `skip` / `abort`）。git のオプション名に揃える
    /// （端末で打つときと読み替えなくて済む）。
    pub word: &'static str,
    /// 何が起きるか。
    pub about: &'static str,
    pub op: GitOp,
}

/// `progress` の途中で打てる手。**am では `None`**（mewgit から打てる手が無い）。
///
/// 網羅 `match` にしているのは、`InProgressOp` に変種を足したときに何を並べるかを
/// 選ばせるため。
pub fn progress_menu(progress: &InProgressOp) -> Option<ProgressMenu> {
    let item = |word, about, op| MenuItem { word, about, op };
    const RESOLVE: &str = "コンフリクトは解消して git add してから continue してください";
    let (body, items) = match progress {
        InProgressOp::Rebase { refused_reword, .. } => {
            // **hook が reword を断って止まった形は、同じ語で中身が違う**（01 の「進行中操作の
            // メニュー（`m`）」）。continue は新しいメッセージを amend してから進め、skip は
            // コミット済みの pick を捨てない（03 の表）。
            let (body, resume, skip) = match refused_reword {
                Some(_) => (
                    "新しいメッセージが断られ、元のメッセージで止まっています",
                    "新しいメッセージを当て直して続ける",
                    "元のメッセージのまま続ける",
                ),
                None => (
                    RESOLVE,
                    "解消した内容で続ける",
                    "止まったコミットを捨てて続ける",
                ),
            };
            let reword = refused_reword.clone();
            (
                body,
                vec![
                    item("continue", resume, GitOp::RebaseContinue { reword }),
                    item("skip", skip, GitOp::RebaseSkip),
                    item("abort", "rebase を始める前に戻す", GitOp::RebaseAbort),
                ],
            )
        }
        InProgressOp::CherryPick { .. } => (
            RESOLVE,
            vec![
                item(
                    "continue",
                    "解消した内容で続ける",
                    GitOp::CherryPickContinue,
                ),
                // **変更が空で止まった pick から出る手**（continue は同じ理由で断られる。
                // 03 の「continue / skip / abort の終わり方」）。
                item(
                    "skip",
                    "止まったコミットを捨てて続ける",
                    GitOp::CherryPickSkip,
                ),
                item(
                    "abort",
                    "cherry-pick を始める前に戻す",
                    GitOp::CherryPickAbort,
                ),
            ],
        ),
        InProgressOp::Revert { .. } => (
            RESOLVE,
            vec![
                item("continue", "解消した内容で続ける", GitOp::RevertContinue),
                // **解消した結果が空で continue が断られた形から出る手**（03 の
                // 「revert の開始」）。
                item("skip", "止まったコミットを捨てて続ける", GitOp::RevertSkip),
                item("abort", "revert を始める前に戻す", GitOp::RevertAbort),
            ],
        ),
        InProgressOp::Merge { .. } => (
            "結ぶときは commit してください",
            vec![item("abort", "merge を始める前に戻す", GitOp::MergeAbort)],
        ),
        InProgressOp::Am { .. } => return None,
    };
    Some(ProgressMenu {
        title: format!("{} の途中", verb(progress)),
        body,
        items,
    })
}

/// 確認モーダルを挟むか（不変ルール 5）。**項目に持たせない** —— 持たせると
/// 「abort か」の判断が項目と `GitOp` の 2 つになる。
///
/// 網羅 `match` にしているのは、`GitOp` に変種を足したときに選ばせるため。`matches!` だと
/// abort 系を足してメニューに載せたときに黙って `false`（確認なしで実行）になる。
fn is_abort(op: &GitOp) -> bool {
    match op {
        GitOp::RebaseAbort | GitOp::CherryPickAbort | GitOp::RevertAbort | GitOp::MergeAbort => {
            true
        }
        GitOp::Stage(_)
        | GitOp::Unstage(_)
        | GitOp::StageHunk(_)
        | GitOp::UnstageHunk(_)
        | GitOp::DiscardHunk(_)
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
        | GitOp::RebaseContinue { .. }
        | GitOp::RebaseSkip
        | GitOp::CherryPick(_)
        | GitOp::CherryPickContinue
        | GitOp::CherryPickSkip
        | GitOp::Revert { .. }
        | GitOp::RevertContinue
        | GitOp::RevertSkip
        | GitOp::StashPush { .. }
        | GitOp::StashApply(_)
        | GitOp::StashPop(_)
        | GitOp::StashDrop(_)
        | GitOp::SetAccount { .. }
        | GitOp::UnsetAccount
        | GitOp::CheckoutPr(_) => false,
    }
}

/// `m`。進行中操作のメニューを開く（01 の「進行中操作のメニュー（`m`）」）。
///
/// **snapshot の鮮度は問わない** —— 終わっていれば git が断り、`exec_finished` の
/// `LoadStatus` がバッジを直す。**開けないときは読み直す** —— mewgit から始められるのは
/// cherry-pick だけで、端末で rebase などを始めた直後に押すのがいちばんよく踏む形で、snapshot は
/// まだ「無い」。1 回目は通知で終わり、2 回目は開く。
pub(super) fn open_progress_menu(state: &mut AppState) -> Cmd {
    let progress = state.worktree.snapshot.progress.as_ref();
    if let Some(menu) = progress.and_then(progress_menu) {
        state.modal = Some(Modal::ProgressMenu { menu, cursor: 0 });
        return Cmd::None;
    }
    // 無い / am。どちらも外で終えた・始めた直後かもしれないので読み直す。
    let message = progress.map_or_else(|| "進行中の操作は見つかりませんでした".to_string(), advice);
    state.push_notice(NoticeLevel::Info, message, None);
    Cmd::LoadStatus
}

/// メニューの確定（`Enter`）。abort は確認モーダルへ移り、ほかはそのまま git を走らせる。
pub(super) fn progress_picked(state: &mut AppState) -> Cmd {
    let picked = match &state.modal {
        Some(Modal::ProgressMenu { menu, cursor }) => {
            menu.items.get(*cursor).map(|item| item.op.clone())
        }
        _ => None,
    };
    let Some(op) = picked else {
        state.modal = None;
        return Cmd::None;
    };
    if is_abort(&op) {
        // abort の変種では `GitOp::verb` が進行中操作の語と同じ字になる。
        let verb = op.verb();
        let body = format!("{verb} を始める前の状態に戻します。解消した内容も捨てます");
        return confirm(state, &format!("{verb} を中止"), body, op);
    }
    // git が走っていれば閉じずに断る（1 行入力の確定と同じ）。
    submit_exec(state, op)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        cells, exec_done_of, fake_output, finish_exec, open_input, press, press_code,
        state_on_branches, state_with_commits, type_text, worktree_state, BESIDE_BADGE_CELLS,
    };
    use super::super::update;
    use super::*;
    use crate::app::{Cmd, KeyCode, Modal};
    use crate::git_exec::{LocalBranch, PrSource, PullMode};
    use crate::git_read::{RefusedReword, Step, WorktreeSection, WorktreeSnapshot};
    use crate::test_util::{
        deliver_status, fake_oid, fake_pr_checkout, fake_snapshot, status_arrived,
    };

    fn rebase() -> InProgressOp {
        InProgressOp::Rebase {
            step: Some(Step {
                current: 1,
                total: 2,
            }),
            branch: Some("topic".to_string()),
            refused_reword: None,
        }
    }

    fn cherry_pick() -> InProgressOp {
        InProgressOp::CherryPick {
            oid: Some(fake_oid(1)),
        }
    }

    /// 全進行中操作の値。値の足し忘れは `all_variants!` が名前を挙げて落とし、変種を
    /// 足して一覧に書き忘れると E0004 になる。
    fn every_progress() -> Vec<InProgressOp> {
        let progresses = vec![
            rebase(),
            InProgressOp::Am { step: None },
            cherry_pick(),
            InProgressOp::Revert { oid: None },
            InProgressOp::Merge { oid: fake_oid(1) },
        ];
        let missing = all_variants!(missing InProgressOp {
            Rebase,
            Am,
            CherryPick,
            Revert,
            Merge,
        } in &progresses);
        assert!(missing.is_empty(), "回していない進行中操作: {missing:?}");
        progresses
    }

    /// 進行中操作を載せた status を届ける（鮮度のある snapshot になる）。
    fn arrive(state: &mut AppState, progress: Option<InProgressOp>) {
        let _ = deliver_status(state, Ok(progressing(progress)));
    }

    /// 進行中操作を載せた status。キーが `Cmd::LoadStatus` を発行済みの経路では
    /// これを `update` に直に渡す。
    fn progressing(progress: Option<InProgressOp>) -> WorktreeSnapshot {
        let mut snapshot = fake_snapshot(&["a.txt"], &[], &[]);
        snapshot.progress = progress;
        snapshot
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
        assert_eq!(
            refusal(&rebase(), &GitOp::RebaseContinue { reword: None }),
            None
        );
        assert_eq!(refusal(&rebase(), &GitOp::RebaseSkip), None);
        assert_eq!(refusal(&cherry_pick(), &GitOp::CherryPickAbort), None);
        // cherry-pick の途中の `rebase --continue` は git が別の操作として扱う。
        assert!(refusal(&cherry_pick(), &GitOp::RebaseContinue { reword: None }).is_some());
        assert!(refusal(&rebase(), &GitOp::RevertAbort).is_some());
        assert!(refusal(&rebase(), &GitOp::CherryPickSkip).is_some());
        // am には mewgit から打てる出口が無い。
        assert!(refusal(
            &InProgressOp::Am { step: None },
            &GitOp::RebaseContinue { reword: None }
        )
        .is_some());
    }

    #[test]
    fn every_advice_fits_the_notice_line() {
        for progress in every_progress() {
            let message = advice(&progress);
            assert!(cells(&message) <= BESIDE_BADGE_CELLS, "{message}");
        }
        assert!(
            cells(REBASING_BRANCH) <= BESIDE_BADGE_CELLS,
            "{REBASING_BRANCH}"
        );
    }

    #[test]
    fn merge_abort_passes_only_in_the_middle_of_a_merge() {
        assert_eq!(
            refusal(
                &InProgressOp::Merge { oid: fake_oid(1) },
                &GitOp::MergeAbort
            ),
            None
        );
        // `rebase -r` の merge で止まった形は `Rebase` として検出する（02）。打つのは
        // rebase の abort で、`merge --abort` は rebase を置き去りにする。
        assert!(refusal(&rebase(), &GitOp::MergeAbort).is_some());
        assert!(refusal(&cherry_pick(), &GitOp::MergeAbort).is_some());
    }

    #[test]
    fn the_branch_being_rebased_cannot_be_renamed_or_deleted() {
        // git が断る（`is being rebased` / `used by worktree`。03 の表）。
        for op in [
            GitOp::BranchRename {
                from: "topic".to_string(),
                to: "x".to_string(),
            },
            GitOp::BranchDelete {
                name: "topic".to_string(),
                force: true,
            },
        ] {
            assert_eq!(
                refusal(&rebase(), &op).as_deref(),
                Some(REBASING_BRANCH),
                "{op:?}"
            );
        }
        // ほかのブランチは通る。**detached HEAD で始めた rebase にはブランチが無い**ので、
        // 同じ名前でも通る。
        let other = GitOp::BranchRename {
            from: "other".to_string(),
            to: "x".to_string(),
        };
        assert_eq!(refusal(&rebase(), &other), None);
        let detached = InProgressOp::Rebase {
            step: None,
            branch: None,
            refused_reword: None,
        };
        let topic = GitOp::BranchDelete {
            name: "topic".to_string(),
            force: false,
        };
        assert_eq!(refusal(&detached, &topic), None);
    }

    #[test]
    fn r_on_the_branch_being_rebased_is_refused_before_the_name_is_typed() {
        // #120 が残した穴。開いてから git に断られると、打った名前が消える（01）。
        let mut state = state_on_branches(&["topic"], Some("topic"));
        arrive(&mut state, Some(rebase()));

        let cmd = press(&mut state, 'r');

        assert_eq!(cmd, Cmd::LoadStatus);
        assert_eq!(state.modal, None);
        assert_eq!(shown(&state), REBASING_BRANCH);
    }

    #[test]
    fn deleting_the_branch_being_rebased_is_refused_when_confirmed() {
        let mut state = state_on_branches(&["topic"], Some("topic"));
        arrive(&mut state, Some(rebase()));
        let _ = press(&mut state, 'D');
        assert!(matches!(state.modal, Some(Modal::Confirm { .. })));

        let cmd = press(&mut state, 'y');

        assert_eq!(cmd, Cmd::LoadStatus);
        assert_eq!(shown(&state), REBASING_BRANCH);
    }

    #[test]
    fn y_in_the_middle_of_a_rebase_is_refused_when_confirmed() {
        // 手前では見ず、確定のあとの `AppState::issue` で断る（ブランチの削除と同じ。
        // 01 の「cherry-pick の開始（`y`）」）。
        let mut state = state_with_commits(3);
        arrive(&mut state, Some(rebase()));
        let _ = press(&mut state, 'j');
        let _ = press(&mut state, 'y');
        assert!(matches!(state.modal, Some(Modal::Confirm { .. })));

        let cmd = press(&mut state, 'y');

        assert_eq!(cmd, Cmd::LoadStatus);
        assert_eq!(shown(&state), advice(&rebase()));
    }

    /// 開いているメニューの項目の `GitOp`。
    fn menu_ops(state: &AppState) -> Vec<GitOp> {
        let Some(Modal::ProgressMenu { menu, .. }) = &state.modal else {
            panic!("メニューが開いていません: {:?}", state.modal);
        };
        menu.items.iter().map(|item| item.op.clone()).collect()
    }

    #[test]
    fn a_refused_reword_changes_what_continue_and_skip_do() {
        // 同じ語で中身が違う（01 の「進行中操作のメニュー（`m`）」）。continue は amend して
        // から進め、skip はコミット済みの pick を捨てないので説明を変える（03 の表）。
        let reword = RefusedReword { head: fake_oid(7) };
        let refused = InProgressOp::Rebase {
            step: None,
            branch: None,
            refused_reword: Some(reword.clone()),
        };
        let menu = progress_menu(&refused).expect("メニューがありません");
        let ops: Vec<GitOp> = menu.items.iter().map(|item| item.op.clone()).collect();
        assert_eq!(
            ops,
            [
                GitOp::RebaseContinue {
                    reword: Some(reword)
                },
                GitOp::RebaseSkip,
                GitOp::RebaseAbort,
            ]
        );
        let abouts: Vec<&str> = menu.items.iter().map(|item| item.about).collect();
        assert!(!abouts[1].contains("捨て"), "{abouts:?}");
        assert!(!menu.body.contains("コンフリクト"), "{}", menu.body);

        // 素の rebase はいつもの説明のまま。
        let plain = progress_menu(&rebase()).expect("メニューがありません");
        assert!(plain.items[1].about.contains("捨て"));
    }

    #[test]
    fn m_lists_the_ways_out_of_what_is_in_progress() {
        let cases = [
            (
                rebase(),
                vec![
                    GitOp::RebaseContinue { reword: None },
                    GitOp::RebaseSkip,
                    GitOp::RebaseAbort,
                ],
            ),
            (
                cherry_pick(),
                vec![
                    GitOp::CherryPickContinue,
                    GitOp::CherryPickSkip,
                    GitOp::CherryPickAbort,
                ],
            ),
            (
                InProgressOp::Revert { oid: None },
                vec![GitOp::RevertContinue, GitOp::RevertSkip, GitOp::RevertAbort],
            ),
            // merge の continue に当たる `GitOp` は無い（結ぶのは commit）。
            (
                InProgressOp::Merge { oid: fake_oid(1) },
                vec![GitOp::MergeAbort],
            ),
        ];
        for (progress, expected) in cases {
            let mut state = state_on_branches(&["main"], Some("main"));
            arrive(&mut state, Some(progress.clone()));

            let cmd = press(&mut state, 'm');

            assert_eq!(cmd, Cmd::None, "{progress:?}");
            assert_eq!(menu_ops(&state), expected, "{progress:?}");
            // どの項目も、その途中で `refusal` に通される（種類の合わない手を並べない）。
            for op in &expected {
                assert_eq!(refusal(&progress, op), None, "{progress:?} {op:?}");
            }
        }
    }

    #[test]
    fn m_is_a_common_key() {
        // 共通キー。解消の途中でどのペインに居るかは決まっていない（01）。
        let mut snapshot = fake_snapshot(&["a.txt"], &[], &[]);
        snapshot.progress = Some(rebase());
        let mut state = worktree_state(snapshot, WorktreeSection::Staged);

        let _ = press(&mut state, 'm');

        assert!(matches!(state.modal, Some(Modal::ProgressMenu { .. })));
    }

    #[test]
    fn m_with_nothing_in_progress_reads_the_status_again() {
        // 端末で rebase を始めた直後は snapshot がまだ「無い」。読み直すので 2 回目は開く。
        let mut state = state_on_branches(&["main"], Some("main"));
        arrive(&mut state, None);

        let cmd = press(&mut state, 'm');

        assert_eq!(cmd, Cmd::LoadStatus);
        assert_eq!(state.modal, None);
        assert_eq!(shown(&state), "進行中の操作は見つかりませんでした");

        let _ = update(&mut state, status_arrived(Ok(progressing(Some(rebase())))));
        let _ = press(&mut state, 'm');

        assert_eq!(menu_ops(&state)[0], GitOp::RebaseContinue { reword: None });
    }

    #[test]
    fn m_in_the_middle_of_an_am_points_to_the_terminal() {
        let mut state = state_on_branches(&["main"], Some("main"));
        arrive(&mut state, Some(InProgressOp::Am { step: None }));

        let cmd = press(&mut state, 'm');

        assert_eq!(cmd, Cmd::LoadStatus);
        assert_eq!(state.modal, None);
        assert!(shown(&state).contains("git am --continue"));
    }

    #[test]
    fn enter_on_continue_runs_it_and_closes_the_menu() {
        let mut state = state_on_branches(&["main"], Some("main"));
        arrive(&mut state, Some(rebase()));
        let _ = press(&mut state, 'm');

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::Exec(GitOp::RebaseContinue { reword: None }));
        assert_eq!(state.modal, None);
    }

    #[test]
    fn j_moves_to_skip_and_stops_at_the_last_item() {
        let mut state = state_on_branches(&["main"], Some("main"));
        arrive(&mut state, Some(rebase()));
        let _ = press(&mut state, 'm');
        let _ = press(&mut state, 'j');
        assert_eq!(
            press_code(&mut state, KeyCode::Enter),
            Cmd::Exec(GitOp::RebaseSkip)
        );

        let _ = press(&mut state, 'm');
        for _ in 0..5 {
            let _ = press(&mut state, 'j');
        }
        let _ = press_code(&mut state, KeyCode::Enter);

        // 末尾で止まる（`stepped_index`）。abort なので確認に移る。
        assert!(matches!(state.modal, Some(Modal::Confirm { .. })));
    }

    #[test]
    fn only_the_abort_items_ask_before_running() {
        // **語と門番を突き合わせる。** 「abort」と見せた項目が確認を挟まずに走ると
        // 不変ルール 5 を破り、ほかの項目が確認を挟むと skip まで止まる。`is_abort` は
        // `GitOp` から導くので、メニューに載せた項目と食い違ってもコンパイラは何も言わない。
        for progress in &every_progress() {
            let Some(menu) = progress_menu(progress) else {
                continue;
            };
            for item in &menu.items {
                assert_eq!(
                    is_abort(&item.op),
                    item.word == "abort",
                    "{progress:?} の {}",
                    item.word
                );
            }
        }
    }

    #[test]
    fn abort_asks_before_running() {
        // 破壊的操作は `Modal::Confirm` を通す（不変ルール 5）。
        let mut state = state_on_branches(&["main"], Some("main"));
        arrive(&mut state, Some(InProgressOp::Merge { oid: fake_oid(1) }));
        let _ = press(&mut state, 'm');

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::None);
        let Some(Modal::Confirm { title, on_yes, .. }) = &state.modal else {
            panic!("確認が開いていません: {:?}", state.modal);
        };
        assert_eq!(title, "merge を中止");
        assert_eq!(on_yes, &Cmd::Exec(GitOp::MergeAbort));
        assert_eq!(press(&mut state, 'y'), Cmd::Exec(GitOp::MergeAbort));
    }

    #[test]
    fn a_busy_git_keeps_the_menu_open() {
        // 1 行入力の確定と同じ（`submit_exec`）。閉じてから落とすと、押した手が
        // 何も起こさずにメニューだけ消える。
        let mut state = state_on_branches(&["main"], Some("main"));
        arrive(&mut state, Some(rebase()));
        let _ = state.issue(Cmd::Exec(GitOp::Fetch {
            remote: None,
            prune: false,
        }));
        let _ = press(&mut state, 'm');

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_ne!(cmd, Cmd::Exec(GitOp::RebaseContinue { reword: None }));
        assert!(matches!(state.modal, Some(Modal::ProgressMenu { .. })));
    }

    #[test]
    fn esc_closes_the_menu_without_running_anything() {
        let mut state = state_on_branches(&["main"], Some("main"));
        arrive(&mut state, Some(rebase()));
        let _ = press(&mut state, 'm');

        let cmd = press_code(&mut state, KeyCode::Esc);

        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.modal, None);
    }

    #[test]
    fn a_rebase_finished_outside_is_noticed_on_the_next_press() {
        // 世代を進めるのは mewgit の書き込みだけなので、端末で `rebase --abort` しても
        // snapshot は「鮮度あり・途中」のまま。断ったときに読み直さないと、git を
        // 起動しないぶん断り続ける（`/code-review` が拾った。pty で再現した）。
        let mut state = state_on_branches(&["another", "main"], Some("main"));
        arrive(&mut state, Some(rebase()));
        let _ = press(&mut state, 'k');
        assert_eq!(press_code(&mut state, KeyCode::Enter), Cmd::LoadStatus);

        // 読み直した status には進行中操作が無い（外で終えた）。
        let _ = update(&mut state, status_arrived(Ok(progressing(None))));
        // 鮮度のある snapshot で「無い」と読めている（古くて git に任せた形ではない）。
        assert!(state.worktree_is_current());

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::Exec(GitOp::Switch("another".to_string())));
    }

    #[test]
    fn refusing_while_git_runs_still_catches_up_with_the_write() {
        // 走っている git のあいだに断ると、読み直しは書き込みより前の世代で刻まれ、
        // 書き込みのあとの `LoadStatus` は同種実行中で落ちる。届いた古い返りから
        // `status_loaded` が頼み直す（01 の「進行中操作があるときの書き込み」）。
        let mut state = state_on_branches(&["another", "main"], Some("main"));
        arrive(&mut state, Some(rebase()));
        let fetch = GitOp::Fetch {
            remote: None,
            prune: false,
        };
        let _ = state.issue(Cmd::Exec(fetch.clone()));
        let _ = press(&mut state, 'k');
        assert_eq!(press_code(&mut state, KeyCode::Enter), Cmd::LoadStatus);

        let after = update(&mut state, exec_done_of(fetch, Ok(fake_output())));
        assert!(
            !format!("{after:?}").contains("LoadStatus"),
            "書き込みのあとの読み直しは落ちるはず: {after:?}"
        );
        let stale = fake_snapshot(&["a.txt"], &[], &[]);
        let again = update(&mut state, status_arrived(Ok(stale)));

        assert!(format!("{again:?}").contains("LoadStatus"), "{again:?}");
        assert!(!state.worktree_is_current());
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
        let mut state = state_on_branches(&["another", "main"], Some("main"));
        arrive(&mut state, Some(rebase()));
        assert!(state.refuse_in_progress(&GitOp::Switch("another".to_string())));
        finish_exec(&mut state, GitOp::RebaseAbort, Ok(fake_output()));
        assert!(!state.worktree_is_current());
        let _ = press(&mut state, 'k');

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::Exec(GitOp::Switch("another".to_string())));
    }

    #[test]
    fn n_is_refused_before_the_name_is_typed() {
        // 打ち終えてから断ると、打った名前が消える（01）。
        let mut state = state_on_branches(&["main"], Some("main"));
        arrive(&mut state, Some(rebase()));

        let cmd = press(&mut state, 'n');

        assert_eq!(cmd, Cmd::LoadStatus);
        assert_eq!(state.modal, None);
        assert!(shown(&state).starts_with("rebase の途中"));
    }

    #[test]
    fn r_still_opens_because_renaming_passes() {
        let mut state = state_on_branches(&["main"], Some("main"));
        arrive(&mut state, Some(rebase()));

        let _ = press(&mut state, 'r');

        assert!(matches!(state.modal, Some(Modal::Input { .. })));
    }

    #[test]
    fn submitting_a_name_after_a_rebase_started_keeps_the_input_open() {
        // 打っているあいだに status が届いて rebase の途中と判った。閉じると打った名前が
        // 消えるので、走っている git で断るときと同じく閉じずに断る。
        let mut state = state_on_branches(&["main"], Some("main"));
        let _ = press(&mut state, 'n');
        type_text(&mut state, "topic");
        arrive(&mut state, Some(rebase()));

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::LoadStatus);
        let (_, _, buf, _) = open_input(&state);
        assert_eq!(buf.text(), "topic");
        assert!(shown(&state).starts_with("rebase の途中"));
    }
}
