//! ワーキングツリーのステータス。設計は `docs/design/02-git-read.md` の「ステータス」。

use git2::{Status, StatusEntry, StatusOptions};

use super::{utf8_path, FileEntry, FileStatus, GitReadError, RepoReader, WorktreeSnapshot};

impl RepoReader {
    /// staged / unstaged / untracked / conflicted に分けたワーキングツリーの状態。
    ///
    /// 同じファイルが staged と unstaged の両方に出ることがある。一部だけ stage した
    /// 状態の正しい表現なので、どちらかに寄せない。
    pub fn status(&self) -> Result<WorktreeSnapshot, GitReadError> {
        let mut opts = StatusOptions::new();
        opts.include_untracked(true)
            // 切ると新しいディレクトリが 1 行にまとまり、中のファイルを個別に
            // stage できない。
            .recurse_untracked_dirs(true)
            .include_ignored(false)
            .include_unmodified(false)
            // 切ると `git mv` が「削除 + 追加」の 2 行になり、`git status` の
            // 見え方と食い違う。
            .renames_head_to_index(true)
            .renames_index_to_workdir(true);

        let mut snapshot = WorktreeSnapshot::default();
        for entry in self.repo.statuses(Some(&mut opts))?.iter() {
            let status = entry.status();

            // CONFLICTED は他のビットと同時に立つ。解消するまで stage も unstage も
            // できないので、conflicted だけに入れて他の区画には出さない。
            if status.is_conflicted() {
                snapshot
                    .conflicted
                    .extend(conflicted_entry(&entry, FileStatus::Conflicted));
                continue;
            }

            if let Some(kind) = index_status(status) {
                snapshot
                    .staged
                    .extend(file_entry(entry.head_to_index(), kind));
            }
            if let Some(kind) = worktree_status(status) {
                // untracked は「まだ index に無い」だけで unstaged とは操作が違う
                // （stage はできるが unstage / discard の意味が変わる）ので分ける。
                let section = if kind == FileStatus::Untracked {
                    &mut snapshot.untracked
                } else {
                    &mut snapshot.unstaged
                };
                section.extend(file_entry(entry.index_to_workdir(), kind));
            }
        }

        // libgit2 の列挙順には依存しない（`RefIndex` のラベルと同じ理由）。
        for section in [
            &mut snapshot.staged,
            &mut snapshot.unstaged,
            &mut snapshot.untracked,
            &mut snapshot.conflicted,
        ] {
            section.sort_by(|a, b| a.path.cmp(&b.path));
        }
        Ok(snapshot)
    }
}

/// conflicted な entry の行。delta はどちらか一方しか無いことがある。
fn conflicted_entry(entry: &StatusEntry<'_>, kind: FileStatus) -> Option<FileEntry> {
    file_entry(
        entry.index_to_workdir().or_else(|| entry.head_to_index()),
        kind,
    )
}

/// delta から 1 行を組み立てる。パスが非 UTF-8 のファイルは落とす。
///
/// **パスは `StatusEntry::path()` からは取らない。** rename のとき `path()` は旧パスを
/// 返すので、staged の一覧に新しい名前が出なくなる（02 の「ステータス」）。
fn file_entry(delta: Option<git2::DiffDelta<'_>>, status: FileStatus) -> Option<FileEntry> {
    let delta = delta?;
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

/// index 側（staged）のビットから変更種別を決める。
///
/// 複数のビットが同時に立つ（`git mv` してから編集すると `RENAMED | MODIFIED`）ので、
/// 上から順に見て最初に当たったものを採る。並びは `git status` の表示に合わせている。
fn index_status(status: Status) -> Option<FileStatus> {
    if status.contains(Status::INDEX_RENAMED) {
        Some(FileStatus::Renamed)
    } else if status.contains(Status::INDEX_NEW) {
        Some(FileStatus::Added)
    } else if status.contains(Status::INDEX_DELETED) {
        Some(FileStatus::Deleted)
    } else if status.contains(Status::INDEX_TYPECHANGE) {
        Some(FileStatus::Typechange)
    } else if status.contains(Status::INDEX_MODIFIED) {
        Some(FileStatus::Modified)
    } else {
        None
    }
}

/// ワークツリー側のビットから変更種別を決める。[`index_status`] と同じ優先順。
fn worktree_status(status: Status) -> Option<FileStatus> {
    if status.contains(Status::WT_NEW) {
        Some(FileStatus::Untracked)
    } else if status.contains(Status::WT_RENAMED) {
        Some(FileStatus::Renamed)
    } else if status.contains(Status::WT_DELETED) {
        Some(FileStatus::Deleted)
    } else if status.contains(Status::WT_TYPECHANGE) {
        Some(FileStatus::Typechange)
    } else if status.contains(Status::WT_MODIFIED) {
        Some(FileStatus::Modified)
    } else {
        None
    }
}
