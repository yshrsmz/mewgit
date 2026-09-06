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
    pub config: Config,               // 起動時に確定。実行中は再読込しない（08）
}
```

`RepoInfo` は `app` が組み立てる。パスとリモート一覧は `git_read`、ホスト名の正規化は
`auth`（`ssh -G`）から来るため、どちらか一方の層には置けない。正規化済みの
host / owner / repo は `RepoInfo.target: Option<RepoTarget>` に入れる。下位層に
`RepoInfo` を渡さない（`auth::resolve` は `auth::RepoTarget`、`config::account_for` は
`host` / `owner` / `repo` の 3 つを受ける）。

`AppState` に `Default` は付けない。リポジトリの無い `AppState` を表現可能にしないため。
入口は `AppState::new(repo: RepoInfo, config: Config)` の 1 つ。サブ状態の `Default` は残す。

`Config` は `AppState` が持つ。`ui` は `theme` を、`update` は `keymap` と
`graph.page_size` を、ランタイムは `general.git_bin` を読む。`ui` が触れるのは
`AppState` 経由だけ（不変ルール 2）なので、ランタイム側の依存として別に持つと
`theme` を渡す経路が無くなる。

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

## 通知

```rust
impl AppState {
    /// 通知を積む。上限を超えた分は古いものから捨てる。
    pub fn push_notice(&mut self, level: NoticeLevel, message: String, detail: Option<String>);
}
```

- 生産者は `update` とランタイムの両方。組み立てを 1 箇所に寄せ、上限もそこで効かせる
- 上限は 100 件。`ui` は最新の 1 件しか読まないが、履歴表示（M6）まで残しておく
- ステータスバーは**最新の通知とキーヒントの両方**を出す（07）。通知で置き換えると、
  起動時の設定警告 1 件でヒントが二度と戻らなくなる
- 明示的な dismiss と期限切れは M6 のステータスバーのタスクで決める

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

**確認モーダルを組む時点の値を `GitOp` に閉じ込める。** force push の
`GitOp::Push { expected }` がその例で、ユーザーに見せたリモートの位置をそのまま持たせる
（03 の「argv への変換」）。実行時に読み直すと、確認してから実行するまでの間に
リモートが動いても気付けない。

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
    pub refs: RefIndex,         // 行の右に並べるラベル（02）
    pub cursor: usize,
    pub scroll: usize,          // 画面最上部に出す行
    pub expanded: bool,
    pub detail: Option<CommitDetail>,  // 遅延取得した選択コミットの詳細（02）
    pub loaded_all: bool,
}

pub struct GraphLine { pub commit: Commit, pub row: GraphRow }
```

`RefIndex` はここに置く。引くのはグラフペインだけで、`RepoInfo` に入れると
「パスとリモートの入れ物」に描画専用の索引が混ざる。起動時に `head` / `remotes` と
一緒に読む。fetch 後の読み直しは M3 で決める。

`scroll` は状態として持つ。`cursor` と `viewport` から毎回導くと、下端から上へ戻る
ときにカーソルが最下行に貼り付いたまま中身だけが流れる。更新は
`GraphState::move_cursor(delta)` / `scroll_into_view()`。

`viewport` は `ui` がレイアウトから出して `Msg::Layout` で運んでくる（07 の
`ui::graph_rows`）。詳細を出すかの判定（`expanded` かつ行がある）は `ui` 側の
1 つの関数にまとめ、行数を出すときと描くときで同じものを使う。core にヘッダや枠の高さを持たせない。持たせると 07 の
レイアウトを変えたときに黙ってずれるうえ、`ui` の描画と `app` の計算のどちらが
正しいかを決める場所が無くなる。

