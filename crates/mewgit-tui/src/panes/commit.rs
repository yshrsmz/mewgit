//! コミット詳細ペインの描画。設計は `docs/design/07-ui.md` の「CommitDetail ペイン」。
//!
//! グラフでコミットを選んでいるあいだ、右の領域に Working tree と差し替えで描く。
//! どちらを描くかは `focus` ではなくグラフの選択で決まる（`ui::render`）。

use mewgit_core::app::{AppState, Pane};
use mewgit_core::git_read::{Commit, CommitDetail, Signature, Timestamp};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::ui::{self, theme, time};

/// 詳細のうち、メッセージに必ず残す行数。
///
/// 見出しは幅が狭いと折り返して伸びる（80 桁では右ペインが 32 桁になり、
/// `Author: 名前 <メール>  日時` が 3 行を超える）。1 つの `Paragraph` にまとめると、
/// 伸びたぶんメッセージが下から押し出されて**本文が 1 行も見えなくなる**。先に
/// メッセージの領域を取り、足りないときは見出しのほうを切る。
const MIN_MESSAGE_ROWS: u16 = 3;

/// コミット詳細ペインを描く。
///
/// **選んでいるコミットを引数で受ける。** `state` から引き直すと「選んでいないのに
/// 呼ばれた」ときの腕が要り、その腕はどのテストからも踏めない（呼ぶかどうかを
/// 決めているのは同じ `selection()` なので）。
pub fn render(frame: &mut Frame, area: Rect, state: &AppState, commit: &Commit) {
    // **対象を枠に出す。** 右が差し替わっているので、出さないと誰の詳細かを画面から
    // 辿れない（Diff ペインと同じ理由）。
    let title = format!("Commit {}", ui::short_oid(&commit.oid));
    let block = ui::pane_block(title, state.focus == Pane::CommitDetail);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // **一覧は先に組んでから高さを決める。** 行数を別に数えると、見出しや文言の
    // 行数を変えたときに片方だけ直しても通り、枠からはみ出すか余白が残る
    // （`panes::worktree::lines` と同じ理由）。残りを詳細に使う。
    let (files, files_cursor) = files_lines(state);
    // 上限は内側の半分。ファイルの多いコミットで**メッセージが 1 行も見えなくなる**のを
    // 防ぐ（見出しに `MIN_MESSAGE_ROWS` を確保しているのと同じ理由）。規則は
    // Branches ペインと共有する（`ui::capped_rows`）。
    let files_rows = ui::capped_rows(files.len(), inner.height);
    let [top, files_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(files_rows)]).areas(inner);

    // **詳細と一覧は別々に届くので、片方だけで描く。** 詳細が無いからと一覧ごと
    // 落とすと、`app` は一覧を「画面に出ている」として扱う（`fresh_files_mut` が
    // カーソルを動かし、`Enter` が `DiffTarget::Commit` を開く）のに、ユーザーは
    // その行を一度も見ていないことになる。
    render_detail(frame, top, state);
    ui::render_rows(frame, files_area, files, files_cursor..files_cursor + 1);
}

/// 見出しとメッセージを描く。届いていなければ理由を出す。
fn render_detail(frame: &mut Frame, area: Rect, state: &AppState) {
    let Some(detail) = state.graph.fresh_detail() else {
        // カーソルの指すコミットのものが無い（07 の「CommitDetail ペイン」）。
        frame.render_widget(Paragraph::new(placeholder(state)), area);
        return;
    };

    // 見出しの高さは折り返し後の行数から出し、メッセージのぶんは必ず残す。
    let (head, head_rows) = ui::wrapped(head_lines(detail), area.width);
    let head_rows = head_rows.min(area.height.saturating_sub(MIN_MESSAGE_ROWS));
    let [head_area, message_area] =
        Layout::vertical([Constraint::Length(head_rows), Constraint::Min(0)]).areas(area);

    frame.render_widget(head, head_area);
    frame.render_widget(
        Paragraph::new(message_lines(detail, message_area.height as usize))
            .wrap(Wrap { trim: false }),
        message_area,
    );
}

