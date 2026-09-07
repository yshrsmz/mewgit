//! `git_read::RepoReader::diff` の統合テスト。実リポジトリを一時ディレクトリに作って読む。

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use mewgit_core::git_read::{
    DiffTarget, FileDiff, FileEntry, FileStatus, Hunk, LineOrigin, RepoReader,
};
use mewgit_core::test_util::{fake_oid, TestRepo};

/// ファイルの並びを `(path, old_path, status)` にする。
fn files(diffs: &[FileDiff]) -> Vec<(&str, Option<&str>, FileStatus)> {
    diffs
        .iter()
        .map(|file| {
            (
                file.path.to_str().expect("パスが UTF-8 ではありません"),
                file.old_path
                    .as_deref()
                    .map(|path| path.to_str().expect("旧パスが UTF-8 ではありません")),
                file.status,
            )
        })
        .collect()
}

/// [`files`] と同じ形で `WorktreeSnapshot` の区画を見る。`FileEntry` と `FileDiff` は
/// 頭の 3 つが同じ値なので、`status` と `diff` の食い違いを直に比べられる。
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

/// ハンクの行を `(origin, 中身)` にする。中身は読みやすさのため lossy で見る。
fn lines(hunk: &Hunk) -> Vec<(LineOrigin, String)> {
    hunk.lines
        .iter()
        .map(|line| {
            (
                line.origin,
                String::from_utf8_lossy(&line.content).into_owned(),
            )
        })
        .collect()
}

/// 20 行のファイル。離れた 2 箇所を変えてハンクを 2 つにするのに使う。
fn twenty_lines() -> String {
    (1..=20).map(|n| format!("line {n}\n")).collect()
}

#[test]
fn index_vs_worktree_reports_unstaged_edits() {
    let repo = TestRepo::init();
    repo.commit_file("file.txt", "one");
    repo.write_worktree_file("file.txt", b"two\n");
    let reader = RepoReader::open(repo.path()).unwrap();

    let diff = reader.diff(DiffTarget::IndexVsWorktree(None)).unwrap();

    assert_eq!(files(&diff), [("file.txt", None, FileStatus::Modified)]);
    assert_eq!(diff[0].hunks[0].header, "@@ -1 +1 @@");
    assert_eq!(
        lines(&diff[0].hunks[0]),
        [
            (LineOrigin::Deletion, "one".to_string()),
            (LineOrigin::Addition, "two".to_string()),
        ]
    );
}

#[test]
fn index_vs_worktree_shows_the_content_of_untracked_files() {
    let repo = TestRepo::init();
    repo.commit("first");
    repo.write_worktree_file("new.txt", b"hello\nworld\n");
    let reader = RepoReader::open(repo.path()).unwrap();

    let diff = reader.diff(DiffTarget::IndexVsWorktree(None)).unwrap();

    // 中身を出さないと worktree ペインで untracked を選んでも空になる。
    assert_eq!(files(&diff), [("new.txt", None, FileStatus::Untracked)]);
    assert_eq!(
        lines(&diff[0].hunks[0]),
        [
            (LineOrigin::Addition, "hello".to_string()),
            (LineOrigin::Addition, "world".to_string()),
        ]
    );
}

#[test]
fn head_vs_index_reports_staged_edits() {
    let repo = TestRepo::init();
    repo.commit_file("file.txt", "one");
    repo.write_worktree_file("file.txt", b"two\n");
    repo.git(&["add", "file.txt"]);
    // ワークツリー側にさらに変更を積んでも staged の diff には出ない。
    repo.write_worktree_file("file.txt", b"three\n");
    let reader = RepoReader::open(repo.path()).unwrap();

    let diff = reader.diff(DiffTarget::HeadVsIndex(None)).unwrap();

    assert_eq!(
        lines(&diff[0].hunks[0]),
        [
            (LineOrigin::Deletion, "one".to_string()),
            (LineOrigin::Addition, "two".to_string()),
        ]
    );
}

#[test]
fn head_vs_index_works_before_the_first_commit() {
    let repo = TestRepo::init();
    repo.write_worktree_file("file.txt", b"first\n");
    repo.git(&["add", "file.txt"]);
    let reader = RepoReader::open(repo.path()).unwrap();

    // unborn HEAD は空ツリーと比べる。`repo.head()` の `Err` で落ちない。
    let diff = reader.diff(DiffTarget::HeadVsIndex(None)).unwrap();

    assert_eq!(files(&diff), [("file.txt", None, FileStatus::Added)]);
    assert_eq!(
        lines(&diff[0].hunks[0]),
        [(LineOrigin::Addition, "first".to_string())]
    );
}

