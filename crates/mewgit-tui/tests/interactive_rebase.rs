//! interactive rebase を本物の `mewgit` を sequence editor にして通しで流す。
//!
//! core のテストからは `mewgit` のバイナリを引けない（`CARGO_BIN_EXE_*` は同じ
//! パッケージの統合テストにしか渡らない）ので、ここに置く。設計は
//! `docs/design/03-git-exec.md` の「Interactive rebase の実装」。

use mewgit_core::git_exec::{ExecError, ExecErrorKind, GitOp, RebaseAction, RebasePlan, Runner};
use mewgit_core::git_read::Oid;
use mewgit_core::test_util::{rebase_entry as entry, reword, TestRepo};

fn runner(repo: &TestRepo) -> Runner {
    repo.runner_with_mewgit(env!("CARGO_BIN_EXE_mewgit"))
}

/// `onto..HEAD` の完全なメッセージ。新しい順。
fn messages(repo: &TestRepo, onto: &str) -> Vec<String> {
    repo.git(&["log", "--format=%B%x00", &format!("{onto}..HEAD")])
        .split('\0')
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .collect()
}

#[tokio::test]
async fn the_plan_replaces_the_todo() {
    let repo = TestRepo::init();
    repo.commit_file("base.txt", "base");
    let a = repo.commit_file("a.txt", "a");
    let b = repo.commit_file("b.txt", "b");
    let c = repo.commit_file("c.txt", "c");
    let d = repo.commit_file("d.txt", "d");

    runner(&repo)
        .run(&GitOp::RebaseInteractive {
            onto: "HEAD~4".into(),
            head: d,
            plan: RebasePlan {
                entries: vec![
                    entry(RebaseAction::Pick, a, "a"),
                    entry(reword("B new\n\n#123 is closed"), b, "b"),
                    entry(RebaseAction::Fixup, c, "c"),
                    entry(RebaseAction::Drop, d, "d"),
                ],
            },
        })
        .await
        .expect("rebase は最後まで通るはずです");

    // fixup は reword の exec より後ろに並ぶが、メッセージは reword のもののまま
    // （fixup はメッセージを捨てる）。
    assert_eq!(messages(&repo, "HEAD~2"), ["B new\n\n#123 is closed", "a"]);
    assert!(repo.path().join("c.txt").exists(), "fixup の変更が落ちた");
    assert!(!repo.path().join("d.txt").exists(), "drop が効いていない");
    assert!(!repo.path().join(".git/rebase-merge").exists());
}

/// `diverged` の side に居て、side の 2 本を reword する rebase を始める。1 本目が
/// main とコンフリクトして止まる。
async fn start_rewording_both(repo: &TestRepo, runner: &Runner, first: Oid, second: Oid) {
    repo.git(&["switch", "-q", "side"]);
    let err = runner
        .run(&GitOp::RebaseInteractive {
            onto: "main".into(),
            head: second,
            plan: RebasePlan {
                entries: vec![
                    entry(reword("side new"), first, "side"),
                    entry(reword("#42 only"), second, "side2"),
                ],
            },
        })
        .await
        .expect_err("side の 1 つ目が main とコンフリクトするはずです");
    assert!(
        matches!(
            err,
            ExecError::Failed {
                kind: ExecErrorKind::Conflict,
                ..
            }
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn rewords_survive_continue() {
    // **行頭の `#` も残る**（`core.commentString` を continue にも載せている。03）。
    // continue は `RebaseInteractive` とは別の `GitOp` なので、`GIT_EDITOR` を
    // rebase の開始にだけ載せると、止まった reword も後ろの reword も元のメッセージの
    // まま通る（03）。
    let (repo, first, second) = TestRepo::diverged();
    let runner = runner(&repo);
    start_rewording_both(&repo, &runner, first, second).await;

    repo.write_worktree_file("f", b"resolved\n");
    repo.git(&["add", "f"]);
    runner
        .run(&GitOp::RebaseContinue)
        .await
        .expect("解消したので最後まで通るはずです");

    assert_eq!(messages(&repo, "main"), ["#42 only", "side new"]);
    assert!(!repo.path().join(".git/rebase-merge").exists());
}

#[tokio::test]
async fn skipping_a_stopped_reword_leaves_other_commits_alone() {
    // reword を `pick` + `exec git commit --amend` で渡す形だと、pick を skip しても
    // exec が走り、**HEAD（ここでは main の先端）のメッセージを書き換える**
    // （`/code-review` が拾った。03）。
    let (repo, first, second) = TestRepo::diverged();
    let runner = runner(&repo);
    start_rewording_both(&repo, &runner, first, second).await;

    runner
        .run(&GitOp::RebaseSkip)
        .await
        .expect("skip で最後まで通るはずです");

    assert_eq!(messages(&repo, "main~1"), ["#42 only", "main"]);
    assert!(!repo.path().join(".git/rebase-merge").exists());
}

#[tokio::test]
async fn a_head_moved_after_the_plan_is_refused_before_the_rebase_starts() {
    // 画面で plan を組んだあとに、端末で 1 本積んだ。通すと、plan に載らなかったそのコミットを
    // git が黙って落とす（03）。
    let repo = TestRepo::init();
    repo.commit_file("base.txt", "base");
    let a = repo.commit_file("a.txt", "a");
    let moved = repo.commit_file("late.txt", "late");

    let err = runner(&repo)
        .run(&GitOp::RebaseInteractive {
            onto: "HEAD~2".into(),
            head: a,
            plan: RebasePlan {
                entries: vec![entry(RebaseAction::Drop, a, "a")],
            },
        })
        .await
        .expect_err("HEAD が動いたので断るはずです");

    // **止まった理由まで見る** —— 印が stderr に届いて分類されたこと。
    let ExecError::Failed { kind, stderr, .. } = &err else {
        panic!("git が失敗として返っていません: {err:?}");
    };
    assert_eq!(*kind, ExecErrorKind::HeadMoved, "{stderr}");
    // git は rebase を始めない（進行中操作が残らない）。HEAD も積んだ先のまま。
    assert!(!repo.path().join(".git/rebase-merge").exists());
    assert_eq!(repo.head_oid(), moved);
}
