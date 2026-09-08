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
    pub diff: DiffState,              // 表示中の diff と、その鮮度
    pub branches: BranchState,     // ローカルブランチの一覧と選択（下の「ブランチ一覧」）
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
pub struct DiffState {
    pub view: Option<DiffView>,     // 届いている diff。未読込 / 失敗のときは None
    pub failed: Option<DiffTarget>, // 直前に失敗した対象。同じ対象には出し直さない
    pub freshness: Freshness,       // Fresh | Stale | Asked（下の「diff のキャッシュと鮮度」）
}

pub struct DiffView {
    pub target: DiffTarget,
    pub files: Vec<FileDiff>,       // 02 の diff() の戻り値そのまま
    pub cursor: DiffCursor,         // (ファイル, ハンク)
}
```

**`failed` は `DiffView` の中には置けない。** 失敗したときは `view` が `None` なので、
中に置くと印ごと消える。

`Vec<Hunk>` に平坦化しない。`GitOp::StageHunk { path, patch }` に渡す `path` を
復元できなくなるうえ、`DiffTarget` の `path` は全変種で `Option` なので複数ファイルに
またがる diff を表せる形になっている。**いま `app` が出す対象はすべて `path` で 1 ファイルに
絞ってある**（コミットの diff も一覧で選んだ 1 件。下の「diff の遅延取得」）ので、複数
ファイルが届く経路は無い。`Range` や stash を出すときに戻ってくる。

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
pub enum Pane { Graph, Branches, Stash, Prs, Worktree, CommitDetail, Diff }

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

巡回に入れるのは実装済みで、**かついま画面に出ているペインだけ**（07）。一覧は定数では
なく `AppState::focus_candidates()` が状態から返す。グラフが WIP 行を選んでいれば
`[Graph, Branches, Worktree, Diff]`、コミットを選んでいれば
`[Graph, Branches, CommitDetail, Diff]`。並びは `Pane` の宣言順。**`Branches` は
どちらにも入る**（右列の上に常に出ていて、左の選択で中身が変わらないため）。
**`Diff` もどちらにも入るが、出る diff は違う**（ワーキングツリーのファイルか、
コミットのファイルか）。決めるのは `AppState::wanted_diff()` の 1 つで、`focus` と
選択の両方を見る（下の「diff の遅延取得」）。

**選択と右ペインの対応は `GraphSelection::right_pane()` の 1 つ。** `Enter` の行き先
（グラフの `Enter` は右ペインへ移る）と巡回の候補が別々に決めると、画面はコミット詳細
なのにキーは Working tree が受ける状態を作れる。`ui::render` だけは中身（`&GraphLine`）を
取り出す必要があるので変種で分岐するが、**候補が必ず `right_pane()` を含むことは
テストで見張る**。

**候補を状態から出すのは、`focus` と右ペインの中身が食い違う状態を表現できなくする
ため。** コミットを選んだまま `focus` が `Worktree` でいられると、`s` / `u` / `x` が
画面に出ていないファイルを stage する。キーごとにガードを置く形にすると、キーを足す
たびに書き足す必要があり、書き忘れが静かに通る。グラフのカーソルが WIP 行を出入り
したところで、候補から外れた `focus` を `Graph` に座り直させる。**`Graph` を選ぶのは
「どちらの選択でも候補にある」からだけではない**（`Branches` と `Diff` もそうなった）。
座り直しはユーザーが選んでいない移動なので、いま見ている左の領域に戻す（07）。

**`Pane::Diff` は左の領域を `Graph` と差し替えで使う**ので、どちらを描くかは `focus`
だけで決まり、「いまどちらを見せているか」を別に持たない（07）。**`Pane::CommitDetail`
は右の領域を `Worktree` と差し替えで使うが、そちらは `focus` では決めない。** 右ペインは
左で選んでいるものの中身なので、フォーカスが左にあるあいだも追随する必要がある。

`Confirm` の `on_yes` は `Msg` ではなく `Cmd`。破壊的操作（force push、`branch -D`、
discard、abort 系）はすべて `Cmd::Exec(GitOp)` で表せるので、専用の `Msg` 変種を
作る必要がない。`Msg` は入力イベントと非同期完了だけの語彙に保つ。

`Confirm` のキーは `y` / `Enter` で実行、`n` / `Esc` で取り消し。**実行の側は先にモーダルを
閉じてから `on_yes` を返し、その `Cmd` も `update` の出口の `issue` を通す。** 閉じずに
返すと、`Cmd` が `JobTracker` に落とされたときに確認モーダルだけが画面に残る。
`q` はモーダル表示中は素通ししない（`CommitMessage` では入力文字になるため。脱出口は
`Ctrl-C`）。

**確認モーダルを組む時点の値を `GitOp` に閉じ込める。** force push の
`GitOp::Push { expected }` がその例で、ユーザーに見せたリモートの位置をそのまま持たせる
（03 の「argv への変換」）。実行時に読み直すと、確認してから実行するまでの間に
リモートが動いても気付けない。

`Input` はコールバック（`fn(String) -> Msg`）ではなく `InputIntent` というデータを持つ。
`fn` ポインタは環境をキャプチャできず、`BranchRename` の `from` のような文脈を運べないため。

```rust
pub enum InputIntent {
    BranchCreate { start: Option<String> },
    BranchRename { from: String },
}
```

変種は必要になったマイルストーンで足す。データで持つことで `Modal` が
`PartialEq` を保て、モーダル遷移のテストが書ける。

- **`start` は起点。** `None` が HEAD（Branches ペインの `n`）、`Some` がグラフで
  選んでいるコミットの Oid（Graph ペインの `n`）。**どちらも UI から踏める**ので
  `Option` を持つ。タグを起点にしたいときは、そのタグのラベルが乗った行を
  グラフで選ぶ（タグ一覧のペインは無い。07 の「Graph ペイン」）
- **`GitOp::BranchCreate` の `switch: bool` は `InputIntent` に持たせない。**
  UI からの作成は常に切り替える（理由は 03 の「argv への変換」に置いた）

`TextBuffer` は複数行の編集バッファ。ratatui のウィジェットや `tui-textarea` は
使わない（core が UI に依存してしまうため）。

```rust
pub struct TextBuffer { pub lines: Vec<String>, pub cursor: (usize, usize) }  // (行, 列)

