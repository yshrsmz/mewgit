//! Branches ペインの描画。設計は `docs/design/07-ui.md` の「Branches ペイン」。
//!
//! 右列の上に置く。下の Working tree / コミット詳細と違い、**左の選択では
//! 入れ替わらない**（ブランチ一覧はグラフで何を選んでいるかと無関係）。

use mewgit_core::app::{AppState, BranchLoad, Pane};
use mewgit_core::git_read::Branch;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Block;
use ratatui::Frame;

use crate::ui::{self, theme};

/// 右列のうち Branches ペインに割く行数。枠の上下を含む。
///
/// **上限は右列の半分**（07）。無いとブランチの多いリポジトリで下のペインが潰れる。
///
/// **行数は組んだ行から数える**ので、組んだものを受け取る。別の式で数えると、文言や
/// 見出しの入れ方を変えたときに片方だけ直しても通り、枠からはみ出すか余白が残る
/// （#22 で実際に起きた形）。**組むのは 1 フレームに 1 回**で、`ui::render` が組んだ
/// ものを高さの決定と描画の両方に回す（`panes::commit` が 1 回で済ませているのと
/// 同じ形。あちらは高さを決めるのも描くのも `render` の中なので受け渡しが要らない）。
pub fn rows(view: &View, state: &AppState, available: u16) -> u16 {
    let border_rows = usize::from(border_rows(state));
    ui::capped_rows(view.lines.len() + border_rows, available)
}

/// 組んだ一覧。高さを決めるのと描くのに使う。
pub struct View {
    lines: Vec<Line<'static>>,
    /// カーソルが何行目か。窓の切り出しに渡す。
    cursor: usize,
}

/// 枠が食う行数。
///
/// **`+ 2` と手で書かない**（`modals::place` と同じ理由）。あとで `Padding` を足すと、
/// 割く高さだけが古いまま残り、最後の 1 行が黙って消える。高さの判る当て木を
/// [`Block::inner`] に通して差を取る。
fn border_rows(state: &AppState) -> u16 {
    let probe = Rect::new(0, 0, 0, u16::MAX);
    probe.height - block(state).inner(probe).height
}

/// このペインの枠。行数を測るときと描くときで同じものを組む。
fn block(state: &AppState) -> Block<'static> {
    ui::pane_block(title(state), state.focus == Pane::Branches)
}

/// Branches ペインを描く。領域の高さは [`rows`] が決める。
pub fn render(frame: &mut Frame, area: Rect, state: &AppState, view: View) {
    let block = block(state);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    ui::render_rows(frame, inner, view.lines, view.cursor..view.cursor + 1);
}

/// 枠のタイトル。件数は出せるときだけ添える。
///
/// 読み込み中や失敗のときに `(0)` を出すと、ブランチが 1 本も無いリポジトリと
/// 同じ見た目になる（本文で出し分けているのと同じ理由）。
fn title(state: &AppState) -> String {
    match state.branches.branches.len() {
        0 => "Branches".to_string(),
        len => format!("Branches ({len})"),
    }
}

/// 一覧の行と、カーソルが何行目か。
///
/// 一覧が出せないときは文言 1 行（カーソルはその行）。**行を組むのはここ 1 本**で、
/// 割く高さもこの行数から出す（`panes::commit::files_lines` と同じ）。
pub fn lines(state: &AppState) -> View {
    let branches = &state.branches.branches;
    if branches.is_empty() {
        return View {
            lines: vec![Line::raw(placeholder(state))],
            cursor: 0,
        };
    }

    let ascii = state.config.graph.ascii;
    let lines = branches
        .iter()
        .enumerate()
        .map(|(index, branch)| {
            let line = row(branch, ascii);
            if index == state.branches.cursor {
                line.style(theme::selected())
            } else {
                line
            }
        })
        .collect();
    View {
        lines,
        cursor: state.branches.cursor,
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
        BranchLoad::Failed => "ブランチを読み込めませんでした",
        // 届いていて空。`git init` の直後。
        BranchLoad::Loaded => "ブランチはありません",
        BranchLoad::Pending => ui::LOADING,
    }
}

#[cfg(test)]
mod tests {
    use crate::test_util::{draw, pane_text, row_is_reversed, row_of, state_at};
    use mewgit_core::app::{update, AppState, Key, Msg, Pane, TermSize};
    use mewgit_core::git_read::{Branch, GitReadError};
    use mewgit_core::test_util::fake_branch;

    /// 一覧が届いた状態。**`update` を通す**ので `JobTracker` も本番と同じ。
    fn state_with_branches(branches: Vec<Branch>) -> AppState {
        let mut state = state_at("/repo");
        let _ = update(&mut state, Msg::BranchesLoaded(Box::new(Ok(branches))));
        state
    }

    /// 既定の 80x24。右列は 32 桁（07）。
    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

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
        ahead.upstream = Some("origin/ahead".into());
        ahead.ahead = 2;
        let mut behind = fake_branch("behind", false);
        behind.upstream = Some("origin/behind".into());
        behind.behind = 3;
        let mut synced = fake_branch("synced", true);
        synced.upstream = Some("origin/synced".into());
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
        branch.upstream = Some("origin/topic".into());
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
        let _ = update(
            &mut state,
            Msg::BranchesLoaded(Box::new(Err(GitReadError::ReaderGone))),
        );

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
