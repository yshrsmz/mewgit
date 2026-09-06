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
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// コミット日時の起点（2023-11-14T22:13:20Z）。`git` を 1 回叩くごとに [`TICK`] 秒進める。
///
/// 実時間を使うと、続けて作ったコミットが同じ秒に落ちて `Sort::TIME` での並びが
/// 揺れる。テストごとに固定の値を与えて順序を決める。
const CLOCK_START: i64 = 1_700_000_000;

/// `git` 1 回あたりに進める秒数。
const TICK: i64 = 60;

use crate::git_read::{Commit, CommitDetail, Oid, Signature, Timestamp};
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
        let out = self.try_git(args);
        assert!(
            out.status.success(),
            "git {} が失敗しました: {}",
            shown(args),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .expect("git の出力が UTF-8 ではありません")
            .trim_end()
            .to_string()
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
            // 実行環境のユーザー設定を持ち込まない。特に `commit.gpgsign` が有効だと
            // 署名を待ってテストが固まる。
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("no-such-gitconfig"))
            .env("HOME", &self.home)
            .env("GIT_AUTHOR_NAME", "mewgit test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "mewgit test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .env("GIT_TERMINAL_PROMPT", "0");

        cmd.output().expect("git を実行できませんでした")
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

    /// 現在の HEAD の Oid。
    pub fn head_oid(&self) -> Oid {
        self.rev_parse("HEAD")
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
}

/// panic のメッセージに出す argv。非 UTF-8 の引数も渡せるので lossy で並べる。
fn shown<S: AsRef<OsStr>>(args: &[S]) -> String {
    args.iter()
        .map(|arg| arg.as_ref().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
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
