//! リポジトリを開いて ref / リモート / HEAD を読む。設計は `docs/design/02-git-read.md`。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use git2::{BranchType, ErrorCode, Reference, ReferenceType, Repository};

use super::{GitReadError, HeadInfo, Oid, RefIndex, RefLabel, Remote};

/// git2 による読み取り専用のリポジトリハンドル。
///
/// `git2::Repository` は `Send` だが `Sync` ではないので、`app` からは専用スレッド越しに
/// 使う（02 の「スレッド」節）。ここはその内側の同期 API。
pub struct RepoReader {
    repo: Repository,
    workdir: PathBuf,
}

/// `git2::Repository` が `Debug` を実装していないので手で書く。`AppState` の一部が
/// `Debug` を要求するため、実装自体は落とせない。
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
        let workdir = repo
            .workdir()
            .ok_or_else(|| GitReadError::BareRepository {
                path: path.to_path_buf(),
            })?
            .to_path_buf();

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
        let mut labels: HashMap<Oid, Vec<RefLabel>> = HashMap::new();

        for branch in self.repo.branches(Some(BranchType::Local))? {
            let (branch, _) = branch?;
            // `repo.head()` との名前比較にしないのは、unborn のとき head() が失敗するため。
            let is_head = branch.is_head();
            push_label(&mut labels, branch.get(), |name| RefLabel::Local {
                name,
                is_head,
            });
        }

        for branch in self.repo.branches(Some(BranchType::Remote))? {
            let (branch, _) = branch?;
            push_label(&mut labels, branch.get(), |name| RefLabel::Remote { name });
        }

        // タグ以外の `refs/*`（`refs/stash`、`refs/notes/*` など）はグラフのラベルにしない。
        for reference in self.repo.references()? {
            let reference = reference?;
            if !reference.is_tag() {
                continue;
            }
            push_label(&mut labels, &reference, |name| RefLabel::Tag { name });
        }

        for labels in labels.values_mut() {
            // git2 の列挙順は安定していないので、ここで揃える。
            labels.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
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
        match head.shorthand() {
            Some(name) if head.is_branch() => Ok(HeadInfo::Branch {
                name: name.to_string(),
                oid,
            }),
            _ => Ok(HeadInfo::Detached(oid)),
        }
    }

    /// unborn な HEAD が指しているブランチ名。
    fn unborn_branch_name(&self) -> Result<String, GitReadError> {
        let head = self.repo.find_reference("HEAD")?;
        let target = head.symbolic_target().unwrap_or("HEAD");
        Ok(target.strip_prefix("refs/heads/").unwrap_or(target).into())
    }
}

/// ref をラベルとして索引に積む。積めないものは黙って飛ばす。
///
/// 飛ばす対象は 3 つ。symbolic ref（`refs/remotes/origin/HEAD`）は解決すると指す先の
/// ラベルと重複する。非 UTF-8 の ref 名は描画できない。commit まで peel できない ref
/// （tree や blob を指すタグ）はグラフのどの行にも乗らない。いずれもラベルが 1 つ
/// 減るだけなので、`refs()` 全体を失敗させない。
fn push_label<F>(labels: &mut HashMap<Oid, Vec<RefLabel>>, reference: &Reference<'_>, make: F)
where
    F: FnOnce(String) -> RefLabel,
{
    if reference.kind() != Some(ReferenceType::Direct) {
        return;
    }
    let Some(name) = reference.shorthand() else {
        return;
    };
    // annotated tag は tag オブジェクトを指すので、commit まで peel してから引く。
    let Ok(commit) = reference.peel_to_commit() else {
        return;
    };

    labels
        .entry(commit.id())
        .or_default()
        .push(make(name.to_string()));
}
