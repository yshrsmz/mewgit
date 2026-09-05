//! `update` が返す副作用の指示。設計は `docs/design/01-app-core.md`。
//!
//! `Cmd` 自体は何もしない。ランタイム（`mewgit-tui`）がこれを tokio タスクに変換し、
//! 結果を [`Msg`](crate::app::Msg) として返す。

use crate::git_exec::GitOp;
use crate::git_read::{DiffTarget, Oid};

/// 実行してほしい副作用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    None,
    Batch(Vec<Cmd>),
    LoadLog {
        from: Option<Oid>,
        count: usize,
    },
    LoadStatus,
    LoadDiff(DiffTarget),
    /// git CLI の実行（03 参照）。
    Exec(GitOp),
    ResolveAccount,
    FetchPr,
    FetchChecks,
    FetchPrList,
    Quit,
}

impl Cmd {
    /// 複数の `Cmd` をまとめる。空なら [`Cmd::None`]、1 つならそれ自身を返す。
    ///
    /// `Batch(vec![])` や `Batch(vec![None])` がランタイムに流れないようにするため。
    pub fn batch(cmds: impl IntoIterator<Item = Cmd>) -> Cmd {
        let mut cmds: Vec<Cmd> = cmds.into_iter().filter(|c| *c != Cmd::None).collect();
        match cmds.len() {
            0 => Cmd::None,
            1 => cmds.remove(0),
            _ => Cmd::Batch(cmds),
        }
    }
}
