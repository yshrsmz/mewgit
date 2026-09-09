//! リモート選択モーダルの描画。設計は `docs/design/07-ui.md` の
//! 「RemotePicker モーダル」。

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::Frame;

use crate::modals;
use crate::ui::{self, theme};

/// 枠のタイトル。**何を選ぶのかを出す** —— 「リモート」だけだと、fetch なのか
/// push なのかが判らない（開く経路は `f` 1 つ。01）。
const TITLE: &str = "fetch するリモート";

/// キーの案内。`update::remote_picker_key` が受けるキーと揃える。
const HINT: &str = "[j/k] 選ぶ   [Enter] fetch   [Esc] やめる";

/// 理由と一覧のあいだに空ける行数。
///
/// **`modals::GAP_ROWS` とは別のもの**（あちらは本文とキーの案内のあいだ）。
const REASON_GAP_ROWS: u16 = 1;

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
        reason_rows.saturating_add(REASON_GAP_ROWS + rows),
        HINT,
    );

    // **一覧に先に行を渡す。** 理由を `Length` にすると、`modals::place` が箱を端末の
    // 高さに丸めたときに**一覧が 0 行に潰れる** —— リモート名が 1 つも出ないのに
    // `j` と `Enter` は効くので、見えていない相手を fetch する。`ui::render_rows` が
    // 守るのは「カーソルが一覧の外へ出ない」ことで、**一覧そのものが消えることは
    // 守らない**（`/code-review` が拾った）。理由の文言はリモート名を並べるので、
    // 本数が増えるほど長くなる = 潰れやすくなる。
    //
    // **空行は理由の枠に含める。** 3 本に割って真ん中を捨てると、捨てる束縛と
    // 制約が 1 つずつ増えるだけで結果は同じ（`Paragraph` は上詰め）。
    let [reason_area, list] = Layout::vertical([
        Constraint::Max(reason_rows.saturating_add(REASON_GAP_ROWS)),
        Constraint::Min(1),
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
    // **`ui::render_rows` を通す。** 素の `Paragraph` で描くと、枠に入りきらない
    // 本数のときにカーソル行が画面の外へ出たまま `j` で動き続け、**見えていない
    // リモートを `Enter` で fetch する** —— このモーダルが防ぐために在る
    // 「黙って選ばない」そのもの。ペインの一覧が 4 つともここを通っている。
    ui::render_rows(frame, list, lines, cursor..cursor + 1);
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
    fn the_cursor_row_stays_visible_when_the_list_is_taller_than_the_box() {
        // **窓を切り出さずに描くと、カーソルが枠の外へ出たまま `j` で動き続け、
        // 見えていないリモートを `Enter` で fetch する。** 既存の 4 本はどちらでも
        // 通るので、この見張りが無いと素の `Paragraph` に戻せてしまう。
        let remotes: Vec<String> = (0..30).map(|i| format!("remote{i:02}")).collect();
        let state = state_with_modal(Modal::RemotePicker {
            reason: "origin がありません".to_string(),
            remotes,
            cursor: 29,
        });

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("remote29"), "{text}");
    }

    #[test]
    fn a_long_reason_never_squeezes_the_list_to_nothing() {
        // **`a_wrapped_reason_does_not_push_the_remotes_out_of_the_box` の狭い版。**
        // あちらは 80x24 に 4 行の理由なので、一覧が 0 行に潰れる形を踏まない。
        // 理由はリモート名を並べるので**本数が増えるほど長くなる**。実機で測った境目は
        // 30 本 × 80x14（直す前は一覧が **0 行**、直したあとは 1 行）。**8 本では
        // 同じ 80x14 でも 5 行出るので踏めない** —— 本数と高さの両方を効かせる。
        let remotes: Vec<String> = (0..30)
            .map(|i| format!("very-long-remote-name-{i:02}"))
            .collect();
        let reason = format!("origin がありません（{}）。", remotes.join(", "));
        let state = state_with_modal(Modal::RemotePicker {
            reason,
            remotes,
            cursor: 29,
        });

        let text = draw(
            &state,
            TermSize {
                width: 80,
                height: 14,
            },
        )
        .join("\n");

        // カーソルが指している相手が画面に出ていること。1 つも出ないまま
        // `Enter` が効く状態を作らない。
        assert!(text.contains("very-long-remote-name-29"), "{text}");
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
