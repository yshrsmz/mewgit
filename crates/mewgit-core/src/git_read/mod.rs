//! `git_read` モジュール。設計は `docs/design/02-git-read.md` を参照。
//!
//! git2 による読み取り専用の層。書き込みは一切行わない（不変ルール 1）。
//!
//! 現時点では `app` の `AppState` / `Msg` / `Cmd` が参照する型だけを定義している。
//! `RepoReader` と実際の読み取りは M1 以降で実装する。

use std::path::PathBuf;

pub use git2::Oid;

/// グラフ 1 行分のコミット。本文とファイル一覧は選択時に遅延取得する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub oid: Oid,
    pub parents: Vec<Oid>,
    pub summary: String,
    pub author: Signature,
    /// author time。Unix epoch からの秒数。
    pub time: i64,
}

/// コミットの author / committer。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub name: String,
    pub email: String,
}

/// リモート 1 件。`url` は `.git/config` の生の値。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub url: String,
}

/// HEAD の状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadInfo {
    Branch {
        name: String,
        oid: Oid,
    },
    Detached(Oid),
    /// コミットが 1 つも無いリポジトリ。
    Unborn {
        name: String,
    },
}

/// ローカルブランチ 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub oid: Oid,
    /// 追跡先（例: `origin/main`）。設定されていなければ `None`。
    pub upstream: Option<String>,
    /// upstream に対する ahead / behind。
    pub ahead: usize,
    pub behind: usize,
    pub is_head: bool,
}

/// ワーキングツリーの状態。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeSnapshot {
    pub staged: Vec<FileEntry>,
    pub unstaged: Vec<FileEntry>,
    pub untracked: Vec<FileEntry>,
    pub conflicted: Vec<FileEntry>,
}

/// ワーキングツリーのどの区画か。[`WorktreeSnapshot`] のフィールドと 1 対 1。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WorktreeSection {
    Staged,
    #[default]
    Unstaged,
    Untracked,
    Conflicted,
}

impl WorktreeSnapshot {
    /// 区画の中身。網羅 `match` なので、区画を足したら実装漏れがコンパイルエラーになる。
    pub fn section(&self, section: WorktreeSection) -> &[FileEntry] {
        match section {
            WorktreeSection::Staged => &self.staged,
            WorktreeSection::Unstaged => &self.unstaged,
            WorktreeSection::Untracked => &self.untracked,
            WorktreeSection::Conflicted => &self.conflicted,
        }
    }
}

/// 変更のあったファイル 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: PathBuf,
    pub status: FileStatus,
}

/// ファイルの変更種別。
///
/// `git2::Delta` をそのまま使わないのは、`Unmodified` / `Ignored` / `Unreadable` が
/// この UI では起きえず、全変種の `match` を強いられるため。逆に `Copied` と
/// `Typechange` は git が実際に出すので落とさない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    /// 通常ファイル ↔ symlink の入れ替わり。
    Typechange,
    Untracked,
    Conflicted,
}

/// diff の対象。`path` を指定すると 1 ファイルに絞る。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffTarget {
    IndexVsWorktree(Option<PathBuf>),
    HeadVsIndex(Option<PathBuf>),
    Commit {
        oid: Oid,
        path: Option<PathBuf>,
    },
    Range {
        from: Oid,
        to: Oid,
        path: Option<PathBuf>,
    },
}

/// 1 ファイル分の diff。
///
/// ハンクを `Vec<Hunk>` に平坦化せずファイル単位で持つ。`GitOp::StageHunk` に渡す
/// `path` を復元できなくなるため。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: PathBuf,
    pub status: FileStatus,
    pub hunks: Vec<Hunk>,
}

/// diff のハンク 1 つ。`git_exec` がパッチとして再構成できる形で持つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// `@@ -1,4 +1,6 @@` の行。
    pub header: String,
    pub lines: Vec<HunkLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkLine {
    pub origin: LineOrigin,
    pub content: String,
}

/// ハンク内の 1 行の種別。
///
/// `NoEol` 系は `\ No newline at end of file` を伴う行。`git_exec` がパッチを
/// 再構成するときにこのマーカーを落とせないので、3 種類とも表現できるようにしている。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineOrigin {
    Context,
    Addition,
    Deletion,
    ContextNoEol,
    AdditionNoEol,
    DeletionNoEol,
}

/// stash 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashEntry {
    /// `stash@{index}` の index。
    pub index: usize,
    pub message: String,
    pub oid: Oid,
}

/// 進行中の操作。continue / skip / abort 以外の書き込みを拒否する判断に使う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InProgressOp {
    Rebase {
        interactive: bool,
        current: usize,
        total: usize,
        onto: String,
    },
    CherryPick {
        oid: Oid,
    },
    Revert {
        oid: Oid,
    },
    Merge {
        oid: Oid,
    },
}
