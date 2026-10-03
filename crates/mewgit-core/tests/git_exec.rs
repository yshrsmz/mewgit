//! `git_exec::Runner` の統合テスト。実リポジトリを一時ディレクトリに作って `git` を叩く。
//!
//! argv の形は `git_exec::argv` のユニットテストが見張る。ここで見るのは
//! 「その argv を実際の git に渡すと意図した結果になるか」。

use std::fs;
use std::path::PathBuf;

use mewgit_core::auth::ssh::SshCommand;
use mewgit_core::auth::{resolve, DegradedReason};
use mewgit_core::config::Config;
use mewgit_core::git_exec::{
    ExecError, ExecErrorKind, GitOp, HunkApply, LocalBranch, PrSource, RebaseAction, RebaseEntry,
    RebasePlan, Runner,
};
use mewgit_core::git_read::Oid;
use mewgit_core::test_util::{
    fake_fork_source, fake_pr_checkout, fake_target, wait_for_pid, wait_until_gone, BareRepo, Fake,
    TestRepo, FAKE_FORK_URL,
};

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
        src: "main".into(),
        dst: None,
        set_upstream,
        lease: None,
    }
}

/// `lease` を主張する force push。
fn force_push(lease: Oid) -> GitOp {
    GitOp::Push {
        remote: "origin".into(),
        src: "main".into(),
        dst: None,
        set_upstream: false,
        lease: Some(lease),
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

/// ダンプ用スクリプトに渡しておく印。ユーザーの指定した引数の代わり。
const SSH_MARKER: &str = "--mewgit-marker";

/// ssh の代わりに走らせて argv を書き出すスクリプトを置き、その `core.sshCommand`
/// 相当の文字列と、書き出し先のパスを返す。
///
/// `Runner` が実効 ssh の後ろに `-o BatchMode=yes` を足すので、**ここに書いた
/// スクリプト自身が argv に残っているか**も一緒に見られる（重ねたのか差し替えたのか）。
fn ssh_dump(repo: &TestRepo) -> (String, PathBuf) {
    let script = repo.path().join("dump-ssh.sh");
    let log = repo.path().join("ssh-argv.txt");
    // 1 行ずつ書くのは、引数の区切りを空白と区別するため。
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nfor arg in \"$@\"; do echo \"$arg\" >> {}; done\nexit 1\n",
            log.display()
        ),
    )
    .expect("スクリプトを書けませんでした");
    // **引数を 1 つ足しておく。** 実効 ssh を捨てて `ssh -o BatchMode=yes` を素で
    // 置く実装ではこのスクリプト自体が起動されず、重ねる順序が逆の実装では
    // この印より前に `-o` が並ぶ。
    (format!("sh {} {SSH_MARKER}", script.display()), log)
}

/// `git` が非 0 で終わったときの分類。それ以外の失敗なら panic する。
fn failure_kind(err: &ExecError) -> ExecErrorKind {
    match err {
        ExecError::Failed { kind, .. } => *kind,
        ExecError::NoResult { reason, .. } => panic!("git が走っていません: {reason}"),
        ExecError::Canceled { argv } => panic!("中断していません: git {}", argv.join(" ")),
        ExecError::Stale { path, .. } => panic!("git が走っていません: {} が古い", path.display()),
    }
}

