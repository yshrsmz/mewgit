//! `git_read::RepoReader` の統合テスト。実リポジトリを一時ディレクトリに作って読む。

use std::fs;

use mewgit_core::git_read::{
    GitReadError, HeadInfo, InProgressOp, LogPage, Oid, RefLabel, RepoReader, Step, Upstream,
    UpstreamConfig,
};
use mewgit_core::test_util::{BareRepo, TestRepo};

#[test]
fn open_finds_the_repository_from_a_subdirectory() {
    let repo = TestRepo::init();
    repo.commit("first");
    let nested = repo.path().join("a/b");
    fs::create_dir_all(&nested).unwrap();

    let reader = RepoReader::open(&nested).unwrap();

    // 渡したパスではなく、見つかったワークツリーのルートを返す。
    assert_eq!(reader.workdir(), repo.path());
}

#[test]
fn open_rejects_a_bare_repository() {
    let repo = BareRepo::init();

    let err = RepoReader::open(repo.path()).unwrap_err();

    assert!(
        matches!(err, GitReadError::BareRepository { .. }),
        "bare を拒否していません: {err:?}"
    );
    // libgit2 の末尾区切りを残さない。
    assert!(
        !err.to_string().contains("/ は"),
        "パスの末尾に区切りが残っています: {err}"
    );
}

#[test]
fn open_fails_outside_a_repository() {
    // `discover` は上に辿るので、ワークスペース内ではなく一時ディレクトリで試す。
    let dir = tempfile::tempdir().unwrap();

    let err = RepoReader::open(dir.path()).unwrap_err();

    assert!(
        matches!(err, GitReadError::Open { .. }),
        "open のエラーになっていません: {err:?}"
    );
    // どこを開こうとして失敗したかがメッセージに出る。
    assert!(err.to_string().contains(&dir.path().display().to_string()));
}

#[test]
fn open_normalizes_the_workdir_path() {
    let repo = TestRepo::init();
    let reader = RepoReader::open(repo.path()).unwrap();

    // libgit2 は末尾に区切りを付けた `/…/repo/` を返す。`Path` の比較は区切りを
    // 無視するので、文字列で見る。
    let workdir = reader.workdir().to_string_lossy().into_owned();
    assert!(
        !workdir.ends_with('/'),
        "末尾に区切りが残っています: {workdir}"
    );
}

#[test]
fn remotes_returns_every_remote_sorted_by_name() {
    let repo = TestRepo::init();
    repo.git(&[
        "remote",
        "add",
        "upstream",
        "git@github.com:upstream/mewgit.git",
    ]);
    repo.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/me/mewgit.git",
    ]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let remotes = reader.remotes().unwrap();

    let pairs: Vec<(&str, &str)> = remotes
        .iter()
        .map(|r| (r.name.as_str(), r.url.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("origin", "https://github.com/me/mewgit.git"),
            ("upstream", "git@github.com:upstream/mewgit.git"),
        ]
    );
}

#[test]
fn remotes_read_the_default_branch_from_the_remote_head_symref() {
    // `refs/remotes/<name>/HEAD` が指すブランチを `head_branch` に入れる（02）。
    // **生の `refs/remotes/origin/main` のままでは PR の base に載せられない**ので、
    // 接頭辞を剥がす。
    //
    // **symref は `git symbolic-ref` で書く。** clone と `git fetch` が書くのと
    // 同じもので（実測 2026-09-21、git 2.55 の `env -i` 一時リポジトリ:
    // clone 直後と `remote add` + `fetch` のあとはどちらも
    // `refs/remotes/origin/HEAD -> refs/remotes/origin/main`）、ここでリモートを
    // 立てずに済む。
    let repo = TestRepo::init();
    repo.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/me/mewgit.git",
    ]);
    repo.git(&["remote", "add", "upstream", "git@github.com:up/mewgit.git"]);
    repo.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/trunk",
    ]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let remotes = reader.remotes().unwrap();

    let heads: Vec<(&str, Option<&str>)> = remotes
        .iter()
        .map(|r| (r.name.as_str(), r.head_branch.as_deref()))
        .collect();
    // **`Some` と `None` を 1 本で見る。** `None` だけを見る assert は、symref を
    // 1 行も書かなくても通る（`.claude/rules/testing.md` の「否定を見る assert は…」）。
    // `upstream` が `None` なのは、一度も fetch していないリモートの形（02）。
    assert_eq!(heads, [("origin", Some("trunk")), ("upstream", None)]);
}

#[test]
fn a_remote_head_pointing_outside_the_remote_is_not_a_branch_name() {
    // 剥がせない値は `None`（02）。**`refs/heads/main` をそのまま返すと**、
    // PR の base に `refs/heads/main` が載って GitHub が 422 を返す。
    let repo = TestRepo::init();
    repo.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/me/mewgit.git",
    ]);
    repo.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/heads/main",
    ]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let remotes = reader.remotes().unwrap();

    assert_eq!(remotes[0].head_branch, None);
}

