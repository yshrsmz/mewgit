//! interactive rebase の todo。設計は `docs/design/03-git-exec.md` の
//! 「Interactive rebase の実装」。
//!
//! `to_commands` が [`RebasePlan`] を JSON にして git の標準入力に流し、`Runner` が
//! 自バイナリを `GIT_SEQUENCE_EDITOR` に置く。git に起こされた隠しサブコマンドが
//! 標準入力を継いで [`write_todo`] を呼び、todo と reword のメッセージを
//! `rebase-merge/` に書く。

use std::ffi::{OsStr, OsString};
use std::io::Read;
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

/// sequence editor として起こされたときの失敗。git が
/// `there was a problem with the editor` と並べて stderr に出す。
#[derive(Debug, thiserror::Error)]
pub enum SequenceEditorError {
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

    /// todo ファイルの中身。`dir` は todo を置くディレクトリ（`rebase-merge/`）で、
    /// reword の `exec` の行が [`reword_path`] の置き場として引く。
    ///
    /// reword は `pick` と `exec git commit --amend` の 2 行にする。**`GIT_EDITOR` で
    /// 渡さない** —— 止まった rebase を `m` から continue すると `GIT_EDITOR=true` で
    /// 走るので、停止点より後ろの reword が元のメッセージのまま通る（03）。
    ///
    /// **バイト列で返す。** `dir` は UTF-8 とは限らず、git は todo をバイト列として読む。
    pub fn to_todo(&self, dir: &Path) -> Vec<u8> {
        let mut todo = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            let line = |keyword: &str| {
                format!("{keyword} {} {}\n", entry.oid, single_line(&entry.summary))
            };
            match &entry.action {
                RebaseAction::Reword { .. } => {
                    todo.extend(line("pick").into_bytes());
                    todo.extend(b"exec git commit --amend --only --allow-empty -F ");
                    todo.extend(sh_quote(reword_path(dir, index).as_os_str()).as_bytes());
                    todo.push(b'\n');
                }
                action => todo.extend(line(action.keyword()).into_bytes()),
            }
        }
        todo
    }
}

impl RebaseAction {
    /// todo の語。`Reword` は [`RebasePlan::to_todo`] が `pick` と `exec` に書き換える
    /// ので、todo には現れない。
    fn keyword(&self) -> &'static str {
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

/// `index` 番目の reword のメッセージを置くファイル。todo と同じ `rebase-merge/` に
/// 置くので、完了でも abort でも git がディレクトリごと消す。
fn reword_path(dir: &Path, index: usize) -> PathBuf {
    dir.join(format!("mewgit-reword-{index}"))
}

/// sequence editor の本体。`plan` は git から継いだ標準入力（`to_commands` が流した
/// JSON）、`todo` は git が渡した todo ファイル（`rebase-merge/git-rebase-todo`）。
///
/// **メッセージを先に書き、todo を最後に書く。** 途中で落ちたら非 0 で返り、git は
/// rebase を始めずに `rebase-merge/` ごと消す（03 の実測）。
pub fn write_todo(mut plan: impl Read, todo: &Path) -> Result<(), SequenceEditorError> {
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| SequenceEditorError::Io { path, source }
    };
    let mut json = String::new();
    plan.read_to_string(&mut json)?;
    let plan: RebasePlan = serde_json::from_str(&json)?;
    plan.validate()?;

    let dir = todo.parent().unwrap_or(Path::new("."));
    for (index, entry) in plan.entries.iter().enumerate() {
        if let RebaseAction::Reword { message } = &entry.action {
            let path = reword_path(dir, index);
            std::fs::write(&path, message).map_err(io(&path))?;
        }
    }
    std::fs::write(todo, plan.to_todo(dir)).map_err(io(todo))
}

/// POSIX シェルの 1 語として引用する。git は todo の `exec` も `GIT_SEQUENCE_EDITOR` も
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

    fn todo_lines(plan: &RebasePlan, dir: &str) -> Vec<String> {
        String::from_utf8(plan.to_todo(Path::new(dir)))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn reword_becomes_a_pick_and_an_exec_that_amends_from_the_file() {
        let plan = RebasePlan {
            entries: vec![
                entry(RebaseAction::Pick, 1),
                entry(reword("new"), 2),
                entry(RebaseAction::Squash, 3),
                entry(RebaseAction::Drop, 4),
            ],
        };

        assert_eq!(
            todo_lines(&plan, "/r x/.git/rebase-merge"),
            [
                format!("pick {} c1", fake_oid(1)),
                format!("pick {} c2", fake_oid(2)),
                "exec git commit --amend --only --allow-empty -F '/r x/.git/rebase-merge/mewgit-reword-1'".to_string(),
                format!("squash {} c3", fake_oid(3)),
                format!("drop {} c4", fake_oid(4)),
            ]
        );
    }

    #[test]
    fn a_summary_with_a_newline_stays_on_its_line() {
        let mut e = entry(RebaseAction::Pick, 1);
        e.summary = "a\nexec rm -rf x".into();

        assert_eq!(todo_lines(&RebasePlan { entries: vec![e] }, "/d").len(), 1);
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
        let todo = rebase_dir.join("git-rebase-todo");
        std::fs::write(&todo, "pick 1234 original\n").unwrap();
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

        write_todo(serde_json::to_vec(&plan).unwrap().as_slice(), &todo).unwrap();

        assert_eq!(std::fs::read(&todo).unwrap(), plan.to_todo(&rebase_dir));
        assert_eq!(
            std::fs::read_to_string(rebase_dir.join("mewgit-reword-1")).unwrap(),
            "new\n\nbody"
        );
    }

    #[test]
    fn write_todo_leaves_the_todo_alone_when_the_plan_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let todo = dir.path().join("git-rebase-todo");
        std::fs::write(&todo, "pick 1234 original\n").unwrap();

        let err = write_todo(b"{\"entries\":[]}".as_slice(), &todo).unwrap_err();

        assert!(matches!(
            err,
            SequenceEditorError::Invalid(RebasePlanError::Empty)
        ));
        assert_eq!(
            std::fs::read_to_string(&todo).unwrap(),
            "pick 1234 original\n"
        );
    }
}
