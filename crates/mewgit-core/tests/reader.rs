//! `git_read::ReaderHandle` の統合テスト。実リポジトリを一時ディレクトリに作って読む。
//!
//! 見たいのは「専用スレッドに置いても `RepoReader` と同じ結果が返るか」と
//! 「ログのセッションがスレッド側で持ち回られるか」の 2 つ。読み取りそのものの
//! 網羅は `git_read.rs` にある。
//!
//! `GitReadError::ReaderGone` はテストしていない。スレッドを外から落とす手段が無く
//! （ハンドルが送信側を持つ限りループは終わらない）、公開 API から到達できないため。

use mewgit_core::git_read::{
    DiffTarget, FileStatus, GitReadError, HeadInfo, LogPage, ReaderHandle,
};
use mewgit_core::test_util::{BareRepo, TestRepo};

/// テストリポジトリに対してリーダースレッドを起こす。
fn spawn(repo: &TestRepo) -> ReaderHandle {
    ReaderHandle::spawn(repo.path()).expect("リーダースレッドを起こせませんでした")
}

#[test]
fn spawn_reports_the_workdir_found_by_open() {
    let repo = TestRepo::init();
    repo.commit("first");
    let nested = repo.path().join("a/b");
    std::fs::create_dir_all(&nested).unwrap();

    let reader = ReaderHandle::spawn(&nested).unwrap();

    // 渡したパスではなく、`open` が見つけたワークツリーのルート。
    assert_eq!(reader.workdir(), repo.path());
}

#[test]
fn spawn_fails_without_starting_the_thread() {
    // open の失敗はスレッドを起こす前に出る。呼び出し側が起動時に気付けること。
    let repo = BareRepo::init();

    let err = ReaderHandle::spawn(repo.path()).unwrap_err();

    assert!(
        matches!(err, GitReadError::BareRepository { .. }),
        "bare を拒否していません: {err:?}"
    );
}

#[tokio::test]
async fn log_page_starts_over_when_asked_to_restart() {
    // commit で HEAD が動くと積んだ行が丸ごと古くなる。続きを読む経路では直せない
    // （新しいコミットは末尾ではなく先頭に付く。01 の「ログの読み直し」）。
    let repo = TestRepo::init();
    for i in 0..3 {
        repo.commit(&format!("commit {i}"));
    }
    let reader = spawn(&repo);

    let first = reader.log_page(2, false).await.unwrap();
    assert_eq!(summaries(&first), ["commit 2", "commit 1"]);

    // セッションを持ったままだと続きが返る。
    repo.commit("commit 3");
    let restarted = reader.log_page(2, true).await.unwrap();

    assert_eq!(summaries(&restarted), ["commit 3", "commit 2"]);
}

#[tokio::test]
async fn a_restart_reopens_a_session_that_was_already_read_to_the_end() {
    // 読み切ったセッションは `Exhausted` として持たれ、空のページを返し続ける。
    // 読み直しはそこからも先頭に戻れること。
    let repo = TestRepo::init();
    repo.commit("only");
    let reader = spawn(&repo);

    assert!(reader.log_page(10, false).await.unwrap().done);
    // 読み切ったらセッションを手放すが、次の要求で履歴全体を読み直したりはしない。
    let after = reader.log_page(10, false).await.unwrap();
    assert!(after.commits.is_empty());
    assert!(after.done, "読み切ったあとも done のまま");

    let restarted = reader.log_page(10, true).await.unwrap();

    assert_eq!(summaries(&restarted), ["only"]);
}

#[tokio::test]
async fn head_message_is_read_on_the_reader_thread() {
    let repo = TestRepo::init();
    repo.commit("subject");
    let reader = spawn(&repo);

    assert_eq!(
        reader.head_message().await.unwrap().as_deref(),
        Some("subject")
    );
}

/// ページの summary。
fn summaries(page: &LogPage) -> Vec<&str> {
    page.commits.iter().map(|c| c.summary.as_str()).collect()
}

#[tokio::test]
async fn log_page_continues_where_the_previous_page_ended() {
    let repo = TestRepo::init();
    for i in 0..5 {
        repo.commit(&format!("commit {i}"));
    }
    let reader = spawn(&repo);

    let first = reader.log_page(2, false).await.unwrap();
    let second = reader.log_page(2, false).await.unwrap();
    let third = reader.log_page(2, false).await.unwrap();

    // セッションはスレッド側が持ち回る。呼ぶたびに先頭から読み直さない。
    let mut all = summaries(&first);
    all.extend(summaries(&second));
    all.extend(summaries(&third));
    assert_eq!(
        all,
        ["commit 4", "commit 3", "commit 2", "commit 1", "commit 0"]
    );
    assert!(!first.done);
    assert!(!second.done);
    assert!(third.done, "最後のページで done が立っていません");
}

