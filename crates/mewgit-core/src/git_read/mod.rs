//! `git_read` モジュール。設計は `docs/design/02-git-read.md` を参照。
//!
//! git2 による読み取り専用の層。書き込みは一切行わない（不変ルール 1）。
//!
//! ここには層をまたいで受け渡す値型を置き、読み取りの実装は `repo` などの
//! サブモジュールに置く。status・diff は M2 で実装する。

mod log;
mod repo;

use std::collections::HashMap;
use std::path::PathBuf;

pub use git2::Oid;
pub use log::LogSession;
pub use repo::RepoReader;

/// 表示にしか使わない文字列を UTF-8 に寄せる。
///
/// 識別子（ref 名・リモート名・パス）は非 UTF-8 なら項目ごと落とすが、表示専用の
/// 文字列は読めない文字が混ざるほうが情報が多い（「非 UTF-8 の扱い」）。方針を
/// 宣言している場所と実装を同じところに置く。
pub(super) fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `git_read` 層のエラー。
///
/// `Clone` / `PartialEq` は付かない（`git2::Error` が実装していない）。
#[derive(Debug, thiserror::Error)]
pub enum GitReadError {
    /// `open` の失敗だけはパスを添える。`discover` が上位ディレクトリまで探すので、
    /// どこを開こうとしたのかが無いと原因を追えない。
    #[error("{} を開けませんでした: {source}", .path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: git2::Error,
    },
    #[error("{} は bare リポジトリです。ワークツリーのあるリポジトリを指定してください", .path.display())]
    BareRepository { path: PathBuf },
    /// HEAD が非 UTF-8 のブランチを指している。他の ref と違って落とすと
    /// detached と区別が付かなくなるので、ここだけはエラーにする。
    #[error("HEAD が指すブランチ名が UTF-8 ではありません")]
    NonUtf8Head,
    #[error("git の読み取りに失敗しました: {0}")]
    Git(#[from] git2::Error),
}

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

/// [`LogSession::next_page`] が返す 1 ページ分。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogPage {
    pub commits: Vec<Commit>,
    /// これ以上読むコミットが無い。walk を 1 つ先読みして決めるので、ちょうど
    /// ページ幅ぴったりで終わったときも立つ。
    pub done: bool,
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

/// Oid からその位置にある ref を引く索引。グラフ行の右にラベルを並べるのに使う（06）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefIndex {
    labels: HashMap<Oid, Vec<RefLabel>>,
}

impl RefIndex {
    /// 並び順の確定はここでやる。索引を組む経路が増えても並べ替えを忘れられないように
    /// するため（git2 の ref の列挙順は安定していない）。
    pub(crate) fn new(mut labels: HashMap<Oid, Vec<RefLabel>>) -> Self {
        for labels in labels.values_mut() {
            labels.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
        }
        Self { labels }
    }

    /// そのコミットに付いている ref。無ければ空。種別・名前順に並んでいる。
    pub fn labels(&self, oid: Oid) -> &[RefLabel] {
        self.labels.get(&oid).map_or(&[], Vec::as_slice)
    }
}

/// ref 1 件の表示用ラベル。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefLabel {
    /// ローカルブランチ。`main`。
    Local { name: String, is_head: bool },
    /// リモート追跡ブランチ。`origin/main`。remote 名とブランチ名を分けないのは、
    /// `ui` が `{origin/main}` と一続きで描くだけのため。
    Remote { name: String },
    /// タグ。`v1.0`。
    Tag { name: String },
}

impl RefLabel {
    /// 表示名。種別ごとの飾り（`[main]` / `{origin/main}` / `<v1.0>`）は `ui` が付ける。
    pub fn name(&self) -> &str {
        match self {
            RefLabel::Local { name, .. } | RefLabel::Remote { name } | RefLabel::Tag { name } => {
                name
            }
        }
    }

    /// 並び順。種別（ローカル → リモート → タグ）、同種内は名前順。
    fn sort_key(&self) -> (u8, &str) {
        let kind = match self {
            RefLabel::Local { .. } => 0,
            RefLabel::Remote { .. } => 1,
            RefLabel::Tag { .. } => 2,
        };
        (kind, self.name())
    }
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
