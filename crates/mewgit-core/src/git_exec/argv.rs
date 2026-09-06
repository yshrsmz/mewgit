//! [`GitOp`] を `git` の argv と標準入力に変換する純粋関数。
//!
//! 設計は `docs/design/03-git-exec.md` の「argv への変換」。表と実装がずれると
//! 静かに違うコマンドを実行するので、変種を足したらどちらも直す。

use std::path::{Path, PathBuf};

use super::{GitOp, PullMode};
use crate::git_read::Oid;

/// `git` に渡す 1 回分の呼び出し。
///
/// argv と標準入力を 1 つの値で返すのは、「どの引数で何を流すか」が 1 つの判断
/// だから（`git apply --cached -`）。2 つの関数に割ると片方だけ直る。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCommand {
    /// `git` 自身は含まない。付けるのは [`Runner`](super::Runner)。
    pub argv: Vec<String>,
    /// 標準入力に流すバイト列。`None` なら stdin は閉じる。
    pub stdin: Option<Vec<u8>>,
}

/// argv を組めなかった理由。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArgvError {
    /// `git_read` が非 UTF-8 のパスを一覧ごと落とすので実際には来ない。それでも
    /// 塞ぐのは、黙って lossy 変換すると別のファイルを操作してしまうため。
    #[error("パスが UTF-8 ではないため git に渡せません: {}", .path.display())]
    NonUtf8Path { path: PathBuf },
}

