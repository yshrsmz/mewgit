//! アカウントとリポジトリ対象の解決（04）。
//!
//! `Cmd::ResolveTarget` → `Cmd::ResolveAccount` の連鎖と、`A` が出すもの。
//! 外の世界（`gh` と GitHub API）はランタイムの向こう側にあるので、ここは
//! 返ってきた結果を [`AppState`] に写して次に何を頼むかを決めるだけ。

use crate::auth::url::RemoteUrl;
use crate::auth::{Account, AccountSource, AccountState, DegradedReason, ResolvedTarget};
use crate::git_exec::GitOp;
use crate::git_read::GitReadError;

use super::remote::conventional_remote;
use super::submit_exec;
use crate::app::{AppState, Cmd, JobKind, Modal, NoticeLevel, RepoSnapshot, TargetState};

/// `A`。アカウント解決の結果を出す（01 の「アカウントの選び直し」）。
///
/// **モーダルを開くのは `Degraded` のときだけ。** 残る 2 つは出す中身が 1 行で、
/// モーダルにしても選ぶものが無い。
pub(super) fn open_account(state: &mut AppState) -> Cmd {
    match account_view(&state.account, &state.repo.target) {
        Ok(modal) => state.modal = Some(modal),
        Err(message) => state.push_notice(NoticeLevel::Info, message, None),
    }
    Cmd::None
}

/// `A` が出すもの。`Ok` がモーダル、`Err` が通知の文言（01 の表）。
///
/// **`AccountState::Resolving` を割らずに `target` を一緒に読む**（04 の
/// 「内訳を出すのは `Modal::AccountPicker`」）。`Resolving` が兼ねている
/// 「走っている最中」と「永久に頼まれない」を見分けるのは [`TargetState`] のほうで、
/// あれは**まさにその理由で 3 値**にしてある。割ると同じ事実が 2 箇所に出る。
fn account_view(account: &AccountState, target: &TargetState) -> Result<Modal, String> {
    match account {
        AccountState::Degraded(reason) => Ok(Modal::AccountPicker {
            reason: reason.clone(),
            cursor: 0,
        }),
        AccountState::Resolved(account) => Err(format!(
            "{} @ {} を使用（{}）",
            account.login,
            account.host,
            source_label(account.source)
        )),
        // **`Unavailable` にだけ理由を出す。** 走っている最中は待てば変わるので、
        // 「次に何をするか」は無い（案内が求める操作を自分が持っているか、の逆側）。
        AccountState::Resolving => Err(match target {
            TargetState::Unavailable => {
                "既定リモートから GitHub のリポジトリを特定できません".to_string()
            }
            TargetState::Unasked | TargetState::Resolved(_) => {
                "アカウントを解決しています".to_string()
            }
        }),
    }
}

/// [`AccountSource`] を通知に出す語（04 の手順 1〜4）。
///
/// **`Config` は置き場所まで出す。** mewgit から選び直す手段がまだ無いので
/// （01 の「アカウントの選び直し」）、**どこを直せば変わるかを言えるのはここだけ**
/// —— 指示を足すのはその操作を持たない側（#65）。**どちらの指定かは判らない**ので
/// 2 つとも出す（`AccountSource::Config` は `.git/config` と設定ファイルの
/// `[[repos]]` を兼ねている）。
fn source_label(source: AccountSource) -> &'static str {
    match source {
        AccountSource::Config => "設定: mewgit.user / [[repos]]",
        AccountSource::Single => "ログインが 1 つだけ",
        AccountSource::Probe => "届いたのが 1 つだけ",
    }
}

