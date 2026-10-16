//! ワーキングツリーペインの描画。設計は `docs/design/07-ui.md` の「Worktree ペイン」。
//!
//! 1 行は `<種別 1 文字> <パス>`。区画の見出しと、その中のファイルを並べる。

use mewgit_core::app::{AppState, JobKind, Pane, WorktreeState};
use mewgit_core::git_read::{FileEntry, WorktreeSection};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::ui::{self, theme};

/// ワーキングツリーペインを描く。
pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let block = ui::pane_block(
        title(state),
        state.focus == Pane::Worktree,
        state.config.graph.ascii,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.worktree.is_empty() {
        frame.render_widget(Paragraph::new(empty_message(state)), inner);
        return;
    }

    let (rows, cursor) = rows(state);
    // 見せたいのはカーソルの 1 行だけ。Diff ペインはハンク全体を渡す。
    ui::render_rows(frame, inner, rows.len(), cursor..cursor + 1, |at| {
        line(
            rows[at],
            at == cursor,
            inner.width,
            state.config.graph.ascii,
        )
    });
}

/// 描く行の 1 つ。**借用だけで `String` を持たない** —— 全行ぶん並べても安く、
/// 文字列を組むのは窓に入った行だけになる（`ui::render_rows`）。
#[derive(Clone, Copy)]
enum Row<'a> {
    Heading(WorktreeSection),
    File(&'a FileEntry),
}

/// 枠のタイトル。件数は全区画の合計。
fn title(state: &AppState) -> String {
    format!("Working tree ({})", state.worktree.len())
}

/// 行が 1 つも無いときの文言。読み込み中と「変更なし」を出し分ける
/// （グラフの空表示と同じ形）。
fn empty_message(state: &AppState) -> &'static str {
    if state.jobs.is_running(JobKind::LoadStatus) {
        ui::LOADING
    } else {
        ui::NO_CHANGES
    }
}

/// 見出しとファイルを [`WorktreeState::SECTIONS`] の順に並べ、カーソルが何行目かを
/// 一緒に返す。
///
/// **空の区画は見出しごと出さない。** カーソルが飛ばす区画（01）を描いても、
/// 選べない行が増えるだけになる。
///
/// 行番号を別の関数で数え直さないのは、見出しを何行入れるかの規則が 2 本になると、
/// 反転する行と窓の基準行が静かにずれるため。
fn rows(state: &AppState) -> (Vec<Row<'_>>, usize) {
    let mut rows = Vec::with_capacity(state.worktree.len() + WorktreeState::SECTIONS.len());
    let mut cursor = 0;

    for &section in WorktreeState::SECTIONS {
        let entries = state.worktree.snapshot.section(section);
        if entries.is_empty() {
            continue;
        }
        rows.push(Row::Heading(section));

        for (index, entry) in entries.iter().enumerate() {
            if state.worktree.section == section && state.worktree.cursor == index {
                cursor = rows.len();
            }
            rows.push(Row::File(entry));
        }
    }
    (rows, cursor)
}

