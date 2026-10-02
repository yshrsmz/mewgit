//! diff とハンク分割。設計は `docs/design/02-git-read.md` の「diff とハンク」。

use std::path::Path;

use git2::{DiffFindOptions, DiffLineType, DiffOptions, ErrorCode, Patch, Tree};

use super::{
    file_entry, lossy, DiffTarget, FileDiff, FileEntry, GitReadError, Hunk, HunkLine, LineOrigin,
    Oid, PathStamp, RepoReader,
};

impl RepoReader {
    /// 対象の diff をファイル単位で返す。
    ///
    /// ハンクは `git_exec` が `git apply --cached` に渡すパッチへ戻せる形で持つ
    /// （02 の「ハンクの持ち方」）。
    pub fn diff(&self, target: DiffTarget) -> Result<Vec<FileDiff>, GitReadError> {
        // **`path` は `DiffOptions` に渡さない。** pathspec は diff の生成時に効くので、
        // `find_similar` が対にすべき旧パスの delta が先に落ち、1 ファイルに絞った
        // rename が「全行追加の `Added`」になる。そのパッチを `git apply --cached` に
        // 戻すと、rename ではなくファイルの新規追加を index に適用してしまう（02）。
        let mut opts = DiffOptions::new();

        // **ハンクを読む前に採る**（`FileDiff::stamp`）。後で採ると、読んでいる最中に
        // 外で書き換えられたとき、古いハンクに新しい指紋が付いて守りを素通りする。
        // 前に採っておけば、ずれは「一致しない → 断る」側に倒れる。
        let stamp = target
            .live_path()
            .map(|path| self.path_stamp(path))
            .transpose()?;

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
            DiffTarget::Commit { oid, .. } => self.commit_diff(*oid, &mut opts)?,
            DiffTarget::Range { from, to, .. } => {
                let from = self.repo.find_commit(*from)?.tree()?;
                let to = self.repo.find_commit(*to)?.tree()?;
                self.repo
                    .diff_tree_to_tree(Some(&from), Some(&to), Some(&mut opts))?
            }
            DiffTarget::Stash(oid) => self.stash_diff(*oid, &mut opts)?,
        };

