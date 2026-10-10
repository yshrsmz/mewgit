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
mod progress;
mod repo;
mod status;
mod thread;
mod watch;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

pub use git2::Oid;
pub use log::LogSession;
use progress::in_progress;
pub use progress::{last_done_reword, reword_message_path};
pub use repo::RepoReader;
pub use thread::ReaderHandle;
pub use watch::{FileStamp, PathStamp, RepoFingerprint};

/// stash の ref の完全な名前。一覧（reflog）と外の変更の指紋が読む。
pub(crate) const STASH_REF: &str = "refs/stash";

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
    /// detached と区別が付かなくなるので、ここだけはエラーにする（`branches` が返す）。
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

impl Timestamp {
    /// いまの時刻を、**見る人の環境のタイムゾーン**で。
    ///
    /// 通知に刻む時刻に使う（01 の「通知」）。コミットの日時と違って、刻む相手は
    /// この端末の前にいる人なので、ローカルのオフセットで持つ。
    ///
    /// **オフセットは libgit2 に数えさせる。** `git_signature_now` は UTC の `tm` を
    /// `mktime` に渡して差を取るので、夏時間も含めたローカルのオフセットが返る
    /// （libgit2 1.9 の `signature.c` の `current_time`）。そのためだけに日時の
    /// クレートを足さない（07 の「詳細の展開」と同じ判断）。呼ぶたびに数え直すので、
    /// 実行中に夏時間をまたいでも追随する。**名前とメールは意味を持たない** ——
    /// `Signature` を作らないと時刻を取り出せないので置いているだけ。
    pub fn now() -> Self {
        // 名前もメールも空でなく `<` `>` を含まない定数なので、`Err` は来ない。
        git2::Signature::now("mewgit", "mewgit")
            .expect("定数の名前とメールは常に通る")
            .when()
            .into()
    }
}

/// git2 の日時を層の値型にする。オフセットは git2 の値が持っていたもの
/// （コミットならコミットに刻まれたもの。02）。
impl From<git2::Time> for Timestamp {
    fn from(time: git2::Time) -> Self {
        Self {
            seconds: time.seconds(),
            offset_minutes: time.offset_minutes(),
        }
    }
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
    /// ローカルブランチ。`main`。**HEAD かどうかは持たない** —— どの行が HEAD かは
    /// [`BranchList`] だけが答える（02 の「refs とラベル」）。
    Local { name: String },
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
            RefLabel::Local { name } | RefLabel::Remote { name } | RefLabel::Tag { name } => name,
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

/// HEAD の状態。[`BranchList::head`] が一覧から導く。
///
/// **ブランチに居るときは HEAD の行そのものを指す。** 名前も oid も追跡設定も、別の
/// 置き場に写さない（02 の「head」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Head<'a> {
    Branch(&'a Branch),
    Detached(Oid),
    /// コミットが 1 つも無いリポジトリ。名前は `HEAD` の symbolic target から取る。
    Unborn(&'a str),
}

impl<'a> Head<'a> {
    /// HEAD の行。detached / unborn では `None`。
    pub fn branch(self) -> Option<&'a Branch> {
        match self {
            Head::Branch(branch) => Some(branch),
            Head::Detached(_) | Head::Unborn(_) => None,
        }
    }
}

/// 一覧に HEAD の行が無いときの HEAD。[`BranchList::new`] が、行が無いときだけ求める。
///
/// **`Branch` の腕を持たない。** ブランチに居ることは HEAD の行があることから導けるので、
/// 腕を持たせると「`Branch` なのに行が無い」組が型で組める（02 の「head」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowlessHead {
    Detached(Oid),
    Unborn { name: String },
}

/// [`RepoReader::branches`] の答え。ローカルブランチの一覧と HEAD の状態。
///
/// **フィールドは private で、組み立ては [`BranchList::new`] だけ。** どの行が HEAD かは
/// ここだけが持ち（`Branch` に旗を持たせない）、HEAD の状態もそこから導くので、
/// 「`Branch` ⇔ HEAD の行がある」が作りから成り立つ（02 の「head」）。読み手は
/// [`BranchList::head`] を網羅 `match` し、行の位置が要るときだけ [`BranchList::head_row`] を読む。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchList {
    /// 名前順（02 の「ブランチ一覧」）。
    branches: Vec<Branch>,
    head: HeadSlot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HeadSlot {
    /// `branches` の添字。
    Row(usize),
    Rowless(RowlessHead),
}