/// 1 行を組む。カーソル行は反転。
fn line(row: Row<'_>, selected: bool, width: u16, ascii: bool) -> Line<'static> {
    match row {
        Row::Heading(section) => Line::from(Span::styled(heading(section), theme::muted())),
        Row::File(entry) => theme::select(Line::raw(ui::file_row(entry, width, ascii)), selected),
    }
}

/// 区画の見出し。
fn heading(section: WorktreeSection) -> &'static str {
    match section {
        WorktreeSection::Conflicted => "Conflicted",
        WorktreeSection::Staged => "Staged",
        WorktreeSection::Unstaged => "Unstaged",
        WorktreeSection::Untracked => "Untracked",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{draw, pane_text, row_is_reversed, state_at, state_with_status};
    use mewgit_core::app::{Cmd, TermSize};
    use mewgit_core::git_read::{FileStatus, WorktreeSnapshot};
    use mewgit_core::test_util::fake_entry;

    /// 右列がいちばん狭くなる端末。テストはその幅で読める内容にする。
    const SIZE: TermSize = crate::test_util::SPLIT_NARROWEST;

    /// スナップショットを `Msg::StatusLoaded` 経由で入れた状態。
    ///
    /// `AppState` に直接書かない。カーソルの詰め直し（`clamp`）を通した形を描く。
    #[test]
    fn sections_and_files_are_listed_in_order() {
        let state = state_with_status(WorktreeSnapshot {
            staged: vec![fake_entry("staged.txt", FileStatus::Added)],
            unstaged: vec![fake_entry("unstaged.txt", FileStatus::Modified)],
            untracked: vec![fake_entry("new.txt", FileStatus::Untracked)],
            conflicted: Vec::new(),
            ..WorktreeSnapshot::default()
        });

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("Working tree (3)"), "{text}");
        assert!(text.contains("Staged"), "{text}");
        assert!(text.contains("A staged.txt"), "{text}");
        assert!(text.contains("M unstaged.txt"), "{text}");
        assert!(text.contains("? new.txt"), "{text}");
        // 空の区画は見出しごと出さない。
        assert!(!text.contains("Conflicted"), "{text}");
    }

    #[test]
    fn a_long_path_keeps_its_file_name() {
        // **右で切るとファイル名が消える**（07）。右列の内側 38 セルから種別の 3 セルを
        // 除いた 35 セルに、`…/` と入るだけの末尾を置く。
        let state = state_with_status(WorktreeSnapshot {
            unstaged: vec![fake_entry(
                "crates/mewgit-tui/src/panes/a_long_file_name.rs",
                FileStatus::Modified,
            )],
            ..WorktreeSnapshot::default()
        });

        let text = pane_text(&state, SIZE, Pane::Worktree);

        assert!(text.contains("M …/src/panes/a_long_file_name.rs"), "{text}");
        assert!(!text.contains("crates/"), "{text}");
    }

    #[test]
    fn a_rename_shows_both_names() {
        let mut renamed = fake_entry("new.txt", FileStatus::Renamed);
        renamed.old_path = Some("old.txt".into());
        let state = state_with_status(WorktreeSnapshot {
            staged: vec![renamed],
            ..WorktreeSnapshot::default()
        });

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("R old.txt -> new.txt"), "{text}");
    }

    #[test]
    fn the_selected_row_is_highlighted() {
        // **区画を 2 つにする。** 反転する行は見出しの行も数えて決まる
        // （見出し(0) s.txt(1) 見出し(2) u.txt(3)）。1 区画だと、見出しを先頭の 1 行と
        // 決め打ちして区画内の位置に 1 を足すだけの数え方でも通ってしまう。
        let mut state = state_with_status(WorktreeSnapshot {
            staged: vec![fake_entry("s.txt", FileStatus::Added)],
            unstaged: vec![fake_entry("u.txt", FileStatus::Modified)],
            ..WorktreeSnapshot::default()
        });
        // 起動時のカーソルは Unstaged。
        assert!(row_is_reversed(&state, SIZE, "u.txt"));
        // 反転するのはカーソル行だけ。
        assert!(!row_is_reversed(&state, SIZE, "s.txt"));

        state.worktree.move_cursor(-1);

        assert!(row_is_reversed(&state, SIZE, "s.txt"));
        assert!(!row_is_reversed(&state, SIZE, "u.txt"));
    }

    #[test]
    fn an_empty_worktree_says_so() {
        let state = state_with_status(WorktreeSnapshot::default());

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("変更はありません"), "{text}");
    }

    #[test]
    fn a_pending_status_reads_as_loading() {
        // 読み込み中と「変更なし」を出し分ける。起動直後に「変更はありません」と
        // 出すと、まだ読んでいないだけなのに読み終えたように見える。
        let mut state = state_at("/repo");
        let _ = state.issue(Cmd::LoadStatus);

        // **このペインの中だけを見る。** 画面全体を見ると、まだ届いていない
        // ブランチ一覧が出した同じ文言で通ってしまう（`ui::LOADING` は共有）。
        let text = pane_text(&state, SIZE, Pane::Worktree);

        assert!(text.contains("読み込み中"), "{text}");
    }
}
