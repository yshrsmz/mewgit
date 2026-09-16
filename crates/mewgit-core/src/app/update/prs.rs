//! PRs ペインのキー処理と、PR / チェックの受け取り（01 の「PR ペイン」）。
//!
//! 引くものを決めるのは [`AppState::wanted_pr`] と [`AppState::wanted_checks`] の 2 つで、
//! **頼む側と、届いた返信を採るかどうかを決める側の両方がそれを読む**。

use crate::auth::AuthError;
use crate::github::{Check, GithubError, PullRequest};

use super::focus;
use crate::app::{AppState, Cmd, Fetch, JobKind, Key, KeyCode, NoticeLevel, Pane};

/// PRs ペインのキー処理。
pub(super) fn prs_key(state: &mut AppState, key: Key) -> Cmd {
    match key.code() {
        KeyCode::Char('j') | KeyCode::Down => {
            state.prs.move_cursor(1);
            Cmd::None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            state.prs.move_cursor(-1);
            Cmd::None
        }
        KeyCode::Char('o') => open_pr(state),
        // **`Tab` でしか入れないペインにも出口を用意する**（07）。Branches ペインと
        // 同じく、左の選択と無関係なので戻り先は `Graph`。
        KeyCode::Esc => focus(state, Pane::Graph),
        _ => Cmd::None,
    }
}

/// `o`。カーソルのブランチの PR をブラウザで開く（01 の「`o`（ブラウザで開く）」）。
///
/// **PR が無いときに `N`（PR 作成）を案内しない。** `Modal::PrCreate` は次のタスクなので、
/// いま書くと存在しないキーを案内することになる（#63）。
fn open_pr(state: &mut AppState) -> Cmd {
    match state.current_pr() {
        Some(pr) => Cmd::OpenPr { number: pr.number },
        None => {
            state.push_notice(
                NoticeLevel::Info,
                "開ける PR がありません".to_string(),
                None,
            );
            Cmd::None
        }
    }
}

/// 持っている答えがいまのブランチのものでなければ引き直す。
///
/// **呼ぶのは返信を採ったあとだけ**（[`pr_loaded`] / [`checks_loaded`]）。**材料が
/// 変わった入口からは [`reload`] を通す** —— 鍵はブランチ名しか見ないので、
/// **アカウントが変わっても「合っている」と答える**（独立したレビューが拾った）。
pub(super) fn refresh(state: &AppState) -> Cmd {
    // **`Cmd::FetchPr` を組むのは [`reload`] の 1 箇所。** 鍵の作り方を変えたときに
    // 片方だけ直せる形にしない。
    if state.prs.fresh(state.wanted_pr()).is_none() {
        return reload(state);
    }
    fetch_checks(state)
}

/// 材料が変わったので、鍵が同じでも引き直す。
///
/// **入口は 2 つで、材料も 2 つ。** ref（`Msg::RepoInfoLoaded`。commit / push / fetch
/// のあとは同じブランチでもチェックの結果やマージ済みかが変わる）と、**どのアカウント
/// で引いたか**（`Msg::AccountResolved`。`A` で選び直すと、前のトークンでは見えなかった
/// PR が見える）。
///
/// **ここが失敗からの出口でもある。** 引けなかった答えは [`refresh`] では鍵が合って
/// しまって捨てられないので、`f` を押すか `A` で選び直せば引き直せる形にしておく
/// （`CLAUDE.md` の「状態を消すのが『その状態を根拠に断つキー』の先にあるなら、
/// 断つ前に消し方を数える」）。**材料を 1 本しか数えていなかったので、`A` で
/// 選び直しても失敗の文言が戻る穴が開いていた**（独立したレビューが拾った）。
pub(super) fn reload(state: &AppState) -> Cmd {
    match state.wanted_pr() {
        Some(branch) => Cmd::FetchPr {
            branch: branch.to_string(),
        },
        None => Cmd::None,
    }
}

/// チェックをまだ引いていなければ頼む。
fn fetch_checks(state: &AppState) -> Cmd {
    match (state.wanted_checks(), &state.prs.checks) {
        (Some(sha), Fetch::Unasked) => Cmd::FetchChecks {
            sha: sha.to_string(),
        },
        _ => Cmd::None,
    }
}

