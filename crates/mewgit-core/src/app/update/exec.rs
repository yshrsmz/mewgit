//! 書き込みの後始末。
//!
//! `Cmd::Exec` が終わったときに、何を読み直し・何を案内するかを決める。
//!
//! **対応する `##` が 01 に 1 つも無い、唯一のモジュール。** 材料は 3 節に散っている
//! —— 01 の「ログの読み直し」（`Reload` を名指ししているのはここ）、「書き込みの
//! あとの status」、「失敗の案内文」。**この 3 つを直したらここも読む**（01 の
//! 「この文書とコードの対応」）。
//! **`git_exec` ではなく `app` に置く** —— どの ref をグラフが辿るかを知って
//! いるのはこちらで、`GitOp` の隣に置くと git の事実を約束しながら中身は UI の
//! 方針、という形になる（[`Reload`] の doc）。

use crate::git_exec::{ExecError, ExecErrorKind, ExecOutput, GitOp, PullMode};

use super::account::resolve_again;
use super::graph::reload_log;
use super::progress::MENU;
use super::remote::{plain_push, push_rejection, PlainPush};
use crate::app::keys::{self, Action, CommonAction, GraphAction};
use crate::app::{AppState, Cmd, ExecDone, JobKind, NoticeLevel, Region};

/// `git` の実行が終わった。
///
/// 進行中操作は、ここで出す `LoadStatus` の返り（`WorktreeSnapshot.progress`）で
/// 入れ替わる（03 の「進行中操作との整合」）。`Reload::Account` だけは `LoadStatus` を
/// 出さないが、`.git/config` の 1 行では進行中操作は動かない。
pub(super) fn exec_finished(state: &mut AppState, done: ExecDone) -> Cmd {
    state.jobs.finish(JobKind::Exec);
    // **押したブランチの行は 1 回だけ引く。** 案内と記憶が同じ答えを読む。
    let pushed = plain_push(state, &done.op);

    if let Err(err) = &done.result {
        // **要約行は案内文、証拠は `detail`。** ステータスバーは `message` の 1 行目
        // しか描かない（07）ので、そこを argv で埋めると案内が画面に出ない
        // （01 の「失敗の案内文」）。案内が無ければ [`exec_summary`] に落とす。
        let detail = exec_detail(err);
        let advice = exec_advice(&done.op, err, pushed, state);
        // **中断は失敗ではない。** 押した本人が止めたものを `Error` で出すと、
        // 頼んでいないことが壊れたように読める。**コンフリクトで止まったのも同じ** ——
        // continue のあとに次のコミットでまた止まるのは想定どおりの結末で、進行中操作は
        // 残ってバッジの数が進む（01 の「失敗の案内文」）。**変更が空で止まった pick も**
        // 進行中操作が残り、出口は `m` の skip にある。**取り消す変更が無かった revert** は
        // 何も始まらずに終わっただけで、壊れたものが無い。**HEAD が動いて断った
        // interactive rebase も**始まっておらず、`i` から開き直せば済む。**未解消の
        // ファイルで断られた書き込みも**何も書いておらず、解消すれば済む。
        let level = match err {
            ExecError::Canceled { .. } => NoticeLevel::Info,
            // git を起こしていない。読み直した差分で押し直せば済む。
            ExecError::Stale { .. } => NoticeLevel::Warn,
            ExecError::Failed {
                kind:
                    ExecErrorKind::Conflict
                    | ExecErrorKind::Empty
                    | ExecErrorKind::NoChanges
                    | ExecErrorKind::HeadMoved
                    | ExecErrorKind::Unmerged,
                ..
            } if advice.is_some() => NoticeLevel::Warn,
            ExecError::NoResult { .. } | ExecError::Failed { .. } => NoticeLevel::Error,
        };
        let message = advice.unwrap_or_else(|| exec_summary(err, state));
        state.push_notice(level, message, Some(detail));
    }

    if let GitOp::Push { src, .. } = &done.op {
        // force に上げられるのは追跡先への素の push だけ（[`push_rejection`]）。
        let upgradable = matches!(
            pushed,
            Some(PlainPush::Upstream { .. } | PlainPush::Unlisted)
        );
        state.push_rejected = push_rejection(state, &done.result, src, upgradable);
    }

    // **書き込みで指紋は必ず変わる。** 持っている指紋と比べると自分の書き込みを外の
    // 変更と読むので、次の probe の返りを基準にし直す（01 の「外部の変更の検知 ›
    // mewgit 自身の書き込みと分ける」）。
    state.watch.fingerprint = None;

    reload(
        state,
        Reload::after(&done.op, ref_may_have_moved(&done.result)),
    )
}

/// `reload` の領域を古くして、読み直しを出す。
///
/// **書き込みの後始末と、外の変更の検知（`watch`）の 2 つが通る。** 外の変更も
/// 「持っている値が古くなった」で、書き込みが 1 つ終わったのと同じ形で読み直す
/// （01 の「外部の変更の検知」）。
pub(super) fn reload(state: &mut AppState, reload: Reload) -> Cmd {
    // **持っている値と、走っている読み取りが、ここで古くなる。** 走っている読み取りは
    // この書き込み（または外の変更）より前に頼まれたものなので、届いても採ってはいけない。世代を進めて
    // おくと、下で出す `Cmd` は `issue` で新しい世代で刻まれ、古い返信とは刻印で
    // 見分けが付く（01 の「実行中ジョブ › 世代」）。
    //
    // **`Cmd` を返す前に進める。** `issue` は `update` の出口なので、返してからでは
    // 遅い。
    for region in reload.regions() {
        state.jobs.advance(*region);
    }

    // 続きを頼む: 書き込みで動いた領域の読み直し（`Reload::after`）。
    match reload {
        Reload::Nothing => Cmd::LoadStatus,
        Reload::Stashes => Cmd::batch([Cmd::LoadStatus, Cmd::LoadStashes]),
        // **ブランチ一覧も ref から導かれる**（`LoadRepoInfo` が運ぶ）。commit は
        // `refs/heads/<現在>` を動かすので、読み直さないと Branches ペインが commit 前の
        // ahead のまま残る。
        Reload::Refs => Cmd::batch([Cmd::LoadStatus, Cmd::LoadRepoInfo]),
        // **stash の一覧もここで読む**（01 の「Stash ペイン」）。stash の書き込みでなくても
        // `refs/stash` は動く —— `merge.autoStash` / `rebase.autoStash` を設定していると、
        // `pull --ff-only` や `rebase --continue` が exit 0 のまま `stash@{0}: autostash` を
        // 積む（git 2.55.0 で実測）。読まないと、退避された変更が再起動まで画面に出ない。
        // 払うのは reflog 1 本の読み取りなので、autostash の起きうる op に絞らない。
        Reload::RefsAndLog => Cmd::batch([
            Cmd::LoadStatus,
            Cmd::LoadRepoInfo,
            Cmd::LoadStashes,
            reload_log(state),
        ]),
        Reload::Account => resolve_again(state),
    }
}

/// 書き込みのあとに読み直すもの。`moved` は ref が動いた可能性があるか
/// （[`ref_may_have_moved`]）。
///
/// **`git_exec` ではなく `app` に置く。** 「どの ref をグラフが辿るか」を知っているのは
/// こちらで、`GitOp` の隣に `moves_refs` のような名前で置くと、git の事実を約束しながら
/// 中身は UI の方針という形になる（stash が偽なのは「`refs/stash` を walk に押して
/// いない」からで、git は ref を動かしている）。Stash ペインが入ったときに黙って
/// 嘘になる。
///
/// 網羅 `match` にしているのは、`GitOp` に変種を足したときにどれなのかを選ばせるため
/// （`to_command` と同じ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reload {
    /// index / ワークツリーだけ。`Cmd::LoadStatus` のほかには何も足さない。
    Nothing,
    /// ref の索引（ラベルと HEAD）。**walk に入るコミットの集合は変わらない**ので、
    /// 積んだログはそのままでよい。
    Refs,
    /// 索引と、積んだログの両方。
    RefsAndLog,
    /// index / ワークツリーと、stash の一覧（`refs/stash`）。**ラベルとログは読み直さない**
    /// —— グラフは `refs/stash` を walk に押していない（02 の「ログ列挙」）。
    Stashes,
    /// git の読み取りは 1 つも古くならない。**アカウントの解決だけをやり直す**
    /// （`.git/config` の `mewgit.user`。01 の「アカウントの選び直し」）。
    ///
    /// **`Nothing` に落とさない。** あちらは `Cmd::LoadStatus` を出して
    /// `Region::Worktree` の世代を進めるので、index もワークツリーも触っていない
    /// 書き込みで**飛んでいた status / diff の返信が捨てられ、頼み直しになる**。
    ///
    /// **`exec_finished` の外に `matches!` を置かない。** ここに腕があれば、
    /// 次に「ref を動かさないが別のものを古くする」書き込みが来たときに
    /// [`Reload::after`] の網羅 `match` が選ばせる（外に置くと、その op は嘘の
    /// 分類に落ちて後始末だけが 2 本目の分岐として積まれる）。
    Account,
}

