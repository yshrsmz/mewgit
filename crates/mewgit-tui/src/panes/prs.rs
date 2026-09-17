//! PRs ペインの描画。設計は `docs/design/07-ui.md` の「PRs ペイン」。
//!
//! 右列の上、Branches と同じ領域をタブで差し替えて使う（07 の「右上のタブ」）。
//! **出すのはこのリポジトリで開いている PR の一覧**で、1 行 1 件。

use std::borrow::Cow;

use mewgit_core::app::{AppState, Fetch};
use mewgit_core::auth::AccountState;
use mewgit_core::github::{PrStatus, PullRequest};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::Rows;
use crate::ui::{self, theme};

/// タブ見出しに出すこのペインの名前。
///
/// **件数を添えない。** 一覧が届くまで数が無く、`(0)` を先に出すと「開いている PR が
/// 無い」と読める（`Fetch::Unasked` と `Loaded(vec![])` を畳むのと同じ誤り。01）。
pub(super) fn title() -> &'static str {
    "PRs"
}

/// 行と、カーソルが何行目か。
///
/// 一覧を出せないときは文言 1 行（カーソルはその行）。
pub(super) fn lines(state: &AppState) -> Rows {
    let Some(list) = open_prs(state) else {
        return Rows {
            lines: vec![Line::raw(placeholder(state))],
            cursor: 0,
        };
    };

    // **状態の桁は出す行から導く。** `OPEN` と `DRAFT` で 1 桁違うので、揃えないと
    // タイトルの開始位置がずれる。**定数に書かない** —— 語を足したときに片方だけ
    // 古くなる（`CLAUDE.md` の「離れた場所に『いま N ある』を書かない」）。
    // 語はすべて ASCII なので `len()` がそのまま表示幅。
    let width = list
        .iter()
        .map(|pr| status_label(pr).len())
        .max()
        .unwrap_or(0);
    let login = my_login(state);
    Rows {
        lines: list
            .iter()
            .map(|pr| pr_row(pr, state, login, width))
            .collect(),
        // **カーソルをここで丸めない。** 範囲に収めるのは `PrState::move_cursor` と
        // `PrState::seat` の側（同じ判定を 2 箇所に書かない）。
        cursor: state.prs.cursor,
    }
}

/// 出す一覧。1 件も無ければ `None`（文言に落とす）。
fn open_prs(state: &AppState) -> Option<&[PullRequest]> {
    let list = state.prs.list.loaded()?;
    (!list.is_empty()).then_some(list.as_slice())
}

/// いま使っているアカウントの login。決まっていなければ `None`。
fn my_login(state: &AppState) -> Option<&str> {
    match &state.account {
        AccountState::Resolved(account) => Some(&account.login),
        AccountState::Resolving | AccountState::Degraded(_) | AccountState::Disabled => None,
    }
}

/// `*#123 OPEN   Add adaptive layout`。
///
/// **印と状態をタイトルより前に置く。** 右列は端末幅の 40% しかなく、ratatui は行の
/// 右端で切るので、末尾に置くと**そこだけが黙って消える**（実測: 幅 100 の右列 38 桁で
/// `MERGED` が出なかった）。行の左端は切られない。
fn pr_row(pr: &PullRequest, state: &AppState, login: Option<&str>, width: usize) -> Line<'static> {
    let label = status_label(pr);
    Line::from(vec![
        Span::styled(format!(" {}", mark(pr, login)), theme::muted()),
        Span::raw(format!("#{} ", pr.number)),
        Span::styled(format!("{label:width$}"), status_style(pr, state)),
        Span::raw(format!("  {}", pr.title)),
    ])
}

/// 行頭の印（07 の「PRs ペイン」）。
///
/// **`*` と `R` は衝突しない** —— 自分の PR のレビューを自分に依頼することはできない。
///
/// **`R` が見るのは `requested_reviewers` だけ。** チーム宛ての依頼
/// （`requested_teams`）には印が付かない —— 自分がそのチームに居るかを答えるには
/// チームの数だけ membership を叩くことになる（05 で受け入れた穴）。
///
/// login が決まっていなければ（`Resolved` 以外）どちらの印も付かない。
fn mark(pr: &PullRequest, login: Option<&str>) -> char {
    let Some(login) = login else { return ' ' };
    // GitHub の login は ASCII で、大小を区別しない。
    if pr.author.eq_ignore_ascii_case(login) {
        '*'
    } else if pr
        .requested_reviewers
        .iter()
        .any(|reviewer| reviewer.eq_ignore_ascii_case(login))
    {
        'R'
    } else {
        ' '
    }
}