/// ファイル一覧の行と、カーソルが何行目か。
///
/// 一覧が出せないときは文言 1 行（カーソルはその行）。**行を組むのはここ 1 本**で、
/// 割く高さもこの行数から出す。行番号を別に数えると、見出しを何行入れるかの規則が
/// 2 本になり、反転する行と窓の基準行が静かにずれる
/// （`panes::worktree::lines` と同じ）。
fn files_lines(state: &AppState) -> (Vec<Line<'_>>, usize) {
    // **空の一覧と「まだ届いていない」を出し分ける**（01 の「コミットのファイル
    // 一覧」）。`--allow-empty` のコミットは変更を持たない。
    let Some(files) = state
        .graph
        .fresh_files()
        .filter(|files| !files.entries.is_empty())
    else {
        return (vec![Line::raw(files_placeholder(state))], 0);
    };

    let mut lines = Vec::with_capacity(files.entries.len() + 1);
    lines.push(Line::from(Span::styled(
        format!("Files ({})", files.entries.len()),
        theme::muted(),
    )));
    let mut cursor = 0;
    for (index, entry) in files.entries.iter().enumerate() {
        let line = Line::raw(ui::file_row(entry));
        if index == files.cursor {
            cursor = lines.len();
            lines.push(line.style(theme::selected()));
        } else {
            lines.push(line);
        }
    }
    (lines, cursor)
}

/// 一覧が出せないときの文言。
fn files_placeholder(state: &AppState) -> &'static str {
    if state.graph.fresh_files().is_some() {
        // 届いていて空。`--allow-empty` のコミット。
        ui::NO_CHANGES
    } else if state.graph.has_files_failed() {
        "ファイル一覧を読み込めませんでした"
    } else {
        ui::LOADING
    }
}

/// 詳細がまだ無いときの文言。
///
/// **「失敗した」は状態から見る**（[`GraphState::has_detail_failed`]）。
/// `jobs.is_running` の否定から消去法で導くと、`Cmd` の発行経路を変えたときに
/// 片方だけ直してもコンパイルもテストも通る（01 の「diff の遅延取得」。
/// `DiffState::has_failed` と同じ形）。
///
/// [`GraphState::has_detail_failed`]: mewgit_core::app::GraphState::has_detail_failed
fn placeholder(state: &AppState) -> &'static str {
    if state.graph.has_detail_failed() {
        "詳細を読み込めませんでした"
    } else {
        ui::LOADING
    }
}

/// 詳細の見出し。`git show` の並びに近づける。
fn head_lines(detail: &CommitDetail) -> Vec<Line<'_>> {
    let mut lines = vec![
        Line::from(Span::styled(
            format!("commit {}", detail.oid),
            theme::muted(),
        )),
        Line::raw(who("Author", &detail.author, detail.author_time)),
        Line::raw(who("Commit", &detail.committer, detail.committer_time)),
    ];

    if !detail.parents.is_empty() {
        let parents: Vec<String> = detail.parents.iter().map(ui::short_oid).collect();
        lines.push(Line::raw(format!("Parent: {}", parents.join(" "))));
    }

    lines.push(Line::raw(""));
    lines
}

/// メッセージ全文。`rows` は描ける行数で、そこまでで打ち切る。
///
/// 1 行が折り返しで 2 行以上になることはあっても 0 行になることはないので、見える行を
/// 落とすことはない。マージや revert の本文は数百行になることがあり、全部組むと毎
/// フレームその数だけ `Line` を作ることになる。
fn message_lines(detail: &CommitDetail, rows: usize) -> Vec<Line<'_>> {
    detail.message.lines().take(rows).map(Line::raw).collect()
}

/// `Author: name <mail>  2026-09-06 15:35:55 +0900`。
fn who(label: &str, sig: &Signature, time: Timestamp) -> String {
    format!(
        "{label}: {} <{}>  {}",
        sig.name,
        sig.email,
        time::format(time)
    )
}

#[cfg(test)]
mod tests {
    use crate::test_util::{draw, row_is_reversed, state_on_first_commit};
    use crate::ui;
    use mewgit_core::app::{update, AppState, JobKind, Key, Msg, TermSize};
    use mewgit_core::git_read::{CommitDetail, FileEntry, FileStatus, GitReadError, Oid};
    use mewgit_core::test_util::{fake_detail, fake_entry, fake_oid};

    /// 1 件目のコミットを選び、その詳細が届いた状態。
    ///
    /// 起動直後のカーソルは WIP 行なので、`j` を 1 回打って乗せる。
    fn with_detail(summaries: &[&str], detail: CommitDetail) -> AppState {
        let mut state = state_on_first_commit(summaries);
        let _ = update(
            &mut state,
            Msg::CommitDetailLoaded {
                oid: detail.oid,
                result: Box::new(Ok(detail)),
            },
        );
        state
    }

    /// 見出しの折り返しを避けたいときの端末。80 桁だと右ペインが 32 桁になる。
    const WIDE: TermSize = TermSize {
        width: 120,
        height: 24,
    };