#[test]
fn remotes_is_empty_without_any_remote() {
    let repo = TestRepo::init();
    let reader = RepoReader::open(repo.path()).unwrap();

    assert_eq!(reader.remotes().unwrap(), []);
}

#[test]
fn refs_labels_local_branches_and_marks_head() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    repo.git(&["branch", "topic"]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let index = reader.refs().unwrap();

    assert_eq!(
        index.labels(first),
        [
            RefLabel::Local {
                name: "main".to_string(),
                is_head: true,
            },
            RefLabel::Local {
                name: "topic".to_string(),
                is_head: false,
            },
        ]
    );
}

#[test]
fn refs_peels_annotated_tags_to_the_commit() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    repo.git(&["tag", "light"]);
    // annotated tag は tag オブジェクトを指すので、peel しないとこの行に乗らない。
    repo.git(&["tag", "-a", "heavy", "-m", "annotated"]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let index = reader.refs().unwrap();

    let tags: Vec<&str> = index
        .labels(first)
        .iter()
        .filter(|label| matches!(label, RefLabel::Tag { .. }))
        .map(RefLabel::name)
        .collect();
    assert_eq!(tags, ["heavy", "light"]);
}

#[test]
fn refs_keeps_remote_branches_without_duplicating_the_symbolic_head() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    // clone せずにリモート追跡 ref だけを作る。
    repo.git(&["update-ref", "refs/remotes/origin/main", &first.to_string()]);
    repo.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let index = reader.refs().unwrap();

    // origin/HEAD を解決すると origin/main が 2 つになる。
    let remotes: Vec<&str> = index
        .labels(first)
        .iter()
        .filter(|label| matches!(label, RefLabel::Remote { .. }))
        .map(RefLabel::name)
        .collect();
    assert_eq!(remotes, ["origin/main"]);
    // **名前からも引ける**（PR 作成の `head` の候補。01 の「head の組み立て」）。
    assert_eq!(index.remote_oid("origin/main"), Some(first));
    assert_eq!(index.remote_oid("origin/other"), None);
    // ローカルブランチの名前では引けない。
    assert_eq!(index.remote_oid("main"), None);
}

#[test]
fn refs_are_sorted_by_kind_then_name() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    repo.git(&["branch", "alpha"]);
    repo.git(&["tag", "v2"]);
    repo.git(&["tag", "v1"]);
    repo.git(&["update-ref", "refs/remotes/origin/main", &first.to_string()]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let index = reader.refs().unwrap();

    let names: Vec<&str> = index.labels(first).iter().map(RefLabel::name).collect();
    assert_eq!(names, ["alpha", "main", "origin/main", "v1", "v2"]);
}

#[test]
fn refs_ignores_the_stash() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    repo.write_worktree_file("file.txt", b"dirty\n");
    repo.git(&["stash", "push", "-m", "wip"]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let index = reader.refs().unwrap();

    // stash コミットは first を親に持つ別コミットなので、first の行にも自分の行にも
    // ラベルは付かない（stash は stash ペインの担当）。
    let stash = repo.rev_parse("refs/stash");
    assert_eq!(index.labels(stash), []);
    let names: Vec<&str> = index.labels(first).iter().map(RefLabel::name).collect();
    assert_eq!(names, ["main"]);
}

#[test]
fn refs_skips_a_tag_that_cannot_be_peeled_to_a_commit() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    // tree を直接指すタグ。commit まで辿れないのでグラフのどの行にも乗らない。
    repo.git(&["tag", "treetag", "HEAD^{tree}"]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let index = reader.refs().unwrap();

    let names: Vec<&str> = index.labels(first).iter().map(RefLabel::name).collect();
    assert_eq!(names, ["main"]);
    assert_eq!(index.labels(repo.rev_parse("HEAD^{tree}")), []);
}

#[test]
fn refs_marks_no_branch_as_head_when_detached() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    repo.git(&["checkout", "--detach", &first.to_string()]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let index = reader.refs().unwrap();

    // HEAD が symbolic でないので、どのブランチも HEAD ではない。
    assert_eq!(
        index.labels(first),
        [RefLabel::Local {
            name: "main".to_string(),
            is_head: false,
        }]
    );
}

#[test]
fn refs_is_empty_before_the_first_commit() {
    let repo = TestRepo::init();
    let reader = RepoReader::open(repo.path()).unwrap();

    let index = reader.refs().unwrap();

    assert_eq!(index.labels(Oid::ZERO_SHA1), []);
}

#[test]
fn head_reports_the_current_branch() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    let reader = RepoReader::open(repo.path()).unwrap();

    assert_eq!(
        reader.head().unwrap(),
        HeadInfo::Branch {
            name: "main".to_string(),
            oid: first,
            upstream: None,
        }
    );
}

