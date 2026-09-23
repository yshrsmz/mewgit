//! `git_read` モジュール。設計は `docs/design/02-git-read.md` を参照。
//!
//! git2 による読み取り専用の層。書き込みは一切行わない（不変ルール 1）。
//!
//! ここには層をまたいで受け渡す値型を置き、読み取りの実装は `repo` などの
//! サブモジュールに置く。**その値型を読む側で表示に直す関数も置く**（`lossy` /
//! [`short_oid`]）。`ui` だけが読み手だったころは `ui` にあったが、`app` が
//! モーダルの文言を組むようになって 2 人目が出た（01 の「1 行入力」）。

mod diff;
mod log;
mod repo;
mod status;
mod thread;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub use git2::Oid;
pub use log::LogSession;
pub use repo::RepoReader;
pub use thread::ReaderHandle;

/// 短縮 Oid の桁数。`git log --oneline` と同じ 7 桁。
pub const SHORT_OID: usize = 7;

/// 先頭 [`SHORT_OID`] 桁。
///
/// **`git_read` に置くのは、`ui` の外にも読み手が出たから。** グラフの行と
/// コミット詳細の親の一覧のほかに、`app` がブランチ作成モーダルのタイトル
/// （`ブランチを作成 (a1b2c3d から)`）を組む（01 の「1 行入力」）。2 本に分けると
/// 桁数を変えたときに片方だけ古いままになる。
///
/// **`Oid::to_string()` を切り詰めない。** あちらは 40 桁を組んでから 33 桁を捨てる。
/// グラフは可視行ぶんを毎フレーム作るので、そのぶんの確保がまるごと無駄になる。
///
/// **同じ理由で、ここでも余分な桁を組んでから切らない。** バイト単位に回して
/// `truncate` する形は、奇数桁（7）だと 8 桁目を push した時点で 7 バイトの確保を
/// 使い切って伸長が走る（`with_capacity(7)` に 8 文字入れると容量が 14 になり、
/// `truncate` しても戻らない。rustc 1.90 で実測）。ニブル単位に回せば確保は 1 回。
pub fn short_oid(oid: &Oid) -> String {
    let bytes = oid.as_bytes();
    let mut text = String::with_capacity(SHORT_OID);
    // 1 バイトが 2 桁。偶数桁が上位ニブル、奇数桁が下位ニブル。
    for digit in 0..SHORT_OID {
        let byte = bytes[digit / 2];
        text.push(hex(if digit % 2 == 0 {
            byte >> 4
        } else {
            byte & 0x0f
        }));
    }
    text
}

/// 下位 4 ビットを 16 進 1 桁に。呼ぶ側がマスクしているので添字は外れない。
fn hex(nibble: u8) -> char {
    b"0123456789abcdef"[usize::from(nibble)] as char
}

/// 表示にしか使わない文字列を UTF-8 に寄せる。
///
/// 識別子（ref 名・リモート名・パス）は非 UTF-8 なら項目ごと落とすが、表示専用の
/// 文字列は読めない文字が混ざるほうが情報が多い（「非 UTF-8 の扱い」）。方針を
/// 宣言している場所と実装を同じところに置く。
pub(super) fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// git2 の delta から変更のあったファイル 1 件を組み立てる。出しても操作できない
/// ものは `None`。
///
/// **`status` と `diff` の両方がここを通る。** [`FileDiff`] は [`FileEntry`] と同じ
/// 3 つを頭に持つので、別々に書くと「非 UTF-8 をどう落とすか」「`old_path` を入れる
/// のはどの種別か」が静かにずれる。
pub(super) fn file_entry(delta: &git2::DiffDelta<'_>) -> Option<FileEntry> {
    let status = file_status(delta.status())?;
    // 削除でも libgit2 は `new_file` にパスを入れる。
    let path = utf8_path(delta.new_file().path_bytes())?;
    let old_path = match status {
        // 旧パスが非 UTF-8 なら、`git_exec` の argv に載せられないのでファイルごと落とす。
        FileStatus::Renamed | FileStatus::Copied => Some(utf8_path(delta.old_file().path_bytes())?),
        _ => None,
    };
    Some(FileEntry {
        path,
        old_path,
        status,
    })
}

/// git2 の delta 種別を層の値型にする。
///
/// `Unmodified` / `Ignored` / `Unreadable` は [`FileStatus`] に対応が無く、この UI で
/// 出しても操作できないので `None`。
fn file_status(delta: git2::Delta) -> Option<FileStatus> {
    match delta {
        git2::Delta::Added => Some(FileStatus::Added),
        git2::Delta::Deleted => Some(FileStatus::Deleted),
        git2::Delta::Modified => Some(FileStatus::Modified),
        git2::Delta::Renamed => Some(FileStatus::Renamed),
        git2::Delta::Copied => Some(FileStatus::Copied),
        git2::Delta::Typechange => Some(FileStatus::Typechange),
        git2::Delta::Untracked => Some(FileStatus::Untracked),
        git2::Delta::Conflicted => Some(FileStatus::Conflicted),
        git2::Delta::Unmodified | git2::Delta::Ignored | git2::Delta::Unreadable => None,
    }
}

