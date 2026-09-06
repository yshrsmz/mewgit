//! `git_exec::Runner` の統合テスト。実リポジトリを一時ディレクトリに作って `git` を叩く。
//!
//! argv の形は `git_exec::argv` のユニットテストが見張る。ここで見るのは
//! 「その argv を実際の git に渡すと意図した結果になるか」。

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use mewgit_core::git_exec::{ExecError, ExecErrorKind, GitOp, Runner};
use mewgit_core::test_util::{BareRepo, TestRepo};

/// 実行環境の設定を持ち込まない `Runner`。
fn runner(repo: &TestRepo) -> Runner {
    Runner::new(repo.path(), "git").with_env(repo.runner_env())
}

/// `git status --porcelain` の出力。
fn porcelain(repo: &TestRepo) -> String {
    repo.git(&["status", "--porcelain"])
}

/// `origin` に bare リポジトリを登録したワークツリー付きリポジトリ。
fn clone_of(remote: &BareRepo) -> TestRepo {
    let repo = TestRepo::init();
    repo.git(&[
        "remote",
        "add",
        "origin",
        remote.path().to_str().expect("パスが UTF-8 ではありません"),
    ]);
    repo
}

/// `origin main` への push。`set_upstream` は初回だけ立てる。
fn push(force: bool, set_upstream: bool) -> GitOp {
    GitOp::Push {
        remote: "origin".into(),
        branch: "main".into(),
        set_upstream,
        force,
    }
}

/// `git` が非 0 で終わったときの分類。それ以外の失敗なら panic する。
fn failure_kind(err: &ExecError) -> ExecErrorKind {
    match err {
        ExecError::Failed { kind, .. } => *kind,
        ExecError::NoResult { reason, .. } => panic!("git が走っていません: {reason}"),
    }
}

#[tokio::test]
async fn stage_and_unstage_a_tracked_file() {
    let repo = TestRepo::init();
    repo.commit("first");
    repo.write_worktree_file("file.txt", b"changed\n");

    let runner = runner(&repo);
    runner
        .run(&GitOp::Stage(vec!["file.txt".into()]))
        .await
        .expect("stage できませんでした");
    assert_eq!(porcelain(&repo), "M  file.txt");

    runner
        .run(&GitOp::Unstage(vec!["file.txt".into()]))
        .await
        .expect("unstage できませんでした");
    assert_eq!(porcelain(&repo), " M file.txt");
}

#[tokio::test]
async fn unstage_works_before_the_first_commit() {
    // unstage が `restore --staged` だと、ここで `could not resolve 'HEAD'` に
    // なる。`reset` を選んでいる理由そのもの。
    let repo = TestRepo::init();
    repo.write_worktree_file("file.txt", b"new\n");
    repo.git(&["add", "file.txt"]);
    assert_eq!(porcelain(&repo), "A  file.txt");

    runner(&repo)
        .run(&GitOp::Unstage(vec!["file.txt".into()]))
        .await
        .expect("unstage できませんでした");
    assert_eq!(porcelain(&repo), "?? file.txt");
}

#[tokio::test]
async fn a_dashed_file_name_is_not_read_as_an_option() {
    let repo = TestRepo::init();
    repo.commit("first");
    repo.write_worktree_file("-n", b"dashed\n");

    runner(&repo)
        .run(&GitOp::Stage(vec!["-n".into()]))
        .await
        .expect("stage できませんでした");
    assert_eq!(porcelain(&repo), "A  -n");
}

#[tokio::test]
async fn commit_runs_hooks() {
    // 不変ルール 1（書き込みは git CLI）の根拠。git2 で commit すると hook は走らない。
    let repo = TestRepo::init();
    repo.commit("first");
    let hook = repo.path().join(".git/hooks/pre-commit");
    fs::write(&hook, "#!/bin/sh\necho ran > hook-ran.txt\n").expect("hook を書けませんでした");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755))
        .expect("hook に実行権限を付けられませんでした");

    repo.write_worktree_file("file.txt", b"second\n");
    let runner = runner(&repo);
    runner
        .run(&GitOp::Stage(vec!["file.txt".into()]))
        .await
        .expect("stage できませんでした");
    let output = runner
        .run(&GitOp::Commit {
            message: "second".into(),
            amend: false,
        })
        .await
        .expect("commit できませんでした");

    assert!(
        repo.path().join("hook-ran.txt").exists(),
        "hook が走っていない"
    );
    assert!(
        output.stdout.contains("second"),
        "stdout: {}",
        output.stdout
    );
    assert_eq!(repo.git(&["log", "-1", "--pretty=%s"]), "second");
}

