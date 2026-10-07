//! [`GitOp`] を `git` の argv と標準入力に変換する純粋関数。
//!
//! 設計は `docs/design/03-git-exec.md` の「argv への変換」。表と実装がずれると
//! 静かに違うコマンドを実行するので、変種を足したらどちらも直す。

use std::path::{Path, PathBuf};

use super::rebase::SequenceInput;
use super::{GitOp, LocalBranch, PrCheckout, PrSource, PullMode, RebasePlanError};
use crate::git_read::RefusedReword;
use crate::git_read::{stash_ref as stash, Oid};

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
    /// git に渡すと途中で止まるか断られる plan（03 の「Interactive rebase の実装」）。
    #[error(transparent)]
    InvalidRebasePlan(#[from] RebasePlanError),
}

/// [`GitOp`] を `git` の呼び出しの列に変換する。
///
/// **ほとんどの変種で列の長さは 1。** git を続けて叩くのは [`GitOp::CheckoutPr`] と、
/// 断られた reword を持つ [`GitOp::RebaseContinue`] だけ（03 の「複数の手順を持つ書き込み」）。`Runner` は列を先頭から順に叩き、非 0 で
/// 止まる。
///
/// 網羅 `match` なので、`GitOp` に変種を足すとここがコンパイルエラーになる。
pub fn to_commands(op: &GitOp) -> Result<Vec<GitCommand>, ArgvError> {
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
        GitOp::StageHunk(hunk) => {
            argv.extend(apply_hunk(&["--cached"]));
            stdin = Some(hunk.patch.clone());
        }
        GitOp::UnstageHunk(hunk) => {
            argv.extend(apply_hunk(&["--cached", "--reverse"]));
            stdin = Some(hunk.patch.clone());
        }
        // **`--cached` を付けない。** 当たるのはワークツリーだけ（03）。
        GitOp::DiscardHunk(hunk) => {
            argv.extend(apply_hunk(&["--reverse"]));
            stdin = Some(hunk.patch.clone());
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
        // `plan` は argv に載らない。JSON にして標準入力に流し、git から継いだ
        // sequence editor が読む（03）。**`--no-update-refs` は todo を丸ごと書き換えるため** ——
        // `rebase.updateRefs` が足す `update-ref` の行が黙って消えるより、効かないことを
        // argv に書いておく。
        GitOp::RebaseInteractive { onto, head, plan } => {
            plan.validate()?;
            argv.extend([
                "rebase".into(),
                "--interactive".into(),
                "--no-update-refs".into(),
                onto.clone(),
            ]);
            let input = SequenceInput {
                head: *head,
                plan: plan.clone(),
            };
            stdin = Some(serde_json::to_vec(&input).expect("文字列だけの値は必ず直列化できる"));
        }
        GitOp::RebaseContinue { reword: None } => {
            argv.extend(["rebase".into(), "--continue".into()]);
        }
        GitOp::RebaseContinue {
            reword: Some(reword),
        } => return Ok(continue_refused_reword(reword)),
        GitOp::RebaseSkip => argv.extend(["rebase".into(), "--skip".into()]),
        GitOp::RebaseAbort => argv.extend(["rebase".into(), "--abort".into()]),
        GitOp::CherryPick(oids) => {
            argv.push("cherry-pick".into());
            argv.extend(oids.iter().map(Oid::to_string));
        }
        GitOp::CherryPickContinue => argv.extend(["cherry-pick".into(), "--continue".into()]),
        GitOp::CherryPickSkip => argv.extend(["cherry-pick".into(), "--skip".into()]),
        GitOp::CherryPickAbort => argv.extend(["cherry-pick".into(), "--abort".into()]),
        GitOp::Revert { oids, mainline } => {
            argv.push("revert".into());
            if let Some(parent) = mainline {
                argv.extend(["--mainline".into(), parent.to_string()]);
            }
            argv.extend(oids.iter().map(Oid::to_string));
        }
        GitOp::RevertContinue => argv.extend(["revert".into(), "--continue".into()]),
        GitOp::RevertSkip => argv.extend(["revert".into(), "--skip".into()]),
        GitOp::RevertAbort => argv.extend(["revert".into(), "--abort".into()]),
        GitOp::MergeAbort => argv.extend(["merge".into(), "--abort".into()]),
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
        // **`--` は `--unset` より後ろ。** 前に置くと `--unset` 自体がキーとして
        // 読まれ、`key does not contain a section: --unset` で exit 2 になる
        // （git 2.55 で実測。03 の表）。
        GitOp::UnsetAccount => argv.extend([
            "config".into(),
            "--local".into(),
            "--unset".into(),
            "--".into(),
            crate::auth::MEWGIT_USER.into(),
        ]),
        GitOp::CheckoutPr(plan) => return Ok(checkout_pr(plan)),
    }

    Ok(vec![GitCommand { argv, stdin }])
}

