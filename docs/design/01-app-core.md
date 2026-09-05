# 01. app: 状態管理とイベントループ

`app` は状態と遷移だけを持つ。ここで定義する型はすべて `mewgit-core` 側にあり、
ratatui / crossterm には依存しない（不変ルール 6）。`ui` は下位の型をここ経由で見る。

## AppState

```rust
pub struct AppState {
    pub repo: RepoInfo,               // パス、リモート一覧、正規化済みホスト
    pub account: AccountState,        // Resolved(login, host) | Degraded(reason) | Resolving
    pub graph: GraphState,            // 読込済みコミット、レーン、選択位置、スクロール
    pub worktree: WorktreeState,      // staged / unstaged / untracked と選択
    pub diff: Option<DiffView>,       // 表示中の diff。target が何の diff かを示す
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
`auth`（`ssh -G`）から来るため、どちらか一方の層には置けない。正規化済みの
host / owner / repo は `RepoInfo.target: Option<RepoTarget>` に入れる。下位層に
`RepoInfo` を渡さない（`auth::resolve` は `auth::RepoTarget`、`config::account_for` は
`host` / `owner` / `repo` の 3 つを受ける）。

`AppState` に `Default` は付けない。リポジトリの無い `AppState` を表現可能にしないため。
入口は `AppState::new(repo: RepoInfo)` の 1 つ。サブ状態の `Default` は残す。

`diff` は `worktree` の下ではなく最上位に置く。`Pane::Diff` は独立したペインで、
グラフで選んだコミットの diff もここに入る。何の diff かは `DiffView.target` が示す。

```rust
pub struct DiffView {
    pub target: DiffTarget,
    pub files: Vec<FileDiff>,       // 02 の diff() の戻り値そのまま
    pub cursor: DiffCursor,         // (ファイル, ハンク)
}
```

`Vec<Hunk>` に平坦化しない。`GitOp::StageHunk { path, patch }` に渡す `path` を
復元できなくなるうえ、`DiffTarget` の `path` は全変種で `Option` なので複数ファイルに
またがる diff（コミット詳細）が普通に起きる。

## Pane と Modal

`focus` と `modal` は状態なので `app` で定義する。`ui`（07）はこれを描画するだけで、
自分では定義しない。

```rust
pub enum Pane { Graph, Branches, Stash, Prs, Worktree, Diff }

pub enum Modal {
    Confirm { title: String, body: String, on_yes: Cmd },
    Input { title: String, prompt: String, buf: TextBuffer, intent: InputIntent },
    CommitMessage { buf: TextBuffer, amend: bool },
    RebaseEditor { plan: RebasePlan, cursor: usize },
    PrCreate { title: TextBuffer, body: TextBuffer, base: String, draft: bool },
    AccountPicker { candidates: Vec<GhLogin>, cursor: usize },
    Help,
}
```

`Confirm` の `on_yes` は `Msg` ではなく `Cmd`。破壊的操作（force push、`branch -D`、
discard、abort 系）はすべて `Cmd::Exec(GitOp)` で表せるので、専用の `Msg` 変種を
作る必要がない。`Msg` は入力イベントと非同期完了だけの語彙に保つ。

`Input` はコールバック（`fn(String) -> Msg`）ではなく `InputIntent` というデータを持つ。
`fn` ポインタは環境をキャプチャできず、`BranchRename` の `from` のような文脈を運べないため。

```rust
pub enum InputIntent {
    BranchCreate { start: Option<String>, checkout: bool },
    BranchRename { from: String },
}
```

変種は必要になったマイルストーンで足す。データで持つことで `Modal` が
`PartialEq` を保て、モーダル遷移のテストが書ける。

`TextBuffer` は複数行の編集バッファ。ratatui のウィジェットや `tui-textarea` は
使わない（core が UI に依存してしまうため）。編集操作の実装は M2 の commit タスクで行う。

```rust
pub struct TextBuffer { pub lines: Vec<String>, pub cursor: (usize, usize) }  // (行, 列)
```

## グラフの行

`commits` と `rows` を添字で対応させる並行配列にしない。ずれを型で消すため 1 本にする。

```rust
pub struct GraphState {
    pub lines: Vec<GraphLine>,
    pub lanes: LaneState,       // 末尾行の直後の状態。追加ページはここから再開（06）
    pub cursor: usize,
    pub scroll: usize,
    pub expanded: bool,
    pub loaded_all: bool,
}

pub struct GraphLine { pub commit: Commit, pub row: GraphRow }
```

## 実行中ジョブ

`JobKind` は `Cmd` の変種と 1 対 1 なので、列挙を二重に持たず `Cmd` を単一の真実にする。

```rust
impl Cmd {
    /// この `Cmd` が生む非同期ジョブの種別。`None` / `Batch` / `Quit` は `None`。
    pub fn job_kind(&self) -> Option<JobKind>;
}
```

網羅 `match` で書くので、`Cmd` に変種を足したら実装漏れがコンパイルエラーになる。

## キー入力

crossterm の `KeyEvent` は core に入れられないので、core 側に等価な型を持つ。
`crossterm::event::KeyEvent → app::Key` の変換は `mewgit-tui` 側で行う。

フィールドは非公開にし、コンストラクタを唯一の入口にする。同じ打鍵が 1 通りの表現に
なるよう正規化するため。正規化しないと `Key` の `Eq` / `Hash` が壊れ、`keymap`（08、M6）
の引き当てが静かに外れる。

- `Char` は shift 適用後の値が入るので `shift` を落とす（crossterm は `P` に `SHIFT` を立てて渡す）
- Shift+Tab は端末が `BackTab` + SHIFT として送る。`Tab` + shift / `BackTab` + shift /
  `BackTab` の 3 通りができるので `BackTab`（修飾なし）に寄せる

```rust
pub struct Key { code: KeyCode, mods: Modifiers }

