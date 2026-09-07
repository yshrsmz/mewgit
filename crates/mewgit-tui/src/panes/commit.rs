//! コミット詳細ペインの描画。設計は `docs/design/07-ui.md` の「CommitDetail ペイン」。
//!
//! グラフでコミットを選んでいるあいだ、右の領域に Working tree と差し替えで描く。
//! どちらを描くかは `focus` ではなくグラフの選択で決まる（`ui::render`）。

use mewgit_core::app::{AppState, JobKind, Pane};
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

    let Some(detail) = state.graph.fresh_detail() else {
        // カーソルの指すコミットのものが無い。届いていないのか失敗したのかは
        // `JobTracker` が知っている（07 の「CommitDetail ペイン」）。
        frame.render_widget(Paragraph::new(placeholder(state)), inner);
        return;
    };

    // 見出しの高さは折り返し後の行数から出し、メッセージのぶんは必ず残す。
    let (head, head_rows) = ui::wrapped(head_lines(detail), inner.width);
    let head_rows = head_rows.min(inner.height.saturating_sub(MIN_MESSAGE_ROWS));
    let [head_area, message_area] =
        Layout::vertical([Constraint::Length(head_rows), Constraint::Min(0)]).areas(inner);

    frame.render_widget(head, head_area);
    frame.render_widget(
        Paragraph::new(message_lines(detail, message_area.height as usize))
            .wrap(Wrap { trim: false }),
        message_area,
    );
}

/// 詳細がまだ無いときの文言。空のグラフの出し分けと同じ形。
fn placeholder(state: &AppState) -> &'static str {
    if state.jobs.is_running(JobKind::LoadCommitDetail) {
        "読み込み中…"
    } else {
        "詳細を読み込めませんでした"
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
    use crate::test_util::{draw, state_with_commits};
    use crate::ui;
    use mewgit_core::app::{update, AppState, JobKind, Key, Msg, TermSize};
    use mewgit_core::git_read::CommitDetail;
    use mewgit_core::test_util::{fake_detail, fake_oid};

    /// 1 件目のコミットを選び、その詳細が届いた状態。
    ///
    /// 起動直後のカーソルは WIP 行なので、`j` を 1 回打って乗せる。
    fn with_detail(summaries: &[&str], detail: CommitDetail) -> AppState {
        let mut state = selecting(summaries);
        let _ = update(
            &mut state,
            Msg::CommitDetailLoaded {
                oid: detail.oid,
                result: Box::new(Ok(detail)),
            },
        );
        state
    }

    /// 1 件目のコミットを選んだだけの状態。詳細はまだ届いていない。
    fn selecting(summaries: &[&str]) -> AppState {
        let mut state = state_with_commits(summaries);
        let _ = update(&mut state, Msg::Key(Key::char('j')));
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
    fn the_worktree_pane_is_not_drawn_next_to_it() {
        // 右は差し替えであって分割ではない（07 の「ペインとフォーカス」）。
        let state = with_detail(&["subject"], fake_detail(0, "subject"));

        let text = draw(&state, WIDE).join("\n");

        assert!(!text.contains("Working tree"), "{text}");
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
        let mut state = selecting(&["first"]);
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

    #[test]
    fn a_tiny_terminal_does_not_panic() {
        // リサイズの途中では枠より小さい端末が来る。
        for (width, height) in [(1, 1), (10, 3), (20, 5)] {
            let state = with_detail(&["subject"], fake_detail(0, "subject\n\nbody"));
            let lines = draw(&state, TermSize { width, height });
            assert_eq!(lines.len(), height as usize);
        }
    }
}
