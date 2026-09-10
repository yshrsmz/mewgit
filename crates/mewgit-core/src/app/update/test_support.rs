//! [`super`] のテストが共有する土台。
//!
//! **2 つ以上のモジュールから呼ばれるヘルパだけを置く**（理由は 00 の
//! 「ディレクトリ構成」）。1 人しか使わないものはそのモジュールの `mod tests` に残す。
//! **他のヘルパから呼ばれるだけのものもここ** —— `failed_with` と `with_remotes` は
//! 直接の利用者が 1 モジュールずつだが、この中の `reject_non_fast_forward` /
//! `state_for_push` を通して 3 モジュールに届く。
//!
//! `crate::test_util` とは別。あちらは `test-util` feature で `mewgit-tui` にも配る
//! 土台で、ここは `app::update` の中だけ。

use super::*;
use crate::app::{ExecDone, Modal, RepoInfo, RepoSnapshot, TextBuffer};
use crate::config::Config;
use crate::git_exec::{ExecError, ExecErrorKind, ExecOutput};
use crate::git_read::{
    Branch, Commit, DiffTarget, FileDiff, GitReadError, HeadInfo, LogPage, Oid, RefIndex,
    UpstreamConfig, WorktreeSection, WorktreeSnapshot,
};
use crate::test_util::{
    fake_branch, fake_commit, fake_detail, fake_file_diff, fake_hunk, fake_remote, fake_snapshot,
    fake_upstream, reply,
};

/// グラフに使える行数。`ui` が決めた値が [`Msg::Resize`] で来る想定。
pub(super) const ROWS: usize = 20;

pub(super) fn state() -> AppState {
    let mut state = AppState::new(RepoInfo::default(), Config::default());
    state.graph.viewport = ROWS;
    state
}

/// `n` 番目のコミット。oid は `n` から作るので、256 件以上は重なる。
pub(super) fn commit(n: usize) -> Commit {
    fake_commit(n as u8, &format!("commit {n}"))
}

/// `n` 件のコミットを積んだ状態。ログは読み切っている。
pub(super) fn state_with_commits(n: usize) -> AppState {
    state_with_page(n, true)
}

/// `n` 件を 1 ページとして積んだ状態。`done` は「これ以上無い」。
///
/// 返信は `test_util::reply` で届ける（発行を通さないと刻印が付かない）。
pub(super) fn state_with_page(n: usize, done: bool) -> AppState {
    let mut state = state();
    let first = more(&state);
    let page = LogPage {
        commits: (0..n).map(commit).collect(),
        done,
    };
    let _ = reply(&mut state, first, page_loaded(Ok(page)));
    state
}

/// staged に 1 ファイルだけある状態。worktree ペインの Staged を選んでいる。
pub(super) fn only_staged() -> AppState {
    worktree_state(fake_snapshot(&["a.txt"], &[], &[]), WorktreeSection::Staged)
}

/// worktree ペインにフォーカスし、その区画の先頭を選んだ状態。
pub(super) fn worktree_state(snapshot: WorktreeSnapshot, section: WorktreeSection) -> AppState {
    let mut state = state();
    let _ = reply(
        &mut state,
        Cmd::LoadStatus,
        Msg::StatusLoaded(Box::new(Ok(snapshot))),
    );
    state.focus = Pane::Worktree;
    state.worktree.section = section;
    state.worktree.cursor = 0;
    state
}

/// 続きのページが届いた。
pub(super) fn page_loaded(result: Result<LogPage, GitReadError>) -> Msg {
    Msg::LogLoaded {
        restart: false,
        result,
    }
}

/// 続きを頼む `Cmd`。
pub(super) fn more(state: &AppState) -> Cmd {
    Cmd::LoadLog {
        count: state.config.graph.page_size,
        restart: false,
    }
}

/// 先頭から読み直す `Cmd`。
pub(super) fn reload(state: &AppState) -> Cmd {
    Cmd::LoadLog {
        count: state.config.graph.page_size,
        restart: true,
    }
}

pub(super) fn press(state: &mut AppState, c: char) -> Cmd {
    update(state, Msg::Key(Key::char(c)))
}

pub(super) fn press_code(state: &mut AppState, code: KeyCode) -> Cmd {
    update(state, Msg::Key(Key::new(code)))
}

/// 文字を 1 つずつ打つ。**貼り付け（`Msg::Paste`）ではない。**
pub(super) fn type_text(state: &mut AppState, text: &str) {
    for c in text.chars() {
        let _ = press(state, c);
    }
}

/// 1 ファイルに `hunks` 個のハンクを持つ diff。
pub(super) fn fake_diff(path: &str, hunks: usize) -> Vec<FileDiff> {
    let hunks = (0..hunks)
        .map(|n| fake_hunk(&format!("@@ -{n},1 +{n},1 @@"), &[]))
        .collect();
    vec![fake_file_diff(path, hunks)]
}

