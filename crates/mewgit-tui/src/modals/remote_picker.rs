//! リモート選択モーダルの描画。設計は `docs/design/07-ui.md` の
//! 「RemotePicker モーダル」。

use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::modals;
use crate::ui::{self, theme};

/// 枠のタイトル。**何を選ぶのかを出す** —— 「リモート」だけだと、fetch なのか
/// push なのかが判らない（開く経路は `f` 1 つ。01）。
const TITLE: &str = "fetch するリモート";

/// キーの案内。`update::remote_picker_key` が受けるキーと揃える。
const HINT: &str = "[j/k] 選ぶ   [Enter] fetch   [Esc] やめる";

/// 理由と一覧のあいだに空ける行数。
const GAP_ROWS: u16 = 1;

/// 一覧から fetch するリモートを選ばせる。
pub fn render(frame: &mut Frame, area: Rect, reason: &str, remotes: &[String], cursor: usize) {
    let block = modals::block(TITLE);
    // **理由は折り返しうる。** 桁数を幅で割る見積もりだと単語境界で折れて増えたぶんを
    // 数え落とし、末尾が黙って切れる（`Confirm` と同じ理由。07）。
    let width = modals::inner_width(area, modals::WIDTH, &block);
    let (reason_text, reason_rows) = ui::wrapped(reason, width);

    // 一覧は折り返さない。リモート名は 1 行に収まる（収まらない名前は端で切れる）。
    let rows = u16::try_from(remotes.len()).unwrap_or(u16::MAX);
    let body = modals::place(
        frame,
        area,
        block,
        modals::WIDTH,
        reason_rows.saturating_add(GAP_ROWS + rows),
        HINT,
    );

    let [reason_area, _gap, list] = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Length(reason_rows),
        ratatui::layout::Constraint::Length(GAP_ROWS),
        ratatui::layout::Constraint::Min(0),
    ])
    .areas(body);

    frame.render_widget(reason_text, reason_area);
    // カーソル行はペインと同じ反転（`theme::selected`）。一覧から 1 つ選ぶ形が
    // 同じなので、見た目も揃える。
    let lines: Vec<Line> = remotes
        .iter()
        .enumerate()
        .map(|(index, remote)| {
            let line = Line::from(remote.as_str());
            if index == cursor {
                line.style(theme::selected())
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), list);
}

#[cfg(test)]
mod tests {
    use crate::test_util::{draw, row_is_reversed, row_of, state_with_modal};
    use mewgit_core::app::{Modal, TermSize};

    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    fn state_with_picker(
        reason: &str,
        remotes: &[&str],
        cursor: usize,
    ) -> mewgit_core::app::AppState {
        state_with_modal(Modal::RemotePicker {
            reason: reason.to_string(),
            remotes: remotes.iter().map(|r| (*r).to_string()).collect(),
            cursor,
        })
    }

    #[test]
    fn the_picker_shows_the_reason_the_remotes_and_the_keys() {
        let state = state_with_picker(
            "origin がありません（fork, upstream）。追跡先を設定してください",
            &["fork", "upstream"],
            0,
        );

        let text = draw(&state, SIZE).join("\n");

        // **なぜ選ばされているか**が無いと、毎回出るのか設定が壊れているのかが
        // 判らない（07）。
        assert!(text.contains("origin がありません"), "{text}");
        assert!(text.contains("fork"), "{text}");
        assert!(text.contains("upstream"), "{text}");
        assert!(text.contains("[Enter] fetch"), "{text}");
        assert!(text.contains("[Esc] やめる"), "{text}");
    }

    #[test]
    fn the_cursor_row_is_highlighted() {
        // ペインと同じ反転。どれを選んでいるかが出ないと `Enter` を押せない。
        let state = state_with_picker("理由", &["fork", "upstream"], 1);

        assert!(
            row_is_reversed(&state, SIZE, "upstream"),
            "カーソル行が反転していない"
        );
        assert!(
            !row_is_reversed(&state, SIZE, "fork"),
            "選んでいない行が反転している"
        );
    }

    #[test]
    fn a_wrapped_reason_does_not_push_the_remotes_out_of_the_box() {
        // 理由が折り返した分だけ一覧が枠の外へ出ると、選ぶ相手が見えない
        // （`Confirm` の「案内が押し出される」と同じ形。07）。
        let reason = format!("{}。追跡先を設定してください", "とても長い理由".repeat(12));
        let state = state_with_picker(&reason, &["fork", "upstream"], 0);

        let lines = draw(&state, SIZE);
        let text = lines.join("\n");

        assert!(text.contains("upstream"), "{text}");
        assert!(
            row_of(&lines, "upstream") < row_of(&lines, "[Enter] fetch"),
            "{text}"
        );
    }

    #[test]
    fn a_tiny_terminal_does_not_panic() {
        // リサイズの途中では枠より小さい端末が来る。
        for (width, height) in [(1, 1), (10, 3), (20, 5)] {
            let state = state_with_picker("理由", &["fork", "upstream"], 0);
            let lines = draw(&state, TermSize { width, height });
            assert_eq!(lines.len(), height as usize);
        }
    }
}
