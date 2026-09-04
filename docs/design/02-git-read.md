# 02. git_read: git2 による読み取り層

## 責務

- コミットログの列挙（グラフ用）
- ステータス（staged / unstaged / untracked）
- diff（コミット間、インデックス vs ワークツリー、ハンク分割）
- ref 一覧（ブランチ・タグ・リモート追跡）
- stash 一覧
- 進行中操作の検出

書き込みは一切行わない。ステージ操作も含めて `git_exec` に委譲する（ハンク単位のステージは `git apply --cached` を使うため）。

## API

```rust
pub struct RepoReader { repo: git2::Repository }

impl RepoReader {
    pub fn open(path: &Path) -> Result<Self>;
    pub fn remotes(&self) -> Vec<Remote>;                 // name, url, parsed host/owner/repo
    pub fn refs(&self) -> RefIndex;                       // Oid → Vec<RefLabel>
    pub fn head(&self) -> HeadInfo;                       // branch name or detached Oid
    pub fn walk(&self, from: Option<Oid>, n: usize) -> Result<LogPage>;
    pub fn status(&self) -> Result<WorktreeSnapshot>;
    pub fn diff(&self, target: DiffTarget) -> Result<Vec<FileDiff>>;
    pub fn stashes(&self) -> Result<Vec<StashEntry>>;
    pub fn in_progress(&self) -> Option<InProgressOp>;
}
```

## ログ列挙

- `Revwalk` に全ブランチ・タグ・HEAD を push、`Sort::TOPOLOGICAL | Sort::TIME`
- ページング: `from` を `hide` ではなく「walk を継続する」形で実装するため、`Revwalk` を `LogSession` として保持し、`next_page(n)` で続きを取る
- 1 コミットあたり `Commit { oid, parents, summary, author, time }` のみ抽出。本文とファイル一覧は選択時に遅延取得

## diff とハンク

- `DiffTarget = IndexVsWorktree(path) | HeadVsIndex(path) | Commit(oid, path) | Range(a, b, path)`
- `git2::Diff` → `FileDiff { path, status, hunks: Vec<Hunk> }`
- `Hunk` は元のヘッダ行と各行を保持し、`git_exec` がパッチとして再構成できる形にする

## ステータス

- `StatusOptions` で untracked を含め、`recurse_untracked_dirs` を有効化
- 結果を staged / unstaged / untracked / conflicted に分類

## 進行中操作の検出

| 判定 | InProgressOp |
|---|---|
| `.git/rebase-merge/` 存在 | `Rebase { interactive, current, total, onto }` |
| `.git/rebase-apply/` 存在 | `Rebase { interactive: false, .. }` |
| `.git/CHERRY_PICK_HEAD` | `CherryPick { oid }` |
| `.git/REVERT_HEAD` | `Revert { oid }` |
| `.git/MERGE_HEAD` | `Merge { oid }` |

`git2::Repository::state()` も参考にするが、rebase の進捗（`msgnum` / `end`）はファイルから直接読む。

## スレッド

`git2::Repository` は `Send` だが `Sync` ではない。`RepoReader` は `spawn_blocking` 内で毎回 open するか、専用スレッドに所有させてチャネル経由で使う。後者を採用し、`ReaderHandle` を `app` に渡す。
