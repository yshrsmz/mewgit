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
use crate::app::{ExecDone, InputIntent, Modal, Modifiers, RepoInfo, TextBuffer};
use crate::config::Config;
use crate::git_exec::{ExecError, ExecErrorKind, ExecOutput};
use crate::git_read::{
    Branch, Commit, DiffTarget, FileDiff, FileEntry, FileStatus, GitReadError, LogPage, Oid,
    Remote, StashEntry, UpstreamConfig, WorktreeSection, WorktreeSnapshot,
};
use crate::test_util::{
    fake_branch, fake_commit, fake_detail, fake_entry, fake_file_diff, fake_hunk, fake_oid,
    fake_remote, fake_snapshot, fake_stash, fake_upstream, reply, FAKE_NOW,
};

/// グラフに使える行数。`ui` が決めた値が [`Msg::Layout`] で来る想定。
///
/// **実端末の値と一致していなくてよい。** テストはこれを viewport として使うだけで、
/// 実際の `ui::graph_rows` は 80x24 で 19 行（フッターが 2 行になったぶん。20 に
/// 当たるのは 80x25）。**一致させると、レイアウトを変えるたびに全テストの期待値が
/// 動く。**
pub(super) const ROWS: usize = 20;

pub(super) fn state() -> AppState {
    let mut state = AppState::new(RepoInfo::default(), Config::default(), FAKE_NOW);
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

/// `n` 件を積み、1 件目のコミットにカーソルを乗せて詳細と一覧が届いた状態。
///
/// 起動直後のカーソルは WIP 行なので、`j` を 1 回打って乗せる。**一覧も届ける。**
/// 届けないと `JobKind::LoadCommitFiles` が走ったままになり、以後の
/// `Cmd::LoadCommitFiles` が `issue` に落とされて、テストが見ているのが
/// 「出なかった」なのか「落とされた」なのか判らなくなる。
pub(super) fn on_first_commit(n: usize) -> AppState {
    on_first_commit_with_files(n, &["a.txt"])
}

/// [`on_first_commit`] で、ファイル一覧を `paths` にしたもの。
///
/// **一覧の行数を選べるようにしてある** —— カーソルを動かすキーの見張りは、端に置くと
/// 片方が正しく無反応になるので 2 件以上要る。
pub(super) fn on_first_commit_with_files(n: usize, paths: &[&str]) -> AppState {
    let mut state = state_with_commits(n);
    let _ = press(&mut state, 'j');
    let _ = update(&mut state, detail_ok(0));
    let _ = update(&mut state, files_of(0, paths));
    state
}

/// `paths` を `Modified` で並べたファイル一覧が届いた [`Msg`]。
pub(super) fn files_of(index: u8, paths: &[&str]) -> Msg {
    let entries = paths
        .iter()
        .map(|path| fake_entry(path, FileStatus::Modified))
        .collect();
    files_loaded(index, Ok(entries))
}

pub(super) fn files_loaded(index: u8, result: Result<Vec<FileEntry>, GitReadError>) -> Msg {
    Msg::CommitFilesLoaded {
        oid: fake_oid(index),
        result: Box::new(result),
    }
}

/// `pane` に届くまで `Tab` を押す。返すのは**最後の 1 回**が出した `Cmd`。
///
/// **回数を書かない。** 巡回の並びはグラフの選択と `config.github.enabled` で変わるので、
/// 固定の回数は別の状態で黙って外れる（`.claude/skills/pty-check/SKILL.md`）。
///
/// **途中の回の `Cmd` は捨てる。** 通過するペインが何かを頼む状態で使うと、そのジョブが
/// [`AppState::issue`] に登録されたまま残り、**後続の `assert_eq!(…, Cmd::None)` が門では
/// なく「落とされた」で通る**（#38 の形）。集計の無い行を持ったまま通過させるテストを
/// 書くときは、ここも受け取って見る。
pub(super) fn tab_to(state: &mut AppState, pane: Pane) -> Cmd {
    for _ in 0..state.focus_candidates().len() {
        let cmd = press_code(state, KeyCode::Tab);
        if state.focus == pane {
            return cmd;
        }
    }
    panic!("{pane:?} に届きませんでした: {:?}", state.focus);
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

/// `Reload::RefsAndLog` が出す読み直し（書き込みのあとと、外の ref の変化のあと）。
pub(super) fn refs_and_log(state: &AppState) -> Cmd {
    Cmd::Batch(vec![
        Cmd::LoadStatus,
        Cmd::LoadRepoInfo,
        Cmd::LoadBranches,
        Cmd::LoadStashes,
        reload(state),
    ])
}

pub(super) fn press(state: &mut AppState, c: char) -> Cmd {
    update(state, Msg::Key(Key::char(c)))
}

pub(super) fn press_code(state: &mut AppState, code: KeyCode) -> Cmd {
    update(state, Msg::Key(Key::new(code)))
}

/// `Enter` に修飾を付けた打鍵。`Alt+Enter` は `ESC CR` として届く。
///
/// **3 人目（`Modal::PrCreate` の body 欄）が出た時点でここへ移した。**
/// `Key::with_mods` の正規化（01 の「キー入力」）を変えたときに触る先が散らないように。
pub(super) fn enter_with(mods: Modifiers) -> Msg {
    Msg::Key(Key::with_mods(KeyCode::Enter, mods))
}

/// `Alt` だけを立てた修飾。`Alt+Enter` を打つのに使う。
pub(super) const ALT: Modifiers = Modifiers {
    alt: true,
    ctrl: false,
    shift: false,
};

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

/// Diff ペインを開き、`paths[0]` の diff が `hunks` 個のハンクで届いた状態。
pub(super) fn diff_state(paths: &[&str], hunks: usize) -> AppState {
    diff_pane(WorktreeSection::Unstaged, paths, fake_diff(paths[0], hunks))
}

/// Diff ペインを開き、`files` が届いた状態。
///
/// **Diff ペインの立ち上げはここ 1 本。** 「worktree の状態を作る → `Enter` →
/// `diff_ok`」を 2 箇所に書くと、片方だけ直したときに差が出る。
pub(super) fn diff_pane(
    section: WorktreeSection,
    paths: &[&str],
    files: Vec<FileDiff>,
) -> AppState {
    let snapshot = match section {
        WorktreeSection::Staged => fake_snapshot(paths, &[], &[]),
        WorktreeSection::Unstaged => fake_snapshot(&[], paths, &[]),
        WorktreeSection::Untracked => fake_snapshot(&[], &[], paths),
        WorktreeSection::Conflicted => panic!("conflicted は diff を出さない"),
    };
    let mut state = worktree_state(snapshot, section);
    let _ = press_code(&mut state, KeyCode::Enter);
    let target = state.wanted_diff().expect("対象が決まっていません");
    let _ = update(&mut state, diff_ok(target, files));
    state
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
    load_branches(&mut state, names);
    state
}

/// `(名前, HEAD か)` のブランチ一覧を届ける。
pub(super) fn load_branches(state: &mut AppState, names: &[(&str, bool)]) {
    let branches = names
        .iter()
        .map(|(name, is_head)| fake_branch(name, *is_head))
        .collect();
    let _ = with_branches(state, Ok(branches));
}

/// stash 一覧の返信を届ける。**発行してから届ける**（`test_util::reply`）。
pub(super) fn with_stashes(
    state: &mut AppState,
    result: Result<Vec<StashEntry>, GitReadError>,
) -> Cmd {
    reply(
        state,
        Cmd::LoadStashes,
        Msg::StashesLoaded(Box::new(result)),
    )
}

/// `n` 件の stash が届き、Stash ペインにフォーカスがある状態。グラフは WIP 行で、
/// ワーキングツリーの一覧はまだ届いていない —— **WIP 側の `Diff` は巡回の候補から
/// 外れている**（01 の「Pane と Modal」）。stash の diff がそれでも開けることを見る土台。
pub(super) fn state_on_stashes(n: usize) -> AppState {
    let mut state = state_with_commits(2);
    let _ = with_stashes(&mut state, Ok((0..n).map(fake_stash).collect()));
    let _ = tab_to(&mut state, Pane::Stash);
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
        stdout: String::new(),
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
    Msg::ExecFinished(Box::new(ExecDone { op, result }))
}

/// ランタイムが 250ms ごとに送る `Tick`（時刻は `FAKE_NOW`）。
pub(super) fn tick() -> Msg {
    Msg::Tick(FAKE_NOW)
}

/// 通知の要約行の予算（セル）。**80 桁の端末から、前に添える時刻の 9 セル
/// （`HH:MM:SS` と空白）を引いた残り**（01 の「失敗の案内文」）。フッターの 2 行目が
/// キーヒントなので、1 行目は時刻のほかは幅いっぱい使える。
///
/// **見張りは複数あって、新しい産出元を自分では拾わない**（`.claude/rules/testing.md`）——
/// `exec_advice` / `A` / `o` / PR の集計が別々に回している。通知を出す場所を足したら、
/// そこでもこれを見る。**実際に描いて確かめるのは `mewgit-tui` 側**
/// （`a_notice_of_the_full_width_fits_on_its_own_line`）。
pub(super) const NOTICE_CELLS: usize = 71;

/// 進行中操作のバッジと**必ず並ぶ**通知の予算。いちばん長いバッジ（`CHERRY-PICK abc1234`）の
/// 隣で切れずに出る幅で、**実際に描いて確かめるのは `mewgit-tui` 側**
/// （`a_notice_beside_the_longest_badge_is_cut_short_not_pushed_off` が `あ×24` = 48 を通す）。
/// コンフリクトで止まった案内と、進行中の断りはここに収めないと、言うべき手（`m`）が
/// `…` に削られる（01 の「失敗の案内文」）。
pub(super) const BESIDE_BADGE_CELLS: usize = 48;

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

/// リモートを登録する。`RepoInfo` は `RepoInfoLoaded` で入る値なので直に置く。
pub(super) fn with_remotes(state: &mut AppState, names: &[&str]) {
    state.repo.remotes = names.iter().copied().map(fake_remote).collect();
}

/// `(名前, URL)` からリモート一覧を組む。
///
/// **URL を見せたい側はこちら。** [`with_remotes`] が使う `test_util::fake_remote` の
/// URL は `ssh://git@example.invalid/<名前>.git` でパスが 1 段しかなく、
/// `RemoteUrl::parse` が拒む —— owner / repo を読む経路（`AppState::current_pr` の
/// 座り先）では**黙って `target` に落ちる**ので、その腕を踏めない。
pub(super) fn remotes_of(pairs: &[(&str, &str)]) -> Vec<Remote> {
    pairs
        .iter()
        .map(|(name, url)| Remote {
            name: (*name).to_string(),
            url: (*url).to_string(),
            head_branch: None,
        })
        .collect()
}
