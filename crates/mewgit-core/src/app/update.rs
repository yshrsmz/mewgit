//! 状態遷移。設計は `docs/design/01-app-core.md`。
//!
//! `update` は同期・純粋（`AppState` の変更を除く）。I/O は呼ばず、必要な副作用は
//! [`Cmd`] として返す。
//!
//! ペインごとのキー処理は、対応するペインのタスクと一緒に足す。いまあるのは
//! グラフ・ワーキングツリー・diff。

use std::path::{Path, PathBuf};

use crate::git_exec::{hunk_patch, supports_hunk_patch, ExecError, GitOp};
use crate::git_read::{
    CommitDetail, DiffTarget, FileDiff, FileEntry, FileStatus, GitReadError, Hunk, LogPage, Oid,
    WorktreeSection, WorktreeSnapshot,
};
use crate::graph::LaneState;

use super::{
    AppState, Cmd, DiffCursor, DiffView, ExecDone, GraphLine, JobKind, Key, KeyCode, Modal, Msg,
    NoticeLevel, Pane, RepoSnapshot,
};

/// 起動直後に実行してほしい副作用。
///
/// `runtime::run` がループに入る前に呼ぶ。`main.rs` から呼ぶとテストの外に出てしまう。
/// アカウント解決は、そのタスクでここに足す。
pub fn init(state: &mut AppState) -> Cmd {
    let cmd = Cmd::batch([
        Cmd::LoadRepoInfo,
        Cmd::LoadLog {
            count: state.config.graph.page_size,
        },
        Cmd::LoadStatus,
    ]);
    state.issue(cmd)
}

/// 1 つの [`Msg`] を状態に適用し、実行してほしい副作用を返す。
///
/// 返す `Cmd` は [`AppState::issue`] を通す。同種のジョブが走っているあいだ二重に
/// 発行しないため（01 の「update の原則」）。
pub fn update(state: &mut AppState, msg: Msg) -> Cmd {
    let cmd = update_inner(state, msg);
    state.issue(cmd)
}

/// **続きを頼む判断は 7 箇所にある。** `update` の出口にまとめて「毎回導き直す」形には
/// できない。導出は「一度試して失敗した」を表せないので、失敗のたびに再発行して
/// 「失敗 → 再読込 → 失敗」を回してしまう。
///
/// ログとコミット詳細（01 の「追加ページの読み込み」「コミット詳細の遅延取得」）:
/// `scrolled`（カーソル移動）、`log_loaded` の成功、`commit_detail_loaded` の成功、
/// `Msg::Layout`。
///
/// diff（01 の「diff の遅延取得」）: `focus`（フォーカスの移動。`Tab` も `Enter` も
/// `Esc` もここを通る）、`status_loaded` の `clamp` のあと、`diff_loaded` の出口
/// （対象が違ったときの追いつきと、`DiffState.stale` が残っているときの読み直し）。
fn update_inner(state: &mut AppState, msg: Msg) -> Cmd {
    match msg {
        Msg::Key(key) => update_key(state, key),
        // 端末サイズはレイアウトの分岐にしか使わない。グラフの行数はこのあと
        // ランタイムが `Msg::Layout` で入れる（01 の「ランタイム」）。
        Msg::Resize { term } => {
            state.term = term;
            Cmd::None
        }
        Msg::Layout { graph_rows } => {
            state.graph.viewport = graph_rows;
            // 縮んだ窓でカーソルが外に出たままにならないようにする。
            state.graph.scroll_into_view();
            // 窓が広がって残りが 1 画面を切ることがある。
            load_more(state)
        }
        Msg::Tick => Cmd::None,

        Msg::LogLoaded(result) => log_loaded(state, result),
        Msg::CommitDetailLoaded { oid, result } => commit_detail_loaded(state, oid, *result),
        Msg::RepoInfoLoaded(result) => repo_info_loaded(state, *result),

        // 以下は対応するマイルストーンのタスクで実装する。ここで `_` を使わないのは、
        // `Msg` に変種を足したときに実装漏れをコンパイルエラーにするため。
        Msg::StatusLoaded(result) => status_loaded(state, *result),
        Msg::DiffLoaded { target, result } => diff_loaded(state, target, *result),
        Msg::ExecFinished(done) => exec_finished(state, *done),
        Msg::ProgressDetected(_) => Cmd::None,
        Msg::AccountResolved(_) => Cmd::None,
        Msg::PrLoaded(_) => Cmd::None,
        Msg::ChecksLoaded(_) => Cmd::None,
        Msg::PrListLoaded(_) => Cmd::None,
    }
}

/// キー入力を `modal` → `focus` の順に振り分ける。
fn update_key(state: &mut AppState, key: Key) -> Cmd {
    // Ctrl-C はモーダルより先に見る。raw mode では SIGINT が届かず、モーダル表示中は
    // `q` が入力文字になる（M2 の CommitMessage）ので、常に効く脱出口が要る。
    // M3 で実行中ジョブのキャンセルを入れたら、ジョブがあるときはそちらを優先する。
    if key == Key::ctrl('c') {
        return Cmd::Quit;
    }

    // モーダル表示中はすべてモーダルが受ける。`q` もここでは終了にならない
    // （`CommitMessage` では入力文字になる。脱出口は上の `Ctrl-C`）。
    if state.modal.is_some() {
        return modal_key(state, key);
    }

    if key == Key::char('q') {
        return Cmd::Quit;
    }

    // フォーカスの巡回はペインより先に見る。ペインごとに書くと、あとから足した
    // ペインが `Tab` を受けずに閉じ込められる。`Shift+Tab` は `BackTab`（修飾なし）に
    // 正規化されている（01 の「キー入力」）。
    match key.code() {
        KeyCode::Tab => return focus(state, state.focus.next()),
        KeyCode::BackTab => return focus(state, state.focus.prev()),
        _ => {}
    }

    match state.focus {
        Pane::Graph => graph_key(state, key),
        Pane::Worktree => worktree_key(state, key),
        Pane::Diff => diff_key(state, key),
        // 各ペインのキー処理は対応するマイルストーンで足す。`Pane::FOCUSABLE` に
        // 入っていないので、いまここには来ない。
        Pane::Branches | Pane::Stash | Pane::Prs => Cmd::None,
    }
}

/// フォーカスを移し、見えるようになった diff が要るなら頼む。
///
/// **フォーカスを動かすキーはすべてここを通す**（`Tab` / `Shift+Tab` の巡回、
/// Worktree の `Enter`、Diff の `Esc`）。腕ごとに `Cmd::LoadDiff` を書くと、あとから
/// ペインを足すたびに同じ特殊ケースが増える（01 の「diff の遅延取得」）。
fn focus(state: &mut AppState, pane: Pane) -> Cmd {
    state.focus = pane;
    load_diff(state)
}

/// 見せている diff が要るなら頼む。
///
/// **同じ対象で一度失敗しているなら出さない。** 出すと「失敗 → 再読込 → 失敗」を回す。
/// 対象が違えば一度も試していないので頼む（01）。
///
/// **同じ対象を既に持っていて、それが直前の書き込みより後のものなら出さない**
/// （01 の「diff のキャッシュと `DiffState.stale`」）。ワーキングツリーの diff は
/// 1 ファイルぶんでもワークツリー全体を走査してから絞るので、`Esc` / `Enter` の
/// たびに走らせるとリーダースレッドが `LoadLog` を待たせる。
fn load_diff(state: &mut AppState) -> Cmd {
    let Some(wanted) = state.wanted_diff() else {
        return Cmd::None;
    };
    if state.diff.failed.as_ref() == Some(&wanted) {
        return Cmd::None;
    }
    if !state.diff.stale && state.diff.fresh_view(Some(&wanted)).is_some() {
        return Cmd::None;
    }

    // **`LoadDiff` が走っていれば `issue` に落とされる。** いま飛んでいる要求は
    // 書き込みより前のものなので、印は残したままにする。`diff_loaded` の出口が
    // 同じここを通るので、届いた時点で頼み直せる（01。`WorktreeState.stale` と同じ形）。
    if !state.jobs.is_running(JobKind::LoadDiff) {
        state.diff.stale = false;
    }
    Cmd::LoadDiff(wanted)
}

/// Diff ペインのキー処理。
///
/// カーソルはハンク単位で、`s` / `u` / `x` はカーソルの指すハンクに効く（07）。
/// 可否の表は Worktree ペインと共通（[`plan`]）。
fn diff_key(state: &mut AppState, key: Key) -> Cmd {
    match key.code() {
        KeyCode::Char('j') | KeyCode::Down => move_hunk(state, 1),
        KeyCode::Char('k') | KeyCode::Up => move_hunk(state, -1),
        KeyCode::Char('s') => act(state, Action::Stage),
        KeyCode::Char('u') => act(state, Action::Unstage),
        KeyCode::Char('x') => act(state, Action::Discard),
        KeyCode::Esc => focus(state, Pane::Worktree),
        _ => Cmd::None,
    }
}

/// diff のカーソルを `delta` ハンク動かす。
///
/// **いま見えている diff だけを動かす。** 別の対象のものが `view` に残っていることが
/// あり（読み込み中・失敗中）、濾さずに動かすと画面に出ていない diff のカーソルが
/// 黙って動く。
fn move_hunk(state: &mut AppState, delta: isize) -> Cmd {
    let wanted = state.wanted_diff();
    if let Some(view) = state.diff.fresh_view_mut(wanted.as_ref()) {
        view.move_cursor(delta);
    }
    Cmd::None
}

/// 届いた diff を積む。
///
/// `JobTracker` の粒度は種別単位なので、要求が飛んでいるあいだの選択の移動は
/// 落とされる。ここで追いつく（`commit_detail_loaded` と同じ形）。
fn diff_loaded(
    state: &mut AppState,
    target: DiffTarget,
    result: Result<Vec<FileDiff>, GitReadError>,
) -> Cmd {
    state.jobs.finish(JobKind::LoadDiff);

    match result {
        Ok(files) => {
            state.diff.failed = None;
            // **届いたのがいま選んでいるファイルのものなら採る。** 無条件に上書きすると、
            // 正しく出ている画面が別のファイルの diff に化ける。
            if state.wanted_diff().as_ref() == Some(&target) {
                // **同じ対象を読み直したときはカーソルを持ち越す。** 書き込みのあとは
                // 同じファイルの diff がもう一度届く。既定に戻すと、選んでいたハンクが
                // 先頭へ飛んでから操作が効くことになる。
                let cursor = state
                    .diff
                    .view
                    .as_ref()
                    .filter(|view| view.target == target)
                    .map_or_else(DiffCursor::default, |view| view.cursor);
                let mut view = DiffView {
                    target,
                    files,
                    cursor,
                };
                // 読み直しでハンクが減っていることがある。選べる位置へ寄せ直す。
                view.move_cursor(0);
                state.diff.view = Some(view);
            }
        }
        Err(err) => {
            state.push_notice(NoticeLevel::Error, err.to_string(), None);
            state.diff.failed = Some(target);
        }
    }

    // **採ったときもここを通る。** 対象が違っていた（選択が動いた / 別の対象が
    // 失敗した）ときは追いつきの再発行、対象が合っていても印（`DiffState.stale`）が
    // 残っていれば読み直し。経路を 1 本にするため出口は 1 つ（01）。
    load_diff(state)
}