#[tokio::test]
async fn amend_replaces_the_last_commit() {
    let repo = TestRepo::init();
    repo.commit("first");

    runner(&repo)
        .run(&GitOp::Commit {
            message: "amended".into(),
            amend: true,
        })
        .await
        .expect("amend できませんでした");

    assert_eq!(repo.git(&["log", "--pretty=%s"]), "amended");
}

#[tokio::test]
async fn stage_hunk_sends_the_patch_on_stdin() {
    let repo = TestRepo::init();
    repo.commit_bytes("file.txt", b"one\ntwo\n", "first");
    repo.write_worktree_file("file.txt", b"one\nTWO\n");

    // 行継続（`\` + 改行）で書くと、コンテキスト行の先頭の空白まで畳まれて
    // `corrupt patch` になる。1 行で書く。
    let patch =
        b"diff --git a/file.txt b/file.txt\n--- a/file.txt\n+++ b/file.txt\n@@ -1,2 +1,2 @@\n one\n-two\n+TWO\n"
            .to_vec();

    let runner = runner(&repo);
    runner
        .run(&GitOp::StageHunk {
            path: "file.txt".into(),
            patch: patch.clone(),
        })
        .await
        .expect("ハンクを stage できませんでした");
    assert_eq!(porcelain(&repo), "M  file.txt");

    // 同じパッチを逆に当てると index が元に戻る。
    runner
        .run(&GitOp::UnstageHunk {
            path: "file.txt".into(),
            patch,
        })
        .await
        .expect("ハンクを unstage できませんでした");
    assert_eq!(porcelain(&repo), " M file.txt");
}

#[tokio::test]
async fn discard_returns_a_tracked_file_to_head() {
    let repo = TestRepo::init();
    repo.commit_bytes("file.txt", b"original\n", "first");
    repo.write_worktree_file("file.txt", b"changed\n");
    repo.git(&["add", "file.txt"]);

    runner(&repo)
        .run(&GitOp::Discard(vec!["file.txt".into()]))
        .await
        .expect("discard できませんでした");

    assert_eq!(porcelain(&repo), "");
    assert_eq!(
        fs::read(repo.path().join("file.txt")).expect("読めませんでした"),
        b"original\n"
    );
}

#[tokio::test]
async fn discard_untracked_deletes_the_file() {
    let repo = TestRepo::init();
    repo.commit("first");
    repo.write_worktree_file("new.txt", b"new\n");

    // tracked 用の discard では untracked を消せない。変種を分けている理由。
    let err = runner(&repo)
        .run(&GitOp::Discard(vec!["new.txt".into()]))
        .await
        .expect_err("untracked を restore できてはいけません");
    assert_eq!(failure_kind(&err), ExecErrorKind::Other);
    assert!(repo.path().join("new.txt").exists());

    runner(&repo)
        .run(&GitOp::DiscardUntracked(vec!["new.txt".into()]))
        .await
        .expect("clean できませんでした");
    assert!(!repo.path().join("new.txt").exists());
    assert_eq!(porcelain(&repo), "");
}

#[tokio::test]
async fn checkout_reads_the_target_as_a_revision() {
    // 末尾の `--` が無いと、`git checkout topic` は「topic というファイルを index から
    // 取り出す」と解釈され、ワークツリーの変更を黙って捨てる。ブランチが無いのだから
    // 失敗しなければならない。
    let repo = TestRepo::init();
    repo.commit_file("topic", "add a file named topic");
    repo.write_worktree_file("topic", b"work in progress\n");

    let err = runner(&repo)
        .run(&GitOp::Checkout("topic".into()))
        .await
        .expect_err("ブランチではないので失敗するはずです");

    assert_eq!(failure_kind(&err), ExecErrorKind::Other);
    assert_eq!(
        fs::read(repo.path().join("topic")).expect("読めませんでした"),
        b"work in progress\n",
        "ファイルとして取り出されている"
    );
}

