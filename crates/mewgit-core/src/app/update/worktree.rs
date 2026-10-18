//! ワーキングツリーのキー処理と、stage / unstage / discard の計画（01）。
//!
//! **`(操作, 区画, 対象)` から `GitOp` を選ぶ表は 1 本**（[`plan`]）で、ファイル単位
//! （Worktree ペイン）とハンク単位（Diff ペイン）が同じ表を通る。`c` / `C` の
//! コミットモーダルもここ。

use std::path::{Path, PathBuf};

use crate::git_exec::{hunk_patch, supports_hunk_patch, GitOp, HunkApply};
use crate::git_read::{
    FileEntry, FileStatus, GitReadError, Hunk, PathStamp, WorktreeSection, WorktreeSnapshot,
};

use super::diff::load_diff;
use super::diff::open_diff;
use super::stash::{open_stash_file, open_stash_push};
use super::{confirm, focus, multiline_input_key, submit_exec, InputKey};
use crate::app::keys::WorktreeAction;
use crate::app::{AppState, Cmd, DiffOrigin, JobKind, Key, Modal, NoticeLevel, Pane, TextBuffer};

/// ワーキングツリーペインのキー処理。上下移動は `update_key` の `move_cursor` が受ける
/// （[`move_entry`]）。
pub(super) fn worktree_key(state: &mut AppState, action: WorktreeAction) -> Cmd {
    match action {
        // 左の領域を Diff に差し替える（07）。読むかどうかは `focus` が決める。
        WorktreeAction::Diff => open_diff(state),
        // **グラフの `Enter` で入ってきた先なので、`Esc` で戻れる。** 戻り先を
        // 用意しないと、`Enter` の行き先がコミット詳細か Working tree かで出口が
        // 変わる（片方は `Esc`、もう片方は `Tab` を探す）ことになる。
        WorktreeAction::Back => focus(state, Pane::Graph),
        WorktreeAction::Stage => act(state, Action::Stage),
        WorktreeAction::Unstage => act(state, Action::Unstage),
        WorktreeAction::Discard => act(state, Action::Discard),
        WorktreeAction::Commit => commit(state),
        WorktreeAction::Amend => amend(state),
        WorktreeAction::Stash => open_stash_push(state),
        WorktreeAction::StashFile => open_stash_file(state),
    }
}

