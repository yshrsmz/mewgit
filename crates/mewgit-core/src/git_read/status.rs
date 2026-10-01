//! ワーキングツリーのステータス。設計は `docs/design/02-git-read.md` の「ステータス」。

use std::collections::BTreeMap;

use git2::StatusOptions;

use super::{
    file_entry, in_progress, FileStamp, FileStatus, GitReadError, RepoReader, WorktreeSnapshot,
};

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
            // CONFLICTED は index 側・ワークツリー側のビットと同時に立つ。解消するまで
            // stage も unstage もできないので、conflicted だけに入れて他の区画には出さない。
            if entry.status().is_conflicted() {
                // delta はどちらか一方しか無いことがある。
                let delta = entry.index_to_workdir().or_else(|| entry.head_to_index());
                snapshot
                    .conflicted
                    .extend(delta.as_ref().and_then(file_entry));
                continue;
            }

            // **変更種別は `Status` のビットではなく delta から決める。** libgit2 が
            // 既に解決していて、複数のビットが同時に立つとき（`git mv` してから編集
            // すると `RENAMED | MODIFIED`）にどちらを採るかも delta 側が答えを持って
            // いる。ビットから決め直すと `diff` の写像と 2 本になって静かにずれる。
            snapshot
                .staged
                .extend(entry.head_to_index().as_ref().and_then(file_entry));

            if let Some(file) = entry.index_to_workdir().as_ref().and_then(file_entry) {
                // untracked は「まだ index に無い」だけで unstaged とは操作が違う
                // （stage はできるが unstage / discard の意味が変わる）ので分ける。
                if file.status == FileStatus::Untracked {
                    snapshot.untracked.push(file);
                } else {
                    snapshot.unstaged.push(file);
                }
            }
        }

        snapshot.sort_sections();
        snapshot.progress = in_progress(self.git_dir());
        // **stat するのは一覧に出たファイルだけ**（02 の「ステータス」）。ワークツリー
        // 全体は走査しない。staged だけのファイルも入れる —— 区画で分けると規則が
        // 2 本になる。staged と unstaged の両方に出るファイルは 1 回だけ stat する。
        let mut stamps = BTreeMap::new();
        for entry in snapshot.entries() {
            if stamps.contains_key(&entry.path) {
                continue;
            }
            if let Some(stamp) = FileStamp::of(&self.workdir().join(&entry.path)) {
                stamps.insert(entry.path.clone(), stamp);
            }
        }
        snapshot.stamps = stamps;
        Ok(snapshot)
    }
}
