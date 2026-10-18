//! Branches ペインのキー処理と一覧の受け取り（07 の「Branches ペイン」）。

use crate::git_exec::GitOp;
use crate::git_read::{short_oid, Oid};

use super::{confirm, focus, input};
use crate::app::keys::BranchesAction;
use crate::app::{AppState, Cmd, InputIntent, Pane, TextBuffer};

/// Branches ペインのキー処理。
pub(super) fn branches_key(state: &mut AppState, action: BranchesAction) -> Cmd {
    match action {
        BranchesAction::Checkout => switch_branch(state),
        BranchesAction::Delete => delete_branch(state, false),
        BranchesAction::ForceDelete => delete_branch(state, true),
        // **起点は HEAD。** このペインは起点の情報を持たない（カーソル行を起点に
        // すると、一覧を眺めているだけのつもりで別のブランチから生やすことになる）。
        // 起点を選んで作るのは Graph ペインの `n`（07 の「Branches ペイン」）。
        BranchesAction::Branch => open_branch_create(state, None),
        BranchesAction::Rename => rename_branch(state),
        // **`Tab` でしか入れないペインにも出口を用意する**（07）。戻り先を
        // `right_pane()` にしないのは、このペインが左の選択と無関係だから。
        BranchesAction::Back => focus(state, Pane::Graph),
    }
}

/// `Enter`。カーソルの指すブランチに切り替える。
///
/// **押せるかを手前で判定しない**（不変ルール 4。git の実文言は 07 の
/// 「Branches ペイン」にある）。手前で弾くと、同じ判定が mewgit と git の 2 箇所に載る。
///
/// **フォーカスは Branches に残す。** 切り替えたあと続けて別のブランチを選べる。
/// 一覧の読み直しは `update::Reload` が出す（`Switch` は `Reload::Refs`）。
fn switch_branch(state: &AppState) -> Cmd {
    // 一覧が空（読み込み中・失敗・コミットが 1 つも無いリポジトリ）。カーソル行が
    // 描かれていないので、通知を出す相手もいない。
    let Some(branch) = state.branches.selected() else {
        return Cmd::None;
    };
    Cmd::Exec(GitOp::Switch(branch.name.clone()))
}

/// `d` / `D`。カーソルの指すブランチを消す。破壊的なので確認モーダルを通す
/// （不変ルール 5）。`force` が `D`（`branch --delete --force`）。
///
/// **未マージかも、HEAD のブランチかも手前で判定しない**（[`switch_branch`] と同じ理由）。
///
/// **本文は `force` で変える。** 同じ文言だと、どちらのキーで開いた確認なのかが画面から
/// 判らない（ハンクとファイルの discard で本文を分けたのと同じ理由）。
fn delete_branch(state: &mut AppState, force: bool) -> Cmd {
    let Some(branch) = state.branches.selected() else {
        return Cmd::None;
    };
    let name = branch.name.clone();
    // **本文に「ブランチ」を重ねない。** 枠のタイトルが言っているうえ、80 桁の端末で
    // 名前の直後に折り返し、「ブランチ topic」「を削除します。…」と割れる（#22 の
    // 「幅を変える変更は既定サイズのままで見えるものを確かめる」）。
    let (title, body) = if force {
        (
            "ブランチを強制削除",
            format!("{name} を削除します。未マージのコミットも失われます。"),
        )
    } else {
        ("ブランチを削除", format!("{name} を削除します。"))
    };
    confirm(state, title, body, GitOp::BranchDelete { name, force })
}

/// `n`。ブランチを作る入力モーダルを開く（01 の「1 行入力」）。
///
/// `start` は起点。`None` なら HEAD で、そのときはタイトルに起点を書かない。
///
/// **一覧が空でも開く。** 作成はカーソル行を要らず、`git init` の直後（ブランチが
/// 1 本も無い）はまさに作りたい状態。`switch --create` は unborn HEAD でも通り、
/// `HEAD` を新しい名前に向け替える（git 2.55 で確認）。
pub(super) fn open_branch_create(state: &mut AppState, start: Option<Oid>) -> Cmd {
    // **タイトルに起点を出す。** どのコミットから生やすかは押す前に読めないといけない
    // （`Confirm` の本文に対象をそのまま出すのと同じ理由。07 の「Input モーダル」）。
    let title = match &start {
        Some(oid) => format!("ブランチを作成 ({} から)", short_oid(oid)),
        None => "ブランチを作成".to_string(),
    };
    input(
        state,
        title,
        "新しいブランチの名前".to_string(),
        TextBuffer::default(),
        // argv には 40 桁をそのまま載せる。短縮はタイトルの表示だけの都合で、
        // 渡す先を短くすると曖昧な省略形を git に解かせることになる。
        InputIntent::BranchCreate {
            start: start.map(|oid| oid.to_string()),
        },
    )
}