/// 断られた reword を amend してから進める continue（03 の「Interactive rebase の実装」）。
///
/// **1 本目は HEAD の照合で、何も書かない。** `verify` だけのトランザクションは ref も
/// reflog も動かさない（03 の実測）。snapshot のあとで HEAD が動いていれば、止まった
/// コミットのメッセージで別のコミットを amend することになるので、ここで止める。
/// **amend にメッセージを渡さない。** `GIT_EDITOR` の commit editor が `done` の最終行から
/// `mewgit-reword-<X>` を差し込む —— rebase の開始と同じ経路なので、cleanup も
/// `core.commentChar` も同じに効く。
fn continue_refused_reword(reword: &RefusedReword) -> Vec<GitCommand> {
    vec![
        GitCommand {
            stdin: Some(format!("verify HEAD {}\n", reword.head).into_bytes()),
            ..call(&["update-ref", "--stdin"])
        },
        call(&["commit", "--amend"]),
        call(&["rebase", "--continue"]),
    ]
}

/// [`GitOp::CheckoutPr`] の手順（05 の「mewgit の手順」の表）。
///
/// **どれも上書きしない。** `+` を付けるのは remote-tracking ref への refspec だけで、
/// ローカルのブランチは `merge --ff-only` でしか動かさない。**新しく作るのは断る形の
/// コマンドだけ**（`switch --create` と `branch`。在れば git が断る）。
///
/// **ローカルのブランチを fetch の refspec で動かさない。** `<src>:refs/heads/<b>` は
/// **ローカルが先に進んでいるだけでも** non-fast-forward で断り（押し出していない
/// コミットを持つ自分の PR が取り出せない）、しかも一覧が古いと在るブランチを
/// 早送りする（05）。
///
/// **`--no-tags` は gh に揃えた。** PR を取り出すのにタグは要らず、fork から取る
/// ときは他人のタグが混ざる。
fn checkout_pr(plan: &PrCheckout) -> Vec<GitCommand> {
    let PrCheckout {
        remote,
        branch,
        source,
        local,
    } = plan;
    let fetch = |refspec: &str| call(&["fetch", "--no-tags", remote, refspec]);
    let switch = || call(&["switch", branch]);
    let fast_forward = |to: &str| call(&["merge", "--ff-only", to]);

    match source {
        PrSource::SameRepo => {
            let tracking = format!("refs/remotes/{remote}/{branch}");
            let fetch = fetch(&format!("+refs/heads/{branch}:{tracking}"));
            match local {
                // **`--track` は明示する**（`branch.autoSetupMerge = false` の環境では
                // 付けないと上流が入らない。05）。**`--create` の直後に名前を置く**
                // （`BranchCreate` の注記と同じ理由で、`--create` は次の argv を名前と
                // して食う）。
                LocalBranch::Absent => vec![
                    fetch,
                    call(&[
                        "switch",
                        "--track",
                        "--create",
                        branch,
                        &format!("{remote}/{branch}"),
                    ]),
                ],
                // **いつも切り替えてから早送りする**（いま居ても `switch` は何もしない）。
                // 一覧の「いま居る」を信じて `switch` を省くと、外で HEAD が動いていた
                // とき**別のブランチを早送りする**（05）。
                LocalBranch::Present => vec![fetch, switch(), fast_forward(&tracking)],
            }
        }
        PrSource::Fork { number, upstream } => {
            // `FETCH_HEAD` に取る。`switch` と `config` は `FETCH_HEAD` を動かさない。
            let fetch = fetch(&format!("refs/pull/{number}/head"));
            match local {
                // **`branch` で作る**（在れば `already exists` で断る）。一覧が古くて
                // 在るブランチを「無い」と読んでも、そのブランチも追跡設定も触らない。
                // **追跡設定は `switch` より前に書く。** 切替が断られても（ワーク
                // ツリーの変更がぶつかる）ブランチが完成しているように —— 後に書くと、
                // 次の回は `Present` へ入って二度と書かれない（05）。
                LocalBranch::Absent => {
                    let mut steps = vec![fetch, call(&["branch", branch, "FETCH_HEAD"])];
                    for (key, value) in [
                        ("remote", &upstream.remote),
                        ("pushRemote", &upstream.remote),
                        ("merge", &upstream.merge),
                    ] {
                        steps.push(call(&[
                            "config",
                            "--local",
                            "--",
                            &format!("branch.{branch}.{key}"),
                            value,
                        ]));
                    }
                    steps.push(switch());
                    steps
                }
                LocalBranch::Present => vec![fetch, switch(), fast_forward("FETCH_HEAD")],
            }
        }
    }
}

