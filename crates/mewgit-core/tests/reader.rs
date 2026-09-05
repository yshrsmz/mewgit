//! `git_read::ReaderHandle` の統合テスト。実リポジトリを一時ディレクトリに作って読む。
//!
//! 見たいのは「専用スレッドに置いても `RepoReader` と同じ結果が返るか」と
//! 「ログのセッションがスレッド側で持ち回られるか」の 2 つ。読み取りそのものの
//! 網羅は `git_read.rs` にある。
//!
//! `GitReadError::ReaderGone` はテストしていない。スレッドを外から落とす手段が無く
//! （ハンドルが送信側を持つ限りループは終わらない）、公開 API から到達できないため。

use std::fs;

use mewgit_core::git_read::{GitReadError, HeadInfo, ReaderHandle};
use mewgit_core::test_util::{BareRepo, TestRepo};

/// テストリポジトリに対してリーダースレッドを起こす。
async fn spawn(repo: &TestRepo) -> ReaderHandle {
    ReaderHandle::spawn(repo.path().to_path_buf())
        .await
        .expect("リーダースレッドを起こせませんでした")
}

#[tokio::test]
async fn spawn_reports_the_workdir_found_by_open() {
    let repo = TestRepo::init();
    repo.commit("first");
    let nested = repo.path().join("a/b");
    fs::create_dir_all(&nested).unwrap();

    let reader = ReaderHandle::spawn(nested).await.unwrap();

    // 渡したパスではなく、`open` が見つけたワークツリーのルート。
    assert_eq!(reader.workdir(), repo.path());
}

#[tokio::test]
async fn spawn_fails_on_a_bare_repository() {
    // open の失敗はスレッドの中で起きる。呼び出し側が起動時に気付けること。
    let repo = BareRepo::init();

    let err = ReaderHandle::spawn(repo.path().to_path_buf())
        .await
        .unwrap_err();

    assert!(
        matches!(err, GitReadError::BareRepository { .. }),
        "bare を拒否していません: {err:?}"
    );
}

#[tokio::test]
async fn spawn_fails_outside_a_repository() {
    let dir = tempfile::tempdir().unwrap();

    let err = ReaderHandle::spawn(dir.path().to_path_buf())
        .await
        .unwrap_err();

    assert!(
        matches!(err, GitReadError::Open { .. }),
        "open のエラーになっていません: {err:?}"
    );
}

#[tokio::test]
async fn log_page_continues_where_the_previous_page_ended() {
    let repo = TestRepo::init();
    for i in 0..5 {
        repo.commit(&format!("commit {i}"));
    }
    let reader = spawn(&repo).await;

    let first = reader.log_page(2).await.unwrap();
    let second = reader.log_page(2).await.unwrap();
    let third = reader.log_page(2).await.unwrap();

    // セッションはスレッド側が持ち回る。呼ぶたびに先頭から読み直さない。
    let summaries: Vec<&str> = first
        .commits
        .iter()
        .chain(&second.commits)
        .chain(&third.commits)
        .map(|c| c.summary.as_str())
        .collect();
    assert_eq!(
        summaries,
        ["commit 4", "commit 3", "commit 2", "commit 1", "commit 0"]
    );
    assert!(!first.done);
    assert!(!second.done);
    assert!(third.done, "最後のページで done が立っていません");
}

#[tokio::test]
async fn log_page_on_an_empty_repository_is_done_immediately() {
    let repo = TestRepo::init();
    let reader = spawn(&repo).await;

    let page = reader.log_page(10).await.unwrap();

    assert!(page.commits.is_empty());
    assert!(page.done);
}

#[tokio::test]
async fn head_refs_and_remotes_come_back_through_the_thread() {
    let repo = TestRepo::init();
    let oid = repo.commit("first");
    repo.git(&["tag", "v1.0"]);
    repo.git(&["remote", "add", "origin", "git@github.com:owner/repo.git"]);
    let reader = spawn(&repo).await;

    let head = reader.head().await.unwrap();
    let refs = reader.refs().await.unwrap();
    let remotes = reader.remotes().await.unwrap();

    assert_eq!(
        head,
        HeadInfo::Branch {
            name: "main".to_string(),
            oid
        }
    );
    let labels: Vec<&str> = refs.labels(oid).iter().map(|l| l.name()).collect();
    assert_eq!(labels, ["main", "v1.0"]);
    assert_eq!(remotes.len(), 1);
    assert_eq!(remotes[0].name, "origin");
}

#[tokio::test]
async fn requests_are_served_in_order_from_clones_of_the_handle() {
    // ランタイムは `Cmd` ごとに tokio タスクを起こし、その中からクローンを使う。
    // スレッドは 1 本なので、同時に投げても直列に処理されて取りこぼしが出ない。
    let repo = TestRepo::init();
    for i in 0..4 {
        repo.commit(&format!("commit {i}"));
    }
    let reader = spawn(&repo).await;

    let mut pages = Vec::new();
    for _ in 0..4 {
        let reader = reader.clone();
        pages.push(tokio::spawn(async move { reader.log_page(1).await }));
    }

    let mut summaries = Vec::new();
    for page in pages {
        let page = page.await.unwrap().unwrap();
        summaries.extend(page.commits.into_iter().map(|c| c.summary));
    }
    // 送った順に 1 件ずつ、重複も欠落も無く返る。
    summaries.sort();
    assert_eq!(summaries, ["commit 0", "commit 1", "commit 2", "commit 3"]);
}