/// [`Modal::AccountPicker`] の `Enter`。選んだ login を `.git/config` に書く。
///
/// **選べるのは `Ambiguous` のときだけ**（[`DegradedReason::candidates`] が空なら
/// 何もしない）。`NoAccess` を選ばせないのは、どれも届かない状態でアカウントを
/// 選んでも何も直らないから（04 の手順 4）。
pub(super) fn account_picked(state: &mut AppState) -> Cmd {
    let Some(Modal::AccountPicker { reason, cursor }) = state.modal.as_ref() else {
        return Cmd::None;
    };
    let Some(login) = reason.candidates().get(*cursor).map(|c| c.login.clone()) else {
        return Cmd::None;
    };
    // **[`submit_exec`] を通す。** ほかの `git` が走っているあいだは閉じない
    // —— 閉じてから `Cmd` が落とされると、選んだのに何も起きない。
    submit_exec(state, GitOp::SetAccount { login })
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
            resolve_target(state)
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
    // **`Cmd::ResolveAccount` の 1 つ目の発行元**（2 つ目は [`resolve_again`]）。
    // `init` からは出せない —— `auth::resolve` は `RepoTarget` を要るので、
    // `Msg::RepoInfoLoaded` → `Cmd::ResolveTarget` → ここ、と 2 往復待ってからでないと
    // 材料が揃わない（01 の「`RepoInfo.target` を埋める」）。
    let account = Cmd::ResolveAccount(resolved.target.clone());
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
        Err(reason) => AccountState::Degraded(reason),
    };
    Cmd::None
}

