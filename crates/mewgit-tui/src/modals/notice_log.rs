//! 通知の履歴モーダルの描画。設計は `docs/design/07-ui.md` の「NoticeLog モーダル」。
//!
//! **ステータスバーでは読めないものを出す場所**（01 の「通知」）—— 日付と、
//! `Notice.detail`（argv と stderr）と、`message` の 2 行目以降。
//!
//! **`modals::picker` を使わない。** あちらは「本文 → 一覧」の順で、本文を先に測って
//! 一覧に残りを渡す。ここは「一覧 → カーソル行の中身」で、中身がカーソルで入れ替わる
//! うえ、長さに上限が無い（stderr）。一覧に取る行数を先に決めて中身に残りを渡すので、
//! 割り方が逆になる。

use std::collections::VecDeque;

use mewgit_core::app::{Notice, NoticeLevel};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::modals;
use crate::ui::{self, theme, time};

const TITLE: &str = "通知の履歴";

/// キーの案内。`update::picker_key` が受けるキーと揃える（`Enter` は何もしない）。
const HINT: &str = "[j/k] 選ぶ   [Esc] 閉じる";

/// 箱の幅。**`modals::WIDTH`（60）より広く取る** —— stderr は英語の長い行で、
/// 60 では 1 行が 2〜3 行に折れる。狭い端末では画面の幅まで縮む。
const WIDTH: u16 = 100;

/// 通知が 1 件も無いときの本文。
const EMPTY: &str = "通知はまだありません";

/// 一覧と中身の区切りに取る行数。
const RULE_ROWS: u16 = 1;

/// 通知の履歴を描く。一覧は新しい順で、`cursor` も新しいほうから数える
/// （`Modal::NoticeLog`）。
pub fn render(frame: &mut Frame, area: Rect, notices: &VecDeque<Notice>, cursor: usize) {
    let block = modals::block(TITLE);
    let width = modals::inner_width(area, WIDTH, &block);

    let Some(last) = notices.len().checked_sub(1) else {
        let (text, rows) = ui::wrapped(EMPTY, width);
        let inner = modals::place(frame, area, block, WIDTH, rows, HINT);
        frame.render_widget(text, inner);
        return;
    };
    // `cursor` は core が一覧の中に保っている（`AppState::push_notice`）。ここで
    // 丸めるのは、外れていても描画で落ちないためだけ。
    let cursor = cursor.min(last);

    let mut rows: Vec<Line<'_>> = notices.iter().rev().map(|n| row(n, width)).collect();
    let (content, content_rows) = ui::wrapped(content(&notices[last - cursor]), width);
    let list_rows = u16::try_from(rows.len()).unwrap_or(u16::MAX);

    let inner = modals::place(
        frame,
        area,
        block,
        WIDTH,
        list_rows
            .saturating_add(RULE_ROWS)
            .saturating_add(content_rows),
        HINT,
    );

    // **一覧に取るのは内側の半分まで。** 一覧を先に満たすと、100 件のときに中身が
    // 0 行になり、開いた理由（`detail` を読む）が消える（07）。
    let list_height = list_rows.min((inner.height / 2).max(1));
    let [list, rule, body] = Layout::vertical([
        Constraint::Length(list_height),
        Constraint::Length(RULE_ROWS),
        Constraint::Min(0),
    ])
    .areas(inner);

    // カーソル行はペインと同じ反転（`theme::selected`）。
    if let Some(line) = rows.get_mut(cursor) {
        line.style = theme::selected();
    }
    ui::render_rows(frame, list, rows, cursor..cursor + 1);

    frame.render_widget(
        Block::default()
            .borders(Borders::TOP)
            .border_style(theme::muted()),
        rule,
    );

    // **入りきらなければ最下行を `…` にする。** 黙って切ると、stderr の続きが
    // あることが画面から判らない（07）。
    if content_rows > body.height && body.height > 0 {
        let [shown, more] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(body);
        frame.render_widget(content, shown);
        frame.render_widget(Paragraph::new(Span::styled("…", theme::muted())), more);
    } else {
        frame.render_widget(content, body);
    }
}

