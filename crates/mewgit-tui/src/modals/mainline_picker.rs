//! merge コミットを revert するときに残す側の親を選ぶモーダルの描画。設計は
//! `docs/design/07-ui.md` の「MainlinePicker モーダル」。
//!
//! **箱の中の割り方は `modals::picker` と共有する。** ここに残るのはタイトル・キーの
//! 案内・行の組み立てだけ。

use mewgit_core::app::ParentChoice;
use mewgit_core::git_read::{short_oid, Oid};
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use crate::modals;
use crate::modals::picker::{self, Picker};

/// 枠のタイトル。**選ぶのは残す側**だと言う —— git の `--mainline` は「本流」の番号で、
/// 取り消される側と取り違えると逆の変更が消える（01 の「revert の開始（`v`）」）。
const TITLE: &str = "revert: 残す側の親";

/// キーの案内。`update::picker_key` と `update::graph::mainline_picked` が受けるキーと揃える。
/// **`Enter` は確認へ進むだけ**で、まだ revert しない。
const HINT: modals::Hint<'static> = &[&["[j/k] 選ぶ", "[Enter] 確認へ", "[Esc] やめる"]];

/// merge コミットの親を並べ、残す側を選ばせる。
pub fn render(
    frame: &mut Frame,
    area: Rect,
    target: &Oid,
    summary: &str,
    parents: &[ParentChoice],
    cursor: usize,
) {
    let body = format!(
        "{} {} は merge コミットです。残す側の親を選ぶと、ほかの親から入った変更を取り消します。",
        short_oid(target),
        summary
    );
    picker::render(
        frame,
        area,
        Picker {
            title: TITLE,
            width: modals::WIDTH,
            body: body.into(),
            rows: parents
                .iter()
                .enumerate()
                .map(|(i, parent)| row(i, parent))
                .collect(),
            cursor,
            hint: HINT,
        },
    );
}

/// 1 行。`1  abc1234 summary`。**summary は読み込み済みの親だけ**（`ParentChoice::summary`）。
fn row(index: usize, parent: &ParentChoice) -> Line<'static> {
    let summary = parent
        .summary
        .as_deref()
        .map_or(String::new(), |s| format!(" {s}"));
    Line::from(format!(
        "{}  {}{summary}",
        index + 1,
        short_oid(&parent.oid)
    ))
}

#[cfg(test)]
mod tests {
    use crate::test_util::{draw, row_is_reversed, state_with_modal};
    use mewgit_core::app::{AppState, Modal, ParentChoice, TermSize};
    use mewgit_core::git_read::short_oid;
    use mewgit_core::test_util::fake_oid;

    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    fn state_with_picker(cursor: usize) -> AppState {
        state_with_modal(Modal::MainlinePicker {
            target: fake_oid(9),
            summary: "Merge branch 'topic'".to_string(),
            parents: vec![
                ParentChoice {
                    oid: fake_oid(1),
                    summary: Some("main の最後".to_string()),
                },
                ParentChoice {
                    oid: fake_oid(2),
                    summary: None,
                },
            ],
            cursor,
        })
    }

    #[test]
    fn the_picker_says_which_side_is_kept_and_lists_every_parent() {
        let state = state_with_picker(0);

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("残す側の親"), "{text}");
        assert!(text.contains("Merge branch 'topic'"), "{text}");
        assert!(
            text.contains(&format!("1  {} main の最後", short_oid(&fake_oid(1)))),
            "{text}"
        );
        // 読み込んでいない親は oid だけ。
        assert!(
            text.contains(&format!("2  {}", short_oid(&fake_oid(2)))),
            "{text}"
        );
        assert!(text.contains("[Enter] 確認へ"), "{text}");
        assert!(text.contains("[Esc] やめる"), "{text}");
    }

    #[test]
    fn the_cursor_row_is_highlighted() {
        let state = state_with_picker(1);

        assert!(
            row_is_reversed(&state, SIZE, &format!("2  {}", short_oid(&fake_oid(2)))),
            "カーソル行が反転していない"
        );
        assert!(
            !row_is_reversed(&state, SIZE, "main の最後"),
            "選んでいない行が反転している"
        );
    }
}