pub(super) fn diff_ok(target: DiffTarget, files: Vec<FileDiff>) -> Msg {
    Msg::DiffLoaded {
        target,
        result: Box::new(Ok(files)),
    }
}

/// ブランチ一覧の返信を届ける。**成功も失敗もここを通す。**
pub(super) fn with_branches(
    state: &mut AppState,
    result: Result<Vec<Branch>, GitReadError>,
) -> Cmd {
    update(state, Msg::BranchesLoaded(Box::new(result)))
}

/// Branches ペインにフォーカスを置き、一覧が届いた状態。
pub(super) fn state_on_branches(names: &[(&str, bool)]) -> AppState {
    let mut state = state_with_commits(2);
    state.focus = Pane::Branches;
    let branches = names
        .iter()
        .map(|(name, is_head)| fake_branch(name, *is_head))
        .collect();
    let _ = with_branches(&mut state, Ok(branches));
    state
}

/// `d` / `D` を押したときの確認モーダルの本文と `on_yes`。
///
/// **押すたびに新しい state を作る。** 続けて押すには確認を閉じることになり、
/// `confirm_key` の見張り（`confirming_runs_the_command_and_closes_the_modal`）が
/// このテストに紛れ込む。
pub(super) fn branch_delete_confirm(force: bool) -> (String, Cmd) {
    let mut state = state_on_branches(&[("topic", false)]);

    let cmd = press(&mut state, if force { 'D' } else { 'd' });

    assert_eq!(cmd, Cmd::None, "確認より先に git を走らせています");
    let Some(Modal::Confirm { body, on_yes, .. }) = state.modal else {
        panic!(
            "確認モーダルが開いていません: force={force} {:?}",
            state.modal
        );
    };
    (body, on_yes)
}

/// 開いている入力モーダル。開いていなければ落とす。
pub(super) fn open_input(state: &AppState) -> (&str, &str, &TextBuffer, &InputIntent) {
    let Some(Modal::Input {
        title,
        prompt,
        buf,
        intent,
    }) = &state.modal
    else {
        panic!("入力モーダルが開いていません: {:?}", state.modal);
    };
    (title, prompt, buf, intent)
}

/// `set_upstream` 付きの非 force な push。
pub(super) fn push_op(remote: &str, branch: &str, set_upstream: bool) -> GitOp {
    GitOp::Push {
        remote: remote.to_string(),
        src: branch.to_string(),
        dst: None,
        set_upstream,
        lease: None,
    }
}

/// `origin main` への force push。
pub(super) fn forced_push_op(lease: Oid) -> GitOp {
    GitOp::Push {
        remote: "origin".to_string(),
        src: "main".to_string(),
        dst: None,
        set_upstream: false,
        lease: Some(lease),
    }
}

/// 追跡設定を持つブランチ。`merge` は `branch.<name>.merge`（完全な ref）。
pub(super) fn tracking_branch(name: &str, remote: &str, merge: &str) -> Branch {
    Branch {
        upstream_config: Some(UpstreamConfig {
            remote: remote.to_string(),
            merge: merge.to_string(),
        }),
        ..fake_branch(name, true)
    }
}

/// 一覧が届いていて、HEAD が `head` の状態。
pub(super) fn state_for_push(remotes: &[&str], head: Branch) -> AppState {
    let mut state = state_with_commits(2);
    with_remotes(&mut state, remotes);
    let _ = with_branches(&mut state, Ok(vec![head]));
    state
}

/// 開いている確認モーダルの本文と `on_yes`。開いていなければ落とす。
pub(super) fn open_confirm(state: &AppState) -> (&str, &Cmd) {
    let Some(Modal::Confirm { body, on_yes, .. }) = &state.modal else {
        panic!("確認モーダルが開いていません: {:?}", state.modal);
    };
    (body, on_yes)
}

/// `f` が出した fetch のリモート名。出していなければ `None`。
pub(super) fn fetched_remote(cmd: &Cmd) -> Option<String> {
    match cmd {
        Cmd::Exec(GitOp::Fetch { remote, .. }) => remote.clone(),
        _ => None,
    }
}

/// `branch.<name>.merge` の既定の形。
pub(super) const HEADS_MAIN: &str = "refs/heads/main";

/// 追跡 ref も持つブランチ。その位置が force push の lease になる。
///
/// **`ahead` は 1。** force push はリモートに無いコミットを出す操作なので、
/// 0 のままだと `push_plan` が「出すものが無い」と断る（`↓n` だけのブランチを
/// 見る腕は [`a_force_push_is_refused_when_there_is_nothing_to_publish`]）。
pub(super) fn tracked_head(
    name: &str,
    remote: &str,
    merge: &str,
    upstream_oid: Option<Oid>,
) -> Branch {
    Branch {
        upstream: upstream_oid.map(|oid| fake_upstream(&format!("{remote}/{name}"), oid)),
        ahead: 1,
        ..tracking_branch(name, remote, merge)
    }
}

