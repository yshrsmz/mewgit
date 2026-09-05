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
pub struct RepoReader {
    repo: git2::Repository,
    /// `open` で bare を弾いたあとのワークツリーのルート。`workdir()` が
    /// `Option` を返さずに済むよう、不変条件を open で 1 回だけ確立する。
    workdir: PathBuf,
}

impl RepoReader {
    pub fn open(path: &Path) -> Result<Self, GitReadError>;
    pub fn workdir(&self) -> &Path;                                 // ワークツリーのルート
    pub fn remotes(&self) -> Result<Vec<Remote>, GitReadError>;     // name, url（生の値）
    pub fn refs(&self) -> Result<RefIndex, GitReadError>;           // Oid → Vec<RefLabel>
    pub fn head(&self) -> Result<HeadInfo, GitReadError>;           // branch name or detached Oid
    pub fn log(&self) -> Result<LogSession<'_>, GitReadError>;   // ページングは LogSession が持つ
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
- 載せるのはローカルブランチ・リモート追跡ブランチ・タグだけ（ホワイトリスト）。
  `refs/stash` や `refs/notes/*` は他のペインの担当なのでグラフのラベルにしない。
  加えて symbolic ref（`refs/remotes/*/HEAD`）は種別を問わず落とす。解決すると
  指す先の ref とラベルが 2 重になる
- 列挙は `references()` の 1 パスで、種別は `is_branch()` / `is_remote()` / `is_tag()`
  （どれも ref 名の接頭辞判定）で振り分ける。`branches()` も内部では全 ref を列挙して
  から接頭辞で捨てるので、種別ごとに呼ぶと同じ走査を繰り返すことになる
- `is_head` は `HEAD` の symbolic target と ref 名の比較で決める。`Branch::is_head()` は
  呼ぶたびに HEAD を解決し直すのでブランチの本数だけファイル読みが増える。unborn は
  symbolic target の指す ref がまだ無く、detached は HEAD が symbolic ではないので、
  どちらも「どのブランチも HEAD ではない」に落ちる
- ラベルの並べ替え（種別 Local → Remote → Tag、同種内は名前順）は `RefIndex` を作る
  ところに置く。git2 の列挙順は安定しておらず、索引を組む経路が増えたときに
  並べ替えを忘れると描画順とテストの期待値だけが静かにずれる

### 非 UTF-8 の扱い

git は ref 名・パス・メッセージにどんなバイト列も許すので、層の入口で方針を 1 つに決める。

- **識別子**（ref 名、リモート名、パス）が非 UTF-8 なら、その ref / リモート / ファイルを
  一覧から落とす。`git_exec` の argv に載せられず、UI で選べても操作できないため
- **表示にしか使わない文字列**（リモート URL、コミットの summary や署名）は lossy 変換して
  残す。読めない文字が混ざるほうが、その項目ごと消えるより情報が多い
- 例外は HEAD が指すブランチ名で、これだけは落とさず `GitReadError::NonUtf8Head` にする。
  落とすと detached と区別が付かなくなり、上位が別の状態として扱ってしまう

### head

- 通常は `Branch { name, oid }`、detached HEAD は `Detached(oid)`
- コミットが 1 つも無い（unborn）ときは `repo.head()` が `ErrorCode::UnbornBranch` を返すので、
  `HEAD` の symbolic target からブランチ名を取って `Unborn { name }` にする

## ログ列挙

```rust
pub struct LogSession<'repo> { /* Revwalk<'repo> */ }

impl LogSession<'_> {
    pub fn next_page(&mut self, n: usize) -> Result<LogPage, GitReadError>;
}

pub struct LogPage {
    pub commits: Vec<Commit>,
    /// これ以上読むコミットが無い。
    pub done: bool,
}
```

- `Revwalk` に `refs/heads/*` `refs/remotes/*` `refs/tags/*` と HEAD を push、
  `Sort::TOPOLOGICAL | Sort::TIME`。リモート追跡ブランチも push するのは、
  ローカルより進んでいる `origin/main` の行が出ないと `{origin/main}` のラベルを
  載せる行が無くなるため
- push は `push_glob` を使う。単体の `push_ref` と違い、commit まで peel できない ref
  （tree を指すタグなど）を黙って飛ばすので、その 1 本のために列挙全体が失敗しない。
  `refs()` が peel できない ref を落とすのと同じ扱いになる
- HEAD の push は `head_detached()` が真のときだけにする。ブランチに乗っていれば
  `refs/heads/*` で既に入っているので、個別に要るのは detached だけ。
  `Revwalk::push_head` を無条件に呼ぶと、コミットが 1 つも無いリポジトリで返る
  エラーが `GenericError`（"reference 'refs/heads/main' not found"）になり、握りつぶす
  対象を絞り込めない。`head_detached` は HEAD が symbolic なら解決しないので unborn を
  踏まない。代わりに `refs/heads/*` の外を指す symbolic HEAD は押されないが、git が
  作らない状態なので許容する
- ページングの位置は API に持たせない。トポロジカル順の walk は途中の Oid からやり直せない
  （他のブランチとの交互配置が失われる）ため、`Revwalk` を `LogSession` として保持し、
  `next_page(n)` で続きを取る
- **セッションは作り直さず使い回す。** `Sort` を指定した `Revwalk` は libgit2 側で
  `limited` になり、最初の `next_page` で push した ref から到達可能なコミットを全部
  読んでから整列する。つまりセッション生成 + 1 ページ目は履歴全体のコストで、2 ページ目
  以降はほぼタダ。スクロールのたびに `log()` を呼び直すと毎回それを払い直すことになる。
  1 ページ目は必ずリーダースレッド側で読む
- `done` は `Revwalk` を `Peekable` にして 1 つ先読みして決める。「ちょうど n 件で終わった」
  ときに `done: false` を返して空のページをもう 1 回読ませないため
- `LogSession<'repo>` は `RepoReader` を借りる。`Repository` と同じ構造体には入れられない
  （自己参照になる）ので、リーダースレッドではループのローカル変数として
  `Option<LogSession<'_>>` を `RepoReader` の隣に置く
- `next_page` が `Err` を返したらセッションを捨てる。そのページのために walk から取り出した
  Oid は失われており、同じセッションで読み直すとその分がどのページにも現れない。グラフには
  親の見つからない行が残り、2 回目は成功するので UI にも異常が出ない
- 1 コミットあたり `Commit { oid, parents, summary, author, time }` のみ抽出。本文とファイル一覧は選択時に遅延取得
- `Commit::time` は author 日時だが、行の並びは `Sort::TIME`（= committer 日時）で決まる。
  rebase / cherry-pick された履歴では表示日時が並び順と逆転しうる。`git log` と同じ挙動なので
  そのままにする
- summary と署名は表示にしか使わないので、非 UTF-8 でも lossy 変換して残す（「非 UTF-8 の扱い」）

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
    NonUtf8Head,
    ReaderGone,
    Git(git2::Error),
}
```

`Open` は渡されたパスを添える（まだ何も見つかっていないので、それしか手掛かりが無い）。
`BareRepository` は `discover` が見つけたリポジトリのパスを添える。引数のサブディレクトリを
返すと「そのディレクトリが bare だ」と読めてしまうため。

`ReaderGone` はリーダースレッド（下記）が落ちているときに返る。待ち続けると `JobTracker` に
登録されたジョブが永久に残り、同種の `Cmd` が二度と発行されなくなる。

## スレッド

`git2::Repository` は `Send` だが `Sync` ではない。`RepoReader` は `spawn_blocking` 内で毎回 open するか、専用スレッドに所有させてチャネル経由で使う。後者を採用し、`ReaderHandle` をランタイムに渡す。

```rust
pub struct ReaderHandle { /* mpsc::UnboundedSender<Request>, workdir: PathBuf */ }