/// 識別子として使うパスを `PathBuf` にする。非 UTF-8 なら `None`。
///
/// 呼び出し側はそのファイルを一覧ごと落とす。Unix の `PathBuf` は任意のバイト列を
/// 持てるが、`git_exec` の argv に載せられず、UI で選べても操作できない
/// （「非 UTF-8 の扱い」）。
fn utf8_path(bytes: Option<&[u8]>) -> Option<PathBuf> {
    Some(PathBuf::from(std::str::from_utf8(bytes?).ok()?))
}

/// git2 の署名を層の値型にする。名前とメールは表示にしか使わないので lossy で残す。
pub(super) fn signature(sig: &git2::Signature<'_>) -> Signature {
    Signature {
        name: lossy(sig.name_bytes()),
        email: lossy(sig.email_bytes()),
    }
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
    /// リーダースレッドが応答しない。チャネルが切れたら待たずにこれを返す。
    /// 待ち続けると、呼び出しを包む非同期タスクが二度と完了しない。
    #[error("リポジトリの読み取りスレッドが停止しています")]
    ReaderGone,
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

/// コミット 1 件の詳細。グラフの行が持たない値だけをまとめる。
///
/// グラフで選択されたときに [`RepoReader::commit_detail`] で遅延取得する。ファイル一覧は
/// 含めない（`diff(DiffTarget::Commit)` の担当。02 の「コミットの詳細」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDetail {
    pub oid: Oid,
    pub parents: Vec<Oid>,
    /// メッセージ全文（summary + 本文）。末尾の改行は落とす。
    pub message: String,
    pub author: Signature,
    pub author_time: Timestamp,
    pub committer: Signature,
    pub committer_time: Timestamp,
}

/// コミットに刻まれた日時。
///
/// オフセットを一緒に持つのは、`git show` と同じくコミット自身のタイムゾーンで
/// 表示するため。裸の秒数だけだと、表示が見る人の環境のタイムゾーンに寄る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamp {
    /// Unix epoch からの秒数。
    pub seconds: i64,
    /// UTC からのオフセット（分）。`+09:00` なら `540`。
    pub offset_minutes: i32,
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
    /// `refs/remotes/<name>/HEAD` が指すブランチの短縮名 —— **そのリモートが言う
    /// 既定ブランチ**（02 の「remotes」）。読み手は `Modal::PrCreate` の base の
    /// 初期値（01 の「PR 作成（`N`）」）と、fork の PR を取り出すときのブランチ名
    /// （05 の「ブランチ名」）。
    ///
    /// **`None` は「symref が無い」。** clone 直後と `git fetch` のあとには在るが、
    /// `git remote add` しただけで一度も fetch していないリポジトリには無い
    /// （実測 2026-09-21、git 2.55）。**どちらの読み手も無いことを理由にしない**
    /// —— PR 作成は空欄で開いて人に打たせ、取り出しは PR の base とも比べる。
    pub head_branch: Option<String>,
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

    /// リモート追跡ブランチ `name`（`origin/main` の短縮形）が指すコミット。無ければ `None`。
    ///
    /// 読み手は PR 作成の `head` の候補（01 の「head の組み立て」）。**索引は Oid で
    /// 引く形なので、名前からは全件を舐める** —— 引くのは `N` を押したときと確定の
    /// ときだけなので、名前の索引を別に持って 2 つを揃え続けるより安い。
    pub fn remote_oid(&self, name: &str) -> Option<Oid> {
        self.labels.iter().find_map(|(oid, labels)| {
            labels
                .iter()
                .any(|label| matches!(label, RefLabel::Remote { name: n } if n == name))
                .then_some(*oid)
        })
    }
}

