//! 外の変更を見張るための値。設計は `docs/design/02-git-read.md` の「指紋」と、
//! `docs/design/01-app-core.md` の「外部の変更の検知」。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::SystemTime;

use super::{GitReadError, Oid, RepoReader};

/// ワークツリーのファイル 1 つの `(mtime, len)`。
///
/// 既に `M` のファイルを外で書き換えても status の一覧は変わらないので、これを
/// 比べて拾う（01 の「外部の変更の検知 › 定期の status」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    pub mtime: SystemTime,
    pub len: u64,
}

impl FileStamp {
    /// `path` の stamp。読めなければ（消えている）`None`。
    ///
    /// **`symlink_metadata`** なのは、git がシンボリックリンクをリンクそのものとして
    /// 扱うため。リンク先を見ると、リンク先の変化で差が出る。
    pub(crate) fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        Some(Self {
            mtime: meta.modified().ok()?,
            len: meta.len(),
        })
    }
}

/// 外の変更を見張る指紋。git を起動せず、git2 と stat だけで組む（02 の「指紋」）。
///
/// どの差から何を読み直すかは `app` が決める（01 の「外部の変更の検知 › 指紋」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoFingerprint {
    /// HEAD（symbolic なら ref 名も）と、`refs/stash` を除く全 ref の名前と指す先の
    /// ハッシュ。
    pub refs: u64,
    /// `refs/stash` の指す先。
    pub stash: Option<Oid>,
    /// commondir の `config` の mtime（追跡設定・リモート一覧）。
    pub config: Option<SystemTime>,
    /// ワークツリーごとの gitdir の `index` と、gitdir そのものの mtime（進行中操作の印）。
    pub worktree: (Option<SystemTime>, Option<SystemTime>),
}

/// `refs/stash` の完全な名前。
const STASH_REF: &str = "refs/stash";

impl RepoReader {
    /// いまの指紋。
    ///
    /// **ref は mtime ではなく列挙する。** 入れ子のブランチ（`feat/y`）を外で作っても
    /// gitdir も `refs/heads` も mtime が動かない（01 の実測）。列挙のコストは ref
    /// 1 万本で 70ms（同じく 01）。
    pub fn fingerprint(&self) -> Result<RepoFingerprint, GitReadError> {
        let mut hasher = DefaultHasher::new();

        // **HEAD は名前と解決先の両方を入れる。** 同じコミットを指す別のブランチへ
        // `switch` しても、どの ref の値も動かない。unborn では解決先が無い。
        if let Ok(head) = self.repo.find_reference("HEAD") {
            head.symbolic_target_bytes().hash(&mut hasher);
            head.resolve()
                .ok()
                .and_then(|r| r.target())
                .hash(&mut hasher);
        }

        let mut stash = None;
        for reference in self.repo.references()? {
            let reference = reference?;
            let name = reference.name_bytes();
            // **`refs/stash` は別に持つ。** 混ぜると stash の書き込みが `RefsAndLog`
            // （グラフの位置を先頭に戻す）に化ける。
            if name == STASH_REF.as_bytes() {
                stash = reference.target();
                continue;
            }
            name.hash(&mut hasher);
            reference.target().hash(&mut hasher);
            reference.symbolic_target_bytes().hash(&mut hasher);
        }

        let mtime = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let git_dir = self.git_dir();
        Ok(RepoFingerprint {
            refs: hasher.finish(),
            stash,
            config: mtime(&self.repo.commondir().join("config")),
            worktree: (mtime(&git_dir.join("index")), mtime(git_dir)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TestRepo;

    fn fingerprint(repo: &TestRepo) -> RepoFingerprint {
        RepoReader::open(repo.path())
            .expect("開けません")
            .fingerprint()
            .expect("指紋を取れません")
    }

    fn committed() -> TestRepo {
        let repo = TestRepo::init();
        repo.commit("1");
        repo
    }

    #[test]
    fn the_fingerprint_is_stable_without_changes() {
        let repo = committed();
        assert_eq!(fingerprint(&repo), fingerprint(&repo));
    }

    #[test]
    fn a_nested_branch_moves_the_refs() {
        // mtime では拾えない形（01 の実測）。
        let repo = committed();
        let before = fingerprint(&repo);
        repo.git(&["branch", "feat/y"]);
        let after = fingerprint(&repo);

        assert_ne!(before.refs, after.refs);
        assert_eq!(before.stash, after.stash);
    }

    #[test]
    fn switching_to_a_branch_on_the_same_commit_moves_the_refs() {
        let repo = committed();
        repo.git(&["branch", "other"]);
        let before = fingerprint(&repo);
        repo.git(&["switch", "-q", "other"]);

        assert_ne!(before.refs, fingerprint(&repo).refs);
    }

    #[test]
    fn a_stash_moves_only_the_stash() {
        let repo = committed();
        let before = fingerprint(&repo);
        repo.write_worktree_file("file.txt", b"2\n");
        repo.git(&["stash", "push", "-q"]);
        let after = fingerprint(&repo);

        assert!(after.stash.is_some());
        assert_eq!(before.refs, after.refs, "stash が ref の差に混ざっています");
    }

    #[test]
    fn the_stamp_of_a_missing_file_is_none() {
        let repo = committed();
        assert!(FileStamp::of(&repo.path().join("nope")).is_none());
        assert!(FileStamp::of(&repo.path().join("file.txt")).is_some());
    }
}
