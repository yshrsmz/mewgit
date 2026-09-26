//! 進行中操作の検出（02 の「進行中操作の検出」）。
//!
//! **git2 を使わず、gitdir のファイルだけを見る。** `git2::Repository::state()` は
//! `-i` の有無を区別できないうえ、`sequencer/` だけが残った形を「無い」と読む
//! （libgit2 1.9。02 の実測）。

use std::fs;
use std::path::Path;

use super::{InProgressOp, Oid, Step};

/// `git_dir` に残っている進行中操作。無ければ `None`。
///
/// `git_dir` は**ワークツリーごとの** gitdir（[`RepoReader::git_dir`](super::RepoReader::git_dir)）。
/// linked worktree の状態は `.git/worktrees/<name>/` に置かれる。
///
/// 判定は上から見て、**最初に当たった 1 つを採る**（`git status` は複数を同時に出す。
/// `rebase -r` の merge が止まると `rebase-merge/` と `MERGE_HEAD` が両方在るが、
/// 打つのは rebase の continue / abort なので rebase を先に見る。02）。ファイルが
/// 読めない・中身が壊れているときはその行を「無い」と読んで次へ進むので、`Result` は
/// 返さない。
pub fn in_progress(git_dir: &Path) -> Option<InProgressOp> {
    let merge = git_dir.join("rebase-merge");
    if merge.is_dir() {
        return Some(InProgressOp::Rebase {
            step: step(&merge, "msgnum", "end"),
        });
    }
    let apply = git_dir.join("rebase-apply");
    if apply.is_dir() {
        let step = step(&apply, "next", "last");
        // 同じディレクトリを `git am` も使う。見分けるのは `applying` の有無で、
        // continue / abort の argv が違う（am の途中の `git rebase --continue` は断られる）。
        return Some(if apply.join("applying").exists() {
            InProgressOp::Am { step }
        } else {
            InProgressOp::Rebase { step }
        });
    }
    if let Some(oid) = head_file(git_dir, "MERGE_HEAD") {
        return Some(InProgressOp::Merge { oid });
    }
    if let Some(oid) = head_file(git_dir, "CHERRY_PICK_HEAD") {
        return Some(InProgressOp::CherryPick { oid: Some(oid) });
    }
    if let Some(oid) = head_file(git_dir, "REVERT_HEAD") {
        return Some(InProgressOp::Revert { oid: Some(oid) });
    }
    // 複数の pick / revert の途中で 1 つを commit で解くと、`*_HEAD` は消えて
    // `sequencer/` だけが残る。`git status` はこれも進行中と呼ぶ。
    let todo = fs::read_to_string(git_dir.join("sequencer/todo")).ok()?;
    match todo.split_whitespace().next()? {
        "pick" | "p" => Some(InProgressOp::CherryPick { oid: None }),
        "revert" => Some(InProgressOp::Revert { oid: None }),
        _ => None,
    }
}

/// `dir/current` と `dir/total` の数。どちらかが読めなければ `None`。
///
/// 外で `git rebase -i` の todo を編集している最中は、ディレクトリがあっても
/// 数のファイルがまだ無い。
fn step(dir: &Path, current: &str, total: &str) -> Option<Step> {
    let read = |name: &str| -> Option<usize> {
        fs::read_to_string(dir.join(name)).ok()?.trim().parse().ok()
    };
    Some(Step {
        current: read(current)?,
        total: read(total)?,
    })
}

/// `MERGE_HEAD` などの先頭行の Oid。octopus の `MERGE_HEAD` は複数行になるが、
/// 先頭だけを採る。
fn head_file(git_dir: &Path, name: &str) -> Option<Oid> {
    let body = fs::read_to_string(git_dir.join(name)).ok()?;
    Oid::from_str(body.lines().next()?.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// gitdir に見立てた空のディレクトリ。
    ///
    /// 実物の git で作る形は `tests/git_read.rs` が踏む。ここは実物では作りにくい
    /// 形（数のファイルが無い、中身が壊れている）だけを見る。
    fn git_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("一時ディレクトリを作れませんでした")
    }

    #[test]
    fn nothing_in_progress_in_an_empty_git_dir() {
        let dir = git_dir();
        assert_eq!(in_progress(dir.path()), None);
    }

    #[test]
    fn a_rebase_without_its_counters_is_still_in_progress() {
        let dir = git_dir();
        fs::create_dir(dir.path().join("rebase-merge")).unwrap();

        assert_eq!(
            in_progress(dir.path()),
            Some(InProgressOp::Rebase { step: None })
        );
    }

    #[test]
    fn a_broken_head_file_is_read_as_absent() {
        let dir = git_dir();
        fs::write(dir.path().join("MERGE_HEAD"), "not an oid\n").unwrap();

        assert_eq!(in_progress(dir.path()), None);
    }

    #[test]
    fn an_unknown_sequencer_verb_is_not_in_progress() {
        let dir = git_dir();
        fs::create_dir(dir.path().join("sequencer")).unwrap();
        fs::write(dir.path().join("sequencer/todo"), "exec make\n").unwrap();

        assert_eq!(in_progress(dir.path()), None);
    }
}