/// 頼んだブランチの PR が届いた。
///
/// **鍵が合わなければ捨てて頼み直す。** 要求が飛んでいるあいだにブランチが変わると
/// 届いた答えは別のブランチのものになる（01 の「非同期の返りは古いことがある」）。
pub(super) fn pr_loaded(
    state: &mut AppState,
    branch: String,
    result: Result<Option<PullRequest>, GithubError>,
) -> Cmd {
    let asked = state.jobs.finish(JobKind::FetchPr);
    if state.wanted_pr() != Some(branch.as_str()) {
        // **捨てたあとに `refresh` を通す。** ここで返さないと、鍵が食い違って
        // 捨てたぶんを誰も頼み直さない（01 の「実行中ジョブ › 世代」の (a)）。
        // 続きを頼む: 鍵の合わなかった返信のぶん。
        return refresh(state);
    }
    let fetched = match result {
        Ok(pr) => Fetch::Loaded(pr),
        Err(err) => Fetch::Failed(err),
    };
    state.prs.set_current(branch, fetched);
    // **採ったうえで頼み直す**（`repo_info_loaded` と同じ形。01 の「実行中ジョブ ›
    // 世代」）。書き込みの最中に飛んでいた要求の答えは**書き込み前の GitHub の状態**
    // だが、出せるものは出したままにして裏で引き直す。
    // 続きを頼む: 書き込みに追い越された `FetchPr`。
    if state.jobs.is_fresh(JobKind::FetchPr, asked) {
        fetch_checks(state)
    } else {
        reload(state)
    }
}

/// 頼んだ head sha のチェック一覧が届いた。
pub(super) fn checks_loaded(
    state: &mut AppState,
    sha: String,
    result: Result<Vec<Check>, GithubError>,
) -> Cmd {
    let asked = state.jobs.finish(JobKind::FetchChecks);
    if state.wanted_checks() != Some(sha.as_str()) {
        // 続きを頼む: 鍵の合わなかった返信のぶん。
        return refresh(state);
    }
    if !state.jobs.is_fresh(JobKind::FetchChecks, asked) {
        // **PR ごと引き直す。** チェックの鍵（head sha）は `current` から導くので、
        // 書き込みで PR の側が進んでいれば古い sha を頼み直しても同じものが返る。
        // 続きを頼む: 書き込みに追い越された `FetchChecks`。
        return reload(state);
    }
    state.prs.checks = match result {
        Ok(checks) => Fetch::Loaded(checks),
        Err(err) => Fetch::Failed(err),
    };
    // **カーソルは戻さない。** ここへ来た時点で必ず 0 —— `set_current` が 0 にして
    // `checks` を `Unasked` に落としており、`move_cursor` が動かせるのは `Loaded` の
    // あいだだけ（`PrState`）。書くと「ここでも戻している」という嘘の保険が残る。
    Cmd::None
}

