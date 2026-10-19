//! PRs ペインの描画。設計は `docs/design/07-ui.md` の「PRs ペイン」。
//!
//! 右列の上、Branches と同じ領域をタブで差し替えて使う（07 の「右上のタブ」）。
//! **出すのはこのリポジトリで開いている PR の一覧**で、1 行 1 件。

use std::borrow::Cow;

use mewgit_core::app::keys::{self, Action, CommonAction, PrsAction};
use mewgit_core::app::{AppState, GithubBlock, PrLoad};
use mewgit_core::auth::AccountState;
use mewgit_core::config::Theme;
use mewgit_core::github::{GithubError, PrDetail, PrStatus, PullRequest};
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
///
/// `width` は枠の内側の幅で、集計の桁の上限を出すのに使う（[`summary_budget`]）。
pub(super) fn lines(state: &AppState, width: u16) -> Rows {
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
    // 古くなる（`.claude/rules/rust-coding.md` の「離れた場所に『いま N ある』を書かない」）。
    // 語はすべて ASCII なので `len()` がそのまま表示幅。
    let width_of_status = state
        .prs
        .list
        .iter()
        .map(|pr| status_label(pr).len())
        .max()
        .unwrap_or(0);
    let theme = &state.config.theme;
    let charset = ui::charset(state);
    let budget = summary_budget(state, &account.login, usize::from(width), width_of_status);
    // **集計の桁も出す行から導く。** 中身は行ごとに違う（引けていない行は待ちの印 1 つ、
    // 引けた行は記号が並ぶ）ので、揃えないとタイトルの開始位置が行ごとにずれる。
    // **どの行も出すものが無ければ桁ごと消える** —— CI もレビューも無いリポジトリで
    // 意味の無い余白を置かないため（07 の「桁を先に確保しない」）。
    let cells = column(
        state
            .prs
            .list
            .iter()
            .map(|pr| summary_cell(state.prs.detail(pr.number), charset, budget)),
    );
    Rows {
        lines: state
            .prs
            .list
            .iter()
            .zip(&cells)
            .map(|(pr, cell)| pr_row(pr, theme, &account.login, width_of_status, cell))
            .collect(),
        // **カーソルをここで丸めない。** 範囲に収めるのは `PrState::move_cursor` と
        // `PrState::loaded` の側（同じ判定を 2 箇所に書かない）。**反転を当てるのも
        // ここではない** —— `super::TopPane::render` が 1 箇所で当てる。
        cursor: Some(state.prs.cursor),
    }
}

/// 桁 1 本ぶんのセルを、いちばん広いものに合わせて詰める。
///
/// **どれも空なら詰めない。** 幅 0 に詰めると区切りの 2 桁だけが残り、タイトルが
/// そのぶん右へずれる。
fn column(cells: impl Iterator<Item = String>) -> Vec<String> {
    let cells: Vec<String> = cells.collect();
    let width = cells
        .iter()
        .map(|cell| cell.chars().count())
        .max()
        .unwrap_or(0);
    if width == 0 {
        return cells;
    }
    cells
        .into_iter()
        .map(|cell| format!("{COLUMN_GAP}{cell:width$}"))
        .collect()
}

/// `*#123 OPEN   ✓3 ✗1 A1  Add adaptive layout`。
///
/// **印と状態と集計をタイトルより前に置く。** 右列は端末幅の 40% しかなく、ratatui は
/// 行の右端で切るので、末尾に置くと**そこだけが黙って消える**（実測: 幅 100 の右列
/// 38 桁で `MERGED` が出なかった）。行の左端は切られない。
///
/// **その守りは自分にも返る。** 集計の桁を無制限に伸ばすと、今度は**タイトルが
/// 1 文字も出なくなる**（[`summary_budget`] の doc）。伸びないように
/// するのは [`summary_budget`] のほう。
///
/// **`AppState` ごとは受け取らない。** 要るのは `theme` と `login` と組み終えたセル
/// だけで、`AppState` を渡すと「状態から引いた login と、引数の login が食い違う対」が
/// 型として組める（`.claude/rules/rust-coding.md` の「導出できる値を引数に取らせない」）。
/// **セルも組み終えたものを受ける** —— 幅は一覧ぜんぶから決まるので、1 行だけ見て
/// 出せない。
fn pr_row(
    pr: &PullRequest,
    theme: &Theme,
    login: &str,
    width: usize,
    summary: &str,
) -> Line<'static> {
    let label = status_label(pr);
    let [mark, number] = lead(pr, login);
    Line::from(vec![
        mark,
        number,
        Span::styled(format!("{label:width$}"), status_style(pr, theme)),
        // **空なら 1 セルも描かれない**ので、幅 0 の桁はここで消える。
        Span::styled(summary.to_string(), theme::muted()),
        Span::raw(format!("{COLUMN_GAP}{}", pr.title)),
    ])
}

