//! モーダルのキー処理（01 の「Pane と Modal」）。
//!
//! 表示中の入力はすべてここが受ける。**確定して何をするかは各モーダルの側**に
//! あり（[`super::account::account_picked`] など）、ここが持つのは骨格 ——
//! カーソル・閉じ方・受けないキー。

use crate::git_exec::GitOp;

use super::account::{account_picked, account_reresolve, account_unset};
use super::prs::pr_create_key;
use super::remote::remote_picked;
use super::worktree::commit_message_key;
use super::{edit_key, submit_exec};
use crate::app::{AppState, Cmd, InputIntent, Key, KeyCode, Modal, NoticeLevel};

/// モーダルのキー処理。表示中の入力はすべてここが受ける。
pub(super) fn modal_key(state: &mut AppState, key: Key) -> Cmd {
    match state.modal {
        Some(Modal::Confirm { .. }) => confirm_key(state, key),
        Some(Modal::CommitMessage { .. }) => commit_message_key(state, key),
        Some(Modal::Input { .. }) => input_key(state, key),
        // **一覧から 1 つ選ぶモーダルは骨格が同じ**（`j` / `k` / `Esc`）。
        // 違うのは `Enter` で出す `Cmd` だけなので、そこだけ各モーダルが持つ。
        Some(Modal::RemotePicker { .. }) => picker_key(state, key, remote_picked),
        Some(Modal::AccountPicker { .. }) => account_key(state, key),
        // **骨格を共有しない。** 欄が 4 つあって `Enter` の意味も欄で変わるので、
        // `picker_key` にも `edit_key` 1 本にも収まらない（01 の「PR 作成（`N`）」）。
        Some(Modal::PrCreate { .. }) => pr_create_key(state, key),
        // 他のモーダルはそれぞれのタスクで実装する。閉じる道だけ開けておく。
        Some(_) => {
            if key == Key::new(KeyCode::Esc) {
                state.modal = None;
            }
            Cmd::None
        }
        None => Cmd::None,
    }
}

/// 確認モーダル。`y` / `Enter` で実行、`n` / `Esc` で取り消し。
///
/// **先にモーダルを閉じてから `on_yes` を返す。** 閉じずに返すと、`Cmd` が
/// [`AppState::issue`] に落とされたときにモーダルだけが画面に残る。
fn confirm_key(state: &mut AppState, key: Key) -> Cmd {
    let confirmed = match key.code() {
        KeyCode::Char('y') | KeyCode::Enter => true,
        KeyCode::Char('n') | KeyCode::Esc => false,
        _ => return Cmd::None,
    };

    match (confirmed, state.modal.take()) {
        (true, Some(Modal::Confirm { on_yes, .. })) => on_yes,
        _ => Cmd::None,
    }
}

/// 一覧から 1 つ選ぶモーダルのキー処理。`j` / `k` で選び、`Enter` で確定、`Esc` で閉じる。
///
/// **確定だけを `on_enter` に委ねる。** 骨格（カーソル・閉じ方・受けないキー）は
/// [`Modal::RemotePicker`] と [`Modal::AccountPicker`] で同じで、2 本に写すと
/// ピッカーのキーを 1 つ足したときに片方だけ古くなる（描画側を
/// `modals::picker` に寄せたのと同じ数え方。07）。**閉じるかどうかも `on_enter` が
/// 決める** —— [`confirm_key`] は先に閉じてから返すが、`Cmd` が
/// [`AppState::issue`] に落とされる形では閉じてはいけない（`submit_exec`）。
///
/// **クロージャではなく `fn` を受ける。** 環境を捕まえる必要が無く、`Modal` に
/// コールバックを持たせない規約とも揃う。
fn picker_key(state: &mut AppState, key: Key, on_enter: fn(&mut AppState) -> Cmd) -> Cmd {
    match key.code() {
        // 端の扱いはペインのカーソルと同じ規則（`Modal::move_cursor`）。
        // **候補が空なら動かない**（`stepped_index`）ので、選ばせない
        // `AccountPicker` でもここで断らなくてよい。
        KeyCode::Char('j') | KeyCode::Down => {
            move_modal_cursor(state, 1);
            Cmd::None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            move_modal_cursor(state, -1);
            Cmd::None
        }
        KeyCode::Enter => on_enter(state),
        KeyCode::Esc => {
            state.modal = None;
            Cmd::None
        }
        _ => Cmd::None,
    }
}