impl Reload {
    /// この書き込みで古くなる領域。**累積**で、`Refs` は index とワークツリーも含む。
    ///
    /// **git のワークツリーに触る書き込みは必ず [`Region::Worktree`] を古くする**
    /// （`switch` も `push` もワークツリーや index を動かしうるし、そのとき
    /// `Cmd::LoadStatus` を必ず出す）。**触らないものが 1 つある** ——
    /// [`Reload::Account`] は `.git/config` の 1 行しか書かないので、どの領域も
    /// 古くならない。判別子をここに置くのは、`Reload` が「どの書き込みがどこを
    /// 古くするか」を既に持っているため（01 の「実行中ジョブ › 世代」）。
    fn regions(self) -> &'static [Region] {
        match self {
            Reload::Nothing => &[Region::Worktree],
            Reload::Stashes => &[Region::Worktree, Region::Stash],
            Reload::Refs => &[Region::Worktree, Region::Refs],
            // autostash の pull / rebase は `refs/stash` も動かす（下の `LoadStashes`）。
            Reload::RefsAndLog => &[Region::Worktree, Region::Refs, Region::Log, Region::Stash],
            // git の読み取りは何も古くならない（`.git/config` の 1 行だけ）。
            Reload::Account => &[],
        }
    }

    fn after(op: &GitOp, moved: bool) -> Self {
        match op {
            // index / ワークツリーだけ。ref も HEAD も動かない。
            GitOp::Stage(_)
            | GitOp::Unstage(_)
            | GitOp::StageHunk(_)
            | GitOp::UnstageHunk(_)
            | GitOp::DiscardHunk(_)
            | GitOp::Discard(_)
            | GitOp::DiscardUntracked(_)
            | GitOp::DiscardAdded(_) => Reload::Nothing,

            // **ブランチに居たまま index とワークツリーを戻す**ので、HEAD の ref は
            // 動かない（`MERGE_HEAD` はグラフが walk に押す ref ではない）。**ただし
            // `refs/stash` は動きうる** —— `merge.autoStash` で始めた merge を、作業ツリーを
            // 触ってから abort すると、autostash を戻すところでコンフリクトし、exit 0 の
            // まま `stash@{0}: autostash` を積む（git 2.55.0 で実測）。一覧を読み直さないと
            // 古い index が別の stash を指す（01 の「stash の書き込み」）。
            GitOp::MergeAbort => Reload::Stashes,

            // **ラベルと HEAD は動くが、行は動かない。** グラフのラベルは行に焼き込まず
            // `GraphState.refs` とブランチ一覧から引く（02 の「refs とラベル」）ので、
            // `Cmd::LoadRepoInfo` だけで足りる。
            // ログの読み直しは `LogSession` の作り直しで履歴全体のコストが掛かる（02 の
            // 「セッションは作り直さず使い回す」）ので、要らないところでは払わない。
            //
            // どれも「既にある、どこかの ref から辿れるコミット」を指すだけ。
            // `Push` が非 force なら `origin/x` はローカルの先端へ早送りされるので、
            // 行はもう並んでいる。
            //
            // **断られた `Switch` でも読み直す。** ref も HEAD も動いていないので
            // 無駄に見えるが（`switch` は断った時点で post-checkout hook すら走らない。
            // git 2.55 で確認）、**断られる理由の 1 つが「一覧が古い」**（別の端末で
            // 消されたブランチに `Enter` を押した）で、そこで読み直さないと画面から
            // 消えない行が残る。払うのはキー押下 1 回あたりの読み取り 2 本。
            GitOp::Switch(_)
            | GitOp::BranchCreate { .. }
            | GitOp::BranchRename { .. }
            | GitOp::Push { lease: None, .. } => Reload::Refs,

            // stash は `refs/stash` しか動かさず、グラフはそれを walk に押していない
            // （02 の「ログ列挙」）。ワークツリーの変化は `Cmd::LoadStatus` が拾う。
            // **ここが `app` に置く理由。** グラフが `refs/stash` を押すようになったら
            // この腕が動く。**成否を見ない** —— pop はコンフリクトで exit 1 でも作業ツリーを
            // 書き換える（stash は残す。01 の「stash の書き込み」）。
            GitOp::StashPush { .. }
            | GitOp::StashApply(_)
            | GitOp::StashPop(_)
            | GitOp::StashDrop(_) => Reload::Stashes,

            // `.git/config` の 1 行だけ。index もワークツリーも ref も動かない。
            //
            // **成否を見ない**（`Reload::after` はどの腕でも結果を見ない）。中断
            // （`ExecError::Canceled`）では**書けたかどうか判らない**（#56 の
            // 「失敗なら ref は元のまま」が中断では成り立たない、と同じ形）。
            // 読み直せば `.git/config` の実物が答えるので、成否で分岐する理由が無い。
            // **`UnsetAccount` も同じ腕。** 外そうとしたキーが既に無ければ
            // `git config --unset` は exit 5 を stderr 空で返す（03 の実測表）ので、
            // 成否から「いま何が設定されているか」は読めない。
            GitOp::SetAccount { .. } | GitOp::UnsetAccount => Reload::Account,

            // **1 本だけの cherry-pick / revert は、失敗したなら HEAD は元のまま**
            // （03 の「cherry-pick の開始」「revert の開始」の実測）。下の `Commit` と同じ
            // 理由で、読み直すと `GraphState::restart` がグラフの位置を先頭に戻す ——
            // `y` / `v` を押したコミットを見失う。**複数では下の腕に落ちる**（途中まで
            // 積んでから止まる）。
            GitOp::CherryPick(oids) | GitOp::Revert { oids, .. } if oids.len() == 1 => {
                Reload::one_shot(moved)
            }

            // walk に入るコミットの集合が変わる。**失敗しても途中まで ref が
            // 動いていることがある**ので成否を見ない。fetch / pull は一部の ref だけ
            // 更新して落ちうるし、rebase / cherry-pick / revert は途中で止まる。
            GitOp::Fetch { .. }
            | GitOp::Pull { .. }
            | GitOp::RebaseInteractive { .. }
            | GitOp::RebaseContinue { .. }
            | GitOp::RebaseSkip
            | GitOp::RebaseAbort
            | GitOp::CherryPick(_)
            | GitOp::CherryPickContinue
            | GitOp::CherryPickSkip
            | GitOp::CherryPickAbort
            | GitOp::Revert { .. }
            | GitOp::RevertContinue
            | GitOp::RevertSkip
            | GitOp::RevertAbort
            // PR の取り出しは fetch で新しいコミットを入れ、ブランチを作り、HEAD を
            // 動かす。**手順の途中で止まっても前の手順の結果は残る**（03 の「複数の
            // 手順を持つ書き込み」）ので、成否を見ない。
            | GitOp::CheckoutPr(_) => Reload::RefsAndLog,

            // 同じく walk が変わるが、**1 回で決まるので失敗したなら ref は元のまま**。
            // そこで読み直すと `GraphState::restart` がカーソルとスクロールを先頭に
            // 戻すので、pre-commit hook に弾かれただけでグラフの位置を失う。
            // **中断は「失敗」ではない**（[`ref_may_have_moved`]）。
            GitOp::Commit { .. }
            | GitOp::BranchDelete { .. }
            | GitOp::Push { lease: Some(_), .. } => Reload::one_shot(moved),
        }
    }

    /// 1 回で決まる書き込み。**失敗したなら ref は元のまま**なので、ログまでは読み直さない
    /// （`GraphState::restart` がグラフの位置を先頭に戻すため）。
    fn one_shot(moved: bool) -> Self {
        if moved {
            Reload::RefsAndLog
        } else {
            Reload::Refs
        }
    }
}

/// この書き込みで ref が動いた可能性があるか。
///
/// **中断は「動いたかもしれない」側に置く。** `git commit` は ref を書いてから
/// post-commit hook を走らせるので、hook の最中に殺すと**コミットは済んでいる**
/// （git 2.55 で確認。exit は signal で 143）。失敗と同じ扱いにすると `Reload::Refs`
/// までしか出ず、**新しいコミットが積んだ行に無いままブランチのラベルだけが動く** ——
/// 次に walk を作り直す操作まで画面に出てこない。
///
/// 網羅 `match` にしているのは、`ExecError` に変種を足したときにどちら側かを
/// 選ばせるため。
fn ref_may_have_moved(result: &Result<ExecOutput, ExecError>) -> bool {
    match result {
        Ok(_) | Err(ExecError::Canceled { .. }) => true,
        // 起動していない / git が自分で断った。どちらも ref は元のまま。
        Err(ExecError::NoResult { .. } | ExecError::Stale { .. } | ExecError::Failed { .. }) => {
            false
        }
    }
}

/// 通知に添える詳細。実行した argv と git の出力を握りつぶさない（不変ルール 4）。
///
/// **stdout も載せる。** 理由が stdout にしか出ない失敗がある（`needs merge` は
/// stderr が `could not write index` だけでも stdout に出る。03 の「エラー分類」）。
/// **順は argv、stderr、stdout** —— 履歴のモーダルは入りきらない `detail` を末尾から
/// `…` で切る（07）ので、長くなりうる stdout（複数の cherry-pick が積んだ行、status の
/// 一覧）を前に置くと stderr の理由が箱から押し出される。
fn exec_detail(err: &ExecError) -> String {
    let mut detail = err.command_line();
    match err {
        ExecError::Failed { stdout, stderr, .. } => {
            for output in [stderr, stdout] {
                let output = output.trim_end();
                if !output.is_empty() {
                    detail.push('\n');
                    detail.push_str(output);
                }
            }
        }
        // **どのファイルかは argv に無い**（パッチは標準入力）。要約行にも入れていない。
        ExecError::Stale { path, .. } => {
            detail.push('\n');
            detail.push_str(&format!(
                "{}: ハンクを読んだあとに、ファイルか index が変わっていました（git は起動していません）",
                path.display()
            ));
        }
        ExecError::NoResult { .. } | ExecError::Canceled { .. } => {}
    }
    detail
}

/// 案内文に書くキーの動作（08 の「文言に書いたキーの字」）。`m` は [`MENU`]。
const REBASE: Action = Action::Graph(GraphAction::Rebase);
const FETCH: Action = Action::Common(CommonAction::Fetch);
const PULL: Action = Action::Common(CommonAction::Pull);
const PUSH: Action = Action::Common(CommonAction::Push);

/// 分かれたブランチの push が断られたときの、キーを書かない案内。**`p` / `P` のどちらかが
/// 引けなければ、mewgit の中の手を挙げずに外の git へ送る**（08 の「文言に書いたキーの字」）。
const DIVERGED: &str = "リモートと分かれています。外の git で取り込むか force push";

/// 変更が空で止まったときの案内（出口は `m` の skip）。
fn empty_stop(state: &AppState) -> String {
    keys::spell(
        state,
        [MENU],
        |[menu]| format!("変更が空で止まった。{menu} から skip"),
        "変更が空で止まった。skip して",
    )
}

/// git の出力はあるのに、理由の行を選べなかった失敗の要約行（01 の「失敗の案内文」）。
/// hook の文言と ssh の行がここに来る。**`L` はどのペインでも効く**共通キー（07）。
/// `L` が引けなければ「通知の履歴」と言う（08 の「文言に書いたキーの字」）。
fn unshaped_failure(state: &AppState) -> String {
    keys::spell(
        state,
        [Action::Common(CommonAction::NoticeLog)],
        |[log]| format!("git が断りました。理由は {log} の履歴で"),
        "git が断りました。理由は通知の履歴で",
    )
}

/// 案内文の無い失敗の要約行。git の出力から選んだ理由の行（`ExecError::reason`）、
/// 選べなければ固定の文言、**出力が空なら `Display`（argv）** —— 空のときに `L` を
/// 案内しても、履歴には argv しか無い。
fn exec_summary(err: &ExecError, state: &AppState) -> String {
    if let Some(reason) = err.reason() {
        return reason.to_string();
    }
    match err {
        ExecError::Failed { stdout, stderr, .. }
            if !stdout.trim().is_empty() || !stderr.trim().is_empty() =>
        {
            unshaped_failure(state)
        }
        _ => err.to_string(),
    }
}