#[test]
fn head_reads_the_tracking_remote_only_once_the_merge_ref_is_there() {
    // **`app` はこの値で fork から出した PR を当てる**（01 の「現ブランチの PR」）。
    // リモートを登録しないのは、`head` が読むのが config だけだから（リモート名を
    // URL に直すのは `app` の仕事）。
    //
    // **`None` の側だけを見ない。** あれは設定を 1 行も書かなくても自明に通るので、
    // 入力を踏んだ証拠にならない（`merge` を足すと `Some` に変わるところまで見る）。
    // `upstream_config` をそのまま呼ぶので、片方だけの設定は `None`（02 の「head」）。
    let repo = TestRepo::init();
    let first = repo.commit("first");
    repo.git(&["config", "branch.main.remote", "fork"]);

    // **開き直す。** git2 の config は変更を stat で見るので、同じ `RepoReader` を
    // 使い回すと 1 秒以内の書き換えを拾わないことがある。
    assert_eq!(
        RepoReader::open(repo.path()).unwrap().head().unwrap(),
        HeadInfo::Branch {
            name: "main".to_string(),
            oid: first,
            upstream: None,
        },
        "`merge` が無いのに追跡先を返している"
    );

    repo.git(&["config", "branch.main.merge", "refs/heads/main"]);

    assert_eq!(
        RepoReader::open(repo.path()).unwrap().head().unwrap(),
        HeadInfo::Branch {
            name: "main".to_string(),
            oid: first,
            upstream: Some(UpstreamConfig {
                remote: "fork".to_string(),
                merge: "refs/heads/main".to_string(),
            }),
        }
    );
}

#[test]
fn head_reports_a_detached_head() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    repo.commit("second");
    repo.git(&["checkout", "--detach", &first.to_string()]);
    let reader = RepoReader::open(repo.path()).unwrap();

    assert_eq!(reader.head().unwrap(), HeadInfo::Detached(first));
}

#[test]
fn head_errors_when_the_branch_name_is_not_utf8() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    // git CLI では作れないので、packed-refs と HEAD を直接書く。ブランチに乗って
    // いるのに名前を落とすと detached と区別が付かないため、ここはエラーになる。
    repo.write_git_file(
        "packed-refs",
        &[
            b"# pack-refs with: peeled fully-peeled sorted \n".to_vec(),
            first.to_string().into_bytes(),
            b" refs/heads/".to_vec(),
            vec![0xff],
            b"\n".to_vec(),
        ]
        .concat(),
    );
    repo.write_git_file(
        "HEAD",
        &[b"ref: refs/heads/".to_vec(), vec![0xff], b"\n".to_vec()].concat(),
    );
    let reader = RepoReader::open(repo.path()).unwrap();

    let err = reader.head().unwrap_err();

    assert!(
        matches!(err, GitReadError::NonUtf8Head),
        "非 UTF-8 のブランチ名を検出していません: {err:?}"
    );
}

#[test]
fn head_reports_an_unborn_branch() {
    // コミットが 1 つも無いと `repo.head()` は失敗する。
    let repo = TestRepo::init();
    let reader = RepoReader::open(repo.path()).unwrap();

    assert_eq!(
        reader.head().unwrap(),
        HeadInfo::Unborn {
            name: "main".to_string(),
        }
    );
}

/// 本文付きのメッセージを持つコミットが 1 つあるリポジトリ。
///
/// `TestRepo::commit` は summary しか付けられないので amend で書き換える。
/// メッセージ全文を読む 2 つのテスト（`commit_detail` と `head_message`）で使う。
fn repo_with_a_body() -> TestRepo {
    let repo = TestRepo::init();
    repo.commit("subject");
    repo.git(&["commit", "--amend", "-q", "-m", WHOLE_MESSAGE]);
    repo
}

/// `repo_with_a_body` が作るメッセージ全文。
const WHOLE_MESSAGE: &str = "subject\n\nbody 1\nbody 2";

#[test]
fn head_message_reports_the_whole_message_of_head() {
    // amend の初期値に使う。summary だけだと、本文のあるコミットを amend したときに
    // 本文が消える（01 の「commit」）。
    let repo = repo_with_a_body();
    let reader = RepoReader::open(repo.path()).unwrap();

    assert_eq!(
        reader.head_message().unwrap().as_deref(),
        Some(WHOLE_MESSAGE)
    );
}

#[test]
fn head_message_follows_head_instead_of_a_branch_name() {
    // detached でも読める。ブランチ名を経由すると detached で読めなくなる。
    let repo = TestRepo::init();
    let first = repo.commit("first");
    repo.commit("second");
    repo.git(&["checkout", "-q", &first.to_string()]);
    let reader = RepoReader::open(repo.path()).unwrap();

    assert_eq!(reader.head_message().unwrap().as_deref(), Some("first"));
}

#[test]
fn head_message_is_none_without_any_commit() {
    // 「読めなかった」と「amend する相手がいない」は上位の案内が違うので、
    // unborn を `Err` に畳まない（02 の「スレッド」）。
    let repo = TestRepo::init();
    let reader = RepoReader::open(repo.path()).unwrap();

    assert_eq!(reader.head_message().unwrap(), None);
}

// ---- ログ列挙 ----