/// [`Modal::AccountPicker`] のキー処理。骨格は [`picker_key`] に委ね、`u` と `r` を
/// 自分で受ける（01 の「アカウントの選び直し」）。
///
/// **どちらも [`picker_key`] に足さない。** [`Modal::RemotePicker`] には外す対象も
/// 解決し直す相手も無いので、共通の骨格に置くと片方でしか意味を持たないキーが増える。
/// **効くかどうかは `AccountView` の述語 1 本ずつ**（`is_unsettable` /
/// `is_reresolvable`）で、キーの案内を出す `ui` 側も同じものを読む（#53）。
/// **3 本目を足すなら、キーと述語と文言の対を型で括ることを考える** —— いまは
/// ここと `modals::account_picker::hint` が手で揃えている。
fn account_key(state: &mut AppState, key: Key) -> Cmd {
    if key == Key::char('u') {
        return account_unset(state);
    }
    if key == Key::char('r') {
        return account_reresolve(state);
    }
    picker_key(state, key, account_picked)
}

fn move_modal_cursor(state: &mut AppState, delta: isize) {
    if let Some(modal) = state.modal.as_mut() {
        modal.move_cursor(delta);
    }
}

/// [`Modal::Input`] のキー処理。**文字キーはすべて入力になる。**
///
/// `CommitMessage` と違い**改行のキーを 1 つも受けない**。行が 1 本しか無く、受けると
/// `TextBuffer` に 2 行目ができて、`text()` の返す `\n` 入りの文字列がそのまま
/// ブランチ名になる（01 の「1 行入力」）。
fn input_key(state: &mut AppState, key: Key) -> Cmd {
    // 取り消し。**確認は挟まない**（`CommitMessage` と同じ。01）。
    if key == Key::new(KeyCode::Esc) {
        state.modal = None;
        return Cmd::None;
    }

    // **修飾の有無を問わず確定。** `CommitMessage` のつもりで `Alt+Enter` を打った人に
    // 「何も起きない」を返さないため。`Ctrl-J` はここに落とさない（修飾付きの文字キーは
    // 入力にしない規則に沿う）。
    if key.code() == KeyCode::Enter {
        return input_submit(state);
    }

    let Some(Modal::Input { buf, .. }) = state.modal.as_mut() else {
        return Cmd::None;
    };

    // `↑` `↓` と `Ctrl-J` は `edit_key` が受けないので落ちる。行が 1 本しか無く、
    // 動かす先も割る先も無い。
    let _ = edit_key(buf, key);
    Cmd::None
}