/// 通知の要約行。失敗の種別から「次に何をすればよいか」を出す（01 の「失敗の案内文」）。
///
/// `None` は案内が無いということ。呼び出し側は [`exec_summary`] に落とす。
///
/// **`op` まで見るのは非 FF のため。** 同じ `(non-fast-forward)` でも、force push が
/// lease に断られた形（fetch を勧める）、`-u` の提案から入って force に上げる経路が
/// 無い形、追跡設定に沿った push が断られた形で押すキーが違う。kind だけで文を選ぶと
/// どれかに嘘を言う。
///
/// **どの文も通知の予算に収める**（80 桁の端末で、時刻の 9 セルを引いた 71 セル。01 の
/// 「失敗の案内文」）。フッターの 1 行目は通知とその時刻だけが使う（キーヒントは 2 行目。
/// 07）。溢れると末尾が `…` に置き換わるので、「どうすればよいか」の側から消える。
/// 見張りは `mewgit-tui` 側にも置く（80 桁で描いて確かめる）。
fn exec_advice(
    op: &GitOp,
    err: &ExecError,
    pushed: Option<PlainPush>,
    state: &AppState,
) -> Option<String> {
    // `NoResult` の `reason` は git の stderr ではなく mewgit の文言で、それ自体が
    // 案内になっている（03）。差し替えると説明が消える。`Canceled` も同じで、
    // `Display` の「git の実行を中断しました」以上に言うことが無い。
    let ExecError::Failed { kind, .. } = err else {
        return None;
    };
    match kind {
        ExecErrorKind::SamlSsoRequired => {
            Some("SAML SSO の承認が要ります。GitHub で鍵を Authorize してください".into())
        }
        ExecErrorKind::SshAuthFailed => {
            // **ホストは入れない。** `ssh -T <host>` を出すには「どのリモートで
            // 失敗したか」が要るが、`Pull` はリモート名を運ばず、`RemoteUrl` は
            // `user@` を捨てる（04）。M4 の「既定リモートを決める」へ送った（03）。
            Some("SSH 鍵で認証できませんでした。ssh-agent と鍵の登録を確認してください".into())
        }
        ExecErrorKind::HttpsAuthFailed => {
            Some("HTTPS 認証に失敗しました。gh auth setup-git を実行してください".into())
        }
        ExecErrorKind::NonFastForward => non_fast_forward_advice(op, pushed, state),
        ExecErrorKind::Conflict => conflict_advice(op, state),
        ExecErrorKind::Empty => empty_advice(op, state),
        ExecErrorKind::NoChanges => no_changes_advice(op, state),
        // 印を出すのは sequence editor だけで、それを起こすのは `RebaseInteractive` の開始
        // だけ（continue / skip は起こさない。03 の実測）。**打った reword は戻らない**
        // —— 開き直すと plan は読み直したグラフから組み直される（01 の「失敗の案内文」）。
        // もう 1 つの入口は断られた reword を持つ continue の照合で、失敗のあとの status が
        // 見分け直すので、開き直したメニューはいまの HEAD で組まれる。
        ExecErrorKind::HeadMoved => match op {
            GitOp::RebaseInteractive { .. } => Some(keys::spell(
                state,
                [REBASE],
                |[rebase]| format!("組んだあとに履歴が動いた。{rebase} から開き直して"),
                "組んだあとに履歴が動いた。開き直して",
            )),
            GitOp::RebaseContinue { reword: Some(_) } => Some(keys::spell(
                state,
                [MENU],
                |[menu]| format!("止まったあとに HEAD が動いた。{menu} から開き直して"),
                "止まったあとに HEAD が動いた。開き直して",
            )),
            _ => None,
        },
        ExecErrorKind::Unmerged => unmerged_advice(op, state),
        // 起動中に mewgit のバイナリが消えた（03 の「消えた自バイナリ」）。起動し直せば
        // `current_exe()` を読み直すので続けられる。**押すキーで 3 つに分ける** —— 開始は
        // 始まっておらず、continue / skip は rebase が止まったまま（バッジと並ぶので 48
        // セルに収める）。ほかの op は git がエディタを起こさないはずで、キーを名指ししない。
        ExecErrorKind::EditorGone => Some(match op {
            GitOp::RebaseInteractive { .. } => keys::spell(
                state,
                [REBASE],
                |[rebase]| format!("mewgit が消えた。起動し直して {rebase} から開き直して"),
                "mewgit が消えた。起動し直して開き直して",
            ),
            GitOp::RebaseContinue { .. } | GitOp::RebaseSkip => keys::spell(
                state,
                [MENU],
                |[menu]| format!("mewgit が消えた。起動し直して {menu} から続けて"),
                "mewgit が消えた。起動し直して続けて",
            ),
            _ => "mewgit が消えた。起動し直してから押し直して".into(),
        }),
        ExecErrorKind::Other => None,
    }
}

/// 未解消のファイルで断られたときの案内（[`ExecErrorKind::Unmerged`]。01 の「失敗の
/// 案内文」）。**入口で git の文言が割れても同じ状態なので、案内を揃える** —— rebase の
/// continue は stdout に、cherry-pick / revert の continue は stderr に理由を書く。
///
/// 網羅 `match` にしているのは [`conflict_advice`] と同じ理由 —— 書き込みを足したときに、
/// 未解消のファイルで git が断る操作かを選ばせる。**git が別の文言で断る op**（`switch` の
/// `you need to resolve your current index first` など）は `Unmerged` にならないので `None`。
fn unmerged_advice(op: &GitOp, state: &AppState) -> Option<String> {
    match op {
        GitOp::RebaseContinue { .. } | GitOp::CherryPickContinue | GitOp::RevertContinue => {
            Some(keys::spell(
                state,
                [MENU],
                |[menu]| format!("未解消のまま。git add したら {menu} から continue"),
                "未解消のまま。git add したら continue",
            ))
        }
        GitOp::Commit { .. } => Some("未解消のまま。git add してから commit".into()),
        // 鮮度のある snapshot なら手前で断る（`refuse_by_held_values`）。ここに来るのは
        // 外で作った unmerged を読む前だけなので、同じ文で断る。
        GitOp::StashPush { .. } => Some(super::stash::UNMERGED.into()),
        GitOp::StashApply(_) | GitOp::StashPop(_) => {
            Some("コンフリクトを解消してから stash を戻して".into())
        }
        GitOp::Stage(_)
        | GitOp::Unstage(_)
        | GitOp::StageHunk(_)
        | GitOp::UnstageHunk(_)
        | GitOp::DiscardHunk(_)
        | GitOp::Discard(_)
        | GitOp::DiscardUntracked(_)
        | GitOp::DiscardAdded(_)
        | GitOp::BranchCreate { .. }
        | GitOp::Switch(_)
        | GitOp::BranchDelete { .. }
        | GitOp::BranchRename { .. }
        | GitOp::Fetch { .. }
        | GitOp::Pull { .. }
        | GitOp::Push { .. }
        | GitOp::RebaseInteractive { .. }
        | GitOp::RebaseSkip
        | GitOp::RebaseAbort
        | GitOp::CherryPick(_)
        | GitOp::CherryPickSkip
        | GitOp::CherryPickAbort
        | GitOp::Revert { .. }
        | GitOp::RevertSkip
        | GitOp::RevertAbort
        | GitOp::MergeAbort
        | GitOp::StashDrop(_)
        | GitOp::SetAccount { .. }
        | GitOp::UnsetAccount
        | GitOp::CheckoutPr(_) => None,
    }
}

/// コンフリクトで止まったときの案内。**続きが continue の操作だけ**に出す。
///
/// merge のコンフリクト（M5 で merge を始める経路が入ったとき）を結ぶのは continue では
/// なく commit なので、同じ文を出すと嘘になる（01 の「失敗の案内文」）。
/// **「`git add` したら」** —— Conflicted の行は mewgit では stage できない（01 の
/// 「ワーキングツリー」）ので、解消の印は端末で付ける。
///
/// 網羅 `match` にしているのは、`GitOp` に変種を足したときに「コンフリクトで止まった
/// あと continue で結ぶ操作か」を選ばせるため（`pull --rebase` を受け皿に落として
/// いた）。
fn conflict_advice(op: &GitOp, state: &AppState) -> Option<String> {
    match op {
        GitOp::RebaseInteractive { .. }
        | GitOp::RebaseContinue { .. }
        | GitOp::RebaseSkip
        | GitOp::Pull {
            mode: PullMode::Rebase,
        }
        | GitOp::CherryPick(_)
        | GitOp::CherryPickContinue
        | GitOp::CherryPickSkip
        | GitOp::Revert { .. }
        | GitOp::RevertContinue
        | GitOp::RevertSkip => Some(keys::spell(
            state,
            [MENU],
            |[menu]| format!("コンフリクト。git add したら {menu} から continue"),
            "コンフリクト。git add したら continue",
        )),
        // **進行中操作にはならない**（continue が無い）ので `m` は案内しない。pop は
        // コンフリクトすると stash を残す（git の `The stash entry is kept`。01 の
        // 「stash の書き込み」）。
        GitOp::StashApply(_) => Some("コンフリクト。解消して git add".into()),
        GitOp::StashPop(_) => Some("コンフリクト。stash は残してある。解消して git add".into()),
        // merge のコンフリクトを結ぶのは commit。
        GitOp::Pull {
            mode: PullMode::Merge,
        } => None,
        // コンフリクトで止まる経路が無い（ff-only と、index / ref だけを触る書き込み）。
        GitOp::Pull {
            mode: PullMode::FastForwardOnly,
        }
        | GitOp::Stage(_)
        | GitOp::Unstage(_)
        | GitOp::StageHunk(_)
        | GitOp::UnstageHunk(_)
        | GitOp::DiscardHunk(_)
        | GitOp::Discard(_)
        | GitOp::DiscardUntracked(_)
        | GitOp::DiscardAdded(_)
        | GitOp::Commit { .. }
        | GitOp::BranchCreate { .. }
        | GitOp::Switch(_)
        | GitOp::BranchDelete { .. }
        | GitOp::BranchRename { .. }
        | GitOp::Fetch { .. }
        | GitOp::Push { .. }
        | GitOp::RebaseAbort
        | GitOp::CherryPickAbort
        | GitOp::RevertAbort
        | GitOp::MergeAbort
        | GitOp::StashPush { .. }
        | GitOp::StashDrop(_)
        | GitOp::SetAccount { .. }
        | GitOp::UnsetAccount
        | GitOp::CheckoutPr(_) => None,
    }
}

/// cherry-pick の変更が空で止まったときの案内。**continue は同じ理由で断られる**ので、
/// 出口は skip（1 本なら abort と同じ結果。03 の「continue / skip / abort の終わり方」）。
///
/// **案内するのは cherry-pick の系統だけ。** revert が空で止まった形は stderr が空で、
/// `Empty` ではなく `NoChanges` に落ちる（[`no_changes_advice`]）。rebase で空の pick に当たったときの出力は確かめていないので、ほかの op で
/// 出たら、案内の無い `Display` に落ちるほうが嘘を言わない。
///
/// 網羅 `match` にしているのは [`conflict_advice`] と同じ理由 —— revert の skip などを
/// 足したときに、空で止まったあと skip で抜ける操作かを選ばせる。
fn empty_advice(op: &GitOp, state: &AppState) -> Option<String> {
    match op {
        GitOp::CherryPick(_) | GitOp::CherryPickContinue | GitOp::CherryPickSkip => {
            Some(empty_stop(state))
        }
        GitOp::Stage(_)
        | GitOp::Unstage(_)
        | GitOp::StageHunk(_)
        | GitOp::UnstageHunk(_)
        | GitOp::DiscardHunk(_)
        | GitOp::Discard(_)
        | GitOp::DiscardUntracked(_)
        | GitOp::DiscardAdded(_)
        | GitOp::Commit { .. }
        | GitOp::BranchCreate { .. }
        | GitOp::Switch(_)
        | GitOp::BranchDelete { .. }
        | GitOp::BranchRename { .. }
        | GitOp::Fetch { .. }
        | GitOp::Pull { .. }
        | GitOp::Push { .. }
        | GitOp::RebaseInteractive { .. }
        | GitOp::RebaseContinue { .. }
        | GitOp::RebaseSkip
        | GitOp::RebaseAbort
        | GitOp::CherryPickAbort
        | GitOp::Revert { .. }
        | GitOp::RevertContinue
        | GitOp::RevertSkip
        | GitOp::RevertAbort
        | GitOp::MergeAbort
        | GitOp::StashPush { .. }
        | GitOp::StashApply(_)
        | GitOp::StashPop(_)
        | GitOp::StashDrop(_)
        | GitOp::SetAccount { .. }
        | GitOp::UnsetAccount
        | GitOp::CheckoutPr(_) => None,
    }
}