/// `op` を発行して、その結果を届ける。**返信まで届ける**ので `JobTracker` に
/// ジョブが残らない（残すと後続の `Cmd::Exec` を `issue` が黙って落とす。#38）。
pub(super) fn finish_exec(state: &mut AppState, op: GitOp, result: Result<ExecOutput, ExecError>) {
    let _ = state.issue(Cmd::Exec(op.clone()));
    let _ = update(state, exec_done_of(op, result));
}

/// 指定した kind で失敗した実行。**分類は済んでいる前提**なので stderr は見ない
/// （実測のパターンは `git_exec` の `classifies_by_pattern` が固定している）。
pub(super) fn failed_with(kind: ExecErrorKind) -> ExecError {
    ExecError::Failed {
        argv: vec!["push".into(), "origin".into(), "main".into()],
        code: Some(1),
        stderr: "stderr の中身".to_string(),
        kind,
    }
}

/// `op` が非 FF で断られた状態にする。**`exec_finished` を通す** —— 状態を直に
/// 置くと、憶える側の条件（非 force・`NonFastForward`）を 1 つも踏まない。
///
/// **`op` を取るのは push 以外も断られるため。** `pull --ff-only` の拒否も
/// `NonFastForward` に落ちる（03 のエラー分類）ので、名前を push に寄せると
/// 呼び出し側で嘘になる。
pub(super) fn reject_non_fast_forward(state: &mut AppState, op: GitOp) {
    finish_exec(state, op, Err(failed_with(ExecErrorKind::NonFastForward)));
}

/// 成功した `git` の出力。中身は見ないので固定でよい。
pub(super) fn fake_output() -> ExecOutput {
    ExecOutput {
        argv: vec!["add".into(), "--".into(), "a.txt".into()],
        stdout: String::new(),
        stderr: String::new(),
    }
}

/// `git` の実行結果。ref を動かさない操作。
pub(super) fn exec_done(result: Result<ExecOutput, ExecError>) -> Msg {
    exec_done_of(GitOp::Stage(vec!["a.txt".into()]), result)
}

/// `op` の実行結果。ref を動かすかどうかで `exec_finished` の出す `Cmd` が変わる。
pub(super) fn exec_done_of(op: GitOp, result: Result<ExecOutput, ExecError>) -> Msg {
    Msg::ExecFinished(Box::new(ExecDone {
        op,
        result,
        progress: None,
    }))
}

/// 端末に描いたときの桁数。**案内文は ASCII と日本語だけ**なので、非 ASCII を
/// 2 桁と数えれば実際の幅と一致する（合成文字や絵文字を入れたらここが嘘になる）。
pub(super) fn cells(text: &str) -> usize {
    text.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

/// カーソルの指すコミットの Oid。
pub(super) fn cursor(state: &AppState) -> Oid {
    state
        .graph
        .cursor_commit()
        .expect("カーソルの指す行がありません")
        .oid
}

/// カーソルがコミットに乗ったときに出る `Cmd`。詳細と一覧を一緒に頼む（01 の
/// 「コミットのファイル一覧」）。
pub(super) fn commit_loads(oid: Oid) -> Cmd {
    Cmd::batch([Cmd::LoadCommitDetail(oid), Cmd::LoadCommitFiles(oid)])
}

/// 索引 `index` のコミットの詳細が届いた [`Msg`]。
pub(super) fn detail_ok(index: u8) -> Msg {
    let detail = fake_detail(index, &format!("commit {index}"));
    Msg::CommitDetailLoaded {
        oid: detail.oid,
        result: Box::new(Ok(detail)),
    }
}

pub(super) fn commit_op() -> GitOp {
    GitOp::Commit {
        message: "fix".to_string(),
        amend: false,
    }
}

/// `cmd` に `wanted` が含まれるか。`Batch` の中も見る。
pub(super) fn issued(cmd: &Cmd, wanted: &Cmd) -> bool {
    match cmd {
        Cmd::Batch(cmds) => cmds.iter().any(|cmd| issued(cmd, wanted)),
        cmd => cmd == wanted,
    }
}

/// ref の索引 1 つぶん。中身は見ないので空でよい。
pub(super) fn repo_snapshot() -> RepoSnapshot {
    RepoSnapshot {
        remotes: Vec::new(),
        head: HeadInfo::Branch {
            name: "main".to_string(),
            oid: commit(1).oid,
        },
        refs: RefIndex::from_iter([]),
    }
}

/// リモートを登録する。`RepoInfo` は `RepoInfoLoaded` で入る値なので直に置く。
pub(super) fn with_remotes(state: &mut AppState, names: &[&str]) {
    state.repo.remotes = names.iter().copied().map(fake_remote).collect();
}
