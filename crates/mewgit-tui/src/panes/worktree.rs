//! ワーキングツリーペインの描画。設計は `docs/design/07-ui.md` の「Worktree ペイン」。
//!
//! 1 行は `<種別 1 文字> <パス>`。区画の見出しと、その中のファイルを並べる。

use mewgit_core::app::{AppState, JobKind, Pane, WorktreeState};
use mewgit_core::git_read::{FileEntry, FileStatus, WorktreeSection};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::ui;

/// ワーキングツリーペインを描く。
pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let block = ui::pane_block(title(state), state.focus == Pane::Worktree);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.worktree.is_empty() {
        frame.render_widget(Paragraph::new(empty_message(state)), inner);
        return;
    }

    let lines = lines(state);
    let window = window(lines.len(), cursor_row(state), inner.height as usize);
    frame.render_widget(Paragraph::new(lines[window].to_vec()), inner);
}

/// 枠のタイトル。件数は全区画の合計。
fn title(state: &AppState) -> String {
    format!("Working tree ({})", state.worktree.len())
}

/// 行が 1 つも無いときの文言。読み込み中と「変更なし」を出し分ける
/// （グラフの空表示と同じ形）。
fn empty_message(state: &AppState) -> &'static str {
    if state.jobs.is_running(JobKind::LoadStatus) {
        "読み込み中…"
    } else {
        "変更はありません"
    }
}

/// 見出しとファイルを [`WorktreeState::SECTIONS`] の順に並べる。
///
/// **空の区画は見出しごと出さない。** カーソルが飛ばす区画（01）を描いても、
/// 選べない行が増えるだけになる。
fn lines(state: &AppState) -> Vec<Line<'_>> {
    let mut lines = Vec::with_capacity(state.worktree.len() + WorktreeState::SECTIONS.len());

    for section in WorktreeState::SECTIONS {
        let entries = state.worktree.snapshot.section(section);
        if entries.is_empty() {
            continue;
        }
        lines.push(Line::from(Span::styled(
            heading(section),
            Style::default().add_modifier(Modifier::DIM),
        )));
        lines.extend(entries.iter().enumerate().map(|(index, entry)| {
            let line = Line::raw(format!(" {} {}", mark(entry.status), path(entry)));
            if state.worktree.section == section && state.worktree.cursor == index {
                line.style(Style::default().add_modifier(Modifier::REVERSED))
            } else {
                line
            }
        }));
    }
    lines
}

/// カーソルの行が [`lines`] の何行目か。見出しのぶんも数える。
fn cursor_row(state: &AppState) -> usize {
    let mut row = 0;
    for section in WorktreeState::SECTIONS {
        let entries = state.worktree.snapshot.section(section);
        if entries.is_empty() {
            continue;
        }
        row += 1; // 見出し
        if state.worktree.section == section {
            return row + state.worktree.cursor;
        }
        row += entries.len();
    }
    row
}

