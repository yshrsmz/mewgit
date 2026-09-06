//! diff とハンク分割。設計は `docs/design/02-git-read.md` の「diff とハンク」。

use git2::{DiffLineType, DiffOptions, ErrorCode, Patch, Tree};

use super::{
    file_entry, lossy, DiffTarget, FileDiff, FileEntry, GitReadError, Hunk, HunkLine, LineOrigin,
    RepoReader,
};

impl RepoReader {
    /// 対象の diff をファイル単位で返す。
    ///
    /// ハンクは `git_exec` が `git apply --cached` に渡すパッチへ戻せる形で持つ
    /// （02 の「ハンクの持ち方」）。
    pub fn diff(&self, target: DiffTarget) -> Result<Vec<FileDiff>, GitReadError> {
        let mut opts = DiffOptions::new();
        if let Some(path) = target.path() {
            // pathspec は既定で glob。`[x].txt` や `a*b` という名前のファイルを選ぶと
            // 別のファイルが出る。
            opts.disable_pathspec_match(true).pathspec(path);
        }

        let mut diff = match &target {
            DiffTarget::IndexVsWorktree(_) => {
                // untracked の中身も出す。出さないと worktree ペインで untracked を
                // 選んでも diff が空になる。
                opts.include_untracked(true)
                    .recurse_untracked_dirs(true)
                    .show_untracked_content(true);
                self.repo.diff_index_to_workdir(None, Some(&mut opts))?
            }
            DiffTarget::HeadVsIndex(_) => {
                // unborn は空ツリーと比べる。最初のコミットを作る前でも staged の
                // 中身が見える。
                let head = self.head_tree()?;
                self.repo
                    .diff_tree_to_index(head.as_ref(), None, Some(&mut opts))?
            }
            DiffTarget::Commit { oid, .. } => {
                let commit = self.repo.find_commit(*oid)?;
                // マージも第一親と比べる。combined diff は出さない（`git show` の
                // 既定はマージに何も出さないが、それではペインが常に空になる）。
                // ルートコミットは親が無いので空ツリーと比べる。
                let parent = commit.parents().next().map(|p| p.tree()).transpose()?;
                let tree = commit.tree()?;
                self.repo
                    .diff_tree_to_tree(parent.as_ref(), Some(&tree), Some(&mut opts))?
            }
            DiffTarget::Range { from, to, .. } => {
                let from = self.repo.find_commit(*from)?.tree()?;
                let to = self.repo.find_commit(*to)?.tree()?;
                self.repo
                    .diff_tree_to_tree(Some(&from), Some(&to), Some(&mut opts))?
            }
        };

        // `Patch` を作る前に呼ぶ。あとから呼んでも、取り出し済みの delta には効かない。
        diff.find_similar(None)?;
        file_diffs(&diff)
    }

    /// HEAD のツリー。コミットが 1 つも無ければ `None`（空ツリーと比べる）。
    fn head_tree(&self) -> Result<Option<Tree<'_>>, GitReadError> {
        match self.repo.head() {
            Ok(head) => Ok(Some(head.peel_to_tree()?)),
            Err(err) if err.code() == ErrorCode::UnbornBranch => Ok(None),
            Err(err) => Err(err.into()),
        }
    }
}

/// diff をファイル単位の値型に落とす。
fn file_diffs(diff: &git2::Diff<'_>) -> Result<Vec<FileDiff>, GitReadError> {
    let mut files = Vec::with_capacity(diff.deltas().len());

    for index in 0..diff.deltas().len() {
        // `from_diff` が `None` を返すのは libgit2 が「出さない」と判断した delta
        // （`Unmodified` / `Ignored` / `Unreadable`）だけで、それは `file_entry` も落とす。
        let Some(patch) = Patch::from_diff(diff, index)? else {
            continue;
        };
        // **delta はパッチ側から読む。** libgit2 はバイナリ判定をパッチ生成のときに
        // 行うので、`Diff::get_delta` を先に読むと `is_binary` が立っていないことがある。
        let delta = patch.delta();

        // 種別とパスの決め方は `status` と同じところを通す。
        let Some(FileEntry {
            path,
            old_path,
            status,
        }) = file_entry(&delta)
        else {
            continue;
        };

        files.push(FileDiff {
            path,
            old_path,
            status,
            binary: delta.flags().is_binary(),
            hunks: read_hunks(&patch)?,
        });
    }

    Ok(files)
}

/// パッチをハンクに割る。バイナリと mode だけの変更はハンクが 0 個になる。
fn read_hunks(patch: &Patch<'_>) -> Result<Vec<Hunk>, GitReadError> {
    let mut hunks = Vec::with_capacity(patch.num_hunks());

    for index in 0..patch.num_hunks() {
        let (hunk, line_count) = patch.hunk(index)?;
        let mut lines = Vec::with_capacity(line_count);
        for line in 0..line_count {
            let line = patch.line_in_hunk(index, line)?;
            let content = line.content();
            let Some(origin) = line_origin(line.origin_value(), content) else {
                continue;
            };
            lines.push(HunkLine {
                origin,
                content: trim_eol(content).to_vec(),
            });
        }

        // ヘッダは非 UTF-8 でも lossy でよい。`git apply` は 2 つ目の `@@` より後ろの
        // 関数コンテキストを読まないので、置換文字が混ざってもパッチとしては壊れない。
        hunks.push(Hunk {
            header: lossy(trim_eol(hunk.header())),
            lines,
        });
    }

    Ok(hunks)
}

/// 行末の改行を 1 つだけ落とす。`\r` は内容の一部なので残す
/// （落とすとパッチに戻したときに CRLF のファイルが壊れる）。
fn trim_eol(content: &[u8]) -> &[u8] {
    content.strip_suffix(b"\n").unwrap_or(content)
}

/// 行の種別。改行の有無は libgit2 の EOFNL ではなく内容から決める。
///
/// libgit2 の `\ No newline at end of file` は独立した 1 行として来る。しかも
/// `AddEOFNL` が付くのは `-` 行の後ろで、名前と出る場所が合っていない。マーカー行は
/// 捨て、内容の行が `\n` で終わるかどうかで判定する（02 の「ハンクの持ち方」）。
fn line_origin(kind: DiffLineType, content: &[u8]) -> Option<LineOrigin> {
    let no_eol = !content.ends_with(b"\n");
    match kind {
        DiffLineType::Context if no_eol => Some(LineOrigin::ContextNoEol),
        DiffLineType::Context => Some(LineOrigin::Context),
        DiffLineType::Addition if no_eol => Some(LineOrigin::AdditionNoEol),
        DiffLineType::Addition => Some(LineOrigin::Addition),
        DiffLineType::Deletion if no_eol => Some(LineOrigin::DeletionNoEol),
        DiffLineType::Deletion => Some(LineOrigin::Deletion),
        // マーカー行。
        DiffLineType::ContextEOFNL | DiffLineType::AddEOFNL | DiffLineType::DeleteEOFNL => None,
        // `line_in_hunk` はハンクの中の行しか返さないので来ない。
        DiffLineType::FileHeader | DiffLineType::HunkHeader | DiffLineType::Binary => None,
    }
}