/// [`GitOp`] を `git` の呼び出しに変換する。
///
/// 網羅 `match` なので、`GitOp` に変種を足すとここがコンパイルエラーになる。
pub fn to_command(op: &GitOp) -> Result<GitCommand, ArgvError> {
    let mut argv: Vec<String> = Vec::new();
    let mut stdin: Option<Vec<u8>> = None;

    match op {
        GitOp::Stage(paths) => {
            argv.push("add".into());
            push_paths(&mut argv, paths)?;
        }
        // unstage が `restore --staged` ではなく `reset` なのは、コミットが 1 つも
        // 無いリポジトリで `restore --staged` が `could not resolve 'HEAD'` で
        // 落ちるため。`--quiet` は「Unstaged changes after reset:」を抑える。
        GitOp::Unstage(paths) => {
            argv.extend(["reset".into(), "--quiet".into()]);
            push_paths(&mut argv, paths)?;
        }
        // `path` は argv に載せない。対象はパッチのファイルヘッダが持つ。
        GitOp::StageHunk { path: _, patch } => {
            argv.extend(["apply".into(), "--cached".into(), "-".into()]);
            stdin = Some(patch.clone());
        }
        GitOp::UnstageHunk { path: _, patch } => {
            argv.extend([
                "apply".into(),
                "--cached".into(),
                "--reverse".into(),
                "-".into(),
            ]);
            stdin = Some(patch.clone());
        }
        GitOp::Discard(paths) => {
            argv.extend([
                "restore".into(),
                "--source=HEAD".into(),
                "--staged".into(),
                "--worktree".into(),
            ]);
            push_paths(&mut argv, paths)?;
        }
        // `-d` は付けない。`git_read::status` が untracked なディレクトリを再帰して
        // ファイルとして列挙するので、ディレクトリごと消す余地は要らない。
        GitOp::DiscardUntracked(paths) => {
            argv.extend(["clean".into(), "--force".into()]);
            push_paths(&mut argv, paths)?;
        }
        GitOp::Commit { message, amend } => {
            argv.push("commit".into());
            if *amend {
                argv.push("--amend".into());
            }
            argv.extend(["--message".into(), message.clone()]);
        }
        GitOp::BranchCreate {
            name,
            start,
            checkout,
        } => {
            if *checkout {
                argv.extend(["switch".into(), "--create".into(), name.clone()]);
            } else {
                argv.extend(["branch".into(), name.clone()]);
            }
            argv.extend(start.clone());
        }
        // 末尾の `--` は revision として読ませるため。同名のファイルがあるときに
        // 「そのファイルを取り出す」と解釈されるのを防ぐ。
        GitOp::Checkout(target) => {
            argv.extend(["checkout".into(), target.clone(), "--".into()]);
        }
        GitOp::BranchDelete { name, force } => {
            argv.extend(["branch".into(), "--delete".into()]);
            if *force {
                argv.push("--force".into());
            }
            argv.push(name.clone());
        }
        GitOp::BranchRename { from, to } => {
            argv.extend(["branch".into(), "--move".into(), from.clone(), to.clone()]);
        }
        GitOp::Fetch { remote, prune } => {
            argv.push("fetch".into());
            if *prune {
                argv.push("--prune".into());
            }
            argv.extend(remote.clone());
        }
        GitOp::Pull { mode } => {
            argv.extend([
                "pull".into(),
                match mode {
                    PullMode::Merge => "--no-rebase".into(),
                    PullMode::Rebase => "--rebase".into(),
                    PullMode::FastForwardOnly => "--ff-only".into(),
                },
            ]);
        }
        // force は `--force-with-lease`。リモートが自分の知らない位置に進んでいたら
        // 弾かれる。`--force` は他人のコミットを消せるので使わない。
        GitOp::Push {
            remote,
            branch,
            set_upstream,
            force,
        } => {
            argv.push("push".into());
            if *force {
                argv.push("--force-with-lease".into());
            }
            if *set_upstream {
                argv.push("--set-upstream".into());
            }
            argv.extend([remote.clone(), branch.clone()]);
        }
        // `plan` は argv に載らない。todo の差し替えは M5（`Runner` はそれまで
        // このオペを実行せずに拒否する）。
        GitOp::RebaseInteractive { onto, plan: _ } => {
            argv.extend(["rebase".into(), "--interactive".into(), onto.clone()]);
        }
        GitOp::RebaseContinue => argv.extend(["rebase".into(), "--continue".into()]),
        GitOp::RebaseSkip => argv.extend(["rebase".into(), "--skip".into()]),
        GitOp::RebaseAbort => argv.extend(["rebase".into(), "--abort".into()]),
        GitOp::CherryPick(oids) => {
            argv.push("cherry-pick".into());
            argv.extend(oids.iter().map(Oid::to_string));
        }
        GitOp::CherryPickContinue => argv.extend(["cherry-pick".into(), "--continue".into()]),
        GitOp::CherryPickAbort => argv.extend(["cherry-pick".into(), "--abort".into()]),
        GitOp::Revert { oids, mainline } => {
            argv.push("revert".into());
            if let Some(parent) = mainline {
                argv.extend(["--mainline".into(), parent.to_string()]);
            }
            argv.extend(oids.iter().map(Oid::to_string));
        }
        GitOp::RevertContinue => argv.extend(["revert".into(), "--continue".into()]),
        GitOp::RevertAbort => argv.extend(["revert".into(), "--abort".into()]),
        GitOp::StashPush {
            message,
            include_untracked,
            paths,
        } => {
            argv.extend(["stash".into(), "push".into()]);
            if *include_untracked {
                argv.push("--include-untracked".into());
            }
            if let Some(message) = message {
                argv.extend(["--message".into(), message.clone()]);
            }
            if let Some(paths) = paths {
                push_paths(&mut argv, paths)?;
            }
        }
        GitOp::StashApply(index) => argv.extend(["stash".into(), "apply".into(), stash(*index)]),
        GitOp::StashPop(index) => argv.extend(["stash".into(), "pop".into(), stash(*index)]),
        GitOp::StashDrop(index) => argv.extend(["stash".into(), "drop".into(), stash(*index)]),
    }

    Ok(GitCommand { argv, stdin })
}

/// pathspec を `--` の後ろに積む。
///
/// `--` は必ず置く。`-n` や `HEAD` という名前のファイルが、オプションや revision
/// として解釈されるのを防ぐため。
fn push_paths(argv: &mut Vec<String>, paths: &[PathBuf]) -> Result<(), ArgvError> {
    argv.push("--".into());
    for path in paths {
        argv.push(path_arg(path)?);
    }
    Ok(())
}