impl TextBuffer {
    pub fn from_text(text: &str) -> Self;
    pub fn text(&self) -> String;
    pub fn insert_char(&mut self, c: char);
    pub fn insert_text(&mut self, text: &str);   // 貼り付け。改行で行を割り、制御文字は落とす
    pub fn insert_line(&mut self, text: &str);   // 行を増やさずに入れる（改行も制御文字として落ちる）
    pub fn insert_newline(&mut self);
    pub fn backspace(&mut self);
    pub fn delete(&mut self);
    pub fn move_cursor(&mut self, to: Motion);   // Left | Right | Up | Down | Home | End
}
```

- **`cursor` の列は「文字数」で数える。** バイト位置にすると非 ASCII の 1 文字を
  消すのに何バイト戻るかを呼び出し側が知ることになり、UI が桁を出すときにも
  変換が要る。挿入・削除の中でだけ文字位置をバイト位置に直す
- **`lines` は必ず 1 行以上**（空のバッファも `[""]`）。`cursor` の指す行が常に
  存在するようにするため。`backspace` で最後の行を消さない
- **行末より右・最終行より下へは出ない。** 上下移動で短い行に移ったときは行末へ
  寄せる。「元の桁を覚えて長い行で戻す」形にはしない（覚えた桁は状態が増えるうえ、
  編集で無効になる時点を決められない）
- **`Motion` は core の enum。** `KeyCode` をそのまま渡すと、文字キーまで移動として
  受けられる形になる

### コミットメッセージの編集

`Modal::CommitMessage` のキーは他のモーダルと違い、**文字キーがすべて入力になる**。

| キー | 動作 |
|---|---|
| 文字 | 挿入 |
| `Enter`（修飾なし） | 確定（`Cmd::Exec(GitOp::Commit)`） |
| `Alt+Enter` / `Shift+Enter` / `Ctrl-J` | 改行 |
| `Backspace` / `Delete` | 削除 |
| `←` `→` `↑` `↓` `Home` `End` | カーソル移動 |
| `Esc` | 取り消し |
| `Ctrl-C` | 終了（`update_key` がモーダルより先に見る） |

**改行のキーを 3 つ受けるのは、1 つでは全端末を覆えないため。** 素の端末は
`Shift+Enter` を `Enter` と同じ `\r` で送り、修飾情報を載せない。そこで Claude Code が
採っている形（`/terminal-setup` が書き込む設定を実際に読んで確かめた）に合わせる。

| キー | 届き方 | 効く端末 |
|---|---|---|
| `Alt+Enter` | `ESC CR`。crossterm が `Enter` + `ALT` に直す | `Shift+Enter` に `\u001b\r` を割り当てた端末（VS Code / Zed / Alacritty の設定）と、Option を Meta にした Terminal.app の `Option+Enter` |
| `Shift+Enter` | `CSI 13;2u` | kitty keyboard protocol を有効にした端末（Ghostty / kitty / WezTerm / foot など） |
| `Ctrl-J` | `0x0A`。raw mode では `\n` が `Ctrl+j` として届く | すべて |

- **`Enter` の意味は端末によらず確定。** kitty protocol が効いていてもいなくても
  変わらない。**増えるのは受けられる改行キーだけ**にする。「対応端末では `Enter` が
  改行、非対応端末では確定」のように割ると、tmux を挟んだだけで同じキーの結果が
  変わり、改行のつもりの打鍵がコミットになる
- kitty protocol の有効化は `main.rs`（端末の初期化。テストを書かない配線）で行う。
  押す旗は `DISAMBIGUATE_ESCAPE_CODES` だけにする。`REPORT_EVENT_TYPES` まで押すと
  離鍵も届き、1 打鍵が 2 つの `Msg::Key` になる
- **対応するかを端末に問い合わせない。** `crossterm::terminal::supports_keyboard_enhancement`
  は `CSI ?u` を投げて応答を待ち、**応答しない端末では 2 秒待ってから諦める**
  （crossterm 0.28.1 の `terminal/sys/unix.rs`）。pty で測ると起動が 0.82 秒 →
  2.04 秒になった。対応しない端末ほど黒い画面を長く見ることになる。`CSI > 1 u` は
  private な CSI なので、解さない端末は読み飛ばす。押せたかを知る必要も無い
  （`Enter` の意味はどちらでも変わらず、増えるのは受けられる改行キーだけ）
- **`Ctrl-Enter` は使えない。** 端末が `Enter` と区別して送らない
- **`Esc` の取り消しに確認を挟まない。** `AppState.modal` は 1 つしか持てず、確認を
  重ねるにはモーダルのスタックが要る。不変ルール 5 が確認を求めるのは git の破壊的
  操作で、編集中バッファの取り消しは含まれない。`Ctrl-C` も同じ（そちらは終了なので
  なおさら挟めない）。**押した本文が消えるのは事実なので、キーの案内に `Esc` を出す**
- **空のメッセージは `update` が断る。** `git commit --message ""` に任せない。git が
  落ちる時点でモーダルは閉じていて、書いた本文が戻らない。空白だけの行も同じ
  （`trim` して空なら断る）
- **`Cmd::Exec` が `issue` に落とされるなら、モーダルを閉じない。** `Confirm` は先に
  閉じてから `on_yes` を返すが、あちらはユーザーが打ったものを持っていない。同じ形に
  すると、前の git 実行（遅い pre-commit hook など）が走っているあいだに確定したときに
  「ほかの git 実行が終わるまで待ってください」だけが出て**本文ごと消える**。判定は
  `jobs.is_running(JobKind::Exec)`（`load_diff` が `Freshness::Asked` を決めるのと同じ形）
- **貼り付けは `Msg::Paste` で 1 通で受ける。** 端末に bracketed paste を要求しないと、
  貼り付けた文字は 1 文字ずつ `Msg::Key` として届く。**コミットメッセージを貼ると
  最初の改行で確定し、残りがペインのキーとして解釈される**（`x` で discard の確認が
  開き、あとから来た `y` がそれを実行する）。要求するのは `main.rs`、バッファへ入れる
  のは `TextBuffer::insert_text`。**受けるのは編集中のモーダルだけ**で、それ以外では
  捨てる（ペインには行き先が無く、`Msg::Key` に割り直すと有効にした意味が消える）。
  制御文字は落とす（CR は CRLF、タブは `ui` が数える桁とずれてカーソルが合わなくなる）
- `amend` は `Modal::CommitMessage` が持つ。`GitOp::Commit { message, amend }` に
  そのまま渡す

### commit

`c` と `C` はワーキングツリーペインで受ける（07 のキーバインド）。

- **`c` は staged が空なら断る。** ただし**いま持っている一覧を信じられるときだけ**。
  書き込み直後の一覧は古く（`WorktreeState.stale`）、一度も読めていない一覧は空と
  区別が付かない（`WorktreeState.loaded`）。どちらを根拠にしても「変更がありません」は
  嘘になりうるので、そのときは開いて git の側に判定を任せる。**空の `snapshot` には
  2 つの意味がある** — 「変更が無い」と「まだ読めていない」。`stale` では後者を
  表せない（あれは書き込みでしか立たない）
- **`C`（amend）は staged が空でも通す。** reword だけの amend は正当な操作
- **amend の初期値は `Cmd::LoadHeadMessage` で読む。** `RepoInfo.head` からは取らない。
  あれは起動時に 1 回読んだきりで、1 回コミットした時点で古い（`Cmd::LoadRepoInfo` が
  `issue` に落とされていれば commit のあとも古いまま）。**古い初期値は「1 つ前の
  コミットのメッセージで HEAD を上書きする」になる。** HEAD の解決と本文の取得は
  リーダースレッドの中で 1 回で行い、途中で HEAD が動く隙を作らない
- **届いた本文でモーダルを開くのは `modal` が空のときだけ。** `C` を押してから届く
  までのあいだに別のモーダル（`x` の確認など）が開いていることがある。無条件に
  上書きすると、確認モーダルが編集画面に化ける
- コミットが 1 つも無い（unborn HEAD）ときは `Ok(None)` が返る。通知を出して開かない

### 1 行入力

`Modal::Input` の最初の利用者はブランチの作成（`n`）とリネーム（`r`）。
`CommitMessage` と同じく**文字キーがすべて入力になる**が、行が 1 本しか無い。

| キー | 動作 |
|---|---|
| 文字 | 挿入 |
| `Enter`（修飾の有無を問わず） | 確定（`InputIntent` の `GitOp` を出す） |
| `Ctrl-J` | 何もしない |
| `Backspace` / `Delete` | 削除 |
| `←` `→` `Home` `End` | カーソル移動 |
| `↑` `↓` | 何もしない |
| `Esc` | 取り消し |
| `Ctrl-C` | 終了（`update_key` がモーダルより先に見る） |

- **改行のキーは 1 つも受けない。** `CommitMessage` が `Alt+Enter` / `Shift+Enter` /
  `Ctrl-J` を改行にしているのは複数行を打つためで、こちらは行が 1 本。受けると
  `TextBuffer` に 2 行目ができ、`text()` が返す `\n` 入りの文字列がそのまま
  ブランチ名になる。**修飾付きの `Enter` を確定に落とすのは、`CommitMessage` の
  つもりで `Alt+Enter` を打った人に「何も起きない」を返さないため**（そちらの
  改行キーはここでは意味を持たない）。`Ctrl-J` だけは確定に落とさない —
  修飾付きの文字キーは入力にしない規則（`CommitMessage` と同じ）に沿う
- **`lines` が 1 本のままであることをテストで見張る。** 改行のキーと貼り付けの
  どちらからも増やせないので、増えたときに気付ける腕がここにしか無い
- **貼り付け（`Msg::Paste`）は改行を除いて 1 行につなぐ。** 除くのは
  `TextBuffer::insert_line`（改行も制御文字として落ちる）で、**`paste` の側で
  `replace` してから `insert_text` に渡さない** — 改行をどう扱うかの判断が 2 箇所に
  割れる。落とすと「末尾に改行が付いただけ」の正当な 1 行貼り付けまで拒むことになり、
  1 行目だけを採ると残りが黙って消える。**2 つの名前を貼れば連結された 1 つの名前になる**が、それは入力欄に
  そのまま出ていて、`Enter` を押すまで git は走らない。**`Msg::Paste` を受ける
  モーダルが 2 つ目になる** — `paste` が `CommitMessage` にしか腕を持たないままだと、
  貼り付けた文字が 1 文字ずつ `Msg::Key` として届く形に戻り、最初の改行で確定して
  残りがペインのキーになる（#34 で踏んだ形）
- **空の入力は `update` が断る**（`commit_submit` と同じ）。`git switch --create ""` は
  `fatal: '' is not a valid branch name` で落ち、その時点でモーダルは閉じていて
  打った文字が戻らない。**空白だけも同じ**（`trim` して空なら断る）
- **`Cmd::Exec` が `issue` に落とされるならモーダルを閉じない**（`commit_submit` と
  同じ理由。閉じると打った文字ごと消える）。**判定を書くのは 2 箇所目**なので、
  `commit_submit` と共有する
- **名前が正しいかは手前で判定しない**（不変ルール 4）。`git check-ref-format` の規則を
  mewgit に写すことになり、写した規則は git の版が変わっても追随しない。空だけを
  断るのは、上のとおり「打った文字が戻らない」を避けるため
- **`r` の初期値はカーソル行のブランチ名**（`TextBuffer::from_text`、カーソルは末尾）。
  リネームは既存の名前を少し直す操作なので、空から打ち直させない。`n` は空で開く

## グラフの行

`commits` と `rows` を添字で対応させる並行配列にしない。ずれを型で消すため 1 本にする。

```rust
pub struct GraphState {
    pub lines: Vec<GraphLine>,
    pub lanes: LaneState,       // 末尾行の直後の状態。追加ページはここから再開（06）
    pub refs: RefIndex,         // 行の右に並べるラベル（02）
    pub cursor: usize,          // 表示行の添字。0 は WIP 行、1.. が `lines`
    pub scroll: usize,          // 画面最上部に出す行
    pub detail: Option<CommitDetail>,  // 遅延取得した選択コミットの詳細（02）
    pub files: Option<CommitFiles>,    // 同じく遅延取得したファイル一覧（下）
    pub detail_failed: Option<Oid>,    // 直前に詳細の読み込みが失敗したコミット
    pub files_failed: Option<Oid>,     // 同じくファイル一覧（下の「失敗の記憶」）
    pub loaded_all: bool,
}