/// 行頭の印と `#番号 `。**[`pr_row`] が描くものと [`summary_budget`] が測るものを
/// 同じ値にする** —— 書式を 2 箇所に書くと、片方を変えたときに上限だけが黙ってずれる。
fn lead(pr: &PullRequest, login: &str) -> [Span<'static>; 2] {
    [
        Span::styled(mark(pr, login), theme::muted()),
        Span::raw(format!("#{} ", pr.number)),
    ]
}

/// 1 行ぶんの集計（07 の「PRs ペイン」）。
///
/// **3 つの意味を 1 箇所で分ける。** 集計が**居ない**のは「まだ引いていない」
/// （`PrState::detail`）、`Err` は「引けなかった」、`Ok` は数えた結果。
/// **どれも同じ見た目にしない** —— 引けなかった行を「チェック 0 件」と同じに
/// 見せると、CI の無い PR と CI を読めなかった PR が区別できなくなる。
///
/// **`&AppState` も `&PullRequest` も取らない。** 引くのは呼ぶ側（一覧を回っている
/// ところ）で、ここが自分で引くと**一覧に居ない PR を渡された対**が型として組め、
/// 黙って「まだ引いていない」を描く。
///
/// **チェックとレビューを 1 本の桁にする。** 別々の桁にすると区切りの 2 セルが
/// もう 1 組要り、狭い右列ではそれだけでタイトルが消える（[`summary_budget`] の doc）。
fn summary_cell(
    detail: Option<&Result<PrDetail, GithubError>>,
    charset: ui::Charset,
    budget: usize,
) -> String {
    match detail {
        None => charset.pick(CHECK_WAIT).to_string(),
        // **理由はここに出ない。** 枠の内側に入らないので、出せるのは「引けなかった」
        // ことだけ（07 で受け入れた穴）。打ち切りの理由は通知に出る。
        Some(Err(_)) => charset.pick(CHECK_ERROR).to_string(),
        Some(Ok(detail)) => {
            let checks = &detail.checks;
            let review = &detail.review;
            counts(
                &[
                    (checks.success, charset.pick(CHECK_OK)),
                    (checks.failure, charset.pick(CHECK_FAIL)),
                    (checks.pending, charset.pick(CHECK_PENDING)),
                    (checks.skipped, charset.pick(CHECK_SKIPPED)),
                    (checks.neutral, charset.pick(CHECK_NEUTRAL)),
                    // **`commented` は出さない** —— 承認でも変更要求でもないコメントは、
                    // 出しても次に何をするかが変わらない。**値は捨てていない**ので、出す
                    // 場所ができたときに数え直さずに済む。
                    (review.approved, REVIEW_APPROVED),
                    (review.changes_requested, REVIEW_CHANGES),
                ],
                budget,
            )
        }
    }
}

