//! 確認モーダルの描画。設計は `docs/design/07-ui.md` の「モーダル」。

use ratatui::layout::Rect;
use ratatui::Frame;

use crate::modals;
use crate::ui;

/// キーの案内。`update::confirm_key` が受けるキーと揃える。
const HINT: &str = "[y] 実行   [n] やめる";

/// モーダルの幅。狭い端末では画面の幅まで縮む。
const WIDTH: u16 = 60;

/// 破壊的操作の確認を描く。
pub fn render(frame: &mut Frame, area: Rect, title: &str, body: &str) {
    let block = modals::block(title);
    // **折り返してからでないと高さが決まらない。** `桁数 / 幅` の切り上げで見積もると
    // 単語境界で折れて増えたぶんを数え落とし、本文の末尾が黙って切れる（07）。
    let (text, body_rows) = ui::wrapped(body, modals::inner_width(area, WIDTH, &block));

    let body_area = modals::place(frame, area, block, WIDTH, body_rows, HINT);
    frame.render_widget(text, body_area);
}

#[cfg(test)]
mod tests {
    use crate::test_util::{draw, row_of, state_with_modal};
    use mewgit_core::app::{Cmd, Modal, TermSize};
    use mewgit_core::git_exec::GitOp;

    /// 既定の 80x24。
    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    /// 確認モーダルを出した状態。
    fn state_with_confirm(title: &str, body: &str) -> mewgit_core::app::AppState {
        state_with_modal(Modal::Confirm {
            title: title.to_string(),
            body: body.to_string(),
            on_yes: Cmd::None,
        })
    }

    #[test]
    fn the_confirmation_shows_the_title_body_and_keys() {
        let state = state_with_modal(Modal::Confirm {
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

    /// 枠（`┌確認`）から、**その下で最初に見つかる** `└` までの行数。
    ///
    /// 先頭から `└` を探さない。モーダルの下に描かれているペインの枠も `└` を持つので、
    /// ペインが増えると先に当たる（Branches ペインが入って実際に当たった）。
    fn height_of(body: &str) -> u16 {
        let state = state_with_confirm("確認", body);
        let lines = draw(&state, SIZE);
        let top = row_of(&lines, "┌確認");
        let bottom = lines[top as usize..]
            .iter()
            .position(|line| line.contains('└'))
            .expect("枠の底がありません") as u16;
        bottom + 1
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
