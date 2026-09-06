//! Diff ペインの描画。設計は `docs/design/07-ui.md` の「Diff ペイン」。
//!
//! 左の領域に Graph と差し替えで描く。1 行はハンクの見出し（`@@ ...`）か、
//! ` ` / `-` / `+` で始まる本文の行。

use mewgit_core::app::{AppState, DiffView, JobKind, Pane};
use mewgit_core::git_read::{DiffTarget, HunkLine, LineOrigin};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::ui::{self, theme};

/// タブ幅。`\t` をここまで開く。
const TAB_WIDTH: usize = 8;

/// Diff ペインを描く。
pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let block = ui::pane_block(title(state), state.focus == Pane::Diff);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(view) = &state.diff.view else {
        frame.render_widget(Paragraph::new(pending_message(state)), inner);
        return;
    };

    let (mut lines, cursor) = lines(view);
    if lines.is_empty() {
        frame.render_widget(Paragraph::new(no_hunks(view)), inner);
        return;
    }

    let window = ui::window(lines.len(), cursor, inner.height as usize);
    // **折り返さない**（`Wrap` を付けない）。diff は桁の揃いが意味を持つので、
    // 折り返すとどの行がどの行に対応するのか読めなくなる（07）。
    frame.render_widget(
        Paragraph::new(lines.drain(window).collect::<Vec<_>>()),
        inner,
    );
}

/// 枠のタイトル。**対象を必ず出す**（07）。
///
/// 左が Graph から差し替わっているので、出さないと何の diff かを画面から辿れない。
/// まだ届いていないあいだも、選んでいるファイルの名前は `wanted_diff` から判る。
fn title(state: &AppState) -> String {
    let Some(view) = &state.diff.view else {
        return match state.wanted_diff().as_ref().and_then(DiffTarget::path) {
            Some(path) => format!("Diff: {}", path.display()),
            None => "Diff".to_string(),
        };
    };

    match view.files.as_slice() {
        [file] => format!(
            "Diff: {}",
            ui::path_label(&file.path, file.old_path.as_deref())
        ),
        files => format!("Diff ({} files)", files.len()),
    }
}

/// diff がまだ無いときの文言。
///
/// 「読みに行っていない」と「読んで失敗した」を混ぜない。conflicted の行を選んでいる
/// ときは `wanted_diff` が `None` を返すので、そもそも要求を出していない。
fn pending_message(state: &AppState) -> &'static str {
    if state.jobs.is_running(JobKind::LoadDiff) {
        "読み込み中…"
    } else if state.wanted_diff().is_none() {
        "差分を出せる行が選ばれていません"
    } else {
        "差分を読み込めませんでした"
    }
}

/// 届いてはいるがハンクが 1 つも無いときの文言。
///
/// バイナリと「mode だけの変更」はどちらも `hunks` が空になるので、`binary` で分ける
/// （02 の「diff とハンク」）。
fn no_hunks(view: &DiffView) -> &'static str {
    if view.files.is_empty() {
        "差分はありません"
    } else if view.files.iter().any(|file| file.binary) {
        "バイナリファイルです"
    } else {
        "内容の変更はありません（mode だけの変更）"
    }
}

/// 描く行と、カーソルの指すハンクの見出しが何行目かを返す。
///
/// 行番号を別の関数で数え直さないのは Worktree ペインと同じ理由で、見出しを何行入れるか
/// の規則が 2 本になると、反転する行と窓の基準行が静かにずれる。
///
/// **ファイルの見出しは 2 ファイル以上のときだけ。** 1 ファイルなら枠のタイトルに
/// 出ているので、同じ名前を 2 度並べても行を消費するだけになる。
fn lines(view: &DiffView) -> (Vec<Line<'static>>, usize) {
    let mut lines = Vec::new();
    let mut cursor = 0;
    let many = view.files.len() > 1;

    for (file_index, file) in view.files.iter().enumerate() {
        if many {
            let label = ui::path_label(&file.path, file.old_path.as_deref());
            lines.push(Line::from(Span::styled(label, theme::muted())));
        }

        for (hunk_index, hunk) in file.hunks.iter().enumerate() {
            let header = Line::from(Span::styled(hunk.header.clone(), theme::muted()));
            if view.cursor.file == file_index && view.cursor.hunk == hunk_index {
                cursor = lines.len();
                lines.push(header.style(theme::selected()));
            } else {
                lines.push(header);
            }
            lines.extend(hunk.lines.iter().map(hunk_line));
        }
    }

    (lines, cursor)
}

