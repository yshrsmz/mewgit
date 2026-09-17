//! PRs ペインのキー処理と、PR 一覧の受け取り（01 の「PR ペイン」）。
//!
//! 引くかどうかを決めるのは [`AppState::wanted_pr_list`] で、**頼む側と、文言を出すか
//! 一覧を描くかを決める `ui` の両方がそれを読む**。

use crate::auth::AuthError;
use crate::github::{GithubError, PullRequest};

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

/// `o`。カーソル行の PR をブラウザで開く（01 の「`o`（ブラウザで開く）」）。
///
/// **一覧が空のときに `N`（PR 作成）を案内しない。** `Modal::PrCreate` はまだ無いので、
/// いま書くと存在しないキーを案内することになる（#63）。
fn open_pr(state: &mut AppState) -> Cmd {
    match state.prs.selected() {
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

/// 材料が揃っていれば一覧を頼む。
///
/// **入口は 2 つで、材料も 2 つ。** ref（`Msg::RepoInfoLoaded`。commit / push のあとは
/// GitHub 側の PR と CI が動いており、マージされていれば open から消える）と、
/// **どのアカウントで引いたか**（`Msg::AccountResolved`。`A` で選び直すと、前の
/// トークンでは見えなかった PR が見える）。
///
/// **ここが失敗からの出口でもある。** 引けなかった答えは自分では落ちないので、
/// `f` を押すか `A` で選び直せば引き直せる形にしておく（`CLAUDE.md` の「状態を消すのが
/// 『その状態を根拠に断つキー』の先にあるなら、断つ前に消し方を数える」）。
pub(super) fn reload(state: &AppState) -> Cmd {
    if state.wanted_pr_list() {
        Cmd::FetchPrList
    } else {
        Cmd::None
    }
}

/// 一覧が届いた。
///
/// **鍵は無いが世代は見る**（01 の「何を引くか」）。書き込みの最中に飛んでいた要求の
/// 答えは**書き込み前の GitHub の状態**だが、出せるものは出したままにして裏で引き直す
/// （`repo_info_loaded` と同じ形）。
pub(super) fn pr_list_loaded(
    state: &mut AppState,
    result: Result<Vec<PullRequest>, GithubError>,
) -> Cmd {
    let asked = state.jobs.finish(JobKind::FetchPrList);
    // **入れ替える前のカーソル行を憶える。** 並びは `sort=updated` なので、ref を
    // 動かす書き込みのあとは添字が別の PR を指す（`PrState::set_list`）。
    let keep = state.prs.selected().map(|pr| pr.number);
    let fetched = match result {
        Ok(list) => Fetch::Loaded(list),
        Err(err) => Fetch::Failed(err),
    };
    state.prs.set_list(fetched);
    // **座り直す先は 2 つ。** 既に一覧を持っていたならカーソル行の PR、初めて持った
    // （起動直後 / 失敗のあと）ならいま居るブランチの PR（01 の「現ブランチの PR」）。
    let seat = keep.or_else(|| state.current_pr().map(|pr| pr.number));
    state.prs.seat(seat);
    if state.jobs.is_fresh(JobKind::FetchPrList, asked) {
        Cmd::None
    } else {
        // 続きを頼む: 書き込みに追い越された `FetchPrList`。
        reload(state)
    }
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
    use crate::app::{Msg, NoticeLevel, Region, RepoSnapshot, TargetState};
    use crate::auth::{AccountState, DegradedReason};
    use crate::config::Config;
    use crate::git_read::{HeadInfo, RefIndex};
    use crate::github::RepoRef;
    use crate::test_util::{
        fake_account, fake_oid, fake_pull_request, fake_target, pr_list_loaded as list_msg,
        put_on_branch, reply,
    };

    /// アカウントと対象が決まり、HEAD が `branch` に居る状態。一覧はまだ引いていない。
    fn state_on_branch(branch: &str) -> AppState {
        let mut state = state_with_commits(2);
        put_on_branch(&mut state, branch);
        state
    }

    /// `Cmd::Batch` の中も見る。
    fn issued(cmd: &Cmd, wanted: &Cmd) -> bool {
        match cmd {
            Cmd::Batch(cmds) => cmds.iter().any(|c| c == wanted),
            other => other == wanted,
        }
    }

    /// 一覧を届けて、カーソルの座り先まで進める。
    fn deliver(state: &mut AppState, list: Vec<PullRequest>) {
        let ask = reload(state);
        let _ = reply(state, ask, list_msg(list));
    }

    #[test]
    fn the_list_is_asked_for_only_once_both_the_account_and_the_target_are_known() {
        // **材料は 2 つとも要る。** `wanted_pr_list()` が偽のあいだは頼まない
        // （01 の「PR ペイン › 何を引くか」）。
        let mut state = state_with_commits(2);
        assert_eq!(reload(&state), Cmd::None, "対象もアカウントも無い");

        state.repo.target = TargetState::Resolved(fake_target());
        assert_eq!(reload(&state), Cmd::None, "アカウントがまだ");

        state.account = AccountState::Resolved(fake_account("taro"));
        assert_eq!(reload(&state), Cmd::FetchPrList);
    }

    #[test]
    fn a_detached_head_still_asks_for_the_list() {
        // **HEAD は材料ではない。** 一覧はブランチに紐づかないので、detached でも
        // unborn でも同じものが出る（#83 の「HEAD のブランチの PR 1 件」との違い）。
        let mut state = state_on_branch("feat/x");
        state.repo.head = Some(HeadInfo::Detached(fake_oid(1)));
        assert_eq!(reload(&state), Cmd::FetchPrList);

        state.repo.head = Some(HeadInfo::Unborn {
            name: "main".to_string(),
        });
        assert_eq!(reload(&state), Cmd::FetchPrList);

        // HEAD がまだ届いていなくても頼む。
        state.repo.head = None;
        assert_eq!(reload(&state), Cmd::FetchPrList);
    }

    #[test]
    fn either_arrival_order_of_the_target_and_the_account_asks_for_the_list() {
        // **#63 と同じ形の見張り。** 材料が 2 本の非同期の返信から来るので、
        // どちらが先に届いても同じ答えになることを両方の順序で回す。

        // 対象が先、アカウントが後。
        let mut state = state_with_commits(2);
        state.repo.target = TargetState::Resolved(fake_target());
        let cmd = update(&mut state, Msg::AccountResolved(Ok(fake_account("taro"))));
        assert!(issued(&cmd, &Cmd::FetchPrList), "{cmd:?}");

        // アカウントが先、対象が後。**片方だけでは頼まない。**
        let mut state = state_with_commits(2);
        let cmd = update(&mut state, Msg::AccountResolved(Ok(fake_account("taro"))));
        assert!(
            !issued(&cmd, &Cmd::FetchPrList),
            "対象が無いのに頼んだ: {cmd:?}"
        );
        // 対象が決まって ref が届く（`repo_info_loaded` の側から出る）。
        state.repo.target = TargetState::Resolved(fake_target());
        let cmd = update(
            &mut state,
            Msg::RepoInfoLoaded(Box::new(Ok(RepoSnapshot {
                remotes: Vec::new(),
                head: HeadInfo::Branch {
                    name: "feat/x".to_string(),
                    oid: fake_oid(0),
                },
                refs: RefIndex::default(),
            }))),
        );
        assert!(issued(&cmd, &Cmd::FetchPrList), "{cmd:?}");
    }

    #[test]
    fn the_cursor_starts_on_the_pr_of_the_current_branch() {
        // 起動直後の座り先は「いま居るブランチの PR」（01 の「現ブランチの PR」）。
        let mut state = state_on_branch("feat/x");
        deliver(
            &mut state,
            vec![
                fake_pull_request(90, "other", "othersha"),
                fake_pull_request(82, "feat/x", "headsha"),
            ],
        );

        assert_eq!(state.prs.cursor, 1);
        assert_eq!(state.prs.selected().map(|pr| pr.number), Some(82));
        assert_eq!(state.current_pr().map(|pr| pr.number), Some(82));
    }

    #[test]
    fn the_cursor_stays_on_the_same_pr_when_the_order_changes() {
        // **添字では座り直さない。** 一覧は `sort=updated` なので、commit するたびに
        // 並びが変わる（`PrState::seat`）。
        let mut state = state_on_branch("feat/x");
        deliver(
            &mut state,
            vec![
                fake_pull_request(82, "feat/x", "headsha"),
                fake_pull_request(90, "other", "othersha"),
            ],
        );
        state.focus = Pane::Prs;
        let _ = press(&mut state, 'j');
        assert_eq!(state.prs.selected().map(|pr| pr.number), Some(90));

        // 90 が更新されて先頭に来る。
        deliver(
            &mut state,
            vec![
                fake_pull_request(90, "other", "othersha"),
                fake_pull_request(82, "feat/x", "headsha"),
            ],
        );
        assert_eq!(
            state.prs.selected().map(|pr| pr.number),
            Some(90),
            "添字のまま座っている"
        );
    }

    #[test]
    fn a_pr_that_left_the_list_puts_the_cursor_back_at_the_top() {
        // マージされると open の一覧から消える（01 の「現ブランチの PR」で受け入れた穴）。
        let mut state = state_on_branch("feat/x");
        deliver(
            &mut state,
            vec![
                fake_pull_request(82, "feat/x", "headsha"),
                fake_pull_request(90, "other", "othersha"),
            ],
        );
        state.focus = Pane::Prs;
        let _ = press(&mut state, 'j');

        deliver(&mut state, vec![fake_pull_request(82, "feat/x", "headsha")]);
        assert_eq!(state.prs.cursor, 0);
        assert_eq!(state.prs.selected().map(|pr| pr.number), Some(82));
    }

    #[test]
    fn a_fork_branch_does_not_match_the_current_branch_row() {
        // **受け入れた穴**（plan.md の「fork のブランチで現ブランチの PR を当てる」）。
        // 一覧には fork の PR も出るが、照合が `head.repo.owner == target.owner` なので
        // 合致せず、カーソルは先頭に座る。
        let mut state = state_on_branch("feat/x");
        let mut fork = fake_pull_request(82, "feat/x", "headsha");
        fork.head.repo = Some(RepoRef {
            owner: "someone-else".to_string(),
            repo: fake_target().repo,
        });
        deliver(
            &mut state,
            vec![fake_pull_request(90, "other", "othersha"), fork],
        );

        assert_eq!(state.current_pr(), None, "fork の PR に合致している");
        assert_eq!(state.prs.cursor, 0);
        assert_eq!(state.prs.list.loaded().map(Vec::len), Some(2), "消えている");
    }

    #[test]
    fn an_empty_list_is_not_the_same_as_an_unfetched_one() {
        // **`Fetch` の 3 状態**（01）。空の `Vec` は「開いている PR が 1 件も無い」。
        let mut state = state_on_branch("feat/x");
        assert!(matches!(state.prs.list, Fetch::Unasked));

        deliver(&mut state, Vec::new());
        assert!(matches!(&state.prs.list, Fetch::Loaded(list) if list.is_empty()));
    }

    #[test]
    fn a_failed_fetch_is_remembered_and_the_material_that_changed_asks_again() {
        // **失敗を状態に持つ**（01 の「失敗の記憶」）。
        let mut state = state_on_branch("feat/x");
        let ask = reload(&state);
        let _ = reply(
            &mut state,
            ask,
            Msg::PrListLoaded(Err(GithubError::NotFound)),
        );
        assert!(matches!(state.prs.list, Fetch::Failed(_)));

        // **出口は材料が変わったとき。** `f`（ref）でも `A`（アカウント）でも
        // 引き直せる（`reload` の doc）。
        assert_eq!(reload(&state), Cmd::FetchPrList);
    }

    #[test]
    fn picking_another_account_asks_for_the_list_again() {
        // **前のトークンで引けなかった答えを残さない。** `A` の流れからは、そこを
        // 抜ける手掛かりが 1 つも無い（独立したレビューが #83 で拾った形）。
        let mut state = state_on_branch("feat/x");
        let ask = reload(&state);
        let _ = reply(
            &mut state,
            ask,
            Msg::PrListLoaded(Err(GithubError::NotFound)),
        );
        assert!(matches!(state.prs.list, Fetch::Failed(_)));

        // `A` → 選び直し → `GitOp::SetAccount` → `resolve_again` → 解決し直し。
        let cmd = update(&mut state, Msg::AccountResolved(Ok(fake_account("hanako"))));
        assert!(
            issued(&cmd, &Cmd::FetchPrList),
            "アカウントが変わったのに頼み直していない: {cmd:?}"
        );
    }

    #[test]
    fn a_fetch_overtaken_by_a_write_is_asked_again() {
        // **鍵が無くても古いことがある。** 書き込みの最中に飛んでいた要求の答えは
        // 書き込み**前**の GitHub の状態で、そのあとの引き直しは `AppState::issue` に
        // 落とされている（通知も出ない）。刻印で拾う（01 の「実行中ジョブ › 世代」）。
        let mut state = state_on_branch("feat/x");
        // 1 本目が飛んでいる。**返信はまだ届けない。**
        let _ = state.issue(reload(&state));
        assert!(state.jobs.is_running(JobKind::FetchPrList));

        // 書き込みが終わって ref が動いた。ここで出る `Cmd::FetchPrList` は落とされる。
        state.jobs.advance(Region::Refs);
        let dropped = state.issue(reload(&state));
        assert_eq!(dropped, Cmd::None, "落とされていない");

        // 1 本目の答えが届く。**採ったうえで頼み直す。**
        let next = update(
            &mut state,
            list_msg(vec![fake_pull_request(82, "feat/x", "headsha")]),
        );

        assert!(state.current_pr().is_some(), "出せるものを捨てている");
        assert_eq!(next, Cmd::FetchPrList, "追いついていない");
    }

    #[test]
    fn o_opens_the_pr_under_the_cursor_and_says_so_when_the_list_is_empty() {
        let mut state = state_on_branch("feat/x");
        state.focus = Pane::Prs;

        // 引く前は開けない。
        let cmd = press(&mut state, 'o');
        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.notices.len(), 1);
        assert_eq!(state.notices[0].level, NoticeLevel::Info);

        deliver(
            &mut state,
            vec![
                fake_pull_request(82, "feat/x", "headsha"),
                fake_pull_request(90, "other", "othersha"),
            ],
        );
        // **カーソル行を開く。** 起動直後は現ブランチの PR に座っている。
        assert_eq!(press(&mut state, 'o'), Cmd::OpenPr { number: 82 });
    }

    #[test]
    fn pressing_o_while_the_browser_is_starting_says_so_instead_of_nothing() {
        // **`gh browse` は起動したブラウザが終わるまで返らない**（gh 2.100.0 で実測。
        // `BROWSER` に 5 秒眠るスクリプトを置くと 5.06 秒）。端末ブラウザや自分で
        // 抜けないラッパーを `BROWSER` にしている環境では、そのあいだ `o` が
        // 無反応になる（#63 の「無反応よりまし」で止めない）。
        let mut state = state_on_branch("feat/x");
        deliver(&mut state, vec![fake_pull_request(82, "feat/x", "headsha")]);
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
    fn j_and_k_move_the_cursor_within_the_list() {
        let mut state = state_on_branch("feat/x");
        deliver(
            &mut state,
            vec![
                fake_pull_request(82, "feat/x", "headsha"),
                fake_pull_request(90, "other", "othersha"),
            ],
        );
        state.focus = Pane::Prs;
        assert_eq!(state.prs.cursor, 0, "現ブランチの PR に座っている");

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