/// 積む変更が無かったときの案内（[`ExecErrorKind::NoChanges`]。03 の「revert の開始」）。
///
/// **同じ文言で、進行中操作が残るかが op で割れる。** 1 本だけの revert の開始では何も
/// 残らない（「取り消し済み」）。continue / skip では `REVERT_HEAD` か `sequencer/` が残り、
/// continue は同じ理由で断られるので出口は skip。**複数の revert の開始も途中が空だと
/// `sequencer/` を残して止まる**（continue でも skip でも抜けられるが、案内は skip に
/// 揃える。03 の「複数のコミットの cherry-pick / revert」）。**commit も同じ文言を出す**が、
/// 案内は付けない（stderr が空なので、要約行に stdout の結びがそのまま出る。[`exec_summary`]）。
///
/// 網羅 `match` にしているのは [`conflict_advice`] と同じ理由。
fn no_changes_advice(op: &GitOp, state: &AppState) -> Option<String> {
    match op {
        GitOp::Revert { oids, .. } if oids.len() == 1 => {
            Some("取り消す変更がありません（取り消し済み）".into())
        }
        // 解消した結果が空で continue が断られた（`REVERT_HEAD` は残る）か、複数の途中が
        // 空で止まった（`sequencer/` だけが残る）。
        GitOp::Revert { .. } | GitOp::RevertContinue | GitOp::RevertSkip => Some(empty_stop(state)),
        GitOp::Stage(_)
        | GitOp::Unstage(_)
        | GitOp::StageHunk(_)
        | GitOp::UnstageHunk(_)
        | GitOp::DiscardHunk(_)
        | GitOp::Discard(_)
        | GitOp::DiscardUntracked(_)
        | GitOp::DiscardAdded(_)
        | GitOp::Commit { .. }
        | GitOp::BranchCreate { .. }
        | GitOp::Switch(_)
        | GitOp::BranchDelete { .. }
        | GitOp::BranchRename { .. }
        | GitOp::Fetch { .. }
        | GitOp::Pull { .. }
        | GitOp::Push { .. }
        | GitOp::RebaseInteractive { .. }
        | GitOp::RebaseContinue { .. }
        | GitOp::RebaseSkip
        | GitOp::RebaseAbort
        | GitOp::CherryPick(_)
        | GitOp::CherryPickContinue
        | GitOp::CherryPickSkip
        | GitOp::CherryPickAbort
        | GitOp::RevertAbort
        | GitOp::MergeAbort
        | GitOp::StashPush { .. }
        | GitOp::StashApply(_)
        | GitOp::StashPop(_)
        | GitOp::StashDrop(_)
        | GitOp::SetAccount { .. }
        | GitOp::UnsetAccount
        | GitOp::CheckoutPr(_) => None,
    }
}

