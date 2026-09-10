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

/// `git` に渡す形（argv とパッチ）を組めなかった理由。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArgvError {
    /// `git_read` が非 UTF-8 のパスを一覧ごと落とすので実際には来ない。それでも
    /// 塞ぐのは、黙って lossy 変換すると別のファイルを操作してしまうため。
    #[error("パスが UTF-8 ではないため git に渡せません: {}", .path.display())]
    NonUtf8Path { path: PathBuf },
    /// パッチのヘッダに quote せずには載せられない名前
    /// （[`hunk_patch`](super::hunk_patch)）。`git apply` が名前の区切りとして読む
    /// 文字を含むので、そのまま載せると**別のファイルに patch を当てる**。
    #[error("パスに制御文字か先頭の引用符が含まれるため、ハンク単位では操作できません: {}", .path.display())]
    UnquotablePath { path: PathBuf },
    /// pathspec が空。`--` だけを置くと「指定なし」= リポジトリ全体になる。
    #[error("パスの指定が空です: git {} -- は対象をリポジトリ全体に広げてしまいます", .argv.join(" "))]
    EmptyPathspec { argv: Vec<String> },
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
            argv.extend(apply_hunk(&["--cached"]));
            stdin = Some(patch.clone());
        }
        GitOp::UnstageHunk { path: _, patch } => {
            argv.extend(apply_hunk(&["--cached", "--reverse"]));
            stdin = Some(patch.clone());
        }
        // **`--cached` を付けない。** 当たるのはワークツリーだけ（03）。
        GitOp::DiscardHunk { path: _, patch } => {
            argv.extend(apply_hunk(&["--reverse"]));
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
        // HEAD に無いファイル専用。`restore --source=HEAD` はコミットが 1 つも
        // 無いリポジトリで `could not resolve 'HEAD'` で落ちるが、`git rm --force` は
        // 通り、しかも HEAD があるときに staged な `Added` へ `restore` を掛けた結果
        // （index からもワークツリーからも消える）と同じになる（git 2.55 で確認）。
        // `--quiet` は消したファイルの一覧を出させないため。
        GitOp::DiscardAdded(paths) => {
            argv.extend(["rm".into(), "--force".into(), "--quiet".into()]);
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
            switch,
        } => {
            if *switch {
                argv.extend(["switch".into(), "--create".into(), name.clone()]);
            } else {
                argv.extend(["branch".into(), name.clone()]);
            }
            argv.extend(start.clone());
        }
        // **`checkout` ではなく `switch`。** `switch` は pathspec を取らないので、
        // 同名のファイルがあっても「そのファイルを取り出す」に化けない（03）。
        GitOp::Switch(target) => {
            argv.extend(["switch".into(), target.clone()]);
        }
        GitOp::BranchDelete { name, force } => {
            argv.extend(["branch".into(), "--delete".into()]);
            if *force {
                argv.push("--force".into());
            }
            argv.push(name.clone());
        }
        // **`--` を置く。** `to` は**ユーザーが打った文字がそのまま positional に
        // 載る唯一の場所**で、`git branch` は parse-options なので `-` で始まる値を
        // オプションとして拾う。`git branch --move topic --force` は「`--force` に
        // 改名」ではなく「HEAD を `topic` へ強制改名」になり、**元の `topic` が
        // 黙って消えて exit 0 で返る**（git 2.55 で確認）。`--` があれば
        // `fatal: '--force' is not a valid branch name` で断られる。
        //
        // 同じ穴が `BranchCreate` に無いのは、`--create` が次の argv を無条件に
        // 名前として食うため（`switch --create --force` も同じ fatal になる）。
        // **`--create` の直後に `--` を置いてはいけない** — `--` が名前になり、
        // 続く名前が起点として読まれて `fatal: invalid reference` になる。
        GitOp::BranchRename { from, to } => {
            argv.extend([
                "branch".into(),
                "--move".into(),
                "--".into(),
                from.clone(),
                to.clone(),
            ]);
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
        // force は `--force-with-lease`。`--force` は他人のコミットを消せるので
        // 使わない。**値は必ず添わる** —— `lease` が `Some` のときだけ force になるので、
        // 値の無い `--force-with-lease`（remote-tracking ref と比べるだけになり、
        // `git fetch` の後は他人が進めた位置がそこに入って検査を素通りする。
        // git 2.55 で確認）はここでは組めない。
        //
        // **lease の refname は refspec の右辺と同じ文字列。** 短縮名でも完全な ref 名でも
        // 効く（git 2.55 で確認）ので、`dst` をそのまま使う。
        GitOp::Push {
            remote,
            src,
            dst,
            set_upstream,
            lease,
        } => {
            argv.push("push".into());
            if let Some(oid) = lease {
                let refname = push_remote_ref(src, dst.as_deref());
                argv.push(format!("--force-with-lease={refname}:{oid}"));
            }
            if *set_upstream {
                argv.push("--set-upstream".into());
            }
            let refspec = match dst {
                Some(dst) => format!("{src}:{dst}"),
                None => src.clone(),
            };
            argv.extend([remote.clone(), refspec]);
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
        // **`--` は守りになっていない**（03 の表）。`git config` はキーより後ろを
        // オプションとして読まないので、値が `--global` でも `-4` でも `--` の有無で
        // 結果は同じ（git 2.55 で実測）。それでも置くのは、外すと根拠が
        // 「いまの git はそう読まない」だけになるから —— `git branch --move` は
        // まさにそれで足をすくわれた（下の `BranchRename`）。
        GitOp::SetAccount { login } => argv.extend([
            "config".into(),
            "--local".into(),
            "--".into(),
            crate::auth::MEWGIT_USER.into(),
            login.clone(),
        ]),
    }

    Ok(GitCommand { argv, stdin })
}

/// `GitOp::Push` が動かすリモート側の ref 名。
///
/// `dst` が `None` なら「ローカルと同じ名前」（01 の「push の行き先」）。**refspec の
/// 右辺と `--force-with-lease` の refname は同じ文字列**なので、確認モーダルの文面を
/// 組む `app` もここを通す —— 2 箇所で `unwrap_or` を書くと、片方だけ直したときに
/// 「見せた行き先」と「動かす ref」がずれる。
pub fn push_remote_ref<'a>(src: &'a str, dst: Option<&'a str>) -> &'a str {
    dst.unwrap_or(src)
}

/// pathspec を `--` の後ろに積む。
///
/// `--` は必ず置く。`-n` や `HEAD` という名前のファイルが、オプションや revision
/// として解釈されるのを防ぐため。
///
/// **空の `paths` は拒否する。** `--` だけを置くと git は「pathspec の指定なし」と
/// 読み、対象がリポジトリ全体に広がる。`git reset --quiet --` は index 全体を戻し、
/// `git clean --force --` は untracked を全部消す（git 2.55 で確認）。「0 件を
/// discard します」と確認して「全部 discard」が走ることになる。
fn push_paths(argv: &mut Vec<String>, paths: &[PathBuf]) -> Result<(), ArgvError> {
    if paths.is_empty() {
        return Err(ArgvError::EmptyPathspec {
            argv: std::mem::take(argv),
        });
    }
    argv.push("--".into());
    for path in paths {
        argv.push(path_arg(path)?);
    }
    Ok(())
}

/// ハンクのパッチを標準入力から流す `git apply` の argv。`extra` は `--cached` /
/// `--reverse`。
///
/// **`--whitespace=nowarn` をここで必ず付ける。** 既定はユーザーの
/// `apply.whitespace` を読むので、`fix`（よく勧められる設定）だと git が行末の空白を
/// 落としてから index に入れる。画面に出したハンクと index の中身が静かに食い違い、
/// `git status` は `MM` のまま残る。`error` だと逆に操作ごと落ちる。**どちらも
/// 「見えているハンクをそのまま index へ」を壊す**ので、設定に関わらず素通しにする
/// （git 2.55 で確認）。
///
/// 3 変種で 1 本にしているのは、変種を足したときに付け忘れないため。
fn apply_hunk(extra: &[&str]) -> Vec<String> {
    let mut argv = vec!["apply".to_string()];
    argv.extend(extra.iter().map(|flag| (*flag).to_string()));
    argv.extend(["--whitespace=nowarn".to_string(), "-".to_string()]);
    argv
}

/// argv やパッチのヘッダに載せるパス。非 UTF-8 は載せられないので拒否する。
pub(super) fn path_arg(path: &Path) -> Result<String, ArgvError> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| ArgvError::NonUtf8Path {
            path: path.to_path_buf(),
        })
}

