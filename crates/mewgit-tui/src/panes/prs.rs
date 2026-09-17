//! PRs ペインの描画。設計は `docs/design/07-ui.md` の「PRs ペイン」。
//!
//! 右列の上、Branches と同じ領域をタブで差し替えて使う（07 の「右上のタブ」）。
//! **出すのは HEAD のブランチの PR 1 件**で、一覧タブは `ui::panes::prs`（2/2）。

use std::borrow::Cow;

use mewgit_core::app::{AppState, Fetch};
use mewgit_core::auth::AccountState;
use mewgit_core::github::{Check, CheckState, PrStatus, PullRequest};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::Rows;
use crate::ui::{self, theme};

/// タブ見出しに出すこのペインの名前。
///
/// **件数を添えない。** 出すのは 1 件だけなので、`(1)` にも `(0)` にも読む情報が無い。
pub(super) fn title() -> &'static str {
    "PRs"
}

/// 行と、カーソルが何行目か。
///
/// PR が出せないときは文言 1 行（カーソルはその行）。
pub(super) fn lines(state: &AppState) -> Rows {
    let Some(pr) = state.current_pr() else {
        return Rows {
            lines: vec![Line::raw(placeholder(state))],
            cursor: 0,
        };
    };

    // 見出し 2 行のあとにチェックを並べる。カーソルはチェックの側にしかない。
    let mut lines = vec![headline(pr, state), refs_line(pr)];
    // **カーソルをここで丸めない。** 範囲に収めるのは `PrState::move_cursor` の
    // 1 箇所（同じ判定を 2 箇所に書かない）。チェックが無いときの 0 は
    // 文言 1 行を指す。
    let cursor = lines.len() + state.prs.cursor;
    lines.extend(check_lines(state, state.config.graph.ascii));
    Rows { lines, cursor }
}

/// `#123 OPEN  Add adaptive layout`。
///
/// **状態をタイトルより前に置く。** 右列は端末幅の 40%（80 桁なら枠を除いて 30 桁）
/// しかなく、`ratatui` は行の右端で切るので、末尾に置くとタイトルが長いリポジトリで
/// **状態だけが黙って消える**（実測: 幅 100 の右列 38 桁で、`#82 test: 重複・空振りの
/// テストを削っ` まで出て `MERGED` が出なかった）。番号と状態は切れずに残す。
fn headline(pr: &PullRequest, state: &AppState) -> Line<'static> {
    Line::from(vec![
        Span::raw(format!(" #{} ", pr.number)),
        Span::styled(status_label(pr).to_string(), status_style(pr, state)),
        Span::raw(format!("  {}", pr.title)),
    ])
}

/// `feat/x → main`。
fn refs_line(pr: &PullRequest) -> Line<'static> {
    Line::styled(
        format!(" {} -> {}", pr.head.branch, pr.base.branch),
        theme::muted(),
    )
}

/// PR の状態。**`draft` を状態と並べて出す** —— どちらも「いま何であるか」で、
/// 2 列に分けるとタイトルの桁を食う。
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

/// チェックの行。出せないときは文言 1 行。
///
/// **空の `Vec` と「まだ引いていない」を出し分ける**（`Fetch` の 3 状態。01）。
fn check_lines(state: &AppState, ascii: bool) -> Vec<Line<'static>> {
    match &state.prs.checks {
        Fetch::Loaded(checks) if checks.is_empty() => {
            vec![Line::styled(" チェックはありません", theme::muted())]
        }
        Fetch::Loaded(checks) => checks
            .iter()
            .enumerate()
            .map(|(index, check)| {
                let line = check_row(check, ascii);
                if index == state.prs.cursor {
                    line.style(theme::selected())
                } else {
                    line
                }
            })
            .collect(),
        Fetch::Unasked => vec![Line::styled(ui::LOADING, theme::muted())],
        Fetch::Failed(err) => vec![Line::styled(
            format!(" チェックを読み込めませんでした: {err}"),
            theme::warn(),
        )],
    }
}

/// `✓ build`。
fn check_row(check: &Check, ascii: bool) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!(" {} ", check_glyph(check.state, ascii)),
            check_style(check.state),
        ),
        Span::raw(check.name.clone()),
    ])
}

/// チェックの記号。**`config.graph.ascii` に従う**（Branches ペインの `↑` `↓` と
/// 同じ理由。同じ端末で一部の記号だけが化けるのを避ける。07）。
///
/// **5 値を 3 つに畳まない。** 畳むと `Skipped` と `Neutral` が成功か失敗のどちらかに
/// 見え、**どちらも嘘**（05）。
fn check_glyph(state: CheckState, ascii: bool) -> &'static str {
    let glyphs = match state {
        CheckState::Success => ("✓", "+"),
        CheckState::Failure => ("✗", "x"),
        CheckState::Pending => ("●", "*"),
        CheckState::Skipped => ("⊘", "-"),
        CheckState::Neutral => ("·", "."),
    };
    ui::pick_charset(glyphs, ascii)
}