/// PR の状態。**`draft` を状態と並べて出す** —— どちらも「いま何であるか」で、
/// 2 列に分けるとタイトルの桁を食う。
///
/// **`MERGED` / `CLOSED` はこの一覧に出ない**（`state=open` だけを引く。01 の
/// 「現ブランチの PR」）。**それでも腕は残す** —— [`PullRequest`] は
/// [`PrStatus`] を運んでおり、出ない腕を削ると `Modal::PrCreate` や PR チェックアウトが
/// 同じ写像をもう 1 本書くことになる。
fn status_label(pr: &PullRequest) -> &'static str {
    match (pr.state, pr.draft) {
        (PrStatus::Open, true) => "DRAFT",
        (PrStatus::Open, false) => "OPEN",
        (PrStatus::Merged, _) => "MERGED",
        (PrStatus::Closed, _) => "CLOSED",
    }
}

/// **色名はここに書かない**（07）。`[theme]` の `pr_open` / `pr_merged` /
/// `pr_closed` を通す。`draft` の色は分けない —— 08 にキーが無く、足すなら設定の側。
fn status_style(pr: &PullRequest, state: &AppState) -> Style {
    let theme = &state.config.theme;
    theme::style(match pr.state {
        PrStatus::Open => &theme.pr_open,
        PrStatus::Merged => &theme.pr_merged,
        PrStatus::Closed => &theme.pr_closed,
    })
}