/// `git` が非 0 で終わったときの stdout と stderr。それ以外の失敗なら panic する。
fn failure_output(err: &ExecError) -> (&str, &str) {
    let ExecError::Failed { stdout, stderr, .. } = err else {
        panic!("git が走っていません: {err:?}");
    };
    (stdout, stderr)
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
async fn set_account_writes_where_the_resolver_reads() {
    // **書く側（この argv）と読む側（`auth::resolve` の git2）が同じ段・同じキーか。**
    // `resolve` は `ConfigLevel::Local` だけを読む（04 の「`.git/config` の読み方」）
    // ので、段が違うと**書けたのに読まれない**。読み手を通さずに
    // `git config --get` で読み返すと、その食い違いを見張れない。
    //
    // **login を `-` 始まりにしても書ける。** ただしこれは `--` の見張りではない
    // —— `--` を外しても同じ結果になることを実測した（03 の表）。
    let repo = TestRepo::init();
    repo.runner()
        .run(&GitOp::SetAccount {
            login: "-taro".into(),
        })
        .await
        .expect("mewgit.user を書けませんでした");

    // **手順 1 で止まる `Backend`。** 一覧は空なので、`mewgit.user` を読めていな
    // ければ手順 2 へ落ちて `NotLoggedIn` になる。読めていれば、その login の
    // トークンが取れずに `ConfiguredUnavailable` で止まる（04 の手順 1）。
    let backend = Fake::new(Vec::new()).without_token("-taro");
    let resolved = resolve(&fake_target(), repo.path(), &backend, &Config::default()).await;

    // 手順 1 まで届いた = 書いた login をそのまま読んでいる。
    assert!(
        matches!(&resolved, Err(DegradedReason::ConfiguredUnavailable { login, .. }) if login == "-taro"),
        "{resolved:?}"
    );
}

#[tokio::test]
async fn unset_account_removes_what_set_account_wrote() {
    // **読み手を通して確かめる**（`set_account_writes_where_the_resolver_reads` と
    // 同じ理由）。`git config --get` で読み返すと、`--unset` が別の段を消した形を
    // 見張れない。
    let repo = TestRepo::init();
    let runner = repo.runner();
    runner
        .run(&GitOp::SetAccount {
            login: "taro".into(),
        })
        .await
        .expect("mewgit.user を書けませんでした");

    runner
        .run(&GitOp::UnsetAccount)
        .await
        .expect("mewgit.user を外せませんでした");

    // 一覧が空なので、`mewgit.user` が残っていれば手順 1 で
    // `ConfiguredUnavailable` になる。外せていれば手順 2 の `NotLoggedIn`。
    let backend = Fake::new(Vec::new());
    let resolved = resolve(&fake_target(), repo.path(), &backend, &Config::default()).await;

    assert!(
        matches!(&resolved, Err(DegradedReason::NotLoggedIn { .. })),
        "{resolved:?}"
    );
}

#[tokio::test]
async fn unsetting_an_account_that_was_never_set_fails_without_saying_why() {
    // **exit 5 を stderr 空で返す**（03 の実測表）。01 が「書き込みのあとは成否に
    // 関わらず解決をやり直す」と決めた根拠がこれ —— 失敗から「いま何が設定されて
    // いるか」は読めない。**実物の git で確かめる**（設計書の断定を叩き直す側）。
    let repo = TestRepo::init();

    let err = repo
        .runner()
        .run(&GitOp::UnsetAccount)
        .await
        .expect_err("設定されていないのに成功した");

    match err {
        ExecError::Failed { code, stderr, .. } => {
            assert_eq!(code, Some(5));
            assert!(stderr.is_empty(), "{stderr:?}");
        }
        other => panic!("{other:?}"),
    }
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
    repo.write_hook("pre-commit", "#!/bin/sh\necho ran > hook-ran.txt\n");

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
        .run(&GitOp::StageHunk(Box::new(HunkApply {
            path: "file.txt".into(),
            patch: patch.clone(),
            expect: repo.path_stamp("file.txt"),
        })))
        .await
        .expect("ハンクを stage できませんでした");
    assert_eq!(repo.porcelain(), "M  file.txt");

    // 同じパッチを逆に当てると index が元に戻る。
    runner
        .run(&GitOp::UnstageHunk(Box::new(HunkApply {
            path: "file.txt".into(),
            patch,
            expect: repo.path_stamp("file.txt"),
        })))
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
    // **`Switch` が効いていることもここで出る。** 切り替わっていなければ、最後の
    // `BranchDelete` を git が断る（チェックアウト中のブランチは消せない）。
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
async fn the_reason_of_a_failure_is_the_line_git_wrote_it_on() {
    // 03 の「エラー分類」の「理由の行」を、実物の git の出力で踏む。どれも案内の無い
    // `Other` で、要約行には `ExecError::reason` がそのまま出る（01 の「失敗の案内文」）。
    let repo = TestRepo::init();
    repo.commit_bytes("file.txt", b"one\n", "first");
    repo.git(&["branch", "old"]);
    repo.git(&["switch", "--quiet", "--create", "side"]);
    repo.commit_bytes("file.txt", b"side\n", "side");
    let side = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["switch", "--quiet", "main"]);
    repo.commit_bytes("file.txt", b"two\n", "second");
    repo.write_worktree_file("file.txt", b"dirty\n");
    let runner = repo.runner();

    // 最初の `error:` の行（下にファイルの一覧と `Aborting` が続く）。
    let err = runner
        .run(&GitOp::Switch("old".into()))
        .await
        .expect_err("上書きされるので断るはずです");
    assert_eq!(failure_kind(&err), ExecErrorKind::Other);
    assert_eq!(
        err.reason(),
        Some("Your local changes to the following files would be overwritten by checkout:")
    );

    // **最初であって最後ではない** —— 最後は `fatal: cherry-pick failed`。
    let err = runner
        .run(&GitOp::CherryPick(vec![side.parse().expect("oid")]))
        .await
        .expect_err("上書きされるので断るはずです");
    let (_, stderr) = failure_output(&err);
    assert!(
        stderr.trim_end().ends_with("fatal: cherry-pick failed"),
        "{stderr}"
    );
    assert_eq!(
        err.reason(),
        Some("Your local changes to the following files would be overwritten by merge:")
    );

    // `hint:` は飛ばす。
    let err = runner
        .run(&GitOp::BranchDelete {
            name: "side".into(),
            force: false,
        })
        .await
        .expect_err("未マージなので断るはずです");
    let (_, stderr) = failure_output(&err);
    assert!(stderr.contains("hint:"), "{stderr}");
    assert_eq!(err.reason(), Some("the branch 'side' is not fully merged"));
}

#[tokio::test]
async fn a_push_refused_by_a_pre_push_hook_has_no_reason_line_and_no_escapes() {
    // stderr は締めの `error: failed to push some refs` だけで、hook の文言は hook が
    // 書いた側に出る（03 の「理由の行」。git 2.56.0）。**hook の文言は理由に選ばない**
    // （要約は固定の文言で `L` を案内する。01）。hook が付けた色は剥がれて届く。
    let remote = BareRepo::init();
    let repo = clone_of(&remote);
    repo.commit("first");
    let runner = repo.runner();
    for script in [
        "#!/bin/sh\nprintf '\\033[31mlint failed (stdout)\\033[0m\\n'\nexit 1\n",
        "#!/bin/sh\nprintf '\\033[31mlint failed (stderr)\\033[0m\\n' >&2\nexit 1\n",
    ] {
        repo.write_hook("pre-push", script);
        let err = runner
            .run(&push(true))
            .await
            .expect_err("hook が断るはずです");
        let (stdout, stderr) = failure_output(&err);
        assert!(stderr.contains("failed to push some refs"), "{stderr}");
        // 色を付けた行が実際に届いていること（剥がれて残る）。
        let output = format!("{stdout}{stderr}");
        assert!(output.contains("lint failed ("), "{output}");
        assert!(!output.contains('\x1b'), "{output:?}");
        assert_eq!(err.reason(), None);
    }
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
async fn unresolved_files_refuse_continue_and_stash_as_unmerged() {
    // 未解消のファイルを残したまま押した形（03 の「エラー分類」）。git の文言は入口で割れる
    // —— rebase の continue は stdout にだけ、stash は stdout の `needs merge` と stderr の
    // `could not write index`、cherry-pick の continue は stderr に理由を書く。
    let repo = TestRepo::init();
    repo.commit_bytes("file.txt", b"base\n", "base");
    repo.git(&["switch", "-qc", "side"]);
    let side = repo.commit_bytes("file.txt", b"side\n", "side");
    repo.git(&["switch", "-q", "main"]);
    repo.commit_bytes("file.txt", b"main\n", "main");
    let runner = repo.runner();

    assert!(
        !repo.try_git(&["rebase", "side"]),
        "コンフリクトするはずです"
    );
    let err = runner
        .run(&GitOp::RebaseContinue { reword: None })
        .await
        .expect_err("未解消のまま continue できてしまいました");
    assert_eq!(failure_kind(&err), ExecErrorKind::Unmerged);
    let (stdout, stderr) = failure_output(&err);
    assert!(stdout.contains("file.txt: needs merge"), "{stdout}");
    assert_eq!(stderr, "");

    let err = runner
        .run(&GitOp::StashPush {
            message: None,
            include_untracked: true,
            paths: None,
        })
        .await
        .expect_err("未解消のまま stash できてしまいました");
    assert_eq!(failure_kind(&err), ExecErrorKind::Unmerged);
    let (stdout, _) = failure_output(&err);
    assert!(stdout.contains("file.txt: needs merge"), "{stdout}");

    runner
        .run(&GitOp::RebaseAbort)
        .await
        .expect("abort できませんでした");
    assert!(
        !repo.try_git(&["cherry-pick", &side.to_string()]),
        "コンフリクトするはずです"
    );
    let err = runner
        .run(&GitOp::CherryPickContinue)
        .await
        .expect_err("未解消のまま continue できてしまいました");
    assert_eq!(failure_kind(&err), ExecErrorKind::Unmerged);
}

#[tokio::test]
async fn an_empty_cherry_pick_stops_and_only_skip_gets_out() {
    // HEAD の祖先を積むと変更が空で止まり、`CHERRY_PICK_HEAD` が残る。continue は
    // 同じ理由で断られ、抜けられるのは skip か abort（03 の「continue / skip / abort の
    // 終わり方」）。**stdout に小文字の `conflicts` が出る**ので `Conflict` に落ちないことも見る。
    let repo = TestRepo::init();
    let base = repo.commit_bytes("file.txt", b"base\n", "base");
    // **別のファイルに積む。** 同じファイルを変えると、祖先の pick は空ではなく
    // コンフリクトで止まる（最初に書いた形はそちらを見ていた）。
    repo.commit_bytes("other.txt", b"next\n", "next");
    let pick_head = repo.path().join(".git").join("CHERRY_PICK_HEAD");

    let err = repo
        .runner()
        .run(&GitOp::CherryPick(vec![base]))
        .await
        .expect_err("変更が空で止まるはずです");
    assert_eq!(failure_kind(&err), ExecErrorKind::Empty);
    assert!(pick_head.exists());

    let err = repo
        .runner()
        .run(&GitOp::CherryPickContinue)
        .await
        .expect_err("continue では抜けられないはずです");
    assert_eq!(failure_kind(&err), ExecErrorKind::Empty);

    repo.runner()
        .run(&GitOp::CherryPickSkip)
        .await
        .expect("skip できませんでした");
    assert!(!pick_head.exists());
}

#[tokio::test]
async fn reverting_a_merge_keeps_the_parent_named_by_mainline() {
    // `--mainline 1` は 1 番目の親（main）を残し、side から入った変更を取り消す
    // （03 の「revert の開始」の実測）。番号の向きを取り違えると main 側が消える。
    let repo = TestRepo::init();
    repo.commit_bytes("file.txt", b"base\n", "base");
    repo.git(&["switch", "-qc", "side"]);
    repo.commit_bytes("side.txt", b"side\n", "side");
    repo.git(&["switch", "-q", "main"]);
    repo.commit_bytes("main.txt", b"main\n", "main");
    repo.git(&["merge", "-q", "--no-ff", "side", "-m", "merge"]);
    let merge = repo.git(&["rev-parse", "HEAD"]);

    repo.runner()
        .run(&GitOp::Revert {
            oids: vec![merge.trim().parse().expect("oid ではありません")],
            mainline: Some(1),
        })
        .await
        .expect("revert できませんでした");

    assert!(!repo.path().join("side.txt").exists());
    assert!(repo.path().join("main.txt").exists());
    assert_eq!(repo.porcelain(), "");
}

#[tokio::test]
async fn a_revert_with_nothing_to_undo_ends_without_leaving_a_revert_in_progress() {
    // 取り消し済みのコミットをもう一度 revert すると、exit 1 で `REVERT_HEAD` を残さずに
    // 返る（03 の「revert の開始」）。**stderr は空**なので、stdout で `NoChanges` に分ける。
    let repo = TestRepo::init();
    repo.commit_bytes("file.txt", b"base\n", "base");
    let next = repo.commit_bytes("other.txt", b"next\n", "next");
    let revert = GitOp::Revert {
        oids: vec![next],
        mainline: None,
    };
    repo.runner()
        .run(&revert)
        .await
        .expect("1 回目の revert ができませんでした");
    let head = repo.git(&["rev-parse", "HEAD"]);

    let err = repo
        .runner()
        .run(&revert)
        .await
        .expect_err("取り消す変更が無いはずです");

    assert_eq!(failure_kind(&err), ExecErrorKind::NoChanges);
    assert!(!repo.path().join(".git").join("REVERT_HEAD").exists());
    assert_eq!(repo.git(&["rev-parse", "HEAD"]), head);
}

#[tokio::test]
async fn a_revert_resolved_to_nothing_stops_on_continue_and_skip_gets_out() {
    // 1 本だけの revert でも、コンフリクトを HEAD 側の内容で解くと continue は空で
    // 断られ、`REVERT_HEAD` が残る（03 の「revert の開始」）。stderr は空。
    let repo = TestRepo::init();
    repo.commit_bytes("file.txt", b"1\n", "c1");
    let c2 = repo.commit_bytes("file.txt", b"2\n", "c2");
    let head = repo.commit_bytes("file.txt", b"3\n", "c3");
    let revert_head = repo.path().join(".git").join("REVERT_HEAD");
    let revert = GitOp::Revert {
        oids: vec![c2],
        mainline: None,
    };

    let err = repo
        .runner()
        .run(&revert)
        .await
        .expect_err("コンフリクトするはずです");
    assert_eq!(failure_kind(&err), ExecErrorKind::Conflict);
    repo.write_worktree_file("file.txt", b"3\n");
    repo.git(&["add", "file.txt"]);

    let err = repo
        .runner()
        .run(&GitOp::RevertContinue)
        .await
        .expect_err("空で断られるはずです");
    assert_eq!(failure_kind(&err), ExecErrorKind::NoChanges);
    assert!(revert_head.exists());

    repo.runner()
        .run(&GitOp::RevertSkip)
        .await
        .expect("skip できませんでした");
    assert!(!revert_head.exists());
    assert_eq!(repo.git(&["rev-parse", "HEAD"]).trim(), head.to_string());
}

#[tokio::test]
async fn batch_mode_is_layered_onto_the_user_ssh_command() {
    // **`GIT_TERMINAL_PROMPT=0` は ssh に効かない**ので、agent に載っていない鍵だと
    // raw mode の画面ごと止まる。`-o BatchMode=yes` で即失敗させる（03 の
    // 「BatchMode の重ね方」）。見るのは 2 つ — オプションが ssh まで届いているか、
    // **ユーザーの設定を捨てずに重ねているか**。
    let repo = TestRepo::init();
    repo.commit("first");
    let (ssh_command, log) = ssh_dump(&repo);
    repo.git(&[
        "remote",
        "add",
        "origin",
        "ssh://git@example.invalid/a/b.git",
    ]);

    let runner = Runner::new(
        repo.path(),
        "git",
        SshCommand::Shell(ssh_command.clone()),
        "mewgit",
    )
    .with_env(repo.runner_env());
    let err = runner
        .run(&GitOp::Fetch {
            remote: Some("origin".into()),
            prune: false,
        })
        .await
        .expect_err("ダンプ用のスクリプトは git の通信を通さない");
    assert!(
        matches!(err, ExecError::Failed { .. }),
        "git は走っているはず: {err:?}"
    );

    // 1 行 1 引数で書き出してある。空白を含む引数と区別するため。
    let dumped = fs::read_to_string(&log).expect("ssh が起動していません");
    let args: Vec<&str> = dumped.lines().collect();
    // **印 → `-o BatchMode=yes` の順**。ユーザーの引数を消していないことと、
    // 重ねているのが後ろであることを 1 つで見る。後ろでなければ、ユーザーが
    // 書いた `BatchMode=no` を黙って上書きする側になる（ssh は先に現れた値を採る）。
    assert_eq!(
        args.iter().take(3).copied().collect::<Vec<_>>(),
        vec![SSH_MARKER, "-o", "BatchMode=yes"],
        "ssh に渡った argv: {args:?}"
    );
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
        .run(&force_push(pushed))
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
        .run(&force_push(pushed))
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
        .run(&force_push(pushed))
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
async fn interactive_rebase_leaves_the_branch_alone_when_the_editor_fails() {
    // `runner()` の自バイナリは存在しないパス。sequence editor が起動できなければ、
    // git は rebase を始めずに断る（03 の実測）。plan を無視した rebase にならない
    // ことの見張り。通しで流す側は `mewgit` クレートの `tests/interactive_rebase.rs`。
    let repo = TestRepo::init();
    repo.commit("first");
    repo.commit("second");
    let before = repo.head_oid();

    let err = repo
        .runner()
        .run(&GitOp::RebaseInteractive {
            onto: "HEAD~1".into(),
            head: before,
            plan: RebasePlan {
                entries: vec![RebaseEntry {
                    action: RebaseAction::Drop,
                    oid: before,
                    summary: "second".into(),
                }],
            },
        })
        .await
        .expect_err("editor が無いので git は断るはずです");

    let ExecError::Failed {
        argv, stderr, kind, ..
    } = &err
    else {
        panic!("git が走っていない: {err:?}");
    };
    assert_eq!(
        argv,
        &["rebase", "--interactive", "--no-update-refs", "HEAD~1"]
    );
    assert!(
        // git 2.39 / 2.43 は `There was …` と大文字で書く（03 の「git の最低版」）。
        stderr
            .to_ascii_lowercase()
            .contains("there was a problem with the editor"),
        "{stderr}"
    );
    // 起動中にバイナリが消えた形と同じ（03 の「消えた自バイナリ」）。
    assert_eq!(*kind, ExecErrorKind::EditorGone);
    assert_eq!(repo.head_oid(), before);
    assert!(!repo.path().join(".git/rebase-merge").exists());
}

#[tokio::test]
async fn an_editor_that_is_there_but_refuses_is_not_a_vanished_binary() {
    // `there was a problem with the editor` は、在るエディタが自分で断った形
    // （HEAD の照合、plan の無い `--sequence-editor`）でも出る。パスが在れば
    // `EditorGone` にしない（03 の「エラー分類」）。`false` は在って、引数に関わらず
    // 非 0 で返る（macOS にも Debian にも `/usr/bin/false` が在る）。
    let repo = TestRepo::init();
    repo.commit("first");
    repo.commit("second");
    let before = repo.head_oid();

    let err = repo
        .runner_with_mewgit("/usr/bin/false")
        .run(&GitOp::RebaseInteractive {
            onto: "HEAD~1".into(),
            head: before,
            plan: RebasePlan {
                entries: vec![RebaseEntry {
                    action: RebaseAction::Drop,
                    oid: before,
                    summary: "second".into(),
                }],
            },
        })
        .await
        .expect_err("editor が断るので git も断るはずです");

    let ExecError::Failed { stderr, kind, .. } = &err else {
        panic!("git が走っていない: {err:?}");
    };
    assert!(
        // git 2.39 / 2.43 は `There was …` と大文字で書く（03 の「git の最低版」）。
        stderr
            .to_ascii_lowercase()
            .contains("there was a problem with the editor"),
        "{stderr}"
    );
    assert_eq!(*kind, ExecErrorKind::Other);
}

#[tokio::test]
async fn the_configured_git_binary_is_the_one_that_runs() {
    // `config` の `general.git_bin` を渡し忘れていないことの見張り。直書きに
    // 戻すと、ここで本物の git が走って成功してしまう。
    let repo = TestRepo::init();
    repo.commit("first");

    let err = Runner::new(
        repo.path(),
        repo.path().join("no-such-git"),
        SshCommand::default(),
        "mewgit",
    )
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

#[tokio::test]
async fn canceling_kills_the_process_group_and_leaves_no_lock() {
    // **SIGKILL では `index.lock` が残る**（git は SIGTERM でしか後始末をしない）。
    // **git だけに送ると子が孤児として残る**（パスフレーズを訊きに `/dev/tty` を
    // 掴んだ ssh が生き残ると画面が壊れる）。どちらも実測して決めた形なので、
    // ここで両方を見張る（03 の「キャンセル」）。
    let repo = TestRepo::init();
    repo.commit("first");
    let (op, pid_file) = repo.slow_stage();

    let runner = repo.runner();
    let (cancel, canceled) = tokio::sync::oneshot::channel();
    let running = tokio::spawn(async move { runner.run_cancelable(&op, canceled).await });

    let filter = wait_for_pid(&pid_file).await;
    assert!(
        repo.index_lock().exists(),
        "index.lock を握った状態を作れていません"
    );

    cancel.send(()).expect("中断を送れませんでした");
    let err = running
        .await
        .expect("タスクが落ちました")
        .expect_err("中断したのに成功しています");

    assert!(
        matches!(err, ExecError::Canceled { .. }),
        "中断は失敗ではなく Canceled で返す: {err:?}"
    );
    assert_eq!(err.argv(), ["add", "--", "slow.txt"]);
    assert!(!repo.index_lock().exists(), "index.lock が残っています");
    assert!(
        wait_until_gone(filter).await,
        "git の子（フィルタ {filter}）が生き残っています"
    );
}

/// PR の取り出しの舞台（05 の「PR チェックアウト」）。`origin` に `main`・`feat`
/// （同一リポジトリの PR の head）・`refs/pull/7/head`（fork の PR の head）を置く。
struct PrStage {
    remote: BareRepo,
    /// `origin/feat` の先端。
    feat: Oid,
    /// `refs/pull/7/head` の先端。
    pull: Oid,
}

impl PrStage {
    fn new() -> Self {
        let remote = BareRepo::init();
        let author = clone_of(&remote);
        author.commit_file("base.txt", "base");
        author.git(&["push", "-q", "origin", "main"]);
        author.git(&["checkout", "-qb", "feat"]);
        author.commit_file("feat.txt", "feat 1");
        author.commit_file("feat.txt", "feat 2");
        author.git(&["push", "-q", "origin", "feat"]);
        let feat = author.head_oid();
        author.git(&["checkout", "-qb", "fork", "main"]);
        author.commit_file("fork.txt", "fork 1");
        author.commit_file("fork.txt", "fork 2");
        author.git(&["push", "-q", "origin", "fork:refs/pull/7/head"]);
        let pull = author.head_oid();
        Self { remote, feat, pull }
    }

    /// `main` だけを持つクローン。`refs/pull/*` は既定の refspec に入らないので取らない。
    fn clone(&self) -> TestRepo {
        let repo = clone_of(&self.remote);
        repo.git(&["fetch", "-q", "origin"]);
        repo.git(&["checkout", "-qB", "main", "origin/main"]);
        repo
    }
}

fn current_branch(repo: &TestRepo) -> String {
    repo.git(&["symbolic-ref", "--short", "HEAD"])
}

#[tokio::test]
async fn a_same_repo_pr_is_checked_out_tracking_the_remote_branch() {
    let stage = PrStage::new();
    let repo = stage.clone();
    // `--track` を明示していることの見張り。既定の `true` のままだと付けなくても入る。
    repo.git(&["config", "branch.autoSetupMerge", "false"]);

    repo.runner()
        .run(&fake_pr_checkout(PrSource::SameRepo, LocalBranch::Absent))
        .await
        .expect("取り出せませんでした");

    assert_eq!(current_branch(&repo), "feat");
    assert_eq!(repo.head_oid(), stage.feat);
    assert_eq!(repo.git(&["config", "branch.feat.remote"]), "origin");
    assert_eq!(
        repo.git(&["config", "branch.feat.merge"]),
        "refs/heads/feat"
    );
}

#[tokio::test]
async fn a_fork_pr_is_checked_out_from_the_pull_ref_with_the_fork_as_upstream() {
    let stage = PrStage::new();
    let repo = stage.clone();

    repo.runner()
        .run(&fake_pr_checkout(fake_fork_source(), LocalBranch::Absent))
        .await
        .expect("取り出せませんでした");

    assert_eq!(current_branch(&repo), "feat");
    assert_eq!(repo.head_oid(), stage.pull);
    // `gh pr checkout` が書くのと同じ 3 つ（05 の表）。
    assert_eq!(repo.git(&["config", "branch.feat.remote"]), FAKE_FORK_URL);
    assert_eq!(
        repo.git(&["config", "branch.feat.pushRemote"]),
        FAKE_FORK_URL
    );
    assert_eq!(
        repo.git(&["config", "branch.feat.merge"]),
        "refs/heads/feat"
    );
}

/// `feat` が既に在るときの取り出し。`head` は PR の先端（同一リポジトリなら
/// `origin/feat`、fork なら `refs/pull/7/head`）の何本手前にローカルの `feat` を
/// 置くか、`mine` はその上に積む自分のコミットの有無、`on_feat` は `feat` に居るか。
/// 返るのは取り出しの結果と、積んだ自分のコミット。
async fn check_out_existing(
    stage: &PrStage,
    repo: &TestRepo,
    source: PrSource,
    behind: usize,
    mine: bool,
    on_feat: bool,
) -> (Result<(), ExecError>, Option<Oid>) {
    let tip = match source {
        PrSource::SameRepo => stage.feat,
        PrSource::Fork { .. } => stage.pull,
    };
    if matches!(source, PrSource::Fork { .. }) {
        repo.git(&["fetch", "-q", "origin", "refs/pull/7/head"]);
    }
    repo.git(&["branch", "feat", &format!("{tip}~{behind}")]);
    let mine = mine.then(|| {
        repo.git(&["switch", "-q", "feat"]);
        let oid = repo.commit_file("mine.txt", "mine");
        repo.git(&["switch", "-q", "main"]);
        oid
    });
    if on_feat {
        repo.git(&["switch", "-q", "feat"]);
    }
    let result = repo
        .runner()
        .run(&fake_pr_checkout(source, LocalBranch::Present))
        .await
        .map(|_| ());
    (result, mine)
}

#[tokio::test]
async fn an_existing_branch_is_switched_to_and_fast_forwarded_wherever_head_was() {
    // **「いま居る」を一覧から決めない**（05）—— 居ても居なくても同じ手順で、
    // 早送りするのは `feat` だけ。居なかったときに `main` が動いたら、外で HEAD が
    // 動いていた回に別のブランチを早送りする形（`/code-review` が拾った）。
    for source in [PrSource::SameRepo, fake_fork_source()] {
        for on_feat in [false, true] {
            let label = format!("{source:?} on_feat = {on_feat}");
            let stage = PrStage::new();
            let repo = stage.clone();
            let main = repo.head_oid();
            let tip = match source {
                PrSource::SameRepo => stage.feat,
                PrSource::Fork { .. } => stage.pull,
            };

            let (result, _) =
                check_out_existing(&stage, &repo, source.clone(), 1, false, on_feat).await;

            result.unwrap_or_else(|err| panic!("取り出せませんでした（{label}）: {err}"));
            assert_eq!(current_branch(&repo), "feat", "{label}");
            assert_eq!(repo.head_oid(), tip, "{label}");
            assert_eq!(repo.rev_parse("refs/heads/main"), main, "{label}");
            // **在るブランチの追跡設定には触らない**（gh と同じ。05）。
            assert!(!repo.try_git(&["config", "branch.feat.remote"]), "{label}");
        }
    }
}

#[tokio::test]
async fn a_branch_ahead_of_the_pr_is_checked_out_as_it_is() {
    // 自分の PR のブランチに、まだ push していないコミットがある形。**分かれては
    // いない**ので断らない（fetch の `<src>:refs/heads/<b>` で動かしていたころは
    // 非 FF で断っていた。`/code-review` が拾った）。
    for source in [PrSource::SameRepo, fake_fork_source()] {
        let stage = PrStage::new();
        let repo = stage.clone();

        let (result, mine) =
            check_out_existing(&stage, &repo, source.clone(), 0, true, false).await;

        result.unwrap_or_else(|err| panic!("取り出せませんでした（{source:?}）: {err}"));
        assert_eq!(current_branch(&repo), "feat", "{source:?}");
        assert_eq!(Some(repo.head_oid()), mine, "{source:?}");
    }
}

#[tokio::test]
async fn a_diverged_local_branch_stops_the_checkout_without_overwriting_it() {
    // PR の先端の 1 本手前から、自分のコミットを積んだ `feat`。**切り替えたあとの
    // `merge --ff-only` で止まる**（gh と同じ。押した本人が行きたかったブランチに居る）。
    for source in [PrSource::SameRepo, fake_fork_source()] {
        for on_feat in [false, true] {
            let label = format!("{source:?} on_feat = {on_feat}");
            let stage = PrStage::new();
            let repo = stage.clone();

            let (result, mine) =
                check_out_existing(&stage, &repo, source.clone(), 1, true, on_feat).await;

            let err = result.expect_err(&format!("止まるはずです（{label}）"));
            assert_eq!(failure_kind(&err), ExecErrorKind::NonFastForward, "{label}");
            assert_eq!(Some(repo.rev_parse("refs/heads/feat")), mine, "{label}");
            assert_eq!(current_branch(&repo), "feat", "{label}");
        }
    }
}

#[tokio::test]
async fn a_fork_pr_does_not_touch_a_branch_the_list_did_not_know_about() {
    // 一覧が古く（外で `git branch` した）、在る `feat` を「無い」と読んだ形。
    // **作る手順が断る**（`branch` は在れば `already exists`）ので、そのブランチも
    // 追跡設定も動かない（`/code-review` が拾った。fetch の `<src>:refs/heads/<b>` で
    // 作っていたころは早送りして追跡設定を fork へ書き換えていた）。
    let stage = PrStage::new();
    let repo = stage.clone();
    repo.git(&["branch", "--track", "feat", "origin/feat"]);
    let before = repo.rev_parse("refs/heads/feat");

    let err = repo
        .runner()
        .run(&fake_pr_checkout(fake_fork_source(), LocalBranch::Absent))
        .await
        .expect_err("作る手順が断るはずです");

    assert_eq!(err.argv(), ["branch", "feat", "FETCH_HEAD"]);
    assert_eq!(repo.rev_parse("refs/heads/feat"), before);
    assert_eq!(repo.git(&["config", "branch.feat.remote"]), "origin");
    assert_eq!(current_branch(&repo), "main");
}

#[tokio::test]
async fn a_refused_switch_still_leaves_the_fork_upstream_written() {
    // 追跡設定を `switch` より前に書く理由（05）。後に書くと、ここで追跡設定の無い
    // ブランチが残り、次の回は `Present` へ入って二度と書かれない。
    let stage = PrStage::new();
    let repo = stage.clone();
    // PR の head が持つ `fork.txt` を、追跡していないファイルとして置いておく。
    repo.write_worktree_file("fork.txt", b"untracked\n");

    let err = repo
        .runner()
        .run(&fake_pr_checkout(fake_fork_source(), LocalBranch::Absent))
        .await
        .expect_err("switch が断るはずです");

    assert_eq!(err.argv(), ["switch", "feat"]);
    assert_eq!(current_branch(&repo), "main");
    assert_eq!(repo.rev_parse("refs/heads/feat"), stage.pull);
    assert_eq!(repo.git(&["config", "branch.feat.remote"]), FAKE_FORK_URL);
    assert_eq!(
        repo.git(&["config", "branch.feat.merge"]),
        "refs/heads/feat"
    );
}

#[tokio::test]
async fn a_cancel_that_arrives_before_the_checkout_leaves_the_head_alone() {
    // 合図が既に届いていれば、走っている 1 本を止めるか、次の 1 本を起こさない
    // （03 の「複数の手順を持つ書き込み」）。どちらでも最後の `switch` には届かない。
    let stage = PrStage::new();
    let repo = stage.clone();
    let (cancel, canceled) = tokio::sync::oneshot::channel();
    cancel.send(()).expect("送れませんでした");

    let err = repo
        .runner()
        .run_cancelable(
            &fake_pr_checkout(fake_fork_source(), LocalBranch::Absent),
            canceled,
        )
        .await
        .expect_err("中断されるはずです");

    assert!(matches!(err, ExecError::Canceled { .. }), "{err:?}");
    assert_eq!(current_branch(&repo), "main");
}
