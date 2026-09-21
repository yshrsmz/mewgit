//! `update` が返す副作用の指示。設計は `docs/design/01-app-core.md`。
//!
//! `Cmd` 自体は何もしない。ランタイム（`mewgit-tui`）がこれを tokio タスクに変換し、
//! 結果を [`Msg`](crate::app::Msg) として返す。

use crate::auth::url::RemoteUrl;
use crate::auth::RepoTarget;
use crate::git_exec::GitOp;
use crate::git_read::{DiffTarget, Oid};
use crate::github::CreatePr;

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
    /// 既定リモートの URL からホストを正規化して [`RepoTarget`](crate::auth::RepoTarget) を組む（01 の
    /// 「`RepoInfo.target` を埋める」）。ssh なら `ssh -G`、https ならそのまま。
    ///
    /// **運ぶのは解析済みの [`RemoteUrl`] で、生の URL ではない。** 解析は純粋関数
    /// なので `update` の中で済み、読めない URL のときは `Cmd` を出さずに済む。
    ///
    /// **https でもこの `Cmd` を通す。** `ssh -G` が要らないぶん往復は無駄だが、
    /// [`RepoInfo::target`](super::RepoInfo::target) を書く場所を 1 つに保つほうが
    /// 強い（2 本にすると、片方だけ通知を出す・片方だけ検査する、が起きる。#18）。
    ResolveTarget(RemoteUrl),
    /// そのリポジトリで使う GitHub アカウントを決める（04 の手順 1〜4）。
    ///
    /// **対象を運ぶ。** ランタイムに
    /// [`RepoInfo::target`](super::RepoInfo::target) を読ませると、`TargetState` の
    /// 3 つのうち `Resolved` 以外を受けたときの腕を `dispatch` に書くことになり、
    /// **どのテストからも踏めない腕**が 2 つ増える（`update` はその 2 つでは
    /// この `Cmd` を出さない）。
    ///
    /// **出すのは [`Msg::TargetResolved`](super::Msg::TargetResolved) を受けたとき
    /// だけで、`init` からは出さない** —— [`RepoTarget`] は起動時にはまだ無い
    /// （01 の「`RepoInfo.target` を埋める」）。
    ///
    /// **`Box` に載せない。** [`Msg`](super::Msg) と違って `Cmd` の大きさを決めて
    /// いるのは [`Cmd::Exec`]（`GitOp` が 96 バイト）で、[`RepoTarget`] は 80 バイト。
    /// 箱に入れても 96 のまま（実測）なので、ヒープ確保と間接参照が増えるだけになる。
    ResolveAccount(RepoTarget),
    /// そのホストの login 一覧だけを引き直す（01 の「候補の取り直し」）。
    ///
    /// **[`Cmd::ResolveAccount`] とは別の `Cmd`。** あちらは 04 の手順 1〜4 を通して
    /// [`AccountState`](crate::auth::AccountState) を作り直すもので、こちらは
    /// `gh auth status` を 1 回叩くだけ。畳むと、選び直しのモーダルを開くたびに
    /// アカウントが `Resolving` に戻る（ヘッダから `[gh: …]` が消える）。
    ///
    /// **運ぶのはホスト名だけ。** `auth::Backend::logins` が要るのが `host` だけで、
    /// [`RepoTarget`] を渡すと `owner` / `repo` を読まない引数が付く（「導出できる値を
    /// 引数に取らせない」）。
    ///
    /// **返事の受け手はモーダルだけ**（[`Msg::AccountsListed`](super::Msg::AccountsListed)）。
    /// `state.account` は動かさない。
    ListAccounts(String),
    /// 開いている PR の一覧を引く（01 の「PR ペイン › 何を引くか」）。
    ///
    /// **鍵を運ばない。** 一覧はブランチにもコミットにも紐づかないので、返信を
    /// 突き合わせる相手が無い。追いつきは世代（[`JobKind::FetchPrList`] の
    /// `region()`）が見る。`owner` / `repo` / トークンも運ばない —— `dispatch` が
    /// `state.repo.target` と `state.account` から読む（[`Cmd::ResolveAccount`] が
    /// `config` を読ませているのと同じ）。
    FetchPrList,
    /// 一覧の各 PR について、チェックとレビューの集計を引く（01 の
    /// 「PR ペイン › 集計」）。
    ///
    /// **扇形でも `Cmd` は 1 本。** [`JobTracker`](super::JobTracker) は種別単位なので、
    /// PR ごとに `Cmd` を出すと 2 本目以降を [`AppState::issue`](super::AppState::issue)
    /// が黙って落とす。同時本数を抑えるのは `github` の中（05 の「`Semaphore`」）。
    ///
    /// **運ぶのは `(番号, head sha)`。** チェックはコミットに紐づくので、鍵には
    /// sha が要る（push で head が進んだ PR に前のコミットの結果を出さないため）。
    /// **一覧そのものは運ばない** —— `dispatch` が必要とするのは叩く先だけで、
    /// `PullRequest` を丸ごと渡すと `Cmd` を比べるテストが一覧の中身に依存する。
    FetchPrDetails {
        /// どのアカウントの一覧に対する要求か（`PrState::epoch`）。返信がこれを
        /// 持ち帰り、アカウントが切り替わっていれば捨てられる。
        epoch: u64,
        targets: Vec<(u64, String)>,
    },
    /// PR を作る（01 の「PR 作成（`N`）」）。`POST /repos/{o}/{r}/pulls` 1 本。
    ///
    /// **`owner` / `repo` とトークンは運ばない。** `dispatch` が
    /// `state.repo.target` と `state.account` から読む（[`Cmd::FetchPrList`] と
    /// 同じ）。運ぶのはモーダルが打たせた字だけ。
    ///
    /// **`Box` に入れる。** 実測（2026-09-21、64bit）—— `CreatePr` は 104 バイトで
    /// [`Cmd::Exec`] が運ぶ `GitOp`（96）より大きく、裸で載せると `Cmd` が
    /// **96 → 112 バイト**になる。箱なら 8 バイトで、`Cmd` は 96 のまま。
    /// **`ResolveAccount` が `RepoTarget`（80）を裸で載せているのと逆の判断**で、
    /// 分け目は「`GitOp` より大きいか」。
    CreatePr(Box<CreatePr>),
    /// `gh browse <番号>` でブラウザに渡す（01 の「`o`（ブラウザで開く）」）。
    ///
    /// **[`Cmd::Exec`] に相乗りさせない。** git の実行中に押せなくなるうえ、
    /// `Ctrl-C` の中断が git 以外のプロセスに効くことになる。
    OpenPr {
        number: u64,
    },
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
            Cmd::ResolveTarget(_) => Some((JobKind::ResolveTarget, "remote")),
            Cmd::ResolveAccount(_) => Some((JobKind::ResolveAccount, "account")),
            Cmd::ListAccounts(_) => Some((JobKind::ListAccounts, "logins")),
            Cmd::FetchPrList => Some((JobKind::FetchPrList, "pr list")),
            Cmd::FetchPrDetails { .. } => Some((JobKind::FetchPrDetails, "pr checks")),
            Cmd::CreatePr(_) => Some((JobKind::CreatePr, "pr create")),
            Cmd::OpenPr { .. } => Some((JobKind::OpenPr, "browse")),
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
    fn cmd_stays_small() {
        // **`ResolveAccount` の doc が書いている「箱に入れても変わらない」の見張り。**
        // 大きさを決めているのは `Exec(GitOp)` で、`RepoTarget`（80 バイト）も
        // `RemoteUrl`（80 バイト）もそれより小さいから裸で載せている。`GitOp` が
        // 伸びればここが落ちて、その断定を数え直すことになる（実測値は 64bit で 96）。
        // `Msg` 側の `msg_stays_small` と同じ形。
        //
        // **`CreatePr`（104 バイト）はこの見張りに従って箱に入れた** —— 裸で載せると
        // ここが 112 で落ちる（実測 2026-09-21）。つまりこの assert は
        // 「`GitOp` より大きいものは箱へ」という規則の enforcement でもある。
        let size = std::mem::size_of::<Cmd>();
        assert!(size <= 96, "Cmd が大きすぎます: {size} バイト");
    }

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
    fn batch_flattens_hand_built_nesting() {
        // Cmd::Batch は公開変種なので、batch を通らずに組まれた入れ子も来る。
        //
        // **`flatten_into` の腕をこの 1 本で踏む** —— `None` を捨て、`Batch` を
        // 再帰で展開し、残りの順序を保つ。1 段だけの展開に戻すと、内側の `Batch` が
        // 畳まれずに残って期待値と食い違う。
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
    fn job_kind_covers_every_side_effecting_cmd() {
        assert_eq!(Cmd::None.job_kind(), None);
        assert_eq!(Cmd::Batch(vec![]).job_kind(), None);
        assert_eq!(Cmd::Quit.job_kind(), None);
        // 中断は走っているタスクへの合図。ジョブを登録すると、`Exec` が終わった
        // ときに誰も畳まないものが `JobTracker` に残る。
        assert_eq!(Cmd::CancelExec.job_kind(), None);

        assert_eq!(Cmd::LoadStatus.job_kind(), Some(JobKind::LoadStatus));
        assert_eq!(Cmd::FetchPrList.job_kind(), Some(JobKind::FetchPrList));
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
