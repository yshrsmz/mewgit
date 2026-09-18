//! PRs ペインの描画。設計は `docs/design/07-ui.md` の「PRs ペイン」。
//!
//! 右列の上、Branches と同じ領域をタブで差し替えて使う（07 の「右上のタブ」）。
//! **出すのはこのリポジトリで開いている PR の一覧**で、1 行 1 件。

use std::borrow::Cow;

use mewgit_core::app::{AppState, PrLoad};
use mewgit_core::auth::AccountState;
use mewgit_core::config::Theme;
use mewgit_core::github::{CheckSummary, PrStatus, PullRequest, ReviewSummary};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::Rows;
use crate::ui::{self, theme};

/// タブ見出しに出すこのペインの名前。
///
/// **件数を添えない。** 一覧が届くまで数が無く、`(0)` を先に出すと「開いている PR が
/// 無い」と読める（`PrLoad::Pending` と空の一覧を畳むのと同じ誤り。01）。
pub(super) fn title() -> &'static str {
    "PRs"
}

/// 行と、カーソルが何行目か。
///
/// 一覧を出せないときは文言 1 行（選べる行は無いので `cursor` は `None`）。
///
/// **アカウントが決まっていなければ一覧を出さない。** 印（`*` / `R`）は login で
/// 決まるので、`Resolved` でないまま一覧を描くと**印だけが黙って消えた前のアカウントの
/// 一覧**になる。落とすのは `update` の側（`PrState::forget`）だが、**描く側でも門に
/// する** —— どちらか一方だと、片方を直したときにもう片方が前提を失う。
pub(super) fn lines(state: &AppState) -> Rows {
    let placeholder = || Rows {
        lines: vec![Line::raw(placeholder(state))],
        cursor: None,
    };
    let AccountState::Resolved(account) = &state.account else {
        return placeholder();
    };
    if state.prs.list.is_empty() {
        return placeholder();
    }

    // **状態の桁は出す行から導く。** `OPEN` と `DRAFT` で 1 桁違うので、揃えないと
    // タイトルの開始位置がずれる。**定数に書かない** —— 語を足したときに片方だけ
    // 古くなる（`CLAUDE.md` の「離れた場所に『いま N ある』を書かない」）。
    // 語はすべて ASCII なので `len()` がそのまま表示幅。
    let width = state
        .prs
        .list
        .iter()
        .map(|pr| status_label(pr).len())
        .max()
        .unwrap_or(0);
    let theme = &state.config.theme;
    let ascii = state.config.graph.ascii;
    // **集計の桁も出す行から導く。** 中身は行ごとに違う（引けていない行は待ちの印 1 つ、
    // 引けた行は 0〜5 個の記号）ので、揃えないとタイトルの開始位置が行ごとにずれる。
    // **空なら桁ごと消える** —— CI もレビューも無いリポジトリで意味の無い余白を
    // 置かないため（07 の「桁を先に確保しない」）。
    let cells: Vec<(String, String)> = state
        .prs
        .list
        .iter()
        .map(|pr| (checks_cell(state, pr, ascii), reviews_cell(state, pr)))
        .collect();
    let checks_width = cells
        .iter()
        .map(|(c, _)| c.chars().count())
        .max()
        .unwrap_or(0);
    let reviews_width = cells
        .iter()
        .map(|(_, r)| r.chars().count())
        .max()
        .unwrap_or(0);
    Rows {
        lines: state
            .prs
            .list
            .iter()
            .zip(&cells)
            .map(|(pr, cell)| {
                pr_row(
                    pr,
                    theme,
                    &account.login,
                    width,
                    cell,
                    (checks_width, reviews_width),
                )
            })
            .collect(),
        // **カーソルをここで丸めない。** 範囲に収めるのは `PrState::move_cursor` と
        // `PrState::loaded` の側（同じ判定を 2 箇所に書かない）。**反転を当てるのも
        // ここではない** —— `super::TopPane::render` が 1 箇所で当てる。
        cursor: Some(state.prs.cursor),
    }
}

