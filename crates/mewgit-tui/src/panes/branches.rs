//! Branches ペインの描画。設計は `docs/design/07-ui.md` の「Branches ペイン」。
//!
//! 右列の上に置く。下の Working tree / コミット詳細と違い、**左の選択では
//! 入れ替わらない**（ブランチ一覧はグラフで何を選んでいるかと無関係）。

use std::borrow::Cow;

use mewgit_core::app::{AppState, ListLoad};
use mewgit_core::git_read::Branch;
use ratatui::text::{Line, Span};

use super::Rows;
use crate::ui::{self, theme};

/// タブ見出しに出すこのペインの名前。件数は出せるときだけ添える（[`super::counted`]）。
pub(super) fn title(state: &AppState) -> Cow<'static, str> {
    super::counted("Branches", state.branches.branches().len())
}

/// 一覧の行と、カーソルが何行目か。
///
/// 一覧が出せないときは文言 1 行（カーソルはその行）。**行を組むのはここ 1 本**で、
/// 割く高さもこの行数から出す（`panes::commit::files_rows` と同じ）。枠と高さの
/// 規則は `Prs` と共有する（[`super::TopPane`]）。
pub(super) fn lines(state: &AppState) -> Rows {
    let branches = state.branches.branches();
    if branches.is_empty() {
        return Rows {
            lines: vec![Line::raw(placeholder(state))],
            // 文言 1 行は選択肢ではない（[`Rows::cursor`]）。
            cursor: None,
        };
    }

    let ascii = state.config.graph.ascii;
    // **カーソル行の反転はここで当てない。** [`super::TopPane::render`] が `cursor` を
    // 見て 1 箇所で当てる（`Prs` と共有）。
    let lines = branches.iter().map(|branch| row(branch, ascii)).collect();
    Rows {
        lines,
        cursor: Some(state.branches.cursor),
    }
}

/// 1 行。`* main  ↑1`。印が付くのは HEAD のブランチだけ。
fn row(branch: &Branch, ascii: bool) -> Line<'static> {
    let mark = if branch.is_head { '*' } else { ' ' };
    Line::from(vec![
        Span::raw(format!(" {mark} {}", branch.name)),
        // 空なら何も描かれない（upstream が無いか、追いついている）。
        Span::styled(tracking(branch, ascii), theme::muted()),
    ])
}

/// upstream に対する ahead / behind。**0 のほうは出さない**（07）。
///
/// 追いついているブランチで `↑0 ↓0` を出しても桁が埋まるだけで、upstream の無い
/// ブランチと同じ見た目になることは承知のうえ（区別するのは push の `-u` 提案。M3）。
///
/// 記号は `config.graph.ascii` に従う。キーは `[graph]` の下にあるが、既定は locale から
/// 決まる（08）ので意味は「この端末が非 ASCII の記号を出せるか」。
fn tracking(branch: &Branch, ascii: bool) -> String {
    let up = ui::pick_charset(AHEAD, ascii);
    let down = ui::pick_charset(BEHIND, ascii);
    let mut out = String::new();
    if branch.ahead > 0 {
        out.push_str(&format!("  {up}{}", branch.ahead));
    }
    if branch.behind > 0 {
        out.push_str(&format!("  {down}{}", branch.behind));
    }
    out
}

/// upstream より進んでいる／遅れている印。`(UTF-8, ASCII)`。
const AHEAD: (&str, &str) = ("↑", "^");
const BEHIND: (&str, &str) = ("↓", "v");

/// 一覧が空のときの文言。
///
/// **3 つを出し分ける。** コミットが 1 つも無いリポジトリにはブランチが 1 本も無く、
/// それは正常な状態（`git init` の直後）なので「読み込み中…」と同じ見た目にしない。
/// 失敗は状態から見る（消去法で導かない。01 の「失敗の記憶」）。
fn placeholder(state: &AppState) -> &'static str {
    match state.branches.load() {
        ListLoad::Failed => "ブランチを読み込めませんでした",
        // 届いていて空。`git init` の直後。
        ListLoad::Loaded => "ブランチはありません",
        ListLoad::Pending => ui::LOADING,
    }
}

#[cfg(test)]
mod tests {
    use crate::test_util::{draw, pane_text, row_is_reversed, row_of, state_at};
    use mewgit_core::app::{update, AppState, Key, Msg, Pane, TermSize};
    use mewgit_core::git_read::{Branch, GitReadError};
    use mewgit_core::test_util::{deliver_branches, fake_branch, fake_oid, fake_upstream};

    /// 一覧が届いた状態。**`update` を通す**ので `JobTracker` も本番と同じ。
    fn state_with_branches(branches: Vec<Branch>) -> AppState {
        let mut state = state_at("/repo");
        let _ = deliver_branches(&mut state, Ok(branches));
        state
    }

    /// 右列がいちばん狭くなる端末（07 の「狭い端末」）。
    const SIZE: TermSize = crate::test_util::SPLIT_NARROWEST;

    #[test]
    fn the_list_marks_the_head_branch_and_counts_the_rest() {
        let state = state_with_branches(vec![
            fake_branch("another", false),
            fake_branch("main", true),
        ]);

        let lines = draw(&state, SIZE);
        let text = lines.join("\n");

        assert!(text.contains("Branches (2)"), "{text}");
        assert!(text.contains("* main"), "{text}");
        // 印が付くのは HEAD だけ。
        let another = &lines[row_of(&lines, "another") as usize];
        assert!(!another.contains('*'), "{another}");
    }