        // `Patch` を作る前に呼ぶ。あとから呼んでも、取り出し済みの delta には効かない。
        diff.find_similar(Some(&mut find_options()))?;
        file_diffs(&diff, target.path(), stamp)
    }

    /// コミットが変更したファイルの一覧。中身（ハンク）は組まない（02）。
    ///
    /// 右ペインのコミット詳細に並べ、選んだ 1 件が
    /// `diff(DiffTarget::Commit { path: Some(..) })` で Diff ペインに出る（07）。
    ///
    /// **`diff(DiffTarget::Commit { path: None })` で代用しない。** 一覧に要るのは
    /// パスと種別だけなので、代用するとコミット全体のハンクを組むことになり、
    /// 依存の更新のような巨大なコミットでリーダースレッドが止まる。
    pub fn commit_files(&self, oid: Oid) -> Result<Vec<FileEntry>, GitReadError> {
        let mut opts = DiffOptions::new();
        let mut diff = self.commit_diff(oid, &mut opts)?;
        // **`diff` と同じ設定で呼ぶ**（[`find_options`]）。ここだけ既定にすると、
        // 一覧では 2 行の Deleted + Added、diff では 1 行の Renamed になる。
        diff.find_similar(Some(&mut find_options()))?;
        Ok(diff
            .deltas()
            .filter_map(|delta| file_entry(&delta))
            .collect())
    }

    /// コミットと第一親の diff。
    ///
    /// **`diff` と `commit_files` の両方がここを通る。** 比べる 2 つのツリーを別々に
    /// 書くと、「一覧に出ているファイルを選んだのに diff が空」という形でずれる（02）。
    /// 出口（[`file_entry`]）だけ共有しても、入口が 2 本あれば結果は揃わない。
    fn commit_diff(
        &self,
        oid: Oid,
        opts: &mut DiffOptions,
    ) -> Result<git2::Diff<'_>, GitReadError> {
        let commit = self.repo.find_commit(oid)?;
        // マージも第一親と比べる。combined diff は出さない（`git show` の既定は
        // マージに何も出さないが、それではペインが常に空になる）。
        // ルートコミットは親が無いので空ツリーと比べる。
        let parent = commit.parents().next().map(|p| p.tree()).transpose()?;
        let tree = commit.tree()?;
        Ok(self
            .repo
            .diff_tree_to_tree(parent.as_ref(), Some(&tree), Some(opts))?)
    }

    /// stash コミットの中身。第一親との差に、第 3 親（untracked）のツリーを足したもの。
    ///
    /// stash コミットの親は「stash した時点の HEAD」「index」「untracked」の順で、
    /// 3 つ目は `--include-untracked` を付けたときだけある（git 2.55 で確認）。
    /// **untracked は空ツリーとの差として混ぜる**ので、種別は `Added` になる ——
    /// 退避したあとの作業ツリーには無いファイルなので、`Untracked` と名乗らせると
    /// 「いまある未追跡のファイル」と読める。
    ///
    /// **index 側（第 2 親）は出さない。** `git stash show` の既定と同じで、stage して
    /// いたかどうかは apply の `--index` の話になる。
    ///
    /// `find_similar` は呼び出し側が混ぜたあとに 1 回だけ掛ける。混ぜる前に掛けると、
    /// tracked の削除と untracked の追加が rename として対にならない。
    fn stash_diff(&self, oid: Oid, opts: &mut DiffOptions) -> Result<git2::Diff<'_>, GitReadError> {
        let stash = self.repo.find_commit(oid)?;
        let base = stash.parent(0)?.tree()?;
        let mut diff =
            self.repo
                .diff_tree_to_tree(Some(&base), Some(&stash.tree()?), Some(opts))?;
        // **`parent(2)` の `Err` を「無い」と読まない。** 数で確かめてから引くので、
        // 壊れたリポジトリで第 3 親が引けないときはそのまま失敗になる。
        if stash.parent_count() > 2 {
            let untracked = stash.parent(2)?.tree()?;
            let untracked = self
                .repo
                .diff_tree_to_tree(None, Some(&untracked), Some(opts))?;
            diff.merge(&untracked)?;
        }
        Ok(diff)
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

/// rename 検出の設定。**`status` と同じ条件にするためのもの。**
///
/// `find_similar(None)` の既定とは 2 点で違う。
///
/// - `for_untracked` を立てる。ワークツリー上の rename（まだ `add` していない
///   `git mv`）は「index から消えた + untracked で増えた」の 2 つの delta なので、
///   これが無いと対にならない。`status` は `renames_index_to_workdir` で検出するため、
///   立てないと同じファイルが status では 1 行の `Renamed`、`diff` では
///   `Deleted` + `Untracked` の 2 件になる
/// - `renames` を明示する。既定は `diff.renames` の設定を読むが、`status` の側は
///   `StatusOptions` で無条件に有効にしている。設定で切っている環境だけ 2 つが
///   食い違うことになる
fn find_options() -> DiffFindOptions {
    let mut options = DiffFindOptions::new();
    options.renames(true).for_untracked(true);
    options
}

/// diff をファイル単位の値型に落とす。`path` があればそのファイルだけ残す。
fn file_diffs(
    diff: &git2::Diff<'_>,
    path: Option<&Path>,
    stamp: Option<PathStamp>,
) -> Result<Vec<FileDiff>, GitReadError> {
    let mut files = Vec::new();

    for index in 0..diff.deltas().len() {
        // **絞り込みは `Patch` を作る手前で行う。** delta のパスは `Patch` を作る前でも
        // 正しい（パッチ生成まで確定しないのは binary の旗だけ）。出口の `Vec<FileDiff>`
        // で `retain` すると、`IndexVsWorktree` は `show_untracked_content` を立てて
        // いるぶん、全 untracked ファイルの中身を読んでから捨てることになる（02）。
        if let Some(want) = path {
            let Some(delta) = diff.get_delta(index) else {
                continue;
            };
            // **`old_path` の側でも見る。** rename を新しい名前で絞ったときに、
            // 旧パスしか持たない delta を落とさないため。
            let hit =
                delta.new_file().path() == Some(want) || delta.old_file().path() == Some(want);
            if !hit {
                continue;
            }
        }

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
            stamp,
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
