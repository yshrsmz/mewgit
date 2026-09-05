# 01. app: 状態管理とイベントループ

`app` は状態と遷移だけを持つ。ここで定義する型はすべて `mewgit-core` 側にあり、
ratatui / crossterm には依存しない（不変ルール 6）。`ui` は下位の型をここ経由で見る。

## AppState

```rust
pub struct AppState {
    pub repo: RepoInfo,               // パス、リモート一覧、正規化済みホスト
    pub account: AccountState,        // Resolved(login, host) | Degraded(reason) | Resolving
    pub graph: GraphState,            // 読込済みコミット、レーン、選択位置、スクロール
    pub worktree: WorktreeState,      // staged / unstaged / untracked、選択、ハンク
    pub branches: BranchState,
    pub stashes: Vec<StashEntry>,
    pub prs: PrState,                 // 現ブランチ PR、一覧、チェック
    pub progress: Option<InProgressOp>, // Rebase | CherryPick | Revert | Merge
    pub focus: Pane,
    pub modal: Option<Modal>,
    pub jobs: JobTracker,             // 実行中の非同期タスク（表示用）
    pub notices: VecDeque<Notice>,    // エラー・情報のトースト
    pub term: TermSize,               // 端末サイズ。レイアウト分岐に使う
}
```

`RepoInfo` は `app` が組み立てる。パスとリモート一覧は `git_read`、ホスト名の正規化は
`auth`（`ssh -G`）から来るため、どちらか一方の層には置けない。下位層に `RepoInfo` を
渡さない（`auth::resolve` は `auth::RepoTarget`、`config::account_for` は
`host` / `owner` / `repo` の 3 つを受ける）。

## Pane と Modal

`focus` と `modal` は状態なので `app` で定義する。`ui`（07）はこれを描画するだけで、
自分では定義しない。

```rust
pub enum Pane { Graph, Branches, Stash, Prs, Worktree, Diff }

pub enum Modal {
    Confirm { title: String, body: String, on_yes: Box<Msg> },
    Input { title: String, prompt: String, buf: TextBuffer, on_submit: fn(String) -> Msg },
    CommitMessage { buf: TextBuffer, amend: bool },
    RebaseEditor { plan: RebasePlan, cursor: usize },
    PrCreate { title: TextBuffer, body: TextBuffer, base: String, draft: bool },
    AccountPicker { candidates: Vec<GhLogin>, cursor: usize },
    Help,
}
```

`TextBuffer` は複数行の編集バッファ。ratatui のウィジェットや `tui-textarea` は
使わない（core が UI に依存してしまうため）。編集操作の実装は M2 の commit タスクで行う。

```rust
pub struct TextBuffer { pub lines: Vec<String>, pub cursor: (usize, usize) }  // (行, 列)
```

## キー入力

crossterm の `KeyEvent` は core に入れられないので、core 側に等価な型を持つ。
`crossterm::event::KeyEvent → app::Key` の変換は `mewgit-tui` 側で行う。

```rust
pub struct Key { pub code: KeyCode, pub mods: Modifiers }

pub enum KeyCode {
    Char(char), Enter, Esc, Tab, BackTab, Backspace, Delete, Insert,
    Left, Right, Up, Down, Home, End, PageUp, PageDown, F(u8),
}

pub struct Modifiers { pub ctrl: bool, pub alt: bool, pub shift: bool }
```

`Char` は crossterm と同様、shift 適用後の文字を入れる（`P` は
`Char('P')` であって `Char('p') + shift` ではない）。`mods.shift` は
`Tab` や矢印など文字を持たないキーの修飾にだけ使う。

## Msg

```rust
pub enum Msg {
    Key(Key), Resize(TermSize), Tick,
    // git_read
    LogLoaded { commits: Vec<Commit>, done: bool },
    StatusLoaded(WorktreeSnapshot),
    DiffLoaded { target: DiffTarget, hunks: Vec<Hunk> },
    // git_exec
    ExecFinished { op: GitOp, result: Result<ExecOutput, ExecError>, progress: Option<InProgressOp> },
    ProgressDetected(Option<InProgressOp>),
    // auth / github
    AccountResolved(Result<Account, AuthError>),
    PrLoaded(Result<Option<PullRequest>, GithubError>),
    ChecksLoaded(Result<Vec<Check>, GithubError>),
    PrListLoaded(Result<Vec<PullRequest>, GithubError>),
}
```

`Msg` は `Clone` を要求しない。`Account` がトークン（`SecretString`）を持つためで、
`Modal::Confirm` は `on_yes` を `state.modal.take()` でムーブして取り出す。

## Cmd

```rust
pub enum Cmd {
    None, Batch(Vec<Cmd>),
    LoadLog { from: Option<Oid>, count: usize },
    LoadStatus, LoadDiff(DiffTarget),
    Exec(GitOp),                       // git CLI 実行（03 参照）
    ResolveAccount,
    FetchPr, FetchChecks, FetchPrList,
    Quit,
}
```

## update の原則

- `update` は `AppState` を直接変更し、`Cmd` を返す。I/O を呼ばない
- キー入力は `focus` と `modal` を見て、対応する pane / modal の handler に委譲する。
  ペインが `Cmd` を組み立てるのは `update` の中だけで、`ui` からは呼ばない
- `ExecFinished` は進行中操作を一緒に運ぶ。`Runner` が実行直後に再検出するため（03）、
  `update` から検出を頼み直さない。受けたら成功・失敗を問わず `progress` を反映し
  `LoadStatus` を発行する。`ProgressDetected` は `.git/HEAD` 監視による外部変更検知用（M6）
- 同種の `Cmd` が実行中なら重複発行しない（`JobTracker` で管理）

## ランタイム

```rust
loop {
    terminal.draw(|f| ui::render(f, &state))?;
    let msg = tokio::select! {
        Some(ev) = term_events.next() => Msg::from(ev),
        Some(m) = rx.recv() => m,
        _ = tick.tick() => Msg::Tick,
    };
    let cmd = app::update(&mut state, msg);
    runtime.dispatch(cmd, tx.clone());   // Cmd → tokio::spawn
}
```

- Git CLI 実行中も入力を受け付けるが、書き込み系 `GitOp` は同時に 1 つのみ（キュー）
- `Tick` は 250ms。進行中ジョブのスピナー更新と、外部変更検知（`.git/HEAD` の mtime 監視）に使う
