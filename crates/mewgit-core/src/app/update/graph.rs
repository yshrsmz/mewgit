//! Graph ペインと、そこから開くコミット詳細（01 の「コミット詳細の遅延取得」）。
//!
//! ログのページングと、カーソルが指すコミットの詳細・ファイル一覧の頼み方もここ。
//! **どれも「届いた返信がいま見ているコミットのものか」を数える**ので、追いつきの
//! 判断を並べて読めるように 1 つのモジュールに置いてある。

use crate::git_exec::GitOp;
use crate::git_read::{
    short_oid, Commit, CommitDetail, FileEntry, GitReadError, HeadInfo, LogPage, Oid,
};

use super::branches::open_branch_create;
use super::diff::{load_diff, open_diff};
use super::rebase::open_rebase_editor;
use super::{confirm, focus};
use crate::app::{
    AppState, Cmd, CommitFiles, GraphLine, JobKind, Key, KeyCode, Modal, NoticeLevel, Pane,
    ParentChoice,
};

/// グラフペインのキー処理。
///
/// いまは上下移動、下端に近づいたときの追加読み込み、`Enter` で右ペインへ移る。
/// `g` / `G` は別のタスクで足す。
pub(super) fn graph_key(state: &mut AppState, key: Key) -> Cmd {
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
        KeyCode::Char(' ') => toggle_mark(state),
        // 印が無ければ何もしない（`Esc` は Graph のほかの何にも使っていない）。
        KeyCode::Esc => {
            state.graph.marks.clear();
            Cmd::None
        }
        KeyCode::Char('y') => cherry_pick(state),
        KeyCode::Char('v') => revert(state),
        KeyCode::Char('i') => open_rebase_editor(state),
        _ => Cmd::None,
    }
}

/// `Space`。カーソルの指すコミットの印を付け外しする（01 の「複数のコミットの
/// cherry-pick / revert（印）」）。
fn toggle_mark(state: &mut AppState) -> Cmd {
    if let Some(commit) = picked_commit(state, "mark") {
        state.graph.toggle_mark(commit.oid);
    }
    Cmd::None
}

/// `y` / `v` の相手。
enum Targets {
    /// 印が 1 つならそのコミット、無ければカーソルの指すコミット。**1 つだけの印は
    /// カーソルの 1 本と同じ経路に通す**（01 の「複数のコミットの cherry-pick / revert（印）」）。
    One(Commit),
    /// 2 本以上の印。**グラフの並び（新しい順）** —— git に渡す向きは op で逆なので、
    /// 並べ直すのは呼び手（03 の「複数のコミットの cherry-pick / revert」）。
    Many(Vec<Commit>),
}

impl Targets {
    fn commits(&self) -> &[Commit] {
        match self {
            Targets::One(commit) => std::slice::from_ref(commit),
            Targets::Many(commits) => commits,
        }
    }
}

/// `y` / `v` の相手を印とカーソルから決める。WIP 行で印も無ければ断る（[`picked_commit`]）。
/// **merge を受けるかは呼び手が決める**（cherry-pick は 1 本でも断り、revert は 1 本なら
/// 親を選ばせる）。
fn targets(state: &mut AppState, verb: &str) -> Option<Targets> {
    let mut marked: Vec<Commit> = state
        .graph
        .marked_lines()
        .into_iter()
        .map(|line| line.commit.clone())
        .collect();
    match marked.len() {
        0 => picked_commit(state, verb).map(Targets::One),
        1 => marked.pop().map(Targets::One),
        _ => Some(Targets::Many(marked)),
    }
}

/// 複数の確認の本文の末尾 —— 並べたとおりの短縮 oid。summary は並べない（本数に上限が
/// 無く、確認の箱に入らない。01）。
fn short_oids(oids: &[Oid]) -> String {
    oids.iter().map(short_oid).collect::<Vec<_>>().join(" ")
}

