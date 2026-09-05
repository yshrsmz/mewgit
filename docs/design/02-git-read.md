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
    pub fn open(path: &Path) -> Result<Self, GitReadError>;
    pub fn workdir(&self) -> &Path;                                 // ワークツリーのルート
    pub fn remotes(&self) -> Result<Vec<Remote>, GitReadError>;     // name, url（生の値）
    pub fn refs(&self) -> Result<RefIndex, GitReadError>;           // Oid → Vec<RefLabel>
    pub fn head(&self) -> Result<HeadInfo, GitReadError>;           // branch name or detached Oid
    pub fn walk(&self, from: Option<Oid>, n: usize) -> Result<LogPage, GitReadError>;
    pub fn status(&self) -> Result<WorktreeSnapshot, GitReadError>;
    pub fn diff(&self, target: DiffTarget) -> Result<Vec<FileDiff>, GitReadError>;
    pub fn stashes(&self) -> Result<Vec<StashEntry>, GitReadError>;
    pub fn in_progress(&self) -> Option<InProgressOp>;
}
```

git2 の呼び出しはどれも壊れたリポジトリや I/O エラーで失敗しうるので、参照系も含めてすべて `Result` を返す。
例外は `in_progress` だけで、これは `.git/` のファイルの有無を見るだけだから「進行中の操作は無い」と区別できない。

### open

- `Repository::discover` で探す。リポジトリのサブディレクトリを渡されても開けるようにするため
- bare リポジトリは `GitReadError::BareRepository` で拒否する。status / diff / checkout がいずれも
  ワークツリー前提で、開けても何もできない
- `workdir()` は `discover` が見つけたワークツリーのルート。`app` の `RepoInfo.path` と
  `git_exec` の cwd はこれを使う（引数で渡されたパスそのものではない）

### remotes

`Remote` は `{ name, url }` だけを持つ。URL の解析（host / owner / repo）は `auth`（04）の担当で、
`ssh -G` によるホスト別名の展開が要る。`git_read` から `auth` は参照できない（00 の依存の向き）ので、
ここでは `.git/config` の生の文字列をそのまま返す。

### refs とラベル

```rust
pub struct RefIndex { /* Oid → Vec<RefLabel> */ }

impl RefIndex {
    pub fn labels(&self, oid: Oid) -> &[RefLabel];
}

pub enum RefLabel {
    Local { name: String, is_head: bool },   // "main"
    Remote { name: String },                 // "origin/main"
    Tag { name: String },                    // "v1.0"
}
```

- `Remote` は remote 名とブランチ名を分けず `origin/main` の短縮形 1 つで持つ。06 は
  `{origin/main}` と一続りで描くだけなので、分けても描画側で連結し直すだけになる
- annotated tag は `target()` が tag オブジェクトの Oid なので、commit まで peel してから引く。
  peel しないとラベルがどのグラフ行にも乗らない
- `refs/stash` と symbolic な `refs/remotes/*/HEAD` は除外する。前者は stash ペインの担当で、
  後者を解決すると `origin/main` のラベルが 2 重になる
- `is_head` は `git2::Branch::is_head()` で見る。`repo.head()` との名前比較にしないのは、
  unborn のとき `head()` がエラーになるため
- ラベルは種別（Local → Remote → Tag）、同種内は名前順に並べる。git2 の `references()` の
  列挙順は安定していないので、ここで揃えないと描画もテストも揺れる

### head

- 通常は `Branch { name, oid }`、detached HEAD は `Detached(oid)`
- コミットが 1 つも無い（unborn）ときは `repo.head()` が `ErrorCode::UnbornBranch` を返すので、
  `HEAD` の symbolic target からブランチ名を取って `Unborn { name }` にする

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

## エラー

```rust
pub enum GitReadError {
    Open { path: PathBuf, source: git2::Error },
    BareRepository { path: PathBuf },
    Git(git2::Error),
}
```

`open` の失敗だけは渡されたパスを添える。「どこを開こうとして失敗したか」が分からないと、
`discover` が上位ディレクトリまで探しにいく仕様と合わせてユーザーが原因を追えないため。

## スレッド

`git2::Repository` は `Send` だが `Sync` ではない。`RepoReader` は `spawn_blocking` 内で毎回 open するか、専用スレッドに所有させてチャネル経由で使う。後者を採用し、`ReaderHandle` を `app` に渡す。