/// `✓2 ✗1 A1`。**0 のものは出さない**（Branches ペインの `↑` `↓` と同じ）。
/// 1 件も無ければ空文字で、桁ごと消える。
///
/// **`budget` を超えたら後ろを落として `+` を付ける。** 落とすのは
/// 並びの後ろから —— 前に置いてあるほど「次に何をするか」が変わる（成功と失敗が
/// いちばん前、レビューがいちばん後ろ）。**`+` を付けるのは、落としたことを
/// 言わずに消すと「それ以上は無い」と読めるから。**
///
/// **いちばん前の 1 つは `budget` を越えても出す**（07 の「PRs ペイン」）。狭い端末で
/// 上限が数セルに縮んでも、成功か失敗のどちらかは読める。
fn counts(items: &[(usize, &str)], budget: usize) -> String {
    let mut out = String::new();
    let mut dropped = false;
    for (count, glyph) in items {
        if *count == 0 {
            continue;
        }
        let group = format!("{glyph}{count}");
        let sep = usize::from(!out.is_empty());
        // `+` のぶんを 1 セル残して測る。
        if !out.is_empty()
            && out.chars().count() + sep + group.chars().count() > budget.saturating_sub(1)
        {
            dropped = true;
            continue;
        }
        if sep == 1 {
            out.push(' ');
        }
        out.push_str(&group);
    }
    if dropped {
        out.push('+');
    }
    out
}

/// 集計の桁に許す幅（07 の「PRs ペイン」）。
///
/// **枠の内側の幅 `width` から、集計の前後に並ぶものを引き、タイトルに [`TITLE_MIN`] を
/// 残した残り。** 行頭（[`lead`]）と状態の桁は一覧の最大から出す —— 状態は行ごとに
/// 揃えて描き（[`pr_row`]）、行頭はいちばん広い行でタイトルが削られる。
///
/// **幅 100 の右列（内側 38 セル）で番号が 2 桁・状態の桁が 5（`DRAFT` がある）なら 17**
/// （`OPEN` だけなら 18）。チェック 5 種の
/// `✓12 ✗3 ○2 ⊘5 ·1`（15 セル）に `+` を足しても入る。定数だったころは「幅 100 で
/// 測った 17」で、右列が広い端末でも `+` で畳まれていた。
fn summary_budget(state: &AppState, login: &str, width: usize, status: usize) -> usize {
    let lead = state
        .prs
        .list
        .iter()
        .map(|pr| lead(pr, login).iter().map(Span::width).sum::<usize>())
        .max()
        .unwrap_or(0);
    width.saturating_sub(lead + status + 2 * COLUMN_GAP.len() + TITLE_MIN)
}

/// 集計の桁の前と、タイトルの前に置く空白（[`column`] と [`pr_row`]）。
const COLUMN_GAP: &str = "  ";

/// 集計がいちばん広い行でも、タイトルに残す幅。
///
/// **定数だったころの上限 17 を決めたときに残っていた幅**（07 の「PRs ペイン」）。
/// これを 0 にするとタイトルが 1 文字も出ない行ができる。
const TITLE_MIN: usize = 6;

/// チェックの記号。`(UTF-8, ASCII)`。**`config.graph.ascii` を通す**
/// （Branches ペインの `↑` `↓` と同じ理由で、同じ端末で一部だけ化けるのを避ける）。
///
/// **中身が同じものも組にしておく**（`CHECK_ERROR`）—— そうしておけば「門を
/// 通し忘れた記号」と「意図して同じにした記号」が型で見分けられる。
const CHECK_OK: (&str, &str) = ("✓", "+");
const CHECK_FAIL: (&str, &str) = ("✗", "x");
const CHECK_PENDING: (&str, &str) = ("○", "o");
const CHECK_SKIPPED: (&str, &str) = ("⊘", "-");
const CHECK_NEUTRAL: (&str, &str) = ("·", "~");
/// まだ集計を引いていない行。
const CHECK_WAIT: (&str, &str) = ("…", ".");
/// 集計を引けなかった行。
const CHECK_ERROR: (&str, &str) = ("!", "!");
/// レビューの記号。**英字にする** —— `✓` / `✗` はチェックの語彙で、同じ記号を使うと
/// 「CI が通った」と「人が承認した」が同じ見た目になる（07）。
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

/// 引けたが open な PR が 1 件も無いときの文言。`（N）` を添えるかは `placeholder` が決める。
const EMPTY: &str = "開いている PR はありません";

/// 既定リモートから GitHub のリポジトリを特定できなかったときの文言（07 の「PRs ペイン」）。
///
/// **通知（`GithubBlock::notice`）とは分ける。** あちらの文言は右列がいちばん狭い幅の
/// 内側に入らない（`.claude/rules/rust-coding.md` の「置かれる場所の予算が違うなら別の
/// 文言でよい」）。**キーを添えない** —— `A` を押しても
/// 同じ事実が通知に出るだけで、直す操作を mewgit は持っていない。
const TARGET_UNAVAILABLE: &str = "GitHub のリポジトリを特定できません";

