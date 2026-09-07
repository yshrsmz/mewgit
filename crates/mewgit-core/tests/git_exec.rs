//! `git_exec::Runner` の統合テスト。実リポジトリを一時ディレクトリに作って `git` を叩く。
//!
//! argv の形は `git_exec::argv` のユニットテストが見張る。ここで見るのは
//! 「その argv を実際の git に渡すと意図した結果になるか」。

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use mewgit_core::git_exec::{ExecError, ExecErrorKind, GitOp, Runner};
use mewgit_core::git_read::Oid;
use mewgit_core::test_util::{BareRepo, TestRepo};

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
fn push(set_upstream: bool) -> GitOp {
    GitOp::Push {
        remote: "origin".into(),
        branch: "main".into(),
        set_upstream,
        force: false,
        expected: None,
    }
}

/// `expected` を主張する force push。
fn force_push(expected: Option<Oid>) -> GitOp {
    GitOp::Push {
        remote: "origin".into(),
        branch: "main".into(),
        set_upstream: false,
        force: true,
        expected,
    }
}

/// 別のクローンが `origin main` を 1 つ進める。返るのはその Oid。
fn someone_else_pushes(remote: &BareRepo) -> Oid {
    let theirs = clone_of(remote);
    theirs.git(&["fetch", "-q", "origin"]);
    theirs.git(&["checkout", "-qB", "main", "origin/main"]);
    theirs.commit_file("theirs.txt", "theirs");
    theirs.git(&["push", "-q", "origin", "main"]);
    theirs.head_oid()
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

    let runner = repo.runner();
    runner
        .run(&GitOp::Stage(vec!["file.txt".into()]))
        .await
        .expect("stage できませんでした");
    assert_eq!(repo.porcelain(), "M  file.txt");

    runner
        .run(&GitOp::Unstage(vec!["file.txt".into()]))
        .await
        .expect("unstage できませんでした");
    assert_eq!(repo.porcelain(), " M file.txt");
}

#[tokio::test]
async fn unstage_works_before_the_first_commit() {
    // unstage が `restore --staged` だと、ここで `could not resolve 'HEAD'` に
    // なる。`reset` を選んでいる理由そのもの。
    let repo = TestRepo::init();
    repo.write_worktree_file("file.txt", b"new\n");
    repo.git(&["add", "file.txt"]);
    assert_eq!(repo.porcelain(), "A  file.txt");

    repo.runner()
        .run(&GitOp::Unstage(vec!["file.txt".into()]))
        .await
        .expect("unstage できませんでした");
    assert_eq!(repo.porcelain(), "?? file.txt");
}