    #[test]
    fn tracking_shows_only_the_side_that_is_not_zero() {
        // 追いついているブランチで `↑0 ↓0` を出しても桁が埋まるだけ（07）。
        let mut ahead = fake_branch("ahead", false);
        ahead.upstream = Some(fake_upstream("origin/ahead", fake_oid(1)));
        ahead.ahead = 2;
        let mut behind = fake_branch("behind", false);
        behind.upstream = Some(fake_upstream("origin/behind", fake_oid(1)));
        behind.behind = 3;
        let mut synced = fake_branch("synced", true);
        synced.upstream = Some(fake_upstream("origin/synced", fake_oid(1)));
        let state = state_with_branches(vec![ahead, behind, synced]);

        let lines = draw(&state, SIZE);

        assert!(lines[row_of(&lines, "ahead") as usize].contains("↑2"));
        assert!(!lines[row_of(&lines, "ahead") as usize].contains("↓"));
        assert!(lines[row_of(&lines, "behind") as usize].contains("↓3"));
        let synced = &lines[row_of(&lines, "synced") as usize];
        assert!(!synced.contains('↑') && !synced.contains('↓'), "{synced}");
    }

    #[test]
    fn the_ascii_setting_swaps_the_arrows() {
        // 罫線だけに効かせると、同じ端末で矢印だけが化ける（07）。
        let mut branch = fake_branch("topic", false);
        branch.upstream = Some(fake_upstream("origin/topic", fake_oid(1)));
        branch.ahead = 1;
        branch.behind = 2;
        let mut state = state_with_branches(vec![branch]);
        state.config.graph.ascii = true;

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("^1"), "{text}");
        assert!(text.contains("v2"), "{text}");
        assert!(!text.contains('↑'), "{text}");
    }

    #[test]
    fn the_selected_branch_is_highlighted() {
        // **一覧の先頭行は見ない。** `row_is_reversed` は行の全幅を見るので、
        // 右列の 1 行目はグラフの WIP 行（カーソル行なので反転している）と同じ
        // 端末行に来る。そこを見ると、反転していなくても真になる。
        let mut state = state_with_branches(vec![
            fake_branch("alpha", false),
            fake_branch("bravo", true),
            fake_branch("charlie", false),
        ]);
        state.focus = Pane::Branches;
        // カーソルは HEAD の行に座る（01 の「ブランチ一覧」）。
        assert!(row_is_reversed(&state, SIZE, "bravo"));
        assert!(!row_is_reversed(&state, SIZE, "charlie"));

        let _ = update(&mut state, Msg::Key(Key::char('j')));

        assert!(row_is_reversed(&state, SIZE, "charlie"));
        assert!(!row_is_reversed(&state, SIZE, "bravo"));
    }

    #[test]
    fn a_repository_without_branches_says_so_instead_of_loading() {
        // `git init` の直後はブランチが 1 本も無い。正常な状態なので「読み込み中…」と
        // 同じ見た目にしない（07）。
        let state = state_with_branches(Vec::new());

        // **このペインの中だけを見る**（`ui::LOADING` は他のペインも出す）。
        let text = pane_text(&state, SIZE, Pane::Branches);

        assert!(text.contains("ブランチはありません"), "{text}");
        assert!(!text.contains("読み込み中"), "{text}");
    }

    #[test]
    fn a_pending_list_reads_as_loading() {
        let state = state_at("/repo");

        let text = pane_text(&state, SIZE, Pane::Branches);

        assert!(text.contains("読み込み中"), "{text}");
        // 件数を出すと、ブランチが 1 本も無いリポジトリと同じ見た目になる。
        assert!(!text.contains("Branches ("), "{text}");
    }

    #[test]
    fn a_list_that_failed_says_so_instead_of_loading() {
        let mut state = state_at("/repo");
        let _ = deliver_branches(&mut state, Err(GitReadError::ReaderGone));

        let text = pane_text(&state, SIZE, Pane::Branches);

        assert!(text.contains("ブランチを読み込めませんでした"), "{text}");
        assert!(!text.contains("読み込み中"), "{text}");
    }

    #[test]
    fn a_long_list_leaves_the_pane_below_visible() {
        // 上限が無いと、ブランチの多いリポジトリで下のペインが潰れる（07）。
        // **既定の 80x24 のまま見る。** 広げて緑にすると見張りが効かない（#22）。
        let branches = (0..40)
            .map(|n| fake_branch(&format!("feat/branch-{n:02}"), n == 0))
            .collect();
        let state = state_with_branches(branches);

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("Branches (40)"), "{text}");
        // **下のペインは枠だけでなく中身まで見えていること。** 行数を数えると、
        // レイアウトを知る場所がテストにも増える（07 の「描画に使える行数は ui が出す」）。
        let below = pane_text(&state, SIZE, Pane::Worktree);
        assert!(below.contains("Working tree"), "{below}");
        assert!(below.contains("変更はありません"), "{below}");
    }

    #[test]
    fn a_tiny_terminal_does_not_panic() {
        // リサイズの途中では枠より小さい端末が来る。
        for (width, height) in [(1, 1), (10, 3), (20, 5)] {
            let state = state_with_branches(vec![fake_branch("main", true)]);
            let lines = draw(&state, TermSize { width, height });
            assert_eq!(lines.len(), height as usize);
        }
    }
}