**`Msg::Resize`（端末サイズ）と `Msg::Layout`（グラフの行数）は別の `Msg`。**
行数が変わる原因は端末サイズだけではなく、`expanded` の切り替えでも変わる
（詳細を出すぶんグラフが縮む）。1 つにまとめると、`Enter` を押しただけなのに
「端末サイズが変わった」という嘘の `Msg` を流すことになる。`Layout` はランタイムが
描画の直前に `ui::graph_rows(state)` と `viewport` を比べ、食い違ったときだけ出す。
`ui::graph_rows` は `term` と `expanded` だけから決まる値を返すこと。詳細の中身
（メッセージの行数など）に依存させると、詳細が届いた瞬間に行数が変わってグラフが
跳ね、`Layout` → 追加読み込みが毎回走る。

### 追加ページの読み込み

`loaded_all` が偽で、**残り行数（`lines.len() - cursor`）が 1 画面以下になったら**
`Cmd::LoadLog { count: page_size }` を出す。判定はカーソルの移動後・ページを積んだ
直後・`Msg::Layout` の 3 箇所。走っているかは見ない。同種のジョブは
`AppState::issue` が落とすので、2 箇所で同じ判断をしない。

カーソルを動かすキーは 1 つの関数（`scrolled`）を通す。腕ごとに書くと、あとから
足したキーが素通りする。下端で `j` を押したときはカーソルが動かないので、
「動いたかどうか」では判定できない。

`page_size` は 0 を許さない（08）。0 件ずつ読むと 1 行も進まないまま「まだ続きがある」
ページが返り、読み込みが止まらない。

読み込みの失敗からは出さない。`log_loaded` の失敗分岐は行を捨てて `cursor` を 0 に
戻すので、そこから再読込を出すと、リーダーが壊れているあいだ「失敗 → 再読込 → 失敗」
を回し続ける。次の読み込みはユーザーの操作から始める。

**「出さない」は 1 回きりの分岐では足りず、`GraphState.load_failed` として状態に持つ。**
行を捨てた直後は「残りが 1 画面以下」が必ず真になるので、そのあとに来るどの
`Msg::Layout` も読み直しを出してしまう。失敗そのものがレイアウトを変えるので
（行が消えると詳細の領域も消える）、失敗が自分で読み直しを引く形になる。
フラグを落とすのはカーソルを動かしたとき（`scrolled`）と、読み込みが成功したとき。

`update` が返した `Cmd` は必ず `runtime` が dispatch する。捨てると `issue` が
`JobTracker` に登録したまま実行されず、以後その種類の `Cmd` が黙って落ち続ける。
`Cmd` に `#[must_use]` を付けてあるので、捨てるとコンパイルが通らない（CI は
`-D warnings`）。意図して捨てるなら `let _ =` と書く。

起動時は `init` を先に流し、そのあとで端末サイズを入れる。逆にすると、サイズを
知った時点で追加読み込みが 1 ページ目を頼み、`init` の `Cmd::LoadLog` が落とされる。
最初のページを頼むのが `init` でなくなり、ここに書いた形とバイナリの動きが食い違う。
`terminal.size()` が失敗したときに読み込みが始まらなくなる、という穴も塞げる。

`LoadRepoInfo` と `LoadLog` はどちらも `tokio::spawn` で投げるので、リーダースレッドに
どちらの要求が先に届くかは決まらない。順序に頼った設計にしない。

### コミット詳細の遅延取得

`Enter` で `expanded` を切り替える。真にしたときと、真のままカーソルが動いたときに
`Cmd::LoadCommitDetail(oid)` を出す。取れた値は `GraphState.detail` に入れる。

- **`detail` は `CommitDetail` をそのまま持ち、どのコミットのものかは `detail.oid` で判る。**
  カーソルの指すコミットと食い違っていれば、まだ届いていない（あるいは失敗した）という
  ことなので、`ui` はそこで読み込み中かエラーかを出し分ける（07）。「読み込み中」の
  フラグを別に持たない
- 既に `detail.oid` がカーソルのコミットと一致していれば出さない。閉じてもう一度開いた
  ときに読み直さないよう、`expanded` を偽にしても `detail` は捨てない
- **鮮度の判定は `GraphState::fresh_detail` の 1 つ。** 次に何を読むかを決める `update` と、
  何を描くかを決める `ui` が同じものを使う。両方に書くと、片方だけ直したときに古い詳細を
  出したままになってもコンパイルは通る