/// 一覧を出せない理由。
///
/// **消去法で導かない。** `AccountState` と [`Fetch`] の腕から出す（01 の
/// 「失敗の記憶」と同じ理由）。**`ui::LOADING` を使う** —— 文言を自分で組むと、
/// 既にある「読み込み中…」の見張りがこの産出元を拾えない。
///
/// **`N`（PR 作成）を案内しない。** `Modal::PrCreate` はまだ無いので、いま書くと
/// 押せないキーを案内することになる（01 の「`o`（ブラウザで開く）」）。
fn placeholder(state: &AppState) -> Cow<'static, str> {
    match &state.account {
        AccountState::Resolving => ui::LOADING.into(),
        AccountState::Degraded(reason) => format!("GitHub: {}（A）", ui::degraded(reason)).into(),
        // **巡回候補にもタブにも出ないのでここへは来ない**（01 の「右上のタブ」）。
        // それでも文言を置くのは、網羅 `match` の腕を `unreachable!()` にすると
        // 描画の途中で落ちるため。
        AccountState::Disabled => "GitHub 機能は切ってあります".into(),
        // まだ頼んでいない（`Unasked`）と、引けたが 1 件も無い、引けなかったを
        // 出し分ける。**対象がまだ決まっていない間も `Unasked`** なので、そこも
        // 「読み込み中…」に落ちる（`wanted_pr_list` が両方を見る。01）。
        AccountState::Resolved(_) => match &state.prs.list {
            Fetch::Unasked => ui::LOADING.into(),
            // 空でないときはここへ来ない（`lines` が先に返す）。
            Fetch::Loaded(_) => "開いている PR はありません".into(),
            Fetch::Failed(err) => format!("PR を読み込めませんでした: {err}").into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{draw, pane_text, state_at};
    use mewgit_core::app::{Cmd, Msg, Pane, TermSize};
    use mewgit_core::auth::{AccountState, DegradedReason};
    use mewgit_core::github::GithubError;
    use mewgit_core::test_util::{fake_pull_request, pr_list_loaded, put_on_branch, reply};

    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    /// PRs タブを開いた状態。アカウント（login は `taro`）と対象は決まっている。
    fn on_prs_tab() -> AppState {
        let mut state = state_at("/repo");
        put_on_branch(&mut state, "feat/x");
        state.focus = Pane::Prs;
        // **タブは `reseat_focus` が揃える**（01 の「右上のタブ」）。直に置くと
        // 実物では出ない画面を見張ることになる。
        state.reseat_focus();
        state
    }

    /// 一覧が届いた状態。
    fn with_list(list: Vec<PullRequest>) -> AppState {
        let mut state = on_prs_tab();
        let _ = reply(&mut state, Cmd::FetchPrList, pr_list_loaded(list));
        state
    }

    #[test]
    fn the_pane_lists_the_open_prs_with_their_number_and_state() {
        // **右列は 80x24 で 32 桁**しかないので、桁を見る見張りと混ざらないよう
        // タイトルは切れない長さにする（切れることは下の
        // `the_number_and_state_survive_a_narrow_pane…` が見る）。
        let mut open = fake_pull_request(82, "feat/x", "headsha");
        open.title = "Lanes".to_string();
        let mut draft = fake_pull_request(90, "other", "othersha");
        draft.draft = true;
        draft.title = "Bump".to_string();
        let state = with_list(vec![open, draft]);

        // **このペインの中だけを見る**（`ui::LOADING` などの文言は他のペインも出す）。
        let text = pane_text(&state, SIZE, Pane::Prs);

        assert!(text.contains("#82"), "{text}");
        assert!(text.contains("OPEN"), "{text}");
        assert!(text.contains("Lanes"), "{text}");
        assert!(text.contains("#90"), "{text}");
        assert!(text.contains("DRAFT"), "{text}");
        assert!(text.contains("Bump"), "{text}");
    }

    #[test]
    fn the_status_column_lines_the_titles_up() {
        // **桁は出す行から導く**（`lines`）。`OPEN` と `DRAFT` で 1 桁違うので、
        // 揃えないとタイトルの開始位置がずれる。
        let mut open = fake_pull_request(82, "feat/x", "headsha");
        open.title = "Lanes".to_string();
        let mut draft = fake_pull_request(90, "other", "othersha");
        draft.draft = true;
        draft.title = "Bump".to_string();
        let state = with_list(vec![open, draft]);

        let text = pane_text(&state, SIZE, Pane::Prs);
        let open = text
            .lines()
            .find(|line| line.contains("#82"))
            .expect("#82 の行");
        let drafted = text
            .lines()
            .find(|line| line.contains("#90"))
            .expect("#90 の行");

        assert_eq!(
            open.find("Lanes"),
            drafted.find("Bump"),
            "タイトルの桁がずれている:\n{open}\n{drafted}"
        );
    }

    #[test]
    fn the_mark_tells_my_prs_from_the_ones_awaiting_my_review() {
        // 印は `*`（自分の PR）と `R`（レビュー依頼）。それ以外は空白（07）。
        let mut mine = fake_pull_request(82, "feat/x", "headsha");
        mine.author = "Taro".to_string(); // login は大小を区別しない
        let mut review = fake_pull_request(90, "other", "othersha");
        review.author = "hanako".to_string();
        review.requested_reviewers = vec!["taro".to_string()];
        let mut theirs = fake_pull_request(91, "third", "thirdsha");
        theirs.author = "hanako".to_string();

        let state = with_list(vec![mine, review, theirs]);
        let text = pane_text(&state, SIZE, Pane::Prs);

        let row = |number: &str| {
            text.lines()
                .find(|line| line.contains(number))
                .unwrap_or_else(|| panic!("{number} の行が無い:\n{text}"))
                .to_string()
        };
        assert!(row("#82").contains("*#82"), "{}", row("#82"));
        assert!(row("#90").contains("R#90"), "{}", row("#90"));
        assert!(row("#91").contains(" #91"), "{}", row("#91"));
        assert!(!row("#91").contains("*#91"), "{}", row("#91"));
        assert!(!row("#91").contains("R#91"), "{}", row("#91"));
    }

    #[test]
    fn a_team_review_request_gets_no_mark() {
        // **受け入れた穴**（05 / 07）。`requested_teams` は運んでいないので、
        // チーム宛ての依頼では `R` が付かない。
        let mut team = fake_pull_request(90, "other", "othersha");
        team.author = "hanako".to_string();
        team.requested_reviewers = Vec::new();
        let state = with_list(vec![team]);

        let text = pane_text(&state, SIZE, Pane::Prs);
        assert!(!text.contains("R#90"), "{text}");
    }

    #[test]
    fn the_number_and_state_survive_a_narrow_pane_but_the_title_is_cut() {
        // **印と状態をタイトルより後ろに置くと、右列の桁ではそこだけが黙って消える**
        // （07 の「PRs ペイン」。実測で踏んだ形）。`ratatui` は行の右端で切るので、
        // **既定の 80x24 のまま**見る（広げて緑にすると見張りが効かない。#22）。
        let mut pr = fake_pull_request(82, "feat/x", "headsha");
        pr.title = "と".repeat(60);
        let state = with_list(vec![pr]);

        let text = pane_text(&state, SIZE, Pane::Prs);

        assert!(text.contains("#82 OPEN"), "{text}");
        // **切れていることも見る。** 切れない幅なら並び順を入れ替えても通るので、
        // この見張りが何を踏んでいるか判らなくなる。
        assert!(!text.contains(&"と".repeat(60)), "{text}");
    }

    #[test]
    fn every_reason_the_pane_cannot_show_the_list_is_told_apart() {
        // **消去法で導かない。** 腕ごとに違う文言が出ることをここで押さえる（07）。
        let loading = on_prs_tab();
        assert!(
            pane_text(&loading, SIZE, Pane::Prs).contains("読み込み中"),
            "頼んだ直後"
        );

        let empty = with_list(Vec::new());
        let text = pane_text(&empty, SIZE, Pane::Prs);
        // **空の `Vec` と「まだ引いていない」を出し分ける**（`Fetch` の 3 状態。01）。
        assert!(text.contains("開いている PR はありません"), "{text}");
        assert!(!text.contains("読み込み中"), "{text}");
        // **`N`（PR 作成）を案内しない。** まだ無いキーなので（01）。**キーの案内の
        // 形で見る** —— 素の `N` を見ると、いつかタイトルや login に `N` が入った
        // だけで落ちる。
        assert!(!text.contains("N で"), "{text}");
        assert!(!text.contains("[N]"), "{text}");

        let mut failed = on_prs_tab();
        let _ = reply(
            &mut failed,
            Cmd::FetchPrList,
            Msg::PrListLoaded(Err(GithubError::NotFound)),
        );
        assert!(
            pane_text(&failed, SIZE, Pane::Prs).contains("読み込めませんでした"),
            "失敗"
        );

        let mut degraded = on_prs_tab();
        degraded.account = AccountState::Degraded(DegradedReason::GhMissing);
        let text = pane_text(&degraded, SIZE, Pane::Prs);
        assert!(text.contains("GitHub"), "{text}");
        assert!(text.contains("A"), "案内が無い: {text}");
    }

    #[test]
    fn the_tab_header_names_both_tabs_and_only_one_when_github_is_off() {
        let state = with_list(vec![fake_pull_request(82, "feat/x", "headsha")]);
        let text = pane_text(&state, SIZE, Pane::Prs);
        assert!(text.contains("Branches"), "{text}");
        assert!(text.contains("PRs"), "{text}");

        // 切ってあれば `Prs` は巡回候補から落ちるので、押しても切り替わらない
        // 見出しを並べない（07 の「右上のタブ」）。
        let mut off = state_at("/repo");
        off.account = AccountState::Disabled;
        let text = draw(&off, SIZE).join("\n");
        assert!(text.contains("Branches"), "{text}");
        assert!(!text.contains("PRs"), "{text}");
    }

    #[test]
    fn a_long_list_leaves_the_pane_below_visible() {
        // 上限が無いと、PR の多いリポジトリで下のペインが潰れる（07）。
        // **既定の 80x24 のまま見る。**
        let list = (0..40)
            .map(|n| fake_pull_request(n, &format!("feat/{n:02}"), "sha"))
            .collect();
        let state = with_list(list);

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("Working tree"), "{text}");
    }
}
