//! PRs ペインのキー処理と、PR 一覧の受け取り（01 の「PR ペイン」）。
//!
//! 引くかどうかを決めるのは [`AppState::wanted_pr_list`] で、**頼む側と、文言を出すか
//! 一覧を描くかを決める `ui` の両方がそれを読む**。

use crate::auth::{url, AuthError, RepoTarget};
use crate::git_exec::{ForkUpstream, GitOp, LocalBranch, PrCheckout, PrSource};
use crate::git_read::{Branch, Remote};
use crate::github::{GithubError, PrDetail, PullRequest, RateLimit, RepoRef};

use super::remote::{conventional_remote, repo_unread, NoRemote};
use super::{edit_key, focus, multiline_edit_key, submit};
use crate::app::state::current_row;
use crate::app::{
    AppState, BranchState, Cmd, JobKind, Key, KeyCode, Modal, NoticeLevel, Pane, PrField, PrHeads,
    TextBuffer,
};
use crate::github::CreatePr;

/// PRs ペインのキー処理。
pub(super) fn prs_key(state: &mut AppState, key: Key) -> Cmd {
    match key.code() {
        KeyCode::Char('j') | KeyCode::Down => {
            state.prs.move_cursor(1);
            Cmd::None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            state.prs.move_cursor(-1);
            Cmd::None
        }
        KeyCode::Enter => checkout(state),
        KeyCode::Char('o') => open_pr(state),
        KeyCode::Char('r') => refresh(state),
        KeyCode::Char('N') => open_pr_create(state),
        // **`Tab` でしか入れないペインにも出口を用意する**（07）。Branches ペインと
        // 同じく、左の選択と無関係なので戻り先は `Graph`。
        KeyCode::Esc => focus(state, Pane::Graph),
        _ => Cmd::None,
    }
}

/// `o`。カーソル行の PR をブラウザで開く（01 の「`o`（ブラウザで開く）」）。
///
/// **一覧が空のときは `N`（PR 作成）を案内する。** 開ける PR が無いのは PR を作る
/// 場面なので、「ありません」で止めると行き止まりになる（#63 の「案内が求める操作を、
/// そもそも自分が持っているか」）。**`N` がこのペインで効くことは
/// [`prs_key`] が答えている** —— 案内を出しているのと同じペインのキーなので、
/// 画面ごとに数え直す必要が無い（`.claude/rules/rust-coding.md`）。
fn open_pr(state: &mut AppState) -> Cmd {
    match state.prs.selected() {
        Some(pr) => Cmd::OpenPr { number: pr.number },
        None => {
            state.push_notice(
                NoticeLevel::Info,
                "開ける PR がありません（N で作成）".to_string(),
                None,
            );
            Cmd::None
        }
    }
}

/// `Enter`。カーソル行の PR をローカルのブランチに取り出す（01 の
/// 「PR チェックアウト（`Enter`）」、手順は 05）。
///
/// **押した時点で断るのは 3 つ。** 一覧が空、fetch するリモートが決まらない、
/// ブランチ一覧がまだ届いていない（「在るか」を決められない）。
/// **GitHub の状態は見ない** —— 叩くのは git だけで、材料は一覧の行が持っている。
///
/// **確認モーダルは出さない。** どの手順も上書きしない（05）ので、不変ルール 5 の
/// 破壊的操作に当たらない。
fn checkout(state: &mut AppState) -> Cmd {
    let Some(pr) = state.prs.selected() else {
        state.push_notice(
            NoticeLevel::Info,
            "取り出せる PR がありません（N で作成）".to_string(),
            None,
        );
        return Cmd::None;
    };
    let refusal = match (conventional_remote(state), state.repo.target.resolved()) {
        // **`target` を決めたのと同じ規則でリモートを選ぶ**（PR 作成の base と同じ）。
        // 一覧はそのリポジトリの PR なので、そこを指しているリモートから取る。
        (Ok(remote), Some(target)) if repo_unread(state).is_none() => {
            let plan = checkout_plan(pr, remote, target, state.branches.branches());
            return Cmd::Exec(GitOp::CheckoutPr(Box::new(plan)));
        }
        // **診断だけを出す。** `NoRemote::into_notice` が足す「追跡先を設定して
        // ください」は `f` / `P` の話で、取り出しはブランチの追跡設定を読まない。
        (Err(NoRemote::Nobody(diagnosis) | NoRemote::Ask(diagnosis)), _) => diagnosis,
        // 一覧が出ているなら `target` は決まっている（一覧を引く前提）。届く前に
        // 一覧だけが残る経路は無いが、組めないものは断る。
        (Ok(_), None) => "取り出す先のリポジトリが決まっていません".to_string(),
        // `target` が決まっているなら一度は届いているので、ここに来るのは読み直しが
        // 失敗したとき（[`repo_unread`]）。
        (Ok(_), Some(_)) => repo_unread(state)
            .unwrap_or("リポジトリ情報を読めませんでした")
            .to_string(),
    };
    state.push_notice(NoticeLevel::Warn, refusal, None);
    Cmd::None
}

/// PR をどう取り出すか（05 の「PR チェックアウト」）。**どの場面かを決めるだけ**で、
/// 手順は `git_exec` が場面から導く。
///
/// - **同一リポジトリか**は `head.repo` と `target` の `owner` / `repo`
///   （`RepoRef::same_as`。現ブランチの PR と同じ比べ方）。**消えた fork
///   （`head.repo` が `None`）は fork**
/// - **「在るか」は `branches`**（05）。古くても在るブランチを書き換えない ——
///   作る手順が `already exists` で断る。**「いま居るか」は見ない**（05）
fn checkout_plan(
    pr: &PullRequest,
    remote: &Remote,
    target: &RepoTarget,
    branches: &[Branch],
) -> PrCheckout {
    let head = &pr.head;
    let target = RepoRef {
        owner: target.owner.clone(),
        repo: target.repo.clone(),
    };
    let same_repo = head.repo.as_ref().is_some_and(|repo| repo.same_as(&target));

    let (branch, source) = if same_repo {
        (head.branch.clone(), PrSource::SameRepo)
    } else {
        let branch = fork_branch_name(pr, remote);
        // **URL を組めれば fork の URL と `refs/heads/<head>`、組めなければ fetch 元と
        // `refs/pull/<n>/head`**（05 の「追跡先」。消えた fork は後者）。
        let fork_url = head
            .repo
            .as_ref()
            .and_then(|repo| url::sibling(&remote.url, &repo.owner, &repo.repo));
        let upstream = match fork_url {
            Some(url) => ForkUpstream {
                remote: url,
                merge: format!("refs/heads/{}", head.branch),
            },
            None => ForkUpstream {
                remote: remote.name.clone(),
                merge: format!("refs/pull/{}/head", pr.number),
            },
        };
        (
            branch,
            PrSource::Fork {
                number: pr.number,
                upstream,
            },
        )
    };

    let local = if branches.iter().any(|local| local.name == branch) {
        LocalBranch::Present
    } else {
        LocalBranch::Absent
    };
    PrCheckout {
        remote: remote.name.clone(),
        branch,
        source,
        local,
    }
}

/// fork の PR を取り出すブランチの名前（05 の「ブランチ名」）。
///
/// **既定ブランチか PR の base と同名なら owner を前に付ける。** 付けないと、
/// fork の `main` でローカルの `main` を早送りすることになる。既定ブランチは
/// `refs/remotes/<r>/HEAD` でしか知らず、一度も fetch していなければ無いので、
/// base とも比べる。**owner が分からない消えた fork は `pr-<n>/`。**
fn fork_branch_name(pr: &PullRequest, remote: &Remote) -> String {
    let name = &pr.head.branch;
    let collides = remote.head_branch.as_deref() == Some(name.as_str()) || pr.base.branch == *name;
    if !collides {
        return name.clone();
    }
    match &pr.head.repo {
        Some(repo) => format!("{}/{name}", repo.owner),
        None => format!("pr-{}/{name}", pr.number),
    }
}

/// `N`。[`Modal::PrCreate`] を開く（01 の「PR 作成（`N`）」）。
///
/// **押した時点で断る**のは、**リクエストを組めない**か**確実に 422 になる**もの
/// —— GitHub が使えない（[`AppState::github_block`] が値で答える。`r` と同じ）、
/// HEAD がブランチでない（head に載せる名前が無い）、このブランチの PR が既にある、
/// `head` の候補が無い（[`AppState::head_candidates`]）。
///
/// **3 つ目は [`AppState::current_pr`] の答えをそのまま使う。** 別の判定を書くと
/// 座り直し（01 の「現ブランチの PR」）と食い違い、**カーソルが座っているその行の
/// PR を「無い」と言って作りに行く**形が作れる。**行き止まりにはならない** ——
/// 一覧を落とす経路（`f` / `A` / `r`）がそのままこの断りを落とす経路になっている
/// （#53）。
///
/// **候補が無いときに断るのは、ブランチ名だけの `head` を送らないため。** owner を
/// 付けない `head` は base リポジトリの中のブランチとして解決されるので、`target` 側に
/// 同名のブランチがあれば他人のブランチから PR が開く（01 の「head の組み立て」）。
fn open_pr_create(state: &mut AppState) -> Cmd {
    if github_ready(state).is_none() {
        return Cmd::None;
    }
    // **問うのは確定と同じ関数**で、2 本の判定に割ると開く側だけが通して閉じられない
    // 確定へ人を運ぶ。欄に写す候補（ブランチ名を含む）は確定のときに組み直すので、
    // 開いているあいだに HEAD が動いても古い名前では送らない（[`pr_create_submit`]）。
    if !on_branch(&state.branches) {
        state.push_notice(NoticeLevel::Warn, OFF_BRANCH.to_string(), None);
        return Cmd::None;
    }
    if let Some(number) = state.current_pr().map(|pr| pr.number) {
        state.push_notice(
            NoticeLevel::Info,
            format!("このブランチの PR は既にあります（#{number}）"),
            None,
        );
        return Cmd::None;
    }
    let heads = state.head_candidates();
    if heads.is_empty() {
        state.push_notice(NoticeLevel::Warn, NOT_PUSHED.to_string(), None);
        return Cmd::None;
    }

    // **既定リモートが言う既定ブランチ**（02 の `Remote::head_branch`）。一度も
    // fetch していないリモートには symref が無いので、そのときは空欄で開く。
    // **`conventional_remote` を使う** —— `RepoInfo.target` を決めたのもこれなので、
    // base は**その target の既定ブランチ**になる（`default_remote` の追跡先の段は
    // 通さない。あちらは fetch の行き先の話）。
    let base = conventional_remote(state)
        .ok()
        .and_then(|remote| remote.head_branch.clone())
        .unwrap_or_default();
    state.modal = Some(Modal::PrCreate {
        title: TextBuffer::default(),
        body: TextBuffer::default(),
        base: TextBuffer::from_text(&base),
        heads: PrHeads::new(heads),
        draft: false,
        field: PrField::default(),
    });
    Cmd::None
}

/// `r`。**カーソル行の集計を落として一覧を引き直す**（01 の「引き直す（`r`）」）。
///
/// **自動の引き金では拾えないものを人に押させる。** 集計の鍵は `(番号, head sha)` なので
/// **sha が動かない変化**（レビューが付いた / チェックが回り直した）を拾えず、一覧の
/// 引き金（[`reload`] の doc）には**タブを開く操作が入っていない**ので、他人が開いた
/// PR は `f` を押すまで出ない。
///
/// **落とすのはカーソル行の集計だけ。** 全件落とすと、押すたびに一覧の行数ぶんの往復を
/// 払う（open 60 件で 181 リクエスト。05）。**一覧のほうは丸ごと引き直す** ——
/// あちらは 1 往復で済み、他人が開いた PR を知る経路がほかに無い。
///
/// **ここで扇形は頼まない。** [`fan_out`] を並べると、**これから引き直す一覧を待たずに
/// 手元の sha で 3 本張る**ので、head が動いていた回はその答えが捨てられて新しい sha で
/// もう 3 本払う。落とした行を頼むのは [`pr_list_loaded`] の出口で、あちらは**届いた
/// 一覧の sha**を使う。
///
/// **ただし、扇形の終端が一覧より先に届くと [`pr_details_finished`] の数え直しが
/// 手元の sha で頼む。** そこを塞ぐには「まだ引いていない」と「`r` が落とした」を状態で
/// 割ることになり、01 の「『まだ引いていない』を状態に書かない」を破る ——
/// **行き止まりにはならない**（どちらの順序でも最後は届いた一覧の sha で入る）ので、
/// その順序のときだけ 3 本ぶん余計に払う（01 の「引き直す（`r`）」）。
///
/// **世代は進めない。** 進めるのは `PrState::forget`（アカウントが変わった）で、`r` は
/// アカウントを変えない —— 飛んでいる扇形の答えは**いまのアカウントの答え**なので、
/// 捨てる理由が無い。**そもそも扇形が走っている行に落とすものは無い** ——
/// 対象は `PrState::wanted_details`（集計を持っていない行）なので、答えが飛んでいる
/// 行は集計を持たない。
///
/// **引けないときは理由を通知に出す**（押しても何も起きない形にしない）。**理由を
/// 消去法で組み立てない** —— [`AppState::github_block`] が腕で答える（潰すと
/// `Degraded` が「アカウントが決まっていない」になり、PRs ペインが同じ瞬間に
/// 出している `GitHub: …（A）` と食い違う）。
fn refresh(state: &mut AppState) -> Cmd {
    if github_ready(state).is_none() {
        return Cmd::None;
    }
    // 一覧が空なら落とす記憶も無い（それでも一覧は引き直す —— 他人が開いた PR は
    // そこで初めて出る）。
    if let Some(number) = state.prs.selected().map(|pr| pr.number) {
        state.prs.forget_detail(number);
    }
    // **`// 続きを頼む:` の印は置かない。** あれは「届いた返信を見て続きを判じる」
    // 場所の索引で（`update_inner` の doc）、ここは**キーが新しく頼む**場所。
    // 落とした行を拾うのは `pr_list_loaded` の出口で、印はそちらにある。
    reload(state)
}

/// [`Modal::PrCreate`] のキー処理（01 の「PR 作成（`N`）」）。
///
/// **`Modal::CommitMessage` に揃える。** 修飾なしの `Enter` はどの欄でも確定、
/// 修飾付きの `Enter` と `Ctrl-J` は body 欄でだけ改行、`Esc` で破棄。
/// **`Enter` の意味をモーダルごとに変えない** —— `CommitMessage` も `Input` も
/// `Enter` で確定する。
///
/// **`Tab` / `Shift+Tab` は欄の巡回。** `update_key` はモーダル表示中に
/// ペインの巡回へ渡さない（`modal.is_some()` が先に受ける）ので、ここで使える。
///
/// **`Space` は draft の欄でだけトグル。** ほかの欄では打った空白がそのまま入る
/// （[`edit_key`](super::edit_key) が受ける）。
///
/// **`←` / `→` は head の欄でだけ候補を選び直す。** ほかの欄では文字カーソルの移動。
pub(super) fn pr_create_key(state: &mut AppState, key: Key) -> Cmd {
    // 取り消し。**確認は挟まない**（`CommitMessage` と同じ。01）。
    if key == Key::new(KeyCode::Esc) {
        state.modal = None;
        return Cmd::None;
    }
    // **修飾なしの `Enter` だけが確定。** 修飾付きは下の改行に落ちる。
    if key == Key::new(KeyCode::Enter) {
        return pr_create_submit(state);
    }
    match key.code() {
        KeyCode::Tab => return move_pr_field(state, PrField::next),
        KeyCode::BackTab => return move_pr_field(state, PrField::prev),
        _ => {}
    }

    let Some(Modal::PrCreate {
        title,
        body,
        base,
        heads,
        draft,
        field,
    }) = state.modal.as_mut()
    else {
        return Cmd::None;
    };

    // **欄の判断は 1 箇所。** どの欄かを何度も問い直すと、到達しない腕が混ざる。
    match field {
        // **文字を打つ欄ではないので、編集キーを一切渡さない。**
        PrField::Head => match key.code() {
            KeyCode::Left => heads.move_cursor(-1),
            KeyCode::Right => heads.move_cursor(1),
            _ => {}
        },
        PrField::Draft => {
            if key == Key::char(' ') {
                *draft = !*draft;
            }
        }
        // **改行できるのは body だけ**（`CommitMessage` と同じ規則を共有する）。
        // 1 行の欄に 2 行目ができると、`text()` の返す `\n` 入りの文字列がそのまま
        // タイトルやブランチ名になる（`Modal::Input` と同じ理由。01 の「1 行入力」）。
        PrField::Body => multiline_edit_key(body, key),
        PrField::Title => {
            let _ = edit_key(title, key);
        }
        PrField::Base => {
            let _ = edit_key(base, key);
        }
    }
    Cmd::None
}

/// GitHub を叩く先。叩けないなら理由を通知に出して `None`。
///
/// **理由を消去法で組み立てない** —— [`AppState::github_target`] が腕で答える
/// （潰すと `Degraded` が「アカウントが決まっていない」になり、PRs ペインが
/// 同じ瞬間に出している `GitHub: …（A）` と食い違う）。**叩く先も同じ答えから
/// 取る** —— 理由だけを見て `target` を読み直すと、「叩けるのに `target` が無い」
/// という到達しない腕が要る。
fn github_ready(state: &mut AppState) -> Option<RepoTarget> {
    match state.github_target() {
        Ok(target) => Some(target.clone()),
        Err(block) => {
            state.push_notice(NoticeLevel::Info, block.notice(), None);
            None
        }
    }
}

