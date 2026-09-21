//! テスト用の一時リポジトリ。`test-util` feature でのみコンパイルされる。
//!
//! `tests/common/mod.rs` から移してきた。`mod common;` はテストバイナリごとに別々に
//! コンパイルされるので、テストファイルが 2 つ以上になると、そのバイナリで使わない
//! ヘルパが `dead_code` になる（`clippy --all-targets -D warnings` で落ちる）。
//! ライブラリの `pub` な項目にすればその問題が消え、`mewgit-tui` のテストからも使える。
//!
//! リポジトリは `git` CLI で作る。git2 で作ると「git が作ったものを git2 で読めるか」
//! ではなく「git2 が書いたものを git2 で読めるか」のテストになるため。
//!
//! ワークツリーのある [`TestRepo`] と [`BareRepo`] は型を分ける。bare にはワークツリーも
//! `.git` ディレクトリも無いので、`commit` や `write_git_file` を呼べてしまうと実行時に
//! `git` の stderr で落ちるだけになる。分けておけばコンパイルエラーで気付く。

use std::cell::Cell;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::future::Future;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;

/// 一時ディレクトリにテキストファイルを 1 枚書く。
///
/// リポジトリを要らない設定ファイル（`config.toml`、`ssh_config`）のためのもの。
/// **返す `TempDir` は drop されると中身ごと消える**ので、呼び出し側が束縛し続ける。
pub fn temp_file(name: &str, body: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作れなかった");
    let path = dir.path().join(name);
    fs::write(&path, body).expect("一時ファイルを書けなかった");
    (dir, path)
}

/// `ssh` の代わりに走らせる偽物と、それを指す [`SshCommand`]。
///
/// **`sh <script>` を実効値にするので実行権限が要らない**（実効 ssh はシェル文字列
/// なので、そのまま `sh -c` に載る）。`body` は `-G -- <alias>` を引数に受けるが、
/// 無視して好きな dump を書き出してよい。
///
/// **返す `TempDir` は drop されると中身ごと消える**ので、呼び出し側が束縛し続ける。
pub fn fake_ssh(body: &str) -> (tempfile::TempDir, SshCommand) {
    let (dir, path) = temp_file("fake-ssh", body);
    (dir, SshCommand::Shell(format!("sh {}", path.display())))
}

/// `hostname <host>` の 1 行だけを返す偽 ssh。[`fake_ssh`] の 4 人めまでの使い道。
///
/// **`host <alias>` の行は入れない。** 「`host` ではなく `hostname` を拾う」の見張りは
/// `hostname_from_dump` のユニットテストが持っており、こちらで重ねても踏む腕は増えない。
pub fn fake_ssh_host(host: &str) -> (tempfile::TempDir, SshCommand) {
    fake_ssh(&format!("echo 'hostname {host}'\n"))
}

/// コミット日時の起点（2023-11-14T22:13:20Z）。`git` を 1 回叩くごとに [`TICK`] 秒進める。
///
/// 実時間を使うと、続けて作ったコミットが同じ秒に落ちて `Sort::TIME` での並びが
/// 揺れる。テストごとに固定の値を与えて順序を決める。
const CLOCK_START: i64 = 1_700_000_000;

/// `git` 1 回あたりに進める秒数。
const TICK: i64 = 60;

use crate::app::{update, AppState, Cmd, Msg, TargetState};
use crate::auth::ssh::SshCommand;
use crate::auth::{
    Account, AccountSource, AccountState, AttemptFailure, AuthError, Backend, DegradedReason,
    GhLogin, LoginState, RepoTarget,
};
use crate::git_exec::{GitOp, Runner};
use crate::git_read::{
    Branch, Commit, CommitDetail, FileDiff, FileEntry, FileStatus, HeadInfo, Hunk, HunkLine,
    LineOrigin, Oid, Remote, Signature, Timestamp, Upstream, WorktreeSnapshot,
};
use crate::github::{GithubError, PrDetail, PrStatus, PullRequest, RateLimit, RefSpec, RepoRef};
use secrecy::SecretString;
use tempfile::TempDir;

/// 一時ディレクトリと、実行環境の設定を持ち込まない `git` の実行。
///
/// [`TestRepo`] と [`BareRepo`] で共通する部分だけを持つ。
struct Sandbox {
    /// drop で削除するために持つ。
    _dir: TempDir,
    root: PathBuf,
    home: PathBuf,
    /// 次の `git` 実行に渡すコミット日時。
    clock: Cell<i64>,
}

impl Sandbox {
    /// 空のディレクトリを用意し、その中で `git` を実行する。
    fn init(init_args: &[&str]) -> Self {
        let dir = tempfile::tempdir().expect("一時ディレクトリを作れませんでした");
        // macOS の `/var` は `/private/var` への symlink なので、git2 が返すパスと
        // 比較できるようにここで正規化しておく。
        let base = dir
            .path()
            .canonicalize()
            .expect("一時ディレクトリを正規化できませんでした");
        let root = base.join("repo");
        let home = base.join("home");
        fs::create_dir(&root).expect("repo ディレクトリを作れませんでした");
        fs::create_dir(&home).expect("home ディレクトリを作れませんでした");

        let sandbox = Self {
            _dir: dir,
            root,
            home,
            clock: Cell::new(CLOCK_START),
        };
        sandbox.git(init_args);
        sandbox
    }