- `JobKind::LoadCommitDetail` は対象の Oid を持たない。`JobTracker` の粒度は種別のまま
  にして、**カーソルが動いたぶんの追いつきは `CommitDetailLoaded` の側で行う**。届いた
  詳細の oid がカーソルのコミットと違えば、そこでもう 1 回 `Cmd::LoadCommitDetail` を出す。
  素早くスクロールしても、飛んでいる要求は常に 1 つで、最後に止まったコミットに収束する。
  Oid を `JobKind` に持たせると、スクロール中の全コミットぶんの要求が同時に走る
- **届いた詳細は、いま見ているコミットのものだけを採る。** 飛んでいる要求は 1 つでも、
  戻ってきたときにカーソルが別のところにいることはある（動いて戻ってきたときなど）。
  無条件に上書きすると、正しいものが出ている画面が「読み込み中」に戻り、同じものを
  もう 1 回読みに行く
- **`Err` からは、失敗したのがいま見ているコミットなら再発行しない。** 通知を出して
  ジョブを畳むだけにする。出すと、リポジトリが壊れているあいだ「失敗 → 再読込 → 失敗」を
  回し続ける（`LoadLog` と同じ）。次の読み込みはユーザーの操作から始める。逆に、
  失敗したのが別のコミットなら、いま見ているものはまだ一度も試していないので頼む。
  ここを一律に止めると「一度も試していないコミット」が
  「詳細を読み込めませんでした」のまま残る
- ログの読み込みが失敗して積んだ行を捨てるときは `detail` も捨てる。`lines` に無い
  コミットの詳細を残さない

## 実行中ジョブ

`JobKind` は `Cmd` の変種と 1 対 1 なので、列挙を二重に持たず `Cmd` を単一の真実にする。

```rust
impl Cmd {
    /// この `Cmd` が生む非同期ジョブの種別。`None` / `Batch` / `Quit` は `None`。
    pub fn job_kind(&self) -> Option<JobKind>;
}

impl AppState {
    /// `update` が返す `Cmd` を発行してよい形にする。同種が走っていれば `Cmd::None`。
    pub fn issue(&mut self, cmd: Cmd) -> Cmd;
}
```

網羅 `match` で書くので、`Cmd` に変種を足したら実装漏れがコンパイルエラーになる。

登録は `update` の出口（`issue`）で行う。ランタイムの `dispatch` が `AppState` に
書いてよいのは `notices` だけなので、そちらでは登録できない。`Batch` は中身を 1 つずつ
判定する。まとめて弾くと、同種が走っている `Cmd` 1 つのために他の `Cmd` まで落ちる。

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
    Key(Key), Resize { term: TermSize }, Layout { graph_rows: usize }, Tick,
    // git_read
    LogLoaded(Result<LogPage, GitReadError>),
    CommitDetailLoaded { oid: Oid, result: Box<Result<CommitDetail, GitReadError>> },
    RepoInfoLoaded(Box<Result<RepoSnapshot, GitReadError>>),  // { remotes, head, refs }
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

`RepoInfoLoaded` はリモート・HEAD・ref の索引を 1 通で運ぶ。どれもリポジトリの ref から
導かれる値で、fetch の後には揃って古くなる。一部だけ成功という形にしないのは、途中まで
新しい状態を持っても意味が無いため。失敗しても通知だけで続ける（ラベルもリモートも
無いままグラフは出せる）。

`LogLoaded` は `Result` を運ぶ。失敗の通り道が無いと `JobKind::LoadLog` が走ったまま
残り、以後の `Cmd::LoadLog` が `JobTracker` に黙って落とされて二度と読めなくなる。
受けたら成功・失敗を問わず `jobs.finish(LoadLog)` する。失敗のときは積んだ行も捨てる。
失敗したセッションは捨てられ、次の読み込みは履歴の先頭から来る（02）ので、残すと
同じコミットが二重に並ぶ。