/// 非 FF で断られたときの案内。**押すキーが op で違う**（01 の「失敗の案内文」）。
/// 素の push は op だけでは決まらず、押したブランチの行から解いた `pushed` で分ける。
fn non_fast_forward_advice(
    op: &GitOp,
    pushed: Option<PlainPush>,
    state: &AppState,
) -> Option<String> {
    match op {
        // force push が lease に断られた形（`stale info`）。**これは守りが効いている** ——
        // 他人が push した印で、`f` で取ってもその先端はこのブランチの reflog から辿り
        // 着けないので、取り込むまで `P` は force に上がらない（01 の「force push」）。
        // `p` は `--ff-only` で分かれた履歴を取り込めないので、外の git へ送る。元は
        // 「`f` のあと `P` を 2 回」と案内していたが、それは他人の push を消す手順だった。
        GitOp::Push { lease: Some(_), .. } => {
            Some(keys::spell(
                state,
                [FETCH, PUSH],
                |[fetch, push]| {
                    format!("リモートが動いています。{fetch} で取得し、外の git で取り込んでから {push}")
                },
                "リモートが動いています。外の git で取り込んでから push",
            ))
        }
        // `-u` の提案から入った形。force に上げられるのは `PushPlan::Tracked` だけ
        // （lease の値が無い）で、`p` も追跡設定が無いと git がどのブランチを取るか
        // 決められない。**mewgit に残る出口は改名だけ。**
        //
        // **キーではなくペインの名前を出す。** `r` が効くのは Branches ペインの中
        // だけで（07 のキー表）、既定のフォーカスである Graph で押しても何も
        // 起きない。しかも `r` はカーソル行を改名するので、HEAD とは限らない。
        GitOp::Push {
            set_upstream: true, ..
        } => Some("追跡先が無いため force push できません。Branches で改名してください".into()),
        // 追跡設定に沿った push。**次の `P` が force の確認を出すかは書かない** ——
        // それを決めるのは `push_plan`（追跡 ref の先端を取り込んでいないときと、追跡 ref が
        // 無いときに断る）で、ここで先読みすると同じ判定が 2 箇所に割れる。断られても
        // 出るのは `PushPlan::Refused` の正しい文言なので、この案内は嘘にならない。
        // **「リモートが進んでいます」とは言わない** —— 巻き戻したブランチの 1 回目の
        // `P` もここに来る（断られる前は force にしない）。リモートは進んでおらず、`p` を
        // 押すと捨てたコミットが戻る（01 の「失敗の案内文」）。
        GitOp::Push { .. } => {
            Some(match pushed {
                // **追跡先ではない push 先が進んでいた形。mewgit に出口が無い。** `p` は
                // 追跡先から取り込み（push 先の分岐は解けない）、force は lease が追跡先に
                // 対する値なので撃たない（01 の「push の行き先」の 2）。
                Some(PlainPush::Elsewhere) => keys::spell(
                    state,
                    [PUSH],
                    |[push]| {
                        format!("push 先が進んでいます。外の git で取り込んでから {push} を押してください")
                    },
                    "push 先が進んでいます。外の git で取り込んでから push してください",
                ),
                // **送るコミットがある（分かれた）ブランチには `p` を挙げない** ——
                // `pull --ff-only` が断る。追跡 ref が無くて比べられない行も同じ側
                // （`remote::pull_fast_forwards`）。
                Some(PlainPush::Upstream {
                    fast_forwards: false,
                }) => keys::spell(
                    state,
                    [PUSH],
                    |[push]| {
                        format!("リモートと分かれています。外の git で取り込むか、上書きは {push} をもう一度")
                    },
                    DIVERGED,
                ),
                // lease も `-u` も無い push なので `None` は来ない（`remote::plain_push`）。
                Some(PlainPush::Upstream {
                    fast_forwards: true,
                })
                | Some(PlainPush::Unlisted)
                | None => keys::spell(
                    state,
                    [PULL, PUSH],
                    |[pull, push]| {
                        format!("リモートと分かれています。{pull} で取り込むか、上書きは {push} をもう一度")
                    },
                    DIVERGED,
                ),
            })
        }
        // `pull --ff-only` が履歴の分かれたブランチを断った形。merge / rebase は
        // M5 なので、mewgit の中に踏める手が無いことをそのまま言う。
        GitOp::Pull { .. } => {
            Some("履歴が分かれています。merge / rebase はまだ実装していません".into())
        }
        // PR の取り出しで、**ローカルに在った同名のブランチが PR と分かれていた**形。
        // 切り替えたあとの `merge --ff-only` が断る（05）ので、**そのブランチには
        // 居る** —— 「取り出せません」と書くと切り替わっていないように読める。
        // **mewgit の中に踏める手は無い** —— 上書きを選ばせる経路は作っていない
        // （ローカルにしか無いコミットを消す操作になる）。
        GitOp::CheckoutPr(_) => {
            Some("ローカルのブランチが PR と分かれているため早送りできません".into())
        }
        // fetch は remote-tracking ref を `+` 付きで更新するので非 FF で断らない
        // （git 2.55 で確認）。ほかの op も非 FF を出す経路が無い。
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        cells, commit, commit_op, exec_done, exec_done_of, failed_with, fake_output, finish_exec,
        forced_push_op, issued, page_loaded, press, push_op, refs_and_log, reject_non_fast_forward,
        reload, state, state_for_push, state_with_commits, state_with_page, tracked_head,
        worktree_state, BESIDE_BADGE_CELLS, HEADS_MAIN, NOTICE_CELLS,
    };
    use super::super::update;
    use super::*;
    use crate::app::{Msg, Pane};

    use crate::git_exec::{ExecErrorKind, HunkApply, LocalBranch, PrSource, PullMode, RebasePlan};
    use crate::git_read::{Branch, GitReadError, LogPage, WorktreeSection};
    use crate::test_util::{
        fake_detail, fake_oid, fake_pr_checkout, fake_repo_snapshot, fake_snapshot, fake_stamp,
        keymap_with, keymap_without, status_arrived,
    };

    #[test]
    fn a_failed_exec_keeps_the_argv_and_stderr() {
        // Git CLI のエラーは握りつぶさない（不変ルール 4）。
        let mut state = state();
        let _ = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));

        let cmd = update(
            &mut state,
            exec_done(Err(ExecError::Failed {
                argv: vec!["add".into(), "--".into(), "a.txt".into()],
                code: Some(128),
                stdout: String::new(),
                stderr: "fatal: pathspec 'a.txt' did not match any files".to_string(),
                kind: ExecErrorKind::Other,
            })),
        );

        // 失敗しても index が途中まで動いていることがあるので読み直す。
        assert_eq!(cmd, Cmd::LoadStatus);
        let notice = state.notices.back().expect("通知がありません");
        assert_eq!(notice.level, NoticeLevel::Error);
        let detail = notice.detail.as_deref().expect("detail がありません");
        assert!(detail.contains("git add -- a.txt"), "detail: {detail}");
        assert!(
            detail.contains("did not match any files"),
            "detail: {detail}"
        );
        // 案内の無い kind では、要約行は stderr の理由の行（01 の「失敗の案内文」）。
        assert_eq!(notice.message, "pathspec 'a.txt' did not match any files");
    }

    #[test]
    fn a_stale_hunk_warns_with_the_path_in_the_detail_and_reloads() {
        // git は起動していない。何も壊れていないので `Warn`。**パスは要約行に入れない**
        // （上限が無く、案内を押し出す）。読み直しは他の失敗と同じく出る ——
        // 届いた status と diff で押し直せる。
        let mut state = state();
        let op = GitOp::StageHunk(Box::new(HunkApply {
            path: "dir/a.txt".into(),
            patch: Vec::new(),
            expect: fake_stamp(),
        }));
        let _ = state.issue(Cmd::Exec(op.clone()));

        let cmd = update(
            &mut state,
            Msg::ExecFinished(Box::new(ExecDone {
                op,
                result: Err(ExecError::Stale {
                    argv: vec!["apply".into(), "--cached".into()],
                    path: "dir/a.txt".into(),
                }),
            })),
        );

        assert_eq!(cmd, Cmd::LoadStatus);
        let notice = state.notices.back().expect("通知がありません");
        assert_eq!(notice.level, NoticeLevel::Warn);
        assert!(notice.message.contains("押し直して"), "{}", notice.message);
        assert!(!notice.message.contains("dir/a.txt"), "{}", notice.message);
        let detail = notice.detail.as_deref().expect("detail がありません");
        assert!(detail.contains("git apply --cached"), "detail: {detail}");
        assert!(detail.contains("dir/a.txt"), "detail: {detail}");
    }

    #[test]
    fn a_failure_puts_the_next_key_on_the_summary_line() {
        // **ステータスバーが描くのは要約行の 1 行目だけ**（07）。ここが argv だと、
        // 「次に何を押すか」が画面に出ない（01 の「失敗の案内文」）。
        let tracked = push_op("origin", "main", false);
        let cases: [(GitOp, ExecErrorKind, &str); 8] = [
            (tracked.clone(), ExecErrorKind::SamlSsoRequired, "Authorize"),
            (tracked.clone(), ExecErrorKind::SshAuthFailed, "ssh-agent"),
            (
                tracked.clone(),
                ExecErrorKind::HttpsAuthFailed,
                "gh auth setup-git",
            ),
            // **同じ kind でも op で押すキーが違う。** kind だけで選ぶとどれかに嘘を言う。
            (
                tracked.clone(),
                ExecErrorKind::NonFastForward,
                "上書きは P をもう一度",
            ),
            (
                push_op("origin", "main", true),
                ExecErrorKind::NonFastForward,
                "Branches で改名してください",
            ),
            (
                forced_push_op(fake_oid(7)),
                ExecErrorKind::NonFastForward,
                "外の git で取り込んでから P",
            ),
            (
                GitOp::Pull {
                    mode: PullMode::FastForwardOnly,
                },
                ExecErrorKind::NonFastForward,
                "merge / rebase はまだ実装していません",
            ),
            // **受け皿の `_` に落ちていた腕。** 取り出しは分かれたブランチを上書きしない
            // ので、何が起きたかを言う（05）。
            (
                fake_pr_checkout(PrSource::SameRepo, LocalBranch::Absent),
                ExecErrorKind::NonFastForward,
                "PR と分かれているため早送りできません",
            ),
        ];

        for (op, kind, expected) in cases {
            let mut state = state();
            finish_exec(&mut state, op.clone(), Err(failed_with(kind)));

            let notice = state.notices.back().expect("通知がありません");
            assert!(
                notice.message.contains(expected),
                "{op:?} / {kind:?}: {}",
                notice.message
            );
            // **案内に差し替えても証拠は捨てない**（不変ルール 4）。argv は `detail`
            // に残り、`L` の履歴から読める。
            let detail = notice.detail.as_deref().expect("detail がありません");
            assert!(detail.contains("git push origin main"), "detail: {detail}");
            // **1 行に収まる幅か。** 超えると末尾が `…` に切れ、「どうすればよいか」
            // の側が消える（01 の「失敗の案内文」）。予算の根拠は `mewgit-tui` の
            // `a_notice_of_the_full_width_fits_on_its_own_line` が見る。
            assert!(
                cells(&notice.message) <= NOTICE_CELLS,
                "{} セル: {}",
                cells(&notice.message),
                notice.message
            );
        }
    }

    #[test]
    fn the_advice_spells_the_keys_from_the_table() {
        // **案内文のキーは表から引く**（08 の「文言に書いたキーの字」）。字を直書きした
        // ままだと、上書きした人に押しても何も起きないキーを案内する。**既定の表だけで
        // 測ると直書きでも緑**なので、上書きした表とキーの引けない表でも回す。
        let reword = crate::git_read::RefusedReword { head: fake_oid(7) };
        let rebase = GitOp::RebaseInteractive {
            onto: fake_oid(1).to_string(),
            head: fake_oid(7),
            plan: RebasePlan::default(),
        };
        let push = push_op("origin", "main", false);
        let upstream = |fast_forwards| Some(PlainPush::Upstream { fast_forwards });
        // (op, 種別, 押したブランチ, 既定の表での句, 上書きした表での句)
        let cases = [
            (
                rebase.clone(),
                ExecErrorKind::HeadMoved,
                None,
                "i から開き直して",
                "F8 から開き直して",
            ),
            (
                GitOp::RebaseContinue {
                    reword: Some(reword),
                },
                ExecErrorKind::HeadMoved,
                None,
                "m から開き直して",
                "F9 から開き直して",
            ),
            (
                rebase,
                ExecErrorKind::EditorGone,
                None,
                "i から開き直して",
                "F8 から開き直して",
            ),
            (
                GitOp::RebaseSkip,
                ExecErrorKind::EditorGone,
                None,
                "m から続けて",
                "F9 から続けて",
            ),
            (
                GitOp::RebaseContinue { reword: None },
                ExecErrorKind::Unmerged,
                None,
                "m から continue",
                "F9 から continue",
            ),
            (
                GitOp::CherryPick(vec![fake_oid(1)]),
                ExecErrorKind::Conflict,
                None,
                "m から continue",
                "F9 から continue",
            ),
            (
                GitOp::CherryPick(vec![fake_oid(1)]),
                ExecErrorKind::Empty,
                None,
                "m から skip",
                "F9 から skip",
            ),
            (
                GitOp::RevertContinue,
                ExecErrorKind::NoChanges,
                None,
                "m から skip",
                "F9 から skip",
            ),
            (
                forced_push_op(fake_oid(9)),
                ExecErrorKind::NonFastForward,
                None,
                "f で取得し、外の git で取り込んでから P",
                "F7 で取得し、外の git で取り込んでから F5",
            ),
            (
                push.clone(),
                ExecErrorKind::NonFastForward,
                Some(PlainPush::Elsewhere),
                "取り込んでから P を押して",
                "取り込んでから F5 を押して",
            ),
            (
                push.clone(),
                ExecErrorKind::NonFastForward,
                upstream(false),
                "上書きは P をもう一度",
                "上書きは F5 をもう一度",
            ),
            (
                push,
                ExecErrorKind::NonFastForward,
                upstream(true),
                "p で取り込むか、上書きは P をもう一度",
                "F6 で取り込むか、上書きは F5 をもう一度",
            ),
        ];
        let keymap = keymap_with(&[
            ("progress_menu", "f9"),
            ("rebase", "f8"),
            ("fetch", "f7"),
            ("pull", "f6"),
            ("push", "f5"),
            ("notice_log", "f4"),
        ]);
        let with = |keymap| AppState { keymap, ..state() };
        let defaults = state();
        let moved = with(keymap.clone());
        let lost = with(keymap_without(&[
            "progress_menu",
            "rebase",
            "fetch",
            "pull",
            "push",
            "notice_log",
        ]));
        // **ペインの動作はそのペインの表から引く**（`keys::key`）。`i` を押して Worktree へ
        // 移ったあとに失敗が届いても、Graph の `i` を案内する。
        let away = AppState {
            focus: Pane::Worktree,
            ..with(keymap.clone())
        };
        for (op, kind, pushed, default, overridden) in cases {
            let err = failed_with(kind);
            let advise =
                |state: &AppState| exec_advice(&op, &err, pushed, state).expect("案内がありません");
            let label = format!("{op:?} / {kind:?}");
            let shipped = advise(&defaults);
            assert!(shipped.contains(default), "{label}: {shipped}");
            assert!(
                advise(&moved).contains(overridden),
                "{label}: {}",
                advise(&moved)
            );
            assert!(
                advise(&away).contains(overridden),
                "{label}: {}",
                advise(&away)
            );
            // **キーを書かない形はキーの句を持たず、既定の形より長くならない**（予算は
            // 既定の形が見ている）。
            let keyless = advise(&lost);
            assert!(
                !keyless.contains("F12") && !keyless.contains(default),
                "{label}: {keyless}"
            );
            assert!(cells(&keyless) <= cells(&shipped), "{label}: {keyless}");
        }

        assert_eq!(
            unshaped_failure(&moved),
            "git が断りました。理由は F4 の履歴で"
        );
        assert_eq!(
            unshaped_failure(&lost),
            "git が断りました。理由は通知の履歴で"
        );

        // **配線も踏む** —— `exec_finished` が `AppState.keymap` から引くか。
        let head = tracked_head("main", "origin", HEADS_MAIN, Some(fake_oid(9)));
        let mut state = state_for_push(&["origin"], head);
        state.keymap = keymap;
        reject_non_fast_forward(&mut state, push_op("origin", "main", false));
        let notice = state.notices.back().expect("通知がありません");
        assert!(
            notice.message.contains("上書きは F5 をもう一度"),
            "{}",
            notice.message
        );
    }

    #[test]
    fn a_rejected_push_of_a_diverged_branch_does_not_offer_p() {
        // `p` は `pull --ff-only` なので、送るコミットがあるブランチでは断られる（01 の
        // 「失敗の案内文」）。**読むのは押したブランチ（`src`）の行** —— 一覧に無い
        // `gone` を押した形では、HEAD の行（ahead 1）を読むと `p` が消える。
        let diverged = tracked_head("main", "origin", HEADS_MAIN, Some(fake_oid(9)));
        let rewound = Branch {
            ahead: 0,
            ..diverged.clone()
        };
        // 追跡 ref が `fetch --prune` で消えた行。ahead は比べる相手がいない 0 で、送る
        // コミットがあっても `p` は断られる（`/code-review` が一時リポジトリで再現）。
        let pruned = Branch {
            upstream: None,
            ahead: 0,
            ..diverged.clone()
        };
        let plain = push_op("origin", "main", false);
        let cases = [
            (diverged.clone(), plain.clone(), Some(false)),
            // 巻き戻した形。`p` を押すと捨てたコミットが戻ることも含めて両方を挙げる。
            (rewound, plain.clone(), Some(true)),
            (pruned, plain, Some(false)),
            (
                diverged.clone(),
                push_op("origin", "gone", false),
                Some(true),
            ),
            // **force push の断りは lease の案内のまま** —— ahead ≥ 1 の行でも、分かれた
            // ブランチの文にすり替えない（`/code-review` の変異で見張りが無かった）。
            (diverged, forced_push_op(fake_oid(9)), None),
        ];

        for (head, op, offers_p) in cases {
            let ahead = head.ahead;
            let mut state = state_for_push(&["origin"], head);
            reject_non_fast_forward(&mut state, op.clone());

            let notice = state.notices.back().expect("通知がありません");
            let label = format!("ahead {ahead} / {op:?}: {}", notice.message);
            match offers_p {
                Some(offers_p) => {
                    assert_eq!(notice.message.contains("p で取り込む"), offers_p, "{label}");
                    assert!(notice.message.contains("P をもう一度"), "{label}");
                }
                None => assert!(
                    notice.message.contains("外の git で取り込んでから P"),
                    "{label}"
                ),
            }
            assert!(cells(&notice.message) <= NOTICE_CELLS, "{label}");
        }
    }

    #[test]
    fn stopping_at_the_next_conflict_or_an_empty_pick_is_a_warning_with_the_way_on() {
        // continue / skip のあとに次のコミットでまた止まるのは想定どおりの結末で、
        // 進行中操作は残る（03 の「continue / skip / abort の終わり方」）。赤で出すと
        // 壊れたように読める。**変更が空で止まった pick も同じ**で、continue は同じ理由で
        // 断られるので出口は skip。
        let conflict = [
            GitOp::RebaseContinue { reword: None },
            GitOp::RebaseSkip,
            GitOp::CherryPickContinue,
            GitOp::CherryPickSkip,
            GitOp::RevertContinue,
            GitOp::RevertSkip,
            // 止まると進行中操作は `Rebase` になり、`m` から continue できる。
            GitOp::Pull {
                mode: PullMode::Rebase,
            },
        ]
        .map(|op| (op, ExecErrorKind::Conflict, "m から continue"));
        let empty = [
            GitOp::CherryPick(vec![fake_oid(1)]),
            GitOp::CherryPick(vec![fake_oid(1), fake_oid(2)]),
            GitOp::CherryPickContinue,
            GitOp::CherryPickSkip,
        ]
        .map(|op| (op, ExecErrorKind::Empty, "m から skip"));
        // revert は空で止まっても stderr が空で、`NoChanges` に落ちる（03 の「revert の開始」）。
        // 複数の開始は途中が空だと `sequencer/` を残して止まる。
        let several = GitOp::Revert {
            oids: vec![fake_oid(1), fake_oid(2)],
            mainline: None,
        };
        let no_changes = [GitOp::RevertContinue, GitOp::RevertSkip, several]
            .map(|op| (op, ExecErrorKind::NoChanges, "m から skip"));
        // 未解消のまま押した形（03 の「エラー分類」）。rebase は stdout に、cherry-pick /
        // revert は stderr に理由を書くが、案内は揃える。stash と commit は進行中操作の
        // 途中（merge の途中の commit など）でも押せるので、同じ幅に収める。
        let unmerged = [
            (GitOp::RebaseContinue { reword: None }, "m から continue"),
            (GitOp::CherryPickContinue, "m から continue"),
            (GitOp::RevertContinue, "m から continue"),
            (
                GitOp::Commit {
                    message: "m".to_string(),
                    amend: false,
                },
                "git add してから commit",
            ),
            (
                GitOp::StashPush {
                    message: None,
                    include_untracked: true,
                    paths: None,
                },
                "解消してから stash して",
            ),
            (GitOp::StashApply(0), "解消してから stash を戻して"),
            (GitOp::StashPop(0), "解消してから stash を戻して"),
        ]
        .map(|(op, way)| (op, ExecErrorKind::Unmerged, way));
        for (op, kind, way) in conflict
            .into_iter()
            .chain(empty)
            .chain(no_changes)
            .chain(unmerged)
        {
            let mut state = state();
            finish_exec(&mut state, op.clone(), Err(failed_with(kind)));

            let notice = state.notices.back().expect("通知がありません");
            assert_eq!(notice.level, NoticeLevel::Warn, "{op:?} {kind:?}");
            assert!(notice.message.contains(way), "{op:?}: {}", notice.message);
            // **必ずバッジと並ぶ**ので、71 ではなくバッジの隣の幅に収める（`/code-review` が
            // `CHERRY-PICK` の隣で `m か…` まで削られるのを描いて拾った）。
            assert!(
                cells(&notice.message) <= BESIDE_BADGE_CELLS,
                "{} セル: {}",
                cells(&notice.message),
                notice.message
            );
        }
    }

    #[test]
    fn an_empty_stop_outside_cherry_pick_is_not_told_to_skip() {
        // 案内するのは cherry-pick の系統だけ。ほかで出たら `Display` に落とす。
        let mut state = state();
        finish_exec(
            &mut state,
            GitOp::RebaseContinue { reword: None },
            Err(failed_with(ExecErrorKind::Empty)),
        );
        let notice = state.notices.back().expect("通知がありません");
        assert_eq!(notice.level, NoticeLevel::Error);
        assert!(!notice.message.contains("skip"), "{}", notice.message);
    }

    #[test]
    fn a_moved_head_before_amending_a_refused_reword_is_a_warning_to_reopen_the_menu() {
        // 照合（`update-ref --stdin` の `verify`）が外れた形。rebase は止まったままなので
        // バッジと並ぶ（01 の「失敗の案内文」）。素の continue には出さない。
        let reword = crate::git_read::RefusedReword { head: fake_oid(7) };
        let mut plain = state();
        let mut state = state();
        finish_exec(
            &mut state,
            GitOp::RebaseContinue {
                reword: Some(reword),
            },
            Err(failed_with(ExecErrorKind::HeadMoved)),
        );
        let notice = state.notices.back().expect("通知がありません");
        assert_eq!(notice.level, NoticeLevel::Warn);
        assert!(
            notice.message.contains("m から開き直して"),
            "{}",
            notice.message
        );
        assert!(
            cells(&notice.message) <= BESIDE_BADGE_CELLS,
            "{} セル: {}",
            cells(&notice.message),
            notice.message
        );

        finish_exec(
            &mut plain,
            GitOp::RebaseContinue { reword: None },
            Err(failed_with(ExecErrorKind::HeadMoved)),
        );
        let notice = plain.notices.back().expect("通知がありません");
        assert!(!notice.message.contains("開き直して"), "{}", notice.message);
    }

    #[test]
    fn a_vanished_binary_is_an_error_that_says_to_restart_with_the_key_to_press() {
        // 起動中に mewgit のバイナリが消え、git がエディタを起こせなかった形（03 の
        // 「消えた自バイナリ」）。理由の行は `there was a problem with the editor ''<パス>'
        // --commit-editor'` で、71 セルで切れて「起動し直せば済む」がどこにも出なかった。
        let reword = crate::git_read::RefusedReword { head: fake_oid(7) };
        let cases = [
            (
                GitOp::RebaseInteractive {
                    onto: fake_oid(1).to_string(),
                    head: fake_oid(7),
                    plan: RebasePlan::default(),
                },
                "i から開き直して",
            ),
            (GitOp::RebaseContinue { reword: None }, "m から続けて"),
            (
                GitOp::RebaseContinue {
                    reword: Some(reword),
                },
                "m から続けて",
            ),
            (GitOp::RebaseSkip, "m から続けて"),
            (
                GitOp::Commit {
                    message: "m".to_string(),
                    amend: true,
                },
                "起動し直してから押し直して",
            ),
        ];
        for (op, way) in cases {
            let mut state = state();
            finish_exec(
                &mut state,
                op.clone(),
                Err(failed_with(ExecErrorKind::EditorGone)),
            );

            let notice = state.notices.back().expect("通知がありません");
            // 想定どおりの結末（コンフリクト）とは違い、起動し直すまで同じ失敗を繰り返す。
            assert_eq!(notice.level, NoticeLevel::Error, "{op:?}");
            assert!(notice.message.contains(way), "{op:?}: {}", notice.message);
            // continue / skip は rebase が止まったまま失敗するので、バッジと並ぶ。
            assert!(
                cells(&notice.message) <= BESIDE_BADGE_CELLS,
                "{} セル: {}",
                cells(&notice.message),
                notice.message
            );
        }
    }

    #[test]
    fn a_moved_head_before_an_interactive_rebase_is_a_warning_to_reopen() {
        // sequence editor が断ると git は rebase を始めない（03）。汎用の赤い
        // 「git の実行に失敗しました」では理由が `L` の stderr にしか出なかった。
        let mut state = state();
        let op = GitOp::RebaseInteractive {
            onto: fake_oid(1).to_string(),
            head: fake_oid(7),
            plan: RebasePlan::default(),
        };
        finish_exec(&mut state, op, Err(failed_with(ExecErrorKind::HeadMoved)));

        let notice = state.notices.back().expect("通知がありません");
        assert_eq!(notice.level, NoticeLevel::Warn);
        assert!(
            notice.message.contains("i から開き直して"),
            "{}",
            notice.message
        );
        // **バッジと並ぶことがある** —— 端末で止めた cherry-pick が HEAD を動かしていると、
        // 失敗のあとの status でバッジが出る（`/code-review` が踏んだ。01）。
        assert!(
            cells(&notice.message) <= BESIDE_BADGE_CELLS,
            "{} セル: {}",
            cells(&notice.message),
            notice.message
        );
    }

    #[test]
    fn a_revert_with_nothing_to_undo_says_so_as_a_warning() {
        // stderr は空で、分けないと argv だけの赤い通知になる（03 の「revert の開始」）。
        let mut state = state();
        let op = GitOp::Revert {
            oids: vec![fake_oid(1)],
            mainline: None,
        };
        finish_exec(&mut state, op, Err(failed_with(ExecErrorKind::NoChanges)));

        let notice = state.notices.back().expect("通知がありません");
        assert_eq!(notice.level, NoticeLevel::Warn);
        assert!(
            notice.message.contains("取り消す変更"),
            "{}",
            notice.message
        );
    }

    #[test]
    fn an_unmerged_refusal_of_an_op_git_words_differently_stays_red() {
        // `Unmerged` の案内は、git が未解消で断ると数えた op だけ（`unmerged_advice`）。
        // `switch` は別の文言で断るので、分類が外れて来ても「解消してから」とは言わない。
        let mut state = state();
        finish_exec(
            &mut state,
            GitOp::Switch("topic".to_string()),
            Err(failed_with(ExecErrorKind::Unmerged)),
        );

        let notice = state.notices.back().expect("通知がありません");
        assert_eq!(notice.level, NoticeLevel::Error);
        assert!(!notice.message.contains("解消"), "{}", notice.message);
    }

    #[test]
    fn a_failure_with_an_empty_stderr_is_summed_up_by_the_last_line_of_stdout() {
        // 何も stage していない `git commit`（git 2.56.0 の出力）。stderr が空なので、
        // `Display` に落とすと argv だけの通知になる（01 の「失敗の案内文」）。
        let stdout = "On branch main\nChanges not staged for commit:\n\tmodified:   a\n\nno changes added to commit (use \"git add\" and/or \"git commit -a\")\n\n";
        let failed = |stderr: &str| ExecError::Failed {
            argv: vec!["commit".into(), "-m".into(), "m".into()],
            code: Some(1),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            kind: ExecErrorKind::NoChanges,
        };
        let commit = GitOp::Commit {
            message: "m".to_string(),
            amend: false,
        };

        let mut state = state();
        finish_exec(&mut state, commit.clone(), Err(failed("")));
        let notice = state.notices.back().expect("通知がありません");
        // 末尾の空行は飛ばす。
        assert_eq!(
            notice.message,
            "no changes added to commit (use \"git add\" and/or \"git commit -a\")"
        );
        assert_eq!(notice.level, NoticeLevel::Error);

        // **stderr があれば stdout は見ない。** 選べる行が無ければ固定の文言で `L` を案内する。
        finish_exec(&mut state, commit.clone(), Err(failed("hint: 何か\n")));
        let notice = state.notices.back().expect("通知がありません");
        assert_eq!(notice.message, "git が断りました。理由は L の履歴で");

        // 出力が両方とも空なら、`L` にも理由は無いので `Display`（argv）に落ちる。
        let mut empty = failed("");
        if let ExecError::Failed { stdout, .. } = &mut empty {
            stdout.clear();
        }
        finish_exec(&mut state, commit, Err(empty));
        let notice = state.notices.back().expect("通知がありません");
        assert_eq!(notice.message, "git の実行に失敗しました: git commit -m m");
    }

    #[test]
    fn the_detail_carries_stderr_before_stdout() {
        // stash を unmerged のまま押した形（git 2.56.0 の出力）。理由は stdout にしか無い
        // （03 の「エラー分類」）。
        let mut state = state();
        finish_exec(
            &mut state,
            GitOp::StashApply(0),
            Err(ExecError::Failed {
                argv: vec!["stash".into(), "apply".into(), "stash@{0}".into()],
                code: Some(128),
                stdout: "b: needs merge\n".to_string(),
                stderr: "error: could not write index\n".to_string(),
                kind: ExecErrorKind::Unmerged,
            }),
        );

        let notice = state.notices.back().expect("通知がありません");
        assert_eq!(
            notice.detail.as_deref(),
            Some("git stash apply stash@{0}\nerror: could not write index\nb: needs merge")
        );
    }

    #[test]
    fn a_conflict_that_continue_does_not_conclude_is_not_told_to_continue() {
        // merge のコンフリクトを結ぶのは commit で、continue ではない（01）。
        let mut state = state();
        finish_exec(
            &mut state,
            GitOp::Pull {
                mode: PullMode::Merge,
            },
            Err(failed_with(ExecErrorKind::Conflict)),
        );

        let notice = state.notices.back().expect("通知がありません");
        assert_eq!(notice.level, NoticeLevel::Error);
        assert!(!notice.message.contains("continue"), "{}", notice.message);
    }

    #[test]
    fn a_diverged_pull_does_not_arm_a_force_push() {
        // `pull --ff-only` の拒否も `NonFastForward` に落ちる（`classify` の 4 つ目）。
        // **憶える側は `GitOp::Push` で門番している**ので、ここで force の腕は立たない
        // （01 の「force push」）。立つと、`p` を押しただけで次の `P` が
        // 「リモートを上書きします」の確認になる。
        let mut state = state_for_push(
            &["origin"],
            tracked_head("main", "origin", HEADS_MAIN, Some(fake_oid(7))),
        );

        reject_non_fast_forward(
            &mut state,
            GitOp::Pull {
                mode: PullMode::FastForwardOnly,
            },
        );

        assert_eq!(state.push_rejected, None);
        assert_eq!(
            press(&mut state, 'P'),
            Cmd::Exec(push_op("origin", "main", false)),
            "force push の確認が出ています"
        );
    }

    #[test]
    fn a_status_dropped_after_a_write_is_asked_again_when_the_older_one_arrives() {
        // 走っている読み取りは書き込みより前に頼まれたものなので、届いても古い。
        let mut state = state();
        let _ = state.issue(Cmd::LoadStatus);
        let _ = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));

        let cmd = update(&mut state, exec_done(Ok(fake_output())));
        // 走っているので落とされる。
        assert_eq!(cmd, Cmd::None);

        // 書き込みより前に頼まれた一覧が届く。出しはするが、いまの世代ではない。
        let cmd = update(&mut state, status_arrived(Ok(fake_snapshot(&[], &[], &[]))));

        assert_eq!(cmd, Cmd::LoadStatus);
        assert!(!state.worktree_is_current());

        // 頼み直したぶんが届いて追いつく。
        let cmd = update(&mut state, status_arrived(Ok(fake_snapshot(&[], &[], &[]))));

        assert_eq!(cmd, Cmd::None);
        assert!(state.worktree_is_current());
    }

    #[test]
    fn a_status_that_fails_after_the_write_is_not_asked_again() {
        // **頼み直すかは届いた返信の刻印で決める。** 持っている一覧の刻印で決めると、
        // 失敗のあいだ「古い → 頼み直す → 失敗」を回す（01 の「失敗の記憶」）。
        let mut state = state();
        let _ = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));
        // 書き込みが `Cmd::LoadStatus` を出す（走っているものは無いので通る）。
        let _ = update(&mut state, exec_done(Ok(fake_output())));

        let cmd = update(&mut state, status_arrived(Err(GitReadError::ReaderGone)));

        assert_eq!(cmd, Cmd::None);
        // 一覧は古いままで、次の読み込みはユーザーの操作から始まる。
        assert!(!state.worktree_is_current());
    }

    #[test]
    fn a_refs_read_dropped_after_a_write_is_asked_again_when_the_older_one_arrives() {
        // `LoadRepoInfo`（ラベルとブランチ一覧）は書き込みのあとに必ず出るが、走って
        // いれば `issue` に落ちる。追いつきが無いと、`switch` のあともラベルとブランチ
        // 一覧が切り替え前のまま残る（01 の「実行中ジョブ › 世代」）。
        let op = GitOp::Switch("main".into());
        let mut state = state();
        let _ = state.issue(Cmd::LoadRepoInfo);
        let _ = state.issue(Cmd::Exec(op.clone()));

        let cmd = update(&mut state, exec_done_of(op, Ok(fake_output())));
        assert!(!issued(&cmd, &Cmd::LoadRepoInfo), "{cmd:?}");

        // 書き込みより前に頼まれた返信が届く。**採ったうえで頼み直す**（古い索引でも
        // ラベルは出せるので、届くまで前のものを出したままにする）。
        let cmd = update(
            &mut state,
            Msg::RepoInfoLoaded(Box::new(Ok(fake_repo_snapshot()))),
        );
        assert_eq!(cmd, Cmd::LoadRepoInfo);

        // 頼み直したぶんは書き込みより後に頼んだものなので、そこで止まる。
        let cmd = update(
            &mut state,
            Msg::RepoInfoLoaded(Box::new(Ok(fake_repo_snapshot()))),
        );
        assert_eq!(cmd, Cmd::None);
    }

    // ---- RepoInfo.target（01 の「`RepoInfo.target` を埋める」）----

    #[test]
    fn a_second_write_while_one_runs_is_dropped_with_a_notice() {
        // 黙って落とすと「押したのに何も起きない」になる。キューには積まない
        // （01 の「ランタイム」）。
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        assert!(matches!(press(&mut state, 's'), Cmd::Exec(_)));

        assert_eq!(press(&mut state, 's'), Cmd::None);

        let notice = state.notices.back().expect("通知がありません");
        assert_eq!(notice.level, NoticeLevel::Warn);
    }

    #[test]
    fn a_commit_reloads_the_log_from_the_top_and_the_refs() {
        // 続きを読む経路では直せない。新しいコミットは末尾ではなく先頭に付く
        // （01 の「ログの読み直し」）。ラベルは `LoadRepoInfo` が読み直す。
        let mut state = state_with_commits(3);
        let _ = state.issue(Cmd::Exec(commit_op()));

        let cmd = update(&mut state, exec_done_of(commit_op(), Ok(fake_output())));

        // **ブランチ一覧も読み直す。** commit は `refs/heads/<現在>` を動かすので、
        // 外すと Branches ペインが commit 前の ahead を出したまま残る。
        assert_eq!(cmd, refs_and_log(&state));
    }

    #[test]
    fn staging_does_not_reload_the_log() {
        // 読み直しは `LogSession` の作り直しで履歴全体のコストが掛かる（02）。
        // index しか触らない操作のたびには払えない。
        let mut state = state_with_commits(3);
        let _ = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));
        let before = state.jobs.epoch(Region::Log);

        let cmd = update(&mut state, exec_done(Ok(fake_output())));

        // **`Cmd::LoadStatus` 単体と厳密に比べる。** `issued` で「読み直しが無い」を
        // 見る形にすると、`LoadRepoInfo` が紛れ込んでも通る ——
        // index しか触らない操作では ref も動かないので、そちらも出てはいけない。
        assert_eq!(cmd, Cmd::LoadStatus);
        // **世代が進まないから積んだ行がそのまま使える。** 1 本のカウンタにすると
        // ここで進み、飛んでいるページも積んだ行も毎回捨てることになる。
        assert_eq!(state.jobs.epoch(Region::Log), before);
        // index とワークツリーのほうは古くなる。
        assert!(!state.worktree_is_current());
    }

    #[test]
    fn a_page_asked_for_before_the_reload_is_dropped_and_the_reload_is_asked_again() {
        // 続きの読み込みが走っていると、読み直しは `issue` に落ちる。落ちたぶんは
        // 印を付け、走っていたページが届いた時点で頼み直す（01）。
        // 1 ページが窓より短いので、`log_loaded` の `load_more` が続きを頼んだまま。
        let mut state = state_with_page(3, false);
        assert!(state.jobs.is_running(JobKind::LoadLog));
        let _ = state.issue(Cmd::Exec(commit_op()));

        let cmd = update(&mut state, exec_done_of(commit_op(), Ok(fake_output())));

        // 読み直しは落ちている。
        assert_eq!(
            cmd,
            Cmd::batch([Cmd::LoadStatus, Cmd::LoadRepoInfo, Cmd::LoadStashes])
        );

        // **走っていたページは積まない。** 積んでも直後の読み直しで捨てるだけで、
        // そのあいだ古い行に新しい行が継ぎ足された状態が画面に出る。
        let page = LogPage {
            commits: vec![commit(9)],
            done: false,
        };
        let cmd = update(&mut state, page_loaded(Ok(page)));

        assert_eq!(state.graph.lines.len(), 3);
        assert_eq!(cmd, reload(&state));

        // 頼み直したぶんは書き込みより後に頼んだページなので、届けば積める
        // （**借りが残らない**）。
        let page = LogPage {
            commits: vec![commit(9)],
            done: true,
        };
        let cmd = update(
            &mut state,
            Msg::LogLoaded {
                restart: true,
                result: Ok(page),
            },
        );

        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.graph.lines.len(), 1);
    }

    #[test]
    fn only_the_operations_that_change_the_walk_reload_the_log() {
        // ラベルと HEAD はグラフの行に焼き込まず `GraphState.refs` とブランチ一覧から引く
        // （02 の「refs とラベル」）ので、既にあるコミットを指すだけの操作は
        // `LoadRepoInfo` で足りる。読み直しは `LogSession` の作り直しで履歴全体のコストが
        // 掛かる（02）。
        assert_eq!(Reload::after(&commit_op(), true), Reload::RefsAndLog);
        assert_eq!(
            Reload::after(
                &GitOp::BranchDelete {
                    name: "x".into(),
                    force: true,
                },
                true
            ),
            Reload::RefsAndLog
        );

        assert_eq!(
            Reload::after(&GitOp::Switch("main".into()), true),
            Reload::Refs
        );
        assert_eq!(
            Reload::after(&push_op("origin", "main", false), true),
            Reload::Refs
        );
        // force push は `origin/x` が巻き戻り、そこからしか辿れないコミットが落ちる。
        assert_eq!(
            Reload::after(&forced_push_op(fake_oid(9)), true),
            Reload::RefsAndLog
        );

        assert_eq!(
            Reload::after(&GitOp::Stage(vec!["a.txt".into()]), true),
            Reload::Nothing
        );
        assert_eq!(
            Reload::after(
                &GitOp::StageHunk(Box::new(HunkApply {
                    path: "a.txt".into(),
                    patch: Vec::new(),
                    expect: fake_stamp(),
                })),
                true
            ),
            Reload::Nothing
        );
        // stash は `refs/stash` しか動かさず、グラフはそれを walk に押していない。
        // 一覧は読み直す（Stash ペイン）が、ラベルとログは読み直さない。
        assert_eq!(Reload::after(&GitOp::StashPop(0), true), Reload::Stashes);
        assert_eq!(Reload::after(&GitOp::StashPop(0), false), Reload::Stashes);
        // autostash を戻すところでコンフリクトすると `refs/stash` に積む。
        assert_eq!(Reload::after(&GitOp::MergeAbort, true), Reload::Stashes);
    }

    #[test]
    fn a_failed_write_reloads_the_log_only_when_it_could_have_moved_refs_anyway() {
        // 読み直しは `GraphState::restart` でカーソルとスクロールを先頭に戻す。
        // 1 回で決まる操作は、失敗したなら ref が動いていないので位置を捨てない。
        assert_eq!(Reload::after(&commit_op(), false), Reload::Refs);
        assert_eq!(
            Reload::after(&forced_push_op(fake_oid(9)), false),
            Reload::Refs
        );

        // fetch / pull は一部の ref だけ更新して落ちうるし、rebase 系は途中で止まる。
        assert_eq!(
            Reload::after(
                &GitOp::Fetch {
                    remote: None,
                    prune: false,
                },
                false
            ),
            Reload::RefsAndLog
        );
        assert_eq!(
            Reload::after(&GitOp::RebaseContinue { reword: None }, false),
            Reload::RefsAndLog
        );
        assert_eq!(
            Reload::after(&GitOp::RebaseAbort, false),
            Reload::RefsAndLog
        );
        // **1 本だけの cherry-pick は 1 回で決まる**（コンフリクトでも空でも HEAD は
        // 元のまま。03 の「cherry-pick の開始」の実測）。複数は途中まで積んでから止まる。
        assert_eq!(
            Reload::after(&GitOp::CherryPick(vec![fake_oid(1)]), false),
            Reload::Refs
        );
        assert_eq!(
            Reload::after(&GitOp::CherryPick(vec![fake_oid(1)]), true),
            Reload::RefsAndLog
        );
        assert_eq!(
            Reload::after(&GitOp::CherryPick(vec![fake_oid(1), fake_oid(2)]), false),
            Reload::RefsAndLog
        );
        // **1 本だけの revert も**（03 の「revert の開始」の実測）。mainline の有無で変わらない。
        for mainline in [None, Some(1)] {
            let one = GitOp::Revert {
                oids: vec![fake_oid(1)],
                mainline,
            };
            assert_eq!(Reload::after(&one, false), Reload::Refs, "{mainline:?}");
            assert_eq!(
                Reload::after(&one, true),
                Reload::RefsAndLog,
                "{mainline:?}"
            );
        }
        let many = GitOp::Revert {
            oids: vec![fake_oid(1), fake_oid(2)],
            mainline: None,
        };
        assert_eq!(Reload::after(&many, false), Reload::RefsAndLog);
        // PR の取り出しは手順の途中で止まっても前の手順の結果が残る（fetch が
        // ブランチを作ったあとで `switch` が断る。03 の「複数の手順を持つ書き込み」）。
        assert_eq!(
            Reload::after(
                &fake_pr_checkout(PrSource::SameRepo, LocalBranch::Absent),
                false
            ),
            Reload::RefsAndLog
        );
    }

    #[test]
    fn an_operation_that_only_moves_labels_does_not_reload_the_log() {
        let mut state = state_with_commits(3);
        let op = GitOp::Switch("main".into());
        let _ = state.issue(Cmd::Exec(op.clone()));
        let before = state.jobs.epoch(Region::Log);

        let cmd = update(&mut state, exec_done_of(op, Ok(fake_output())));

        // ラベル・HEAD・ブランチ一覧は ref から導かれるので読み直す。行は動かない。
        assert_eq!(cmd, Cmd::batch([Cmd::LoadStatus, Cmd::LoadRepoInfo]));
        assert_eq!(state.jobs.epoch(Region::Log), before);
    }

    #[test]
    fn a_second_write_while_the_reload_is_in_flight_is_not_lost() {
        // 1 回目のコミット。読み直しが飛ぶ。
        let mut state = state_with_commits(3);
        let _ = state.issue(Cmd::Exec(commit_op()));
        let _ = update(&mut state, exec_done_of(commit_op(), Ok(fake_output())));
        assert!(state.jobs.is_running(JobKind::LoadLog));

        // 2 回目。読み直しは飛んでいるぶんに落とされる。
        let _ = state.issue(Cmd::Exec(commit_op()));
        let cmd = update(&mut state, exec_done_of(commit_op(), Ok(fake_output())));
        assert!(
            !issued(&cmd, &reload(&state)),
            "飛んでいるぶんに落とされる: {cmd:?}"
        );

        // **1 回目の読み直しが届いても採らない。** 2 回目より前に頼まれたページなので、
        // 採ると 2 回目のコミットが二度とグラフに出ない。`restart` かどうかで捨てるかを
        // 決めると、ここで印だけ落ちて読み直しが止まる。
        let page = LogPage {
            commits: vec![commit(1)],
            done: true,
        };
        let cmd = update(
            &mut state,
            Msg::LogLoaded {
                restart: true,
                result: Ok(page),
            },
        );

        assert_eq!(cmd, reload(&state));
        assert_eq!(state.graph.lines.len(), 3);
    }

    #[test]
    fn a_reloaded_page_replaces_the_rows_and_seats_the_cursor_at_the_top() {
        let mut state = state_with_commits(3);
        state.graph.set_cursor(2);
        state.graph.detail = Some(fake_detail(2, "old"));
        let _ = state.issue(Cmd::Exec(commit_op()));
        let _ = update(&mut state, exec_done_of(commit_op(), Ok(fake_output())));

        let page = LogPage {
            commits: vec![commit(7)],
            done: true,
        };
        let _ = update(
            &mut state,
            Msg::LogLoaded {
                restart: true,
                result: Ok(page),
            },
        );

        assert_eq!(state.graph.lines.len(), 1);
        assert_eq!(state.graph.lines[0].commit.oid, commit(7).oid);
        assert_eq!(state.graph.cursor(), 0);
        // `lines` に無いコミットの詳細を残さない。
        assert!(state.graph.detail.is_none());
    }

    #[test]
    fn a_page_from_before_the_write_that_fails_is_not_asked_again() {
        // **古いページでも失敗を先に処理する。** 順序を逆にすると `load_failed` を
        // 立てないまま読み直しを出し、「失敗 → 再読込 → 失敗」を回す。
        let mut state = state_with_page(3, false);
        assert!(state.jobs.is_running(JobKind::LoadLog));
        let _ = state.issue(Cmd::Exec(commit_op()));
        let _ = update(&mut state, exec_done_of(commit_op(), Ok(fake_output())));

        let cmd = update(&mut state, page_loaded(Err(GitReadError::NonUtf8Head)));

        assert_eq!(cmd, Cmd::None);
        assert!(state.graph.load_failed);
        assert!(state.graph.lines.is_empty());
    }

    #[test]
    fn a_reload_that_fails_does_not_leave_the_log_waiting_for_one() {
        // 失敗のあとの読み込みはユーザーの操作から始まる。印を残すと、届かない
        // 読み直しを待ち続ける。
        let mut state = state_with_commits(3);
        let _ = state.issue(Cmd::Exec(commit_op()));
        let _ = update(&mut state, exec_done_of(commit_op(), Ok(fake_output())));

        let cmd = update(
            &mut state,
            Msg::LogLoaded {
                restart: true,
                result: Err(GitReadError::NonUtf8Head),
            },
        );

        assert_eq!(cmd, Cmd::None);
        assert!(state.graph.load_failed);
        assert!(state.graph.lines.is_empty());
    }

    #[test]
    fn a_canceled_exec_is_not_reported_as_an_error() {
        // 押した本人が止めたものを `Error` で出すと、頼んでいないことが壊れたように
        // 読める。ジョブは畳まれ、次の書き込みは通る。
        let mut state = state();
        let _ = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));

        let cmd = update(
            &mut state,
            exec_done(Err(ExecError::Canceled {
                argv: vec!["add".into(), "--".into(), "a.txt".into()],
            })),
        );

        assert_eq!(cmd, Cmd::LoadStatus);
        assert!(!state.jobs.is_running(JobKind::Exec));
        let notice = state.notices.back().expect("通知がありません");
        assert_eq!(notice.level, NoticeLevel::Info);
        assert_eq!(notice.message, "git の実行を中断しました");
        // 証拠は `detail` に残す（不変ルール 4）。
        assert_eq!(notice.detail.as_deref(), Some("git add -- a.txt"));
    }

    #[test]
    fn a_canceled_commit_reloads_the_graph() {
        // **中断は「失敗」ではない。** `git commit` は ref を書いてから post-commit
        // hook を走らせるので、hook の最中に殺すとコミットは済んでいる（git 2.55 で
        // 確認）。失敗と同じ扱いにすると、新しいコミットが積んだ行に無いまま
        // ブランチのラベルだけが動く。
        assert!(ref_may_have_moved(&Err(ExecError::Canceled {
            argv: vec!["commit".into()],
        })));
        assert!(!ref_may_have_moved(&Err(ExecError::Failed {
            argv: vec!["commit".into()],
            code: Some(1),
            stdout: String::new(),
            stderr: "pre-commit hook が断りました".to_string(),
            kind: ExecErrorKind::Other,
        })));
        assert!(!ref_may_have_moved(&Err(ExecError::NoResult {
            argv: Vec::new(),
            reason: "起動できませんでした".to_string(),
        })));

        let mut state = state_with_commits(3);
        let _ = state.issue(Cmd::Exec(commit_op()));

        let cmd = update(
            &mut state,
            exec_done_of(
                commit_op(),
                Err(ExecError::Canceled {
                    argv: vec!["commit".into()],
                }),
            ),
        );

        // 積んだ行の読み直しまで出す（`Reload::RefsAndLog`）。
        let Cmd::Batch(cmds) = cmd else {
            panic!("読み直しが 1 つしか出ていません: {cmd:?}");
        };
        assert!(
            cmds.iter()
                .any(|cmd| matches!(cmd, Cmd::LoadLog { restart: true, .. })),
            "{cmds:?}"
        );
    }
}
