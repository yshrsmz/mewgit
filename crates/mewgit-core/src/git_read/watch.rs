//! 外の変更を見張るための値。設計は `docs/design/02-git-read.md` の「指紋」と、
//! `docs/design/01-app-core.md` の「外部の変更の検知」。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::SystemTime;

use super::{GitReadError, Oid, RepoReader, STASH_REF};

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

/// 一覧に出たファイル 1 つの、ワークツリー側と index 側の指紋（`WorktreeSnapshot.stamps`）。
///
/// **index 側も要る。** unstaged の diff（`IndexVsWorktree`）は index にも依存するので、
/// 外で `git add -p` すると、ワークツリーのファイルは動かないまま diff だけが変わる
/// （`/code-review` が拾った）。index 側の blob は status の delta が既に持っている。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathStamp {
    /// ワークツリーに無ければ `None`。
    pub worktree: Option<FileStamp>,
    /// index の blob。index に無ければ（untracked）`None`。
    pub index: Option<Oid>,
}

/// 外の変更を見張る指紋。git を起動せず、git2 と stat だけで組む（02 の「指紋」）。
///
/// どの差から何を読み直すかは `app` が決める（01 の「外部の変更の検知 › 指紋」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoFingerprint {
    /// HEAD（symbolic なら ref 名も）と、`refs/stash` を除く全 ref の名前と指す先の
    /// ハッシュ。
    pub refs: u64,
    /// `refs/stash` の指す先と、reflog（stash の一覧）の件数。
    ///
    /// **件数も要る。** 上以外の stash を外で drop / pop しても `refs/stash` は動かない
    /// （`git stash drop stash@{1}` の前後で同じ oid。実測）ので、指す先だけでは一覧が
    /// 読み直されず、mewgit の `d` / `a` / `Z` が `stash@{n}` で別の stash に効く
    /// （`/code-review` が拾った）。
    pub stash: (Option<Oid>, usize),
    /// commondir の `config` の mtime（追跡設定・リモート一覧）。
    pub config: Option<SystemTime>,
    /// ワークツリーごとの gitdir の `index` と、gitdir そのものの mtime（進行中操作の印）。
    pub worktree: (Option<SystemTime>, Option<SystemTime>),
}

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

        let mut top = None;
        for reference in self.repo.references()? {
            let reference = reference?;
            let name = reference.name_bytes();
            // **`refs/stash` は別に持つ。** 混ぜると stash の書き込みが `RefsAndLog`
            // （グラフの位置を先頭に戻す）に化ける。
            if name == STASH_REF.as_bytes() {
                top = reference.target();
                continue;
            }
            name.hash(&mut hasher);
            reference.target().hash(&mut hasher);
            reference.symbolic_target_bytes().hash(&mut hasher);
        }

        let mtime = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let git_dir = self.git_dir();
        // 無い reflog は空で返る（libgit2。`stashes` と同じ）。
        let stashes = self.repo.reflog(STASH_REF)?.len();
        Ok(RepoFingerprint {
            refs: hasher.finish(),
            stash: (top, stashes),
            config: mtime(&self.repo.commondir().join("config")),
            worktree: (mtime(&git_dir.join("index")), mtime(git_dir)),
        })
    }

    /// `path`（ワークツリーのルートからの相対）のいまの指紋。
    ///
    /// ハンク操作の守りが使う（01 の「外部の変更の検知 › 古いハンクの守り」）。diff を
    /// 組む**前**に `diff` が採り、`git apply` を起こす直前に `git_exec::Runner` が
    /// 採り直して比べる。**`git_exec` が呼んでよい読み取りは `open` とこれだけ**（00 の
    /// レイヤーの例外）。
    ///
    /// **index は読み直してから引く。** libgit2 の `Index` はリポジトリに貼り付いた
    /// 写しで、外で `git add` されても自分からは読み直さない（diff の側は
    /// `diff_load_index` が読み直す）。
    pub fn path_stamp(&self, path: &Path) -> Result<PathStamp, GitReadError> {
        let mut index = self.repo.index()?;
        index.read(false)?;
        Ok(PathStamp {
            worktree: FileStamp::of(&self.workdir().join(path)),
            // コンフリクト中のファイルは stage 0 が無いので `None`（ハンク操作の対象に
            // ならない）。
            index: index.get_path(path, 0).map(|entry| entry.id),
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

        assert!(after.stash.0.is_some());
        assert_eq!(before.refs, after.refs, "stash が ref の差に混ざっています");
    }

    #[test]
    fn dropping_a_stash_below_the_top_moves_the_stash() {
        // `refs/stash` は動かない（実測）。件数で拾う。
        let repo = committed();
        for n in 1..=3 {
            repo.write_worktree_file("file.txt", format!("{n}\n").as_bytes());
            repo.git(&["stash", "push", "-q"]);
        }
        let before = fingerprint(&repo);
        repo.git(&["stash", "drop", "-q", "stash@{1}"]);
        let after = fingerprint(&repo);

        assert_eq!(
            before.stash.0, after.stash.0,
            "上の stash が動いています（前提）"
        );
        assert_ne!(before.stash, after.stash);
    }

    #[test]
    fn rewriting_an_already_modified_file_changes_only_the_stamps() {
        // 一覧は ` M` のまま（01 の実測の 1 行目）。`stamps` でしか拾えない。
        let repo = committed();
        let reader = RepoReader::open(repo.path()).expect("開けません");
        repo.write_worktree_file("file.txt", b"two-A\n");
        let before = reader.status().expect("status を読めません");
        repo.write_worktree_file("file.txt", b"two-BB\n");
        let after = reader.status().expect("status を読めません");

        assert_eq!(before.unstaged, after.unstaged);
        assert_ne!(before.stamps, after.stamps);
        assert!(after.stamps.contains_key(Path::new("file.txt")));
    }

    #[test]
    fn staging_outside_changes_only_the_index_side_of_the_stamp() {
        // 外で `git add` しても、ワークツリーのファイルは動かない。unstaged の diff は
        // index にも依存するので、index 側の blob で拾う。
        let repo = committed();
        let reader = RepoReader::open(repo.path()).expect("開けません");
        repo.write_worktree_file("file.txt", b"two-A\n");
        let before = reader.status().expect("status を読めません");
        repo.git(&["add", "file.txt"]);
        let after = reader.status().expect("status を読めません");

        let path = Path::new("file.txt");
        let (before, after) = (before.stamps[path], after.stamps[path]);
        assert_eq!(before.worktree, after.worktree);
        assert_ne!(before.index, after.index);
        assert!(after.index.is_some());
    }

    #[test]
    fn a_deleted_file_has_no_worktree_side() {
        let repo = committed();
        std::fs::remove_file(repo.path().join("file.txt")).expect("消せません");
        let snapshot = RepoReader::open(repo.path())
            .expect("開けません")
            .status()
            .expect("status を読めません");

        assert_eq!(snapshot.unstaged.len(), 1);
        let stamp = snapshot.stamps[Path::new("file.txt")];
        assert_eq!(stamp.worktree, None);
        assert!(stamp.index.is_some());
    }

    #[test]
    fn the_stamp_of_a_missing_file_is_none() {
        let repo = committed();
        assert!(FileStamp::of(&repo.path().join("nope")).is_none());
        assert!(FileStamp::of(&repo.path().join("file.txt")).is_some());
    }

    #[test]
    fn a_long_lived_reader_sees_an_outside_add_in_the_path_stamp() {
        // リーダースレッドは同じ `RepoReader` を使い続ける。libgit2 の `Index` は
        // 自分からは読み直さないので、外の `git add` が指紋に出ることを同じ reader で見る。
        let repo = committed();
        let reader = RepoReader::open(repo.path()).expect("開けません");
        repo.write_worktree_file("file.txt", b"two-A\n");
        let before = reader
            .path_stamp(Path::new("file.txt"))
            .expect("指紋を採れません");
        repo.git(&["add", "file.txt"]);
        let after = reader
            .path_stamp(Path::new("file.txt"))
            .expect("指紋を採れません");

        assert_eq!(
            before.worktree, after.worktree,
            "ワークツリーは動いていない（前提）"
        );
        assert_ne!(before.index, after.index);
    }
}