#[tokio::test]
async fn log_page_on_an_empty_repository_is_done_immediately() {
    let repo = TestRepo::init();
    let reader = spawn(&repo);

    let page = reader.log_page(10, false).await.unwrap();

    assert!(page.commits.is_empty());
    assert!(page.done);
}

#[tokio::test]
async fn head_refs_and_remotes_come_back_through_the_thread() {
    let repo = TestRepo::init();
    let oid = repo.commit("first");
    repo.git(&["tag", "v1.0"]);
    repo.git(&["remote", "add", "origin", "git@github.com:owner/repo.git"]);
    let reader = spawn(&repo);

    let head = reader.branches().await.unwrap().head;
    let refs = reader.refs().await.unwrap();
    let remotes = reader.remotes().await.unwrap();

    assert_eq!(head, HeadInfo::Branch);
    let labels: Vec<&str> = refs.labels(oid).iter().map(|l| l.name()).collect();
    assert_eq!(labels, ["main", "v1.0"]);
    assert_eq!(remotes.len(), 1);
    assert_eq!(remotes[0].name, "origin");
}

#[tokio::test]
async fn commit_detail_comes_back_through_the_thread() {
    let repo = TestRepo::init();
    repo.commit("subject");
    repo.git(&["commit", "--amend", "-q", "-m", "subject\n\nbody"]);
    let oid = repo.head_oid();
    let reader = spawn(&repo);

    let detail = reader.commit_detail(oid).await.unwrap();

    assert_eq!(detail.oid, oid);
    assert_eq!(detail.message, "subject\n\nbody");
}

#[tokio::test]
async fn commit_detail_does_not_disturb_the_log_session() {
    // ログのセッションはスレッドのローカル変数。他の要求が挟まっても続きから読める。
    let repo = TestRepo::init();
    let first = repo.commit("first");
    let second = repo.commit("second");
    let reader = spawn(&repo);

    let page = reader.log_page(1, false).await.unwrap();
    assert_eq!(page.commits[0].oid, second);

    let _ = reader.commit_detail(second).await.unwrap();

    let page = reader.log_page(1, false).await.unwrap();
    assert_eq!(page.commits[0].oid, first);
}

#[tokio::test]
async fn concurrent_requests_from_clones_lose_nothing() {
    // ランタイムは `Cmd` ごとに tokio タスクを起こし、その中からクローンを使う。
    // スレッドは 1 本なので、同時に投げても直列に処理されて取りこぼしが出ない。
    // 到着順はタスクの起動順に依存するので、順序ではなく集合で見る。
    let repo = TestRepo::init();
    for i in 0..4 {
        repo.commit(&format!("commit {i}"));
    }
    let reader = spawn(&repo);

    let mut pages = Vec::new();
    for _ in 0..4 {
        let reader = reader.clone();
        pages.push(tokio::spawn(async move { reader.log_page(1, false).await }));
    }

    let mut summaries = Vec::new();
    for page in pages {
        let page = page.await.unwrap().unwrap();
        summaries.extend(page.commits.into_iter().map(|c| c.summary));
    }
    summaries.sort();
    assert_eq!(summaries, ["commit 0", "commit 1", "commit 2", "commit 3"]);
}

#[tokio::test]
async fn status_and_diff_come_back_through_the_thread() {
    // 状態を持たない要求なので、見たいのは「スレッド越しでも同じ結果か」だけ。
    // 分類とハンク分割の網羅は `status.rs` / `diff.rs` にある。
    let repo = TestRepo::init();
    repo.commit_file("file.txt", "one");
    repo.write_worktree_file("file.txt", b"two\n");
    let reader = spawn(&repo);

    let status = reader.status().await.unwrap();
    assert_eq!(status.unstaged.len(), 1);
    assert_eq!(status.unstaged[0].status, FileStatus::Modified);

    let diff = reader
        .diff(DiffTarget::IndexVsWorktree(None))
        .await
        .unwrap();
    assert_eq!(diff.len(), 1);
    assert_eq!(diff[0].path, status.unstaged[0].path);
    assert_eq!(diff[0].hunks.len(), 1);
}

#[tokio::test]
async fn diff_reports_a_missing_commit_as_an_error() {
    // 失敗の通り道が無いと、ランタイムの `JobKind::LoadDiff` が走ったまま残る。
    let repo = TestRepo::init();
    repo.commit("first");
    let reader = spawn(&repo);

    let missing = mewgit_core::test_util::fake_oid(7);
    let err = reader
        .diff(DiffTarget::Commit {
            oid: missing,
            path: None,
        })
        .await
        .unwrap_err();

    assert!(
        matches!(err, GitReadError::Git(_)),
        "git のエラーになっていません: {err:?}"
    );
}
