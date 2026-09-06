//! ハンク単位の書き込みの round-trip。`git_read::diff` で読んだハンクを
//! `git_exec::hunk_patch` でパッチに戻し、実際の `git apply` に通す。
//!
//! **ここで見るのは「戻したパッチが元の変更と同じものを表しているか」。** パッチの
//! 形は `git_exec::patch` のユニットテストが、argv は `git_exec::argv` が見張る。
//! 形が正しくても git が受け取らない・別の場所に当たる、が起きうるので実際に叩く。

use std::path::Path;

use mewgit_core::git_exec::{hunk_patch, GitOp};
use mewgit_core::git_read::{DiffTarget, FileDiff, Hunk, RepoReader};
use mewgit_core::test_util::TestRepo;

/// `target` の diff を 1 ファイルぶん読む。
fn one_file(repo: &TestRepo, target: DiffTarget) -> FileDiff {
    let reader = RepoReader::open(repo.path()).expect("リポジトリを開けませんでした");
    let mut diff = reader.diff(target).expect("diff を読めませんでした");
    assert_eq!(diff.len(), 1, "1 ファイルに絞れていません: {diff:?}");
    diff.remove(0)
}

/// `file` の `index` 番目のハンクだけを含むパッチ。
fn patch_of(file: &FileDiff, index: usize) -> Vec<u8> {
    let hunk: &Hunk = file
        .hunks
        .get(index)
        .unwrap_or_else(|| panic!("ハンクが足りません: {:?}", file.hunks));
    hunk_patch(&file.path, hunk).expect("パッチを組めませんでした")
}

/// ワークツリーの中身。
fn worktree(repo: &TestRepo, path: &str) -> Vec<u8> {
    std::fs::read(repo.path().join(path)).expect("ファイルを読めませんでした")
}

/// index の中身。**末尾の改行まで含めて生バイト列で取る**（`TestRepo::git` は
/// 末尾の改行を落とすので、CRLF や「改行で終わらない」を見るのに使えない）。
fn staged(repo: &TestRepo, path: &str) -> Vec<u8> {
    repo.git_bytes(&["show", &format!(":{path}")])
}

/// [`staged`] を読みやすさのため lossy で見たもの。中身の一部を探すのに使う。
fn staged_text(repo: &TestRepo, path: &str) -> String {
    String::from_utf8_lossy(&staged(repo, path)).into_owned()
}

/// 20 行のファイル。離れた 2 箇所を変えるとハンクが 2 つになる。
fn twenty_lines() -> Vec<String> {
    (1..=20).map(|n| format!("line {n}\n")).collect()
}

/// 20 行のうち 3 行目と 15 行目を差し替えたもの。**元の 20 行から作る。**
/// 別に並べ直すと、行の中身を変えたときに 2 箇所直すことになる。
fn twenty_lines_edited() -> Vec<u8> {
    let mut lines = twenty_lines();
    lines[2] = "THREE\n".to_string();
    lines[14] = "FIFTEEN\n".to_string();
    lines.concat().into_bytes()
}

/// 2 ハンクのワークツリー変更を持つリポジトリ。
fn two_hunk_repo() -> TestRepo {
    let repo = TestRepo::init();
    repo.commit_bytes("a.txt", twenty_lines().concat().as_bytes(), "first");
    repo.write_worktree_file("a.txt", &twenty_lines_edited());
    repo
}