/// 一覧の 1 行 —— 日時、重さの 1 文字、`message` の 1 行目。
///
/// **重さを字でも出す。** 色だけにすると、色の出ない端末で区別が消える（07）。
fn row(notice: &Notice, width: u16) -> Line<'_> {
    let (mark, style) = level(notice.level);
    let at = Span::styled(format!("{} ", time::format(notice.at)), theme::muted());
    let mark = Span::styled(format!("{mark} "), style);
    let used = u16::try_from(at.width() + mark.width()).unwrap_or(u16::MAX);
    let message = Span::raw(ui::ellipsize(
        ui::first_line(&notice.message),
        width.saturating_sub(used),
    ));
    Line::from(vec![at, mark, message])
}

/// 重さの字とその色。
///
/// **網羅 `match`。** 重さを足したら、ここで字を選ばされる。
fn level(level: NoticeLevel) -> (char, Style) {
    match level {
        NoticeLevel::Error => ('E', theme::removed()),
        NoticeLevel::Warn => ('W', theme::warn()),
        NoticeLevel::Info => ('I', theme::muted()),
    }
}

/// カーソル行の中身 —— `message` の全文、空行、`detail` の全文。
///
/// **`message` を 1 行目で切らない。** toml のパースエラーのように複数行の案内がある
/// （ステータスバーが 1 行目だけを描くのはこのため。07）。
fn content(notice: &Notice) -> Text<'static> {
    let mut text = Text::from(expand_tabs(&notice.message));
    if let Some(detail) = &notice.detail {
        text.push_line(Line::default());
        text.extend(Text::from(expand_tabs(detail)));
    }
    text
}

/// タブを空白 [`TAB_WIDTH`] 個に置き換える。
///
/// **ratatui はタブを描かない**（幅 0 の字として落ちる）。git の stderr は対象の
/// ファイルをタブで字下げして並べる（`would be overwritten by checkout:` の続き）ので、
/// そのままだと字下げが消えて見出しと区別できない（07）。
fn expand_tabs(text: &str) -> String {
    text.replace('\t', &" ".repeat(TAB_WIDTH))
}

const TAB_WIDTH: usize = 4;

#[cfg(test)]
mod tests {
    use crate::test_util::{draw, row_is_reversed, row_of, state_with_modal};
    use crate::ui::time;
    use mewgit_core::app::{update, AppState, Key, Modal, Msg, NoticeLevel, TermSize};

    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    /// 通知を `(重さ, message, detail)` の順に積み、履歴を開いた状態（最後が最新）。
    fn log_of(notices: &[(NoticeLevel, &str, Option<&str>)]) -> AppState {
        let mut state = state_with_modal(Modal::NoticeLog { cursor: 0 });
        for (level, message, detail) in notices {
            state.push_notice(*level, (*message).to_string(), detail.map(str::to_string));
        }
        // 積むたびにカーソルが同じ通知に留まるので、最新へ戻す。
        state.modal = Some(Modal::NoticeLog { cursor: 0 });
        state
    }

    #[test]
    fn the_log_lists_the_newest_first_with_the_date_and_the_level() {
        let state = log_of(&[
            (NoticeLevel::Info, "fetch しました", None),
            (NoticeLevel::Error, "切り替えられません", None),
        ]);
        let at = time::format(state.notices[0].at);
        let lines = draw(&state, SIZE);

        let error = row_of(&lines, "E 切り替えられません");
        let info = row_of(&lines, "I fetch しました");
        assert!(error < info, "新しい順ではありません: {lines:#?}");
        // **日付まで出す**（ステータスバーは `HH:MM:SS` だけ。07）。
        assert!(
            lines[usize::from(info)].contains(&format!("{at} I fetch")),
            "{lines:#?}"
        );
        assert!(row_is_reversed(&state, SIZE, "E 切り替えられません"));
        assert!(!row_is_reversed(&state, SIZE, "I fetch しました"));
    }

    #[test]
    fn the_notice_under_the_cursor_shows_its_whole_message_and_detail() {
        // **ステータスバーでは読めない argv と stderr が出るのはここだけ**（07）。
        // 2 行目以降の `message` も同じ。
        let mut state = log_of(&[
            (
                NoticeLevel::Error,
                "切り替えられません\ncommit か stash してください",
                Some("git switch feat/x\nerror: would be overwritten by checkout"),
            ),
            (NoticeLevel::Info, "fetch しました", None),
        ]);
        let before = draw(&state, SIZE).join("\n");
        assert!(!before.contains("git switch feat/x"), "{before}");

        let _ = update(&mut state, Msg::Key(Key::char('j')));
        let lines = draw(&state, SIZE);
        let text = lines.join("\n");

        assert!(row_is_reversed(&state, SIZE, "E 切り替えられません"));
        let second = row_of(&lines, "commit か stash してください");
        let argv = row_of(&lines, "git switch feat/x");
        let stderr = row_of(&lines, "error: would be overwritten");
        assert!(second < argv && argv < stderr, "{text}");
        // `message` と `detail` のあいだに空行を 1 つ。
        assert_eq!(argv, second + 2, "{text}");
    }