/// `.git/config` にアカウントを書いたあとの読み直し（01 の「アカウントの選び直し」）。
///
/// **`Cmd::ResolveAccount` の 2 つ目の発行元**（1 つ目は [`target_resolved`]）。
pub(super) fn resolve_again(state: &mut AppState) -> Cmd {
    // **`target` が無ければ何もしない。** `Degraded` は `target` が決まったあとにしか
    // 出ないのでここには来ないが、来たときに `Resolving` へ落とすと
    // 頼み直す相手がいないまま**ヘッダから `[gh: …]` が消えたきり**になる。
    let Some(target) = state.repo.target.resolved().cloned() else {
        return Cmd::None;
    };
    state.account = AccountState::Resolving;
    Cmd::ResolveAccount(target)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        cells, commit, exec_done_of, fake_output, fetched_remote, issued, press, press_code,
        repo_snapshot, state, state_with_commits, tracking_branch, with_branches, HEADS_MAIN,
    };
    use super::super::{init, update};
    use super::*;
    use crate::app::{KeyCode, Modal, Msg};
    use crate::auth::{AttemptFailure, AuthError, ProbeAttempt};

    use crate::git_exec::ExecError;
    use crate::git_read::{HeadInfo, RefIndex, RefLabel, Remote};

    use crate::test_util::{fake_account, fake_ambiguous, fake_target, reply};

    /// リモートを載せた `RepoInfoLoaded` を 1 通届ける。
    ///
    /// **`reply` を通すのは、ランタイムでは `Cmd` が必ず `AppState::issue` を通るから**
    /// （ヘルパが実物では起きない形を作らないため）。
    fn deliver_repo_info(state: &mut AppState, remotes: &[(&str, &str)]) -> Cmd {
        let info = RepoSnapshot {
            remotes: remotes
                .iter()
                .map(|(name, url)| Remote {
                    name: (*name).to_string(),
                    url: (*url).to_string(),
                })
                .collect(),
            ..repo_snapshot()
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
        // **`Cmd::ResolveAccount` の発行元は `target_resolved` の 1 箇所**（01）。
        // 既定リモートが決まらないリポジトリでは `target` が `Unavailable` に落ちるので、
        // アカウントは一度も頼まれず `Resolving` のまま残る（ヘッダは何も出さない。07）。
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

        assert_eq!(cmd, Cmd::None);
        // **理由をそのまま持つ。** 候補を写すと `state.account` と 2 箇所になる（01）。
        assert_eq!(
            state.modal,
            Some(Modal::AccountPicker {
                // **理由をそのまま持つ**ので、`Ambiguous` の中身も一致する。
                reason: fake_ambiguous(&["taro_acme", "taro"]),
                cursor: 0
            })
        );
    }

    #[test]
    fn a_says_which_account_is_in_use_instead_of_opening_a_picker() {
        // `Resolved` は選ぶものが無いので通知 1 行。モーダルにする理由が無い（01 の表）。
        let mut state = state();
        state.account = AccountState::Resolved(fake_account("taro"));

        let cmd = press(&mut state, 'A');

        assert_eq!(cmd, Cmd::None);
        assert!(state.modal.is_none(), "選ぶものが無いのに開いた");
        let notice = state.notices.back().expect("通知が無い");
        assert!(notice.message.contains("taro"), "{}", notice.message);
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
    fn every_notice_that_a_prints_fits_the_status_bar() {
        // **72 セルはステータスバーの予算**（01 の「失敗の案内文」）。`A` は通知の
        // **新しい産出元**なので、`exec_advice` を回している見張りは自分では拾わない
        // （CLAUDE.md の「共有した見張りは新しい産出元を自分では拾わない」）。
        //
        // **いちばん長いのは `Resolved(Config)`** —— 選び直す手段がまだ無いので、
        // どこを直せば変わるかまで出している（01）。login は EMU の shortcode を
        // 想定して長めにする。
        let mut configured = fake_account("yamada_taro_acme");
        configured.source = AccountSource::Config;
        let cases = [
            (AccountState::Resolved(configured), TargetState::Unasked),
            (
                AccountState::Resolved(fake_account("taro")),
                TargetState::Unasked,
            ),
            (AccountState::Resolving, TargetState::Unavailable),
            (
                AccountState::Resolving,
                TargetState::Resolved(fake_target()),
            ),
        ];

        for (account, target) in cases {
            let message = account_view(&account, &target).expect_err("モーダルを開いている");
            assert!(cells(&message) <= 72, "{} セル: {message}", cells(&message));
        }
        // **login の長さに上限は無い**ので「いちばん長い形」は作れない。ここが
        // 見張るのは、現実的な長さで予算に収まるところまで（ヘッダの
        // `every_degraded_reason_fits_the_header` と同じ数え方）。
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

    #[test]
    fn the_picker_never_lets_you_choose_when_nothing_reached_the_repository() {
        // **`NoAccess` は並べるだけ**（04 の手順 4）—— どれも届かない状態で選んでも
        // 何も直らない。`j` も `Enter` も効かず、`Esc` で閉じる。
        let mut state = state_degraded(DegradedReason::NoAccess {
            attempts: vec![ProbeAttempt {
                login: "taro".to_string(),
                failure: AttemptFailure::Denied { status: 404 },
            }],
        });
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
    }

    #[test]
    fn writing_the_account_asks_for_the_resolution_again() {
        // **`Cmd::ResolveAccount` の 2 つ目の発行元。** 書いた `mewgit.user` は
        // 手順 1 で読まれるので、`Resolved(source: Config)` に落ち着く。
        let mut state = state_degraded(fake_ambiguous(&["taro_acme", "taro"]));
        let op = GitOp::SetAccount {
            login: "taro".to_string(),
        };

        let cmd = reply(
            &mut state,
            Cmd::Exec(op.clone()),
            exec_done_of(op, Ok(fake_output())),
        );

        // **`Cmd::LoadStatus` は出さない**（`Reload::Account`）。index もワークツリーも
        // 触っていないので、出すと空振りの status 走査に加えて `Region::Worktree` の
        // 世代が進み、飛んでいた読み取りが捨てられる。
        assert_eq!(cmd, Cmd::ResolveAccount(fake_target()));
        // ヘッダの古い degraded を残さない（07 は `Resolving` のあいだ何も出さない）。
        assert!(matches!(state.account, AccountState::Resolving));
    }

    #[test]
    fn a_write_that_did_not_finish_still_asks_for_the_resolution_again() {
        // **成否を見ない。** 中断では**書けたかどうか判らない**（#56 の「失敗なら
        // ref は元のまま」が中断では成り立たない、と同じ形）。読み直せば
        // `.git/config` の実物が答える。
        let mut state = state_degraded(fake_ambiguous(&["taro_acme", "taro"]));
        let op = GitOp::SetAccount {
            login: "taro".to_string(),
        };

        let cmd = reply(
            &mut state,
            Cmd::Exec(op.clone()),
            exec_done_of(
                op,
                Err(ExecError::Canceled {
                    argv: vec!["config".to_string()],
                }),
            ),
        );

        assert_eq!(cmd, Cmd::ResolveAccount(fake_target()));
        assert!(matches!(state.account, AccountState::Resolving));
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
            }],
            head: HeadInfo::Branch {
                name: "main".to_string(),
                oid,
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