impl Key {
    pub fn with_mods(code: KeyCode, mods: Modifiers) -> Self;   // ここで正規化
    pub fn new(code: KeyCode) -> Self;
    pub fn char(c: char) -> Self;
    pub fn ctrl(c: char) -> Self;
    pub fn code(&self) -> KeyCode;
    pub fn mods(&self) -> Modifiers;
}

pub enum KeyCode {
    Char(char), Enter, Esc, Tab, BackTab, Backspace, Delete, Insert,
    Left, Right, Up, Down, Home, End, PageUp, PageDown, F(u8),
}

pub struct Modifiers { pub ctrl: bool, pub alt: bool, pub shift: bool }
```

`Char` は crossterm と同様、shift 適用後の文字を入れる（`P` は
`Char('P')` であって `Char('p') + shift` ではない）。`mods.shift` が残るのは、
矢印や `Home` など文字を持たず専用のキーコードも無いキーだけ。正規化があるので、
判定は `key == Key::char('q')` のような等値比較で足りる。

## Msg

```rust
pub enum Msg {
    Key(Key), Resize(TermSize), Tick,
    // git_read
    LogLoaded { commits: Vec<Commit>, done: bool },
    StatusLoaded(WorktreeSnapshot),
    DiffLoaded { target: DiffTarget, files: Vec<FileDiff> },
    // git_exec
    ExecFinished(Box<ExecDone>),       // { op, result, progress }
    ProgressDetected(Option<InProgressOp>),
    // auth / github
    AccountResolved(AccountState),
    PrLoaded(Box<Result<Option<PullRequest>, GithubError>>),
    ChecksLoaded(Result<Vec<Check>, GithubError>),
    PrListLoaded(Result<Vec<PullRequest>, GithubError>),
}
```

`Msg` は `Clone` を要求しない。`Account` がトークン（`SecretString`）を持つため。

`AccountResolved` は 04 の `resolve` の戻り値をそのまま運ぶ（`AccountState`）。
`Result<Account, AuthError>` にすると `AuthError` と `DegradedReason` が同じ失敗を
二重に表現し、`update` に手書きのマッピングが生える。`AuthError` は `gh` / `ssh` 実行の
内部エラーに留め、`resolve` の中で `DegradedReason` に畳む。

`ExecFinished` と `PrLoaded` は payload を `Box` に入れる。入れないと `Msg` が 336 バイトに
なり、1 打鍵ごとに作られる `Msg::Key`（実データ 12 バイト）まで同じ大きさで動く。
2 つ Box するだけで 96 バイトになる（実測）。

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

ランタイムは `mewgit-tui` 側にある。`Msg` は core の型なので crossterm の `Event` から
`From` は生やせない（孤児ルール、かつ不変ルール 6）。変換は tui の自由関数で行う。

```rust
loop {
    terminal.draw(|f| ui::render(f, &state))?;
    let msg = tokio::select! {
        Some(Ok(ev)) = term_events.next() => match event_to_msg(ev) { Some(m) => m, None => continue },
        Some(m) = rx.recv() => m,
        _ = tick.tick() => Msg::Tick,
    };
    let cmd = app::update(&mut state, msg);
    if runtime.dispatch(&mut state, cmd, tx.clone()).is_break() {
        break;
    }
}
```

- `event_to_msg` は `Option<Msg>`。マウス・フォーカス・ペーストは対応する `Msg` が無いので落とす
- キー変換は押下（`KeyEventKind::Press`）だけを通す。kitty プロトコルが有効な端末では
  離鍵も飛んでくるため、1 回の打鍵が 2 つの `Msg::Key` になる
- `dispatch` は `ControlFlow` を返す。`Cmd::Quit` で `Break`、`Cmd::Batch` は再帰する
- 未実装の `Cmd` は黙って捨てず `Notice::Warn` に積む。開発中に落ちているのが見えるように
- 初期の端末サイズは `crossterm::terminal::size()` で取る。起動時に `Resize` は飛んでこない
- Git CLI 実行中も入力を受け付けるが、書き込み系 `GitOp` は同時に 1 つのみ（キュー）
- `Tick` は 250ms。進行中ジョブのスピナー更新と、外部変更検知（`.git/HEAD` の mtime 監視）に使う

## 端末の初期化と復元

raw mode と代替画面の切替は `mewgit-tui` の担当。パニックフックで復元してから既定の
フックに渡す。順序を逆にすると、パニックメッセージが raw mode の代替画面に出て読めない。
イベントループが `Err` で抜けた場合も復元する。