fn path_arg(path: &Path) -> Result<String, ArgvError> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| ArgvError::NonUtf8Path {
            path: path.to_path_buf(),
        })
}

/// `stash@{index}`。
fn stash(index: usize) -> String {
    format!("stash@{{{index}}}")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::git_exec::{RebaseAction, RebaseEntry, RebasePlan};

    /// 変種の名前。**網羅 `match` なので、`GitOp` に変種を足すとここが
    /// コンパイルエラーになる。** そのとき [`VARIANTS`] と [`cases`] にも足す
    /// （足し忘れは `covers_every_variant` が落とす）。
    fn variant(op: &GitOp) -> &'static str {
        match op {
            GitOp::Stage(_) => "Stage",
            GitOp::Unstage(_) => "Unstage",
            GitOp::StageHunk { .. } => "StageHunk",
            GitOp::UnstageHunk { .. } => "UnstageHunk",
            GitOp::Discard(_) => "Discard",
            GitOp::DiscardUntracked(_) => "DiscardUntracked",
            GitOp::Commit { .. } => "Commit",
            GitOp::BranchCreate { .. } => "BranchCreate",
            GitOp::Checkout(_) => "Checkout",
            GitOp::BranchDelete { .. } => "BranchDelete",
            GitOp::BranchRename { .. } => "BranchRename",
            GitOp::Fetch { .. } => "Fetch",
            GitOp::Pull { .. } => "Pull",
            GitOp::Push { .. } => "Push",
            GitOp::RebaseInteractive { .. } => "RebaseInteractive",
            GitOp::RebaseContinue => "RebaseContinue",
            GitOp::RebaseSkip => "RebaseSkip",
            GitOp::RebaseAbort => "RebaseAbort",
            GitOp::CherryPick(_) => "CherryPick",
            GitOp::CherryPickContinue => "CherryPickContinue",
            GitOp::CherryPickAbort => "CherryPickAbort",
            GitOp::Revert { .. } => "Revert",
            GitOp::RevertContinue => "RevertContinue",
            GitOp::RevertAbort => "RevertAbort",
            GitOp::StashPush { .. } => "StashPush",
            GitOp::StashApply(_) => "StashApply",
            GitOp::StashPop(_) => "StashPop",
            GitOp::StashDrop(_) => "StashDrop",
        }
    }

    /// [`variant`] が返しうる名前の全部。
    const VARIANTS: &[&str] = &[
        "Stage",
        "Unstage",
        "StageHunk",
        "UnstageHunk",
        "Discard",
        "DiscardUntracked",
        "Commit",
        "BranchCreate",
        "Checkout",
        "BranchDelete",
        "BranchRename",
        "Fetch",
        "Pull",
        "Push",
        "RebaseInteractive",
        "RebaseContinue",
        "RebaseSkip",
        "RebaseAbort",
        "CherryPick",
        "CherryPickContinue",
        "CherryPickAbort",
        "Revert",
        "RevertContinue",
        "RevertAbort",
        "StashPush",
        "StashApply",
        "StashPop",
        "StashDrop",
    ];

    fn oid(tag: char) -> Oid {
        Oid::from_str(&tag.to_string().repeat(40)).expect("Oid を作れませんでした")
    }

    fn path(name: &str) -> PathBuf {
        PathBuf::from(name)
    }

    /// 設計書「argv への変換」の表をそのまま写したもの。
    fn cases() -> Vec<(GitOp, Vec<&'static str>)> {
        vec![
            (
                GitOp::Stage(vec![path("a.txt"), path("dir/b.txt")]),
                vec!["add", "--", "a.txt", "dir/b.txt"],
            ),
            (
                GitOp::Unstage(vec![path("a.txt")]),
                vec!["reset", "--quiet", "--", "a.txt"],
            ),
            (
                GitOp::StageHunk {
                    path: path("a.txt"),
                    patch: b"@@ -1 +1 @@\n".to_vec(),
                },
                vec!["apply", "--cached", "-"],
            ),
            (
                GitOp::UnstageHunk {
                    path: path("a.txt"),
                    patch: b"@@ -1 +1 @@\n".to_vec(),
                },
                vec!["apply", "--cached", "--reverse", "-"],
            ),
            (
                GitOp::Discard(vec![path("a.txt")]),
                vec![
                    "restore",
                    "--source=HEAD",
                    "--staged",
                    "--worktree",
                    "--",
                    "a.txt",
                ],
            ),
            (
                GitOp::DiscardUntracked(vec![path("new.txt")]),
                vec!["clean", "--force", "--", "new.txt"],
            ),
            (
                GitOp::Commit {
                    message: "feat: 追加".into(),
                    amend: false,
                },
                vec!["commit", "--message", "feat: 追加"],
            ),
            (
                GitOp::Commit {
                    message: "fix".into(),
                    amend: true,
                },
                vec!["commit", "--amend", "--message", "fix"],
            ),
            (
                GitOp::BranchCreate {
                    name: "feat/x".into(),
                    start: Some("main".into()),
                    checkout: true,
                },
                vec!["switch", "--create", "feat/x", "main"],
            ),
            (
                GitOp::BranchCreate {
                    name: "feat/x".into(),
                    start: None,
                    checkout: false,
                },
                vec!["branch", "feat/x"],
            ),
            (
                GitOp::Checkout("main".into()),
                vec!["checkout", "main", "--"],
            ),
            (
                GitOp::BranchDelete {
                    name: "old".into(),
                    force: false,
                },
                vec!["branch", "--delete", "old"],
            ),
            (
                GitOp::BranchDelete {
                    name: "old".into(),
                    force: true,
                },
                vec!["branch", "--delete", "--force", "old"],
            ),
            (
                GitOp::BranchRename {
                    from: "old".into(),
                    to: "new".into(),
                },
                vec!["branch", "--move", "old", "new"],
            ),
            (
                GitOp::Fetch {
                    remote: Some("origin".into()),
                    prune: true,
                },
                vec!["fetch", "--prune", "origin"],
            ),
            (
                GitOp::Fetch {
                    remote: None,
                    prune: false,
                },
                vec!["fetch"],
            ),
            (
                GitOp::Pull {
                    mode: PullMode::Merge,
                },
                vec!["pull", "--no-rebase"],
            ),
            (
                GitOp::Pull {
                    mode: PullMode::Rebase,
                },
                vec!["pull", "--rebase"],
            ),
            (
                GitOp::Pull {
                    mode: PullMode::FastForwardOnly,
                },
                vec!["pull", "--ff-only"],
            ),
            (
                GitOp::Push {
                    remote: "origin".into(),
                    branch: "main".into(),
                    set_upstream: true,
                    force: true,
                },
                vec![
                    "push",
                    "--force-with-lease",
                    "--set-upstream",
                    "origin",
                    "main",
                ],
            ),
            (
                GitOp::Push {
                    remote: "origin".into(),
                    branch: "main".into(),
                    set_upstream: false,
                    force: false,
                },
                vec!["push", "origin", "main"],
            ),
            (
                GitOp::RebaseInteractive {
                    onto: "main".into(),
                    plan: RebasePlan {
                        entries: vec![RebaseEntry {
                            action: RebaseAction::Pick,
                            oid: oid('1'),
                            summary: "c1".into(),
                            message: None,
                        }],
                    },
                },
                vec!["rebase", "--interactive", "main"],
            ),
            (GitOp::RebaseContinue, vec!["rebase", "--continue"]),
            (GitOp::RebaseSkip, vec!["rebase", "--skip"]),
            (GitOp::RebaseAbort, vec!["rebase", "--abort"]),
            (
                GitOp::CherryPick(vec![oid('1'), oid('2')]),
                vec![
                    "cherry-pick",
                    "1111111111111111111111111111111111111111",
                    "2222222222222222222222222222222222222222",
                ],
            ),
            (GitOp::CherryPickContinue, vec!["cherry-pick", "--continue"]),
            (GitOp::CherryPickAbort, vec!["cherry-pick", "--abort"]),
            (
                GitOp::Revert {
                    oids: vec![oid('3')],
                    mainline: Some(1),
                },
                vec![
                    "revert",
                    "--mainline",
                    "1",
                    "3333333333333333333333333333333333333333",
                ],
            ),
            (
                GitOp::Revert {
                    oids: vec![oid('3')],
                    mainline: None,
                },
                vec!["revert", "3333333333333333333333333333333333333333"],
            ),
            (GitOp::RevertContinue, vec!["revert", "--continue"]),
            (GitOp::RevertAbort, vec!["revert", "--abort"]),
            (
                GitOp::StashPush {
                    message: Some("wip".into()),
                    include_untracked: true,
                    paths: Some(vec![path("a.txt")]),
                },
                vec![
                    "stash",
                    "push",
                    "--include-untracked",
                    "--message",
                    "wip",
                    "--",
                    "a.txt",
                ],
            ),
            (
                GitOp::StashPush {
                    message: None,
                    include_untracked: false,
                    paths: None,
                },
                vec!["stash", "push"],
            ),
            (GitOp::StashApply(2), vec!["stash", "apply", "stash@{2}"]),
            (GitOp::StashPop(0), vec!["stash", "pop", "stash@{0}"]),
            (GitOp::StashDrop(1), vec!["stash", "drop", "stash@{1}"]),
        ]
    }

    #[test]
    fn argv_matches_the_design_table() {
        for (op, expected) in cases() {
            let command = to_command(&op).expect("argv を組めませんでした");
            assert_eq!(command.argv, expected, "{op:?}");
        }
    }

    #[test]
    fn covers_every_variant() {
        let covered: BTreeSet<_> = cases().iter().map(|(op, _)| variant(op)).collect();
        let all: BTreeSet<_> = VARIANTS.iter().copied().collect();
        assert_eq!(covered, all, "argv の表に無い変種がある");
    }

    #[test]
    fn only_hunk_ops_use_stdin() {
        for (op, _) in cases() {
            let command = to_command(&op).expect("argv を組めませんでした");
            let expects_stdin = matches!(op, GitOp::StageHunk { .. } | GitOp::UnstageHunk { .. });
            assert_eq!(command.stdin.is_some(), expects_stdin, "{op:?}");
        }
    }

    #[test]
    fn hunk_patch_goes_to_stdin_unchanged() {
        // 非 UTF-8 のバイト列が lossy にならないこと。ここが崩れると、ステージ
        // した結果が元のファイルと変わる。
        let patch = b"@@ -1 +1 @@\n-\xff\xfe\n+\xff\xfd\n".to_vec();
        let command = to_command(&GitOp::StageHunk {
            path: path("a.bin"),
            patch: patch.clone(),
        })
        .expect("argv を組めませんでした");
        assert_eq!(command.stdin, Some(patch));
    }

    #[test]
    fn pathspec_is_separated_so_dashed_names_are_not_options() {
        // `-n` という名前のファイルがオプションとして読まれないこと。
        let command = to_command(&GitOp::Stage(vec![path("-n"), path("--all")]))
            .expect("argv を組めませんでした");
        assert_eq!(command.argv, ["add", "--", "-n", "--all"]);
    }

    #[test]
    fn non_utf8_path_is_refused() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let bad = PathBuf::from(OsString::from_vec(vec![0x66, 0xff, 0x2e, 0x74]));
        let err = to_command(&GitOp::Stage(vec![bad.clone()])).expect_err("通ってはいけません");
        assert_eq!(err, ArgvError::NonUtf8Path { path: bad });
    }
}
