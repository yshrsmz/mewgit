//! 統合テスト用の一時リポジトリ。
//!
//! リポジトリは `git` CLI で作る。git2 で作ると「git が作ったものを git2 で読めるか」
//! ではなく「git2 が書いたものを git2 で読めるか」のテストになるため。
//!
//! ワークツリーのある [`TestRepo`] と [`BareRepo`] は型を分ける。bare にはワークツリーも
//! `.git` ディレクトリも無いので、`commit` や `write_git_file` を呼べてしまうと実行時に
//! `git` の stderr で落ちるだけになる。分けておけばコンパイルエラーで気付く。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use mewgit_core::git_read::Oid;
use tempfile::TempDir;

/// 一時ディレクトリと、実行環境の設定を持ち込まない `git` の実行。
///
/// [`TestRepo`] と [`BareRepo`] で共通する部分だけを持つ。
struct Sandbox {
    /// drop で削除するために持つ。
    _dir: TempDir,
    root: PathBuf,
    home: PathBuf,
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
        };
        sandbox.git(init_args);
        sandbox
    }

    /// `git` を実行し、標準出力を末尾の改行を落として返す。失敗したら panic する。
    fn git(&self, args: &[&str]) -> String {
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
            .env("GIT_TERMINAL_PROMPT", "0");

        let out = cmd.output().expect("git を実行できませんでした");
        assert!(
            out.status.success(),
            "git {} が失敗しました: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .expect("git の出力が UTF-8 ではありません")
            .trim_end()
            .to_string()
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
    pub fn git(&self, args: &[&str]) -> String {
        self.sandbox.git(args)
    }

    /// ファイルを 1 つ書き換えてコミットし、その Oid を返す。
    pub fn commit(&self, message: &str) -> Oid {
        fs::write(self.path().join("file.txt"), format!("{message}\n"))
            .expect("ファイルを書けませんでした");
        self.git(&["add", "file.txt"]);
        self.git(&["commit", "-m", message]);
        self.head_oid()
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
