//! PRs ペインのキー処理と、PR 一覧の受け取り（01 の「PR ペイン」）。
//!
//! 引くかどうかを決めるのは [`AppState::wanted_pr_list`] で、**頼む側と、文言を出すか
//! 一覧を描くかを決める `ui` の両方がそれを読む**。

use crate::auth::AuthError;
use crate::github::{GithubError, PrDetail, PullRequest, RateLimit};

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
///
/// **出口は一覧だけでなく集計にも要る。** 集計を落とすのは `PrState::loaded`（届いた
/// 一覧で `Err` の行を捨てる）と `PrState::forget`（アカウントが変わった）で、
/// **どちらもこの経路の先にある** —— 一覧を引き直さないと集計の失敗も落ちない。
/// 集計の失敗は通知に出ないので（07 で受け入れた穴）、ここが塞がると行の `!` が
/// 消えない行き止まりになる。
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
///
/// **PRs タブが見えていなければ空**（門は [`AppState::wanted_details`]）。だから
/// ここは [`update::focus`](super::focus) からも呼ぶ —— **タブに入った回に頼むのは
/// この経路だけ**で、無いと開いた直後の行が `…` のまま次に一覧が届くまで埋まらない。
/// 一覧が届く経路（[`pr_list_loaded`]）だけでは、タブを開くこと自体が引き金に
/// ならない。
pub(super) fn fan_out(state: &AppState) -> Cmd {
    let targets = state.wanted_details();
    if targets.is_empty() {
        Cmd::None
    } else {
        Cmd::FetchPrDetails {
            epoch: state.prs.epoch(),
            targets,
        }
    }
}

/// 扇形の答えが 1 件届いた。
///
/// **ジョブは畳まない。** 1 つの `Cmd::FetchPrDetails` から何度も届くので、畳むのは
/// [`pr_details_finished`] のほう。
///
/// **追いつきは 2 つ要る。** `(番号, sha)` は「一覧のどの行に当てるか」を答え
/// （[`JobKind::region`] に世代を使わない理由がある）、世代は「**どのアカウントで
/// 引いたか**」を答える —— アカウントを切り替えても番号も head も変わらないので、
/// 鍵だけでは前のトークンの答えが新しい一覧に入る。
pub(super) fn pr_detail_loaded(
    state: &mut AppState,
    epoch: u64,
    number: u64,
    sha: String,
    result: Result<PrDetail, GithubError>,
) -> Cmd {
    if !state.prs.is_current(epoch) {
        return Cmd::None;
    }
    state.prs.detail_loaded(number, sha, result);
    Cmd::None
}

/// 扇形が終わった。
///
/// **打ち切りは残りの行に書く。** `Some(err)` はレート制限で残りを頼まなかったという
/// 意味で、埋めずに返すと答えの無い行が `PrState::wanted_details` に拾われ、**同じ
/// 失敗を買い直す輪**になる（01 の「`Err` から再発行してよいのは対象が違うときだけ」）。
///
/// **打ち切ったときだけ通知を出す。** 行に出せるのは「引けなかった」という印 1 つ
/// だけで（枠の内側にそれ以上入らない。07）、**理由を知る経路がほかに無い** ——
/// レート制限は待てば直るものなので、そのまま `!` を並べると行き止まりになる
/// （#63 の「無反応よりまし」で止めない）。
///
/// **1 件ごとの失敗では出さない。** fork が消えた PR の 404 は行ごとの事情で、
/// 一覧ぶん通知を積んでも次に何をするかが変わらない。
pub(super) fn pr_details_finished(
    state: &mut AppState,
    epoch: u64,
    aborted: Option<(GithubError, Vec<(u64, String)>)>,
) -> Cmd {
    // **ジョブは世代に関係なく畳む。** 走っていたのはこのジョブなので、畳まないと
    // 以後の `Cmd::FetchPrDetails` が `AppState::issue` に落とされ続ける。
    state.jobs.finish(JobKind::FetchPrDetails);
    // **効果のほうは世代で門にする。** 前のアカウントで頼んだ扇形の打ち切りを
    // 新しい一覧に書くと、`PrState::forget` が落とした集計をそのまま書き戻す。
    if state.prs.is_current(epoch) {
        if let Some((err, targets)) = aborted {
            state.prs.details_aborted(&targets, &err);
            // **理由は `detail` に載せる。** 要約行に `GithubError` の `Display` を
            // 混ぜると **1 行目の予算を超えて末尾から切れる** ——
            // `InvalidHost` はホスト名をそのまま持ち、**ホスト名に上限が無い**ので、
            // 予算をいくつにしても変わらない（`-bad` の 4 文字でちょうど 72 セル。
            // 実測 2026-09-18。当時の予算が 72 だったのは `q:quit` と同じ行だったため）。
            // `exec_finished` が argv と stderr を `detail` に載せるのと同じ形で、
            // **描くのは M6 のステータスバーのタスク**（01 の「通知」）。
            state.push_notice(
                NoticeLevel::Warn,
                "PR の集計を中断しました".to_string(),
                Some(err.to_string()),
            );
        }
    }
    // 続きを頼む: 扇形が走っているあいだに一覧へ増えた行と、世代が変わって捨てた行。
    fan_out(state)
}

