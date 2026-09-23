//! `git_exec::Runner` の統合テスト。実リポジトリを一時ディレクトリに作って `git` を叩く。
//!
//! argv の形は `git_exec::argv` のユニットテストが見張る。ここで見るのは
//! 「その argv を実際の git に渡すと意図した結果になるか」。

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use mewgit_core::auth::ssh::SshCommand;
use mewgit_core::auth::{resolve, DegradedReason};
use mewgit_core::config::Config;
use mewgit_core::git_exec::{
    ExecError, ExecErrorKind, ForkUpstream, GitOp, LocalBranch, PrCheckout, PrSource, Runner,
};
use mewgit_core::git_read::Oid;
use mewgit_core::test_util::{
    fake_target, wait_for_pid, wait_until_gone, BareRepo, Fake, TestRepo,
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

    let runner = Runner::new(repo.path(), "git", SshCommand::Shell(ssh_command.clone()))
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

    let err = Runner::new(
        repo.path(),
        repo.path().join("no-such-git"),
        SshCommand::default(),
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

const FORK_URL: &str = "git@github.com:bob/app.git";

fn checkout_pr(source: PrSource, local: LocalBranch) -> GitOp {
    GitOp::CheckoutPr(Box::new(PrCheckout {
        remote: "origin".into(),
        branch: "feat".into(),
        source,
        local,
    }))
}

fn from_fork() -> PrSource {
    PrSource::Fork {
        number: 7,
        upstream: ForkUpstream {
            remote: FORK_URL.into(),
            merge: "refs/heads/feat".into(),
        },
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
        .run(&checkout_pr(PrSource::SameRepo, LocalBranch::Absent))
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
async fn a_same_repo_pr_fast_forwards_the_local_branch_whether_or_not_it_is_checked_out() {
    for current in [false, true] {
        let stage = PrStage::new();
        let repo = stage.clone();
        // 1 つ手前で止まっているローカルの `feat`。
        repo.git(&["branch", "feat", &format!("{}~1", stage.feat)]);
        let local = if current {
            repo.git(&["switch", "-q", "feat"]);
            LocalBranch::Current
        } else {
            LocalBranch::Present
        };

        repo.runner()
            .run(&checkout_pr(PrSource::SameRepo, local))
            .await
            .unwrap_or_else(|err| panic!("取り出せませんでした（current = {current}）: {err}"));

        assert_eq!(current_branch(&repo), "feat", "current = {current}");
        assert_eq!(repo.head_oid(), stage.feat, "current = {current}");
    }
}

#[tokio::test]
async fn a_fork_pr_is_checked_out_from_the_pull_ref_with_the_fork_as_upstream() {
    let stage = PrStage::new();
    let repo = stage.clone();

    repo.runner()
        .run(&checkout_pr(from_fork(), LocalBranch::Absent))
        .await
        .expect("取り出せませんでした");

    assert_eq!(current_branch(&repo), "feat");
    assert_eq!(repo.head_oid(), stage.pull);
    // `gh pr checkout` が書くのと同じ 3 つ（05 の表）。
    assert_eq!(repo.git(&["config", "branch.feat.remote"]), FORK_URL);
    assert_eq!(repo.git(&["config", "branch.feat.pushRemote"]), FORK_URL);
    assert_eq!(
        repo.git(&["config", "branch.feat.merge"]),
        "refs/heads/feat"
    );
}

#[tokio::test]
async fn a_fork_pr_fast_forwards_an_existing_branch_whether_or_not_it_is_checked_out() {
    for current in [false, true] {
        let stage = PrStage::new();
        let repo = stage.clone();
        repo.git(&["fetch", "-q", "origin", "refs/pull/7/head"]);
        repo.git(&["branch", "feat", "FETCH_HEAD~1"]);
        let local = if current {
            repo.git(&["switch", "-q", "feat"]);
            LocalBranch::Current
        } else {
            LocalBranch::Present
        };

        repo.runner()
            .run(&checkout_pr(from_fork(), local))
            .await
            .unwrap_or_else(|err| panic!("取り出せませんでした（current = {current}）: {err}"));

        assert_eq!(current_branch(&repo), "feat", "current = {current}");
        assert_eq!(repo.head_oid(), stage.pull, "current = {current}");
        // **在るブランチの追跡設定には触らない**（gh と同じ。05）。
        assert!(
            !repo.try_git(&["config", "branch.feat.remote"]),
            "current = {current}"
        );
    }
}

#[tokio::test]
async fn a_diverged_local_branch_stops_the_checkout_without_overwriting_it() {
    for (source, current) in [
        (PrSource::SameRepo, false),
        (PrSource::SameRepo, true),
        (from_fork(), false),
        (from_fork(), true),
    ] {
        let label = format!("{source:?} current = {current}");
        let stage = PrStage::new();
        let repo = stage.clone();
        // PR の head にも無いコミットを 1 つ持った `feat`。
        repo.git(&["switch", "-qc", "feat"]);
        let mine = repo.commit_file("mine.txt", "mine");
        if !current {
            repo.git(&["switch", "-q", "main"]);
        }
        let local = if current {
            LocalBranch::Current
        } else {
            LocalBranch::Present
        };

        let err = repo
            .runner()
            .run(&checkout_pr(source, local))
            .await
            .expect_err(&format!("止まるはずです（{label}）"));

        assert_eq!(failure_kind(&err), ExecErrorKind::NonFastForward, "{label}");
        assert_eq!(repo.rev_parse("refs/heads/feat"), mine, "{label}");
        let expected_head = if current { "feat" } else { "main" };
        assert_eq!(current_branch(&repo), expected_head, "{label}");
    }
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
        .run(&checkout_pr(from_fork(), LocalBranch::Absent))
        .await
        .expect_err("switch が断るはずです");

    assert_eq!(err.argv(), ["switch", "feat"]);
    assert_eq!(current_branch(&repo), "main");
    assert_eq!(repo.rev_parse("refs/heads/feat"), stage.pull);
    assert_eq!(repo.git(&["config", "branch.feat.remote"]), FORK_URL);
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
        .run_cancelable(&checkout_pr(from_fork(), LocalBranch::Absent), canceled)
        .await
        .expect_err("中断されるはずです");

    assert!(matches!(err, ExecError::Canceled { .. }), "{err:?}");
    assert_eq!(current_branch(&repo), "main");
}