impl GitOp {
    /// この操作が走らせる `git` のサブコマンド。スピナーの説明に出す
    /// （01 の「実行中ジョブ」）。
    ///
    /// **[`to_command`] の `argv[0]` と同じものを返す。** 別々に書いても
    /// コンパイルは通るので、`verb_matches_the_argv` が全変種で突き合わせる。
    /// `to_command` から取らないのは、語を 1 つ得るためにハンクのパッチまで
    /// 組み直すことになるため（[`to_command`] は `Result` でもある）。
    ///
    /// 網羅 `match` にしているのは、変種を足したときに選ばせるため。
    pub fn verb(&self) -> &'static str {
        match self {
            GitOp::Stage(_) => "add",
            GitOp::Unstage(_) => "reset",
            GitOp::StageHunk { .. } | GitOp::UnstageHunk { .. } | GitOp::DiscardHunk { .. } => {
                "apply"
            }
            GitOp::Discard(_) => "restore",
            GitOp::DiscardUntracked(_) => "clean",
            GitOp::DiscardAdded(_) => "rm",
            GitOp::Commit { .. } => "commit",
            // 切り替えるかどうかで `switch --create` と `branch` に分かれる。
            GitOp::BranchCreate { switch: true, .. } => "switch",
            GitOp::BranchCreate { switch: false, .. } => "branch",
            GitOp::Switch(_) => "switch",
            GitOp::BranchDelete { .. } | GitOp::BranchRename { .. } => "branch",
            GitOp::Fetch { .. } => "fetch",
            GitOp::Pull { .. } => "pull",
            GitOp::Push { .. } => "push",
            GitOp::RebaseInteractive { .. }
            | GitOp::RebaseContinue
            | GitOp::RebaseSkip
            | GitOp::RebaseAbort => "rebase",
            GitOp::CherryPick(_) | GitOp::CherryPickContinue | GitOp::CherryPickAbort => {
                "cherry-pick"
            }
            GitOp::Revert { .. } | GitOp::RevertContinue | GitOp::RevertAbort => "revert",
            GitOp::StashPush { .. }
            | GitOp::StashApply(_)
            | GitOp::StashPop(_)
            | GitOp::StashDrop(_) => "stash",
            GitOp::SetAccount { .. } => "config",
        }
    }
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
    use crate::test_util::fake_oid;

    /// 変種の名前。**網羅 `match` なので、`GitOp` に変種を足すとここが
    /// コンパイルエラーになる。** そのとき [`VARIANTS`] と [`cases`] にも足す
    /// （足し忘れは `covers_every_variant` が落とす）。
    fn variant(op: &GitOp) -> &'static str {
        match op {
            GitOp::Stage(_) => "Stage",
            GitOp::Unstage(_) => "Unstage",
            GitOp::StageHunk { .. } => "StageHunk",
            GitOp::UnstageHunk { .. } => "UnstageHunk",
            GitOp::DiscardHunk { .. } => "DiscardHunk",
            GitOp::Discard(_) => "Discard",
            GitOp::DiscardUntracked(_) => "DiscardUntracked",
            GitOp::DiscardAdded(_) => "DiscardAdded",
            GitOp::Commit { .. } => "Commit",
            GitOp::BranchCreate { .. } => "BranchCreate",
            GitOp::Switch(_) => "Switch",
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
            GitOp::SetAccount { .. } => "SetAccount",
        }
    }

    /// [`variant`] のアームの数。`GitOp` に変種を足すと `variant` の網羅 `match` が
    /// コンパイルエラーになり、ここに来る。アームと一緒にこの数と [`cases`] も足す
    /// （足し忘れは `covers_every_variant` が落とす）。
    const VARIANT_COUNT: usize = 31;

    /// 設計書「argv への変換」の表をそのまま写したもの。
    fn cases() -> Vec<(GitOp, Vec<&'static str>)> {
        vec![
            (
                GitOp::Stage(vec!["a.txt".into(), "dir/b.txt".into()]),
                vec!["add", "--", "a.txt", "dir/b.txt"],
            ),
            (
                GitOp::Unstage(vec!["a.txt".into()]),
                vec!["reset", "--quiet", "--", "a.txt"],
            ),
            (
                GitOp::StageHunk {
                    path: "a.txt".into(),
                    patch: b"@@ -1 +1 @@\n".to_vec(),
                },
                vec!["apply", "--cached", "--whitespace=nowarn", "-"],
            ),
            (
                GitOp::UnstageHunk {
                    path: "a.txt".into(),
                    patch: b"@@ -1 +1 @@\n".to_vec(),
                },
                vec!["apply", "--cached", "--reverse", "--whitespace=nowarn", "-"],
            ),
            (
                GitOp::DiscardHunk {
                    path: "a.txt".into(),
                    patch: b"@@ -1 +1 @@\n".to_vec(),
                },
                // `--cached` が無い。付くと index を戻してワークツリーが残る。
                vec!["apply", "--reverse", "--whitespace=nowarn", "-"],
            ),
            (
                GitOp::Discard(vec!["a.txt".into()]),
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
                GitOp::DiscardUntracked(vec!["new.txt".into()]),
                vec!["clean", "--force", "--", "new.txt"],
            ),
            (
                GitOp::DiscardAdded(vec!["a.txt".into()]),
                vec!["rm", "--force", "--quiet", "--", "a.txt"],
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
                    switch: true,
                },
                vec!["switch", "--create", "feat/x", "main"],
            ),
            (
                GitOp::BranchCreate {
                    name: "feat/x".into(),
                    start: None,
                    switch: false,
                },
                vec!["branch", "feat/x"],
            ),
            (GitOp::Switch("main".into()), vec!["switch", "main"]),
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
                vec!["branch", "--move", "--", "old", "new"],
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
                    src: "main".into(),
                    dst: None,
                    set_upstream: true,
                    lease: Some(fake_oid(4)),
                },
                vec![
                    "push",
                    "--force-with-lease=main:0000000000000000000000000000000000000004",
                    "--set-upstream",
                    "origin",
                    "main",
                ],
            ),
            (
                GitOp::Push {
                    remote: "origin".into(),
                    src: "main".into(),
                    dst: None,
                    set_upstream: false,
                    lease: None,
                },
                vec!["push", "origin", "main"],
            ),
            (
                GitOp::RebaseInteractive {
                    onto: "main".into(),
                    plan: RebasePlan {
                        entries: vec![RebaseEntry {
                            action: RebaseAction::Pick,
                            oid: fake_oid(1),
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
                GitOp::CherryPick(vec![fake_oid(1), fake_oid(2)]),
                vec![
                    "cherry-pick",
                    "0000000000000000000000000000000000000001",
                    "0000000000000000000000000000000000000002",
                ],
            ),
            (GitOp::CherryPickContinue, vec!["cherry-pick", "--continue"]),
            (GitOp::CherryPickAbort, vec!["cherry-pick", "--abort"]),
            (
                GitOp::Revert {
                    oids: vec![fake_oid(3)],
                    mainline: Some(1),
                },
                vec![
                    "revert",
                    "--mainline",
                    "1",
                    "0000000000000000000000000000000000000003",
                ],
            ),
            (
                GitOp::Revert {
                    oids: vec![fake_oid(3)],
                    mainline: None,
                },
                vec!["revert", "0000000000000000000000000000000000000003"],
            ),
            (GitOp::RevertContinue, vec!["revert", "--continue"]),
            (GitOp::RevertAbort, vec!["revert", "--abort"]),
            (
                GitOp::StashPush {
                    message: Some("wip".into()),
                    include_untracked: true,
                    paths: Some(vec!["a.txt".into()]),
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
            (
                GitOp::SetAccount {
                    login: "taro_acme".into(),
                },
                vec!["config", "--local", "--", "mewgit.user", "taro_acme"],
            ),
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
    fn verb_matches_the_argv() {
        // スピナーの説明と実際に走る git がずれないこと。`cases` は全変種を
        // 覆っている（`covers_every_variant`）ので、ここも全変種を回る。
        for (op, _) in cases() {
            let command = to_command(&op).expect("argv を組めませんでした");
            assert_eq!(op.verb(), command.argv[0], "{op:?}");
        }
    }

    #[test]
    fn covers_every_variant() {
        let covered: BTreeSet<_> = cases().iter().map(|(op, _)| variant(op)).collect();
        assert_eq!(
            covered.len(),
            VARIANT_COUNT,
            "argv の表に無い変種がある。覆えているのは {covered:?}"
        );
    }

    #[test]
    fn only_hunk_ops_use_stdin() {
        for (op, _) in cases() {
            let command = to_command(&op).expect("argv を組めませんでした");
            let expects_stdin = matches!(
                op,
                GitOp::StageHunk { .. } | GitOp::UnstageHunk { .. } | GitOp::DiscardHunk { .. }
            );
            assert_eq!(command.stdin.is_some(), expects_stdin, "{op:?}");
        }
    }

    #[test]
    fn hunk_patch_goes_to_stdin_unchanged() {
        // 非 UTF-8 のバイト列が lossy にならないこと。ここが崩れると、ステージ
        // した結果が元のファイルと変わる。
        let patch = b"@@ -1 +1 @@\n-\xff\xfe\n+\xff\xfd\n".to_vec();
        let command = to_command(&GitOp::StageHunk {
            path: "a.bin".into(),
            patch: patch.clone(),
        })
        .expect("argv を組めませんでした");
        assert_eq!(command.stdin, Some(patch));
    }

    #[test]
    fn pathspec_is_separated_so_dashed_names_are_not_options() {
        // `-n` という名前のファイルがオプションとして読まれないこと。
        let command = to_command(&GitOp::Stage(vec!["-n".into(), "--all".into()]))
            .expect("argv を組めませんでした");
        assert_eq!(command.argv, ["add", "--", "-n", "--all"]);
    }

    #[test]
    fn an_empty_pathspec_is_refused() {
        // `--` だけを置くと「指定なし」= リポジトリ全体になる。確認モーダルが
        // 「0 件」と言いながら全体を巻き込むので、argv を組む側で止める。
        for op in [
            GitOp::Stage(Vec::new()),
            GitOp::Unstage(Vec::new()),
            GitOp::Discard(Vec::new()),
            GitOp::DiscardUntracked(Vec::new()),
            GitOp::DiscardAdded(Vec::new()),
            GitOp::StashPush {
                message: None,
                include_untracked: false,
                paths: Some(Vec::new()),
            },
        ] {
            let err = to_command(&op).expect_err("通ってはいけません: {op:?}");
            assert!(
                matches!(err, ArgvError::EmptyPathspec { .. }),
                "{op:?} -> {err:?}"
            );
        }
    }

    #[test]
    fn stash_without_paths_still_covers_everything() {
        // `paths: None` は「全部」を表す正当な指定。`Some(vec![])` と区別する。
        let command = to_command(&GitOp::StashPush {
            message: None,
            include_untracked: false,
            paths: None,
        })
        .expect("argv を組めませんでした");
        assert_eq!(command.argv, ["stash", "push"]);
    }

    #[test]
    fn a_renamed_destination_lands_in_both_the_refspec_and_the_lease() {
        // **同じ「リモート側の ref 名」が 2 箇所に要る。** refspec では右辺、lease では
        // 先頭。1 本の文字列に畳んでいた頃は lease が `x:main:<oid>` に化けていた。
        let command = to_command(&GitOp::Push {
            remote: "origin".into(),
            src: "x".into(),
            dst: Some("main".into()),
            set_upstream: false,
            lease: Some(fake_oid(4)),
        })
        .expect("argv を組めませんでした");
        assert_eq!(
            command.argv,
            [
                "push",
                "--force-with-lease=main:0000000000000000000000000000000000000004",
                "origin",
                "x:main",
            ]
        );
    }

    #[test]
    fn a_full_ref_destination_is_passed_through() {
        // `refs/heads/` の下に無い追跡先は短縮できない（01）。lease の refname も
        // 同じ文字列でよい（完全な ref 名でも効く。git 2.55 で確認）。
        let command = to_command(&GitOp::Push {
            remote: "origin".into(),
            src: "x".into(),
            dst: Some("refs/for/main".into()),
            set_upstream: false,
            lease: None,
        })
        .expect("argv を組めませんでした");
        assert_eq!(command.argv, ["push", "origin", "x:refs/for/main"]);
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