impl BranchList {
    /// 一覧から組む。**`head`（HEAD の指すブランチの名前）の行があればそれが HEAD で、
    /// `rowless` は呼ばない。** 行が無いときだけ `rowless` で HEAD を求める。
    ///
    /// **行無しの HEAD を値ではなく closure で受ける。** 値で受けると「行があるのに
    /// `Detached` も渡された」組が引数で書ける（`.claude/rules/rust-coding.md` の
    /// 「導出できる値を引数に取らせない」）。
    ///
    /// **`head` を受けても `rowless` は畳めない。** 名前があるのに行が無い理由は unborn
    /// だけではなく、HEAD の行を peel できずに落とした形もある（`git_read` はそこを
    /// `Err` にする）。どちらかを決められるのは ref を持っている呼び手だけなので、
    /// `RowlessHead::Unborn` の名前は `head` と同じ値でも closure の側が組む。
    pub fn new<E>(
        branches: Vec<Branch>,
        head: Option<&str>,
        rowless: impl FnOnce() -> Result<RowlessHead, E>,
    ) -> Result<Self, E> {
        let row = head.and_then(|head| branches.iter().position(|branch| branch.name == head));
        let head = match row {
            Some(row) => HeadSlot::Row(row),
            None => HeadSlot::Rowless(rowless()?),
        };
        Ok(BranchList { branches, head })
    }

    /// 名前順の一覧。
    pub fn branches(&self) -> &[Branch] {
        &self.branches
    }

    /// HEAD の行の位置（[`BranchList::branches`] の添字）。detached / unborn では `None`。
    ///
    /// [`BranchList::head`] の `Branch` の腕と同じ添字から導く。位置が要る読み手
    /// （カーソルの座り直し、行ごとの印）のためにある。
    pub fn head_row(&self) -> Option<usize> {
        match self.head {
            HeadSlot::Row(row) => Some(row),
            HeadSlot::Rowless(_) => None,
        }
    }

    /// HEAD の状態。
    pub fn head(&self) -> Head<'_> {
        match &self.head {
            HeadSlot::Row(row) => Head::Branch(&self.branches[*row]),
            HeadSlot::Rowless(RowlessHead::Detached(oid)) => Head::Detached(*oid),
            HeadSlot::Rowless(RowlessHead::Unborn { name }) => Head::Unborn(name),
        }
    }
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
    /// `branch.<name>.pushRemote`、無ければ `remote.pushDefault`。書かれたままの値
    /// （名前・URL・`.`）。
    ///
    /// **git の push はこの 2 つを追跡設定より先に見る**（01 の「push の行き先」）。
    /// 畳むのはこの 2 段だけで、fetch の規則（3 段目）は `app` が持つ（02）。
    pub push_remote: Option<String>,
    /// upstream に対する ahead / behind。
    pub ahead: usize,
    pub behind: usize,
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
    /// 進行中の操作（02 の「進行中操作の検出」）。**区画ではない**が、`git status` と
    /// 同じく「いまのワークツリーの状態」なので同じ読み取りで運ぶ —— 別の読み取りに
    /// すると、`conflicted` と進行中操作が別々の時点の値になる。鮮度は
    /// `WorktreeState::snapshot_epoch` の 1 本で判る（01）。
    pub progress: Option<InProgressOp>,
    /// 一覧に出たファイルのワークツリー側の `(mtime, len)` と index 側・HEAD 側の blob
    /// （02 の「ステータス」）。
    ///
    /// **外の変更を拾うための値。** 既に `M` のファイルを外で書き換えても区画も
    /// [`FileStatus`] も変わらないので、一覧を比べるだけでは拾えない（01 の「外部の
    /// 変更の検知 › 定期の status」）。[`FileEntry`] に載せないのは、あちらがコミットの
    /// ファイル一覧と共用で、そちらでは常に空になるため。
    pub stamps: BTreeMap<PathBuf, PathStamp>,
}

