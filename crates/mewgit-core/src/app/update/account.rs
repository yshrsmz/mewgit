//! アカウントとリポジトリ対象の解決（04）。
//!
//! `Cmd::ResolveTarget` → `Cmd::ResolveAccount` の連鎖と、`A` が出すもの。
//! 外の世界（`gh` と GitHub API）はランタイムの向こう側にあるので、ここは
//! 返ってきた結果を [`AppState`] に写して次に何を頼むかを決めるだけ。

use crate::auth::url::RemoteUrl;
use crate::auth::{Account, AccountState, AuthError, DegradedReason, GhLogin, ResolvedTarget};
use crate::git_exec::GitOp;
use crate::git_read::GitReadError;

use super::prs;
use super::remote::conventional_remote;
use super::submit_exec;
use crate::app::{
    AccountListing, AccountView, AppState, Cmd, GithubBlock, JobKind, Modal, NoticeLevel, Repick,
    RepoSnapshot, TargetState,
};

/// `A`。アカウント解決の結果を出す（01 の「アカウントの選び直し」）。
///
/// **モーダルを開くのは `Degraded` と `Resolved` の 2 つ。** 通知に落ちるのは
/// `Resolving` だけで、あれは待てば変わるので出す中身が 1 行しかない。
pub(super) fn open_account(state: &mut AppState) -> Cmd {
    match account_view(&state.account, &state.repo.target) {
        Ok(view) => {
            // **一覧の取り直しはモーダルを開いてから。** `gh auth status` は 0.95 秒
            // かかる（#61 の実測）ので、返事を待ってから開くと `A` が 1 秒無反応になる。
            let list = match &view {
                AccountView::Repick(repick) => Cmd::ListAccounts(repick.host.clone()),
                AccountView::Degraded(_) => Cmd::None,
            };
            state.modal = Some(Modal::AccountPicker { view, cursor: 0 });
            list
        }
        Err(message) => {
            state.push_notice(NoticeLevel::Info, message, None);
            Cmd::None
        }
    }
}

/// `A` が出すもの。`Ok` がモーダルの中身、`Err` が通知の文言（01 の表）。
///
/// **`AccountState::Resolving` を割らずに `target` を一緒に読む**（04 の
/// 「内訳を出すのは `Modal::AccountPicker`」）。`Resolving` が兼ねている
/// 「走っている最中」と「永久に頼まれない」を見分けるのは [`TargetState`] のほうで、
/// あれは**まさにその理由で 3 値**にしてある。割ると同じ事実が 2 箇所に出る。
fn account_view(account: &AccountState, target: &TargetState) -> Result<AccountView, String> {
    match account {
        AccountState::Degraded(reason) => Ok(AccountView::Degraded(reason.clone())),
        // **出どころで腕を割らない。** `Config` 由来のときだけ開く形にすると、
        // `Single` / `Probe` で決まった相手を mewgit から変えられないまま残る
        // （01 の「`Resolved` を出どころで割らない」）。出どころで変わるのは
        // `u` が効くかどうかだけで、それは [`AccountView::is_unsettable`] が答える。
        AccountState::Resolved(account) => Ok(AccountView::Repick(Repick {
            login: account.login.clone(),
            host: account.host.clone(),
            source: account.source,
            logins: AccountListing::Waiting,
        })),
        // **`Unavailable` にだけ理由を出す。** 走っている最中は待てば変わるので、
        // 「次に何をするか」は無い（案内が求める操作を自分が持っているか、の逆側）。
        //
        // **文言は [`GithubBlock`] の 1 本から採る。** `r`（PRs ペインの引き直し）が
        // 同じ状態に別の説明を出さないため —— あちらも通知 1 行で予算が同じ
        // （01 の「引き直す（`r`）」）。**腕を自分で並べているのは、`A` の答えが
        // 「引けるか」ではなく「モーダルを開けるか」だから** —— `Degraded` と
        // `Resolved` はここでは `Ok` に落ちるので、`AppState::github_block` の
        // `Option` とは境目が違う。
        AccountState::Resolving => Err(match target {
            TargetState::Unavailable => GithubBlock::TargetUnavailable.notice(),
            TargetState::Unasked | TargetState::Resolved(_) => {
                GithubBlock::AccountResolving.notice()
            }
        }),
        AccountState::Disabled => Err(GithubBlock::AccountDisabled.notice()),
    }
}

/// 開いている [`Modal::AccountPicker`] の中身。
///
/// **キーを受ける 3 本が同じ分解を書いていたので寄せた**（`.claude/rules/rust-coding.md` の「2 つ目の
/// 利用者が出た時点で共有先を作る」）。`None` の腕は**どれも踏めない** ——
/// [`modal_key`](super::modal_key) が `Modal::AccountPicker` に一致したときだけ
/// この 3 本を呼ぶ。踏めない腕を 3 本に散らさないためでもある。
fn account_picker(state: &AppState) -> Option<(&AccountView, usize)> {
    match state.modal.as_ref() {
        Some(Modal::AccountPicker { view, cursor }) => Some((view, *cursor)),
        _ => None,
    }
}

/// [`Modal::AccountPicker`] の `Enter`。選んだ login を `.git/config` に書く。
///
/// **選べるのは候補が在るときだけ**（[`AccountView::candidates`] が空なら何もしない）。
/// `NoAccess` を選ばせないのは、どれも届かない状態でアカウントを選んでも何も直らない
/// から（04 の手順 4）。一覧の取り直しが終わっていないあいだも同じ扱いになる。
pub(super) fn account_picked(state: &mut AppState) -> Cmd {
    let Some((view, cursor)) = account_picker(state) else {
        return Cmd::None;
    };
    let Some(login) = view.candidates().get(cursor).map(|c| c.login.clone()) else {
        return Cmd::None;
    };
    submit_account_write(state, GitOp::SetAccount { login })
}

/// [`Modal::AccountPicker`] の `u`。`.git/config` の `mewgit.user` を外す。
///
/// **効く条件は [`AccountView::is_unsettable`] の 1 本**で、キーの案内
/// （`[u] 指定を解除`）も同じものを読む —— 別々に見ると、効かない画面に案内だけが
/// 出る（#53）。設定ファイルの `[[repos]]` は `git config` の手が届かないので、
/// そちらは本文で外の直し方を案内する（#65 の「指示を足すのは、その操作を持たない側」）。
pub(super) fn account_unset(state: &mut AppState) -> Cmd {
    let Some((view, _)) = account_picker(state) else {
        return Cmd::None;
    };
    if !view.is_unsettable() {
        return Cmd::None;
    }
    submit_account_write(state, GitOp::UnsetAccount)
}

