//! 状態遷移。設計は `docs/design/01-app-core.md`。
//!
//! `update` は同期・純粋（`AppState` の変更を除く）。I/O は呼ばず、必要な副作用は
//! [`Cmd`] として返す。
//!
//! **ここにあるのは入口と、2 つ以上のモジュールが使う道具だけ** —— `focus` /
//! `confirm` / `submit_exec` / `edit_key`（**利用者は
//! `grep -rn "<名前>(" crates/mewgit-core/src/app/update/` で数える。ここに数を
//! 書かない** —— `.claude/rules/rust-coding.md` の「離れた場所に『いま N ある』を書かない」）。
//! ペインごとのキー処理はそれぞれのモジュールにある（01 の「この文書とコードの対応」）。
//!
//! **利用者が 1 つしかない道具はここに置かない** —— `test_support` と同じ数え方で、
//! 1 人のものを寄せると置き場所を探す手間だけが増える。`Modal::Input` を組む `input`
//! が [`branches`] にあるのはこのため（`Modal::Confirm` の `confirm` と対に
//! 見えるが、いまの利用者はブランチの作成とリネームだけ）。

use crate::git_exec::{GitOp, PullMode};

use super::keys::{self, Action, CommonAction};
use super::{
    AppState, Cmd, InputIntent, JobKind, Key, KeyCode, Modal, Motion, Msg, NoticeLevel, Pane,
    PrField, RewordInput, TextBuffer,
};
use account::{account_resolved, accounts_listed, open_account, repo_info_loaded, target_resolved};
use branches::branches_key;
use diff::{diff_key, diff_loaded, load_diff, move_hunk};
use exec::exec_finished;
use graph::{
    commit_detail_key, commit_detail_loaded, commit_files_loaded, graph_key, load_more, log_loaded,
    move_file, scrolled,
};
use modal::{modal_key, open_notice_log};
use progress::open_progress_menu;
pub use progress::{progress_menu, MenuItem, ProgressMenu};
use prs::{
    open_pr_finished, pr_created, pr_detail_loaded, pr_details_finished, pr_list_loaded, prs_key,
    rate_limit_seen,
};
use rebase::commit_message_loaded;
use remote::{fetch, push};
use stash::{stash_key, stashes_loaded};
use watch::{fingerprint_loaded, focus_gained, reload_all};
use worktree::{head_message_loaded, move_entry, status_loaded, worktree_key};

mod account;
mod branches;
mod diff;
mod exec;
mod graph;
mod modal;
mod progress;
mod prs;
mod rebase;
mod remote;
mod stash;
#[cfg(test)]
mod test_support;
mod watch;
mod worktree;