/// 編集中の欄を巡回する。
///
/// **クロージャではなく `fn` を受ける**（`modal::picker_key` と同じ）。環境を
/// 捕まえる必要が無い。
fn move_pr_field(state: &mut AppState, step: fn(PrField) -> PrField) -> Cmd {
    if let Some(Modal::PrCreate { field, .. }) = state.modal.as_mut() {
        *field = step(*field);
    }
    Cmd::None
}

/// 打った字で `Cmd::CreatePr` を組む。
///
/// **title が空なら断る。** GitHub も 422 を返すが、**モーダルを閉じてからでは
/// 打った本文が戻らない**（`commit_submit` と同じ理由。01）。**base は断らない**
/// —— 空のまま送ると GitHub が「base が無い」と 422 で答えるが、**何が正しい値かを
/// 知っているのは GitHub 側**で、こちらが持つのは symref だけ（無いことがふつうに
/// ある。02）。
///
/// **head は欄で選んだ候補**（01 の「head の組み立て」）。**写しを送らない** ——
/// 開いているあいだに `RepoInfo` が読み直されると候補が変わるので、ここで
/// [`AppState::head_candidates`] を引き直し、選んでいた候補が残っていなければ**閉じずに欄を
/// 入れ替えて確かめ直させる**。
///
/// **`Cmd` が落とされるなら閉じない**（`update::submit`）。**材料が消えていても
/// 閉じない** —— 開いてから `A` の書き込みが終わると `account` が `Resolving` に
/// 戻る（下のコメント）。
fn pr_create_submit(state: &mut AppState) -> Cmd {
    let Some(Modal::PrCreate {
        title,
        body,
        base,
        heads,
        draft,
        ..
    }) = &state.modal
    else {
        return Cmd::None;
    };
    if title.is_blank() {
        state.push_notice(NoticeLevel::Warn, "タイトルが空です".to_string(), None);
        return Cmd::None;
    }
    // **打った字を先に写す。** 下の断りが `state` を可変で借りるので、モーダルを
    // 借りたままでは通せない。
    let typed = (title.text(), body.text(), base.text(), *draft);
    let chosen = heads.selected().cloned();

    // **叩く先は確定のときに取る**（[`github_ready`]）。いまは開いてから材料が消える
    // 経路が無い（`account` を落とすのは `Modal::AccountPicker` の中だけ。
    // `update::account::leave_account`）が、開いたときの答えを箱に写すと、落とす
    // 経路が増えたときに材料の無い `Cmd::CreatePr` が出て**打った本文が消える**。
    let Some(target) = github_ready(state) else {
        return Cmd::None;
    };
    if !on_branch(&state.branches) {
        // 開いたあとに HEAD が動いた形。**閉じない** —— 打った本文を返せない。
        state.push_notice(NoticeLevel::Warn, OFF_BRANCH.to_string(), None);
        return Cmd::None;
    }
    let fresh = state.head_candidates();
    let Some(chosen) = chosen.filter(|chosen| fresh.contains(chosen)) else {
        // 開いたあとに候補が変わった形（`f` / 外の checkout で `RepoInfo` が読み直された）。
        // **閉じずに欄を入れ替え、そこへ移す** —— 選び直すのは人。
        let notice = if fresh.is_empty() {
            NOT_PUSHED
        } else {
            HEADS_CHANGED
        };
        if let Some(Modal::PrCreate { heads, field, .. }) = state.modal.as_mut() {
            *heads = PrHeads::new(fresh);
            *field = PrField::Head;
        }
        state.push_notice(NoticeLevel::Warn, notice.to_string(), None);
        return Cmd::None;
    };
    let head = chosen.api_value(&target.owner);
    let (title, body, base, draft) = typed;
    let input = CreatePr {
        title,
        body,
        head,
        base,
        draft,
    };
    submit(state, Cmd::CreatePr(Box::new(input)))
}

/// ブランチに乗っていないときの案内。**開く側と確定する側で同じ字を出す。**
const OFF_BRANCH: &str = "ブランチに乗っていないので PR を作れません";

/// HEAD がブランチに乗っているか。**開く側と確定する側で同じ判定を使う。**
///
/// `head` に載せる名前は [`AppState::head_candidates`] の候補が持つ（01 の
/// 「head の組み立て」）。ここは「乗っていない」を候補が空と区別して案内するためだけ。
/// **候補と同じく HEAD の行から読む**（[`BranchState::head_branch`]。01 の「現ブランチの PR」）。
fn on_branch(branches: &BranchState) -> bool {
    branches.head_branch().is_some()
}

/// どのリモートにもこのブランチが無いときの案内。**開く側と確定する側で同じ字を出す。**
///
/// **`f` を案内する** —— 候補は remote-tracking ref から組むので、別の端末から
/// push しただけのブランチは取得するまで候補に出ない（01 の「head の組み立て」の代償）。
const NOT_PUSHED: &str = "どのリモートにもこのブランチがありません（push 済みなら f で取得）";

/// 開いているあいだに候補が変わったときの案内。
const HEADS_CHANGED: &str = "head の候補が変わりました。確かめてから確定してください";

/// PR を作った答えが届いた（01 の「PR 作成（`N`）」）。
///
/// **成功したら一覧を引き直す。** PR 作成は ref を動かさないので、既存の引き金
/// （[`reload`] の doc）では拾えない —— 引かないと、いま作った PR が画面に出ない。
///
/// **カーソルは作った行へ。** `PrState::loaded` は「いま選んでいる PR の番号」を
/// 優先するので、番号を憶えさせないと**新しい行が先頭に入ってもカーソルは残る**。
///
/// **運ぶのは番号だけ**（`Msg::PrCreated`）。作った PR の残りのフィールドを読む場所が
/// 無く、載せると `Msg` の上限のために `Box` が要る。
///
/// **失敗の本文は `detail` へ。** 422 の本文にも 403 のホスト名にも上限が無いので、
/// 要約行に混ぜると端末の幅から末尾が切れる（01 の「失敗の案内文」）。
pub(super) fn pr_created(state: &mut AppState, result: Result<u64, GithubError>) -> Cmd {
    state.jobs.finish(JobKind::CreatePr);
    match result {
        Ok(number) => {
            state.push_notice(
                NoticeLevel::Info,
                format!("PR #{number} を作りました"),
                None,
            );
            // **落とされることを憶える。** 下の `reload` が返す `Cmd::FetchPrList` は、
            // 既に一覧が飛んでいれば `AppState::issue` に落とされる。そこへ届くのは
            // **作成より前に頼まれた一覧**なので、作った PR は入っていない。
            let behind = state.jobs.is_running(JobKind::FetchPrList);
            state.prs.seat_on_created(number, behind);
            // 続きを頼む: 作った PR が入った一覧。
            reload(state)
        }
        Err(err) => {
            state.push_notice(
                NoticeLevel::Error,
                "PR を作れませんでした".to_string(),
                Some(err.to_string()),
            );
            Cmd::None
        }
    }
}

/// 材料が揃っていれば一覧を頼む。
///
/// **材料は 2 つ**（`AppState::wanted_pr_list`）。**入口はそれを動かすものと、人の意思。**
/// ref（`Msg::RepoInfoLoaded`。commit / push のあとは GitHub 側の PR と CI が動いており、
/// マージされていれば open から消える）、**どのアカウントで引いたか**
/// （`Msg::AccountResolved`。`A` で選び直すと、前のトークンでは見えなかった PR が
/// 見える）、[`refresh`]（`r`）、そして [`pr_created`]（`N` で作った直後。**ref は
/// 動かないので前の 2 つでは拾えない**）。
///
/// **ここが失敗からの出口でもある。** 引けなかった答えは自分では落ちないので、
/// `f` / `A` / `r` のどれかで引き直せる形にしておく（`.claude/rules/rust-coding.md` の「状態を消すのが
/// 『その状態を根拠に断つキー』の先にあるなら、断つ前に消し方を数える」）。
///
/// **出口は一覧だけでなく集計にも要る。** 集計を落とすのは `PrState::loaded`（届いた
/// 一覧で `Err` の行を捨てる）、`PrState::forget`（アカウントが変わった）、
/// `PrState::forget_detail`（`r` がカーソル行を名指し）。**前の 2 つはこの経路の先に
/// ある** —— 一覧を引き直さないと集計の失敗も落ちない。集計の失敗は通知に出ない
/// （07 で受け入れた穴）ので、そこが塞がると行の `!` は `r` でしか消せなくなる。
pub(super) fn reload(state: &AppState) -> Cmd {
    if state.wanted_pr_list() {
        Cmd::FetchPrList
    } else {
        Cmd::None
    }
}

/// アカウントが変わるので一覧を落とす（`PrState::forget`）。**飛んでいる一覧の
/// 返事も捨てさせる**（`PrState::orphan_list`。01 の「一覧はアカウントに紐づく」）。
pub(super) fn forget(state: &mut AppState) {
    state.prs.forget();
    if state.jobs.is_running(JobKind::FetchPrList) {
        state.prs.orphan_list();
    }
}

/// 一覧が届いた。
///
/// **鍵は無いが世代は見る**（01 の「何を引くか」）。書き込みの最中に飛んでいた要求の
/// 答えは**書き込み前の GitHub の状態**だが、出せるものは出したままにして裏で引き直す
/// （`repo_info_loaded` と同じ形）。
///
/// **失敗しても読めていた一覧は捨てない**（`PrState::failed`）。引き直すのは ref を
/// 動かす書き込みのたびなので、捨てると commit 直後に 1 回転けただけでペインが
/// エラー 1 行になる。
pub(super) fn pr_list_loaded(
    state: &mut AppState,
    result: Result<Vec<PullRequest>, GithubError>,
) -> Cmd {
    let asked = state.jobs.finish(JobKind::FetchPrList);
    // **前のアカウントで頼んだ返事は捨てて頼み直す**（[`forget`]）。`Resolving` の
    // あいだなら `reload` は何も出さず、決まったときの `reload` が頼む。
    if state.prs.take_list_orphaned() {
        return reload(state);
    }
    match result {
        // **座り直す先は `PrState::loaded` が決める。** 呼ぶ側が渡すのは「いま選んで
        // いる PR が消えていたときの行き先」だけで、それは新しい一覧といまの HEAD から
        // 決まる（01 の「現ブランチの PR」）。
        Ok(list) => {
            // **`repo` と `branches` だけを借りる。** `state.prs` を書き換えながら読むので、
            // 閉包が掴むのをその 2 フィールドに絞る（Rust 2021 の分割キャプチャ）。
            let (repo, branches) = (&state.repo, &state.branches);
            state.prs.loaded(list, |list| {
                current_row(repo, branches, list).map(|pr| pr.number)
            })
        }
        Err(err) => state.prs.failed(err),
    }
    let again = if state.jobs.is_fresh(JobKind::FetchPrList, asked) {
        Cmd::None
    } else {
        // 続きを頼む: 書き込みに追い越された `FetchPrList`。
        reload(state)
    };
    // 続きを頼む: 作成に追い越された `FetchPrList`（`PrState::list_behind`）。
    //
    // **`is_fresh` では拾えない。** あれが見るのは領域の世代で、**PR 作成は
    // ローカルの ref を動かさない**ので進まない（`JobKind::CreatePr::region()`）。
    // **1 回きり** —— 当たらないたびに頼み直すと、作った PR が即座に閉じられた
    // ときに同じ往復を回し続ける。当たらなかった記憶はここで落とす。
    let chased = if state.prs.take_list_behind() {
        reload(state)
    } else {
        if state.prs.has_unseated_creation() {
            state.prs.forget_created();
        }
        Cmd::None
    };
    // 続きを頼む: 一覧に増えた行の集計。
    Cmd::batch([again, chased, fan_out(state)])
}

/// 集計をまだ持っていない行があれば扇形を頼む（01 の「PR ペイン › 集計」）。
///
/// **`Cmd` は 1 本。** PR ごとに出すと [`JobKind`] が種別単位なので 2 本目以降を
/// [`AppState::issue`] が黙って落とす（05 の「`Semaphore`」）。
///
/// **落とされても詰まらない。** 扇形が走っているあいだに新しい一覧が届くと、この
/// `Cmd` は `issue` に落ちるが、走っているほうが [`pr_details_finished`] で終わる
/// ときに残りを数え直す。**`Pending` を状態に書かないのはそのため** —— 書くと、
/// 落とされた `Cmd` のぶんが「頼んだのに誰も返さない行」として残る。
///
/// **PRs タブが見えていなければ空**（門は [`AppState::wanted_details`]）。だから
/// ここは [`update::focus`](super::focus) からも呼ぶ —— **タブに入った回に頼むのは
/// この経路だけ**で、無いと開いた直後の行が `…` のまま次に一覧が届くまで埋まらない。
/// 一覧が届く経路（[`pr_list_loaded`]）だけでは、タブを開くこと自体が引き金に
/// ならない。
pub(super) fn fan_out(state: &AppState) -> Cmd {
    let targets = state.wanted_details();
    if targets.is_empty() {
        Cmd::None
    } else {
        Cmd::FetchPrDetails {
            epoch: state.prs.epoch(),
            targets,
        }
    }
}

/// 扇形の答えが 1 件届いた。
///
/// **ジョブは畳まない。** 1 つの `Cmd::FetchPrDetails` から何度も届くので、畳むのは
/// [`pr_details_finished`] のほう。
///
/// **追いつきは 2 つ要る。** `(番号, sha)` は「一覧のどの行に当てるか」を答え
/// （[`JobKind::region`] に世代を使わない理由がある）、世代は「**どのアカウントで
/// 引いたか**」を答える —— アカウントを切り替えても番号も head も変わらないので、
/// 鍵だけでは前のトークンの答えが新しい一覧に入る。
pub(super) fn pr_detail_loaded(
    state: &mut AppState,
    epoch: u64,
    number: u64,
    sha: String,
    result: Result<PrDetail, GithubError>,
) -> Cmd {
    if !state.prs.is_current(epoch) {
        return Cmd::None;
    }
    state.prs.detail_loaded(number, sha, result);
    Cmd::None
}

/// 扇形が終わった。
///
/// **打ち切りは残りの行に書く。** `Some(err)` はレート制限で残りを頼まなかったという
/// 意味で、埋めずに返すと答えの無い行が `PrState::wanted_details` に拾われ、**同じ
/// 失敗を買い直す輪**になる（01 の「`Err` から再発行してよいのは対象が違うときだけ」）。
///
/// **打ち切ったときだけ通知を出す。** 行に出せるのは「引けなかった」という印 1 つ
/// だけで（枠の内側にそれ以上入らない。07）、**理由を知る経路がほかに無い** ——
/// レート制限は待てば直るものなので、そのまま `!` を並べると行き止まりになる
/// （#63 の「無反応よりまし」で止めない）。
///
/// **1 件ごとの失敗では出さない。** fork が消えた PR の 404 は行ごとの事情で、
/// 一覧ぶん通知を積んでも次に何をするかが変わらない。
pub(super) fn pr_details_finished(
    state: &mut AppState,
    epoch: u64,
    aborted: Option<(GithubError, Vec<(u64, String)>)>,
) -> Cmd {
    // **ジョブは世代に関係なく畳む。** 走っていたのはこのジョブなので、畳まないと
    // 以後の `Cmd::FetchPrDetails` が `AppState::issue` に落とされ続ける。
    state.jobs.finish(JobKind::FetchPrDetails);
    // **効果のほうは世代で門にする。** 前のアカウントで頼んだ扇形の打ち切りを
    // 新しい一覧に書くと、`PrState::forget` が落とした集計をそのまま書き戻す。
    if state.prs.is_current(epoch) {
        if let Some((err, targets)) = aborted {
            state.prs.details_aborted(&targets, &err);
            // **理由は `detail` に載せる。** 要約行に `GithubError` の `Display` を
            // 混ぜると **1 行目の予算を超えて末尾から切れる** ——
            // `InvalidHost` はホスト名をそのまま持ち、**ホスト名に上限が無い**ので、
            // 予算をいくつにしても変わらない（`-bad` の 4 文字でちょうど 72 セル。
            // 実測 2026-09-18。当時の予算が 72 だったのは `q:quit` と同じ行だったため）。
            // `exec_finished` が argv と git の出力を `detail` に載せるのと同じ形で、
            // **読めるのは `L` の履歴**（01 の「通知」）。
            state.push_notice(
                NoticeLevel::Warn,
                "PR の集計を中断しました".to_string(),
                Some(err.to_string()),
            );
        }
    }
    // 続きを頼む: 扇形が走っているあいだに一覧へ増えた行と、世代が変わって捨てた行。
    fan_out(state)
}

/// 応答が言ったレートリミット残量が届いた。
///
/// **世代で門にする。** 残量は**そのトークンの予算**なので、アカウントを切り替えた
/// あとに前のトークンの扇形が返す数は嘘になる。`(reset, used)` は「どちらの応答が
/// 新しいか」しか答えられない（#89 の「鍵が答えられる問いを数える」）。
///
/// **ジョブは畳まない。** 走っているのは一覧か扇形のジョブで、これはその途中に
/// 何度でも流れる（[`Msg::RateLimitSeen`](crate::app::Msg::RateLimitSeen)）。
pub(super) fn rate_limit_seen(state: &mut AppState, epoch: u64, seen: RateLimit) -> Cmd {
    if state.prs.is_current(epoch) {
        state.prs.rate_limit_seen(seen);
    }
    Cmd::None
}

