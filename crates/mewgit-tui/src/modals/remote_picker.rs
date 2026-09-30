//! リモート選択モーダルの描画。設計は `docs/design/07-ui.md` の
//! 「RemotePicker モーダル」。
//!
//! **箱の中の割り方は `modals::picker` と共有する**（2 人目の `AccountPicker` が
//! 出た時点で寄せた）。ここに残るのはタイトル・キーの案内・行の組み立てだけ。

use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use crate::modals;
use crate::modals::picker::{self, Picker};

/// 枠のタイトル。**何を選ぶのかを出す** —— 「リモート」だけだと、fetch なのか
/// push なのかが判らない（開く経路は `f` 1 つ。01）。
const TITLE: &str = "fetch するリモート";

/// キーの案内。`update::picker_key` と `update::remote_picked` が受けるキーと揃える。
const HINT: &str = "[j/k] 選ぶ   [Enter] fetch   [Esc] やめる";

/// 一覧から fetch するリモートを選ばせる。
pub fn render(frame: &mut Frame, area: Rect, reason: &str, remotes: &[String], cursor: usize) {
    picker::render(
        frame,
        area,
        Picker {
            title: TITLE,
            width: modals::WIDTH,
            // 1 行目は**なぜ選ばされているか**。これが無いと、毎回出るのか設定が
            // 壊れているのかが判らない（07）。
            body: reason.into(),
            rows: remotes.iter().map(|r| Line::from(r.as_str())).collect(),
            cursor,
            hint: HINT,
        },
    );
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