/// ハンクの 1 行。先頭に種別の記号を付ける。
fn hunk_line(line: &HunkLine) -> Line<'static> {
    let (mark, style) = match line.origin {
        LineOrigin::Addition | LineOrigin::AdditionNoEol => ('+', theme::added()),
        LineOrigin::Deletion | LineOrigin::DeletionNoEol => ('-', theme::removed()),
        LineOrigin::Context | LineOrigin::ContextNoEol => (' ', Style::default()),
    };
    Line::from(Span::styled(
        format!("{mark}{}", text(&line.content)),
        style,
    ))
}

/// 画面に出す本文。
///
/// `content` は `Vec<u8>` なので**ここで lossy にする**（02 の「ハンクの持ち方」）。
/// パッチに戻す入力は `content` のままで、こちらは表示専用。
///
/// `\t` は [`TAB_WIDTH`] 桁のタブストップまで空白に開く。1 個の空白に潰すと
/// インデントが崩れて、上下の行の対応が読めなくなる。桁は**本文の先頭から**数える。
/// 先頭の `-` / `+` / ` ` を含めて数えると、変更行と文脈行でタブストップが 1 桁ずれる。
/// 桁は文字数で数えるので、タブの手前に全角文字があるとタブストップがずれる
/// （タブはほぼインデントに使われるので、実際には手前は空白だけになる）。
///
/// `\r` は落とす。落とさないと CRLF のファイルで制御文字が画面に出る。
fn text(content: &[u8]) -> String {
    let mut out = String::new();
    let mut column = 0;

    for ch in String::from_utf8_lossy(content).chars() {
        match ch {
            '\t' => {
                let pad = TAB_WIDTH - column % TAB_WIDTH;
                out.push_str(&" ".repeat(pad));
                column += pad;
            }
            '\r' => {}
            ch => {
                out.push(ch);
                column += 1;
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{draw, row_of, state_at, terminal_with};
    use mewgit_core::app::{update, Cmd, Key, KeyCode, Msg, TermSize};
    use mewgit_core::git_read::{FileDiff, FileStatus, Hunk, WorktreeSnapshot};
    use mewgit_core::test_util::fake_entry;
    use ratatui::style::Modifier;

    /// 既定の 80x24。左は 46 桁になるので、テストはその幅で読める内容にする。
    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    fn line(origin: LineOrigin, content: &str) -> HunkLine {
        HunkLine {
            origin,
            content: content.as_bytes().to_vec(),
        }
    }

    fn hunk(header: &str, lines: Vec<HunkLine>) -> Hunk {
        Hunk {
            header: header.to_string(),
            lines,
        }
    }

    fn file(path: &str, hunks: Vec<Hunk>) -> FileDiff {
        FileDiff {
            path: path.into(),
            old_path: None,
            status: FileStatus::Modified,
            binary: false,
            hunks,
        }
    }

    /// ワーキングツリーに `a.txt` があり、Diff ペインを開いた状態。
    ///
    /// `AppState` に直に書かず `Enter` を通すのは、`focus` と `wanted_diff` が
    /// 実際の経路で揃った形を描くため。
    fn opened(snapshot: WorktreeSnapshot) -> AppState {
        let mut state = state_at("/repo");
        let _ = update(&mut state, Msg::StatusLoaded(Box::new(Ok(snapshot))));
        state.focus = Pane::Worktree;
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));
        state
    }

    /// 変更のある `a.txt` 1 つを選び、`files` が届いた状態。
    fn state_with_diff(files: Vec<FileDiff>) -> AppState {
        let snapshot = WorktreeSnapshot {
            unstaged: vec![fake_entry("a.txt", FileStatus::Modified)],
            ..WorktreeSnapshot::default()
        };
        let mut state = opened(snapshot);
        let target = state.wanted_diff().expect("対象が決まっていません");
        let _ = update(
            &mut state,
            Msg::DiffLoaded {
                target,
                result: Box::new(Ok(files)),
            },
        );
        state
    }

    #[test]
    fn hunks_and_lines_are_listed_with_their_marks() {
        let state = state_with_diff(vec![file(
            "a.txt",
            vec![hunk(
                "@@ -1,2 +1,2 @@ fn main() {",
                vec![
                    line(LineOrigin::Context, "keep"),
                    line(LineOrigin::Deletion, "old"),
                    line(LineOrigin::Addition, "new"),
                ],
            )],
        )]);

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("Diff: a.txt"), "{text}");
        assert!(text.contains("@@ -1,2 +1,2 @@ fn main() {"), "{text}");
        assert!(text.contains(" keep"), "{text}");
        assert!(text.contains("-old"), "{text}");
        assert!(text.contains("+new"), "{text}");
    }

    #[test]
    fn a_single_file_is_not_named_twice() {
        // 枠のタイトルに出ているので、本文に見出しを足すと行を消費するだけになる。
        let state = state_with_diff(vec![file(
            "a.txt",
            vec![hunk("@@ -1 +1 @@", vec![line(LineOrigin::Context, "keep")])],
        )]);

        let lines = draw(&state, SIZE);
        let named = lines.iter().filter(|line| line.contains("a.txt")).count();

        // 枠のタイトルと、右のワーキングツリーの行の 2 つだけ。
        assert_eq!(named, 2, "{lines:?}");
    }

    #[test]
    fn the_cursor_hunk_header_is_highlighted() {
        let mut state = state_with_diff(vec![file(
            "a.txt",
            vec![
                hunk("@@ -1 +1 @@ first", vec![line(LineOrigin::Context, "a")]),
                hunk("@@ -9 +9 @@ second", vec![line(LineOrigin::Context, "b")]),
            ],
        )]);
        // `j` で 2 つ目のハンクへ。
        let _ = update(&mut state, Msg::Key(Key::char('j')));

        let terminal = terminal_with(&state, SIZE);
        let buffer = terminal.backend().buffer();
        let lines = draw(&state, SIZE);
        let reversed =
            |y: u16| (0..SIZE.width).any(|x| buffer[(x, y)].modifier.contains(Modifier::REVERSED));

        assert!(reversed(row_of(&lines, "second")));
        assert!(!reversed(row_of(&lines, "first")));
    }

    #[test]
    fn a_tab_is_opened_to_the_tab_stop() {
        // 1 個の空白に潰すとインデントが崩れて、上下の行の対応が読めなくなる。
        // 桁は本文の先頭から数える（記号を含めると変更行と文脈行で 1 桁ずれる）。
        let state = state_with_diff(vec![file(
            "a.txt",
            vec![hunk(
                "@@ -1 +1 @@",
                vec![
                    line(LineOrigin::Context, "\tone"),
                    line(LineOrigin::Addition, "\ttwo"),
                ],
            )],
        )]);

        let lines = draw(&state, SIZE);
        let row = |needle: &str| lines[row_of(&lines, needle) as usize].clone();

        // 枠(1) + 記号(1) + タブ 8 桁。
        assert!(row("one").starts_with("│         one"), "{:?}", row("one"));
        assert!(row("two").starts_with("│+        two"), "{:?}", row("two"));
    }

    #[test]
    fn a_carriage_return_does_not_reach_the_screen() {
        // 落とさないと CRLF のファイルで制御文字が画面に出る。
        let state = state_with_diff(vec![file(
            "a.txt",
            vec![hunk(
                "@@ -1 +1 @@",
                vec![line(LineOrigin::Addition, "crlf\r")],
            )],
        )]);

        let lines = draw(&state, SIZE);
        let row = &lines[row_of(&lines, "crlf") as usize];

        assert!(!row.contains('\r'), "{row:?}");
    }

    #[test]
    fn a_long_line_is_cut_instead_of_wrapped() {
        // 折り返すと、どの行がどの行に対応するのか読めなくなる（07）。
        let state = state_with_diff(vec![file(
            "a.txt",
            vec![hunk(
                "@@ -1,2 +1,2 @@",
                vec![
                    line(LineOrigin::Addition, &"x".repeat(200)),
                    line(LineOrigin::Context, "TAIL"),
                ],
            )],
        )]);

        let lines = draw(&state, SIZE);

        // 長い行の次の行がそのまま来る。折り返していれば 2 行以上ずれる。
        assert_eq!(
            row_of(&lines, "TAIL"),
            row_of(&lines, "xxx") + 1,
            "{lines:?}"
        );
    }

    #[test]
    fn the_title_shows_both_names_of_a_rename() {
        let mut renamed = file("new.txt", vec![hunk("@@ -1 +1 @@", Vec::new())]);
        renamed.old_path = Some("old.txt".into());
        renamed.status = FileStatus::Renamed;

        let state = state_with_diff(vec![renamed]);

        let text = draw(&state, SIZE).join("\n");
        assert!(text.contains("Diff: old.txt -> new.txt"), "{text}");
    }

    #[test]
    fn a_binary_file_says_so() {
        // バイナリと「mode だけの変更」はどちらもハンクが空になる。
        let mut binary = file("a.png", Vec::new());
        binary.binary = true;

        let state = state_with_diff(vec![binary]);

        let text = draw(&state, SIZE).join("\n");
        assert!(text.contains("バイナリ"), "{text}");
    }

    #[test]
    fn a_mode_only_change_is_not_called_binary() {
        let state = state_with_diff(vec![file("a.txt", Vec::new())]);

        let text = draw(&state, SIZE).join("\n");
        assert!(text.contains("mode だけの変更"), "{text}");
    }

    #[test]
    fn a_pending_diff_reads_as_loading() {
        // 起動直後に「読み込めませんでした」と出すと、まだ読んでいないだけなのに
        // 失敗したように見える。
        let state = opened(WorktreeSnapshot {
            unstaged: vec![fake_entry("a.txt", FileStatus::Modified)],
            ..WorktreeSnapshot::default()
        });

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("読み込み中"), "{text}");
        // 届く前でも、何の diff を待っているかは出す。
        assert!(text.contains("Diff: a.txt"), "{text}");
    }

    #[test]
    fn a_conflicted_row_is_not_reported_as_a_failure() {
        // 読みに行っていないので「読み込めませんでした」ではない。
        let state = opened(WorktreeSnapshot {
            conflicted: vec![fake_entry("c.txt", FileStatus::Conflicted)],
            ..WorktreeSnapshot::default()
        });

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("選ばれていません"), "{text}");
        assert!(!text.contains("読み込めませんでした"), "{text}");
    }

    #[test]
    fn a_failed_diff_says_so() {
        let mut state = opened(WorktreeSnapshot {
            unstaged: vec![fake_entry("a.txt", FileStatus::Modified)],
            ..WorktreeSnapshot::default()
        });
        let target = state.wanted_diff().unwrap();
        let cmd = update(
            &mut state,
            Msg::DiffLoaded {
                target,
                result: Box::new(Err(mewgit_core::git_read::GitReadError::NonUtf8Head)),
            },
        );
        assert_eq!(cmd, Cmd::None);

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("読み込めませんでした"), "{text}");
    }
}