pub struct CommitFiles {
    pub oid: Oid,                 // どのコミットのものか。`detail.oid` と同じ役目
    pub entries: Vec<FileEntry>,  // 02 の commit_files() の戻り値そのまま
    pub cursor: usize,            // 一覧の選択位置。左の Diff の対象を決める
}

pub struct GraphLine { pub commit: Commit, pub row: GraphRow }

pub enum GraphSelection<'a> { Wip, Commit(&'a GraphLine) }
```

**先頭の WIP 行はコミットではないので `lines` に入れない。** かわりに `cursor` を
「表示行」の添字とし、表示行数を `GraphState::rows()`（= `1 + lines.len()`）で表す。
`scroll` / `viewport` / 追加読み込みの算術は表示行の上で回るのでそのまま使える。

**表示行の添字を `lines` の添字に読み替えるのは `GraphState::row(index)` の 1 つ。**
`ui` が窓を切り出すときもここを通す。各所で `lines.get(index - 1)` と書くと、`- 1` の
書き忘れが 1 行ずれとして静かに通り、**反転する行と右ペインの中身が食い違う**。
`selection()` は `row(cursor)`、`cursor_commit()` はその上の薄い包み。

コミット側が `&GraphLine`（コミット + 罫線）なのは、`ui` が罫線を描くのに要るため。
`&Commit` にすると、窓を切り出した `ui` が同じ添字で `lines` を引き直すことになり、
上の「読み替えは 1 箇所」が崩れる。

**WIP 行は変更が無くても常に出す**（07）。件数で出し入れすると `StatusLoaded` が届く
たびに表示行数が変わり、カーソルが指すコミットを oid で座り直させる必要が出る。

`RefIndex` はここに置く。引くのはグラフペインだけで、`RepoInfo` に入れると
「パスとリモートの入れ物」に描画専用の索引が混ざる。起動時に `head` / `remotes` と
一緒に読む。fetch 後の読み直しは M3 で決める。

`scroll` は状態として持つ。`cursor` と `viewport` から毎回導くと、下端から上へ戻る
ときにカーソルが最下行に貼り付いたまま中身だけが流れる。更新は
`GraphState::move_cursor(delta)` / `scroll_into_view()`。

`viewport` は `ui` がレイアウトから出して `Msg::Layout` で運んでくる（07 の
`ui::graph_rows`）。core にヘッダや枠の高さを持たせない。持たせると 07 のレイアウトを
変えたときに黙ってずれるうえ、`ui` の描画と `app` の計算のどちらが正しいかを決める
場所が無くなる。

**`Msg::Resize`（端末サイズ）と `Msg::Layout`（グラフの行数）は別の `Msg`。**
`Resize` が運ぶのは端末の幅と高さで、そこからグラフの行数を出せるのは `ui` だけ
（ヘッダ・ステータスバー・枠のぶんを知っているのはあちら）。1 つにまとめると、
その引き算を core に持ち込むことになる。`Layout` はランタイムが描画の直前に
`ui::graph_rows(state.term)` と `viewport` を比べ、食い違ったときだけ出す。

**`ui::graph_rows` は `term` だけから決まる値を返すこと。** M1 ではコミット詳細を
グラフの下部に展開していたので「詳細を出すか」も引数にあり、`Enter` を押すだけで
行数が変わっていた。詳細を右ペインへ移してこの依存は消えている（07 の
「CommitDetail ペイン」）。状態を引数に戻さない。詳細の中身（メッセージの行数など）に
依存させると、詳細が届いた瞬間に行数が変わってグラフが跳ね、`Layout` → 追加読み込みが
毎回走る。

### 追加ページの読み込み

`loaded_all` が偽で、**残り行数（`rows() - cursor`）が 1 画面以下になったら**
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

### ログの読み直し

commit は HEAD を動かすので、積んだ行が丸ごと古くなる。**続きを読む経路では直せない**
（新しいコミットは末尾ではなく先頭に付く）。`Cmd::LoadLog` に `restart` を足し、
リーダースレッドの `LogSession` を捨てて先頭から読み直す。

```rust
Cmd::LoadLog { count: usize, restart: bool }
Msg::LogLoaded { restart: bool, result: Result<LogPage, GitReadError> }
```

**出すかどうかは `update::Reload` が決める。** `app` に置く。

| | 出す `Cmd` | 例 |
|---|---|---|
| `Nothing` | `LoadStatus` | stage / unstage / discard、stash |
| `Refs` | `+ LoadRepoInfo` `+ LoadBranches` | checkout、branch 作成 / リネーム、非 force push |
| `RefsAndLog` | `+ LoadLog { restart }` | commit、fetch / pull、force push、branch 削除、rebase / cherry-pick / revert |

- **ブランチ一覧も `Refs` に入れる。** ラベルと HEAD（`LoadRepoInfo`）と同じく ref から
  導かれる値で、同じ書き込みで古くなる。commit は `refs/heads/<現在>` を動かすので、
  外すと Branches ペインが commit 前の ahead を出したまま残る
- **`Refs` と `RefsAndLog` を分ける。** グラフのラベルと HEAD は行に焼き込まず
  `GraphState.refs` から引く（07）ので、**既にあるコミットを指し直すだけの操作では
  行は 1 つも動かない**。ログの読み直しは `LogSession` の作り直しで履歴全体のコストが
  掛かる（02 の「セッションは作り直さず使い回す」）ので、要らないところでは払わない
- **`git_exec` に `GitOp::moves_refs()` として置かない。** stash が `Nothing` なのは
  「`refs/stash` をグラフの walk に押していない」からで、git は ref を動かしている。
  git の事実を約束する名前で UI の方針を返すと、Stash ペインが入ったときに黙って
  嘘になる。**どの ref を辿るかを知っているのは `app` 側**
- **失敗した書き込みで読み直すのは、途中まで ref が動きうる操作だけ。** fetch / pull は
  一部の ref だけ更新して落ちうるし、rebase / cherry-pick / revert は途中で止まる。
  commit / branch 削除 / force push は 1 回で決まるので、失敗したなら ref は元のまま。
  **読み直しは行を捨てるだけでなく `GraphState::restart` がカーソルとスクロールを
  先頭に戻す**ので、pre-commit hook に弾かれただけでグラフの位置を失う
- **新しい `Cmd` の変種にせず `LoadLog` のフィールドにする。** `job_kind` が
  `JobKind::LoadLog` と必ず同じになり、続きの読み込みと読み直しが同時に走らない。
  別変種にすると `job_kind` を揃えるのは手書きの約束になる
- **`Msg::LogLoaded` は `restart` を運ぶ。** 届いたページが「続き」か「先頭から」かは
  中身からは判らない（`LogPage` に位置が無い）。`CommitDetailLoaded` の `oid` と
  `DiffLoaded` の `target` と同じで、**要求した内容を返信に載せる**
- **読み直すのは先頭 1 ページ（`graph.page_size`）だけ。** 積んでいたぶんを全部
  読み直すと、履歴を深く辿ったあとの commit が高くつく。commit の直後に見たいのは
  新しい先頭なので、カーソルとスクロールは 0 に戻す
- 行を捨てて積み直す処理は**ログ読み込みの失敗と同じ**（02 の契約により、失敗した
  セッションも先頭から読み直しになる）。`GraphState::restart()` の 1 本にまとめ、
  `lines` / `lanes` / `detail` / `cursor` / `scroll` / `loaded_all` を揃って戻す

**読み直しは `issue` に落とされうる。** 続きの読み込みが走っているあいだに commit すると
同種なので落ちる。落ちたぶんは `GraphState.reload_pending` に印を付け、走っていた
ページが届いた時点で頼み直す。**立てる条件まで `WorktreeState.stale` と同じ**にする
（`jobs.is_running(LoadLog)`）。「常に立てて届いたら落とす」形にすると、同じ形だと
書いてある 2 つが別のことを指す。

- **印が立っているあいだに届いたページは、続きでも読み直しでも捨てる。** 印が立つのは
  「読み直しを頼めなかった」ときで、そのとき飛んでいたページは必ず印を立てた書き込みより
  前に頼まれたもの。積んでも直後の読み直しで捨てるだけで、そのあいだ古い行に新しい行が
  継ぎ足された状態が画面に出る。**`restart` かどうかで分けてはいけない** — 1 つ前の
  書き込みの読み直しが届いたときに印だけ落ちて、新しい書き込みが二度とグラフに出なく
  なる（続けて 2 回コミットすると踏む）
- **印が立っているあいだは必ず `JobKind::LoadLog` が走っている**（落とした相手が
  それ）。だから `load_more` に印を見る分岐は要らない — 続きを出しても `issue` が
  落とす。**見る形にすると、テストで踏めない分岐が残る**
- **読み直しを組む関数にも「出すか」の判断を置かない。** 呼び出し元は 2 つあり、
  どちらも出すと決めたあとで呼ぶ。置くと、やはり踏めない偽の腕が残る
- 印を落とすのは `restart: true` のページが届いたときと、ログ読み込みが失敗したとき。
  **3 状態は要らない。** `DiffState.freshness` が 3 つ要ったのは「短絡してよいか」と
  「パッチを組んでよいか」で見る時点が違ったため。ここは読む 2 箇所（読み直しを
  出すか・届いたページを採るか）がどちらも「読み直しが欠けている」という同じ時点を
  見ている
- **`Cmd::LoadRepoInfo` も一緒に出す。** ref のラベル（`main` が新しいコミットへ動く）と
  `RepoInfo.head` が古くなるため。こちらが `issue` に落ちたときの追いつきは持たない。
  発行元が起動時とここの 2 つしかなく、重なる窓が実質無い。M3 の世代で畳む

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

コミットを選んでいるあいだ、詳細は常に右ペインに出ている（07 の「CommitDetail ペイン」）。
カーソルがコミットの行に乗るたびに `Cmd::LoadCommitDetail(oid)` を出し、取れた値は
`GraphState.detail` に入れる。WIP 行に乗っているあいだは出さない（読むものが無い）。

**「開いているか」の状態を持たない。** M1 では `Enter` で `expanded` を切り替え、真の
あいだだけ読んでいた。右ペインへ移すと出す・出さないはグラフの選択だけで決まるので、
その旗は消える。`Enter` はコミットの行で `focus` を `CommitDetail` に移すキーになった。

- **`detail` は `CommitDetail` をそのまま持ち、どのコミットのものかは `detail.oid` で判る。**
  カーソルの指すコミットと食い違っていれば、まだ届いていない（あるいは失敗した）という
  ことなので、`ui` はそこで読み込み中かエラーかを出し分ける（07）。「読み込み中」の
  フラグを別に持たない
- 既に `detail.oid` がカーソルのコミットと一致していれば出さない。WIP 行を通り過ぎる
  たびに読み直さないよう、選択が WIP に移っても `detail` は捨てない
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
- **`Err` は失敗した相手を `GraphState.detail_failed` に覚えて、同じコミットには
  再発行しない。** 出すと、リポジトリが壊れているあいだ「失敗 → 再読込 → 失敗」を
  回し続ける（`LoadLog` と同じ）。逆に、失敗したのが別のコミットなら、いま見ている
  ものはまだ一度も試していないので頼む。一律に止めると「一度も試していないコミット」が
  「詳細を読み込めませんでした」のまま残る。**印は `Option<Oid>`**（下の
  「失敗の記憶」）で、`bool` にすると別のコミットの失敗が今のコミットまで塞ぐ
- ログの読み込みが失敗して積んだ行を捨てるときは `detail` も捨てる。`lines` に無い
  コミットの詳細を残さない

### コミットのファイル一覧

コミット詳細と同じペインに、そのコミットが変更したファイルを並べる（07 の
「CommitDetail ペイン」）。カーソルがコミットの行に乗るたびに `Cmd::LoadCommitFiles(oid)` を
出し、取れた値は `GraphState.files` に入れる。**詳細と同じ形**（oid で鮮度を見る・
届いたものは今見ているコミットのものだけ採る・`Err` は同じコミットなら再発行しない）
なので、ここには違うところだけを書く。

- **`Cmd` を詳細と分ける。** 一覧はツリー同士の比較で、本文の読み出しより重い（02）。
  1 通にまとめると、行から行へカーソルを送るあいだ、本文しか見えていない時点でも
  一覧のコストを払う。分けておけば、詳細が先に出て一覧が後から埋まる
- **読むのは 1 つの関数から。** 詳細と一覧は同じ「カーソルがコミットに乗った」で
  要るので、`load_detail` / `load_files` を別々に呼び出し側へ配らず、両方を出す
  1 つの関数を `scrolled` / `log_loaded` / `commit_detail_loaded` /
  `commit_files_loaded` / `Msg::Layout` が呼ぶ。片方だけ書き足す事故を無くすため
- **一覧が届いたら `load_diff` も通す。** 左が Diff に差し替わっていても、一覧が
  届くまでは選んでいるファイルが決まらない（`wanted_diff()` が `None`）。`Enter` や
  `Tab` で先に `Pane::Diff` へ移れるので、届いた時点で頼み直す経路が無いと
  **Diff ペインが「読み込めませんでした」のまま残る**。ワーキングツリー側で
  `status_loaded` が `clamp` のあとに `load_diff` を呼ぶのと同じ形
- **一覧のカーソルを動かしたら `DiffState.failed` を落とす。** 対象が変わるので、
  一度失敗したファイルにも戻ってくれば 1 回だけ試し直せる（ワーキングツリーの
  `moved` と同じ形。下の「diff の遅延取得」）
- **空の `entries` と「まだ読めていない」を区別する。** `--allow-empty` のコミットは
  変更を 1 つも持たない。`Option<CommitFiles>` の `None` が「届いていない」、
  `Some` で `entries` が空が「変更なし」。`Vec` 1 本にすると、読み込み中と空の
  コミットが同じ見た目になる（`WorktreeState.loaded` と同じ理由）
- ログを積み直すとき（`GraphState::restart`）は `detail` と一緒に捨てる。`lines` に
  無いコミットの一覧を残さない
- **速くスクロールしているあいだ、一覧は読まれて捨てられる。** カーソル移動が
  「オブジェクト 1 個の読み出し」から「ツリー比較 + rename 検出」を伴うものになった。
  `JobTracker` は同時 1 本に絞るだけなので、捨てるぶんの計算自体は走り、その要求は
  `LoadLog` の次ページと同じ順番待ちに並ぶ。**詳細が届いてから一覧を頼む**形にすれば
  通り過ぎたコミットの一覧は最初から要求されないが、それは「詳細と一覧は 1 つの関数から
  頼む」（片方だけ書き足す事故を防ぐ）を崩す。**いまは素直な側を採っている。**
  実際に待たされるのが見えたら、02 の pathspec による絞り込みと一緒に測り直す

## ワーキングツリー

```rust
pub struct WorktreeState {
    pub snapshot: WorktreeSnapshot,   // staged / unstaged / untracked / conflicted（02）
    pub section: WorktreeSection,     // カーソルのいる区画
    pub cursor: usize,                // 区画の中の添字
    pub stale: bool,                  // 届く予定の status が書き込みより前のもの
}
```

- **カーソルは「区画 + 区画内の添字」で持ち、`j` / `k` は区画をまたいで 1 本の
  リストのように動く。** 平坦な添字 1 つにすると、`snapshot` が入れ替わるたびに
  どの区画のどのファイルを指していたのかを復元できない。逆に区画の移動を別のキーに
  割ると、空の区画に入って何も選べない状態が作れてしまう。**空の区画は飛ばす**
- 表示順（= カーソルの進む順）は Conflicted → Staged → Unstaged → Untracked。
  Conflicted を先頭に置くのは、他の操作より先に解消が要るため
- `StatusLoaded` で `snapshot` を入れ替えたら、カーソルを新しい長さに合わせ直す。
  区画が空になっていたら次の区画へ寄せる。寄せ先も無ければ先頭に戻す。**この
  詰め直しは `WorktreeState` の 1 つの関数（`clamp`）に置く。** `j` / `k` の移動と
  同じ「次に選べる行」の定義を 2 度書かない
- **`(操作, 区画)` から `GitOp` を決める表は 1 つ**（`update::plan`）。ファイル単位も
  ハンク単位もここを通る。**できない組み合わせは理由を返し、通知に出す。**
  黙って落とすと、キーが効いていないのか対象が悪いのかが判らない。
  「stage されていません」「すでに stage されています」「コンフリクトしたファイルは
  操作できません」はどちらの単位でも同じ文言なので、表を 2 つに割らない
- **単位の違いは `Scope` で運ぶ。** `Scope::File { status, paths }` と
  `Scope::Hunk { path, hunk }` の 2 つ。`plan` は `(操作, 区画)` で可否を決め、
  `GitOp` は各アームが `Scope` を見て組む。discard だけは変種の選び方が単位ごとに
  違うので、そこで枝が分かれる（ファイルは 3 変種、ハンクは Unstaged 限定の
  `DiscardHunk`）
- **`Scope::Hunk` はハンクを借用で持ち、パッチを組むのは `plan` のアームの中。**
  断られる組み合わせ（Unstaged で `u` など）でハンクを丸ごとコピーしない。パッチに
  できない名前（03 の `UnquotablePath`）も「できない理由」の 1 つとして同じ `Err` から
  出るので、断る経路が `act` に 2 本並ばない
- **「ハンクで表せるファイルか」の述語は `app` に置かない。**
  `git_exec::supports_hunk_patch` がパッチを組む関数の隣で持つ。`app` に写すと、
  表せる範囲を広げたときに呼び出し側が取り残され、テストは fallback を期待した
  まま緑で通る
- stage / unstage / discard が渡すパスは `FileEntry` から出す。**rename は旧パスと
  新パスの両方を載せる**（`git add -- <old> <new>` は旧パスの削除も index に入る。
  staged rename の unstage も両方要る。02 の `old_path`）
- discard は区画と変更種別で `GitOp` が変わる（03 の「discard は 3 変種」）。
  Untracked なら `DiscardUntracked`、staged の `Added`（= HEAD に無い）なら
  `DiscardAdded`、それ以外は `Discard`。**判断はここにしか無い。**`to_command` は
  純粋関数でどちらも見られない。**`RepoInfo.head` では判断しない。** あれは起動時に
  1 回読むきりで外部のコミットで古くなり、古い `Unborn` を信じると HEAD にある
  ファイルを `git rm --force` で消してしまう。`FileStatus` なら `LoadStatus` が
  毎回運んでくる
- Conflicted の行では stage / unstage / discard を出さない。解消ビューは MVP 外で、
  中途半端に `git add` だけ通すと「解消した」と誤解させる

### 書き込みのあとの status

`ExecFinished` は成否を問わず `Cmd::LoadStatus` を出すが、**そのとき読み取りが走って
いれば `issue` に落とされる。落とされたぶんは `WorktreeState.stale` に印を付け、
走っていたものが届いた時点で頼み直す。**走っている読み取りは書き込みより前に頼まれた
ものなので、届いた中身は必ず古い。

**`CommitDetailLoaded` の追いつき（上）とは形が違う。** あちらは届いた値の `oid` を
カーソルと比べて鮮度を*導出*できるが、`WorktreeSnapshot` には「いつの状態か」を示す鍵が
無く、到着時には判定できない。そのため書き込みの側で「あとで頼み直す」と印を付ける。
鍵を持てるようになるか、M3 で `Exec` をキューにするときに、この印は消える。

### diff の遅延取得

**`Pane::Diff` を見せているあいだだけ読む。** コミット詳細と同じで、見えていないものを
読んでもリーダーを塞ぐだけ。左の領域を差し替えて出すので、見せているかは
`focus == Pane::Diff` で判る（07）。

**欲しい対象は `AppState::wanted_diff() -> Option<DiffTarget>` の 1 つで導く。**

| 選んでいる行 | DiffTarget |
|---|---|
| WIP 行 › Staged | `HeadVsIndex(Some(path))` |
| WIP 行 › Unstaged / Untracked | `IndexVsWorktree(Some(path))` |
| コミット › ファイル一覧の 1 件 | `Commit { oid, path: Some(path) }` |
| Conflicted / 選択なし / 一覧がまだ届いていない / Diff を見せていない | `None` |

**グラフが WIP 行を選んでいるかコミットを選んでいるかで、見る一覧が変わる**
（`GraphSelection`）。`focus == Pane::Diff` のゲートは両方に効く。コミット側の `path` は
右ペインのファイル一覧のカーソルから取る。**`path: None`（コミット全体）は出さない** —
一覧を出すためだけに全ファイルのハンクを組むことになり、`focus` のゲートも
「見せているあいだだけ読む」も崩れる（一覧はフォーカスが左にあるあいだも要る）。
一覧は `Cmd::LoadCommitFiles` の担当（上の「コミットのファイル一覧」）。

core（次に何を読むか）と `ui`（中身を描くか、読み込み中と出すか）が同じものを使う。

**`ui` は `DiffState.view` を直に見ず、`DiffState::fresh_view(wanted)` を通す。**
`GraphState::fresh_detail` と同じ形で、`view.target` が `wanted_diff()` と一致する
ときだけ中身を返す。読む側で濾さないと、対象が変わってから届くまでのあいだ
**別のファイルの diff をタイトルごと出したまま**になる（そのファイルの読み込みが
失敗すると恒久的に残る）。`view` を読む消費者はこの古さをそのまま引き継ぐので、
ハンク単位の stage を足すと「右で b.txt を選んでいるのに a.txt のハンクが stage
される」になる。

**代わりに `view` を消して回らない。** フォーカスの移動やカーソル移動のたびに `None`
にすると、`Cmd::LoadDiff` の発行で避けた「腕ごとの特殊ケース」を消す側で作ることに
なる。消さないことには利点もあって、同じファイルを開き直したときは読み直しが届くまで
前の中身を出せる（「読み込み中…」が挟まらない）。

「読み込み中」と「読み込めなかった」の出し分けも `DiffState::has_failed(wanted)` で
直接見る。`jobs.is_running(LoadDiff)` の否定から消去法で導くと、`Cmd` の発行経路を
変えたときに片方だけ直してもコンパイルもテストも通る。

**書き込みの鮮度（`Freshness`）はコミットの diff にも同じように効く。** コミットの
diff は書き込みで変わらないので、書き込み直後に 1 通だけ捨てて読み直すのは無駄では
あるが、**単位ごとに例外を作らない**（`Freshness` は対象を持たない。持たせる話は
下の「diff のキャッシュと鮮度」）。捨てるのは 1 通で、読み直しは同じ出口から出る。

**カーソルを動かすのも `fresh_view_mut` を通す。** 濾さずに動かすと、画面に出て
いない `view`（読み込み中・失敗中に残っているもの）のカーソルが黙って動く。

**同じ対象を読み直したときはカーソルを持ち越す。** キャッシュしないので、開き直す
たびに読み直しが届く。既定に戻すと、選んでいたハンクが先頭へ飛んでから操作が効く
ことになる。読み直しでハンクが減っていることがあるので、持ち越したあとに選べる
位置へ寄せ直す（`WorktreeState::clamp` と同じ役目）。**Untracked も `IndexVsWorktree`。** `git_read` が
untracked の中身を出すようにしてあるので、これで新規ファイルの全行が `+` で並ぶ（02）。

**`Cmd::LoadDiff` を出す場所は 3 つ。**

- **フォーカスが変わった直後**（`update_key` の中で 1 回）。`Tab` / `Shift+Tab` の巡回でも、
  Worktree の `Enter` でも、Diff の `Esc` でも、同じ 1 箇所を通る
- `status_loaded` の `clamp` のあと。書き込みで選択が別の区画へ移ると対象が変わる
- `diff_loaded` の追いつき

`update_inner` の doc コメントにある「続きを頼む判断」の一覧に、この 3 つを足す。

**`Tab` の腕や `Enter` の腕に個別に書かない。** フォーカスの巡回はペインより先に見る
共通の経路（01 の「キー入力」）なので、そこに Diff 固有の判断を埋めると、あとから
ペインを足したときに同じ特殊ケースが増える。`Enter` も「`focus` を `Diff` にする」
だけを行い、読むかどうかは共通の出口が決める。`Esc` で出たときは `wanted_diff()` が
`None` になるので、同じ出口を通っても何も出ない。

**カーソル移動（`worktree` の `j` / `k`）では出さない。** Diff を見せているあいだは
ワーキングツリーにフォーカスが無く、カーソルは動かない。

**届いた diff が別の対象のものであることがある。** `DiffLoaded` が運ぶ `target` が
`wanted_diff()` と違えば、そこでもう 1 回頼む。`CommitDetailLoaded` の追いつきと同じで、
`JobTracker` の粒度は種別単位なので、要求が飛んでいるあいだの選択の移動は落とされる。

**`Err` から再発行してよいのは対象が違うときだけ。** `DiffState.failed` に失敗した対象を
入れ、`wanted_diff()` がそれと同じなら出さない。`bool` にすると、失敗のあとに書き込みで
対象だけが変わった（Unstaged → Staged）ときも塞いでしまう。`failed` を落とすのは
読み込みの成功と、ワーキングツリーのカーソル移動（`GraphState.load_failed` と同じ）。
移動して戻ってくれば 1 回だけ試し直せる。

### diff のキャッシュと鮮度

**同じ対象を既に持っているなら読み直さない。** ワーキングツリーの diff は 1 ファイル
ぶんでもワークツリー全体を走査してから絞る（02 で pathspec を捨てたため）ので、
`Esc` / `Enter` のたびに走らせると `LoadLog` を待たせる。リーダースレッドは直列。

**キャッシュには無効化の印が要る。** 対象は変わらないまま中身だけが変わる経路が
あるため（ハンク単位で stage すると `IndexVsWorktree(a.txt)` のまま中身が減る）。

**印は `bool` ではなく 3 状態（`Freshness`）。** 短絡してよいか（= 読み直しを
**頼んだ**か）と、出しているハンクからパッチを組んでよいか（= 読み直しが**届いた**か）
は切り替わる時点が違う。1 つの `bool` はどちらか片方の時点でしか倒れず、もう片方は
「頼んだだけで届いていない diff」を新しいものとして扱う。

| 状態 | 意味 |
|---|---|
| `Fresh` | 書き込みより後の diff を持っている（書き込みがまだ無いときも含む） |
| `Stale` | 書き込みより前の diff しか無く、読み直しをまだ頼めていない |
| `Asked` | 書き込みより前の diff しか無いが、読み直しは頼んである |

- **`ExecFinished` は無条件に `Stale`。** 対象は見ない。書き込みの時点では `clamp` が
  まだで、そのとき選ばれているファイルが読み直しの対象になるとは限らない。どの
  `GitOp` がどの diff を変えるかを表にするより、1 回余分に読むほうが安い
- **`Stale` → `Asked` は `load_diff` が `Cmd::LoadDiff` を実際に出せたときだけ。**
  `LoadDiff` が走っていれば `issue` に落とされ、いま飛んでいる要求は書き込みより前の
  ものなので `Stale` のまま（`WorktreeState.stale` と同じ形）。判定は
  `jobs.is_running(LoadDiff)`
- **`Asked` のあいだ `LoadDiff` はちょうど 1 つ飛んでいる。** だから `DiffLoaded` は
  その 1 通を指していると言える。**届いた時点で必ず `Fresh` か `Stale` に倒す。**
  倒さないと、`Esc` で対象が消えたときや読み取りが失敗したときに「頼んである」まま
  取り残され、次の書き込みまで古い diff を新しいものとして扱う。`Fresh` になるのは
  実際に `view` へ採れたときだけ
- **`Stale` のあいだに届いた返信は採らない。** その返信は書き込みより前に頼まれた
  ものなので、採ると読み直しが届くまでのあいだ書き込み前の中身を出すことになる
- そのため **`diff_loaded` は必ず最後に `load_diff` を通る。** 採ったときも、採らな
  かったとき（対象違い・鮮度不足）も、続きの判断は同じ 1 箇所で行う

`JobTracker` に世代を持たせれば `WorktreeState.stale` ごと畳める。**それは M3 の
fetch で入れる**（下の「実行中ジョブ › 世代」）。`Freshness` はその局所版。

**`Freshness` が `Fresh` でないあいだ、ハンク単位の stage / unstage / discard は
理由を出して断る。** 古い `view` から組んだパッチは、内容の同じハンクが 2 つある
ファイルで `git apply` のオフセット追従に拾われ、**別のハンクに当たりうる**。
#24 で「見えていないハンクに破壊的な操作を押させない」と決めたのと同じ理由。

- **ファイル単位の操作に落とさない**（`s` がファイル全体を stage することになる）
- **断るのはハンク単位のときだけ。** ペイン名（`focus == Pane::Diff`）で書くと、
  古いパッチを一切使わない操作（Diff ペインで untracked に `s`）まで塞ぐうえ、
  ハンクを出す場所が増えたときにガードだけが外れる
- 文言は「待っています」ではなく「最新の差分をまだ読めていません」。`Asked` の
  読み直しが失敗すると `Stale` に戻り、そこからは何も来ない

### 失敗の記憶

読み取りは失敗しうるので、**「一度試して失敗した」を状態に持つ。** 持たないと、
失敗のたびに再発行して「失敗 → 再読込 → 失敗」を回す。5 つあり、上の 4 つは同じ形
（何に対して失敗したかを覚え、対象が変わるか成功したら落とす）。

| 印 | 何の失敗か | 落とすところ |
|---|---|---|
| `GraphState.load_failed` | ログのページ | カーソル移動（`scrolled`）、読み込みの成功 |
| `GraphState.detail_failed` | コミット詳細 | 同上 |
| `GraphState.files_failed` | ファイル一覧 | 同上 |
| `DiffState.failed` | diff | ワーキングツリーとファイル一覧のカーソル移動、成功 |
| `BranchState.load` | ブランチ一覧 | 読み込みの成功（下の「ブランチ一覧」） |

- **対象を持つものは `Option<T>` にする**（`detail_failed` / `files_failed` は `Oid`、
  `DiffState.failed` は `DiffTarget`）。`bool` にすると、別の対象の失敗のあとに来た
  「一度も試していない対象」まで塞ぐ。`load_failed` だけが `bool` なのは、ログの
  ページに対象が無いため（位置は `LogSession` が持つ）
- **落とすのはユーザーの操作から。** 自動で落とすと、失敗が自分で読み直しを引く。
  移動して戻ってくれば 1 回だけ試し直せる
- **`ui` は印を直接読む**（`GraphState::has_detail_failed` / `has_files_failed` /
  `DiffState::has_failed`）。`jobs.is_running` の否定から消去法で導かない。`Cmd` の
  発行経路を変えたときに、片方だけ直してもコンパイルもテストも通ってしまう
- **詳細と一覧は別の印。** 片方だけが失敗することがあり（一覧はツリーの比較で、
  本文の読み出しとは別の失敗をする）、1 本にすると成功したほうまで塞ぐ
- **`BranchState.load` だけは `Option<T>` ではなく 3 状態の enum。** ブランチ一覧は
  対象を持たない（リポジトリ全体で 1 つ）ので覚える対象が無く、代わりに
  「まだ届いていない / 届いた / 失敗した」を出し分ける必要がある。`Vec` が空でも
  それが `git init` 直後の正常な状態なのか、まだ来ていないのかは中身から判らない。
  **`bool` を 2 つ（`loaded` と `failed`）にしない。** 4 通りのうち踏めない組が
  できる（下の「ブランチ一覧」）

**4 つとも冪等になったので、読み込みの発行は `update` の出口で導出できる形になった。**
いまは呼び出し箇所に散っている（下の `update_inner` の doc）。畳むかどうかは M3 で
`JobTracker` に世代を入れるときに決める（そこで `Freshness` と `stale` も一緒に見る）。

## ブランチ一覧

```rust
pub struct BranchState {
    pub branches: Vec<Branch>,   // 名前順。02 の branches() の戻り値そのまま
    pub cursor: usize,
    load: BranchLoad,            // Pending | Loaded | Failed
}
```

- **`load` は 3 状態の enum。** 一覧が空でも、それが `git init` 直後（ブランチが 1 本も
  無い）なのか、まだ届いていないのかは `Vec` からは判らない。`ui` はこの 3 つを
  「ブランチはありません」「読み込み中…」「ブランチを読み込めませんでした」に写す
  （07 の「Branches ペイン」）
- **`bool` を 2 つ（`loaded` / `failed`）にしない。** 4 通りのうち
  「まだ届いていないのに失敗した」は表現できてしまい、どのテストからも踏めない
- **`ui` が `load` を見るのは一覧が空のときだけ。** 中身があるならそれを描く。
  読み直しが失敗しても読めていた一覧は捨てない（`StatusLoaded` と同じ理由。捨てると
  画面から消えるだけになる）ので、「失敗したが一覧はある」を文言に化けさせない
- **`WorktreeState.loaded`（`bool`）と形が違う理由。** ワーキングツリーは書き込みの
  たびに読み直すので「読み込み中…」のまま留まらないが、ブランチ一覧を読むのは
  起動時と ref を動かす書き込みの後だけで、起動時に失敗すると次が無い
- **カーソルは選んでいたブランチの名前で座り直す。** 見つからなければ（消えた /
  リネームされた）HEAD のブランチ、それも無ければ（detached / unborn）先頭。
  **添字では座り直さない。** 一覧は名前順なので、ブランチが 1 本増えるだけで
  同じ添字が別のブランチを指す。**読み直しは commit のたびに来る**ので
  （上の `update::Reload`）、カーソルを HEAD に戻すだけだと、別のブランチを見ながら
  commit した人のカーソルが毎回飛ぶ

## リモート操作（fetch / pull / push）

`f` / `p` / `P` はペインに依らない共通キー（07 のキー表）。`update_key` では
フォーカスの `match` **より先に**見る。`Tab` と同じ位置で、ペインごとの腕に書くと
あとから足したペインで押せなくなる。

| キー | 出す `GitOp` |
|---|---|
| `f` | `Fetch { remote: None, prune: false }` |
| `p` | `Pull { mode: FastForwardOnly }` |
| `P` | `Push { .. }`（下の「push の行き先」） |

- **fetch と pull はリモートを選ばない。** `remote: None` で git に決めさせる。
  リモートが 1 本ならその名前が `origin` でなくてもそれを使い、複数あれば
  `remote.pushDefault` / `branch.<name>.remote` / `origin` の順で選ぶ（git 2.55 で確認）。
  こちらで選び直すと、config を読まないまま git の規則を写した 2 本目の実装になる
- **pull は `--ff-only`。** MVP にコンフリクト解消ビューが無い（requirements）ので、
  merge / rebase が衝突すると mewgit の中では抜けられない。`--ff-only` は早送りできな
  ければ何も変えずに終わる。モードを選ばせるのは continue / abort が揃う M5
- **prune は付けない。** 消えた remote-tracking ref を消す操作で、`f` が黙って ref を
  消すのは「fetch は読むだけ」という期待と食い違う。出すなら M6 の設定から

### push の行き先

`git push`（引数なし）ではなく `<remote> <branch>` を明示する。force push が
`--force-with-lease=<branch>:<expected>` に remote 側の名前を要ることと、`-u` の提案が
行き先を確認モーダルに出すためで、**どちらも「git が選んだもの」では文面を書けない**。

決めるのは `app` の純粋関数。**HEAD のブランチは `BranchState` から取る**
（`RepoInfo.head` は起動時に読んだきりで古くなる。#22 の「キャッシュした状態から
破壊的操作を選ばない」）。

1. HEAD のブランチが無い（detached / unborn）→ 通知して何も出さない
2. upstream があるなら `<remote>/<branch>` に分ける。分ける鍵は `RepoInfo.remotes` の
   名前で、**最長一致**（リモート名にも `/` を入れられる）。どの名前でも始まらなければ
   通知して断る（リモートが消えている）
   - `set_upstream: false`。**確認モーダルは通さない。** 行き先はユーザーが設定した
     ものなので、推測していない
   - **ローカル名と upstream のブランチ名が違うときは `<local>:<remote>` を渡す。**
     `push <remote> <local>` が動かすのは remote 側の**同名**の ref なので、`x` が
     `origin/y` を追跡している状態では `origin/x` を新しく作ってしまう。git の
     `push.default = simple`（既定）がこの形を断るのも同じ理由。ブランチ名に `:` は
     使えないので、この形が普通のブランチ名と読み違えられる余地は無い
3. upstream が無いなら `-u` を提案する。リモートは 1 本ならそれ、複数なら `origin`、
   どちらでもなければ通知して断る。**`Modal::Confirm` に行き先を出してから実行する**
   （不変ルール 5 が求める破壊的操作ではないが、行き先を推測しているのはこの経路だけ）

**これは git の規則そのものではない。** git は `remote.pushDefault` と
`branch.<name>.remote` も見るが、`update` は同期・純粋で config を読めない。
**上流と同じ判断だとは書かない**（CLAUDE.md）。食い違ったときに黙って別のリモートへ
push しないよう、推測した経路にだけ確認モーダルを置いている。

### force push

**専用のキーは作らない**（07 のキー表に force push の行が無いのはそのため）。
通常の `P` が `ExecErrorKind::NonFastForward` で断られたときだけ、次の `P` が確認モーダル
経由の force push になる。`expected` にはそのとき画面に出した upstream の Oid を渡す
（上の「確認モーダルを組む時点の値を `GitOp` に閉じ込める」）。

**この経路は次のタスク**（plan.md の M3 2 本目）。`git_read::Branch` は upstream の
**名前**しか持たず、lease に渡す Oid が無い。読み取りを 1 つ足す変更なので、既にある
`GitOp` を配線するだけの `f` / `p` / `P` とは別の PR にする。

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

### 世代（M3 の fetch で入れる）

**粒度が種別単位であることの影響は 2 つあり、別々の問題として扱う。**

| | 問題 | いまの解 |
|---|---|---|
| (a) | 同種が走っていると**要求が落ちる** | 返信が届いた場所で必ず頼み直す（`diff_loaded` の出口、`status_loaded` の `WorktreeState.stale`） |
| (b) | 書き込みで**持っている値が古くなる** | `WorktreeState.stale`、`DiffState.freshness` |

**世代（epoch）が解くのは (b) だけ。** (a) を解くには `JobKind` が対象
（`DiffTarget` など）まで持つ必要があり、それは別の変更。世代を入れても (a) は残る。

```rust
pub struct JobTracker {
    /// 書き込み（`ExecFinished`）が起きた回数。持っている値の鮮度の基準。
    writes: u64,
    ...
}