/// `.git/config` の `mewgit.user` を書く `op` を出し、**出せたら `account` を
/// `Resolving` に落とす**（01 の「写しの寿命」）。
///
/// **書き込みが終わるまで待たない。** 終わってから落とすと、走っているあいだに
/// `A` で**書き込み前の写しを持ったモーダルが開き直せる**（[`submit_exec`] は
/// `git` を走らせる前に閉じる）。`Resolving` の `A` は通知に落ちるので、写しが
/// 書き込みより長生きする窓がここで消える。**断られたら動かさない** —— 落としてから
/// `Cmd` が消えると、頼み直す相手がいないまま `Resolving` に残る。ほかの `git` が
/// 走っているあいだは [`submit_exec`] がモーダルを閉じずに断る（閉じてから `Cmd` が
/// 落とされると、選んだのに何も起きない）。
///
/// **`mewgit.user` を書く `Cmd::Exec` はここからしか出さない。** `r` が書き込み中でも
/// 断らないことと、`JobKind::ResolveAccount` が世代を持たないことは、この 1 本に
/// 頼っている。入口を足すなら、呼び出し元ではなく op（`Reload::Account`）から導いて
/// 受理の共通経路で落とす形に移す（`/simplify` の altitude が出した案。いまは
/// 入口が 2 つとも同じファイルなので見送った）。
fn submit_account_write(state: &mut AppState, op: GitOp) -> Cmd {
    let cmd = submit_exec(state, op);
    if matches!(cmd, Cmd::Exec(_)) {
        leave_account(state);
    }
    cmd
}

/// [`Modal::AccountPicker`] の `r`。外で直したあとの再解決。
///
/// 効く条件は [`AccountView::is_reresolvable`] の 1 本で、キーの案内
/// （`[r] 解決し直す`）も同じものを読む（#53）。**ほかの `git` が走っていても
/// 断らない** —— `mewgit.user` の書き込みとは重ならない（[`submit_account_write`]）。
pub(super) fn account_reresolve(state: &mut AppState) -> Cmd {
    let Some((view, _)) = account_picker(state) else {
        return Cmd::None;
    };
    if !view.is_reresolvable() {
        return Cmd::None;
    }
    resolve_again(state)
}

/// 取り直した候補を [`Modal::AccountPicker`] に入れる（01 の「候補の取り直し」）。
///
/// **採るのは、モーダルがまだ `Repick` の待ちのときだけ。** `gh auth status` の
/// 0.95 秒のあいだに `Esc` で閉じられるし、別のモーダルも開く。**もう一度 `A` を
/// 押された形も通る** —— 2 度目の [`Cmd::ListAccounts`] は
/// [`AppState::issue`](crate::app::AppState::issue) が落とすので、遅れて届いた 1 通が
/// **新しいほうのモーダル**を埋める。だから見るのは**モーダルの形**であって
/// 「自分が頼んだか」ではない。
///
/// **`state.account` は動かさない。** `Err` を [`DegradedReason`] に畳むと
/// `auth::resolve` の外に [`AccountState`] の産出元ができ、04 が決めた境目が消える。
///
/// **`Repick` に着いたら上書きしてよい。** 「もう埋まっていないか」を先に見たく
/// なるが、その形は組めない —— この `Cmd` を出すのは [`open_account`] だけで、
/// あれは必ず [`AccountListing::Waiting`] を置き、2 通目は
/// [`AppState::issue`](crate::app::AppState::issue) が落とす。**踏めない状態を
/// 見張るコードを置かない**（`.claude/rules/testing.md`）。カーソルも同じで、待っているあいだは
/// 候補が空なので `j` / `k` が動かせない（`stepped_index`）。
pub(super) fn accounts_listed(
    state: &mut AppState,
    result: Result<Vec<GhLogin>, AuthError>,
) -> Cmd {
    state.jobs.finish(JobKind::ListAccounts);
    let Some(Modal::AccountPicker {
        view: AccountView::Repick(repick),
        ..
    }) = state.modal.as_mut()
    else {
        return Cmd::None;
    };
    repick.logins = match result {
        Ok(logins) => AccountListing::Listed(logins),
        // **`AuthError` のまま持たない。** 変種で分岐する読み手が UI に無く
        // （直し方の違いは `Display` に出ている）、持たせると `DegradedReason::Unusable`
        // が「3 つを分けない」と決めたのと同じ理由で読まれない状態が増える。
        Err(err) => AccountListing::Failed(err.to_string()),
    };
    Cmd::None
}

/// リポジトリの概要を反映する。
///
/// 一部だけ成功という形にしていないのは、3 つとも同じ ref から導かれる値で、
/// 途中まで新しい状態を持っても意味が無いため。失敗しても通知だけで続ける
/// （ラベルもリモートも無いままグラフは出せる）。
pub(super) fn repo_info_loaded(
    state: &mut AppState,
    result: Result<RepoSnapshot, GitReadError>,
) -> Cmd {
    let asked = state.jobs.finish(JobKind::LoadRepoInfo);

    let resolve = match result {
        Ok(info) => {
            state.repo.remotes = info.remotes;
            state.repo.head = Some(info.head);
            state.graph.refs = info.refs;
            // **`Ok` のときだけ。** `Err` の直後は `remotes` が前のまま（起動時なら
            // 空）で、`jobs.finish` は上で済んでいるから「まだ読めていません」の
            // 腕にも入らない。呼ぶと**読めなかっただけ**の一回を
            // `TargetState::Unavailable` として latch し、下の `again` が読み直しに
            // 成功しても `Unasked` ではなくなっているので二度と頼まない
            // （`TargetState` の doc が言う「決められなかった」は読めなかったことでは
            // ない）。`/code-review` が拾った。
            // **PR の一覧も引き直す**（`prs::reload` の doc）。ref が動いたあとは
            // GitHub 側の PR も CI も動いており、マージされていれば open から消える。
            // **`Ok` のときだけ** —— `Err` の直後は `head` を更新していないので、
            // 引き直しても同じ答えを 1 往復ぶん買うだけになる（`resolve_target` を
            // `Ok` の側だけで呼ぶのと同じ理由）。
            Cmd::batch([resolve_target(state), prs::reload(state)])
        }
        Err(err) => {
            state.push_notice(NoticeLevel::Warn, err.to_string(), None);
            Cmd::None
        }
    };
    // 書き込みの直後に頼んだぶんが落とされていたら、ここで頼み直す
    // （`status_loaded` と同じ形）。**採ったうえで頼み直す** —— 古い索引でも
    // ラベルは出せるので、届くまで前のものを出したままにする。
    let again = if state.jobs.is_fresh(JobKind::LoadRepoInfo, asked) {
        Cmd::None
    } else {
        Cmd::LoadRepoInfo
    };
    // 続きを頼む: 落とされたぶんの `LoadRepoInfo` の追いつき。
    Cmd::batch([again, resolve])
}