#[tokio::test]
async fn checkout_switches_to_a_branch() {
    let repo = TestRepo::init();
    repo.commit("first");
    repo.git(&["branch", "topic"]);

    runner(&repo)
        .run(&GitOp::Checkout("topic".into()))
        .await
        .expect("checkout できませんでした");

    assert_eq!(repo.git(&["rev-parse", "--abbrev-ref", "HEAD"]), "topic");
}

#[tokio::test]
async fn branch_create_switch_delete_and_rename() {
    let repo = TestRepo::init();
    repo.commit("first");
    let runner = runner(&repo);

    runner
        .run(&GitOp::BranchCreate {
            name: "feat/x".into(),
            start: Some("main".into()),
            checkout: true,
        })
        .await
        .expect("ブランチを作れませんでした");
    assert_eq!(repo.git(&["rev-parse", "--abbrev-ref", "HEAD"]), "feat/x");

    runner
        .run(&GitOp::BranchRename {
            from: "feat/x".into(),
            to: "feat/y".into(),
        })
        .await
        .expect("ブランチを改名できませんでした");
    assert_eq!(repo.git(&["rev-parse", "--abbrev-ref", "HEAD"]), "feat/y");

    runner
        .run(&GitOp::Checkout("main".into()))
        .await
        .expect("checkout できませんでした");
    runner
        .run(&GitOp::BranchDelete {
            name: "feat/y".into(),
            force: true,
        })
        .await
        .expect("ブランチを消せませんでした");
    assert_eq!(repo.git(&["branch", "--format=%(refname:short)"]), "main");
}

#[tokio::test]
async fn stash_push_and_pop() {
    let repo = TestRepo::init();
    repo.commit_bytes("file.txt", b"original\n", "first");
    repo.write_worktree_file("file.txt", b"changed\n");
    let runner = runner(&repo);

    runner
        .run(&GitOp::StashPush {
            message: "wip".to_string().into(),
            include_untracked: false,
            paths: None,
        })
        .await
        .expect("stash できませんでした");
    assert_eq!(porcelain(&repo), "");
    assert!(repo.git(&["stash", "list"]).contains("wip"));

    runner
        .run(&GitOp::StashPop(0))
        .await
        .expect("stash を戻せませんでした");
    assert_eq!(porcelain(&repo), " M file.txt");
}

#[tokio::test]
async fn a_conflicting_cherry_pick_is_classified_as_conflict() {
    let repo = TestRepo::init();
    repo.commit_bytes("file.txt", b"base\n", "base");
    repo.git(&["switch", "-qc", "side"]);
    let side = repo.commit_bytes("file.txt", b"side\n", "side");
    repo.git(&["switch", "-q", "main"]);
    repo.commit_bytes("file.txt", b"main\n", "main");

    let err = runner(&repo)
        .run(&GitOp::CherryPick(vec![side]))
        .await
        .expect_err("コンフリクトするはずです");

    let ExecError::Failed {
        code, argv, kind, ..
    } = &err
    else {
        panic!("git が走っていません: {err:?}");
    };
    assert_eq!(*kind, ExecErrorKind::Conflict);
    assert_eq!(*code, Some(1));
    assert_eq!(argv[0], "cherry-pick");

    runner(&repo)
        .run(&GitOp::CherryPickAbort)
        .await
        .expect("abort できませんでした");
    assert_eq!(porcelain(&repo), "");
}

#[tokio::test]
async fn a_rejected_push_is_classified_as_non_fast_forward() {
    let remote = BareRepo::init();
    let repo = clone_of(&remote);
    repo.commit("first");
    let runner = runner(&repo);

    runner
        .run(&push(false, true))
        .await
        .expect("push できませんでした");

    // リモートが知らない履歴に書き換えてから push すると弾かれる。
    repo.git(&["commit", "-q", "--amend", "-m", "rewritten"]);
    let err = runner
        .run(&push(false, false))
        .await
        .expect_err("弾かれるはずです");
    assert_eq!(failure_kind(&err), ExecErrorKind::NonFastForward);

    // force は `--force-with-lease`。リモートは自分が push した位置のままなので通る。
    runner
        .run(&push(true, false))
        .await
        .expect("force push できませんでした");
    assert_eq!(
        remote.git(&["rev-parse", "refs/heads/main"]),
        repo.git(&["rev-parse", "HEAD"])
    );
}