impl JobTracker {
    /// 書き込みが 1 つ終わった。以後の `start` は新しい世代を刻む。
    pub fn advance(&mut self);
    /// 走らせたジョブの世代。`finish` が返すので、届いた返信がいつ頼まれたものかが判る。
    pub fn finish(&mut self, kind: JobKind) -> u64;
    /// いまの世代。持っている値がこれと同じなら最新。
    pub fn epoch(&self) -> u64;
}
```

- **刻むのは `AppState::issue`**（`start` の場所）。`dispatch` が `AppState` に書ける
  範囲を広げずに済む
- **リーダースレッドには何も足さない。** 世代は `Msg` ではなく `JobTracker` が持ち、
  `finish` が返す。`Cmd` / `Msg` の形は変わらない
- **失敗した書き込みでも世代を進める。** `git apply` が途中で落ちても index は動いて
  いることがある（いまの `exec_finished` も成否を問わず `Stale` にしている）

**畳めるもの**: `DiffState.freshness`（`Fresh` = `view.epoch == epoch()`、
`Asked` = 走っているジョブの世代が最新、`Stale` = どちらでもない）と
`WorktreeState.stale`（`status_loaded` での比較 1 本になる）。`DiffView` は読めた
世代を持つ。

**畳めないもの**: 「失敗の記憶」の 4 つ（`load_failed` / `detail_failed` /
`files_failed` / `DiffState.failed`。上の節）は鮮度ではない。`Msg::CommitDetailLoaded` が Oid を運ぶ形も残す。**値そのものが鍵を
持っているなら、そちらのほうが強い**（世代は「リポジトリが変わった」しか言えず、
「選択が動いた」を言えない）。

**3 人目は M2 で出た。** 元はここに「M3 の fetch で 3 人目（ref が変わったあとの
ログの読み直し）が出る」と書いていたが、commit のタスクで先に出た
（`GraphState.reload_pending`）。同じタスクで 4 人目の穴も見えている — 書き込みの
あとに出す `Cmd::LoadRepoInfo` は `issue` に落とされうるのに追いつきを持たない。
発行元が起動時とここの 2 つしかなく重なる窓が実質無いので、いまは塞いでいない。

**それでも M2 では入れない。** ここで待っていた材料のうち、片方は揃った —
上の `update::Reload` が「どの書き込みがどの領域を無効化するか」の判別子で、
**カウンタを 1 本にするとログまで毎回無効化して高い**という懸念には
「領域ごとに分ける」という答えが出せる。揃っていないのはもう片方で、fetch は

- 落ちた要求の追いつき（(a)）を**世代では解けない**まま残す
- ref が変わったあとのログの読み直しを、この `restart` に乗せる（plan.md の M3）

の 2 つを同時に持ち込む。**3 人目を数え終わってから畳むほうが、畳む形を 2 回
変えずに済む。** M3 の世代のタスクで、`DiffState.freshness` / `WorktreeState.stale` /
`GraphState.reload_pending` と `LoadRepoInfo` の穴をまとめて見る。

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
    Paste(String),                     // bracketed paste。編集中のモーダルだけが受ける
    LogLoaded { restart: bool, result: Result<LogPage, GitReadError> },
    CommitDetailLoaded { oid: Oid, result: Box<Result<CommitDetail, GitReadError>> },
    CommitFilesLoaded { oid: Oid, result: Box<Result<Vec<FileEntry>, GitReadError>> },
    HeadMessageLoaded(Result<Option<String>, GitReadError>),  // amend の初期値。None は unborn
    RepoInfoLoaded(Box<Result<RepoSnapshot, GitReadError>>),  // { remotes, head, refs }
    StatusLoaded(Box<Result<WorktreeSnapshot, GitReadError>>),
    BranchesLoaded(Box<Result<Vec<Branch>, GitReadError>>),   // ローカルブランチの一覧
    DiffLoaded { target: DiffTarget, result: Box<Result<Vec<FileDiff>, GitReadError>> },
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

`LogLoaded` は `Result` と、要求した `restart` を運ぶ。`Result` にするのは、失敗の
通り道が無いと `JobKind::LoadLog` が走ったまま残り、以後の `Cmd::LoadLog` が
`JobTracker` に黙って落とされて二度と読めなくなるため。`restart` を運ぶのは、
届いたページが先頭からのものか続きかを `LogPage` から導けないため（上の
「ログの読み直し」）。
受けたら成功・失敗を問わず `jobs.finish(LoadLog)` する。失敗のときは積んだ行も捨てる。
失敗したセッションは捨てられ、次の読み込みは履歴の先頭から来る（02）ので、残すと
同じコミットが二重に並ぶ。

`AccountResolved` は 04 の `resolve` の戻り値をそのまま運ぶ（`AccountState`）。
`Result<Account, AuthError>` にすると `AuthError` と `DegradedReason` が同じ失敗を
二重に表現し、`update` に手書きのマッピングが生える。`AuthError` は `gh` / `ssh` 実行の
内部エラーに留め、`resolve` の中で `DegradedReason` に畳む。

`StatusLoaded` も `Result` を運ぶ。理由は `LogLoaded` と同じで、失敗の通り道が無いと
`JobKind::LoadStatus` が走ったまま残り、以後の `Cmd::LoadStatus` が黙って落ちる。
書き込みのたびに `ExecFinished` が `LoadStatus` を出すので、ここが詰まると
「stage したのに一覧が変わらない」になる。**失敗しても `worktree.snapshot` は捨てない。**
`LogLoaded` が行を捨てるのは、失敗したセッションを作り直すと同じコミットが二重に並ぶため
（02）で、status にはその事情が無い。捨てると読めていた一覧が消えるだけになる。

`CommitDetailLoaded` も `Result` を運ぶ。`LogLoaded` と同じで、失敗の通り道が無いと
`JobKind::LoadCommitDetail` が走ったまま残り、以後の要求が黙って落ちる。**要求した Oid を
一緒に運ぶ。** `Err` のときにどのコミットが失敗したのかを知る手掛かりがこれしかなく、
無いと「別のコミットの失敗」と「いま見ているコミットの失敗」を区別できない。

`DiffLoaded` も `Result` を運び、**要求した `target` を一緒に運ぶ**。`Result` にする
理由は `LogLoaded` / `StatusLoaded` と同じで、失敗の通り道が無いと
`JobKind::LoadDiff` が走ったまま残り、以後の `Cmd::LoadDiff` が黙って落ちる。
`target` を運ぶ理由は `CommitDetailLoaded` の `oid` と同じで、`Err` のときに
「どの対象が失敗したか」を知る手掛かりがこれしかない。加えて `Ok` でも、届いた
diff がいま選んでいるファイルのものかを判定するのに要る。

`BranchesLoaded` も `Result` を運ぶ。理由は `StatusLoaded` と同じで、失敗の通り道が
無いと `JobKind::LoadBranches` が走ったまま残り、以後の `Cmd::LoadBranches` が黙って
落ちる。**要求した内容は運ばない。** 一覧は対象を持たない（リポジトリ全体で 1 つ）ので、
`CommitDetailLoaded` の `oid` に当たるものが無い。`LogLoaded` の `restart` のような
「同じ `Cmd` の 2 つの意味」も無い。

`CommitFilesLoaded` は `CommitDetailLoaded` と同じ形（`Result` + 要求した Oid）。
理由も同じなので繰り返さない。**別の `Msg` にするのは `Cmd` を分けているから**で、
1 通にまとめると本文だけ見えていれば足りる時点でも一覧を待つことになる
（上の「コミットのファイル一覧」）。

`ExecFinished` と `PrLoaded` と `CommitDetailLoaded` と `CommitFilesLoaded` と `StatusLoaded` と `DiffLoaded` は payload を `Box` に入れる。入れないと `Msg` が
数百バイトになり、1 打鍵ごとに作られる `Msg::Key`（実データ 12 バイト）まで同じ大きさで動く。
これらを Box すると 96 バイトに収まる。上限は `msg.rs` のテスト（128 バイト）で見張る。

## Cmd

```rust
pub enum Cmd {
    None, Batch(Vec<Cmd>),
    LoadLog { count: usize, restart: bool },  // 位置は LogSession が持つ。restart で捨てる（02）
    LoadCommitDetail(Oid),             // 選択コミットのメッセージ全文など（02）
    LoadCommitFiles(Oid),              // 同じコミットのファイル一覧（02）。詳細より重いので別
    LoadHeadMessage,                   // amend の初期値。HEAD の解決ごとリーダーで行う
    LoadStatus, LoadDiff(DiffTarget),
    LoadRepoInfo,                      // remotes / head / refs。起動時と ref 変更後（M3）
    LoadBranches,                      // ローカルブランチ一覧。RepoInfo とは別（02）
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
`LoadLog { count: config.graph.page_size, restart: false }` と `LoadStatus` と
`LoadBranches` の 4 つで、`ResolveAccount` はそのタスクでここに足す。起動時はまだ何も読んでいないので
`restart` は偽でよい（`LogSession` が無ければ `restart` の有無に関わらず先頭から開く）。

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
    // レイアウトの変化（いまは端末のリサイズだけ）を core に伝える唯一の経路。
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
- **描画の直前に毎回 `ui::graph_rows(state.term)` と `state.graph.viewport` を比べ、違えば
  `Msg::Layout` を流す。** これがレイアウトの変化を core に伝える唯一の経路になる。
  いま行数を動かすのは端末のリサイズだけだが（07 の「CommitDetail ペイン」）、比べる
  場所を 1 つにしておくと、狭いレイアウト（M6）でペインを隠すときも同じ経路で通る
- Git CLI 実行中も入力を受け付けるが、書き込み系 `GitOp` は同時に 1 つのみ。
  **いまはキューに積まず、`AppState::issue` が落として通知を出す。** `JobKind::Exec` の
  粒度は種別単位なので 2 本目は必ず落ちる。黙って落とすと「押したのに何も起きない」に
  なるので、落としたことだけは伝える。キューにするのは M3（実行中ジョブのスピナーと
  キャンセル）
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
- 編集中のモーダルを `Ctrl-C` で捨てる際も確認は挟まない。`modal` は 1 つしか持てず、
  確認を重ねるにはモーダルのスタックが要る（上の「コミットメッセージの編集」）
