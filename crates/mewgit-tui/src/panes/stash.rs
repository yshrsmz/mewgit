//! Stash ペインの描画。設計は `docs/design/07-ui.md` の「Stash ペイン」。
//!
//! 右列の上のタブの 1 枚。Branches と同じく、**左の選択では入れ替わらない**。

use mewgit_core::app::{AppState, ListLoad};
use mewgit_core::git_read::StashEntry;
use ratatui::text::{Line, Span};

use super::Rows;
use crate::ui::{self, theme};

/// タブ見出しに出すこのペインの名前。
///
/// **件数を添えない**（Branches とは違う）。見出しは 3 枚ぶんが右列の枠の上辺に並び、
/// 決めたときの 80x24 の右列は枠の内側が 30 セルだった。`Branches (12)` と `Stash (3)` と
/// `PRs` を前後の空白と区切りごと並べると 33 セルで、`PRs` が切れる（件数なしなら 29 セル）。
/// 件数は一覧の `stash@{n}` から読める。**80 桁で右列を割らなくなってこの前提は崩れた**
/// ので、添えるかは M6 の狭い幅での畳み方で決め直す（07 の「右上のタブ」）。
pub(super) fn title() -> &'static str {
    "Stash"
}

/// 一覧の行と、カーソルが何行目か。一覧が出せないときは文言 1 行（Branches と同じ）。
pub(super) fn lines(state: &AppState) -> Rows {
    let entries = &state.stashes.entries;
    if entries.is_empty() {
        return Rows {
            lines: vec![Line::raw(placeholder(state))],
            // 文言 1 行は選択肢ではない（[`Rows::cursor`]）。
            cursor: None,
        };
    }

    // **カーソル行の反転はここで当てない**（[`super::TopPane::render`] が 1 箇所で当てる）。
    Rows {
        lines: entries.iter().map(row).collect(),
        cursor: Some(state.stashes.cursor),
    }
}

/// 1 行。`stash@{0}  On main: wip`。
///
/// **`stash@{n}` を先に置く。** 右列は狭く（07 の「狭い端末」）、メッセージは
/// 右で切れる。番号が残れば、端末で `git stash show stash@{n}` を打つときの手掛かりに
/// なる。
fn row(stash: &StashEntry) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {}", stash.name()), theme::muted()),
        Span::raw(format!("  {}", stash.message)),
    ])
}

/// 一覧が空のときの文言。**3 つを出し分ける**（Branches と同じ理由）。stash が 1 つも
/// 無いのは正常な状態で、「読み込み中…」と同じ見た目にしない。
fn placeholder(state: &AppState) -> &'static str {
    match state.stashes.load() {
        ListLoad::Failed => "stash を読み込めませんでした",
        ListLoad::Loaded => "stash はありません",
        ListLoad::Pending => ui::LOADING,
    }
}

#[cfg(test)]
mod tests {
    use crate::test_util::{
        draw, pane_text, row_is_reversed, row_of, state_at, tab_to, with_stashes,
    };
    use mewgit_core::app::{update, AppState, Cmd, Key, Msg, Pane, TermSize};
    use mewgit_core::git_read::{GitReadError, StashEntry};
    use mewgit_core::test_util::{fake_branch, fake_stash, reply};

    /// 一覧が届き、Stash のタブを出した状態。
    fn on_stash_tab(entries: Vec<StashEntry>) -> AppState {
        let mut state = state_at("/repo");
        with_stashes(&mut state, Ok(entries));
        tab_to(&mut state, Pane::Stash);
        state
    }

    /// 右列がいちばん狭くなる端末（07 の「狭い端末」）。
    const SIZE: TermSize = crate::test_util::SPLIT_NARROWEST;

    #[test]
    fn the_list_shows_each_stash_by_its_name_and_message() {
        let state = on_stash_tab(vec![fake_stash(0), fake_stash(1)]);

        let text = pane_text(&state, SIZE, Pane::Stash);

        assert!(text.contains("stash@{0}  stash 0"), "{text}");
        assert!(text.contains("stash@{1}  stash 1"), "{text}");
    }

    #[test]
    fn the_selected_stash_is_highlighted() {
        // **一覧の先頭行は見ない**（Branches と同じ。右列の 1 行目はグラフの WIP 行と
        // 同じ端末行に来る）。
        let mut state = on_stash_tab(vec![fake_stash(0), fake_stash(1), fake_stash(2)]);
        let _ = update(&mut state, Msg::Key(Key::char('j')));
        assert!(row_is_reversed(&state, SIZE, "stash@{1}"));
        assert!(!row_is_reversed(&state, SIZE, "stash@{2}"));

        let _ = update(&mut state, Msg::Key(Key::char('j')));

        assert!(row_is_reversed(&state, SIZE, "stash@{2}"));
        assert!(!row_is_reversed(&state, SIZE, "stash@{1}"));
    }

    #[test]
    fn a_repository_without_stashes_says_so_instead_of_loading() {
        let state = on_stash_tab(Vec::new());

        let text = pane_text(&state, SIZE, Pane::Stash);

        assert!(text.contains("stash はありません"), "{text}");
        assert!(!text.contains("読み込み中"), "{text}");
    }

    #[test]
    fn a_pending_list_reads_as_loading() {
        let mut state = state_at("/repo");
        let _ = state.issue(Cmd::LoadStashes);
        tab_to(&mut state, Pane::Stash);

        let text = pane_text(&state, SIZE, Pane::Stash);

        assert!(text.contains("読み込み中"), "{text}");
    }

    #[test]
    fn a_list_that_failed_says_so_instead_of_loading() {
        let mut state = state_at("/repo");
        with_stashes(&mut state, Err(GitReadError::ReaderGone));
        tab_to(&mut state, Pane::Stash);

        let text = pane_text(&state, SIZE, Pane::Stash);

        assert!(text.contains("stash を読み込めませんでした"), "{text}");
    }

    #[test]
    fn every_tab_title_fits_in_the_narrowest_right_column() {
        // **右列がいちばん狭い幅で、3 枚の見出しが入るかを見る。** 件数を添えない
        // 判断の境目だった幅はもう出ない（`title` の doc）ので、ブランチの件数を
        // 2 桁にしたまま、見出しが入ることだけを見る。
        let mut state = state_at("/repo");
        with_stashes(&mut state, Ok((0..3).map(fake_stash).collect()));
        let branches = (0..12)
            .map(|n| fake_branch(&format!("b{n:02}"), n == 0))
            .collect();
        let _ = reply(
            &mut state,
            Cmd::LoadBranches,
            Msg::BranchesLoaded(Box::new(Ok(branches))),
        );

        let lines = draw(&state, SIZE);
        let header = &lines[row_of(&lines, "Branches (12)") as usize];

        assert!(header.contains("Stash"), "{header}");
        // **末尾の見出しが空白ごと残っていること。** 切れると ` PR` や ` PRs` で止まる。
        assert!(header.contains(" PRs "), "{header}");
    }
}