    #[test]
    fn the_frame_names_the_commit_it_is_showing() {
        // 右が差し替わっているので、出さないと誰の詳細かを画面から辿れない（07）。
        let state = with_detail(&["subject"], fake_detail(0, "subject"));

        let text = draw(&state, WIDE).join("\n");

        assert!(
            text.contains(&format!("Commit {}", ui::short_oid(&fake_oid(0)))),
            "{text}"
        );
    }

    #[test]
    fn the_detail_shows_the_message_signatures_and_parents() {
        let mut detail = fake_detail(0, "subject\n\nbody line");
        detail.parents = vec![fake_oid(1)];
        let state = with_detail(&["subject"], detail);

        let text = draw(&state, WIDE).join("\n");

        assert!(text.contains("subject"), "{text}");
        assert!(text.contains("body line"), "{text}");
        assert!(text.contains("Author: mewgit test"), "{text}");
        assert!(text.contains("Commit: mewgit test"), "{text}");
        // 日時はコミットのタイムゾーンで出す（`fake_detail` は epoch の +0000）。
        assert!(text.contains("1970-01-01 00:00:00 +0000"), "{text}");
        assert!(
            text.contains(&format!("Parent: {}", ui::short_oid(&fake_oid(1)))),
            "{text}"
        );
    }

    #[test]
    fn the_message_survives_a_narrow_pane() {
        // 80 桁だと右ペインは 32 桁で、見出しが折り返して高さを埋める。1 つの
        // `Paragraph` にまとめていると本文が 1 行も見えなくなる（07）。
        let detail = fake_detail(0, "subject line\n\nbody line");
        let state = with_detail(&["subject line"], detail);

        let text = draw(&state, TermSize::default()).join("\n");

        assert!(text.contains("subject line"), "{text}");
        assert!(text.contains("body line"), "{text}");
    }

    #[test]
    fn a_detail_for_another_commit_says_it_is_loading() {
        // カーソルを動かした直後は、まだ前のコミットの詳細しか無い。
        let mut state = with_detail(&["first", "second"], fake_detail(0, "first"));
        let _ = update(&mut state, Msg::Key(Key::char('j')));
        assert!(state.jobs.is_running(JobKind::LoadCommitDetail));

        let text = draw(&state, WIDE).join("\n");

        assert!(text.contains("読み込み中"), "{text}");
        // 前のコミットの本文を出したままにしない。
        assert!(!text.contains("Author: mewgit test"), "{text}");
    }

    #[test]
    fn a_detail_that_failed_says_so_instead_of_loading() {
        let mut state = state_on_first_commit(&["first"]);
        let oid = state.graph.cursor_commit().unwrap().oid;
        let _ = update(
            &mut state,
            Msg::CommitDetailLoaded {
                oid,
                result: Box::new(Err(mewgit_core::git_read::GitReadError::NonUtf8Head)),
            },
        );

        let text = draw(&state, WIDE).join("\n");

        assert!(text.contains("詳細を読み込めませんでした"), "{text}");
    }

    /// ファイル一覧の返信を届ける。**成功も失敗もここを通す。**
    fn with_files(state: &mut AppState, oid: Oid, result: Result<Vec<FileEntry>, GitReadError>) {
        let _ = update(
            state,
            Msg::CommitFilesLoaded {
                oid,
                result: Box::new(result),
            },
        );
    }

    /// 詳細と一覧の両方が届いた状態。
    fn with_detail_and_files(entries: Vec<FileEntry>) -> AppState {
        let mut state = with_detail(&["subject"], fake_detail(0, "subject"));
        with_files(&mut state, fake_oid(0), Ok(entries));
        state
    }

    #[test]
    fn the_file_list_shows_what_the_commit_changed() {
        // 80x24 のまま見る。右ペインは 32 桁（07）。
        let state = with_detail_and_files(vec![
            fake_entry("a.txt", FileStatus::Modified),
            fake_entry("b.txt", FileStatus::Added),
        ]);

        let text = draw(&state, TermSize::default()).join("\n");

        assert!(text.contains("Files (2)"), "{text}");
        assert!(text.contains("M a.txt"), "{text}");
        assert!(text.contains("A b.txt"), "{text}");
    }

    #[test]
    fn the_selected_file_is_highlighted() {
        // どのファイルの diff が左に出るかを、左のタイトルと合わせて 2 箇所から
        // 辿れるようにする（07）。
        let mut state = with_detail_and_files(vec![
            fake_entry("a.txt", FileStatus::Modified),
            fake_entry("b.txt", FileStatus::Added),
        ]);
        state.focus = mewgit_core::app::Pane::CommitDetail;
        let _ = update(&mut state, Msg::Key(Key::char('j')));

        let size = TermSize::default();

        assert!(row_is_reversed(&state, size, "b.txt"));
        assert!(!row_is_reversed(&state, size, "a.txt"));
    }