/// 描く行の範囲。カーソルが窓に入るまで下へずらす。
///
/// **スクロール位置を状態に持たない。** グラフ（01 の `scroll`）と違って
/// カーソルが下端に貼り付く癖が残るが、そのために `Msg::Layout` と同じ経路を
/// もう 1 本引くのは、右ペインの高さが決まる M6（狭いレイアウト）と一緒にする。
fn window(len: usize, cursor: usize, height: usize) -> std::ops::Range<usize> {
    if height == 0 {
        return 0..0;
    }
    let start = cursor
        .saturating_sub(height - 1)
        .min(len.saturating_sub(height));
    start..(start + height).min(len)
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

/// 変更種別の 1 文字。`git status --short` と同じ字を使う。
fn mark(status: FileStatus) -> char {
    match status {
        FileStatus::Added => 'A',
        FileStatus::Modified => 'M',
        FileStatus::Deleted => 'D',
        FileStatus::Renamed => 'R',
        FileStatus::Copied => 'C',
        FileStatus::Typechange => 'T',
        FileStatus::Untracked => '?',
        FileStatus::Conflicted => 'U',
    }
}

/// 行に出すパス。rename は元の名前も出す。
fn path(entry: &FileEntry) -> String {
    match &entry.old_path {
        Some(old) => format!("{} -> {}", old.display(), entry.path.display()),
        None => entry.path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{draw, state_at, terminal_with};
    use mewgit_core::app::{update, Msg, TermSize};
    use mewgit_core::git_read::WorktreeSnapshot;

    /// 80x24 だと右ペインは 32 桁。テストはその幅で読める内容にする。
    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    fn entry(path: &str, status: FileStatus) -> FileEntry {
        FileEntry {
            path: path.into(),
            old_path: None,
            status,
        }
    }

    /// スナップショットを `Msg::StatusLoaded` 経由で入れた状態。
    ///
    /// `AppState` に直接書かない。カーソルの詰め直し（`clamp`）を通した形を描く。
    fn state_with(snapshot: WorktreeSnapshot) -> AppState {
        let mut state = state_at("/repo");
        let _ = update(&mut state, Msg::StatusLoaded(Box::new(Ok(snapshot))));
        state
    }

    #[test]
    fn sections_and_files_are_listed_in_order() {
        let state = state_with(WorktreeSnapshot {
            staged: vec![entry("staged.txt", FileStatus::Added)],
            unstaged: vec![entry("unstaged.txt", FileStatus::Modified)],
            untracked: vec![entry("new.txt", FileStatus::Untracked)],
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
        let mut renamed = entry("new.txt", FileStatus::Renamed);
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
                entry("a.txt", FileStatus::Modified),
                entry("b.txt", FileStatus::Modified),
            ],
            ..WorktreeSnapshot::default()
        });
        state.worktree.move_cursor(1);

        let terminal = terminal_with(&state, SIZE);
        let buffer = terminal.backend().buffer();
        let lines = draw(&state, SIZE);
        let row_of = |needle: &str| {
            lines
                .iter()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("{needle} の行がありません: {lines:?}"))
                as u16
        };
        // 反転しているセルがあるか。行の右側は余白なのでスタイルが付かない。
        let reversed =
            |y: u16| (0..SIZE.width).any(|x| buffer[(x, y)].modifier.contains(Modifier::REVERSED));

        assert!(reversed(row_of("b.txt")));
        // 反転するのはカーソル行だけ。
        assert!(!reversed(row_of("a.txt")));
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
        let _ = state.issue(mewgit_core::app::Cmd::LoadStatus);

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("読み込み中"), "{text}");
    }

    #[test]
    fn the_window_follows_the_cursor_past_the_bottom() {
        // 窓に入っているあいだは先頭から。入らなくなったらカーソルが見えるまでずらす。
        assert_eq!(window(10, 0, 4), 0..4);
        assert_eq!(window(10, 3, 4), 0..4);
        assert_eq!(window(10, 4, 4), 1..5);
        assert_eq!(window(10, 9, 4), 6..10);
        // 行が窓より少ないときは全部出す。
        assert_eq!(window(2, 1, 4), 0..2);
        // 高さ 0 でも破綻しない（リサイズ中に起きる）。
        assert_eq!(window(10, 5, 0), 0..0);
    }

    #[test]
    fn the_cursor_row_counts_the_headings() {
        let mut state = state_with(WorktreeSnapshot {
            staged: vec![entry("s.txt", FileStatus::Added)],
            unstaged: vec![entry("u.txt", FileStatus::Modified)],
            ..WorktreeSnapshot::default()
        });
        // 見出し(0) s.txt(1) 見出し(2) u.txt(3)。起動時のカーソルは Unstaged。
        assert_eq!(cursor_row(&state), 3);
        state.worktree.move_cursor(-1);
        assert_eq!(cursor_row(&state), 1);
    }
}
