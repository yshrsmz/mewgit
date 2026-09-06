//! 確認モーダルの描画。設計は `docs/design/07-ui.md` の「モーダル」。

use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

/// キーの案内。`update::confirm_key` が受けるキーと揃える。
const HINT: &str = "[y] 実行   [n] やめる";

/// モーダルの幅。狭い端末では画面の幅まで縮む。
const WIDTH: u16 = 60;

/// 破壊的操作の確認を描く。
///
/// 下のペインを `Clear` で消してから描く。覆わずに重ねると下の文字が透けて読めなくなる。
pub fn render(frame: &mut Frame, area: Rect, title: &str, body: &str) {
    let width = WIDTH.min(area.width);
    // 本文 + 空行 + キーの案内 + 枠の上下。画面に収まらない分は `centered` が削る。
    let height = rows(body, width.saturating_sub(2)) + 4;
    let area = centered(area, width, height);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title.to_string())
        .border_style(Style::default().add_modifier(Modifier::BOLD));
    let inner = block.inner(area);

    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    // **キーの案内は本文とは別に、最下行へ描く。** 1 つの `Paragraph` にまとめると、
    // 折り返しで本文が伸びたときに案内だけが枠の外へ押し出される。
    let [body_area, hint_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);

    let lines: Vec<Line> = body.lines().map(Line::raw).collect();
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body_area);
    frame.render_widget(Paragraph::new(Line::from(HINT)), hint_area);
}

/// `body` が `width` 桁で何行になるか。
///
/// ratatui の折り返し行数（`Paragraph::line_count`）は unstable feature の後ろに
/// あるので、表示幅から見積もる。単語の途中で折れないぶん実際はもう 1 行増える
/// ことがあるが、本文は下の空行を食うだけで、案内の行は上の分割で守られている。
fn rows(body: &str, width: u16) -> u16 {
    if width == 0 {
        return 0;
    }
    body.lines()
        .map(|line| {
            let cells = Line::raw(line).width() as u16;
            cells.div_ceil(width).max(1)
        })
        .sum()
}

/// 画面の中央に `width` x `height` を取る。画面が小さければそのぶん縮める。
///
/// 中央寄せの算術は書かず ratatui の [`Flex::Center`] に任せる。`Max` にしているのは、
/// 端末が小さいときに縮めるため（`Length` だとはみ出す）。
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [area] = Layout::horizontal([Constraint::Max(width)])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Max(height)])
        .flex(Flex::Center)
        .areas(area);
    area
}

#[cfg(test)]
mod tests {
    use crate::test_util::{draw, row_of, state_with_commits};
    use mewgit_core::app::{Cmd, Modal, TermSize};
    use mewgit_core::git_exec::GitOp;

    /// 既定の 80x24。
    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    /// 確認モーダルを出した状態。
    fn state_with_confirm(title: &str, body: &str) -> mewgit_core::app::AppState {
        let mut state = state_with_commits(&["first", "second"]);
        state.modal = Some(Modal::Confirm {
            title: title.to_string(),
            body: body.to_string(),
            on_yes: Cmd::None,
        });
        state
    }

    #[test]
    fn the_confirmation_shows_the_title_body_and_keys() {
        let mut state = state_with_commits(&["first", "second"]);
        state.modal = Some(Modal::Confirm {
            title: "変更を破棄".to_string(),
            body: "a.txt を HEAD の状態に戻します。".to_string(),
            on_yes: Cmd::Exec(GitOp::Discard(vec!["a.txt".into()])),
        });

        let lines = draw(&state, SIZE);
        let text = lines.join("\n");

        assert!(text.contains("変更を破棄"), "{text}");
        // 対象をそのまま出す。「選択中のファイル」では何を消すか判らない（07）。
        assert!(text.contains("a.txt"), "{text}");
        assert!(text.contains("[y] 実行"), "{text}");
        assert!(text.contains("[n] やめる"), "{text}");
    }

    #[test]
    fn a_wrapped_body_does_not_push_the_keys_out_of_the_box() {
        // 本文とキーの案内を 1 つの Paragraph にまとめると、折り返した分だけ
        // 案内が枠の外へ出る。押せるキーが見えないと確認にならない。
        let state = state_with_confirm(
            "変更を破棄",
            "とても長いパスの一覧 aaaaaaaaaaaaaaaaaaaa/bbbbbbbbbbbbbbbbbbbb/cccccccccccccccccccc.txt を HEAD の状態に戻します。\nstaged の内容も一緒に戻ります。元に戻せません。",
        );

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("[y] 実行"), "{text}");
        assert!(text.contains("[n] やめる"), "{text}");
    }

    #[test]
    fn the_box_grows_with_the_body() {
        // 高さを固定にすると短い確認で下半分が空く。
        let short = height_of("1 行");
        let long = height_of("1 行目\n2 行目\n3 行目");
        assert!(long > short, "{long} <= {short}");
    }

    /// 枠（`┌`）から `└` までの行数。
    fn height_of(body: &str) -> u16 {
        let state = state_with_confirm("確認", body);
        let lines = draw(&state, SIZE);
        row_of(&lines, "└") - row_of(&lines, "┌確認") + 1
    }

    #[test]
    fn the_confirmation_covers_what_is_under_it() {
        // 覆わずに重ねると下の文字が透けて読めなくなる。
        let state = state_with_confirm("確認", "本文");

        let lines = draw(&state, SIZE);
        let body_row = &lines[row_of(&lines, "本文") as usize];

        assert!(!body_row.contains("first"), "{body_row}");
        assert!(!body_row.contains("second"), "{body_row}");
    }

    #[test]
    fn a_tiny_terminal_does_not_panic() {
        // リサイズの途中では枠より小さい端末が来る。中央寄せが縮められること。
        for (width, height) in [(1, 1), (10, 3), (20, 5)] {
            let state = state_with_confirm("確認", "本文");
            let lines = draw(&state, TermSize { width, height });
            assert_eq!(lines.len(), height as usize);
        }
    }
}