    /// `git` を実行し、標準出力を末尾の改行を落として返す。失敗したら panic する。
    fn git<S: AsRef<OsStr>>(&self, args: &[S]) -> String {
        String::from_utf8(self.git_bytes(args))
            .expect("git の出力が UTF-8 ではありません")
            .trim_end()
            .to_string()
    }

    /// `git` を実行し、標準出力を**そのまま**返す。失敗したら panic する。
    ///
    /// [`git`](Self::git) が末尾の改行を落とすので、CRLF や「改行で終わらない」を
    /// 見るテストはこちらを使う（`\r\n` の `\r` まで一緒に落ちる）。
    fn git_bytes<S: AsRef<OsStr>>(&self, args: &[S]) -> Vec<u8> {
        let out = self.try_git(args);
        assert!(
            out.status.success(),
            "git {} が失敗しました: {}",
            shown(args),
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    /// `git` を実行して結果をそのまま返す。失敗しても panic しない。
    ///
    /// コンフリクトする `git merge` のように、失敗が期待どおりのコマンドに使う。
    fn try_git<S: AsRef<OsStr>>(&self, args: &[S]) -> Output {
        // 日時は実時間ではなく固定のカウンタから与える。同じ秒に落ちたコミットは
        // `Sort::TIME` での並びが揺れてテストが不安定になる。
        let date = format!("{} +0000", self.tick());
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.root)
            .args(args)
            .envs(self.env())
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date);

        cmd.output().expect("git を実行できませんでした")
    }

    /// `git` に渡す環境変数のうち、実行環境の設定を持ち込まないためのもの。
    ///
    /// 日時はここに含めない。含めると「呼ぶと時計が進む getter」になり、
    /// スナップショットを取る使い方（[`TestRepo::runner_env`]）と両立しない。
    fn env(&self) -> Vec<(&'static str, OsString)> {
        vec![
            // 特に `commit.gpgsign` が有効だと署名を待ってテストが固まる。
            ("GIT_CONFIG_NOSYSTEM", OsString::from("1")),
            (
                "GIT_CONFIG_GLOBAL",
                self.home.join("no-such-gitconfig").into_os_string(),
            ),
            ("HOME", self.home.clone().into_os_string()),
            ("GIT_AUTHOR_NAME", OsString::from("mewgit test")),
            ("GIT_AUTHOR_EMAIL", OsString::from("test@example.invalid")),
            ("GIT_COMMITTER_NAME", OsString::from("mewgit test")),
            (
                "GIT_COMMITTER_EMAIL",
                OsString::from("test@example.invalid"),
            ),
            ("GIT_TERMINAL_PROMPT", OsString::from("0")),
        ]
    }

    /// 現在の日時を返し、カウンタを [`TICK`] 秒進める。
    fn tick(&self) -> i64 {
        let now = self.clock.get();
        self.clock.set(now + TICK);
        now
    }
}

/// ワークツリーのある一時リポジトリ。`TempDir` が drop されると消える。
pub struct TestRepo {
    sandbox: Sandbox,
}

impl TestRepo {
    /// 空のリポジトリを作る。デフォルトブランチは `main`。
    pub fn init() -> Self {
        Self {
            sandbox: Sandbox::init(&["init", "-b", "main"]),
        }
    }

    /// ワークツリーのルート。
    pub fn path(&self) -> &Path {
        &self.sandbox.root
    }

    /// `git` を実行し、標準出力を末尾の改行を落として返す。失敗したら panic する。
    ///
    /// 引数は `AsRef<OsStr>` なら何でも渡せる。非 UTF-8 のパスを `git` に渡すテストが
    /// [`Sandbox`] を直接触らずに済むようにするため。
    pub fn git<S: AsRef<OsStr>>(&self, args: &[S]) -> String {
        self.sandbox.git(args)
    }

    /// `git` を実行し、標準出力を生バイト列で返す。
    ///
    /// [`git`](Self::git) は末尾の改行を落とすので、CRLF や「改行で終わらない」を
    /// 見るテストはこちらを使う。
    pub fn git_bytes<S: AsRef<OsStr>>(&self, args: &[S]) -> Vec<u8> {
        self.sandbox.git_bytes(args)
    }

    /// `git status --porcelain` の出力。
    pub fn porcelain(&self) -> String {
        self.git(&["status", "--porcelain"])
    }

    /// このリポジトリを対象にした [`Runner`]。実行環境の設定を持ち込まない。
    ///
    /// **統合テストごとに書かない。** テストバイナリは別々にコンパイルされるので、
    /// 各 `tests/*.rs` に置くとファイルを足すたびにコピーが増える。
    pub fn runner(&self) -> Runner {
        Runner::new(self.path(), "git", SshCommand::default()).with_env(self.runner_env())
    }

