//! interactive rebase の todo。設計は `docs/design/03-git-exec.md` の
//! 「Interactive rebase の実装」。
//!
//! `to_commands` が [`RebasePlan`] を JSON にして git の標準入力に流し、`Runner` が
//! 自バイナリを `GIT_SEQUENCE_EDITOR` と `GIT_EDITOR` に置く。git に起こされた隠し
//! サブコマンドのうち、`--sequence-editor` は標準入力を継いで [`write_todo`] を呼び、
//! todo と reword のメッセージを `rebase-merge/` に書く。`--commit-editor` は
//! [`write_commit_message`] を呼び、いま reword しているコミットのメッセージを
//! 差し込む。

use std::ffi::{OsStr, OsString};
use std::io::{ErrorKind, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::git_read::Oid;

/// interactive rebase の todo。[`write_todo`] が todo ファイルの内容にする。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RebasePlan {
    pub entries: Vec<RebaseEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RebaseEntry {
    #[serde(flatten)]
    pub action: RebaseAction,
    #[serde(with = "oid_hex")]
    pub oid: Oid,
    /// 件名。todo の行末に添えるだけで、git は読まない（止まったときの stderr に出る）。
    pub summary: String,
}

/// todo の 1 行の動作。
///
/// **メッセージを持つのは `Reword` だけ。** `RebaseEntry` に `Option` で持たせると、
/// 値の無い reword も値のある pick も組めてしまう。squash は git が 2 本をつないだ
/// メッセージをそのまま使う（03）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum RebaseAction {
    Pick,
    Reword { message: String },
    Edit,
    Squash,
    Fixup,
    Drop,
}

/// git に渡す前に断る plan。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RebasePlanError {
    /// git は空の todo を `nothing to do` で断る。
    #[error("rebase する行がありません")]
    Empty,
    /// 手前に残るコミットが無い squash / fixup。git は `cannot 'squash' without a
    /// previous commit` で断る。
    #[error("{} を最初のコミットにはできません: {oid}", .action)]
    NothingToMeld { action: &'static str, oid: Oid },
    /// 空のメッセージは `commit -F` が断り、rebase が途中で止まる。
    #[error("reword のメッセージが空です: {oid}")]
    EmptyMessage { oid: Oid },
}

/// エディタとして起こされた mewgit が、失敗を stderr に書くときの行頭。
/// `runner::classify` が印を行頭で照合するので、書く側と読む側でこれを共有する。
pub const EDITOR_STDERR_PREFIX: &str = "mewgit: ";

/// [`EditorError::HeadMoved`] の `Display` の先頭に置く印。git が sequence editor の stderr を
/// 自分の stderr に並べるので、`runner::classify` がこれで
/// [`ExecErrorKind::HeadMoved`](super::ExecErrorKind::HeadMoved) に分ける。
///
/// **日本語の文言を鍵にしない**（直すたびに分類が外れる。03 の「エラー分類」）。
pub(crate) const HEAD_MOVED_MARKER: &str = "[head-moved]";

/// エディタとして起こされたときの失敗。git が `there was a problem with the editor` と
/// 並べて stderr に出す。
#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    #[error("{}: {source}", .path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("plan を標準入力から読めません: {0}")]
    Read(#[from] std::io::Error),
    #[error("plan を読めません: {0}")]
    Parse(#[from] serde_json::Error),
    /// **手で叩かれたときの守り。** `Runner` 経由なら `to_commands` が先に断っている。
    #[error(transparent)]
    Invalid(#[from] RebasePlanError),
    /// plan を組んだあとに HEAD が動いた（[`write_todo`]）。端末で積んだコミットなど。
    #[error(
        "{HEAD_MOVED_MARKER} rebase を組んだあとに HEAD が動きました（{expected} → {actual}）。読み直してからやり直して"
    )]
    HeadMoved { expected: Oid, actual: String },
}