/// ページの Oid だけを取り出す。
fn oids(page: &LogPage) -> Vec<Oid> {
    page.commits.iter().map(|c| c.oid).collect()
}

#[test]
fn log_lists_commits_newest_first() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    let second = repo.commit("second");
    let third = repo.commit("third");
    let reader = RepoReader::open(repo.path()).unwrap();

    let page = reader.log().unwrap().next_page(10).unwrap();

    assert_eq!(oids(&page), [third, second, first]);
    assert!(page.done, "全部読んだのに done が立っていません");
}

#[test]
fn log_pages_through_a_linear_history() {
    let repo = TestRepo::init();
    let commits: Vec<Oid> = (0..5).map(|i| repo.commit(&format!("c{i}"))).collect();
    let reader = RepoReader::open(repo.path()).unwrap();
    let mut session = reader.log().unwrap();

    let first = session.next_page(2).unwrap();
    let second = session.next_page(2).unwrap();
    let third = session.next_page(2).unwrap();

    assert_eq!(oids(&first), [commits[4], commits[3]]);
    assert!(!first.done);
    assert_eq!(oids(&second), [commits[2], commits[1]]);
    assert!(!second.done);
    assert_eq!(oids(&third), [commits[0]]);
    assert!(third.done);
}

#[test]
fn log_is_done_when_the_page_ends_exactly_at_the_last_commit() {
    let repo = TestRepo::init();
    repo.commit("first");
    repo.commit("second");
    let reader = RepoReader::open(repo.path()).unwrap();
    let mut session = reader.log().unwrap();

    let page = session.next_page(2).unwrap();

    // 先読みしないとここが `false` になり、空のページをもう 1 回読むことになる。
    assert_eq!(page.commits.len(), 2);
    assert!(
        page.done,
        "ちょうど n 件で終わったのに done が立っていません"
    );
    // それでも続きを求められたら空を返す。
    let after = session.next_page(2).unwrap();
    assert!(after.commits.is_empty());
    assert!(after.done);
}

#[test]
fn log_on_an_unborn_repository_is_empty() {
    // HEAD は `head_detached()` が真のときだけ push する。無条件に `push_head` を
    // 呼ぶと、ここで解決できずに起動直後の空リポジトリでログの読み込みが失敗する。
    let repo = TestRepo::init();
    let reader = RepoReader::open(repo.path()).unwrap();

    let page = reader.log().unwrap().next_page(10).unwrap();

    assert!(page.commits.is_empty());
    assert!(page.done);
}