#[tokio::test]
async fn a_dashed_file_name_is_not_read_as_an_option() {
    let repo = TestRepo::init();
    repo.commit("first");
    repo.write_worktree_file("-n", b"dashed\n");

    repo.runner()
        .run(&GitOp::Stage(vec!["-n".into()]))
        .await
        .expect("stage できませんでした");
    assert_eq!(repo.porcelain(), "A  -n");
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
    let runner = repo.runner();
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
async fn a_commit_message_keeps_its_body() {
    // モーダルは改行の打てる複数行バッファなので、`--message` に本文ごと渡る
    // （01 の「コミットメッセージの編集」）。1 行に潰れると要約しか残らない。
    let repo = TestRepo::init();
    repo.commit("first");
    repo.write_worktree_file("file.txt", b"second\n");
    let runner = repo.runner();
    runner
        .run(&GitOp::Stage(vec!["file.txt".into()]))
        .await
        .expect("stage できませんでした");

    runner
        .run(&GitOp::Commit {
            message: "subject\n\nbody 1\nbody 2".into(),
            amend: false,
        })
        .await
        .expect("commit できませんでした");

    assert_eq!(
        repo.git(&["log", "-1", "--pretty=%B"]),
        "subject\n\nbody 1\nbody 2"
    );
}

#[tokio::test]
async fn amend_replaces_the_last_commit() {
    let repo = TestRepo::init();
    repo.commit("first");

    repo.runner()
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

    let runner = repo.runner();
    runner
        .run(&GitOp::StageHunk {
            path: "file.txt".into(),
            patch: patch.clone(),
        })
        .await
        .expect("ハンクを stage できませんでした");
    assert_eq!(repo.porcelain(), "M  file.txt");

    // 同じパッチを逆に当てると index が元に戻る。
    runner
        .run(&GitOp::UnstageHunk {
            path: "file.txt".into(),
            patch,
        })
        .await
        .expect("ハンクを unstage できませんでした");
    assert_eq!(repo.porcelain(), " M file.txt");
}

#[tokio::test]
async fn discard_returns_a_tracked_file_to_head() {
    let repo = TestRepo::init();
    repo.commit_bytes("file.txt", b"original\n", "first");
    repo.write_worktree_file("file.txt", b"changed\n");
    repo.git(&["add", "file.txt"]);

    repo.runner()
        .run(&GitOp::Discard(vec!["file.txt".into()]))
        .await
        .expect("discard できませんでした");

    assert_eq!(repo.porcelain(), "");
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
    let err = repo
        .runner()
        .run(&GitOp::Discard(vec!["new.txt".into()]))
        .await
        .expect_err("untracked を restore できてはいけません");
    assert_eq!(failure_kind(&err), ExecErrorKind::Other);
    assert!(repo.path().join("new.txt").exists());

    repo.runner()
        .run(&GitOp::DiscardUntracked(vec!["new.txt".into()]))
        .await
        .expect("clean できませんでした");
    assert!(!repo.path().join("new.txt").exists());
    assert_eq!(repo.porcelain(), "");
}

#[tokio::test]
async fn switch_never_takes_the_target_as_a_path() {
    // `git checkout topic` は「topic というファイルを index から取り出す」と解釈され、
    // ワークツリーの変更を黙って捨てる。`switch` は pathspec を取らないので
    // `fatal: invalid reference: topic` で止まる（git 2.55 で確認）。ブランチが無いの
    // だから失敗しなければならない。
    //
    // **`switch` を使っていること自体はここでは見張れない。** `checkout <target> --`
    // に戻しても同じ結果になる（`--` が付いていれば revision として読む）。argv の
    // 単体テスト（`git_exec::argv`）がそちらを見ている。
    let repo = TestRepo::init();
    repo.commit_file("topic", "add a file named topic");
    repo.write_worktree_file("topic", b"work in progress\n");

    let err = repo
        .runner()
        .run(&GitOp::Switch("topic".into()))
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
async fn switch_moves_head_to_a_branch() {
    let repo = TestRepo::init();
    repo.commit("first");
    repo.git(&["branch", "topic"]);

    repo.runner()
        .run(&GitOp::Switch("topic".into()))
        .await
        .expect("ブランチを切り替えられませんでした");

    assert_eq!(repo.git(&["rev-parse", "--abbrev-ref", "HEAD"]), "topic");
}

#[tokio::test]
async fn a_rename_target_starting_with_a_dash_never_becomes_an_option() {
    // **`git branch` は parse-options なので、`--` が無いと `-` 始まりの `to` を
    // オプションとして拾う。** `git branch --move topic --force` は「`--force` へ
    // 改名」ではなく「HEAD を `topic` へ強制改名」になり、**元の `topic` が黙って
    // 消えて exit 0 で返る**（git 2.55 で確認）。`to` は `Modal::Input` が受けた
    // 文字がそのまま載る唯一の positional なので、ここだけは踏める。
    let repo = TestRepo::init();
    repo.commit("first");
    repo.git(&["branch", "topic"]);
    repo.commit("second");
    let topic = repo.git(&["rev-parse", "topic"]);

    repo.runner()
        .run(&GitOp::BranchRename {
            from: "topic".into(),
            to: "--force".into(),
        })
        .await
        .expect_err("git が断るはずです");

    // **成否だけでは見張れない。** `--` を落とすと git は exit 0 で返るので、
    // 消えていないことをブランチの側で見る。
    assert_eq!(repo.git(&["rev-parse", "topic"]), topic);
    assert_eq!(repo.git(&["rev-parse", "--abbrev-ref", "HEAD"]), "main");
}

#[tokio::test]
async fn branch_create_switch_delete_and_rename() {
    let repo = TestRepo::init();
    repo.commit("first");
    let runner = repo.runner();

    runner
        .run(&GitOp::BranchCreate {
            name: "feat/x".into(),
            start: Some("main".into()),
            switch: true,
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
        .run(&GitOp::Switch("main".into()))
        .await
        .expect("ブランチを切り替えられませんでした");
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
    let runner = repo.runner();

    runner
        .run(&GitOp::StashPush {
            message: "wip".to_string().into(),
            include_untracked: false,
            paths: None,
        })
        .await
        .expect("stash できませんでした");
    assert_eq!(repo.porcelain(), "");
    assert!(repo.git(&["stash", "list"]).contains("wip"));

    runner
        .run(&GitOp::StashPop(0))
        .await
        .expect("stash を戻せませんでした");
    assert_eq!(repo.porcelain(), " M file.txt");
}

#[tokio::test]
async fn a_conflicting_cherry_pick_is_classified_as_conflict() {
    let repo = TestRepo::init();
    repo.commit_bytes("file.txt", b"base\n", "base");
    repo.git(&["switch", "-qc", "side"]);
    let side = repo.commit_bytes("file.txt", b"side\n", "side");
    repo.git(&["switch", "-q", "main"]);
    repo.commit_bytes("file.txt", b"main\n", "main");

    let err = repo
        .runner()
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

    repo.runner()
        .run(&GitOp::CherryPickAbort)
        .await
        .expect("abort できませんでした");
    assert_eq!(repo.porcelain(), "");
}

#[tokio::test]
async fn a_rejected_push_is_classified_as_non_fast_forward() {
    let remote = BareRepo::init();
    let repo = clone_of(&remote);
    repo.commit("first");
    let runner = repo.runner();

    runner
        .run(&push(true))
        .await
        .expect("push できませんでした");
    let pushed = repo.head_oid();

    // リモートが知らない履歴に書き換えてから push すると弾かれる。
    repo.git(&["commit", "-q", "--amend", "-m", "rewritten"]);
    let err = runner
        .run(&push(false))
        .await
        .expect_err("弾かれるはずです");
    assert_eq!(failure_kind(&err), ExecErrorKind::NonFastForward);

    // 主張した位置にリモートが居るので force push は通る。
    runner
        .run(&force_push(Some(pushed)))
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
    let runner = mine.runner();
    runner
        .run(&push(true))
        .await
        .expect("push できませんでした");
    let pushed = mine.head_oid();

    let theirs_head = someone_else_pushes(&remote);

    mine.git(&["commit", "-q", "--amend", "-m", "rewritten"]);
    let err = runner
        .run(&force_push(Some(pushed)))
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
async fn the_lease_survives_a_fetch() {
    // **値の無い `--force-with-lease` はここで素通りする。** lease は
    // remote-tracking ref と比べるだけなので、`fetch` で `origin/main` が他人の
    // 位置に進むと検査が意味を失う。UI は「push を弾かれた → fetch して見る →
    // やっぱり force する」を普通にやるので、この経路が本命。
    let remote = BareRepo::init();
    let mine = clone_of(&remote);
    mine.commit("first");
    let runner = mine.runner();
    runner
        .run(&push(true))
        .await
        .expect("push できませんでした");
    let pushed = mine.head_oid();

    let theirs_head = someone_else_pushes(&remote);

    mine.git(&["commit", "-q", "--amend", "-m", "rewritten"]);
    runner
        .run(&GitOp::Fetch {
            remote: Some("origin".into()),
            prune: false,
        })
        .await
        .expect("fetch できませんでした");
    assert_eq!(
        mine.git(&["rev-parse", "origin/main"]),
        theirs_head.to_string(),
        "fetch で origin/main が他人の位置に進んでいる前提"
    );

    let err = runner
        .run(&force_push(Some(pushed)))
        .await
        .expect_err("主張した位置と違うので弾かれるはずです");

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

    let err = repo
        .runner()
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
        .run(&GitOp::Switch("main".into()))
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

    let err = repo
        .runner()
        .run(&GitOp::Stage(vec![bad]))
        .await
        .expect_err("実行してはいけません");

    assert!(
        matches!(err, ExecError::NoResult { .. }),
        "git が走ってしまった: {err:?}"
    );
    assert!(err.argv().is_empty(), "argv を組めていない");
    assert_eq!(repo.porcelain(), "");
}

#[tokio::test]
async fn a_failing_command_carries_argv_and_stderr() {
    // 不変ルール 4。握りつぶさずに UI まで届ける。
    let repo = TestRepo::init();
    repo.commit("first");

    let err = repo
        .runner()
        .run(&GitOp::Switch("no-such-branch".into()))
        .await
        .expect_err("失敗するはずです");

    let ExecError::Failed {
        argv, code, stderr, ..
    } = &err
    else {
        panic!("git が走っていません: {err:?}");
    };
    assert_eq!(argv, &["switch", "no-such-branch"]);
    assert_eq!(*code, Some(128));
    assert!(stderr.contains("no-such-branch"), "stderr: {stderr}");
}