impl RebasePlan {
    /// git に渡せる形か。**全部 `drop` は断らない**（git は通して、ブランチが
    /// `onto` まで戻る。確認を挟むのは UI）。
    pub fn validate(&self) -> Result<(), RebasePlanError> {
        if self.entries.is_empty() {
            return Err(RebasePlanError::Empty);
        }
        let mut kept = false;
        for entry in &self.entries {
            match &entry.action {
                RebaseAction::Drop => {}
                RebaseAction::Squash | RebaseAction::Fixup if !kept => {
                    return Err(RebasePlanError::NothingToMeld {
                        action: entry.action.keyword(),
                        oid: entry.oid,
                    });
                }
                RebaseAction::Reword { message } if message.trim().is_empty() => {
                    return Err(RebasePlanError::EmptyMessage { oid: entry.oid });
                }
                _ => kept = true,
            }
        }
        Ok(())
    }

    /// todo ファイルの中身。reword は git 自身の `reword` のまま書き、メッセージは
    /// git が起こす `GIT_EDITOR`（[`write_commit_message`]）が差し込む。
    ///
    /// **`pick` + `exec git commit --amend` の 2 行にしない。** `exec` は直前の pick が
    /// どうなったかを見ずに HEAD を amend するので、止まった pick を skip すると
    /// **別のコミット**（1 つ前の pick、先頭なら `onto`）のメッセージを書き換える。
    /// `general.git_bin` も効かない（`/code-review` が拾った。03）。
    pub fn to_todo(&self) -> String {
        self.entries
            .iter()
            .map(|entry| {
                let keyword = entry.action.keyword();
                format!("{keyword} {} {}\n", entry.oid, single_line(&entry.summary))
            })
            .collect()
    }
}

impl RebaseAction {
    /// todo の語。RebaseEditor の行もこの字で出す（07）。
    pub fn keyword(&self) -> &'static str {
        match self {
            RebaseAction::Pick => "pick",
            RebaseAction::Reword { .. } => "reword",
            RebaseAction::Edit => "edit",
            RebaseAction::Squash => "squash",
            RebaseAction::Fixup => "fixup",
            RebaseAction::Drop => "drop",
        }
    }
}

/// sequence editor が標準入力から読むもの。`to_commands` が
/// [`GitOp::RebaseInteractive`](super::GitOp::RebaseInteractive) から組む。
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SequenceInput {
    /// plan を組んだ時点の HEAD。[`write_todo`] が `orig-head` と比べる。
    #[serde(with = "oid_hex")]
    pub head: Oid,
    pub plan: RebasePlan,
}

/// `git2::Oid` は serde を実装しないので、16 進の文字列で運ぶ。
mod oid_hex {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::git_read::Oid;