/// 既定リモートの URL から [`Cmd::ResolveTarget`] を組む（01 の
/// 「`RepoInfo.target` を埋める」）。
///
/// **`TargetState::Unasked` のときだけ動く。** `repo_info_loaded` は ref を動かす
/// 書き込みのたびに走るので、印を見ないと commit / checkout / fetch のたびに
/// `ssh -G` のサブプロセスが増える。
///
/// **決まらなかったことの通知は出し分ける。**
///
/// - 既定リモートが決まらない（リモートが無い / `origin` が無い）→ **黙る。**
///   リモートの無いローカルリポジトリで起動のたびに警告が出る。押したときに
///   困るのは `f` なので、そちらが押された時点で言う
/// - URL は見つかったが GitHub の形として読めない → **1 回だけ通知する。**
///   `Unavailable` に落ちるので繰り返さない。GitHub 機能が丸ごと効かない状態を、
///   ヘッダの degraded 表示（M4 の別タスク）が入るまで黙って隠さないため
fn resolve_target(state: &mut AppState) -> Cmd {
    if state.repo.target != TargetState::Unasked {
        return Cmd::None;
    }
    // **[`default_remote`] ではない。** あちらの 1 段目（`branch.<HEAD>.remote`）を
    // 通すと、`Msg::BranchesLoaded` が先に届いたかどうかで答えが変わる（あちらの doc）。
    let Ok(remote) = conventional_remote(state) else {
        state.repo.target = TargetState::Unavailable;
        return Cmd::None;
    };
    match RemoteUrl::parse(&remote.url) {
        // **ここで `issue` しない。** `update` の出口が 1 箇所で通す（同じ形の
        // 二重発行は `JobTracker` に落とされ、`Cmd` が黙って消える）。
        Ok(url) => Cmd::ResolveTarget(url),
        Err(err) => {
            state.repo.target = TargetState::Unavailable;
            state.push_notice(NoticeLevel::Warn, err.to_string(), None);
            Cmd::None
        }
    }
}

/// `ssh -G` の結果を `RepoInfo.target` に入れる。
///
/// `unnormalized` があっても `target` は埋める。**degraded ではなく正規化だけを
/// 諦める**（04）。
pub(super) fn target_resolved(state: &mut AppState, resolved: ResolvedTarget) -> Cmd {
    state.jobs.finish(JobKind::ResolveTarget);
    // **起動時の 1 本目はここが直接出す**（読み直す前の状態が無いので
    // [`resolve_again`] を通せない）。`init` からは出せない —— `auth::resolve` は `RepoTarget` を要るので、
    // `Msg::RepoInfoLoaded` → `Cmd::ResolveTarget` → ここ、と 2 往復待ってからでないと
    // 材料が揃わない（01 の「`RepoInfo.target` を埋める」）。
    // **`gh` を一度も起動しない**（01 の「GitHub 機能を切る」）。断るのはここ 1 箇所で
    // 足りる —— [`resolve_again`] へ入る 2 本（`Modal::AccountPicker` の `r` と
    // `GitOp::SetAccount` / `UnsetAccount` の後始末）は、どちらもそのモーダルを
    // 開いたあとにしかなく、`Disabled` では [`account_view`] が通知に落とす。
    let account = if state.account.is_disabled() {
        Cmd::None
    } else {
        Cmd::ResolveAccount(resolved.target.clone())
    };
    state.repo.target = TargetState::Resolved(resolved.target);
    if let Some(reason) = resolved.unnormalized {
        state.push_notice(NoticeLevel::Warn, reason.to_string(), None);
    }
    account
}

/// 決まったアカウントを状態に入れる（04 の手順 1〜4 の結果）。
///
/// **通知は出さない。** `Degraded` はヘッダに出るので（07）、同じことを 2 箇所で
/// 言わない。理由の内訳（`NoAccess` の `attempts`）は `A` で開く
/// [`Modal::AccountPicker`] が並べる（04 の「内訳を出すのは `Modal::AccountPicker`」）。
pub(super) fn account_resolved(
    state: &mut AppState,
    result: Result<Account, DegradedReason>,
) -> Cmd {
    state.jobs.finish(JobKind::ResolveAccount);
    state.account = match result {
        Ok(account) => AccountState::Resolved(account),
        Err(reason) => {
            // **前のアカウントで引いた一覧を残さない**（`PrState::forget`）。
            // `Degraded` では `prs::reload` が `Cmd::None` を返すので、落とす経路が
            // ここしか無い —— 残すと `*` / `R` の印（login で決まる）だけが黙って
            // 消えた一覧が出たままになる。
            state.prs.forget();
            AccountState::Degraded(reason)
        }
    };
    // **アカウントが決まるまで `wanted_pr_list()` は偽**なので、ここも PR 一覧を頼む
    // 入口（一覧は `prs::reload` の doc）。**非同期で届くもう 1 本
    // （`repo_info_loaded`）とはどちらが先に届くかが決まらない**
    // （01 の「PR ペイン › 何を引くか」）。
    //
    // **鍵が同じでも引き直す、ではなく鍵が無い。** 一覧は選択に紐づかないので、
    // `A` でアカウントを選び直したらそのまま引き直す（`prs::reload` の doc）。
    prs::reload(state)
}

/// 04 の手順 1〜4 を通し直す（01 の「アカウントの選び直し」）。
///
/// **呼ぶ条件は 1 つ** —— `target` が決まっていて、決めたときの材料が変わったあと。
/// 誰が呼ぶかはここに書かない（`resolve_again` で grep すれば出る）。
pub(super) fn resolve_again(state: &mut AppState) -> Cmd {
    // **`target` が無ければ何もしない。** `Degraded` は `target` が決まったあとにしか
    // 出ないのでここには来ないが、来たときに `Resolving` へ落とすと
    // 頼み直す相手がいないまま**ヘッダから `[gh: …]` が消えたきり**になる。
    let Some(target) = state.repo.target.resolved().cloned() else {
        return Cmd::None;
    };
    leave_account(state);
    Cmd::ResolveAccount(target)
}

