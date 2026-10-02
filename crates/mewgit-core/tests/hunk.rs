//! ハンク単位の書き込みの round-trip。`git_read::diff` で読んだハンクを
//! `git_exec::hunk_patch` でパッチに戻し、実際の `git apply` に通す。
//!
//! **ここで見るのは「戻したパッチが元の変更と同じものを表しているか」。** パッチの
//! 形は `git_exec::patch` のユニットテストが、argv は `git_exec::argv` が見張る。
//! 形が正しくても git が受け取らない・別の場所に当たる、が起きうるので実際に叩く。

use std::path::Path;

use mewgit_core::git_exec::{hunk_patch, ExecError, GitOp, HunkApply};
use mewgit_core::git_read::{DiffTarget, FileDiff, Hunk, PathStamp, RepoReader};
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

/// `file` を読んだ時点の指紋（`HunkApply` の `expect`）。
fn stamp_of(file: &FileDiff) -> PathStamp {
    file.stamp.expect("ワークツリーの diff に指紋がありません")
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
        .run(&GitOp::StageHunk(Box::new(HunkApply {
            path: "a.txt".into(),
            patch: patch_of(&file, 1),
            expect: stamp_of(&file),
        })))
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
        .run(&GitOp::UnstageHunk(Box::new(HunkApply {
            path: "a.txt".into(),
            patch: patch_of(&file, 1),
            expect: stamp_of(&file),
        })))
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
    let staged_file = one_file(&repo, DiffTarget::IndexVsWorktree(Some("a.txt".into())));
    repo.runner()
        .run(&GitOp::StageHunk(Box::new(HunkApply {
            path: "a.txt".into(),
            patch: patch_of(&staged_file, 0),
            expect: stamp_of(&staged_file),
        })))
        .await
        .expect("stage できませんでした");

    let file = one_file(&repo, DiffTarget::IndexVsWorktree(Some("a.txt".into())));
    assert_eq!(file.hunks.len(), 1, "unstaged なハンクは 1 つのはず");
    repo.runner()
        .run(&GitOp::DiscardHunk(Box::new(HunkApply {
            path: "a.txt".into(),
            patch: patch_of(&file, 0),
            expect: stamp_of(&file),
        })))
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
        .run(&GitOp::StageHunk(Box::new(HunkApply {
            path: "n.txt".into(),
            patch: patch_of(&file, 0),
            expect: stamp_of(&file),
        })))
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
        .run(&GitOp::StageHunk(Box::new(HunkApply {
            path: "crlf.txt".into(),
            patch: patch_of(&file, 0),
            expect: stamp_of(&file),
        })))
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
        .run(&GitOp::StageHunk(Box::new(HunkApply {
            path: "bin.txt".into(),
            patch: patch_of(&file, 0),
            expect: stamp_of(&file),
        })))
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
        .run(&GitOp::StageHunk(Box::new(HunkApply {
            path: "f.txt".into(),
            patch: patch_of(&file, 0),
            expect: stamp_of(&file),
        })))
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
            .run(&GitOp::StageHunk(Box::new(HunkApply {
                path: path.into(),
                patch: patch_of(&file, 0),
                expect: stamp_of(&file),
            })))
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

// ---- 古いハンクの守り（03 の「古いハンクの守り」） ----
//
// 外で書き換えるときは**長さも変える。** 守りが比べるのは `(mtime, len)` と index・HEAD の
// blob で、同じ長さだと mtime の分解能に頼ることになる（ナノ秒の APFS / ext4 では
// 拾えるが、テストをそこに依存させない）。

/// 1 行目だけを変えた 3 行のファイルを持つリポジトリ。
fn one_hunk_repo() -> TestRepo {
    let repo = TestRepo::init();
    repo.commit_bytes("f.txt", b"line1\nline2\nline3\n", "first");
    repo.write_worktree_file("f.txt", b"line1\ntwo-A\nline3\n");
    repo
}

/// 守りに断られたことを確かめ、断ったパスを返す。
fn stale_path(result: Result<mewgit_core::git_exec::ExecOutput, ExecError>) -> std::path::PathBuf {
    match result {
        Err(ExecError::Stale { path, .. }) => path,
        other => panic!("守りに断られていません: {other:?}"),
    }
}

#[tokio::test]
async fn a_hunk_read_before_an_outside_edit_is_not_staged() {
    // plan.md の実測（git 2.56.0 でも同じ）—— 守りが無いと `git apply --cached` は
    // exit 0 で通り、index に `two-A`（HEAD にもワークツリーにも無い内容）が入る。
    let repo = one_hunk_repo();
    let file = one_file(&repo, DiffTarget::IndexVsWorktree(Some("f.txt".into())));
    repo.write_worktree_file("f.txt", b"line1\ntwo-BB\nline3\n");

    let result = repo
        .runner()
        .run(&GitOp::StageHunk(Box::new(HunkApply {
            path: "f.txt".into(),
            patch: patch_of(&file, 0),
            expect: stamp_of(&file),
        })))
        .await;

    assert_eq!(stale_path(result), Path::new("f.txt"));
    assert_eq!(staged(&repo, "f.txt"), b"line1\nline2\nline3\n");
    assert_eq!(repo.porcelain(), " M f.txt");

    // **読み直せば通る**（守りが常に断る形ではないことを、同じ 1 本で見る）。
    let file = one_file(&repo, DiffTarget::IndexVsWorktree(Some("f.txt".into())));
    repo.runner()
        .run(&GitOp::StageHunk(Box::new(HunkApply {
            path: "f.txt".into(),
            patch: patch_of(&file, 0),
            expect: stamp_of(&file),
        })))
        .await
        .expect("読み直したハンクを stage できませんでした");
    assert_eq!(staged(&repo, "f.txt"), b"line1\ntwo-BB\nline3\n");
}

#[tokio::test]
async fn a_hunk_read_before_an_outside_add_is_not_staged() {
    // ワークツリーは動かず index だけが動く形（外で `git add -p` した）。パッチの
    // 基準（index）が変わっている。
    let repo = TestRepo::init();
    repo.commit_bytes("a.txt", twenty_lines().concat().as_bytes(), "first");
    repo.write_worktree_file("a.txt", &twenty_lines_edited());
    let file = one_file(&repo, DiffTarget::IndexVsWorktree(Some("a.txt".into())));
    repo.git(&["add", "a.txt"]);

    let result = repo
        .runner()
        .run(&GitOp::StageHunk(Box::new(HunkApply {
            path: "a.txt".into(),
            patch: patch_of(&file, 0),
            expect: stamp_of(&file),
        })))
        .await;

    assert_eq!(stale_path(result), Path::new("a.txt"));
}

#[tokio::test]
async fn a_hunk_read_before_an_outside_edit_is_not_discarded() {
    let repo = one_hunk_repo();
    let file = one_file(&repo, DiffTarget::IndexVsWorktree(Some("f.txt".into())));
    repo.write_worktree_file("f.txt", b"line1\ntwo-BB\nline3\n");

    let result = repo
        .runner()
        .run(&GitOp::DiscardHunk(Box::new(HunkApply {
            path: "f.txt".into(),
            patch: patch_of(&file, 0),
            expect: stamp_of(&file),
        })))
        .await;

    assert_eq!(stale_path(result), Path::new("f.txt"));
    assert_eq!(worktree(&repo, "f.txt"), b"line1\ntwo-BB\nline3\n");
}

#[tokio::test]
async fn unstaging_a_hunk_ignores_the_worktree_but_not_the_index() {
    // **ワークツリーの編集では断らない。** unstage のパッチは HEAD → index で、当てる先も
    // index（`GitOp::stale_at`）。エディタで編集を続けながら staged なハンクを外す形。
    let repo = one_hunk_repo();
    repo.git(&["add", "f.txt"]);
    let file = one_file(&repo, DiffTarget::HeadVsIndex(Some("f.txt".into())));
    repo.write_worktree_file("f.txt", b"line1\ntwo-BB\nline3\n");

    repo.runner()
        .run(&GitOp::UnstageHunk(Box::new(HunkApply {
            path: "f.txt".into(),
            patch: patch_of(&file, 0),
            expect: stamp_of(&file),
        })))
        .await
        .expect("ワークツリーの編集で unstage が断られました");
    assert_eq!(staged(&repo, "f.txt"), b"line1\nline2\nline3\n");

    // **index が外で動いていれば断る。** 読み直した staged なハンクのあとで、外で
    // 別の中身を stage し直す。
    repo.git(&["add", "f.txt"]);
    let file = one_file(&repo, DiffTarget::HeadVsIndex(Some("f.txt".into())));
    repo.write_worktree_file("f.txt", b"line1\ntwo-CCC\nline3\n");
    repo.git(&["add", "f.txt"]);

    let result = repo
        .runner()
        .run(&GitOp::UnstageHunk(Box::new(HunkApply {
            path: "f.txt".into(),
            patch: patch_of(&file, 0),
            expect: stamp_of(&file),
        })))
        .await;
    assert_eq!(stale_path(result), Path::new("f.txt"));
    assert_eq!(staged(&repo, "f.txt"), b"line1\ntwo-CCC\nline3\n");
}

#[test]
fn the_stamp_is_read_only_for_the_worktree_and_the_index() {
    let repo = one_hunk_repo();
    let now = repo.path_stamp("f.txt");

    let unstaged = one_file(&repo, DiffTarget::IndexVsWorktree(Some("f.txt".into())));
    assert_eq!(unstaged.stamp, Some(now));
    assert!(now.worktree.is_some() && now.index.is_some(), "{now:?}");

    // コミットの中身はオブジェクトで、外の編集では変わらない。
    let commit = one_file(
        &repo,
        DiffTarget::Commit {
            oid: repo.head_oid(),
            path: Some("f.txt".into()),
        },
    );
    assert_eq!(commit.stamp, None);
}

#[tokio::test]
async fn unstaging_a_hunk_read_before_an_outside_commit_is_refused() {
    // **外の commit は index もワークツリーも動かさず、HEAD だけを動かす。** unstage の
    // パッチの基準は HEAD なので、守りが HEAD を見ないと `git apply --cached --reverse` が
    // exit 0 で通り、index に `line2`（HEAD にもワークツリーにも無い）が入る
    // （`/code-review` が拾った。git 2.56.0 で再現）。
    let repo = one_hunk_repo();
    repo.git(&["add", "f.txt"]);
    let file = one_file(&repo, DiffTarget::HeadVsIndex(Some("f.txt".into())));
    repo.git(&["commit", "-q", "-m", "outside"]);

    let result = repo
        .runner()
        .run(&GitOp::UnstageHunk(Box::new(HunkApply {
            path: "f.txt".into(),
            patch: patch_of(&file, 0),
            expect: stamp_of(&file),
        })))
        .await;

    assert_eq!(stale_path(result), Path::new("f.txt"));
    assert_eq!(staged(&repo, "f.txt"), b"line1\ntwo-A\nline3\n");
    assert_eq!(repo.porcelain(), "");
}

#[tokio::test]
async fn staging_a_hunk_does_not_look_at_head() {
    // stage のパッチは index → ワークツリーで、HEAD を基準にしない。`two-A` を stage した
    // うえで `two-BB` に編集し、その unstaged なハンクを読んでから外で commit する ——
    // `f.txt` の HEAD 側の blob だけが動き、index もワークツリーも動かない。断らずに当てる。
    let repo = one_hunk_repo();
    repo.git(&["add", "f.txt"]);
    repo.write_worktree_file("f.txt", b"line1\ntwo-BB\nline3\n");
    let file = one_file(&repo, DiffTarget::IndexVsWorktree(Some("f.txt".into())));
    let before = stamp_of(&file);
    repo.git(&["commit", "-q", "-m", "outside"]);
    let after = repo.path_stamp("f.txt");
    // 前提: 動いたのは HEAD 側だけ。
    assert_ne!(before.head, after.head);
    assert_eq!(
        (before.worktree, before.index),
        (after.worktree, after.index)
    );

    repo.runner()
        .run(&GitOp::StageHunk(Box::new(HunkApply {
            path: "f.txt".into(),
            patch: patch_of(&file, 0),
            expect: before,
        })))
        .await
        .expect("HEAD が動いただけで stage が断られました");
    assert_eq!(staged(&repo, "f.txt"), b"line1\ntwo-BB\nline3\n");
}
