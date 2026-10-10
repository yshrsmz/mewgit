//! 通知の履歴モーダルの描画。設計は `docs/design/07-ui.md` の「NoticeLog モーダル」。
//!
//! **ステータスバーでは読めないものを出す場所**（01 の「通知」）—— 日付と、
//! `Notice.detail`（argv と git の出力）と、`message` の 2 行目以降。
//!
//! **`modals::picker` を使わない。** あちらは「本文 → 一覧」の順で、本文を先に測って
//! 一覧に残りを渡す。ここは「一覧 → カーソル行の中身」で、中身がカーソルで入れ替わる
//! うえ、長さに上限が無い（stderr）。一覧に取る行数を先に決めて中身に残りを渡すので、
//! 割り方が逆になる。

use mewgit_core::app::{AppState, Notice, NoticeLevel};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::modals;
use crate::ui::{self, theme, time};

const TITLE: &str = "通知の履歴";

/// キーの案内。`update::picker_key` が受けるキーと揃える（`Enter` は何もしない）。
const HINT: modals::Hint<'static> = &[&["[j/k] 選ぶ", "[Esc] 閉じる"]];

/// 箱の幅。**`modals::WIDTH`（60）より広く取る** —— stderr は英語の長い行で、
/// 60 では 1 行が 2〜3 行に折れる。狭い端末では画面の幅まで縮む。
const WIDTH: u16 = 100;

/// 通知が 1 件も無いときの本文。
const EMPTY: &str = "通知はまだありません";

/// 一覧と中身の区切りに取る行数。
const RULE_ROWS: u16 = 1;

/// 通知の履歴を描く。一覧は新しい順で、`selected` の通知を反転して中身を下に出す
/// （`Modal::NoticeLog`）。
pub fn render(frame: &mut Frame, area: Rect, state: &AppState, selected: u64) {
    let block = modals::block(TITLE);
    let width = modals::inner_width(area, WIDTH, &block);

    // **描く行は core が決める**（`AppState::notice_log_row`）。`j` / `k` も同じ 1 本を
    // 読むので、捨てられた通知を選んでいたときの寄せ方が食い違わない。
    let Some(cursor) = state.notice_log_row(selected) else {
        let (text, rows) = ui::wrapped(EMPTY, width);
        let inner = modals::place(frame, area, block, WIDTH, rows, HINT);
        frame.render_widget(text, inner);
        return;
    };
    let newest_first = || state.notices.iter().rev();

    let mut rows: Vec<Line<'_>> = newest_first().map(|n| row(n, width)).collect();
    let chosen = newest_first()
        .nth(cursor)
        .expect("notice_log_row は一覧の中を返す");
    // **中身は画面に入るぶん + 1 行で打ち切る**（07 の「見えない行の組み立て」）。
    // 箱は端末の高さで頭打ちになるので、それを越えたかが判れば `…` も箱の高さも決まる。
    let (content, content_rows) = ui::wrapped(content(chosen, usize::from(area.height) + 1), width);

    let inner = modals::place(
        frame,
        area,
        block,
        WIDTH,
        u16::try_from(rows.len())
            .unwrap_or(u16::MAX)
            .saturating_add(RULE_ROWS)
            .saturating_add(content_rows),
        HINT,
    );

    // **一覧に取るのは内側の半分まで**（ペインの一覧と同じ規則）。一覧を先に満たすと、
    // 100 件のときに中身が 0 行になり、開いた理由（`detail` を読む）が消える（07）。
    // 高さ 1 でも一覧は 1 行残す（カーソル行が見えないまま `j` が効くのを避ける）。
    let [list, rule, body] = Layout::vertical([
        Constraint::Length(ui::capped_rows(rows.len(), inner.height).max(1)),
        Constraint::Length(RULE_ROWS),
        Constraint::Min(0),
    ])
    .areas(inner);

    // カーソル行はペインと同じ反転（`theme::selected`）。
    if let Some(line) = rows.get_mut(cursor) {
        line.style = theme::selected();
    }
    ui::render_built(frame, list, rows, cursor..cursor + 1);

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
    let lead = vec![
        Span::styled(format!("{} ", time::format(notice.at)), theme::muted()),
        Span::styled(format!("{mark} "), style),
    ];
    ui::lead_line(lead, &notice.message, width)
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

/// カーソル行の中身 —— `message` の全文、空行、`detail` の全文。ただし先頭の
/// `max_lines` 行まで。
///
/// **`message` を 1 行目で切らない。** toml のパースエラーのように複数行の案内がある
/// （ステータスバーが 1 行目だけを描くのはこのため。07）。**タブは開く**
/// （`ui::push_expanded`）—— git の stderr は上書きされるファイルをタブで字下げする。
///
/// **`max_lines` で打ち切る。** `detail` は長さに上限が無い（hook の出力をそのまま
/// 運ぶ）ので、全文を組んで折り返しを測ると 5000 行で 1 フレーム 1.78ms かかった（打ち切って 133µs。07）。
/// 元の 1 行は折り返して 1 行以上になるので、元の行で数えて打ち切っても「描ける行を
/// 越えたか」の判定は変わらない。
fn content(notice: &Notice, max_lines: usize) -> Text<'static> {
    let detail = notice
        .detail
        .as_deref()
        .into_iter()
        .flat_map(|detail| std::iter::once("").chain(detail.lines()));
    let mut out = String::new();
    for (index, line) in notice
        .message
        .lines()
        .chain(detail)
        .take(max_lines)
        .enumerate()
    {
        if index > 0 {
            out.push('\n');
        }
        ui::push_expanded(&mut out, line);
    }
    Text::from(out)
}