/// モーダルのキー処理。表示中の入力はすべてここが受ける。
fn modal_key(state: &mut AppState, key: Key) -> Cmd {
    match state.modal {
        Some(Modal::Confirm { .. }) => confirm_key(state, key),
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

/// グラフペインのキー処理。
///
/// いまは上下移動、下端に近づいたときの追加読み込み、`Enter` の詳細展開。
/// `g` / `G` は別のタスクで足す。
fn graph_key(state: &mut AppState, key: Key) -> Cmd {
    match key.code() {
        KeyCode::Char('j') | KeyCode::Down => scrolled(state, 1),
        KeyCode::Char('k') | KeyCode::Up => scrolled(state, -1),
        KeyCode::Enter => toggle_detail(state),
        _ => Cmd::None,
    }
}

/// カーソルを動かし、続きが要るなら頼む。
///
/// **カーソルを動かすキーはすべてここを通す**（`g` / `G` や `PageDown` を足すときも）。
/// 腕ごとに `load_more` を書くと、足したキーが素通りする。下端で `j` を押したときは
/// カーソルが動かないので、「動いたかどうか」では判定できない。
fn scrolled(state: &mut AppState, delta: isize) -> Cmd {
    state.graph.move_cursor(delta);
    // カーソルを動かすのはユーザーの操作なので、失敗したログの読み直しはここから
    // 始める。自動では出さない（`load_more`）。
    state.graph.load_failed = false;
    Cmd::batch([load_more(state), load_detail(state)])
}

/// 詳細の開閉。開くときは、まだ持っていない詳細を頼む。
///
/// 閉じるときに `detail` は捨てない。同じコミットをもう一度開いたときに読み直さない
/// ようにするため（01 の「コミット詳細の遅延取得」）。
fn toggle_detail(state: &mut AppState) -> Cmd {
    state.graph.expanded = !state.graph.expanded;
    load_detail(state)
}

/// カーソルの指すコミットの詳細が要るなら頼む。
///
/// 畳んでいるあいだは読まない（見えないものを読んでもリーダーを塞ぐだけ）。既に
/// 同じ oid の詳細を持っていれば出さない。
fn load_detail(state: &AppState) -> Cmd {
    if !state.graph.expanded || state.graph.fresh_detail().is_some() {
        return Cmd::None;
    }
    match state.graph.cursor_commit() {
        Some(commit) => Cmd::LoadCommitDetail(commit.oid),
        None => Cmd::None,
    }
}

/// 届いた詳細を積む。
///
/// `JobTracker` の粒度は種別単位なので、要求が飛んでいるあいだのカーソル移動は
/// 落とされる。ここで追いつくことで、素早くスクロールしても最後に止まったコミットに
/// 収束する。
fn commit_detail_loaded(
    state: &mut AppState,
    oid: Oid,
    result: Result<CommitDetail, GitReadError>,
) -> Cmd {
    state.jobs.finish(JobKind::LoadCommitDetail);
    let cursor = state.graph.cursor_commit().map(|commit| commit.oid);

    match result {
        // **いま見ているコミットのものだけを採る。** 飛んでいる要求は 1 つでも、
        // 戻ってきたときにカーソルが別のところにいることはある（動いて戻ってきた
        // ときなど）。無条件に上書きすると、正しいものが出ている画面が
        // 「読み込み中」に戻り、同じものをもう 1 回読みに行く。
        Ok(detail) if cursor == Some(detail.oid) => {
            state.graph.detail = Some(detail);
        }
        Ok(_) => {}
        Err(err) => {
            state.push_notice(NoticeLevel::Error, err.to_string(), None);
            // **失敗したのがいま見ているコミットなら頼み直さない。** 出すと、
            // リポジトリが壊れているあいだ「失敗 → 再読込 → 失敗」を回し続ける
            // （`log_loaded` と同じ）。別のコミットの失敗なら、いま見ているものは
            // まだ一度も試していないので下で頼む。
            if cursor == Some(oid) {
                return Cmd::None;
            }
        }
    }
    load_detail(state)
}

/// 下端に近づいたら次のページを頼む。
///
/// 残りが 1 画面を切ったら出す。同じ種類のジョブが走っているあいだは
/// [`AppState::issue`] が落とすので、ここでは走っているかを見ない
/// （見ると 2 箇所で同じことを判断することになる）。
fn load_more(state: &AppState) -> Cmd {
    let remaining = state.graph.lines.len().saturating_sub(state.graph.cursor);
    // 失敗したあとは行が空なので「残りが 1 画面以下」が必ず真になる。見ないと、
    // 以後のレイアウト変化のたびに読み直しを出す（失敗自体がレイアウトを変える。
    // 行が消えると詳細の領域も消えるため）。
    if state.graph.load_failed || state.graph.loaded_all || remaining > state.graph.viewport {
        return Cmd::None;
    }

    Cmd::LoadLog {
        count: state.config.graph.page_size,
    }
}

/// リポジトリの概要を反映する。
///
/// 一部だけ成功という形にしていないのは、3 つとも同じ ref から導かれる値で、
/// 途中まで新しい状態を持っても意味が無いため。失敗しても通知だけで続ける
/// （ラベルもリモートも無いままグラフは出せる）。
fn repo_info_loaded(state: &mut AppState, result: Result<RepoSnapshot, GitReadError>) -> Cmd {
    state.jobs.finish(JobKind::LoadRepoInfo);

    match result {
        Ok(info) => {
            state.repo.remotes = info.remotes;
            state.repo.head = Some(info.head);
            state.graph.refs = info.refs;
        }
        Err(err) => state.push_notice(NoticeLevel::Warn, err.to_string(), None),
    }
    Cmd::None
}

/// ログのページを積む。
fn log_loaded(state: &mut AppState, result: Result<LogPage, GitReadError>) -> Cmd {
    state.jobs.finish(JobKind::LoadLog);

    let page = match result {
        Ok(page) => page,
        Err(err) => {
            state.push_notice(NoticeLevel::Error, err.to_string(), None);
            // 失敗したセッションは捨てられ、次の読み込みは先頭から来る（02）。
            // 積んだ行を残すと同じコミットが二重に並ぶ。レーンの状態も、もう
            // `lines` に無いコミットを指したままにしない。
            state.graph.lines.clear();
            state.graph.lanes = LaneState::default();
            // `lines` に無いコミットの詳細を残さない。
            state.graph.detail = None;
            state.graph.cursor = 0;
            state.graph.scroll = 0;
            state.graph.loaded_all = false;
            state.graph.load_failed = true;
            return Cmd::None;
        }
    };

    // レーンの割り当ては `graph` の担当。`app` は返ってきた行を並べるだけ。
    let rows = state.graph.lanes.extend(&page.commits);
    state.graph.lines.extend(
        page.commits
            .into_iter()
            .zip(rows)
            .map(|(commit, row)| GraphLine { commit, row }),
    );
    state.graph.loaded_all = page.done;
    state.graph.load_failed = false;
    // 1 ページが窓より短いことがある。埋まるまで続けて読む。1 ページ目が届くまでは
    // カーソルの指すコミットが無いので、展開したままの詳細もここで頼み直す。
    Cmd::batch([load_more(state), load_detail(state)])
}

/// ワーキングツリーペインのキー処理。
///
/// `j` / `k` は区画をまたいで 1 本のリストのように動く（01 の「ワーキングツリー」）。
fn worktree_key(state: &mut AppState, key: Key) -> Cmd {
    match key.code() {
        KeyCode::Char('j') | KeyCode::Down => moved(state, 1),
        KeyCode::Char('k') | KeyCode::Up => moved(state, -1),
        // 左の領域を Diff に差し替える（07）。読むかどうかは `focus` が決める。
        KeyCode::Enter => focus(state, Pane::Diff),
        KeyCode::Char('s') => act(state, Action::Stage),
        KeyCode::Char('u') => act(state, Action::Unstage),
        KeyCode::Char('x') => act(state, Action::Discard),
        _ => Cmd::None,
    }
}

/// ワーキングツリーのカーソルを動かす。
///
/// **カーソルを動かすキーはすべてここを通す。** 動かすと diff の対象が変わるので、
/// 直前の失敗を落として 1 回だけ試し直せるようにする（`GraphState.load_failed` を
/// `scrolled` で落とすのと同じ形。01 の「diff の遅延取得」）。
fn moved(state: &mut AppState, delta: isize) -> Cmd {
    state.worktree.move_cursor(delta);
    state.diff.failed = None;
    Cmd::None
}

/// ワーキングツリーで押せる操作。`(操作, 区画)` の表を 1 本にするための語彙。
#[derive(Debug, Clone, Copy)]
enum Action {
    Stage,
    Unstage,
    Discard,
}

/// 操作の結果としてすること。
enum Plan {
    /// そのまま実行する。
    Exec(GitOp),
    /// 確認を取ってから実行する（不変ルール 5）。
    Confirm {
        title: &'static str,
        body: String,
        op: GitOp,
    },
}

/// コンフリクトした行を断る文言。`plan` と `discard` の両方に出る。
const CONFLICTED: &str = "コンフリクトしたファイルは操作できません";

/// 出しているハンクが書き込みより前のものだったときに断る文言。
///
/// **「待っています」とだけ書かない。** 読み直しに失敗して二度と来ないこともある。
const NOT_LATEST: &str = "最新の差分をまだ読めていません";

/// 操作の単位。
///
/// [`plan`] は `(操作, 区画)` で可否を決め、**`GitOp` はアームごとにこれを見て組む**。
/// 「stage されていません」のような可否の理由は単位が違っても同じ文言なので、表を
/// 2 つに割らない（01 の「ワーキングツリー」）。
///
/// **ハンクは借用で持つ。** パッチを組むのは実際に `GitOp` になるアームだけなので、
/// 断られる組み合わせ（Unstaged で `u` など）でハンクを丸ごとコピーせずに済む。
enum Scope<'a> {
    File {
        status: FileStatus,
        paths: Vec<PathBuf>,
    },
    Hunk {
        path: &'a Path,
        hunk: &'a Hunk,
    },
}

/// 選択行に `action` を適用する。
///
/// Diff ペインではカーソルの指すハンクに効く。**ハンクで表せないファイルでは、同じ
/// キーがファイル単位の操作に落ちる**（07）。
fn act(state: &mut AppState, action: Action) -> Cmd {
    let Some((section, status, paths)) = state
        .worktree
        .selected()
        .map(|(section, entry)| (section, entry.status, paths_of(entry)))
    else {
        return Cmd::None;
    };

    let scope = hunk_scope(state).unwrap_or(Scope::File { status, paths });

    // **出しているハンクが書き込みより前のものなら断る**（01）。そこから組んだパッチは
    // `git apply` のオフセット追従で別のハンクに当たりうる。**ファイル単位に落とさない**
    // （`s` がファイル全体を stage することになる）。
    //
    // 見るのは単位であってペインではない。`focus == Pane::Diff` で書くと、ハンクを
    // 出す場所が増えたときにガードだけが外れる。
    let planned = if matches!(scope, Scope::Hunk { .. }) && state.diff.stale {
        Err(NOT_LATEST.to_string())
    } else {
        plan(action, section, scope)
    };

    match planned {
        Ok(Plan::Exec(op)) => Cmd::Exec(op),
        Ok(Plan::Confirm { title, body, op }) => {
            state.modal = Some(Modal::Confirm {
                title: title.to_string(),
                body,
                on_yes: Cmd::Exec(op),
            });
            Cmd::None
        }
        // 黙って何も起きないと、キーが効いていないのか対象が悪いのかが判らない。
        Err(reason) => {
            state.push_notice(NoticeLevel::Warn, reason.to_string(), None);
            Cmd::None
        }
    }
}

/// `(操作, 区画)` から出す `GitOp` を決める。できないときはその理由。
///
/// **判定表はここ 1 つ。** ファイル単位もハンク単位もここを通る。可否の理由は単位が
/// 違っても同じ文言なので、表を 2 つに割らない。単位の違いは [`Scope`] が運び、
/// `GitOp` はアームごとにそれを見て組む。網羅 `match` なので、`WorktreeSection` に
/// 区画を足すと実装漏れがコンパイルエラーになる。
///
/// discard だけは変種の選び方が単位ごとに違うので [`discard`] に分ける。
///
/// **パッチを組むのはここ。** 断られる組み合わせでハンクを丸ごとコピーしないため。
/// 組めなかった名前も「できない理由」の 1 つとして同じ `Err` から出る。
fn plan(action: Action, section: WorktreeSection, scope: Scope<'_>) -> Result<Plan, String> {
    match (action, section) {
        // 解消ビューは MVP 外。`git add` だけ通すと「解消した」と誤解させる。
        (_, WorktreeSection::Conflicted) => Err(CONFLICTED.to_string()),

        (Action::Stage, WorktreeSection::Unstaged | WorktreeSection::Untracked) => {
            Ok(Plan::Exec(match scope {
                Scope::File { paths, .. } => GitOp::Stage(paths),
                Scope::Hunk { path, hunk } => GitOp::StageHunk {
                    path: path.to_path_buf(),
                    patch: patch_of(path, hunk)?,
                },
            }))
        }
        (Action::Stage, WorktreeSection::Staged) => Err("すでに stage されています".to_string()),

        (Action::Unstage, WorktreeSection::Staged) => Ok(Plan::Exec(match scope {
            Scope::File { paths, .. } => GitOp::Unstage(paths),
            Scope::Hunk { path, hunk } => GitOp::UnstageHunk {
                path: path.to_path_buf(),
                patch: patch_of(path, hunk)?,
            },
        })),
        (Action::Unstage, WorktreeSection::Unstaged | WorktreeSection::Untracked) => {
            Err("stage されていません".to_string())
        }

        (Action::Discard, section) => discard(section, scope),
    }
}

/// ハンクを `git apply` に渡すパッチに戻す。載せられない名前はその理由。
fn patch_of(path: &Path, hunk: &Hunk) -> Result<Vec<u8>, String> {
    hunk_patch(path, hunk).map_err(|err| err.to_string())
}

/// discard の `GitOp` を選ぶ。**変種の選び方が単位ごとに違うのでここだけ分かれる。**
///
/// ファイルは 3 変種で、区画と変更種別で決まる（03 の「discard は 3 変種」）。
/// **この判断はここにしか無い。**`to_command` は純粋関数でどちらも見られない。
///
/// **HEAD に無いか（`Added`）は `RepoInfo.head` では判断しない。** `head` は起動時に
/// 1 回読んだきりで、外部でコミットされると古くなる。古い `Unborn` を信じて
/// `DiscardAdded` を出すと、HEAD にあるファイルを `git rm --force` で消してしまう。
/// `FileStatus` は `LoadStatus` が毎回運んでくるので、そちらで判断する。
///
/// ハンクは Unstaged 限定の `DiscardHunk` 1 つだけ。
fn discard(section: WorktreeSection, scope: Scope<'_>) -> Result<Plan, String> {
    match scope {
        // staged なハンクを捨てるには index とワークツリーの両方に当てることになり、
        // `git apply` は 1 回で片方にしか効かない（03）。2 回に割ると片方だけ通った
        // 中途半端な状態が残るので、先に unstage させる。
        Scope::Hunk { .. } if section == WorktreeSection::Staged => {
            Err("staged なハンクは破棄できません。先に unstage してください".to_string())
        }
        // 確認モーダルにはハンクの見出しも出す。ファイル名だけだとファイル単位の
        // discard と同じ本文になり、どちらを押したのか画面から判らない。
        Scope::Hunk { path, hunk } => Ok(Plan::Confirm {
            title: "ハンクを破棄",
            body: format!(
                "{} の {} をワークツリーから取り消します。元に戻せません。",
                path.display(),
                hunk.header
            ),
            op: GitOp::DiscardHunk {
                path: path.to_path_buf(),
                patch: patch_of(path, hunk)?,
            },
        }),

        Scope::File { status, paths } => {
            let names = names(&paths);
            match section {
                WorktreeSection::Untracked => Ok(Plan::Confirm {
                    title: "untracked なファイルを削除",
                    body: format!("{names} を削除します。元に戻せません。"),
                    op: GitOp::DiscardUntracked(paths),
                }),
                // HEAD に無いファイル。`restore --source=HEAD` はコミットがまだ無いと
                // 落ちるが、`rm --force` は HEAD の有無に依らず同じ結果になる（03）。
                WorktreeSection::Staged if status == FileStatus::Added => Ok(Plan::Confirm {
                    title: "変更を破棄",
                    body: format!("{names} を index からもワークツリーからも削除します。\nまだコミットに含まれていないので、戻す先がありません。元に戻せません。"),
                    op: GitOp::DiscardAdded(paths),
                }),
                WorktreeSection::Staged | WorktreeSection::Unstaged => Ok(Plan::Confirm {
                    title: "変更を破棄",
                    body: format!("{names} を HEAD の状態に戻します。\nstaged の内容も一緒に戻ります。元に戻せません。"),
                    op: GitOp::Discard(paths),
                }),
                // `plan` が先に断っている。文言は 1 箇所に置く。
                WorktreeSection::Conflicted => Err(CONFLICTED.to_string()),
            }
        }
    }
}

/// カーソルの指すハンクを [`Scope::Hunk`] にする。
///
/// **ハンクで表せないものは `None`。** 何が表せるかは
/// [`supports_hunk_patch`](crate::git_exec::supports_hunk_patch) が持つ（パッチを組む
/// 関数の隣。述語をここに写すと、表せる範囲が広がったときに取り残される）。ハンクの
/// 無いファイルも `None` になる。呼び出し側はファイル単位の操作に落とす（07）。
///
/// Diff ペインを見せていないときも `None`。`wanted_diff()` がそこで `None` を返す。
fn hunk_scope(state: &AppState) -> Option<Scope<'_>> {
    let wanted = state.wanted_diff();
    let view = state.diff.fresh_view(wanted.as_ref())?;
    let file = view.files.get(view.cursor.file)?;
    if !supports_hunk_patch(file) {
        return None;
    }

    Some(Scope::Hunk {
        path: &file.path,
        hunk: file.hunks.get(view.cursor.hunk)?,
    })
}

