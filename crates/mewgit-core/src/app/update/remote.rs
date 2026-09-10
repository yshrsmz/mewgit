//! `f` / `p` / `P` と、既定リモートの決め方（01 の「リモート操作」）。
//!
//! **どのペインからも押せる共通キー**なので、ペインのモジュールには置かない ——
//! アカウントと同じくリポジトリ単位の話で、どのペインに居るかと関係が無い。

use crate::git_exec::{push_remote_ref, ExecError, ExecErrorKind, ExecOutput, GitOp};
use crate::git_read::{short_oid, Remote};

use super::confirm;
use crate::app::{AppState, BranchLoad, Cmd, JobKind, Modal, NoticeLevel, PushRejection};

/// `P`。HEAD のブランチを push する（01 の「push の行き先」）。
///
/// **行き先を推測したときだけ確認モーダルを通す。** 追跡設定があるなら行き先は
/// ユーザーが書いたもので、見せて確かめる相手がいない。
pub(super) fn push(state: &mut AppState) -> Cmd {
    match push_plan(state) {
        PushPlan::Tracked(op) => Cmd::Exec(op),
        PushPlan::Ask { title, body, op } => confirm(state, title, body, op),
        PushPlan::Refused(message) => {
            state.push_notice(NoticeLevel::Warn, message, None);
            Cmd::None
        }
    }
}

/// 完了した push を憶えるか（01 の「force push」）。
///
/// **憶えるのは、次を force に上げられる経路の拒否だけ**（`upgradable`）。上げられ
/// ない経路で憶えても読む側が使えず、`push_plan` が「立っているのに何もできない」印を
/// 持つことになる。上げられないのは 2 つ。
///
/// - force push（`lease: Some`）。lease が古いときの `! [rejected] ... (stale info)`
///   も `rejected` を含むので `NonFastForward` に落ちる（git 2.55 で確認）。立て直すと
///   同じ古い lease で撃ち続ける輪になる
/// - `-u` の提案（`set_upstream: true`）。追跡 ref を持たないので lease の値を作れず、
///   force に上げられるのは `PushPlan::Tracked` だけ（01 の「force push」）。**憶えると、
///   あとから追跡設定が外で入ったときに、追跡していない push の拒否を根拠に force の腕が
///   立つ**
///
/// **それ以外の Push の完了はすべて忘れる**（成功した push、非 FF 以外の失敗）。
///
/// **憶えるのは押したブランチ**（`GitOp::Push` の `src`）で、いまの HEAD ではない。
/// 一覧の読み直しは別のジョブなので、`Switch` の直後に押した push が返るころには
/// HEAD が別のブランチになっていることがある。そこで HEAD を憶えると、**押して
/// いないブランチに force push の腕が立つ**。
///
/// 位置は一覧から取る。push はローカルの ref を動かさないので、名前で引いた先端は
/// 押したときのまま（読み直しは `Reload` が別に出す）。一覧に無ければ憶えない。
pub(super) fn push_rejection(
    state: &AppState,
    result: &Result<ExecOutput, ExecError>,
    src: &str,
    upgradable: bool,
) -> Option<PushRejection> {
    if !upgradable {
        return None;
    }
    match result {
        Err(ExecError::Failed {
            kind: ExecErrorKind::NonFastForward,
            ..
        }) => state
            .branches
            .branches
            .iter()
            .find(|branch| branch.name == src)
            .map(|branch| PushRejection {
                branch: branch.name.clone(),
                oid: branch.oid,
            }),
        _ => None,
    }
}

/// `P` が何をするか。
#[derive(Debug, PartialEq, Eq)]
enum PushPlan {
    /// 追跡設定に沿って push する。確認モーダルを通さない。
    Tracked(GitOp),
    /// 確認モーダルを通す。`-u` の提案（行き先を推測している）と force push
    /// （破壊的。不変ルール 5）の 2 つで、**違うのは文言だけ**なので変種は分けない。
    Ask {
        title: &'static str,
        body: String,
        op: GitOp,
    },
    /// 出せない。通知に出す文言。
    Refused(String),
}