/// `*#123 OPEN   Add adaptive layout`。
///
/// **印と状態をタイトルより前に置く。** 右列は端末幅の 40% しかなく、ratatui は行の
/// 右端で切るので、末尾に置くと**そこだけが黙って消える**（実測: 幅 100 の右列 38 桁で
/// `MERGED` が出なかった）。行の左端は切られない。
///
/// **`AppState` ごとは受け取らない。** 要るのは `theme` と `login` の 2 つで、
/// `AppState` を渡すと「状態から引いた login と、引数の login が食い違う対」が型として
/// 組める（`CLAUDE.md` の「導出できる値を引数に取らせない」）。
fn pr_row(
    pr: &PullRequest,
    theme: &Theme,
    login: &str,
    width: usize,
    cell: &(String, String),
    cells: (usize, usize),
) -> Line<'static> {
    let label = status_label(pr);
    let (checks, reviews) = cell;
    let (checks_width, reviews_width) = cells;
    let mut spans = vec![
        Span::styled(mark(pr, login), theme::muted()),
        Span::raw(format!("#{} ", pr.number)),
        Span::styled(format!("{label:width$}"), status_style(pr, theme)),
    ];
    // **幅 0 の桁は区切りごと出さない。** `format!("{:0$}", 0)` は空文字を返すが、
    // 区切りの空白は残るので、ここで畳まないと CI もレビューも無いリポジトリで
    // タイトルが 2 桁ぶん右へずれる。
    if checks_width > 0 {
        spans.push(Span::styled(
            format!("  {checks:checks_width$}"),
            theme::muted(),
        ));
    }
    if reviews_width > 0 {
        spans.push(Span::styled(
            format!("  {reviews:reviews_width$}"),
            theme::muted(),
        ));
    }
    spans.push(Span::raw(format!("  {}", pr.title)));
    Line::from(spans)
}

/// チェックの集計 1 行ぶん（07 の「PRs ペイン」）。
///
/// **3 つの意味を 1 つの桁に載せる。** 集計が**居ない**のは「まだ引いていない」
/// （`app::PrDetailEntry` の doc）、`Err` は「引けなかった」、`Ok` は数えた結果。
/// **どれも同じ見た目にしない** —— 引けなかった行を「チェック 0 件」と同じに
/// 見せると、CI の無い PR と CI を読めなかった PR が区別できなくなる。
///
/// **`CheckState` の 5 値を 3 つに畳まない**（07）。`Skipped` と `Neutral` を成功か
/// 失敗のどちらかに足すとどちらも嘘になる。
fn checks_cell(state: &AppState, pr: &PullRequest, ascii: bool) -> String {
    let Some(entry) = state.prs.detail(pr.number) else {
        return ui::pick_charset(CHECK_WAIT, ascii).to_string();
    };
    match &entry.result {
        // **理由はここに出ない。** 枠の内側に入らないので、出せるのは「引けなかった」
        // ことだけ（07 で受け入れた穴）。打ち切りの理由は通知に出る。
        Err(_) => CHECK_ERROR.to_string(),
        Ok(detail) => check_counts(&detail.checks, ascii),
    }
}