/// 入力を確定し、[`InputIntent`] の指す `GitOp` を出す。
///
/// **名前が正しいかは手前で判定しない**（不変ルール 4）。`git check-ref-format` の規則を
/// 写すことになり、写した規則は git の版が変わっても追随しない。**空だけは断る** —
/// `git switch --create ""` は落ちるが、その時点でモーダルは閉じていて打った文字が
/// 戻らない（`commit_submit` と同じ理由。01 の「1 行入力」）。
fn input_submit(state: &mut AppState) -> Cmd {
    let Some(Modal::Input { buf, intent, .. }) = &state.modal else {
        return Cmd::None;
    };

    if buf.is_blank() {
        state.push_notice(NoticeLevel::Warn, "名前が空です".to_string(), None);
        return Cmd::None;
    }

    let name = buf.text();
    let op = match intent {
        InputIntent::BranchCreate { start } => GitOp::BranchCreate {
            name,
            start: start.clone(),
            switch: true,
        },
        InputIntent::BranchRename { from } => GitOp::BranchRename {
            from: from.clone(),
            to: name,
        },
    };
    submit_exec(state, op)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        branch_delete_confirm, open_confirm, open_input, press, press_code, push_op,
        reject_non_fast_forward, state, state_for_push, state_on_branches, tracked_head, type_text,
        worktree_state, HEADS_MAIN,
    };
    use super::super::update;
    use super::*;
    use crate::app::{JobKind, Modal, Modifiers, Msg};

    use crate::git_read::{FileStatus, WorktreeSection};

    use crate::test_util::{fake_branch, fake_oid, fake_snapshot};

    #[test]
    fn a_blank_name_is_refused_without_closing_the_input() {
        // `git switch --create ""` は落ちるが、その時点でモーダルは閉じていて
        // 打った文字が戻らない（01 の「1 行入力」）。空白だけも同じ。
        let mut state = state_on_branches(&[("main", true)]);
        let _ = press(&mut state, 'n');
        let _ = press(&mut state, ' ');

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::None);
        let (_, _, buf, _) = open_input(&state);
        assert_eq!(buf.text(), " ");
        assert_eq!(
            state.notices.back().map(|notice| notice.level),
            Some(NoticeLevel::Warn)
        );
    }

    #[test]
    fn a_busy_git_keeps_the_input_and_what_was_typed() {
        // 閉じてから `Cmd` を返すと、`issue` に落とされたときに打った文字ごと消える
        // （`commit_submit` と同じ。01 の「1 行入力」）。
        let mut state = state_on_branches(&[("main", true)]);
        let _ = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));
        let _ = press(&mut state, 'n');
        type_text(&mut state, "feat/x");

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::None);
        let (_, _, buf, _) = open_input(&state);
        assert_eq!(buf.text(), "feat/x");
    }

    #[test]
    fn the_input_never_grows_a_second_line() {
        // 行が 2 本になると `text()` に `\n` が混ざり、そのままブランチ名になる
        // （01 の「1 行入力」）。`Ctrl-J` は `CommitMessage` では改行のキー。
        let mut state = state_on_branches(&[("main", true)]);
        let _ = press(&mut state, 'n');
        let _ = press(&mut state, 'a');

        let cmd = update(&mut state, Msg::Key(Key::ctrl('j')));

        assert_eq!(cmd, Cmd::None);
        let (_, _, buf, _) = open_input(&state);
        assert_eq!(buf.lines.len(), 1);
        assert_eq!(buf.text(), "a");
    }

    #[test]
    fn a_modified_enter_confirms_like_a_plain_one() {
        // `CommitMessage` のつもりで `Alt+Enter` を打った人に「何も起きない」を
        // 返さない。こちらに改行の意味は無い（01 の「1 行入力」）。
        let mut state = state_on_branches(&[("main", true)]);
        let _ = press(&mut state, 'n');
        let _ = press(&mut state, 'x');

        let cmd = update(
            &mut state,
            Msg::Key(Key::with_mods(
                KeyCode::Enter,
                Modifiers {
                    alt: true,
                    ..Modifiers::default()
                },
            )),
        );

        // 見たいのは「修飾付きでも確定するか」だけ。組む `GitOp` の中身は
        // `enter_creates_the_branch_and_switches_to_it` が見張っている。
        assert!(
            matches!(cmd, Cmd::Exec(GitOp::BranchCreate { .. })),
            "{cmd:?}"
        );
        assert_eq!(state.modal, None);
    }

    #[test]
    fn pasting_several_lines_joins_them_into_one_name() {
        // **`Msg::Paste` を受けるモーダルが 2 つ目。** 腕が無いと貼り付けが
        // 1 文字ずつ `Msg::Key` として届く形に戻り、最初の改行で確定して残りが
        // ペインのキーになる（#34 で踏んだ形）。
        let mut state = state_on_branches(&[("main", true)]);
        let _ = press(&mut state, 'n');

        let cmd = update(
            &mut state,
            Msg::Paste(
                "feat/a
feat/b
"
                .to_string(),
            ),
        );

        assert_eq!(cmd, Cmd::None);
        let (_, _, buf, _) = open_input(&state);
        assert_eq!(buf.lines.len(), 1);
        assert_eq!(buf.text(), "feat/afeat/b");
    }

    #[test]
    fn esc_closes_the_input_without_running_anything() {
        let mut state = state_on_branches(&[("main", true)]);
        let _ = press(&mut state, 'n');
        let _ = press(&mut state, 'x');

        let cmd = press_code(&mut state, KeyCode::Esc);

        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.modal, None);
    }

    #[test]
    fn every_confirmation_body_is_free_of_folded_indentation() {
        // 文字列の行継続（`\` + 改行）は `cargo fmt` が 1 行に畳むときに、次の行の
        // インデントをリテラルの空白として残す。見た目は「staged」の前に空白が
        // 20 個並んだ確認文になるが、`contains` を見るテストは素通りする。
        let cases = [
            (WorktreeSection::Untracked, FileStatus::Untracked),
            (WorktreeSection::Staged, FileStatus::Added),
            (WorktreeSection::Staged, FileStatus::Modified),
        ];

        let mut bodies = Vec::new();
        for (section, status) in cases {
            let mut state =
                worktree_state(fake_snapshot(&["a.txt"], &["a.txt"], &["a.txt"]), section);
            state.worktree.snapshot.staged[0].status = status;

            let _ = press(&mut state, 'x');
            let Some(Modal::Confirm { body, .. }) = &state.modal else {
                panic!("確認モーダルが出ていません: {section:?} {status:?}");
            };
            bodies.push(body.clone());
        }
        // **本文を組む場所は worktree の discard だけではない。** 産出元が増えたら
        // ここに通す。片方だけ守っていても `contains` を見るテストは素通りする。
        bodies.push(branch_delete_confirm(false).0);
        bodies.push(branch_delete_confirm(true).0);

        // push の確認は 2 つ（`-u` の提案と force push）。**`\n` を埋めているのは
        // force push だけ**なので、畳み込みが混ざっても行数では見えない。
        let lease = fake_oid(7);
        let mut forced = state_for_push(
            &["origin"],
            tracked_head("main", "origin", HEADS_MAIN, Some(lease)),
        );
        reject_non_fast_forward(&mut forced, push_op("origin", "main", false));
        let _ = press(&mut forced, 'P');
        bodies.push(open_confirm(&forced).0.to_string());

        let mut proposed = state_for_push(&["origin"], fake_branch("main", true));
        let _ = press(&mut proposed, 'P');
        bodies.push(open_confirm(&proposed).0.to_string());

        for body in bodies {
            for line in body.lines() {
                assert_eq!(line, line.trim_start(), "行頭に空白がある: {body:?}");
                assert!(!line.contains("  "), "空白が連なっている: {body:?}");
            }
        }
    }

    #[test]
    fn confirming_runs_the_command_and_closes_the_modal() {
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        let _ = press(&mut state, 'x');

        let cmd = press(&mut state, 'y');

        assert_eq!(cmd, Cmd::Exec(GitOp::Discard(vec!["a.txt".into()])));
        // 閉じずに返すと、`issue` に落とされたときモーダルだけが残る。
        assert!(state.modal.is_none());
        assert!(state.jobs.is_running(JobKind::Exec));
    }

    #[test]
    fn declining_closes_the_modal_without_running_anything() {
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        let _ = press(&mut state, 'x');

        assert_eq!(press(&mut state, 'n'), Cmd::None);

        assert!(state.modal.is_none());
        assert!(!state.jobs.is_running(JobKind::Exec));
    }

    #[test]
    fn keys_do_not_reach_the_pane_while_a_modal_is_open() {
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt", "b.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        let _ = press(&mut state, 'x');

        // モーダルが受けるので、カーソルは動かない。
        let _ = press(&mut state, 'j');
        assert_eq!(state.worktree.cursor, 0);
        assert!(state.modal.is_some());
    }

    #[test]
    fn esc_closes_the_modal() {
        let mut state = AppState {
            modal: Some(Modal::Help),
            ..state()
        };

        assert_eq!(
            update(&mut state, Msg::Key(Key::new(KeyCode::Esc))),
            Cmd::None
        );
        assert!(state.modal.is_none());
    }

    #[test]
    fn modal_swallows_other_keys() {
        let mut state = AppState {
            modal: Some(Modal::Help),
            ..state()
        };

        // モーダル表示中の `q` は終了ではない。入力中に閉じられると困る。
        assert_eq!(update(&mut state, Msg::Key(Key::char('q'))), Cmd::None);
        assert_eq!(state.modal, Some(Modal::Help));
    }
}
