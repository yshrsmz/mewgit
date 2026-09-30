//! interactive rebase の todo。設計は `docs/design/03-git-exec.md` の
//! 「Interactive rebase の実装」。
//!
//! `Runner` が [`RebasePlan`] を JSON で一時ファイルに書き、自バイナリを
//! `GIT_SEQUENCE_EDITOR` に置く。git に起こされた隠しサブコマンドが
//! [`write_todo`] を呼び、todo と reword のメッセージを `rebase-merge/` に書く。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::git_read::Oid;

/// interactive rebase の todo。[`write_todo`] が todo ファイルの内容にする。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebasePlan {
    pub entries: Vec<RebaseEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebaseEntry {
    pub action: RebaseAction,
    pub oid: Oid,
    /// 件名。todo の行末に添えるだけで、git は読まない（止まったときの stderr に出る）。
    pub summary: String,
}

/// todo の 1 行の動作。
///
/// **メッセージを持つのは `Reword` だけ。** `RebaseEntry` に `Option` で持たせると、
/// 値の無い reword も値のある pick も組めてしまう。squash は git が 2 本をつないだ
/// メッセージをそのまま使う（03）。
#[derive(Debug, Clone, PartialEq, Eq)]
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
    #[error("plan を読めません: {0}")]
    Parse(String),
    #[error(transparent)]
    Invalid(#[from] RebasePlanError),
    /// todo の置き場が UTF-8 でない。`exec` の行に載せられない。
    #[error("todo の置き場が UTF-8 ではありません: {}", .0.display())]
    NonUtf8Dir(PathBuf),
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
    /// reword の `exec` の行が [`reword_file`] の置き場として引く。
    ///
    /// reword は `pick` と `exec git commit --amend` の 2 行にする。**`GIT_EDITOR` で
    /// 渡さない** —— 止まった rebase を `m` から continue すると `GIT_EDITOR=true` で
    /// 走るので、停止点より後ろの reword が元のメッセージのまま通る（03）。
    pub fn to_todo(&self, dir: &str) -> String {
        let mut todo = String::new();
        for (index, entry) in self.entries.iter().enumerate() {
            let summary = single_line(&entry.summary);
            let keyword = match entry.action {
                RebaseAction::Reword { .. } => "pick",
                ref action => action.keyword(),
            };
            todo.push_str(&format!("{keyword} {} {summary}\n", entry.oid));
            if let RebaseAction::Reword { .. } = entry.action {
                let file = format!("{dir}/{}", reword_file(index));
                todo.push_str(&format!(
                    "exec git commit --amend --only --allow-empty -F {}\n",
                    sh_quote(&file)
                ));
            }
        }
        todo
    }

    /// `Runner` が一時ファイルに書く形。
    pub fn to_json(&self) -> String {
        let wire: Vec<WireEntry> = self
            .entries
            .iter()
            .map(|entry| WireEntry {
                action: entry.action.keyword().to_string(),
                oid: entry.oid.to_string(),
                summary: entry.summary.clone(),
                message: match &entry.action {
                    RebaseAction::Reword { message } => Some(message.clone()),
                    _ => None,
                },
            })
            .collect();
        serde_json::to_string(&wire).expect("文字列だけの値は必ず直列化できる")
    }

    pub fn from_json(json: &str) -> Result<Self, SequenceEditorError> {
        let wire: Vec<WireEntry> = serde_json::from_str(json)
            .map_err(|err| SequenceEditorError::Parse(err.to_string()))?;
        let entries = wire
            .into_iter()
            .map(|entry| {
                let oid = entry.oid.parse().map_err(|_| {
                    SequenceEditorError::Parse(format!("Oid ではありません: {}", entry.oid))
                })?;
                let action = match (entry.action.as_str(), entry.message) {
                    ("pick", None) => RebaseAction::Pick,
                    ("reword", Some(message)) => RebaseAction::Reword { message },
                    ("edit", None) => RebaseAction::Edit,
                    ("squash", None) => RebaseAction::Squash,
                    ("fixup", None) => RebaseAction::Fixup,
                    ("drop", None) => RebaseAction::Drop,
                    (action, message) => {
                        return Err(SequenceEditorError::Parse(format!(
                            "動作とメッセージの組が合いません: {action}（メッセージ {}）",
                            if message.is_some() {
                                "あり"
                            } else {
                                "なし"
                            }
                        )))
                    }
                };
                Ok(RebaseEntry {
                    action,
                    oid,
                    summary: entry.summary,
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { entries })
    }
}

impl RebaseAction {
    /// todo の語。`Reword` は `reword` を返すが、[`RebasePlan::to_todo`] は `pick` と
    /// `exec` に書き換える。
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

#[derive(Serialize, Deserialize)]
struct WireEntry {
    action: String,
    oid: String,
    summary: String,
    message: Option<String>,
}

/// `index` 番目の reword のメッセージを置くファイル名。todo と同じ
/// `rebase-merge/` に置くので、完了でも abort でも git がディレクトリごと消す。
fn reword_file(index: usize) -> String {
    format!("mewgit-reword-{index}")
}

/// sequence editor の本体。`plan` は `Runner` が書いた JSON、`todo` は git が渡した
/// todo ファイル（`rebase-merge/git-rebase-todo`）。
///
/// **メッセージを先に書き、todo を最後に書く。** 途中で落ちたら非 0 で返り、git は
/// rebase を始めずに `rebase-merge/` ごと消す（03 の実測）。
pub fn write_todo(plan: &Path, todo: &Path) -> Result<(), SequenceEditorError> {
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| SequenceEditorError::Io { path, source }
    };
    let json = std::fs::read_to_string(plan).map_err(io(plan))?;
    let plan = RebasePlan::from_json(&json)?;
    plan.validate()?;

    let dir = todo.parent().unwrap_or(Path::new("."));
    let dir_str = dir
        .to_str()
        .ok_or_else(|| SequenceEditorError::NonUtf8Dir(dir.to_path_buf()))?;
    for (index, entry) in plan.entries.iter().enumerate() {
        if let RebaseAction::Reword { message } = &entry.action {
            let path = dir.join(reword_file(index));
            std::fs::write(&path, message).map_err(io(&path))?;
        }
    }
    std::fs::write(todo, plan.to_todo(dir_str)).map_err(io(todo))
}

/// POSIX シェルの 1 語として引用する。git は todo の `exec` も `GIT_SEQUENCE_EDITOR` も
/// `sh -c` で起こすので、空白や `'` を含むパスを 1 語で渡すにはこれが要る。
pub(crate) fn sh_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
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
    use crate::test_util::fake_oid;

    fn entry(action: RebaseAction, n: u8) -> RebaseEntry {
        RebaseEntry {
            action,
            oid: fake_oid(n),
            summary: format!("c{n}"),
        }
    }

    fn reword(message: &str) -> RebaseAction {
        RebaseAction::Reword {
            message: message.into(),
        }
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

        let todo = plan.to_todo("/r x/.git/rebase-merge");

        assert_eq!(
            todo.lines().collect::<Vec<_>>(),
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
        let todo = RebasePlan { entries: vec![e] }.to_todo("/d");

        assert_eq!(todo.lines().count(), 1, "{todo}");
    }

    #[test]
    fn quoting_survives_a_single_quote() {
        assert_eq!(sh_quote("/a b/it's"), r"'/a b/it'\''s'");
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
    fn json_round_trips_every_action() {
        let plan = RebasePlan {
            entries: vec![
                entry(RebaseAction::Pick, 1),
                entry(reword("line\n\nbody"), 2),
                entry(RebaseAction::Edit, 3),
                entry(RebaseAction::Squash, 4),
                entry(RebaseAction::Fixup, 5),
                entry(RebaseAction::Drop, 6),
            ],
        };

        assert_eq!(RebasePlan::from_json(&plan.to_json()).unwrap(), plan);
    }

    #[test]
    fn json_with_a_message_on_a_pick_is_refused() {
        let json = format!(
            r#"[{{"action":"pick","oid":"{}","summary":"s","message":"m"}}]"#,
            fake_oid(1)
        );

        assert!(matches!(
            RebasePlan::from_json(&json),
            Err(SequenceEditorError::Parse(_))
        ));
    }

    #[test]
    fn write_todo_puts_messages_next_to_the_todo() {
        let dir = tempfile::tempdir().unwrap();
        let plan_path = dir.path().join("plan.json");
        let rebase_dir = dir.path().join("rebase merge");
        std::fs::create_dir(&rebase_dir).unwrap();
        let todo = rebase_dir.join("git-rebase-todo");
        std::fs::write(&todo, "pick 1234 original\n").unwrap();
        let plan = RebasePlan {
            entries: vec![
                entry(RebaseAction::Pick, 1),
                entry(reword("new\n\nbody"), 2),
            ],
        };
        std::fs::write(&plan_path, plan.to_json()).unwrap();

        write_todo(&plan_path, &todo).unwrap();

        assert_eq!(
            std::fs::read_to_string(&todo).unwrap(),
            plan.to_todo(rebase_dir.to_str().unwrap())
        );
        assert_eq!(
            std::fs::read_to_string(rebase_dir.join("mewgit-reword-1")).unwrap(),
            "new\n\nbody"
        );
    }

    #[test]
    fn write_todo_leaves_the_todo_alone_when_the_plan_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let plan_path = dir.path().join("plan.json");
        let todo = dir.path().join("git-rebase-todo");
        std::fs::write(&todo, "pick 1234 original\n").unwrap();
        std::fs::write(&plan_path, RebasePlan::default().to_json()).unwrap();

        let err = write_todo(&plan_path, &todo).unwrap_err();

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
