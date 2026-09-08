//! ワーキングツリーペインの描画。設計は `docs/design/07-ui.md` の「Worktree ペイン」。
//!
//! 1 行は `<種別 1 文字> <パス>`。区画の見出しと、その中のファイルを並べる。

use mewgit_core::app::{AppState, JobKind, Pane, WorktreeState};
use mewgit_core::git_read::WorktreeSection;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::ui::{self, theme};

/// ワーキングツリーペインを描く。
pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let block = ui::pane_block(title(state), state.focus == Pane::Worktree);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.worktree.is_empty() {
        frame.render_widget(Paragraph::new(empty_message(state)), inner);
        return;
    }

    let (lines, cursor) = lines(state);
    // 見せたいのはカーソルの 1 行だけ。Diff ペインはハンク全体を渡す。
    ui::render_rows(frame, inner, lines, cursor..cursor + 1);
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
fn lines(state: &AppState) -> (Vec<Line<'_>>, usize) {
    let mut lines = Vec::with_capacity(state.worktree.len() + WorktreeState::SECTIONS.len());
    let mut cursor = 0;

    for section in WorktreeState::SECTIONS {
        let entries = state.worktree.snapshot.section(section);
        if entries.is_empty() {
            continue;
        }
        lines.push(Line::from(Span::styled(heading(section), theme::muted())));

        for (index, entry) in entries.iter().enumerate() {
            let line = Line::raw(ui::file_row(entry));
            if state.worktree.section == section && state.worktree.cursor == index {
                cursor = lines.len();
                lines.push(line.style(theme::selected()));
            } else {
                lines.push(line);
            }
        }
    }
    (lines, cursor)
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
    use crate::test_util::{draw, pane_text, row_is_reversed, state_at};
    use mewgit_core::app::{Cmd, Msg, TermSize};
    use mewgit_core::git_read::{FileStatus, WorktreeSnapshot};
    use mewgit_core::test_util::fake_entry;
    use mewgit_core::test_util::reply;

    /// 既定の 80x24。右ペインは 32 桁になるので、テストはその幅で読める内容にする。
    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    /// スナップショットを `Msg::StatusLoaded` 経由で入れた状態。
    ///
    /// `AppState` に直接書かない。カーソルの詰め直し（`clamp`）を通した形を描く。
    fn state_with(snapshot: WorktreeSnapshot) -> AppState {
        let mut state = state_at("/repo");
        // **発行してから届ける。** 通さない返信は刻印を持たず、`status_loaded` が
        // 「書き込みより前に頼まれたもの」として頼み直す（01 の「実行中ジョブ › 世代」）。
        let _ = reply(
            &mut state,
            Cmd::LoadStatus,
            Msg::StatusLoaded(Box::new(Ok(snapshot))),
        );
        state
    }

    #[test]
    fn sections_and_files_are_listed_in_order() {
        let state = state_with(WorktreeSnapshot {
            staged: vec![fake_entry("staged.txt", FileStatus::Added)],
            unstaged: vec![fake_entry("unstaged.txt", FileStatus::Modified)],
            untracked: vec![fake_entry("new.txt", FileStatus::Untracked)],
            conflicted: Vec::new(),
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
    fn a_rename_shows_both_names() {
        let mut renamed = fake_entry("new.txt", FileStatus::Renamed);
        renamed.old_path = Some("old.txt".into());
        let state = state_with(WorktreeSnapshot {
            staged: vec![renamed],
            ..WorktreeSnapshot::default()
        });

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("R old.txt -> new.txt"), "{text}");
    }

    #[test]
    fn the_selected_row_is_highlighted() {
        let mut state = state_with(WorktreeSnapshot {
            unstaged: vec![
                fake_entry("a.txt", FileStatus::Modified),
                fake_entry("b.txt", FileStatus::Modified),
            ],
            ..WorktreeSnapshot::default()
        });
        state.worktree.move_cursor(1);

        assert!(row_is_reversed(&state, SIZE, "b.txt"));
        // 反転するのはカーソル行だけ。
        assert!(!row_is_reversed(&state, SIZE, "a.txt"));
    }

    #[test]
    fn an_empty_worktree_says_so() {
        let state = state_with(WorktreeSnapshot::default());

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

    #[test]
    fn the_cursor_row_counts_the_headings() {
        let mut state = state_with(WorktreeSnapshot {
            staged: vec![fake_entry("s.txt", FileStatus::Added)],
            unstaged: vec![fake_entry("u.txt", FileStatus::Modified)],
            ..WorktreeSnapshot::default()
        });
        // 見出し(0) s.txt(1) 見出し(2) u.txt(3)。起動時のカーソルは Unstaged。
        assert_eq!(lines(&state).1, 3);
        state.worktree.move_cursor(-1);
        assert_eq!(lines(&state).1, 1);
    }
}