/// 応答が言ったレートリミット残量が届いた。
///
/// **世代で門にする。** 残量は**そのトークンの予算**なので、アカウントを切り替えた
/// あとに前のトークンの扇形が返す数は嘘になる。`(reset, used)` は「どちらの応答が
/// 新しいか」しか答えられない（#89 の「鍵が答えられる問いを数える」）。
///
/// **ジョブは畳まない。** 走っているのは一覧か扇形のジョブで、これはその途中に
/// 何度でも流れる（[`Msg::RateLimitSeen`](crate::app::Msg::RateLimitSeen)）。
pub(super) fn rate_limit_seen(state: &mut AppState, epoch: u64, seen: RateLimit) -> Cmd {
    if state.prs.is_current(epoch) {
        state.prs.rate_limit_seen(seen);
    }
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
    use super::super::test_support::{
        cells, issued, press, press_code, repo_snapshot, state, state_with_commits, tab_to,
        NOTICE_CELLS,
    };
    use super::super::update;
    use super::*;
    use crate::app::{Msg, NoticeLevel, PrLoad, Region, RepoSnapshot, RightTab, TargetState};
    use crate::auth::{AccountState, DegradedReason};
    use crate::config::Config;
    use crate::git_read::HeadInfo;
    use crate::github::RepoRef;
    use crate::github::{CheckSummary, ReviewSummary};
    use crate::test_util::{
        deliver_pr_details, fake_account, fake_oid, fake_pull_request, fake_rate_limit,
        fake_target, fetch_pr_details, pr_list_loaded as list_msg, put_on_branch, reply,
    };

    /// アカウントと対象が決まり、HEAD が `branch` に居る状態。一覧はまだ引いていない。
    fn state_on_branch(branch: &str) -> AppState {
        let mut state = state_with_commits(2);
        put_on_branch(&mut state, branch);
        state
    }

    /// [`state_on_branch`] に加えて、**PRs タブを開いた**状態。
    ///
    /// **門を踏むテストでは `state.focus` に直に代入しない。** 門
    /// （`AppState::wanted_details`）はタブに入る経路（`Tab` → `update::focus`）が
    /// 扇形を頼むかまで見るので、代入だけだと**引き金を一度も踏まないまま**開いた
    /// 状態になる。門に関わらないキー処理のテスト（`j` / `o` など）は直代入でよい。
    ///
    /// **開いた回の `Cmd` を捨てない。** 一覧がまだ無いので `Cmd::None` のはずで、
    /// そうでなければ `AppState::issue` に `FetchPrDetails` が登録され、**以後の扇形が
    /// 黙って落ちる**（#38 の形）。
    fn state_on_prs_tab(branch: &str) -> AppState {
        let mut state = state_on_branch(branch);
        let opened = tab_to(&mut state, Pane::Prs);
        assert_eq!(opened, Cmd::None, "一覧がまだ無いのに扇形が出た");
        state
    }

    /// [`state_on_prs_tab`] に加えて、**`#82` の集計まで憶えた**状態。
    fn state_with_one_summary(branch: &str) -> AppState {
        let mut state = state_on_prs_tab(branch);
        let ask = fan_out_after(&mut state, vec![fake_pull_request(82, branch, "sha82")]);
        let _ = deliver_pr_details(&mut state, ask, &[(82, "sha82", Ok(detail(2)))], None);
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
        // **知らない形は panic させる。** `Cmd::None` に畳むと、`assert_eq!(x,
        // Cmd::None)` 型の見張り（持っている行を引き直さない / 輪にならない）が
        // **別の何かが出ていた場合も緑**になる。
        match out {
            Cmd::None => Cmd::None,
            cmd @ Cmd::FetchPrDetails { .. } => cmd,
            Cmd::Batch(cmds) => cmds
                .into_iter()
                .find(|cmd| matches!(cmd, Cmd::FetchPrDetails { .. }))
                .unwrap_or(Cmd::None),
            other => panic!("一覧の返信がこれを出した: {other:?}"),
        }
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
    fn nothing_is_asked_for_until_the_prs_tab_is_opened() {
        // **既定のタブは Branches。** 門が無いと、PRs を一度も開かない起動でも
        // 一覧の行数ぶんの往復を払う（数は 05 の「扇形は毎回引き直さない」。
        // 01 の「PR ペイン › 集計」）。
        let mut state = state_on_branch("feat/x");
        let list = vec![
            fake_pull_request(82, "feat/x", "sha82"),
            fake_pull_request(83, "other", "sha83"),
        ];

        let quiet = fan_out_after(&mut state, list);
        assert_eq!(quiet, Cmd::None, "タブを開く前に引いた: {quiet:?}");
        assert!(state.prs.detail(82).is_none(), "集計が入っている");

        // タブに入った回の引き金（理由は `fan_out` の doc）。
        let opened = tab_to(&mut state, Pane::Prs);
        assert!(
            issued(
                &opened,
                &Cmd::FetchPrDetails {
                    epoch: 0,
                    targets: vec![(82, "sha82".to_string()), (83, "sha83".to_string())],
                }
            ),
            "タブを開いても引かない: {opened:?}"
        );
    }

    #[test]
    fn the_tab_keeps_fetching_while_the_focus_is_back_on_the_graph() {
        // **門が見るのは「タブが見えているか」で、フォーカスの居場所ではない。**
        // `Esc` で Graph に戻ってもタブは残る（01 の「右上のタブ」）ので、画面に
        // 出ている行は引き続き埋まる —— `focus == Pane::Prs` で門にすると、
        // ペインを見ているのに `…` が埋まらない状態が出る。
        let mut state = state_with_one_summary("feat/x");

        let _ = press_code(&mut state, KeyCode::Esc);
        assert_eq!(state.focus, Pane::Graph);
        assert_eq!(state.right_tab, RightTab::Prs, "タブまで戻っている");

        let grown = fan_out_after(
            &mut state,
            vec![
                fake_pull_request(82, "feat/x", "sha82"),
                fake_pull_request(83, "other", "sha83"),
            ],
        );
        assert_eq!(
            grown,
            Cmd::FetchPrDetails {
                epoch: 0,
                targets: vec![(83, "sha83".to_string())],
            },
            "見えている行を引かない"
        );
    }

    #[test]
    fn leaving_the_prs_tab_stops_the_fan_out_and_coming_back_picks_the_rows_up() {
        // **latch にしない判断（01 の「PR ペイン › 集計」）を踏む腕。** 離れている
        // あいだは引かず、戻ってきたら増えた行を拾う。
        let mut state = state_with_one_summary("feat/x");

        let _ = tab_to(&mut state, Pane::Branches);
        let grown = vec![
            fake_pull_request(82, "feat/x", "sha82"),
            fake_pull_request(83, "other", "sha83"),
        ];
        let quiet = fan_out_after(&mut state, grown);
        assert_eq!(quiet, Cmd::None, "離れているあいだに引いた: {quiet:?}");

        let back = tab_to(&mut state, Pane::Prs);
        assert!(
            issued(
                &back,
                &Cmd::FetchPrDetails {
                    epoch: 0,
                    targets: vec![(83, "sha83".to_string())],
                }
            ),
            "戻っても増えた行を拾わない: {back:?}"
        );
        // **持っている行は引き直さない。** 門を通ったからといって全件に戻らない。
        assert_eq!(*state.prs.detail(82).unwrap(), Ok(detail(2)));
    }

    #[test]
    fn the_fan_out_asks_only_for_the_rows_that_have_no_summary() {
        // **1 件ぶんの往復が PR の数だけ積む**ので、毎回全件引くと `core` の予算に
        // 対して重すぎる（数は 05 の「扇形は毎回引き直さない」）。
        //
        // **順序の片方（タブが先 → 一覧が後）を踏むのもここ。** 逆
        // （一覧が先 → タブが後）は `nothing_is_asked_for_until_the_prs_tab_is_opened`
        // にある（`CLAUDE.md` の「非同期の返信が 2 本あるならテストは両方の順序を
        // 回す」の、片方がキーの版）。**専用の 1 本は置かない** —— 置いてみたが、
        // こちらと別に単独で落ちる変異が無かった（実測）。
        let mut state = state_on_prs_tab("feat/x");
        let list = vec![
            fake_pull_request(82, "feat/x", "sha82"),
            fake_pull_request(83, "other", "sha83"),
        ];

        let ask = fan_out_after(&mut state, list.clone());
        assert_eq!(
            ask,
            Cmd::FetchPrDetails {
                epoch: 0,
                targets: vec![(82, "sha82".to_string()), (83, "sha83".to_string())],
            },
            "初回は全件"
        );

        let next = deliver_pr_details(
            &mut state,
            ask,
            &[(82, "sha82", Ok(detail(2))), (83, "sha83", Ok(detail(1)))],
            None,
        );
        assert_eq!(*state.prs.detail(82).unwrap(), Ok(detail(2)));
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
        let mut state = state_on_prs_tab("feat/x");
        let ask = fan_out_after(&mut state, vec![fake_pull_request(82, "feat/x", "old")]);
        let _ = deliver_pr_details(&mut state, ask, &[(82, "old", Ok(detail(2)))], None);
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
                epoch: 0,
                targets: vec![(82, "new".to_string())],
            }
        );
    }

    #[test]
    fn an_answer_for_a_head_that_already_moved_is_not_taken() {
        // **飛んでいるあいだに一覧が入れ替わることがある。** 鍵は `(番号, sha)` で、
        // 世代は見ない（`JobKind::region` にその理由がある）。
        let mut state = state_on_prs_tab("feat/x");
        let ask = fan_out_after(&mut state, vec![fake_pull_request(82, "feat/x", "new")]);
        // 答えだけが古い sha で返ってくる。
        let _ = deliver_pr_details(&mut state, ask, &[(82, "old", Ok(detail(9)))], None);
        assert!(
            state.prs.detail(82).is_none(),
            "前のコミットの結果を採っている"
        );

        // 一覧から消えた PR の答えも採らない。**`ask` は終端が返した続きを使う**
        // —— そこで既に頼み直しているので、一覧を届け直すと `AppState::issue` に
        // 落とされて `Cmd::None` が返る。
        let again = fan_out_after(&mut state, vec![fake_pull_request(82, "feat/x", "new")]);
        assert_eq!(again, Cmd::None, "ジョブが走っているのに二重に出た");
        let ask = fetch_pr_details(&state, &[(99, "sha99", Ok(detail(1)))]);
        let _ = deliver_pr_details(&mut state, ask, &[(99, "sha99", Ok(detail(1)))], None);
        assert!(state.prs.detail(99).is_none(), "居ない行の集計が溜まった");
    }

    #[test]
    fn an_aborted_fan_out_fills_the_rest_says_why_and_does_not_loop() {
        // **打ち切ったら残りを埋める。** 埋めないと同じ失敗を買い直す輪になる
        // （01 の「`Err` から再発行してよいのは対象が違うときだけ」）。
        let mut state = state_on_prs_tab("feat/x");
        let list = vec![
            fake_pull_request(82, "feat/x", "sha82"),
            fake_pull_request(83, "other", "sha83"),
        ];
        let ask = fan_out_after(&mut state, list.clone());
        let limited = GithubError::RateLimited {
            reset_at: Some(1_789_703_295),
        };
        let next = deliver_pr_details(
            &mut state,
            ask,
            &[(82, "sha82", Err(limited.clone()))],
            Some(limited.clone()),
        );

        assert_eq!(*state.prs.detail(82).unwrap(), Err(limited.clone()));
        assert_eq!(
            *state.prs.detail(83).unwrap(),
            Err(limited.clone()),
            "頼まなかった行が埋まっていない"
        );
        // **理由を出す経路はここだけ。** 行に出せるのは「引けなかった」の印 1 つで、
        // レート制限は待てば直るものなので、出さないと行き止まりになる。
        let notice = state.notices.back().expect("通知");
        assert_eq!(notice.level, NoticeLevel::Warn);
        assert!(notice.message.contains("中断"), "{}", notice.message);
        // **理由は要約行ではなく `detail` に入る**（上限の無いホスト名を要約行に
        // 混ぜると予算を超える）。
        assert!(
            notice
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("レート制限"),
            "{:?}",
            notice.detail
        );
        // **ステータスバーの予算**（01 の「失敗の案内文」）。**見張りは複数あって、
        // 新しい産出元を自分では拾わない**（`CLAUDE.md`）—— `exec_advice` も `A` も
        // `o` も別々に見ているので、通知を出す場所を足したらここにも足す。
        // **本文に `GithubError` の `Display` が乗る**ので、腕を足したときに超えうる。
        assert!(
            cells(&notice.message) <= NOTICE_CELLS,
            "{} セル: {}",
            cells(&notice.message),
            notice.message
        );

        // **終端では頼み直さない。** ここで出すと「失敗 → 再取得 → 失敗」の輪になる。
        assert_eq!(next, Cmd::None, "終端が頼み直している: {next:?}");

        // **一覧を引き直せば出口がある。** `f` や書き込みのあとに届く一覧で
        // 引けなかった記憶は落ち、もう一度頼む —— 落ちないと、回線が 1 回切れた
        // だけでセッション中ずっと `!` のままになる。
        let again = fan_out_after(&mut state, list);
        assert_eq!(
            again,
            Cmd::FetchPrDetails {
                epoch: 0,
                targets: vec![(82, "sha82".to_string()), (83, "sha83".to_string())],
            },
            "失敗からの出口が無い"
        );
    }

    #[test]
    fn an_abort_does_not_poison_rows_it_never_asked_for() {
        // **埋める相手は頼んだ対象で、いまの一覧ではない。** 扇形が走っているあいだに
        // 増えた行は `AppState::issue` に落とされて頼まれていないので、そこへ理由を
        // 書くと次の `fan_out` も空になり、**一度も叩いていない行が二度と引かれない**。
        let mut state = state_on_prs_tab("feat/x");
        let ask = fan_out_after(&mut state, vec![fake_pull_request(82, "feat/x", "sha82")]);

        // 走っているあいだに commit などで一覧が増える。**この `Cmd` は落とされる。**
        let grown = vec![
            fake_pull_request(82, "feat/x", "sha82"),
            fake_pull_request(91, "other", "sha91"),
        ];
        let dropped = fan_out_after(&mut state, grown);
        assert_eq!(dropped, Cmd::None, "ジョブが走っているのに二重に出た");

        let limited = GithubError::RateLimited { reset_at: None };
        let next = deliver_pr_details(&mut state, ask, &[], Some(limited.clone()));

        assert_eq!(*state.prs.detail(82).unwrap(), Err(limited));
        assert!(
            state.prs.detail(91).is_none(),
            "頼んでいない行を塞いだ: {:?}",
            state.prs.detail(91)
        );
        // 続きは終端が数え直す。
        assert_eq!(
            next,
            Cmd::FetchPrDetails {
                epoch: 0,
                targets: vec![(91, "sha91".to_string())],
            }
        );
    }

    #[test]
    fn an_abort_from_the_previous_account_does_not_touch_the_new_list() {
        // **鍵では足りない。** アカウントを切り替えても番号も head も変わらないので、
        // 前のトークンで飛んでいた扇形の答えが新しい一覧にそのまま当たる ——
        // `PrState::forget` が落とすと言っている守りを、走っている扇形が外から破る。
        let mut state = state_on_prs_tab("feat/x");
        let list = vec![fake_pull_request(82, "feat/x", "sha82")];
        let stale = fan_out_after(&mut state, list.clone());

        // degraded → 別アカウントで解決し直し → 同じ一覧が届く。
        let _ = update(
            &mut state,
            Msg::AccountResolved(Err(DegradedReason::GhMissing)),
        );
        let _ = update(&mut state, Msg::AccountResolved(Ok(fake_account("hanako"))));
        // **新しいぶんは `AppState::issue` に落ちる** —— 前の扇形のジョブがまだ
        // 走っているため。頼み直すのはその終端。
        let dropped = fan_out_after(&mut state, list);
        assert_eq!(dropped, Cmd::None, "ジョブが走っているのに二重に出た");

        // **前のトークンの扇形が、いま届く。**
        let limited = GithubError::RateLimited { reset_at: None };
        let before = state.notices.len();
        let next = deliver_pr_details(
            &mut state,
            stale,
            &[(82, "sha82", Ok(detail(9)))],
            Some(limited),
        );

        assert!(
            state.prs.detail(82).is_none(),
            "前のアカウントの答えが入った: {:?}",
            state.prs.detail(82)
        );
        assert_eq!(state.notices.len(), before, "前のアカウントの理由を出した");
        // **ジョブは畳む**（畳まないと以後の扇形が落ち続ける）ので、新しい
        // アカウントぶんはここで出る。
        assert_eq!(
            next,
            Cmd::FetchPrDetails {
                epoch: state.prs.epoch(),
                targets: vec![(82, "sha82".to_string())],
            },
            "新しいアカウントで頼み直していない"
        );
        assert_ne!(state.prs.epoch(), 0, "世代が進んでいない");
    }

    #[test]
    fn picking_another_account_forgets_the_summaries() {
        // **集計もアカウントに紐づく。** 残すと、次のトークンで引き直した一覧に
        // 前のトークンで見えた集計が番号だけで当たる。
        let mut state = state_with_one_summary("feat/x");
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
    fn the_newer_quota_wins_whichever_order_it_arrives_in() {
        // **`Msg` の届く順は応答の新しさの順ではない。** 一覧と扇形は同時に走り、
        // 扇形の中でも 4 本が並ぶ（05 の `Semaphore`）。あとから届いた古い数で
        // 上書きすると、ヘッダの残量が増えたり減ったりする。
        let mut state = state_on_branch("feat/x");
        let epoch = state.prs.epoch();

        let _ = update(
            &mut state,
            Msg::RateLimitSeen {
                epoch,
                seen: fake_rate_limit(4900, 100),
            },
        );
        let _ = update(
            &mut state,
            Msg::RateLimitSeen {
                epoch,
                seen: fake_rate_limit(4950, 100),
            },
        );
        assert_eq!(
            state.prs.rate_limit(),
            Some(fake_rate_limit(4900, 100)),
            "あとから届いた古い数で上書きした"
        );

        // **窓が変われば残量は戻る。** 比べるのは `(reset, used)` で `reset` が先。
        let _ = update(
            &mut state,
            Msg::RateLimitSeen {
                epoch,
                seen: fake_rate_limit(5000, 200),
            },
        );
        assert_eq!(state.prs.rate_limit(), Some(fake_rate_limit(5000, 200)));
    }

    #[test]
    fn the_quota_of_the_previous_account_is_dropped_and_does_not_come_back() {
        // **残量はそのトークンの予算。** `(reset, used)` は「どちらの応答が新しいか」
        // しか答えないので、アカウントを切り替えたら世代で弾くしかない
        // （#89 の「鍵が答えられる問いを数える」）。
        let mut state = state_on_branch("feat/x");
        let stale = state.prs.epoch();
        let _ = update(
            &mut state,
            Msg::RateLimitSeen {
                epoch: stale,
                seen: fake_rate_limit(4900, 100),
            },
        );
        assert!(state.prs.rate_limit().is_some());

        let _ = update(
            &mut state,
            Msg::AccountResolved(Err(DegradedReason::GhMissing)),
        );
        assert_eq!(
            state.prs.rate_limit(),
            None,
            "前のトークンの残量が残った（ヘッダの `[gh: …]` だけが変わる）"
        );

        let _ = update(&mut state, Msg::AccountResolved(Ok(fake_account("hanako"))));
        // **前のトークンで飛んでいた扇形が、いま返す。**
        let _ = update(
            &mut state,
            Msg::RateLimitSeen {
                epoch: stale,
                seen: fake_rate_limit(10, 100),
            },
        );
        assert_eq!(
            state.prs.rate_limit(),
            None,
            "前のトークンの残量が新しいアカウントに入った"
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
        assert!(
            cells(&message) <= NOTICE_CELLS,
            "{} セル: {message}",
            cells(&message)
        );
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