#[test]
fn log_includes_commits_on_other_branches() {
    let repo = TestRepo::init();
    let base = repo.commit("base");
    repo.git(&["checkout", "-b", "feature"]);
    let feature = repo.commit("feature");
    repo.git(&["checkout", "main"]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let page = reader.log().unwrap().next_page(10).unwrap();

    // HEAD から辿れない行もグラフに出す。
    assert_eq!(oids(&page), [feature, base]);
}

#[test]
fn log_includes_remote_tracking_branches() {
    let repo = TestRepo::init();
    let base = repo.commit("base");
    repo.git(&["checkout", "-b", "ahead"]);
    let remote_only = repo.commit("remote only");
    repo.git(&["checkout", "main"]);
    // ローカルブランチは消し、リモート追跡 ref だけが指す状態にする。
    repo.git(&[
        "update-ref",
        "refs/remotes/origin/main",
        &remote_only.to_string(),
    ]);
    repo.git(&["branch", "-D", "ahead"]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let page = reader.log().unwrap().next_page(10).unwrap();

    // `{origin/main}` のラベルを載せる行が無くならないよう refs/remotes/* も push する。
    assert_eq!(oids(&page), [remote_only, base]);
}

#[test]
fn log_includes_tagged_commits() {
    let repo = TestRepo::init();
    let base = repo.commit("base");
    repo.git(&["checkout", "-b", "tagged"]);
    let tagged = repo.commit("tagged");
    repo.git(&["tag", "-a", "v1", "-m", "annotated"]);
    repo.git(&["checkout", "main"]);
    repo.git(&["branch", "-D", "tagged"]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let page = reader.log().unwrap().next_page(10).unwrap();

    assert_eq!(oids(&page), [tagged, base]);
}

#[test]
fn log_survives_a_tag_that_cannot_be_peeled_to_a_commit() {
    let repo = TestRepo::init();
    let first = repo.commit("first");
    // tree を指すタグ。単体の `push_ref` だと列挙全体が失敗する。
    repo.git(&["tag", "treetag", "HEAD^{tree}"]);
    let reader = RepoReader::open(repo.path()).unwrap();

    let page = reader.log().unwrap().next_page(10).unwrap();

    assert_eq!(oids(&page), [first]);
}

#[test]
fn log_includes_a_detached_head() {
    let repo = TestRepo::init();
    let base = repo.commit("base");
    repo.git(&["checkout", "--detach"]);
    let detached = repo.commit("detached");
    let reader = RepoReader::open(repo.path()).unwrap();

    let page = reader.log().unwrap().next_page(10).unwrap();

    // どのブランチからも辿れないので、`head_detached()` の分岐が無いと消える。
    assert_eq!(oids(&page), [detached, base]);
}

#[test]
fn log_reports_parents_of_a_merge_commit() {
    let repo = TestRepo::init();
    let base = repo.commit("base");
    repo.git(&["checkout", "-b", "side"]);
    let side = repo.commit_file("side.txt", "side");
    repo.git(&["checkout", "main"]);
    let main = repo.commit_file("main.txt", "main");
    repo.git(&["merge", "--no-ff", "-m", "merge", "side"]);
    let merge = repo.head_oid();
    let reader = RepoReader::open(repo.path()).unwrap();

    let page = reader.log().unwrap().next_page(10).unwrap();

    // 祖先関係の無い main / side の順序は日時で決まる（`TestRepo` の固定クロック）。
    assert_eq!(oids(&page), [merge, main, side, base]);
    // 第 1 親が現在のブランチ側。`graph` のレーン継承がこの順に依存する（06）。
    assert_eq!(page.commits[0].parents, [main, side]);
}

#[test]
fn log_keeps_a_non_utf8_summary_instead_of_dropping_the_commit() {
    let repo = TestRepo::init();
    // 0xff は UTF-8 として不正。表示にしか使わない文字列は lossy で残す。
    let oid = repo.commit_raw_message(b"broken \xff summary\n");
    let reader = RepoReader::open(repo.path()).unwrap();

    let page = reader.log().unwrap().next_page(10).unwrap();

    assert_eq!(oids(&page), [oid]);
    assert_eq!(page.commits[0].summary, "broken \u{fffd} summary");
}

#[test]
fn log_reports_the_author_and_time() {
    let repo = TestRepo::init();
    repo.commit("first");
    let expected_time: i64 = repo
        .git(&["show", "-s", "--format=%at", "HEAD"])
        .parse()
        .unwrap();
    let reader = RepoReader::open(repo.path()).unwrap();

    let page = reader.log().unwrap().next_page(10).unwrap();

    let commit = &page.commits[0];
    assert_eq!(commit.summary, "first");
    assert_eq!(commit.author.name, "mewgit test");
    assert_eq!(commit.author.email, "test@example.invalid");
    assert_eq!(commit.time, expected_time);
}

#[test]
fn log_orders_by_committer_time_while_reporting_author_time() {
    let repo = TestRepo::init();
    let base = repo.commit("base");
    repo.git(&["checkout", "-b", "side"]);
    repo.commit_file("side.txt", "side");
    // author 日時だけを未来にずらす。committer 日時は `TestRepo` の固定クロックのまま。
    repo.git(&[
        "commit",
        "--amend",
        "--no-edit",
        "--date",
        "1800000000 +0000",
    ]);
    let side = repo.head_oid();
    repo.git(&["checkout", "main"]);
    let main = repo.commit_file("main.txt", "main");
    let reader = RepoReader::open(repo.path()).unwrap();

    let page = reader.log().unwrap().next_page(10).unwrap();

    // 並びは `Sort::TIME` = committer 日時で決まる。
    assert_eq!(oids(&page), [main, side, base]);
    // 一方 `Commit::time` は author 日時なので、行の並びとは逆転しうる（`git log` と同じ）。
    assert_eq!(page.commits[1].time, 1_800_000_000);
    assert!(
        page.commits[0].time < page.commits[1].time,
        "author 日時が並び順どおりになっており、逆転のケースを踏めていません"
    );
}

#[test]
fn commit_detail_reports_the_whole_message() {
    // グラフの行が持つのは summary だけなので、詳細では本文まで読めることが要る。
    let repo = repo_with_a_body();
    let oid = repo.head_oid();
    let reader = RepoReader::open(repo.path()).unwrap();

    let detail = reader.commit_detail(oid).unwrap();

    assert_eq!(detail.oid, oid);
    // 末尾に git が付ける改行は落とす。残すと詳細の最後に空行が出る。
    assert_eq!(detail.message, WHOLE_MESSAGE);
}

#[test]
fn commit_detail_reports_the_author_and_committer_separately() {
    let repo = TestRepo::init();
    repo.commit("first");
    // author 日時だけを別のタイムゾーンにずらす。committer は `TestRepo` の
    // 固定クロック（+0000）のまま。
    repo.git(&[
        "commit",
        "--amend",
        "--no-edit",
        "-q",
        "--date",
        "1700000000 +0900",
    ]);
    let oid = repo.head_oid();
    let reader = RepoReader::open(repo.path()).unwrap();

    let detail = reader.commit_detail(oid).unwrap();

    assert_eq!(detail.author.name, "mewgit test");
    assert_eq!(detail.author.email, "test@example.invalid");
    assert_eq!(detail.author_time.seconds, 1_700_000_000);
    // コミットに刻まれたオフセットをそのまま出す（`git show` と同じ）。
    assert_eq!(detail.author_time.offset_minutes, 540);
    assert_eq!(detail.committer.name, "mewgit test");
    assert_eq!(detail.committer_time.offset_minutes, 0);
    assert_ne!(detail.author_time.seconds, detail.committer_time.seconds);
}

#[test]
fn commit_detail_reports_the_parents_of_a_merge() {
    let repo = TestRepo::init();
    let base = repo.commit("base");
    repo.git(&["checkout", "-b", "side"]);
    let side = repo.commit_file("side.txt", "side");
    repo.git(&["checkout", "main"]);
    let main = repo.commit_file("main.txt", "main");
    repo.git(&["merge", "--no-ff", "-m", "merge side", "side"]);
    let merge = repo.head_oid();
    let reader = RepoReader::open(repo.path()).unwrap();

    let detail = reader.commit_detail(merge).unwrap();

    assert_eq!(detail.parents, [main, side]);
    assert!(reader.commit_detail(base).unwrap().parents.is_empty());
}

#[test]
fn commit_detail_keeps_a_non_utf8_message() {
    let repo = TestRepo::init();
    // 表示にしか使わない文字列は lossy で残す（02 の「非 UTF-8 の扱い」）。
    let oid = repo.commit_raw_message(b"broken \xff message\n");
    let reader = RepoReader::open(repo.path()).unwrap();

    let detail = reader.commit_detail(oid).unwrap();

    assert_eq!(detail.message, "broken \u{fffd} message");
}

#[test]
fn commit_detail_fails_for_an_oid_that_is_not_in_the_repository() {
    let repo = TestRepo::init();
    repo.commit("first");
    let reader = RepoReader::open(repo.path()).unwrap();

    let err = reader
        .commit_detail(Oid::from_str("0000000000000000000000000000000000000001").unwrap())
        .unwrap_err();

    assert!(
        matches!(err, GitReadError::Git(_)),
        "予期しないエラーです: {err:?}"
    );
}

#[test]
fn branches_lists_local_branches_by_name_with_the_head_marked() {
    let repo = TestRepo::init();
    repo.commit("first");
    repo.git(&["branch", "topic"]);
    repo.git(&["branch", "another"]);
    // `/` を含む名前も落とさない（列挙の絞り込みが `refs/heads/*` なので）。
    repo.git(&["branch", "feat/nested"]);

    let reader = RepoReader::open(repo.path()).unwrap();
    let branches = reader.branches().unwrap();

    // git2 の列挙順には依存しない。名前順に並べ替えて返す。
    let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["another", "feat/nested", "main", "topic"]);
    // HEAD の印は 1 本だけ。
    let head: Vec<&str> = branches
        .iter()
        .filter(|b| b.is_head)
        .map(|b| b.name.as_str())
        .collect();
    assert_eq!(head, ["main"]);
    // upstream が無ければ ahead / behind は出さない。
    assert!(branches.iter().all(|b| b.upstream.is_none()));
}

#[test]
fn branches_reports_the_upstream_and_how_far_it_diverged() {
    let repo = TestRepo::init();
    repo.commit("first");
    // リモートを実際に持たせる。libgit2 の `upstream()` は `branch.<name>.remote` が
    // 指すリモートが `.git/config` に無いと `NotFound` になる。
    repo.git(&["remote", "add", "origin", "https://example.invalid/x.git"]);
    let base = repo.head_oid().to_string();
    repo.git(&["update-ref", "refs/remotes/origin/main", &base]);
    repo.git(&["config", "branch.main.remote", "origin"]);
    repo.git(&["config", "branch.main.merge", "refs/heads/main"]);
    // ローカルだけを 2 つ進める。
    repo.commit("second");
    repo.commit("third");

    let reader = RepoReader::open(repo.path()).unwrap();
    let branches = reader.branches().unwrap();

    let main = branches.iter().find(|b| b.name == "main").unwrap();
    // 名前と、force push の lease に渡す位置。**位置はローカルの先端ではなく
    // 追跡 ref のもの**（01）。
    assert_eq!(
        main.upstream,
        Some(Upstream {
            name: "origin/main".to_string(),
            oid: base.parse().unwrap(),
        })
    );
    assert_eq!((main.ahead, main.behind), (2, 0));
    // push の行き先は短縮名ではなく設定から取る（02）。
    assert_eq!(
        main.upstream_config,
        Some(UpstreamConfig {
            remote: "origin".to_string(),
            merge: "refs/heads/main".to_string(),
        })
    );
}

#[test]
fn branches_keeps_a_branch_whose_upstream_ref_is_gone() {
    // 設定はあるが追跡 ref が消えている（`fetch --prune` のあと）。`upstream()` は
    // 未設定と同じ `NotFound` を返すので、どちらも「比べる相手がいない」に畳む（02）。
    let repo = TestRepo::init();
    repo.commit("first");
    repo.git(&["remote", "add", "origin", "https://example.invalid/x.git"]);
    repo.git(&["config", "branch.main.remote", "origin"]);
    repo.git(&["config", "branch.main.merge", "refs/heads/main"]);

    let reader = RepoReader::open(repo.path()).unwrap();
    let branches = reader.branches().unwrap();

    // 一覧ごと `Err` にしない。
    let main = branches.iter().find(|b| b.name == "main").unwrap();
    // **名前も位置も無い。** 比べる相手を知らないので force push は断る（01）。
    assert_eq!(main.upstream, None);
    assert_eq!((main.ahead, main.behind), (0, 0));
    // **追跡設定のほうは残る。** ここが `None` に落ちると、`fetch --prune` のあとに
    // `P` を押しただけで「追跡先が無い」と読まれ、別のリモートへ向け替わる（02）。
    assert_eq!(
        main.upstream_config,
        Some(UpstreamConfig {
            remote: "origin".to_string(),
            merge: "refs/heads/main".to_string(),
        })
    );
}

#[test]
fn branches_has_no_upstream_config_when_nothing_is_configured() {
    // `branch.<name>.remote` も `.merge` も無い。`-u` を提案してよいのはここだけ。
    let repo = TestRepo::init();
    repo.commit("first");
    repo.git(&["remote", "add", "origin", "https://example.invalid/x.git"]);

    let reader = RepoReader::open(repo.path()).unwrap();
    let branches = reader.branches().unwrap();

    let main = branches.iter().find(|b| b.name == "main").unwrap();
    assert_eq!(main.upstream_config, None);
}

#[test]
fn branches_needs_both_halves_of_the_tracking_config() {
    // 片方だけ書かれた設定から push の行き先は決まらない。組み合わせを呼び出し側に
    // 数えさせないよう、揃っていなければ `None`（02）。
    let repo = TestRepo::init();
    repo.commit("first");
    repo.git(&["remote", "add", "origin", "https://example.invalid/x.git"]);
    repo.git(&["config", "branch.main.remote", "origin"]);

    let reader = RepoReader::open(repo.path()).unwrap();
    let branches = reader.branches().unwrap();

    let main = branches.iter().find(|b| b.name == "main").unwrap();
    assert_eq!(main.upstream_config, None);
}

#[test]
fn branches_survives_a_broken_tracking_ref() {
    // 中断した fetch のあとなど、追跡 ref が存在しないオブジェクトを指すことがある。
    // **その 1 本のために一覧全体を失わない**（他のブランチはどれも読めている）。
    let repo = TestRepo::init();
    repo.commit("first");
    repo.git(&["branch", "topic"]);
    repo.git(&["remote", "add", "origin", "https://example.invalid/x.git"]);
    repo.git(&["config", "branch.main.remote", "origin"]);
    repo.git(&["config", "branch.main.merge", "refs/heads/main"]);
    // まず正しい追跡 ref を作り（ディレクトリもここでできる）、中身をどのオブジェクトも
    // 指さない oid に書き換える。`update-ref` は存在を検査するのでファイルを直に書く。
    let oid = repo.head_oid().to_string();
    repo.git(&["update-ref", "refs/remotes/origin/main", &oid]);
    repo.write_git_file(
        "refs/remotes/origin/main",
        b"0000000000000000000000000000000000000001\n",
    );

    let reader = RepoReader::open(repo.path()).unwrap();
    let branches = reader.branches().unwrap();

    let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["main", "topic"]);
    // 比べる相手が引けないので ahead / behind は出さない。
    let main = branches.iter().find(|b| b.name == "main").unwrap();
    // **名前も位置も無い。** 比べる相手を知らないので force push は断る（01）。
    assert_eq!(main.upstream, None);
    assert_eq!((main.ahead, main.behind), (0, 0));
}

#[test]
fn branches_drops_a_non_utf8_name() {
    let repo = TestRepo::init();
    repo.commit("first");
    let oid = repo.head_oid().to_string();
    // **loose ref としては作れない。** ref 名がそのままファイル名になるので、macOS の
    // APFS が `Illegal byte sequence` で弾く（非 UTF-8 のパスを index にだけ入れるのと
    // 同じ制約。`.claude/rules/testing.md`）。`packed-refs` は 1 つのファイルの中身なので通る。
    let mut packed = b"# pack-refs with: peeled fully-peeled sorted \n".to_vec();
    packed.extend_from_slice(oid.as_bytes());
    packed.extend_from_slice(b" refs/heads/\xff\xfe\n");
    repo.write_git_file("packed-refs", &packed);

    let reader = RepoReader::open(repo.path()).unwrap();
    let branches = reader.branches().unwrap();

    // 識別子として使えないので落とす。残すと切替も削除もできない行が出る（02）。
    let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["main"]);
}