#[tokio::test]
async fn staging_only_the_second_hunk_leaves_the_first_unstaged() {
    let repo = two_hunk_repo();
    let file = one_file(&repo, DiffTarget::IndexVsWorktree(Some("a.txt".into())));
    assert_eq!(file.hunks.len(), 2, "ハンクが 2 つに割れていません");

    // **2 番目のハンクだけ。** 1 番目を飛ばすので、パッチのヘッダが指す位置が
    // そのまま使えるかどうかがここで出る。
    repo.runner()
        .run(&GitOp::StageHunk {
            path: "a.txt".into(),
            patch: patch_of(&file, 1),
        })
        .await
        .expect("stage できませんでした");

    // staged 側は 15 行目だけ、ワークツリー側は 3 行目だけが残る。
    assert_eq!(repo.porcelain(), "MM a.txt");
    let index = staged_text(&repo, "a.txt");
    assert!(index.contains("FIFTEEN"), "{index}");
    assert!(
        index.contains("line 3\n"),
        "1 番目のハンクまで入っている: {index}"
    );
    // ワークツリーは触られていない。
    assert_eq!(worktree(&repo, "a.txt"), twenty_lines_edited());
}

#[tokio::test]
async fn unstaging_only_the_second_hunk_leaves_the_first_staged() {
    let repo = two_hunk_repo();
    repo.git(&["add", "a.txt"]);
    let file = one_file(&repo, DiffTarget::HeadVsIndex(Some("a.txt".into())));
    assert_eq!(file.hunks.len(), 2, "ハンクが 2 つに割れていません");

    repo.runner()
        .run(&GitOp::UnstageHunk {
            path: "a.txt".into(),
            patch: patch_of(&file, 1),
        })
        .await
        .expect("unstage できませんでした");

    assert_eq!(repo.porcelain(), "MM a.txt");
    let index = staged_text(&repo, "a.txt");
    assert!(
        index.contains("THREE"),
        "1 番目のハンクまで戻っている: {index}"
    );
    assert!(index.contains("line 15\n"), "{index}");
    // ワークツリーは触られていない。
    assert_eq!(worktree(&repo, "a.txt"), twenty_lines_edited());
}

#[tokio::test]
async fn discarding_only_the_second_hunk_keeps_the_first_and_the_index() {
    let repo = two_hunk_repo();
    // 3 行目だけを stage しておく。`--cached` が付いていると、ここが巻き添えになる。
    let staged_hunk = {
        let file = one_file(&repo, DiffTarget::IndexVsWorktree(Some("a.txt".into())));
        patch_of(&file, 0)
    };
    repo.runner()
        .run(&GitOp::StageHunk {
            path: "a.txt".into(),
            patch: staged_hunk,
        })
        .await
        .expect("stage できませんでした");

    let file = one_file(&repo, DiffTarget::IndexVsWorktree(Some("a.txt".into())));
    assert_eq!(file.hunks.len(), 1, "unstaged なハンクは 1 つのはず");
    repo.runner()
        .run(&GitOp::DiscardHunk {
            path: "a.txt".into(),
            patch: patch_of(&file, 0),
        })
        .await
        .expect("discard できませんでした");

    // ワークツリーから 15 行目の変更だけが消え、3 行目の変更は index にもワークツリーにも残る。
    assert_eq!(repo.porcelain(), "M  a.txt");
    let content = String::from_utf8(worktree(&repo, "a.txt")).expect("UTF-8 ではありません");
    assert!(content.contains("THREE"), "{content}");
    assert!(content.contains("line 15\n"), "{content}");
    assert!(staged_text(&repo, "a.txt").contains("THREE"));
}

#[tokio::test]
async fn a_file_without_a_trailing_newline_keeps_its_missing_newline() {
    let repo = TestRepo::init();
    repo.commit_bytes("n.txt", b"a\nb\nc", "first");
    repo.write_worktree_file("n.txt", b"a\nb\nC");

    let file = one_file(&repo, DiffTarget::IndexVsWorktree(Some("n.txt".into())));
    repo.runner()
        .run(&GitOp::StageHunk {
            path: "n.txt".into(),
            patch: patch_of(&file, 0),
        })
        .await
        .expect("stage できませんでした");

    // `\ No newline at end of file` を落とすと、ここに `\n` が足される。
    assert_eq!(staged(&repo, "n.txt"), b"a\nb\nC");
}

