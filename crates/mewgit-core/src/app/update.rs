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
    short_oid, Branch, CommitDetail, DiffTarget, FileDiff, FileEntry, FileStatus, GitReadError,
    Hunk, LogPage, Oid, WorktreeSection, WorktreeSnapshot,
};

use super::{
    AppState, Cmd, CommitFiles, DiffCursor, DiffView, ExecDone, Freshness, GraphLine,
    GraphSelection, InputIntent, JobKind, Key, KeyCode, Modal, Motion, Msg, NoticeLevel, Pane,
    RepoSnapshot, TextBuffer,
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
            // まだ何も読んでいないので、セッションを捨てる必要が無い。
            restart: false,
        },
        Cmd::LoadStatus,
        Cmd::LoadBranches,
    ]);
    state.issue(cmd)
}

/// 1 つの [`Msg`] を状態に適用し、実行してほしい副作用を返す。
///
/// 返す `Cmd` は [`AppState::issue`] を通す。同種のジョブが走っているあいだ二重に
/// 発行しないため（01 の「update の原則」）。
pub fn update(state: &mut AppState, msg: Msg) -> Cmd {
    let cmd = update_inner(state, msg);
    // **候補から外れた `focus` はここで座り直させる**（01 の「Pane と Modal」）。
    // グラフの選択が変わる経路はカーソルの移動だけではない。commit のあとの読み直しは
    // `GraphState::restart` でカーソルを WIP 行に戻すので、コミット詳細を見たまま
    // 候補から外れる。経路ごとに書くと足し忘れが静かに通るので、`issue` と同じく
    // 出口 1 箇所で診る。
    state.reseat_focus();
    state.issue(cmd)
}

/// **続きを頼む判断は 10 箇所にある。** いまはそれぞれの経路に置いてある。
///
/// **「導出では失敗を表せない」はもう理由にならない。** 4 つの loader はどれも失敗を
/// 状態に持つようになったので（01 の「失敗の記憶」）、`focus` の座り直しと同じく
/// `update` の出口で毎回導き直せる形になっている。畳むかどうかは M3 で `JobTracker` に
/// 世代を入れるときに決める（`Freshness` と `WorktreeState.stale` も同じ判断の材料）。
/// **それまでは、読む状態を書き換えた経路がここに載っていることを保つ。**
///
/// ログ（01 の「追加ページの読み込み」）: `scrolled`（カーソル移動）、`log_loaded` の
/// 成功、`Msg::Layout`。
///
/// コミットの詳細とファイル一覧（01 の「コミット詳細の遅延取得」「コミットのファイル
/// 一覧」）: `scrolled`、`commit_detail_loaded` の出口、`commit_files_loaded` の出口。
/// **この 3 つはどれも `load_commit` を呼ぶ**（詳細と一覧を別々に配らない）。
///
/// ログの読み直し（01 の「ログの読み直し」）: `exec_finished`（書き込みで HEAD が
/// 動きうる）、`log_loaded` の出口（`issue` に落とされたぶんの追いつき）。
///
/// diff（01 の「diff の遅延取得」）: `focus`（フォーカスの移動。`Tab` も `Enter` も
/// `Esc` もここを通る）、`status_loaded` の `clamp` のあと、`diff_loaded` の出口
/// （対象が違ったときの追いつきと、書き込みより前の diff しか無いときの読み直し）、
/// `commit_files_loaded` の出口（一覧が届くまで対象が決まらないため）。
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
        Msg::Paste(text) => paste(state, &text),
        Msg::Tick => Cmd::None,

        Msg::LogLoaded { restart, result } => log_loaded(state, restart, result),
        Msg::CommitDetailLoaded { oid, result } => commit_detail_loaded(state, oid, *result),
        Msg::CommitFilesLoaded { oid, result } => commit_files_loaded(state, oid, *result),
        Msg::HeadMessageLoaded(result) => head_message_loaded(state, result),
        Msg::RepoInfoLoaded(result) => repo_info_loaded(state, *result),

        // 以下は対応するマイルストーンのタスクで実装する。ここで `_` を使わないのは、
        // `Msg` に変種を足したときに実装漏れをコンパイルエラーにするため。
        Msg::StatusLoaded(result) => status_loaded(state, *result),
        Msg::BranchesLoaded(result) => branches_loaded(state, *result),
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
        KeyCode::Tab => return focus(state, state.next_focus()),
        KeyCode::BackTab => return focus(state, state.prev_focus()),
        _ => {}
    }

    match state.focus {
        Pane::Graph => graph_key(state, key),
        Pane::Branches => branches_key(state, key),
        Pane::Worktree => worktree_key(state, key),
        Pane::CommitDetail => commit_detail_key(state, key),
        Pane::Diff => diff_key(state, key),
        // 各ペインのキー処理は対応するマイルストーンで足す。巡回の候補
        // （`AppState::focus_candidates`）に入っていないので、いまここには来ない。
        Pane::Stash | Pane::Prs => Cmd::None,
    }
}