#[test]
fn branches_is_empty_before_the_first_commit() {
    // unborn HEAD ではブランチの ref がまだ無い。**`Err` ではない。**
    let repo = TestRepo::init();

    let reader = RepoReader::open(repo.path()).unwrap();

    assert_eq!(reader.branches().unwrap(), Vec::new());
}

// ---- 進行中操作（02 の「進行中操作の検出」）。表の実測を実物の git で踏む。

/// **`status()` から読む**（`WorktreeSnapshot.progress`）。検出の関数を直に呼ぶと、
/// `status` がどの gitdir を渡しているかを誰も見ない。
fn progress_of(path: &std::path::Path) -> Option<InProgressOp> {
    RepoReader::open(path).unwrap().status().unwrap().progress
}

fn step(current: usize, total: usize) -> Option<Step> {
    Some(Step { current, total })
}

#[test]
fn nothing_is_in_progress_in_a_clean_repository() {
    let (repo, _, _) = TestRepo::diverged();
    assert_eq!(progress_of(repo.path()), None);
}

#[test]
fn a_stopped_rebase_reports_its_step() {
    // `-i` を付けない rebase。`rebase-merge/interactive` は付けなくても作られる
    // ので、`-i` の有無は持たない（02 の実測）。
    let (repo, _, _) = TestRepo::diverged();
    repo.git(&["switch", "side"]);
    assert!(!repo.try_git(&["rebase", "main"]));

    assert_eq!(
        progress_of(repo.path()),
        Some(InProgressOp::Rebase {
            step: step(1, 2),
            branch: Some("side".to_string()),
        })
    );
}