#[tokio::test]
async fn force_with_lease_refuses_when_someone_else_moved_the_remote() {
    // `--force` ならここで他人のコミットを消せてしまう。lease を選んでいる理由。
    let remote = BareRepo::init();
    let mine = clone_of(&remote);
    mine.commit("first");
    let runner = runner(&mine);
    runner
        .run(&push(false, true))
        .await
        .expect("push できませんでした");

    // 別のクローンがリモートを進める。こちらの `origin/main` は古いままになる。
    let theirs = clone_of(&remote);
    theirs.git(&["fetch", "-q", "origin"]);
    theirs.git(&["checkout", "-qB", "main", "origin/main"]);
    theirs.commit_file("theirs.txt", "theirs");
    theirs.git(&["push", "-q", "origin", "main"]);
    let theirs_head = theirs.head_oid();

    mine.git(&["commit", "-q", "--amend", "-m", "rewritten"]);
    let err = runner
        .run(&push(true, false))
        .await
        .expect_err("lease が古いので弾かれるはずです");

    assert_eq!(failure_kind(&err), ExecErrorKind::NonFastForward);
    assert_eq!(
        remote.git(&["rev-parse", "refs/heads/main"]),
        theirs_head.to_string(),
        "他人のコミットを消してしまった"
    );
}

#[tokio::test]
async fn interactive_rebase_is_refused_without_running_git() {
    // todo を差し替える経路がまだ無い（M5）。走らせると plan を無視した rebase に
    // なるので、起動しないことをここで見張る。
    let repo = TestRepo::init();
    repo.commit("first");
    let before = repo.head_oid();

    let err = runner(&repo)
        .run(&GitOp::RebaseInteractive {
            onto: "main".into(),
            plan: Default::default(),
        })
        .await
        .expect_err("実行してはいけません");

    assert!(
        matches!(err, ExecError::NoResult { .. }),
        "git が走ってしまった: {err:?}"
    );
    assert_eq!(err.argv(), ["rebase", "--interactive", "main"]);
    assert_eq!(repo.head_oid(), before);
    assert!(
        !repo.path().join(".git/rebase-merge").exists(),
        "rebase が始まってしまっている"
    );
}

#[tokio::test]
async fn the_configured_git_binary_is_the_one_that_runs() {
    // `config` の `general.git_bin` を渡し忘れていないことの見張り。直書きに
    // 戻すと、ここで本物の git が走って成功してしまう。
    let repo = TestRepo::init();
    repo.commit("first");

    let err = Runner::new(repo.path(), repo.path().join("no-such-git"))
        .with_env(repo.runner_env())
        .run(&GitOp::Checkout("main".into()))
        .await
        .expect_err("起動できないはずです");

    let ExecError::NoResult { reason, .. } = &err else {
        panic!("git が走ってしまった: {err:?}");
    };
    assert!(reason.contains("no-such-git"), "reason: {reason}");
}

#[tokio::test]
async fn a_non_utf8_path_never_reaches_git() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let repo = TestRepo::init();
    repo.commit("first");
    let bad = PathBuf::from(OsString::from_vec(vec![0x66, 0xff]));

    let err = runner(&repo)
        .run(&GitOp::Stage(vec![bad]))
        .await
        .expect_err("実行してはいけません");

    assert!(
        matches!(err, ExecError::NoResult { .. }),
        "git が走ってしまった: {err:?}"
    );
    assert!(err.argv().is_empty(), "argv を組めていない");
    assert_eq!(porcelain(&repo), "");
}

#[tokio::test]
async fn a_failing_command_carries_argv_and_stderr() {
    // 不変ルール 4。握りつぶさずに UI まで届ける。
    let repo = TestRepo::init();
    repo.commit("first");

    let err = runner(&repo)
        .run(&GitOp::Checkout("no-such-branch".into()))
        .await
        .expect_err("失敗するはずです");

    let ExecError::Failed {
        argv, code, stderr, ..
    } = &err
    else {
        panic!("git が走っていません: {err:?}");
    };
    assert_eq!(argv, &["checkout", "no-such-branch", "--"]);
    assert_eq!(*code, Some(128));
    assert!(stderr.contains("no-such-branch"), "stderr: {stderr}");
}