/// ワーキングツリーのどの区画か。[`WorktreeSnapshot`] の区画のフィールドと 1 対 1。
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
            progress: _,
            stamps: _,
        } = self;
        for section in [staged, unstaged, untracked, conflicted] {
            section.sort_by(|a, b| a.path.cmp(&b.path));
        }
    }

    /// 全区画のエントリ。分解束縛なので、区画を足したら漏れがコンパイルエラーになる
    /// （[`sort_sections`](Self::sort_sections) と同じ狙い）。
    pub(crate) fn entries(&self) -> impl Iterator<Item = &FileEntry> {
        let Self {
            staged,
            unstaged,
            untracked,
            conflicted,
            progress: _,
            stamps: _,
        } = self;
        [staged, unstaged, untracked, conflicted]
            .into_iter()
            .flatten()
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

/// diff の対象。`path` を指定すると 1 ファイルに絞る（`Stash` は絞らない）。
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
    /// stash 1 件の中身。`oid` は stash コミット（`stash@{n}` が指すもの）。
    ///
    /// **[`DiffTarget::Commit`] で代用しない。** 第一親（stash した時点の HEAD）との
    /// 差は tracked な変更だけで、`--include-untracked` で退避したファイルは
    /// **第 3 親のツリー**に入っている（git 2.55 で確認）。こちらはそれを「追加」として
    /// 同じ diff に混ぜる（02 の「stash の diff」）。
    ///
    /// **`path` を持たない。** 絞る経路が無い —— Stash ペインは全ファイルを
    /// 1 枚に並べる（01 の「Stash ペイン」）。持たせると、組む側が毎回 `None` を書き、
    /// 読む側は「1 ファイルに絞る経路がある」と読む。
    Stash(Oid),
}

impl DiffTarget {
    /// 絞り込みのパス。`None` は対象全体。変種ごとの `match` を呼び出し側に配らない。
    pub fn path(&self) -> Option<&Path> {
        match self {
            DiffTarget::IndexVsWorktree(path)
            | DiffTarget::HeadVsIndex(path)
            | DiffTarget::Commit { path, .. }
            | DiffTarget::Range { path, .. } => path.as_deref(),
            DiffTarget::Stash(_) => None,
        }
    }

    /// ワークツリーか index を読む対象で、パスを絞っているときのそのパス。
    ///
    /// **外の編集で中身が変わりうる対象はこれだけ** —— コミットと stash の中身は
    /// オブジェクトで、外の編集では変わらない。指紋（`PathStamp`）を付けるかを決める
    /// 規則で、`diff` が付ける側（`FileDiff::stamp`）と `app` が比べる側
    /// （`AppState::diff_stamp`）の両方がここを見る。
    pub fn live_path(&self) -> Option<&Path> {
        match self {
            DiffTarget::IndexVsWorktree(path) | DiffTarget::HeadVsIndex(path) => path.as_deref(),
            DiffTarget::Commit { .. } | DiffTarget::Range { .. } | DiffTarget::Stash(_) => None,
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
    /// このハンクを読む**前**に採った、対象のファイルの指紋
    /// （[`RepoReader::path_stamp`]）。ハンク操作が `HunkApply` の `expect` に載せ、
    /// `git apply` の直前に採り直した値と比べる（01 の「外部の変更の検知 › 古いハンクの守り」）。
    ///
    /// **ワークツリーか index を読む対象（`IndexVsWorktree` / `HeadVsIndex`）で、
    /// パスを絞ったときだけ `Some`。** コミットと stash の中身はオブジェクトで、外の
    /// 編集では変わらない。
    ///
    /// **`DiffView::stamp`（status の stamp）とは別の値。** あちらは「読み直すか」を
    /// 決めるために status と比べる値で、こちらは「見えているハンクがいまのファイルと
    /// 合っているか」を確かめる値。片方に寄せると、status が diff より古いときに
    /// 「違う → 読み直す → また違う」が次の status まで回る。
    pub stamp: Option<PathStamp>,
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

impl StashEntry {
    /// `stash@{index}`。画面に出す名前（[`stash_ref`]）。
    pub fn name(&self) -> String {
        stash_ref(self.index)
    }
}

/// `stash@{index}`。**表記はここ 1 本** —— `git_exec` の argv と画面の名前が同じ関数を
/// 通るので、画面の `stash@{n}` を端末の `git stash show stash@{n}` にそのまま打てる。
/// `git_exec` から呼ぶのは値の語彙（00 の「レイヤー構成」）と同じ扱い。
pub fn stash_ref(index: usize) -> String {
    format!("stash@{{{index}}}")
}

/// 進行中の操作。HEAD か作業ツリー全体を動かす書き込みを断る判断に使う（03 の
/// 「進行中に通す書き込み」）。
///
/// 見分け方と、持たない値（`-i` の有無、止まった理由）は 02 の「進行中操作の検出」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InProgressOp {
    /// `rebase-merge/` か、`applying` の無い `rebase-apply/`。
    Rebase {
        step: Option<Step>,
        /// rebase しているブランチ（`head-name` の `refs/heads/` を外したもの）。
        /// detached HEAD で始めた rebase と、読めないときは `None`。
        ///
        /// **このブランチは git が改名も削除も断る**（03 の表）ので、`app` が入力を
        /// 開く前に断るのに使う（01 の「進行中操作があるときの書き込み」）。
        branch: Option<String>,
        /// mewgit が渡した reword を commit-msg hook などが断り、元のメッセージで
        /// コミットしたまま止まった形（02 の「断られた reword」）。`m` の continue が
        /// 新しいメッセージを amend してから進める（03）。
        refused_reword: Option<RefusedReword>,
    },
    /// `git am`（`rebase-apply/applying`）。**mewgit からは始めない**が、外で始めた am の
    /// 途中で開いたときに「無い」と読まないために持つ。
    Am {
        step: Option<Step>,
    },
    /// `oid` が `None` なのは `sequencer/` だけが残った形（複数の pick の途中で 1 つを
    /// commit で解いた直後）。
    CherryPick {
        oid: Option<Oid>,
    },
    Revert {
        oid: Option<Oid>,
    },
    /// octopus では `MERGE_HEAD` が複数行になるが、先頭だけを持つ。
    Merge {
        oid: Oid,
    },
}

/// 断られた reword（[`InProgressOp::Rebase::refused_reword`]）。見分け方は 02 の
/// 「断られた reword」、continue で何を叩くかは 03 の「Interactive rebase の実装」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedReword {
    /// 見分けた時点の HEAD。continue の前に git に照合させる（`update-ref --stdin` の
    /// `verify`）—— snapshot が古いと別のコミットを amend するため。
    ///
    /// **新しいメッセージは持たない。** amend は `rebase-merge/mewgit-reword-<X>` を
    /// commit editor が差し込む経路（開始と同じ）で通すので、値として運ぶ必要が無い。
    pub head: Oid,
}

/// rebase / am の進み具合。**git の数えた todo の行数**で、コミット数ではない
/// （`-x` を付けると exec の行が足される）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    pub current: usize,
    pub total: usize,
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use super::{short_oid, BranchList, Head, Oid, RowlessHead, SHORT_OID};
    use crate::test_util::{fake_branch, fake_oid};

