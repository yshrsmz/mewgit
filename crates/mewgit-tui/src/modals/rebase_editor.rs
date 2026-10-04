//! interactive rebase の plan を編むモーダルの描画。設計は `docs/design/07-ui.md` の
//! 「RebaseEditor」。
//!
//! **箱の中の割り方は `modals::picker` と共有する**（本文・一覧・窓の切り出し・
//! カーソル行の反転）。ここに残るのは本文と行の組み立てだけ。

use mewgit_core::app::TextBuffer;
use mewgit_core::git_exec::{RebaseAction, RebaseEntry, RebasePlan};
use mewgit_core::git_read::{short_oid, Oid};
use ratatui::layout::Rect;
use ratatui::text::{Line, Text};
use ratatui::Frame;

use crate::modals::picker::{self, Picker};
use crate::modals::{self, commit_message};
use crate::ui::theme;

const TITLE: &str = "interactive rebase";

/// キーの案内。`update::rebase::rebase_editor_key` が受けるキーと揃える。
///
/// **全部ここに並べて 2 行に折る**（07 の「モーダルのキーの案内」）。1 行目が動作を変える
/// キーで、2 行目が選ぶ・入れ替える・閉じるキー。本文に凡例を置く形（#135）は、1 行に
/// 収まらなかったキーの置き場だった。
const HINT: modals::Hint<'static> = &[ACTION_KEYS, MOVE_KEYS];

/// 案内の 1 行目。動作を変えるキー。
const ACTION_KEYS: &[&str] = &[
    "[p] pick",
    "[r] reword",
    "[e] edit",
    "[s] squash",
    "[f] fixup",
    "[d] drop",
];

/// 案内の 2 行目。選ぶ・入れ替える・閉じるキー。
const MOVE_KEYS: &[&str] = &[
    "[j/k] 選ぶ",
    "[J/K] 入れ替え",
    "[Enter] 実行",
    "[Esc] やめる",
];

/// 箱の幅。**`CommitMessage` と同じ**（07）—— ほかのピッカーの `modals::WIDTH` では
/// 動作の 6 キーだけで案内の 1 行に収まらない。reword の入力も同じ箱で開くので、
/// 切り替えても幅が変わらない。
const WIDTH: u16 = commit_message::WIDTH;

/// reword の入力のキーの案内。`update::rebase::rebase_editor_key` が入力のあいだ
/// `update::multiline_input_key` に振り分けるキーと揃える。
///
/// **`Esc` は「一覧へ戻る」と書く**（ほかのモーダルの `Esc` は閉じる。01）。改行の
/// キーを 2 つ出す理由は `commit_message::HINT` と同じ。
const REWORD_HINT: modals::Hint<'static> = &[&[
    "[Enter] 決める",
    "[Shift+Enter / Ctrl-J] 改行",
    "[Esc] 一覧へ戻る",
]];

/// 動作の桁。**最長の `squash` / `reword` に 2 桁足す**（Oid の頭を揃える）。
const ACTION_CELLS: usize = "squash".len() + 2;

/// plan を古い順に並べる。**通らない plan は理由を本文の下に赤で出す**（`Modal::RebaseEditor`
/// の doc。07）。
pub fn render(frame: &mut Frame, area: Rect, onto: &Oid, plan: &RebasePlan, cursor: usize) {
    let mut body = Text::from(Line::from(format!(
        "上から順に {} の上へ積み直します。",
        short_oid(onto)
    )));
    if let Err(err) = plan.validate() {
        body.push_line(Line::styled(err.to_string(), theme::removed()));
    }
    picker::render(
        frame,
        area,
        Picker {
            title: TITLE,
            width: WIDTH,
            body,
            rows: plan.entries.iter().map(row).collect(),
            cursor,
            hint: HINT,
        },
    );
}

/// reword のメッセージを打つ入力。**一覧は隠す**（07）—— 入力は複数行で高さが要る。
/// 箱は `CommitMessage` と同じ（`commit_message::render_editor`）で、題に対象の短縮 Oid を出す。
pub fn render_reword(frame: &mut Frame, area: Rect, oid: &Oid, buf: &TextBuffer) {
    let title = format!("reword {}", short_oid(oid));
    commit_message::render_editor(frame, area, &title, REWORD_HINT, buf);
}