    #[test]
    fn a_tab_in_the_detail_keeps_its_indent() {
        // git は上書きされるファイルをタブで字下げして並べる。ratatui はタブを
        // 描かないので、展開しないと字下げが消える（07）。
        let state = log_of(&[(
            NoticeLevel::Error,
            "失敗しました",
            Some("error: would be overwritten by checkout:\n\tsrc/main.rs"),
        )]);

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("│    src/main.rs"), "{text}");
    }

    #[test]
    fn a_dismissed_notice_is_still_in_the_log() {
        // 下ろすのはステータスバーからだけ（01 の「通知」）。
        let mut state = log_of(&[(NoticeLevel::Warn, "変更がありません", None)]);
        state.dismiss_notice();
        assert_eq!(state.shown_notice(), None);

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("W 変更がありません"), "{text}");
    }

    #[test]
    fn an_empty_log_says_so() {
        let state = state_with_modal(Modal::NoticeLog { cursor: 0 });

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("通知はまだありません"), "{text}");
        assert!(text.contains("[Esc] 閉じる"), "{text}");
    }

    #[test]
    fn a_full_log_still_leaves_room_for_the_detail() {
        // 一覧を先に満たすと、100 件のときに中身が 0 行になる（07）。
        let names: Vec<String> = (0..100).map(|i| format!("通知 {i}")).collect();
        let mut notices: Vec<(NoticeLevel, &str, Option<&str>)> = names
            .iter()
            .map(|name| (NoticeLevel::Info, name.as_str(), None))
            .collect();
        notices.push((NoticeLevel::Error, "最新", Some("stderr の中身")));
        let state = log_of(&notices);

        let lines = draw(&state, SIZE);
        let text = lines.join("\n");

        assert!(text.contains("stderr の中身"), "{text}");
        // **一覧と中身で内側を半分ずつ分ける**（80x24 で 10 行ずつ。07）。
        let top = row_of(&lines, "E 最新");
        let rule = row_of(&lines, "│───");
        let hint = row_of(&lines, "[j/k] 選ぶ");
        assert_eq!(rule - top, 10, "{text}");
        assert_eq!(hint - rule - 1, 10, "{text}");
    }

    #[test]
    fn a_detail_too_long_for_the_box_ends_with_an_ellipsis() {
        // 黙って切ると、stderr の続きがあることが画面から判らない（07）。
        let detail: String = (0..40).map(|i| format!("stderr {i}\n")).collect();
        let state = log_of(&[(NoticeLevel::Error, "失敗しました", Some(&detail))]);

        let lines = draw(&state, SIZE);
        let text = lines.join("\n");

        // **fixture が境目を越えていること**を先に見る（`.claude/rules/testing.md`）。
        assert!(!text.contains("stderr 39"), "{text}");
        assert!(text.contains("stderr 0"), "{text}");
        let hint = row_of(&lines, "[j/k] 選ぶ");
        // `…` は中身の最下行。**箱が端末の高さに丸められると、案内の上の空行も
        // 中身に回る**（`modals::place` が割るのは本文と案内だけ）ので、案内の直上。
        assert!(
            lines[usize::from(hint) - 1]
                .trim_start_matches('│')
                .starts_with('…'),
            "{text}"
        );
    }

    #[test]
    fn a_detail_that_fits_has_no_ellipsis() {
        let state = log_of(&[(NoticeLevel::Error, "失敗しました", Some("stderr 0"))]);

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("stderr 0"), "{text}");
        assert!(!text.contains('…'), "{text}");
    }

    #[test]
    fn a_tiny_terminal_does_not_panic() {
        let state = log_of(&[(NoticeLevel::Error, "失敗しました", Some("a\nb\nc"))]);
        for (width, height) in [(80, 3), (10, 5), (1, 1)] {
            let _ = draw(&state, TermSize { width, height });
        }
    }
}
