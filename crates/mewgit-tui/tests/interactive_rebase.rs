//! interactive rebase を本物の `mewgit` を sequence editor にして通しで流す。
//!
//! core のテストからは `mewgit` のバイナリを引けない（`CARGO_BIN_EXE_*` は同じ
//! パッケージの統合テストにしか渡らない）ので、ここに置く。設計は
//! `docs/design/03-git-exec.md` の「Interactive rebase の実装」。

use mewgit_core::git_exec::{ExecError, ExecErrorKind, GitOp, RebaseAction, RebasePlan, Runner};
use mewgit_core::git_read::{InProgressOp, Oid, RefusedReword, RepoReader};
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

#[tokio::test]
async fn comment_lines_of_existing_messages_survive_squash_and_a_conflicted_continue() {
    // **reword 以外でも git はメッセージを cleanup にかける**（03 の「git の最低版」）。
    // squash でつなぐメッセージと、コンフリクトで止まった cherry-pick を continue した
    // ときの元のメッセージ。どちらも `core.commentChar` が効かないと `#` の行が消える。
    let repo = TestRepo::init();
    repo.commit_file("base.txt", "base");
    let a = repo.commit_bytes("a.txt", b"a\n", "a\n\n#1 kept");
    let b = repo.commit_bytes("b.txt", b"b\n", "b\n\n#2 kept");
    let runner = runner(&repo);
    runner
        .run(&GitOp::RebaseInteractive {
            onto: "HEAD~2".into(),
            head: b,
            plan: RebasePlan {
                entries: vec![
                    entry(RebaseAction::Pick, a, "a"),
                    entry(RebaseAction::Squash, b, "b"),
                ],
            },
        })
        .await
        .expect("squash は最後まで通るはずです");
    assert_eq!(messages(&repo, "HEAD~1"), ["a\n\n#1 kept\n\nb\n\n#2 kept"]);

    repo.git(&["switch", "-q", "-c", "side", "HEAD~1"]);
    let side = repo.commit_bytes("f", b"side\n", "side\n\n#7 kept");
    repo.git(&["switch", "-q", "main"]);
    repo.commit_bytes("f", b"main\n", "main");
    runner
        .run(&GitOp::CherryPick(vec![side]))
        .await
        .expect_err("f がコンフリクトするはずです");
    repo.write_worktree_file("f", b"resolved\n");
    repo.git(&["add", "f"]);
    runner
        .run(&GitOp::CherryPickContinue)
        .await
        .expect("解消したので通るはずです");
    assert_eq!(messages(&repo, "HEAD~1"), ["side\n\n#7 kept"]);
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
    // **行頭の `#` も残る**（`core.commentChar` を continue にも載せている。03）。
    // continue は `RebaseInteractive` とは別の `GitOp` なので、`GIT_EDITOR` を
    // rebase の開始にだけ載せると、止まった reword も後ろの reword も元のメッセージの
    // まま通る（03）。
    let (repo, first, second) = TestRepo::diverged();
    let runner = runner(&repo);
    start_rewording_both(&repo, &runner, first, second).await;

    repo.write_worktree_file("f", b"resolved\n");
    repo.git(&["add", "f"]);
    runner
        .run(&GitOp::RebaseContinue { reword: None })
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

// ---- hook が reword を断ったあとの continue（03 の「Interactive rebase の実装」、
// 02 の「断られた reword」）。

/// `status()` が見分けた断られた reword。
fn refused_reword(repo: &TestRepo) -> Option<RefusedReword> {
    match RepoReader::open(repo.path())
        .unwrap()
        .status()
        .unwrap()
        .progress
    {
        Some(InProgressOp::Rebase { refused_reword, .. }) => refused_reword,
        other => panic!("rebase の途中ではありません: {other:?}"),
    }
}

const NEW_MESSAGE: &str = "a new\n\n#1 kept";

/// `a` / `b` の 2 本を積み、`a` を reword する rebase を始めて、hook が `new` を断って
/// 止まった形にする。`fast_forward` なら `onto` は元の親のまま（pick が早送りする形）、
/// そうでなければ `main` に 1 本積んだ先へ載せ直す形。返すのは `onto`。
async fn stop_on_a_refused_reword(repo: &TestRepo, runner: &Runner, fast_forward: bool) -> String {
    repo.commit_file("base.txt", "base");
    repo.git(&["switch", "-q", "-c", "side"]);
    let a = repo.commit_file("a.txt", "a");
    let b = repo.commit_file("b.txt", "b");
    let onto = if fast_forward {
        "side~2".to_string()
    } else {
        repo.git(&["switch", "-q", "main"]);
        repo.commit_file("z.txt", "main");
        repo.git(&["switch", "-q", "side"]);
        "main".to_string()
    };
    let onto = repo.rev_parse(&onto).to_string();
    repo.refuse_messages_containing("new");

    let err = runner
        .run(&GitOp::RebaseInteractive {
            onto: onto.clone(),
            head: b,
            plan: RebasePlan {
                entries: vec![
                    entry(reword(NEW_MESSAGE), a, "a"),
                    entry(RebaseAction::Pick, b, "b"),
                ],
            },
        })
        .await
        .expect_err("hook が reword を断るはずです");
    let ExecError::Failed { stderr, .. } = &err else {
        panic!("git が失敗として返っていません: {err:?}");
    };
    assert!(stderr.contains("hook says no"), "{stderr}");
    // **止まった理由まで見る**（`.claude/rules/verifying-assumptions.md`）—— pick は元の
    // メッセージでコミット済み。コンフリクトで止まった形ではない。
    assert_eq!(messages(repo, &onto), ["a"]);
    assert!(!repo.path().join(".git/MERGE_MSG").exists());
    onto
}

#[tokio::test]
async fn a_reword_refused_by_a_hook_is_amended_before_continuing() {
    // 早送りする形と載せ直す形の両方。`rebase-merge/amend` は早送りの形にしか立たない
    // ので、見分けがそれに頼っていると片方で落ちる（03 の実測）。
    for fast_forward in [true, false] {
        let repo = TestRepo::init();
        let runner = runner(&repo);
        let onto = stop_on_a_refused_reword(&repo, &runner, fast_forward).await;
        assert_eq!(
            repo.path().join(".git/rebase-merge/amend").exists(),
            fast_forward
        );

        let reword = refused_reword(&repo).expect("断られた reword を見分けていません");
        assert_eq!(reword.head, repo.head_oid());

        repo.allow_every_message();
        // 素の continue では git がエディタを起こし直さず、元のメッセージのまま進む。
        runner
            .run(&GitOp::RebaseContinue {
                reword: Some(reword),
            })
            .await
            .expect("hook を外したので最後まで通るはずです");

        // **行頭の `#` も残る**（amend も commit editor を通るので、`core.commentChar` が効く）。
        assert_eq!(
            messages(&repo, &onto),
            ["b", NEW_MESSAGE],
            "早送り: {fast_forward}"
        );
        assert!(!repo.path().join(".git/rebase-merge").exists());
    }
}

#[tokio::test]
async fn a_reword_refused_again_leaves_the_rebase_stopped_as_it_was() {
    let repo = TestRepo::init();
    let runner = runner(&repo);
    let onto = stop_on_a_refused_reword(&repo, &runner, false).await;
    let reword = refused_reword(&repo).expect("断られた reword を見分けていません");

    let err = runner
        .run(&GitOp::RebaseContinue {
            reword: Some(reword.clone()),
        })
        .await
        .expect_err("hook はまだ断るはずです");
    let ExecError::Failed { argv, stderr, .. } = &err else {
        panic!("git が失敗として返っていません: {err:?}");
    };
    // 止まったのは amend で、continue は走っていない。hook の理由が通知に届く。
    assert_eq!(argv[..2], ["commit", "--amend"], "{argv:?}");
    assert!(stderr.contains("hook says no"), "{stderr}");
    assert_eq!(messages(&repo, &onto), ["a"]);
    // 同じ形のままなので、hook を直して押し直せる。
    assert_eq!(refused_reword(&repo), Some(reword));
}

#[tokio::test]
async fn a_head_moved_after_the_refusal_is_not_amended() {
    // 見分けたあとに端末で別のメッセージへ amend し直した。古い snapshot のまま押すと、
    // 照合が無ければそのコミットを reword のメッセージで書き換える。
    let repo = TestRepo::init();
    let runner = runner(&repo);
    let onto = stop_on_a_refused_reword(&repo, &runner, false).await;
    let stale = refused_reword(&repo).expect("断られた reword を見分けていません");
    repo.git(&["commit", "-q", "--amend", "-m", "by hand"]);

    let err = runner
        .run(&GitOp::RebaseContinue {
            reword: Some(stale),
        })
        .await
        .expect_err("HEAD が動いたので断るはずです");
    let ExecError::Failed { kind, stderr, .. } = &err else {
        panic!("git が失敗として返っていません: {err:?}");
    };
    assert_eq!(*kind, ExecErrorKind::HeadMoved, "{stderr}");
    assert_eq!(messages(&repo, &onto), ["by hand"]);
    // 読み直すと素の continue に戻る（HEAD のメッセージが元のものと違う）。
    assert_eq!(refused_reword(&repo), None);
}

#[tokio::test]
async fn a_reword_stopped_by_a_conflict_is_not_a_refused_one() {
    // `done` の最終行は reword で、mewgit のメッセージも置いてある。HEAD は 1 本前の
    // コミットなので外れる（02 の「断られた reword」）。**1 本前のコミットを X の双子に
    // してある** —— main に X を cherry-pick して中身だけ変えたもので、メッセージも author
    // （日時まで）も X と同じ。メッセージと author だけで比べると取り違え、continue が
    // onto 側のコミットを amend して X が消える（`/code-review` が踏んだ）。
    let repo = TestRepo::init();
    repo.commit_file("f", "base");
    repo.git(&["switch", "-q", "-c", "side"]);
    let fix = repo.commit_bytes("f", b"side\n", "fix");
    repo.git(&["switch", "-q", "main"]);
    repo.git(&["cherry-pick", &fix.to_string()]);
    repo.write_worktree_file("f", b"main\n");
    repo.git(&["commit", "-q", "-a", "--amend", "--no-edit"]);
    let signature = |rev: &str| repo.git(&["log", "-1", "--format=%B%an%ae%ad", rev]);
    assert_eq!(
        signature("main"),
        signature(&fix.to_string()),
        "双子になっていない"
    );

    repo.git(&["switch", "-q", "side"]);
    let err = runner(&repo)
        .run(&GitOp::RebaseInteractive {
            onto: "main".into(),
            head: fix,
            plan: RebasePlan {
                entries: vec![entry(reword("fix new"), fix, "fix")],
            },
        })
        .await
        .expect_err("双子とコンフリクトするはずです");
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
    assert_eq!(signature("HEAD"), signature(&fix.to_string()));
    assert_eq!(refused_reword(&repo), None);

    repo.write_worktree_file("f", b"resolved\n");
    repo.git(&["add", "f"]);
    assert_eq!(refused_reword(&repo), None, "解消して add したあと");

    repo.git(&["reset", "-q", "--hard"]);
    assert!(!repo.path().join(".git/MERGE_MSG").exists());
    assert_eq!(refused_reword(&repo), None, "reset --hard のあと");
}

#[tokio::test]
async fn a_conflict_right_after_another_reword_is_not_a_refused_one() {
    // 1 本前の reword が通ると、HEAD の reflog の先頭は `rebase (reword): …` のまま X の
    // pick がコンフリクトして止まる。reflog だけで見ると取り違えるので、HEAD が X と
    // 同じメッセージ・author であることも見る（02 の「断られた reword」）。
    let (repo, first, second) = TestRepo::diverged();
    repo.git(&["switch", "-q", "side"]);
    let err = runner(&repo)
        .run(&GitOp::RebaseInteractive {
            onto: "main".into(),
            head: second,
            plan: RebasePlan {
                entries: vec![
                    entry(reword("side2 new"), second, "side2"),
                    entry(reword("side new"), first, "side"),
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
    assert!(
        repo.git(&["reflog", "-1", "--format=%gs"])
            .starts_with("rebase (reword): "),
        "前提の形になっていない"
    );
    assert_eq!(refused_reword(&repo), None);
}