    /// `git` を実行し、成功したかどうかだけを返す。失敗しても panic しない。
    ///
    /// コンフリクトする `git merge` のように、失敗が期待どおりのコマンドに使う。
    pub fn try_git(&self, args: &[&str]) -> bool {
        self.sandbox.try_git(args).status.success()
    }

    /// 非 UTF-8 のパスを index にだけ足し、その blob の Oid を返す。
    /// ワークツリーにはファイルを作らない。
    ///
    /// macOS の APFS は非 UTF-8 のファイル名を受け付けない（`Illegal byte sequence`）ので、
    /// ワークツリーには置けない。index にだけ入れれば「staged で追加され、ワークツリー
    /// からは消えている」状態になり、staged 側と unstaged 側の両方を 1 回で踏める。
    ///
    /// 返る Oid は、同じ blob を別の名前で index に入れて rename を作るのに使う。
    pub fn add_raw_path_to_index(&self, path: &[u8]) -> String {
        // 中身は何でもよいので、リポジトリの外に置いたファイルから blob を作る。
        let source = self.sandbox.home.join("raw-blob");
        fs::write(&source, b"raw\n").expect("blob の元ファイルを書けませんでした");
        let blob = self.git(&[
            "hash-object",
            "-w",
            source.to_str().expect("一時パスが UTF-8 ではありません"),
        ]);

        // パスが非 UTF-8 なので argv は `OsStr` で組む。`--cacheinfo` はワークツリーを
        // 見ないので、ファイルが存在しなくても index に入る。
        let mut cacheinfo = OsString::from(format!("100644,{blob},"));
        cacheinfo.push(OsStr::from_bytes(path));
        self.git(&[
            OsStr::new("update-index"),
            OsStr::new("--add"),
            OsStr::new("--cacheinfo"),
            &cacheinfo,
        ]);
        blob
    }

    /// `file.txt` を書き換えてコミットし、その Oid を返す。
    pub fn commit(&self, message: &str) -> Oid {
        self.commit_file("file.txt", message)
    }

    /// 指定したファイルを書き換えてコミットし、その Oid を返す。中身はメッセージ。
    ///
    /// ブランチを分けて後でマージするテストは、同じファイルを触るとマージが
    /// コンフリクトするのでこちらを使う。
    pub fn commit_file(&self, path: &str, message: &str) -> Oid {
        self.commit_bytes(path, format!("{message}\n").as_bytes(), message)
    }

    /// 中身を指定してコミットし、その Oid を返す。
    ///
    /// 改行で終わらないファイルやバイナリなど、[`commit_file`](Self::commit_file) の
    /// 「中身 = メッセージ + 改行」では作れないものに使う。
    pub fn commit_bytes(&self, path: &str, content: &[u8], message: &str) -> Oid {
        self.write_worktree_file(path, content);
        self.git(&["add", path]);
        self.git(&["commit", "-q", "-m", message]);
        self.head_oid()
    }

    /// 非 UTF-8 のコミットメッセージでコミットする。
    ///
    /// - `-m` では argv に載せられないので `-F` でファイルから渡す
    /// - `-q` を付けないと git が summary を stdout に echo し、[`Sandbox::git`] の
    ///   UTF-8 検証に引っかかる（落ちるのはテスト側で、読む側の実装ではない）
    /// - `i18n.commitEncoding` を UTF-8 以外にしないと、git が「UTF-8 ではない」と
    ///   判断したメッセージを latin-1 とみなして UTF-8 に変換してしまい、
    ///   生バイト列がオブジェクトに残らない（git 2.55 で確認）
    pub fn commit_raw_message(&self, message: &[u8]) -> Oid {
        let message_file = self.sandbox.home.join("commit-msg");
        fs::write(&message_file, message).expect("コミットメッセージを書けませんでした");
        self.write_worktree_file("file.txt", b"raw\n");
        self.git(&["add", "file.txt"]);
        self.git(&[
            "-c",
            "i18n.commitEncoding=ISO-8859-1",
            "commit",
            "-q",
            "-F",
            message_file
                .to_str()
                .expect("一時パスが UTF-8 ではありません"),
        ]);
        self.head_oid()
    }

    /// ワークツリーのファイルを書く。親ディレクトリは作らない。
    pub fn write_worktree_file(&self, path: &str, content: &[u8]) {
        fs::write(self.path().join(path), content)
            .unwrap_or_else(|err| panic!("{path} を書けませんでした: {err}"));
    }

    /// `git_exec::Runner` に渡す環境変数。`TestRepo` 自身が `git` を叩くときと
    /// 同じ隔離（system / global config を読まない、author と committer を固定）を
    /// 与える。付けないと実行環境の `commit.gpgsign` を拾って固まる。
    ///
    /// 日時は含めない。`Runner` 経由のコミットは実時間になるので、[`TestRepo`] が
    /// 作った固定日時（2023 年）のコミットより必ず後ろに並ぶ。
    pub fn runner_env(&self) -> Vec<(&'static str, OsString)> {
        self.sandbox.env()
    }

