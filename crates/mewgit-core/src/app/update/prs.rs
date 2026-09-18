//! PRs ペインのキー処理と、PR 一覧の受け取り（01 の「PR ペイン」）。
//!
//! 引くかどうかを決めるのは [`AppState::wanted_pr_list`] で、**頼む側と、文言を出すか
//! 一覧を描くかを決める `ui` の両方がそれを読む**。

use crate::auth::AuthError;
use crate::github::{GithubError, PrDetail, PullRequest};

use super::focus;
use crate::app::state::current_row;
use crate::app::{AppState, Cmd, JobKind, Key, KeyCode, NoticeLevel, Pane};

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
///
/// **失敗しても読めていた一覧は捨てない**（`PrState::failed`）。引き直すのは ref を
/// 動かす書き込みのたびなので、捨てると commit 直後に 1 回転けただけでペインが
/// エラー 1 行になる。
pub(super) fn pr_list_loaded(
    state: &mut AppState,
    result: Result<Vec<PullRequest>, GithubError>,
) -> Cmd {
    let asked = state.jobs.finish(JobKind::FetchPrList);
    match result {
        // **座り直す先は `PrState::loaded` が決める。** 呼ぶ側が渡すのは「いま選んで
        // いる PR が消えていたときの行き先」だけで、それは新しい一覧といまの HEAD から
        // 決まる（01 の「現ブランチの PR」）。
        Ok(list) => {
            // **`repo` だけを借りる。** `state.prs` を書き換えながら `state.repo` を
            // 読むので、閉包が掴むのを 1 フィールドに絞る（Rust 2021 の分割キャプチャ）。
            let repo = &state.repo;
            state
                .prs
                .loaded(list, |list| current_row(repo, list).map(|pr| pr.number))
        }
        Err(err) => state.prs.failed(err),
    }
    let again = if state.jobs.is_fresh(JobKind::FetchPrList, asked) {
        Cmd::None
    } else {
        // 続きを頼む: 書き込みに追い越された `FetchPrList`。
        reload(state)
    };
    // 続きを頼む: 一覧に増えた行の集計。
    Cmd::batch([again, fan_out(state)])
}

/// 集計をまだ持っていない行があれば扇形を頼む（01 の「PR ペイン › 集計」）。
///
/// **`Cmd` は 1 本。** PR ごとに出すと [`JobKind`] が種別単位なので 2 本目以降を
/// [`AppState::issue`] が黙って落とす（05 の「`Semaphore`」）。
///
/// **落とされても詰まらない。** 扇形が走っているあいだに新しい一覧が届くと、この
/// `Cmd` は `issue` に落ちるが、走っているほうが [`pr_details_finished`] で終わる
/// ときに残りを数え直す。**`Pending` を状態に書かないのはそのため** —— 書くと、
/// 落とされた `Cmd` のぶんが「頼んだのに誰も返さない行」として残る。
fn fan_out(state: &AppState) -> Cmd {
    let targets = state.prs.wanted_details();
    if targets.is_empty() {
        Cmd::None
    } else {
        Cmd::FetchPrDetails { targets }
    }
}

/// 扇形の答えが 1 件届いた。
///
/// **ジョブは畳まない。** 1 つの `Cmd::FetchPrDetails` から何度も届くので、畳むのは
/// [`pr_details_finished`] のほう。**追いつきは鍵**（`(番号, sha)`）で、世代は見ない
/// （[`JobKind::region`] にその理由がある）。
pub(super) fn pr_detail_loaded(
    state: &mut AppState,
    number: u64,
    sha: String,
    result: Result<PrDetail, GithubError>,
) -> Cmd {
    state.prs.detail_loaded(number, sha, result);
    Cmd::None
}

