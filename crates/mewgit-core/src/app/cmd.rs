//! `update` が返す副作用の指示。設計は `docs/design/01-app-core.md`。
//!
//! `Cmd` 自体は何もしない。ランタイム（`mewgit-tui`）がこれを tokio タスクに変換し、
//! 結果を [`Msg`](crate::app::Msg) として返す。

use crate::git_exec::GitOp;
use crate::git_read::{DiffTarget, Oid};

use super::JobKind;

/// 実行してほしい副作用。
///
/// `#[must_use]` を付けている。捨てると `AppState::issue` がジョブを登録したまま
/// 実行されず、以後その種類の `Cmd` が黙って落ち続ける（01 の「追加ページの読み込み」）。
/// 捨ててよい場面では `let _ = ...` と書いて意図を示す。
#[must_use]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    None,
    Batch(Vec<Cmd>),
    /// ログを `count` 件読む。どこから読むかは持たない。トポロジカル順の walk は
    /// 途中の Oid からやり直せないので、位置はランタイム側の `LogSession` が持つ（02）。
    LoadLog {
        count: usize,
    },
    /// 選択コミットの詳細（メッセージ全文・committer・日時）を読む（02）。
    LoadCommitDetail(Oid),
    LoadStatus,
    LoadDiff(DiffTarget),
    /// リモート・HEAD・ref の索引を読む。起動時と、ref が変わったあと（M3）。
    LoadRepoInfo,
    /// git CLI の実行（03 参照）。
    Exec(GitOp),
    ResolveAccount,
    FetchPr,
    FetchChecks,
    FetchPrList,
    Quit,
}

impl Cmd {
    /// 複数の `Cmd` を 1 つに正規化する。
    ///
    /// [`Cmd::None`] を捨て、ネストした [`Cmd::Batch`] は展開し、残りが 0 個なら
    /// `None`、1 個ならそれ自身を返す。`Batch(vec![])` や `Batch(vec![Batch(..)])` を
    /// ランタイムに流さないため。
    pub fn batch(cmds: impl IntoIterator<Item = Cmd>) -> Cmd {
        let mut flat = Vec::new();
        for cmd in cmds {
            flatten_into(cmd, &mut flat);
        }

        match flat.len() {
            0 => Cmd::None,
            1 => flat.remove(0),
            _ => Cmd::Batch(flat),
        }
    }

    /// この `Cmd` が生む非同期ジョブの種別。副作用を持たないものは `None`。
    ///
    /// [`JobKind`] を別に列挙せずここで対応させることで、`Cmd` を単一の真実にする。
    /// 網羅 `match` なので変種を足したら実装漏れがコンパイルエラーになる。
    pub fn job_kind(&self) -> Option<JobKind> {
        match self {
            Cmd::None | Cmd::Batch(_) | Cmd::Quit => None,
            Cmd::LoadLog { .. } => Some(JobKind::LoadLog),
            Cmd::LoadCommitDetail(_) => Some(JobKind::LoadCommitDetail),
            Cmd::LoadStatus => Some(JobKind::LoadStatus),
            Cmd::LoadRepoInfo => Some(JobKind::LoadRepoInfo),
            Cmd::LoadDiff(_) => Some(JobKind::LoadDiff),
            Cmd::Exec(_) => Some(JobKind::Exec),
            Cmd::ResolveAccount => Some(JobKind::ResolveAccount),
            Cmd::FetchPr => Some(JobKind::FetchPr),
            Cmd::FetchChecks => Some(JobKind::FetchChecks),
            Cmd::FetchPrList => Some(JobKind::FetchPrList),
        }
    }
}

/// `Cmd::None` を捨て、`Batch` を再帰的に展開して `out` に積む。
///
/// `Cmd::Batch` は公開変種なので、`batch` を通らずに手で組まれた入れ子も来る。
/// 1 段だけの展開だと `Batch(vec![None, Batch(..)])` がそのまま残る。
fn flatten_into(cmd: Cmd, out: &mut Vec<Cmd>) {
    match cmd {
        Cmd::None => {}
        Cmd::Batch(inner) => {
            for cmd in inner {
                flatten_into(cmd, out);
            }
        }
        cmd => out.push(cmd),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_drops_none_and_unwraps_single() {
        assert_eq!(Cmd::batch([]), Cmd::None);
        assert_eq!(Cmd::batch([Cmd::None, Cmd::None]), Cmd::None);
        assert_eq!(Cmd::batch([Cmd::LoadStatus]), Cmd::LoadStatus);
        assert_eq!(
            Cmd::batch([Cmd::None, Cmd::LoadStatus, Cmd::None]),
            Cmd::LoadStatus
        );
    }

    #[test]
    fn batch_keeps_order_of_the_rest() {
        assert_eq!(
            Cmd::batch([Cmd::LoadStatus, Cmd::None, Cmd::Quit]),
            Cmd::Batch(vec![Cmd::LoadStatus, Cmd::Quit])
        );
    }

    #[test]
    fn batch_flattens_nested_batches() {
        let nested = Cmd::batch([Cmd::LoadStatus, Cmd::FetchPr]);
        assert_eq!(
            Cmd::batch([nested, Cmd::Quit]),
            Cmd::Batch(vec![Cmd::LoadStatus, Cmd::FetchPr, Cmd::Quit])
        );
    }

    #[test]
    fn batch_flattens_hand_built_nesting() {
        // Cmd::Batch は公開変種なので、batch を通らずに組まれた入れ子も来る。
        let hand_built = Cmd::Batch(vec![
            Cmd::None,
            Cmd::Batch(vec![Cmd::LoadStatus, Cmd::None]),
        ]);
        assert_eq!(
            Cmd::batch([hand_built, Cmd::Quit]),
            Cmd::Batch(vec![Cmd::LoadStatus, Cmd::Quit])
        );
    }

    #[test]
    fn batch_of_only_empty_nesting_is_none() {
        let empty = Cmd::Batch(vec![Cmd::Batch(vec![Cmd::None]), Cmd::None]);
        assert_eq!(Cmd::batch([empty]), Cmd::None);
    }

    #[test]
    fn job_kind_covers_every_side_effecting_cmd() {
        assert_eq!(Cmd::None.job_kind(), None);
        assert_eq!(Cmd::Batch(vec![]).job_kind(), None);
        assert_eq!(Cmd::Quit.job_kind(), None);

        assert_eq!(Cmd::LoadStatus.job_kind(), Some(JobKind::LoadStatus));
        assert_eq!(Cmd::FetchPr.job_kind(), Some(JobKind::FetchPr));
        assert_eq!(
            Cmd::Exec(GitOp::RebaseAbort).job_kind(),
            Some(JobKind::Exec)
        );
    }
}