/// `y`。カーソルの指すコミットを HEAD に cherry-pick する（01 の「cherry-pick の開始（`y`）」）。
/// **印があれば印の全部を古い順に**積む（01 の「複数のコミットの cherry-pick / revert（印）」）。
///
/// **確認モーダルを挟む**（2026-09-29 にユーザーと決めた）。破壊的ではないが、積んだ
/// コミットを戻す手が mewgit に無く、どのコミットをどこへ積むかを押す前に見せる。
///
/// **進行中操作は手前で見ない。** 断るのは確定のあとの `AppState::issue`（ブランチの
/// 削除と同じ。01 の「進行中操作があるときの書き込み」）。
fn cherry_pick(state: &mut AppState) -> Cmd {
    let Some(targets) = targets(state, "cherry-pick") else {
        return Cmd::None;
    };
    // **merge コミットは手前で断る。** git は `-m` が無いと exit 128 で断り、複数なら
    // その手前まで積んでから止まる（03 の実測）。`GitOp::CherryPick` は mainline を運ばない。
    if targets
        .commits()
        .iter()
        .any(|commit| commit.parents.len() > 1)
    {
        state.push_notice(
            NoticeLevel::Warn,
            "merge コミットは cherry-pick できません".to_string(),
            None,
        );
        return Cmd::None;
    }
    let (body, oids) = match targets {
        Targets::One(commit) => (
            format!(
                "{} {} を {} に積みます。",
                short_oid(&commit.oid),
                commit.summary,
                head_name(state)
            ),
            vec![commit.oid],
        ),
        Targets::Many(commits) => {
            // git は渡した順に積むので、グラフの並びを逆にして古い順に渡す。
            let oids: Vec<Oid> = commits.iter().rev().map(|commit| commit.oid).collect();
            let body = format!(
                "{} 本のコミットを {} に古い順に積みます: {}",
                oids.len(),
                head_name(state),
                short_oids(&oids)
            );
            (body, oids)
        }
    };
    confirm(state, "cherry-pick", body, GitOp::CherryPick(oids))
}

/// `v`。カーソルの指すコミットを取り消すコミットを HEAD に積む（01 の「revert の開始（`v`）」）。
/// **印があれば印の全部を新しい順に**取り消す（[`revert_many`]）。
///
/// **確認モーダルを挟む**のも、**進行中操作を手前で見ない**のも `y` と同じ理由。
/// **merge コミットは残す側の親を選ばせてから確認へ進む**（[`Modal::MainlinePicker`]）。
/// git は `-m` が無いと exit 128 で断る（03 の「revert の開始」）。
fn revert(state: &mut AppState) -> Cmd {
    let commit = match targets(state, "revert") {
        None => return Cmd::None,
        Some(Targets::One(commit)) => commit,
        Some(Targets::Many(commits)) => return revert_many(state, &commits),
    };
    if commit.parents.len() <= 1 {
        return confirm_revert(state, commit.oid, &commit.summary, None);
    }
    // `ParentChoice::summary`
    let parents = commit
        .parents
        .iter()
        .map(|&oid| ParentChoice {
            oid,
            summary: state
                .graph
                .lines
                .iter()
                .find(|line| line.commit.oid == oid)
                .map(|line| line.commit.summary.clone()),
        })
        .collect();
    state.modal = Some(Modal::MainlinePicker {
        target: commit.oid,
        summary: commit.summary,
        parents,
        cursor: 0,
    });
    Cmd::None
}

/// 2 本以上の印の revert。**グラフの並び（新しい順）のまま渡す** —— 古い順だと、後の
/// コミットが同じ箇所を変えていれば 1 本目でコンフリクトする（03 の実測）。
///
/// **印に merge があれば断る。** `--mainline N` は全部の merge に同じ N が効くので親を
/// 選ばせられず、`-m` が無いと git はその手前まで積んでから止まる（03）。
fn revert_many(state: &mut AppState, commits: &[Commit]) -> Cmd {
    if commits.iter().any(|commit| commit.parents.len() > 1) {
        state.push_notice(
            NoticeLevel::Warn,
            "印に merge コミットがあります。複数の revert には入れられません".to_string(),
            None,
        );
        return Cmd::None;
    }
    let oids: Vec<Oid> = commits.iter().map(|commit| commit.oid).collect();
    let body = format!(
        "{} 本のコミットを取り消すコミットを {} に新しい順に積みます: {}",
        oids.len(),
        head_name(state),
        short_oids(&oids)
    );
    let op = GitOp::Revert {
        oids,
        mainline: None,
    };
    confirm(state, "revert", body, op)
}