    pub fn serialize<S: Serializer>(oid: &Oid, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(oid)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Oid, D::Error> {
        let hex = String::deserialize(deserializer)?;
        hex.parse().map_err(serde::de::Error::custom)
    }
}

/// `oid` を reword したメッセージを置くファイル。`dir` は `rebase-merge/`。完了でも
/// abort でも git がディレクトリごと消すので、後始末は要らない。
///
/// **Oid で引く。** commit editor が知っているのは `done` の最終行（`reword <Oid> …`）
/// だけなので、何番目の行かでは引けない。
fn reword_path(dir: &Path, oid: &str) -> PathBuf {
    dir.join(format!("mewgit-reword-{oid}"))
}

/// sequence editor の本体。`plan` は git から継いだ標準入力（`to_commands` が流した
/// JSON）、`todo` は git が渡した todo ファイル（`rebase-merge/git-rebase-todo`）。
///
/// **plan を組んだ HEAD が git の `rebase-merge/orig-head` と違えば断る**
/// （[`EditorError::HeadMoved`]）。plan は UI が読み込み済みのグラフから組むので、
/// 端末で積んだコミットがあると範囲が古く、**載らなかったコミットを git は黙って
/// 落とす**（03）。`onto` は Oid で固定され、範囲の merge は UI が開く前に断るので、
/// HEAD が同じなら範囲も同じ。
///
/// **メッセージを先に書き、todo を最後に書く。** 途中で落ちたら非 0 で返り、git は
/// rebase を始めずに `rebase-merge/` ごと消す（03 の実測）。
pub fn write_todo(mut input: impl Read, todo: &Path) -> Result<(), EditorError> {
    let mut json = String::new();
    input.read_to_string(&mut json)?;
    let SequenceInput { head, plan } = serde_json::from_str(&json)?;
    plan.validate()?;

    let dir = todo.parent().unwrap_or(Path::new("."));
    // sequence editor が起きた時点で git が書いている（ブランチ・detached・linked
    // worktree のどれでも。03 の実測）。
    let orig_head = dir.join("orig-head");
    let actual = std::fs::read_to_string(&orig_head).map_err(io(&orig_head))?;
    let actual = actual.trim();
    if actual != head.to_string() {
        return Err(EditorError::HeadMoved {
            expected: head,
            actual: actual.to_string(),
        });
    }

    for entry in &plan.entries {
        if let RebaseAction::Reword { message } = &entry.action {
            let path = reword_path(dir, &entry.oid.to_string());
            std::fs::write(&path, message).map_err(io(&path))?;
        }
    }
    std::fs::write(todo, plan.to_todo()).map_err(io(todo))
}

/// commit editor の本体。`file` は git が渡したメッセージのファイル
/// （`<gitdir>/COMMIT_EDITMSG`）。
///
/// **いま reword しているコミットのメッセージがあるときだけ書き換える。** 判定は
/// `<gitdir>/rebase-merge/done` の最終行が `reword <Oid> …` で、[`write_todo`] が
/// その Oid のメッセージを置いていること。それ以外（squash、revert の continue、
/// rebase の外）は触らずに 0 で返す —— 以前の `GIT_EDITOR=true` と同じ振る舞い。
///
/// `GIT_EDITOR` は `Runner` がどのコマンドにも載せるので、**止まった reword を `m` から
/// continue したときもここを通る**（03 の実測）。
pub fn write_commit_message(file: &Path) -> Result<(), EditorError> {
    let Some(rebase_dir) = file.parent().map(|gitdir| gitdir.join("rebase-merge")) else {
        return Ok(());
    };
    let done = match std::fs::read_to_string(rebase_dir.join("done")) {
        Ok(done) => done,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io(&rebase_dir.join("done"))(source)),
    };
    let mut last = done
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .split_whitespace();
    let (Some("reword"), Some(oid)) = (last.next(), last.next()) else {
        return Ok(());
    };
    let path = reword_path(&rebase_dir, oid);
    match std::fs::read(&path) {
        Ok(message) => std::fs::write(file, message).map_err(io(file)),
        // 端末で始めた rebase の reword など、mewgit がメッセージを置いていない。
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io(&path)(source)),
    }
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> EditorError {
    let path = path.to_path_buf();
    move |source| EditorError::Io { path, source }
}

/// POSIX シェルの 1 語として引用する。git は `GIT_SEQUENCE_EDITOR` も `GIT_EDITOR` も
/// `sh -c` で起こすので、空白や `'` を含むパスを 1 語で渡すにはこれが要る。
///
/// **バイト列のまま引用する。** 特別扱いするのは `'` だけなので、UTF-8 でないパスも
/// 断らずに渡せる。
pub(crate) fn sh_quote(word: &OsStr) -> OsString {
    let mut quoted = vec![b'\''];
    for &byte in word.as_bytes() {
        if byte == b'\'' {
            quoted.extend(br"'\''");
        } else {
            quoted.push(byte);
        }
    }
    quoted.push(b'\'');
    OsString::from_vec(quoted)
}