`AccountResolved` は 04 の `resolve` の戻り値をそのまま運ぶ（`AccountState`）。
`Result<Account, AuthError>` にすると `AuthError` と `DegradedReason` が同じ失敗を
二重に表現し、`update` に手書きのマッピングが生える。`AuthError` は `gh` / `ssh` 実行の
内部エラーに留め、`resolve` の中で `DegradedReason` に畳む。

`CommitDetailLoaded` も `Result` を運ぶ。`LogLoaded` と同じで、失敗の通り道が無いと
`JobKind::LoadCommitDetail` が走ったまま残り、以後の要求が黙って落ちる。**要求した Oid を
一緒に運ぶ。** `Err` のときにどのコミットが失敗したのかを知る手掛かりがこれしかなく、
無いと「別のコミットの失敗」と「いま見ているコミットの失敗」を区別できない。

`ExecFinished` と `PrLoaded` と `CommitDetailLoaded` は payload を `Box` に入れる。入れないと `Msg` が
数百バイトになり、1 打鍵ごとに作られる `Msg::Key`（実データ 12 バイト）まで同じ大きさで動く。
これらを Box すると 96 バイトに収まる。上限は `msg.rs` のテスト（128 バイト）で見張る。

## Cmd

```rust
pub enum Cmd {
    None, Batch(Vec<Cmd>),
    LoadLog { count: usize },           // 読む位置は git_read の LogSession が持つ（02）
    LoadCommitDetail(Oid),             // 選択コミットのメッセージ全文など（02）
    LoadStatus, LoadDiff(DiffTarget),
    LoadRepoInfo,                      // remotes / head / refs。起動時と ref 変更後（M3）
    Exec(GitOp),                       // git CLI 実行（03 参照）
    ResolveAccount,
    FetchPr, FetchChecks, FetchPrList,
    Quit,
}
```

## 起動時の読み込み

```rust
pub fn init(state: &mut AppState) -> Cmd;
```

`runtime::run` がループに入る前に 1 回呼ぶ。`main.rs` から呼ぶと、テストを書かない
配線の側に処理が出てしまう。いまは `LoadRepoInfo` と
`LoadLog { count: config.graph.page_size }` の 2 つで、`LoadStatus` / `ResolveAccount` は
それぞれのタスクでここに足す。

## update の原則

- `update` は `AppState` を直接変更し、`Cmd` を返す。I/O を呼ばない。返す `Cmd` は
  出口で `AppState::issue` を通し、同種のジョブが走っていれば発行しない
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
pub fn open(path: &Path, config: Config) -> Result<(AppState, ReaderHandle), GitReadError>;

pub struct Runtime { reader: ReaderHandle, tx: mpsc::UnboundedSender<Msg> }