    #[test]
    fn a_commit_without_changes_says_so() {
        // `--allow-empty` のコミット。**「読み込み中」と同じ見た目にしない**（01）。
        let state = with_detail_and_files(Vec::new());

        let text = draw(&state, TermSize::default()).join("\n");

        assert!(text.contains("変更はありません"), "{text}");
        assert!(!text.contains("読み込み中"), "{text}");
    }

    #[test]
    fn a_pending_file_list_reads_as_loading() {
        // 詳細と一覧は別々に届く。メッセージが出ていて一覧だけ「読み込み中…」に
        // なる時点が普通にある（07）。
        let state = with_detail(&["subject"], fake_detail(0, "subject"));

        let text = draw(&state, WIDE).join("\n");

        assert!(state.jobs.is_running(JobKind::LoadCommitFiles));
        assert!(text.contains("subject"), "{text}");
        assert!(text.contains("読み込み中"), "{text}");
    }

    #[test]
    fn a_file_list_that_failed_says_so_instead_of_loading() {
        let mut state = with_detail(&["subject"], fake_detail(0, "subject"));
        with_files(&mut state, fake_oid(0), Err(GitReadError::NonUtf8Head));

        let text = draw(&state, WIDE).join("\n");

        assert!(
            text.contains("ファイル一覧を読み込めませんでした"),
            "{text}"
        );
    }

    #[test]
    fn a_long_file_list_leaves_the_message_visible() {
        // 一覧は必要なぶんだけ取るが、内側の半分で頭打ちにする。上限が無いと
        // ファイルの多いコミットでメッセージが 1 行も見えなくなる（07）。
        let entries = (0..40)
            .map(|n| fake_entry(&format!("file{n}.txt"), FileStatus::Modified))
            .collect();
        let mut state = with_detail(
            &["subject line"],
            fake_detail(0, "subject line\n\nbody line"),
        );
        with_files(&mut state, fake_oid(0), Ok(entries));

        let text = draw(&state, TermSize::default()).join("\n");

        assert!(text.contains("subject line"), "{text}");
        assert!(text.contains("body line"), "{text}");
        assert!(text.contains("Files (40)"), "{text}");
    }

    #[test]
    fn the_file_list_shows_even_when_the_detail_failed() {
        // 詳細と一覧は別々に届く。詳細が無いからと一覧ごと落とすと、`app` は一覧を
        // 「画面に出ている」として扱う（カーソルが動き、`Enter` が diff を開く）のに、
        // ユーザーはその行を一度も見ていないことになる。
        let mut state = state_on_first_commit(&["subject"]);
        let oid = state.graph.cursor_commit().unwrap().oid;
        let _ = update(
            &mut state,
            Msg::CommitDetailLoaded {
                oid,
                result: Box::new(Err(GitReadError::NonUtf8Head)),
            },
        );
        with_files(
            &mut state,
            oid,
            Ok(vec![fake_entry("a.txt", FileStatus::Modified)]),
        );

        let text = draw(&state, WIDE).join("\n");

        assert!(text.contains("詳細を読み込めませんでした"), "{text}");
        assert!(text.contains("M a.txt"), "{text}");
    }

    #[test]
    fn a_detail_for_another_commit_hides_the_file_list_too() {
        // 一覧も oid の一致で濾す。前のコミットのファイルを出したままにしない。
        let mut state = with_detail(&["first", "second"], fake_detail(0, "first"));
        with_files(
            &mut state,
            fake_oid(0),
            Ok(vec![fake_entry("a.txt", FileStatus::Modified)]),
        );
        // 2 件目へ。1 件目の一覧しか持っていない。
        let _ = update(&mut state, Msg::Key(Key::char('j')));

        let text = draw(&state, WIDE).join("\n");

        assert!(!text.contains("a.txt"), "{text}");
    }

    #[test]
    fn a_tiny_terminal_does_not_panic() {
        // リサイズの途中では枠より小さい端末が来る。
        for (width, height) in [(1, 1), (10, 3), (20, 5)] {
            let mut state = with_detail(&["subject"], fake_detail(0, "subject\n\nbody"));
            with_files(
                &mut state,
                fake_oid(0),
                Ok(vec![fake_entry("a.txt", FileStatus::Modified)]),
            );
            let lines = draw(&state, TermSize { width, height });
            assert_eq!(lines.len(), height as usize);
        }
    }
}