/// HEAD のブランチと登録済みのリモートから push の行き先を決める（01 の
/// 「push の行き先」）。
///
/// **HEAD のブランチは一覧から取る。** `RepoInfo.head` は起動時に読んだきりで、
/// 外部で切り替えられると古くなる（#22 の「キャッシュした状態から破壊的操作を
/// 選ばない」）。一覧は ref を動かす書き込みのたびに読み直される。
///
/// **拒否の出口はここ 1 本。** 「一覧が届いていない」だけを呼び出し側の早期 return に
/// すると、同じ「Warn を出して `Cmd::None`」が 2 箇所に割れる。
///
/// `&AppState` を丸ごと取るのは、**空の `Vec` が「無い」なのか「まだ届いていない」なのか
/// を、ブランチ一覧とリモート一覧の両方で区別する**ため。片方だけ区別すると、起動直後の
/// 一瞬に押した `P` が「リモートが登録されていません」と嘘を言う。
fn push_plan(state: &AppState) -> PushPlan {
    let branches = &state.branches;
    // **空の `Vec` からは「まだ届いていない」と detached / unborn を区別できない**
    // ので `load` を見る（01 の「ブランチ一覧」で `bool` 2 つにしなかったのと同じ）。
    if branches.load() != BranchLoad::Loaded {
        return PushPlan::Refused("ブランチ一覧をまだ読めていません".to_string());
    }
    let Some(head) = branches.head() else {
        return PushPlan::Refused("HEAD がブランチを指していません".to_string());
    };

    // **`upstream`（短縮名）では決めない。** あちらは remote-tracking ref から作るので、
    // `fetch --prune` で ref が消えただけのブランチも `None` になる。そこで `-u` を
    // 提案すると、ユーザーが設定した追跡先を黙って別のリモートへ向け替える（02 の
    // 「ブランチ一覧」がこの `None` を数えている）。
    let Some(config) = &head.upstream_config else {
        // **ここで記憶を読んで断らない。** 断ると `P` が `Push` を出さなくなり、
        // 記憶を落とす経路（`exec_finished`）へ二度と入れない。リモート側の同名
        // ブランチが外で消えても「force push できません」と言い続ける。**`P` は
        // 押し直せる**ままにして、force に上げられない事実は失敗時の案内で伝える
        // （01 の「失敗の案内文」）。
        return propose_upstream(&head.name, state);
    };

    // **`.` はローカル追跡**（`git branch --set-upstream-to=main topic`）。渡すと
    // `git push . topic:main` になり、**ネットワークに出ないままローカルの `main` を
    // 早送りして exit 0 で返る**（git 2.55 で確認）。素の `git push` は
    // `push.default = simple` で断る場面で、[`push_dst`] が組む refspec が
    // その守りを外している。`-u` の提案にも落とさない —— そちらは
    // `branch.<name>.remote` を上書きするので、ユーザーが書いた `.` が黙って消える。
    if config.remote == "." {
        return PushPlan::Refused(NoRemote::Ask(LOCAL_TRACKING.to_string()).into_notice());
    }

    // **直前の `P` が断られたときだけ force に上げる**（01 の「force push」）。
    // 専用のキーを作らないのは、断られていない場面で押せる形にしないため（07）。
    let rejected = state
        .push_rejected
        .as_ref()
        .is_some_and(|rejected| rejected.branch == head.name && rejected.oid == head.oid);
    // **lease の値が無いなら force は撃たない。** 値の無い `--force-with-lease` は
    // remote-tracking ref と比べるだけで守りにならない（03）。`upstream_config` が
    // あってもここが `None` になるのは `fetch --prune` のあと（02）。**判定を
    // 1 箇所に畳む** —— 断りと lease を別々に書くと、片方だけ緩めたときに気付けない。
    let lease = match (rejected, &head.upstream) {
        (true, None) => {
            return PushPlan::Refused(
                "追跡 ref が無いので force push できません。f で取得してください".to_string(),
            )
        }
        // **リモートに無いコミットが 1 つも無いなら、force push は消すだけ。**
        // `↓n` だけのブランチも非 FF で断られる（git 2.55 で確認）ので、断られた記憶は
        // 立つ。そこで `f` を踏むと lease がリモートの位置に追いつき、**通ってしまう**
        // —— 出すものが無いのに他人のコミットを消す（01 の「force push」）。
        (true, Some(_)) if head.ahead == 0 => {
            return PushPlan::Refused(
                "リモートに無いコミットがありません。p で取り込んでください".to_string(),
            )
        }
        (true, Some(upstream)) => Some(upstream.oid),
        (false, _) => None,
    };

    let dst = push_dst(&head.name, &config.merge);
    let body = lease.map(|lease| {
        format!("{} を {} の {} に force push します。\nリモートが {} から動いていれば git が断ります。上書きした履歴は戻せません。", head.name, config.remote, push_remote_ref(&head.name, dst.as_deref()), short_oid(&lease))
    });
    let op = GitOp::Push {
        remote: config.remote.clone(),
        src: head.name.clone(),
        dst,
        set_upstream: false,
        lease,
    };

    match body {
        None => PushPlan::Tracked(op),
        Some(body) => PushPlan::Ask {
            title: "force push",
            body,
            op,
        },
    }
}

/// `f`。既定リモートを決めて fetch する。
///
/// **決まらないときは行き止まりにしない**（01 の「決まらないときの逃げ道」）。
/// 候補が 1 本でもあれば [`Modal::RemotePicker`] を開いて選ばせる —— `Err` になるのは
/// 「黙って選ぶと危ない」形で、**危ないのは mewgit が選ぶこと**だから。候補が 0 本
/// （まだ読めていない / 登録されていない）は選ばせる相手がいないので通知に落とす。
pub(super) fn fetch(state: &mut AppState) -> Cmd {
    let reason = match default_remote(state) {
        Ok(remote) => return fetch_from(&remote.name),
        // **選ばせる相手がいるかは規則の側が既に判じている。** ここで一覧を数え直すと、
        // `conventional_remote` の `[]` の腕と同じ判断が 2 本並ぶ。
        // **`Nobody` は選ばせる相手がいない。** 指示を足して通知に落とす。
        Err(no @ NoRemote::Nobody(_)) => {
            state.push_notice(NoticeLevel::Warn, no.into_notice(), None);
            return Cmd::None;
        }
        // **`Ask` は診断のまま。** 指示はモーダルが提供する操作そのものなので足さない。
        Err(NoRemote::Ask(diagnosis)) => diagnosis,
    };
    state.modal = Some(Modal::RemotePicker {
        reason,
        remotes: state
            .repo
            .remotes
            .iter()
            .map(|remote| remote.name.clone())
            .collect(),
        cursor: 0,
    });
    Cmd::None
}

/// 既定リモートが決まらなかった形（01 の「決まらないときの逃げ道」）。
///
/// **`String` 1 本で返さない。** 「選ばせる相手がいるか」は規則の側でしか判らないのに
/// 文字列だけを返すと、呼ぶ側がリモート一覧を数え直して**同じ判断を 2 本目に書く**
/// ことになる。どちらの腕も案内文を持つのは、行き先（通知 / モーダルの本文）が違うだけで
/// 「なぜ決まらないか」は同じだから。
pub(super) enum NoRemote {
    /// 選ばせる相手がいない（まだ読めていない / 1 本も登録されていない）。通知に落とす。
    Nobody(String),
    /// 候補はあるが、mewgit が選ぶと危ない。**人に選ばせる。**
    Ask(String),
}

