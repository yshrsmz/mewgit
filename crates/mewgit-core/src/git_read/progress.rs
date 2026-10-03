//! 進行中操作の検出（02 の「進行中操作の検出」）。
//!
//! **git2 を使わず、gitdir のファイルだけを見る。** `git2::Repository::state()` は
//! `-i` の有無を区別できないうえ、`sequencer/` だけが残った形を「無い」と読む
//! （libgit2 1.9。02 の実測）。

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::{InProgressOp, Oid, RefusedReword, RepoReader, Step};

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
            branch: head_name(&merge),
            // 見るにはコミットのメッセージが要る（git2）ので、`status()` が
            // [`RepoReader::refused_reword`] で書き足す。
            refused_reword: None,
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
            InProgressOp::Rebase {
                step,
                branch: head_name(&apply),
                // mewgit の reword は merge backend でしか走らない。
                refused_reword: None,
            }
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

impl RepoReader {
    /// mewgit が渡した reword が断られ、元のメッセージでコミットしたまま止まった形
    /// （02 の「断られた reword」）。当たらなければ `None`。
    ///
    /// 条件は 3 つ —— `done` の最終行が `reword <X>`、`mewgit-reword-<X>` が在る、HEAD が
    /// X の pick である（メッセージと author が X と同じ。sequencer は author を日時まで
    /// 保つ）。最後の条件が、reword の pick がコンフリクトして止まった形（HEAD は 1 本前の
    /// コミット）、そこから `reset --hard` した形、端末で amend し直した形を外す。
    /// **`rebase-merge/amend` は見ない** —— pick が fast-forward した形にしか立たない
    /// （03 の実測）。読めないものは「当たらない」と読む（[`in_progress`] と同じ）。
    pub fn refused_reword(&self) -> Option<RefusedReword> {
        let rebase_dir = self.git_dir().join("rebase-merge");
        let oid = last_done_reword(&rebase_dir).ok()??;
        let message = fs::read_to_string(reword_message_path(&rebase_dir, &oid)).ok()?;
        let stopped = self.repo.find_commit(Oid::from_str(&oid).ok()?).ok()?;
        let head = self.repo.head().ok()?.peel_to_commit().ok()?;
        // **メッセージだけでは足りない** —— 1 本前のコミットが同じメッセージだと、
        // コンフリクトから `reset --hard` した形を取り違える（`MERGE_MSG` も消える）。
        let author = |commit: &git2::Commit| {
            let author = commit.author();
            (
                author.name_bytes().to_vec(),
                author.email_bytes().to_vec(),
                author.when(),
            )
        };
        let picked =
            head.message_bytes() == stopped.message_bytes() && author(&head) == author(&stopped);
        picked.then(|| RefusedReword {
            head: head.id(),
            message,
        })
    }
}

/// `oid` を reword したメッセージを置くファイル。`rebase_dir` は `rebase-merge/`。完了でも
/// abort でも git がディレクトリごと消すので、後始末は要らない。
///
/// **Oid で引く。** commit editor が知っているのは `done` の最終行（`reword <Oid> …`）
/// だけなので、何番目の行かでは引けない。書くのは `git_exec::rebase` の sequence editor、
/// 読むのは commit editor と [`RepoReader::refused_reword`] で、名前の規則はここ 1 本
/// （02 の「断られた reword」）。
pub fn reword_message_path(rebase_dir: &Path, oid: &str) -> PathBuf {
    rebase_dir.join(format!("mewgit-reword-{oid}"))
}

/// `rebase_dir/done` の最終行が `reword <Oid> …` なら、その Oid の字。
///
/// `done` が無い（rebase の外、reword より前）ときは `Ok(None)`。ほかの読めない理由は
/// `Err` で返す —— commit editor は git に非 0 で知らせ、見分けの側は「当たらない」と読む。
pub fn last_done_reword(rebase_dir: &Path) -> std::io::Result<Option<String>> {
    let done = match fs::read_to_string(rebase_dir.join("done")) {
        Ok(done) => done,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let mut last = done
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .split_whitespace();
    Ok(match (last.next(), last.next()) {
        (Some("reword"), Some(oid)) => Some(oid.to_string()),
        _ => None,
    })
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

/// `dir/head-name` のブランチ名。**detached HEAD で始めた rebase では
/// `detached HEAD` の 1 行**なので、`refs/heads/` で始まらなければ `None`
/// （merge / apply の両 backend で同じ形。02 の実測）。
fn head_name(dir: &Path) -> Option<String> {
    let body = fs::read_to_string(dir.join("head-name")).ok()?;
    body.trim_end()
        .strip_prefix("refs/heads/")
        .map(str::to_string)
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
            Some(InProgressOp::Rebase {
                step: None,
                branch: None,
                refused_reword: None,
            })
        );
    }

    #[test]
    fn a_rebase_started_on_a_detached_head_has_no_branch() {
        // 実物の git が書く字そのまま（git 2.55.0）。
        let dir = git_dir();
        fs::create_dir(dir.path().join("rebase-merge")).unwrap();
        fs::write(dir.path().join("rebase-merge/head-name"), "detached HEAD\n").unwrap();

        assert_eq!(
            in_progress(dir.path()),
            Some(InProgressOp::Rebase {
                step: None,
                branch: None,
                refused_reword: None,
            })
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