#[tokio::test]
async fn crlf_lines_survive_the_round_trip() {
    let repo = TestRepo::init();
    repo.commit_bytes("crlf.txt", b"one\r\ntwo\r\n", "first");
    repo.write_worktree_file("crlf.txt", b"one\r\nTWO\r\n");

    let file = one_file(&repo, DiffTarget::IndexVsWorktree(Some("crlf.txt".into())));
    repo.runner()
        .run(&GitOp::StageHunk {
            path: "crlf.txt".into(),
            patch: patch_of(&file, 0),
        })
        .await
        .expect("stage できませんでした");

    // `\r` を落とすと `one\nTWO\n` になる。
    assert_eq!(staged(&repo, "crlf.txt"), b"one\r\nTWO\r\n");
    assert_eq!(repo.porcelain(), "M  crlf.txt");
}

#[tokio::test]
async fn non_utf8_lines_survive_the_round_trip() {
    let repo = TestRepo::init();
    repo.commit_bytes("bin.txt", b"caf\xe9\nnext\n", "first");
    repo.write_worktree_file("bin.txt", b"caf\xe8\nnext\n");

    let file = one_file(&repo, DiffTarget::IndexVsWorktree(Some("bin.txt".into())));
    assert!(!file.binary, "テキストとして扱われていません");
    repo.runner()
        .run(&GitOp::StageHunk {
            path: "bin.txt".into(),
            patch: patch_of(&file, 0),
        })
        .await
        .expect("stage できませんでした");

    // lossy 変換すると `caf\xef\xbf\xbd` になる。
    assert_eq!(repo.porcelain(), "M  bin.txt");
    assert_eq!(staged(&repo, "bin.txt"), b"caf\xe8\nnext\n");
}

#[tokio::test]
async fn the_users_apply_whitespace_setting_does_not_change_what_gets_staged() {
    // `apply.whitespace = fix` は git に「行末の空白を落としてから当てる」と言う設定で、
    // よく勧められる。**それが効くと、画面に出したハンクと index の中身が食い違う。**
    let repo = TestRepo::init();
    repo.commit_bytes("f.txt", b"one\ntwo\n", "first");
    repo.write_worktree_file("f.txt", b"one\nTWO   \n");
    repo.git(&["config", "apply.whitespace", "fix"]);

    let file = one_file(&repo, DiffTarget::IndexVsWorktree(Some("f.txt".into())));
    repo.runner()
        .run(&GitOp::StageHunk {
            path: "f.txt".into(),
            patch: patch_of(&file, 0),
        })
        .await
        .expect("stage できませんでした");

    // 空白が落ちると staged は `TWO\n` になり、ワークツリーとの差が残って `MM`。
    assert_eq!(staged(&repo, "f.txt"), b"one\nTWO   \n");
    assert_eq!(repo.porcelain(), "M  f.txt");
}

#[tokio::test]
async fn paths_with_a_space_or_non_ascii_apply_to_the_right_file() {
    // `git diff` は非 ASCII を quote して出すが、`hunk_patch` は quote しない。
    // 受け側がそれを読めるか、しかも**別のファイルに当たらないか**を見る。
    let repo = TestRepo::init();
    for path in ["sp ace.txt", "日本語.txt"] {
        repo.commit_bytes(path, b"one\n", "first");
        repo.write_worktree_file(path, b"two\n");
    }

    for path in ["sp ace.txt", "日本語.txt"] {
        let file = one_file(&repo, DiffTarget::IndexVsWorktree(Some(path.into())));
        assert_eq!(file.path, Path::new(path));
        repo.runner()
            .run(&GitOp::StageHunk {
                path: path.into(),
                patch: patch_of(&file, 0),
            })
            .await
            .expect("stage できませんでした");
        assert_eq!(staged(&repo, path), b"two\n");
    }

    // 2 つとも stage されている = どちらも自分のファイルに当たった。
    // porcelain は空白も非 ASCII も quote して出す。
    assert_eq!(
        repo.porcelain(),
        "M  \"sp ace.txt\"\nM  \"\\346\\227\\245\\346\\234\\254\\350\\252\\236.txt\""
    );
}