/// 端末からの貼り付け。
///
/// **受けるのは編集中のモーダルだけで、それ以外では捨てる。** ペインには貼り付けの
/// 行き先が無く、`Msg::Key` に割り直すと bracketed paste を有効にした意味が消える
/// （01 の「コミットメッセージの編集」）。
fn paste(state: &mut AppState, text: &str) -> Cmd {
    match state.modal.as_mut() {
        Some(Modal::CommitMessage { buf, .. }) => buf.insert_text(text),
        // **1 行入力では改行を除いて 1 行につなぐ**（01 の「1 行入力」）。落とすと
        // 「末尾に改行が付いただけ」の正当な貼り付けまで拒み、1 行目だけを採ると
        // 残りが黙って消える。つないだ結果は入力欄にそのまま出ていて、`Enter` を
        // 押すまで git は走らない。**改行を落とすのは `insert_line` の担当**で、
        // ここで `replace` すると同じ判断が 2 箇所に割れる。
        Some(Modal::Input { buf, .. }) => buf.insert_line(text),
        _ => {}
    }
    Cmd::None
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
/// （01 の「diff のキャッシュと鮮度」）。ワーキングツリーの diff は
/// 1 ファイルぶんでもワークツリー全体を走査してから絞るので、`Esc` / `Enter` の
/// たびに走らせるとリーダースレッドが `LoadLog` を待たせる。
fn load_diff(state: &mut AppState) -> Cmd {
    let Some(wanted) = state.wanted_diff() else {
        return Cmd::None;
    };
    if state.diff.failed.as_ref() == Some(&wanted) {
        return Cmd::None;
    }
    // 書き込みより後の diff を既に持っているなら読み直さない。
    if state.diff.freshness == Freshness::Fresh && state.diff.fresh_view(Some(&wanted)).is_some() {
        return Cmd::None;
    }

    // **`LoadDiff` が走っていれば `issue` に落とされる。** いま飛んでいる要求は
    // 書き込みより前のものなので `Stale` のままにする。`diff_loaded` の出口が同じ
    // ここを通るので、届いた時点で頼み直せる（01。`WorktreeState.stale` と同じ形）。
    if state.diff.freshness == Freshness::Stale && !state.jobs.is_running(JobKind::LoadDiff) {
        state.diff.freshness = Freshness::Asked;
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
        // **戻り先は `Enter` の行き先と同じ 1 つの関数から出す**
        // （`GraphSelection::right_pane`）。WIP 行なら Working tree、コミットなら
        // コミット詳細。別々に書くと、来た側と違うペインに戻る腕ができる。
        KeyCode::Esc => focus(state, state.graph.selection().right_pane()),
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

    // **この返信が書き込みより後に頼んだものか。** `Freshness::Asked` はいま届いた
    // 1 通を指している（`Asked` のあいだ `LoadDiff` はちょうど 1 つしか飛ばない）。
    let asked = state.diff.freshness == Freshness::Asked;
    let mut stored = false;

    match result {
        Ok(files) => {
            state.diff.failed = None;
            // **採るのは「いま選んでいるファイルのもの」かつ「書き込みより後のもの」
            // だけ。** 対象を見ないと正しく出ている画面が別のファイルの diff に化け、
            // 鮮度を見ないと書き込み前の中身に戻る（`Stale` = この返信は書き込みより
            // 前に頼まれたもの）。
            if state.diff.freshness != Freshness::Stale
                && state.wanted_diff().as_ref() == Some(&target)
            {
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
                stored = true;
            }
        }
        Err(err) => {
            state.push_notice(NoticeLevel::Error, err.to_string(), None);
            state.diff.failed = Some(target);
        }
    }

    // **`Asked` はここで必ず倒す。** 倒さないと、`Esc` で対象が消えたり読み取りが
    // 失敗したりしたときに「頼んである」まま取り残され、以後の書き込みが
    // `Stale` を立て直すまで古い diff を新しいものとして扱う。新しくなったと
    // 言えるのは、実際に採れたときだけ。
    if asked {
        state.diff.freshness = if stored {
            Freshness::Fresh
        } else {
            Freshness::Stale
        };
    }

    // **採ったときもここを通る。** 対象が違っていた（選択が動いた / 別の対象が
    // 失敗した）ときは追いつきの再発行、まだ書き込みより前の diff しか無ければ
    // 読み直し。経路を 1 本にするため出口は 1 つ（01）。
    load_diff(state)
}

/// モーダルのキー処理。表示中の入力はすべてここが受ける。
fn modal_key(state: &mut AppState, key: Key) -> Cmd {
    match state.modal {
        Some(Modal::Confirm { .. }) => confirm_key(state, key),
        Some(Modal::CommitMessage { .. }) => commit_message_key(state, key),
        Some(Modal::Input { .. }) => input_key(state, key),
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
/// いまは上下移動、下端に近づいたときの追加読み込み、`Enter` で右ペインへ移る。
/// `g` / `G` は別のタスクで足す。
fn graph_key(state: &mut AppState, key: Key) -> Cmd {
    match key.code() {
        KeyCode::Char('j') | KeyCode::Down => scrolled(state, 1),
        KeyCode::Char('k') | KeyCode::Up => scrolled(state, -1),
        // 右の領域に移る。行き先は選択で決まる（`GraphSelection::right_pane`）。
        // Worktree の `Enter` → Diff と同じ形で、`Esc` で戻る。
        KeyCode::Enter => focus(state, state.graph.selection().right_pane()),
        // **カーソルの指すコミットを起点にブランチを作る**（07 の「Graph ペイン」）。
        // WIP 行なら起点は無し（= HEAD）。**Oid を渡すのは、その行に何のラベルが
        // 乗っていても同じ結果になるから** — タグから作りたい人はそのタグの行を選ぶ。
        KeyCode::Char('n') => {
            let start = state.graph.cursor_commit().map(|commit| commit.oid);
            open_branch_create(state, start)
        }
        _ => Cmd::None,
    }
}

/// コミット詳細ペインのキー処理。
///
/// カーソルはファイル一覧の行を指す。`Enter` で左を `DiffTarget::Commit` の diff に
/// 差し替える（07 の「CommitDetail ペイン」）。
fn commit_detail_key(state: &mut AppState, key: Key) -> Cmd {
    match key.code() {
        KeyCode::Char('j') | KeyCode::Down => move_file(state, 1),
        KeyCode::Char('k') | KeyCode::Up => move_file(state, -1),
        KeyCode::Enter => focus(state, Pane::Diff),
        KeyCode::Esc => focus(state, Pane::Graph),
        _ => Cmd::None,
    }
}

/// Branches ペインのキー処理。
fn branches_key(state: &mut AppState, key: Key) -> Cmd {
    match key.code() {
        KeyCode::Char('j') | KeyCode::Down => {
            state.branches.move_cursor(1);
            Cmd::None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            state.branches.move_cursor(-1);
            Cmd::None
        }
        KeyCode::Enter => switch_branch(state),
        KeyCode::Char('d') => delete_branch(state, false),
        KeyCode::Char('D') => delete_branch(state, true),
        // **起点は HEAD。** このペインは起点の情報を持たない（カーソル行を起点に
        // すると、一覧を眺めているだけのつもりで別のブランチから生やすことになる）。
        // 起点を選んで作るのは Graph ペインの `n`（07 の「Branches ペイン」）。
        KeyCode::Char('n') => open_branch_create(state, None),
        KeyCode::Char('r') => rename_branch(state),
        // **`Tab` でしか入れないペインにも出口を用意する**（07）。戻り先を
        // `right_pane()` にしないのは、このペインが左の選択と無関係だから。
        KeyCode::Esc => focus(state, Pane::Graph),
        _ => Cmd::None,
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
fn open_branch_create(state: &mut AppState, start: Option<Oid>) -> Cmd {
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

/// 入力モーダルを開く。
///
/// **`Modal::Input` を組むのはここ 1 本**（[`confirm`] と同じ形）。フィールドを足せば
/// リテラルは全部コンパイルエラーになるので、揃えたいのは**開き方**のほう —
/// 「`Cmd` を返さずに `modal` へ置くだけ」を各キーの腕に散らさない。
fn input(
    state: &mut AppState,
    title: String,
    prompt: String,
    buf: TextBuffer,
    intent: InputIntent,
) -> Cmd {
    state.modal = Some(Modal::Input {
        title,
        prompt,
        buf,
        intent,
    });
    Cmd::None
}

/// 破壊的な `GitOp` を確認モーダルに載せる（不変ルール 5）。
///
/// **`Modal::Confirm` を組むのはここ 1 本。** 2 箇所で組むと、フィールドが増えたときに
/// 片方だけ直した状態が生まれる（`Modal` の構築は網羅を強制しない）。
fn confirm(state: &mut AppState, title: &str, body: String, op: GitOp) -> Cmd {
    state.modal = Some(Modal::Confirm {
        title: title.to_string(),
        body,
        on_yes: Cmd::Exec(op),
    });
    Cmd::None
}

/// ファイル一覧のカーソルを `delta` 行動かす。
///
/// **動かすと diff の対象が変わるので、直前の失敗を落とす**（ワーキングツリーの
/// [`moved`] と同じ形）。移動して戻ってくれば 1 回だけ試し直せる。
///
/// **動かすのは出ている一覧だけ。** 別のコミットのものが残っていることがあり
/// （読み込み中・失敗中）、濾さずに動かすと画面に出ていない一覧のカーソルが黙って動く。
fn move_file(state: &mut AppState, delta: isize) -> Cmd {
    if let Some(files) = state.graph.fresh_files_mut() {
        files.move_cursor(delta);
    }
    state.diff.failed = None;
    Cmd::None
}

/// カーソルを動かし、続きが要るなら頼む。
///
/// **カーソルを動かすキーはすべてここを通す**（`g` / `G` や `PageDown` を足すときも）。
/// 腕ごとに `load_more` を書くと、足したキーが素通りする。下端で `j` を押したときは
/// カーソルが動かないので、「動いたかどうか」では判定できない。
fn scrolled(state: &mut AppState, delta: isize) -> Cmd {
    let before = state.graph.cursor;
    state.graph.move_cursor(delta);
    // カーソルを動かすのはユーザーの操作なので、失敗した読み直しはここから始める。
    // 自動では出さない（`load_more`）。**ログの印は無条件に落とす** — 失敗すると
    // 行を捨てるので、動ける先が無いところからでも押し直せる必要がある。
    state.graph.load_failed = false;
    // **詳細と一覧の印はカーソルが動いたときだけ落とす。** どちらもカーソルの指す
    // コミットに紐づくので、端で押しても対象は変わらず、試し直す根拠が無い。
    // 無条件に落とすと、末尾の行で `j` を押し続けるあいだ「失敗 → 再読込 → 失敗」を
    // 打鍵のたびに回して通知を積む（01 の「失敗の記憶」）。
    if state.graph.cursor != before {
        state.graph.detail_failed = None;
        state.graph.files_failed = None;
    }
    Cmd::batch([load_more(state), load_commit(state)])
}

/// カーソルの指すコミットについて、まだ持っていないものを頼む。
///
/// **詳細と一覧は必ずここから頼む。** どちらも「カーソルがコミットに乗った」で要る
/// ので、呼び出し側に 2 つ配ると片方だけ書き足す事故が起きる（01 の「コミットの
/// ファイル一覧」）。`Cmd` を分けているのは、一覧のほうが重いため。
fn load_commit(state: &AppState) -> Cmd {
    Cmd::batch([load_detail(state), load_files(state)])
}

/// カーソルの指すコミットの詳細が要るなら頼む。
///
/// WIP 行を指しているあいだは読むものが無い。既に同じ oid の詳細を持っていれば
/// 出さない（01 の「コミット詳細の遅延取得」）。
fn load_detail(state: &AppState) -> Cmd {
    if state.graph.fresh_detail().is_some() {
        return Cmd::None;
    }
    match state.graph.cursor_commit() {
        // **同じコミットで一度失敗しているなら出さない。** 出すと
        // 「失敗 → 再読込 → 失敗」を回す（`load_diff` と同じ形）。
        Some(commit) if state.graph.detail_failed != Some(commit.oid) => {
            Cmd::LoadCommitDetail(commit.oid)
        }
        _ => Cmd::None,
    }
}

/// カーソルの指すコミットのファイル一覧が要るなら頼む。[`load_detail`] と同じ形。
fn load_files(state: &AppState) -> Cmd {
    if state.graph.fresh_files().is_some() {
        return Cmd::None;
    }
    match state.graph.cursor_commit() {
        Some(commit) if state.graph.files_failed != Some(commit.oid) => {
            Cmd::LoadCommitFiles(commit.oid)
        }
        _ => Cmd::None,
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
        //
        // **カーソルが WIP 行にあるときは採る。** そこでは詳細を 1 つも出していない
        // ので、置いても新しいものを潰さない。捨てると、コミットに乗って戻るだけで
        // 同じものを読み直して「読み込み中…」をもう 1 回見せることになる
        // （01 の「選択が WIP に移っても `detail` は捨てない」）。
        Ok(detail) if cursor.is_none() || cursor == Some(detail.oid) => {
            state.graph.detail_failed = None;
            state.graph.detail = Some(detail);
        }
        Ok(_) => {}
        Err(err) => {
            state.push_notice(NoticeLevel::Error, err.to_string(), None);
            // **失敗した相手を覚えて頼み直さない。** 出すと、リポジトリが壊れて
            // いるあいだ「失敗 → 再読込 → 失敗」を回し続ける（`log_loaded` と同じ）。
            // 別のコミットの失敗なら、いま見ているものはまだ一度も試していないので
            // 下で頼む（`load_detail` が `detail_failed` を見る）。
            state.graph.detail_failed = Some(oid);
        }
    }
    load_commit(state)
}

/// 届いたファイル一覧を積む。[`commit_detail_loaded`] と同じ形。
///
/// **一覧が届いたら `load_diff` も通す。** 左が Diff に差し替わっていても、一覧が
/// 届くまでは選んでいるファイルが決まらない（`wanted_diff()` が `None`）。`Enter` や
/// `Tab` で先に `Pane::Diff` へ移れるので、ここで頼み直さないと Diff ペインが
/// 「読み込めませんでした」のまま残る（01 の「コミットのファイル一覧」）。
fn commit_files_loaded(
    state: &mut AppState,
    oid: Oid,
    result: Result<Vec<FileEntry>, GitReadError>,
) -> Cmd {
    state.jobs.finish(JobKind::LoadCommitFiles);
    let cursor = state.graph.cursor_commit().map(|commit| commit.oid);

    match result {
        // 採る条件は詳細と同じ（いま見ているコミットのものか、WIP 行にいるか）。
        Ok(entries) if cursor.is_none() || cursor == Some(oid) => {
            state.graph.files_failed = None;
            state.graph.files = Some(CommitFiles {
                oid,
                entries,
                cursor: 0,
            });
        }
        Ok(_) => {}
        Err(err) => {
            state.push_notice(NoticeLevel::Error, err.to_string(), None);
            state.graph.files_failed = Some(oid);
        }
    }
    Cmd::batch([load_commit(state), load_diff(state)])
}

/// 下端に近づいたら次のページを頼む。
///
/// 残りが 1 画面を切ったら出す。同じ種類のジョブが走っているあいだは
/// [`AppState::issue`] が落とすので、ここでは走っているかを見ない
/// （見ると 2 箇所で同じことを判断することになる）。
///
/// **`GraphState::reload_pending` も見ない。** 印が立っているあいだは必ず `LoadLog` が
/// 走っている（立てる `exec_finished` がその場で読み直しを出し、出せなければ落とした
/// 相手が走っているため。落とすのも `log_loaded` の出口で必ず出し直す）ので、ここで
/// 続きを出しても `issue` が落とす。見る形にすると、テストで踏めない分岐が残る。
fn load_more(state: &AppState) -> Cmd {
    let remaining = state.graph.rows().saturating_sub(state.graph.cursor);
    // 失敗したあとは行が空なので「残りが 1 画面以下」が必ず真になる。見ないと、
    // 以後のレイアウト変化のたびに読み直しを出す（失敗自体がレイアウトを変える。
    // 行が消えると詳細の領域も消えるため）。
    if state.graph.load_failed || state.graph.loaded_all || remaining > state.graph.viewport {
        return Cmd::None;
    }

    Cmd::LoadLog {
        count: state.config.graph.page_size,
        restart: false,
    }
}

/// 先頭から読み直す。
///
/// 出すかどうかはここでは決めない。呼び出し元は 2 つあり、どちらも出すと決めた
/// あとで呼ぶ（`exec_finished` と、落とされたぶんを頼み直す `log_loaded`）。
/// 判断を置くと、テストで踏めない偽の腕が残る。
fn reload_log(state: &AppState) -> Cmd {
    Cmd::LoadLog {
        count: state.config.graph.page_size,
        restart: true,
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
///
/// `restart` は**要求した内容**で、届いたページが先頭からのものか続きかを表す
/// （`LogPage` は位置を持たない）。
fn log_loaded(state: &mut AppState, restart: bool, result: Result<LogPage, GitReadError>) -> Cmd {
    state.jobs.finish(JobKind::LoadLog);

    let page = match result {
        Ok(page) => page,
        Err(err) => {
            state.push_notice(NoticeLevel::Error, err.to_string(), None);
            // 失敗したセッションは捨てられ、次の読み込みは先頭から来る（02）。
            // 積んだ行を残すと同じコミットが二重に並ぶ。
            state.graph.restart();
            state.graph.load_failed = true;
            return Cmd::None;
        }
    };

    // **読み直しを待っているあいだに届いたページは、続きでも読み直しでも捨てる。**
    // 印が立つのは「読み直しを頼めなかった」ときで、そのとき飛んでいたページは
    // 必ず印を立てた書き込みより前に頼まれたもの。`restart` かどうかで分けると、
    // **1 つ前の書き込みの読み直しが届いたときに印だけ落ちて、新しい書き込みが
    // 二度とグラフに出ない**（続けて 2 回コミットすると踏む）。ここまで来ていれば
    // `jobs.finish` のあとなので、頼み直しは必ず出せる（01 の「ログの読み直し」）。
    if state.graph.reload_pending {
        state.graph.reload_pending = false;
        return reload_log(state);
    }

    if restart {
        state.graph.restart();
    }

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
    // 1 ページが窓より短いことがある。埋まるまで続けて読む。
    //
    // **詳細と一覧はここから頼まない。** カーソルはページを積んでも動かず、動かすのは
    // `scrolled`（そこで頼む）か `restart`（WIP 行に戻るので頼むものが無い）だけ。
    // 頼んでも `load_detail` / `load_files` が「持っている」「失敗した」で断るので
    // 害は無いが、読む状態が変わっていない経路を一覧に増やさない。
    load_more(state)
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
        // **グラフの `Enter` で入ってきた先なので、`Esc` で戻れる。** 戻り先を
        // 用意しないと、`Enter` の行き先がコミット詳細か Working tree かで出口が
        // 変わる（片方は `Esc`、もう片方は `Tab` を探す）ことになる。
        KeyCode::Esc => focus(state, Pane::Graph),
        KeyCode::Char('s') => act(state, Action::Stage),
        KeyCode::Char('u') => act(state, Action::Unstage),
        KeyCode::Char('x') => act(state, Action::Discard),
        KeyCode::Char('c') => commit(state),
        // **初期値は読んでから開く**（01 の「commit」）。`RepoInfo.head` は起動時に
        // 1 回読んだきりなので、1 回コミットした時点で古い。
        KeyCode::Char('C') => Cmd::LoadHeadMessage,
        _ => Cmd::None,
    }
}

/// `c`。staged を新しいコミットにする。
///
/// **staged が空でも、いま持っている一覧を信じられないなら断らない。** 書き込み直後の
/// 一覧は古く（`stale`）、一度も読めていない一覧は空と区別が付かない（`loaded`）。
/// どちらも「変更がありません」の根拠にすると、実際には stage されているのに拒否する
/// （01 の「commit」）。そのときは開いて git の側に判定を任せる。
fn commit(state: &mut AppState) -> Cmd {
    let trusted = state.worktree.loaded && !state.worktree.stale;
    if trusted && state.worktree.snapshot.staged.is_empty() {
        state.push_notice(
            NoticeLevel::Warn,
            "stage された変更がありません".to_string(),
            None,
        );
        return Cmd::None;
    }

    state.modal = Some(Modal::CommitMessage {
        buf: TextBuffer::default(),
        amend: false,
    });
    Cmd::None
}

/// [`Modal::CommitMessage`] のキー処理。**文字キーはすべて入力になる。**
///
/// 確定は `Enter`、改行は `Alt+Enter` / `Shift+Enter` / `Ctrl-J`
/// （01 の「コミットメッセージの編集」）。
fn commit_message_key(state: &mut AppState, key: Key) -> Cmd {
    // **確認は挟まない。** `modal` は 1 つしか持てず、重ねるにはモーダルのスタックが
    // 要る。不変ルール 5 が確認を求めるのは git の破壊的操作で、編集中バッファの
    // 取り消しは含まれない（01）。
    if key == Key::new(KeyCode::Esc) {
        state.modal = None;
        return Cmd::None;
    }

    // **修飾なしの `Enter` だけが確定。** 修飾付きは下の改行に落ちる。
    if key == Key::new(KeyCode::Enter) {
        return commit_submit(state);
    }

    let Some(Modal::CommitMessage { buf, .. }) = state.modal.as_mut() else {
        return Cmd::None;
    };

    // `Ctrl-J` はどの端末でも届く改行。raw mode では `\n` が `Ctrl+j` として来る。
    if key == Key::ctrl('j') {
        buf.insert_newline();
        return Cmd::None;
    }

    match key.code() {
        // ここに来る `Enter` は修飾付き（`Alt+Enter` = `ESC CR`、`Shift+Enter`）。
        KeyCode::Enter => buf.insert_newline(),
        // 行が複数あるモーダルにしか要らない。残りは `edit_key` が受ける。
        KeyCode::Up => buf.move_cursor(Motion::Up),
        KeyCode::Down => buf.move_cursor(Motion::Down),
        _ => {
            let _ = edit_key(buf, key);
        }
    }
    Cmd::None
}

/// 編集中のモーダルで共通のキー。扱えたら `true`。
///
/// **`CommitMessage` と `Input` で腕を写さない。** 「修飾付きの文字は入力にしない」を
/// 変えるときに片方だけ古くなり、**どちらのテストも自分のモーダルしか踏まないので
/// 通ってしまう**。行の上下移動と改行はここに入れない — 行が 1 本か複数かで違い、
/// それがこの 2 つのモーダルの唯一の差だから（01 の「1 行入力」）。
fn edit_key(buf: &mut TextBuffer, key: Key) -> bool {
    match key.code() {
        KeyCode::Backspace => buf.backspace(),
        KeyCode::Delete => buf.delete(),
        KeyCode::Left => buf.move_cursor(Motion::Left),
        KeyCode::Right => buf.move_cursor(Motion::Right),
        KeyCode::Home => buf.move_cursor(Motion::Home),
        KeyCode::End => buf.move_cursor(Motion::End),
        // 修飾付きの文字は入力にしない（`Ctrl-C` は `update_key` が先に見る）。
        KeyCode::Char(c) if !key.mods().ctrl && !key.mods().alt => buf.insert_char(c),
        _ => return false,
    }
    true
}

/// コミットメッセージを確定する。
fn commit_submit(state: &mut AppState) -> Cmd {
    let Some(Modal::CommitMessage { buf, amend }) = &state.modal else {
        return Cmd::None;
    };

    // **`git commit --message ""` に任せない。** git が落ちる時点でモーダルは閉じて
    // いて、書いた本文が戻らない（01 の「コミットメッセージの編集」）。
    if buf.is_blank() {
        state.push_notice(
            NoticeLevel::Warn,
            "コミットメッセージが空です".to_string(),
            None,
        );
        return Cmd::None;
    }

    let op = GitOp::Commit {
        message: buf.text(),
        amend: *amend,
    };
    submit_exec(state, op)
}

/// 編集中のモーダルを確定して `Cmd::Exec` を返す。
///
/// **`Cmd::Exec` が落とされるなら閉じない。** [`confirm_key`] は先に閉じてから返すが、
/// あちらのモーダルはユーザーが打ったものを持っていない。ここで同じことをすると、
/// 前の git 実行（遅い pre-commit hook など）が走っているあいだに確定したときに
/// 「ほかの git 実行が終わるまで待ってください」と出て**打った文字ごと消える**。
/// 判定は [`AppState::issue`] と同じ `jobs.is_running`（`load_diff` が
/// `Freshness::Asked` を決めるのと同じ形）。
///
/// **2 人目（`Modal::Input`）が出た時点でここへ移した。** 写すと、片方だけ
/// 「閉じてから返す」形に戻ったときに気付けない（01 の「1 行入力」）。
fn submit_exec(state: &mut AppState, op: GitOp) -> Cmd {
    if state.jobs.is_running(JobKind::Exec) {
        if let Some(message) = JobKind::Exec.drop_notice() {
            state.push_notice(NoticeLevel::Warn, message.to_string(), None);
        }
        return Cmd::None;
    }

    // 閉じずに返すと、`Cmd` が落とされたときにモーダルだけが画面に残る。
    state.modal = None;
    Cmd::Exec(op)
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

/// amend の初期値が届いた。
fn head_message_loaded(state: &mut AppState, result: Result<Option<String>, GitReadError>) -> Cmd {
    state.jobs.finish(JobKind::LoadHeadMessage);

    let message = match result {
        Ok(Some(message)) => message,
        Ok(None) => {
            state.push_notice(
                NoticeLevel::Warn,
                "amend できるコミットがありません".to_string(),
                None,
            );
            return Cmd::None;
        }
        Err(err) => {
            state.push_notice(NoticeLevel::Error, err.to_string(), None);
            return Cmd::None;
        }
    };

    // **開くのは `modal` が空のときだけ。** 頼んでから届くまでのあいだに別のモーダル
    // （`x` の確認など）が開いていることがある。無条件に上書きすると、確認モーダルが
    // 編集画面に化ける（01 の「commit」）。
    if state.modal.is_some() {
        return Cmd::None;
    }

    state.modal = Some(Modal::CommitMessage {
        buf: TextBuffer::from_text(&message),
        amend: true,
    });
    Cmd::None
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

/// コミットの diff で `s` / `u` / `x` を断る文言。
const COMMIT_DIFF: &str = "コミットの差分は変更できません";

/// 出している diff が最新でないときに断る文言。
///
/// 「まだ届いていない」「読み込めなかった」「書き込みより前のもの」の 3 つに使う。
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
    // **コミットを選んでいるあいだは効かせない。** 対象はワーキングツリーの選択行
    // だが、右ペインに出ているのはコミット詳細なので、通すと**画面に出ていない
    // ファイル**を stage する（07 の「Diff ペイン」）。`focus` の候補で
    // `Pane::Worktree` を外しているのと同じ事故を、`Pane::Diff` の側で防ぐ。
    //
    // **見るのはグラフの選択で、`wanted_diff()` が `None` かどうかではない。**
    // あちらの `None` は 3 つのことを指す — 「Diff ペインを見せていない」
    // 「コミットの一覧がまだ届いていない」「ワーキングツリーの行に diff が無い
    // （conflicted）」。無いことを根拠にすると、3 つ目で
    // 「コンフリクトしたファイルは操作できません」がこの文言に化ける。
    //
    // **`DiffTarget::Range` を Diff ペインに出すときはここを見直す**（stash や比較
    // ビュー、M3 以降）。WIP 行を選んだまま stash の diff を出せるようになると、
    // この判定だけでは足りない（そのハンクは worktree のファイルに当たらない）。
    // いま出す経路が無いので、踏めない腕は先に入れない。
    if !matches!(state.graph.selection(), GraphSelection::Wip) {
        state.push_notice(NoticeLevel::Warn, COMMIT_DIFF.to_string(), None);
        return Cmd::None;
    }

    let Some((section, status, paths)) = state
        .worktree
        .selected()
        .map(|(section, entry)| (section, entry.status, paths_of(entry)))
    else {
        return Cmd::None;
    };

    // **Diff ペインが出していない diff では何もしない。** 「読み込み中…」や
    // 「差分を読み込めませんでした」を出しているあいだ、`hunk_scope` は `None` を
    // 返す。そのままファイル単位へ落とすと、ハンク 1 つのつもりで押した `s` が
    // **ファイル全体**を stage する。07 の fallback は「ハンクで表せないファイル」の
    // ためのもので、届いていないファイルのためではない。
    let wanted = state.wanted_diff();
    if wanted.is_some() && state.diff.fresh_view(wanted.as_ref()).is_none() {
        state.push_notice(NoticeLevel::Warn, NOT_LATEST.to_string(), None);
        return Cmd::None;
    }

    let scope = hunk_scope(state).unwrap_or(Scope::File { status, paths });

    // **出しているハンクが書き込みより前のものなら断る**（01）。そこから組んだパッチは
    // `git apply` のオフセット追従で別のハンクに当たりうる。**ファイル単位に落とさない**
    // （`s` がファイル全体を stage することになる）。
    //
    // 見るのは単位であってペインではない。`focus == Pane::Diff` で書くと、ハンクを
    // 出す場所が増えたときにガードだけが外れる。
    let planned = if matches!(scope, Scope::Hunk { .. }) && state.diff.freshness != Freshness::Fresh
    {
        Err(NOT_LATEST.to_string())
    } else {
        plan(action, section, scope)
    };

    match planned {
        Ok(Plan::Exec(op)) => Cmd::Exec(op),
        Ok(Plan::Confirm { title, body, op }) => confirm(state, title, body, op),
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
/// **`view` が出ていないときも `None` を返す。** その区別は呼び出し側（[`act`]）が
/// 先に断って付ける。ここで返す `None` は「表せない」と「まだ無い」の両方を含む。
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

/// ブランチ一覧が届いた。
///
/// **成功も失敗も `jobs.finish` を通る。** 通らないと `JobKind::LoadBranches` が走った
/// まま残り、以後の `Cmd::LoadBranches` が黙って落ちる（`status_loaded` と同じ）。
fn branches_loaded(state: &mut AppState, result: Result<Vec<Branch>, GitReadError>) -> Cmd {
    state.jobs.finish(JobKind::LoadBranches);

    match result {
        Ok(branches) => state.branches.loaded(branches),
        // 読めていた一覧は捨てない（`status_loaded` と同じ理由）。空のまま失敗した
        // ときに「読み込み中…」で固まらないよう、印だけ立てる（01 の「ブランチ一覧」）。
        Err(err) => {
            state.branches.failed();
            state.push_notice(NoticeLevel::Error, err.to_string(), None);
        }
    }

    Cmd::None
}

/// ワーキングツリーの一覧を入れ替える。
fn status_loaded(state: &mut AppState, result: Result<WorktreeSnapshot, GitReadError>) -> Cmd {
    state.jobs.finish(JobKind::LoadStatus);

    match result {
        Ok(snapshot) => {
            state.worktree.snapshot = snapshot;
            // 以後、空の一覧は「変更が無い」と読んでよい。
            state.worktree.loaded = true;
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
    state.diff.freshness = Freshness::Stale;

    match Reload::after(&done.op, done.result.is_ok()) {
        Reload::Nothing => Cmd::LoadStatus,
        // **ブランチ一覧も ref から導かれる。** commit は `refs/heads/<現在>` を
        // 動かすので、読み直さないと Branches ペインが commit 前の ahead のまま残る。
        Reload::Refs => Cmd::batch([Cmd::LoadStatus, Cmd::LoadRepoInfo, Cmd::LoadBranches]),
        Reload::RefsAndLog => {
            // 落とされたぶんは印を付け、走っていたページが届いた時点で頼み直す
            // （`WorktreeState.stale` と同じ形。01 の「ログの読み直し」）。
            state.graph.reload_pending = state.jobs.is_running(JobKind::LoadLog);
            Cmd::batch([
                Cmd::LoadStatus,
                Cmd::LoadRepoInfo,
                Cmd::LoadBranches,
                reload_log(state),
            ])
        }
    }
}

/// 書き込みのあとに読み直すもの。`ok` は git が成功したか。
///
/// **`git_exec` ではなく `app` に置く。** 「どの ref をグラフが辿るか」を知っているのは
/// こちらで、`GitOp` の隣に `moves_refs` のような名前で置くと、git の事実を約束しながら
/// 中身は UI の方針という形になる（stash が偽なのは「`refs/stash` を walk に押して
/// いない」からで、git は ref を動かしている）。Stash ペインが入ったときに黙って
/// 嘘になる。
///
/// 網羅 `match` にしているのは、`GitOp` に変種を足したときにどれなのかを選ばせるため
/// （`to_command` と同じ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reload {
    /// index / ワークツリーだけ。`Cmd::LoadStatus` は常に出すので、ここでは何も足さない。
    Nothing,
    /// ref の索引（ラベルと HEAD）。**walk に入るコミットの集合は変わらない**ので、
    /// 積んだログはそのままでよい。
    Refs,
    /// 索引と、積んだログの両方。
    RefsAndLog,
}

impl Reload {
    fn after(op: &GitOp, ok: bool) -> Self {
        match op {
            // index / ワークツリーだけ。ref も HEAD も動かない。
            GitOp::Stage(_)
            | GitOp::Unstage(_)
            | GitOp::StageHunk { .. }
            | GitOp::UnstageHunk { .. }
            | GitOp::DiscardHunk { .. }
            | GitOp::Discard(_)
            | GitOp::DiscardUntracked(_)
            | GitOp::DiscardAdded(_) => Reload::Nothing,

            // **ラベルと HEAD は動くが、行は動かない。** グラフのラベルは行に焼き込まず
            // `GraphState.refs` から引く（07）ので、`Cmd::LoadRepoInfo` だけで足りる。
            // ログの読み直しは `LogSession` の作り直しで履歴全体のコストが掛かる（02 の
            // 「セッションは作り直さず使い回す」）ので、要らないところでは払わない。
            //
            // どれも「既にある、どこかの ref から辿れるコミット」を指すだけ。
            // `Push` が非 force なら `origin/x` はローカルの先端へ早送りされるので、
            // 行はもう並んでいる。
            //
            // **断られた `Switch` でも読み直す。** ref も HEAD も動いていないので
            // 無駄に見えるが（`switch` は断った時点で post-checkout hook すら走らない。
            // git 2.55 で確認）、**断られる理由の 1 つが「一覧が古い」**（別の端末で
            // 消されたブランチに `Enter` を押した）で、そこで読み直さないと画面から
            // 消えない行が残る。払うのはキー押下 1 回あたりの読み取り 2 本。
            GitOp::Switch(_)
            | GitOp::BranchCreate { .. }
            | GitOp::BranchRename { .. }
            | GitOp::Push { force: false, .. } => Reload::Refs,

            // stash は `refs/stash` しか動かさず、グラフはそれを walk に押していない
            // （02 の「ログ列挙」）。ワークツリーの変化は `Cmd::LoadStatus` が拾う。
            // **ここが `app` に置く理由。** グラフが `refs/stash` を押すようになったら
            // この腕が動く。
            GitOp::StashPush { .. }
            | GitOp::StashApply(_)
            | GitOp::StashPop(_)
            | GitOp::StashDrop(_) => Reload::Nothing,

            // walk に入るコミットの集合が変わる。**失敗しても途中まで ref が
            // 動いていることがある**ので成否を見ない。fetch / pull は一部の ref だけ
            // 更新して落ちうるし、rebase / cherry-pick / revert は途中で止まる。
            GitOp::Fetch { .. }
            | GitOp::Pull { .. }
            | GitOp::RebaseInteractive { .. }
            | GitOp::RebaseContinue
            | GitOp::RebaseSkip
            | GitOp::RebaseAbort
            | GitOp::CherryPick(_)
            | GitOp::CherryPickContinue
            | GitOp::CherryPickAbort
            | GitOp::Revert { .. }
            | GitOp::RevertContinue
            | GitOp::RevertAbort => Reload::RefsAndLog,

            // 同じく walk が変わるが、**1 回で決まるので失敗したなら ref は元のまま**。
            // そこで読み直すと `GraphState::restart` がカーソルとスクロールを先頭に
            // 戻すので、pre-commit hook に弾かれただけでグラフの位置を失う。
            GitOp::Commit { .. } | GitOp::BranchDelete { .. } | GitOp::Push { force: true, .. } => {
                if ok {
                    Reload::RefsAndLog
                } else {
                    Reload::Refs
                }
            }
        }
    }
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
    use crate::app::{
        BranchLoad, GraphSelection, Job, Modal, Modifiers, RepoInfo, TermSize, TextBuffer,
    };
    use crate::config::Config;
    use crate::git_exec::{ExecErrorKind, ExecOutput};
    use crate::git_read::{Commit, HeadInfo, Hunk, LineOrigin, RefIndex, RefLabel, Remote};
    use crate::graph::LaneState;
    use crate::test_util::{
        fake_branch, fake_commit, fake_commit_with_parents, fake_detail, fake_entry,
        fake_file_diff, fake_hunk, fake_oid, fake_snapshot,
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
        let _ = update(&mut state, page_loaded(Ok(page)));
        state
    }

    /// staged に 1 ファイルだけある状態。worktree ペインの Staged を選んでいる。
    fn only_staged() -> AppState {
        worktree_state(fake_snapshot(&["a.txt"], &[], &[]), WorktreeSection::Staged)
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

    /// 続きのページが届いた。
    fn page_loaded(result: Result<LogPage, GitReadError>) -> Msg {
        Msg::LogLoaded {
            restart: false,
            result,
        }
    }

    /// 続きを頼む `Cmd`。
    fn more(state: &AppState) -> Cmd {
        Cmd::LoadLog {
            count: state.config.graph.page_size,
            restart: false,
        }
    }

    /// 先頭から読み直す `Cmd`。
    fn reload(state: &AppState) -> Cmd {
        Cmd::LoadLog {
            count: state.config.graph.page_size,
            restart: true,
        }
    }

    fn press(state: &mut AppState, c: char) -> Cmd {
        update(state, Msg::Key(Key::char(c)))
    }

    fn press_code(state: &mut AppState, code: KeyCode) -> Cmd {
        update(state, Msg::Key(Key::new(code)))
    }

    /// 文字を 1 つずつ打つ。**貼り付け（`Msg::Paste`）ではない。**
    fn type_text(state: &mut AppState, text: &str) {
        for c in text.chars() {
            let _ = press(state, c);
        }
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
        let mut state = only_staged();

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
    fn a_file_level_fallback_is_not_refused_by_the_freshness_guard() {
        // **断るのはハンク単位のときだけ**（07）。ハンクにできないファイルへの `s` は
        // 古いパッチを一切使わないので、読み直しを待たせる理由が無い。ペイン名で
        // ガードを書くとここまで塞ぐ。
        let binary = FileDiff {
            binary: true,
            ..fake_file_diff("a.png", vec![real_hunk(1)])
        };
        let mut state = diff_pane(WorktreeSection::Unstaged, &["a.png"], vec![binary]);
        let _ = update(&mut state, exec_done(Ok(fake_output())));
        assert_eq!(state.diff.freshness, Freshness::Stale);

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
        // **区画で断られる組み合わせを混ぜない。** Unstaged で `u` は鮮度に関わらず
        // 「stage されていません」で断られるので、その腕は見張りを踏まない。
        for (section, key) in [
            (WorktreeSection::Unstaged, 's'),
            (WorktreeSection::Unstaged, 'x'),
            (WorktreeSection::Staged, 'u'),
        ] {
            let mut state = diff_pane(
                section,
                &["a.txt"],
                vec![fake_file_diff("a.txt", vec![real_hunk(1)])],
            );
            // 書き込みが終わった直後。読み直しはまだ届いていない。
            let _ = update(&mut state, exec_done(Ok(fake_output())));
            assert_eq!(state.diff.freshness, Freshness::Stale);

            // 古いハンクから組んだパッチは、同じ内容のハンクが 2 つあると別の場所に当たる。
            assert_eq!(press(&mut state, key), Cmd::None, "{key}");
            assert!(state.modal.is_none(), "{key}");
            assert_eq!(
                state.notices.back().map(|notice| notice.message.as_str()),
                Some(NOT_LATEST),
                "{key}"
            );
        }
    }

    #[test]
    fn keys_are_refused_while_the_reload_has_been_asked_for_but_has_not_arrived() {
        // **`Cmd::LoadDiff` を出しただけでは新しくならない。** 返信が届くまで、
        // 出している view は書き込みより前のものそのもの。
        let mut state = diff_pane(
            WorktreeSection::Unstaged,
            &["a.txt"],
            vec![fake_file_diff("a.txt", vec![real_hunk(1)])],
        );
        let _ = update(&mut state, exec_done(Ok(fake_output())));
        let asked = update(
            &mut state,
            Msg::StatusLoaded(Box::new(Ok(fake_snapshot(&[], &["a.txt"], &[])))),
        );
        assert_eq!(asked, Cmd::LoadDiff(unstaged_diff("a.txt")), "頼んではいる");

        assert_eq!(press(&mut state, 's'), Cmd::None);
        assert_eq!(
            state.notices.back().map(|notice| notice.level),
            Some(NoticeLevel::Warn)
        );
    }

    #[test]
    fn keys_do_nothing_while_the_diff_pane_has_nothing_to_show() {
        // 「読み込み中…」や「差分を読み込めませんでした」を出しているあいだ、
        // **ファイル単位に落とすとハンク 1 つのつもりの `s` がファイル全体を stage する。**
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        let _ = press_code(&mut state, KeyCode::Enter);
        assert!(state.diff.view.is_none(), "まだ届いていない");

        for key in ['s', 'u', 'x'] {
            assert_eq!(press(&mut state, key), Cmd::None, "{key}");
            assert!(state.modal.is_none(), "{key}");
            assert_eq!(
                state.notices.back().map(|notice| notice.message.as_str()),
                Some(NOT_LATEST),
                "{key}"
            );
        }

        // 読み込めなかったあとも同じ。
        let _ = update(&mut state, diff_err(unstaged_diff("a.txt")));
        assert_eq!(press(&mut state, 's'), Cmd::None);
        assert_eq!(
            state.notices.back().map(|notice| notice.message.as_str()),
            Some(NOT_LATEST)
        );
    }

    #[test]
    fn a_reload_that_fails_leaves_the_shown_diff_marked_as_old() {
        // 頼んだ読み直しが失敗したら、出しているのは書き込み前の diff のまま。
        // **`Asked` を倒さないと、来ない返信を待つあいだ古い diff を新しい扱いにする。**
        let mut state = diff_pane(
            WorktreeSection::Unstaged,
            &["a.txt"],
            vec![fake_file_diff("a.txt", vec![real_hunk(1)])],
        );
        let _ = update(&mut state, exec_done(Ok(fake_output())));
        let _ = update(
            &mut state,
            Msg::StatusLoaded(Box::new(Ok(fake_snapshot(&[], &["a.txt"], &[])))),
        );

        let cmd = update(&mut state, diff_err(unstaged_diff("a.txt")));

        // 同じ対象には出し直さない（「失敗 → 再読込 → 失敗」を回さない）。
        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.diff.freshness, Freshness::Stale);
        assert_eq!(press(&mut state, 's'), Cmd::None);
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
        // 同じままなのでキャッシュが効いてしまう**のを `Freshness` が止める。
        let mut state = diff_state(&["a.txt"], 2);
        let _ = update(&mut state, exec_done(Ok(fake_output())));

        let cmd = update(
            &mut state,
            Msg::StatusLoaded(Box::new(Ok(fake_snapshot(&[], &["a.txt"], &[])))),
        );

        assert_eq!(cmd, Cmd::LoadDiff(unstaged_diff("a.txt")));
        // **まだ `Fresh` ではない。** 頼んだだけで、出している diff は書き込みより前。
        assert_eq!(state.diff.freshness, Freshness::Asked);
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
        assert_eq!(
            state.diff.freshness,
            Freshness::Stale,
            "落とされたので、まだ頼めていない"
        );

        // 書き込みより前に頼まれていた返信が届く（中身が違うと判るように 5 ハンク）。
        let cmd = update(
            &mut state,
            diff_ok(unstaged_diff("a.txt"), fake_diff("a.txt", 5)),
        );

        assert_eq!(cmd, Cmd::LoadDiff(unstaged_diff("a.txt")));
        assert_eq!(state.diff.freshness, Freshness::Asked);
        // **書き込みより前の返信は採らない。** 採ると、頼み直しが届くまでのあいだ
        // 書き込み前の中身を新しいものとして出すことになる。
        assert_eq!(state.diff.view.as_ref().unwrap().files[0].hunks.len(), 2);
    }

    #[test]
    fn tab_moves_the_focus_and_the_worktree_keys_follow_it() {
        // グラフに居るあいだは `j` がコミットのカーソルを動かす。
        let mut state = state_with_commits(3);
        state.worktree.snapshot = fake_snapshot(&[], &["a.txt", "b.txt"], &[]);
        let _ = press(&mut state, 'j');
        assert_eq!(state.graph.cursor, 1);
        // WIP 行に戻す。コミットを選んでいるあいだ Worktree は巡回に入らない（01）。
        let _ = press(&mut state, 'k');

        // 巡回の並びは `Pane` の宣言順なので、Graph の次は Branches（01）。
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Tab)));
        assert_eq!(state.focus, Pane::Branches);
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Tab)));
        assert_eq!(state.focus, Pane::Worktree);

        // 同じ `j` が worktree のカーソルに効く。グラフは動かない。
        let _ = press(&mut state, 'j');
        assert_eq!(state.graph.cursor, 0);
        assert_eq!(state.worktree.cursor, 1);

        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::BackTab)));
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::BackTab)));
        assert_eq!(state.focus, Pane::Graph);
    }

    /// ブランチ一覧の返信を届ける。**成功も失敗もここを通す。**
    fn with_branches(state: &mut AppState, result: Result<Vec<Branch>, GitReadError>) -> Cmd {
        update(state, Msg::BranchesLoaded(Box::new(result)))
    }

    /// Branches ペインにフォーカスを置き、一覧が届いた状態。
    fn state_on_branches(names: &[(&str, bool)]) -> AppState {
        let mut state = state_with_commits(2);
        state.focus = Pane::Branches;
        let branches = names
            .iter()
            .map(|(name, is_head)| fake_branch(name, *is_head))
            .collect();
        let _ = with_branches(&mut state, Ok(branches));
        state
    }

    #[test]
    fn branches_loaded_fills_in_the_list_and_ends_the_job() {
        let mut state = state();
        let _ = state.issue(Cmd::LoadBranches);

        let cmd = with_branches(
            &mut state,
            Ok(vec![fake_branch("main", true), fake_branch("topic", false)]),
        );

        assert_eq!(cmd, Cmd::None);
        // 通らないとジョブが走ったまま残り、以後の `LoadBranches` が黙って落ちる。
        assert!(!state.jobs.is_running(JobKind::LoadBranches));
        assert_eq!(state.branches.branches.len(), 2);
        assert_eq!(state.branches.load(), BranchLoad::Loaded);
    }

    #[test]
    fn branches_loaded_marks_the_failure_and_tells_the_user() {
        let mut state = state();
        let _ = state.issue(Cmd::LoadBranches);

        let cmd = with_branches(&mut state, Err(GitReadError::ReaderGone));

        assert_eq!(cmd, Cmd::None);
        assert!(!state.jobs.is_running(JobKind::LoadBranches));
        // **「読み込み中…」のまま固まらせない。** 印は `ui` が文言を出し分けるのに要る。
        assert_eq!(state.branches.load(), BranchLoad::Failed);
        assert_eq!(
            state.notices.back().map(|notice| notice.level),
            Some(NoticeLevel::Error)
        );
    }

    #[test]
    fn j_and_k_move_the_branch_cursor_without_touching_the_graph() {
        let mut state = state_on_branches(&[("another", false), ("main", true)]);
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
        let mut state = state_on_branches(&[("main", true)]);

        let _ = press_code(&mut state, KeyCode::Esc);

        assert_eq!(state.focus, Pane::Graph);
    }

    #[test]
    fn enter_switches_to_the_branch_under_the_cursor() {
        let mut state = state_on_branches(&[("another", false), ("main", true)]);
        let _ = press(&mut state, 'k');

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::Exec(GitOp::Switch("another".to_string())));
        // 切り替えたあと続けて別のブランチを選べる。確認モーダルも通さない。
        assert_eq!(state.focus, Pane::Branches);
        assert_eq!(state.modal, None);
    }

    #[test]
    fn enter_on_the_head_branch_still_asks_git() {
        // **押せるかを手前で判定しない**（不変ルール 4）。`is_head` は最後に読んだ
        // 一覧の値でしかないので、これで「切り替えなくてよい」と決めると、別の端末で
        // 切り替えられていたときに `Enter` が何も起こさなくなる（07 の「Branches ペイン」）。
        let mut state = state_on_branches(&[("another", false), ("main", true)]);
        assert!(state.branches.selected().expect("空です").is_head);

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::Exec(GitOp::Switch("main".to_string())));
    }

    /// `d` / `D` を押したときの確認モーダルの本文と `on_yes`。
    ///
    /// **押すたびに新しい state を作る。** 続けて押すには確認を閉じることになり、
    /// `confirm_key` の見張り（`confirming_runs_the_command_and_closes_the_modal`）が
    /// このテストに紛れ込む。
    fn branch_delete_confirm(force: bool) -> (String, Cmd) {
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
    fn the_branch_keys_do_nothing_while_the_list_is_empty() {
        // 読み込み中・失敗・コミットが 1 つも無いリポジトリ。カーソル行が描かれて
        // いないので、`branches[cursor]` を書くと添字が外れる。
        let mut state = state_with_commits(2);
        state.focus = Pane::Branches;
        assert_eq!(state.branches.load(), BranchLoad::Pending);

        assert_eq!(press_code(&mut state, KeyCode::Enter), Cmd::None);
        assert_eq!(press(&mut state, 'd'), Cmd::None);
        assert_eq!(press(&mut state, 'D'), Cmd::None);
        // `r` も同じ。**`n` はここに入れない** — 作成はカーソル行を要らず、
        // ブランチが 1 本も無い状態はまさに作りたい状態（下のテスト）。
        assert_eq!(press(&mut state, 'r'), Cmd::None);
        assert_eq!(state.modal, None);
    }

    /// 開いている入力モーダル。開いていなければ落とす。
    fn open_input(state: &AppState) -> (&str, &str, &TextBuffer, &InputIntent) {
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

    #[test]
    fn n_on_the_branches_pane_opens_an_empty_input_for_a_branch_from_head() {
        let mut state = state_on_branches(&[("main", true), ("topic", false)]);
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
    fn n_opens_even_when_the_branch_list_is_empty() {
        // `git init` の直後（ブランチが 1 本も無い）はまさに作りたい状態。
        // `switch --create` は unborn HEAD でも通る（git 2.55 で確認）。
        let mut state = state_with_commits(2);
        state.focus = Pane::Branches;
        assert_eq!(state.branches.load(), BranchLoad::Pending);

        let _ = press(&mut state, 'n');

        let (_, _, _, intent) = open_input(&state);
        assert_eq!(intent, &InputIntent::BranchCreate { start: None });
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
        let mut state = state_on_branches(&[("main", true), ("topic", false)]);
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
        let mut state = state_on_branches(&[("main", true)]);
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
        let mut state = state_on_branches(&[("topic", false)]);
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
        let mut state = only_staged();
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
        let mut state = only_staged();
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

    /// `git` の実行結果。ref を動かさない操作。
    fn exec_done(result: Result<ExecOutput, ExecError>) -> Msg {
        exec_done_of(GitOp::Stage(vec!["a.txt".into()]), result)
    }

    /// `op` の実行結果。ref を動かすかどうかで `exec_finished` の出す `Cmd` が変わる。
    fn exec_done_of(op: GitOp, result: Result<ExecOutput, ExecError>) -> Msg {
        Msg::ExecFinished(Box::new(ExecDone {
            op,
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
                more(&state),
                Cmd::LoadStatus,
                Cmd::LoadBranches,
            ])
        );
        assert!(state.jobs.is_running(JobKind::LoadLog));
        assert!(state.jobs.is_running(JobKind::LoadRepoInfo));
        assert!(state.jobs.is_running(JobKind::LoadStatus));
        assert!(state.jobs.is_running(JobKind::LoadBranches));
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

        let again = state.issue(Cmd::LoadLog {
            count: 10,
            restart: false,
        });

        assert_eq!(again, Cmd::None);
    }

    #[test]
    fn batch_only_drops_the_duplicated_cmd() {
        let mut state = state();
        let _ = init(&mut state);

        let cmd = state.issue(Cmd::Batch(vec![
            Cmd::LoadLog {
                count: 10,
                restart: false,
            },
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
        assert_eq!(update(&mut state, page_loaded(Ok(page))), Cmd::None);

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

        assert_eq!(update(&mut state, page_loaded(Ok(page))), more(&state));
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
        let _ = update(&mut state, page_loaded(Ok(first)));

        let second = LogPage {
            commits: vec![fake_commit_with_parents(2, &[], "commit 2")],
            done: true,
        };
        let _ = update(&mut state, page_loaded(Ok(second)));

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

        let cmd = update(&mut state, page_loaded(Err(GitReadError::NonUtf8Head)));

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
        // 表示行は WIP 行 + コミット 2 件の 3 行。上端は WIP 行。
        let mut state = state_with_commits(2);

        let _ = update(&mut state, Msg::Key(Key::char('k')));
        assert_eq!(state.graph.cursor, 0);

        for _ in 0..5 {
            let _ = update(&mut state, Msg::Key(Key::char('j')));
        }
        assert_eq!(state.graph.cursor, 2);
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

        // 表示行は WIP 行を含めて 31。窓が 25 行なら先頭は 6 行目まで下がれる。
        assert_eq!(state.graph.scroll, 6);
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
            more(&state)
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
            more(&state)
        );
    }

    #[test]
    fn moving_the_cursor_towards_the_end_asks_for_the_next_page() {
        // 残りが 1 画面を切ったら読む。カーソルが下端に着いてからだと、
        // 読み込みのあいだ動けなくなる。
        // 表示行は WIP 行を含めて 23、窓は 20 行。カーソルが 3 行目に来た時点で
        // 残りが 20 になる。カーソルがコミットに乗ると詳細も一緒に頼む。
        let mut state = state_with_page(ROWS + 2, false);

        assert_eq!(
            update(&mut state, Msg::Key(Key::char('j'))),
            commit_loads(cursor(&state))
        );
        // 2 回目の詳細と一覧は `JobTracker` に落ちる。続きはまだ出さない。
        assert_eq!(update(&mut state, Msg::Key(Key::char('j'))), Cmd::None);
        assert_eq!(update(&mut state, Msg::Key(Key::char('j'))), more(&state));
    }

    #[test]
    fn the_next_page_is_not_asked_for_twice_while_it_is_loading() {
        let mut state = state_with_page(ROWS + 2, false);
        state.graph.cursor = ROWS;
        // 1 ページ目の続きを頼んだ直後の状態を作る。
        let _ = state.issue(Cmd::LoadLog {
            count: 1,
            restart: false,
        });

        // 下端に着いても `JobTracker` が落とす。`update` を通さないと見えない。
        // 残るのは新しいカーソルの詳細と一覧だけ。
        assert_eq!(
            update(&mut state, Msg::Key(Key::char('j'))),
            commit_loads(cursor(&state))
        );
    }

    #[test]
    fn nothing_more_is_asked_for_once_the_log_is_exhausted() {
        let mut state = state_with_commits(3);
        // 最終行（3 件目のコミット）に置き、その詳細も届けておく。頼み直しが
        // 混ざると、出なかったのが続きなのか詳細なのか判らなくなる。
        state.graph.cursor = 3;
        let _ = update(&mut state, detail_ok(2));

        assert!(state.graph.loaded_all);
        assert_eq!(update(&mut state, Msg::Key(Key::char('j'))), Cmd::None);
    }

    #[test]
    fn a_smaller_viewport_pulls_the_cursor_back_into_view() {
        // 縮んだ窓でカーソルが外に残ると、選択行が見えないまま操作することになる。
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

    /// `n` 件を積み、1 件目のコミットにカーソルを乗せて詳細と一覧が届いた状態。
    ///
    /// 起動直後のカーソルは WIP 行なので、`j` を 1 回打って乗せる。**一覧も届ける。**
    /// 届けないと `JobKind::LoadCommitFiles` が走ったままになり、以後の
    /// `Cmd::LoadCommitFiles` が `issue` に落とされて、テストが見ているのが
    /// 「出なかった」なのか「落とされた」なのか判らなくなる。
    fn on_first_commit(n: usize) -> AppState {
        let mut state = state_with_commits(n);
        let _ = update(&mut state, Msg::Key(Key::char('j')));
        let _ = update(&mut state, detail_ok(0));
        let _ = update(&mut state, files_ok(0));
        state
    }

    /// カーソルがコミットに乗ったときに出る `Cmd`。詳細と一覧を一緒に頼む（01 の
    /// 「コミットのファイル一覧」）。
    fn commit_loads(oid: Oid) -> Cmd {
        Cmd::batch([Cmd::LoadCommitDetail(oid), Cmd::LoadCommitFiles(oid)])
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

    /// コミットの diff の対象。`assert_eq!` を短くするため。
    fn commit_diff_target(state: &AppState, path: &str) -> DiffTarget {
        DiffTarget::Commit {
            oid: cursor(state),
            path: Some(path.into()),
        }
    }

    /// 1 件目のコミットを選び、一覧の 1 件目で Diff ペインを開いた状態。
    ///
    /// **コミット側の Diff ペインの立ち上げはここ 1 本**（ワーキングツリー側の
    /// `diff_pane` と同じ理由）。
    /// 一覧は `paths` を `Modified` で並べたもの。
    fn commit_diff_pane(paths: &[&str]) -> AppState {
        let mut state = state_with_commits(3);
        let _ = press(&mut state, 'j');
        let _ = update(&mut state, detail_ok(0));
        let _ = update(&mut state, files_of(0, paths));
        let _ = press_code(&mut state, KeyCode::Enter);
        let _ = press_code(&mut state, KeyCode::Enter);
        assert_eq!(state.focus, Pane::Diff);
        let target = state.wanted_diff().expect("対象が決まっていません");
        let _ = update(&mut state, diff_ok(target, fake_diff(paths[0], 1)));
        state
    }

    #[test]
    fn a_commit_diff_is_the_file_the_list_has_selected() {
        // `path: None`（コミット全体）は出さない。一覧を出すためだけに全ファイルの
        // ハンクを組むことになる（01 の「diff の遅延取得」）。
        let mut state = state_with_commits(3);
        let _ = press(&mut state, 'j');
        let _ = update(&mut state, files_of(0, &["a.txt", "b.txt"]));
        // 一覧の 2 件目へ。`Enter` で CommitDetail に入ってから動かす。
        let _ = press_code(&mut state, KeyCode::Enter);
        let _ = press(&mut state, 'j');
        let _ = press_code(&mut state, KeyCode::Enter);

        assert_eq!(
            state.wanted_diff(),
            Some(commit_diff_target(&state, "b.txt"))
        );
    }

    #[test]
    fn a_commit_without_a_file_list_wants_no_diff() {
        // 一覧が届くまで対象が決まらない。ここで `path: None` に落とすと、
        // 一覧のためにコミット全体のハンクを組むことになる。
        let mut state = state_with_commits(3);
        let _ = press(&mut state, 'j');
        state.focus = Pane::Diff;

        assert_eq!(state.wanted_diff(), None);
    }

    #[test]
    fn a_file_list_that_lands_while_the_diff_pane_is_open_asks_for_the_diff() {
        // **`Tab` や `Enter` で先に Diff ペインへ移れる。** 一覧が届くまでは
        // `wanted_diff()` が `None` なので、届いた時点で頼み直さないと Diff ペインが
        // 「読み込めませんでした」のまま残る（01 の「コミットのファイル一覧」）。
        let mut state = state_with_commits(3);
        let _ = press(&mut state, 'j');
        let _ = update(&mut state, detail_ok(0));
        let _ = press_code(&mut state, KeyCode::Enter);
        let _ = press_code(&mut state, KeyCode::Enter);
        assert_eq!(state.focus, Pane::Diff);
        assert_eq!(state.wanted_diff(), None);

        let cmd = update(&mut state, files_ok(0));

        assert_eq!(cmd, Cmd::LoadDiff(commit_diff_target(&state, "a.txt")));
    }

    #[test]
    fn moving_the_file_cursor_changes_the_diff_target() {
        let mut state = commit_diff_pane(&["a.txt", "b.txt"]);
        let _ = press_code(&mut state, KeyCode::Esc);
        assert_eq!(state.focus, Pane::CommitDetail);

        let _ = press(&mut state, 'j');
        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::LoadDiff(commit_diff_target(&state, "b.txt")));
    }

    #[test]
    fn moving_the_file_cursor_clears_the_failed_diff() {
        // 対象が変わるので、一度失敗したファイルにも戻ってくれば試し直せる
        // （ワーキングツリーの `moved` と同じ形）。
        let mut state = on_first_commit(3);
        let _ = press_code(&mut state, KeyCode::Enter);
        state.diff.failed = Some(commit_diff_target(&state, "a.txt"));

        let _ = press(&mut state, 'j');

        assert_eq!(state.diff.failed, None);
    }

    #[test]
    fn the_file_cursor_only_moves_the_list_that_is_on_screen() {
        // 別のコミットの一覧が残っていることがある（読み込み中・失敗中）。
        // 濾さずに動かすと、画面に出ていない一覧のカーソルが黙って動く。
        //
        // **フォーカスを CommitDetail に置く。** `Pane::Graph` のままだと `j` は
        // `graph_key` → `scrolled` に入り、`move_file` を一度も通らない。
        let mut state = on_first_commit(3);
        let _ = press_code(&mut state, KeyCode::Enter);
        assert_eq!(state.focus, Pane::CommitDetail);
        // **ここだけ直接代入する。** `Msg::CommitFilesLoaded` は oid 違いを弾くので、
        // 「カーソルのコミットのものではない一覧が残っている」状態は経由して作れない。
        state.graph.files = Some(CommitFiles {
            oid: fake_oid(2),
            entries: vec![
                fake_entry("a.txt", FileStatus::Modified),
                fake_entry("b.txt", FileStatus::Added),
            ],
            cursor: 0,
        });

        let _ = press(&mut state, 'j');

        assert_eq!(state.graph.files.as_ref().unwrap().cursor, 0);
    }

    #[test]
    fn a_commit_diff_without_a_file_list_refuses_stage_too() {
        // **`wanted_diff()` の `None` には 2 つの意味がある** — 「Diff ペインを
        // 見せていない」と「見せているが一覧がまだ届いていない」。後者で通すと、
        // 画面に出ていないワーキングツリーのファイルに `s` が効く。
        let mut state = state_with_commits(3);
        let _ = press(&mut state, 'j');
        let _ = update(&mut state, detail_ok(0));
        let _ = press_code(&mut state, KeyCode::Enter);
        let _ = press_code(&mut state, KeyCode::Enter);
        let _ = update(
            &mut state,
            Msg::StatusLoaded(Box::new(Ok(fake_snapshot(&[], &["u.txt"], &[])))),
        );
        assert_eq!(state.focus, Pane::Diff);
        assert_eq!(state.wanted_diff(), None);
        assert!(state.worktree.selected().is_some());

        let cmd = press(&mut state, 's');

        assert_eq!(cmd, Cmd::None);
        assert_eq!(
            state.notices.back().map(|notice| notice.message.as_str()),
            Some(COMMIT_DIFF)
        );
    }

    #[test]
    fn esc_from_a_commit_diff_returns_to_the_commit_detail() {
        // 戻り先は `Enter` の行き先と同じ関数から出す（`right_pane`）。WIP 行から
        // 来たときは Working tree に戻る（既存のテスト）。
        let mut state = commit_diff_pane(&["a.txt"]);

        let _ = press_code(&mut state, KeyCode::Esc);

        assert_eq!(state.focus, Pane::CommitDetail);
    }

    #[test]
    fn a_commit_diff_refuses_stage_unstage_and_discard() {
        // **対象はワーキングツリーの選択行だが、画面に出ているのはコミットの中身。**
        // 通すと見えていないファイルを stage する（07 の「Diff ペイン」）。
        // ワーキングツリー側にも選択を用意する。用意しないと `act` の
        // 「選択が無い」腕で戻ってしまい、この見張りを踏まない。
        let mut state = commit_diff_pane(&["a.txt"]);
        let _ = update(
            &mut state,
            Msg::StatusLoaded(Box::new(Ok(fake_snapshot(&["s.txt"], &["u.txt"], &[])))),
        );
        assert!(state.worktree.selected().is_some());

        for key in ['s', 'u', 'x'] {
            let cmd = press(&mut state, key);

            assert_eq!(cmd, Cmd::None, "{key}");
            assert_eq!(state.modal, None, "{key}");
            assert_eq!(
                state.notices.back().map(|notice| notice.message.as_str()),
                Some(COMMIT_DIFF),
                "{key}"
            );
        }
    }

    #[test]
    fn a_conflicted_row_still_says_it_is_conflicted_in_the_diff_pane() {
        // **`wanted_diff()` の `None` は 3 つのことを指す。** ここは 3 つ目
        // （ワーキングツリーの行に diff が無い）で、コミットは 1 つも関わらない。
        // 「無い」を根拠に断ると、コンフリクトの案内がコミットの文言に化ける。
        let snapshot = WorktreeSnapshot {
            conflicted: vec![fake_entry("both.txt", FileStatus::Conflicted)],
            ..WorktreeSnapshot::default()
        };
        let mut state = worktree_state(snapshot, WorktreeSection::Conflicted);
        let _ = press_code(&mut state, KeyCode::Enter);
        assert_eq!(state.focus, Pane::Diff);
        assert_eq!(state.wanted_diff(), None);

        let cmd = press(&mut state, 's');

        assert_eq!(cmd, Cmd::None);
        assert_eq!(
            state.notices.back().map(|notice| notice.message.as_str()),
            Some(CONFLICTED)
        );
    }

    #[test]
    fn pressing_j_at_the_last_row_does_not_retry_a_failed_file_list() {
        // カーソルが動かないなら対象も変わらないので、試し直す根拠が無い。
        // 落とすと、末尾で `j` を押し続けるあいだ打鍵のたびに失敗を繰り返す。
        let mut state = state_with_commits(1);
        let _ = press(&mut state, 'j');
        let _ = update(&mut state, detail_ok(0));
        let _ = update(&mut state, files_err(0));
        assert_eq!(state.graph.files_failed, Some(fake_oid(0)));
        let notices = state.notices.len();

        // 末尾の行。`j` を押してもカーソルは動かない。
        assert_eq!(press(&mut state, 'j'), Cmd::None);

        assert_eq!(state.graph.files_failed, Some(fake_oid(0)));
        assert_eq!(state.notices.len(), notices);
    }

    #[test]
    fn a_file_list_for_another_commit_is_dropped_and_asked_again() {
        // `JobTracker` の粒度は種別単位なので、要求が飛んでいるあいだのカーソル移動は
        // 落とされる。届いた側で追いつく（詳細と同じ形）。
        let mut state = state_with_commits(5);
        let _ = press(&mut state, 'j');
        assert_eq!(press(&mut state, 'j'), Cmd::None);

        let cmd = update(&mut state, files_ok(0));

        assert_eq!(cmd, Cmd::LoadCommitFiles(cursor(&state)));
        assert!(state.graph.fresh_files().is_none());
    }

    #[test]
    fn a_file_list_that_lands_on_the_wip_row_is_kept() {
        // WIP 行では一覧を 1 つも出していないので、置いても新しいものを潰さない
        // （詳細と同じ）。
        let mut state = state_with_commits(3);
        let _ = press(&mut state, 'j');
        assert_eq!(press(&mut state, 'k'), Cmd::None);

        let _ = update(&mut state, files_ok(0));

        assert_eq!(press(&mut state, 'j'), Cmd::None);
        assert!(state.graph.fresh_files().is_some());
    }

    #[test]
    fn a_failed_file_list_is_not_asked_for_again() {
        // 出すと「失敗 → 再読込 → 失敗」を回す（詳細と同じ）。
        let mut state = state_with_commits(3);
        let _ = press(&mut state, 'j');
        let _ = update(&mut state, detail_ok(0));

        let cmd = update(&mut state, files_err(0));

        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.notices.back().unwrap().level, NoticeLevel::Error);

        // カーソルが動いて戻ってくれば 1 回だけ試し直せる。**2 件目のぶんを届けて
        // ジョブを畳んでから戻る。** 畳まないと、戻ったときの要求が `issue` に
        // 落とされて「印が落ちたか」を見られない。
        let _ = press(&mut state, 'j');
        let _ = update(&mut state, detail_ok(1));
        let _ = update(&mut state, files_ok(1));

        assert_eq!(press(&mut state, 'k'), commit_loads(fake_oid(0)));
    }

    #[test]
    fn a_file_list_failure_for_another_commit_still_asks_for_the_one_on_screen() {
        // 一律に止めると、一度も試していないコミットが「読み込めませんでした」の
        // まま残る（詳細と同じ）。
        let mut state = state_with_commits(3);
        let _ = press(&mut state, 'j');
        let _ = update(&mut state, detail_ok(0));
        // 読み込み中に 1 行動く。一覧の要求は `JobTracker` に落とされ、詳細だけが飛ぶ。
        assert_eq!(press(&mut state, 'j'), Cmd::LoadCommitDetail(fake_oid(1)));

        let cmd = update(&mut state, files_err(0));

        // いま見ているコミットの一覧はまだ一度も試していないので頼む
        // （詳細は走っているので `issue` が落とす）。
        assert_eq!(cmd, Cmd::LoadCommitFiles(cursor(&state)));
    }

    #[test]
    fn a_failed_detail_does_not_block_the_file_list() {
        // 詳細と一覧は別々に失敗する。1 本の印にすると、片方の失敗でもう片方まで
        // 塞ぐ（`GraphState::detail_failed` / `files_failed`）。
        let mut state = state_with_commits(3);
        let _ = press(&mut state, 'j');

        let cmd = update(&mut state, detail_err(0));

        assert_eq!(cmd, Cmd::None, "一覧は既に走っているので落とされる");
        assert!(state.jobs.is_running(JobKind::LoadCommitFiles));
        // 一覧が届いても、失敗した詳細は頼み直さない。
        assert_eq!(update(&mut state, files_ok(0)), Cmd::None);
    }

    #[test]
    fn a_reload_drops_the_file_list() {
        // `lines` に無いコミットの一覧を残さない（詳細と同じ）。
        let mut state = on_first_commit(3);
        assert!(state.graph.files.is_some());

        state.graph.restart();

        assert!(state.graph.files.is_none());
        assert!(state.graph.files_failed.is_none());
    }

    /// 索引 `index` のコミットのファイル一覧が届いた [`Msg`]。中身は `a.txt` 1 件。
    fn files_ok(index: u8) -> Msg {
        files_of(index, &["a.txt"])
    }

    /// `paths` を `Modified` で並べたファイル一覧が届いた [`Msg`]。
    fn files_of(index: u8, paths: &[&str]) -> Msg {
        let entries = paths
            .iter()
            .map(|path| fake_entry(path, FileStatus::Modified))
            .collect();
        files_loaded(index, Ok(entries))
    }

    /// 索引 `index` のコミットのファイル一覧が失敗した [`Msg`]。
    fn files_err(index: u8) -> Msg {
        files_loaded(index, Err(GitReadError::NonUtf8Head))
    }

    fn files_loaded(index: u8, result: Result<Vec<FileEntry>, GitReadError>) -> Msg {
        Msg::CommitFilesLoaded {
            oid: fake_oid(index),
            result: Box::new(result),
        }
    }

    #[test]
    fn moving_onto_a_commit_asks_for_its_detail() {
        let mut state = state_with_commits(3);
        // 起動直後は WIP 行。詳細は右ペインに出るので、乗った時点で読む。
        assert!(state.graph.cursor_commit().is_none());

        let cmd = update(&mut state, Msg::Key(Key::char('j')));

        assert_eq!(cmd, commit_loads(cursor(&state)));
        assert!(state.jobs.is_running(JobKind::LoadCommitDetail));
        assert!(state.jobs.is_running(JobKind::LoadCommitFiles));
    }

    #[test]
    fn moving_back_to_the_wip_row_asks_for_nothing_and_keeps_the_detail() {
        let mut state = on_first_commit(3);

        let cmd = update(&mut state, Msg::Key(Key::char('k')));

        assert_eq!(state.graph.selection(), GraphSelection::Wip);
        assert_eq!(cmd, Cmd::None);
        // WIP 行を通り過ぎるたびに読み直さない（01 の「コミット詳細の遅延取得」）。
        assert!(state.graph.detail.is_some());
    }

    #[test]
    fn coming_back_to_the_same_commit_does_not_ask_again() {
        let mut state = on_first_commit(3);
        let _ = update(&mut state, Msg::Key(Key::char('k')));

        let cmd = update(&mut state, Msg::Key(Key::char('j')));

        assert_eq!(cmd, Cmd::None);
    }

    #[test]
    fn the_detail_follows_the_cursor() {
        let mut state = on_first_commit(3);

        let cmd = update(&mut state, Msg::Key(Key::char('j')));

        assert_eq!(cmd, commit_loads(cursor(&state)));
    }

    #[test]
    fn enter_on_a_commit_moves_to_the_commit_detail_pane() {
        let mut state = on_first_commit(3);

        let cmd = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));

        assert_eq!(state.focus, Pane::CommitDetail);
        assert_eq!(cmd, Cmd::None);
    }

    #[test]
    fn enter_on_the_wip_row_moves_to_the_worktree_pane() {
        // 右ペインの中身は選択で決まるので、`Enter` の行き先も選択で決まる（07）。
        let mut state = state_with_commits(3);

        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));

        assert_eq!(state.focus, Pane::Worktree);
    }

    #[test]
    fn esc_leaves_the_commit_detail_for_the_graph() {
        let mut state = on_first_commit(3);
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));

        let cmd = update(&mut state, Msg::Key(Key::new(KeyCode::Esc)));

        assert_eq!(state.focus, Pane::Graph);
        assert_eq!(cmd, Cmd::None);
    }

    #[test]
    fn esc_leaves_the_worktree_for_the_graph() {
        // グラフの `Enter` で入ってくる先なので、出口を揃える。無いと `Enter` の
        // 行き先によって戻り方が変わる。
        let mut state = state_with_commits(3);
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));
        assert_eq!(state.focus, Pane::Worktree);

        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Esc)));

        assert_eq!(state.focus, Pane::Graph);
    }

    #[test]
    fn a_detail_that_lands_while_the_wip_row_is_selected_is_kept() {
        // WIP 行では詳細を 1 つも出していないので、置いても新しいものを潰さない。
        // 捨てると、コミットに乗って戻るだけで同じものを読み直すことになる。
        let mut state = state_with_commits(3);
        // 1 件目に乗って頼み、返る前に WIP 行へ戻る。
        assert_eq!(
            update(&mut state, Msg::Key(Key::char('j'))),
            commit_loads(fake_oid(0))
        );
        assert_eq!(update(&mut state, Msg::Key(Key::char('k'))), Cmd::None);
        assert_eq!(state.graph.selection(), GraphSelection::Wip);

        let cmd = update(&mut state, detail_ok(0));

        assert_eq!(cmd, Cmd::None);
        // 戻っても読み直しは出ない。
        assert_eq!(update(&mut state, Msg::Key(Key::char('j'))), Cmd::None);
        assert!(state.graph.fresh_detail().is_some());
    }

    #[test]
    fn tab_does_not_reach_the_worktree_while_a_commit_is_selected() {
        // コミットを選んだまま Working tree にフォーカスできると、`s` / `u` / `x` が
        // 画面に出ていないファイルを stage する（01 の「Pane と Modal」）。
        // **`Diff` は候補に残る**（コミットのファイル一覧で選んだ 1 件を左に出す）。
        let mut state = on_first_commit(3);

        for _ in 0..4 {
            let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Tab)));
            assert_ne!(state.focus, Pane::Worktree, "{:?}", state.focus);
        }
    }

    #[test]
    fn a_reload_that_returns_to_the_wip_row_takes_the_focus_off_the_detail() {
        // 選択が変わる経路はカーソルの移動だけではない。commit のあとの読み直しは
        // 先頭から積み直すのでカーソルが WIP 行に戻る（01 の「Pane と Modal」）。
        let mut state = on_first_commit(3);
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));
        assert_eq!(state.focus, Pane::CommitDetail);

        let page = LogPage {
            commits: vec![commit(9)],
            done: true,
        };
        let _ = update(
            &mut state,
            Msg::LogLoaded {
                restart: true,
                result: Ok(page),
            },
        );

        assert_eq!(state.graph.selection(), GraphSelection::Wip);
        assert_eq!(state.focus, Pane::Graph);
    }

    #[test]
    fn a_detail_that_arrives_for_the_wrong_commit_is_asked_for_again() {
        // 要求が飛んでいるあいだのカーソル移動は `JobTracker` に落とされる。
        // 追いつきはここで行う（01 の「コミット詳細の遅延取得」）。
        let mut state = state_with_commits(5);
        let _ = update(&mut state, Msg::Key(Key::char('j')));
        // 読み込み中に 2 行動く。この 2 回は落とされる。
        assert_eq!(update(&mut state, Msg::Key(Key::char('j'))), Cmd::None);
        assert_eq!(update(&mut state, Msg::Key(Key::char('j'))), Cmd::None);

        let cmd = update(&mut state, detail_ok(0));

        assert_eq!(cmd, Cmd::LoadCommitDetail(cursor(&state)));
    }

    #[test]
    fn a_detail_for_the_current_commit_settles() {
        let mut state = state_with_commits(3);
        let _ = update(&mut state, Msg::Key(Key::char('j')));

        let cmd = update(&mut state, detail_ok(0));

        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.graph.detail.as_ref().unwrap().oid, cursor(&state));
        assert!(!state.jobs.is_running(JobKind::LoadCommitDetail));
    }

    #[test]
    fn a_failed_detail_notices_without_asking_again() {
        // 出すと、リポジトリが壊れているあいだ「失敗 → 再読込 → 失敗」を回し続ける。
        let mut state = state_with_commits(3);
        let _ = update(&mut state, Msg::Key(Key::char('j')));

        let cmd = update(&mut state, detail_err(0));

        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.notices.back().unwrap().level, NoticeLevel::Error);
        // 畳まないと、以後の `LoadCommitDetail` が黙って捨てられる。
        assert!(!state.jobs.is_running(JobKind::LoadCommitDetail));
    }

    #[test]
    fn a_page_that_arrives_after_a_failed_detail_does_not_ask_for_it_again() {
        // ページを積んでもカーソルは動かない。ここから頼み直すと、
        // 「同じコミットの失敗からは頼み直さない」の判断が次のページで崩れる。
        let mut state = state_with_page(3, false);
        let _ = update(&mut state, Msg::Key(Key::char('j')));
        let _ = update(&mut state, detail_err(0));

        let page = LogPage {
            commits: vec![commit(7)],
            done: true,
        };
        let cmd = update(&mut state, page_loaded(Ok(page)));

        assert_eq!(cmd, Cmd::None);
        assert!(!state.jobs.is_running(JobKind::LoadCommitDetail));
    }

    #[test]
    fn the_wip_row_of_an_empty_graph_asks_for_no_detail() {
        // 行が 1 つも無くても WIP 行は選べる。読むものは無い。
        let mut state = state();

        let cmd = update(&mut state, Msg::Key(Key::char('j')));

        assert_eq!(cmd, more(&state));
        assert!(!state.jobs.is_running(JobKind::LoadCommitDetail));
    }

    #[test]
    fn a_failed_page_drops_the_detail_with_the_lines() {
        // `lines` に無いコミットの詳細を残さない。
        let mut state = on_first_commit(3);
        let _ = init(&mut state);

        let _ = update(&mut state, page_loaded(Err(GitReadError::NonUtf8Head)));

        assert!(state.graph.detail.is_none());
    }

    #[test]
    fn a_detail_that_arrives_after_the_cursor_came_back_is_dropped() {
        // カーソルが動いて戻ってくると、飛んでいる要求の返りは古いものになる。
        // 上書きすると、正しいものが出ている画面が「読み込み中」に戻り、
        // 同じものをもう 1 回読みに行く。
        let mut state = on_first_commit(3);
        // 2 件目を頼んだ直後に戻る。戻りは `JobTracker` に落とされる。
        assert_eq!(
            update(&mut state, Msg::Key(Key::char('j'))),
            commit_loads(fake_oid(1))
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
        let _ = update(&mut state, Msg::Key(Key::char('j')));
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
        let _ = update(&mut state, page_loaded(Err(GitReadError::NonUtf8Head)));

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
        let _ = update(&mut state, page_loaded(Err(GitReadError::NonUtf8Head)));

        assert_eq!(update(&mut state, Msg::Key(Key::char('j'))), more(&state));
    }

    #[test]
    fn a_successful_page_clears_the_failure() {
        let mut state = state_with_page(3, false);
        let _ = init(&mut state);
        let _ = update(&mut state, page_loaded(Err(GitReadError::NonUtf8Head)));
        assert!(state.graph.load_failed);

        let page = LogPage {
            commits: vec![commit(1)],
            done: false,
        };
        let _ = update(&mut state, page_loaded(Ok(page)));

        assert!(!state.graph.load_failed);
    }

    // ---- commit / amend ----

    /// `Enter` に修飾を付けた打鍵。`Alt+Enter` は `ESC CR` として届く。
    fn enter_with(mods: Modifiers) -> Msg {
        Msg::Key(Key::with_mods(KeyCode::Enter, mods))
    }

    /// 編集中のバッファ。モーダルが出ていなければ `None`。
    fn buffer(state: &AppState) -> Option<&TextBuffer> {
        match &state.modal {
            Some(Modal::CommitMessage { buf, .. }) => Some(buf),
            _ => None,
        }
    }

    #[test]
    fn c_opens_the_commit_message_modal() {
        let mut state = only_staged();

        assert_eq!(press(&mut state, 'c'), Cmd::None);

        assert_eq!(
            state.modal,
            Some(Modal::CommitMessage {
                buf: TextBuffer::default(),
                amend: false,
            })
        );
    }

    #[test]
    fn c_is_refused_when_nothing_is_staged() {
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );

        assert_eq!(press(&mut state, 'c'), Cmd::None);

        assert!(state.modal.is_none());
        assert!(
            state.notices.back().unwrap().message.contains("stage"),
            "{:?}",
            state.notices.back()
        );
    }

    #[test]
    fn c_is_not_refused_before_the_first_status_has_arrived() {
        // 空の一覧には「変更が無い」と「まだ読めていない」の 2 つの意味がある。
        // 後者を根拠に断ると、実際には stage されているのに拒否する。`stale` では
        // 区別できない（あれは書き込みでしか立たない）。
        let mut state = state();
        state.focus = Pane::Worktree;

        let _ = press(&mut state, 'c');

        assert!(state.modal.is_some());
    }

    #[test]
    fn a_commit_while_another_git_run_is_going_keeps_the_message() {
        // **`Cmd::Exec` が落とされるなら閉じない。** 閉じてから返すと、
        // 「ほかの git 実行が終わるまで待ってください」と出て本文ごと消える。
        let mut state = only_staged();
        let _ = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));
        let _ = press(&mut state, 'c');
        type_text(&mut state, "fix");

        assert_eq!(press_code(&mut state, KeyCode::Enter), Cmd::None);

        assert_eq!(buffer(&state).unwrap().text(), "fix");
        assert!(
            state.notices.back().unwrap().message.contains("git 実行"),
            "{:?}",
            state.notices.back()
        );
    }

    #[test]
    fn c_is_not_refused_while_the_status_is_older_than_the_last_write() {
        // 書き込み直後の一覧は届いていない。古い「空の staged」を根拠に断ると、
        // 実際には stage されているのに拒否する（01 の「commit」）。
        let mut state = worktree_state(fake_snapshot(&[], &[], &[]), WorktreeSection::Unstaged);
        state.worktree.stale = true;

        let _ = press(&mut state, 'c');

        assert!(state.modal.is_some());
    }

    #[test]
    fn typing_goes_into_the_buffer_and_q_does_not_quit() {
        let mut state = only_staged();
        let _ = press(&mut state, 'c');

        for c in "fix: q".chars() {
            assert_eq!(press(&mut state, c), Cmd::None);
        }

        // `q` はモーダル表示中は入力文字（01 の「終了経路」）。脱出口は `Ctrl-C`。
        assert_eq!(buffer(&state).unwrap().text(), "fix: q");
    }

    #[test]
    fn a_paste_goes_into_the_message_and_is_ignored_elsewhere() {
        let mut state = only_staged();
        let _ = press(&mut state, 'c');
        let _ = press(&mut state, 'f');

        let cmd = update(&mut state, Msg::Paste("ix: 直す\n\n本文".to_string()));

        assert_eq!(cmd, Cmd::None);
        assert_eq!(buffer(&state).unwrap().text(), "fix: 直す\n\n本文");

        // モーダルが無いときは捨てる。ペインには貼り付けの行き先が無い。
        let mut state = only_staged();
        let before = state.worktree.snapshot.clone();
        let cmd = update(&mut state, Msg::Paste("sux".to_string()));

        assert_eq!(cmd, Cmd::None);
        assert!(state.modal.is_none());
        assert_eq!(state.worktree.snapshot, before);
    }

    #[test]
    fn enter_commits_what_was_typed() {
        let mut state = only_staged();
        let _ = press(&mut state, 'c');
        type_text(&mut state, "fix");

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(
            cmd,
            Cmd::Exec(GitOp::Commit {
                message: "fix".to_string(),
                amend: false,
            })
        );
        // **先に閉じてから返す。** 閉じずに返すと、`Cmd` が `issue` に落とされたときに
        // モーダルだけが画面に残る。
        assert!(state.modal.is_none());
    }

    #[test]
    fn a_modified_enter_and_ctrl_j_insert_a_newline() {
        // 修飾なしの `Enter` だけが確定。`Alt+Enter`（`ESC CR`）と `Shift+Enter`
        // （kitty protocol）と `Ctrl-J` は改行で、どれか 1 つはどの端末でも届く
        // （01 の「コミットメッセージの編集」）。
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };

        for newline in [enter_with(alt), enter_with(shift), Msg::Key(Key::ctrl('j'))] {
            let mut state = only_staged();
            let _ = press(&mut state, 'c');
            let _ = press(&mut state, 'a');

            assert_eq!(update(&mut state, newline), Cmd::None);
            let _ = press(&mut state, 'b');

            assert_eq!(buffer(&state).unwrap().text(), "a\nb");
        }
    }

    #[test]
    fn an_empty_message_is_refused_instead_of_being_handed_to_git() {
        // `git commit --message ""` に任せると、落ちる時点でモーダルは閉じていて
        // 書いた本文が戻らない（01）。空白だけの本文も同じ。
        let mut state = only_staged();
        let _ = press(&mut state, 'c');
        let _ = press(&mut state, ' ');

        assert_eq!(press_code(&mut state, KeyCode::Enter), Cmd::None);

        assert!(state.modal.is_some());
        assert!(
            state.notices.back().unwrap().message.contains("空"),
            "{:?}",
            state.notices.back()
        );
    }

    #[test]
    fn esc_throws_the_message_away_without_running_git() {
        let mut state = only_staged();
        let _ = press(&mut state, 'c');
        let _ = press(&mut state, 'x');

        // 確認は挟まない。`modal` は 1 つしか持てず、重ねるにはスタックが要る（01）。
        assert_eq!(press_code(&mut state, KeyCode::Esc), Cmd::None);
        assert!(state.modal.is_none());
    }

    #[test]
    fn amend_reads_the_head_message_before_opening() {
        // **`RepoInfo.head` からは取らない。** 起動時に 1 回読んだきりで、1 回
        // コミットした時点で古い（01 の「commit」）。
        let mut state = only_staged();

        assert_eq!(press(&mut state, 'C'), Cmd::LoadHeadMessage);
        assert!(state.modal.is_none());

        let cmd = update(
            &mut state,
            Msg::HeadMessageLoaded(Ok(Some("feat: 前のコミット\n\n本文".to_string()))),
        );

        assert_eq!(cmd, Cmd::None);
        assert_eq!(
            state.modal,
            Some(Modal::CommitMessage {
                buf: TextBuffer::from_text("feat: 前のコミット\n\n本文"),
                amend: true,
            })
        );
    }

    #[test]
    fn amend_commits_with_the_amend_flag() {
        let mut state = only_staged();
        let _ = press(&mut state, 'C');
        let _ = update(
            &mut state,
            Msg::HeadMessageLoaded(Ok(Some("old".to_string()))),
        );

        assert_eq!(
            press_code(&mut state, KeyCode::Enter),
            Cmd::Exec(GitOp::Commit {
                message: "old".to_string(),
                amend: true,
            })
        );
    }

    #[test]
    fn amend_on_a_repository_without_commits_says_so() {
        // `Ok(None)` は unborn HEAD。「読めなかった」とは案内が違うので `Err` に
        // 畳まない（02 の `head_message`）。
        let mut state = only_staged();
        let _ = press(&mut state, 'C');

        let _ = update(&mut state, Msg::HeadMessageLoaded(Ok(None)));

        assert!(state.modal.is_none());
        assert!(
            state.notices.back().unwrap().message.contains("amend"),
            "{:?}",
            state.notices.back()
        );
    }

    #[test]
    fn a_head_message_that_arrives_after_another_modal_opened_is_dropped() {
        // 頼んでから届くまでのあいだに `x` の確認が開いていることがある。無条件に
        // 上書きすると、確認モーダルが編集画面に化ける（01 の「commit」）。
        let mut state = only_staged();
        let _ = press(&mut state, 'C');
        let confirm = Modal::Confirm {
            title: "変更を破棄".to_string(),
            body: "a.txt".to_string(),
            on_yes: Cmd::Exec(GitOp::Discard(vec!["a.txt".into()])),
        };
        state.modal = Some(confirm.clone());

        let _ = update(
            &mut state,
            Msg::HeadMessageLoaded(Ok(Some("old".to_string()))),
        );

        assert_eq!(state.modal, Some(confirm));
    }

    // ---- ログの読み直し ----

    fn commit_op() -> GitOp {
        GitOp::Commit {
            message: "fix".to_string(),
            amend: false,
        }
    }

    #[test]
    fn a_commit_reloads_the_log_from_the_top_and_the_refs() {
        // 続きを読む経路では直せない。新しいコミットは末尾ではなく先頭に付く
        // （01 の「ログの読み直し」）。ラベルは `LoadRepoInfo` が読み直す。
        let mut state = state_with_commits(3);
        let _ = state.issue(Cmd::Exec(commit_op()));

        let cmd = update(&mut state, exec_done_of(commit_op(), Ok(fake_output())));

        // **ブランチ一覧も読み直す。** commit は `refs/heads/<現在>` を動かすので、
        // 外すと Branches ペインが commit 前の ahead を出したまま残る。
        assert_eq!(
            cmd,
            Cmd::batch([
                Cmd::LoadStatus,
                Cmd::LoadRepoInfo,
                Cmd::LoadBranches,
                reload(&state)
            ])
        );
    }

    #[test]
    fn a_commit_reloads_the_branch_list_but_staging_does_not() {
        // commit は `refs/heads/<現在>` を動かすので、読み直さないと Branches ペインが
        // commit 前の ahead を出したまま残る。index しか触らない操作では要らない。
        let mut state = state_with_commits(3);
        let _ = state.issue(Cmd::Exec(commit_op()));
        let cmd = update(&mut state, exec_done_of(commit_op(), Ok(fake_output())));
        assert!(issued(&cmd, &Cmd::LoadBranches), "{cmd:?}");

        let mut state = state_with_commits(3);
        let _ = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));
        let cmd = update(&mut state, exec_done(Ok(fake_output())));
        assert!(!issued(&cmd, &Cmd::LoadBranches), "{cmd:?}");
    }

    /// `cmd` に `wanted` が含まれるか。`Batch` の中も見る。
    fn issued(cmd: &Cmd, wanted: &Cmd) -> bool {
        match cmd {
            Cmd::Batch(cmds) => cmds.iter().any(|cmd| issued(cmd, wanted)),
            cmd => cmd == wanted,
        }
    }

    #[test]
    fn staging_does_not_reload_the_log() {
        // 読み直しは `LogSession` の作り直しで履歴全体のコストが掛かる（02）。
        // index しか触らない操作のたびには払えない。
        let mut state = state_with_commits(3);
        let _ = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));

        let cmd = update(&mut state, exec_done(Ok(fake_output())));

        assert_eq!(cmd, Cmd::LoadStatus);
        assert!(!state.graph.reload_pending);
    }

    #[test]
    fn a_page_asked_for_before_the_reload_is_dropped_and_the_reload_is_asked_again() {
        // 続きの読み込みが走っていると、読み直しは `issue` に落ちる。落ちたぶんは
        // 印を付け、走っていたページが届いた時点で頼み直す（01）。
        // 1 ページが窓より短いので、`log_loaded` の `load_more` が続きを頼んだまま。
        let mut state = state_with_page(3, false);
        assert!(state.jobs.is_running(JobKind::LoadLog));
        let _ = state.issue(Cmd::Exec(commit_op()));

        let cmd = update(&mut state, exec_done_of(commit_op(), Ok(fake_output())));

        // 読み直しは落ちている。
        assert_eq!(
            cmd,
            Cmd::batch([Cmd::LoadStatus, Cmd::LoadRepoInfo, Cmd::LoadBranches])
        );
        assert!(state.graph.reload_pending);

        // **走っていたページは積まない。** 積んでも直後の読み直しで捨てるだけで、
        // そのあいだ古い行に新しい行が継ぎ足された状態が画面に出る。
        let page = LogPage {
            commits: vec![commit(9)],
            done: false,
        };
        let cmd = update(&mut state, page_loaded(Ok(page)));

        assert_eq!(state.graph.lines.len(), 3);
        assert_eq!(cmd, reload(&state));
        // 頼み直せたので借りは消える。`jobs.finish` のあとなので必ず出せる。
        assert!(!state.graph.reload_pending);
    }

    #[test]
    fn only_the_operations_that_change_the_walk_reload_the_log() {
        // ラベルと HEAD はグラフの行に焼き込まず `GraphState.refs` から引く（07）ので、
        // 既にあるコミットを指すだけの操作は `LoadRepoInfo` で足りる。読み直しは
        // `LogSession` の作り直しで履歴全体のコストが掛かる（02）。
        assert_eq!(Reload::after(&commit_op(), true), Reload::RefsAndLog);
        assert_eq!(
            Reload::after(
                &GitOp::BranchDelete {
                    name: "x".into(),
                    force: true,
                },
                true
            ),
            Reload::RefsAndLog
        );

        assert_eq!(
            Reload::after(&GitOp::Switch("main".into()), true),
            Reload::Refs
        );
        assert_eq!(Reload::after(&push(false), true), Reload::Refs);
        // force push は `origin/x` が巻き戻り、そこからしか辿れないコミットが落ちる。
        assert_eq!(Reload::after(&push(true), true), Reload::RefsAndLog);

        assert_eq!(
            Reload::after(&GitOp::Stage(vec!["a.txt".into()]), true),
            Reload::Nothing
        );
        assert_eq!(
            Reload::after(
                &GitOp::StageHunk {
                    path: "a.txt".into(),
                    patch: Vec::new(),
                },
                true
            ),
            Reload::Nothing
        );
        // stash は `refs/stash` しか動かさず、グラフはそれを walk に押していない。
        assert_eq!(Reload::after(&GitOp::StashPop(0), true), Reload::Nothing);
    }

    #[test]
    fn a_failed_write_reloads_the_log_only_when_it_could_have_moved_refs_anyway() {
        // 読み直しは `GraphState::restart` でカーソルとスクロールを先頭に戻す。
        // 1 回で決まる操作は、失敗したなら ref が動いていないので位置を捨てない。
        assert_eq!(Reload::after(&commit_op(), false), Reload::Refs);
        assert_eq!(Reload::after(&push(true), false), Reload::Refs);

        // fetch / pull は一部の ref だけ更新して落ちうるし、rebase 系は途中で止まる。
        assert_eq!(
            Reload::after(
                &GitOp::Fetch {
                    remote: None,
                    prune: false,
                },
                false
            ),
            Reload::RefsAndLog
        );
        assert_eq!(
            Reload::after(&GitOp::RebaseContinue, false),
            Reload::RefsAndLog
        );
        assert_eq!(
            Reload::after(&GitOp::RebaseAbort, false),
            Reload::RefsAndLog
        );
    }

    fn push(force: bool) -> GitOp {
        GitOp::Push {
            remote: "origin".into(),
            branch: "main".into(),
            set_upstream: false,
            force,
            expected: None,
        }
    }

    #[test]
    fn an_operation_that_only_moves_labels_does_not_reload_the_log() {
        let mut state = state_with_commits(3);
        let op = GitOp::Switch("main".into());
        let _ = state.issue(Cmd::Exec(op.clone()));

        let cmd = update(&mut state, exec_done_of(op, Ok(fake_output())));

        // ラベル・HEAD・ブランチ一覧は ref から導かれるので読み直す。行は動かない。
        assert_eq!(
            cmd,
            Cmd::batch([Cmd::LoadStatus, Cmd::LoadRepoInfo, Cmd::LoadBranches])
        );
        assert!(!state.graph.reload_pending);
    }

    #[test]
    fn a_second_write_while_the_reload_is_in_flight_is_not_lost() {
        // 1 回目のコミット。読み直しが飛ぶ。
        let mut state = state_with_commits(3);
        let _ = state.issue(Cmd::Exec(commit_op()));
        let _ = update(&mut state, exec_done_of(commit_op(), Ok(fake_output())));
        assert!(state.jobs.is_running(JobKind::LoadLog));

        // 2 回目。読み直しは飛んでいるぶんに落とされる。
        let _ = state.issue(Cmd::Exec(commit_op()));
        let _ = update(&mut state, exec_done_of(commit_op(), Ok(fake_output())));
        assert!(state.graph.reload_pending);

        // **1 回目の読み直しが届いても採らない。** 2 回目より前に頼まれたページなので、
        // 採ると 2 回目のコミットが二度とグラフに出ない。`restart` かどうかで捨てるかを
        // 決めると、ここで印だけ落ちて読み直しが止まる。
        let page = LogPage {
            commits: vec![commit(1)],
            done: true,
        };
        let cmd = update(
            &mut state,
            Msg::LogLoaded {
                restart: true,
                result: Ok(page),
            },
        );

        assert_eq!(cmd, reload(&state));
        assert_eq!(state.graph.lines.len(), 3);
    }

    #[test]
    fn a_reloaded_page_replaces_the_rows_and_seats_the_cursor_at_the_top() {
        let mut state = state_with_commits(3);
        state.graph.cursor = 2;
        state.graph.detail = Some(fake_detail(2, "old"));
        let _ = state.issue(Cmd::Exec(commit_op()));
        let _ = update(&mut state, exec_done_of(commit_op(), Ok(fake_output())));

        let page = LogPage {
            commits: vec![commit(7)],
            done: true,
        };
        let _ = update(
            &mut state,
            Msg::LogLoaded {
                restart: true,
                result: Ok(page),
            },
        );

        assert_eq!(state.graph.lines.len(), 1);
        assert_eq!(state.graph.lines[0].commit.oid, commit(7).oid);
        assert_eq!(state.graph.cursor, 0);
        // `lines` に無いコミットの詳細を残さない。
        assert!(state.graph.detail.is_none());
        assert!(!state.graph.reload_pending);
    }

    #[test]
    fn a_reload_that_fails_does_not_leave_the_log_waiting_for_one() {
        // 失敗のあとの読み込みはユーザーの操作から始まる。印を残すと、届かない
        // 読み直しを待ち続ける。
        let mut state = state_with_commits(3);
        let _ = state.issue(Cmd::Exec(commit_op()));
        let _ = update(&mut state, exec_done_of(commit_op(), Ok(fake_output())));

        let _ = update(
            &mut state,
            Msg::LogLoaded {
                restart: true,
                result: Err(GitReadError::NonUtf8Head),
            },
        );

        assert!(!state.graph.reload_pending);
        assert!(state.graph.load_failed);
        assert!(state.graph.lines.is_empty());
    }
}