/// 起動直後に実行してほしい副作用。
///
/// `runtime::run` がループに入る前に呼ぶ。`main.rs` から呼ぶとテストの外に出てしまう。
/// アカウント解決は、そのタスクでここに足す。
pub fn init(state: &mut AppState) -> Cmd {
    let cmd = Cmd::batch([
        Cmd::LoadLog {
            count: state.config.graph.page_size,
            // まだ何も読んでいないので、セッションを捨てる必要が無い。
            restart: false,
        },
        Cmd::LoadStatus,
        // **log と status の後ろに置く**（01 の「起動時の読み込み」）。ブランチ一覧の walk を
        // 運ぶので、最初のグラフを待たせない。リーダースレッドに届く順は決まらないので、
        // 寄せるだけ。
        Cmd::LoadRepoInfo,
        Cmd::LoadStashes,
        // 外の変更を比べる基準（01 の「外部の変更の検知」）。次の probe は 2 秒後。
        Cmd::Probe,
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
/// grep -rnE "^ *// 続きを頼む" crates/mewgit-core/src/app/
/// ```
///
/// **ここに一覧も数も写さない** —— 導出できる値を離れた場所に置いたコピーになる
/// （`.claude/rules/rust-coding.md`）。**印は索引であって導出ではない**ので、**忘れた印は見えない**。
/// そこで**別の見方と突き合わせられる形にしてある** —— `update/` の production で
/// [`load_more`] / `load_commit` / [`load_diff`] / `reload_log` / `prs::fan_out` /
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
        // **下ろすのは入力だけ**（01 の「通知」）。`Tick` や非同期の完了で下ろすと、
        // 離れているあいだに出た失敗が誰にも読まれずに消える。
        Msg::Key(key) => {
            state.dismiss_notice();
            update_key(state, key)
        }
        // 端末サイズはレイアウトの分岐にしか使わない。グラフの行数はこのあと
        // ランタイムが `Msg::Layout` で入れる（01 の「ランタイム」）。
        Msg::Resize { term } => {
            state.term = term;
            Cmd::None
        }
        Msg::Layout { graph_rows } => {
            state.graph.set_viewport(graph_rows);
            // 続きを頼む: 窓が広がって残りが 1 画面を切ったぶんのログの追加ページ。
            load_more(state)
        }
        Msg::Paste(text) => {
            state.dismiss_notice();
            paste(state, &text)
        }
        // スピナーの駒を進め、外の変更を見張る期限を見る。**`Cmd` を出すのは期限を
        // 過ぎたときだけ**（`Tick` は 250ms ごとに来るので、毎回出すと 1 秒に 4 回
        // ref を列挙することになる。01 の「外部の変更の検知 › 間隔とフォーカス」）。
        Msg::Tick(now) => {
            state.tick = state.tick.wrapping_add(1);
            state.now = now;
            watch::tick(state)
        }
        Msg::FocusGained => focus_gained(state),

        Msg::LogLoaded { restart, result } => log_loaded(state, restart, result),
        Msg::CommitDetailLoaded { oid, result } => commit_detail_loaded(state, oid, *result),
        Msg::CommitFilesLoaded { oid, result } => commit_files_loaded(state, oid, *result),
        Msg::HeadMessageLoaded(result) => head_message_loaded(state, result),
        Msg::CommitMessageLoaded { oid, result } => commit_message_loaded(state, oid, *result),
        Msg::RepoInfoLoaded(result) => repo_info_loaded(state, *result),

        // 以下は対応するマイルストーンのタスクで実装する。ここで `_` を使わないのは、
        // `Msg` に変種を足したときに実装漏れをコンパイルエラーにするため。
        Msg::StatusLoaded(result) => status_loaded(state, *result),
        Msg::FingerprintLoaded(result) => fingerprint_loaded(state, *result),
        Msg::StashesLoaded(result) => stashes_loaded(state, *result),
        Msg::DiffLoaded { target, result } => diff_loaded(state, target, *result),
        Msg::ExecFinished(done) => exec_finished(state, *done),
        Msg::TargetResolved(resolved) => target_resolved(state, *resolved),
        Msg::AccountResolved(result) => account_resolved(state, result),
        Msg::AccountsListed(result) => accounts_listed(state, result),
        Msg::PrListLoaded(result) => pr_list_loaded(state, result),
        Msg::PrDetailLoaded {
            epoch,
            number,
            sha,
            result,
        } => pr_detail_loaded(state, epoch, number, sha, result),
        Msg::PrDetailsFinished { epoch, aborted } => pr_details_finished(state, epoch, aborted),
        Msg::RateLimitSeen { epoch, seen } => rate_limit_seen(state, epoch, seen),
        Msg::PrCreated(result) => pr_created(state, result),
        Msg::OpenPrFinished(result) => open_pr_finished(state, result),
    }
}

/// キー入力を `modal` → `focus` の順に振り分ける。
///
/// **表（`AppState.keymap`）を引いてから `match` する**（01 の「表が dispatch を兼ねる」）。
/// 引くのはフォーカスのペインの表 1 つで、共通キーもそこに入っている。既定の表では
/// 共通キーとペインのキーが重ならず、上書きが重ねたときは上書きが勝つ（08 の後勝ち）。
fn update_key(state: &mut AppState, key: Key) -> Cmd {
    // Ctrl-C はモーダルより先に見る。raw mode では SIGINT が届かず、モーダル表示中は
    // `q` が入力文字になる（M2 の CommitMessage）ので、常に効く脱出口が要る。
    // **表を引かずに定数と比べる**（`keys::INTERRUPT`）—— `config.keymap` の上書き（08）が
    // 表をどう書き換えても、脱出口は消えない。
    if key == keys::INTERRUPT {
        return interrupt(state);
    }

    // モーダル表示中はすべてモーダルが受ける。`q` もここでは終了にならない
    // （`CommitMessage` では入力文字になる。脱出口は上の `Ctrl-C`）。
    if state.modal.is_some() {
        return modal_key(state, key);
    }

    match state.keymap.pane(state.focus).action(key) {
        None => Cmd::None,
        Some(Action::Down) => move_cursor(state, 1),
        Some(Action::Up) => move_cursor(state, -1),
        Some(Action::Common(action)) => common_key(state, action),
        Some(Action::Graph(action)) => graph_key(state, action),
        Some(Action::Branches(action)) => branches_key(state, action),
        Some(Action::Worktree(action)) => worktree_key(state, action),
        Some(Action::CommitDetail(action)) => commit_detail_key(state, action),
        Some(Action::Diff(action)) => diff_key(state, action),
        Some(Action::Prs(action)) => prs_key(state, action),
        Some(Action::Stash(action)) => stash_key(state, action),
    }
}

/// どのペインでも効くキー（`keys` の `COMMON` と `PINNED`）。
fn common_key(state: &mut AppState, action: CommonAction) -> Cmd {
    match action {
        // 先頭の `keys::INTERRUPT` が先に受けるので、表から来ることは無い。
        CommonAction::Interrupt => interrupt(state),
        CommonAction::Quit => Cmd::Quit,
        // **リモートを明示する。** git に決めさせると、**複数あって `origin` も追跡先も
        // 無いときに何もせず exit 0 で返る**（git 2.55 で実測）。stderr が空なので
        // 不変ルール 4 の経路も効かず、`f` が無反応になる（01 の「既定リモート」）。
        //
        // **`--prune` は付けない。** `f` が黙って remote-tracking ref を消すのは
        // 「fetch は読むだけ」という期待と食い違う（01）。
        CommonAction::Fetch => fetch(state),
        // **`--ff-only`。** MVP にコンフリクト解消ビューが無いので、merge / rebase が
        // 衝突すると mewgit の中では抜けられない。モードを選ばせるのは continue /
        // abort が揃う M5（01）。
        CommonAction::Pull => Cmd::Exec(GitOp::Pull {
            mode: PullMode::FastForwardOnly,
        }),
        CommonAction::Push => push(state),
        // **アカウントも共通キー。** リポジトリ単位の話で、どのペインに居るかと関係が無い
        // （01 の「アカウントの選び直し」）。
        CommonAction::Account => open_account(state),
        // **通知の履歴も共通キー。** 失敗はどのペインに居ても積まれる（01 の「通知」）。
        CommonAction::NoticeLog => open_notice_log(state),
        // **進行中操作のメニューも共通キー。** 押すのはコンフリクトの解消の途中で、どの
        // ペインに居るかは決まっていない（01 の「進行中操作のメニュー（`m`）」）。
        CommonAction::ProgressMenu => open_progress_menu(state),
        // **読み直しも共通キー。** 外の変更の検知が外れたときの逃げ道で、どのペインに
        // 居るかと関係が無い（01 の「外部の変更の検知 › `R`」）。
        CommonAction::Reload => reload_all(state),
        // **キーの一覧も共通キー**（07 の「Help モーダル」）。
        CommonAction::Help => {
            state.modal = Some(Modal::Help {
                pane: state.focus,
                cursor: 0,
            });
            Cmd::None
        }
        // フォーカスの巡回。ペインごとに書くと、あとから足したペインが `Tab` を受けずに
        // 閉じ込められる。`Shift+Tab` は `BackTab`（修飾なし）に正規化されている
        // （01 の「キー入力」）。
        CommonAction::NextPane => focus(state, state.next_focus()),
        CommonAction::PrevPane => focus(state, state.prev_focus()),
        // 番号で直接移る。番号は画面の場所に付ける（07 の「ペインとフォーカス」）。
        // **`5` は右下で、相手はグラフの選択で決まる**（`GraphSelection::right_pane`）。
        // `Diff` には番号を付けない —— 対象が無いと候補から外れるので、断られる場面が増える。
        CommonAction::ToGraph => jump(state, Pane::Graph),
        CommonAction::ToBranches => jump(state, Pane::Branches),
        CommonAction::ToStash => jump(state, Pane::Stash),
        CommonAction::ToPrs => jump(state, Pane::Prs),
        CommonAction::ToRight => jump(state, state.graph.selection().right_pane()),
    }
}

/// `Ctrl-C`。走っている git の中断を頼み、無ければ終了する。
fn interrupt(state: &mut AppState) -> Cmd {
    // **中断を優先するのは `Exec` が走っているときだけ。** 「ジョブがあれば」に
    // すると、スクロール中はほぼ常に `LoadLog` が走っているので終了できない
    // 瞬間ができる。そもそも中断できるのは `Cmd::Exec` だけで、読み取りの
    // future は途中で捨てられない（02 の「スレッド」）。
    //
    // **しかも「まだ頼んでいない」ときだけ。** 走っているかどうかで決めると、
    // SIGTERM で死なない git のあいだ `Ctrl-C` が中断を返し続け、モーダルを
    // 開いていると脱出口が 1 つも無くなる（`q` は入力文字）。`update_key` の先頭が
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
    Cmd::Quit
}

/// `j` / `k`。フォーカス中のペインのカーソルを `delta` 行動かす。
///
/// **移動はペインの外の動作**（01 の「表が dispatch を兼ねる」）。何のカーソルかは
/// ここの網羅 `match` が決めるので、ペインを足すと書き忘れがコンパイルで落ちる。
fn move_cursor(state: &mut AppState, delta: isize) -> Cmd {
    match state.focus {
        Pane::Graph => scrolled(state, delta),
        Pane::Branches => {
            state.branches.move_cursor(delta);
            Cmd::None
        }
        Pane::Stash => {
            state.stashes.move_cursor(delta);
            Cmd::None
        }
        Pane::Prs => {
            state.prs.move_cursor(delta);
            Cmd::None
        }
        Pane::Worktree => move_entry(state, delta),
        Pane::CommitDetail => move_file(state, delta),
        Pane::Diff => move_hunk(state, delta),
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
        // **欄ごとに規則が変わる**（01 の「PR 作成（`N`）」）。body は複数行、
        // title と base は 1 行なので `Modal::Input` と同じ扱い。
        Some(Modal::PrCreate {
            title,
            body,
            base,
            field,
            ..
        }) => match field {
            PrField::Title => title.insert_line(text),
            PrField::Body => body.insert_text(text),
            PrField::Base => base.insert_line(text),
            // 文字を打つ欄ではない。**`Head` に貼ると候補の外の値を作れてしまう**
            // （01 の「PR 作成（`N`）」—— 推定を人の目に通す欄なので打たせない）。
            PrField::Head | PrField::Draft => {}
        },
        // **reword の入力が開いているときだけ受ける**（01 の「interactive rebase の
        // 開始（`i`）」）。一覧のあいだは行き先が無い。
        Some(Modal::RebaseEditor {
            reword: RewordInput::Open(buf),
            ..
        }) => buf.insert_text(text),
        // **`_` にしない。** ここを受け皿にしていたせいで、`Modal::PrCreate` を
        // 足した回に**貼り付けだけが黙って無反応**になっていた（#63 の
        // 「無反応よりまし」で止めない、の手前の形）。網羅にすると、次に編集できる
        // モーダルを足す人が「貼り付けを受けるか」を選ばされる。
        Some(
            Modal::Confirm { .. }
            | Modal::RemotePicker { .. }
            | Modal::AccountPicker { .. }
            | Modal::NoticeLog { .. }
            | Modal::ProgressMenu { .. }
            | Modal::MainlinePicker { .. }
            | Modal::RebaseEditor {
                reword: RewordInput::Closed | RewordInput::Loading(_),
                ..
            }
            | Modal::Help { .. },
        )
        | None => {}
    }
    Cmd::None
}

/// 番号で `pane` へ移る。**候補に入っていなければ通知で断り、フォーカスを動かさない**
/// （07 の「ペインとフォーカス」）。黙って何も起きないと、キーが効いていないのか
/// 対象が悪いのか判らない。
fn jump(state: &mut AppState, pane: Pane) -> Cmd {
    match state.focus_refusal(pane) {
        None => focus(state, pane),
        Some(reason) => {
            state.push_notice(NoticeLevel::Info, reason, None);
            Cmd::None
        }
    }
}

/// フォーカスを移し、見えるようになったものが要るなら頼む。
///
/// **フォーカスを動かすキーはすべてここを通す**（`Tab` / `Shift+Tab` の巡回、`1`〜`5`、
/// Worktree の `Enter`、Diff の `Esc`）。腕ごとに `Cmd::LoadDiff` を書くと、あとから
/// ペインを足すたびに同じ特殊ケースが増える（01 の「diff の遅延取得」）。
///
/// **頼むものが 2 つになっても腕は増やさない。** PR の集計も「見えているペインの
/// ぶんだけ引く」形なので、どちらも述語（[`AppState::wanted_diff`] /
/// [`AppState::wanted_details`]）が中で門にする —— ここに `pane` を見る `if` を
/// 置くと、ペインごとの特殊ケースが結局ここへ戻ってくる。
fn focus(state: &mut AppState, pane: Pane) -> Cmd {
    state.focus = pane;
    // 続きを頼む: 見えるようになったペインの diff と、開いた PRs タブの集計。
    Cmd::batch([load_diff(state), prs::fan_out(state)])
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
    // **開く前に断る。** 確定で断ると、打ち終えた名前を捨てさせる（01 の
    // 「進行中操作があるときの書き込み」）。断ったら `issue` と同じく読み直す
    // （`AppState::refusal`）。
    if let Some(instead) = state.refuse_write(&intent.op(String::new())) {
        return instead;
    }
    state.modal = Some(Modal::Input {
        title,
        prompt,
        buf,
        intent,
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

/// [`multiline_input_key`] が振り分けた結果。
enum InputKey {
    /// `Esc`。何を捨てるか（モーダルを閉じるか、一覧へ戻るか）はモーダルの側。
    Cancel,
    /// 修飾なしの `Enter`。
    Submit,
    /// それ以外。バッファに当てた。
    Edited,
}

/// 複数行の入力欄のキーの振り分け（`CommitMessage` と RebaseEditor の reword）。
/// **修飾なしの `Enter` だけが確定**で、修飾付きは改行に落ちる（01 の「コミット
/// メッセージの編集」）。**振り分けを 1 本にする** —— 入力欄ごとに写すと、`Enter` の
/// 意味が欄で割れる。
fn multiline_input_key(buf: &mut TextBuffer, key: Key) -> InputKey {
    if key == Key::new(KeyCode::Esc) {
        return InputKey::Cancel;
    }
    if key == Key::new(KeyCode::Enter) {
        return InputKey::Submit;
    }
    multiline_edit_key(buf, key);
    InputKey::Edited
}

/// 複数行を編集できる欄のキー。改行と行移動を受け、残りは [`edit_key`] に落とす。
///
/// **2 人目（[`Modal::PrCreate`] の body 欄）が出た時点で `commit_message_key` から
/// 割った。** 写すと、端末ごとの改行キー（`Shift+Enter` は区別して送れる端末だけ、
/// `Ctrl-J` はどの端末でも届く。01 の「コミットメッセージの編集」）を直したときに
/// 片方だけ古くなり、**どちらのテストも通る**（それぞれ自分の経路しか踏まない）。
///
/// **確定は受けない。** 修飾なしの `Enter` をどう扱うかはモーダルごとの判断で、
/// ここに入れると「確定して何をするか」まで共有することになる。1 つの欄だけの入力は
/// [`multiline_input_key`] が `Enter` を `Submit` に振り分けるところまでを持ち、
/// 何をするかは返さない（`PrCreate` は欄で `Enter` の意味が変わるので使わない）。
fn multiline_edit_key(buf: &mut TextBuffer, key: Key) {
    // `Ctrl-J` はどの端末でも届く改行。raw mode では `\n` が `Ctrl+j` として来る。
    if key == Key::ctrl('j') {
        buf.insert_newline();
        return;
    }
    match key.code() {
        // ここに来る `Enter` は修飾付き（`Alt+Enter` = `ESC CR`、`Shift+Enter`）。
        KeyCode::Enter => buf.insert_newline(),
        // 行が複数ある欄にしか要らない。残りは `edit_key` が受ける。
        KeyCode::Up => buf.move_cursor(Motion::Up),
        KeyCode::Down => buf.move_cursor(Motion::Down),
        _ => {
            let _ = edit_key(buf, key);
        }
    }
}

/// 編集中のモーダルを確定して `Cmd::Exec` を返す。
///
/// **`Cmd::Exec` が落とされるなら閉じない。** `modal::confirm_key` は先に閉じてから返すが、
/// あちらのモーダルはユーザーが打ったものを持っていない。ここで同じことをすると、
/// 前の git 実行（遅い pre-commit hook など）が走っているあいだに確定したときに
/// 「ほかの git 実行が終わるまで待ってください」と出て**打った文字ごと消える**。
/// 判定は [`AppState::issue`] と同じ `AppState::refusal`。
///
/// **2 人目（`Modal::Input`）が出た時点でここへ移した。** 写すと、片方だけ
/// 「閉じてから返す」形に戻ったときに気付けない（01 の「1 行入力」）。
fn submit_exec(state: &mut AppState, op: GitOp) -> Cmd {
    submit(state, Cmd::Exec(op))
}

/// 編集中のモーダルを確定して `cmd` を返す。`issue` が落とすなら閉じずに断る
/// （同種が走っている、進行中操作がある）。
///
/// **2 人目（`Modal::PrCreate`）が出た時点で `submit_exec` から割った。**
/// `Cmd::Exec` かどうかではなく、**[`AppState::issue`] に落とされうる `Cmd` を
/// 打った本文と引き換えに出すか**が問い。
///
/// **種別は引数に取らない。** [`Cmd::job`] が網羅 `match` で必ず答えるので、渡させると
/// `submit(state, JobKind::Exec, Cmd::CreatePr(..))` という**食い違った対**が型で
/// 組める —— 組むと**走っていない種別を見てモーダルを閉じ、そのあと `issue` が
/// 本当の種別で落とす**ので、この関数が在る理由（打った本文を消さない）がそのまま
/// 破れる（`.claude/rules/rust-coding.md` の「導出できる値を引数に取らせない」）。
fn submit(state: &mut AppState, cmd: Cmd) -> Cmd {
    if let Some(instead) = state.refusal(&cmd) {
        return instead;
    }

    // 閉じずに返すと、`Cmd` が落とされたときにモーダルだけが画面に残る。
    state.modal = None;
    cmd
}

#[cfg(test)]
mod tests {

    use super::test_support::{
        commit_loads, cursor, detail_ok, diff_state, exec_done_of, github_disabled_state, more,
        on_first_commit, on_first_commit_with_files, press, press_code, state, state_on_branches,
        state_on_stashes, state_with_commits, state_with_page, tab_to, tick, worktree_state, ROWS,
    };
    use super::*;
    use crate::app::{Keymap, Modal, RightTab, TermSize};
    use crate::git_read::WorktreeSection;

    use crate::git_exec::ExecError;

    use crate::git_read::Timestamp;
    use crate::test_util::{
        deliver_status, fake_pull_request, fake_snapshot, fake_target, pr_list_loaded,
        put_on_branch, reply, FAKE_NOW,
    };

    /// 最新の通知の文言。下ろしていれば `None`。
    fn shown(state: &AppState) -> Option<&str> {
        state.shown_notice().map(|notice| notice.message.as_str())
    }

    #[test]
    fn a_key_takes_the_notice_down_but_keeps_the_one_it_raised() {
        // **下ろすのは押したキーが何かする前**（01 の「通知」）。後にすると、`A` が
        // 出した「確かめている最中」の案内がその場で消える。
        let mut state = state();
        state.push_notice(NoticeLevel::Warn, "古い通知".to_string(), None);
        assert_eq!(shown(&state), Some("古い通知"));

        let _ = press(&mut state, 'j');
        assert_eq!(shown(&state), None, "キーで下ろしていない");
        // 履歴には残る。下ろしたのは表示だけ。
        assert_eq!(state.notices.len(), 1);

        // `Resolving` の `A` は通知に落ちる（01 の「アカウント選択」）。
        let _ = press(&mut state, 'A');
        let raised = shown(&state)
            .expect("A が出した通知まで下ろしている")
            .to_string();
        assert_ne!(raised, "古い通知");

        let _ = press(&mut state, 'j');
        assert_eq!(shown(&state), None);
    }

    #[test]
    fn a_paste_takes_the_notice_down_too() {
        let mut state = state();
        state.push_notice(NoticeLevel::Warn, "古い通知".to_string(), None);

        let _ = update(&mut state, Msg::Paste("x".to_string()));

        assert_eq!(shown(&state), None);
    }

    #[test]
    fn only_input_takes_the_notice_down() {
        // 離れているあいだに出た失敗を、`Tick` や非同期の完了で消さない。
        let mut state = state();
        state.push_notice(NoticeLevel::Error, "push に失敗しました".to_string(), None);

        let _ = update(&mut state, tick());
        let _ = update(&mut state, Msg::Layout { graph_rows: ROWS });
        let _ = update(
            &mut state,
            Msg::Resize {
                term: TermSize::default(),
            },
        );
        // **非同期の完了も回す。** 通知を積まない成功の返信で、失敗のあとに必ず届く
        // 形（書き込みのあとの読み直し）。
        let _ = deliver_status(&mut state, Ok(fake_snapshot(&[], &["a.txt"], &[])));
        assert_eq!(state.notices.len(), 1, "返信が通知を積んでいる");

        assert_eq!(shown(&state), Some("push に失敗しました"));
    }

    #[test]
    fn a_notice_carries_the_time_of_the_last_tick() {
        // 起動時は `AppState::new` に渡した時刻、以後は `Tick` が運んだ時刻。
        let mut state = state();
        state.push_notice(NoticeLevel::Info, "前".to_string(), None);
        let later = Timestamp {
            seconds: FAKE_NOW.seconds + 1,
            ..FAKE_NOW
        };
        let _ = update(&mut state, Msg::Tick(later));
        state.push_notice(NoticeLevel::Info, "後".to_string(), None);

        let at: Vec<_> = state.notices.iter().map(|notice| notice.at).collect();
        assert_eq!(at, [FAKE_NOW, later]);
    }

    /// **表（`app::keys`）に載せたキーがそのペインで本当に何かすることを見る。**
    ///
    /// 表は dispatch を兼ねる（01 の「表が dispatch を兼ねる」）ので、「表 → 腕」は網羅
    /// `match` が、「動作 → 表」は `keys` の `every_action_is_bound_in_its_table` が見る。
    /// **それでも残すのは、腕が `Cmd::None` を返すだけの形を拾うため** —— 網羅 `match` が
    /// 強いるのは腕が在ることだけ。
    ///
    /// **`update` を通す。** ペインの `*_key` を直に呼ぶと [`AppState::issue`] を通らず、
    /// 「出なかった」と「落とされた」が混ざる（#38）。
    ///
    /// **1 打鍵ごとに状態を組み直す。** 続けて押すと、先に開いたモーダルが後続のキーを
    /// 全部受ける。
    ///
    /// **「何かする」は `Cmd` か状態の変化。** 断りの通知も状態を動かすので通る ——
    /// ヒントはペイン単位なので、カーソル行によっては理由を出して断るのが正しい
    /// （コミットの diff での `s` など。07）。
    ///
    /// **どこでも効くキーは 1 つの状態でしか回さない。** あれは `common_key` に渡るので、
    /// ペインごとに押しても踏む経路は同じ（`.claude/rules/testing.md` の「通る組み合わせを
    /// 足しても見張りは増えない」）。
    #[test]
    fn every_hinted_key_does_something() {
        let keymap = Keymap::default();
        for &pane in Pane::ALL {
            for hint in keymap.pane(pane).hints() {
                for key in hint.keys() {
                    assert_reacts(pane, key, hint.label());
                }
            }
        }
        // **Diff から押す。** いま居るペインの番号はフォーカスを動かさない（07 の
        // 「ペインとフォーカス」）ので、番号の付いていないペインから押せば特例が要らない。
        for hint in keymap.pane(Pane::Diff).anywhere() {
            for key in hint.keys() {
                assert_reacts(Pane::Diff, key, hint.label());
            }
        }
    }

    #[test]
    fn question_mark_opens_the_key_list_from_any_pane_and_the_cursor_stops_at_its_end() {
        // 共通キー（07 の「Help モーダル」）。ペインの `match` より先に見るので、ペインの
        // キーに食われない。
        for pane in [Pane::Graph, Pane::Worktree] {
            let mut state = state();
            state.focus = pane;
            assert_eq!(update(&mut state, Msg::Key(Key::char('?'))), Cmd::None);
            assert_eq!(
                state.modal,
                Some(Modal::Help { pane, cursor: 0 }),
                "{pane:?}"
            );

            // 窓を動かすためのカーソルは、そのペインの一覧の長さで止まる。
            let rows = state.keymap.pane(pane).help().len();
            for _ in 0..rows + 3 {
                let _ = update(&mut state, Msg::Key(Key::char('j')));
            }
            assert_eq!(
                state.modal,
                Some(Modal::Help {
                    pane,
                    cursor: rows - 1
                }),
                "{pane:?}"
            );

            let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Esc)));
            assert_eq!(state.modal, None);
        }
    }

    /// `pane` で `hint` のキーを 1 つずつ押し、無反応でないことを見る。
    fn assert_reacts(pane: Pane, key: Key, label: &str) {
        let mut state = hinted_state(pane);
        assert_eq!(state.focus, pane, "{pane:?} に居ません");
        // **出ている通知があると、どのキーも「下ろした」ことで反応したように
        // 見える**（01 の「通知」）。この見張りが空になるので、無い状態から押す。
        assert_eq!(state.shown_notice(), None, "{pane:?} に通知が出ています");
        let before = format!("{state:?}");

        let cmd = update(&mut state, Msg::Key(key));

        assert!(
            cmd != Cmd::None || format!("{state:?}") != before,
            "{pane:?} の {key:?}（{label}）が無反応です"
        );
    }

    /// **dispatch も上書きした表を引く**（08）。新しいキーが既定のキーと同じことをし、
    /// 既定のキーは何もしなくなる。
    #[test]
    fn an_overridden_key_does_what_the_default_did_and_the_default_goes_quiet() {
        let overrides = [("stage".to_string(), "S".to_string())].into();
        let fresh = |keymap: Keymap| {
            let mut state = hinted_state(Pane::Worktree);
            state.keymap = keymap;
            state
        };
        let mut by_default = fresh(Keymap::default());
        let expected = update(&mut by_default, Msg::Key(Key::char('s')));
        assert_ne!(expected, Cmd::None, "既定の `s` が何もしていません");

        let mut overridden = fresh(Keymap::new(&overrides).0);
        assert_eq!(update(&mut overridden, Msg::Key(Key::char('S'))), expected);

        let mut overridden = fresh(Keymap::new(&overrides).0);
        let before = format!("{overridden:?}");
        assert_eq!(update(&mut overridden, Msg::Key(Key::char('s'))), Cmd::None);
        assert_eq!(
            format!("{overridden:?}"),
            before,
            "既定の `s` が効いています"
        );
    }

    /// `pane` にフォーカスがあり、**表のどのキーも空振りしない**状態。
    ///
    /// **カーソルは一覧の真ん中に置く。** 端に置くと `j` か `k` の片方が正しく
    /// 無反応になり、表が嘘でないのに落ちる。**置き方は `j` を 1 回押すので揃える**
    /// —— フィールドに直に書くと、カーソルを動かす規則を通らない状態が作れる。
    fn hinted_state(pane: Pane) -> AppState {
        // 2 件以上ないと `j` と `k` の両方を踏めない。
        let files = &["a.txt", "b.txt", "c.txt"];
        match pane {
            Pane::Graph => {
                let mut state = on_first_commit(3);
                // 印が無いと `Esc`（印を全部外す）が正しく無反応になる。
                let oid = state.graph.lines[1].commit.oid;
                state.graph.toggle_mark(oid);
                state
            }
            Pane::Branches => {
                let mut state = state_on_branches(&["main", "topic", "x"], Some("main"));
                let _ = press(&mut state, 'j');
                state
            }
            Pane::Prs => {
                let mut state = state_with_commits(3);
                // 一覧を引く前に、アカウントと対象を決めておく（`prs::reload` が見る）。
                put_on_branch(&mut state, "topic");
                let list = (1..=3)
                    .map(|n| fake_pull_request(n, "topic", "sha"))
                    .collect();
                let ask = prs::reload(&state);
                let _ = reply(&mut state, ask, pr_list_loaded(list));
                let _ = tab_to(&mut state, Pane::Prs);
                let _ = press(&mut state, 'j');
                state
            }
            Pane::Worktree => {
                // staged が 1 つある（`c` が開ける）状態で、カーソルは unstaged に置く。
                let mut state = worktree_state(
                    fake_snapshot(&["a.txt"], &["b.txt", "c.txt", "d.txt"], &[]),
                    WorktreeSection::Unstaged,
                );
                let _ = press(&mut state, 'j');
                state
            }
            Pane::CommitDetail => {
                let mut state = on_first_commit_with_files(3, files);
                // `Enter` で右へ（コミットを選んでいるので CommitDetail）。
                let _ = press_code(&mut state, KeyCode::Enter);
                let _ = press(&mut state, 'j');
                state
            }
            Pane::Diff => {
                let mut state = diff_state(&["a.txt"], 3);
                let _ = press(&mut state, 'j');
                state
            }
            Pane::Stash => {
                let mut state = state_on_stashes(3);
                let _ = press(&mut state, 'j');
                state
            }
        }
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

        // 巡回の並びは `Pane` の宣言順なので、Graph の次は Branches、Stash、Prs
        // （01 の「右上のタブ」）。
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Tab)));
        assert_eq!(state.focus, Pane::Branches);
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Tab)));
        assert_eq!(state.focus, Pane::Stash);
        assert_eq!(state.right_tab, RightTab::Stash);
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Tab)));
        assert_eq!(state.focus, Pane::Prs);
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Tab)));
        assert_eq!(state.focus, Pane::Worktree);

        // **タブは離れても残る。** `focus` が右列の上を離れても `right_tab` は
        // 最後に見ていたほうのまま（01 の「右上のタブ」）。
        assert_eq!(state.right_tab, RightTab::Prs);

        // 同じ `j` が worktree のカーソルに効く。グラフは動かない。
        let _ = press(&mut state, 'j');
        assert_eq!(state.graph.cursor, 0);
        assert_eq!(state.worktree.cursor, 1);

        for _ in 0..4 {
            let _ = update(&mut state, Msg::Key(Key::new(KeyCode::BackTab)));
        }
        assert_eq!(state.focus, Pane::Graph);
        // 逆回りで Branches を通ったので、タブはそちらに戻っている。
        assert_eq!(state.right_tab, RightTab::Branches);
    }

    #[test]
    fn digits_jump_to_the_place_and_5_follows_the_graph_selection() {
        // 番号は画面の場所に付ける（07 の「ペインとフォーカス」）。
        let mut state = state_with_commits(3);
        state.worktree.snapshot = fake_snapshot(&[], &["a.txt"], &[]);

        for (c, pane, tab) in [
            ('2', Pane::Branches, RightTab::Branches),
            ('3', Pane::Stash, RightTab::Stash),
            ('4', Pane::Prs, RightTab::Prs),
        ] {
            let _ = press(&mut state, c);
            assert_eq!(state.focus, pane, "{c}");
            // タブも揃う（`Tab` で入ったときと同じ出口を通る）。
            assert_eq!(state.right_tab, tab, "{c}");
        }

        // WIP 行を選んでいるあいだ、右下は Working tree。
        let _ = press(&mut state, '5');
        assert_eq!(state.focus, Pane::Worktree);

        // Diff からも `1` で Graph に戻れる。
        let _ = press_code(&mut state, KeyCode::Enter);
        assert_eq!(state.focus, Pane::Diff);
        let _ = press(&mut state, '1');
        assert_eq!(state.focus, Pane::Graph);

        // コミットを選ぶと、右下はコミット詳細になる。
        let _ = press(&mut state, 'j');
        let _ = press(&mut state, '5');
        assert_eq!(state.focus, Pane::CommitDetail);
    }

    #[test]
    fn a_digit_for_a_pane_out_of_the_candidates_is_refused_with_a_notice() {
        // `config.github.enabled = false` だと `Prs` が候補から落ちる（01 の「GitHub 機能を
        // 切る」）。黙って何も起きないと、キーが効いていないのか判らない。
        let mut state = github_disabled_state();
        state.focus = Pane::Branches;

        let cmd = press(&mut state, '4');

        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.focus, Pane::Branches, "フォーカスが動きました");
        assert_eq!(
            shown(&state),
            Some(crate::app::GithubBlock::AccountDisabled.notice().as_str())
        );
    }

    #[test]
    fn a_digit_under_a_modal_goes_to_the_modal() {
        // モーダル表示中はキーを全部モーダルが受ける。`CommitMessage` では入力文字。
        let mut state = state_with_commits(1);
        state.worktree.snapshot = fake_snapshot(&["a.txt"], &[], &[]);
        state.focus = Pane::Worktree;
        let _ = press(&mut state, 'c');
        assert!(matches!(state.modal, Some(Modal::CommitMessage { .. })));

        let _ = press(&mut state, '2');

        assert_eq!(state.focus, Pane::Worktree);
        let Some(Modal::CommitMessage { buf, .. }) = &state.modal else {
            panic!("モーダルが閉じました");
        };
        assert_eq!(buf.text(), "2");
    }

    #[test]
    fn init_asks_for_the_repository_the_first_page_and_the_status() {
        let mut state = state();

        let cmd = init(&mut state);

        assert_eq!(
            cmd,
            Cmd::Batch(vec![
                more(&state),
                Cmd::LoadStatus,
                // **log と status の後ろ。** ブランチ一覧の walk を運ぶので、最初の
                // グラフを待たせない（02 の「ブランチ一覧」）。
                Cmd::LoadRepoInfo,
                Cmd::LoadStashes,
                // 外の変更を比べる基準（01 の「外部の変更の検知」）。
                Cmd::Probe,
            ])
        );
        assert!(state.jobs.is_running(JobKind::LoadLog));
        assert!(state.jobs.is_running(JobKind::LoadRepoInfo));
        assert!(state.jobs.is_running(JobKind::LoadStatus));
        assert!(state.jobs.is_running(JobKind::LoadStashes));
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
    fn q_quits_but_ctrl_q_does_not() {
        let mut state = state();
        // 終了は修飾なしの `q` だけ。Ctrl-q で落ちると事故になる。
        assert_eq!(update(&mut state, Msg::Key(Key::ctrl('q'))), Cmd::None);
        assert_eq!(update(&mut state, Msg::Key(Key::char('q'))), Cmd::Quit);
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
    fn the_spinner_advances_on_every_tick() {
        // 駒を選ぶのは `ui` だが、進めるのは `update`（`ui` は `AppState` を描くだけ）。
        let mut state = state();
        assert_eq!(state.tick, 0);

        assert_eq!(update(&mut state, tick()), Cmd::None);

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
    fn ctrl_c_quits_even_with_a_modal_open() {
        // raw mode では SIGINT が届かず、モーダル中は `q` が入力文字になる。
        // 常に効く脱出口でないと閉じ込められる。
        let mut state = AppState {
            modal: Some(Modal::Help {
                pane: Pane::Graph,
                cursor: 0,
            }),
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
        assert_eq!(state.graph.viewport(), ROWS);
    }

    #[test]
    fn layout_sets_the_viewport_and_asks_for_a_page_when_there_is_room() {
        let mut state = state();
        state.graph.set_viewport(0);

        // 1 行も積んでいないので、窓を知った時点で次のページを頼む。
        assert_eq!(
            update(&mut state, Msg::Layout { graph_rows: 36 }),
            more(&state)
        );
        assert_eq!(state.graph.viewport(), 36);
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
