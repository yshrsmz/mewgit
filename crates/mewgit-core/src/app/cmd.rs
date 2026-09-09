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
        /// セッションを捨てて先頭から読み直す。commit で HEAD が動くと積んだ行が
        /// 丸ごと古くなり、続きを読む経路では直せない（新しいコミットは末尾ではなく
        /// 先頭に付く）。
        ///
        /// **別の `Cmd` の変種にしないのはここが理由。** [`Cmd::job_kind`] が
        /// [`JobKind::LoadLog`] と必ず同じになり、続きの読み込みと読み直しが同時に
        /// 走らない。変種を分けると、揃えるのは手書きの約束になる（01 の
        /// 「ログの読み直し」）。
        restart: bool,
    },
    /// 選択コミットの詳細（メッセージ全文・committer・日時）を読む（02）。
    LoadCommitDetail(Oid),
    /// 選択コミットが変更したファイルの一覧を読む（02 の「コミットのファイル一覧」）。
    ///
    /// **詳細と別の `Cmd` にする。** 一覧はツリー同士の比較で本文の読み出しより重く、
    /// 1 通にまとめると本文しか見えていない時点でも一覧のコストを払う（01）。
    LoadCommitFiles(Oid),
    /// amend の初期値にする HEAD のメッセージ全文（02 の `head_message`）。
    ///
    /// `RepoInfo.head` からは取らない。起動時に 1 回読んだきりなので、1 回コミット
    /// した時点で古い。古い初期値は「1 つ前のコミットのメッセージで HEAD を上書き
    /// する」になる（01 の「commit」）。
    LoadHeadMessage,
    LoadStatus,
    LoadDiff(DiffTarget),
    /// リモート・HEAD・ref の索引を読む。起動時と、ref が変わったあと（M3）。
    LoadRepoInfo,
    /// ローカルブランチの一覧を読む（02 の「ブランチ一覧」）。
    ///
    /// **[`Cmd::LoadRepoInfo`] に乗せない。** ブランチの本数だけ upstream の解決と
    /// `graph_ahead_behind` の walk が要り、グラフのラベルと HEAD だけが欲しい経路に
    /// そのコストを払わせることになる（`LoadCommitFiles` を `LoadCommitDetail` と
    /// 分けたのと同じ理由）。
    ///
    /// **古くなる時点は [`Cmd::LoadRepoInfo`] と同じ**なので、起動時と、ref を動かす
    /// 書き込みの後（`update::Reload` の `Refs` 以上）に出す。commit は
    /// `refs/heads/<現在>` を動かすので、ここを外すと Branches ペインが commit 前の
    /// ahead を出したまま残る。
    LoadBranches,
    /// git CLI の実行（03 参照）。
    Exec(GitOp),
    /// 走っている [`Cmd::Exec`] を中断する（01 の「実行中ジョブのキャンセル」）。
    ///
    /// **[`JobKind`] を持たない。** 中断は新しいタスクを起こすのではなく、走って
    /// いるタスクに合図するだけ。ジョブを登録すると、`Exec` が終わったときに
    /// 誰も畳まないものが `JobTracker` に残る。
    ///
    /// **どの `Exec` かは持たない。** 書き込みは同時に 1 つだけなので
    /// （`AppState::issue` が 2 本目を落とす）、走っているものは常に 1 つに定まる。
    CancelExec,
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
        self.job().map(|(kind, _)| kind)
    }

    /// この `Cmd` が生む非同期ジョブ —— 種別と、スピナーに添える説明
    /// （01 の「スピナー」）。副作用の無い `Cmd` は `None`。
    ///
    /// **種別と説明を 1 つの `match` から出す。** 別々の関数に割ると、`Exec` のように
    /// ペイロードで説明が変わる変種を足したときに、説明の側だけ足りない形が組める
    /// （受け皿の `_` があるとコンパイルも通り、スピナーが静的な語のまま黙る）。
    ///
    /// 網羅 `match` で書くので、`Cmd` に変種を足したら実装漏れがコンパイルエラーになる。
    pub fn job(&self) -> Option<(JobKind, &'static str)> {
        match self {
            // `CancelExec` は走っているタスクへの合図で、新しいタスクを起こさない。
            Cmd::None | Cmd::Batch(_) | Cmd::Quit | Cmd::CancelExec => None,
            Cmd::LoadLog { .. } => Some((JobKind::LoadLog, "log")),
            Cmd::LoadCommitDetail(_) => Some((JobKind::LoadCommitDetail, "commit")),
            Cmd::LoadCommitFiles(_) => Some((JobKind::LoadCommitFiles, "files")),
            Cmd::LoadHeadMessage => Some((JobKind::LoadHeadMessage, "head")),
            Cmd::LoadStatus => Some((JobKind::LoadStatus, "status")),
            Cmd::LoadRepoInfo => Some((JobKind::LoadRepoInfo, "repo")),
            Cmd::LoadBranches => Some((JobKind::LoadBranches, "branches")),
            Cmd::LoadDiff(_) => Some((JobKind::LoadDiff, "diff")),
            // **`Exec` だけは走らせる `GitOp` で説明が変わる**（`fetch` / `push`）。
            // `verb()` は `to_command` の `argv[0]` と同じ語を返す（03）。
            Cmd::Exec(op) => Some((JobKind::Exec, op.verb())),
            Cmd::ResolveAccount => Some((JobKind::ResolveAccount, "account")),
            Cmd::FetchPr => Some((JobKind::FetchPr, "pr")),
            Cmd::FetchChecks => Some((JobKind::FetchChecks, "checks")),
            Cmd::FetchPrList => Some((JobKind::FetchPrList, "pr list")),
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
        // 中断は走っているタスクへの合図。ジョブを登録すると、`Exec` が終わった
        // ときに誰も畳まないものが `JobTracker` に残る。
        assert_eq!(Cmd::CancelExec.job_kind(), None);

        assert_eq!(Cmd::LoadStatus.job_kind(), Some(JobKind::LoadStatus));
        assert_eq!(Cmd::FetchPr.job_kind(), Some(JobKind::FetchPr));
        assert_eq!(
            Cmd::Exec(GitOp::RebaseAbort).job_kind(),
            Some(JobKind::Exec)
        );
    }

    #[test]
    fn the_label_of_an_exec_is_the_git_subcommand() {
        // スピナーに `git` とだけ出しても、何が走っているかは判らない（01）。
        assert_eq!(
            Cmd::Exec(GitOp::RebaseAbort).job(),
            Some((JobKind::Exec, "rebase"))
        );
        assert_eq!(Cmd::LoadStatus.job(), Some((JobKind::LoadStatus, "status")));
    }
}