#[test]
fn a_stopped_apply_backend_rebase_is_a_rebase() {
    let (repo, _, _) = TestRepo::diverged();
    repo.git(&["switch", "side"]);
    assert!(!repo.try_git(&["rebase", "--apply", "main"]));

    assert_eq!(
        progress_of(repo.path()),
        Some(InProgressOp::Rebase {
            step: step(1, 2),
            branch: Some("side".to_string()),
        })
    );
}

#[test]
fn a_stopped_am_is_not_a_rebase() {
    // 同じ `rebase-apply/` を使うが、`git rebase --continue` は am の途中を断る。
    let (repo, first, _) = TestRepo::diverged();
    let patches = repo.path().parent().unwrap().join("patches");
    repo.git(&[
        "format-patch",
        "-q",
        "-1",
        &first.to_string(),
        "-o",
        patches.to_str().unwrap(),
    ]);
    let patch = fs::read_dir(&patches)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(!repo.try_git(&["am", "-3", patch.to_str().unwrap()]));

    assert_eq!(
        progress_of(repo.path()),
        Some(InProgressOp::Am { step: step(1, 1) })
    );
}

#[test]
fn a_stopped_cherry_pick_reports_the_commit() {
    let (repo, first, _) = TestRepo::diverged();
    assert!(!repo.try_git(&["cherry-pick", &first.to_string()]));

    assert_eq!(
        progress_of(repo.path()),
        Some(InProgressOp::CherryPick { oid: Some(first) })
    );
}