/// `GitOp` に載せるパス。rename は新しい名前と元の名前の両方。
///
/// `git add -- <新> <旧>` は旧パスの削除も index に入る。staged rename の unstage も
/// 両方の名前が要る（02 の `old_path`）。片方だけ渡すと「削除」と「追加」が別々に残る。
fn paths_of(entry: &FileEntry) -> Vec<PathBuf> {
    let mut paths = vec![entry.path.clone()];
    paths.extend(entry.old_path.clone());
    paths
}

/// 確認モーダルに出すパス。**対象をそのまま並べる**（07）。「選択中のファイル」と
/// だけ書くと、カーソルがどこにあるか思い出せないまま `y` を押すことになる。
fn names(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// ワーキングツリーの一覧を入れ替える。
fn status_loaded(state: &mut AppState, result: Result<WorktreeSnapshot, GitReadError>) -> Cmd {
    state.jobs.finish(JobKind::LoadStatus);

    match result {
        Ok(snapshot) => {
            state.worktree.snapshot = snapshot;
            // 選んでいたファイルは別の区画へ移る（stage / unstage）か消えている
            // （discard）。選べる行へ寄せ直す。
            state.worktree.clamp();
        }
        // 読めていた一覧は捨てない。次の読み込みはユーザーの操作から始まるので、
        // 捨てると画面から消えるだけになる（01）。
        Err(err) => state.push_notice(NoticeLevel::Error, err.to_string(), None),
    }

    // 書き込みの直後に頼んだぶんが落とされていたら、ここで頼み直す。
    let again = if std::mem::take(&mut state.worktree.stale) {
        Cmd::LoadStatus
    } else {
        Cmd::None
    };

    // **`clamp` のあとに見る。** 書き込みで選んでいたファイルが別の区画へ移ると
    // diff の対象も変わる（01 の「diff の遅延取得」）。
    Cmd::batch([again, load_diff(state)])
}

/// `git` の実行が終わった。
///
/// 進行中操作は `Runner` が実行直後に再検出したものをそのまま使う（03）。ここから
/// 検出を頼み直さない。
fn exec_finished(state: &mut AppState, done: ExecDone) -> Cmd {
    state.jobs.finish(JobKind::Exec);
    state.progress = done.progress;

    if let Err(err) = &done.result {
        let detail = exec_detail(err);
        state.push_notice(NoticeLevel::Error, err.to_string(), Some(detail));
    }

    // **走っている読み取りは、この書き込みより前に頼まれたものなので届いても古い。**
    // その場合 `issue` は下の `LoadStatus` を落とすので、届いた時点で頼み直せるよう
    // 印を付ける（01 の「書き込みのあとの status」）。`CommitDetailLoaded` の
    // 追いつきとは形が違う。あちらは届いた値の oid から鮮度を導けるが、
    // `WorktreeSnapshot` には「いつの状態か」を示す鍵が無く、到着時には判定できない。
    state.worktree.stale = state.jobs.is_running(JobKind::LoadStatus);
    // **持っている diff は対象が同じでも中身が古い。** ハンク単位で stage すると
    // `IndexVsWorktree(a.txt)` のまま中身が減る。対象は見ない（この時点では `clamp`
    // がまだで、いま選ばれているファイルが読み直しの対象になるとは限らない。01）。
    state.diff.stale = true;
    // 成否は問わない。失敗しても index が途中まで動いていることがある。
    Cmd::LoadStatus
}

/// 通知に添える詳細。実行した argv と stderr を握りつぶさない（不変ルール 4）。
fn exec_detail(err: &ExecError) -> String {
    let mut detail = err.command_line();
    if let ExecError::Failed { stderr, .. } = err {
        let stderr = stderr.trim_end();
        if !stderr.is_empty() {
            detail.push('\n');
            detail.push_str(stderr);
        }
    }
    detail
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Job, Modal, RepoInfo, TermSize};
    use crate::config::Config;
    use crate::git_exec::{ExecErrorKind, ExecOutput};
    use crate::git_read::{Commit, HeadInfo, Hunk, LineOrigin, RefIndex, RefLabel, Remote};
    use crate::test_util::{
        fake_commit, fake_commit_with_parents, fake_detail, fake_entry, fake_file_diff, fake_hunk,
        fake_oid, fake_snapshot,
    };

    /// グラフに使える行数。`ui` が決めた値が [`Msg::Resize`] で来る想定。
    const ROWS: usize = 20;

    fn state() -> AppState {
        let mut state = AppState::new(RepoInfo::default(), Config::default());
        state.graph.viewport = ROWS;
        state
    }

    /// `n` 番目のコミット。oid は `n` から作るので、256 件以上は重なる。
    fn commit(n: usize) -> Commit {
        fake_commit(n as u8, &format!("commit {n}"))
    }

    /// `n` 件のコミットを積んだ状態。ログは読み切っている。
    fn state_with_commits(n: usize) -> AppState {
        state_with_page(n, true)
    }

    /// `n` 件を 1 ページとして積んだ状態。`done` は「これ以上無い」。
    fn state_with_page(n: usize, done: bool) -> AppState {
        let mut state = state();
        let page = LogPage {
            commits: (0..n).map(commit).collect(),
            done,
        };
        let _ = update(&mut state, Msg::LogLoaded(Ok(page)));
        state
    }

    /// worktree ペインにフォーカスし、その区画の先頭を選んだ状態。
    fn worktree_state(snapshot: WorktreeSnapshot, section: WorktreeSection) -> AppState {
        let mut state = state();
        let _ = update(&mut state, Msg::StatusLoaded(Box::new(Ok(snapshot))));
        state.focus = Pane::Worktree;
        state.worktree.section = section;
        state.worktree.cursor = 0;
        state
    }

    fn press(state: &mut AppState, c: char) -> Cmd {
        update(state, Msg::Key(Key::char(c)))
    }

    fn press_code(state: &mut AppState, code: KeyCode) -> Cmd {
        update(state, Msg::Key(Key::new(code)))
    }

    /// ワーキングツリー側の diff の対象。`assert_eq!` を短くするため。
    fn unstaged_diff(path: &str) -> DiffTarget {
        DiffTarget::IndexVsWorktree(Some(path.into()))
    }

    /// 1 ファイルに `hunks` 個のハンクを持つ diff。
    fn fake_diff(path: &str, hunks: usize) -> Vec<FileDiff> {
        let hunks = (0..hunks)
            .map(|n| fake_hunk(&format!("@@ -{n},1 +{n},1 @@"), &[]))
            .collect();
        vec![fake_file_diff(path, hunks)]
    }

    fn diff_ok(target: DiffTarget, files: Vec<FileDiff>) -> Msg {
        Msg::DiffLoaded {
            target,
            result: Box::new(Ok(files)),
        }
    }

    fn diff_err(target: DiffTarget) -> Msg {
        Msg::DiffLoaded {
            target,
            result: Box::new(Err(GitReadError::NonUtf8Head)),
        }
    }

    /// Diff ペインを開き、`paths[0]` の diff が `hunks` 個のハンクで届いた状態。
    fn diff_state(paths: &[&str], hunks: usize) -> AppState {
        diff_pane(WorktreeSection::Unstaged, paths, fake_diff(paths[0], hunks))
    }

    /// Diff ペインを開き、`files` が届いた状態。
    ///
    /// **Diff ペインの立ち上げはここ 1 本。** 「worktree の状態を作る → `Enter` →
    /// `diff_ok`」を 2 箇所に書くと、片方だけ直したときに差が出る。
    fn diff_pane(section: WorktreeSection, paths: &[&str], files: Vec<FileDiff>) -> AppState {
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

    #[test]
    fn enter_opens_the_diff_pane_and_asks_for_the_selected_file() {
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt", "b.txt"], &[]),
            WorktreeSection::Unstaged,
        );

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(state.focus, Pane::Diff);
        assert_eq!(cmd, Cmd::LoadDiff(unstaged_diff("a.txt")));
    }

    #[test]
    fn a_staged_row_is_diffed_against_head() {
        // 同じファイルでも区画で対象が変わる。パスだけでは決まらない。
        let mut state =
            worktree_state(fake_snapshot(&["a.txt"], &[], &[]), WorktreeSection::Staged);

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(
            cmd,
            Cmd::LoadDiff(DiffTarget::HeadVsIndex(Some("a.txt".into())))
        );
    }

    #[test]
    fn a_conflicted_row_has_no_diff() {
        // 解消ビューは MVP 外。2 者の diff では表せない。
        let snapshot = WorktreeSnapshot {
            conflicted: vec![fake_entry("c.txt", FileStatus::Conflicted)],
            ..WorktreeSnapshot::default()
        };
        let mut state = worktree_state(snapshot, WorktreeSection::Conflicted);

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(state.focus, Pane::Diff);
        assert_eq!(cmd, Cmd::None);
    }

    #[test]
    fn tab_into_the_diff_pane_asks_for_it_too() {
        // `Tab` の腕に `LoadDiff` を書かず、フォーカスの移動が 1 箇所を通ることの確認。
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );

        let cmd = press_code(&mut state, KeyCode::Tab);

        assert_eq!(state.focus, Pane::Diff);
        assert_eq!(cmd, Cmd::LoadDiff(unstaged_diff("a.txt")));
    }

    #[test]
    fn esc_goes_back_to_the_worktree_without_asking_for_anything() {
        let mut state = diff_state(&["a.txt"], 1);

        let cmd = press_code(&mut state, KeyCode::Esc);

        assert_eq!(state.focus, Pane::Worktree);
        // 見せていないものは読まない。`wanted_diff` が `None` になる。
        assert_eq!(cmd, Cmd::None);
    }

    #[test]
    fn a_diff_for_another_file_is_dropped_and_the_right_one_is_asked_again() {
        // `JobKind` の粒度は種別単位なので、要求が飛んでいるあいだの選択の移動は
        // 落とされる。届いた側で追いつく。
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        let _ = press_code(&mut state, KeyCode::Enter);

        let cmd = update(
            &mut state,
            diff_ok(unstaged_diff("b.txt"), fake_diff("b.txt", 1)),
        );

        assert_eq!(state.diff.view, None);
        assert_eq!(cmd, Cmd::LoadDiff(unstaged_diff("a.txt")));
    }

    #[test]
    fn a_failed_diff_is_not_asked_again_for_the_same_file() {
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        let _ = press_code(&mut state, KeyCode::Enter);

        let cmd = update(&mut state, diff_err(unstaged_diff("a.txt")));

        // 出すと「失敗 → 再読込 → 失敗」を回す。
        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.notices.len(), 1);
        // 出入りしても同じ。
        let _ = press_code(&mut state, KeyCode::Esc);
        assert_eq!(press_code(&mut state, KeyCode::Enter), Cmd::None);
    }

    #[test]
    fn moving_the_cursor_lets_a_failed_file_be_tried_again() {
        // 一律に止めると、失敗した対象が二度と読めなくなる。`GraphState.load_failed` を
        // カーソル移動で落とすのと同じ形。
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt", "b.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        let _ = press_code(&mut state, KeyCode::Enter);
        let _ = update(&mut state, diff_err(unstaged_diff("a.txt")));
        let _ = press_code(&mut state, KeyCode::Esc);

        // 動かして戻る。対象は同じ a.txt。
        let _ = press(&mut state, 'j');
        let _ = press(&mut state, 'k');

        assert_eq!(
            press_code(&mut state, KeyCode::Enter),
            Cmd::LoadDiff(unstaged_diff("a.txt"))
        );
    }

    #[test]
    fn a_failure_for_another_file_does_not_block_the_current_one() {
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        let _ = press_code(&mut state, KeyCode::Enter);

        let cmd = update(&mut state, diff_err(unstaged_diff("b.txt")));

        // a.txt はまだ一度も試していない。塞ぐと「読み込めませんでした」のまま残る。
        assert_eq!(cmd, Cmd::LoadDiff(unstaged_diff("a.txt")));
    }

    #[test]
    fn a_write_that_moves_the_file_to_another_section_reloads_the_diff() {
        // stage すると対象が `IndexVsWorktree` から `HeadVsIndex` に変わる。
        // 判定は `clamp` のあとでないと、古い区画で対象を決めてしまう。
        let mut state = diff_state(&["a.txt"], 1);

        let cmd = update(
            &mut state,
            Msg::StatusLoaded(Box::new(Ok(fake_snapshot(&["a.txt"], &[], &[])))),
        );

        assert_eq!(
            cmd,
            Cmd::LoadDiff(DiffTarget::HeadVsIndex(Some("a.txt".into())))
        );
    }

    #[test]
    fn a_reload_of_the_same_file_keeps_the_hunk_cursor() {
        // 同じ対象の diff がもう一度届くのは書き込みのあと。既定に戻すと、選んで
        // いたハンクが先頭へ飛んでから次の操作が効くことになる。
        let mut state = diff_state(&["a.txt"], 3);
        let _ = press(&mut state, 'j');
        let _ = press(&mut state, 'j');
        assert_eq!(state.diff.view.as_ref().unwrap().cursor.hunk, 2);

        let _ = update(&mut state, exec_done(Ok(fake_output())));
        let _ = update(
            &mut state,
            Msg::StatusLoaded(Box::new(Ok(fake_snapshot(&[], &["a.txt"], &[])))),
        );
        let _ = update(
            &mut state,
            diff_ok(unstaged_diff("a.txt"), fake_diff("a.txt", 3)),
        );

        assert_eq!(state.diff.view.as_ref().unwrap().cursor.hunk, 2);
    }

    #[test]
    fn a_reload_with_fewer_hunks_seats_the_cursor_again() {
        let mut state = diff_state(&["a.txt"], 3);
        let _ = press(&mut state, 'j');
        let _ = press(&mut state, 'j');

        // ハンクが 1 つに減って届く。
        let target = unstaged_diff("a.txt");
        let _ = update(&mut state, diff_ok(target, fake_diff("a.txt", 1)));

        assert_eq!(state.diff.view.as_ref().unwrap().cursor.hunk, 0);
    }

    #[test]
    fn keys_do_not_move_the_cursor_of_a_diff_that_is_not_shown() {
        // 別の対象のものが `view` に残っているあいだ（読み込み中・失敗中）に `j` を
        // 押しても、画面に出ていない diff のカーソルは動かさない。
        let mut state = diff_state(&["a.txt", "b.txt"], 3);
        let _ = press_code(&mut state, KeyCode::Esc);
        let _ = press(&mut state, 'j');
        let _ = press_code(&mut state, KeyCode::Enter);

        // b.txt はまだ届いていない。`view` には a.txt が残っている。
        let _ = press(&mut state, 'j');

        assert_eq!(state.diff.view.as_ref().unwrap().cursor.hunk, 0);
    }

    #[test]
    fn j_and_k_move_the_hunk_cursor_and_stop_at_the_ends() {
        let mut state = diff_state(&["a.txt"], 3);
        assert_eq!(state.diff.view.as_ref().unwrap().cursor.hunk, 0);

        let hunk = |state: &AppState| state.diff.view.as_ref().unwrap().cursor.hunk;
        let _ = press(&mut state, 'j');
        assert_eq!(hunk(&state), 1);
        let _ = press(&mut state, 'j');
        let _ = press(&mut state, 'j');
        assert_eq!(hunk(&state), 2, "下端で止まる");
        let _ = press(&mut state, 'k');
        assert_eq!(hunk(&state), 1);
        let _ = press(&mut state, 'k');
        let _ = press(&mut state, 'k');
        assert_eq!(hunk(&state), 0, "上端で止まる");
    }

    // ---- ハンク単位の stage / unstage / discard -------------------------------

    /// 中身のあるハンク。パッチに戻したときに行が並ぶ。
    fn real_hunk(start: usize) -> Hunk {
        fake_hunk(
            &format!("@@ -{start},2 +{start},2 @@"),
            &[
                (LineOrigin::Context, "ctx"),
                (LineOrigin::Deletion, "old"),
                (LineOrigin::Addition, "new"),
            ],
        )
    }

    /// ハンク 1 つから組んだパッチ。**実装と同じ関数を通す。**
    /// 期待値を手で書くと、パッチの形を変えたときにテストの側だけが古くなる
    /// （形そのものは `git_exec::patch` のユニットテストと `tests/hunk.rs` が見張る）。
    fn patch_of(path: &str, hunk: &Hunk) -> Vec<u8> {
        hunk_patch(std::path::Path::new(path), hunk).expect("パッチを組めませんでした")
    }

    #[test]
    fn s_in_the_diff_pane_stages_only_the_hunk_under_the_cursor() {
        let hunks = vec![real_hunk(1), real_hunk(9)];
        let mut state = diff_pane(
            WorktreeSection::Unstaged,
            &["a.txt"],
            vec![fake_file_diff("a.txt", hunks.clone())],
        );
        // 2 番目のハンクへ移る。1 番目のパッチが出たら、選んでいる場所を見ていない。
        let _ = press(&mut state, 'j');

        let cmd = press(&mut state, 's');

        assert_eq!(
            cmd,
            Cmd::Exec(GitOp::StageHunk {
                path: "a.txt".into(),
                patch: patch_of("a.txt", &hunks[1]),
            })
        );
    }

    #[test]
    fn u_in_the_diff_pane_unstages_only_the_hunk_under_the_cursor() {
        let hunks = vec![real_hunk(1)];
        let mut state = diff_pane(
            WorktreeSection::Staged,
            &["a.txt"],
            vec![fake_file_diff("a.txt", hunks.clone())],
        );

        let cmd = press(&mut state, 'u');

        assert_eq!(
            cmd,
            Cmd::Exec(GitOp::UnstageHunk {
                path: "a.txt".into(),
                patch: patch_of("a.txt", &hunks[0]),
            })
        );
    }

    #[test]
    fn x_in_the_diff_pane_confirms_and_names_the_hunk() {
        let hunks = vec![real_hunk(1)];
        let mut state = diff_pane(
            WorktreeSection::Unstaged,
            &["a.txt"],
            vec![fake_file_diff("a.txt", hunks.clone())],
        );

        assert_eq!(press(&mut state, 'x'), Cmd::None);

        let Some(Modal::Confirm { body, on_yes, .. }) = &state.modal else {
            panic!("確認モーダルが出ていません: {:?}", state.modal);
        };
        // **ファイル名だけでは足りない。** ファイル単位の discard と同じ本文になり、
        // どちらを押したのか画面から判らなくなる。
        assert!(body.contains("a.txt"), "本文に対象が無い: {body}");
        assert!(
            body.contains(&hunks[0].header),
            "本文にハンクが無い: {body}"
        );
        assert_eq!(
            on_yes,
            &Cmd::Exec(GitOp::DiscardHunk {
                path: "a.txt".into(),
                patch: patch_of("a.txt", &hunks[0]),
            })
        );
    }

    #[test]
    fn x_on_a_staged_hunk_is_refused_with_a_reason() {
        let mut state = diff_pane(
            WorktreeSection::Staged,
            &["a.txt"],
            vec![fake_file_diff("a.txt", vec![real_hunk(1)])],
        );

        // index とワークツリーの両方に当てることになるので断る（03）。
        assert_eq!(press(&mut state, 'x'), Cmd::None);
        assert!(state.modal.is_none(), "{:?}", state.modal);
        let notice = state.notices.back().expect("通知が出ていません");
        assert_eq!(notice.level, NoticeLevel::Warn);
        assert!(notice.message.contains("unstage"), "{}", notice.message);
    }

    #[test]
    fn a_file_that_hunks_cannot_describe_falls_back_to_the_whole_file() {
        // ハンク = ファイル全体になるもの。パッチには `--- /dev/null` や
        // `rename from` のヘッダが要るので、ハンク単位の `GitOp` は出さない（07）。
        let untracked = FileDiff {
            status: FileStatus::Untracked,
            ..fake_file_diff("n.txt", vec![real_hunk(1)])
        };
        let mut state = diff_pane(WorktreeSection::Untracked, &["n.txt"], vec![untracked]);

        assert_eq!(
            press(&mut state, 's'),
            Cmd::Exec(GitOp::Stage(vec!["n.txt".into()]))
        );
    }

    #[test]
    fn a_binary_file_falls_back_to_the_whole_file() {
        // `binary` が立つとハンクは空になるが、`hunks` を見るだけでは
        // 「mode だけの変更」と区別が付かない。旗のほうで落とす。
        let binary = FileDiff {
            binary: true,
            ..fake_file_diff("a.png", vec![real_hunk(1)])
        };
        let mut state = diff_pane(WorktreeSection::Unstaged, &["a.png"], vec![binary]);

        assert_eq!(
            press(&mut state, 's'),
            Cmd::Exec(GitOp::Stage(vec!["a.png".into()]))
        );
    }

    #[test]
    fn a_path_that_cannot_go_into_a_patch_is_refused_instead_of_falling_back() {
        // tab を含むパスは `git apply` が名前の終わりと読む（03）。ファイル単位に
        // 落とすと、ハンク 1 つのつもりでファイル全体を stage することになる。
        let mut state = diff_pane(
            WorktreeSection::Unstaged,
            &["a\tb.txt"],
            vec![fake_file_diff("a\tb.txt", vec![real_hunk(1)])],
        );

        assert_eq!(press(&mut state, 's'), Cmd::None);
        assert_eq!(
            state.notices.back().map(|notice| notice.level),
            Some(NoticeLevel::Warn)
        );
    }

    #[test]
    fn keys_are_refused_while_the_shown_diff_is_older_than_the_last_write() {
        let mut state = diff_pane(
            WorktreeSection::Unstaged,
            &["a.txt"],
            vec![fake_file_diff("a.txt", vec![real_hunk(1)])],
        );
        // 書き込みが終わった直後。読み直しはまだ届いていない。
        let _ = update(&mut state, exec_done(Ok(fake_output())));
        assert!(state.diff.stale);

        // 古いハンクから組んだパッチは、同じ内容のハンクが 2 つあると別の場所に当たる。
        for key in ['s', 'u', 'x'] {
            assert_eq!(press(&mut state, key), Cmd::None, "{key}");
            assert!(state.modal.is_none(), "{key}");
        }
        assert_eq!(
            state.notices.back().map(|notice| notice.level),
            Some(NoticeLevel::Warn)
        );
    }

    #[test]
    fn reopening_the_same_file_uses_the_diff_already_in_hand() {
        // 1 ファイルぶんの diff でもワークツリー全体を走査するので、開き直すたびに
        // 読むとリーダーが `LoadLog` を待たせる（01）。
        let mut state = diff_state(&["a.txt"], 1);

        let _ = press_code(&mut state, KeyCode::Esc);
        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::None);
    }

    #[test]
    fn a_write_makes_the_same_target_be_read_again() {
        // ハンク 1 つを stage しても残りが unstaged なら区画は変わらない。**対象が
        // 同じままなのでキャッシュが効いてしまう**のを `DiffState.stale` が止める。
        let mut state = diff_state(&["a.txt"], 2);
        let _ = update(&mut state, exec_done(Ok(fake_output())));

        let cmd = update(
            &mut state,
            Msg::StatusLoaded(Box::new(Ok(fake_snapshot(&[], &["a.txt"], &[])))),
        );

        assert_eq!(cmd, Cmd::LoadDiff(unstaged_diff("a.txt")));
        assert!(!state.diff.stale, "出せたら印は落とす");
    }

    #[test]
    fn a_reply_that_was_asked_for_before_the_write_is_asked_for_again() {
        // 書き込みの時点で `LoadDiff` が走っていると、`status_loaded` が出す
        // `Cmd::LoadDiff` は `issue` に落とされる。飛んでいた返信は書き込みより前の
        // ものなので、それを採ったまま黙って終わると古い diff が残る（01）。
        let mut state = diff_state(&["a.txt"], 2);
        assert!(state.jobs.start(Job {
            kind: JobKind::LoadDiff,
            label: JobKind::LoadDiff.label().to_string(),
        }));

        let _ = update(&mut state, exec_done(Ok(fake_output())));
        let dropped = update(
            &mut state,
            Msg::StatusLoaded(Box::new(Ok(fake_snapshot(&[], &["a.txt"], &[])))),
        );
        assert_eq!(dropped, Cmd::None, "走っているので落とされる");
        assert!(state.diff.stale, "落とされたぶんの印が残る");

        // 書き込みより前に頼まれていた返信が届く。
        let cmd = update(
            &mut state,
            diff_ok(unstaged_diff("a.txt"), fake_diff("a.txt", 2)),
        );

        assert_eq!(cmd, Cmd::LoadDiff(unstaged_diff("a.txt")));
        assert!(!state.diff.stale);
    }

    #[test]
    fn tab_moves_the_focus_and_the_worktree_keys_follow_it() {
        // グラフに居るあいだは `j` がコミットのカーソルを動かす。
        let mut state = state_with_commits(3);
        state.worktree.snapshot = fake_snapshot(&[], &["a.txt", "b.txt"], &[]);
        let _ = press(&mut state, 'j');
        assert_eq!(state.graph.cursor, 1);

        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Tab)));
        assert_eq!(state.focus, Pane::Worktree);

        // 同じ `j` が worktree のカーソルに効く。グラフは動かない。
        let _ = press(&mut state, 'j');
        assert_eq!(state.graph.cursor, 1);
        assert_eq!(state.worktree.cursor, 1);

        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::BackTab)));
        assert_eq!(state.focus, Pane::Graph);
    }

    #[test]
    fn status_loaded_replaces_the_snapshot_and_reseats_the_cursor() {
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt", "b.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        state.worktree.cursor = 1;

        // b.txt を stage した後の状態が届く。
        let _ = update(
            &mut state,
            Msg::StatusLoaded(Box::new(Ok(fake_snapshot(&["b.txt"], &["a.txt"], &[])))),
        );

        assert!(!state.jobs.is_running(JobKind::LoadStatus));
        // 選んでいた行は消えているので、選べる行へ寄る。
        let (_, entry) = state.worktree.selected().expect("選択が空になっています");
        assert_eq!(entry.path.display().to_string(), "a.txt");
    }

    #[test]
    fn status_loaded_keeps_the_previous_list_when_it_fails() {
        let mut state =
            worktree_state(fake_snapshot(&["a.txt"], &[], &[]), WorktreeSection::Staged);
        let _ = state.issue(Cmd::LoadStatus);

        let cmd = update(
            &mut state,
            Msg::StatusLoaded(Box::new(Err(GitReadError::ReaderGone))),
        );

        // 失敗の通り道が無いとジョブが走ったまま残り、以後の LoadStatus が黙って落ちる。
        assert!(!state.jobs.is_running(JobKind::LoadStatus));
        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.worktree.snapshot.staged.len(), 1);
        assert_eq!(
            state.notices.back().map(|notice| notice.level),
            Some(NoticeLevel::Error)
        );
    }

    #[test]
    fn s_stages_the_selected_file_and_u_unstages_it() {
        let mut state = worktree_state(
            fake_snapshot(&["staged.txt"], &["unstaged.txt"], &["new.txt"]),
            WorktreeSection::Unstaged,
        );

        assert_eq!(
            press(&mut state, 's'),
            Cmd::Exec(GitOp::Stage(vec!["unstaged.txt".into()]))
        );

        let mut state = worktree_state(
            fake_snapshot(&["staged.txt"], &["unstaged.txt"], &[]),
            WorktreeSection::Staged,
        );
        assert_eq!(
            press(&mut state, 'u'),
            Cmd::Exec(GitOp::Unstage(vec!["staged.txt".into()]))
        );
    }

    #[test]
    fn a_key_that_does_not_apply_to_the_section_says_why() {
        // 黙って何も起きないと、キーが効いていないのか対象が悪いのかが判らない。
        for (section, key) in [
            (WorktreeSection::Staged, 's'),
            (WorktreeSection::Unstaged, 'u'),
            (WorktreeSection::Untracked, 'u'),
        ] {
            let mut state =
                worktree_state(fake_snapshot(&["a.txt"], &["a.txt"], &["a.txt"]), section);

            assert_eq!(press(&mut state, key), Cmd::None);
            assert!(!state.jobs.is_running(JobKind::Exec));
            assert_eq!(
                state.notices.back().map(|notice| notice.level),
                Some(NoticeLevel::Warn),
                "{section:?} で {key} を押したときの通知がありません"
            );
        }
    }

    #[test]
    fn staging_a_rename_carries_both_names() {
        // 片方だけ渡すと「削除」と「追加」が別々に残る（02 の old_path）。
        let mut state = worktree_state(
            fake_snapshot(&[], &["new.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        state.worktree.snapshot.unstaged[0].old_path = Some("old.txt".into());
        state.worktree.snapshot.unstaged[0].status = FileStatus::Renamed;

        assert_eq!(
            press(&mut state, 's'),
            Cmd::Exec(GitOp::Stage(vec!["new.txt".into(), "old.txt".into()]))
        );
    }

    #[test]
    fn x_confirms_before_discarding_and_names_the_file() {
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );

        // 破壊的操作は必ず確認を通す（不変ルール 5）。押しただけでは実行しない。
        assert_eq!(press(&mut state, 'x'), Cmd::None);

        let Some(Modal::Confirm { body, on_yes, .. }) = &state.modal else {
            panic!("確認モーダルが出ていません: {:?}", state.modal);
        };
        assert!(body.contains("a.txt"), "本文に対象が無い: {body}");
        assert_eq!(on_yes, &Cmd::Exec(GitOp::Discard(vec!["a.txt".into()])));
    }

    #[test]
    fn x_on_an_untracked_file_deletes_it_and_says_so() {
        let mut state = worktree_state(
            fake_snapshot(&[], &[], &["new.txt"]),
            WorktreeSection::Untracked,
        );

        let _ = press(&mut state, 'x');

        let Some(Modal::Confirm { body, on_yes, .. }) = &state.modal else {
            panic!("確認モーダルが出ていません");
        };
        // tracked の discard と同じ文面にしない。こちらはファイルが消える。
        assert!(body.contains("削除"), "本文: {body}");
        assert_eq!(
            on_yes,
            &Cmd::Exec(GitOp::DiscardUntracked(vec!["new.txt".into()]))
        );
    }

    #[test]
    fn x_on_a_file_that_is_not_in_head_uses_rm_instead_of_restore() {
        // `restore --source=HEAD` はコミットが 1 つも無いと解決できずに落ちる。
        // `Added` なら HEAD の有無に依らず `rm --force` が同じ結果になる（03）。
        let mut state =
            worktree_state(fake_snapshot(&["a.txt"], &[], &[]), WorktreeSection::Staged);
        state.worktree.snapshot.staged[0].status = FileStatus::Added;

        let _ = press(&mut state, 'x');

        let Some(Modal::Confirm { on_yes, .. }) = &state.modal else {
            panic!("確認モーダルが出ていません");
        };
        assert_eq!(
            on_yes,
            &Cmd::Exec(GitOp::DiscardAdded(vec!["a.txt".into()]))
        );
    }

    #[test]
    fn a_stale_unborn_head_does_not_turn_discard_into_a_delete() {
        // `repo.head` は起動時に 1 回読んだきりなので、外部でコミットされると古くなる。
        // 古い `Unborn` を信じて `rm --force` を出すと、HEAD にあるファイルが消える。
        // 判断は `LoadStatus` が毎回運んでくる `FileStatus` で行う。
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        state.repo.head = Some(HeadInfo::Unborn {
            name: "main".to_string(),
        });

        let _ = press(&mut state, 'x');

        let Some(Modal::Confirm { on_yes, .. }) = &state.modal else {
            panic!("確認モーダルが出ていません");
        };
        assert_eq!(on_yes, &Cmd::Exec(GitOp::Discard(vec!["a.txt".into()])));
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

        for (section, status) in cases {
            let mut state =
                worktree_state(fake_snapshot(&["a.txt"], &["a.txt"], &["a.txt"]), section);
            state.worktree.snapshot.staged[0].status = status;

            let _ = press(&mut state, 'x');
            let Some(Modal::Confirm { body, .. }) = &state.modal else {
                panic!("確認モーダルが出ていません: {section:?} {status:?}");
            };
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
    fn a_conflicted_row_is_refused_with_a_notice() {
        let mut state = worktree_state(fake_snapshot(&[], &[], &[]), WorktreeSection::Conflicted);
        state.worktree.snapshot.conflicted = vec![fake_entry("both.txt", FileStatus::Modified)];

        for key in ['s', 'u', 'x'] {
            assert_eq!(press(&mut state, key), Cmd::None);
            assert!(state.modal.is_none());
        }
        // 黙って何も起きないと、キーが効いていないのか対象が悪いのか判らない。
        assert_eq!(
            state.notices.back().map(|notice| notice.level),
            Some(NoticeLevel::Warn)
        );
    }

    /// 成功した `git` の出力。中身は見ないので固定でよい。
    fn fake_output() -> ExecOutput {
        ExecOutput {
            argv: vec!["add".into(), "--".into(), "a.txt".into()],
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    /// `git` の実行結果。
    fn exec_done(result: Result<ExecOutput, ExecError>) -> Msg {
        Msg::ExecFinished(Box::new(ExecDone {
            op: GitOp::Stage(vec!["a.txt".into()]),
            result,
            progress: None,
        }))
    }

    #[test]
    fn exec_finished_reloads_the_status() {
        let mut state = state();
        let _ = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));

        let cmd = update(&mut state, exec_done(Ok(fake_output())));

        assert_eq!(cmd, Cmd::LoadStatus);
        assert!(!state.jobs.is_running(JobKind::Exec));
    }

    #[test]
    fn a_failed_exec_keeps_the_argv_and_stderr() {
        // Git CLI のエラーは握りつぶさない（不変ルール 4）。
        let mut state = state();
        let _ = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));

        let cmd = update(
            &mut state,
            exec_done(Err(ExecError::Failed {
                argv: vec!["add".into(), "--".into(), "a.txt".into()],
                code: Some(128),
                stderr: "fatal: pathspec 'a.txt' did not match any files".to_string(),
                kind: ExecErrorKind::Other,
            })),
        );

        // 失敗しても index が途中まで動いていることがあるので読み直す。
        assert_eq!(cmd, Cmd::LoadStatus);
        let notice = state.notices.back().expect("通知がありません");
        assert_eq!(notice.level, NoticeLevel::Error);
        let detail = notice.detail.as_deref().expect("detail がありません");
        assert!(detail.contains("git add -- a.txt"), "detail: {detail}");
        assert!(
            detail.contains("did not match any files"),
            "detail: {detail}"
        );
    }

    #[test]
    fn a_status_dropped_after_a_write_is_asked_again_when_the_older_one_arrives() {
        // 走っている読み取りは書き込みより前に頼まれたものなので、届いても古い。
        let mut state = state();
        let _ = state.issue(Cmd::LoadStatus);
        let _ = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));

        let cmd = update(&mut state, exec_done(Ok(fake_output())));
        // 走っているので落とされる。
        assert_eq!(cmd, Cmd::None);
        assert!(state.worktree.stale);

        let cmd = update(
            &mut state,
            Msg::StatusLoaded(Box::new(Ok(fake_snapshot(&[], &[], &[])))),
        );

        assert_eq!(cmd, Cmd::LoadStatus);
        assert!(!state.worktree.stale);
    }

    #[test]
    fn a_second_write_while_one_runs_is_dropped_with_a_notice() {
        // 黙って落とすと「押したのに何も起きない」になる。キューは M3。
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        assert!(matches!(press(&mut state, 's'), Cmd::Exec(_)));

        assert_eq!(press(&mut state, 's'), Cmd::None);

        let notice = state.notices.back().expect("通知がありません");
        assert_eq!(notice.level, NoticeLevel::Warn);
    }

    #[test]
    fn init_asks_for_the_repository_the_first_page_and_the_status() {
        let mut state = state();

        let cmd = init(&mut state);

        assert_eq!(
            cmd,
            Cmd::Batch(vec![
                Cmd::LoadRepoInfo,
                Cmd::LoadLog {
                    count: state.config.graph.page_size
                },
                Cmd::LoadStatus,
            ])
        );
        assert!(state.jobs.is_running(JobKind::LoadLog));
        assert!(state.jobs.is_running(JobKind::LoadRepoInfo));
        assert!(state.jobs.is_running(JobKind::LoadStatus));
    }

    #[test]
    fn repo_info_fills_in_the_remotes_head_and_ref_labels() {
        let mut state = state();
        let _ = init(&mut state);
        let oid = commit(1).oid;
        let info = RepoSnapshot {
            remotes: vec![Remote {
                name: "origin".to_string(),
                url: "git@github.com:owner/repo.git".to_string(),
            }],
            head: HeadInfo::Branch {
                name: "main".to_string(),
                oid,
            },
            refs: RefIndex::from_iter([(
                oid,
                RefLabel::Local {
                    name: "main".to_string(),
                    is_head: true,
                },
            )]),
        };

        let _ = update(&mut state, Msg::RepoInfoLoaded(Box::new(Ok(info))));

        assert_eq!(state.repo.remotes.len(), 1);
        assert!(state.repo.head.is_some());
        assert_eq!(state.graph.refs.labels(oid).len(), 1);
        assert!(!state.jobs.is_running(JobKind::LoadRepoInfo));
    }

    #[test]
    fn a_failed_repo_info_notices_and_keeps_going() {
        // ラベルもリモートも無いままグラフは出せる。起動は止めない。
        let mut state = state();
        let _ = init(&mut state);

        let _ = update(
            &mut state,
            Msg::RepoInfoLoaded(Box::new(Err(GitReadError::NonUtf8Head))),
        );

        assert_eq!(state.notices.back().unwrap().level, NoticeLevel::Warn);
        assert!(!state.jobs.is_running(JobKind::LoadRepoInfo));
    }

    #[test]
    fn a_second_load_while_one_is_running_is_dropped() {
        // 二重に投げると同じページを 2 回積む。
        let mut state = state();
        let _ = init(&mut state);

        let again = state.issue(Cmd::LoadLog { count: 10 });

        assert_eq!(again, Cmd::None);
    }

    #[test]
    fn batch_only_drops_the_duplicated_cmd() {
        let mut state = state();
        let _ = init(&mut state);

        let cmd = state.issue(Cmd::Batch(vec![
            Cmd::LoadLog { count: 10 },
            Cmd::ResolveAccount,
        ]));

        // `init` が走らせていないので、`ResolveAccount` は残る。
        assert_eq!(cmd, Cmd::ResolveAccount);
    }

    #[test]
    fn log_loaded_appends_lines_and_finishes_the_job() {
        let mut state = state();
        let _ = init(&mut state);

        let page = LogPage {
            commits: vec![commit(1), commit(2)],
            done: true,
        };
        assert_eq!(update(&mut state, Msg::LogLoaded(Ok(page))), Cmd::None);

        let summaries: Vec<&str> = state
            .graph
            .lines
            .iter()
            .map(|line| line.commit.summary.as_str())
            .collect();
        assert_eq!(summaries, ["commit 1", "commit 2"]);
        assert!(state.graph.loaded_all);
        // ジョブを片付けないと、以後の LoadLog が黙って捨てられる。
        assert!(!state.jobs.is_running(JobKind::LoadLog));
    }

    #[test]
    fn a_page_that_does_not_fill_the_window_asks_for_the_next_one() {
        let mut state = state();
        let _ = init(&mut state);

        let page = LogPage {
            commits: vec![commit(1), commit(2)],
            done: false,
        };

        assert_eq!(
            update(&mut state, Msg::LogLoaded(Ok(page))),
            Cmd::LoadLog {
                count: state.config.graph.page_size
            }
        );
    }

    #[test]
    fn a_page_that_fills_the_window_leaves_no_job_running() {
        // 続きを頼まないので、`LogLoaded` を受けた時点でジョブは無くなる。
        let state = state_with_page(ROWS + 5, false);

        assert!(!state.jobs.is_running(JobKind::LoadLog));
    }

    #[test]
    fn a_second_page_is_appended_after_the_first() {
        // 2 件目は 1 件目の親。ページをまたいでレーンが続くことは `graph::lane` が
        // 保証しているので、ここで見るのは `update` がその状態を持ち回っていること。
        let mut state = state();
        let first = LogPage {
            commits: vec![fake_commit_with_parents(1, &[2], "commit 1")],
            done: false,
        };
        let _ = update(&mut state, Msg::LogLoaded(Ok(first)));

        let second = LogPage {
            commits: vec![fake_commit_with_parents(2, &[], "commit 2")],
            done: true,
        };
        let _ = update(&mut state, Msg::LogLoaded(Ok(second)));

        assert_eq!(state.graph.lines[1].row.lane, state.graph.lines[0].row.lane);
        assert_eq!(
            state.graph.lines[1].row.color,
            state.graph.lines[0].row.color
        );

        assert_eq!(state.graph.lines.len(), 2);
        assert!(state.graph.loaded_all);
    }

    #[test]
    fn a_failed_load_notices_and_drops_what_was_stacked() {
        // 失敗したセッションは捨てられ、次の読み込みは先頭から来る（02）。
        // 積んだ行を残すと同じコミットが二重に並ぶ。
        let mut state = state_with_commits(3);
        let _ = init(&mut state);

        let cmd = update(&mut state, Msg::LogLoaded(Err(GitReadError::NonUtf8Head)));

        // ここから読み直しを頼まない。行を捨てて `cursor` を 0 に戻した直後なので、
        // 出すとリーダーが壊れているあいだ「失敗 → 再読込 → 失敗」を回し続ける。
        assert_eq!(cmd, Cmd::None);
        assert!(state.graph.lines.is_empty());
        assert_eq!(state.graph.lanes, LaneState::default());
        assert_eq!(state.graph.cursor, 0);
        assert!(!state.graph.loaded_all);
        assert!(!state.jobs.is_running(JobKind::LoadLog));
        assert_eq!(state.notices.back().unwrap().level, NoticeLevel::Error);
    }

    #[test]
    fn j_and_k_move_the_graph_cursor() {
        let mut state = state_with_commits(3);

        let _ = update(&mut state, Msg::Key(Key::char('j')));
        assert_eq!(state.graph.cursor, 1);

        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Down)));
        assert_eq!(state.graph.cursor, 2);

        let _ = update(&mut state, Msg::Key(Key::char('k')));
        assert_eq!(state.graph.cursor, 1);

        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Up)));
        assert_eq!(state.graph.cursor, 0);
    }

    #[test]
    fn the_cursor_stops_at_both_ends() {
        let mut state = state_with_commits(2);

        let _ = update(&mut state, Msg::Key(Key::char('k')));
        assert_eq!(state.graph.cursor, 0);

        for _ in 0..5 {
            let _ = update(&mut state, Msg::Key(Key::char('j')));
        }
        assert_eq!(state.graph.cursor, 1);
    }

    #[test]
    fn moving_the_cursor_on_an_empty_graph_does_nothing() {
        let mut state = state();

        let _ = update(&mut state, Msg::Key(Key::char('j')));

        assert_eq!(state.graph.cursor, 0);
        assert_eq!(state.graph.scroll, 0);
    }

    #[test]
    fn the_view_follows_the_cursor_past_the_bottom() {
        // 端末 24 行 = グラフ 20 行。21 行目に降りるとスクロールが 1 進む。
        let mut state = state_with_commits(30);

        for _ in 0..20 {
            let _ = update(&mut state, Msg::Key(Key::char('j')));
        }

        assert_eq!(state.graph.cursor, 20);
        assert_eq!(state.graph.scroll, 1);
    }

    #[test]
    fn widening_the_terminal_pulls_the_view_back_to_the_end() {
        // 末尾に貼り付いたまま端末を大きくすると、カーソルは窓の中にいるので
        // スクロールが動かず、上に行があるのに空白が残る。
        let mut state = state_with_commits(30);
        state.graph.viewport = 5;
        state.graph.cursor = 29;
        state.graph.scroll_into_view();
        assert_eq!(state.graph.scroll, 25);

        state.graph.viewport = 25;
        state.graph.scroll_into_view();

        assert_eq!(state.graph.scroll, 5);
    }

    #[test]
    fn a_viewport_larger_than_the_history_scrolls_to_the_top() {
        let mut state = state_with_commits(3);
        state.graph.scroll = 2;

        state.graph.scroll_into_view();

        assert_eq!(state.graph.scroll, 0);
    }

    #[test]
    fn the_view_stays_put_while_the_cursor_is_visible() {
        // 下端から 1 つ戻ってもスクロールは動かない。動かすとカーソルが最下行に
        // 貼り付いたまま中身だけが流れる。
        let mut state = state_with_commits(30);
        for _ in 0..20 {
            let _ = update(&mut state, Msg::Key(Key::char('j')));
        }

        let _ = update(&mut state, Msg::Key(Key::char('k')));

        assert_eq!(state.graph.cursor, 19);
        assert_eq!(state.graph.scroll, 1);
    }

    #[test]
    fn q_quits() {
        let mut state = state();
        assert_eq!(update(&mut state, Msg::Key(Key::char('q'))), Cmd::Quit);
    }

    #[test]
    fn ctrl_q_does_not_quit() {
        let mut state = state();
        // 終了は修飾なしの `q` だけ。Ctrl-q で落ちると事故になる。
        assert_eq!(update(&mut state, Msg::Key(Key::ctrl('q'))), Cmd::None);
    }

    #[test]
    fn ctrl_c_quits() {
        let mut state = state();
        assert_eq!(update(&mut state, Msg::Key(Key::ctrl('c'))), Cmd::Quit);
    }

    #[test]
    fn ctrl_c_quits_even_with_a_modal_open() {
        // raw mode では SIGINT が届かず、モーダル中は `q` が入力文字になる。
        // 常に効く脱出口でないと閉じ込められる。
        let mut state = AppState {
            modal: Some(Modal::Help),
            ..state()
        };
        assert_eq!(update(&mut state, Msg::Key(Key::ctrl('c'))), Cmd::Quit);
    }

    #[test]
    fn unhandled_key_does_nothing() {
        let mut state = state();
        assert_eq!(update(&mut state, Msg::Key(Key::char('z'))), Cmd::None);
        assert_eq!(state.focus, Pane::Graph);
        assert!(state.modal.is_none());
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

    #[test]
    fn resize_updates_the_terminal_size_only() {
        // グラフの行数は端末サイズだけからは決まらない（詳細の展開でも変わる）。
        // 入れるのはこのあと来る `Msg::Layout`。
        let mut state = state();
        let term = TermSize {
            width: 120,
            height: 40,
        };

        assert_eq!(update(&mut state, Msg::Resize { term }), Cmd::None);
        assert_eq!(state.term, term);
        assert_eq!(state.graph.viewport, ROWS);
    }

    #[test]
    fn layout_sets_the_viewport_and_asks_for_a_page_when_there_is_room() {
        let mut state = state();
        state.graph.viewport = 0;

        // 1 行も積んでいないので、窓を知った時点で次のページを頼む。
        assert_eq!(
            update(&mut state, Msg::Layout { graph_rows: 36 }),
            Cmd::LoadLog {
                count: state.config.graph.page_size
            }
        );
        assert_eq!(state.graph.viewport, 36);
    }

    #[test]
    fn widening_the_window_asks_for_the_next_page() {
        // 30 行あって窓は 20 行。35 行に広がると残りが 1 画面を切る。
        let mut state = state_with_page(30, false);

        assert_eq!(
            update(&mut state, Msg::Layout { graph_rows: ROWS }),
            Cmd::None
        );

        assert_eq!(
            update(&mut state, Msg::Layout { graph_rows: 35 }),
            Cmd::LoadLog {
                count: state.config.graph.page_size
            }
        );
    }

    #[test]
    fn moving_the_cursor_towards_the_end_asks_for_the_next_page() {
        // 残りが 1 画面を切ったら読む。カーソルが下端に着いてからだと、
        // 読み込みのあいだ動けなくなる。
        // 22 行あって窓は 20 行。カーソルが 2 行目に来た時点で残りが 20 になる。
        let mut state = state_with_page(ROWS + 2, false);

        assert_eq!(update(&mut state, Msg::Key(Key::char('j'))), Cmd::None);
        assert_eq!(
            update(&mut state, Msg::Key(Key::char('j'))),
            Cmd::LoadLog {
                count: state.config.graph.page_size
            }
        );
    }

    #[test]
    fn the_next_page_is_not_asked_for_twice_while_it_is_loading() {
        let mut state = state_with_page(ROWS + 2, false);
        state.graph.cursor = ROWS;
        // 1 ページ目の続きを頼んだ直後の状態を作る。
        let _ = state.issue(Cmd::LoadLog { count: 1 });

        // 下端に着いても `JobTracker` が落とす。`update` を通さないと見えない。
        assert_eq!(update(&mut state, Msg::Key(Key::char('j'))), Cmd::None);
    }

    #[test]
    fn nothing_more_is_asked_for_once_the_log_is_exhausted() {
        let mut state = state_with_commits(3);
        state.graph.cursor = 2;

        assert!(state.graph.loaded_all);
        assert_eq!(update(&mut state, Msg::Key(Key::char('j'))), Cmd::None);
    }

    #[test]
    fn a_smaller_viewport_pulls_the_cursor_back_into_view() {
        // 縮んだ窓でカーソルが外に残ると、選択行が見えないまま操作することになる。
        // 端末を縮めたときと、詳細を展開してグラフが縮んだときの両方で起きる。
        let mut state = state_with_commits(30);
        for _ in 0..25 {
            let _ = update(&mut state, Msg::Key(Key::char('j')));
        }
        assert_eq!(state.graph.cursor, 25);

        let _ = update(&mut state, Msg::Layout { graph_rows: 5 });

        assert_eq!(state.graph.scroll, 21);
    }

    #[test]
    fn tick_does_nothing_yet() {
        let mut state = state();
        assert_eq!(update(&mut state, Msg::Tick), Cmd::None);
    }

    /// カーソルの指すコミットの Oid。
    fn cursor(state: &AppState) -> Oid {
        state
            .graph
            .cursor_commit()
            .expect("カーソルの指す行がありません")
            .oid
    }

    /// `n` 件を積んで詳細を展開し、先頭コミットの詳細が届いた状態。
    fn expanded_with_detail(n: usize) -> AppState {
        let mut state = state_with_commits(n);
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));
        let _ = update(&mut state, detail_ok(0));
        state
    }

    /// 索引 `index` のコミットの詳細が届いた [`Msg`]。
    fn detail_ok(index: u8) -> Msg {
        let detail = fake_detail(index, &format!("commit {index}"));
        Msg::CommitDetailLoaded {
            oid: detail.oid,
            result: Box::new(Ok(detail)),
        }
    }

    /// 索引 `index` のコミットの詳細が失敗した [`Msg`]。
    fn detail_err(index: u8) -> Msg {
        Msg::CommitDetailLoaded {
            oid: fake_oid(index),
            result: Box::new(Err(GitReadError::NonUtf8Head)),
        }
    }

    #[test]
    fn enter_expands_and_asks_for_the_detail() {
        let mut state = state_with_commits(3);

        let cmd = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));

        assert!(state.graph.expanded);
        assert_eq!(cmd, Cmd::LoadCommitDetail(cursor(&state)));
        assert!(state.jobs.is_running(JobKind::LoadCommitDetail));
    }

    #[test]
    fn enter_again_collapses_without_asking_for_anything() {
        let mut state = expanded_with_detail(3);

        let cmd = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));

        assert!(!state.graph.expanded);
        assert_eq!(cmd, Cmd::None);
        // 畳んでも捨てない。同じコミットを開き直したときに読み直さないため。
        assert!(state.graph.detail.is_some());
    }

    #[test]
    fn reopening_the_same_commit_does_not_ask_again() {
        let mut state = expanded_with_detail(3);
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));

        let cmd = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));

        assert!(state.graph.expanded);
        assert_eq!(cmd, Cmd::None);
    }

    #[test]
    fn the_detail_follows_the_cursor_while_expanded() {
        let mut state = expanded_with_detail(3);

        let cmd = update(&mut state, Msg::Key(Key::char('j')));

        assert_eq!(cmd, Cmd::LoadCommitDetail(cursor(&state)));
    }

    #[test]
    fn moving_the_cursor_while_collapsed_asks_for_nothing() {
        // 見えないものを読んでもリーダーを塞ぐだけ。
        let mut state = state_with_commits(3);

        assert_eq!(update(&mut state, Msg::Key(Key::char('j'))), Cmd::None);
        assert!(state.graph.detail.is_none());
    }

    #[test]
    fn a_detail_that_arrives_for_the_wrong_commit_is_asked_for_again() {
        // 要求が飛んでいるあいだのカーソル移動は `JobTracker` に落とされる。
        // 追いつきはここで行う（01 の「コミット詳細の遅延取得」）。
        let mut state = state_with_commits(5);
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));
        // 読み込み中に 2 行動く。この 2 回は落とされる。
        assert_eq!(update(&mut state, Msg::Key(Key::char('j'))), Cmd::None);
        assert_eq!(update(&mut state, Msg::Key(Key::char('j'))), Cmd::None);

        let cmd = update(&mut state, detail_ok(0));

        assert_eq!(cmd, Cmd::LoadCommitDetail(cursor(&state)));
    }

    #[test]
    fn a_detail_for_the_current_commit_settles() {
        let mut state = state_with_commits(3);
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));

        let cmd = update(&mut state, detail_ok(0));

        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.graph.detail.as_ref().unwrap().oid, cursor(&state));
        assert!(!state.jobs.is_running(JobKind::LoadCommitDetail));
    }

    #[test]
    fn a_failed_detail_notices_without_asking_again() {
        // 出すと、リポジトリが壊れているあいだ「失敗 → 再読込 → 失敗」を回し続ける。
        let mut state = state_with_commits(3);
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));

        let cmd = update(&mut state, detail_err(0));

        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.notices.back().unwrap().level, NoticeLevel::Error);
        // 畳まないと、以後の `LoadCommitDetail` が黙って捨てられる。
        assert!(!state.jobs.is_running(JobKind::LoadCommitDetail));
    }

    #[test]
    fn expanding_an_empty_graph_asks_for_nothing() {
        let mut state = state();

        let cmd = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));

        assert!(state.graph.expanded);
        assert_eq!(cmd, Cmd::None);
    }

    #[test]
    fn the_first_page_brings_the_detail_for_an_already_expanded_graph() {
        // 起動直後に展開すると、まだ 1 行も無いのでカーソルの指すコミットが無い。
        // ページが届いたところで頼み直す。
        let mut state = state();
        assert_eq!(
            update(&mut state, Msg::Key(Key::new(KeyCode::Enter))),
            Cmd::None
        );

        let page = LogPage {
            commits: vec![commit(1)],
            done: true,
        };
        let cmd = update(&mut state, Msg::LogLoaded(Ok(page)));

        assert_eq!(cmd, Cmd::LoadCommitDetail(cursor(&state)));
    }

    #[test]
    fn a_failed_page_drops_the_detail_with_the_lines() {
        // `lines` に無いコミットの詳細を残さない。
        let mut state = expanded_with_detail(3);
        let _ = init(&mut state);

        let _ = update(&mut state, Msg::LogLoaded(Err(GitReadError::NonUtf8Head)));

        assert!(state.graph.detail.is_none());
    }

    #[test]
    fn a_detail_that_arrives_after_the_cursor_came_back_is_dropped() {
        // カーソルが動いて戻ってくると、飛んでいる要求の返りは古いものになる。
        // 上書きすると、正しいものが出ている画面が「読み込み中」に戻り、
        // 同じものをもう 1 回読みに行く。
        let mut state = expanded_with_detail(3);
        // 2 件目を頼んだ直後に戻る。戻りは `JobTracker` に落とされる。
        assert_eq!(
            update(&mut state, Msg::Key(Key::char('j'))),
            Cmd::LoadCommitDetail(fake_oid(1))
        );
        assert_eq!(update(&mut state, Msg::Key(Key::char('k'))), Cmd::None);

        let cmd = update(&mut state, detail_ok(1));

        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.graph.detail.as_ref().unwrap().oid, cursor(&state));
    }

    #[test]
    fn a_failure_for_another_commit_still_asks_for_the_one_on_screen() {
        // 「`Err` からは頼み直さない」のは同じコミットの話。カーソルが動いた先の
        // コミットはまだ一度も試していないので、放っておくと
        // 「詳細を読み込めませんでした」のまま止まる。
        let mut state = state_with_commits(3);
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));
        // 読み込み中に 1 行動く。この移動は `JobTracker` に落とされる。
        assert_eq!(update(&mut state, Msg::Key(Key::char('j'))), Cmd::None);

        let cmd = update(&mut state, detail_err(0));

        assert_eq!(cmd, Cmd::LoadCommitDetail(cursor(&state)));
        assert_eq!(state.notices.back().unwrap().level, NoticeLevel::Error);
    }

    #[test]
    fn a_failed_page_is_not_reloaded_by_a_layout_change() {
        // 失敗すると行を捨てるので「残りが 1 画面以下」が必ず真になる。見ないと、
        // 以後のレイアウト変化のたびに読み直しを出す。
        let mut state = state_with_page(3, false);
        let _ = init(&mut state);
        let _ = update(&mut state, Msg::LogLoaded(Err(GitReadError::NonUtf8Head)));

        assert_eq!(
            update(&mut state, Msg::Layout { graph_rows: 10 }),
            Cmd::None
        );
        assert_eq!(update(&mut state, Msg::Tick), Cmd::None);
    }

    #[test]
    fn moving_the_cursor_restarts_a_failed_load() {
        // 自動では出さないが、ユーザーの操作からは始める。
        let mut state = state_with_page(3, false);
        let _ = init(&mut state);
        let _ = update(&mut state, Msg::LogLoaded(Err(GitReadError::NonUtf8Head)));

        assert_eq!(
            update(&mut state, Msg::Key(Key::char('j'))),
            Cmd::LoadLog {
                count: state.config.graph.page_size
            }
        );
    }

    #[test]
    fn a_successful_page_clears_the_failure() {
        let mut state = state_with_page(3, false);
        let _ = init(&mut state);
        let _ = update(&mut state, Msg::LogLoaded(Err(GitReadError::NonUtf8Head)));
        assert!(state.graph.load_failed);

        let page = LogPage {
            commits: vec![commit(1)],
            done: false,
        };
        let _ = update(&mut state, Msg::LogLoaded(Ok(page)));

        assert!(!state.graph.load_failed);
    }
}