    #[test]
    fn a_branch_list_takes_head_from_its_head_row_and_asks_only_without_one() {
        // HEAD の行は先頭でも末尾でもない（添字を取り違える変異を落とす）。行があれば
        // 行無しの HEAD は求めない —— 求めると「行があるのに `Detached`」の組を値で渡せる
        // （02 の「head」）。
        let rows = vec![
            fake_branch("another"),
            fake_branch("main"),
            fake_branch("zzz"),
        ];
        let list = BranchList::new(rows, Some("main"), || -> Result<RowlessHead, Infallible> {
            panic!("HEAD の行があるのに行無しの HEAD を求めた")
        })
        .unwrap();
        assert!(matches!(list.head(), Head::Branch(b) if b.name == "main"));
        assert_eq!(list.head_row(), Some(1));

        // 名前が行に当たらなければ（unborn）、名前があっても行無しの HEAD を求める。
        let rows = vec![fake_branch("main")];
        let list = BranchList::new(rows, Some("unborn"), || {
            Ok::<_, Infallible>(RowlessHead::Unborn {
                name: "unborn".to_string(),
            })
        })
        .unwrap();
        assert_eq!(list.head(), Head::Unborn("unborn"));
        assert_eq!(list.head_row(), None);

        // 名前が無ければ（detached）行は探さない。
        let list = BranchList::new(vec![fake_branch("main")], None, || {
            Ok::<_, Infallible>(RowlessHead::Detached(fake_oid(7)))
        })
        .unwrap();
        assert_eq!(list.head(), Head::Detached(fake_oid(7)));
        assert_eq!(list.head().branch(), None);
    }

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

    #[test]
    fn now_is_the_current_second_with_a_real_world_offset() {
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("epoch より後")
            .as_secs() as i64;
        let now = super::Timestamp::now();
        // 秒の境目をまたぐぶんだけ許す。
        assert!((before..=before + 1).contains(&now.seconds), "{now:?}");
        // 実在するオフセットは UTC-12:00 から UTC+14:00 まで。
        assert!(
            (-12 * 60..=14 * 60).contains(&now.offset_minutes),
            "{now:?}"
        );
    }
}
