# 01. app: 状態管理とイベントループ

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
}
```

## Msg

```rust
pub enum Msg {
    Key(KeyEvent), Resize(u16, u16), Tick,
    // git_read
    LogLoaded { commits: Vec<Commit>, done: bool },
    StatusLoaded(WorktreeSnapshot),
    DiffLoaded { target: DiffTarget, hunks: Vec<Hunk> },
    // git_exec
    ExecFinished { op: GitOp, result: Result<ExecOutput, ExecError> },
    ProgressDetected(Option<InProgressOp>),
    // auth / github
    AccountResolved(Result<Account, AuthError>),
    PrLoaded(Result<Option<PullRequest>, GithubError>),
    ChecksLoaded(Result<Vec<Check>, GithubError>),
    PrListLoaded(Result<Vec<PullRequest>, GithubError>),
}
```

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
- キー入力は `focus` と `modal` を見て、対応する pane / modal の handler に委譲する
- `ExecFinished` を受けたら必ず `LoadStatus` + `ProgressDetected` を発行し、状態を再読込する（成功・失敗問わず）
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