#[test]
fn head_vs_index_reports_a_rename_with_both_paths() {
    let repo = TestRepo::init();
    repo.commit_file("old.txt", "one\ntwo\nthree\nfour\nfive");
    repo.git(&["mv", "old.txt", "new.txt"]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let diff = reader.diff(DiffTarget::HeadVsIndex(None)).unwrap();

    // `find_similar` を `Patch` より先に呼んでいないと「削除 + 追加」の 2 行になる。
    assert_eq!(
        files(&diff),
        [("new.txt", Some("old.txt"), FileStatus::Renamed)]
    );
}

#[test]
fn index_vs_worktree_agrees_with_status_on_a_worktree_rename() {
    let repo = TestRepo::init();
    repo.commit_file("old.txt", "one\ntwo\nthree\nfour\nfive");
    // `add` していない `git mv`。index からは消え、untracked が 1 つ増えた状態。
    std::fs::rename(repo.path().join("old.txt"), repo.path().join("new.txt")).unwrap();
    let reader = RepoReader::open(repo.path()).unwrap();

    let status = reader.status().unwrap();
    let diff = reader.diff(DiffTarget::IndexVsWorktree(None)).unwrap();

    // `for_untracked` が無いと、status は `Renamed` 1 行、diff は
    // `Deleted` + `Untracked` の 2 件になって食い違う。
    assert_eq!(
        rows(&status.unstaged),
        [("new.txt", Some("old.txt"), FileStatus::Renamed)]
    );
    assert_eq!(files(&diff), rows(&status.unstaged));
}

/// `main` に `topic` を `--no-ff` でマージしたリポジトリと、そのマージコミット。
///
/// **第一親（main）側の変更は `main.txt`、第二親（topic）側は `topic.txt`。**
/// 第一親と比べていれば `topic.txt` だけが出る。
fn merged_repo() -> (TestRepo, mewgit_core::git_read::Oid) {
    let repo = TestRepo::init();
    repo.commit_file("base.txt", "base");
    repo.git(&["checkout", "-q", "-b", "topic"]);
    repo.commit_file("topic.txt", "topic");
    repo.git(&["checkout", "-q", "main"]);
    repo.commit_file("main.txt", "main");
    repo.git(&["merge", "-q", "--no-ff", "-m", "merge topic", "topic"]);
    let merge = repo.head_oid();
    (repo, merge)
}

#[test]
fn commit_diffs_against_the_first_parent() {
    let (repo, merge) = merged_repo();
    let reader = RepoReader::open(repo.path()).unwrap();

    let diff = reader
        .diff(DiffTarget::Commit {
            oid: merge,
            path: None,
        })
        .unwrap();

    // 第一親（main）との差なので、topic 側で足したファイルだけが出る。
    assert_eq!(files(&diff), [("topic.txt", None, FileStatus::Added)]);
}

#[test]
fn commit_diffs_the_root_commit_against_an_empty_tree() {
    let repo = TestRepo::init();
    let root = repo.commit_file("file.txt", "first");
    let reader = RepoReader::open(repo.path()).unwrap();

    let diff = reader
        .diff(DiffTarget::Commit {
            oid: root,
            path: None,
        })
        .unwrap();

    assert_eq!(files(&diff), [("file.txt", None, FileStatus::Added)]);
}

#[test]
fn range_diffs_the_two_commits() {
    let repo = TestRepo::init();
    let first = repo.commit_file("a.txt", "a");
    repo.commit_file("b.txt", "b");
    let third = repo.commit_file("c.txt", "c");
    let reader = RepoReader::open(repo.path()).unwrap();

    let diff = reader
        .diff(DiffTarget::Range {
            from: first,
            to: third,
            path: None,
        })
        .unwrap();

    assert_eq!(
        files(&diff),
        [
            ("b.txt", None, FileStatus::Added),
            ("c.txt", None, FileStatus::Added),
        ]
    );
}

#[test]
fn a_path_narrows_the_diff_and_is_not_a_glob() {
    let repo = TestRepo::init();
    repo.commit("first");
    repo.write_worktree_file("x.txt", b"plain\n");
    repo.write_worktree_file("[x].txt", b"bracketed\n");
    let reader = RepoReader::open(repo.path()).unwrap();

    let diff = reader
        .diff(DiffTarget::IndexVsWorktree(Some("[x].txt".into())))
        .unwrap();

    // 絞り込みは delta のパスとの完全一致。`[x]` を文字クラスとして扱うと
    // `x.txt` の方が出る。
    assert_eq!(files(&diff), [("[x].txt", None, FileStatus::Untracked)]);
}

#[test]
fn a_path_keeps_a_staged_rename_under_either_name() {
    let repo = TestRepo::init();
    repo.commit_file("old.txt", "one\ntwo\nthree\nfour\nfive");
    repo.git(&["mv", "old.txt", "new.txt"]);
    let reader = RepoReader::open(repo.path()).unwrap();

    // pathspec で絞ると、`find_similar` が対にすべき旧パスの delta が先に落ちて
    // 「全行追加の Added」になる。そのパッチを `git apply --cached` に戻すと、
    // rename ではなくファイルの新規追加を index に適用することになる（02）。
    for name in ["new.txt", "old.txt"] {
        let diff = reader
            .diff(DiffTarget::HeadVsIndex(Some(name.into())))
            .unwrap();

        assert_eq!(
            files(&diff),
            [("new.txt", Some("old.txt"), FileStatus::Renamed)],
            "絞り込み: {name}"
        );
    }
}

#[test]
fn a_path_keeps_a_worktree_rename_under_either_name() {
    let repo = TestRepo::init();
    repo.commit_file("old.txt", "one\ntwo\nthree\nfour\nfive");
    // `add` していない `git mv`。index からは消え、untracked が 1 つ増えた状態。
    std::fs::rename(repo.path().join("old.txt"), repo.path().join("new.txt")).unwrap();
    let reader = RepoReader::open(repo.path()).unwrap();

    // ワーキングツリー側の rename は 2 つの delta を `for_untracked` で対にしている。
    // pathspec で絞ると、対にする前に片方が落ちる。
    for name in ["new.txt", "old.txt"] {
        let diff = reader
            .diff(DiffTarget::IndexVsWorktree(Some(name.into())))
            .unwrap();

        assert_eq!(
            files(&diff),
            [("new.txt", Some("old.txt"), FileStatus::Renamed)],
            "絞り込み: {name}"
        );
    }
}

#[test]
fn a_path_narrows_an_untracked_file_without_reading_the_others() {
    let repo = TestRepo::init();
    repo.commit("first");
    repo.write_worktree_file("wanted.txt", b"wanted\n");
    repo.write_worktree_file("other.txt", b"other\n");
    let reader = RepoReader::open(repo.path()).unwrap();

    let diff = reader
        .diff(DiffTarget::IndexVsWorktree(Some("wanted.txt".into())))
        .unwrap();

    // untracked も中身が出る（`show_untracked_content`）。絞ったファイルだけ。
    assert_eq!(files(&diff), [("wanted.txt", None, FileStatus::Untracked)]);
    assert_eq!(diff[0].hunks.len(), 1);
}

#[test]
fn a_line_without_a_trailing_newline_is_marked_on_the_line_itself() {
    let repo = TestRepo::init();
    repo.commit_bytes("noeol.txt", b"a\nb", "noeol");
    repo.write_worktree_file("noeol.txt", b"a\nb\nc");
    let reader = RepoReader::open(repo.path()).unwrap();

    let diff = reader.diff(DiffTarget::IndexVsWorktree(None)).unwrap();

    // libgit2 の `\ No newline at end of file` は独立した行として来る。マーカー行は
    // 捨て、改行の有無は内容の行の種別に畳む。
    assert_eq!(
        lines(&diff[0].hunks[0]),
        [
            (LineOrigin::Context, "a".to_string()),
            (LineOrigin::DeletionNoEol, "b".to_string()),
            (LineOrigin::Addition, "b".to_string()),
            (LineOrigin::AdditionNoEol, "c".to_string()),
        ]
    );
}

#[test]
fn a_context_line_without_a_trailing_newline_is_marked_too() {
    let repo = TestRepo::init();
    repo.commit_bytes("ctx.txt", b"a\nb\nc", "ctx");
    repo.write_worktree_file("ctx.txt", b"A\nb\nc");
    let reader = RepoReader::open(repo.path()).unwrap();

    let diff = reader.diff(DiffTarget::IndexVsWorktree(None)).unwrap();

    assert_eq!(
        lines(&diff[0].hunks[0]),
        [
            (LineOrigin::Deletion, "a".to_string()),
            (LineOrigin::Addition, "A".to_string()),
            (LineOrigin::Context, "b".to_string()),
            (LineOrigin::ContextNoEol, "c".to_string()),
        ]
    );
}

#[test]
fn distant_changes_are_split_into_separate_hunks() {
    let repo = TestRepo::init();
    let original = twenty_lines();
    repo.commit_bytes("long.txt", original.as_bytes(), "long");
    let edited = original
        .replace("line 1\n", "LINE 1\n")
        .replace("line 20\n", "LINE 20\n");
    repo.write_worktree_file("long.txt", edited.as_bytes());
    let reader = RepoReader::open(repo.path()).unwrap();

    let diff = reader.diff(DiffTarget::IndexVsWorktree(None)).unwrap();

    let headers: Vec<&str> = diff[0]
        .hunks
        .iter()
        .map(|hunk| hunk.header.as_str())
        .collect();
    // 2 つ目のヘッダの末尾はハンクの直前の行（`@@` の後ろの関数コンテキスト）。
    // `git apply` は読まないが、表示に使うので落とさない。
    assert_eq!(headers, ["@@ -1,4 +1,4 @@", "@@ -17,4 +17,4 @@ line 16"]);
}

#[test]
fn a_binary_file_is_flagged_and_has_no_hunks() {
    let repo = TestRepo::init();
    repo.commit_bytes("bin.dat", &[0u8, 1, 2, 3, 0, 255], "bin");
    repo.write_worktree_file("bin.dat", &[9u8, 9, 0, 1]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let diff = reader.diff(DiffTarget::IndexVsWorktree(None)).unwrap();

    // `Diff::get_delta` を先に読むと立っていないことがある。`Patch` を作ったあとの
    // delta から読む。
    assert!(diff[0].binary, "バイナリと判定されていません");
    assert_eq!(diff[0].hunks, []);
}

#[test]
fn a_mode_only_change_has_no_hunks_and_is_not_binary() {
    let repo = TestRepo::init();
    repo.commit_file("run.sh", "echo hi");
    repo.git(&["update-index", "--chmod=+x", "run.sh"]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let diff = reader.diff(DiffTarget::HeadVsIndex(None)).unwrap();

    // バイナリと同じくハンクが空になるので、区別はフラグで付ける。
    assert_eq!(files(&diff), [("run.sh", None, FileStatus::Modified)]);
    assert!(!diff[0].binary, "バイナリと誤判定しています");
    assert_eq!(diff[0].hunks, []);
}

#[test]
fn hunk_lines_keep_bytes_that_are_not_utf8() {
    let repo = TestRepo::init();
    repo.commit_bytes("latin.txt", b"caf\xe9\n", "latin");
    repo.write_worktree_file("latin.txt", b"th\xe9\n");
    let reader = RepoReader::open(repo.path()).unwrap();

    let diff = reader.diff(DiffTarget::IndexVsWorktree(None)).unwrap();

    // lossy にすると `git apply --cached` に戻したときに中身が変わる。
    let contents: Vec<&[u8]> = diff[0].hunks[0]
        .lines
        .iter()
        .map(|line| line.content.as_slice())
        .collect();
    assert_eq!(contents, [b"caf\xe9".as_slice(), b"th\xe9".as_slice()]);
}

#[test]
fn diff_drops_a_file_whose_path_is_not_utf8() {
    let repo = TestRepo::init();
    repo.commit("first");
    repo.add_raw_path_to_index(b"bad\xff.txt");
    let reader = RepoReader::open(repo.path()).unwrap();

    let diff = reader.diff(DiffTarget::HeadVsIndex(None)).unwrap();

    assert_eq!(files(&diff), []);
}

#[test]
fn diff_drops_a_rename_whose_old_path_is_not_utf8() {
    let repo = TestRepo::init();
    let blob = repo.add_raw_path_to_index(b"bad\xff.txt");
    repo.git(&["commit", "-q", "-m", "raw"]);
    // 同じ blob を UTF-8 の名前に付け替える。Oid が同じなので rename は必ず検出される。
    repo.git(&[
        OsStr::new("update-index"),
        OsStr::new("--force-remove"),
        OsStr::from_bytes(b"bad\xff.txt"),
    ]);
    repo.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("100644,{blob},good.txt"),
    ]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let diff = reader.diff(DiffTarget::HeadVsIndex(None)).unwrap();

    // 新しいパスは UTF-8 でも、旧パスを argv に載せられないので unstage できない。
    assert_eq!(files(&diff), []);
}

#[test]
fn a_deleted_file_keeps_its_path() {
    let repo = TestRepo::init();
    repo.commit_file("gone.txt", "gone");
    repo.git(&["rm", "-q", "gone.txt"]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let diff = reader.diff(DiffTarget::HeadVsIndex(None)).unwrap();

    assert_eq!(files(&diff), [("gone.txt", None, FileStatus::Deleted)]);
    assert_eq!(
        lines(&diff[0].hunks[0]),
        [(LineOrigin::Deletion, "gone".to_string())]
    );
}

// --- commit_files（02 の「コミットのファイル一覧」）------------------------------

#[test]
fn commit_files_lists_what_the_commit_changed() {
    let repo = TestRepo::init();
    repo.commit_file("a.txt", "a");
    repo.write_worktree_file("a.txt", b"a2\n");
    repo.write_worktree_file("b.txt", b"b\n");
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-q", "-m", "second"]);
    let second = repo.head_oid();
    let reader = RepoReader::open(repo.path()).unwrap();

    let entries = reader.commit_files(second).unwrap();

    assert_eq!(
        rows(&entries),
        [
            ("a.txt", None, FileStatus::Modified),
            ("b.txt", None, FileStatus::Added),
        ]
    );
}

#[test]
fn commit_files_pairs_a_rename_like_the_diff_does() {
    // **`find_similar` の設定を `diff` と共有していないと 2 行になる**（Deleted +
    // Added）。一覧の行と diff の対象が食い違う形（02）。
    let repo = TestRepo::init();
    repo.commit_file("old.txt", &twenty_lines());
    repo.git(&["mv", "old.txt", "new.txt"]);
    repo.git(&["commit", "-q", "-m", "rename"]);
    let renamed = repo.head_oid();
    let reader = RepoReader::open(repo.path()).unwrap();

    let entries = reader.commit_files(renamed).unwrap();

    assert_eq!(
        rows(&entries),
        [("new.txt", Some("old.txt"), FileStatus::Renamed)]
    );
}

#[test]
fn commit_files_uses_the_first_parent_of_a_merge() {
    let (repo, merge) = merged_repo();
    let reader = RepoReader::open(repo.path()).unwrap();

    let entries = reader.commit_files(merge).unwrap();

    // 第一親（main）との差なので、topic 側で足したファイルだけが出る。
    assert_eq!(rows(&entries), [("topic.txt", None, FileStatus::Added)]);
}

#[test]
fn commit_files_lists_the_root_commit_against_an_empty_tree() {
    let repo = TestRepo::init();
    let root = repo.commit_file("file.txt", "first");
    let reader = RepoReader::open(repo.path()).unwrap();

    let entries = reader.commit_files(root).unwrap();

    assert_eq!(rows(&entries), [("file.txt", None, FileStatus::Added)]);
}

#[test]
fn an_empty_commit_has_no_files() {
    // `--allow-empty` のコミットは変更を 1 つも持たない。**「まだ読めていない」と
    // 同じ見た目にしないのは `app` の仕事**で、こちらは空の `Vec` を返す（02）。
    let repo = TestRepo::init();
    repo.commit_file("file.txt", "first");
    repo.git(&["commit", "-q", "--allow-empty", "-m", "empty"]);
    let empty = repo.head_oid();
    let reader = RepoReader::open(repo.path()).unwrap();

    assert!(reader.commit_files(empty).unwrap().is_empty());
}

#[test]
fn commit_files_and_the_commit_diff_see_the_same_files() {
    // **入口（比べる 2 つのツリーと `find_similar` の設定）が 2 本あれば結果はずれる。**
    // 出口（`file_entry`）だけを共有しても揃わない（02、#18 と同じ形）。ずれは
    // 「一覧に出ているファイルを選んだのに diff が空」という形で出るので、
    // 両方を同じリポジトリで突き合わせる。
    let repo = TestRepo::init();
    repo.commit_file("old.txt", &twenty_lines());
    repo.git(&["mv", "old.txt", "new.txt"]);
    repo.write_worktree_file("added.txt", b"added\n");
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-q", "-m", "rename and add"]);
    let commit = repo.head_oid();
    let reader = RepoReader::open(repo.path()).unwrap();

    let entries = reader.commit_files(commit).unwrap();
    let diff = reader
        .diff(DiffTarget::Commit {
            oid: commit,
            path: None,
        })
        .unwrap();

    assert_eq!(rows(&entries), files(&diff));
}

#[test]
fn commit_files_reports_a_missing_commit() {
    let repo = TestRepo::init();
    repo.commit_file("file.txt", "first");
    let reader = RepoReader::open(repo.path()).unwrap();

    // `fake_oid` は実リポジトリの Oid と重ならない（`test_util`）。
    assert!(reader.commit_files(fake_oid(7)).is_err());
}