    /// 現在の HEAD の Oid。
    pub fn head_oid(&self) -> Oid {
        self.rev_parse("HEAD")
    }

    /// 途中で止まる書き込みを 1 つ用意する。`GitOp` と、フィルタが pid を書く
    /// ファイルを返す。
    ///
    /// clean filter が自分の pid を書いてから眠るので、`git add` は
    /// **`index.lock` を握ったまま止まる**。中断のテストはこの状態を作ってから
    /// 合図を送る（03 の「キャンセル」）。**pid が書かれた時点が「git が走って
    /// いる」**なので、[`wait_for_pid`] で待ってから中断する。
    ///
    /// pid のファイルはリポジトリの外に置く。中に置くと untracked として一覧に
    /// 現れ、status を見るテストと混ざる。
    pub fn slow_stage(&self) -> (GitOp, PathBuf) {
        let pid_file = self
            .path()
            .parent()
            .expect("一時ディレクトリの親がありません")
            .join("filter.pid");
        self.write_worktree_file(".gitattributes", b"slow.txt filter=slow\n");
        self.write_worktree_file("slow.txt", b"content\n");
        // git はフィルタをシェル経由で起こすので、`$$` はそのシェル自身の pid。
        // **`exec` で置き換える。** `sleep` を fork すると、グループ宛ての signal を
        // 送った瞬間にその子がまだメンバーに入っていない窓ができ、取り逃がした子が
        // git の pipe を握ったまま残る（`wait_with_output` は pipe が閉じるまで
        // 返らないので、テストが 30 秒待つ）。
        self.git(&[
            "config",
            "filter.slow.clean",
            &format!("echo $$ > {}; exec sleep 30", pid_file.display()),
        ]);
        (GitOp::Stage(vec!["slow.txt".into()]), pid_file)
    }

    /// `.git/index.lock` の場所。中断したあとに残っていないことを見る。
    pub fn index_lock(&self) -> PathBuf {
        self.path().join(".git/index.lock")
    }

    /// `.git/` の下のファイルを生バイト列で書く。git CLI では作れない ref 名
    /// （非 UTF-8）を仕込むのに使う。
    pub fn write_git_file(&self, name: &str, content: &[u8]) {
        fs::write(self.path().join(".git").join(name), content)
            .unwrap_or_else(|err| panic!(".git/{name} を書けませんでした: {err}"));
    }

    /// リビジョン指定を Oid に解決する。
    pub fn rev_parse(&self, rev: &str) -> Oid {
        let out = self.git(&["rev-parse", rev]);
        Oid::from_str(&out).unwrap_or_else(|err| panic!("{rev} の Oid を読めませんでした: {err}"))
    }
}

