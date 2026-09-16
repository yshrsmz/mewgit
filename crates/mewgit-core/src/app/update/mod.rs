//! 状態遷移。設計は `docs/design/01-app-core.md`。
//!
//! `update` は同期・純粋（`AppState` の変更を除く）。I/O は呼ばず、必要な副作用は
//! [`Cmd`] として返す。
//!
//! **ここにあるのは入口と、2 つ以上のモジュールが使う道具だけ** —— `focus`
//! （4 モジュール）、`confirm` と `submit_exec`（3）、`edit_key` と `exec_busy`（2）。
//! ペインごとのキー処理はそれぞれのモジュールにある（01 の「この文書とコードの対応」）。
//!
//! **利用者が 1 つしかない道具はここに置かない** —— `test_support` と同じ数え方で、
//! 1 人のものを寄せると置き場所を探す手間だけが増える。`Modal::Input` を組む `input`
//! が [`branches`] にあるのはこのため（`Modal::Confirm` の `confirm` と対に
//! 見えるが、いまの利用者はブランチの作成とリネームだけ）。

use crate::git_exec::{GitOp, PullMode};

use super::{
    AppState, Cmd, JobKind, Key, KeyCode, Modal, Motion, Msg, NoticeLevel, Pane, TextBuffer,
};
use account::{account_resolved, accounts_listed, open_account, repo_info_loaded, target_resolved};
use branches::{branches_key, branches_loaded};
use diff::{diff_key, diff_loaded, load_diff};
use exec::exec_finished;
use graph::{
    commit_detail_key, commit_detail_loaded, commit_files_loaded, graph_key, load_more, log_loaded,
};
use modal::modal_key;
use prs::{checks_loaded, open_pr_finished, pr_loaded, prs_key};
use remote::{fetch, push};
use worktree::{head_message_loaded, status_loaded, worktree_key};

mod account;
mod branches;
mod diff;
mod exec;
mod graph;
mod modal;
mod prs;
mod remote;
#[cfg(test)]
mod test_support;
mod worktree;

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

/// **続きを頼む判断は、それぞれの経路に置いてある。** 場所には `// 続きを頼む:` の
/// 印があり、**一覧はそこから出る**。
///
/// ```text
/// grep -rnE "^ *// 続きを頼む" crates/mewgit-core/src/app/update/
/// ```
///
/// **ここに一覧も数も写さない** —— 導出できる値を離れた場所に置いたコピーになる
/// （`CLAUDE.md`）。**印は索引であって導出ではない**ので、**忘れた印は見えない**。
/// そこで**別の見方と突き合わせられる形にしてある** —— `update/` の production で
/// [`load_more`] / `load_commit` / [`load_diff`] / `reload_log` /
/// `JobTracker::is_fresh` を呼ぶ場所は、**次の 1 つを除いて全部どれかの印の下にある**。
///
/// - **除くのは [`diff::diff_loaded`] の `is_fresh` だけ。** あれは「届いた
///   返信を採るか」の判定で、続きを頼む判断ではない（採らなかったときに頼み直すのは
///   出口の `load_diff` のほうで、そちらには印がある）。**印を持たないのが正しい。**
///
/// **1 対 1 ではない。** 1 つの印が複数の呼び出しを覆うことがあり（同じ判断なので）、
/// 1 つの関数が印を 2 つ持つこともある（`log_loaded` は前半の追いつきと出口で別）。
/// **数えるのは「印の無い呼び出しがあるか」だけ**で、あれば印の漏れか、上の除外と
/// 同じ種類のものかを決める。判断を足したら印も置く。
///
/// **`update` の出口で導出する形には畳まない**（M3 の世代のタスクで決めた。01 の
/// 「実行中ジョブ › 世代」）。追いつきの判断材料は**届いた返信の刻印**で、それが読める
/// のは返信を受ける関数の中だけ。持っている値の刻印から導くと、読み取りが失敗し続けた
/// ときに「古い → 頼み直す → 失敗」を回す（失敗は値を入れ替えないので刻印が古いまま
/// になる）。**世代を入れたことで、導出の形は採れなくなった。**
///
/// 印を読むときに要る、`grep` では出ない約束。
///
/// - **詳細とファイル一覧は別々に配らない。** どちらを頼む腕も `load_commit` を
///   通す（01 の「コミット詳細の遅延取得」「コミットのファイル一覧」）。
/// - **落とされたぶんの追いつきは、届いた返信の刻印がいまの世代と違えば同じ `Cmd` を
///   出し直す**（判定は `JobTracker::is_fresh` の 1 つ。01 の「実行中ジョブ › 世代」）。
///   **[`head_message_loaded`] だけは中身を見る前に返す** —— 古い本文で
///   入力欄を開いてしまうと、あとから届く返信では取り消せない。
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
            // 続きを頼む: 窓が広がって残りが 1 画面を切ったぶんのログの追加ページ。
            load_more(state)
        }
        Msg::Paste(text) => paste(state, &text),
        // スピナーの駒を進めるだけ。読み直しは引かない（`Tick` は 250ms ごとに
        // 来るので、ここから `Cmd` を出すと git を 1 秒に 4 回叩くことになる）。
        Msg::Tick => {
            state.tick = state.tick.wrapping_add(1);
            Cmd::None
        }

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
        Msg::TargetResolved(resolved) => target_resolved(state, *resolved),
        Msg::AccountResolved(result) => account_resolved(state, result),
        Msg::AccountsListed(result) => accounts_listed(state, result),
        Msg::PrLoaded { branch, result } => pr_loaded(state, branch, *result),
        Msg::ChecksLoaded { sha, result } => checks_loaded(state, sha, result),
        Msg::OpenPrFinished(result) => open_pr_finished(state, result),
        Msg::PrListLoaded(_) => Cmd::None,
    }
}