/// `gh browse` が終わった。
///
/// **`Ok` では何も出さない。** ブラウザが前に出てくるので、画面に足す言葉が無い。
pub(super) fn open_pr_finished(state: &mut AppState, result: Result<(), AuthError>) -> Cmd {
    state.jobs.finish(JobKind::OpenPr);
    if let Err(err) = result {
        // `AuthError::GhFailed` は argv と出力を持っている（04）。
        state.push_notice(NoticeLevel::Error, err.to_string(), None);
    }
    Cmd::None
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{cells, press, state, state_with_commits};
    use super::super::update;
    use super::*;
    use crate::app::{Msg, NoticeLevel, Region, TargetState};
    use crate::auth::{AccountState, DegradedReason};
    use crate::config::Config;
    use crate::git_read::HeadInfo;
    use crate::github::CheckState;
    use crate::test_util::{
        checks_loaded, fake_account, fake_check, fake_oid, fake_pull_request, fake_target,
        pr_loaded as pr_loaded_msg, put_on_branch, reply,
    };

    /// アカウントと対象が決まり、HEAD が `branch` に居る状態。PR はまだ引いていない。
    fn state_on_branch(branch: &str) -> AppState {
        let mut state = state_with_commits(2);
        put_on_branch(&mut state, branch);
        state
    }

    #[test]
    fn a_pr_is_asked_for_only_once_both_the_account_and_the_branch_are_known() {
        // **材料は 2 つとも要る。** `wanted_pr()` が `None` を返すあいだは頼まない
        // （01 の「PR ペイン › 何を引くか」）。
        let mut state = state_with_commits(2);
        assert_eq!(refresh(&state), Cmd::None, "対象もアカウントも無い");

        state.repo.target = TargetState::Resolved(fake_target());
        assert_eq!(refresh(&state), Cmd::None, "アカウントがまだ");

        state.account = AccountState::Resolved(fake_account("taro"));
        assert_eq!(refresh(&state), Cmd::None, "HEAD がまだ");

        state.repo.head = Some(HeadInfo::Branch {
            name: "feat/x".to_string(),
            oid: fake_oid(0),
        });
        assert_eq!(
            refresh(&state),
            Cmd::FetchPr {
                branch: "feat/x".to_string()
            }
        );
    }

    #[test]
    fn a_detached_head_never_asks_for_a_pr() {
        // API が要るのはブランチ名で、切り離された HEAD には無い。
        let mut state = state_on_branch("feat/x");
        state.repo.head = Some(HeadInfo::Detached(fake_oid(1)));
        assert_eq!(refresh(&state), Cmd::None);
        assert_eq!(state.wanted_pr(), None);

        // コミットが 1 つも無いブランチも同じ。push されていないので PR は無い。
        state.repo.head = Some(HeadInfo::Unborn {
            name: "main".to_string(),
        });
        assert_eq!(refresh(&state), Cmd::None);
    }

    #[test]
    fn either_arrival_order_of_the_account_and_the_refs_asks_for_the_pr() {
        // **#63 と同じ形の見張り。** 材料が 2 本の非同期の返信から来るので、
        // どちらが先に届いても同じ答えになることを両方の順序で回す。
        let wanted = Cmd::FetchPr {
            branch: "feat/x".to_string(),
        };

        // アカウントが後。
        let mut state = state_with_commits(2);
        state.repo.target = TargetState::Resolved(fake_target());
        state.repo.head = Some(HeadInfo::Branch {
            name: "feat/x".to_string(),
            oid: fake_oid(0),
        });
        let cmd = update(&mut state, Msg::AccountResolved(Ok(fake_account("taro"))));
        assert!(issued_pr(&cmd, &wanted), "{cmd:?}");

        // HEAD が後。`repo_info_loaded` の側から出る。
        let mut state = state_with_commits(2);
        state.repo.target = TargetState::Resolved(fake_target());
        let _ = update(&mut state, Msg::AccountResolved(Ok(fake_account("taro"))));
        let cmd = update(
            &mut state,
            Msg::RepoInfoLoaded(Box::new(Ok(crate::app::RepoSnapshot {
                remotes: Vec::new(),
                head: HeadInfo::Branch {
                    name: "feat/x".to_string(),
                    oid: fake_oid(0),
                },
                refs: crate::git_read::RefIndex::default(),
            }))),
        );
        assert!(issued_pr(&cmd, &wanted), "{cmd:?}");
    }

    /// `Cmd::Batch` の中も見る。
    fn issued_pr(cmd: &Cmd, wanted: &Cmd) -> bool {
        match cmd {
            Cmd::Batch(cmds) => cmds.iter().any(|c| c == wanted),
            other => other == wanted,
        }
    }

    #[test]
    fn a_loaded_pr_asks_for_its_checks() {
        let mut state = state_on_branch("feat/x");
        let ask = refresh(&state);
        let next = reply(
            &mut state,
            ask,
            pr_loaded_msg("feat/x", Some(fake_pull_request(82, "feat/x", "headsha"))),
        );
        assert_eq!(
            next,
            Cmd::FetchChecks {
                sha: "headsha".to_string()
            }
        );
        // 鍵は `current` から導く（別に持たせない）。
        assert_eq!(state.wanted_checks(), Some("headsha"));
    }

    #[test]
    fn a_branch_without_a_pr_asks_for_nothing_more() {
        let mut state = state_on_branch("feat/x");
        let ask = refresh(&state);
        let next = reply(&mut state, ask, pr_loaded_msg("feat/x", None));
        assert_eq!(next, Cmd::None);
        assert_eq!(state.wanted_checks(), None);
        // **「PR が無い」と「まだ引いていない」を分ける**（`Fetch` の 3 状態）。
        assert!(matches!(state.prs.current, Fetch::Loaded(None)));
    }

    #[test]
    fn a_reply_for_another_branch_is_dropped_and_asked_again() {
        // 要求が飛んでいるあいだにブランチが変わると、届いた答えは別のブランチのもの。
        let mut state = state_on_branch("feat/x");
        let ask = refresh(&state);
        state.repo.head = Some(HeadInfo::Branch {
            name: "feat/y".to_string(),
            oid: fake_oid(1),
        });
        let next = reply(
            &mut state,
            ask,
            pr_loaded_msg("feat/x", Some(fake_pull_request(82, "feat/x", "headsha"))),
        );

        assert!(state.prs.branch.is_none(), "捨てていない");
        assert_eq!(
            next,
            Cmd::FetchPr {
                branch: "feat/y".to_string()
            },
            "頼み直していない"
        );
    }

    #[test]
    fn a_reply_for_another_sha_is_dropped() {
        let mut state = state_on_branch("feat/x");
        let ask = refresh(&state);
        let next = reply(
            &mut state,
            ask,
            pr_loaded_msg("feat/x", Some(fake_pull_request(82, "feat/x", "headsha"))),
        );
        let _ = reply(
            &mut state,
            next,
            checks_loaded("oldsha", vec![fake_check("build", CheckState::Success)]),
        );
        assert!(
            matches!(state.prs.checks, Fetch::Unasked),
            "{:?}",
            state.prs.checks
        );
    }

    #[test]
    fn a_new_pr_drops_the_checks_of_the_old_one() {
        // チェックは `current` の head sha に紐付いていて、鍵を別に持たせていない。
        let mut state = state_on_branch("feat/x");
        let ask = refresh(&state);
        let next = reply(
            &mut state,
            ask,
            pr_loaded_msg("feat/x", Some(fake_pull_request(82, "feat/x", "headsha"))),
        );
        let _ = reply(
            &mut state,
            next,
            checks_loaded("headsha", vec![fake_check("build", CheckState::Success)]),
        );
        assert!(matches!(state.prs.checks, Fetch::Loaded(_)));

        // ref が動いて引き直した（`reload`）。新しい答えが入ると古いチェックは落ちる。
        let again = reload(&state);
        let _ = reply(
            &mut state,
            again,
            pr_loaded_msg("feat/x", Some(fake_pull_request(82, "feat/x", "newsha"))),
        );
        assert!(
            matches!(state.prs.checks, Fetch::Unasked),
            "{:?}",
            state.prs.checks
        );
    }

    #[test]
    fn a_failed_fetch_is_remembered_and_the_material_that_changed_asks_again() {
        // **失敗を状態に持つ**（01 の「失敗の記憶」）。鍵が合っているので `refresh`
        // は頼み直さない —— 回すと「失敗 → 再読込 → 失敗」になる。
        let mut state = state_on_branch("feat/x");
        let ask = refresh(&state);
        let _ = reply(
            &mut state,
            ask,
            Msg::PrLoaded {
                branch: "feat/x".to_string(),
                result: Box::new(Err(GithubError::NotFound)),
            },
        );
        assert!(matches!(state.prs.current, Fetch::Failed(_)));
        assert_eq!(refresh(&state), Cmd::None, "失敗を頼み直している");

        // **出口は材料が変わったとき。** `f`（ref）でも `A`（アカウント）でも
        // 引き直せる（`reload` の doc）。
        assert_eq!(
            reload(&state),
            Cmd::FetchPr {
                branch: "feat/x".to_string()
            }
        );
    }

    #[test]
    fn picking_another_account_asks_for_the_pr_again() {
        // **鍵はブランチ名しか見ない**ので、`refresh` に通すと「合っている」と
        // 答えて前のトークンで引けなかった答えが残る（独立したレビューが拾った）。
        // **`A` の流れからは、そこを抜ける手掛かりが 1 つも無い。**
        let mut state = state_on_branch("feat/x");
        let ask = reload(&state);
        let _ = reply(
            &mut state,
            ask,
            Msg::PrLoaded {
                branch: "feat/x".to_string(),
                result: Box::new(Err(GithubError::NotFound)),
            },
        );
        assert!(matches!(state.prs.current, Fetch::Failed(_)));

        // `A` → 選び直し → `GitOp::SetAccount` → `resolve_again` → 解決し直し。
        let cmd = update(&mut state, Msg::AccountResolved(Ok(fake_account("hanako"))));

        assert!(
            issued_pr(
                &cmd,
                &Cmd::FetchPr {
                    branch: "feat/x".to_string()
                }
            ),
            "アカウントが変わったのに頼み直していない: {cmd:?}"
        );
    }

    #[test]
    fn a_fetch_overtaken_by_a_write_is_asked_again() {
        // **鍵が合っていても古いことがある。** 書き込みの最中に飛んでいた要求の答えは
        // 書き込み**前**の GitHub の状態で、そのあとの引き直しは `AppState::issue` に
        // 落とされている（通知も出ない）。刻印で拾う（01 の「実行中ジョブ › 世代」）。
        let mut state = state_on_branch("feat/x");
        // 1 本目が飛んでいる。**返信はまだ届けない。**
        let _ = state.issue(reload(&state));
        assert!(state.jobs.is_running(JobKind::FetchPr));

        // 書き込みが終わって ref が動いた。ここで出る `Cmd::FetchPr` は落とされる。
        state.jobs.advance(Region::Refs);
        let dropped = state.issue(reload(&state));
        assert_eq!(dropped, Cmd::None, "落とされていない");

        // 1 本目の答えが届く。**採ったうえで頼み直す。**
        let next = update(
            &mut state,
            pr_loaded_msg("feat/x", Some(fake_pull_request(82, "feat/x", "headsha"))),
        );

        assert!(state.current_pr().is_some(), "出せるものを捨てている");
        assert_eq!(
            next,
            Cmd::FetchPr {
                branch: "feat/x".to_string()
            },
            "追いついていない"
        );
    }

    #[test]
    fn checks_overtaken_by_a_write_ask_for_the_pr_again() {
        let mut state = state_on_branch("feat/x");
        let ask = reload(&state);
        let next = reply(
            &mut state,
            ask,
            pr_loaded_msg("feat/x", Some(fake_pull_request(82, "feat/x", "headsha"))),
        );
        let _ = state.issue(next);
        assert!(state.jobs.is_running(JobKind::FetchChecks));

        state.jobs.advance(Region::Refs);
        let again = update(
            &mut state,
            checks_loaded("headsha", vec![fake_check("build", CheckState::Success)]),
        );

        // **PR ごと引き直す。** head sha が進んでいれば、古い sha を頼み直しても
        // 同じものが返る。
        assert_eq!(
            again,
            Cmd::FetchPr {
                branch: "feat/x".to_string()
            }
        );
    }

    #[test]
    fn o_opens_the_pr_and_says_so_when_there_is_none() {
        let mut state = state_on_branch("feat/x");
        state.focus = Pane::Prs;

        // 引く前は開けない。
        let cmd = press(&mut state, 'o');
        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.notices.len(), 1);
        assert_eq!(state.notices[0].level, NoticeLevel::Info);

        let ask = refresh(&state);
        let _ = reply(
            &mut state,
            ask,
            pr_loaded_msg("feat/x", Some(fake_pull_request(82, "feat/x", "headsha"))),
        );
        assert_eq!(press(&mut state, 'o'), Cmd::OpenPr { number: 82 });
    }

    #[test]
    fn pressing_o_while_the_browser_is_starting_says_so_instead_of_nothing() {
        // **`gh browse` は起動したブラウザが終わるまで返らない**（gh 2.100.0 で実測。
        // `BROWSER` に 5 秒眠るスクリプトを置くと 5.06 秒）。端末ブラウザや自分で
        // 抜けないラッパーを `BROWSER` にしている環境では、そのあいだ `o` が
        // 無反応になる（#63 の「無反応よりまし」で止めない）。
        let mut state = state_on_branch("feat/x");
        let ask = reload(&state);
        let _ = reply(
            &mut state,
            ask,
            pr_loaded_msg("feat/x", Some(fake_pull_request(82, "feat/x", "headsha"))),
        );
        state.focus = Pane::Prs;

        // 1 回目は出る（`press` は `update` の出口を通るので登録まで済む）。
        assert_eq!(press(&mut state, 'o'), Cmd::OpenPr { number: 82 });
        assert!(state.jobs.is_running(JobKind::OpenPr));
        state.notices.clear();

        // 2 回目は落とされる。**黙らない。**
        assert_eq!(press(&mut state, 'o'), Cmd::None);
        assert_eq!(state.notices.len(), 1, "黙って落としている");
        let message = state.notices[0].message.clone();
        assert!(message.contains("ブラウザ"), "{message}");
        // ステータスバーの予算（01 の「失敗の案内文」）。
        assert!(cells(&message) <= 72, "{} セル: {message}", cells(&message));
    }

    #[test]
    fn a_failed_browse_reaches_the_notices() {
        // **argv と出力を握り潰さない**（不変ルール 4 と同じ趣旨）。
        let mut state = state_on_branch("feat/x");
        let err = crate::auth::AuthError::GhFailed {
            argv: vec!["gh".to_string(), "browse".to_string()],
            message: "exit status 4".to_string(),
        };
        let _ = open_pr_finished(&mut state, Err(err));

        assert_eq!(state.notices.len(), 1);
        assert_eq!(state.notices[0].level, NoticeLevel::Error);
        assert!(
            state.notices[0].message.contains("browse"),
            "{:?}",
            state.notices[0]
        );
        assert!(!state.jobs.is_running(JobKind::OpenPr));
    }

    #[test]
    fn j_and_k_move_the_check_cursor_within_the_list() {
        let mut state = state_on_branch("feat/x");
        let ask = refresh(&state);
        let next = reply(
            &mut state,
            ask,
            pr_loaded_msg("feat/x", Some(fake_pull_request(82, "feat/x", "headsha"))),
        );
        let _ = reply(
            &mut state,
            next,
            checks_loaded(
                "headsha",
                vec![
                    fake_check("build", CheckState::Success),
                    fake_check("test", CheckState::Failure),
                ],
            ),
        );
        state.focus = Pane::Prs;

        let _ = press(&mut state, 'j');
        assert_eq!(state.prs.cursor, 1);
        // 末尾より先へは進まない。
        let _ = press(&mut state, 'j');
        assert_eq!(state.prs.cursor, 1);
        let _ = press(&mut state, 'k');
        assert_eq!(state.prs.cursor, 0);
    }

    #[test]
    fn a_degraded_account_asks_for_nothing() {
        let mut state = state_on_branch("feat/x");
        state.account = AccountState::Degraded(DegradedReason::GhMissing);
        assert_eq!(refresh(&state), Cmd::None);
        assert_eq!(reload(&state), Cmd::None);
    }

    #[test]
    fn github_disabled_never_starts_gh_and_drops_the_prs_pane() {
        // 旗を読むのは `AppState::new` の 1 つ（01 の「GitHub 機能を切る」）。
        let default_state = state();
        let mut config = Config::default();
        config.github.enabled = false;
        let mut state = AppState::new(crate::app::RepoInfo::default(), config);
        assert!(matches!(state.account, AccountState::Disabled));
        assert!(!state.focus_candidates().contains(&Pane::Prs));

        // `ssh -G` の結果が来ても `Cmd::ResolveAccount` は出ない。
        let cmd = update(
            &mut state,
            Msg::TargetResolved(Box::new(crate::auth::ResolvedTarget {
                target: fake_target(),
                unnormalized: None,
            })),
        );
        assert_eq!(cmd, Cmd::None);
        assert!(matches!(state.account, AccountState::Disabled));
        // 旗が立っていれば候補に入る（この見張りが空振りしていないことの確認）。
        assert!(default_state.focus_candidates().contains(&Pane::Prs));
    }
}
