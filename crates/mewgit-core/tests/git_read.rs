//! `git_read::RepoReader` の統合テスト。実リポジトリを一時ディレクトリに作って読む。

mod common;

use std::fs;

use common::TestRepo;
use mewgit_core::git_read::{GitReadError, HeadInfo, Oid, RefLabel, RepoReader};

#[test]
fn open_finds_the_repository_from_a_subdirectory() {
    let repo = TestRepo::init();
    repo.commit("first");
    let nested = repo.path().join("a/b");
    fs::create_dir_all(&nested).unwrap();

    let reader = RepoReader::open(&nested).unwrap();

    // 渡したパスではなく、見つかったワークツリーのルートを返す。
    assert_eq!(reader.workdir(), repo.path());
}

#[test]
fn open_rejects_a_bare_repository() {
    let repo = TestRepo::init_bare();

    let err = RepoReader::open(repo.path()).unwrap_err();

    assert!(
        matches!(err, GitReadError::BareRepository { .. }),
        "bare を拒否していません: {err:?}"
    );
}

#[test]
fn open_fails_outside_a_repository() {
    // `discover` は上に辿るので、ワークスペース内ではなく一時ディレクトリで試す。
    let dir = tempfile::tempdir().unwrap();

    let err = RepoReader::open(dir.path()).unwrap_err();

    assert!(
        matches!(err, GitReadError::Open { .. }),
        "open のエラーになっていません: {err:?}"
    );
    // どこを開こうとして失敗したかがメッセージに出る。
    assert!(err.to_string().contains(&dir.path().display().to_string()));
}

#[test]
fn remotes_returns_every_remote_sorted_by_name() {
    let repo = TestRepo::init();
    repo.git(&[
        "remote",
        "add",
        "upstream",
        "git@github.com:upstream/mewgit.git",
    ]);
    repo.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/me/mewgit.git",
    ]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let remotes = reader.remotes().unwrap();

    let pairs: Vec<(&str, &str)> = remotes
        .iter()
        .map(|r| (r.name.as_str(), r.url.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("origin", "https://github.com/me/mewgit.git"),
            ("upstream", "git@github.com:upstream/mewgit.git"),
        ]
    );
}

#[test]
fn remotes_is_empty_without_any_remote() {
    let repo = TestRepo::init();
    let reader = RepoReader::open(repo.path()).unwrap();

    assert_eq!(reader.remotes().unwrap(), []);
}

#[test]
fn refs_labels_local_branches_and_marks_head() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    repo.git(&["branch", "topic"]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let index = reader.refs().unwrap();

    assert_eq!(
        index.labels(first),
        [
            RefLabel::Local {
                name: "main".to_string(),
                is_head: true,
            },
            RefLabel::Local {
                name: "topic".to_string(),
                is_head: false,
            },
        ]
    );
}

#[test]
fn refs_peels_annotated_tags_to_the_commit() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    repo.git(&["tag", "light"]);
    // annotated tag は tag オブジェクトを指すので、peel しないとこの行に乗らない。
    repo.git(&["tag", "-a", "heavy", "-m", "annotated"]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let index = reader.refs().unwrap();

    let tags: Vec<&str> = index
        .labels(first)
        .iter()
        .filter(|label| matches!(label, RefLabel::Tag { .. }))
        .map(RefLabel::name)
        .collect();
    assert_eq!(tags, ["heavy", "light"]);
}

#[test]
fn refs_keeps_remote_branches_without_duplicating_the_symbolic_head() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    // clone せずにリモート追跡 ref だけを作る。
    repo.git(&["update-ref", "refs/remotes/origin/main", &first.to_string()]);
    repo.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let index = reader.refs().unwrap();

    // origin/HEAD を解決すると origin/main が 2 つになる。
    let remotes: Vec<&str> = index
        .labels(first)
        .iter()
        .filter(|label| matches!(label, RefLabel::Remote { .. }))
        .map(RefLabel::name)
        .collect();
    assert_eq!(remotes, ["origin/main"]);
}

#[test]
fn refs_are_sorted_by_kind_then_name() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    repo.git(&["branch", "alpha"]);
    repo.git(&["tag", "v2"]);
    repo.git(&["tag", "v1"]);
    repo.git(&["update-ref", "refs/remotes/origin/main", &first.to_string()]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let index = reader.refs().unwrap();

    let names: Vec<&str> = index.labels(first).iter().map(RefLabel::name).collect();
    assert_eq!(names, ["alpha", "main", "origin/main", "v1", "v2"]);
}

#[test]
fn refs_ignores_the_stash() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    fs::write(repo.path().join("file.txt"), "dirty\n").unwrap();
    repo.git(&["stash", "push", "-m", "wip"]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let index = reader.refs().unwrap();

    // stash コミットは first を親に持つ別コミットなので、first の行にも自分の行にも
    // ラベルは付かない（stash は stash ペインの担当）。
    let stash = repo.rev_parse("refs/stash");
    assert_eq!(index.labels(stash), []);
    let names: Vec<&str> = index.labels(first).iter().map(RefLabel::name).collect();
    assert_eq!(names, ["main"]);
}

#[test]
fn refs_skips_a_tag_that_cannot_be_peeled_to_a_commit() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    // tree を直接指すタグ。commit まで辿れないのでグラフのどの行にも乗らない。
    repo.git(&["tag", "treetag", "HEAD^{tree}"]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let index = reader.refs().unwrap();

    let names: Vec<&str> = index.labels(first).iter().map(RefLabel::name).collect();
    assert_eq!(names, ["main"]);
    assert_eq!(index.labels(repo.rev_parse("HEAD^{tree}")), []);
}

#[test]
fn refs_marks_no_branch_as_head_when_detached() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    repo.git(&["checkout", "--detach", &first.to_string()]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let index = reader.refs().unwrap();

    // HEAD が symbolic でないので、どのブランチも HEAD ではない。
    assert_eq!(
        index.labels(first),
        [RefLabel::Local {
            name: "main".to_string(),
            is_head: false,
        }]
    );
}

#[test]
fn refs_is_empty_before_the_first_commit() {
    let repo = TestRepo::init();
    let reader = RepoReader::open(repo.path()).unwrap();

    let index = reader.refs().unwrap();

    assert_eq!(index.labels(Oid::zero()), []);
}

#[test]
fn head_reports_the_current_branch() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    let reader = RepoReader::open(repo.path()).unwrap();

    assert_eq!(
        reader.head().unwrap(),
        HeadInfo::Branch {
            name: "main".to_string(),
            oid: first,
        }
    );
}

#[test]
fn head_reports_a_detached_head() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    repo.commit("second");
    repo.git(&["checkout", "--detach", &first.to_string()]);
    let reader = RepoReader::open(repo.path()).unwrap();

    assert_eq!(reader.head().unwrap(), HeadInfo::Detached(first));
}

#[test]
fn head_reports_an_unborn_branch() {
    // コミットが 1 つも無いと `repo.head()` は失敗する。
    let repo = TestRepo::init();
    let reader = RepoReader::open(repo.path()).unwrap();

    assert_eq!(
        reader.head().unwrap(),
        HeadInfo::Unborn {
            name: "main".to_string(),
        }
    );
}