let (runtime, mut msgs) = Runtime::new(reader);
runtime.dispatch(&mut state, app::init(&mut state));
loop {
    // レイアウトの変化（リサイズ・詳細の展開）を core に伝える唯一の経路。
    runtime.dispatch(&mut state, layout_cmd(&mut state));
    terminal.draw(|f| ui::render(f, &state))?;
    let msg = tokio::select! {
        Some(ev) = events.next() => match event_to_msg(ev?) { Some(m) => m, None => continue },
        Some(msg) = msgs.recv() => msg,
        _ = tick.tick() => Msg::Tick,
    };
    let cmd = app::update(&mut state, msg);
    if runtime.dispatch(&mut state, cmd).is_break() {
        return Ok(());
    }
}
```

`open` がやるのは `RepoReader::open` だけ。bare や存在しないパスは起動できないので
`Err` を返す（端末を初期化する前に出す）。リモート・HEAD・ref は `LoadRepoInfo` として
ループの中で読む。ref の列挙は本数ぶんの object 読み出しになるので、ここで待つと
最初の 1 フレームが出るまで画面が真っ黒になる。

初期の端末サイズも `run` がループの前に `Msg::Resize` として `update` に流す。
`AppState` を直接書かないことで、`viewport` の計算経路を 1 本に保つ。グラフの行数は
そのあとループの先頭の `Msg::Layout` が入れる（`terminal.size()` が失敗しても、
既定のサイズぶんの `viewport` は入る）。

`run` は `Backend` とイベントストリームをジェネリックに取る。`TestBackend` と
`futures::stream::iter` を渡してループ全体をテストできるようにするため。テストを
書かない例外は端末の raw mode 切替・復元と `main.rs` の配線だけ（CLAUDE.md）。

- `event_to_msg` は `Option<Msg>`。マウス・フォーカス・ペーストは対応する `Msg` が無いので落とす。
  実装は `mewgit-tui/src/input.rs`（`keymap` の名前は 08 のキーバインド設定に取っておく）
- ストリームのエラーは握りつぶさず `?` で上げる（不変ルール 4）
- キー変換は押下（`KeyEventKind::Press`）だけを通す。kitty プロトコルが有効な端末では
  離鍵も飛んでくるため、1 回の打鍵が 2 つの `Msg::Key` になる
- `dispatch` は `ControlFlow` を返す。`Cmd::Quit` で `Break`、`Cmd::Batch` は再帰する
- 非同期 `Cmd` は tokio タスクにし、結果を `Msg` にして mpsc に流す。**ログ読み込みの
  タスクは abort しない。** `ReaderHandle::log_page` の future を途中で捨てると、
  リーダースレッドは walk を進めたあとで返り先を失い、そのページのコミットがどの
  ページにも現れなくなる（02 の「スレッド」）。M3 のキャンセルは `Cmd::Exec` に効かせる
- `dispatch` の `match` でも `_` を使わず全 `Cmd` 変種を列挙する。`update` / `Cmd::job_kind`
  と同じ理由で、変種を足したときに実装漏れをコンパイルエラーにするため
- 未実装の `Cmd` は黙って捨てず `Notice::Warn` に積む。開発中に落ちているのが見えるように
- **`dispatch` が `AppState` に書いてよいのは `notices` だけ。** それ以外の状態変化は
  `Msg` にして `update` に戻す。状態遷移の入口を `update` 1 つに保つため
- 初期の端末サイズは `Terminal::size()` で取り、`Msg::Resize` にして `update` に流す。
  起動時に `Resize` イベントは飛んでこない
- **描画の直前に毎回 `ui::graph_rows(state)` と `state.graph.viewport` を比べ、違えば
  `Msg::Layout` を流す。** これがレイアウトの変化を core に伝える唯一の経路になる。
  端末のリサイズも `Enter` の展開も同じところを通るので、片方だけ更新を忘れることがない
- Git CLI 実行中も入力を受け付けるが、書き込み系 `GitOp` は同時に 1 つのみ（キュー）
- `Tick` は 250ms。進行中ジョブのスピナー更新と、外部変更検知（`.git/HEAD` の mtime 監視）に使う

## 端末の初期化と復元

`ratatui::try_init()` / `ratatui::restore()` に委譲する。raw mode の有効化、代替画面への
切替、復元してから既定のフックに渡すパニックフックの設置まで含めて ratatui が持っている
（自前で書くと復元順を間違えやすい。raw mode を先に落とすのが正しい）。

イベントループが `Err` で抜けた場合も復元する。`restore()` は `()` を返すので、
ループのエラーを後始末のエラーで踏み潰すことがない。

## 終了経路

- `q`（修飾なし）で終了。モーダル表示中は入力文字なので終了しない
- `Ctrl-C` でも終了する。raw mode では SIGINT が届かず、モーダルで `q` が入力文字に
  なる（M2 の `CommitMessage`）ため、常に効く脱出口が要る。モーダルより先に判定する
- M3 で実行中ジョブのキャンセルを入れるときは、ジョブがあれば `Ctrl-C` はキャンセルを
  優先し、無いときだけ終了する
- 編集中のモーダルを `Ctrl-C` で捨てる際の確認は M2 の commit タスクで判断する