/// 一覧を出せない理由。
///
/// **消去法で導かない。** [`AccountState`] と [`PrLoad`] の腕から出す（01 の
/// 「失敗の記憶」と同じ理由）。**`ui::LOADING` を使う** —— 文言を自分で組むと、
/// 既にある「読み込み中…」の見張りがこの産出元を拾えない。
///
/// **空の一覧には、`N`（PR 作成）が効くときだけ `（N）` を添える**（degraded の `（A）` と
/// 同じ書き方。07 の「PRs ペイン」）。幅 100 以上ではフォーカスが Graph のままこの文言が
/// 見えるので、そこで添えると押しても何も起きない案内になる。**効くかは
/// [`keys::live_key`] に聞く** —— `focus` をここで見ると、モーダル表示中の扱いなど
/// 振り分けの規則の写しが 2 本になる。**字も表から引く**（動作で聞くので、
/// `config.keymap` で上書きしたキーがそのまま出る。08）。
fn placeholder(state: &AppState) -> Cow<'static, str> {
    match &state.account {
        // **既定リモートが GitHub でないときもここに落ちる**（`TargetState::Unavailable`
        // なら `Cmd::ResolveAccount` が出ないので `Resolving` のまま）。「読み込み中…」を
        // 出すと**永久に待たせる**ので、待てば変わるかを `github_block` に聞く。
        AccountState::Resolving if state.github_block() == Some(GithubBlock::TargetUnavailable) => {
            TARGET_UNAVAILABLE.into()
        }
        AccountState::Resolving => ui::LOADING.into(),
        // **`A` も `N` と同じく `live_key` で引く**（07 の「PRs ペイン」）。引けなければ括弧ごと出さない。
        AccountState::Degraded(reason) => {
            match keys::live_key(state, Action::Common(CommonAction::Account)) {
                Some(key) => format!("GitHub: {}（{key}）", ui::degraded(reason)).into(),
                None => format!("GitHub: {}", ui::degraded(reason)).into(),
            }
        }
        // **巡回候補にもタブにも出ないのでここへは来ない**（01 の「右上のタブ」）。
        // それでも文言を置くのは、網羅 `match` の腕を `unreachable!()` にすると
        // 描画の途中で落ちるため。
        AccountState::Disabled => "GitHub 機能は切ってあります".into(),
        // まだ頼んでいない・引けたが 1 件も無い・引けなかったを出し分ける。
        // **一覧が空のときしか来ない**（`lines` が先に返す）ので、`Failed` でも
        // 読めていた一覧があればそちらが出る。
        AccountState::Resolved(_) => match state.prs.load() {
            PrLoad::Pending => ui::LOADING.into(),
            PrLoad::Loaded => match keys::live_key(state, Action::Prs(PrsAction::Create)) {
                Some(key) => format!("{EMPTY}（{key}）").into(),
                None => EMPTY.into(),
            },
            PrLoad::Failed(err) => format!("PR を読み込めませんでした: {err}").into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{
        col_of, draw, pane_row, pane_row_is_reversed, pane_text, state_at, terminal_with,
    };
    use crate::ui;
    use mewgit_core::app::{update, Cmd, Key, KeyCode, Msg, Pane, TargetState, TermSize};
    use mewgit_core::auth::{AccountState, DegradedReason};
    use mewgit_core::github::{CheckSummary, ReviewSummary};
    use mewgit_core::test_util::{
        deliver_pr_details, fake_pull_request, fake_repo_snapshot, fetch_pr_details, keymap_with,
        pr_list_loaded, put_on_branch, reply,
    };
    use ratatui::style::Modifier;

    const SIZE: TermSize = crate::test_util::SPLIT_NARROWEST;

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
    /// **右列は狭い**ので、桁を見る見張りと混ざらないようタイトルは
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

    /// 集計まで届いた状態。**手順は core の `test_util` が持つ**（クレートごとに
    /// 書くと、片方だけが実物では起きない形に退化しても両方緑になる）。
    fn with_details(
        list: Vec<PullRequest>,
        answers: &[(u64, &str, Result<PrDetail, GithubError>)],
    ) -> AppState {
        let mut state = with_list(list);
        let ask = fetch_pr_details(&state, answers);
        let _ = deliver_pr_details(&mut state, ask, answers, None);
        state
    }

    /// `needle` が何**セル**目から始まるか。
    ///
    /// **`str::find` を使わない。** あれが返すのは**バイト**位置で、記号が多バイトの
    /// 行と ASCII だけの行を比べると、揃っているのにずれて見える（実際に踏んだ）。
    fn column_of(row: &str, needle: &str) -> usize {
        let at = row
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} がありません: {row}"));
        row[..at].chars().count()
    }

    /// [`CheckState`](mewgit_core::github::CheckState) の腕を全部踏む集計。
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
    fn every_check_state_gets_its_own_glyph_and_none_is_folded_into_another() {
        // **`Skipped` と `Neutral` を成功か失敗に足すとどちらも嘘になる**（07）。
        // **1 腕ずつ回す** —— 5 つ同時に出す行は集計の上限で後ろが落ちるので、
        // 畳んだのか予算で落ちたのかが見分けられない。
        let mut seen = Vec::new();
        for (name, summary) in [
            (
                "success",
                CheckSummary {
                    success: 7,
                    ..CheckSummary::default()
                },
            ),
            (
                "failure",
                CheckSummary {
                    failure: 7,
                    ..CheckSummary::default()
                },
            ),
            (
                "pending",
                CheckSummary {
                    pending: 7,
                    ..CheckSummary::default()
                },
            ),
            (
                "skipped",
                CheckSummary {
                    skipped: 7,
                    ..CheckSummary::default()
                },
            ),
            (
                "neutral",
                CheckSummary {
                    neutral: 7,
                    ..CheckSummary::default()
                },
            ),
        ] {
            let detail = PrDetail {
                checks: summary,
                review: ReviewSummary::default(),
            };
            let state = with_details(open_and_draft(), &[(82, "headsha", Ok(detail))]);
            let row = pane_row(&state, SIZE, Pane::Prs, "#82");
            let glyph = row
                .split_whitespace()
                .find(|word| word.ends_with('7'))
                .unwrap_or_else(|| panic!("{name} の記号が出ていません: {row}"))
                .to_string();
            seen.push((name, glyph));
        }
        // **腕の数だけ違う記号が出る。** 畳むとここで重複が出る。
        let mut glyphs: Vec<&str> = seen.iter().map(|(_, g)| g.as_str()).collect();
        glyphs.sort_unstable();
        glyphs.dedup();
        assert_eq!(glyphs.len(), seen.len(), "記号が重なっている: {seen:?}");
    }

    #[test]
    fn a_pr_with_many_checks_does_not_eat_the_whole_title() {
        // **`pr_row` の doc が「末尾に置くと黙って消える」を理由に集計を前へ置いた
        // その守りが、桁を足したことで自分に返る。** 右列は狭い（`summary_budget` の
        // doc）ので、集計を伸ばし放題にすると**タイトルが 1 文字も出ない**
        // （実測: 幅 100 のこの行で、予算 17 ならタイトルに 7 セル、19 なら 4 セル）。
        let state = with_details(open_and_draft(), &[(82, "headsha", Ok(many_checks()))]);

        let wide = pane_row(&state, SIZE, Pane::Prs, "#82");
        assert!(wide.contains("Lanes"), "タイトルが消えた: {wide}");
        // **落としたことを言う。** 黙って消すと「それ以上は無い」と読める。
        assert!(wide.contains('+'), "落としたことを言っていない: {wide}");
        // **いちばん前は残す** —— 次に何をするかが変わるのはそちら。
        assert!(wide.contains("✓12"), "{wide}");
        // **チェック 5 種は丸ごと出す**（幅 100 で上限が 17 になる理由）。落ちるのは
        // 後ろのレビューから。
        assert!(wide.contains("·1"), "チェックの末尾が落ちた: {wide}");
        // **巻き添えにしない。** 幅は一覧の最大から決まるので、広い行が 1 つあると
        // 狭い行のタイトルまで押し出される。
        let narrow = pane_row(&state, SIZE, Pane::Prs, "#90");
        assert!(narrow.contains("Bump"), "巻き添えで消えた: {narrow}");
    }

    /// チェック 5 種とレビュー 2 種がどれも出る集計（`✓12 ✗3 ○2 ⊘5 ·1 A2 C1`）。
    fn many_checks() -> PrDetail {
        PrDetail {
            checks: CheckSummary {
                success: 12,
                failure: 3,
                pending: 2,
                skipped: 5,
                neutral: 1,
            },
            review: ReviewSummary {
                approved: 2,
                changes_requested: 1,
                commented: 0,
            },
        }
    }

    #[test]
    fn the_budget_at_the_narrowest_split_is_the_one_measured_by_hand() {
        // **幅 100 の右列（内側 38 セル）で番号 2 桁・状態 5 桁なら 17**（07 の
        // 「PRs ペイン」）。定数だったころの値と同じになるよう `TITLE_MIN` を決めた。
        let state = with_list(open_and_draft());
        let status = "DRAFT".len();
        assert_eq!(summary_budget(&state, "taro", 38, status), 17);
    }

    #[test]
    fn a_wide_right_column_does_not_fold_the_counts() {
        // **上限は枠の幅から導く**ので、右列が広ければ `+` で畳まない（定数だったころは
        // 幅 100 で測った 17 のままで、広い端末でもレビューが落ちていた）。
        let state = with_details(open_and_draft(), &[(82, "headsha", Ok(many_checks()))]);
        let wide = TermSize {
            width: 200,
            height: 24,
        };

        let row = pane_row(&state, wide, Pane::Prs, "#82");
        assert!(row.contains("A2 C1"), "{row}");
        assert!(!row.contains('+'), "{row}");
        assert!(row.contains("Lanes"), "{row}");
    }

    #[test]
    fn the_first_count_survives_a_budget_too_small_for_it() {
        // 全画面の狭い端末では上限が数セルに縮む。**いちばん前は越えても出す** ——
        // 成功か失敗のどちらかは読める（07）。
        assert_eq!(counts(&[(12, "✓"), (3, "✗")], 0), "✓12+");
        assert_eq!(counts(&[(0, "✓"), (3, "✗")], 0), "✗3");
    }

    #[test]
    fn a_row_tells_apart_not_asked_yet_from_could_not_be_read() {
        // **3 つの意味が 1 つの桁に載る**（`summary_cell`）。同じ見た目にすると、
        // CI の無い PR と CI を読めなかった PR が区別できない。
        let state = with_details(
            open_and_draft(),
            &[(82, "headsha", Err(GithubError::NotFound))],
        );

        let failed = pane_row(&state, SIZE, Pane::Prs, "#82");
        let waiting = pane_row(&state, SIZE, Pane::Prs, "#90");
        assert!(failed.contains('!'), "引けなかった行: {failed}");
        assert!(waiting.contains('…'), "まだ引いていない行: {waiting}");
    }

    #[test]
    fn the_ascii_setting_swaps_the_check_glyphs() {
        // Branches ペインの `↑` `↓` と同じ理由（同じ端末で一部だけ化けるのを避ける）。
        let mut state = with_details(open_and_draft(), &[(82, "headsha", Ok(every_state()))]);
        state.config.graph.ascii = true;

        let row = pane_row(&state, SIZE, Pane::Prs, "#82");
        assert!(row.contains("+2"), "{row}");
        assert!(row.contains("x1"), "{row}");
        assert!(row.contains("o1"), "{row}");
        assert!(!row.contains('✓'), "{row}");
        // まだ引いていない行の待ちの印も同じ設定に従う。
        let waiting = pane_row(&state, SIZE, Pane::Prs, "#90");
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

        let row = pane_row(&state, SIZE, Pane::Prs, "#82");
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

        let row = pane_row(&state, SIZE, Pane::Prs, "#82");
        // `OPEN ` は `DRAFT` に合わせた 1 桁の詰めぶんで、そのあとは区切りの 2 桁。
        // 集計の桁が残っていればここに割り込む。
        assert!(row.contains("#82 OPEN   Lanes"), "桁が残っている: {row}");
    }

    #[test]
    fn the_summary_column_lines_the_titles_up() {
        // 状態の桁と同じ理由。行ごとに中身の幅が違うので、揃えないとタイトルの
        // 開始位置がずれる。
        let state = with_details(open_and_draft(), &[(82, "headsha", Ok(every_state()))]);

        let wide = pane_row(&state, SIZE, Pane::Prs, "#82");
        let narrow = pane_row(&state, SIZE, Pane::Prs, "#90");
        assert_eq!(
            column_of(&wide, "Lanes"),
            column_of(&narrow, "Bump"),
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
    fn the_empty_list_offers_n_only_while_n_works() {
        // **`N` が効くのはこのペインにフォーカスがあるときだけ**（`prs_key`）。幅 100 以上では
        // `Esc` でフォーカスが Graph に移っても右上は PRs のまま見えるので、そこで `（N）` を
        // 出すと押しても何も起きない案内になる。
        let mut state = with_list(Vec::new());
        // **先にフォーカスがある形で、この fixture でも `N` が反応することを見る。** ブランチが
        // push されていないので箱は開かず、断りの通知が積まれる —— 下の「増えない」が
        // 自明に通らない証拠。
        let before = state.notices.len();
        let _ = update(&mut state, Msg::Key(Key::char('N')));
        assert_eq!(state.notices.len(), before + 1, "Prs で N が反応しなかった");

        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Esc)));
        assert_eq!(state.focus, Pane::Graph);
        let text = pane_text(&state, SIZE, Pane::Prs);
        assert!(text.contains("開いている PR はありません"), "{text}");
        assert!(!text.contains("（N）"), "効かない N を案内した: {text}");
        // 案内が嘘でないことを、同じ状態で `N` を押して確かめる。
        let before = state.notices.len();
        let _ = update(&mut state, Msg::Key(Key::char('N')));
        assert_eq!(state.notices.len(), before, "Graph で N が効いた");
        assert!(state.modal.is_none(), "{:?}", state.modal);
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
        // **フォーカスがあるので `（N）` を添える**（`placeholder` の doc）。右列が
        // いちばん狭い幅で、閉じ括弧まで入ることを見る。
        assert!(text.contains("開いている PR はありません（N）"), "{text}");

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
    fn the_keys_in_the_placeholder_follow_the_keymap() {
        // **`（N）` と `（A）` は表から引く**（`keys::live_key`。08 の「文言に書いたキーの字」）。
        // 引けない画面（衝突に負けた）では括弧ごと出さない。**既定の表だけで測ると直書きでも緑。**
        let mut empty = with_list(Vec::new());
        empty.keymap = keymap_with(&[("create_pr", "f9")]);
        let text = pane_text(&empty, SIZE, Pane::Prs);
        assert!(text.contains("開いている PR はありません（F9）"), "{text}");

        let mut degraded = on_prs_tab();
        degraded.account = AccountState::Degraded(DegradedReason::GhMissing);
        degraded.keymap = keymap_with(&[("account", "f8")]);
        let text = pane_text(&degraded, SIZE, Pane::Prs);
        assert!(text.contains("GitHub: ありません（F8）"), "{text}");
        // `reload` が名前の昇順で後ろなので勝ち、`account` は表から外れる。
        degraded.keymap = keymap_with(&[("account", "f8"), ("reload", "f8")]);
        let text = pane_text(&degraded, SIZE, Pane::Prs);
        assert!(text.contains("GitHub: ありません"), "{text}");
        assert!(!text.contains("ありません（"), "{text}");
    }

    #[test]
    fn a_repository_github_cannot_be_found_for_says_so_instead_of_loading_forever() {
        // **`TargetState::Unavailable` ではアカウントの解決を頼まない**ので、`Resolving` の
        // まま二度と動かない。「読み込み中…」を出すと永久に待たせる（`placeholder`）。
        let mut state = state_at("/repo");
        state.focus = Pane::Prs;
        state.reseat_focus();
        assert_eq!(state.focus, Pane::Prs, "巡回候補に入っていません");
        // **まだ見ていないあいだは待てば変わる**ので「読み込み中…」のまま。
        assert!(
            pane_text(&state, SIZE, Pane::Prs).contains(ui::LOADING),
            "既定リモートを見る前"
        );

        // リモートの無いリポジトリ。**状態を直に置かず、実物と同じ返信で落とす。**
        let _ = reply(
            &mut state,
            Cmd::LoadRepoInfo,
            Msg::RepoInfoLoaded(Box::new(Ok(fake_repo_snapshot()))),
        );
        assert_eq!(
            state.repo.target,
            TargetState::Unavailable,
            "前提が崩れている"
        );
        assert!(
            matches!(state.account, AccountState::Resolving),
            "前提が崩れている"
        );

        // **右列がいちばん狭い幅で、末尾まで入る**（`contains` は右端で切れた形を拾わない）。
        let text = pane_text(&state, SIZE, Pane::Prs);
        assert!(text.contains(TARGET_UNAVAILABLE), "{text}");
        assert!(!text.contains(ui::LOADING), "永久に待たせている: {text}");
    }

    /// 右上の見出しで `title` を描いた升目の修飾。**前後の空白 1 つずつを含める**
    /// （空白ごと反転する。07 の「右上のタブ」）。
    fn tab_modifiers(state: &AppState, title: &str) -> Vec<Modifier> {
        let area = ui::pane_area(Pane::Prs, SIZE, state);
        let terminal = terminal_with(state, SIZE);
        let buffer = terminal.backend().buffer();
        let header: String = (area.x..area.right())
            .map(|x| buffer[(x, area.y)].symbol())
            .collect();
        let x = area.x + col_of(&header, title);
        let width = u16::try_from(title.len()).unwrap();
        (x - 1..x + width + 1)
            .map(|x| buffer[(x, area.y)].modifier)
            .collect()
    }

    #[test]
    fn the_tab_on_display_is_reversed_and_the_other_is_dimmed() {
        // `DIM` だけだと、`DIM` を無視する端末で 2 枚が同じに見える（07）。
        let reversed = |m: &Modifier| m.contains(Modifier::REVERSED);
        let dimmed = |m: &Modifier| m.contains(Modifier::DIM) && !reversed(m);

        let on_prs = on_prs_tab();
        assert!(tab_modifiers(&on_prs, "PRs").iter().all(reversed));
        assert!(tab_modifiers(&on_prs, "Branches").iter().all(dimmed));

        // **強調は `focus` ではなく `right_tab` に付く。** フォーカスが Graph に
        // 移っても、右上に出しているのは PRs のまま（07）。
        let mut on_graph = on_prs_tab();
        on_graph.focus = Pane::Graph;
        on_graph.reseat_focus();
        assert!(tab_modifiers(&on_graph, "PRs").iter().all(reversed));

        let mut on_branches = on_prs_tab();
        on_branches.focus = Pane::Branches;
        on_branches.reseat_focus();
        assert!(tab_modifiers(&on_branches, "Branches").iter().all(reversed));
        assert!(tab_modifiers(&on_branches, "PRs").iter().all(dimmed));
    }

    #[test]
    fn with_github_off_the_tabs_left_are_still_told_apart() {
        // GitHub を切っても `Stash` が残るので、タブは 2 枚で選ぶ相手がある（07）。
        // **以前は 1 枚になって強調を外していた**（Stash のタブが入る前）。
        let mut off = state_at("/repo");
        off.account = AccountState::Disabled;
        off.reseat_focus();

        let reversed = |m: &Modifier| m.contains(Modifier::REVERSED);
        assert!(tab_modifiers(&off, "Branches").iter().all(reversed));
        assert!(tab_modifiers(&off, "Stash").iter().all(|m| !reversed(m)));
        // 切ってあれば `Prs` は巡回候補から落ちるので、押しても切り替わらない
        // 見出しを並べない（07 の「右上のタブ」）。
        let text = draw(&off, SIZE).join("\n");
        assert!(!text.contains("PRs"), "{text}");
    }
}