#[cfg(test)]
mod tests {
    use crate::test_util::{draw, modal_text, row_is_reversed, row_of, state_at};
    use crate::ui::time;
    use mewgit_core::app::{update, AppState, Key, Msg, NoticeLevel, TermSize};

    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    /// 通知を `(重さ, message, detail)` の順に積み、`L` で履歴を開いた状態（最後が最新）。
    fn log_of(notices: &[(NoticeLevel, &str, Option<&str>)]) -> AppState {
        let mut state = state_at("/repo");
        for (level, message, detail) in notices {
            state.push_notice(*level, (*message).to_string(), detail.map(str::to_string));
        }
        let _ = update(&mut state, Msg::Key(Key::char('L')));
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
        // **ステータスバーでは読めない argv と git の出力が出るのはここだけ**（07）。
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

        assert!(text.contains("│        src/main.rs"), "{text}");
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
        let state = log_of(&[]);

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("通知はまだありません"), "{text}");
        assert!(text.contains("[Esc] 閉じる"), "{text}");
    }

    #[test]
    fn a_full_log_still_leaves_room_for_the_detail() {
        // 一覧を先に満たすと、100 件のときに中身が 0 行になる（07）。
        let mut state = state_at("/repo");
        for i in 0..99 {
            state.push_notice(NoticeLevel::Info, format!("通知 {i}"), None);
        }
        state.push_notice(
            NoticeLevel::Error,
            "最新".to_string(),
            Some("stderr の中身".to_string()),
        );
        let _ = update(&mut state, Msg::Key(Key::char('L')));

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
        // 収まる detail には付けない —— 否定だけだと「`…` を一切付けない」実装でも
        // 通るので、同じ 1 本で付く側まで見る。
        let fits = log_of(&[(NoticeLevel::Error, "失敗しました", Some("stderr 0"))]);
        let text = draw(&fits, SIZE).join("\n");
        assert!(text.contains("stderr 0"), "{text}");
        // **箱の内側の行頭だけを見る。** 下のペインの「読み込み中…」も `…` を持つ。
        let marked = modal_text(&fits, SIZE)
            .lines()
            .any(|line| line.starts_with('…'));
        assert!(!marked, "{text}");

        let detail: String = (0..40).map(|i| format!("stderr {i}\n")).collect();
        let state = log_of(&[(NoticeLevel::Error, "失敗しました", Some(&detail))]);

        let lines = draw(&state, SIZE);
        let text = lines.join("\n");

        // **fixture が境目を越えていること**を先に見る（`.claude/rules/testing.md`）。
        assert!(!text.contains("stderr 39"), "{text}");
        assert!(text.contains("stderr 0"), "{text}");
        let inner: Vec<String> = modal_text(&state, SIZE).lines().map(String::from).collect();
        let hint = usize::from(row_of(&inner, "[j/k] 選ぶ"));
        // `…` は中身の最下行。**箱が端末の高さに丸められると、案内の上の空行も
        // 中身に回る**（`modals::place` が割るのは本文と案内だけ）ので、案内の直上。
        assert!(inner[hint - 1].starts_with('…'), "{text}");
    }

    #[test]
    fn the_content_is_cut_at_the_lines_it_can_show() {
        // **全文を組まない**（07 の「見えない行の組み立て」）。`detail` は長さに上限が
        // 無いので、描ける行 + 1 を越えたぶんは組む前に落とす。
        let mut state = state_at("/repo");
        let detail: String = (0..5000).map(|i| format!("stderr {i}\n")).collect();
        state.push_notice(NoticeLevel::Error, "失敗しました".to_string(), Some(detail));
        let notice = state.notices.back().expect("積んだ");

        let text = super::content(notice, 4);

        // message、空行、detail の 2 行。
        assert_eq!(text.lines.len(), 4, "{text:?}");
        assert_eq!(text.lines[3].to_string(), "stderr 1");
    }

    #[test]
    fn a_tiny_terminal_does_not_panic() {
        let state = log_of(&[(NoticeLevel::Error, "失敗しました", Some("a\nb\nc"))]);
        for (width, height) in [(80, 3), (10, 5), (1, 1)] {
            let _ = draw(&state, TermSize { width, height });
        }
    }
}