/// 件名を todo の 1 行に収める。改行が混ざると git が次の行として読む。
fn single_line(summary: &str) -> String {
    summary
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{fake_oid, rebase_entry, reword};

    fn entry(action: RebaseAction, n: u8) -> RebaseEntry {
        rebase_entry(action, fake_oid(n), &format!("c{n}"))
    }

    /// git が起こす sequence editor の周りを模す。`dir` は `rebase-merge/` で、git は
    /// そこに `orig-head` を書いてから editor を起こす（03 の実測）。HEAD は `fake_oid(0)`。
    fn with_orig_head(dir: &Path) -> PathBuf {
        std::fs::write(dir.join("orig-head"), format!("{}\n", fake_oid(0))).unwrap();
        dir.join("git-rebase-todo")
    }

    /// `to_commands` が流す標準入力。`head` は plan を組んだ時点の HEAD。
    fn input(head: Oid, plan: &RebasePlan) -> Vec<u8> {
        serde_json::to_vec(&SequenceInput {
            head,
            plan: plan.clone(),
        })
        .unwrap()
    }

    fn todo_lines(plan: &RebasePlan) -> Vec<String> {
        plan.to_todo().lines().map(str::to_string).collect()
    }

    #[test]
    fn every_action_is_written_as_gits_own_keyword() {
        let plan = RebasePlan {
            entries: vec![
                entry(RebaseAction::Pick, 1),
                entry(reword("new"), 2),
                entry(RebaseAction::Squash, 3),
                entry(RebaseAction::Drop, 4),
            ],
        };

        assert_eq!(
            todo_lines(&plan),
            [
                format!("pick {} c1", fake_oid(1)),
                format!("reword {} c2", fake_oid(2)),
                format!("squash {} c3", fake_oid(3)),
                format!("drop {} c4", fake_oid(4)),
            ]
        );
    }

    #[test]
    fn a_summary_with_a_newline_stays_on_its_line() {
        let mut e = entry(RebaseAction::Pick, 1);
        e.summary = "a\nexec rm -rf x".into();

        assert_eq!(todo_lines(&RebasePlan { entries: vec![e] }).len(), 1);
    }

    #[test]
    fn quoting_survives_a_single_quote_and_non_utf8() {
        assert_eq!(sh_quote(OsStr::new("/a b/it's")), r"'/a b/it'\''s'");
        assert_eq!(sh_quote(OsStr::from_bytes(b"/\xff")).as_bytes(), b"'/\xff'");
    }

    #[test]
    fn validate_refuses_what_git_would_stop_on() {
        let plan = |entries| RebasePlan { entries };

        assert_eq!(plan(vec![]).validate(), Err(RebasePlanError::Empty));
        // drop の後ろでも、手前に残るコミットが無ければ同じ。
        assert_eq!(
            plan(vec![
                entry(RebaseAction::Drop, 1),
                entry(RebaseAction::Fixup, 2)
            ])
            .validate(),
            Err(RebasePlanError::NothingToMeld {
                action: "fixup",
                oid: fake_oid(2)
            })
        );
        assert_eq!(
            plan(vec![entry(reword(" \n"), 1)]).validate(),
            Err(RebasePlanError::EmptyMessage { oid: fake_oid(1) })
        );
        // 全部 drop は通す（03）。
        assert_eq!(plan(vec![entry(RebaseAction::Drop, 1)]).validate(), Ok(()));
        assert_eq!(
            plan(vec![
                entry(RebaseAction::Edit, 1),
                entry(RebaseAction::Squash, 2)
            ])
            .validate(),
            Ok(())
        );
    }

    #[test]
    fn write_todo_puts_messages_next_to_the_todo() {
        let dir = tempfile::tempdir().unwrap();
        let rebase_dir = dir.path().join("rebase merge");
        std::fs::create_dir(&rebase_dir).unwrap();
        let todo = with_orig_head(&rebase_dir);
        // 全部の動作を JSON に通す。
        let plan = RebasePlan {
            entries: vec![
                entry(RebaseAction::Pick, 1),
                entry(reword("new\n\nbody"), 2),
                entry(RebaseAction::Edit, 3),
                entry(RebaseAction::Squash, 4),
                entry(RebaseAction::Fixup, 5),
                entry(RebaseAction::Drop, 6),
            ],
        };

        write_todo(input(fake_oid(0), &plan).as_slice(), &todo).unwrap();

        assert_eq!(std::fs::read_to_string(&todo).unwrap(), plan.to_todo());
        assert_eq!(
            std::fs::read_to_string(rebase_dir.join(format!("mewgit-reword-{}", fake_oid(2))))
                .unwrap(),
            "new\n\nbody"
        );
    }

    #[test]
    fn the_commit_editor_fills_in_only_the_reword_being_committed() {
        // gitdir を模す。git は `<gitdir>/COMMIT_EDITMSG` を渡し、`rebase-merge/` は
        // その隣にある（linked worktree でも。03 の実測）。
        let gitdir = tempfile::tempdir().unwrap();
        let rebase_dir = gitdir.path().join("rebase-merge");
        std::fs::create_dir(&rebase_dir).unwrap();
        let todo = with_orig_head(&rebase_dir);
        let plan = RebasePlan {
            entries: vec![entry(RebaseAction::Pick, 1), entry(reword("new"), 2)],
        };
        write_todo(input(fake_oid(0), &plan).as_slice(), &todo).unwrap();
        let msg = gitdir.path().join("COMMIT_EDITMSG");
        let commit_with_done = |done: &str| {
            std::fs::write(rebase_dir.join("done"), done).unwrap();
            std::fs::write(&msg, "original\n").unwrap();
            write_commit_message(&msg).unwrap();
            std::fs::read_to_string(&msg).unwrap()
        };

        // `done` の最終行が reword の行なら差し込む。
        assert_eq!(
            commit_with_done(&format!(
                "pick {} c1\nreword {} c2\n",
                fake_oid(1),
                fake_oid(2)
            )),
            "new"
        );
        // squash のメッセージは git が連結したまま（03）。
        assert_eq!(
            commit_with_done(&format!(
                "reword {} c2\nsquash {} c3\n",
                fake_oid(2),
                fake_oid(3)
            )),
            "original\n"
        );
        // 端末で始めた rebase の reword。mewgit のメッセージが無い。
        assert_eq!(
            commit_with_done(&format!("reword {} c3\n", fake_oid(3))),
            "original\n"
        );
        // rebase の外（revert の continue など）。
        std::fs::remove_dir_all(&rebase_dir).unwrap();
        std::fs::write(&msg, "original\n").unwrap();
        write_commit_message(&msg).unwrap();
        assert_eq!(std::fs::read_to_string(&msg).unwrap(), "original\n");
    }

    #[test]
    fn write_todo_leaves_the_todo_alone_when_the_plan_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let todo = with_orig_head(dir.path());
        std::fs::write(&todo, "pick 1234 original\n").unwrap();

        let err =
            write_todo(input(fake_oid(0), &RebasePlan::default()).as_slice(), &todo).unwrap_err();

        assert!(matches!(err, EditorError::Invalid(RebasePlanError::Empty)));
        assert_eq!(
            std::fs::read_to_string(&todo).unwrap(),
            "pick 1234 original\n"
        );
    }

    #[test]
    fn write_todo_leaves_the_todo_alone_when_head_moved_after_the_plan() {
        // 画面を開いたあとに端末で 1 本積んだ。**通すと、plan に載らなかったそのコミットを
        // git が黙って落とす**（`rebase.missingCommitsCheck` の既定は `ignore`。03）。
        let dir = tempfile::tempdir().unwrap();
        let todo = with_orig_head(dir.path());
        std::fs::write(&todo, "pick 1234 original\n").unwrap();
        let plan = RebasePlan {
            entries: vec![entry(RebaseAction::Pick, 1)],
        };

        let err = write_todo(input(fake_oid(7), &plan).as_slice(), &todo).unwrap_err();

        assert!(
            matches!(
                &err,
                EditorError::HeadMoved { expected, actual }
                    if *expected == fake_oid(7) && *actual == fake_oid(0).to_string()
            ),
            "{err:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&todo).unwrap(),
            "pick 1234 original\n"
        );
    }
}