/// キー入力を `modal` → `focus` の順に振り分ける。
fn update_key(state: &mut AppState, key: Key) -> Cmd {
    // Ctrl-C はモーダルより先に見る。raw mode では SIGINT が届かず、モーダル表示中は
    // `q` が入力文字になる（M2 の CommitMessage）ので、常に効く脱出口が要る。
    if key == Key::ctrl('c') {
        // **中断を優先するのは `Exec` が走っているときだけ。** 「ジョブがあれば」に
        // すると、スクロール中はほぼ常に `LoadLog` が走っているので終了できない
        // 瞬間ができる。そもそも中断できるのは `Cmd::Exec` だけで、読み取りの
        // future は途中で捨てられない（02 の「スレッド」）。
        //
        // **しかも「まだ頼んでいない」ときだけ。** 走っているかどうかで決めると、
        // SIGTERM で死なない git のあいだ `Ctrl-C` が中断を返し続け、モーダルを
        // 開いていると脱出口が 1 つも無くなる（`q` は入力文字）。この関数の先頭が
        // 「常に効く脱出口が要る」と言っているのは、まさにその形を作らないため。
        if state.jobs.request_cancel(JobKind::Exec) {
            // **2 回目が何をするかを書く。** 頼んだ直後は画面が何も変わらないので、
            // 出さないと「効いていない」と読める（01 の「失敗の案内文」と同じ方針）。
            state.push_notice(
                NoticeLevel::Info,
                "中断を頼みました。終わらなければもう一度 Ctrl-C で終了します".to_string(),
                None,
            );
            return Cmd::CancelExec;
        }
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

    // **リモート操作は共通キー**（07 のキー表）。ペインの `match` より先に見る。
    // 後ろに置くと、あとから足したペインで押せなくなる（`Tab` と同じ理由）。
    //
    // **`Key` 全体で比べる**（`q` と同じ形）。`key.code()` で見ると `Ctrl-f` や
    // `Alt-p` まで拾い、しかもこの段は全ペインを覆うので、07 が移動に予約している
    // `Ctrl-d` / `Ctrl-u` の隣で修飾付きのキーを先に食う。
    //
    // **リモートを明示する。** git に決めさせると、**複数あって `origin` も追跡先も
    // 無いときに何もせず exit 0 で返る**（git 2.55 で実測）。stderr が空なので
    // 不変ルール 4 の経路も効かず、`f` が無反応になる（01 の「既定リモート」）。
    //
    // **`--prune` は付けない。** `f` が黙って remote-tracking ref を消すのは
    // 「fetch は読むだけ」という期待と食い違う（01）。
    if key == Key::char('f') {
        return fetch(state);
    }
    // **`--ff-only`。** MVP にコンフリクト解消ビューが無いので、merge / rebase が
    // 衝突すると mewgit の中では抜けられない。モードを選ばせるのは continue /
    // abort が揃う M5（01）。
    if key == Key::char('p') {
        return Cmd::Exec(GitOp::Pull {
            mode: PullMode::FastForwardOnly,
        });
    }
    if key == Key::char('P') {
        return push(state);
    }
    // **アカウントも共通キー。** リポジトリ単位の話で、どのペインに居るかと関係が無い
    // （01 の「アカウントの選び直し」）。
    if key == Key::char('A') {
        return open_account(state);
    }

    // フォーカスの巡回もペインより先に見る。ペインごとに書くと、あとから足した
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
        Pane::Prs => prs_key(state, key),
        // Stash ペインのキー処理は M5 で足す。巡回の候補
        // （`AppState::focus_candidates`）に入っていないので、いまここには来ない。
        Pane::Stash => Cmd::None,
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
    // 続きを頼む: 見えるようになったペインの diff。
    load_diff(state)
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

/// 書き込みが走っているか。走っていれば理由を通知に出す。
///
/// **「押したのに何も起きない」に見せない**（`AppState::notify_dropped`）。
/// **キューには積まないと決めた**（01 の「ランタイム」）。案内を `issue` と同じ
/// 関数から出すのは、次に押すキーが状態で変わるため（中断を頼んだあとの `Ctrl-C` は
/// 終了）。2 本に割ると、こちらだけが古い案内を出す。
fn exec_busy(state: &mut AppState) -> bool {
    if !state.jobs.is_running(JobKind::Exec) {
        return false;
    }
    state.notify_dropped(JobKind::Exec);
    true
}

/// 編集中のモーダルを確定して `Cmd::Exec` を返す。
///
/// **`Cmd::Exec` が落とされるなら閉じない。** `modal::confirm_key` は先に閉じてから返すが、
/// あちらのモーダルはユーザーが打ったものを持っていない。ここで同じことをすると、
/// 前の git 実行（遅い pre-commit hook など）が走っているあいだに確定したときに
/// 「ほかの git 実行が終わるまで待ってください」と出て**打った文字ごと消える**。
/// 判定は [`AppState::issue`] と同じ `jobs.is_running`。
///
/// **2 人目（`Modal::Input`）が出た時点でここへ移した。** 写すと、片方だけ
/// 「閉じてから返す」形に戻ったときに気付けない（01 の「1 行入力」）。
fn submit_exec(state: &mut AppState, op: GitOp) -> Cmd {
    if exec_busy(state) {
        return Cmd::None;
    }

    // 閉じずに返すと、`Cmd` が落とされたときにモーダルだけが画面に残る。
    state.modal = None;
    Cmd::Exec(op)
}

#[cfg(test)]
mod tests {

    use super::test_support::{
        commit_loads, cursor, detail_ok, exec_done_of, more, press, state, state_with_commits,
        state_with_page, ROWS,
    };
    use super::*;
    use crate::app::{Modal, TermSize};

    use crate::git_exec::ExecError;

    use crate::test_util::{fake_snapshot, fake_target};

    #[test]
    fn tab_moves_the_focus_and_the_worktree_keys_follow_it() {
        // グラフに居るあいだは `j` がコミットのカーソルを動かす。
        let mut state = state_with_commits(3);
        state.worktree.snapshot = fake_snapshot(&[], &["a.txt", "b.txt"], &[]);
        let _ = press(&mut state, 'j');
        assert_eq!(state.graph.cursor, 1);
        // WIP 行に戻す。コミットを選んでいるあいだ Worktree は巡回に入らない（01）。
        let _ = press(&mut state, 'k');

        // 巡回の並びは `Pane` の宣言順なので、Graph の次は Branches、その次が
        // Prs（01 の「右上のタブ」）。
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Tab)));
        assert_eq!(state.focus, Pane::Branches);
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Tab)));
        assert_eq!(state.focus, Pane::Prs);
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Tab)));
        assert_eq!(state.focus, Pane::Worktree);

        // **タブは離れても残る。** `focus` が右列の上を離れても `right_tab` は
        // 最後に見ていたほうのまま（01 の「右上のタブ」）。
        assert_eq!(state.right_tab, Pane::Prs);

        // 同じ `j` が worktree のカーソルに効く。グラフは動かない。
        let _ = press(&mut state, 'j');
        assert_eq!(state.graph.cursor, 0);
        assert_eq!(state.worktree.cursor, 1);

        for _ in 0..3 {
            let _ = update(&mut state, Msg::Key(Key::new(KeyCode::BackTab)));
        }
        assert_eq!(state.focus, Pane::Graph);
        // 逆回りで Branches を通ったので、タブはそちらに戻っている。
        assert_eq!(state.right_tab, Pane::Branches);
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

        let account = Cmd::ResolveAccount(fake_target());
        let cmd = state.issue(Cmd::Batch(vec![
            Cmd::LoadLog {
                count: 10,
                restart: false,
            },
            account.clone(),
        ]));

        // `init` が走らせていないので、`ResolveAccount` は残る。
        assert_eq!(cmd, account);
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
    fn ctrl_c_quits_when_the_cancel_did_not_take() {
        // **git が SIGTERM で死なないときの脱出口。** 走っているかどうかだけで
        // 決めると、`Ctrl-C` が中断を返し続けて終われなくなる（モーダルを開いて
        // いれば `q` は入力文字なので、ほかに出口が無い）。
        let mut state = state();
        let _ = state.issue(Cmd::Exec(GitOp::Fetch {
            remote: None,
            prune: false,
        }));

        assert_eq!(
            update(&mut state, Msg::Key(Key::ctrl('c'))),
            Cmd::CancelExec
        );
        // 2 回目。`Exec` はまだ走っている（`Msg::ExecFinished` が来ていない）。
        assert!(state.jobs.is_running(JobKind::Exec));
        assert_eq!(update(&mut state, Msg::Key(Key::ctrl('c'))), Cmd::Quit);
    }

    #[test]
    fn a_canceled_write_is_folded_and_the_next_one_can_be_canceled_too() {
        // **中断を頼んだ `Exec` は `Msg::ExecFinished` で畳まれる。** 畳まれたあとの
        // `Ctrl-C` は終了に落ち（01 の「実行中ジョブのキャンセル」）、新しい書き込みを
        // 出せばまた中断できる —— 刻みはジョブと一緒に消えるので、1 度中断したあとの
        // 書き込みが二度と中断できなくなることはない。
        let mut state = state();
        let fetch = GitOp::Fetch {
            remote: None,
            prune: false,
        };
        let _ = state.issue(Cmd::Exec(fetch.clone()));

        // 走っているあいだは中断が先。
        assert_eq!(
            update(&mut state, Msg::Key(Key::ctrl('c'))),
            Cmd::CancelExec
        );

        let _ = update(
            &mut state,
            exec_done_of(
                fetch.clone(),
                Err(ExecError::Canceled {
                    argv: vec!["fetch".into()],
                }),
            ),
        );

        // 畳まれたので終了できる。**中断が終了を塞がない**ことの見張り。
        assert_eq!(update(&mut state, Msg::Key(Key::ctrl('c'))), Cmd::Quit);

        // 新しい書き込みはまた中断できる（刻みが落ちている）。
        let _ = state.issue(Cmd::Exec(fetch));
        assert_eq!(
            update(&mut state, Msg::Key(Key::ctrl('c'))),
            Cmd::CancelExec
        );
    }

    #[test]
    fn ctrl_c_quits_while_a_read_is_running() {
        // **読み取りでは中断しない。** スクロール中はほぼ常に `LoadLog` が走って
        // いるので、「ジョブがあれば中断」にすると終了できない瞬間ができる。
        let mut state = state();
        let _ = state.issue(Cmd::LoadLog {
            count: 50,
            restart: false,
        });

        assert_eq!(update(&mut state, Msg::Key(Key::ctrl('c'))), Cmd::Quit);
    }

    #[test]
    fn waiting_for_a_canceled_git_does_not_advertise_cancel_again() {
        // **中断を頼んだあとの `Ctrl-C` は終了。** そのまま「Ctrl-C で中断」と案内すると、
        // モーダルに打った本文ごとアプリが終わる。
        let mut state = state();
        let _ = state.issue(Cmd::Exec(GitOp::Fetch {
            remote: None,
            prune: false,
        }));

        let _ = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));
        let before = state
            .notices
            .back()
            .expect("通知がありません")
            .message
            .clone();
        assert!(before.contains("Ctrl-C で中断"), "{before}");

        let _ = update(&mut state, Msg::Key(Key::ctrl('c')));
        let _ = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));

        let after = &state.notices.back().expect("通知がありません").message;
        assert!(after.contains("Ctrl-C で終了"), "{after}");
    }

    #[test]
    fn a_canceled_push_does_not_arm_force() {
        // force に上げてよいのは git が非 FF で断ったときだけ（01 の「force push」）。
        // 中断は git の判断ではない。
        let mut state = state();
        let op = GitOp::Push {
            remote: "origin".into(),
            src: "main".into(),
            dst: None,
            set_upstream: false,
            lease: None,
        };
        let _ = state.issue(Cmd::Exec(op.clone()));

        let _ = update(
            &mut state,
            exec_done_of(
                op,
                Err(ExecError::Canceled {
                    argv: vec!["push".into(), "origin".into(), "main".into()],
                }),
            ),
        );

        assert_eq!(state.push_rejected, None);
    }

    #[test]
    fn the_spinner_advances_on_every_tick() {
        // 駒を選ぶのは `ui` だが、進めるのは `update`（`ui` は `AppState` を描くだけ）。
        let mut state = state();
        assert_eq!(state.tick, 0);

        assert_eq!(update(&mut state, Msg::Tick), Cmd::None);

        assert_eq!(state.tick, 1);
    }

    #[test]
    fn a_running_exec_is_labeled_with_the_git_subcommand() {
        // スピナーに `git` と出しても、何が走っているか判らない（01 / 07）。
        let mut state = state();
        let _ = state.issue(Cmd::Exec(GitOp::Fetch {
            remote: None,
            prune: false,
        }));

        let labels: Vec<&str> = state.jobs.iter().map(|job| job.label).collect();
        assert_eq!(labels, ["fetch"]);
    }

    #[test]
    fn a_dropped_exec_says_how_to_stop_waiting() {
        // 落としたことだけでなく、待たずに済む手（`Ctrl-C`）まで書く。
        // 2 本目は落とす（キューには積まない。01 の「ランタイム」）。落としたことを
        // 伝えるだけでなく、待たずに済む手（`Ctrl-C`）まで書く。
        let mut state = state();
        let _ = state.issue(Cmd::Exec(GitOp::Fetch {
            remote: None,
            prune: false,
        }));

        let cmd = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));

        assert_eq!(cmd, Cmd::None);
        let notice = state.notices.back().expect("通知がありません");
        assert!(notice.message.contains("Ctrl-C"), "{}", notice.message);
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
}