/// `(Oid, RefLabel)` の並びから索引を組む。同じ Oid のラベルはまとめられる。
///
/// `refs()` を通さずに索引を作れるようにしてあるのは、`ui` のテストがラベルの
/// 描画を実リポジトリ無しで表現できるようにするため。並べ替えは [`RefIndex::new`]
/// を通るので、どちらの経路でも順序は同じになる。
impl FromIterator<(Oid, RefLabel)> for RefIndex {
    fn from_iter<I: IntoIterator<Item = (Oid, RefLabel)>>(iter: I) -> Self {
        let mut labels: HashMap<Oid, Vec<RefLabel>> = HashMap::new();
        for (oid, label) in iter {
            labels.entry(oid).or_default().push(label);
        }
        RefIndex::new(labels)
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
        /// `branch.<name>.remote` / `branch.<name>.merge`（02 の「head」）。
        ///
        /// **[`Branch::upstream_config`] と同じ設定を、別のジョブがもう一度読む。**
        /// 読み手は `app` の「現ブランチの PR」で、**fork から出した PR を当てる**のに
        /// 要る（01）。あちらの `BranchState` から取ると、`Cmd::LoadBranches` の返信と
        /// PR 一覧のどちらが先に届くかで**座り直しの答えが変わる** —— 一覧が 1 度
        /// 届いたら以後は選んでいる番号で座るので、**間違えても二度と直らない**。
        ///
        /// **対で載せる。** `merge` の読み手は PR 作成の `head` の候補と「現ブランチの
        /// PR」で、どちらも追跡設定が fork の URL のときだけ読む —— そこに在るブランチの
        /// 名前は**ローカルの名前ではなく `merge` のほう**（01 の「現ブランチの PR」の
        /// 「比べる名前」）。
        /// `RepoReader::upstream_config`（private なのでリンクにできない）をそのまま
        /// 呼ぶので、**片方でも欠けていれば `None`**（同じ設定の読み方を 2 通り持たない
        /// ため）。
        upstream: Option<UpstreamConfig>,
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
    /// 追跡 ref（`refs/remotes/origin/x`）から読んだ名前と位置。
    ///
    /// remote-tracking ref から作るので、`fetch --prune` で ref が消えていれば
    /// 設定が残っていても `None` になる。**push の行き先はここから決めない** —
    /// そちらは [`Branch::upstream_config`]。
    pub upstream: Option<Upstream>,
    /// `branch.<name>.remote` / `branch.<name>.merge` に書かれた追跡設定。
    ///
    /// **[`Branch::upstream`] とは `None` になる条件が違う。** こちらは config だけを
    /// 見るので、追跡 ref が消えていても残る。あちらの `None` は 4 つの理由に落ちていて
    /// （02 の「ブランチ一覧」）、そのうち「設定が無い」だけを取り出せない。
    /// **`-u` を付けるかはここで決める**（02）。
    pub upstream_config: Option<UpstreamConfig>,
    /// upstream に対する ahead / behind。
    pub ahead: usize,
    pub behind: usize,
    pub is_head: bool,
}

/// 追跡 ref から読めた値。
///
/// **名前と位置を 1 つの `Option` に入れる。** 両方 remote-tracking ref から作るので、
/// 別々の `Option` にすると「名前はあるが位置は無い」という起こらない形が型で組めて、
/// 揃っていることを散文で約束することになる（`GitOp::Push` の `lease` と同じ理由。03）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    /// 短縮名（例: `origin/main`）。**表示のための値**（02）。
    pub name: String,
    /// 指している位置。force push の `--force-with-lease=<dst>:<lease>` に渡す
    /// （01 の「force push」）。
    pub oid: Oid,
}

/// ブランチの追跡設定。`git_exec` の `GitOp::Push` を組むのに使う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamConfig {
    /// `branch.<name>.remote`。ローカル追跡では `"."`。
    pub remote: String,
    /// `branch.<name>.merge`。リモート側の**完全な** ref 名（`refs/heads/main`）。
    pub merge: String,
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
    /// 各区画を `path` 順に並べ替える。
    ///
    /// libgit2 の列挙順には依存しない（[`RefIndex::new`] と同じ理由）。区画を手で
    /// 並べる代わりに分解束縛にしてあるのは、区画を足したときに並べ替え漏れが
    /// コンパイルエラーになるようにするため（[`section`](Self::section) と同じ狙い）。
    pub(crate) fn sort_sections(&mut self) {
        let Self {
            staged,
            unstaged,
            untracked,
            conflicted,
        } = self;
        for section in [staged, unstaged, untracked, conflicted] {
            section.sort_by(|a, b| a.path.cmp(&b.path));
        }
    }

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
    /// rename のときだけ入る元のパス。
    ///
    /// 無いと `FileStatus::Renamed` の行が「どこから来たか」を出せず、staged rename の
    /// unstage も組み立てられない（旧パスと新パスの両方を argv に載せる必要がある）。
    pub old_path: Option<PathBuf>,
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

impl DiffTarget {
    /// 絞り込みのパス。全変種が持つので、変種ごとの `match` を呼び出し側に配らない。
    pub fn path(&self) -> Option<&Path> {
        match self {
            DiffTarget::IndexVsWorktree(path)
            | DiffTarget::HeadVsIndex(path)
            | DiffTarget::Commit { path, .. }
            | DiffTarget::Range { path, .. } => path.as_deref(),
        }
    }
}

