//! 統合テスト用の一時リポジトリ。
//!
//! リポジトリは `git` CLI で作る。git2 で作ると「git が作ったものを git2 で読めるか」
//! ではなく「git2 が書いたものを git2 で読めるか」のテストになるため。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use mewgit_core::git_read::Oid;
use tempfile::TempDir;

/// テスト用の一時リポジトリ。`TempDir` が drop されると消える。
pub struct TestRepo {
    /// drop で削除するために持つ。
    _dir: TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl TestRepo {
    /// ワークツリーのある空のリポジトリを作る。デフォルトブランチは `main`。
    pub fn init() -> Self {
        Self::new(&["init", "-b", "main"])
    }

    /// bare リポジトリを作る。
    pub fn init_bare() -> Self {
        Self::new(&["init", "--bare", "-b", "main"])
    }

    fn new(init_args: &[&str]) -> Self {
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

        let repo = Self {
            _dir: dir,
            root,
            home,
        };
        repo.git(init_args);
        repo
    }

    /// リポジトリのルート。bare なら `.git` は無く、ここが bare リポジトリそのもの。
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// `git` を実行し、標準出力を末尾の改行を落として返す。失敗したら panic する。
    pub fn git(&self, args: &[&str]) -> String {
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

    /// ファイルを 1 つ書き換えてコミットし、その Oid を返す。
    pub fn commit(&self, message: &str) -> Oid {
        fs::write(self.root.join("file.txt"), format!("{message}\n"))
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
        fs::write(self.root.join(".git").join(name), content)
            .unwrap_or_else(|err| panic!(".git/{name} を書けませんでした: {err}"));
    }

    /// リビジョン指定を Oid に解決する。
    pub fn rev_parse(&self, rev: &str) -> Oid {
        let out = self.git(&["rev-parse", rev]);
        Oid::from_str(&out).unwrap_or_else(|err| panic!("{rev} の Oid を読めませんでした: {err}"))
    }
}