/// 扇形が終わった。
///
/// **打ち切りは残りの行に書く。** `Some(err)` はレート制限で残りを頼まなかったという
/// 意味で、埋めずに返すと答えの無い行が [`PrState::wanted_details`] に拾われ、**同じ
/// 失敗を買い直す輪**になる（01 の「`Err` から再発行してよいのは対象が違うときだけ」）。
///
/// **打ち切ったときだけ通知を出す。** 行に出せるのは「引けなかった」という印 1 つ
/// だけで（枠の内側にそれ以上入らない。07）、**理由を知る経路がほかに無い** ——
/// レート制限は待てば直るものなので、そのまま `!` を並べると行き止まりになる
/// （#63 の「無反応よりまし」で止めない）。
///
/// **1 件ごとの失敗では出さない。** fork が消えた PR の 404 は行ごとの事情で、
/// 一覧ぶん通知を積んでも次に何をするかが変わらない。
pub(super) fn pr_details_finished(state: &mut AppState, aborted: Option<GithubError>) -> Cmd {
    state.jobs.finish(JobKind::FetchPrDetails);
    if let Some(err) = aborted {
        state.prs.details_aborted(&err);
        state.push_notice(
            NoticeLevel::Warn,
            format!("PR の集計を打ち切りました: {err}"),
            None,
        );
    }
    // 続きを頼む: 扇形が走っているあいだに一覧へ増えた行。
    fan_out(state)
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
    use super::super::test_support::{
        cells, issued, press, repo_snapshot, state, state_with_commits,
    };
    use super::super::update;
    use super::*;
    use crate::app::{Msg, NoticeLevel, PrLoad, Region, RepoSnapshot, TargetState};
    use crate::auth::{AccountState, DegradedReason};
    use crate::config::Config;
    use crate::git_read::HeadInfo;
    use crate::github::RepoRef;
    use crate::github::{CheckSummary, ReviewSummary};
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

    /// 一覧を届けて、カーソルの座り先まで進める。
    fn deliver(state: &mut AppState, list: Vec<PullRequest>) {
        let ask = reload(state);
        let _ = reply(state, ask, list_msg(list));
    }

    /// 集計 1 件ぶんの値。中身は見分けが付けばよい。
    fn detail(success: usize) -> PrDetail {
        PrDetail {
            checks: CheckSummary {
                success,
                ..CheckSummary::default()
            },
            review: ReviewSummary::default(),
        }
    }

    /// 一覧を届けたあとに出る扇形。**`Cmd` が 1 本であることもここで見る。**
    fn fan_out_after(state: &mut AppState, list: Vec<PullRequest>) -> Cmd {
        let ask = reload(state);
        let out = reply(state, ask, list_msg(list));
        match out {
            Cmd::Batch(ref cmds) => cmds
                .iter()
                .find(|cmd| matches!(cmd, Cmd::FetchPrDetails { .. }))
                .cloned()
                .unwrap_or(Cmd::None),
            cmd @ Cmd::FetchPrDetails { .. } => cmd,
            _ => Cmd::None,
        }
    }

    /// 扇形を発行して答えを届け、終端まで運ぶ。
    ///
    /// **`AppState::issue` を通す**（`test_util::reply` と同じ理由）。通さずに
    /// `Msg::PrDetail*` だけ届けると、ジョブの登録されていない**実物では起きない形**を
    /// 作る（`CLAUDE.md`）。
    fn deliver_details(
        state: &mut AppState,
        ask: Cmd,
        answers: &[(u64, &str, Result<PrDetail, GithubError>)],
        aborted: Option<GithubError>,
    ) -> Cmd {
        let _ = state.issue(ask);
        for (number, sha, result) in answers {
            let _ = update(
                state,
                Msg::PrDetailLoaded {
                    number: *number,
                    sha: (*sha).to_string(),
                    result: result.clone(),
                },
            );
        }
        update(state, Msg::PrDetailsFinished(aborted))
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
                head: HeadInfo::Branch {
                    name: "feat/x".to_string(),
                    oid: fake_oid(0),
                },
                ..repo_snapshot()
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
        assert_eq!(state.prs.list.len(), 2, "消えている");
    }

    #[test]
    fn an_empty_list_is_not_the_same_as_an_unfetched_one() {
        // **`Fetch` の 3 状態**（01）。空の `Vec` は「開いている PR が 1 件も無い」。
        let mut state = state_on_branch("feat/x");
        assert_eq!(state.prs.load(), &PrLoad::Pending);
        assert!(state.prs.list.is_empty());

        deliver(&mut state, Vec::new());
        assert_eq!(state.prs.load(), &PrLoad::Loaded);
        assert!(state.prs.list.is_empty());
    }

    #[test]
    fn a_failed_fetch_is_remembered_and_the_material_that_changed_asks_again() {
        // **失敗を状態に持つ**（01 の「失敗の記憶」）。
        let mut state = state_on_branch("feat/x");
        deliver(&mut state, vec![fake_pull_request(82, "feat/x", "headsha")]);

        let ask = reload(&state);
        let next = reply(
            &mut state,
            ask,
            Msg::PrListLoaded(Err(GithubError::NotFound)),
        );
        // **自分では頼み直さない。** 回すと「失敗 → 再読込 → 失敗」になる
        // （01 の「失敗の記憶」）。
        assert_eq!(next, Cmd::None, "失敗を頼み直している");
        assert!(matches!(state.prs.load(), PrLoad::Failed(_)));
        // **読めていた一覧は捨てない**（`PrState::failed`）。捨てると、commit 直後の
        // 引き直しが 1 回転けただけでペインがエラー 1 行になる。
        assert_eq!(state.prs.list.len(), 1, "読めていた一覧を捨てている");

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
        assert!(matches!(state.prs.load(), PrLoad::Failed(_)));

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
        // **`==` で見ない。** 同じ出口が集計の扇形も出すので（`fan_out`）、
        // 一覧の頼み直しはその中に居る。
        assert!(
            issued(&next, &Cmd::FetchPrList),
            "追いついていない: {next:?}"
        );
    }

    #[test]
    fn the_fan_out_asks_only_for_the_rows_that_have_no_summary() {
        // **1 件に 3 リクエスト**（check-runs / status / reviews）かかるので、毎回
        // 全件引くと ref を動かす書き込み 1 回が `3N + 1` になる（05 の予算）。
        let mut state = state_on_branch("feat/x");
        let list = vec![
            fake_pull_request(82, "feat/x", "sha82"),
            fake_pull_request(83, "other", "sha83"),
        ];

        let ask = fan_out_after(&mut state, list.clone());
        assert_eq!(
            ask,
            Cmd::FetchPrDetails {
                targets: vec![(82, "sha82".to_string()), (83, "sha83".to_string())],
            },
            "初回は全件"
        );

        let next = deliver_details(
            &mut state,
            ask,
            &[(82, "sha82", Ok(detail(2))), (83, "sha83", Ok(detail(1)))],
            None,
        );
        assert_eq!(state.prs.detail(82).unwrap().result, Ok(detail(2)));
        // **終端で頼み直さないことをここで見る。** 下の `fan_out_after` だけでは
        // 足りない —— そちらが見るのは `update` の出口を通ったあとの `Cmd` で、
        // **終端が既に同じジョブを起こしていると `AppState::issue` が落とす**ので、
        // 引き直していても `Cmd::None` になる（変異で実際に素通りした）。
        assert_eq!(next, Cmd::None, "終端が引き直している: {next:?}");

        // **同じ一覧がもう一度届いても頼み直さない。**
        let again = fan_out_after(&mut state, list);
        assert_eq!(again, Cmd::None, "持っている行を引き直した: {again:?}");
    }

    #[test]
    fn a_summary_whose_head_moved_is_dropped_and_asked_again() {
        // **チェックはコミットに紐づく。** push で head が進んだ PR に前のコミットの
        // 結果を出したままにしない。
        let mut state = state_on_branch("feat/x");
        let ask = fan_out_after(&mut state, vec![fake_pull_request(82, "feat/x", "old")]);
        let _ = deliver_details(&mut state, ask, &[(82, "old", Ok(detail(2)))], None);
        assert!(state.prs.detail(82).is_some());

        // push して head が進んだ一覧が届く。
        let ask = fan_out_after(&mut state, vec![fake_pull_request(82, "feat/x", "new")]);
        assert!(
            state.prs.detail(82).is_none(),
            "古い sha の集計が残っている"
        );
        assert_eq!(
            ask,
            Cmd::FetchPrDetails {
                targets: vec![(82, "new".to_string())],
            }
        );
    }

    #[test]
    fn an_answer_for_a_head_that_already_moved_is_not_taken() {
        // **飛んでいるあいだに一覧が入れ替わることがある。** 鍵は `(番号, sha)` で、
        // 世代は見ない（`JobKind::region` にその理由がある）。
        let mut state = state_on_branch("feat/x");
        let ask = fan_out_after(&mut state, vec![fake_pull_request(82, "feat/x", "new")]);
        // 答えだけが古い sha で返ってくる。
        let _ = deliver_details(&mut state, ask, &[(82, "old", Ok(detail(9)))], None);
        assert!(
            state.prs.detail(82).is_none(),
            "前のコミットの結果を採っている"
        );

        // 一覧から消えた PR の答えも採らない。
        let ask = fan_out_after(&mut state, vec![fake_pull_request(82, "feat/x", "new")]);
        let _ = deliver_details(&mut state, ask, &[(99, "sha99", Ok(detail(1)))], None);
        assert!(state.prs.detail(99).is_none(), "居ない行の集計が溜まった");
    }

    #[test]
    fn an_aborted_fan_out_fills_the_rest_says_why_and_does_not_loop() {
        // **打ち切ったら残りを埋める。** 埋めないと同じ失敗を買い直す輪になる
        // （01 の「`Err` から再発行してよいのは対象が違うときだけ」）。
        let mut state = state_on_branch("feat/x");
        let list = vec![
            fake_pull_request(82, "feat/x", "sha82"),
            fake_pull_request(83, "other", "sha83"),
        ];
        let ask = fan_out_after(&mut state, list.clone());
        let limited = GithubError::RateLimited {
            reset_at: Some(1_789_703_295),
        };
        let next = deliver_details(
            &mut state,
            ask,
            &[(82, "sha82", Err(limited.clone()))],
            Some(limited.clone()),
        );

        assert_eq!(state.prs.detail(82).unwrap().result, Err(limited.clone()));
        assert_eq!(
            state.prs.detail(83).unwrap().result,
            Err(limited.clone()),
            "頼まなかった行が埋まっていない"
        );
        // **理由を出す経路はここだけ。** 行に出せるのは「引けなかった」の印 1 つで、
        // レート制限は待てば直るものなので、出さないと行き止まりになる。
        let notice = state.notices.back().expect("通知");
        assert_eq!(notice.level, NoticeLevel::Warn);
        assert!(notice.message.contains("レート制限"), "{}", notice.message);

        assert_eq!(next, Cmd::None, "終端が頼み直している: {next:?}");
        // **同じ一覧がもう一度届いても頼み直さない**（輪にならない）。
        let again = fan_out_after(&mut state, list);
        assert_eq!(again, Cmd::None, "同じ失敗を買い直した: {again:?}");
    }

    #[test]
    fn picking_another_account_forgets_the_summaries() {
        // **集計もアカウントに紐づく。** 残すと、次のトークンで引き直した一覧に
        // 前のトークンで見えた集計が番号だけで当たる。
        let mut state = state_on_branch("feat/x");
        let ask = fan_out_after(&mut state, vec![fake_pull_request(82, "feat/x", "sha82")]);
        let _ = deliver_details(&mut state, ask, &[(82, "sha82", Ok(detail(2)))], None);
        assert!(state.prs.detail(82).is_some());

        let _ = update(
            &mut state,
            Msg::AccountResolved(Err(DegradedReason::GhMissing)),
        );
        assert!(
            state.prs.detail(82).is_none(),
            "前のアカウントの集計が残った"
        );
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

        // **動かしてからも押す。** 先頭で押すだけだと「カーソル行を開く」ではなく
        // 「先頭を開く」でも通る。
        state.jobs.finish(JobKind::OpenPr);
        let _ = press(&mut state, 'j');
        assert_eq!(press(&mut state, 'o'), Cmd::OpenPr { number: 90 });
    }

    #[test]
    fn losing_the_account_drops_the_list_it_was_fetched_with() {
        // **印（`*` / `R`）は login で決まる。** 残すと、印だけが黙って消えた
        // 前のアカウントの一覧が出たままになる（`PrState::forget`）。
        // **`Degraded` では `reload` が `Cmd::None` を返す**ので、落とす経路は
        // `account_resolved` の `Err` しか無い。
        let mut state = state_on_branch("feat/x");
        deliver(&mut state, vec![fake_pull_request(82, "feat/x", "headsha")]);
        assert_eq!(state.prs.list.len(), 1);

        let cmd = update(
            &mut state,
            Msg::AccountResolved(Err(DegradedReason::GhMissing)),
        );
        assert_eq!(cmd, Cmd::None, "degraded なのに頼んでいる");
        assert!(
            state.prs.list.is_empty(),
            "前のアカウントの一覧が残っている"
        );
        assert_eq!(state.prs.load(), &PrLoad::Pending);
        assert_eq!(state.prs.cursor, 0);
    }

    #[test]
    fn resolving_again_drops_the_list_until_the_account_comes_back() {
        // `A` → 選び直しの最中も同じ（`resolve_again`）。**戻ってきたら引き直す**ので、
        // 落としたままにはならない。
        let mut state = state_on_branch("feat/x");
        deliver(&mut state, vec![fake_pull_request(82, "feat/x", "headsha")]);

        let cmd = super::super::account::resolve_again(&mut state);
        assert!(matches!(cmd, Cmd::ResolveAccount(_)));
        assert!(state.prs.list.is_empty(), "解決し直しの最中に残っている");

        let again = update(&mut state, Msg::AccountResolved(Ok(fake_account("taro"))));
        assert!(
            issued(&again, &Cmd::FetchPrList),
            "引き直していない: {again:?}"
        );
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