/// stdin を持たない 1 回分の呼び出し。
fn call(argv: &[&str]) -> GitCommand {
    GitCommand {
        argv: argv.iter().map(|arg| arg.to_string()).collect(),
        stdin: None,
    }
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
    /// **[`to_commands`] が返す列のうち、時間を使う 1 本の `argv[0]` と同じものを返す**
    /// （スピナーの語。03 の「複数の手順を持つ書き込み」）。1 本の変種ではその 1 本、
    /// `CheckoutPr` は最初の fetch、断られた reword を持つ continue は最後の
    /// `rebase --continue`（照合と amend は一瞬）。別々に書いてもコンパイルは通るので、
    /// `verb_matches_the_argv` が全変種で突き合わせる。
    /// `to_commands` から取らないのは、語を 1 つ得るためにハンクのパッチまで
    /// 組み直すことになるため（[`to_commands`] は `Result` でもある）。
    ///
    /// 網羅 `match` にしているのは、変種を足したときに選ばせるため。
    pub fn verb(&self) -> &'static str {
        match self {
            GitOp::Stage(_) => "add",
            GitOp::Unstage(_) => "reset",
            GitOp::StageHunk(_) | GitOp::UnstageHunk(_) | GitOp::DiscardHunk(_) => "apply",
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
            | GitOp::RebaseContinue { .. }
            | GitOp::RebaseSkip
            | GitOp::RebaseAbort => "rebase",
            GitOp::CherryPick(_)
            | GitOp::CherryPickContinue
            | GitOp::CherryPickSkip
            | GitOp::CherryPickAbort => "cherry-pick",
            GitOp::Revert { .. }
            | GitOp::RevertContinue
            | GitOp::RevertSkip
            | GitOp::RevertAbort => "revert",
            GitOp::MergeAbort => "merge",
            GitOp::StashPush { .. }
            | GitOp::StashApply(_)
            | GitOp::StashPop(_)
            | GitOp::StashDrop(_) => "stash",
            GitOp::SetAccount { .. } | GitOp::UnsetAccount => "config",
            // **最初の 1 本。** 時間のほとんどは fetch が使う（03）。
            GitOp::CheckoutPr(_) => "fetch",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::git_exec::{HunkApply, RebaseAction, RebaseEntry, RebasePlan};
    use crate::test_util::{
        fake_fork_source, fake_oid, fake_pr_checkout, fake_stamp, FAKE_FORK_URL,
    };

    /// [`variant`] と [`VARIANTS`] を同じ並びから作る。**`variant` は網羅 `match` なので、
    /// `GitOp` に変種を足すと 1 行足すまでコンパイルエラーになり、足せば `VARIANTS` にも
    /// 載る**（変種の本数をどこにも書かない）。そのとき [`cases`] にも足す
    /// （足し忘れは `covers_every_variant` が落とす）。
    macro_rules! variants {
        ($($name:ident: $pat:pat,)*) => {
            /// 変種の名前。
            fn variant(op: &GitOp) -> &'static str {
                match op {
                    $($pat => stringify!($name),)*
                }
            }

            /// 全変種の名前。
            const VARIANTS: &[&str] = &[$(stringify!($name),)*];
        };
    }

    variants! {
        Stage: GitOp::Stage(_),
        Unstage: GitOp::Unstage(_),
        StageHunk: GitOp::StageHunk(_),
        UnstageHunk: GitOp::UnstageHunk(_),
        DiscardHunk: GitOp::DiscardHunk(_),
        Discard: GitOp::Discard(_),
        DiscardUntracked: GitOp::DiscardUntracked(_),
        DiscardAdded: GitOp::DiscardAdded(_),
        Commit: GitOp::Commit { .. },
        BranchCreate: GitOp::BranchCreate { .. },
        Switch: GitOp::Switch(_),
        BranchDelete: GitOp::BranchDelete { .. },
        BranchRename: GitOp::BranchRename { .. },
        Fetch: GitOp::Fetch { .. },
        Pull: GitOp::Pull { .. },
        Push: GitOp::Push { .. },
        RebaseInteractive: GitOp::RebaseInteractive { .. },
        RebaseContinue: GitOp::RebaseContinue { .. },
        RebaseSkip: GitOp::RebaseSkip,
        RebaseAbort: GitOp::RebaseAbort,
        CherryPick: GitOp::CherryPick(_),
        CherryPickContinue: GitOp::CherryPickContinue,
        CherryPickSkip: GitOp::CherryPickSkip,
        CherryPickAbort: GitOp::CherryPickAbort,
        Revert: GitOp::Revert { .. },
        RevertContinue: GitOp::RevertContinue,
        RevertSkip: GitOp::RevertSkip,
        RevertAbort: GitOp::RevertAbort,
        MergeAbort: GitOp::MergeAbort,
        StashPush: GitOp::StashPush { .. },
        StashApply: GitOp::StashApply(_),
        StashPop: GitOp::StashPop(_),
        StashDrop: GitOp::StashDrop(_),
        SetAccount: GitOp::SetAccount { .. },
        UnsetAccount: GitOp::UnsetAccount,
        CheckoutPr: GitOp::CheckoutPr(_),
    }

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
                GitOp::StageHunk(Box::new(HunkApply {
                    path: "a.txt".into(),
                    patch: b"@@ -1 +1 @@\n".to_vec(),
                    expect: fake_stamp(),
                })),
                vec!["apply", "--cached", "--whitespace=nowarn", "-"],
            ),
            (
                GitOp::UnstageHunk(Box::new(HunkApply {
                    path: "a.txt".into(),
                    patch: b"@@ -1 +1 @@\n".to_vec(),
                    expect: fake_stamp(),
                })),
                vec!["apply", "--cached", "--reverse", "--whitespace=nowarn", "-"],
            ),
            (
                GitOp::DiscardHunk(Box::new(HunkApply {
                    path: "a.txt".into(),
                    patch: b"@@ -1 +1 @@\n".to_vec(),
                    expect: fake_stamp(),
                })),
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
                    head: fake_oid(2),
                    plan: RebasePlan {
                        entries: vec![RebaseEntry {
                            action: RebaseAction::Pick,
                            oid: fake_oid(1),
                            summary: "c1".into(),
                        }],
                    },
                },
                vec!["rebase", "--interactive", "--no-update-refs", "main"],
            ),
            (
                GitOp::RebaseContinue { reword: None },
                vec!["rebase", "--continue"],
            ),
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
            (GitOp::CherryPickSkip, vec!["cherry-pick", "--skip"]),
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
            (GitOp::RevertSkip, vec!["revert", "--skip"]),
            (GitOp::RevertAbort, vec!["revert", "--abort"]),
            (GitOp::MergeAbort, vec!["merge", "--abort"]),
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
            (
                GitOp::UnsetAccount,
                vec!["config", "--local", "--unset", "--", "mewgit.user"],
            ),
        ]
    }

    /// 1 本だけを返す変種の呼び出し。**2 本以上を返したら落とす** —— 表（[`cases`]）の
    /// 1 行が 1 本の argv を写しているので、列を黙って先頭だけ見ると残りが見張られない。
    fn to_command(op: &GitOp) -> Result<GitCommand, ArgvError> {
        to_commands(op).map(|mut commands| {
            assert_eq!(commands.len(), 1, "{op:?}");
            commands.remove(0)
        })
    }

    /// 設計書 05 の「mewgit の手順」の表をそのまま写したもの。
    fn checkout_cases() -> Vec<(GitOp, Vec<Vec<&'static str>>)> {
        let same_fetch = vec![
            "fetch",
            "--no-tags",
            "origin",
            "+refs/heads/feat:refs/remotes/origin/feat",
        ];
        let fork_fetch = vec!["fetch", "--no-tags", "origin", "refs/pull/7/head"];
        vec![
            (
                fake_pr_checkout(PrSource::SameRepo, LocalBranch::Absent),
                vec![
                    same_fetch.clone(),
                    vec!["switch", "--track", "--create", "feat", "origin/feat"],
                ],
            ),
            (
                fake_pr_checkout(PrSource::SameRepo, LocalBranch::Present),
                vec![
                    same_fetch,
                    vec!["switch", "feat"],
                    vec!["merge", "--ff-only", "refs/remotes/origin/feat"],
                ],
            ),
            (
                fake_pr_checkout(fake_fork_source(), LocalBranch::Absent),
                vec![
                    fork_fetch.clone(),
                    vec!["branch", "feat", "FETCH_HEAD"],
                    vec![
                        "config",
                        "--local",
                        "--",
                        "branch.feat.remote",
                        FAKE_FORK_URL,
                    ],
                    vec![
                        "config",
                        "--local",
                        "--",
                        "branch.feat.pushRemote",
                        FAKE_FORK_URL,
                    ],
                    vec![
                        "config",
                        "--local",
                        "--",
                        "branch.feat.merge",
                        "refs/heads/feat",
                    ],
                    vec!["switch", "feat"],
                ],
            ),
            (
                fake_pr_checkout(fake_fork_source(), LocalBranch::Present),
                vec![
                    fork_fetch,
                    vec!["switch", "feat"],
                    vec!["merge", "--ff-only", "FETCH_HEAD"],
                ],
            ),
        ]
    }

    /// 全変種の表。1 本の変種は [`cases`] の 1 行を 1 本の列にしたもの。
    fn all_cases() -> Vec<(GitOp, Vec<Vec<&'static str>>)> {
        cases()
            .into_iter()
            .map(|(op, argv)| (op, vec![argv]))
            .chain(checkout_cases())
            .chain([(
                GitOp::RebaseContinue {
                    reword: Some(refused_reword()),
                },
                vec![
                    vec!["update-ref", "--stdin"],
                    vec!["commit", "--amend"],
                    vec!["rebase", "--continue"],
                ],
            )])
            .collect()
    }

    fn refused_reword() -> RefusedReword {
        RefusedReword { head: fake_oid(7) }
    }

    #[test]
    fn a_refused_reword_is_checked_against_head_before_it_is_amended() {
        // 照合が先、amend、continue の順。stdin は照合の 1 本だけに載る —— amend の
        // メッセージは commit editor が差し込む（03 の「Interactive rebase の実装」）。
        let commands = to_commands(&GitOp::RebaseContinue {
            reword: Some(refused_reword()),
        })
        .expect("argv を組めませんでした");
        let stdin: Vec<Option<Vec<u8>>> = commands.into_iter().map(|c| c.stdin).collect();
        assert_eq!(
            stdin,
            [
                Some(format!("verify HEAD {}\n", fake_oid(7)).into_bytes()),
                None,
                None,
            ]
        );
    }

    #[test]
    fn argv_matches_the_design_table() {
        for (op, expected) in all_cases() {
            let commands = to_commands(&op).expect("argv を組めませんでした");
            let argv: Vec<Vec<&str>> = commands
                .iter()
                .map(|command| command.argv.iter().map(String::as_str).collect())
                .collect();
            assert_eq!(argv, expected, "{op:?}");
        }
    }

    #[test]
    fn verb_matches_the_argv() {
        // スピナーの説明と実際に走る git がずれないこと。`all_cases` は全変種を
        // 覆っている（`covers_every_variant`）ので、ここも全変種を回る。複数の手順は、
        // 時間を使う 1 本の語（03 の「複数の手順を持つ書き込み」）—— どの 1 本かは
        // ここにも書く（列のどれかと一致、では語が別の手順へずれても通る）。
        for (op, _) in all_cases() {
            let commands = to_commands(&op).expect("argv を組めませんでした");
            let spinning = match op {
                GitOp::CheckoutPr(_) => commands.first(),
                GitOp::RebaseContinue { reword: Some(_) } => commands.last(),
                _ => {
                    assert_eq!(commands.len(), 1, "{op:?}");
                    commands.first()
                }
            };
            assert_eq!(op.verb(), spinning.unwrap().argv[0], "{op:?}");
        }
    }

    #[test]
    fn covers_every_variant() {
        let covered: BTreeSet<_> = all_cases().iter().map(|(op, _)| variant(op)).collect();
        let every: BTreeSet<_> = VARIANTS.iter().copied().collect();
        // 名前が重なると集合が縮み、覆っていない変種を隠す。
        assert_eq!(every.len(), VARIANTS.len(), "名前が重なっている");
        let missing: Vec<_> = every.difference(&covered).collect();
        assert!(missing.is_empty(), "argv の表に無い変種がある: {missing:?}");
    }

    #[test]
    fn only_hunk_ops_the_rebase_plan_and_the_head_check_use_stdin() {
        for (op, _) in all_cases() {
            let commands = to_commands(&op).expect("argv を組めませんでした");
            for command in commands {
                // 断られた reword を持つ continue は、照合（`update-ref --stdin`）の 1 本だけ。
                let expects_stdin = matches!(
                    op,
                    GitOp::StageHunk(_)
                        | GitOp::UnstageHunk(_)
                        | GitOp::DiscardHunk(_)
                        | GitOp::RebaseInteractive { .. }
                ) || command.argv[0] == "update-ref";
                assert_eq!(command.stdin.is_some(), expects_stdin, "{op:?}");
            }
        }
    }

    #[test]
    fn hunk_patch_goes_to_stdin_unchanged() {
        // 非 UTF-8 のバイト列が lossy にならないこと。ここが崩れると、ステージ
        // した結果が元のファイルと変わる。
        let patch = b"@@ -1 +1 @@\n-\xff\xfe\n+\xff\xfd\n".to_vec();
        let command = to_command(&GitOp::StageHunk(Box::new(HunkApply {
            path: "a.bin".into(),
            patch: patch.clone(),
            expect: fake_stamp(),
        })))
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
    fn an_invalid_rebase_plan_is_refused_before_git_runs() {
        // 空の todo は git が断るが、squash が先頭の todo は git が rebase を始めてから
        // 止まる。どちらも argv を組む側で止める。
        let err = to_command(&GitOp::RebaseInteractive {
            onto: "main".into(),
            head: fake_oid(1),
            plan: RebasePlan::default(),
        })
        .expect_err("空の plan は通ってはいけません");

        assert_eq!(err, ArgvError::InvalidRebasePlan(RebasePlanError::Empty));
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