/// `gh browse` が終わった。
///
/// **`Ok` では何も出さない。** ブラウザが前に出てくるので、画面に足す言葉が無い。
pub(super) fn open_pr_finished(state: &mut AppState, result: Result<(), AuthError>) -> Cmd {
    state.jobs.finish(JobKind::OpenPr);
    if let Err(err) = result {
        // `AuthError::GhFailed` は argv と出力を持っている（04）。
        state.push_notice(NoticeLevel::Error, err.to_string(), None);
    }
    Cmd::None
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        cells, deliver_branches, enter_with, github_disabled_state, issued, load_branches, press,
        press_code, remotes_of, state, state_with_commits, tab_to, tracking_branch, type_text, ALT,
        NOTICE_CELLS,
    };
    use super::super::update;
    use super::*;
    use crate::app::{
        GithubBlock, Msg, NoticeLevel, PrLoad, Region, RepoSnapshot, RightTab, TargetState,
    };
    use crate::app::{ListLoad, PrHead};
    use crate::auth::{AccountState, DegradedReason};
    use crate::git_exec::GitOp;
    use crate::git_read::GitReadError;
    use crate::git_read::{BranchList, RefIndex, RefLabel, Remote, RowlessHead, UpstreamConfig};
    use crate::github::RepoRef;
    use crate::github::{CheckSummary, ReviewSummary};
    use crate::test_util::{
        deliver_pr_details, fake_account, fake_branch, fake_branch_list, fake_branch_list_or,
        fake_head_list, fake_logins, fake_oid, fake_pull_request, fake_rate_limit,
        fake_repo_snapshot, fake_target, fetch_pr_details, pr_list_loaded as list_msg,
        put_on_branch, reply, start_pr_details,
    };

    /// アカウントと対象が決まり、HEAD が `branch` に居る状態。一覧はまだ引いていない。
    fn state_on_branch(branch: &str) -> AppState {
        let mut state = state_with_commits(2);
        put_on_branch(&mut state, branch);
        state
    }

    /// [`state_on_branch`] に加えて、**HEAD のブランチが `tracked` の同名ブランチを
    /// 追跡している**状態（`branch.<HEAD>.remote` / `.merge`）。`remotes` は
    /// `(名前, URL)` で登録する。
    fn state_tracking(branch: &str, tracked: &str, remotes: &[(&str, &str)]) -> AppState {
        let mut state = state_on_branch(branch);
        track(&mut state, tracked, &format!("refs/heads/{branch}"));
        state.repo.remotes = remotes_of(remotes);
        state
    }

    /// HEAD のブランチの追跡設定を置き直す。
    ///
    /// **`put_on_branch` が置いた HEAD の行に足す。** あちらは追跡設定を持たない形
    /// （`upstream_config: None`）なので、そのままでは追跡先の腕に入らない。
    /// **いまの行を書き換えて組み直す** —— `Branch` のリテラルを写すと、フィールドが
    /// 増えるたびに直す構築サイトが 1 つ増える。
    fn track(state: &mut AppState, remote: &str, merge: &str) {
        let row = state
            .branches
            .head_row()
            .expect("put_on_branch が HEAD の行を置いている");
        edit_rows(state, |rows| {
            rows[row].upstream_config = Some(UpstreamConfig {
                remote: remote.to_string(),
                merge: merge.to_string(),
            });
        });
    }

    /// ブランチ一覧の行を書き換えて届け直す（一覧のフィールドは private で、行だけを
    /// 書き換えられない）。
    fn edit_rows(state: &mut AppState, edit: impl FnOnce(&mut Vec<Branch>)) {
        let mut rows = state.branches.branches().to_vec();
        edit(&mut rows);
        let list = relist(state, rows);
        state.branches.loaded(list);
    }

    /// `rows` で一覧を組み直す。**HEAD はいまの HEAD の行の名前を持ち越す**
    /// （[`fake_branch_list`]）。
    fn relist(state: &AppState, rows: Vec<Branch>) -> BranchList {
        let head = state.branches.head_branch().expect("HEAD の行がありません");
        fake_branch_list(rows, Some(&head.name))
    }

    /// `owner` の fork から出た、同じブランチ名の PR。
    fn pr_from(number: u64, branch: &str, owner: &str) -> PullRequest {
        let mut pr = fake_pull_request(number, branch, "headsha");
        pr.head.repo = Some(RepoRef {
            owner: owner.to_string(),
            repo: fake_target().repo,
        });
        pr
    }

    /// [`state_on_branch`] に加えて、**PRs タブを開いた**状態。
    ///
    /// **門を踏むテストでは `state.focus` に直に代入しない。** 門
    /// （`AppState::wanted_details`）はタブに入る経路（`Tab` → `update::focus`）が
    /// 扇形を頼むかまで見るので、代入だけだと**引き金を一度も踏まないまま**開いた
    /// 状態になる。門に関わらないキー処理のテスト（`j` / `o` など）は直代入でよい。
    ///
    /// **開いた回の `Cmd` を捨てない。** 一覧がまだ無いので `Cmd::None` のはずで、
    /// そうでなければ `AppState::issue` に `FetchPrDetails` が登録され、**以後の扇形が
    /// 黙って落ちる**（#38 の形）。
    fn state_on_prs_tab(branch: &str) -> AppState {
        let mut state = state_on_branch(branch);
        let opened = tab_to(&mut state, Pane::Prs);
        assert_eq!(opened, Cmd::None, "一覧がまだ無いのに扇形が出た");
        state
    }

    /// [`state_on_prs_tab`] に加えて、**`head` の候補が 1 つある**状態 ——
    /// `origin`（[`fake_target`] と同じリポジトリ）に同名のブランチが在る。
    ///
    /// **`N` を押すテストの土台。** 候補が無いと `N` は開かない（01 の
    /// 「head の組み立て」）。
    fn state_for_create(branch: &str) -> AppState {
        let mut state = state_on_prs_tab(branch);
        state.repo.remotes = remotes_of(&[("origin", "git@github.com:yshrsmz/mewgit.git")]);
        set_copies(&mut state, &[("origin", true)]);
        state
    }

    /// HEAD のブランチと同名の remote-tracking ref を置き直す
    /// （`graph.refs`。`(リモート名, HEAD と同じ位置か)`）。
    fn set_copies(state: &mut AppState, copies: &[(&str, bool)]) {
        state.graph.refs = copies_of(&state.branches, copies);
    }

    /// [`set_copies`] の索引。`Msg::RepoInfoLoaded` で届ける形のテストも同じものを使う。
    fn copies_of(branches: &BranchState, copies: &[(&str, bool)]) -> RefIndex {
        let Some(Branch { name, oid, .. }) = branches.head_branch() else {
            unreachable!("put_on_branch が HEAD の行を置いている");
        };
        copies
            .iter()
            .map(|(remote, at_head)| {
                let at = if *at_head { *oid } else { fake_oid(9) };
                (
                    at,
                    RefLabel::Remote {
                        name: format!("{remote}/{name}"),
                    },
                )
            })
            .collect()
    }

    /// 開いている [`Modal::PrCreate`] の候補（欄に出る字）と、選んでいる位置。
    fn heads_of(state: &AppState) -> (Vec<String>, usize) {
        let Some(Modal::PrCreate { heads, .. }) = &state.modal else {
            panic!("PrCreate が開いていません: {:?}", state.modal);
        };
        (
            heads.list().iter().map(PrHead::label).collect(),
            heads.cursor(),
        )
    }

    /// [`state_on_prs_tab`] に加えて、**`#82` の集計まで憶えた**状態。
    fn state_with_one_summary(branch: &str) -> AppState {
        let mut state = state_on_prs_tab(branch);
        let ask = fan_out_after(&mut state, vec![fake_pull_request(82, branch, "sha82")]);
        let _ = deliver_pr_details(&mut state, ask, &[(82, "sha82", Ok(detail(2)))], None);
        state
    }

    /// 一覧を届けて、カーソルの座り先まで進める。
    fn deliver(state: &mut AppState, list: Vec<PullRequest>) {
        let ask = reload(state);
        let _ = reply(state, ask, list_msg(list));
    }

    /// 集計 1 件ぶんの値。中身は見分けが付けばよい。
    fn detail(success: usize) -> PrDetail {
        PrDetail {
            checks: CheckSummary {
                success,
                ..CheckSummary::default()
            },
            review: ReviewSummary::default(),
        }
    }

    /// 一覧を届けたあとに出る扇形。**`Cmd` が 1 本であることもここで見る。**
    fn fan_out_after(state: &mut AppState, list: Vec<PullRequest>) -> Cmd {
        let ask = reload(state);
        let out = reply(state, ask, list_msg(list));
        // **知らない形は panic させる。** `Cmd::None` に畳むと、`assert_eq!(x,
        // Cmd::None)` 型の見張り（持っている行を引き直さない / 輪にならない）が
        // **別の何かが出ていた場合も緑**になる。
        match out {
            Cmd::None => Cmd::None,
            cmd @ Cmd::FetchPrDetails { .. } => cmd,
            Cmd::Batch(cmds) => cmds
                .into_iter()
                .find(|cmd| matches!(cmd, Cmd::FetchPrDetails { .. }))
                .unwrap_or(Cmd::None),
            other => panic!("一覧の返信がこれを出した: {other:?}"),
        }
    }

    /// `N` を押した状態の [`Modal::PrCreate`] を取り出す。
    fn modal_of(state: &AppState) -> (&TextBuffer, &TextBuffer, &TextBuffer, bool, PrField) {
        let Some(Modal::PrCreate {
            title,
            body,
            base,
            draft,
            field,
            ..
        }) = &state.modal
        else {
            panic!("PrCreate が開いていません: {:?}", state.modal);
        };
        (title, body, base, *draft, *field)
    }

    /// 直近の通知の要約行。**末尾が直近**（ステータスバーが描くのもそこ）。
    fn last_notice(state: &AppState) -> String {
        state
            .notices
            .back()
            .map(|notice| notice.message.clone())
            .expect("通知が無い")
    }

    #[test]
    fn n_fills_the_base_from_the_default_remotes_head_and_leaves_it_empty_without_one() {
        // **base は `refs/remotes/<既定リモート>/HEAD`**（01 の「PR 作成（`N`）」）。
        // **`Some` と `None` を 1 本で見る** —— 空欄のほうだけを見る assert は、
        // `head_branch` を 1 行も置かなくても通る（`.claude/rules/testing.md`）。
        let mut state = state_for_create("feat/x");
        state.repo.remotes = vec![Remote {
            name: "origin".to_string(),
            url: "https://github.com/yshrsmz/mewgit.git".to_string(),
            head_branch: Some("trunk".to_string()),
        }];

        let cmd = press(&mut state, 'N');

        assert_eq!(cmd, Cmd::None, "開くだけで往復しない");
        let (title, body, base, draft, field) = modal_of(&state);
        assert_eq!(base.text(), "trunk");
        assert_eq!(title.text(), "", "title は空で開く");
        assert_eq!(body.text(), "", "body は空で開く");
        assert!(!draft, "draft は false で開く");
        assert_eq!(field, PrField::Title, "最初の欄は title");

        // 一度も fetch していないリモート（symref が無い）。
        let mut state = state_for_create("feat/x");
        state.repo.remotes = remotes_of(&[("origin", "https://github.com/yshrsmz/mewgit.git")]);
        let _ = press(&mut state, 'N');
        let (_, _, base, _, _) = modal_of(&state);
        assert_eq!(base.text(), "", "symref が無ければ空欄で開く");
    }

    #[test]
    fn n_refuses_when_the_branch_already_has_a_pull_request() {
        // **`AppState::current_pr` の答えをそのまま使う**（01）。別の判定を書くと、
        // **カーソルが座っているその行の PR を「無い」と言って作りに行く**形が作れる。
        let mut state = state_for_create("feat/x");
        deliver(&mut state, vec![fake_pull_request(7, "feat/x", "sha7")]);

        let cmd = press(&mut state, 'N');

        assert_eq!(cmd, Cmd::None);
        assert!(state.modal.is_none(), "開いてしまった");
        let notice = last_notice(&state);
        assert!(notice.contains("#7"), "番号を出していない: {notice}");

        // **行き止まりにはならない。** 一覧を引き直してその PR が消えれば開く
        // （#53 の「断つ前に消し方を数える」）。
        deliver(&mut state, Vec::new());
        let _ = press(&mut state, 'N');
        assert!(state.modal.is_some(), "一覧から消えても断り続けている");
    }

    #[test]
    fn n_refuses_off_a_branch_and_while_github_is_unavailable() {
        // **どちらもリクエストを組めない形**（01 の表）。
        // 実物の detached はブランチ一覧に HEAD の行も無い（判定はそちらを読む）。
        let mut state = state_for_create("feat/x");
        state.branches.loaded(fake_branch_list_or(
            vec![fake_branch("feat/x")],
            RowlessHead::Detached(fake_oid(1)),
        ));
        let cmd = press(&mut state, 'N');
        assert_eq!(cmd, Cmd::None);
        assert!(state.modal.is_none(), "detached で開いた");
        // **文言まで見る。** 候補も空になるので、`on_branch` を素通りしても「どのリモートにも
        // この**ブランチ**がありません」で断られ、`contains("ブランチ")` では区別できない。
        assert_eq!(last_notice(&state), OFF_BRANCH);

        // **理由は `github_block` が値で答える**（`r` と同じ。消去法で組まない）。
        let mut state = state_for_create("feat/x");
        state.account = AccountState::Degraded(DegradedReason::GhMissing);
        let _ = press(&mut state, 'N');
        assert!(state.modal.is_none(), "degraded で開いた");
        assert_eq!(last_notice(&state), GithubBlock::AccountDegraded.notice());
    }

    #[test]
    fn tab_walks_the_fields_and_space_only_toggles_draft() {
        let mut state = state_for_create("feat/x");
        let _ = press(&mut state, 'N');

        // 07 の縦の並びで巡る。**並びはここに書き下す** —— `PrField::ALL` から導くと、
        // `ALL` を並べ替えたときに 07 の並びと割れても通る（画面と `Tab` はどちらも `ALL` から出る）。
        for wanted in [
            PrField::Body,
            PrField::Base,
            PrField::Head,
            PrField::Draft,
            PrField::Title,
        ] {
            let _ = press_code(&mut state, KeyCode::Tab);
            assert_eq!(modal_of(&state).4, wanted, "Tab の行き先");
        }
        // `Shift+Tab` は同じ並びを逆に 1 周する（端で巻く）。
        for wanted in [
            PrField::Draft,
            PrField::Head,
            PrField::Base,
            PrField::Body,
            PrField::Title,
        ] {
            let _ = press_code(&mut state, KeyCode::BackTab);
            assert_eq!(modal_of(&state).4, wanted, "Shift+Tab の行き先");
        }
        // draft の欄へ戻る。
        let _ = press_code(&mut state, KeyCode::BackTab);
        assert_eq!(modal_of(&state).4, PrField::Draft);

        // **draft の欄でだけトグル。** ここで 1 回押して真になる。
        let _ = press(&mut state, ' ');
        assert!(modal_of(&state).3, "Space が draft を立てていない");
        let _ = press(&mut state, ' ');
        assert!(!modal_of(&state).3, "2 回目で戻らない");

        // title の欄では空白が入る文字（トグルではない）。
        let _ = press_code(&mut state, KeyCode::Tab);
        assert_eq!(modal_of(&state).4, PrField::Title);
        let _ = press(&mut state, ' ');
        assert!(!modal_of(&state).3, "title の欄で draft が変わった");
        assert_eq!(modal_of(&state).0.text(), " ", "空白が入っていない");
    }

    #[test]
    fn a_paste_lands_in_the_field_being_edited_and_keeps_one_line_fields_flat() {
        // **貼り付けも欄ごとに規則が変わる**（`update::paste`）。body は複数行、
        // title と base は 1 行（`Modal::Input` と同じ。改行を落とすと「末尾に改行が
        // 付いただけ」の正当な貼り付けまで拒む）。
        let mut state = state_for_create("feat/x");
        let _ = press(&mut state, 'N');

        let _ = update(&mut state, Msg::Paste("fix: a\nb".to_string()));
        assert_eq!(modal_of(&state).0.text(), "fix: ab", "title で改行が残った");

        let _ = press_code(&mut state, KeyCode::Tab);
        let _ = update(&mut state, Msg::Paste("1 行目\n2 行目".to_string()));
        assert_eq!(
            modal_of(&state).1.text(),
            "1 行目\n2 行目",
            "body で改行が落ちた"
        );

        let _ = press_code(&mut state, KeyCode::Tab);
        let _ = update(&mut state, Msg::Paste("ma\nin".to_string()));
        assert_eq!(modal_of(&state).2.text(), "main", "base で改行が残った");

        // **`Head` と `Draft` は文字を打つ欄ではない。** 貼り付けても何も変わらない
        // （`Head` に入ると候補の外の値が作れる）。
        let snapshot = |state: &AppState| {
            let (title, body, base, draft, _) = modal_of(state);
            (
                title.text(),
                body.text(),
                base.text(),
                draft,
                heads_of(state),
            )
        };
        for wanted in [PrField::Head, PrField::Draft] {
            let _ = press_code(&mut state, KeyCode::Tab);
            assert_eq!(modal_of(&state).4, wanted);
            let before = snapshot(&state);
            let _ = update(&mut state, Msg::Paste("x".to_string()));
            assert_eq!(
                snapshot(&state),
                before,
                "{wanted:?} の欄で貼り付けが入った"
            );
        }
    }

    #[test]
    fn a_modified_enter_makes_a_newline_only_in_the_body() {
        // **`CommitMessage` に揃える**（01）。修飾なしは確定、修飾付きは改行、
        // **改行できるのは body だけ** —— 1 行の欄に 2 行目ができると `text()` の
        // 返す字に `\n` が混ざる。
        let mut state = state_for_create("feat/x");
        let _ = press(&mut state, 'N');
        let _ = press(&mut state, 'a');
        let _ = update(&mut state, enter_with(ALT));
        assert_eq!(modal_of(&state).0.text(), "a", "title に改行が入った");

        let _ = press_code(&mut state, KeyCode::Tab);
        let _ = press(&mut state, 'b');
        let _ = update(&mut state, enter_with(ALT));
        let _ = press(&mut state, 'c');
        assert_eq!(modal_of(&state).1.text(), "b\nc");

        // `Ctrl-J` も同じ（どの端末でも届く改行）。
        let _ = update(&mut state, Msg::Key(Key::ctrl('j')));
        let _ = press(&mut state, 'd');
        assert_eq!(modal_of(&state).1.text(), "b\nc\nd");
    }

    #[test]
    fn enter_sends_what_was_typed_and_an_empty_title_keeps_the_box_open() {
        let mut state = state_for_create("feat/x");
        state.repo.remotes = vec![Remote {
            name: "origin".to_string(),
            url: "https://github.com/yshrsmz/mewgit.git".to_string(),
            head_branch: Some("main".to_string()),
        }];
        let _ = press(&mut state, 'N');

        // **title が空なら断る。** GitHub も 422 を返すが、閉じてからでは打った
        // 本文が戻らない（01）。
        let cmd = press_code(&mut state, KeyCode::Enter);
        assert_eq!(cmd, Cmd::None);
        assert!(state.modal.is_some(), "空の title で閉じた");
        assert!(last_notice(&state).contains("タイトル"), "{state:?}");

        type_text(&mut state, "fix");
        let cmd = press_code(&mut state, KeyCode::Enter);

        assert!(state.modal.is_none(), "確定したのに閉じていない");
        let Cmd::CreatePr(input) = cmd else {
            panic!("CreatePr が出ていない: {cmd:?}");
        };
        assert_eq!(
            *input,
            CreatePr {
                title: "fix".to_string(),
                body: String::new(),
                // **同一リポジトリなので owner を付けない**（01）。
                head: "feat/x".to_string(),
                base: "main".to_string(),
                draft: false,
            }
        );
    }

    #[test]
    fn the_head_comes_from_where_the_branch_is_and_not_from_what_it_tracks() {
        // **追跡先から owner を組まない**（01 の「head の組み立て」）。
        // `branch.<name>.remote` が答えるのは**何を基にしたか**で、
        // `git checkout -b feat bob/feat` はそれを `bob` にする（実測 2026-09-21、
        // git 2.55）—— そこから `<owner>:<branch>` を組むと、**自分の fork に
        // push したブランチが bob の owner で送られる**。候補は同名の
        // remote-tracking ref が在るリモート（ここでは自分の fork の `origin`）。
        //
        // **追跡先が死んでいないことを同じ 1 本で見る。** `head` が読まない以上、
        // 追跡設定を消しても `head` の assert は通る（#96 の「入力を 1 つ消しても
        // 緑」）ので、**座り直しがそれを読んでいること**を先に見る —— この 2 つが
        // 同じ状態から出る（寛容な照合と、人に選ばせる head）のが分けた理由そのもの。
        let mut state = state_tracking(
            "feat/x",
            "fork",
            &[
                ("fork", "git@github.com:someone-else/mewgit.git"),
                ("origin", "git@github.com:me/mewgit.git"),
            ],
        );
        set_copies(&mut state, &[("origin", true)]);
        let opened = tab_to(&mut state, Pane::Prs);
        assert_eq!(opened, Cmd::None, "一覧がまだ無いのに扇形が出た");

        // 追跡先の fork から出た PR に座る（#98）。**ここは `head_homes` を読む。**
        deliver(&mut state, vec![pr_from(7, "feat/x", "someone-else")]);
        assert_eq!(
            state.current_pr().map(|pr| pr.number),
            Some(7),
            "追跡先が座り直しに効いていない"
        );

        // その PR が閉じられた（一覧から消えた）ところから作る。
        deliver(&mut state, Vec::new());
        let _ = press(&mut state, 'N');
        assert_eq!(heads_of(&state), (vec!["me:feat/x".to_string()], 0));
        type_text(&mut state, "fix");

        let cmd = press_code(&mut state, KeyCode::Enter);

        let Cmd::CreatePr(input) = cmd else {
            panic!("CreatePr が出ていない: {cmd:?}");
        };
        // **fork の owner を付ける**（`target` は yshrsmz/mewgit）。
        assert_eq!(input.head, "me:feat/x");
    }

    #[test]
    fn n_refuses_when_no_remote_has_the_branch() {
        // **ブランチ名だけで送らない**（01 の「head の組み立て」）。owner を付けない
        // `head` は base リポジトリの中のブランチとして解決される。
        let mut state = state_for_create("feat/x");
        set_copies(&mut state, &[]);

        let cmd = press(&mut state, 'N');

        assert_eq!(cmd, Cmd::None);
        assert!(state.modal.is_none(), "候補が無いのに開いた");
        assert_eq!(last_notice(&state), NOT_PUSHED);
        assert!(cells(NOT_PUSHED) <= NOTICE_CELLS, "{NOT_PUSHED}");

        // **肯定まで 1 本で見る** —— 否定だけだと、何を置いても開かない実装で通る。
        // `f` で ref が届けば開く（案内のとおり）。
        set_copies(&mut state, &[("origin", false)]);
        let _ = press(&mut state, 'N');
        assert!(state.modal.is_some(), "ref が在るのに断った");
    }

    #[test]
    fn the_candidates_put_what_is_pushed_up_to_head_first_and_fold_by_owner() {
        // 並びは「HEAD と同じ位置の ref」→「遅れた ref」→「追跡設定の URL」
        // （`head_candidates` の doc）。**GitHub の形でないリモートと、同じ owner の
        // 2 つ目は候補にしない。**
        let mut state = state_tracking(
            "feat/x",
            // `gh pr checkout` の形（追跡設定が URL）。
            "https://github.com/bob/mewgit.git",
            &[
                ("local", "/srv/git/mewgit.git"),
                ("origin", "git@github.com:me/mewgit.git"),
                // **owner の大小文字は区別しない**（GitHub の login）。
                ("origin2", "git@github.com:Me/mewgit-other.git"),
                ("upstream", "git@github.com:yshrsmz/mewgit.git"),
            ],
        );
        set_copies(
            &mut state,
            &[
                ("local", true),
                ("origin", true),
                ("origin2", true),
                ("upstream", false),
            ],
        );
        state.focus = Pane::Prs;

        let _ = press(&mut state, 'N');

        assert_eq!(
            heads_of(&state).0,
            ["me:feat/x", "yshrsmz:feat/x", "bob:feat/x"]
        );

        // **遅れた ref が先に並んでいても、同じ位置のほうが前に来る。**
        let mut state = state_for_create("feat/x");
        state.repo.remotes = remotes_of(&[
            ("a", "git@github.com:alice/mewgit.git"),
            ("origin", "git@github.com:yshrsmz/mewgit.git"),
        ]);
        set_copies(&mut state, &[("a", false), ("origin", true)]);
        let _ = press(&mut state, 'N');
        assert_eq!(heads_of(&state).0, ["yshrsmz:feat/x", "alice:feat/x"]);
    }

    #[test]
    fn a_checked_out_fork_branch_is_offered_under_the_name_it_has_on_the_fork() {
        // **取り出しはローカルを `bob/main` にし、`merge` に `refs/heads/main` を書く**
        // （05 の「ブランチ名」—— fork の `main` でローカルの `main` を早送りしない
        // ため）。URL の候補をローカルの名前で組むと `bob:bob/main` —— fork に無い
        // ブランチを指す（`/code-review` が拾った）。
        let mut state = state_for_create("bob/main");
        set_copies(&mut state, &[]);
        track(
            &mut state,
            "git@github.com:bob/mewgit.git",
            "refs/heads/main",
        );

        let _ = press(&mut state, 'N');
        assert_eq!(heads_of(&state).0, ["bob:main"]);
        type_text(&mut state, "fix");
        let cmd = press_code(&mut state, KeyCode::Enter);

        let Cmd::CreatePr(input) = cmd else {
            panic!("CreatePr が出ていない: {cmd:?}");
        };
        assert_eq!(input.head, "bob:main");

        // **ブランチでない `merge`**（`refs/pull/<n>/head`）は候補にしない。
        let mut state = state_for_create("bob/main");
        set_copies(&mut state, &[]);
        track(
            &mut state,
            "git@github.com:bob/mewgit.git",
            "refs/pull/5/head",
        );
        let _ = press(&mut state, 'N');
        assert!(
            state.modal.is_none(),
            "ブランチでない merge から候補を組んだ"
        );
    }

    #[test]
    fn left_and_right_pick_another_head_only_in_the_head_field() {
        let mut state = state_for_create("feat/x");
        state.repo.remotes = remotes_of(&[
            ("fork", "git@github.com:me/mewgit.git"),
            ("origin", "git@github.com:yshrsmz/mewgit.git"),
        ]);
        set_copies(&mut state, &[("fork", true), ("origin", true)]);
        let _ = press(&mut state, 'N');
        type_text(&mut state, "fix");

        // **title の欄では文字カーソルの移動**で、候補は動かない。
        let _ = press_code(&mut state, KeyCode::Right);
        assert_eq!(heads_of(&state).1, 0, "title の欄で候補が動いた");

        for _ in 0..3 {
            let _ = press_code(&mut state, KeyCode::Tab);
        }
        assert_eq!(modal_of(&state).4, PrField::Head);
        let _ = press_code(&mut state, KeyCode::Right);
        assert_eq!(heads_of(&state).1, 1);
        // **端で止まる**（一覧のカーソルと同じ `stepped_index`）。
        let _ = press_code(&mut state, KeyCode::Right);
        assert_eq!(heads_of(&state).1, 1, "端を越えた");
        let _ = press_code(&mut state, KeyCode::Left);
        assert_eq!(heads_of(&state).1, 0);
        let _ = press_code(&mut state, KeyCode::Right);

        let cmd = press_code(&mut state, KeyCode::Enter);

        let Cmd::CreatePr(input) = cmd else {
            panic!("CreatePr が出ていない: {cmd:?}");
        };
        // **選んだ側を送る。** `target` と同じ owner なのでブランチ名だけ。
        assert_eq!(input.head, "feat/x");
    }

    #[test]
    fn candidates_that_changed_while_the_box_was_open_are_shown_again_before_sending() {
        // **写しを送らない**（`pr_create_submit`）。開いているあいだに `f` で
        // `RepoInfo` が読み直されると候補が変わる。
        let mut state = state_for_create("feat/x");
        state.repo.remotes = remotes_of(&[
            ("fork", "git@github.com:me/mewgit.git"),
            ("origin", "git@github.com:yshrsmz/mewgit.git"),
        ]);
        set_copies(&mut state, &[("fork", true)]);
        let _ = press(&mut state, 'N');
        type_text(&mut state, "fix");
        assert_eq!(heads_of(&state).0, ["me:feat/x"]);

        // fork の ref が消え（`fetch --prune`）、origin に在った。**`Msg` で届ける**
        // —— 画面の外で読み直された形そのもの。一覧はいまの行を持ち越す
        // （組み直すと `put_on_branch` の値に黙って依存する）。
        let deliver_copies = |state: &mut AppState, copies: &[(&str, bool)]| {
            let mut snapshot = fake_repo_snapshot();
            snapshot.remotes = state.repo.remotes.clone();
            snapshot.branches = relist(state, state.branches.branches().to_vec());
            snapshot.refs = copies_of(&state.branches, copies);
            let _ = update(state, Msg::RepoInfoLoaded(Box::new(Ok(snapshot))));
            assert!(state.github_block().is_none(), "GitHub 側で先に断られる");
        };
        deliver_copies(&mut state, &[("origin", true)]);

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::None, "選んでいない head で送った");
        assert_eq!(heads_of(&state), (vec!["yshrsmz:feat/x".to_string()], 0));
        assert_eq!(modal_of(&state).4, PrField::Head, "欄へ移していない");
        assert_eq!(modal_of(&state).0.text(), "fix", "打った本文が消えた");
        assert_eq!(last_notice(&state), HEADS_CHANGED);
        assert!(cells(HEADS_CHANGED) <= NOTICE_CELLS, "{HEADS_CHANGED}");

        // 見せたあとの `Enter` は通る。
        let cmd = press_code(&mut state, KeyCode::Enter);
        assert!(matches!(cmd, Cmd::CreatePr(_)), "{cmd:?}");

        // **候補が消えきった形**は開く側と同じ文言で、閉じない。
        let mut state = state_for_create("feat/x");
        let _ = press(&mut state, 'N');
        type_text(&mut state, "fix");
        deliver_copies(&mut state, &[]);
        let cmd = press_code(&mut state, KeyCode::Enter);
        assert_eq!(cmd, Cmd::None);
        assert_eq!(heads_of(&state).0, Vec::<String>::new());
        assert_eq!(last_notice(&state), NOT_PUSHED);
    }

    #[test]
    fn the_box_stays_open_while_another_create_is_running() {
        // **`Cmd` が落とされるなら閉じない**（`update::submit`）。閉じると
        // 打った本文ごと消える。
        let mut state = state_for_create("feat/x");
        let _ = press(&mut state, 'N');
        type_text(&mut state, "fix");
        // 1 本目は `update` の出口で発行済み。走らせたままにする。
        let first = press_code(&mut state, KeyCode::Enter);
        assert!(matches!(first, Cmd::CreatePr(_)), "{first:?}");

        let _ = press(&mut state, 'N');
        type_text(&mut state, "two");
        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::None);
        assert!(state.modal.is_some(), "落とされたのに閉じた");
        assert_eq!(modal_of(&state).0.text(), "two", "打った本文が消えた");
        assert!(
            last_notice(&state).contains("PR を作っています"),
            "{state:?}"
        );
    }

    #[test]
    fn n_is_refused_while_the_account_write_runs() {
        // **書き込みを出した時点で `account` が `Resolving` に落ちる**（01 の
        // 「写しの寿命」）ので、走っているあいだに `N` で箱を開いても、確定の時点で
        // 材料が消えることはない —— 開く時点で断られる。
        let mut state = state_for_create("feat/x");
        let _ = press(&mut state, 'A');
        let _ = update(
            &mut state,
            Msg::AccountsListed(Ok(fake_logins(&["hanako"]))),
        );
        let cmd = press_code(&mut state, KeyCode::Enter);
        assert_eq!(
            cmd,
            Cmd::Exec(GitOp::SetAccount {
                login: "hanako".to_string()
            })
        );

        let cmd = press(&mut state, 'N');

        assert_eq!(cmd, Cmd::None);
        assert!(state.modal.is_none(), "書き込みのあいだに箱が開いた");
        assert_eq!(last_notice(&state), GithubBlock::AccountResolving.notice());
    }

    #[test]
    fn a_created_pull_request_is_asked_for_again_and_takes_the_cursor() {
        // **PR 作成は一覧の新しい引き金**（01 の「何を引くか」）—— ref が動かないので
        // 既存の引き金では拾えない。**カーソルも作った行へ** —— `PrState::loaded` は
        // 「いま選んでいる PR の番号」を優先するので、憶えさせないと座らない。
        let mut state = state_for_create("feat/x");
        deliver(&mut state, vec![fake_pull_request(3, "other", "sha3")]);
        assert_eq!(state.prs.selected().map(|pr| pr.number), Some(3));

        let _ = start_create(&mut state);
        let cmd = update(&mut state, Msg::PrCreated(Ok(9)));

        assert!(issued(&cmd, &Cmd::FetchPrList), "引き直していない: {cmd:?}");
        assert!(last_notice(&state).contains("#9"), "{state:?}");

        // 届いた一覧では、いま選んでいた #3 ではなく作った #9 に座る。
        let _ = reply(
            &mut state,
            Cmd::FetchPrList,
            list_msg(vec![
                fake_pull_request(9, "feat/x", "sha9"),
                fake_pull_request(3, "other", "sha3"),
            ]),
        );
        assert_eq!(state.prs.selected().map(|pr| pr.number), Some(9));

        // **1 回きり。** 次の一覧では、いま選んでいる行がそのまま残る。
        state.prs.move_cursor(1);
        assert_eq!(state.prs.selected().map(|pr| pr.number), Some(3));
        deliver(
            &mut state,
            vec![
                fake_pull_request(3, "other", "sha3"),
                fake_pull_request(9, "feat/x", "sha9"),
            ],
        );
        assert_eq!(state.prs.selected().map(|pr| pr.number), Some(3));
    }

    #[test]
    fn a_creation_that_overtakes_a_flying_list_still_shows_the_new_row() {
        // **作成の直後に頼む一覧は `AppState::issue` に落とされうる。** `r` などで
        // `FetchPrList` が飛んでいる最中に POST のほうが先に返ると、`pr_created` の
        // `reload` は同種実行中なので落ちる。そこへ**作成より前に頼まれた一覧**が
        // 届く —— 落とされたことを憶えていないと、その古い一覧が座り先の記憶を
        // 食べて**作った PR が画面に出ないまま**になる。
        let mut state = state_for_create("feat/x");
        deliver(&mut state, vec![fake_pull_request(3, "other", "sha3")]);

        // 一覧を頼んで飛ばしたままにする（`r`）。
        let flying = press(&mut state, 'r');
        assert!(issued(&flying, &Cmd::FetchPrList), "{flying:?}");
        assert!(state.jobs.is_running(JobKind::FetchPrList));

        // POST が先に返る。
        let _ = start_create(&mut state);
        let _ = update(&mut state, Msg::PrCreated(Ok(9)));

        // 作成より前に頼まれた一覧（#9 は入っていない）。
        let cmd = update(
            &mut state,
            list_msg(vec![fake_pull_request(3, "other", "sha3")]),
        );

        // **ここで頼み直す。** 出さないと `f` / `r` / `A` まで誰も引かない。
        assert!(issued(&cmd, &Cmd::FetchPrList), "頼み直していない: {cmd:?}");

        // 頼み直した一覧に #9 が居る。**記憶が残っているので座る。**
        let _ = reply(
            &mut state,
            Cmd::FetchPrList,
            list_msg(vec![
                fake_pull_request(9, "feat/x", "sha9"),
                fake_pull_request(3, "other", "sha3"),
            ]),
        );
        assert_eq!(state.prs.selected().map(|pr| pr.number), Some(9));
    }

    #[test]
    fn a_creation_that_never_shows_up_does_not_keep_asking() {
        // **頼み直すのは 1 回だけ。** 作った PR が即座に閉じられると一覧に永久に
        // 出ないので、当たらないたびに頼み直すと**同じ往復を回し続ける**。
        let mut state = state_for_create("feat/x");
        deliver(&mut state, vec![fake_pull_request(3, "other", "sha3")]);
        let flying = press(&mut state, 'r');
        assert!(issued(&flying, &Cmd::FetchPrList), "{flying:?}");

        let _ = start_create(&mut state);
        let _ = update(&mut state, Msg::PrCreated(Ok(9)));

        // 1 通目（古い）は頼み直しを出す。
        let first = update(
            &mut state,
            list_msg(vec![fake_pull_request(3, "other", "sha3")]),
        );
        assert!(issued(&first, &Cmd::FetchPrList), "{first:?}");

        // 2 通目にも #9 が居ない。**ここでは出さない。**
        let second = reply(
            &mut state,
            Cmd::FetchPrList,
            list_msg(vec![fake_pull_request(3, "other", "sha3")]),
        );
        assert!(
            !issued(&second, &Cmd::FetchPrList),
            "頼み直しを回している: {second:?}"
        );

        // 記憶も落ちている（3 通目で座り直さない）。
        deliver(
            &mut state,
            vec![
                fake_pull_request(3, "other", "sha3"),
                fake_pull_request(9, "feat/x", "sha9"),
            ],
        );
        assert_eq!(state.prs.selected().map(|pr| pr.number), Some(3));
    }

    #[test]
    fn the_box_stays_open_when_the_head_left_the_branch_while_it_was_open() {
        // **確定側の `OFF_BRANCH` の腕。** 開く側は
        // `n_refuses_off_a_branch_and_while_github_is_unavailable` が踏んでいるが、
        // **開いたあとに HEAD が detach する**経路は別 —— 外で `git checkout <sha>`
        // して書き込みが起きると `Msg::RepoInfoLoaded` が detached な HEAD を運ぶ。
        // ここで断らないと `head: ""` を POST してモーダルが閉じ、打った本文が消える。
        let mut state = state_for_create("feat/x");
        let _ = press(&mut state, 'N');
        type_text(&mut state, "fix");

        let mut snapshot = fake_repo_snapshot();
        // detached では HEAD の行が無い（実物では同じ返信が運ぶ）。
        snapshot.branches = fake_branch_list_or(
            vec![fake_branch("feat/x")],
            RowlessHead::Detached(fake_oid(1)),
        );
        snapshot.remotes = remotes_of(&[("origin", "git@github.com:yshrsmz/mewgit.git")]);
        let _ = update(&mut state, Msg::RepoInfoLoaded(Box::new(Ok(snapshot))));
        assert!(
            state.github_block().is_none(),
            "GitHub 側で先に断られては腕を踏めない"
        );

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert_eq!(cmd, Cmd::None, "head の無い PR を頼んだ");
        assert!(state.modal.is_some(), "閉じてしまった");
        assert_eq!(modal_of(&state).0.text(), "fix", "打った本文が消えた");
        assert_eq!(last_notice(&state), OFF_BRANCH);
    }

    #[test]
    fn switching_the_account_drops_the_seat_it_was_holding() {
        // **一覧を捨てるなら、それを指していた記憶も捨てる**（`PrState::forget`）。
        let mut state = state_for_create("feat/x");
        let _ = start_create(&mut state);
        let _ = update(&mut state, Msg::PrCreated(Ok(9)));
        assert!(state.prs.has_unseated_creation(), "憶えていない");

        // `A` から `r`（解決し直し）。**`resolve_again` が `PrState::forget` を通る。**
        state.account = AccountState::Degraded(DegradedReason::GhMissing);
        let _ = press(&mut state, 'A');
        let _ = press(&mut state, 'r');

        assert!(!state.prs.has_unseated_creation(), "記憶が残っている");
    }

    #[test]
    fn a_failed_create_keeps_the_reason_in_the_detail() {
        // **要約行に混ぜない**（01 の「失敗の案内文」）。422 の本文にも 403 の
        // ホスト名にも上限が無いので、端末の幅から末尾が切れる。
        let mut state = state_for_create("feat/x");
        let _ = start_create(&mut state);
        let err = GithubError::Api {
            status: 422,
            body_head: "x".repeat(200),
        };
        let cmd = update(&mut state, Msg::PrCreated(Err(err)));

        assert_eq!(cmd, Cmd::None, "失敗では引き直さない");
        let notice = state.notices.back().expect("通知");
        assert!(cells(&notice.message) <= NOTICE_CELLS, "{}", notice.message);
        assert!(
            notice.detail.as_deref().is_some_and(|d| d.contains("422")),
            "本文が detail に無い: {notice:?}"
        );

        // **ジョブは畳む。** 畳まないと以後の `N` が `AppState::issue` に落とされ続ける。
        let again = start_create(&mut state);
        assert!(matches!(again, Cmd::CreatePr(_)), "{again:?}");
    }

    /// `N` → タイトルを打つ → `Enter`。**返る `Cmd` は発行済み** ——
    /// `update` の出口が `AppState::issue` を通すので、**ここで `issue` を呼ぶと
    /// 2 度目になって落とされる**（`test_util::reply` に渡せないのも同じ理由。
    /// あちらは未発行の `Cmd` を受ける）。返信は `update` に直に届ける。
    fn start_create(state: &mut AppState) -> Cmd {
        let _ = press(state, 'N');
        let _ = press(state, 'x');
        let cmd = press_code(state, KeyCode::Enter);
        assert!(matches!(cmd, Cmd::CreatePr(_)), "{cmd:?}");
        cmd
    }

    #[test]
    fn the_list_is_asked_for_only_once_both_the_account_and_the_target_are_known() {
        // **材料は 2 つとも要る。** `wanted_pr_list()` が偽のあいだは頼まない
        // （01 の「PR ペイン › 何を引くか」）。
        let mut state = state_with_commits(2);
        assert_eq!(reload(&state), Cmd::None, "対象もアカウントも無い");

        state.repo.target = TargetState::Resolved(fake_target());
        assert_eq!(reload(&state), Cmd::None, "アカウントがまだ");

        state.account = AccountState::Resolved(fake_account("taro"));
        assert_eq!(reload(&state), Cmd::FetchPrList);
    }

    #[test]
    fn a_detached_head_still_asks_for_the_list() {
        // **HEAD は材料ではない。** 一覧はブランチに紐づかないので、detached でも
        // unborn でも同じものが出る（#83 の「HEAD のブランチの PR 1 件」との違い）。
        let mut state = state_on_branch("feat/x");
        state.branches.loaded(fake_branch_list_or(
            vec![fake_branch("feat/x")],
            RowlessHead::Detached(fake_oid(1)),
        ));
        assert_eq!(reload(&state), Cmd::FetchPrList);

        state.branches.loaded(fake_branch_list_or(
            Vec::new(),
            RowlessHead::Unborn {
                name: "main".to_string(),
            },
        ));
        assert_eq!(reload(&state), Cmd::FetchPrList);

        // HEAD がまだ届いていなくても頼む。
        state.branches = BranchState::default();
        assert_eq!(reload(&state), Cmd::FetchPrList);
    }

    #[test]
    fn either_arrival_order_of_the_target_and_the_account_asks_for_the_list() {
        // **#63 と同じ形の見張り。** 材料が 2 本の非同期の返信から来るので、
        // どちらが先に届いても同じ答えになることを両方の順序で回す。

        // 対象が先、アカウントが後。
        let mut state = state_with_commits(2);
        state.repo.target = TargetState::Resolved(fake_target());
        let cmd = update(&mut state, Msg::AccountResolved(Ok(fake_account("taro"))));
        assert!(issued(&cmd, &Cmd::FetchPrList), "{cmd:?}");

        // アカウントが先、対象が後。**片方だけでは頼まない。**
        let mut state = state_with_commits(2);
        let cmd = update(&mut state, Msg::AccountResolved(Ok(fake_account("taro"))));
        assert!(
            !issued(&cmd, &Cmd::FetchPrList),
            "対象が無いのに頼んだ: {cmd:?}"
        );
        // 対象が決まって ref が届く（`repo_info_loaded` の側から出る）。
        state.repo.target = TargetState::Resolved(fake_target());
        let cmd = update(
            &mut state,
            Msg::RepoInfoLoaded(Box::new(Ok(RepoSnapshot {
                branches: fake_head_list(fake_branch("feat/x")),
                ..fake_repo_snapshot()
            }))),
        );
        assert!(issued(&cmd, &Cmd::FetchPrList), "{cmd:?}");
    }

    #[test]
    fn the_cursor_starts_on_the_pr_of_the_current_branch() {
        // 起動直後の座り先は「いま居るブランチの PR」（01 の「現ブランチの PR」）。
        let mut state = state_on_branch("feat/x");
        deliver(
            &mut state,
            vec![
                fake_pull_request(90, "other", "othersha"),
                fake_pull_request(82, "feat/x", "headsha"),
            ],
        );

        assert_eq!(state.prs.cursor, 1);
        assert_eq!(state.prs.selected().map(|pr| pr.number), Some(82));
        assert_eq!(state.current_pr().map(|pr| pr.number), Some(82));
    }

    #[test]
    fn the_cursor_stays_on_the_same_pr_when_the_order_changes() {
        // **添字では座り直さない。** 一覧は `sort=updated` なので、commit するたびに
        // 並びが変わる（`PrState::seat`）。
        let mut state = state_on_branch("feat/x");
        deliver(
            &mut state,
            vec![
                fake_pull_request(82, "feat/x", "headsha"),
                fake_pull_request(90, "other", "othersha"),
            ],
        );
        state.focus = Pane::Prs;
        let _ = press(&mut state, 'j');
        assert_eq!(state.prs.selected().map(|pr| pr.number), Some(90));

        // 90 が更新されて先頭に来る。
        deliver(
            &mut state,
            vec![
                fake_pull_request(90, "other", "othersha"),
                fake_pull_request(82, "feat/x", "headsha"),
            ],
        );
        assert_eq!(
            state.prs.selected().map(|pr| pr.number),
            Some(90),
            "添字のまま座っている"
        );
    }

    #[test]
    fn a_pr_that_left_the_list_puts_the_cursor_back_at_the_top() {
        // マージされると open の一覧から消える（01 の「現ブランチの PR」で受け入れた穴）。
        let mut state = state_on_branch("feat/x");
        deliver(
            &mut state,
            vec![
                fake_pull_request(82, "feat/x", "headsha"),
                fake_pull_request(90, "other", "othersha"),
            ],
        );
        state.focus = Pane::Prs;
        let _ = press(&mut state, 'j');

        deliver(&mut state, vec![fake_pull_request(82, "feat/x", "headsha")]);
        assert_eq!(state.prs.cursor, 0);
        assert_eq!(state.prs.selected().map(|pr| pr.number), Some(82));
    }

    #[test]
    fn a_fork_branch_matches_the_current_branch_row() {
        // 追跡先の URL が fork を指していれば、そこから出た PR に座る
        // （01 の「現ブランチの PR」）。
        let mut state = state_tracking(
            "feat/x",
            "fork",
            &[
                ("origin", "git@github.com:yshrsmz/mewgit.git"),
                ("fork", "git@github.com:someone-else/mewgit.git"),
            ],
        );
        deliver(
            &mut state,
            vec![
                fake_pull_request(90, "other", "othersha"),
                pr_from(82, "feat/x", "someone-else"),
            ],
        );

        assert_eq!(state.current_pr().map(|pr| pr.number), Some(82));
        assert_eq!(state.prs.cursor, 1);
    }

    #[test]
    fn the_fork_row_wins_over_the_one_from_the_target_repository() {
        // **候補は追跡先が先。** fork へ push しているブランチと同じ名前のブランチが
        // upstream 側にもあるのはよくある形で、そこから出た PR は**他人の PR**
        // （01 の「現ブランチの PR」）。
        let mut state = state_tracking(
            "feat/x",
            "fork",
            &[
                ("origin", "git@github.com:yshrsmz/mewgit.git"),
                ("fork", "git@github.com:someone-else/mewgit.git"),
            ],
        );
        deliver(
            &mut state,
            vec![
                // `fake_pull_request` の head は `target` のリポジトリ。**一覧では
                // こちらが先**なので、順に当てるだけの実装ではこちらに座る。
                fake_pull_request(90, "feat/x", "headsha"),
                pr_from(82, "feat/x", "someone-else"),
            ],
        );

        assert_eq!(state.current_pr().map(|pr| pr.number), Some(82));
        assert_eq!(state.prs.cursor, 1);
    }

    #[test]
    fn the_target_row_is_the_fallback_when_the_tracked_repository_has_none() {
        // **追跡設定は「どこへ push したか」ではない。** `git checkout -b fix bob/fix`
        // のあと origin へ push しても `branch.fix.remote` は `bob` のまま（実測。
        // 表は 01）。**そこで打ち切ると、main では座っていた行に座らなくなる。**
        let mut state = state_tracking(
            "feat/x",
            "bob",
            &[
                ("origin", "git@github.com:yshrsmz/mewgit.git"),
                ("bob", "git@github.com:bob/mewgit.git"),
            ],
        );
        deliver(
            &mut state,
            vec![
                fake_pull_request(90, "other", "othersha"),
                fake_pull_request(82, "feat/x", "headsha"),
            ],
        );

        assert_eq!(state.current_pr().map(|pr| pr.number), Some(82));
        assert_eq!(state.prs.cursor, 1);
    }

    #[test]
    fn a_url_in_the_tracking_config_names_the_fork() {
        // **`gh pr checkout` が書くのは URL**（実測 2026-09-20、gh 2.101.0）。
        // リモートとして登録されていないので、名前では引けない。
        let mut state = state_tracking(
            "feat/x",
            "https://github.com/someone-else/mewgit.git",
            &[("origin", "git@github.com:yshrsmz/mewgit.git")],
        );
        deliver(
            &mut state,
            vec![
                fake_pull_request(90, "other", "othersha"),
                pr_from(82, "feat/x", "someone-else"),
            ],
        );

        assert_eq!(state.current_pr().map(|pr| pr.number), Some(82));
        assert_eq!(state.prs.cursor, 1);
    }

    #[test]
    fn a_checked_out_fork_branch_matches_under_the_name_it_has_on_the_fork() {
        // **取り出しはローカルを `bob/main` にし、`merge` に `refs/heads/main` を書く**
        // （05 の「ブランチ名」）。ローカルの名前で比べると座らず、**`N` の「既に
        // あります」もここを読む**ので、作り直しに行く。
        let mut state = state_for_create("bob/main");
        track(
            &mut state,
            "git@github.com:bob/mewgit.git",
            "refs/heads/main",
        );
        deliver(
            &mut state,
            vec![
                fake_pull_request(90, "other", "othersha"),
                pr_from(82, "main", "bob"),
            ],
        );

        assert_eq!(state.current_pr().map(|pr| pr.number), Some(82));
        assert_eq!(state.prs.cursor, 1);

        let cmd = press(&mut state, 'N');
        assert_eq!(cmd, Cmd::None);
        assert!(
            state.modal.is_none(),
            "取り出した PR をもう一度作りに行った"
        );
        assert_eq!(
            last_notice(&state),
            "このブランチの PR は既にあります（#82）"
        );
    }

    #[test]
    fn the_tracking_config_is_read_from_the_head_row() {
        // **追跡設定は一覧の HEAD の行から読む**（01 の「現ブランチの PR」）。HEAD でない行を
        // 一覧の先頭に置き、別の fork の URL を追跡させる —— `put_on_branch` の一覧は
        // HEAD の 1 行だけなので、先頭の行や選択の行を読む実装でも答えが同じになり、
        // 見張りが空振りする。
        let mut state = state_for_create("bob/main");
        set_copies(&mut state, &[]);
        track(
            &mut state,
            "git@github.com:bob/mewgit.git",
            "refs/heads/main",
        );
        let other = tracking_branch(
            "alice/main",
            "git@github.com:alice/mewgit.git",
            "refs/heads/main",
        );
        edit_rows(&mut state, |rows| rows.insert(0, other));
        state.branches.cursor = 0;

        deliver(
            &mut state,
            vec![pr_from(90, "main", "alice"), pr_from(82, "main", "bob")],
        );
        assert_eq!(state.current_pr().map(|pr| pr.number), Some(82));
        assert_eq!(
            state.head_candidates(),
            [PrHead {
                owner: "bob".to_string(),
                branch: "main".to_string(),
            }]
        );

        // **名前の追跡先と ref の候補も見る。** URL の腕は `url_tracked` が HEAD の行を
        // 引き直すので、`head_homes` / `head_candidates` が名前を別の行から取っても
        // 上では答えが変わらない。こちらは名前（ローカルの名前）で当てる腕。
        let mut state = state_tracking(
            "feat",
            "fork",
            &[
                ("fork", "git@github.com:bob/mewgit.git"),
                ("origin", "git@github.com:yshrsmz/mewgit.git"),
            ],
        );
        set_copies(&mut state, &[("fork", true)]);
        edit_rows(&mut state, |rows| rows.insert(0, fake_branch("alpha")));
        state.branches.cursor = 0;

        deliver(&mut state, vec![pr_from(82, "feat", "bob")]);
        assert_eq!(state.current_pr().map(|pr| pr.number), Some(82));
        assert_eq!(
            state.head_candidates(),
            [PrHead {
                owner: "bob".to_string(),
                branch: "feat".to_string(),
            }]
        );
    }

    #[test]
    fn a_named_tracking_config_is_matched_under_the_local_name() {
        // **名前の追跡先の `merge` は「何を基にしたか」**（01 の「現ブランチの PR」）。
        // `git checkout -b feat upstream/main` は `merge` を `refs/heads/main` にする
        // ので、そこで `merge` の名前を使うと upstream の `main` から出た PR に座り、
        // `N` がそれを「既にある」として断る。
        let mut state = state_tracking(
            "feat",
            "upstream",
            &[
                ("origin", "git@github.com:me/mewgit.git"),
                ("upstream", "git@github.com:yshrsmz/mewgit.git"),
            ],
        );
        track(&mut state, "upstream", "refs/heads/main");
        deliver(&mut state, vec![fake_pull_request(90, "main", "othersha")]);
        assert_eq!(state.current_pr(), None, "基にしたブランチの PR に座った");

        // **肯定まで 1 本で見る** —— 否定だけだと、何も当てない実装で通る。
        deliver(
            &mut state,
            vec![
                fake_pull_request(90, "main", "othersha"),
                fake_pull_request(82, "feat", "headsha"),
            ],
        );
        assert_eq!(state.current_pr().map(|pr| pr.number), Some(82));
    }

    #[test]
    fn a_fork_pr_does_not_match_without_the_tracking_config() {
        // 追跡設定が無ければ出どころを知る手段が無いので、`target` と比べるしかない
        // （`head_homes` の候補が `target` だけになる）。**カーソルは先頭に座るだけ**で、
        // PR は一覧に出ている。
        let mut state = state_on_branch("feat/x");
        deliver(
            &mut state,
            vec![
                fake_pull_request(90, "other", "othersha"),
                pr_from(82, "feat/x", "someone-else"),
            ],
        );

        assert_eq!(state.current_pr(), None, "fork の PR に合致している");
        assert_eq!(state.prs.cursor, 0);
        assert_eq!(state.prs.list.len(), 2, "消えている");
    }

    #[test]
    fn a_tracked_remote_that_is_not_registered_falls_back_to_the_target() {
        // `branch.<HEAD>.remote` に書かれた名前がリモート一覧に無い（`.` の
        // ローカル追跡もここに落ちる）。**`target` との照合は生きている。**
        //
        // **`target` でないリモートを先頭に置く。** 1 本目で済ませる実装（名前で
        // 引かない）でも、`origin` しか無ければ答えが同じになって見張りが空振りする。
        let mut state = state_tracking(
            "feat/x",
            ".",
            &[
                ("backup", "git@github.com:someone-else/mewgit.git"),
                ("origin", "git@github.com:yshrsmz/mewgit.git"),
            ],
        );
        deliver(&mut state, vec![fake_pull_request(82, "feat/x", "headsha")]);

        assert_eq!(state.current_pr().map(|pr| pr.number), Some(82));
    }

    #[test]
    fn a_tracked_url_that_is_not_github_falls_back_to_the_target() {
        // GitHub の形として読めない URL（ローカルパス）。`RemoteUrl::parse` が拒む。
        let mut state = state_tracking(
            "feat/x",
            "backup",
            &[
                ("origin", "git@github.com:yshrsmz/mewgit.git"),
                ("backup", "/srv/git/mewgit.git"),
            ],
        );
        deliver(&mut state, vec![fake_pull_request(82, "feat/x", "headsha")]);

        assert_eq!(state.current_pr().map(|pr| pr.number), Some(82));
    }

    #[test]
    fn an_empty_list_is_not_the_same_as_an_unfetched_one() {
        // **`Fetch` の 3 状態**（01）。空の `Vec` は「開いている PR が 1 件も無い」。
        let mut state = state_on_branch("feat/x");
        assert_eq!(state.prs.load(), &PrLoad::Pending);
        assert!(state.prs.list.is_empty());

        deliver(&mut state, Vec::new());
        assert_eq!(state.prs.load(), &PrLoad::Loaded);
        assert!(state.prs.list.is_empty());
    }

    #[test]
    fn a_failed_fetch_is_remembered_and_the_material_that_changed_asks_again() {
        // **失敗を状態に持つ**（01 の「失敗の記憶」）。
        let mut state = state_on_branch("feat/x");
        deliver(&mut state, vec![fake_pull_request(82, "feat/x", "headsha")]);

        let ask = reload(&state);
        let next = reply(
            &mut state,
            ask,
            Msg::PrListLoaded(Err(GithubError::NotFound)),
        );
        // **自分では頼み直さない。** 回すと「失敗 → 再読込 → 失敗」になる
        // （01 の「失敗の記憶」）。
        assert_eq!(next, Cmd::None, "失敗を頼み直している");
        assert!(matches!(state.prs.load(), PrLoad::Failed(_)));
        // **読めていた一覧は捨てない**（`PrState::failed`）。捨てると、commit 直後の
        // 引き直しが 1 回転けただけでペインがエラー 1 行になる。
        assert_eq!(state.prs.list.len(), 1, "読めていた一覧を捨てている");

        // **出口は材料が変わったとき。** `f`（ref）でも `A`（アカウント）でも
        // 引き直せる（`reload` の doc）。
        assert_eq!(reload(&state), Cmd::FetchPrList);
    }

    #[test]
    fn picking_another_account_asks_for_the_list_again() {
        // **前のトークンで引けなかった答えを残さない。** `A` の流れからは、そこを
        // 抜ける手掛かりが 1 つも無い（独立したレビューが #83 で拾った形）。
        let mut state = state_on_branch("feat/x");
        let ask = reload(&state);
        let _ = reply(
            &mut state,
            ask,
            Msg::PrListLoaded(Err(GithubError::NotFound)),
        );
        assert!(matches!(state.prs.load(), PrLoad::Failed(_)));

        // `A` → 選び直し → `GitOp::SetAccount` → `resolve_again` → 解決し直し。
        let cmd = update(&mut state, Msg::AccountResolved(Ok(fake_account("hanako"))));
        assert!(
            issued(&cmd, &Cmd::FetchPrList),
            "アカウントが変わったのに頼み直していない: {cmd:?}"
        );
    }

    #[test]
    fn a_fetch_overtaken_by_a_write_is_asked_again() {
        // **鍵が無くても古いことがある。** 書き込みの最中に飛んでいた要求の答えは
        // 書き込み**前**の GitHub の状態で、そのあとの引き直しは `AppState::issue` に
        // 落とされている（通知も出ない）。刻印で拾う（01 の「実行中ジョブ › 世代」）。
        let mut state = state_on_branch("feat/x");
        // 1 本目が飛んでいる。**返信はまだ届けない。**
        let _ = state.issue(reload(&state));
        assert!(state.jobs.is_running(JobKind::FetchPrList));

        // 書き込みが終わって ref が動いた。ここで出る `Cmd::FetchPrList` は落とされる。
        state.jobs.advance(Region::Refs);
        let dropped = state.issue(reload(&state));
        assert_eq!(dropped, Cmd::None, "落とされていない");

        // 1 本目の答えが届く。**採ったうえで頼み直す。**
        let next = update(
            &mut state,
            list_msg(vec![fake_pull_request(82, "feat/x", "headsha")]),
        );

        assert!(state.current_pr().is_some(), "出せるものを捨てている");
        // **`==` で見ない。** 同じ出口が集計の扇形も出すので（`fan_out`）、
        // 一覧の頼み直しはその中に居る。
        assert!(
            issued(&next, &Cmd::FetchPrList),
            "追いついていない: {next:?}"
        );
    }

    #[test]
    fn nothing_is_asked_for_until_the_prs_tab_is_opened() {
        // **既定のタブは Branches。** 門が無いと、PRs を一度も開かない起動でも
        // 一覧の行数ぶんの往復を払う（数は 05 の「扇形は毎回引き直さない」。
        // 01 の「PR ペイン › 集計」）。
        let mut state = state_on_branch("feat/x");
        let list = vec![
            fake_pull_request(82, "feat/x", "sha82"),
            fake_pull_request(83, "other", "sha83"),
        ];

        let quiet = fan_out_after(&mut state, list);
        assert_eq!(quiet, Cmd::None, "タブを開く前に引いた: {quiet:?}");
        assert!(state.prs.detail(82).is_none(), "集計が入っている");

        // タブに入った回の引き金（理由は `fan_out` の doc）。
        let opened = tab_to(&mut state, Pane::Prs);
        assert!(
            issued(
                &opened,
                &Cmd::FetchPrDetails {
                    epoch: 0,
                    targets: vec![(82, "sha82".to_string()), (83, "sha83".to_string())],
                }
            ),
            "タブを開いても引かない: {opened:?}"
        );
    }

    #[test]
    fn the_tab_keeps_fetching_while_the_focus_is_back_on_the_graph() {
        // **門が見るのは「タブが見えているか」で、フォーカスの居場所ではない。**
        // `Esc` で Graph に戻ってもタブは残る（01 の「右上のタブ」）ので、画面に
        // 出ている行は引き続き埋まる —— `focus == Pane::Prs` で門にすると、
        // ペインを見ているのに `…` が埋まらない状態が出る。
        let mut state = state_with_one_summary("feat/x");

        let _ = press_code(&mut state, KeyCode::Esc);
        assert_eq!(state.focus, Pane::Graph);
        assert_eq!(state.right_tab, RightTab::Prs, "タブまで戻っている");

        let grown = fan_out_after(
            &mut state,
            vec![
                fake_pull_request(82, "feat/x", "sha82"),
                fake_pull_request(83, "other", "sha83"),
            ],
        );
        assert_eq!(
            grown,
            Cmd::FetchPrDetails {
                epoch: 0,
                targets: vec![(83, "sha83".to_string())],
            },
            "見えている行を引かない"
        );
    }

    #[test]
    fn leaving_the_prs_tab_stops_the_fan_out_and_coming_back_picks_the_rows_up() {
        // **latch にしない判断（01 の「PR ペイン › 集計」）を踏む腕。** 離れている
        // あいだは引かず、戻ってきたら増えた行を拾う。
        let mut state = state_with_one_summary("feat/x");

        let _ = tab_to(&mut state, Pane::Branches);
        let grown = vec![
            fake_pull_request(82, "feat/x", "sha82"),
            fake_pull_request(83, "other", "sha83"),
        ];
        let quiet = fan_out_after(&mut state, grown);
        assert_eq!(quiet, Cmd::None, "離れているあいだに引いた: {quiet:?}");

        let back = tab_to(&mut state, Pane::Prs);
        assert!(
            issued(
                &back,
                &Cmd::FetchPrDetails {
                    epoch: 0,
                    targets: vec![(83, "sha83".to_string())],
                }
            ),
            "戻っても増えた行を拾わない: {back:?}"
        );
        // **持っている行は引き直さない。** 門を通ったからといって全件に戻らない。
        assert_eq!(*state.prs.detail(82).unwrap(), Ok(detail(2)));
    }

    #[test]
    fn the_fan_out_asks_only_for_the_rows_that_have_no_summary() {
        // **1 件ぶんの往復が PR の数だけ積む**ので、毎回全件引くと `core` の予算に
        // 対して重すぎる（数は 05 の「扇形は毎回引き直さない」）。
        //
        // **順序の片方（タブが先 → 一覧が後）を踏むのもここ。** 逆
        // （一覧が先 → タブが後）は `nothing_is_asked_for_until_the_prs_tab_is_opened`
        // にある（`.claude/rules/rust-coding.md` の「非同期の返信が 2 本あるならテストは両方の順序を
        // 回す」の、片方がキーの版）。**専用の 1 本は置かない** —— 置いてみたが、
        // こちらと別に単独で落ちる変異が無かった（実測）。
        let mut state = state_on_prs_tab("feat/x");
        let list = vec![
            fake_pull_request(82, "feat/x", "sha82"),
            fake_pull_request(83, "other", "sha83"),
        ];

        let ask = fan_out_after(&mut state, list.clone());
        assert_eq!(
            ask,
            Cmd::FetchPrDetails {
                epoch: 0,
                targets: vec![(82, "sha82".to_string()), (83, "sha83".to_string())],
            },
            "初回は全件"
        );

        let next = deliver_pr_details(
            &mut state,
            ask,
            &[(82, "sha82", Ok(detail(2))), (83, "sha83", Ok(detail(1)))],
            None,
        );
        assert_eq!(*state.prs.detail(82).unwrap(), Ok(detail(2)));
        // **終端で頼み直さないことをここで見る。** 下の `fan_out_after` だけでは
        // 足りない —— そちらが見るのは `update` の出口を通ったあとの `Cmd` で、
        // **終端が既に同じジョブを起こしていると `AppState::issue` が落とす**ので、
        // 引き直していても `Cmd::None` になる（変異で実際に素通りした）。
        assert_eq!(next, Cmd::None, "終端が引き直している: {next:?}");

        // **同じ一覧がもう一度届いても頼み直さない。**
        let again = fan_out_after(&mut state, list);
        assert_eq!(again, Cmd::None, "持っている行を引き直した: {again:?}");
    }

    #[test]
    fn a_summary_whose_head_moved_is_dropped_and_asked_again() {
        // **チェックはコミットに紐づく。** push で head が進んだ PR に前のコミットの
        // 結果を出したままにしない。
        let mut state = state_on_prs_tab("feat/x");
        let ask = fan_out_after(&mut state, vec![fake_pull_request(82, "feat/x", "old")]);
        let _ = deliver_pr_details(&mut state, ask, &[(82, "old", Ok(detail(2)))], None);
        assert!(state.prs.detail(82).is_some());

        // push して head が進んだ一覧が届く。
        let ask = fan_out_after(&mut state, vec![fake_pull_request(82, "feat/x", "new")]);
        assert!(
            state.prs.detail(82).is_none(),
            "古い sha の集計が残っている"
        );
        assert_eq!(
            ask,
            Cmd::FetchPrDetails {
                epoch: 0,
                targets: vec![(82, "new".to_string())],
            }
        );
    }

    #[test]
    fn an_answer_for_a_head_that_already_moved_is_not_taken() {
        // **飛んでいるあいだに一覧が入れ替わることがある。** 鍵は `(番号, sha)` で、
        // 世代は見ない（`JobKind::region` にその理由がある）。
        let mut state = state_on_prs_tab("feat/x");
        let ask = fan_out_after(&mut state, vec![fake_pull_request(82, "feat/x", "new")]);
        // 答えだけが古い sha で返ってくる。
        let _ = deliver_pr_details(&mut state, ask, &[(82, "old", Ok(detail(9)))], None);
        assert!(
            state.prs.detail(82).is_none(),
            "前のコミットの結果を採っている"
        );

        // 一覧から消えた PR の答えも採らない。**`ask` は終端が返した続きを使う**
        // —— そこで既に頼み直しているので、一覧を届け直すと `AppState::issue` に
        // 落とされて `Cmd::None` が返る。
        let again = fan_out_after(&mut state, vec![fake_pull_request(82, "feat/x", "new")]);
        assert_eq!(again, Cmd::None, "ジョブが走っているのに二重に出た");
        let ask = fetch_pr_details(&state, &[(99, "sha99", Ok(detail(1)))]);
        let _ = deliver_pr_details(&mut state, ask, &[(99, "sha99", Ok(detail(1)))], None);
        assert!(state.prs.detail(99).is_none(), "居ない行の集計が溜まった");
    }

    #[test]
    fn an_aborted_fan_out_fills_the_rest_says_why_and_does_not_loop() {
        // **打ち切ったら残りを埋める。** 埋めないと同じ失敗を買い直す輪になる
        // （01 の「`Err` から再発行してよいのは対象が違うときだけ」）。
        let mut state = state_on_prs_tab("feat/x");
        let list = vec![
            fake_pull_request(82, "feat/x", "sha82"),
            fake_pull_request(83, "other", "sha83"),
        ];
        let ask = fan_out_after(&mut state, list.clone());
        let limited = GithubError::RateLimited {
            reset_at: Some(1_789_703_295),
        };
        let next = deliver_pr_details(
            &mut state,
            ask,
            &[(82, "sha82", Err(limited.clone()))],
            Some(limited.clone()),
        );

        assert_eq!(*state.prs.detail(82).unwrap(), Err(limited.clone()));
        assert_eq!(
            *state.prs.detail(83).unwrap(),
            Err(limited.clone()),
            "頼まなかった行が埋まっていない"
        );
        // **理由を出す経路はここだけ。** 行に出せるのは「引けなかった」の印 1 つで、
        // レート制限は待てば直るものなので、出さないと行き止まりになる。
        let notice = state.notices.back().expect("通知");
        assert_eq!(notice.level, NoticeLevel::Warn);
        assert!(notice.message.contains("中断"), "{}", notice.message);
        // **理由は要約行ではなく `detail` に入る**（上限の無いホスト名を要約行に
        // 混ぜると予算を超える）。
        assert!(
            notice
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("レート制限"),
            "{:?}",
            notice.detail
        );
        // **ステータスバーの予算**（01 の「失敗の案内文」）。**見張りは複数あって、
        // 新しい産出元を自分では拾わない**（`.claude/rules/testing.md`）—— `exec_advice` も `A` も
        // `o` も別々に見ているので、通知を出す場所を足したらここにも足す。
        // **本文に `GithubError` の `Display` が乗る**ので、腕を足したときに超えうる。
        assert!(
            cells(&notice.message) <= NOTICE_CELLS,
            "{} セル: {}",
            cells(&notice.message),
            notice.message
        );

        // **終端では頼み直さない。** ここで出すと「失敗 → 再取得 → 失敗」の輪になる。
        assert_eq!(next, Cmd::None, "終端が頼み直している: {next:?}");

        // **一覧を引き直せば出口がある。** `f` や書き込みのあとに届く一覧で
        // 引けなかった記憶は落ち、もう一度頼む —— 落ちないと、回線が 1 回切れた
        // だけでセッション中ずっと `!` のままになる。
        let again = fan_out_after(&mut state, list);
        assert_eq!(
            again,
            Cmd::FetchPrDetails {
                epoch: 0,
                targets: vec![(82, "sha82".to_string()), (83, "sha83".to_string())],
            },
            "失敗からの出口が無い"
        );
    }

    #[test]
    fn an_abort_does_not_poison_rows_it_never_asked_for() {
        // **埋める相手は頼んだ対象で、いまの一覧ではない。** 扇形が走っているあいだに
        // 増えた行は `AppState::issue` に落とされて頼まれていないので、そこへ理由を
        // 書くと次の `fan_out` も空になり、**一度も叩いていない行が二度と引かれない**。
        let mut state = state_on_prs_tab("feat/x");
        let ask = fan_out_after(&mut state, vec![fake_pull_request(82, "feat/x", "sha82")]);

        // 走っているあいだに commit などで一覧が増える。**この `Cmd` は落とされる。**
        let grown = vec![
            fake_pull_request(82, "feat/x", "sha82"),
            fake_pull_request(91, "other", "sha91"),
        ];
        let dropped = fan_out_after(&mut state, grown);
        assert_eq!(dropped, Cmd::None, "ジョブが走っているのに二重に出た");

        let limited = GithubError::RateLimited { reset_at: None };
        let next = deliver_pr_details(&mut state, ask, &[], Some(limited.clone()));

        assert_eq!(*state.prs.detail(82).unwrap(), Err(limited));
        assert!(
            state.prs.detail(91).is_none(),
            "頼んでいない行を塞いだ: {:?}",
            state.prs.detail(91)
        );
        // 続きは終端が数え直す。
        assert_eq!(
            next,
            Cmd::FetchPrDetails {
                epoch: 0,
                targets: vec![(91, "sha91".to_string())],
            }
        );
    }

    #[test]
    fn an_abort_from_the_previous_account_does_not_touch_the_new_list() {
        // **鍵では足りない。** アカウントを切り替えても番号も head も変わらないので、
        // 前のトークンで飛んでいた扇形の答えが新しい一覧にそのまま当たる ——
        // `PrState::forget` が落とすと言っている守りを、走っている扇形が外から破る。
        let mut state = state_on_prs_tab("feat/x");
        let list = vec![fake_pull_request(82, "feat/x", "sha82")];
        let stale = fan_out_after(&mut state, list.clone());

        // degraded → 別アカウントで解決し直し → 同じ一覧が届く。
        let _ = update(
            &mut state,
            Msg::AccountResolved(Err(DegradedReason::GhMissing)),
        );
        let _ = update(&mut state, Msg::AccountResolved(Ok(fake_account("hanako"))));
        // **新しいぶんは `AppState::issue` に落ちる** —— 前の扇形のジョブがまだ
        // 走っているため。頼み直すのはその終端。
        let dropped = fan_out_after(&mut state, list);
        assert_eq!(dropped, Cmd::None, "ジョブが走っているのに二重に出た");

        // **前のトークンの扇形が、いま届く。**
        let limited = GithubError::RateLimited { reset_at: None };
        let before = state.notices.len();
        let next = deliver_pr_details(
            &mut state,
            stale,
            &[(82, "sha82", Ok(detail(9)))],
            Some(limited),
        );

        assert!(
            state.prs.detail(82).is_none(),
            "前のアカウントの答えが入った: {:?}",
            state.prs.detail(82)
        );
        assert_eq!(state.notices.len(), before, "前のアカウントの理由を出した");
        // **ジョブは畳む**（畳まないと以後の扇形が落ち続ける）ので、新しい
        // アカウントぶんはここで出る。
        assert_eq!(
            next,
            Cmd::FetchPrDetails {
                epoch: state.prs.epoch(),
                targets: vec![(82, "sha82".to_string())],
            },
            "新しいアカウントで頼み直していない"
        );
        assert_ne!(state.prs.epoch(), 0, "世代が進んでいない");
    }

    #[test]
    fn picking_another_account_forgets_the_summaries() {
        // **集計もアカウントに紐づく。** 残すと、次のトークンで引き直した一覧に
        // 前のトークンで見えた集計が番号だけで当たる。
        let mut state = state_with_one_summary("feat/x");
        assert!(state.prs.detail(82).is_some());

        let _ = update(
            &mut state,
            Msg::AccountResolved(Err(DegradedReason::GhMissing)),
        );
        assert!(
            state.prs.detail(82).is_none(),
            "前のアカウントの集計が残った"
        );
    }

    #[test]
    fn the_newer_quota_wins_whichever_order_it_arrives_in() {
        // **`Msg` の届く順は応答の新しさの順ではない。** 一覧と扇形は同時に走り、
        // 扇形の中でも 4 本が並ぶ（05 の `Semaphore`）。あとから届いた古い数で
        // 上書きすると、ヘッダの残量が増えたり減ったりする。
        let mut state = state_on_branch("feat/x");
        let epoch = state.prs.epoch();

        let _ = update(
            &mut state,
            Msg::RateLimitSeen {
                epoch,
                seen: fake_rate_limit(4900, 100),
            },
        );
        let _ = update(
            &mut state,
            Msg::RateLimitSeen {
                epoch,
                seen: fake_rate_limit(4950, 100),
            },
        );
        assert_eq!(
            state.prs.rate_limit(),
            Some(fake_rate_limit(4900, 100)),
            "あとから届いた古い数で上書きした"
        );

        // **窓が変われば残量は戻る。** 比べるのは `(reset, used)` で `reset` が先。
        let _ = update(
            &mut state,
            Msg::RateLimitSeen {
                epoch,
                seen: fake_rate_limit(5000, 200),
            },
        );
        assert_eq!(state.prs.rate_limit(), Some(fake_rate_limit(5000, 200)));
    }

    #[test]
    fn the_quota_of_the_previous_account_is_dropped_and_does_not_come_back() {
        // **残量はそのトークンの予算。** `(reset, used)` は「どちらの応答が新しいか」
        // しか答えないので、アカウントを切り替えたら世代で弾くしかない
        // （#89 の「鍵が答えられる問いを数える」）。
        let mut state = state_on_branch("feat/x");
        let stale = state.prs.epoch();
        let _ = update(
            &mut state,
            Msg::RateLimitSeen {
                epoch: stale,
                seen: fake_rate_limit(4900, 100),
            },
        );
        assert!(state.prs.rate_limit().is_some());

        let _ = update(
            &mut state,
            Msg::AccountResolved(Err(DegradedReason::GhMissing)),
        );
        assert_eq!(
            state.prs.rate_limit(),
            None,
            "前のトークンの残量が残った（ヘッダの `[gh: …]` だけが変わる）"
        );

        let _ = update(&mut state, Msg::AccountResolved(Ok(fake_account("hanako"))));
        // **前のトークンで飛んでいた扇形が、いま返す。**
        let _ = update(
            &mut state,
            Msg::RateLimitSeen {
                epoch: stale,
                seen: fake_rate_limit(10, 100),
            },
        );
        assert_eq!(
            state.prs.rate_limit(),
            None,
            "前のトークンの残量が新しいアカウントに入った"
        );
    }

    /// PR の取り出しの前提（01 の「PR チェックアウト（`Enter`）」）。リモートは
    /// `origin` 1 本（`target` と同じ `yshrsmz/mewgit`）、ローカルのブランチは
    /// `branches`、PRs ペインにフォーカス。**一覧はまだ届けない**（届けるのは各テスト）。
    fn state_for_checkout(branches: &[&str]) -> AppState {
        let mut state = state_on_branch("main");
        state.repo.remotes = remotes_of(&[("origin", "git@github.com:yshrsmz/mewgit.git")]);
        load_branches(&mut state, branches, Some("main"));
        assert_eq!(state.branches.load(), ListLoad::Loaded);
        state.focus = Pane::Prs;
        state
    }

    /// `Enter` が出した取り出しの中身。
    fn checkout_of(cmd: Cmd) -> PrCheckout {
        match cmd {
            Cmd::Exec(GitOp::CheckoutPr(plan)) => *plan,
            other => panic!("取り出しを出していません: {other:?}"),
        }
    }

    #[test]
    fn enter_checks_out_the_pr_under_the_cursor_by_where_its_branch_is() {
        // 同一リポジトリの PR 3 件。いま居る / ローカルに無い / 在る。**いま居るのも
        // 在るのと同じ扱い**（一覧の「いま居る」は外の `git switch` で古くなる。05）。
        // **いま居る行を先頭に置く** —— 起動直後のカーソルは現ブランチの PR に座るので、
        // そこから `j` で下る。
        let mut state = state_for_checkout(&["main", "feat/b"]);
        deliver(
            &mut state,
            vec![
                fake_pull_request(3, "main", "m"),
                fake_pull_request(1, "feat/a", "a"),
                fake_pull_request(2, "feat/b", "b"),
            ],
        );
        let mut plans = Vec::new();
        for _ in 0..3 {
            plans.push(checkout_of(press_code(&mut state, KeyCode::Enter)));
            // 書き込みは同時に 1 つだけ。終わらせないと次の `Enter` は落とされる。
            state.jobs.finish(JobKind::Exec);
            let _ = press(&mut state, 'j');
        }
        // **カーソル行を取り出す** —— 先頭だけ見ると「先頭を取り出す」でも通る。
        let seen: Vec<_> = plans
            .iter()
            .map(|plan| (plan.branch.as_str(), plan.local))
            .collect();
        assert_eq!(
            seen,
            [
                ("main", LocalBranch::Present),
                ("feat/a", LocalBranch::Absent),
                ("feat/b", LocalBranch::Present),
            ]
        );
        for plan in plans {
            assert_eq!(plan.remote, "origin");
            assert_eq!(plan.source, PrSource::SameRepo);
        }
    }

    #[test]
    fn a_fork_pr_is_tracked_at_the_fork_url_in_the_shape_of_the_remote() {
        let mut state = state_for_checkout(&["main"]);
        deliver(&mut state, vec![pr_from(7, "fix", "bob")]);

        let plan = checkout_of(press_code(&mut state, KeyCode::Enter));

        assert_eq!(
            plan,
            PrCheckout {
                remote: "origin".into(),
                branch: "fix".into(),
                source: PrSource::Fork {
                    number: 7,
                    upstream: ForkUpstream {
                        // origin と同じ ssh の形で、owner / repo だけが fork のもの。
                        remote: "git@github.com:bob/mewgit.git".into(),
                        merge: "refs/heads/fix".into(),
                    },
                },
                local: LocalBranch::Absent,
            }
        );
    }

    #[test]
    fn the_same_repo_is_recognised_whatever_the_case_of_its_name() {
        // GitHub の名前は大文字小文字を区別しない。URL の綴りが API と違っても
        // fork 扱いにしない（fork 扱いだと `refs/pull` から取り、URL を追跡先に書く）。
        let mut state = state_for_checkout(&["main"]);
        let mut pr = fake_pull_request(1, "feat", "a");
        pr.head.repo = Some(RepoRef {
            owner: "YSHRSMZ".into(),
            repo: "MewGit".into(),
        });
        deliver(&mut state, vec![pr]);

        let plan = checkout_of(press_code(&mut state, KeyCode::Enter));
        assert_eq!(plan.source, PrSource::SameRepo);
    }

    #[test]
    fn the_current_pr_is_found_whatever_the_case_of_the_repository_name() {
        // 取り出し（`Enter`）と同じ比べ方（`RepoRef::same_as`）。片方だけが綴りの
        // 違いを飲むと、取り出した行に座らない。
        let mut state = state_on_branch("feat");
        let mut pr = fake_pull_request(1, "feat", "a");
        pr.head.repo = Some(RepoRef {
            owner: "YSHRSMZ".into(),
            repo: "MewGit".into(),
        });
        deliver(&mut state, vec![pr]);
        assert_eq!(state.current_pr().map(|pr| pr.number), Some(1));
    }

    #[test]
    fn a_fork_branch_named_like_the_default_or_the_base_gets_the_owner_in_front() {
        // 付けないと fork の `main` でローカルの `main` を早送りする（05 の「ブランチ名」）。
        // 既定ブランチ（`refs/remotes/origin/HEAD`）と base のどちらと重なっても付ける。
        // 既定ブランチを知らない（一度も fetch していない）ときに base で拾えることも見る。
        for (head_branch, pr_branch, base, expected) in [
            (Some("develop"), "develop", "main", "bob/develop"),
            (None, "main", "main", "bob/main"),
            (Some("develop"), "fix", "main", "fix"),
        ] {
            let mut state = state_for_checkout(&["main"]);
            state.repo.remotes[0].head_branch = head_branch.map(str::to_string);
            let mut pr = pr_from(7, pr_branch, "bob");
            pr.base.branch = base.into();
            deliver(&mut state, vec![pr]);

            let plan = checkout_of(press_code(&mut state, KeyCode::Enter));
            assert_eq!(
                plan.branch, expected,
                "{head_branch:?} / {pr_branch} / {base}"
            );
        }
    }

    #[test]
    fn a_deleted_fork_is_tracked_at_the_pull_ref_of_the_remote() {
        // URL を組む相手が無い（gh と同じ形。05 の「追跡先」）。名前が重なるときは
        // owner も分からないので番号を付ける。
        for (branch, expected) in [("fix", "fix"), ("main", "pr-7/main")] {
            let mut state = state_for_checkout(&["main"]);
            let mut pr = pr_from(7, branch, "bob");
            pr.head.repo = None;
            deliver(&mut state, vec![pr]);

            let plan = checkout_of(press_code(&mut state, KeyCode::Enter));
            assert_eq!(plan.branch, expected);
            assert_eq!(
                plan.source,
                PrSource::Fork {
                    number: 7,
                    upstream: ForkUpstream {
                        remote: "origin".into(),
                        merge: "refs/pull/7/head".into(),
                    },
                }
            );
        }
    }

    #[test]
    fn enter_refuses_what_it_cannot_decide_and_says_why() {
        // 一覧が空。**`N` を案内する**（`o` と同じ）。
        let mut state = state_for_checkout(&["main"]);
        assert_eq!(press_code(&mut state, KeyCode::Enter), Cmd::None);
        let message = &state.notices.back().expect("通知が無い").message;
        assert!(message.contains("N で作成"), "{message}");

        // ブランチ一覧の読み直しが失敗した。どれだけ古いかが判らないので「在るか」を
        // 決められない。**`target` が決まっているなら一度は届いている**ので、未着の形は
        // 組めない（リモート一覧と同じ返信。02 の「ブランチ一覧」）。
        let mut state = state_on_branch("main");
        state.repo.remotes = remotes_of(&[("origin", "git@github.com:yshrsmz/mewgit.git")]);
        state.focus = Pane::Prs;
        deliver(&mut state, vec![fake_pull_request(1, "feat", "a")]);
        let _ = deliver_branches(&mut state, Err(GitReadError::ReaderGone));
        assert_eq!(press_code(&mut state, KeyCode::Enter), Cmd::None);
        let message = &state.notices.back().expect("通知が無い").message;
        assert_eq!(message, "リポジトリ情報を読めませんでした");
        // **届いたら取り出せる** —— 断ったのが一覧の有無だったことの裏返し。
        let _ = deliver_branches(&mut state, Ok(fake_head_list(fake_branch("main"))));
        let plan = checkout_of(press_code(&mut state, KeyCode::Enter));
        assert_eq!(plan.branch, "feat");

        // fetch するリモートが決まらない（2 本あって origin が無い）。**診断だけ** ——
        // 「追跡先を設定してください」は取り出しには当たらない。
        let mut state = state_for_checkout(&["main"]);
        state.repo.remotes = remotes_of(&[
            ("a", "git@github.com:x/y.git"),
            ("b", "git@github.com:z/w.git"),
        ]);
        deliver(&mut state, vec![fake_pull_request(1, "feat", "a")]);
        assert_eq!(press_code(&mut state, KeyCode::Enter), Cmd::None);
        let message = &state.notices.back().expect("通知が無い").message;
        assert_eq!(message, "origin がありません（a, b）");
    }

    #[test]
    fn o_opens_the_pr_under_the_cursor_and_says_so_when_the_list_is_empty() {
        let mut state = state_on_branch("feat/x");
        state.focus = Pane::Prs;

        // 引く前は開けない。
        let cmd = press(&mut state, 'o');
        assert_eq!(cmd, Cmd::None);
        assert_eq!(state.notices.len(), 1);
        assert_eq!(state.notices[0].level, NoticeLevel::Info);

        deliver(
            &mut state,
            vec![
                fake_pull_request(82, "feat/x", "headsha"),
                fake_pull_request(90, "other", "othersha"),
            ],
        );
        // **カーソル行を開く。** 起動直後は現ブランチの PR に座っている。
        assert_eq!(press(&mut state, 'o'), Cmd::OpenPr { number: 82 });

        // **動かしてからも押す。** 先頭で押すだけだと「カーソル行を開く」ではなく
        // 「先頭を開く」でも通る。
        state.jobs.finish(JobKind::OpenPr);
        let _ = press(&mut state, 'j');
        assert_eq!(press(&mut state, 'o'), Cmd::OpenPr { number: 90 });
    }

    #[test]
    fn losing_the_account_drops_the_list_it_was_fetched_with() {
        // **印（`*` / `R`）は login で決まる。** 残すと、印だけが黙って消えた
        // 前のアカウントの一覧が出たままになる（`PrState::forget`）。
        // **`Degraded` では `reload` が `Cmd::None` を返す**ので、落とす経路は
        // `account_resolved` の `Err` しか無い。
        let mut state = state_on_branch("feat/x");
        deliver(&mut state, vec![fake_pull_request(82, "feat/x", "headsha")]);
        assert_eq!(state.prs.list.len(), 1);

        let cmd = update(
            &mut state,
            Msg::AccountResolved(Err(DegradedReason::GhMissing)),
        );
        assert_eq!(cmd, Cmd::None, "degraded なのに頼んでいる");
        assert!(
            state.prs.list.is_empty(),
            "前のアカウントの一覧が残っている"
        );
        assert_eq!(state.prs.load(), &PrLoad::Pending);
        assert_eq!(state.prs.cursor, 0);
    }

    #[test]
    fn resolving_again_drops_the_list_until_the_account_comes_back() {
        // `r` → 解決し直しの最中も同じ（`resolve_again`）。**戻ってきたら引き直す**ので、
        // 落としたままにはならない。
        let mut state = state_on_branch("feat/x");
        deliver(&mut state, vec![fake_pull_request(82, "feat/x", "headsha")]);

        let cmd = super::super::account::resolve_again(&mut state);
        assert!(matches!(cmd, Cmd::ResolveAccount(_)));
        assert!(state.prs.list.is_empty(), "解決し直しの最中に残っている");

        let again = update(&mut state, Msg::AccountResolved(Ok(fake_account("taro"))));
        assert!(
            issued(&again, &Cmd::FetchPrList),
            "引き直していない: {again:?}"
        );
    }

    #[test]
    fn saving_another_account_drops_the_list_as_soon_as_the_write_is_issued() {
        // **書き込みを出した時点で `Resolving` に落ちる**（`submit_account_write`）ので、
        // 一覧もそこで落とす —— 終わるまで残すと、ヘッダから `[gh: …]` が消えたまま
        // `*` / `R` の印だけが消えた前のアカウントの一覧が出る。
        let mut state = state_on_branch("feat/x");
        deliver(&mut state, vec![fake_pull_request(82, "feat/x", "headsha")]);
        let _ = press(&mut state, 'A');
        let _ = update(
            &mut state,
            Msg::AccountsListed(Ok(fake_logins(&["hanako"]))),
        );

        let cmd = press_code(&mut state, KeyCode::Enter);

        assert!(
            matches!(cmd, Cmd::Exec(GitOp::SetAccount { .. })),
            "{cmd:?}"
        );
        assert!(state.prs.list.is_empty(), "書き込みの最中に残っている");
    }

    #[test]
    fn pressing_o_while_the_browser_is_starting_says_so_instead_of_nothing() {
        // **`gh browse` は起動したブラウザが終わるまで返らない**（gh 2.100.0 で実測。
        // `BROWSER` に 5 秒眠るスクリプトを置くと 5.06 秒）。端末ブラウザや自分で
        // 抜けないラッパーを `BROWSER` にしている環境では、そのあいだ `o` が
        // 無反応になる（#63 の「無反応よりまし」で止めない）。
        let mut state = state_on_branch("feat/x");
        deliver(&mut state, vec![fake_pull_request(82, "feat/x", "headsha")]);
        state.focus = Pane::Prs;

        // 1 回目は出る（`press` は `update` の出口を通るので登録まで済む）。
        assert_eq!(press(&mut state, 'o'), Cmd::OpenPr { number: 82 });
        assert!(state.jobs.is_running(JobKind::OpenPr));
        state.notices.clear();

        // 2 回目は落とされる。**黙らない。**
        assert_eq!(press(&mut state, 'o'), Cmd::None);
        assert_eq!(state.notices.len(), 1, "黙って落としている");
        let message = state.notices[0].message.clone();
        assert!(message.contains("ブラウザ"), "{message}");
        // ステータスバーの予算（01 の「失敗の案内文」）。
        assert!(
            cells(&message) <= NOTICE_CELLS,
            "{} セル: {message}",
            cells(&message)
        );
    }

    #[test]
    fn a_failed_browse_reaches_the_notices() {
        // **argv と出力を握り潰さない**（不変ルール 4 と同じ趣旨）。
        let mut state = state_on_branch("feat/x");
        let err = crate::auth::AuthError::GhFailed {
            argv: vec!["gh".to_string(), "browse".to_string()],
            message: "exit status 4".to_string(),
        };
        let _ = open_pr_finished(&mut state, Err(err));

        assert_eq!(state.notices.len(), 1);
        assert_eq!(state.notices[0].level, NoticeLevel::Error);
        assert!(
            state.notices[0].message.contains("browse"),
            "{:?}",
            state.notices[0]
        );
        assert!(!state.jobs.is_running(JobKind::OpenPr));
    }

    #[test]
    fn j_and_k_move_the_cursor_within_the_list() {
        let mut state = state_on_branch("feat/x");
        deliver(
            &mut state,
            vec![
                fake_pull_request(82, "feat/x", "headsha"),
                fake_pull_request(90, "other", "othersha"),
            ],
        );
        state.focus = Pane::Prs;
        assert_eq!(state.prs.cursor, 0, "現ブランチの PR に座っている");

        let _ = press(&mut state, 'j');
        assert_eq!(state.prs.cursor, 1);
        // 末尾より先へは進まない。
        let _ = press(&mut state, 'j');
        assert_eq!(state.prs.cursor, 1);
        let _ = press(&mut state, 'k');
        assert_eq!(state.prs.cursor, 0);
    }

    #[test]
    fn a_degraded_account_asks_for_nothing() {
        let mut state = state_on_branch("feat/x");
        state.account = AccountState::Degraded(DegradedReason::GhMissing);
        assert_eq!(reload(&state), Cmd::None);
    }

    #[test]
    fn github_disabled_never_starts_gh_and_drops_the_prs_pane() {
        // 旗を読むのは `AppState::new` の 1 つ（01 の「GitHub 機能を切る」）。
        let default_state = state();
        let mut state = github_disabled_state();
        assert!(matches!(state.account, AccountState::Disabled));
        assert!(!state.focus_candidates().contains(&Pane::Prs));

        // `ssh -G` の結果が来ても `Cmd::ResolveAccount` は出ない。
        let cmd = update(
            &mut state,
            Msg::TargetResolved(Box::new(crate::auth::ResolvedTarget {
                target: fake_target(),
                unnormalized: None,
            })),
        );
        assert_eq!(cmd, Cmd::None);
        assert!(matches!(state.account, AccountState::Disabled));
        // 旗が立っていれば候補に入る（この見張りが空振りしていないことの確認）。
        assert!(default_state.focus_candidates().contains(&Pane::Prs));
    }
    #[test]
    fn r_drops_the_cursor_rows_summary_and_refetches_it_with_the_sha_the_list_brings() {
        // **落とすのはカーソル行だけ**、**`Err` も落とす**、**扇形はその場で頼まない**
        // の 3 つを 1 本で踏む（01 の「引き直す（`r`）」）。カーソル行を `Err` に
        // しておくと、「`Ok` のときだけ落とす」形もここで落ちる。
        let mut state = state_on_prs_tab("feat/x");
        let ask = fan_out_after(
            &mut state,
            vec![
                fake_pull_request(82, "feat/x", "sha82"),
                fake_pull_request(90, "other", "sha90"),
            ],
        );
        let _ = deliver_pr_details(
            &mut state,
            ask,
            &[
                (82, "sha82", Ok(detail(2))),
                (90, "sha90", Err(GithubError::NotFound)),
            ],
            None,
        );
        let _ = press(&mut state, 'j');
        assert_eq!(state.prs.selected().map(|pr| pr.number), Some(90));

        let cmd = press(&mut state, 'r');

        assert!(state.prs.detail(90).is_none(), "カーソル行が残っている");
        assert!(state.prs.detail(82).is_some(), "隣の行まで落としている");
        // **出るのは一覧の 1 本だけ。** ここで扇形も並べると、これから引き直す一覧を
        // 待たずに**手元の sha**で 3 本張ることになる。
        assert_eq!(cmd, Cmd::FetchPrList, "扇形まで出ている: {cmd:?}");

        // 届いた一覧では 90 の head が動いている。**引き直すのはその sha。**
        let again = update(
            &mut state,
            list_msg(vec![
                fake_pull_request(82, "feat/x", "sha82"),
                fake_pull_request(90, "other", "sha90b"),
            ]),
        );
        assert_eq!(
            again,
            Cmd::FetchPrDetails {
                // **世代は進めない**（進めるのは `PrState::forget` だけ）。進めると、
                // 走っている扇形の答えがこのアカウントのものなのに捨てられる。
                epoch: 0,
                targets: vec![(90, "sha90b".to_string())],
            },
            "届いた一覧の sha で引き直していない"
        );
    }

    #[test]
    fn r_pressed_while_the_list_is_in_flight_is_covered_by_the_running_fetch() {
        // **`r` が「押しても何も起きない」に見える 1 通り**（01 の「引き直す（`r`）」）。
        // `AppState::issue` が同種を落とすので `Cmd::None` になるが、走っているほうが
        // 返す一覧の出口が、落とした行を拾う。
        let mut state = state_with_one_summary("feat/x");
        // 1 本目が飛んでいる。**返信はまだ届けない。**
        let _ = state.issue(reload(&state));
        assert!(state.jobs.is_running(JobKind::FetchPrList));

        let dropped = press(&mut state, 'r');
        assert_eq!(dropped, Cmd::None, "落とされていない");
        assert!(state.prs.detail(82).is_none(), "落ちていない");

        // 1 本目の答えが届く。**その出口が記憶の無い行を拾う。**
        let again = update(
            &mut state,
            list_msg(vec![fake_pull_request(82, "feat/x", "sha82")]),
        );
        assert_eq!(
            again,
            Cmd::FetchPrDetails {
                epoch: 0,
                targets: vec![(82, "sha82".to_string())],
            },
            "走っていたほうが拾っていない"
        );
    }

    #[test]
    fn r_during_a_fan_out_asks_with_the_local_sha_and_the_list_corrects_it() {
        // **`refresh` の doc が約束する「届いた一覧の sha で引く」が破れる唯一の順序**
        // （01 の「引き直す（`r`）」）—— 扇形の終端が一覧より先に届くと、
        // `pr_details_finished` の数え直しが**手元の sha**で頼む。
        // **行き止まりにならないことまで見る。**
        let mut state = state_on_prs_tab("feat/x");
        let ask = fan_out_after(
            &mut state,
            vec![
                fake_pull_request(82, "feat/x", "sha82"),
                fake_pull_request(90, "other", "sha90"),
            ],
        );
        // 82 の答えだけ届いた時点で終端はまだ。**カーソルは集計を持っている 82**
        // （答えの飛んでいる行には落とすものが無い —— 扇形の対象は集計を持っていない
        // 行なので、そこで押しても `forget_detail` は空振りする）。
        let (epoch, _) = start_pr_details(&mut state, ask, &[(82, "sha82", Ok(detail(2)))]);
        assert_eq!(state.prs.selected().map(|pr| pr.number), Some(82));
        assert!(state.prs.detail(82).is_some(), "落とす相手が居ない");

        let cmd = press(&mut state, 'r');
        assert_eq!(cmd, Cmd::FetchPrList, "一覧を頼んでいない: {cmd:?}");
        assert!(state.prs.detail(82).is_none(), "落ちていない");

        // 残りの答えと終端が、引き直した一覧より**先に**届く。
        let _ = update(
            &mut state,
            Msg::PrDetailLoaded {
                epoch,
                number: 90,
                sha: "sha90".to_string(),
                result: Ok(detail(3)),
            },
        );
        let recount = update(
            &mut state,
            Msg::PrDetailsFinished {
                epoch,
                aborted: None,
            },
        );
        assert_eq!(
            recount,
            Cmd::FetchPrDetails {
                epoch: 0,
                targets: vec![(82, "sha82".to_string())],
            },
            "数え直しが手元の sha で頼んでいない"
        );

        // その 3 本が飛んでいるあいだに、引き直した一覧が届く。**82 の head が動いている。**
        let (stale_epoch, _) = start_pr_details(&mut state, recount, &[]);
        let after_list = update(
            &mut state,
            list_msg(vec![
                fake_pull_request(82, "feat/x", "sha82b"),
                fake_pull_request(90, "other", "sha90"),
            ]),
        );
        assert_eq!(after_list, Cmd::None, "扇形が走っているのに落ちていない");

        // 古い sha の答えは捨てられ、**次の終端が届いた一覧の sha で頼み直す。**
        let _ = update(
            &mut state,
            Msg::PrDetailLoaded {
                epoch: stale_epoch,
                number: 82,
                sha: "sha82".to_string(),
                result: Ok(detail(9)),
            },
        );
        assert!(
            state.prs.detail(82).is_none(),
            "古い sha の答えを採っている"
        );
        let again = update(
            &mut state,
            Msg::PrDetailsFinished {
                epoch: stale_epoch,
                aborted: None,
            },
        );
        assert_eq!(
            again,
            Cmd::FetchPrDetails {
                epoch: 0,
                targets: vec![(82, "sha82b".to_string())],
            },
            "行き止まりになっている"
        );
    }

    #[test]
    fn r_says_why_it_cannot_refresh_instead_of_doing_nothing() {
        // **押しても何も起きない形にしない**（#63）。**理由は消去法で組み立てない**
        // —— `AppState::github_block` が腕で答える（01 の「引き直す（`r`）」）。
        let mut state = state_with_commits(2);
        state.focus = Pane::Prs;
        state.reseat_focus();
        assert_eq!(state.focus, Pane::Prs, "巡回候補に入っていません");

        // **直近の通知は末尾**（ステータスバーが描くのもそこ）。
        let said = |state: &mut AppState| {
            let cmd = press(state, 'r');
            assert_eq!(cmd, Cmd::None, "材料が無いのに頼んだ: {cmd:?}");
            last_notice(state)
        };

        // まだ決まっていない / 見たが GitHub ではなかった / degraded。
        // **待てば変わるものと変わらないものを潰さない。**
        let resolving = said(&mut state);
        state.repo.target = TargetState::Unavailable;
        let unavailable = said(&mut state);
        state.repo.target = TargetState::Resolved(fake_target());
        assert_eq!(
            said(&mut state),
            resolving,
            "同じ「待てば変わる」が割れている"
        );
        state.account = AccountState::Degraded(DegradedReason::GhMissing);
        let degraded = said(&mut state);

        let all = [&unavailable, &resolving, &degraded];
        for (i, one) in all.iter().enumerate() {
            for other in &all[i + 1..] {
                assert_ne!(one, other, "同じ文言に潰れている: {one:?}");
            }
        }
        // **`A` の通知と同じ字**（予算が同じ 1 行なので分けない。01）。
        assert_eq!(unavailable, GithubBlock::TargetUnavailable.notice());
        assert_eq!(resolving, GithubBlock::AccountResolving.notice());
        // **`A` を案内してよいのは degraded だけ** —— そこにだけ内訳がある。
        assert!(
            degraded.contains('A'),
            "内訳の在りかを出していない: {degraded:?}"
        );
        assert!(
            !resolving.contains('A'),
            "待つだけの状態に操作を案内している"
        );

        // **案内は通知の予算に収める**（01 の「失敗の案内文」）。
        for message in all {
            assert!(
                cells(message) <= NOTICE_CELLS,
                "通知が長すぎます: {message:?}"
            );
        }
    }
}