/// `✓2 ✗1`。**0 のものは出さない**（Branches ペインの `↑` `↓` と同じ）。
/// 1 件も無ければ空文字で、桁ごと消える。
fn check_counts(checks: &CheckSummary, ascii: bool) -> String {
    let mut out = String::new();
    for (count, glyphs) in [
        (checks.success, CHECK_OK),
        (checks.failure, CHECK_FAIL),
        (checks.pending, CHECK_PENDING),
        (checks.skipped, CHECK_SKIPPED),
        (checks.neutral, CHECK_NEUTRAL),
    ] {
        if count == 0 {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(ui::pick_charset(glyphs, ascii));
        out.push_str(&count.to_string());
    }
    out
}

/// レビューの集計 1 行ぶん。`A2 C1`。
///
/// **記号ではなく英字にする。** `✓` / `✗` はチェックの語彙で、同じ記号を使うと
/// 「CI が通った」と「人が承認した」が同じ見た目になる（07）。
///
/// **`commented` は出さない。** 承認でも変更要求でもないコメントは、出しても次に
/// 何をするかが変わらない。**引いた値は捨てていない**（`ReviewSummary` が持っている）
/// ので、出す場所ができたときに数え直さずに済む。
///
/// **まだ引いていない行と引けなかった行では空にする** —— 待ちの印も失敗の印も
/// チェックの桁が出しており、同じことを 2 桁が言う必要が無い。
fn reviews_cell(state: &AppState, pr: &PullRequest) -> String {
    let Some(entry) = state.prs.detail(pr.number) else {
        return String::new();
    };
    match &entry.result {
        Err(_) => String::new(),
        Ok(detail) => review_counts(&detail.review),
    }
}

fn review_counts(review: &ReviewSummary) -> String {
    let mut out = String::new();
    for (count, glyph) in [
        (review.approved, REVIEW_APPROVED),
        (review.changes_requested, REVIEW_CHANGES),
    ] {
        if count == 0 {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(glyph);
        out.push_str(&count.to_string());
    }
    out
}

/// チェックの記号。`(UTF-8, ASCII)`。**`config.graph.ascii` を通す**
/// （Branches ペインの `↑` `↓` と同じ理由で、同じ端末で一部だけ化けるのを避ける）。
const CHECK_OK: (&str, &str) = ("✓", "+");
const CHECK_FAIL: (&str, &str) = ("✗", "x");
const CHECK_PENDING: (&str, &str) = ("○", "o");
const CHECK_SKIPPED: (&str, &str) = ("⊘", "-");
const CHECK_NEUTRAL: (&str, &str) = ("·", "~");
/// まだ集計を引いていない行。
const CHECK_WAIT: (&str, &str) = ("…", ".");
/// 集計を引けなかった行。**ASCII とそうでない版を分けない** —— どちらも `!`。
const CHECK_ERROR: &str = "!";
/// レビューの記号。**英字**（上の `reviews_cell` の doc）。
const REVIEW_APPROVED: &str = "A";
const REVIEW_CHANGES: &str = "C";

/// 行頭の印（07 の「PRs ペイン」）。先頭の 1 セルは余白。
///
/// **`*` と `R` は衝突しない** —— 自分の PR のレビューを自分に依頼することはできない。
///
/// **`R` が見るのは `requested_reviewers` だけ。** チーム宛ての依頼
/// （`requested_teams`）には印が付かない —— 自分がそのチームに居るかを答えるには
/// チームの数だけ membership を叩くことになる（05 で受け入れた穴）。
fn mark(pr: &PullRequest, login: &str) -> &'static str {
    // GitHub の login は ASCII で、大小を区別しない。
    if pr.author.eq_ignore_ascii_case(login) {
        " *"
    } else if pr
        .requested_reviewers
        .iter()
        .any(|reviewer| reviewer.eq_ignore_ascii_case(login))
    {
        " R"
    } else {
        "  "
    }
}

/// PR の状態。**`draft` を状態と並べて出す** —— どちらも「いま何であるか」で、
/// 2 列に分けるとタイトルの桁を食う。
///
/// **`MERGED` / `CLOSED` はこの一覧に出ない**（`state=open` だけを引く。01 の
/// 「現ブランチの PR」）。[`PrStatus`] が 3 値なので腕は網羅 `match` が要求しており、
/// 選べるのは**出ない腕に何と書くか**だけ。`Modal::PrCreate` や PR チェックアウトが
/// 同じ写像をもう 1 本書かずに済むよう、4 語とも本来の語にしてある。
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
fn status_style(pr: &PullRequest, theme: &Theme) -> Style {
    theme::style(match pr.state {
        PrStatus::Open => &theme.pr_open,
        PrStatus::Merged => &theme.pr_merged,
        PrStatus::Closed => &theme.pr_closed,
    })
}

/// 一覧を出せない理由。
///
/// **消去法で導かない。** [`AccountState`] と [`PrLoad`] の腕から出す（01 の
/// 「失敗の記憶」と同じ理由）。**`ui::LOADING` を使う** —— 文言を自分で組むと、
/// 既にある「読み込み中…」の見張りがこの産出元を拾えない。
///
/// **`N`（PR 作成）を案内しない。** `Modal::PrCreate` はまだ無いので、いま書くと
/// 押せないキーを案内することになる（01 の「`o`（ブラウザで開く）」）。
fn placeholder(state: &AppState) -> Cow<'static, str> {
    match &state.account {
        // **既定リモートが GitHub でないときもここに落ちる**（`TargetState::Unavailable`
        // なら `Cmd::ResolveAccount` が出ないので `Resolving` のまま）。理由を出せる
        // のは `A` だけで、このペインでは「読み込み中…」のまま留まる。**枠の内側は
        // 80x24 で 30 セル**しか無く、理由（`A` が出す「既定リモートから GitHub の
        // リポジトリを特定できません」）が入らない。畳み方は plan.md に送ってある。
        AccountState::Resolving => ui::LOADING.into(),
        AccountState::Degraded(reason) => format!("GitHub: {}（A）", ui::degraded(reason)).into(),
        // **巡回候補にもタブにも出ないのでここへは来ない**（01 の「右上のタブ」）。
        // それでも文言を置くのは、網羅 `match` の腕を `unreachable!()` にすると
        // 描画の途中で落ちるため。
        AccountState::Disabled => "GitHub 機能は切ってあります".into(),
        // まだ頼んでいない・引けたが 1 件も無い・引けなかったを出し分ける。
        // **一覧が空のときしか来ない**（`lines` が先に返す）ので、`Failed` でも
        // 読めていた一覧があればそちらが出る。
        AccountState::Resolved(_) => match state.prs.load() {
            PrLoad::Pending => ui::LOADING.into(),
            PrLoad::Loaded => "開いている PR はありません".into(),
            PrLoad::Failed(err) => format!("PR を読み込めませんでした: {err}").into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{draw, pane_row_is_reversed, pane_text, state_at};
    use mewgit_core::app::update;
    use mewgit_core::app::{Cmd, Msg, Pane, TermSize};
    use mewgit_core::auth::{AccountState, DegradedReason};
    use mewgit_core::github::{GithubError, PrDetail};
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

    /// `#82 OPEN "Lanes"` と `#90 DRAFT "Bump"` の 2 件。
    ///
    /// **右列は 80x24 で 32 桁**しかないので、桁を見る見張りと混ざらないようタイトルは
    /// 切れない長さにする（切れることは
    /// `the_number_and_state_survive_a_narrow_pane…` が見る）。
    fn open_and_draft() -> Vec<PullRequest> {
        let mut open = fake_pull_request(82, "feat/x", "headsha");
        open.title = "Lanes".to_string();
        let mut draft = fake_pull_request(90, "other", "othersha");
        draft.draft = true;
        draft.title = "Bump".to_string();
        vec![open, draft]
    }

    /// 集計まで届いた状態。
    ///
    /// **`Cmd` を `AppState::issue` に通してから `Msg` を届ける**（`test_util::reply`
    /// と同じ理由）。通さないと、ジョブの登録されていない実物では起きない形になる。
    fn with_details(
        list: Vec<PullRequest>,
        answers: &[(u64, &str, Result<PrDetail, GithubError>)],
    ) -> AppState {
        let mut state = with_list(list);
        let targets = answers
            .iter()
            .map(|(number, sha, _)| (*number, (*sha).to_string()))
            .collect();
        let _ = state.issue(Cmd::FetchPrDetails { targets });
        for (number, sha, result) in answers {
            let _ = update(
                &mut state,
                Msg::PrDetailLoaded {
                    number: *number,
                    sha: (*sha).to_string(),
                    result: result.clone(),
                },
            );
        }
        let _ = update(&mut state, Msg::PrDetailsFinished(None));
        state
    }

    /// 5 値をすべて 1 件ずつ持つ集計。
    fn every_state() -> PrDetail {
        PrDetail {
            checks: CheckSummary {
                success: 2,
                failure: 1,
                pending: 1,
                skipped: 1,
                neutral: 1,
            },
            review: ReviewSummary::default(),
        }
    }

    #[test]
    fn the_row_shows_every_check_state_and_does_not_fold_them() {
        // **`Skipped` と `Neutral` を成功か失敗に足すとどちらも嘘になる**（07）。
        // 5 値のうち 1 つでも畳むと、ここで記号が 1 つ消える。
        let state = with_details(open_and_draft(), &[(82, "headsha", Ok(every_state()))]);

        let text = pane_text(&state, SIZE, Pane::Prs);
        let row = text
            .lines()
            .find(|line| line.contains("#82"))
            .expect("#82 の行");
        assert!(row.contains("✓2"), "{row}");
        assert!(row.contains("✗1"), "{row}");
        assert!(row.contains("○1"), "{row}");
        assert!(row.contains("⊘1"), "{row}");
        assert!(row.contains("·1"), "{row}");
    }

    #[test]
    fn a_row_tells_apart_not_asked_yet_from_could_not_be_read() {
        // **3 つの意味が 1 つの桁に載る**（`checks_cell`）。同じ見た目にすると、
        // CI の無い PR と CI を読めなかった PR が区別できない。
        let state = with_details(
            open_and_draft(),
            &[(82, "headsha", Err(GithubError::NotFound))],
        );

        let text = pane_text(&state, SIZE, Pane::Prs);
        let failed = text
            .lines()
            .find(|line| line.contains("#82"))
            .expect("#82 の行");
        let waiting = text
            .lines()
            .find(|line| line.contains("#90"))
            .expect("#90 の行");
        assert!(failed.contains('!'), "引けなかった行: {failed}");
        assert!(waiting.contains('…'), "まだ引いていない行: {waiting}");
    }

    #[test]
    fn the_ascii_setting_swaps_the_check_glyphs() {
        // Branches ペインの `↑` `↓` と同じ理由（同じ端末で一部だけ化けるのを避ける）。
        let mut state = with_details(open_and_draft(), &[(82, "headsha", Ok(every_state()))]);
        state.config.graph.ascii = true;

        let text = pane_text(&state, SIZE, Pane::Prs);
        let row = text
            .lines()
            .find(|line| line.contains("#82"))
            .expect("#82 の行");
        assert!(row.contains("+2"), "{row}");
        assert!(row.contains("x1"), "{row}");
        assert!(row.contains("o1"), "{row}");
        assert!(!row.contains('✓'), "{row}");
        // まだ引いていない行の待ちの印も同じ設定に従う。
        let waiting = text
            .lines()
            .find(|line| line.contains("#90"))
            .expect("#90 の行");
        assert!(!waiting.contains('…'), "{waiting}");
    }

    #[test]
    fn the_review_column_uses_letters_so_it_is_not_read_as_a_check() {
        // **`✓` / `✗` はチェックの語彙。** 同じ記号を使うと「CI が通った」と
        // 「人が承認した」が同じ見た目になる（07）。
        let detail = PrDetail {
            checks: CheckSummary::default(),
            review: ReviewSummary {
                approved: 1,
                changes_requested: 2,
                commented: 3,
            },
        };
        let state = with_details(open_and_draft(), &[(82, "headsha", Ok(detail))]);

        let text = pane_text(&state, SIZE, Pane::Prs);
        let row = text
            .lines()
            .find(|line| line.contains("#82"))
            .expect("#82 の行");
        assert!(row.contains("A1"), "{row}");
        assert!(row.contains("C2"), "{row}");
        // **`commented` は出さない** —— 出しても次に何をするかが変わらない。
        assert!(!row.contains("3"), "コメント数まで出ている: {row}");
    }

    #[test]
    fn the_summary_columns_disappear_when_no_row_has_anything_to_show() {
        // **桁を先に確保しない**（07）。CI もレビューも無いリポジトリで、意味の無い
        // 余白がタイトルの桁を食わないこと。
        let empty = PrDetail {
            checks: CheckSummary::default(),
            review: ReviewSummary::default(),
        };
        let state = with_details(
            open_and_draft(),
            &[(82, "headsha", Ok(empty)), (90, "othersha", Ok(empty))],
        );

        let text = pane_text(&state, SIZE, Pane::Prs);
        let row = text
            .lines()
            .find(|line| line.contains("#82"))
            .expect("#82 の行");
        // `OPEN ` は `DRAFT` に合わせた 1 桁の詰めぶんで、そのあとは区切りの 2 桁。
        // 集計の桁が残っていればここに割り込む。
        assert!(row.contains("#82 OPEN   Lanes"), "桁が残っている: {row}");
    }

    #[test]
    fn the_summary_column_lines_the_titles_up() {
        // 状態の桁と同じ理由。行ごとに中身の幅が違うので、揃えないとタイトルの
        // 開始位置がずれる。
        let state = with_details(open_and_draft(), &[(82, "headsha", Ok(every_state()))]);

        let text = pane_text(&state, SIZE, Pane::Prs);
        let wide = text
            .lines()
            .find(|line| line.contains("#82"))
            .expect("#82 の行");
        let narrow = text
            .lines()
            .find(|line| line.contains("#90"))
            .expect("#90 の行");
        assert_eq!(
            wide.find("Lanes"),
            narrow.find("Bump"),
            "揃っていない\n{wide}\n{narrow}"
        );
    }

    #[test]
    fn the_pane_lists_the_open_prs_with_their_number_and_state() {
        let state = with_list(open_and_draft());

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
        let state = with_list(open_and_draft());

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
    fn the_cursor_row_is_highlighted() {
        // **ペインの桁だけを見る。** 全幅を見る `row_is_reversed` だと、右列の 1 行目が
        // 左のグラフのカーソル行と同じ端末行に来るので、反転していなくても真になる
        // （`pane_row_is_reversed` の doc）。
        let state = with_list(open_and_draft());
        assert_eq!(state.prs.cursor, 0, "現ブランチの PR に座っている");
        assert!(
            pane_row_is_reversed(&state, SIZE, Pane::Prs, "#82"),
            "カーソル行が反転していない"
        );
        assert!(
            !pane_row_is_reversed(&state, SIZE, Pane::Prs, "#90"),
            "座っていない行が反転している"
        );

        let mut moved = with_list(open_and_draft());
        moved.prs.cursor = 1;
        assert!(
            pane_row_is_reversed(&moved, SIZE, Pane::Prs, "#90"),
            "カーソルを動かしても反転が付いてこない"
        );
        assert!(!pane_row_is_reversed(&moved, SIZE, Pane::Prs, "#82"));
    }

    #[test]
    fn the_placeholder_row_is_not_highlighted() {
        // 文言 1 行は選択肢ではない（`Rows::cursor` が `None`）。反転させると
        // 「選べる行がある」と読める。
        let empty = with_list(Vec::new());
        assert!(!pane_row_is_reversed(
            &empty,
            SIZE,
            Pane::Prs,
            "開いている PR はありません"
        ));
    }

    #[test]
    fn losing_the_account_takes_the_list_off_the_screen() {
        // **印（`*` / `R`）は login で決まる**ので、`Resolved` でないまま一覧を描くと
        // 印だけが黙って消えた前のアカウントの一覧になる。**状態の側でも落としている**
        // （`PrState::forget`）が、描く側でも門にする。
        let mut state = with_list(open_and_draft());
        assert!(
            pane_text(&state, SIZE, Pane::Prs).contains("#82"),
            "前提が崩れている"
        );

        // 状態を落とさずにアカウントだけ degraded にする（描く側の門を直に踏む）。
        state.account = AccountState::Degraded(DegradedReason::GhMissing);
        let text = pane_text(&state, SIZE, Pane::Prs);
        assert!(
            !text.contains("#82"),
            "前のアカウントの一覧が出ている: {text}"
        );
        assert!(text.contains("GitHub"), "{text}");
    }

    #[test]
    fn a_failed_reload_keeps_the_list_that_was_already_on_the_screen() {
        // 引き直しは ref を動かす書き込みのたびに来るので、捨てると commit 直後に
        // 1 回転けただけでペインがエラー 1 行になる（`PrState::failed`）。
        let mut state = with_list(open_and_draft());
        let _ = reply(
            &mut state,
            Cmd::FetchPrList,
            Msg::PrListLoaded(Err(GithubError::NotFound)),
        );

        let text = pane_text(&state, SIZE, Pane::Prs);
        assert!(text.contains("#82"), "読めていた一覧が消えた: {text}");
        assert!(!text.contains("読み込めませんでした"), "{text}");
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
