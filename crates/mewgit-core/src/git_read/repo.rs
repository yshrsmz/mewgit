//! リポジトリを開いて ref / リモート / HEAD を読む。設計は `docs/design/02-git-read.md`。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use git2::{ErrorCode, Reference, ReferenceType, Repository};

use super::{GitReadError, HeadInfo, Oid, RefIndex, RefLabel, Remote};

/// git2 による読み取り専用のリポジトリハンドル。
///
/// `git2::Repository` は `Send` だが `Sync` ではないので、`app` からは専用スレッド越しに
/// 使う（02 の「スレッド」節）。ここはその内側の同期 API。
pub struct RepoReader {
    repo: Repository,
    workdir: PathBuf,
}

/// `git2::Repository` が `Debug` を実装していないので手で書く。`Result<RepoReader, _>` を
/// `unwrap_err` するテストとログ出力のために `Debug` 自体は要る。`AppState` は
/// `RepoReader` を持たない（02 の「スレッド」節のとおり `app` には handle を渡す）。
impl std::fmt::Debug for RepoReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepoReader")
            .field("workdir", &self.workdir)
            .finish_non_exhaustive()
    }
}

impl RepoReader {
    /// リポジトリを開く。サブディレクトリを渡されても上に辿って探す。
    ///
    /// bare リポジトリは拒否する。status / diff / checkout がいずれもワークツリー前提で、
    /// 開けても何もできないため。
    pub fn open(path: &Path) -> Result<Self, GitReadError> {
        let repo = Repository::discover(path).map_err(|source| GitReadError::Open {
            path: path.to_path_buf(),
            source,
        })?;
        // 見つかったリポジトリのパスを添える。引数のサブディレクトリを返すと
        // 「そのディレクトリが bare だ」と読めてしまう。
        let workdir = repo
            .workdir()
            .ok_or_else(|| GitReadError::BareRepository {
                path: repo.path().components().collect(),
            })?
            // libgit2 は末尾に区切りを付けた `/…/repo/` を返す。そのまま持つと
            // 表示が `mewgit/` になり、設定や引数のパスとの文字列比較も外れる。
            .components()
            .collect();

        Ok(Self { repo, workdir })
    }

    /// ワークツリーのルート。`open` に渡したパスではなく、実際に見つかった方を返す。
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// リモート一覧。`url` は `.git/config` の生の値で、host / owner / repo の解析は
    /// `auth` の担当（00 の依存の向き）。
    pub fn remotes(&self) -> Result<Vec<Remote>, GitReadError> {
        let names = self.repo.remotes()?;
        let mut remotes = Vec::with_capacity(names.len());

        for name in names.iter() {
            // 名前が非 UTF-8 のリモートは `git_exec` の argv にも載せられないので落とす。
            let Some(name) = name else { continue };
            let remote = self.repo.find_remote(name)?;
            // URL は表示と `auth` の解析にしか使わないので、非 UTF-8 でもリモート自体は
            // 消さずに lossy で残す。
            let url = String::from_utf8_lossy(remote.url_bytes()).into_owned();
            remotes.push(Remote {
                name: name.to_string(),
                url,
            });
        }

        remotes.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(remotes)
    }

    /// Oid → ラベルの索引。グラフの各行の右に出す ref を引くのに使う（06）。
    pub fn refs(&self) -> Result<RefIndex, GitReadError> {
        // HEAD は 1 回だけ解決する。`Branch::is_head()` は呼ぶたびに `.git/HEAD` と
        // その参照先を読み直すので、ブランチの本数だけファイル読みが増える。
        // unborn なら symbolic target の指す ref が存在せず、detached なら HEAD が
        // symbolic ではないので、どちらも「どのブランチも HEAD ではない」に落ちる。
        let head = self.repo.find_reference("HEAD")?;
        let head_target = head.symbolic_target();

        // `refs/*` を 1 パスで走査して種別に振り分ける。`branches()` も内部では全 ref を
        // 列挙してから接頭辞で捨てるので、種別ごとに呼ぶと同じ走査を繰り返すことになる。
        let mut labels: HashMap<Oid, Vec<RefLabel>> = HashMap::new();
        for reference in self.repo.references()? {
            let reference = reference?;
            let Some(label) = ref_label(&reference, head_target) else {
                continue;
            };
            // annotated tag は tag オブジェクトを指すので、commit まで peel してから引く。
            // peel できない ref（tree や blob を指すタグ）はグラフのどの行にも乗らないので
            // 落とす。ラベルが 1 つ減るだけなので `refs()` 全体は失敗させない。
            let Ok(commit) = reference.peel_to_commit() else {
                continue;
            };
            labels.entry(commit.id()).or_default().push(label);
        }

        Ok(RefIndex::new(labels))
    }

    /// HEAD の状態。
    pub fn head(&self) -> Result<HeadInfo, GitReadError> {
        let head = match self.repo.head() {
            Ok(head) => head,
            // コミットが 1 つも無いリポジトリ。ブランチ名は HEAD の symbolic target から取る。
            Err(err) if err.code() == ErrorCode::UnbornBranch => {
                return Ok(HeadInfo::Unborn {
                    name: self.unborn_branch_name()?,
                });
            }
            Err(err) => return Err(err.into()),
        };

        let oid = head.peel_to_commit()?.id();
        if !head.is_branch() {
            return Ok(HeadInfo::Detached(oid));
        }
        // ブランチに乗っているのに名前を落とすと detached と区別が付かなくなるので、
        // ここだけは非 UTF-8 を握りつぶさずエラーにする（02 の「非 UTF-8 の扱い」）。
        let name = head.shorthand().ok_or(GitReadError::NonUtf8Head)?;
        Ok(HeadInfo::Branch {
            name: name.to_string(),
            oid,
        })
    }

    /// unborn な HEAD が指しているブランチ名。
    ///
    /// unborn は「HEAD が symbolic で、指す先の ref がまだ無い」状態なので、
    /// `symbolic_target()` が `None` を返すのは target が非 UTF-8 のときだけ。
    fn unborn_branch_name(&self) -> Result<String, GitReadError> {
        let head = self.repo.find_reference("HEAD")?;
        let target = head.symbolic_target().ok_or(GitReadError::NonUtf8Head)?;
        Ok(target.strip_prefix("refs/heads/").unwrap_or(target).into())
    }
}

/// グラフのラベルにする ref を選び、種別を決める。載せないものは `None`。
///
/// `head_target` は `HEAD` の symbolic target（`refs/heads/main`）。
fn ref_label(reference: &Reference<'_>, head_target: Option<&str>) -> Option<RefLabel> {
    // symbolic ref（`refs/remotes/origin/HEAD`）は解決すると指す先のラベルと重複する。
    if reference.kind() != Some(ReferenceType::Direct) {
        return None;
    }
    // 非 UTF-8 の ref 名は識別子として扱えないので落とす（02 の「非 UTF-8 の扱い」）。
    let name = reference.shorthand()?.to_string();

    // 載せるのはローカル / リモート追跡ブランチとタグだけ。`refs/stash` や
    // `refs/notes/*` はそれぞれ別のペインの担当で、グラフのラベルにはしない。
    if reference.is_branch() {
        let is_head = reference
            .name()
            .is_some_and(|full| head_target == Some(full));
        Some(RefLabel::Local { name, is_head })
    } else if reference.is_remote() {
        Some(RefLabel::Remote { name })
    } else if reference.is_tag() {
        Some(RefLabel::Tag { name })
    } else {
        None
    }
}