/// 1 ファイル分の diff。
///
/// ハンクを `Vec<Hunk>` に平坦化せずファイル単位で持つ。`GitOp::StageHunk` に渡す
/// `path` を復元できなくなるため。
/// 頭の 3 つは [`FileEntry`] と同じ値で、どちらも [`file_entry`] が組み立てる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: PathBuf,
    /// rename のときだけ入る元のパス。[`FileEntry::old_path`] と同じ。
    pub old_path: Option<PathBuf>,
    pub status: FileStatus,
    /// バイナリと判定された。このとき `hunks` は空。
    ///
    /// 「mode だけの変更」も `hunks` が空になるので、区別はこのフラグで付ける。
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

/// diff のハンク 1 つ。`git_exec` がパッチとして再構成できる形で持つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// `@@ -1,4 +1,6 @@ fn foo() {` の行から末尾の改行を落としたもの。
    ///
    /// `@@` の後ろの関数コンテキストも残す。ここだけは非 UTF-8 でも lossy でよい。
    /// `git apply` は 2 つ目の `@@` より後ろを読まないため。
    pub header: String,
    pub lines: Vec<HunkLine>,
}

/// ハンク内の 1 行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkLine {
    pub origin: LineOrigin,
    /// 行末の改行を落とした中身。
    ///
    /// **`String` ではなく `Vec<u8>`。** ハンクは表示専用ではなく `git apply --cached` に
    /// 戻す入力でもあるので、非 UTF-8 のファイルを lossy で通すとステージした結果が
    /// 元のファイルと変わる（02 の「非 UTF-8 の扱い」の例外）。描くときに `ui` が
    /// その場で lossy にする。
    pub content: Vec<u8>,
}

/// ハンク内の 1 行の種別。
///
/// `NoEol` 系は `\ No newline at end of file` を伴う行、つまり改行で終わっていない行。
/// libgit2 はこのマーカーを独立した行として出すが、こちらは行の種別に畳んで持つ
/// （02 の「ハンクの持ち方」）。`git_exec` がパッチを組み直すときにマーカーを
/// 復元できるよう、3 種類とも表現できるようにしている。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineOrigin {
    Context,
    Addition,
    Deletion,
    ContextNoEol,
    AdditionNoEol,
    DeletionNoEol,
}

impl LineOrigin {
    /// 行頭に置く記号。
    ///
    /// **パッチに戻す `git_exec::hunk_patch` と、画面に出す `ui` が同じものを使う。**
    /// 別々に書くと、片方だけ直したときに「画面では文脈行、パッチでは追加行」に
    /// なりうる。色は `ui` が `origin` から別に決める（`theme` は core に無い）。
    pub fn mark(self) -> char {
        match self {
            LineOrigin::Context | LineOrigin::ContextNoEol => ' ',
            LineOrigin::Addition | LineOrigin::AdditionNoEol => '+',
            LineOrigin::Deletion | LineOrigin::DeletionNoEol => '-',
        }
    }

    /// 改行で終わっていない行か。
    ///
    /// パッチに戻すとき、この行の後ろにだけ `\ No newline at end of file` を足す
    /// （02 の「ハンクの持ち方」）。**読むのは `git_exec::hunk_patch` だけ**で、
    /// [`mark`](Self::mark) のように 2 者で共有する語彙ではない。ここに置くのは
    /// 変種の並びを知っているのが `git_read` だからで、`origin` の 6 変種を
    /// `git_exec` で数え直させないため。
    pub fn is_no_eol(self) -> bool {
        matches!(
            self,
            LineOrigin::ContextNoEol | LineOrigin::AdditionNoEol | LineOrigin::DeletionNoEol
        )
    }
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

#[cfg(test)]
mod tests {
    use super::{short_oid, Oid, SHORT_OID};

    #[test]
    fn a_short_oid_is_the_first_seven_hex_digits() {
        // `Oid::to_string()` を切り詰めるのをやめて自前で 16 進を組んでいるので、
        // 桁の並びが git と同じであることを見張る。奇数桁（7）の切り方も含む。
        //
        // **先頭が 0 で埋まった Oid だけでは足りない。** 上下のニブルを入れ替えても
        // 通ってしまうので、両方が立っている値を混ぜる。
        for hex in [
            "0000000000000000000000000000000000000000",
            "0f1e2d3c4b5a69788796a5b4c3d2e1f001234567",
            "ffffffffffffffffffffffffffffffffffffffff",
            "a1b2c3d4e5f60718293a4b5c6d7e8f9001122334",
        ] {
            let oid = Oid::from_str(hex).expect("40 桁の 16 進");
            assert_eq!(short_oid(&oid), hex[..SHORT_OID], "{hex}");
        }
    }
}