fn check_style(state: CheckState) -> Style {
    match state {
        CheckState::Success => theme::added(),
        CheckState::Failure => theme::removed(),
        CheckState::Pending => theme::warn(),
        CheckState::Skipped | CheckState::Neutral => theme::muted(),
    }
}

/// PR が出せない理由。
///
/// **消去法で導かない。** `AccountState` と [`Fetch`] の腕から出す（01 の
/// 「失敗の記憶」と同じ理由）。**`ui::LOADING` を使う** —— 文言を自分で組むと、
/// 既にある「読み込み中…」の見張りがこの産出元を拾えない。
///
/// **`N`（PR 作成）を案内しない。** `Modal::PrCreate` は次のタスクなので、いま書くと
/// 押せないキーを案内することになる（01 の「`o`（ブラウザで開く）」）。
fn placeholder(state: &AppState) -> Cow<'static, str> {
    match &state.account {
        AccountState::Resolving => ui::LOADING.into(),
        AccountState::Degraded(reason) => format!("GitHub: {}（A）", ui::degraded(reason)).into(),
        // **巡回候補にもタブにも出ないのでここへは来ない**（01 の「右上のタブ」）。
        // それでも文言を置くのは、網羅 `match` の腕を `unreachable!()` にすると
        // 描画の途中で落ちるため。
        AccountState::Disabled => "GitHub 機能は切ってあります".into(),
        // **鍵より先に見る。** detached / unborn では `wanted_pr()` が `None` を返し、
        // `fresh` も `None` になるので、下へ落とすと「読み込み中…」に化ける。
        AccountState::Resolved(_) if state.wanted_pr().is_none() => {
            "ブランチに居ません（detached / コミットがありません）".into()
        }
        // 鍵の突き合わせは [`PrState::fresh`] の 1 つ。まだ頼んでいない（`None` /
        // `Unasked`）と、引けたが PR が無い、引けなかったを出し分ける。
        AccountState::Resolved(_) => match state.prs.fresh(state.wanted_pr()) {
            None | Some(Fetch::Unasked) => ui::LOADING.into(),
            // `Loaded(Some(_))` はここへ来ない（`lines` が先に返す）。
            Some(Fetch::Loaded(_)) => "このブランチの PR はありません".into(),
            Some(Fetch::Failed(err)) => format!("PR を読み込めませんでした: {err}").into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{draw, pane_text, state_at};
    use mewgit_core::app::{Cmd, Msg, Pane, TermSize};
    use mewgit_core::auth::{AccountState, DegradedReason};
    use mewgit_core::git_read::HeadInfo;
    use mewgit_core::github::GithubError;
    use mewgit_core::test_util::{
        checks_loaded, fake_check, fake_oid, fake_pull_request, pr_loaded, put_on_branch, reply,
    };

    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    /// PRs タブを開いた状態。アカウントと対象は決まっている。
    fn on_prs_tab() -> AppState {
        let mut state = state_at("/repo");
        put_on_branch(&mut state, "feat/x");
        state.focus = Pane::Prs;
        // **タブは `reseat_focus` が揃える**（01 の「右上のタブ」）。直に置くと
        // 実物では出ない画面を見張ることになる。
        state.reseat_focus();
        state
    }

    /// PR とチェックが届いた状態。
    fn with_pr(checks: Vec<Check>) -> AppState {
        let mut state = on_prs_tab();
        let next = reply(
            &mut state,
            Cmd::FetchPr {
                branch: "feat/x".to_string(),
            },
            pr_loaded("feat/x", Some(fake_pull_request(82, "feat/x", "headsha"))),
        );
        let _ = reply(&mut state, next, checks_loaded("headsha", checks));
        state
    }

    #[test]
    fn the_pane_shows_the_number_the_state_the_refs_and_the_checks() {
        let state = with_pr(vec![
            fake_check("build", CheckState::Success),
            fake_check("test", CheckState::Failure),
        ]);

        // **このペインの中だけを見る**（`ui::LOADING` などの文言は他のペインも出す）。
        let text = pane_text(&state, SIZE, Pane::Prs);

        assert!(text.contains("#82"), "{text}");
        assert!(text.contains("OPEN"), "{text}");
        assert!(text.contains("feat/x -> main"), "{text}");
        assert!(text.contains("✓ build"), "{text}");
        assert!(text.contains("✗ test"), "{text}");
    }

    #[test]
    fn the_state_survives_a_narrow_pane_but_the_title_is_cut() {
        // **状態をタイトルより後ろに置くと、右列の桁では状態だけが黙って消える**
        // （07 の「PRs ペイン」。実測で踏んだ形）。`ratatui` は行の右端で切るので、
        // **既定の 80x24 のまま**見る（広げて緑にすると見張りが効かない。#22）。
        let mut state = with_pr(Vec::new());
        if let Fetch::Loaded(Some(pr)) = &mut state.prs.current {
            pr.title = "と".repeat(60);
        }

        let text = pane_text(&state, SIZE, Pane::Prs);

        assert!(text.contains("#82 OPEN"), "{text}");
        // **切れていることも見る。** 切れない幅なら並び順を入れ替えても通るので、
        // この見張りが何を踏んでいるか判らなくなる。
        assert!(!text.contains(&"と".repeat(60)), "{text}");
    }

    #[test]
    fn an_empty_check_list_is_not_the_same_as_an_unfetched_one() {
        // `Vec` が空なのは「チェックが 0 件」で、「まだ引いていない」ではない（01）。
        let state = with_pr(Vec::new());
        let text = pane_text(&state, SIZE, Pane::Prs);
        assert!(text.contains("チェックはありません"), "{text}");
        assert!(!text.contains("読み込み中"), "{text}");

        // 引く前は「読み込み中…」。
        let mut state = on_prs_tab();
        let _ = reply(
            &mut state,
            Cmd::FetchPr {
                branch: "feat/x".to_string(),
            },
            pr_loaded("feat/x", Some(fake_pull_request(82, "feat/x", "headsha"))),
        );
        let text = pane_text(&state, SIZE, Pane::Prs);
        assert!(text.contains("読み込み中"), "{text}");
        assert!(!text.contains("チェックはありません"), "{text}");
    }

    #[test]
    fn every_reason_the_pane_cannot_show_a_pr_is_told_apart() {
        // **消去法で導かない。** 腕ごとに違う文言が出ることをここで押さえる（07）。
        let loading = on_prs_tab();
        assert!(
            pane_text(&loading, SIZE, Pane::Prs).contains("読み込み中"),
            "頼んだ直後"
        );

        let mut none = on_prs_tab();
        let _ = reply(
            &mut none,
            Cmd::FetchPr {
                branch: "feat/x".to_string(),
            },
            pr_loaded("feat/x", None),
        );
        let text = pane_text(&none, SIZE, Pane::Prs);
        assert!(text.contains("PR はありません"), "{text}");
        // **`N`（PR 作成）を案内しない。** まだ無いキーなので（01）。**キーの案内の
        // 形で見る** —— 素の `N` を見ると、いつかタイトルや login に `N` が入った
        // だけで落ちる。
        assert!(!text.contains("N で"), "{text}");
        assert!(!text.contains("[N]"), "{text}");

        let mut failed = on_prs_tab();
        let _ = reply(
            &mut failed,
            Cmd::FetchPr {
                branch: "feat/x".to_string(),
            },
            Msg::PrLoaded {
                branch: "feat/x".to_string(),
                result: Box::new(Err(GithubError::NotFound)),
            },
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

        let mut detached = on_prs_tab();
        detached.repo.head = Some(HeadInfo::Detached(fake_oid(1)));
        assert!(
            pane_text(&detached, SIZE, Pane::Prs).contains("ブランチに居ません"),
            "detached"
        );
    }

    #[test]
    fn the_glyphs_fall_back_to_ascii() {
        // 同じ端末で一部の記号だけが化けないよう、`config.graph.ascii` に従う（07）。
        let mut state = with_pr(vec![
            fake_check("build", CheckState::Success),
            fake_check("test", CheckState::Failure),
        ]);
        state.config.graph.ascii = true;

        let text = pane_text(&state, SIZE, Pane::Prs);

        assert!(text.contains("+ build"), "{text}");
        assert!(text.contains("x test"), "{text}");
        assert!(!text.contains('✓'), "{text}");
    }

    #[test]
    fn the_tab_header_names_both_tabs_and_only_one_when_github_is_off() {
        let state = with_pr(Vec::new());
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
    fn a_long_check_list_leaves_the_pane_below_visible() {
        // 上限が無いと、チェックの多い PR で下のペインが潰れる（07）。
        // **既定の 80x24 のまま見る。**
        let checks = (0..40)
            .map(|n| fake_check(&format!("job-{n:02}"), CheckState::Success))
            .collect();
        let state = with_pr(checks);

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("Working tree"), "{text}");
    }
}
