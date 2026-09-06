//! `git_read::RepoReader::status` の統合テスト。実リポジトリを一時ディレクトリに作って読む。

use std::fs;

use mewgit_core::git_read::{FileEntry, FileStatus, RepoReader, WorktreeSection};
use mewgit_core::test_util::TestRepo;

/// 区画の中身を `(path, old_path, status)` の並びにする。
fn rows(entries: &[FileEntry]) -> Vec<(&str, Option<&str>, FileStatus)> {
    entries
        .iter()
        .map(|entry| {
            (
                entry.path.to_str().expect("パスが UTF-8 ではありません"),
                entry
                    .old_path
                    .as_deref()
                    .map(|path| path.to_str().expect("旧パスが UTF-8 ではありません")),
                entry.status,
            )
        })
        .collect()
}

#[test]
fn status_is_empty_in_a_clean_repository() {
    let repo = TestRepo::init();
    repo.commit("first");
    let reader = RepoReader::open(repo.path()).unwrap();

    assert_eq!(reader.status().unwrap(), Default::default());
}

#[test]
fn status_lists_untracked_files_inside_a_new_directory() {
    let repo = TestRepo::init();
    repo.commit("first");
    fs::create_dir_all(repo.path().join("a/b")).unwrap();
    repo.write_worktree_file("a/b/deep.txt", b"deep\n");
    repo.write_worktree_file("top.txt", b"top\n");
    let reader = RepoReader::open(repo.path()).unwrap();

    let status = reader.status().unwrap();

    // `recurse_untracked_dirs` が効いていないと `a/` の 1 行にまとまる。
    assert_eq!(
        rows(&status.untracked),
        [
            ("a/b/deep.txt", None, FileStatus::Untracked),
            ("top.txt", None, FileStatus::Untracked),
        ]
    );
    assert_eq!(rows(&status.unstaged), []);
}

#[test]
fn status_puts_a_partially_staged_file_in_both_sections() {
    let repo = TestRepo::init();
    repo.commit("first");
    repo.write_worktree_file("file.txt", b"staged\n");
    repo.git(&["add", "file.txt"]);
    repo.write_worktree_file("file.txt", b"staged then edited again\n");
    let reader = RepoReader::open(repo.path()).unwrap();

    let status = reader.status().unwrap();

    // 一部だけ stage した状態の正しい表現なので、どちらかに寄せない。
    assert_eq!(
        rows(&status.staged),
        [("file.txt", None, FileStatus::Modified)]
    );
    assert_eq!(
        rows(&status.unstaged),
        [("file.txt", None, FileStatus::Modified)]
    );
}

#[test]
fn status_reports_deleted_files_on_both_sides() {
    let repo = TestRepo::init();
    repo.commit_file("kept.txt", "kept");
    repo.commit_file("staged.txt", "staged");
    repo.commit_file("worktree.txt", "worktree");
    repo.git(&["rm", "-q", "staged.txt"]);
    fs::remove_file(repo.path().join("worktree.txt")).unwrap();
    let reader = RepoReader::open(repo.path()).unwrap();

    let status = reader.status().unwrap();

    assert_eq!(
        rows(&status.staged),
        [("staged.txt", None, FileStatus::Deleted)]
    );
    assert_eq!(
        rows(&status.unstaged),
        [("worktree.txt", None, FileStatus::Deleted)]
    );
}

#[test]
fn status_reports_a_staged_rename_with_both_paths() {
    let repo = TestRepo::init();
    repo.commit_file("old.txt", "one\ntwo\nthree\nfour\nfive");
    repo.git(&["mv", "old.txt", "new.txt"]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let status = reader.status().unwrap();

    // `StatusEntry::path()` は rename のとき旧パスを返すので、そのまま使うと
    // 一覧に新しい名前が出ない。
    assert_eq!(
        rows(&status.staged),
        [("new.txt", Some("old.txt"), FileStatus::Renamed)]
    );
}

#[test]
fn status_puts_a_conflicted_file_only_in_the_conflicted_section() {
    let repo = TestRepo::init();
    repo.commit_file("conflict.txt", "base");
    repo.git(&["checkout", "-q", "-b", "topic"]);
    repo.commit_file("conflict.txt", "topic");
    repo.git(&["checkout", "-q", "main"]);
    repo.commit_file("conflict.txt", "main");
    assert!(
        !repo.try_git(&["merge", "topic"]),
        "マージがコンフリクトしていません"
    );
    let reader = RepoReader::open(repo.path()).unwrap();

    let status = reader.status().unwrap();

    assert_eq!(
        rows(&status.conflicted),
        [("conflict.txt", None, FileStatus::Conflicted)]
    );
    // CONFLICTED は他のビットと同時に立つので、素直に分類すると両方に出る。
    assert_eq!(rows(&status.staged), []);
    assert_eq!(rows(&status.unstaged), []);
}

#[test]
fn status_drops_a_file_whose_path_is_not_utf8() {
    let repo = TestRepo::init();
    repo.commit("first");
    // index にだけ入れると「staged で追加 / ワークツリーからは消えている」になり、
    // staged 側と unstaged 側の両方の経路を 1 回で踏める。
    repo.add_raw_path_to_index(b"bad\xff.txt");
    let reader = RepoReader::open(repo.path()).unwrap();

    let status = reader.status().unwrap();

    // `git_exec` の argv に載せられないので、選べても操作できない。
    assert_eq!(status, Default::default());
}

#[test]
fn status_sorts_each_section_by_path() {
    let repo = TestRepo::init();
    repo.commit("first");
    for name in ["c.txt", "a.txt", "b.txt"] {
        repo.write_worktree_file(name, b"x\n");
    }
    let reader = RepoReader::open(repo.path()).unwrap();

    let status = reader.status().unwrap();

    // libgit2 の列挙順には依存しない。
    let paths: Vec<&str> = status
        .section(WorktreeSection::Untracked)
        .iter()
        .map(|entry| entry.path.to_str().unwrap())
        .collect();
    assert_eq!(paths, ["a.txt", "b.txt", "c.txt"]);
}