/// 決まっていたアカウントを手放して `Resolving` に落とす。**落とす側はここ 1 本**
/// （[`resolve_again`] と [`submit_account_write`]）。
///
/// **開いている [`Modal::AccountPicker`] は閉じる。** [`Repick`] は
/// [`AppState::account`] の写しを持っていて、ここから先その写しは嘘になる ——
/// 閉じないと、外した直後の画面が `[u] 指定を解除` を出したままになり、押すと
/// 既に無いキーへ `git config --unset` が飛ぶ（argv だけの赤い通知が出る。
/// 01 の「写しの寿命」）。**写しを禁じるのではなく、写しの寿命を切る** ——
/// `ui` のモーダル描画は `Modal` しか受け取らないので写し自体は要る（[`Repick`] の doc）。
fn leave_account(state: &mut AppState) {
    if matches!(state.modal, Some(Modal::AccountPicker { .. })) {
        state.modal = None;
    }
    // **一覧も落とす**（`account_resolved` の `Err` と同じ理由）。解決し直しているあいだ
    // 出したままにすると、`*` / `R` の印だけが消えた前のアカウントの一覧が残る。
    state.prs.forget();
    state.account = AccountState::Resolving;
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        cells, commit, exec_done_of, fake_output, fetched_remote, issued, press, press_code,
        remotes_of, state, state_with_commits, tracking_branch, with_branches, HEADS_MAIN,
        NOTICE_CELLS,
    };
    use super::super::{init, update};
    use super::*;
    use crate::app::{KeyCode, Modal, Msg};
    use crate::auth::{AccountSource, GhLogin};

    use crate::git_exec::ExecError;
    use crate::git_read::{HeadInfo, RefIndex, RefLabel, Remote};

    use crate::test_util::{
        fake_account, fake_ambiguous, fake_attempt, fake_configured_unavailable, fake_denied,
        fake_logins, fake_no_access, fake_repo_snapshot, fake_target, reply,
    };

    /// リモートを載せた `RepoInfoLoaded` を 1 通届ける。
    ///
    /// **`reply` を通すのは、ランタイムでは `Cmd` が必ず `AppState::issue` を通るから**
    /// （ヘルパが実物では起きない形を作らないため）。
    fn deliver_repo_info(state: &mut AppState, remotes: &[(&str, &str)]) -> Cmd {
        let info = RepoSnapshot {
            remotes: remotes_of(remotes),
            ..fake_repo_snapshot()
        };
        reply(
            state,
            Cmd::LoadRepoInfo,
            Msg::RepoInfoLoaded(Box::new(Ok(info))),
        )
    }

    const ORIGIN_SSH: &str = "git@github.com:yshrsmz/mewgit.git";

    #[test]
    fn the_repo_info_asks_to_resolve_the_target() {
        let mut state = state();
        let cmd = deliver_repo_info(&mut state, &[("origin", ORIGIN_SSH)]);
        assert_eq!(
            cmd,
            Cmd::ResolveTarget(RemoteUrl::parse(ORIGIN_SSH).unwrap())
        );
    }

    #[test]
    fn the_target_ignores_the_branch_tracking_remote() {
        // **`f` とは別の規則**（`conventional_remote`）。1 段目を通すと、
        // `Msg::BranchesLoaded` と `Msg::RepoInfoLoaded` のどちらが先に届くかで
        // `target` が変わり、latch されるので二度と直らない。
        //
        // fork 運用（`origin` = 自分、`upstream` = 本家、HEAD が `upstream` 追跡）で
        // **`f` は `upstream` を、`target` は `origin` を選ぶ**。
        let mut state = state_with_commits(2);
        let _ = with_branches(
            &mut state,
            Ok(vec![tracking_branch("main", "upstream", HEADS_MAIN)]),
        );
        let cmd = deliver_repo_info(
            &mut state,
            &[
                ("origin", "git@github.com:me/x.git"),
                ("upstream", "git@github.com:them/x.git"),
            ],
        );
        let Cmd::ResolveTarget(url) = cmd else {
            panic!("target を頼んでいない: {cmd:?}");
        };
        assert_eq!(url.owner, "me", "追跡先のリモートを見ている");

        // 同じ状態で `f` は追跡先を採る（規則が 2 つに割れていることの見張り）。
        let cmd = press(&mut state, 'f');
        assert_eq!(fetched_remote(&cmd).as_deref(), Some("upstream"), "{cmd:?}");
    }

    #[test]
    fn the_target_is_resolved_once_not_on_every_write() {
        // **これが `TargetState::Unasked` の見張り。** `RepoInfoLoaded` は ref を動かす
        // 書き込みのたびに届くので、印を見ないと commit / checkout / fetch のたびに
        // `ssh -G` のサブプロセスが 1 つ増える。
        let mut state = state();
        let _ = deliver_repo_info(&mut state, &[("origin", ORIGIN_SSH)]);
        let cmd = update(&mut state, resolved_target(None));
        assert_eq!(cmd, Cmd::ResolveAccount(fake_target()));
        assert_eq!(state.repo.target, TargetState::Resolved(fake_target()));

        let cmd = deliver_repo_info(&mut state, &[("origin", ORIGIN_SSH)]);
        let want = Cmd::ResolveTarget(RemoteUrl::parse(ORIGIN_SSH).unwrap());
        assert!(!issued(&cmd, &want), "2 度目を頼んでいる: {cmd:?}");
    }

    #[test]
    fn a_failed_repo_info_does_not_give_up_on_the_target() {
        // **読めなかっただけの一回で latch しない。** `Err` の直後は `remotes` が
        // 前のまま（起動時なら空）なので、そこで `Unavailable` を書くと、読み直しが
        // 成功しても `Unasked` ではなくなっていて二度と頼まない。
        let mut state = state();
        let cmd = reply(
            &mut state,
            Cmd::LoadRepoInfo,
            Msg::RepoInfoLoaded(Box::new(Err(GitReadError::NonUtf8Head))),
        );
        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.repo.target, TargetState::Unasked, "諦めている");

        // 読み直しが通れば target は決まる。
        let cmd = deliver_repo_info(&mut state, &[("origin", ORIGIN_SSH)]);
        assert_eq!(
            cmd,
            Cmd::ResolveTarget(RemoteUrl::parse(ORIGIN_SSH).unwrap())
        );
    }

    #[test]
    fn f_waits_when_the_branch_list_arrives_before_the_remotes() {
        // **`f_works_before_the_branch_list_arrives` の逆順。** 一覧が空のまま
        // 1 段目を通すと、追跡先が見つからないのは当たり前なので「追跡先の origin が
        // リモートにありません」——**設定が壊れていると言ってしまう**。
        let mut state = state_with_commits(2);
        let _ = state.issue(Cmd::LoadRepoInfo);
        let _ = with_branches(
            &mut state,
            Ok(vec![tracking_branch("main", "origin", HEADS_MAIN)]),
        );

        assert_eq!(press(&mut state, 'f'), Cmd::None);
        let notice = state.notices.back().expect("通知が出ていない");
        assert!(
            notice.message.contains("まだ読めていません"),
            "{}",
            notice.message
        );
    }

    #[test]
    fn a_url_that_cannot_be_read_is_reported_once() {
        // **ホストが GitHub かは見ない**（GHES があるので判定できない。04）。ここで
        // 落ちるのは*形*が読めないもの —— これは GitLab のサブグループで、
        // `owner/repo` の 2 段にならない。
        //
        // GitHub 機能が丸ごと効かない状態なので 1 回は言う。**`Unavailable` に落ちる
        // ので繰り返さない** —— 書き込みのたびに出るのを防ぐ。
        const SUBGROUP: &str = "https://gitlab.com/group/sub/repo.git";
        let mut state = state();
        let _ = deliver_repo_info(&mut state, &[("origin", SUBGROUP)]);
        assert_eq!(state.repo.target, TargetState::Unavailable);
        assert_eq!(state.notices.len(), 1, "{:?}", state.notices);

        let _ = deliver_repo_info(&mut state, &[("origin", SUBGROUP)]);
        assert_eq!(
            state.notices.len(),
            1,
            "2 度目も通知した: {:?}",
            state.notices
        );
    }

    #[test]
    fn a_repository_without_remotes_says_nothing() {
        // リモートの無いローカルリポジトリで、起動のたびに警告を出さない。
        // 押したときに困るのは `f` なので、そちらが押された時点で言う（01）。
        let mut state = state();
        let cmd = deliver_repo_info(&mut state, &[]);
        assert_eq!(state.repo.target, TargetState::Unavailable);
        assert!(state.notices.is_empty(), "{:?}", state.notices);
        assert_eq!(cmd, Cmd::None, "頼む相手がいないのに頼んでいる");
    }

    #[test]
    fn a_host_that_could_not_be_normalized_still_fills_the_target() {
        // **degraded ではなく正規化だけを諦める**（04）。`Msg` を `Result` にすると
        // ここで `target` が捨てられ、その経路が消える。
        let mut state = state();
        let cmd = reply(
            &mut state,
            Cmd::ResolveTarget(RemoteUrl::parse(ORIGIN_SSH).unwrap()),
            resolved_target(Some(AuthError::SshResolveFailed {
                alias: "gh-work".to_string(),
                message: "出力に hostname の行がありませんでした".to_string(),
            })),
        );
        // **正規化できなくてもアカウントは頼む。** `host_raw` のままの `target` で
        // GitHub に届く形（`ssh -G` が無い環境のホスト名がそのまま正しい）はある。
        assert_eq!(cmd, Cmd::ResolveAccount(fake_target()));
        assert_eq!(state.repo.target, TargetState::Resolved(fake_target()));
        assert_eq!(state.notices.len(), 1, "{:?}", state.notices);
    }

    #[test]
    fn a_repository_that_is_not_on_github_never_asks_for_an_account() {
        // **`Cmd::ResolveAccount` は `target` が決まっていないと出ない**（`target_resolved`
        // はその返信そのもの、`resolve_again` は `resolved()` で断る）。既定リモートが
        // 決まらないリポジトリでは `target` が `Unavailable` に落ちるので、アカウントは
        // 一度も頼まれず `Resolving` のまま残る（ヘッダは何も出さない。07）。
        let mut state = state();
        let cmd = deliver_repo_info(&mut state, &[]);

        assert_eq!(cmd, Cmd::None);
        assert!(matches!(state.account, AccountState::Resolving));
        assert!(!state.jobs.is_running(JobKind::ResolveAccount));
    }

    #[test]
    fn a_resolved_account_lands_in_the_state() {
        let mut state = state();
        let cmd = reply(
            &mut state,
            Cmd::ResolveAccount(fake_target()),
            Msg::AccountResolved(Ok(fake_account("taro"))),
        );

        assert_eq!(cmd, Cmd::None);
        match &state.account {
            AccountState::Resolved(account) => assert_eq!(account.login, "taro"),
            other => panic!("解決していない: {other:?}"),
        }
        // 畳んでいないと、以後の `Cmd::ResolveAccount` が `issue` に黙って落とされる。
        assert!(!state.jobs.is_running(JobKind::ResolveAccount));
    }

    #[test]
    fn a_degraded_account_lands_in_the_state_without_a_notice() {
        // **通知を出さない。** degraded はヘッダに出るので（07）、同じことを 2 箇所で
        // 言わない。理由の内訳を並べる先は `Modal::AccountPicker` のタスク（04）。
        let mut state = state();
        let cmd = reply(
            &mut state,
            Cmd::ResolveAccount(fake_target()),
            Msg::AccountResolved(Err(DegradedReason::GhMissing)),
        );

        assert_eq!(cmd, Cmd::None);
        assert!(matches!(
            state.account,
            AccountState::Degraded(DegradedReason::GhMissing)
        ));
        assert!(state.notices.is_empty(), "{:?}", state.notices);
        assert!(!state.jobs.is_running(JobKind::ResolveAccount));
    }

    /// `Degraded` まで進んだ状態。`target` は決まっている（degraded はその後にしか
    /// 出ない）。
    fn state_degraded(reason: DegradedReason) -> AppState {
        let mut state = state();
        state.repo.target = TargetState::Resolved(fake_target());
        state.account = AccountState::Degraded(reason);
        state
    }

    #[test]
    fn a_opens_the_picker_when_the_account_is_degraded() {
        let mut state = state_degraded(fake_ambiguous(&["taro_acme", "taro"]));

        let cmd = press(&mut state, 'A');

        // **degraded では一覧を引き直さない。** 候補は `Ambiguous` が運んでいる（04）。
        assert_eq!(cmd, Cmd::None);
        // **理由をそのまま持つ。** 候補を写すと `state.account` と 2 箇所になる（01）。
        assert_eq!(
            state.modal,
            Some(Modal::AccountPicker {
                // **理由をそのまま持つ**ので、`Ambiguous` の中身も一致する。
                view: AccountView::Degraded(fake_ambiguous(&["taro_acme", "taro"])),
                cursor: 0
            })
        );
    }

    /// `Resolved` まで進んだ状態。
    fn state_resolved(source: AccountSource) -> AppState {
        let mut state = state();
        state.repo.target = TargetState::Resolved(fake_target());
        let mut account = fake_account("taro");
        account.source = source;
        state.account = AccountState::Resolved(account);
        state
    }

    #[test]
    fn a_opens_the_repick_and_asks_for_the_logins_again() {
        // **候補は state のどこにも無い**ので `gh auth status` を引き直す（01 の
        // 「候補の取り直し」）。**モーダルは先に開く** —— 0.95 秒待ってから開くと
        // `A` が無反応に見える。
        let mut state = state_resolved(AccountSource::Probe);

        let cmd = press(&mut state, 'A');

        assert_eq!(cmd, Cmd::ListAccounts("github.com".to_string()));
        assert_eq!(
            state.modal,
            Some(Modal::AccountPicker {
                view: AccountView::Repick(Repick {
                    login: "taro".to_string(),
                    host: "github.com".to_string(),
                    source: AccountSource::Probe,
                    logins: AccountListing::Waiting,
                }),
                cursor: 0,
            })
        );
    }

    #[test]
    fn a_tells_apart_the_two_meanings_of_resolving() {
        // **`AccountState::Resolving` は割らず、`target` を一緒に読む**（04）。
        // 「走っている最中」と「永久に頼まれない」で案内が変わるのはここだけ。
        let mut running = state();
        running.repo.target = TargetState::Resolved(fake_target());
        let _ = press(&mut running, 'A');
        let while_running = running.notices.back().expect("通知が無い").message.clone();

        let mut never = state();
        never.repo.target = TargetState::Unavailable;
        let _ = press(&mut never, 'A');
        let never_asked = never.notices.back().expect("通知が無い").message.clone();

        assert!(never.modal.is_none(), "頼んでもいないのに開いた");
        assert_ne!(
            while_running, never_asked,
            "2 つの Resolving に同じ案内を出している"
        );
        assert!(never_asked.contains("GitHub"), "{never_asked}");
    }

    #[test]
    fn every_notice_that_a_prints_fits_the_notice_line() {
        // **予算はフッター 1 行目の幅**（01 の「失敗の案内文」）。`A` は通知の
        // **新しい産出元**なので、`exec_advice` を回している見張りは自分では拾わない
        // （`.claude/rules/testing.md` の「共有した見張りは新しい産出元を自分では拾わない」）。
        //
        // **回すのは通知に落ちる腕を並べたもの。** `Resolved` と `Degraded` は
        // モーダルを開くので出てこない。**範囲を実装側の集合から導かずに並べている**
        // のは #54 の裏返しで、腕を足したらここにも足す。
        let cases = [
            (AccountState::Resolving, TargetState::Unavailable),
            (
                AccountState::Resolving,
                TargetState::Resolved(fake_target()),
            ),
            (AccountState::Disabled, TargetState::Unasked),
        ];
        // **`Resolved` は 1 件も通知を出さない。** 出せば予算の外で伸びる文言が
        // 生えるので、ここで押さえる。
        let resolved = AccountState::Resolved(fake_account("yamada_taro_acme"));
        assert!(
            account_view(&resolved, &TargetState::Unasked).is_ok(),
            "Resolved が通知に落ちている"
        );

        for (account, target) in cases {
            let message = account_view(&account, &target).expect_err("モーダルを開いている");
            assert!(
                cells(&message) <= NOTICE_CELLS,
                "{} セル: {message}",
                cells(&message)
            );
        }
    }

    #[test]
    fn the_picker_saves_the_chosen_login_to_git_config() {
        let mut state = state_degraded(fake_ambiguous(&["taro_acme", "taro"]));
        let _ = press(&mut state, 'A');
        let _ = press(&mut state, 'j');

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(
            cmd,
            Cmd::Exec(GitOp::SetAccount {
                login: "taro".to_string()
            })
        );
        assert!(state.modal.is_none(), "確定したのに開いたまま");
    }

    /// `A` を押して候補待ちにし、そこへ返信を 1 通届けた状態。
    ///
    /// **[`reply`] は通さない。** あれは「まだ発行していない `Cmd`」に返信を届ける
    /// ためのもので、ここは `press`（= `update` の出口）が既に
    /// [`AppState::issue`](crate::app::AppState::issue) を通しているから
    /// ——ジョブは登録済みで、`accounts_listed` の `finish` が畳む相手がいる。
    fn repick_with(source: AccountSource, result: Result<Vec<GhLogin>, AuthError>) -> AppState {
        let mut state = state_resolved(source);
        assert_eq!(
            press(&mut state, 'A'),
            Cmd::ListAccounts("github.com".to_string())
        );
        let _ = update(&mut state, Msg::AccountsListed(result));
        state
    }

    #[test]
    fn the_listed_logins_land_in_the_open_repick_and_can_be_chosen() {
        let mut state = repick_with(
            AccountSource::Probe,
            Ok(fake_logins(&["taro_acme", "hanako"])),
        );
        // 畳んでいないと、次に `A` を押したときの `Cmd` が `issue` に落とされる。
        assert!(!state.jobs.is_running(JobKind::ListAccounts));

        let _ = press(&mut state, 'j');
        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(
            cmd,
            Cmd::Exec(GitOp::SetAccount {
                login: "hanako".to_string()
            })
        );
    }

    #[test]
    fn nothing_can_be_chosen_while_the_logins_are_still_loading() {
        // **`Waiting` の候補は空**（`AccountView::candidates`）。`j` も `Enter` も
        // 効かないので、見えていない相手を保存できない。
        let mut state = state_resolved(AccountSource::Probe);
        let _ = press(&mut state, 'A');

        let _ = press(&mut state, 'j');
        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::None);
        assert!(state.modal.is_some(), "選べないのに閉じた");
    }

    #[test]
    fn a_listing_that_failed_shows_up_without_touching_the_account() {
        // **`DegradedReason` に畳まない**（01 の「候補の取り直し」）—— 畳むと
        // `auth::resolve` の外に `AccountState` の産出元ができる。
        let state = repick_with(
            AccountSource::Probe,
            Err(AuthError::GhMissing {
                bin: "gh".to_string(),
            }),
        );

        let Some(Modal::AccountPicker {
            view: AccountView::Repick(repick),
            ..
        }) = &state.modal
        else {
            panic!("{:?}", state.modal);
        };
        assert!(
            matches!(&repick.logins, AccountListing::Failed(message) if message.contains("gh")),
            "{:?}",
            repick.logins
        );
        // 引けなかっただけで、いま使っているアカウントは決まったまま。
        assert!(matches!(state.account, AccountState::Resolved(_)));
    }

    #[test]
    fn a_late_login_list_fills_the_modal_that_is_open_now() {
        // **見るのはモーダルの形で、「自分が頼んだか」ではない**（01 の
        // 「候補の取り直し」）。`Esc` で閉じてもう一度 `A` を押すと、2 度目の
        // `Cmd::ListAccounts` は `issue` に落とされるので、**遅れて届いた 1 通が
        // 新しいほうのモーダルを埋めるしかない**。
        let mut state = state_resolved(AccountSource::Probe);
        // `update` の出口が `AppState::issue` を通しているので、返ってきた `Cmd` は
        // 発行済み（ジョブが登録されている）。
        let first = press(&mut state, 'A');
        assert_eq!(first, Cmd::ListAccounts("github.com".to_string()));
        let _ = press_code(&mut state, KeyCode::Esc);

        let second = press(&mut state, 'A');
        assert_eq!(second, Cmd::None, "2 度目が落とされていない");

        let cmd = update(
            &mut state,
            Msg::AccountsListed(Ok(fake_logins(&["taro_acme"]))),
        );

        assert_eq!(cmd, Cmd::None);
        let Some(Modal::AccountPicker {
            view: AccountView::Repick(repick),
            ..
        }) = &state.modal
        else {
            panic!("{:?}", state.modal);
        };
        assert_eq!(
            repick.logins,
            AccountListing::Listed(fake_logins(&["taro_acme"]))
        );
    }

    #[test]
    fn a_login_list_that_arrives_after_the_modal_closed_is_dropped() {
        let mut state = state_resolved(AccountSource::Probe);
        let cmd = press(&mut state, 'A');
        let _ = press_code(&mut state, KeyCode::Esc);

        let _ = reply(
            &mut state,
            cmd,
            Msg::AccountsListed(Ok(fake_logins(&["taro"]))),
        );

        assert!(state.modal.is_none(), "閉じたモーダルが戻っている");
        // **畳むのは形を見る前。** 畳まないと、次に `A` を押した `Cmd` が落とされる。
        assert!(!state.jobs.is_running(JobKind::ListAccounts));
    }

    #[test]
    fn u_removes_the_setting_only_when_it_lives_in_git_config() {
        // **出どころで割った意味がここに出る。** `[[repos]]` は `git config` の手が
        // 届かないので `u` を効かせない（効かないなら案内も出ない。#53）。
        let cases = [
            (AccountSource::GitConfig, true),
            (AccountSource::RepoRule, false),
            (AccountSource::Single, false),
            (AccountSource::Probe, false),
        ];

        for (source, unsettable) in cases {
            let mut state = state_resolved(source);
            let _ = press(&mut state, 'A');

            let cmd = press(&mut state, 'u');

            if unsettable {
                assert_eq!(cmd, Cmd::Exec(GitOp::UnsetAccount), "{source:?}");
                assert!(state.modal.is_none(), "{source:?}: 確定したのに開いたまま");
            } else {
                assert_eq!(cmd, Cmd::None, "{source:?}");
                assert!(state.modal.is_some(), "{source:?}: 効かないのに閉じた");
            }
        }
    }

    #[test]
    fn a_cannot_reopen_the_picker_while_the_account_write_runs() {
        // **`Repick` は `state.account` の写し**なので、書き込みのあいだ開き直せると
        // 書き込み前の写しが残る（#78 で `/code-review` が拾った）。**書き込みを
        // 出した時点で `Resolving` に落とす**ので、`A` は通知に落ちる（01 の
        // 「写しの寿命」）。
        //
        // **出口が 2 つある**（`u` → `UnsetAccount`、`Enter` → `SetAccount`）ので両方を踏む。
        // 一覧を届けてから押すのは、`Enter` が候補を要るから。
        // 確定のキー（`u` / `Enter`）と、それが出す書き込み。
        type Confirm = fn(&mut AppState) -> Cmd;
        let cases: [(Confirm, GitOp); 2] = [
            (|state| press(state, 'u'), GitOp::UnsetAccount),
            (
                |state| press_code(state, KeyCode::Enter),
                GitOp::SetAccount {
                    login: "taro".to_string(),
                },
            ),
        ];
        for (confirm, op) in cases {
            let mut state = state_resolved(AccountSource::GitConfig);
            let _ = press(&mut state, 'A');
            let _ = update(&mut state, Msg::AccountsListed(Ok(fake_logins(&["taro"]))));

            let cmd = confirm(&mut state);
            assert_eq!(cmd, Cmd::Exec(op.clone()));
            assert!(state.modal.is_none(), "{op:?}: 確定したのに開いたまま");
            // **これが本体。** 終わるまで待つと、そのあいだ `A` が写しを開き直せる。
            assert!(
                matches!(state.account, AccountState::Resolving),
                "{op:?}: {:?}",
                state.account
            );

            // **開き直す経路を実際に踏む**（`.claude/rules/testing.md`）。モーダルが
            // 閉じていることは上で押さえたので、`A` は `open_account` に届く。
            let reopened = press(&mut state, 'A');
            assert_eq!(reopened, Cmd::None, "{op:?}: 一覧を引き直した");
            assert!(state.modal.is_none(), "{op:?}: 書き込み前の写しが開いた");
            assert_eq!(
                state.notices.back().map(|n| n.message.clone()),
                Some(GithubBlock::AccountResolving.notice()),
                "{op:?}"
            );

            // 書き込みが終われば解決し直す。
            let cmd = update(&mut state, exec_done_of(op.clone(), Ok(fake_output())));
            assert!(
                issued(&cmd, &Cmd::ResolveAccount(fake_target())),
                "{op:?}: {cmd:?}"
            );
        }
    }

    #[test]
    fn u_removes_a_configured_login_whose_token_went_missing() {
        // **degraded に落ちた指定も外せる。** ここを塞がないと、壊れた
        // `mewgit.user` を憶えたまま `A` が案内しか出せない（`Resolved` 側と
        // 同じ穴の degraded 版）。
        let mut state = state_degraded(fake_configured_unavailable());
        let _ = press(&mut state, 'A');

        let cmd = press(&mut state, 'u');

        assert_eq!(cmd, Cmd::Exec(GitOp::UnsetAccount));
    }

    #[test]
    fn u_does_nothing_on_a_degraded_reason_that_has_nothing_to_remove() {
        // `gh` が居ないのは**プロセス全体の条件**で、`mewgit.user` を外しても
        // 変わらない（04 の `configured_failed`）。
        let mut state = state_degraded(DegradedReason::GhMissing);
        let _ = press(&mut state, 'A');

        let cmd = press(&mut state, 'u');

        assert_eq!(cmd, Cmd::None);
        assert!(state.modal.is_some(), "効かないのに閉じた");
    }

    #[test]
    fn r_resolves_again_after_the_outside_was_fixed() {
        // **`gh auth login` は mewgit の外で起きる**ので、変わったことを知る手掛かりが
        // 無い —— 押されたときに手順 1〜4 を通し直す（01 の「外で直したあとの再解決」）。
        let mut state = state_degraded(DegradedReason::NotLoggedIn {
            host: "github.com".to_string(),
        });
        let _ = press(&mut state, 'A');

        let cmd = press(&mut state, 'r');

        assert_eq!(cmd, Cmd::ResolveAccount(fake_target()));
        // **落とされたのではなく出た**、の区別（`Cmd` の戻り値だけからは読めない）。
        assert!(state.jobs.is_running(JobKind::ResolveAccount));
        // 閉じるのは `leave_account` の 1 箇所。開いたまま差し替えると、`Resolving` の
        // あいだも古い `DegradedReason` の写しが残る（01 の「写しの寿命」）。
        assert!(state.modal.is_none(), "写しが残った");
        assert!(matches!(state.account, AccountState::Resolving));

        // 返ってくれば決まる（ヘッダに `[gh: …]` が戻る）。
        let _ = update(&mut state, Msg::AccountResolved(Ok(fake_account("taro"))));
        assert!(matches!(state.account, AccountState::Resolved(_)));
    }

    #[test]
    fn r_does_nothing_on_the_repick() {
        // **`Repick` を外してある理由は 01 の「外で直したあとの再解決」。**
        let mut state = state_resolved(AccountSource::GitConfig);
        let _ = press(&mut state, 'A');

        let cmd = press(&mut state, 'r');

        assert_eq!(cmd, Cmd::None);
        assert!(state.modal.is_some(), "効かないのに閉じた");
        assert!(!state.jobs.is_running(JobKind::ResolveAccount));
    }

    #[test]
    fn r_is_not_refused_while_an_unrelated_git_runs() {
        // **追い越すと困るのは `mewgit.user` の書き込みだけ**で、そのあいだはこの
        // モーダルが開かない（上の `a_cannot_reopen_the_picker_while_the_account_write_runs`）。
        // stage が `.git/config` を書くことは無いので、読み取りの `r` を書き込み向けの
        // 文言（「ほかの git 実行が終わるまで」）で断らない（01 の「外で直したあとの再解決」）。
        let mut state = state_degraded(DegradedReason::NotLoggedIn {
            host: "github.com".to_string(),
        });
        let _ = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));
        let _ = press(&mut state, 'A');

        let cmd = press(&mut state, 'r');

        assert_eq!(cmd, Cmd::ResolveAccount(fake_target()));
        assert!(state.jobs.is_running(JobKind::ResolveAccount));
        assert!(state.notices.is_empty(), "{:?}", state.notices);
    }

    #[test]
    fn the_picker_never_lets_you_choose_when_nothing_reached_the_repository() {
        // **`NoAccess` は並べるだけ**（04 の手順 4）—— どれも届かない状態で選んでも
        // 何も直らない。`j` も `Enter` も効かず、`Esc` で閉じる。
        let mut state =
            state_degraded(fake_no_access(vec![fake_attempt("taro", fake_denied(404))]));
        let _ = press(&mut state, 'A');
        let _ = press(&mut state, 'j');

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::None);
        assert!(state.modal.is_some(), "選べないのに閉じた");

        let _ = press_code(&mut state, KeyCode::Esc);
        assert!(state.modal.is_none(), "Esc で閉じない");
    }

    #[test]
    fn choosing_an_account_is_refused_while_another_git_is_running() {
        // **`submit_exec` を通す。** 閉じてから `Cmd` が落とされると、選んだのに
        // 何も起きない（01 の「アカウントの選び直し」）。
        let mut state = state_degraded(fake_ambiguous(&["taro_acme", "taro"]));
        let _ = state.issue(Cmd::Exec(GitOp::Fetch {
            remote: None,
            prune: false,
        }));
        let _ = press(&mut state, 'A');

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::None);
        assert!(state.modal.is_some(), "落とされたのにモーダルを閉じた");
        let notice = state.notices.back().expect("通知が無い");
        assert!(notice.message.contains("ほかの git"), "{}", notice.message);
        // **断られたら `account` を動かさない**（`submit_account_write`）。落とすと、
        // 頼み直す相手がいないまま `Resolving` に残る。
        assert!(
            matches!(state.account, AccountState::Degraded(_)),
            "{:?}",
            state.account
        );
    }

    /// `.git/config` を書き換える `GitOp` の 2 つ。**どちらも `Reload::Account`。**
    fn account_writes() -> [GitOp; 2] {
        [
            GitOp::SetAccount {
                login: "taro".to_string(),
            },
            GitOp::UnsetAccount,
        ]
    }

    #[test]
    fn writing_the_account_asks_for_the_resolution_again() {
        // 書き込みは `resolve_again` を通る。書いた `mewgit.user` は手順 1 で読まれるので
        // `Resolved(GitConfig)` に、外したあとは手順 2 以降へ落ちる。
        for op in account_writes() {
            let mut state = state_degraded(fake_ambiguous(&["taro_acme", "taro"]));

            let cmd = reply(
                &mut state,
                Cmd::Exec(op.clone()),
                exec_done_of(op.clone(), Ok(fake_output())),
            );

            // **`Cmd::LoadStatus` は出さない**（`Reload::Account`）。index もワークツリーも
            // 触っていないので、出すと空振りの status 走査に加えて `Region::Worktree` の
            // 世代が進み、飛んでいた読み取りが捨てられる。
            assert_eq!(cmd, Cmd::ResolveAccount(fake_target()), "{op:?}");
            // ヘッダの古い degraded を残さない（07 は `Resolving` のあいだ何も出さない）。
            assert!(matches!(state.account, AccountState::Resolving), "{op:?}");
        }
    }

    #[test]
    fn a_write_that_did_not_finish_still_asks_for_the_resolution_again() {
        // **成否を見ない。** 中断では**書けたかどうか判らない**（#56 の「失敗なら
        // ref は元のまま」が中断では成り立たない、と同じ形）。`UnsetAccount` は
        // 失敗の側も同じで、設定されていないキーへの `git config --unset` は
        // **exit 5 を stderr 空**で返す（03 の実測表。実物の git で叩くのは
        // `tests/git_exec.rs`）。読み直せば `.git/config` の実物が答える。
        for op in account_writes() {
            let mut state = state_degraded(fake_ambiguous(&["taro_acme", "taro"]));

            let cmd = reply(
                &mut state,
                Cmd::Exec(op.clone()),
                exec_done_of(
                    op.clone(),
                    Err(ExecError::Canceled {
                        argv: vec!["config".to_string()],
                    }),
                ),
            );

            assert_eq!(cmd, Cmd::ResolveAccount(fake_target()), "{op:?}");
            assert!(matches!(state.account, AccountState::Resolving), "{op:?}");
        }
    }

    #[test]
    fn a_write_from_another_op_leaves_the_account_alone() {
        // **`SetAccount` だけが解決をやり直す。** 全部の書き込みで頼み直すと、
        // stage のたびに `gh` が走る。
        let mut state = state_degraded(fake_ambiguous(&["taro_acme", "taro"]));
        let op = GitOp::Stage(vec!["a.txt".into()]);

        let cmd = reply(
            &mut state,
            Cmd::Exec(op.clone()),
            exec_done_of(op, Ok(fake_output())),
        );

        assert_eq!(cmd, Cmd::LoadStatus);
        assert!(matches!(state.account, AccountState::Degraded(_)));
    }

    /// [`Msg::TargetResolved`] 1 通。中身は [`fake_target`]。
    fn resolved_target(unnormalized: Option<AuthError>) -> Msg {
        Msg::TargetResolved(Box::new(ResolvedTarget {
            target: fake_target(),
            unnormalized,
        }))
    }

    #[test]
    fn repo_info_fills_in_the_remotes_head_and_ref_labels() {
        let mut state = state();
        let _ = init(&mut state);
        let oid = commit(1).oid;
        let info = RepoSnapshot {
            remotes: vec![Remote {
                name: "origin".to_string(),
                url: "git@github.com:owner/repo.git".to_string(),
                head_branch: None,
            }],
            head: HeadInfo::Branch {
                name: "main".to_string(),
                oid,
                upstream: None,
            },
            refs: RefIndex::from_iter([(
                oid,
                RefLabel::Local {
                    name: "main".to_string(),
                    is_head: true,
                },
            )]),
        };

        let _ = update(&mut state, Msg::RepoInfoLoaded(Box::new(Ok(info))));

        assert_eq!(state.repo.remotes.len(), 1);
        assert!(state.repo.head.is_some());
        assert_eq!(state.graph.refs.labels(oid).len(), 1);
        assert!(!state.jobs.is_running(JobKind::LoadRepoInfo));
    }

    #[test]
    fn a_failed_repo_info_notices_and_keeps_going() {
        // ラベルもリモートも無いままグラフは出せる。起動は止めない。
        let mut state = state();
        let _ = init(&mut state);

        let _ = update(
            &mut state,
            Msg::RepoInfoLoaded(Box::new(Err(GitReadError::NonUtf8Head))),
        );

        assert_eq!(state.notices.back().unwrap().level, NoticeLevel::Warn);
        assert!(!state.jobs.is_running(JobKind::LoadRepoInfo));
    }
}