#[test]
fn a_cherry_pick_sequence_is_still_in_progress_after_committing_the_conflict() {
    // 解いて commit すると `CHERRY_PICK_HEAD` は消え、`sequencer/` だけが残る。
    // `git2::Repository::state()` はこの形を「無い」と読む（02）。
    let (repo, first, second) = TestRepo::diverged();
    assert!(!repo.try_git(&["cherry-pick", &first.to_string(), &second.to_string()]));
    assert_eq!(
        progress_of(repo.path()),
        Some(InProgressOp::CherryPick { oid: Some(first) })
    );
    repo.commit_file("f", "resolved");

    assert_eq!(
        progress_of(repo.path()),
        Some(InProgressOp::CherryPick { oid: None })
    );
}

#[test]
fn a_stopped_revert_reports_the_commit() {
    let (repo, first, _) = TestRepo::diverged();
    repo.git(&["switch", "side"]);
    repo.commit_file("f", "later");
    assert!(!repo.try_git(&["revert", "--no-edit", &first.to_string()]));

    assert_eq!(
        progress_of(repo.path()),
        Some(InProgressOp::Revert { oid: Some(first) })
    );
}

#[test]
fn a_stopped_merge_reports_the_merged_commit() {
    let (repo, _, second) = TestRepo::diverged();
    assert!(!repo.try_git(&["merge", "side"]));

    assert_eq!(
        progress_of(repo.path()),
        Some(InProgressOp::Merge { oid: second })
    );
}

#[test]
fn a_linked_worktree_reads_its_own_state() {
    // linked worktree の `MERGE_HEAD` は `.git/worktrees/<name>/` に置かれる。
    // `workdir/.git` を決め打ちすると、どちらのワークツリーからも読み違える。
    let (repo, _, second) = TestRepo::diverged();
    let linked = repo.path().parent().unwrap().join("linked");
    repo.git(&[
        "worktree",
        "add",
        "-b",
        "wt",
        linked.to_str().unwrap(),
        "main",
    ]);
    assert!(!repo.try_git(&["-C", linked.to_str().unwrap(), "merge", "side"]));

    assert_eq!(
        progress_of(&linked),
        Some(InProgressOp::Merge { oid: second })
    );
    assert_eq!(progress_of(repo.path()), None);
}