/// `c`。staged を新しいコミットにする。
///
/// **staged が空でも、いま持っている一覧を信じられないなら断らない。** 書き込み直後の
/// 一覧は古く（`stale`）、一度も読めていない一覧は空と区別が付かない（`loaded`）。
/// どちらも「変更がありません」の根拠にすると、実際には stage されているのに拒否する
/// （01 の「commit」）。そのときは開いて git の側に判定を任せる。
fn commit(state: &mut AppState) -> Cmd {
    let trusted = state.worktree_is_current();
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
pub(super) fn commit_message_key(state: &mut AppState, key: Key) -> Cmd {
    let Some(Modal::CommitMessage { buf, .. }) = state.modal.as_mut() else {
        return Cmd::None;
    };
    // 振り分けと改行・行移動の規則は RebaseEditor の reword と同じ
    // （`update::multiline_input_key`）。
    match multiline_input_key(buf, key) {
        // **確認は挟まない。** `modal` は 1 つしか持てず、重ねるにはモーダルのスタックが
        // 要る。不変ルール 5 が確認を求めるのは git の破壊的操作で、編集中バッファの
        // 取り消しは含まれない（01）。
        InputKey::Cancel => {
            state.modal = None;
            Cmd::None
        }
        InputKey::Submit => commit_submit(state),
        InputKey::Edited => Cmd::None,
    }
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

/// `C`。HEAD のメッセージを読んでから amend の入力欄を開く。
///
/// **初期値は読んでから開く**（01 の「commit」）。ブランチ一覧の HEAD の行は書き込みの
/// あとに読み直されるが、返るまでのあいだは古い（コミットの直後に押すと 1 つ前のコミット）。
///
/// **書き込みが走っているあいだは断る。** 走っている `git` が HEAD を動かせば、
/// 読んだ本文は 1 つ前のコミットのものになる。**返信の刻印では捕まえられない** ——
/// git2 の読みは commit（pre-commit hook や署名を含む）より先に返るのが普通で、
/// そのときの刻印は「最新」。刻印が拾えるのは「読みが走っているあいだに書き込みが
/// 終わった」形だけで、ここは順序が逆（01 の「commit」）。
fn amend(state: &mut AppState) -> Cmd {
    if exec_busy(state) {
        return Cmd::None;
    }
    Cmd::LoadHeadMessage
}

/// 書き込み（`Cmd::Exec`）が走っているか。走っていれば理由を通知に出す。
///
/// **「押したのに何も起きない」に見せない**（`AppState::notify_dropped`）。
/// **キューには積まないと決めた**（01 の「ランタイム」）。案内を `issue` と同じ
/// 関数から出すのは、次に押すキーが状態で変わるため（中断を頼んだあとの `Ctrl-C` は
/// 終了）。2 本に割ると、こちらだけが古い案内を出す。
///
/// **利用者は `amend` だけ**なので `mod.rs` から移した（アカウントの `r` が
/// 断らなくなった。01 の「外で直したあとの再解決」）。
fn exec_busy(state: &mut AppState) -> bool {
    if !state.jobs.is_running(JobKind::Exec) {
        return false;
    }
    state.notify_dropped(JobKind::Exec);
    true
}

/// amend の初期値が届いた。
pub(super) fn head_message_loaded(
    state: &mut AppState,
    result: Result<Option<String>, GitReadError>,
) -> Cmd {
    let asked = state.jobs.finish(JobKind::LoadHeadMessage);

    // **書き込みより前に読んだ本文で入力欄を埋めない。** commit が走っているあいだに
    // `C` を押すと、amend の対象は新しいコミットなのに本文だけ 1 つ前のものになる。
    // 中身を見る前に頼み直す（`Ok(None)` の「amend できるコミットがありません」も、
    // その時点では正しくない）。**別のモーダルが開いていれば頼み直さない** ——
    // 下の `modal.is_some()` が届いたものを捨てるので、読むだけ無駄になる。
    // 続きを頼む: 書き込みより前に読んだ本文を捨てたぶんの頼み直し。
    if !state.jobs.is_fresh(JobKind::LoadHeadMessage, asked) {
        return if state.modal.is_some() {
            Cmd::None
        } else {
            Cmd::LoadHeadMessage
        };
    }

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

/// ワーキングツリーのカーソルを動かす。区画をまたいで 1 本のリストのように動く
/// （01 の「ワーキングツリー」）。
///
/// **カーソルを動かすキーはすべてここを通す。** 動かすと diff の対象が変わるので、
/// 直前の失敗を落として 1 回だけ試し直せるようにする（`GraphState.load_failed` を
/// `scrolled` で落とすのと同じ形。01 の「diff の遅延取得」）。
pub(super) fn move_entry(state: &mut AppState, delta: isize) -> Cmd {
    state.worktree.move_cursor(delta);
    state.diff.failed = None;
    Cmd::None
}

/// ワーキングツリーで押せる操作。`(操作, 区画)` の表を 1 本にするための語彙。
#[derive(Debug, Clone, Copy)]
pub(super) enum Action {
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
pub(super) const COMMIT_DIFF: &str = "コミットの差分は変更できません";

/// stash の diff で `s` / `u` / `x` を断る文言。
pub(super) const STASH_DIFF: &str = "stash の差分は変更できません";

/// 出している diff が最新でないときに断る文言。
///
/// 「まだ届いていない」「読み込めなかった」「書き込みより前のもの」の 3 つに使う。
/// **「待っています」とだけ書かない。** 読み直しに失敗して二度と来ないこともある。
pub(super) const NOT_LATEST: &str = "最新の差分をまだ読めていません";

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
        /// ハンクを読んだ時点の指紋（`FileDiff::stamp`）。`HunkApply` の `expect` に写す。
        stamp: Option<PathStamp>,
    },
}

/// 選択行に `action` を適用する。
///
/// Diff ペインではカーソルの指すハンクに効く。**ハンクで表せないファイルでは、同じ
/// キーがファイル単位の操作に落ちる**（07）。
pub(super) fn act(state: &mut AppState, action: Action) -> Cmd {
    // **コミットを選んでいるあいだは効かせない。** 対象はワーキングツリーの選択行
    // だが、右ペインに出ているのはコミット詳細なので、通すと**画面に出ていない
    // ファイル**を stage する（07 の「Diff ペイン」）。`focus` の候補で
    // `Pane::Worktree` を外しているのと同じ事故を、`Pane::Diff` の側で防ぐ。
    //
    // **見るのは出どころ（`AppState::diff_origin`）で、`wanted_diff()` が `None` か
    // どうかではない。** あちらの `None` は 2 つのことを指す — 「Diff ペインを見せて
    // いない」と「コミットの一覧がまだ届いていない」。無いことを根拠にすると、**2 つ目で
    // すり抜ける** —— コミットを選んだまま一覧を待っているあいだ、ワーキングツリーの
    // 選択行に `s` が効く。
    //
    // **3 つ目だった「ワーキングツリーの行に diff が無い（conflicted）」は消えた。**
    // その行では `Diff` が巡回の候補から外れ、`Enter` も断られる（01 の
    // 「Pane と Modal」）ので、Diff ペインを見せたまま `None` にはならない。
    // conflicted の断りは Worktree ペインの側で出る
    // （[`a_conflicted_row_is_refused_with_a_notice`]）。
    //
    // **stash の diff も断る**（01 の「Stash ペイン」）。WIP 行を選んだまま Stash
    // ペインから Diff を開けるので、グラフの選択だけを見ると、stash のハンクを見ながら
    // ワーキングツリーの選択行を stage することになる。出どころは Diff ペインを離れれば
    // `Worktree` か `Commit` に戻るので、Worktree ペインの `s` がここで断られることは無い。
    match state.diff_origin() {
        DiffOrigin::Worktree => {}
        DiffOrigin::Commit(_) => {
            state.push_notice(NoticeLevel::Warn, COMMIT_DIFF.to_string(), None);
            return Cmd::None;
        }
        DiffOrigin::Stash(_) => {
            state.push_notice(NoticeLevel::Warn, STASH_DIFF.to_string(), None);
            return Cmd::None;
        }
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
    let planned = if matches!(scope, Scope::Hunk { .. }) && !state.diff_is_current() {
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
                Scope::Hunk { path, hunk, stamp } => GitOp::StageHunk(Box::new(HunkApply {
                    path: path.to_path_buf(),
                    patch: patch_of(path, hunk)?,
                    expect: expect_of(stamp)?,
                })),
            }))
        }
        (Action::Stage, WorktreeSection::Staged) => Err("すでに stage されています".to_string()),

        (Action::Unstage, WorktreeSection::Staged) => Ok(Plan::Exec(match scope {
            Scope::File { paths, .. } => GitOp::Unstage(paths),
            Scope::Hunk { path, hunk, stamp } => GitOp::UnstageHunk(Box::new(HunkApply {
                path: path.to_path_buf(),
                patch: patch_of(path, hunk)?,
                expect: expect_of(stamp)?,
            })),
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

/// ハンクを読んだ時点の指紋。無ければ断る。
///
/// **`None` はワークツリーか index を読む diff では起きない**（`FileDiff::stamp` は
/// パスを絞った `IndexVsWorktree` / `HeadVsIndex` で必ず埋まり、Worktree の diff は
/// 必ずパスを絞る）。守りを外して当てるより断るほうに倒す。
fn expect_of(stamp: Option<PathStamp>) -> Result<PathStamp, String> {
    stamp.ok_or_else(|| NOT_LATEST.to_string())
}

/// discard の `GitOp` を選ぶ。**変種の選び方が単位ごとに違うのでここだけ分かれる。**
///
/// ファイルは 3 変種で、区画と変更種別で決まる（03 の「discard は 3 変種」）。
/// **この判断はここにしか無い。**`to_command` は純粋関数でどちらも見られない。
///
/// **HEAD に無いか（`Added`）はブランチ一覧の HEAD（`BranchState::head`）では判断しない。**
/// あれは外部でコミットされると、probe が拾って読み直しが返るまで古い。古い `Unborn` を信じて
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
        Scope::Hunk { path, hunk, stamp } => Ok(Plan::Confirm {
            title: "ハンクを破棄",
            body: format!(
                "{} の {} をワークツリーから取り消します。元に戻せません。",
                path.display(),
                hunk.header
            ),
            op: GitOp::DiscardHunk(Box::new(HunkApply {
                path: path.to_path_buf(),
                patch: patch_of(path, hunk)?,
                expect: expect_of(stamp)?,
            })),
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
        stamp: file.stamp,
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
pub(super) fn status_loaded(
    state: &mut AppState,
    result: Result<WorktreeSnapshot, GitReadError>,
) -> Cmd {
    let asked = state.jobs.finish(JobKind::LoadStatus);

    // **定期の status の期限を数え直す**（01 の「外部の変更の検知 › 間隔とフォーカス」）。
    // どの経路の status でも、届いた時点のワークツリーは読めている。数え直さないと、
    // 書き込みや外の変更のあとの読み直しの直後に、定期のぶんがもう一度同じものを読む。
    state.watch.restart_status(state.now, &state.config);

    match result {
        Ok(snapshot) => {
            state.worktree.snapshot = snapshot;
            // **いつの一覧かを値に添える。** 以後、空の一覧は「変更が無い」と読んで
            // よくなり（`None` = まだ読めていない）、書き込みより前のものかも同じ
            // 1 本から判る（01 の「実行中ジョブ › 世代」）。
            state.worktree.snapshot_epoch = asked;
            // 選んでいたファイルは別の区画へ移る（stage / unstage）か消えている
            // （discard）。選べる行へ寄せ直す。
            state.worktree.clamp();
        }
        // 読めていた一覧は捨てない。次の読み込みはユーザーの操作から始まるので、
        // 捨てると画面から消えるだけになる（01）。
        Err(err) => state.push_notice(NoticeLevel::Error, err.to_string(), None),
    }

    // 書き込みの直後に頼んだぶんが落とされていたら、ここで頼み直す。**見るのは
    // 届いた返信の刻印**（`snapshot_epoch` ではない）。失敗した返信は一覧を入れ替えて
    // いないので、持っている値で判じると「古い → 頼み直す → 失敗」を回す。
    let again = if state.jobs.is_fresh(JobKind::LoadStatus, asked) {
        Cmd::None
    } else {
        Cmd::LoadStatus
    };

    // **`clamp` のあとに見る**（01 の「diff の遅延取得」）。
    //
    // 続きを頼む: 落とされたぶんの status の追いつきと、区画が変わった diff。
    Cmd::batch([again, load_diff(state)])
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        commit_op, enter_with, exec_done, exec_done_of, fake_output, only_staged, open_confirm,
        press, press_code, state, type_text, worktree_state,
    };
    use super::super::update;
    use super::*;
    use crate::app::{KeyCode, Modal, Modifiers, Msg, TextBuffer};

    use crate::git_read::RowlessHead;

    use crate::test_util::{deliver_status, fake_branch_list_or, fake_entry, fake_snapshot};

    #[test]
    fn status_loaded_replaces_the_snapshot_and_reseats_the_cursor() {
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt", "b.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        state.worktree.cursor = 1;

        // b.txt を stage した後の状態が届く。
        let _ = deliver_status(&mut state, Ok(fake_snapshot(&["b.txt"], &["a.txt"], &[])));

        assert!(!state.jobs.is_running(JobKind::LoadStatus));
        // 選んでいた行は消えているので、選べる行へ寄る。
        let (_, entry) = state.worktree.selected().expect("選択が空になっています");
        assert_eq!(entry.path.display().to_string(), "a.txt");
    }

    #[test]
    fn status_loaded_keeps_the_previous_list_when_it_fails() {
        let mut state = only_staged();
        let cmd = deliver_status(&mut state, Err(GitReadError::ReaderGone));

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

        let (body, on_yes) = open_confirm(&state);
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

        let (body, on_yes) = open_confirm(&state);
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

        let (_, on_yes) = open_confirm(&state);
        assert_eq!(
            on_yes,
            &Cmd::Exec(GitOp::DiscardAdded(vec!["a.txt".into()]))
        );
    }

    #[test]
    fn a_stale_unborn_head_does_not_turn_discard_into_a_delete() {
        // ブランチ一覧の HEAD は外部でコミットされると、読み直しが返るまで古い。
        // 古い `Unborn` を信じて `rm --force` を出すと、HEAD にあるファイルが消える。
        // 判断は `LoadStatus` が毎回運んでくる `FileStatus` で行う。
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        state.branches.loaded(fake_branch_list_or(
            Vec::new(),
            RowlessHead::Unborn {
                name: "main".to_string(),
            },
        ));

        let _ = press(&mut state, 'x');

        let (_, on_yes) = open_confirm(&state);
        assert_eq!(on_yes, &Cmd::Exec(GitOp::Discard(vec!["a.txt".into()])));
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

    #[test]
    fn c_upper_is_refused_while_a_write_is_running() {
        // **押した時点で断らないと、刻印では捕まえられない。** git2 の読みは commit
        // （pre-commit hook や署名を含む）より先に返るのが普通で、そのとき
        // `Region::Refs` はまだ進んでいないので返信は「最新」に見える。開いた入力欄は
        // 1 つ前の本文のままで、確定すると新しいコミットを古い本文で amend する。
        let mut state = only_staged();
        let _ = state.issue(Cmd::Exec(commit_op()));

        let cmd = press(&mut state, 'C');

        assert_eq!(cmd, Cmd::None);
        assert!(state.modal.is_none());
        assert!(
            state.notices.back().unwrap().message.contains("git 実行"),
            "{:?}",
            state.notices.back()
        );
    }

    #[test]
    fn a_head_message_read_before_the_write_does_not_fill_the_amend_box() {
        // commit が走っているあいだに `C` を押すと、amend の対象は新しいコミットなのに
        // 本文だけ 1 つ前のものになる。中身を見る前に頼み直す。
        let mut state = only_staged();
        let _ = state.issue(Cmd::LoadHeadMessage);
        let _ = state.issue(Cmd::Exec(commit_op()));
        let _ = update(&mut state, exec_done_of(commit_op(), Ok(fake_output())));

        let cmd = update(
            &mut state,
            Msg::HeadMessageLoaded(Ok(Some("書き込み前の本文".to_string()))),
        );

        assert_eq!(cmd, Cmd::LoadHeadMessage);
        assert!(state.modal.is_none(), "古い本文で入力欄を開かない");
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
    fn c_is_not_refused_while_the_status_from_before_the_write_is_the_only_one_in_hand() {
        // **書き込みの時点で `LoadStatus` が走っていなくても、持っている一覧は古い。**
        // 「落とされたときだけ立てる」旗にしていたときは、この窓が開いたままで、
        // `s` の直後に `c` を押すと「stage された変更がありません」と誤って断っていた
        // （01 の「実行中ジョブ › 世代」）。
        let mut state = worktree_state(
            fake_snapshot(&[], &["a.txt"], &[]),
            WorktreeSection::Unstaged,
        );
        assert!(state.worktree_is_current());
        let _ = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));
        let _ = update(&mut state, exec_done(Ok(fake_output())));

        let _ = press(&mut state, 'c');

        assert!(state.modal.is_some(), "判定は git に任せる");
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
        // **ブランチ一覧の HEAD の行からは取らない。** 書き込みのあとの読み直しが返るまでは
        // 古い（01 の「commit」）。
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
}