impl NoRemote {
    /// 押せるキーが無い場面（`P`）に出す案内。**診断に指示を足す。**
    ///
    /// **指示を診断側へ混ぜない。** `f` は同じ診断を [`Modal::RemotePicker`] の本文に
    /// 出すが、そこでは**すぐ下にリモートが並んでいて選べる** —— 「設定してください」と
    /// 書くと、**その操作を今まさに提供しているモーダルの中で、外でやれと言う**ことに
    /// なる（`/code-review` が拾った。CLAUDE.md の「案内が求める操作を自分が持って
    /// いるかを数える」の裏返し）。指示を足すのは、**足せる操作を持たない側**の仕事。
    fn into_notice(self) -> String {
        match self {
            // **足さない。** 待つか `git remote add` するかで、こちらから出せる
            // 「次に何をするか」が無い（「追跡先を設定してください」はリモートが
            // 1 本も無い相手には嘘になる）。
            NoRemote::Nobody(message) => message,
            NoRemote::Ask(diagnosis) => format!("{diagnosis}。追跡先を設定してください"),
        }
    }
}

/// リモートを明示した fetch。**`--prune` は付けない**（01 の「リモート操作」）。
///
/// **組むのはここ 1 本。** `f` と [`Modal::RemotePicker`] の確定の 2 人が出したので、
/// 別々に書くと片方だけ `--prune` が付く形になる。
pub(super) fn fetch_from(remote: &str) -> Cmd {
    Cmd::Exec(GitOp::Fetch {
        remote: Some(remote.to_string()),
        prune: false,
    })
}

/// `branch.<name>.remote` が `.`（ローカル追跡）だったときの断り文句。
///
/// **`f` と `P` で同じものを出す。** 別々に書くと、片方を書き換えたときに同じ設定へ
/// 別のことを言い始める（`git branch --set-upstream-to=main topic` の 1 つの状態）。
const LOCAL_TRACKING: &str = "追跡先がローカルです";

/// `git fetch`（引数なし）が選ぶリモート（01 と 04 の「既定リモート」）。
///
/// ```text
/// branch.<HEAD>.remote → conventional_remote()
/// ```
///
/// **git 2.55 で実測した規則と同じ**（表は 04）。読み手は `f` だけ。
///
/// **`RepoInfo.target` はこれを呼ばない。** あちらは「このワークツリーはどの GitHub
/// リポジトリか」というリポジトリ単位の問いで、1 段目のブランチ追跡設定を混ぜると
/// **答えが 2 通りに割れる** —— `Msg::BranchesLoaded` と `Msg::RepoInfoLoaded` の
/// どちらが先に届くかで 1 段目を通るかが変わり、しかも `target` は latch されるので
/// 二度と直らない（CLAUDE.md の「ループテストは非同期の返信の順序に依存できない」を
/// production 側で踏む形）。**ブランチを切り替えても `target` は変わらない**という
/// 事実とも噛み合わない。共有するのは [`conventional_remote`] のほうだけ。
///
/// **`propose_upstream`（push の `-u`）もここに畳めない。** git 自身が fetch と push で
/// 違う規則を持っており（push は `branch.<name>.pushRemote` > `remote.pushDefault` >
/// fetch の規則）、畳むと **fetch の既定へ push する**という git がしない挙動になる。
/// あちらが呼ぶのは追跡設定が無いと判った**あと**なので、[`conventional_remote`] でよい。
fn default_remote(state: &AppState) -> Result<&Remote, NoRemote> {
    // **リモート一覧が空のあいだは 1 段目に入らない。** `LoadRepoInfo` と
    // `LoadBranches` は別のジョブなので、ブランチだけ先に届いた瞬間がある。そこで
    // 1 段目を通すと、追跡先が一覧に無いのは当たり前なので「追跡先の origin が
    // リモートにありません」—— **設定が壊れていると言ってしまう**（`/code-review` が
    // 拾った）。空のときの文言は [`conventional_remote`] が 2 通り持っている。
    if state.repo.remotes.is_empty() {
        return conventional_remote(state);
    }

    // **ブランチ一覧がまだなら、この段だけ飛ばす。** 決まらないに落とすと、リモートが
    // 1 本のふつうのリポジトリで起動直後の `f` が断られる（01 の「既定リモート」）。
    // **`f` はキーなので、遅れて届いた一覧は次に押したときに効く。**
    if let Some(config) = state
        .branches
        .head()
        .and_then(|head| head.upstream_config.as_ref())
    {
        // **`.` はローカル追跡。** push が既に断っている形で、fetch に渡すと git が
        // 「そんなリモートは無い」で落ちる。
        if config.remote == "." {
            return Err(NoRemote::Ask(LOCAL_TRACKING.to_string()));
        }
        // **`origin` へ落とさない。** 落とすと、ユーザーが書いた行き先とは別の
        // リモートを黙って fetch することになる。
        return state
            .repo
            .remotes
            .iter()
            .find(|remote| remote.name == config.remote)
            .ok_or_else(|| {
                NoRemote::Ask(format!("追跡先の {} がリモートにありません", config.remote))
            });
    }

    conventional_remote(state)
}

