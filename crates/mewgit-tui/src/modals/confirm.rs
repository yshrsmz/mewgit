//! 確認モーダルの描画。設計は `docs/design/07-ui.md` の「モーダル」。

use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::ui;

/// キーの案内。`update::confirm_key` が受けるキーと揃える。
const HINT: &str = "[y] 実行   [n] やめる";

/// モーダルの幅。狭い端末では画面の幅まで縮む。
const WIDTH: u16 = 60;

/// 本文とキーの案内のあいだに空ける行数。
const GAP_ROWS: u16 = 1;

/// キーの案内に取る行数。高さの見積もりと `Layout` で同じものを使う。
const HINT_ROWS: u16 = 1;

/// 破壊的操作の確認を描く。
///
/// 下のペインを `Clear` で消してから描く。覆わずに重ねると下の文字が透けて読めなくなる。
pub fn render(frame: &mut Frame, area: Rect, title: &str, body: &str) {
    let width = WIDTH.min(area.width);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title.to_string())
        .border_style(Style::default().add_modifier(Modifier::BOLD));

    // **枠が食う幅と行数は `Block` 自身から出す。** `- 2` / `+ 2` と手で書くと、
    // あとで `Padding` を足したときに折り返し幅だけが古いまま残り、本文の末尾が
    // 黙って切れる（`contains` を見るテストは通ってしまう）。高さはこれから
    // 決めるので、幅だけ本物の当て木で測る。
    let probe = Rect::new(0, 0, width, u16::MAX);
    let probe_inner = block.inner(probe);
    let border_rows = probe.height - probe_inner.height;

    let (text, body_rows) = ui::wrapped(body, probe_inner.width);
    // 本文 + 空行 + キーの案内 + 枠。画面に収まらない分は `centered` が削る。
    let height = body_rows.saturating_add(GAP_ROWS + HINT_ROWS + border_rows);
    let area = centered(area, width, height);
    let inner = block.inner(area);

    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    // **キーの案内は本文とは別に、最下行へ描く。** 1 つの `Paragraph` にまとめると、
    // 折り返しで本文が伸びたときに案内だけが枠の外へ押し出される。
    let [body_area, hint_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(HINT_ROWS)]).areas(inner);

    frame.render_widget(text, body_area);
    frame.render_widget(Paragraph::new(Line::from(HINT)), hint_area);
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
    fn a_body_wrapped_at_word_boundaries_keeps_its_last_line() {
        // 幅で割った切り上げは、単語境界で折れて増えたぶんを数え落とす。1 行ぶんは
        // 枠に取った空行が吸うので、2 行足りない本文でないと踏めない。題材は
        // `ui::wrapped` のテストと同じ 30 桁の語 3 つ。
        let a = "a".repeat(30);
        let b = "b".repeat(30);
        let c = "c".repeat(30);
        let tail = format!("{}MARKER", "d".repeat(24));
        let body = format!("{a} {b} {c}\n{a} {b} {tail}");
        let state = state_with_confirm("変更を破棄", &body);

        let lines = draw(&state, SIZE);
        let text = lines.join("\n");

        // 末尾が枠の中に、キーの案内より上にあること。`contains` だけだと、
        // 案内を押し出しただけの状態も通る。
        assert!(text.contains("MARKER"), "{text}");
        assert!(
            row_of(&lines, "MARKER") < row_of(&lines, "[y] 実行"),
            "{text}"
        );
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