/// [`Modal::MainlinePicker`] の `Enter`。選んだ親を残す側として確認へ進む。
pub(super) fn mainline_picked(state: &mut AppState) -> Cmd {
    let picked = match &state.modal {
        Some(Modal::MainlinePicker {
            target,
            summary,
            parents,
            cursor,
        }) => parents
            .get(*cursor)
            .map(|parent| (*target, summary.clone(), (*cursor, parent.oid))),
        _ => None,
    };
    let Some((target, summary, keep)) = picked else {
        state.modal = None;
        return Cmd::None;
    };
    confirm_revert(state, target, &summary, Some(keep))
}

/// `y` / `v` / `i` の相手。**WIP 行では断る**（積むコミットも取り消すコミットも無い）。
///
/// **写しを返す**のは、呼び出し側が続けて `state` を書き換える（通知・モーダル）ため。
pub(super) fn picked_commit(state: &mut AppState, verb: &str) -> Option<Commit> {
    let commit = state.graph.cursor_commit().cloned();
    if commit.is_none() {
        state.push_notice(
            NoticeLevel::Info,
            format!("{verb} するコミットの行を選んでください"),
            None,
        );
    }
    commit
}

/// revert の確認を開く。`keep` は merge コミットで残す側の親（[`Modal::MainlinePicker`] の
/// 添字と oid）。**添字 + 1 が `--mainline`**（git は 1 始まり）。
fn confirm_revert(
    state: &mut AppState,
    target: Oid,
    summary: &str,
    keep: Option<(usize, Oid)>,
) -> Cmd {
    let mut body = format!(
        "{} {} を取り消すコミットを {} に積みます。",
        short_oid(&target),
        summary,
        head_name(state)
    );
    let mainline = keep.map(|(index, parent)| {
        let mainline = index + 1;
        body.push_str(&format!(
            "残す側は {mainline} 番目の親 {} です。",
            short_oid(&parent)
        ));
        // 親の数を超えないので u32 に収まる。
        u32::try_from(mainline).expect("親の番号が u32 に収まりません")
    });
    let op = GitOp::Revert {
        oids: vec![target],
        mainline,
    };
    confirm(state, "revert", body, op)
}

/// 確認の本文に出す積む先。**表示だけに使う**（git は HEAD に積むので、ここが古くても
/// 積む先は変わらない）。
///
/// **HEAD の行を先に読む**（02 の「head」）。行が無いときだけ unborn の名前を見る。
fn head_name(state: &AppState) -> &str {
    if let Some(branch) = state.branches.head() {
        return &branch.name;
    }
    match &state.repo.head {
        Some(HeadInfo::Unborn { name }) => name,
        _ => "HEAD",
    }
}

/// コミット詳細ペインのキー処理。
///
/// カーソルはファイル一覧の行を指す。`Enter` で左を `DiffTarget::Commit` の diff に
/// 差し替える（07 の「CommitDetail ペイン」）。
pub(super) fn commit_detail_key(state: &mut AppState, key: Key) -> Cmd {
    match key.code() {
        KeyCode::Char('j') | KeyCode::Down => move_file(state, 1),
        KeyCode::Char('k') | KeyCode::Up => move_file(state, -1),
        KeyCode::Enter => open_diff(state),
        KeyCode::Esc => focus(state, Pane::Graph),
        _ => Cmd::None,
    }
}

/// ファイル一覧のカーソルを `delta` 行動かす。
///
/// **動かすと diff の対象が変わるので、直前の失敗を落とす**（ワーキングツリーの
/// `worktree::moved` と同じ形）。移動して戻ってくれば 1 回だけ試し直せる。
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
    // 続きを頼む: ログの追加ページと、カーソルの指すコミットの詳細 / ファイル一覧。
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
pub(super) fn commit_detail_loaded(
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
    // 続きを頼む: 別のコミットの詳細が届いたときの頼み直し。
    load_commit(state)
}

/// 届いたファイル一覧を積む。[`commit_detail_loaded`] と同じ形。
///
/// **一覧が届いたら `load_diff` も通す。** 左が Diff に差し替わっていても、一覧が
/// 届くまでは選んでいるファイルが決まらない（`wanted_diff()` が `None`）。`Enter` や
/// `Tab` で先に `Pane::Diff` へ移れるので、ここで頼み直さないと Diff ペインが
/// 「読み込めませんでした」のまま残る（01 の「コミットのファイル一覧」）。
pub(super) fn commit_files_loaded(
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
    // 続きを頼む: 別のコミットの一覧が届いたときの頼み直しと、一覧が届いて対象が
    // 決まった diff。
    Cmd::batch([load_commit(state), load_diff(state)])
}

