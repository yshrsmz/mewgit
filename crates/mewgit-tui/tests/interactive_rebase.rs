//! interactive rebase を本物の `mewgit` を sequence editor にして通しで流す。
//!
//! core のテストからは `mewgit` のバイナリを引けない（`CARGO_BIN_EXE_*` は同じ
//! パッケージの統合テストにしか渡らない）ので、ここに置く。設計は
//! `docs/design/03-git-exec.md` の「Interactive rebase の実装」。

use mewgit_core::git_exec::{ExecError, ExecErrorKind, GitOp, RebaseAction, RebasePlan, Runner};
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
            plan: RebasePlan {
                entries: vec![
                    entry(RebaseAction::Pick, a, "a"),
                    entry(reword("B new\n\nbody"), b, "b"),
                    entry(RebaseAction::Fixup, c, "c"),
                    entry(RebaseAction::Drop, d, "d"),
                ],
            },
        })
        .await
        .expect("rebase は最後まで通るはずです");

    // fixup は reword の exec より後ろに並ぶが、メッセージは reword のもののまま
    // （fixup はメッセージを捨てる）。
    assert_eq!(messages(&repo, "HEAD~2"), ["B new\n\nbody", "a"]);
    assert!(repo.path().join("c.txt").exists(), "fixup の変更が落ちた");
    assert!(!repo.path().join("d.txt").exists(), "drop が効いていない");
    assert!(!repo.path().join(".git/rebase-merge").exists());
}

#[tokio::test]
async fn a_reword_after_a_conflict_survives_continue() {
    // **`exec` の行で渡す理由そのもの。** continue は `GIT_EDITOR=true` の通常の
    // 経路で走るので、エディタで渡す形だと停止点より後ろの reword は元のメッセージの
    // まま通る（03）。
    let (repo, first, second) = TestRepo::diverged();
    repo.git(&["switch", "-q", "side"]);
    let runner = runner(&repo);

    let err = runner
        .run(&GitOp::RebaseInteractive {
            onto: "main".into(),
            plan: RebasePlan {
                entries: vec![
                    entry(RebaseAction::Pick, first, "side"),
                    entry(reword("side2 new"), second, "side2"),
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

    repo.write_worktree_file("f", b"resolved\n");
    repo.git(&["add", "f"]);
    runner
        .run(&GitOp::RebaseContinue)
        .await
        .expect("解消したので最後まで通るはずです");

    assert_eq!(messages(&repo, "main"), ["side2 new", "side"]);
    assert!(!repo.path().join(".git/rebase-merge").exists());
}