/// リモート集合だけで決まる既定リモート。
///
/// ```text
/// リモートが 1 本ならそれ → origin → 決まらない
/// ```
///
/// **ブランチの状態を読まない。** 読み手が 3 人（`f` / `-u` の提案 /
/// [`RepoInfo::target`](super::RepoInfo::target)）いて、そのうち `target` は 1 回きり
/// 決めて latch するので、**返信の到着順で答えが変わってはいけない**
/// （[`default_remote`] の doc）。
///
/// **80 桁に収める。** ステータスバーは要約行を 1 行で切るので、長いと
/// 「どうすればよいか」の側から消える（M6 で詳細を出すまで）。
pub(super) fn conventional_remote(state: &AppState) -> Result<&Remote, NoRemote> {
    match state.repo.remotes.as_slice() {
        // **`RepoInfo` にも「届いたか」の印は無い。** `LoadRepoInfo` と `LoadBranches` は
        // 別のジョブなので、一覧だけ先に届いた瞬間がある。走っているなら待たせる。
        [] if state.jobs.is_running(JobKind::LoadRepoInfo) => Err(NoRemote::Nobody(
            "リモート一覧をまだ読めていません".to_string(),
        )),
        [] => Err(NoRemote::Nobody("リモートが登録されていません".to_string())),
        [only] => Ok(only),
        many => many
            .iter()
            .find(|remote| remote.name == "origin")
            .ok_or_else(|| {
                let names = many
                    .iter()
                    .map(|remote| remote.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                NoRemote::Ask(format!("origin がありません（{names}）"))
            }),
    }
}

/// 追跡設定の無いブランチに `-u` 付きの push を提案する。
///
/// リモートは [`conventional_remote`] が選ぶ。**適当に選ばない** — fork 運用の
/// リポジトリで他人のリポジトリへ push する形になる。
///
/// **git の規則そのものではない**（git は `branch.<name>.pushRemote` と
/// `remote.pushDefault` も見る）。読むこと自体は `git_read` にならできるが、そこまで
/// 写すのは追跡設定を読む本 PR の範囲を越える。**食い違っても確認モーダルに出た
/// 行き先を見て `n` で止められる**ので、危ないのは「見ずに `y`」だけ。読む判断は
/// 01 の「push の行き先」に送り先を書いた。
fn propose_upstream(branch: &str, state: &AppState) -> PushPlan {
    let candidate = match conventional_remote(state) {
        Ok(remote) => &remote.name,
        // **`Ask` でもモーダルは開かない。** push の行き先を選ばせるのは不変ルール 5 の
        // 手前の判断で、読むだけの fetch と同じ扱いにはできない（01）。
        Err(no) => return PushPlan::Refused(no.into_notice()),
    };

    PushPlan::Ask {
        title: "upstream を設定して push",
        body: format!("{branch} を {candidate} に push し、追跡先に設定します。"),
        op: GitOp::Push {
            remote: candidate.clone(),
            src: branch.to_string(),
            dst: None,
            set_upstream: true,
            lease: None,
        },
    }
}

/// `GitOp::Push` の `dst`（リモート側の ref 名）。`merge` は `branch.<name>.merge`。
///
/// **名前が違うときだけ `Some`。** argv は `<local>:<remote 側>` になる。
/// `push <remote> <local>` が動かすのは remote 側の**同名**の ref なので、`x` が
/// `refs/heads/y` を追跡している状態では `origin/x` を新しく作ってしまう（git の
/// `push.default = simple` がこの形を断るのも同じ理由）。
///
/// **`refs/heads/` の下に無い `merge` は完全な ref のまま渡す。** 短縮できない名前を
/// 短縮したことにすると、別の ref を動かす。`git push <remote> <local>:refs/…` は
/// そのまま通る形式。
fn push_dst(local: &str, merge: &str) -> Option<String> {
    match merge.strip_prefix("refs/heads/") {
        Some(short) if short == local => None,
        Some(short) => Some(short.to_string()),
        None => Some(merge.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        fake_output, fetched_remote, finish_exec, forced_push_op, open_confirm, press, press_code,
        push_op, reject_non_fast_forward, state_for_push, state_with_commits, tracked_head,
        tracking_branch, with_branches, with_remotes, HEADS_MAIN,
    };
    use super::super::update;

    use super::*;
    use crate::app::{Key, KeyCode, Modal, Msg, Pane};

    use crate::git_exec::PullMode;
    use crate::git_read::Branch;

    use crate::test_util::{fake_branch, fake_oid};

    /// ローカル名と違う行き先への push（argv では `<src>:<dst>`）。
    fn renamed_push_op(remote: &str, src: &str, dst: &str) -> GitOp {
        GitOp::Push {
            remote: remote.to_string(),
            src: src.to_string(),
            dst: Some(dst.to_string()),
            set_upstream: false,
            lease: None,
        }
    }

    #[test]
    fn the_remote_keys_work_from_every_pane() {
        // **共通キー**（07 のキー表）。フォーカスの `match` より先に見ないと、
        // あとから足したペインで押せなくなる。
        //
        // `f` は既定リモートを明示し、`p` は選ばない。`--prune` は付けない（01）。
        let expected = [
            (
                'f',
                Cmd::Exec(GitOp::Fetch {
                    remote: Some("origin".to_string()),
                    prune: false,
                }),
            ),
            (
                'p',
                Cmd::Exec(GitOp::Pull {
                    mode: PullMode::FastForwardOnly,
                }),
            ),
            ('P', Cmd::Exec(push_op("origin", "main", false))),
        ];

        for pane in [Pane::Graph, Pane::Worktree, Pane::Branches] {
            for (key, want) in &expected {
                // **キーごとに状態を作り直す。** 1 つの状態で続けて押すと、先に出た
                // `Cmd::Exec` のジョブが走ったままになり、`AppState::issue` が
                // 2 つ目を黙って落とす（見張りが 2 本死ぬ）。
                let mut state =
                    state_for_push(&["origin"], tracking_branch("main", "origin", HEADS_MAIN));
                state.focus = pane;

                assert_eq!(
                    &press(&mut state, *key),
                    want,
                    "{pane:?} で {key} が効いていません"
                );
            }
        }
    }

    // ---- 既定リモート（01 / 04 の「既定リモート」）----

    #[test]
    fn f_fetches_the_tracked_remote_not_origin() {
        // 1 段目。**`origin` があっても追跡先が勝つ。**
        let mut state = state_for_push(
            &["origin", "upstream"],
            tracking_branch("main", "upstream", HEADS_MAIN),
        );
        let cmd = press(&mut state, 'f');
        assert_eq!(fetched_remote(&cmd).as_deref(), Some("upstream"), "{cmd:?}");
    }

    #[test]
    fn f_fetches_the_only_remote_even_when_it_is_not_origin() {
        // 2 段目。**git 2.55 も同じ**（単一リモート `upstream` で `git fetch` が
        // `refs/remotes/upstream/main` を作るのを実測。表は 04）。
        let mut state = state_for_push(&["upstream"], fake_branch("main", true));
        let cmd = press(&mut state, 'f');
        assert_eq!(fetched_remote(&cmd).as_deref(), Some("upstream"), "{cmd:?}");
    }

    /// 開いているリモート選択モーダルの理由と候補。開いていなければ落とす。
    fn open_picker(state: &AppState) -> (&str, &[String], usize) {
        let Some(Modal::RemotePicker {
            reason,
            remotes,
            cursor,
        }) = &state.modal
        else {
            panic!("リモート選択モーダルが開いていません: {:?}", state.modal);
        };
        (reason, remotes, *cursor)
    }

    #[test]
    fn f_asks_which_remote_when_it_cannot_choose() {
        // **元は「`f` が無反応」の見張りだった**（git に決めさせると何もせず exit 0 で
        // 返り、stderr が空なので不変ルール 4 の経路も効かない）。通知で断るだけだと
        // **mewgit にリモートを指定する操作が無い**ので行き止まりだった（01 の
        // 「決まらないときの逃げ道」）。守っているのは今も「**mewgit が黙って選ばない**」。
        let mut state = state_for_push(&["fork", "upstream"], fake_branch("main", true));

        assert_eq!(press(&mut state, 'f'), Cmd::None, "黙って fetch している");
        let (reason, remotes, _) = open_picker(&state);
        assert_eq!(remotes, ["fork", "upstream"]);
        // **なぜ選ばされているか**を出す。無いと、毎回出るのか設定が壊れているのかが
        // 判らない。
        assert!(reason.contains("origin がありません"), "{reason}");
    }

    #[test]
    fn f_asks_even_when_only_one_remote_is_left() {
        // **候補が 1 本でも素通しにしない。** `.` は
        // `git branch --set-upstream-to=main topic` が書く形で、`Err` になるのは
        // 「黙って選ぶと危ない」から。**危ないのは mewgit が選ぶこと**なので、
        // 人が選ぶなら 1 本でも出す。
        let mut state = state_for_push(&["origin"], tracking_branch("main", ".", HEADS_MAIN));

        assert_eq!(press(&mut state, 'f'), Cmd::None);
        let (_, remotes, _) = open_picker(&state);
        assert_eq!(remotes, ["origin"]);
    }

    #[test]
    fn f_asks_instead_of_falling_back_when_the_tracked_remote_is_gone() {
        // **`origin` へ落とさない。** 落とすと、ユーザーが書いた行き先とは別の
        // リモートを黙って fetch する。選ばせるのは「黙って」ではない。
        let mut state = state_for_push(&["origin"], tracking_branch("main", "gone", HEADS_MAIN));

        assert_eq!(press(&mut state, 'f'), Cmd::None);
        let (reason, remotes, _) = open_picker(&state);
        assert_eq!(remotes, ["origin"]);
        assert!(reason.contains("gone"), "{reason}");
    }

    #[test]
    fn f_notifies_when_there_is_nobody_to_pick_from() {
        // **候補が 0 本なら開かない。** 「まだ読めていません」と「登録されていません」は
        // 選ばせる相手がいないので通知のまま（01）。
        let mut state = state_with_commits(2);
        let _ = with_branches(&mut state, Ok(vec![fake_branch("main", true)]));

        assert_eq!(press(&mut state, 'f'), Cmd::None);
        assert_eq!(state.modal, None, "選ぶ相手がいないのに開いている");
        let notice = state.notices.back().expect("通知が出ていない");
        assert!(
            notice.message.contains("登録されていません"),
            "{}",
            notice.message
        );
    }

    #[test]
    fn picking_a_remote_fetches_it_and_closes_the_modal() {
        let mut state = state_for_push(&["fork", "upstream"], fake_branch("main", true));
        let _ = press(&mut state, 'f');

        // `j` で 2 本目へ。端の扱いはペインのカーソルと同じ規則。
        assert_eq!(press(&mut state, 'j'), Cmd::None);
        assert_eq!(open_picker(&state).2, 1);
        // 端で止まる。
        let _ = press(&mut state, 'j');
        assert_eq!(open_picker(&state).2, 1, "端を越えている");

        let cmd = press_code(&mut state, KeyCode::Enter);
        assert_eq!(
            cmd,
            Cmd::Exec(GitOp::Fetch {
                remote: Some("upstream".to_string()),
                prune: false,
            })
        );
        // **先に閉じてから `Cmd` を返す。** 閉じずに返すと、`issue` に落とされたときに
        // モーダルだけが画面に残る。
        assert_eq!(state.modal, None);
    }

    #[test]
    fn esc_closes_the_remote_picker_without_fetching() {
        let mut state = state_for_push(&["fork", "upstream"], fake_branch("main", true));
        let _ = press(&mut state, 'f');

        assert_eq!(press_code(&mut state, KeyCode::Esc), Cmd::None);
        assert_eq!(state.modal, None);
    }

    #[test]
    fn f_works_before_the_branch_list_arrives() {
        // **1 段目だけ飛ばす。** 決まらないに落とすと、リモートが 1 本のふつうの
        // リポジトリで起動直後の `f` が断られる（01 の「既定リモート」）。
        let mut state = state_with_commits(2);
        with_remotes(&mut state, &["origin"]);
        let cmd = press(&mut state, 'f');
        assert_eq!(fetched_remote(&cmd).as_deref(), Some("origin"), "{cmd:?}");
    }

    #[test]
    fn a_modifier_keeps_the_remote_keys_from_firing() {
        // **`Ctrl-f` で fetch が走らない。** この段は全ペインを覆うので、`key.code()`
        // で見ると 07 が移動に予約している `Ctrl-d` / `Ctrl-u` の隣で修飾付きを食う。
        let mut state = state_for_push(&["origin"], tracking_branch("main", "origin", HEADS_MAIN));

        for c in ['f', 'p', 'P'] {
            let cmd = update(&mut state, Msg::Key(Key::ctrl(c)));
            assert_eq!(cmd, Cmd::None, "Ctrl-{c} が効いてしまいました");
        }
        assert_eq!(state.modal, None);
    }

    #[test]
    fn shift_p_pushes_to_the_tracking_config_without_asking() {
        // 行き先はユーザーが書いたもので、推測していない。
        let mut state = state_for_push(
            &["origin", "upstream"],
            tracking_branch("main", "origin", HEADS_MAIN),
        );

        let cmd = press(&mut state, 'P');

        assert_eq!(cmd, Cmd::Exec(push_op("origin", "main", false)));
        assert_eq!(state.modal, None);
    }

    #[test]
    fn a_pruned_tracking_ref_still_pushes_to_the_configured_upstream() {
        // **`upstream`（短縮名）で決めていると、ここで `-u` の提案に落ちる**。
        // `fetch --prune` のあとに `P` を押しただけで追跡先が別のリモートへ
        // 向け替わる（02 の「ブランチ一覧」が数えている `None` の 4 つ目）。
        let mut state = state_for_push(
            &["origin", "fork"],
            Branch {
                upstream: None,
                ..tracking_branch("main", "fork", HEADS_MAIN)
            },
        );

        let cmd = press(&mut state, 'P');

        assert_eq!(cmd, Cmd::Exec(push_op("fork", "main", false)));
        assert_eq!(state.modal, None, "追跡設定があるのに提案しています");
    }

    #[test]
    fn a_rejected_push_turns_the_next_one_into_a_confirmed_force_push() {
        // **専用のキーは作らない**（07）。断られたあとの再 `P` だけが force になる。
        let lease = fake_oid(7);
        let mut state = state_for_push(
            &["origin"],
            tracked_head("main", "origin", HEADS_MAIN, Some(lease)),
        );
        reject_non_fast_forward(&mut state, push_op("origin", "main", false));

        assert_eq!(press(&mut state, 'P'), Cmd::None, "確認せずに撃っています");
        let (body, on_yes) = open_confirm(&state);
        assert_eq!(on_yes, &Cmd::Exec(forced_push_op(lease)));
        // 見せた位置がそのまま lease に入る（01 の「確認モーダルを組む時点の値」）。
        assert!(
            body.contains(&short_oid(&lease)),
            "lease の位置が本文にありません: {body}"
        );
    }

    #[test]
    fn the_lease_names_the_remote_side_ref() {
        // refspec の右辺と lease の refname は同じ名前（03）。`dst` を持たない形に
        // 畳むと、ここで lease が `topic:other:<oid>` に化ける。
        let lease = fake_oid(7);
        let mut state = state_for_push(
            &["origin"],
            tracked_head("topic", "origin", "refs/heads/other", Some(lease)),
        );
        reject_non_fast_forward(&mut state, renamed_push_op("origin", "topic", "other"));

        assert_eq!(press(&mut state, 'P'), Cmd::None);
        let (_, on_yes) = open_confirm(&state);
        assert_eq!(
            on_yes,
            &Cmd::Exec(GitOp::Push {
                remote: "origin".to_string(),
                src: "topic".to_string(),
                dst: Some("other".to_string()),
                set_upstream: false,
                lease: Some(lease),
            })
        );
    }

    #[test]
    fn a_rejection_expires_when_the_branch_tip_moves() {
        // **`↓n` だけのブランチも断られる**（git 2.55 で確認）。名前だけで憶えると、
        // `p` で早送りした直後の `P` に force の確認が出る。
        let lease = fake_oid(7);
        let head = tracked_head("main", "origin", HEADS_MAIN, Some(lease));
        let mut state = state_for_push(&["origin"], head.clone());
        reject_non_fast_forward(&mut state, push_op("origin", "main", false));

        // pull が通って先端が動いた。
        let _ = with_branches(
            &mut state,
            Ok(vec![Branch {
                oid: fake_oid(3),
                ..head
            }]),
        );

        assert_eq!(
            press(&mut state, 'P'),
            Cmd::Exec(push_op("origin", "main", false))
        );
        assert_eq!(state.modal, None);
    }

    #[test]
    fn a_rejection_on_another_branch_does_not_arm_this_one() {
        let lease = fake_oid(7);
        let mut state = state_for_push(
            &["origin"],
            tracked_head("main", "origin", HEADS_MAIN, Some(lease)),
        );
        reject_non_fast_forward(&mut state, push_op("origin", "main", false));
        // 別のブランチへ切り替わった（一覧の読み直しで届く形）。
        let _ = with_branches(
            &mut state,
            Ok(vec![tracked_head(
                "topic",
                "origin",
                "refs/heads/topic",
                Some(lease),
            )]),
        );

        assert_eq!(
            press(&mut state, 'P'),
            Cmd::Exec(push_op("origin", "topic", false))
        );
        assert_eq!(state.modal, None);
    }

    #[test]
    fn a_force_push_is_refused_without_a_tracking_ref() {
        // 値の無い `--force-with-lease` は守りにならない（03）ので撃たない。
        // `upstream_config` はあるのに追跡 ref が無いのは `fetch --prune` のあと（02）。
        let mut state = state_for_push(
            &["origin"],
            tracked_head("main", "origin", HEADS_MAIN, None),
        );
        reject_non_fast_forward(&mut state, push_op("origin", "main", false));

        assert_eq!(press(&mut state, 'P'), Cmd::None);
        assert_eq!(state.modal, None);
        let notice = state.notices.back().expect("通知が出ていません");
        assert!(
            notice.message.contains("force push できません"),
            "断った理由が出ていません: {}",
            notice.message
        );
    }

    #[test]
    fn a_force_push_is_refused_when_there_is_nothing_to_publish() {
        // **`↓n` だけのブランチも非 FF で断られる**（git 2.55 で確認）ので、記憶は立つ。
        // そこで案内どおり `f` を踏むと lease がリモートの位置に追いつき、force が
        // **通ってしまう** —— 出すものが 1 つも無いのに他人のコミットを消す。
        let lease = fake_oid(7);
        let head = Branch {
            ahead: 0,
            behind: 2,
            ..tracked_head("main", "origin", HEADS_MAIN, Some(lease))
        };
        let mut state = state_for_push(&["origin"], head);
        reject_non_fast_forward(&mut state, push_op("origin", "main", false));

        assert_eq!(press(&mut state, 'P'), Cmd::None);
        assert_eq!(state.modal, None);
        let notice = state.notices.back().expect("通知が出ていません");
        assert!(
            notice.message.contains("p で取り込んでください"),
            "pull を勧めていません: {}",
            notice.message
        );
    }

    #[test]
    fn a_rejection_is_remembered_for_the_branch_that_was_pushed() {
        // **一覧の読み直しは別のジョブ。** `Switch` の直後に押した push が返るころには
        // HEAD が別のブランチになっていることがある。そこで HEAD を憶えると、押して
        // いないブランチに force push の腕が立つ。
        let lease = fake_oid(7);
        let mut state = state_for_push(
            &["origin"],
            tracked_head("main", "origin", HEADS_MAIN, Some(lease)),
        );
        // 押したのは main。返るまでに HEAD が topic へ移った。
        let _ = with_branches(
            &mut state,
            Ok(vec![tracked_head(
                "topic",
                "origin",
                "refs/heads/topic",
                Some(lease),
            )]),
        );
        reject_non_fast_forward(&mut state, push_op("origin", "main", false));

        assert_eq!(
            press(&mut state, 'P'),
            Cmd::Exec(push_op("origin", "topic", false)),
            "押していない topic に force の腕が立っています"
        );
        assert_eq!(state.modal, None);
    }

    #[test]
    fn a_failed_force_push_does_not_arm_the_next_one() {
        // lease が古いときの `(stale info)` も `NonFastForward` に落ちる（git 2.55 で
        // 確認）。ここで憶え直すと、同じ古い lease で force を撃ち続ける輪になる。
        let lease = fake_oid(7);
        let mut state = state_for_push(
            &["origin"],
            tracked_head("main", "origin", HEADS_MAIN, Some(lease)),
        );
        reject_non_fast_forward(&mut state, forced_push_op(lease));

        assert_eq!(
            press(&mut state, 'P'),
            Cmd::Exec(push_op("origin", "main", false))
        );
        assert_eq!(state.modal, None);
    }

    #[test]
    fn a_fetch_between_the_two_pushes_keeps_the_rejection() {
        // **本命の回復経路。** 拒否された時点の `origin/x` は他人が進める前なので、
        // そのまま lease に渡すと `stale info` で落ちる（git 2.55 で確認）。`f` で
        // 位置を見てから `P` を 1 回押すだけで force に上がる（01 の「force push」）。
        let stale = fake_oid(7);
        let fresh = fake_oid(8);
        let head = tracked_head("main", "origin", HEADS_MAIN, Some(stale));
        let mut state = state_for_push(&["origin"], head.clone());
        reject_non_fast_forward(&mut state, push_op("origin", "main", false));

        finish_exec(
            &mut state,
            GitOp::Fetch {
                remote: None,
                prune: false,
            },
            Ok(fake_output()),
        );
        // fetch はローカルの先端を動かさないので、記憶は生きたまま。届く一覧では
        // 追跡 ref だけが進んでいる。
        let _ = with_branches(
            &mut state,
            Ok(vec![tracked_head(
                "main",
                "origin",
                HEADS_MAIN,
                Some(fresh),
            )]),
        );

        assert_eq!(press(&mut state, 'P'), Cmd::None);
        let (_, on_yes) = open_confirm(&state);
        assert_eq!(on_yes, &Cmd::Exec(forced_push_op(fresh)));
    }

    #[test]
    fn a_successful_push_forgets_the_rejection() {
        let lease = fake_oid(7);
        let mut state = state_for_push(
            &["origin"],
            tracked_head("main", "origin", HEADS_MAIN, Some(lease)),
        );
        reject_non_fast_forward(&mut state, push_op("origin", "main", false));
        finish_exec(
            &mut state,
            push_op("origin", "main", false),
            Ok(fake_output()),
        );

        assert_eq!(
            press(&mut state, 'P'),
            Cmd::Exec(push_op("origin", "main", false))
        );
        assert_eq!(state.modal, None);
    }

    #[test]
    fn a_differently_named_upstream_is_pushed_as_a_refspec() {
        // `push origin topic` が動かすのは `origin/topic`。**追跡先が
        // `refs/heads/other` なら、それは別のブランチを新しく作る**（01）。
        let mut state = state_for_push(
            &["origin"],
            tracking_branch("topic", "origin", "refs/heads/other"),
        );

        assert_eq!(
            press(&mut state, 'P'),
            Cmd::Exec(renamed_push_op("origin", "topic", "other"))
        );
    }

    #[test]
    fn a_merge_ref_outside_refs_heads_is_passed_whole() {
        // 短縮できない名前を短縮したことにすると、別の ref を動かす。
        let mut state = state_for_push(
            &["origin"],
            tracking_branch("topic", "origin", "refs/for/main"),
        );

        assert_eq!(
            press(&mut state, 'P'),
            Cmd::Exec(renamed_push_op("origin", "topic", "refs/for/main"))
        );
    }

    #[test]
    fn a_branch_without_a_tracking_config_proposes_set_upstream() {
        // **行き先を推測しているのはこの経路だけ**なので、確認モーダルに出す（01）。
        let mut state = state_for_push(&["upstream"], fake_branch("topic", true));

        let cmd = press(&mut state, 'P');

        assert_eq!(cmd, Cmd::None, "確認より先に git を走らせています");
        let (body, on_yes) = open_confirm(&state);
        // 行き先が本文に出ていないと、推測を見せた意味が無い。
        assert!(
            body.contains("topic") && body.contains("upstream"),
            "{body}"
        );
        assert_eq!(*on_yes, Cmd::Exec(push_op("upstream", "topic", true)));
    }

    #[test]
    fn a_rejected_set_upstream_push_is_not_remembered() {
        // **force に上げられない経路の拒否は憶えない**（01 の「force push」）。憶えると
        // 読む側が使えない印になり、あとから追跡設定が外で入ったときに、追跡して
        // いない push の拒否を根拠に force の腕が立つ。
        let mut state = state_for_push(&["origin"], fake_branch("topic", true));
        reject_non_fast_forward(&mut state, push_op("origin", "topic", true));

        assert_eq!(state.push_rejected, None);
        // **`P` は押し直せる。** ここで断ると `Push` が二度と出ず、記憶を落とす経路
        // （`exec_finished`）へ入れなくなる —— リモート側の同名ブランチが外で消えても
        // 断り続けることになる。
        assert_eq!(press(&mut state, 'P'), Cmd::None);
        let (_, on_yes) = open_confirm(&state);
        assert_eq!(*on_yes, Cmd::Exec(push_op("origin", "topic", true)));
    }

    #[test]
    fn several_remotes_fall_back_to_origin() {
        let mut state = state_for_push(&["fork", "origin"], fake_branch("topic", true));

        let _ = press(&mut state, 'P');

        let (_, on_yes) = open_confirm(&state);
        assert_eq!(*on_yes, Cmd::Exec(push_op("origin", "topic", true)));
    }

    /// `push_plan` が断った理由。断る腕は 4 つあるので、キーを押す形ではなく
    /// 純粋関数を直に呼ぶ（`Refused` を返した理由がそのまま読める）。
    fn refusal(remotes: &[&str], branches: Option<Vec<Branch>>) -> String {
        let mut state = state_with_commits(2);
        with_remotes(&mut state, remotes);
        if let Some(branches) = branches {
            let _ = with_branches(&mut state, Ok(branches));
        }
        match push_plan(&state) {
            PushPlan::Refused(message) => message,
            other => panic!("断られていません: {other:?}"),
        }
    }

    #[test]
    fn a_locally_tracking_branch_is_refused() {
        // `branch.topic.remote = "."`（`git branch --set-upstream-to=main topic`）。
        // **渡すと `git push . topic:main` になり、ネットワークに出ないまま
        // ローカルの `main` を早送りして exit 0 で返る**（git 2.55 で確認）。
        let mut state = state_for_push(&["origin"], tracking_branch("topic", ".", HEADS_MAIN));

        assert_eq!(press(&mut state, 'P'), Cmd::None);
        // **`-u` の提案にも落とさない。** そちらは `branch.<name>.remote` を
        // 上書きするので、ユーザーが書いた `.` が黙って消える。
        assert_eq!(state.modal, None);
        let notice = state.notices.back().expect("通知が出ていません");
        assert!(notice.message.contains("ローカル"), "{}", notice.message);
    }

    #[test]
    fn remotes_that_have_not_arrived_are_not_reported_as_missing() {
        // `LoadRepoInfo` と `LoadBranches` は別のジョブなので、一覧だけ先に届いた
        // 瞬間がある。そこで「リモートが登録されていません」と言うと嘘になる。
        let mut state = state_with_commits(2);
        let _ = state.issue(Cmd::LoadRepoInfo);
        let _ = with_branches(&mut state, Ok(vec![fake_branch("topic", true)]));

        let PushPlan::Refused(message) = push_plan(&state) else {
            panic!("断られていません");
        };
        assert!(message.contains("読めていません"), "{message}");
    }

    #[test]
    fn push_is_refused_when_the_destination_cannot_be_named() {
        // **一覧が届く前**。空の `Vec` からは detached と区別できないので `load` を見る
        // （01 の「ブランチ一覧」）。
        assert!(refusal(&["origin"], None).contains("読めていません"));
        // **detached / unborn**。一覧は届いているが、どの行にも印が付いていない。
        assert!(refusal(&["origin"], Some(vec![fake_branch("main", false)])).contains("HEAD"));
        // **リモートが 1 本も無い。**
        let none = refusal(&[], Some(vec![fake_branch("topic", true)]));
        assert!(none.contains("リモート"), "{none}");
        // **複数あって origin が無い。** 適当に選ぶと、fork 運用のリポジトリで
        // 他人のリポジトリへ push する形になる。名前を出して選ばせる。
        let many = refusal(
            &["fork", "upstream"],
            Some(vec![fake_branch("topic", true)]),
        );
        assert!(many.contains("fork") && many.contains("upstream"), "{many}");
    }

    #[test]
    fn a_refused_push_opens_no_modal_and_warns() {
        let mut state = state_for_push(&[], fake_branch("topic", true));

        assert_eq!(press(&mut state, 'P'), Cmd::None);
        assert_eq!(state.modal, None);
        assert_eq!(
            state.notices.back().map(|notice| notice.level),
            Some(NoticeLevel::Warn)
        );
    }
}