/// 下端に近づいたら次のページを頼む。
///
/// 残りが 1 画面を切ったら出す。同じ種類のジョブが走っているあいだは
/// [`AppState::issue`] が落とすので、ここでは走っているかを見ない
/// （見ると 2 箇所で同じことを判断することになる）。
///
/// **世代も見ない。** 読み直しが要る状態では必ず `LoadLog` が走っている
/// （`exec_finished` がその場で読み直しを出し、出せなければ落とした相手が走っている
/// ため。`log_loaded` の出口でも必ず出し直す）ので、ここで続きを出しても `issue` が
/// 落とす。見る形にすると、テストで踏めない分岐が残る。
pub(super) fn load_more(state: &AppState) -> Cmd {
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
/// 出すかどうかはここでは決めない。呼び出し元はどれも出すと決めたあとで呼ぶ
/// （`exec::reload` —— 書き込みと外の変更 —— と、落とされたぶんを頼み直す `log_loaded`）。
/// 判断を置くと、テストで踏めない偽の腕が残る。
pub(super) fn reload_log(state: &AppState) -> Cmd {
    Cmd::LoadLog {
        count: state.config.graph.page_size,
        restart: true,
    }
}

/// ログのページを積む。
///
/// `restart` は**要求した内容**で、届いたページが先頭からのものか続きかを表す
/// （`LogPage` は位置を持たない）。
pub(super) fn log_loaded(
    state: &mut AppState,
    restart: bool,
    result: Result<LogPage, GitReadError>,
) -> Cmd {
    let asked = state.jobs.finish(JobKind::LoadLog);

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

    // **書き込みより前に頼んだページは、続きでも読み直しでも捨てる。** ref が動いた
    // あとの walk には別のコミットが並ぶので、積むと消えたはずの行が残る。`restart`
    // かどうかでは分けない —— 分けると、1 つ前の書き込みの読み直しが届いたときに
    // 借りだけ消えて、新しい書き込みが二度とグラフに出ない（続けて 2 回コミットすると
    // 踏む）。ここまで来ていれば `jobs.finish` のあとなので、頼み直しは必ず出せる
    // （01 の「ログの読み直し」）。
    //
    // **失敗の処理より後に置く。** 先に置くと、古いページが失敗したときに
    // `load_failed` を立てないまま読み直しを出し、「失敗 → 再読込 → 失敗」を回す。
    // 続きを頼む: 書き込みより前に頼んだページを捨てたぶんの読み直し。
    if !state.jobs.is_fresh(JobKind::LoadLog, asked) {
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
    //
    // 続きを頼む: 窓が埋まるまでのログの追加ページ。
    load_more(state)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        cells, commit, commit_loads, cursor, detail_ok, diff_ok, fake_diff, files_loaded, files_of,
        more, on_first_commit, open_confirm, page_loaded, press, press_code, reload, state,
        state_with_commits, state_with_page, tick, NOTICE_CELLS, ROWS,
    };
    use super::super::worktree::COMMIT_DIFF;
    use super::super::{init, update};
    use super::*;
    use crate::app::{GraphSelection, Msg};

    use crate::git_read::{DiffTarget, FileStatus};
    use crate::graph::LaneState;
    use crate::test_util::{
        fake_branch, fake_commit_with_parents, fake_entry, fake_oid, fake_snapshot, reply,
    };

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
        let cmd = more(&state);
        let _ = reply(&mut state, cmd, page_loaded(Ok(first)));

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

    /// 索引 `index` のコミットのファイル一覧が届いた [`Msg`]。中身は `a.txt` 1 件。
    ///
    /// **利用者はこのモジュールだけなので `test_support` に置かない**（00 の
    /// 「ディレクトリ構成」）。`files_of` のほうは 2 者が呼ぶのであちら。
    fn files_ok(index: u8) -> Msg {
        files_of(index, &["a.txt"])
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

    /// 索引 `index` のコミットのファイル一覧が失敗した [`Msg`]。
    fn files_err(index: u8) -> Msg {
        files_loaded(index, Err(GitReadError::NonUtf8Head))
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
        let cmd = reload(&state);
        let _ = reply(
            &mut state,
            cmd,
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
        assert_eq!(update(&mut state, tick()), Cmd::None);
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
        let cmd = more(&state);
        let _ = reply(&mut state, cmd, page_loaded(Ok(page)));

        assert!(!state.graph.load_failed);
    }

    // ---- commit / amend ----

    /// HEAD を `main` にする。確認の本文が積む先の名前を出すかを見るため。名前は
    /// HEAD の行から読む（02 の「head」）。
    fn on_main(state: &mut AppState) {
        state.branches.branches = vec![fake_branch("main", true)];
    }

    #[test]
    fn the_target_name_comes_from_the_head_row_and_falls_back_to_repo_head() {
        // HEAD の行を先に読み、行が無いときだけ `repo.head` を見る（02 の「head」）。
        // 取り違えを見張るため、行と `repo.head` に別の名前を持たせる。
        let body_with = |row: Option<&str>, head: Option<HeadInfo>| {
            let mut state = state_with_commits(3);
            state.branches.branches = row
                .map(|name| fake_branch(name, true))
                .into_iter()
                .collect();
            state.repo.head = head;
            let _ = press(&mut state, 'j');
            let _ = press(&mut state, 'y');
            open_confirm(&state).0.to_string()
        };
        let unborn = || {
            Some(HeadInfo::Unborn {
                name: "fresh".to_string(),
            })
        };

        let body = body_with(Some("main"), unborn());
        assert!(body.contains("main") && !body.contains("fresh"), "{body}");
        let body = body_with(None, unborn());
        assert!(body.contains("fresh"), "{body}");
        let body = body_with(None, Some(HeadInfo::Detached(fake_oid(9))));
        assert!(body.contains("を HEAD に"), "{body}");
    }

    #[test]
    fn y_on_a_commit_asks_before_cherry_picking_it_onto_head() {
        let mut state = state_with_commits(3);
        on_main(&mut state);
        let _ = press(&mut state, 'j');
        let oid = cursor(&state);

        let cmd = press(&mut state, 'y');

        assert_eq!(cmd, Cmd::None);
        let (body, on_yes) = open_confirm(&state);
        // どのコミットをどこへ積むかを押す前に見せる（01 の「cherry-pick の開始（`y`）」）。
        assert!(body.contains(&short_oid(&oid)), "{body}");
        assert!(body.contains("commit 0"), "{body}");
        assert!(body.contains("main"), "{body}");
        let expected = Cmd::Exec(GitOp::CherryPick(vec![oid]));
        assert_eq!(on_yes, &expected);
        assert_eq!(press(&mut state, 'y'), expected);
    }

    #[test]
    fn y_on_the_wip_row_has_nothing_to_pick() {
        let mut state = state_with_commits(3);
        assert_eq!(state.graph.selection(), GraphSelection::Wip);

        let cmd = press(&mut state, 'y');

        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.modal, None);
        let notice = state.shown_notice().expect("通知が出ていません");
        assert!(
            notice.message.contains("コミットの行"),
            "{}",
            notice.message
        );
    }

    #[test]
    fn y_on_a_merge_commit_is_refused_before_git_runs() {
        // `-m` が無いと git は exit 128 で断る（03 の実測）。`GitOp::CherryPick` は
        // mainline を運ばないので、確認を開く前に断る。
        let mut state = state_with_commits(3);
        state.graph.lines[0].commit.parents = vec![fake_oid(1), fake_oid(2)];
        let _ = press(&mut state, 'j');

        let cmd = press(&mut state, 'y');

        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.modal, None);
        let notice = state.shown_notice().expect("通知が出ていません");
        assert_eq!(notice.level, NoticeLevel::Warn);
        assert!(
            notice.message.contains("merge コミット"),
            "{}",
            notice.message
        );
    }

    #[test]
    fn v_on_a_commit_asks_before_reverting_it_onto_head() {
        let mut state = state_with_commits(3);
        on_main(&mut state);
        let _ = press(&mut state, 'j');
        let oid = cursor(&state);

        let cmd = press(&mut state, 'v');

        assert_eq!(cmd, Cmd::None);
        let (body, on_yes) = open_confirm(&state);
        // どのコミットを取り消し、どこへ積むかを押す前に見せる（01 の「revert の開始（`v`）」）。
        assert!(body.contains(&short_oid(&oid)), "{body}");
        assert!(body.contains("commit 0"), "{body}");
        assert!(body.contains("main"), "{body}");
        let expected = Cmd::Exec(GitOp::Revert {
            oids: vec![oid],
            mainline: None,
        });
        assert_eq!(on_yes, &expected);
        assert_eq!(press(&mut state, 'y'), expected);
    }

    #[test]
    fn v_on_the_wip_row_has_nothing_to_revert() {
        let mut state = state_with_commits(3);
        assert_eq!(state.graph.selection(), GraphSelection::Wip);

        let cmd = press(&mut state, 'v');

        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.modal, None);
        let notice = state.shown_notice().expect("通知が出ていません");
        assert!(
            notice.message.contains("コミットの行"),
            "{}",
            notice.message
        );
    }

    #[test]
    fn v_on_a_merge_commit_picks_the_parent_to_keep_before_asking() {
        // 1 番目の親は読み込み済み（commit 1）、2 番目は読み込んでいない。
        let mut state = state_with_commits(3);
        let unloaded = fake_oid(42);
        state.graph.lines[0].commit.parents = vec![fake_oid(1), unloaded];
        let _ = press(&mut state, 'j');
        let target = cursor(&state);

        assert_eq!(press(&mut state, 'v'), Cmd::None);

        let Some(Modal::MainlinePicker { parents, .. }) = &state.modal else {
            panic!("親を選ぶモーダルが開いていません: {:?}", state.modal);
        };
        // **読み込み済みの親だけ summary を持つ**（`ParentChoice::summary`）。
        assert_eq!(
            parents,
            &vec![
                ParentChoice {
                    oid: fake_oid(1),
                    summary: Some("commit 1".to_string()),
                },
                ParentChoice {
                    oid: unloaded,
                    summary: None,
                },
            ]
        );

        // 2 番目を選ぶと `--mainline 2`（git は 1 始まり）。確認を挟む。
        let _ = press(&mut state, 'j');
        assert_eq!(press_code(&mut state, KeyCode::Enter), Cmd::None);
        let (body, on_yes) = open_confirm(&state);
        assert!(body.contains("2 番目の親"), "{body}");
        assert!(body.contains(&short_oid(&unloaded)), "{body}");
        let expected = Cmd::Exec(GitOp::Revert {
            oids: vec![target],
            mainline: Some(2),
        });
        assert_eq!(on_yes, &expected);
        assert_eq!(press(&mut state, 'y'), expected);
    }

    #[test]
    fn esc_on_the_parent_picker_reverts_nothing() {
        let mut state = state_with_commits(3);
        state.graph.lines[0].commit.parents = vec![fake_oid(1), fake_oid(2)];
        let _ = press(&mut state, 'j');
        let _ = press(&mut state, 'v');
        assert!(matches!(state.modal, Some(Modal::MainlinePicker { .. })));

        assert_eq!(press_code(&mut state, KeyCode::Esc), Cmd::None);

        assert_eq!(state.modal, None);
    }

    /// 表示行 `row`（1 始まりのコミット行）へカーソルを移して印を付ける。
    fn mark_row(state: &mut AppState, row: usize) -> Oid {
        state.graph.cursor = row;
        assert_eq!(press(state, ' '), Cmd::None);
        cursor(state)
    }

    #[test]
    fn space_marks_and_unmarks_the_cursor_commit() {
        let mut state = state_with_commits(3);
        let oid = mark_row(&mut state, 2);
        assert_eq!(state.graph.marks, vec![oid]);

        let _ = press(&mut state, ' ');

        assert!(state.graph.marks.is_empty());
    }

    #[test]
    fn space_on_the_wip_row_marks_nothing() {
        let mut state = state_with_commits(3);

        assert_eq!(press(&mut state, ' '), Cmd::None);

        assert!(state.graph.marks.is_empty());
        let notice = state.shown_notice().expect("通知が出ていません");
        // `picked_commit` は `{verb} する…` と組むので、動詞は `y` / `v` と同じ git の語の形。
        assert!(
            notice.message.contains("mark するコミットの行"),
            "{}",
            notice.message
        );
    }

    #[test]
    fn esc_in_the_graph_takes_every_mark_off() {
        let mut state = state_with_commits(3);
        let _ = mark_row(&mut state, 1);
        let _ = mark_row(&mut state, 3);

        assert_eq!(press_code(&mut state, KeyCode::Esc), Cmd::None);

        assert!(state.graph.marks.is_empty());
        // Graph に居たまま（Esc は戻る先を持たない）。
        assert_eq!(state.focus, Pane::Graph);
    }

    #[test]
    fn y_with_marks_picks_all_of_them_oldest_first() {
        // git は渡した順に積む（03 の実測）。付けた順（新しい → 古い）ではなく、
        // グラフの並びを逆にして古い順に渡す。
        let mut state = state_with_commits(3);
        on_main(&mut state);
        let newest = mark_row(&mut state, 1);
        let oldest = mark_row(&mut state, 3);
        // カーソルの行（commit 1）は印が無いので入らない。
        state.graph.cursor = 2;

        assert_eq!(press(&mut state, 'y'), Cmd::None);

        let (body, on_yes) = open_confirm(&state);
        assert!(body.contains("2 本"), "{body}");
        assert!(body.contains("main"), "{body}");
        assert!(
            body.contains(&format!("{} {}", short_oid(&oldest), short_oid(&newest))),
            "{body}"
        );
        assert_eq!(on_yes, &Cmd::Exec(GitOp::CherryPick(vec![oldest, newest])));
    }

    #[test]
    fn v_with_marks_reverts_all_of_them_newest_first() {
        // 古い順に取り消すと、後のコミットが同じ箇所を変えていれば 1 本目で
        // コンフリクトする（03 の実測）。付けた順に関わらず新しい順に渡す。
        let mut state = state_with_commits(3);
        on_main(&mut state);
        let oldest = mark_row(&mut state, 3);
        let newest = mark_row(&mut state, 1);

        assert_eq!(press(&mut state, 'v'), Cmd::None);

        let (body, on_yes) = open_confirm(&state);
        assert!(body.contains("2 本"), "{body}");
        assert!(
            body.contains(&format!("{} {}", short_oid(&newest), short_oid(&oldest))),
            "{body}"
        );
        let expected = Cmd::Exec(GitOp::Revert {
            oids: vec![newest, oldest],
            mainline: None,
        });
        assert_eq!(on_yes, &expected);
    }

    #[test]
    fn marks_with_a_merge_commit_are_refused_before_git_runs() {
        // git は merge の手前まで積んでから exit 128 で止まり、`sequencer/` だけが残る
        // （03 の「複数のコミットの cherry-pick / revert」）。
        for key in ['y', 'v'] {
            let mut state = state_with_commits(3);
            state.graph.lines[1].commit.parents = vec![fake_oid(1), fake_oid(2)];
            let _ = mark_row(&mut state, 1);
            let _ = mark_row(&mut state, 2);

            assert_eq!(press(&mut state, key), Cmd::None);

            assert_eq!(state.modal, None, "{key}");
            let notice = state.shown_notice().expect("通知が出ていません");
            assert_eq!(notice.level, NoticeLevel::Warn);
            assert!(
                notice.message.contains("merge コミット"),
                "{}",
                notice.message
            );
            assert!(
                cells(&notice.message) <= NOTICE_CELLS,
                "{} セル: {}",
                cells(&notice.message),
                notice.message
            );
        }
    }

    #[test]
    fn a_single_mark_takes_the_place_of_the_cursor() {
        // 1 つだけの印はカーソルの 1 本と同じ経路（01）—— merge なら親を選ばせる。
        let mut state = state_with_commits(3);
        state.graph.lines[2].commit.parents = vec![fake_oid(1), fake_oid(2)];
        let marked = mark_row(&mut state, 3);
        state.graph.cursor = 0;

        assert_eq!(press(&mut state, 'v'), Cmd::None);

        let Some(Modal::MainlinePicker { target, .. }) = &state.modal else {
            panic!("親を選ぶモーダルが開いていません: {:?}", state.modal);
        };
        assert_eq!(*target, marked);
    }

    #[test]
    fn reloading_the_log_takes_the_marks_off() {
        // 並べ直すには印のコミットが読み込み済みの行に居る必要がある（01）。
        let mut state = state_with_commits(3);
        let _ = mark_row(&mut state, 1);

        state.graph.restart();

        assert!(state.graph.marks.is_empty());
    }
}