/// 条件が満たされるまで待つ。**待ち時間の予算はここ 1 本。**
///
/// 別々に書くと刻みと上限が散り、片方だけ延ばしたときに実機の遅さで落ちるのが
/// もう片方だけ、という形になる。上限は 5 秒（10ms × 500）。
async fn poll_for<T>(mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    for _ in 0..500 {
        if let Some(value) = probe() {
            return Some(value);
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    None
}

/// [`TestRepo::slow_stage`] のフィルタが起動して pid を書くまで待つ。
///
/// 書かれなければ panic する。**ここを待たずに中断すると、git がまだ lock を
/// 握る前に殺すことがあり、見張りが何も見ていない状態で緑になる。**
pub async fn wait_for_pid(path: &Path) -> i32 {
    poll_for(|| fs::read_to_string(path).ok()?.trim().parse().ok())
        .await
        .unwrap_or_else(|| panic!("フィルタが起動しませんでした: {}", path.display()))
}

/// そのプロセスが消えるまで待つ。消えなければ `false`。
///
/// 孤児は init に引き取られてから消えるので、即座には居なくならない。
pub async fn wait_until_gone(pid: i32) -> bool {
    // SAFETY: signal 0 は「送れるかどうか」を見るだけで、相手にも自分にも触らない。
    poll_for(|| (unsafe { libc::kill(pid, 0) } != 0).then_some(()))
        .await
        .is_some()
}

/// bare な一時リポジトリ。
///
/// ワークツリーが無いので `path()` しか生やさない。コミットや `.git/` への書き込みを
/// 足したくなったら、それが bare で意味のある操作かを先に確かめる。
pub struct BareRepo {
    sandbox: Sandbox,
}

impl BareRepo {
    /// bare リポジトリを作る。デフォルトブランチは `main`。
    pub fn init() -> Self {
        Self {
            sandbox: Sandbox::init(&["init", "--bare", "-b", "main"]),
        }
    }

    /// bare リポジトリのルート。`.git` サブディレクトリは無く、ここが直接その中身。
    pub fn path(&self) -> &Path {
        &self.sandbox.root
    }

    /// bare リポジトリの中で `git` を実行し、標準出力を返す。失敗したら panic する。
    ///
    /// push された結果を bare 側で確かめるのに使う。ワークツリーを触るコマンドは
    /// bare では動かないので渡さない。
    pub fn git(&self, args: &[&str]) -> String {
        self.sandbox.git(args)
    }
}

/// panic のメッセージに出す argv。非 UTF-8 の引数も渡せるので lossy で並べる。
fn shown<S: AsRef<OsStr>>(args: &[S]) -> String {
    args.iter()
        .map(|arg| arg.as_ref().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// アカウント解決の対象。`app` の `update` と `auth::resolve` の両方が使う。
///
/// **owner / repo は実在のものに合わせてある** —— `[[repos]]` の glob を書く
/// テストがこの値に当たる。
pub fn fake_target() -> RepoTarget {
    RepoTarget {
        host: "github.com".to_string(),
        port: None,
        owner: "yshrsmz".to_string(),
        repo: "mewgit".to_string(),
    }
}

/// 解決済みのアカウント 1 件（[`fake_target`] のホスト）。
///
/// **`ui` からも使う。** トークンを組むのに `secrecy` が要り、`mewgit-tui` は
/// あのクレートに依存していない（ヘッダはトークンを描かない）。
pub fn fake_account(login: &str) -> Account {
    Account {
        host: fake_target().host,
        login: login.to_string(),
        token: SecretString::from("fake-token".to_string()),
        source: AccountSource::Single,
    }
}

/// 手順 1〜4 を踏むための [`Backend`](crate::auth::Backend)。
///
/// **実 `gh` では踏めない**（04 の「`&GhCli` を `resolve` の引数にすると
/// テストできない」）ので、trait の向こう側はここで組む。
///
/// **`test_util` に置くのは 2 人目（`tests/git_exec.rs` の `SetAccount`）が出たから。**
/// テストバイナリは別々にコンパイルされるので、`#[cfg(test)] mod tests` の中では
/// 統合テストから見えない（規約）。
pub struct Fake {
    logins: Result<Vec<GhLogin>, AuthError>,
    /// login ごとの probe の結果。無い login は届かなかったことにする。
    probes: BTreeMap<String, Result<(), AttemptFailure>>,
    /// トークンを取れない login と、そのときの `gh` の失敗。**種類を持つ** ——
    /// 手順 1 は login 固有の失敗（`GhFailed`）とプロセス全体の条件
    /// （`GhMissing` など）を分けるので、1 種類しか作れないと片方を踏めない。
    no_token: BTreeMap<String, AuthError>,
    calls: Mutex<Vec<String>>,
    /// 全候補が揃うまで probe を返さない関所。**逐次に回すと開かない。**
    gate: Option<tokio::sync::Barrier>,
}

impl Fake {
    pub fn new(logins: Vec<GhLogin>) -> Self {
        Fake {
            logins: Ok(logins),
            probes: BTreeMap::new(),
            no_token: BTreeMap::new(),
            calls: Mutex::default(),
            gate: None,
        }
    }

    pub fn failing(err: AuthError) -> Self {
        Fake {
            logins: Err(err),
            ..Fake::new(Vec::new())
        }
    }

    pub fn probing(mut self, login: &str, outcome: Result<(), AttemptFailure>) -> Self {
        self.probes.insert(login.to_string(), outcome);
        self
    }

    pub fn without_token(self, login: &str) -> Self {
        self.failing_token(
            login,
            AuthError::GhFailed {
                argv: vec!["gh".to_string(), "auth".to_string(), "token".to_string()],
                message: format!("exit 1: no oauth token found for account {login}"),
            },
        )
    }

    pub fn failing_token(mut self, login: &str, err: AuthError) -> Self {
        self.no_token.insert(login.to_string(), err);
        self
    }

    pub fn gated(mut self, parties: usize) -> Self {
        self.gate = Some(tokio::sync::Barrier::new(parties));
        self
    }

    fn record(&self, call: String) {
        self.calls.lock().expect("記録").push(call);
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("記録").clone()
    }
}

impl Backend for Fake {
    fn logins(&self, host: &str) -> impl Future<Output = Result<Vec<GhLogin>, AuthError>> + Send {
        self.record(format!("logins:{host}"));
        let result = self.logins.clone();
        async move { result }
    }

    fn token(
        &self,
        _host: &str,
        login: &str,
    ) -> impl Future<Output = Result<SecretString, AuthError>> + Send {
        self.record(format!("token:{login}"));
        let result = match self.no_token.get(login) {
            Some(err) => Err(err.clone()),
            None => Ok(SecretString::from(format!("token-for-{login}"))),
        };
        async move { result }
    }

    fn probe(
        &self,
        _target: &RepoTarget,
        token: &SecretString,
    ) -> impl Future<Output = Result<(), AttemptFailure>> + Send {
        use secrecy::ExposeSecret;
        let login = token
            .expose_secret()
            .strip_prefix("token-for-")
            .expect("トークンは token() が作ったもの")
            .to_string();
        self.record(format!("probe:{login}"));
        let outcome = self.probes.get(&login).cloned().unwrap_or_else(|| {
            Err(AttemptFailure::Undetermined {
                message: "テストが結果を用意していない".to_string(),
            })
        });
        let gate = self.gate.as_ref();
        async move {
            if let Some(gate) = gate {
                gate.wait().await;
            }
            outcome
        }
    }
}

/// そのホストの 1 ログイン（`gh auth status` の 1 件）。
///
/// **`active` は偽、`scopes` は 1 本、`state` は `Success`。** 行の見た目を変える
/// 3 つなので、既定は「印の付かない普通の 1 件」にしておき、見張りたい側が上書きする。
pub fn fake_login(login: &str) -> GhLogin {
    GhLogin {
        host: fake_target().host,
        login: login.to_string(),
        active: false,
        scopes: vec!["repo".to_string()],
        state: LoginState::Success,
    }
}

/// 候補が複数ある `Ambiguous`。
///
/// **候補を空にしない形をここで持つ。** ヘッダの長さの見張り
/// （`every_degraded_reason_fits_the_header`）は候補があるときの文言
/// （`A で選ぶ` が付く）を測るためにこれを要る。空の `Ambiguous` を各テストが
/// 手で組めるようにしておくと、**出荷される形を測らないまま緑になる**。
pub fn fake_ambiguous(logins: &[&str]) -> DegradedReason {
    DegradedReason::Ambiguous {
        candidates: fake_logins(logins),
    }
}

/// `.git/config` の指定が原因で degraded になった形。
///
/// **`[u]`（指定の解除）と `[r]`（解決し直す）が両方出る degraded。**
/// `DegradedReason::configured_source` が `Some` を返す変種なので、外す対象が在る。
/// 3 人目（`update::account` の `u`、`modals::account_picker` の 2 本）が出たので
/// ここへ寄せた。
pub fn fake_configured_unavailable() -> DegradedReason {
    DegradedReason::ConfiguredUnavailable {
        login: "taro".to_string(),
        source: AccountSource::GitConfig,
        message: "no oauth token found".to_string(),
    }
}

/// ログイン一覧。`gh auth status` が返したぶんの代わり。
///
/// **`test_util` に置くのは 2 人目（`update::account` の選び直し）が出たから。**
/// 1 人目は [`fake_ambiguous`]。
pub fn fake_logins(logins: &[&str]) -> Vec<GhLogin> {
    logins.iter().map(|login| fake_login(login)).collect()
}

/// リモート 1 件。URL は使う側が見ないので、名前から作った偽物でよい。
///
/// **`head_branch` は `None`。** 既定ブランチを見る経路（`Modal::PrCreate` の base）を
/// 踏むテストは、そこだけ `Remote` を自分で組む —— ここで `Some` を既定にすると、
/// **空欄で開く腕**（一度も fetch していないリモート。02）がどのテストからも
/// 踏めなくなる。
pub fn fake_remote(name: &str) -> Remote {
    Remote {
        name: name.to_string(),
        url: format!("ssh://git@example.invalid/{name}.git"),
        head_branch: None,
    }
}

/// ブランチ 1 件。upstream は持たない（要るテストは戻り値のフィールドを足す）。
///
/// oid は [`fake_oid`] の 0 番。ブランチの一覧は名前と印だけで読めるので、
/// テストが oid を気にせずに済むようにする。
pub fn fake_branch(name: &str, is_head: bool) -> Branch {
    Branch {
        name: name.to_string(),
        oid: fake_oid(0),
        upstream: None,
        upstream_config: None,
        ahead: 0,
        behind: 0,
        is_head,
    }
}

/// 追跡 ref から読めた値。位置は指定する（force push の lease に渡る値なので、
/// テストが「どの位置を主張したか」を見られるようにする）。
pub fn fake_upstream(name: &str, oid: Oid) -> Upstream {
    Upstream {
        name: name.to_string(),
        oid,
    }
}

/// 変更のあったファイル 1 件。rename は `old_path` を呼び出し側で足す。
///
/// `fake_commit` と同じく、`app` の遷移テストと `ui` の描画テストの両方から使う。
pub fn fake_entry(path: &str, status: FileStatus) -> FileEntry {
    FileEntry {
        path: path.into(),
        old_path: None,
        status,
    }
}

/// 各区画にファイルを並べたスナップショット。中身は `path` だけが違う `Modified`。
///
/// conflicted は空。conflicted を要るテストは戻り値に足す。
pub fn fake_snapshot(staged: &[&str], unstaged: &[&str], untracked: &[&str]) -> WorktreeSnapshot {
    let entries = |paths: &[&str]| {
        paths
            .iter()
            .map(|path| fake_entry(path, FileStatus::Modified))
            .collect()
    };
    WorktreeSnapshot {
        staged: entries(staged),
        unstaged: entries(unstaged),
        untracked: entries(untracked),
        conflicted: Vec::new(),
    }
}

/// 1 ファイルぶんの diff。`path` と `hunks` 以外は `Modified` の既定値。
///
/// `fake_entry` と同じく、`app` の遷移テストと `ui` の描画テストの両方から使う。
/// 2 箇所で組み立てを持つと、`FileDiff` にフィールドを足したときに片方だけ直しても
/// `cargo test -p <crate>` の単体実行でしか落ちない。
pub fn fake_file_diff(path: &str, hunks: Vec<Hunk>) -> FileDiff {
    FileDiff {
        path: path.into(),
        old_path: None,
        status: FileStatus::Modified,
        binary: false,
        hunks,
    }
}

/// ハンク 1 つ。`lines` は `(種別, 内容)` の並び。
pub fn fake_hunk(header: &str, lines: &[(LineOrigin, &str)]) -> Hunk {
    Hunk {
        header: header.to_string(),
        lines: lines
            .iter()
            .map(|(origin, content)| HunkLine {
                origin: *origin,
                content: content.as_bytes().to_vec(),
            })
            .collect(),
    }
}

/// 中身を作り込んだ [`Commit`]。実リポジトリを作らずに一覧の描画を試すのに使う。
///
/// `index` が Oid の最下位バイトになるので、同じ `index` なら常に同じ Oid。
pub fn fake_commit(index: u8, summary: &str) -> Commit {
    Commit {
        oid: fake_oid(index),
        parents: Vec::new(),
        summary: summary.to_string(),
        author: fake_signature(),
        time: 0,
    }
}

/// 親を持つ [`fake_commit`]。親も索引で指す。
///
/// レーン割り当てを通すテストは `mewgit-core` と `mewgit-tui` の両方にあるので、
/// 組み立てはここに置く。
pub fn fake_commit_with_parents(index: u8, parents: &[u8], summary: &str) -> Commit {
    Commit {
        parents: parents.iter().copied().map(fake_oid).collect(),
        ..fake_commit(index, summary)
    }
}

/// [`fake_commit`] と同じ Oid を持つ詳細。
///
/// `app` の遷移テストと `ui` の描画テストの両方で使うのでここに置く。日時は
/// 固定（1970-01-01 00:00:00 +0000）で、author と committer は同じ値。
pub fn fake_detail(index: u8, message: &str) -> CommitDetail {
    let when = Timestamp {
        seconds: 0,
        offset_minutes: 0,
    };
    CommitDetail {
        oid: fake_oid(index),
        parents: Vec::new(),
        message: message.to_string(),
        author: fake_signature(),
        author_time: when,
        committer: fake_signature(),
        committer_time: when,
    }
}

/// テストの署名。[`TestRepo`] が git config に書き込む名前と同じにしてある。
fn fake_signature() -> Signature {
    Signature {
        name: "mewgit test".to_string(),
        email: "test@example.invalid".to_string(),
    }
}

/// `index` から作る Oid。実リポジトリの Oid とは重ならない。
///
/// 親を持つコミットを組むときに要る（`Commit.parents` は Oid の並び）。
pub fn fake_oid(index: u8) -> Oid {
    let mut bytes = [0u8; 20];
    bytes[19] = index;
    Oid::from_bytes(&bytes).expect("20 バイトなので Oid になる")
}

/// 現ブランチの PR 1 件（[`fake_target`] のリポジトリ）。
///
/// **`ui` からも使う。** PRs ペインの描画を見張るのに同じ値が要るので、
/// `#[cfg(test)] mod tests` ではなくここに置く（下の [`reply`] と同じ理由）。
pub fn fake_pull_request(number: u64, branch: &str, sha: &str) -> PullRequest {
    let repo = Some(RepoRef {
        owner: fake_target().owner,
        repo: fake_target().repo,
    });
    PullRequest {
        number,
        title: "Add adaptive layout".to_string(),
        state: PrStatus::Open,
        draft: false,
        head: RefSpec {
            repo: repo.clone(),
            branch: branch.to_string(),
            sha: sha.to_string(),
        },
        base: RefSpec {
            repo,
            branch: "main".to_string(),
            sha: "basesha".to_string(),
        },
        url: format!("https://github.com/yshrsmz/mewgit/pull/{number}"),
        author: "yshrsmz".to_string(),
        requested_reviewers: Vec::new(),
    }
}

/// [`Msg::PrListLoaded`] を組む。
///
/// **`ui` からも使う**（[`fake_pull_request`] と同じ理由）。`Msg` の形を変えたときに
/// 直す場所を、クレートをまたいで散らさない。
pub fn pr_list_loaded(list: Vec<PullRequest>) -> Msg {
    Msg::PrListLoaded(Ok(list))
}

/// 残量 1 件ぶん。上限は github.com の個人トークンの値（05 の実測表）。
///
/// **`ui` からも使う**（[`fake_pull_request`] と同じ理由）。上限を書く場所が
/// クレートごとに割れると、`is_low` の境目を別々の数で見張ることになる。
pub fn fake_rate_limit(remaining: u32, reset: i64) -> RateLimit {
    RateLimit {
        limit: 5000,
        remaining,
        reset,
    }
}

/// `answers` の対象を、**いまの世代で**頼む `Cmd`。
///
/// **世代を外から書かせない。** 手で `0` と書くと、`PrState::forget` が進めたあとの
/// 状態で「古い返信」を組めなくなるうえ、初期値が変わったときに黙ってずれる。
pub fn fetch_pr_details(
    state: &AppState,
    answers: &[(u64, &str, Result<PrDetail, GithubError>)],
) -> Cmd {
    Cmd::FetchPrDetails {
        epoch: state.prs.epoch(),
        targets: answers
            .iter()
            .map(|(number, sha, _)| (*number, (*sha).to_string()))
            .collect(),
    }
}

/// 扇形を発行して、答えを届けて、終端まで運ぶ（01 の「PR ペイン › 集計」）。
///
/// **`ui` からも使う。** PRs ペインの描画を見張るのに同じ手順が要るので、[`reply`] と
/// 同じくここに置く —— クレートごとに書くと、片方だけが**実物では起きない形**に
/// 退化しても両方緑になる。
///
/// **世代と対象は `ask` から取る。** ランタイムもそうしているので、**頼んだときの
/// 世代を返信が持ち帰る**形がそのまま再現される。手で渡せるようにすると、状態の
/// いまの世代を書いてしまって「古い返信」を一度も踏めなくなる。
///
/// **答えは `ask` と食い違っていてよい。** 飛んでいるあいだに一覧が入れ替わる経路を
/// 踏むのに要る。
pub fn deliver_pr_details(
    state: &mut AppState,
    ask: Cmd,
    answers: &[(u64, &str, Result<PrDetail, GithubError>)],
    aborted: Option<GithubError>,
) -> Cmd {
    let (epoch, targets) = start_pr_details(state, ask, answers);
    update(
        state,
        Msg::PrDetailsFinished {
            epoch,
            aborted: aborted.map(|err| (err, targets)),
        },
    )
}

/// 扇形を発行して答えを届けるところまで。**終端は送らない**ので、ジョブは走ったまま。
///
/// **[`deliver_pr_details`] から割った前半。** 「走っている最中に何かを押す」経路を
/// 踏むテストが 2 人目の利用者になったので寄せた（`CLAUDE.md` の「2 つ目の利用者が
/// 出た時点で共有先を作る」）。手で組み直すと、`AppState::issue` を通す理由
/// （[`reply`] の doc）の写しがもう 1 本増える。
///
/// 返すのは `ask` が刻んだ世代と対象 —— 終端を送るのに要る。
pub fn start_pr_details(
    state: &mut AppState,
    ask: Cmd,
    answers: &[(u64, &str, Result<PrDetail, GithubError>)],
) -> (u64, Vec<(u64, String)>) {
    let Cmd::FetchPrDetails { epoch, targets } = ask.clone() else {
        panic!("扇形の `Cmd` ではありません: {ask:?}");
    };
    // **`AppState::issue` を通す**（[`reply`] と同じ理由）。通さずに `Msg` だけ
    // 届けると、ジョブの登録されていない実物では起きない形になる。
    let _ = state.issue(ask);
    for (number, sha, result) in answers {
        let _ = update(
            state,
            Msg::PrDetailLoaded {
                epoch,
                number: *number,
                sha: (*sha).to_string(),
                result: result.clone(),
            },
        );
    }
    (epoch, targets)
}

/// アカウントと対象が決まり、HEAD が `branch` に居る状態にする。
///
/// **`Msg` を通さずに置く。** ここまで運ぶには `Msg::RepoInfoLoaded` と
/// `Msg::AccountResolved` の両方が要り、**その 2 本の順序そのものを見張るテストが
/// `app::update::prs` にある**。土台が片方の順序を焼き込むと、そちらが空振りする。
pub fn put_on_branch(state: &mut AppState, branch: &str) {
    state.repo.target = TargetState::Resolved(fake_target());
    state.account = AccountState::Resolved(fake_account("taro"));
    state.repo.head = Some(HeadInfo::Branch {
        name: branch.to_string(),
        oid: fake_oid(0),
        upstream_remote: None,
    });
}

/// `cmd` を発行してから、その返信を届ける。
///
/// **ランタイムでは `Cmd` が必ず [`AppState::issue`] を通る**（`update` の出口）。
/// 通さずに返信だけ届けると刻印の無いジョブの返信になり、「書き込みより前に頼まれた
/// もの」として捨てられる（01 の「実行中ジョブ › 世代」）。書き込みやキー入力から
/// `Cmd` が出ている経路では、そちらが既に発行しているので要らない。
///
/// **`mewgit-tui` からも呼ぶ `app` のテストヘルパは、ここが唯一の置き場所。**
/// `mewgit-core` の中の `mod tests` に書くと `mewgit-tui` から呼べず、同じ理由の
/// コメントごと写すことになる。**`app::update` の中だけで使うものは
/// `app/update/test_support.rs`**（`AppState` の組み立てとキー送り）。
pub fn reply(state: &mut AppState, cmd: Cmd, msg: Msg) -> Cmd {
    let _ = state.issue(cmd);
    update(state, msg)
}
