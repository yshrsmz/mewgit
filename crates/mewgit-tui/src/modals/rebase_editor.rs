//! interactive rebase の plan を編むモーダルの描画。設計は `docs/design/07-ui.md` の
//! 「RebaseEditor」。
//!
//! **箱の中の割り方は `modals::picker` と共有する**（本文・一覧・窓の切り出し・
//! カーソル行の反転）。ここに残るのは本文と行の組み立てだけ。

use mewgit_core::git_exec::{RebaseAction, RebaseEntry, RebasePlan};
use mewgit_core::git_read::{short_oid, Oid};
use ratatui::layout::Rect;
use ratatui::text::{Line, Text};
use ratatui::Frame;

use crate::modals::picker::{self, Picker};
use crate::ui::theme;

const TITLE: &str = "interactive rebase";

/// 動作を変えるキーの凡例。**本文に置く** —— 案内の行に並べると箱の幅（`modals::WIDTH`）を
/// 超える。`update::rebase::rebase_editor_key` が受けるキーと揃える。
const LEGEND: &str = "p:pick e:edit s:squash f:fixup d:drop  J/K:入れ替え";

/// キーの案内。`update::rebase::rebase_editor_key` が受けるキーと揃える。
const HINT: &str = "[j/k] 選ぶ   [Enter] 実行   [Esc] やめる";

/// 動作の桁。**最長の `squash` / `reword` に 2 桁足す**（Oid の頭を揃える）。
const ACTION_CELLS: usize = "squash".len() + 2;

/// plan を古い順に並べる。**通らない plan は理由を本文の下に赤で出す**（`Modal::RebaseEditor`
/// の doc。07）。
pub fn render(frame: &mut Frame, area: Rect, onto: &Oid, plan: &RebasePlan, cursor: usize) {
    let mut body = Text::from(vec![
        Line::from(format!(
            "上から順に {} の上へ積み直します。",
            short_oid(onto)
        )),
        Line::styled(LEGEND, theme::muted()),
    ]);
    if let Err(err) = plan.validate() {
        body.push_line(Line::styled(err.to_string(), theme::removed()));
    }
    picker::render(
        frame,
        area,
        Picker {
            title: TITLE,
            body,
            rows: plan.entries.iter().map(row).collect(),
            cursor,
            hint: HINT,
        },
    );
}

/// 1 行。`pick    abc1234  summary`。**drop の行は弱く出す**（残らないコミット）。
fn row(entry: &RebaseEntry) -> Line<'static> {
    let line = Line::from(format!(
        "{:<ACTION_CELLS$}{}  {}",
        entry.action.keyword(),
        short_oid(&entry.oid),
        entry.summary
    ));
    if entry.action == RebaseAction::Drop {
        line.style(theme::muted())
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use crate::test_util::{draw, row_is_reversed, state_with_modal};
    use mewgit_core::app::{AppState, Modal, TermSize};
    use mewgit_core::git_exec::{RebaseAction, RebasePlan};
    use mewgit_core::git_read::short_oid;
    use mewgit_core::test_util::{fake_oid, rebase_entry};

    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    fn state_with_editor(actions: &[RebaseAction], cursor: usize) -> AppState {
        let entries = actions
            .iter()
            .enumerate()
            .map(|(i, action)| {
                let n = i as u8 + 1;
                rebase_entry(action.clone(), fake_oid(n), &format!("commit {n}"))
            })
            .collect();
        state_with_modal(Modal::RebaseEditor {
            onto: fake_oid(9),
            head: fake_oid(8),
            plan: RebasePlan { entries },
            cursor,
        })
    }

    #[test]
    fn the_editor_lists_each_row_with_its_action_and_says_where_it_lands() {
        let state = state_with_editor(&[RebaseAction::Pick, RebaseAction::Fixup], 0);

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("interactive rebase"), "{text}");
        assert!(
            text.contains(&format!("上から順に {} の上へ", short_oid(&fake_oid(9)))),
            "{text}"
        );
        assert!(
            text.contains("p:pick e:edit s:squash f:fixup d:drop"),
            "{text}"
        );
        assert!(
            text.contains(&format!("pick    {}  commit 1", short_oid(&fake_oid(1)))),
            "{text}"
        );
        assert!(
            text.contains(&format!("fixup   {}  commit 2", short_oid(&fake_oid(2)))),
            "{text}"
        );
        assert!(text.contains("[Enter] 実行"), "{text}");
        // 通る plan には理由の行が無い。
        assert!(!text.contains("最初のコミット"), "{text}");
    }

    #[test]
    fn a_plan_git_would_stop_on_says_why_in_the_editor() {
        let state = state_with_editor(&[RebaseAction::Squash, RebaseAction::Pick], 0);

        let text = draw(&state, SIZE).join("\n");

        assert!(
            text.contains("squash を最初のコミットにはできません"),
            "{text}"
        );
    }

    #[test]
    fn the_cursor_row_is_highlighted() {
        let state = state_with_editor(&[RebaseAction::Pick, RebaseAction::Pick], 1);

        assert!(row_is_reversed(&state, SIZE, "commit 2"));
        assert!(!row_is_reversed(&state, SIZE, "commit 1"));
    }
}