/// 1 行。`pick    abc1234  summary`。**drop の行は弱く出す**（残らないコミット）。
/// **reword の行は打ったメッセージの 1 行目を出す** —— 行は「実行したらどうなるか」を
/// 見せるので、元の件名のままだと決めたメッセージが画面のどこにも残らない（07）。
fn row(entry: &RebaseEntry) -> Line<'static> {
    let summary = match &entry.action {
        RebaseAction::Reword { message } => message.lines().next().unwrap_or_default(),
        _ => entry.summary.as_str(),
    };
    let line = Line::from(format!(
        "{:<ACTION_CELLS$}{}  {summary}",
        entry.action.keyword(),
        short_oid(&entry.oid),
    ));
    if entry.action == RebaseAction::Drop {
        line.style(theme::muted())
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use super::{HINT, REWORD_HINT};
    use crate::modals;
    use crate::test_util::{cursor, draw, row_is_reversed, row_of, state_with_modal};
    use mewgit_core::app::{AppState, Modal, RewordInput, TermSize, TextBuffer};
    use mewgit_core::git_exec::{RebaseAction, RebasePlan};
    use mewgit_core::git_read::short_oid;
    use mewgit_core::test_util::{fake_oid, rebase_entry, reword};

    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    fn state_with_editor(actions: &[RebaseAction], cursor: usize) -> AppState {
        editor_with_reword(actions, cursor, None)
    }

    /// `reword` が `Some` なら、カーソルの行の reword の入力を開いた状態。
    fn editor_with_reword(
        actions: &[RebaseAction],
        cursor: usize,
        reword: Option<&str>,
    ) -> AppState {
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
            reword: reword.map_or(RewordInput::Closed, |text| {
                RewordInput::Open(TextBuffer::from_text(text))
            }),
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
            text.contains(&format!("pick    {}  commit 1", short_oid(&fake_oid(1)))),
            "{text}"
        );
        assert!(
            text.contains(&format!("fixup   {}  commit 2", short_oid(&fake_oid(2)))),
            "{text}"
        );
        // 通る plan には理由の行が無い。
        assert!(!text.contains("最初のコミット"), "{text}");
    }

    #[test]
    fn every_key_is_in_the_hint_rows_and_each_row_fits_the_box() {
        // **キーは全部案内の行に並べ、本文に凡例を置かない**（07 の「モーダルのキーの
        // 案内」）。**行ごとに丸ごと 1 行に収まったか**を見る —— 溢れると行末から切れ、
        // 末尾の `[Esc]` や `[d] drop` が消える（`contains` で語を探すと、切れた
        // `[Esc] やめ` でも素通りする。`AccountPicker` の幅の見張りと同じ）。
        let state = state_with_editor(&[RebaseAction::Pick], 0);

        let rows = draw(&state, SIZE);

        for groups in HINT {
            let want = modals::hint_row(groups);
            let row = &rows[row_of(&rows, groups[0]) as usize];
            assert!(row.contains(&want), "案内が 1 行に収まっていない: {row}");
        }
        // 2 行目は 1 行目のすぐ下（案内は箱の底に続けて描く）。
        let first = row_of(&rows, "[p] pick");
        assert_eq!(row_of(&rows, "[j/k] 選ぶ"), first + 1);
        let text = rows.join("\n");
        assert!(!text.contains("p:pick"), "凡例が残っている: {text}");
    }

    #[test]
    fn a_reworded_row_shows_the_first_line_of_the_new_message() {
        let reworded = reword("新しい件名\n\n本文");
        let state = state_with_editor(&[RebaseAction::Pick, reworded], 0);

        let text = draw(&state, SIZE).join("\n");

        assert!(
            text.contains(&format!("reword  {}  新しい件名", short_oid(&fake_oid(2)))),
            "{text}"
        );
        // 行は 1 行。本文は一覧に出さない。
        assert!(!text.contains("本文"), "{text}");
    }

    #[test]
    fn the_reword_input_hides_the_list_and_shows_the_message_with_its_keys() {
        let state = editor_with_reword(
            &[RebaseAction::Pick, RebaseAction::Pick],
            1,
            Some("件名\n\n本文"),
        );

        let rows = draw(&state, SIZE);
        let text = rows.join("\n");

        assert!(
            text.contains(&format!("reword {}", short_oid(&fake_oid(2)))),
            "{text}"
        );
        assert!(text.contains("件名"), "{text}");
        assert!(text.contains("本文"), "{text}");
        // 一覧は隠す（07）。
        assert!(!text.contains("commit 1"), "{text}");
        // **`Esc` は閉じずに一覧へ戻る**ので、ほかのモーダルと違う語で出す（01）。
        let row = &rows[row_of(&rows, "[Enter]") as usize];
        assert!(
            row.contains(&modals::hint_row(REWORD_HINT[0])),
            "案内が 1 行に収まっていない: {row}"
        );
        // 端末カーソルは入力の末尾（`TextBuffer::from_text`）。
        assert_eq!(cursor(&state, SIZE).y, row_of(&rows, "本文"));
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