impl ReaderHandle {
    pub fn spawn(path: &Path) -> Result<Self, GitReadError>;
    pub fn workdir(&self) -> &Path;
    pub async fn log_page(&self, count: usize) -> Result<LogPage, GitReadError>;
    pub async fn refs(&self) -> Result<RefIndex, GitReadError>;
    pub async fn head(&self) -> Result<HeadInfo, GitReadError>;
    pub async fn remotes(&self) -> Result<Vec<Remote>, GitReadError>;
}
```

- `spawn_blocking` を毎回呼ぶ形にできないのは `LogSession` があるため。セッションはスレッドの
  ローカル変数として生き続ける必要がある（「ログ列挙」の「セッションは作り直さず使い回す」）
- スレッドの本体は `RepoReader` をローカル変数に置き、その隣に `Option<LogSession<'_>>` を持つ。
  `LogSession<'repo>` が `RepoReader` を借りるので、同じ構造体には入れられない（自己参照になる）
- 要求は変種ごとに `oneshot::Sender<Result<T, GitReadError>>` を持つ非公開の enum で送る。
  `ReaderHandle` は型付きの async メソッドだけを見せる。公開の要求 / 応答 enum を作ると、
  `Cmd`（01）と `Msg`（01）に並ぶ 3 本目の語彙になり、変種を足すたびに 3 箇所直すことになる
- `open` は呼び出し側で行い、開いた `RepoReader` をスレッドへムーブする（`Repository` は
  `Send`）。bare や存在しないパスは `spawn` の `Err` として起動時に出る。`workdir()` は
  その値を持つので同期で読める。スレッドの中で open して結果を `oneshot` で返す形にすると、
  `?` で足りるところにチャネルが 1 本増える
- ログ列挙の状態は `Option<LogSession>` ではなく `Idle` / `Live` / `Exhausted` の 3 状態で持つ。
  読み切ったセッションを `Live` のまま抱えると、libgit2 が walk のために読んだコミットの
  プールがプロセスの寿命だけ残る（`git_revwalk_free` まで解放されない）。かといって `Idle` に
  戻すと、末尾までスクロールしたあとの 1 回で履歴全体を読み直す。`Exhausted` は空のページを
  `done` 付きで返す
- **`next_page` の `Err` でセッションを捨てる契約は、ここで実装として守る。** スレッドは
  セッションを一度取り出し、成功したときだけ戻す。契約を書ける場所が他に無い
- `ReaderHandle` は `Clone`。ランタイムは `Cmd` ごとに tokio タスクを起こし、その中から呼ぶ。
  スレッドは 1 本なので要求は直列に処理される
- 送信側・受信側のどちらのチャネルが切れても `ReaderGone` にする。ハンドルが全部 drop されたら
  スレッドはループを抜けて終わる