/// `r`。カーソルの指すブランチの名前を変える入力モーダルを開く。
///
/// **初期値は元の名前**（カーソルは末尾）。名前を少し直す操作なので空から打ち直させない。
///
/// **`Modal::Confirm` を通さない。** 不変ルール 5 が確認を求めるのは破壊的な操作で、
/// `branch --move` は `--force` を付けないので既存の名前と衝突すれば git が断る
/// （`fatal: a branch named 'x' already exists`。git 2.55 で確認）。
fn rename_branch(state: &mut AppState) -> Cmd {
    // 一覧が空。カーソル行が描かれていないので、通知を出す相手もいない（`d` と同じ）。
    let Some(branch) = state.branches.selected() else {
        return Cmd::None;
    };
    let from = branch.name.clone();
    input(
        state,
        "ブランチをリネーム".to_string(),
        format!("{from} の新しい名前"),
        TextBuffer::from_text(&from),
        InputIntent::BranchRename { from },
    )
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        branch_delete_confirm, open_input, press, press_code, state_on_branches,
        state_with_commits, type_text,
    };
    use super::*;
    use crate::app::{KeyCode, ListLoad};

    #[test]
    fn j_and_k_move_the_branch_cursor_without_touching_the_graph() {
        let mut state = state_on_branches(&["another", "main"], Some("main"));
        // カーソルは HEAD の行に座っている。
        assert_eq!(state.branches.cursor, 1);

        let _ = press(&mut state, 'k');

        assert_eq!(state.branches.cursor, 0);
        // 同じ `j` / `k` がグラフに漏れない。
        assert_eq!(state.graph.cursor, 0);
    }

    #[test]
    fn esc_leaves_the_branches_pane_for_the_graph() {
        // **`Tab` でしか入れないペインにも出口を用意する**（07）。戻り先は
        // `right_pane()` ではない。このペインは左の選択と無関係なので。
        let mut state = state_on_branches(&["main"], Some("main"));

        let _ = press_code(&mut state, KeyCode::Esc);

        assert_eq!(state.focus, Pane::Graph);
    }

    #[test]
    fn enter_switches_to_the_branch_under_the_cursor() {
        let mut state = state_on_branches(&["another", "main"], Some("main"));
        let _ = press(&mut state, 'k');

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::Exec(GitOp::Switch("another".to_string())));
        // 切り替えたあと続けて別のブランチを選べる。確認モーダルも通さない。
        assert_eq!(state.focus, Pane::Branches);
        assert_eq!(state.modal, None);
    }

    #[test]
    fn enter_on_the_head_branch_still_asks_git() {
        // **押せるかを手前で判定しない**（不変ルール 4）。一覧の HEAD の行は最後に読んだ
        // 値でしかないので、これで「切り替えなくてよい」と決めると、別の端末で
        // 切り替えられていたときに `Enter` が何も起こさなくなる（07 の「Branches ペイン」）。
        let mut state = state_on_branches(&["another", "main"], Some("main"));
        assert_eq!(state.branches.head_row(), Some(state.branches.cursor));

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::Exec(GitOp::Switch("main".to_string())));
    }

    fn branch_delete(force: bool) -> Cmd {
        Cmd::Exec(GitOp::BranchDelete {
            name: "topic".to_string(),
            force,
        })
    }

    #[test]
    fn d_and_shift_d_confirm_before_deleting_and_say_which_one_is_open() {
        let (plain, on_yes) = branch_delete_confirm(false);
        assert!(plain.contains("topic"), "{plain}");
        assert_eq!(on_yes, branch_delete(false));

        let (forced, on_yes) = branch_delete_confirm(true);
        assert_eq!(on_yes, branch_delete(true));
        // **同じ本文にしない。** どちらのキーで開いた確認なのかが画面から判らなくなる。
        assert_ne!(forced, plain, "d と D の本文が同じです");
    }

    #[test]
    fn only_n_works_while_the_branch_list_is_empty() {
        // 読み込み中・失敗・コミットが 1 つも無いリポジトリ。カーソル行が描かれて
        // いないので、`branches[cursor]` を書くと添字が外れる。
        let mut state = state_with_commits(2);
        state.focus = Pane::Branches;
        assert_eq!(state.branches.load(), ListLoad::Pending);

        assert_eq!(press_code(&mut state, KeyCode::Enter), Cmd::None);
        assert_eq!(press(&mut state, 'd'), Cmd::None);
        assert_eq!(press(&mut state, 'D'), Cmd::None);
        assert_eq!(press(&mut state, 'r'), Cmd::None);
        assert_eq!(state.modal, None);

        // **`n` だけは開く** —— 作成はカーソル行を要らず、`git init` の直後（ブランチが
        // 1 本も無い）はまさに作りたい状態。`switch --create` は unborn HEAD でも通る
        // （git 2.55 で確認）。同じ状態でキーが Branches ペインに届いている証拠も兼ねる。
        let _ = press(&mut state, 'n');
        let (_, _, _, intent) = open_input(&state);
        assert_eq!(intent, &InputIntent::BranchCreate { start: None });
    }

    #[test]
    fn n_on_the_branches_pane_opens_an_empty_input_for_a_branch_from_head() {
        let mut state = state_on_branches(&["main", "topic"], Some("main"));
        let _ = press(&mut state, 'k');

        let cmd = press(&mut state, 'n');

        assert_eq!(cmd, Cmd::None, "確認より先に git を走らせています");
        let (title, _, buf, intent) = open_input(&state);
        // **カーソル行（main）を起点にしない。** 一覧を眺めているだけのつもりで
        // 別のブランチから生やすことになる（07 の「Branches ペイン」）。
        assert_eq!(intent, &InputIntent::BranchCreate { start: None });
        // 起点が HEAD のときはタイトルに起点を書かない。
        assert!(!title.contains("から"), "{title}");
        assert_eq!(buf.text(), "");
    }

    #[test]
    fn n_on_the_graph_starts_the_branch_at_the_commit_under_the_cursor() {
        let mut state = state_with_commits(2);
        // **`fake_oid` のままでは見張れない。** 20 バイト目にだけ index を置くので
        // 短縮 Oid が常に `0000000` になり、別のコミットの Oid を出しても通る。
        let oid = Oid::from_str("a1b2c3d4e5f60718293a4b5c6d7e8f9001122334").expect("40 桁");
        state.graph.lines[1].commit.oid = oid;
        // WIP 行 → 1 件目 → 2 件目。
        let _ = press(&mut state, 'j');
        let _ = press(&mut state, 'j');

        let cmd = press(&mut state, 'n');

        assert_eq!(cmd, Cmd::None);
        let (title, _, _, intent) = open_input(&state);
        // **どのコミットから生やすかは押す前に読めないといけない**（07）。
        assert!(title.contains("a1b2c3d"), "{title}");
        // argv には 40 桁をそのまま載せる。曖昧な省略形を git に解かせない。
        assert_eq!(
            intent,
            &InputIntent::BranchCreate {
                start: Some(oid.to_string()),
            }
        );
    }

    #[test]
    fn n_on_the_wip_row_starts_the_branch_at_head() {
        // WIP 行にはコミットが無い。**「グラフに居るから起点がある」ではない。**
        let mut state = state_with_commits(2);
        assert_eq!(state.graph.cursor, 0);

        let _ = press(&mut state, 'n');

        let (_, _, _, intent) = open_input(&state);
        assert_eq!(intent, &InputIntent::BranchCreate { start: None });
    }

    #[test]
    fn r_prefills_the_current_name_and_says_which_branch_is_being_renamed() {
        let mut state = state_on_branches(&["main", "topic"], Some("main"));
        assert_eq!(state.branches.selected().expect("空です").name, "main");

        let cmd = press(&mut state, 'r');

        assert_eq!(cmd, Cmd::None);
        let (_, prompt, buf, intent) = open_input(&state);
        // 名前を少し直す操作なので空から打ち直させない。カーソルは末尾。
        assert_eq!(buf.text(), "main");
        assert_eq!(buf.cursor, (0, 4));
        assert!(prompt.contains("main"), "{prompt}");
        assert_eq!(
            intent,
            &InputIntent::BranchRename {
                from: "main".to_string(),
            }
        );
    }

    #[test]
    fn enter_creates_the_branch_and_switches_to_it() {
        let mut state = state_on_branches(&["main"], Some("main"));
        let _ = press(&mut state, 'n');
        type_text(&mut state, "feat/x");

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(
            cmd,
            Cmd::Exec(GitOp::BranchCreate {
                name: "feat/x".to_string(),
                start: None,
                // UI からの作成は常に切り替える（03）。
                switch: true,
            })
        );
        assert_eq!(state.modal, None);
    }

    #[test]
    fn enter_renames_the_branch_under_the_cursor() {
        let mut state = state_on_branches(&["topic"], None);
        let _ = press(&mut state, 'r');
        // 末尾に足す（初期値がそのまま残っている）。
        type_text(&mut state, "-2");

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(
            cmd,
            Cmd::Exec(GitOp::BranchRename {
                from: "topic".to_string(),
                to: "topic-2".to_string(),
            })
        );
        assert_eq!(state.modal, None);
    }
}
