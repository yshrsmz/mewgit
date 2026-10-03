# 01. app: 状態管理とイベントループ

`app` は状態と遷移だけを持つ。ここで定義する型はすべて `mewgit-core` 側にあり、
ratatui / crossterm には依存しない（不変ルール 6）。`ui` は下位の型をここ経由で見る。

## この文書とコードの対応

`update` は 1 ファイルではない（`app/update/` に割れている。00 の「ディレクトリ構成」）。
**この表がその対応**で、節を直したときにどのファイルを見るかはここで引く。

| 節 | モジュール |
|---|---|
| 「グラフの行」とその `###`（追加ページ / ログの読み直しの発行 / コミット詳細 / ファイル一覧 / cherry-pick の開始 / revert の開始 / 複数（印）） | `update/graph.rs` |
| 「interactive rebase の開始（`i`）」 | `update/rebase.rs`（`i` の腕は `update/graph.rs`） |
| 「ワーキングツリー」「書き込みのあとの status」「失敗の記憶」 | `update/worktree.rs` |
| 「diff の遅延取得」「diff のキャッシュと鮮度」 | `update/diff.rs` |
| 「ブランチ一覧」 | `update/branches.rs` |
| 「Stash ペイン」 | `update/stash.rs`（Diff に出す側は `update/diff.rs`、`s` / `u` / `x` の断りは `update/worktree.rs`） |
| 「リモート操作」「既定リモート」「push の行き先」「force push」 | `update/remote.rs` |
| 「`RepoInfo.target` を埋める」「アカウントの選び直し」（+ 04） | `update/account.rs` |
| 「Pane と Modal」とその `###`（コミットメッセージの編集 / 1 行入力） | `update/modal.rs`（骨格）と、確定して何をするかは各モジュール |
| 「ログの読み直し」の**適用側**、「失敗の案内文」 | `update/exec.rs` |
| 「進行中操作があるときの書き込み」とその `###`（メニュー） | `update/progress.rs`（落とす場所は `state.rs` の `AppState::issue`） |
| 「外部の変更の検知」 | `update/watch.rs`（指紋の差から何を読むかは `Changed::between`、読み直しの適用は `update/exec.rs` の `reload`、diff の stamp の比較は `state.rs` の `diff_is_current`。「古いハンクの守り」の照合は `git_exec` の `Runner`、指紋を `GitOp` に写すのは `update/worktree.rs`） |
| 「キー入力」「起動時の読み込み」「update の原則」 | `update/mod.rs` |

**「ログの読み直し」だけ 2 つに割れている。** 頼む側（どこまで積んだか、追いつき）は
`graph.rs`、書き込みのあとに何を読み直すか（`Reload`）は `exec.rs`。**`exec.rs` に
対応する `##` は無く**、上の 3 節を横断する。

## AppState

```rust
pub struct AppState {
    pub repo: RepoInfo,               // パス、リモート一覧、正規化済みホスト
    pub account: AccountState,        // Resolved(login, host) | Degraded(reason) | Resolving | Disabled
    pub graph: GraphState,            // 読込済みコミット、レーン、選択位置、スクロール
    pub worktree: WorktreeState,      // staged / unstaged / untracked と選択
    pub diff: DiffState,              // 表示中の diff と、その鮮度
    pub branches: BranchState,     // ローカルブランチの一覧と選択（下の「ブランチ一覧」）
    pub stashes: StashState,          // stash の一覧と選択（下の「Stash ペイン」）
    pub prs: PrState,                 // 開いている PR の一覧・カーソル・API の残量（下の「PR ペイン」）
    pub focus: Pane,
    pub right_tab: RightTab,          // 右列の上に出すタブ（下の「右上のタブ」）
    pub modal: Option<Modal>,
    pub jobs: JobTracker,             // 実行中の非同期タスク（表示用）
    pub watch: WatchState,            // 外の変更の見張り。基準の指紋と期限（下の「外部の変更の検知」）
    pub notices: VecDeque<Notice>,    // エラー・情報のトースト
    pub term: TermSize,               // 端末サイズ。レイアウト分岐に使う
    pub tick: u64,                    // Msg::Tick が来た回数。スピナーの駒を選ぶ
    pub now: Timestamp,               // いまの時刻（起動時に受け取り、Tick で更新）。通知に刻む
    pub config: Config,               // 起動時に確定。実行中は再読込しない（08）
}
```

`RepoInfo` は `app` が組み立てる。パスとリモート一覧は `git_read`、ホスト名の正規化は
`auth`（`ssh -G`）から来るため、どちらか一方の層には置けない。正規化済みの
host / port / owner / repo は `RepoInfo.target: TargetState` に入れる（`Option` に
しない理由は下の「`RepoInfo.target` を埋める」）。下位層に
`RepoInfo` を渡さない（`auth::resolve` は `auth::RepoTarget`、`config::account_for` は
`host` / `owner` / `repo` の 3 つを受ける）。

`AppState` に `Default` は付けない。リポジトリの無い `AppState` を表現可能にしないため。
入口は `AppState::new(repo: RepoInfo, config: Config, now: Timestamp)` の 1 つ（`now` を取る理由は下の「通知」）。サブ状態の `Default` は残す。

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
    pub source: DiffSource,         // Selection | Stash(Oid)。Enter で来た側（下の「Stash ペイン」）
}

pub struct DiffView {
    pub target: DiffTarget,
    pub files: Vec<FileDiff>,       // 02 の diff() の戻り値そのまま
    pub cursor: DiffCursor,         // (ファイル, ハンク)
    pub epoch: u64,                 // 読めた時点の Region::Worktree の世代
}
```

**`failed` は `DiffView` の中には置けない。** 失敗したときは `view` が `None` なので、
中に置くと印ごと消える。

`Vec<Hunk>` に平坦化しない。`GitOp::StageHunk { path, patch }` に渡す `path` を
復元できなくなるうえ、`DiffTarget` は複数ファイルにまたがる diff を表せる形になって
いる。**複数ファイルが届くのは stash の diff だけ**（`DiffTarget::Stash(oid)` は `path` を
持たない。下の「Stash ペイン」）。ワーキングツリーと
コミットの diff は一覧で選んだ 1 件に `path` で絞ってある（下の「diff の遅延取得」）。

## 通知

```rust
impl AppState {
    /// 通知を積む。上限を超えた分は古いものから捨てる。
    pub fn push_notice(&mut self, level: NoticeLevel, message: String, detail: Option<String>);
}
```

- 生産者は `update` とランタイムの両方。組み立てを 1 箇所に寄せ、上限もそこで効かせる
- 上限は 100 件。ステータスバーは最新の 1 件しか読まず、残りは履歴のモーダル
  （`Modal::NoticeLog`。下）が読む
- ステータスバーは**最新の通知とキーヒントの両方**を出す（07）。通知で置き換えると、
  起動時の設定警告 1 件でヒントが二度と戻らなくなる
- **`message` は「次に何をすればよいか」、`detail` は証拠**（argv と git の stdout・stderr）。
  ステータスバーが描くのは `message` の 1 行目だけなので、そこを証拠で埋めると
  案内が画面から消える（下の「失敗の案内文」）
- **ステータスバーの通知は次の入力で下ろす**（M6。2026-09-26 に決めた）。下ろすのは
  `Msg::Key` と `Msg::Paste` の入口で、**そのキーが何かする前**（`AppState::dismiss_notice`）
  —— 後にすると、押した結果の警告がその場で消える。`Tick`・`Resize`・`Layout` と
  非同期の完了では下ろさない。**離れているあいだに出た失敗を、読まれないまま消さない
  ため**で、時間で消す形（Tick で期限切れ）を採らなかった理由もこれ。時間に依存しないので、
  遷移のテストに時計も要らない
- **下ろした旗は通知ごとに持つ**（`Notice.dismissed`）。`AppState` に 1 つの旗で持つと、
  「出ている」と「通知が 1 件も無い」が同時に立つ形が組める（`.claude/rules/rust-coding.md` の「旗と値を
  割らない」）。ステータスバーが読むのは `AppState::shown_notice`（最新の 1 件で、
  下ろしていないもの）。**下ろしても履歴からは消えない**
- **通知には積まれた時刻を刻む**（`Notice.at`）。**時計を読むのはランタイムだけ** ——
  `Msg::Tick(Timestamp)` で運び、`update` は `AppState.now` に写して `push_notice` が
  刻む。`update` の中で時計を読むと、同じ入力から違う状態ができて遷移をテストできない。
  そのぶん刻まれる時刻は最大 1 周期（250ms）古い。**起動時の設定の警告は最初の
  `Tick` より前に積まれる**ので、`AppState::new` が時刻を引数に取る（時計の無い状態を
  組めると、その通知だけが時刻を持たない。`Option` にしていた版は、その腕が本番では
  通らないまま描画とテストに残っていた）。
  時刻は見る人のローカルのタイムゾーンで持つ（`Timestamp::now`。オフセットは
  libgit2 の `git_signature_now` に数えさせ、日時のクレートは足さない）
- **全部を見るのは `Modal::NoticeLog`**（`L`。どのペインでも効く）。新しい順の一覧と、
  カーソル行の `message` の全文と `detail` を出す（07 の「NoticeLog モーダル」）。
  **ステータスバーでは読めない証拠（argv と git の出力）が画面に出るのはここだけ**で、
  不変ルール 4 の「UI まで届ける」はこのモーダルで閉じる
- **選んでいる通知は番号で持つ**（`Notice.seq`。`push_notice` が積んだ順に振る）。
  **添字で持たない** —— 開いているあいだに非同期の完了が 1 件積むと、添字の下の通知が
  黙って入れ替わり、読んでいた `detail` が別の失敗のものになる。最初の版は添字を
  持って `push_notice` が補正していたが、共有の入口が 1 つのモーダルを知る形になった
  （`/simplify` の altitude が拾った）。**描く行は `AppState::notice_log_row`** の 1 本で、
  上限で捨てられた通知を選んでいたら最古の行に寄せる。`ui` と `j` / `k`
  （`AppState::move_modal_cursor`。一覧は `AppState.notices` にあるのでモーダル自身は
  中身を知らない）が同じ 1 本を読む
- **通知が 1 件も無くても開く**（「まだありません」と出す）。断る通知を積むと、
  その 1 件が履歴に入って「無い」が嘘になる。そのときは次に積まれる番号（0）を
  選んでおくので、開いたあとの最初の 1 件に座る

## Pane と Modal

`focus` と `modal` は状態なので `app` で定義する。`ui`（07）はこれを描画するだけで、
自分では定義しない。

```rust
pub enum Pane { Graph, Branches, Stash, Prs, Worktree, CommitDetail, Diff }

pub enum Modal {
    Confirm { title: String, body: String, on_yes: Cmd },
    Input { title: String, prompt: String, buf: TextBuffer, intent: InputIntent },
    CommitMessage { buf: TextBuffer, amend: bool },
    RebaseEditor { onto: Oid, head: Oid, plan: RebasePlan, cursor: usize, reword: RewordInput }, // 下の「interactive rebase の開始（`i`）」
    PrCreate { title: TextBuffer, body: TextBuffer, base: TextBuffer, draft: bool, field: PrField },
    AccountPicker { view: AccountView, cursor: usize },
    RemotePicker { reason: String, remotes: Vec<String>, cursor: usize },
    NoticeLog { selected: u64 },   // 通知の履歴。選んでいる Notice.seq（上の「通知」）
    ProgressMenu { menu: ProgressMenu, cursor: usize },
    MainlinePicker { target: Oid, summary: String, parents: Vec<ParentChoice>, cursor: usize },
    Help { pane: Pane, cursor: usize }, // キーの一覧（07 の「Help モーダル」）。開いたときのペインを持つ。行は keys::help
}
```

巡回に入れるのは実装済みで、**かついま画面に出ているペインだけ**（07）。一覧は定数では
なく `AppState::focus_candidates()` が状態から返す。グラフが WIP 行を選んでいれば
`[Graph, Branches, Stash, Prs, Worktree, Diff]`、コミットを選んでいれば
`[Graph, Branches, Stash, Prs, CommitDetail, Diff]`（**WIP 側の `Diff` は下の条件で落ちる**）。
並びは `Pane` の宣言順。**`Branches` と `Stash` と `Prs` はどちらにも入る**（右列の上に
常に出ていて、左の選択で中身が変わらないため）。**`Prs` が落ちるのは
`config.github.enabled = false` のときだけ**で、`AccountState` では変えない
（下の「右上のタブ」）。
**`Diff` はどちらの一覧にも並ぶが、出る diff は違う**（ワーキングツリーのファイルか、
コミットのファイルか）。決めるのは `AppState::wanted_diff()` の 1 つで、`focus` と
選択の両方を見る（下の「diff の遅延取得」）。**「並ぶ」と「候補に残る」は別で、
WIP 側は次の条件で落ちる。**

**WIP 行の側だけ、カーソルが差分で表せる行にあるときに絞る。** 変更の無い
ワーキングツリー、一覧がまだ届いていないあいだ、conflicted の行を指しているとき
（2 者の diff で表せない）は `Diff` を候補から外す。外さないと `Tab` で**グラフが空の
Diff に差し替わり**、「実装済みで、かついま画面に出ているペインだけ」を `Diff` だけが
破ることになる。**同じ判定を `Enter` の入口も見る**（`update::open_diff`）ので、
入り方が 2 つあっても空のペインには入れない。**stash の diff を出しているあいだは
絞らない**（`DiffState.source` が `Stash`。下の「Stash ペイン」）—— 出しているのは
ワーキングツリーの行ではないので、変更の無いワーキングツリーで stash を開いた瞬間に
座り直しが Graph へ戻してしまう。

**見るのは区画ではなく行。** conflicted の行と変更のある行が並んでいると、`j` / `k` で
候補が増減する。**それでよい** —— 出せるかどうかは行ごとに決まり、Worktree ペインが
出している中身と揃う。コミット側で増減を避けたのは、あちらが**同じ行のまま時間で
変わる**（一覧が届く）ためで、こちらはユーザーが動かした結果。

**コミット側は絞らない。** あちらの「対象が無い」はファイル一覧が届くまでの一時的な
もので、届いた時点で頼み直す経路が別にある（下の「コミットのファイル一覧」）。
**ここで外すと、待っているあいだ候補が増減するうえ、その頼み直しへ入れなくなる。**
WIP 行の側が Worktree ペインと**同じスナップショット**を読む（無い状態はペインが
既に出している）のに対し、コミット側は 2 つ目の遅延読み込みを待つ、という違い。
空のコミット（ファイルが 0 件）はいまの挙動のまま。

**選択と右ペインの対応は `GraphSelection::right_pane()` の 1 つ。** `Enter` の行き先
（グラフの `Enter` は右ペインへ移る）と巡回の候補が別々に決めると、画面はコミット詳細
なのにキーは Working tree が受ける状態を作れる。`ui::render` だけは中身（`&GraphLine`）を
取り出す必要があるので変種で分岐するが、**候補が必ず `right_pane()` を含むことは
テストで見張る**。

**候補を状態から出すのは、`focus` と右ペインの中身が食い違う状態を表現できなくする
ため。** コミットを選んだまま `focus` が `Worktree` でいられると、`s` / `u` / `x` が
画面に出ていないファイルを stage する。キーごとにガードを置く形にすると、キーを足す
たびに書き足す必要があり、書き忘れが静かに通る。候補から外れた `focus` は `Graph` に
座り直させる。**診るのは `update` の出口 1 箇所**（`AppState::reseat_focus`）——
候補が変わるのはグラフのカーソルが WIP 行を出入りしたときだけではなく、**ワーキング
ツリーの行を移って `Diff` が出入りするときもある**（上の「見るのは区画ではなく行」）。
経路ごとに書くと足し忘れが静かに通る。

**`Graph` を選ぶのは「どちらの選択でも候補にある」からだけではない。** そちらの条件は
`Branches` / `Stash` / `Prs` も満たす（落ちることがあるのは `Diff` だけ）が、どちらも右列の上に
あり、「グラフの行を選び直したら右の一覧に飛ぶ」という説明の付かない動きになる。
座り直しはユーザーが選んでいない移動なので、いま見ている左の領域に戻す（07）。

**同じ出口で `right_tab` も揃える**（下の「PR ペイン › 右上のタブ」）。`focus` が
右列の上（`Branches` / `Stash` / `Prs`）に居るなら `right_tab` はそれと同じでなければ
ならず、書く場所を 2 つに割ると食い違った状態が残る。**`DiffState.source` を戻すのも
同じ出口**（Diff ペインを離れたら `Selection`。下の「Stash ペイン」）。

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
`GitOp::Push { lease }` がその例で、ユーザーに見せたリモートの位置をそのまま持たせる
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
  `jobs.is_running(JobKind::Exec)`
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
  根拠は `WorktreeState.snapshot_epoch` 1 本で、いまの `Region::Worktree` の世代と
  同じときだけ信じる。**空の `snapshot` には 2 つの意味がある** — 「変更が無い」と
  「まだ読めていない」（`None`）。書き込みより前に読んだ一覧（刻印が古い）も同じく
  根拠にしない。どちらも「変更がありません」が嘘になりうるので、開いて git の側に
  判定を任せる
- **`C`（amend）は staged が空でも通す。** reword だけの amend は正当な操作
- **`C` は書き込みが走っているあいだ断る。** 走っている `git` が HEAD を動かせば、
  読んだ本文は 1 つ前のコミットのものになる。**返信の刻印では捕まえられない** ——
  git2 の読みは commit（pre-commit hook や署名を含む）より先に返るのが普通で、その
  時点の刻印は「最新」。断り方は確定時（`submit_exec`）と同じ `JobKind::Exec` の
  `drop_notice` で、判定は `exec_busy` の 1 箇所
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
| `Nothing` | `LoadStatus` | stage / unstage / discard |
| `Stashes` | `+ LoadStashes`（`Stash` を進める） | stash の push / apply / pop / drop、merge の abort |
| `Refs` | `+ LoadRepoInfo` `+ LoadBranches` | checkout、branch 作成 / リネーム、非 force push |
| `RefsAndLog` | `+ LoadLog { restart }` | commit、fetch / pull、force push、branch 削除、rebase / cherry-pick / revert |

- **ブランチ一覧も `Refs` に入れる。** ラベルと HEAD（`LoadRepoInfo`）と同じく ref から
  導かれる値で、同じ書き込みで古くなる。commit は `refs/heads/<現在>` を動かすので、
  外すと Branches ペインが commit 前の ahead を出したまま残る
- **`Refs` と `RefsAndLog` を分ける。** グラフのラベルと HEAD は行に焼き込まず
  `GraphState.refs` から引く（07）ので、**既にあるコミットを指し直すだけの操作では
  行は 1 つも動かない**。ログの読み直しは `LogSession` の作り直しで履歴全体のコストが
  掛かる（02 の「セッションは作り直さず使い回す」）ので、要らないところでは払わない
- **`git_exec` に `GitOp::moves_refs()` として置かない。** stash がラベルもログも読み
  直さないのは「`refs/stash` をグラフの walk に押していない」からで、git は ref を
  動かしている。git の事実を約束する名前で UI の方針を返すと、Stash ペインが入った
  ときに黙って嘘になる（**入る前は stash を `Nothing` にしていて、実際に嘘になった**
  —— `Refs` を進めず、stash の一覧を読み直さなかった）。**どの ref を辿るかを知って
  いるのは `app` 側**
- **失敗した書き込みで読み直すのは、途中まで ref が動きうる操作だけ。** fetch / pull は
  一部の ref だけ更新して落ちうるし、rebase / cherry-pick / revert は途中で止まる。
  commit / branch 削除 / force push は 1 回で決まるので、失敗したなら ref は元のまま。
  **1 本だけの cherry-pick も 1 回で決まる側**（コンフリクトでも空でも HEAD は動かない。
  03 の「cherry-pick の開始」）—— `y` を押したコミットの位置を失わない。**1 本だけの
  revert も同じ**（コンフリクト・取り消す変更が無い・mainline の誤りでも HEAD は動かない。
  03 の「revert の開始」）。
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
同種なので落ちる。落ちたぶんは**世代**で追いつく（下の「実行中ジョブ › 世代」）。
`Region::Log` の世代は書き込みで進み、飛んでいたページは進む前の刻印を持っているので、
`log_loaded` はそれを捨てて読み直しを出し直す。**印（`GraphState.reload_pending`）は
持たない** —— 印は「落とされた」という発行側の事情を覚えるもので、届いた返信が古いか
どうかとは別の時点を指していた。

- **世代が古いページは、続きでも読み直しでも捨てる。** 積んでも直後の読み直しで捨てる
  だけで、そのあいだ古い行に新しい行が継ぎ足された状態が画面に出る
- **`restart` かどうかでは分けない。** 分けると、1 つ前の書き込みの読み直しが届いた
  ときに借りだけ消えて、新しい書き込みが二度とグラフに出ない（続けて 2 回コミットすると
  踏む）
- **失敗の処理を先に通す。** 古いページが失敗したときは `load_failed` を立てて止まる。
  順序を逆にすると、失敗のたびに読み直しを出して「失敗 → 再読込 → 失敗」を回す
- **読み直しが要る状態では必ず `JobKind::LoadLog` が走っている**（`exec_finished` が
  その場で出し、出せなければ落とした相手が走っている）。だから `load_more` に世代を
  見る分岐は要らない — 続きを出しても `issue` が落とす。**見る形にすると、テストで
  踏めない分岐が残る**
- **読み直しを組む関数にも「出すか」の判断を置かない。** 呼び出し元は 2 つあり、
  どちらも出すと決めたあとで呼ぶ。置くと、やはり踏めない偽の腕が残る
- **`Cmd::LoadRepoInfo` も一緒に出す。** ref のラベル（`main` が新しいコミットへ動く）と
  `RepoInfo.head` が古くなるため。こちらが `issue` に落ちたときの追いつきは持たない。
  発行元が起動時とここの 2 つしかなく重なる窓が実質無いので長らく塞いでいなかったが、
  世代を入れたときに塞いだ（下の「実行中ジョブ › 世代」）

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
  **Diff ペインが「読み込み中…」のまま残る**（対象は決まったのに誰も頼んでいない
  状態。失敗ではないので `DiffState::has_failed` も立たない）。ワーキングツリー側で
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

### cherry-pick の開始（`y`）

**Graph の `y` で、カーソルの指すコミットを HEAD に cherry-pick する**。

- **確認モーダルを挟む**（2026-09-29 にユーザーと決めた）。`CLAUDE.md` の確認対象
  （force push、`branch -D`、discard、abort 系）には入らないが、積んだコミットを戻す手
  （reset）が mewgit に無い。本文は「`abc1234 <summary>` を `<ブランチ>` に積みます。」で、
  **どのコミットをどこへ積むかを押す前に見せる**。積む先の名前は `RepoInfo.head` から
  取る表示だけの値（detached なら `HEAD`）で、git は HEAD に積むので古くても行き先は変わらない
- **WIP 行では断る**（積むコミットが無い）。通知で行を選ぶよう言う
- **merge コミットは確認を開く前に断る。** git は `-m` が無いと exit 128 で断り
  （03 の「cherry-pick の開始」）、`GitOp::CherryPick` は mainline を運ばない
- **進行中操作は手前で見ない。** 確定のあとの `AppState::issue` が断る（下の「進行中操作が
  あるときの書き込み」。ブランチの削除と同じ）。確認は打った文字を持たないので、閉じてから
  断っても失うものが無い
- **HEAD 自身や、もう取り込み済みのコミットも手前で断らない。** git は変更が空で止まり
  （`CHERRY_PICK_HEAD` が残る）、`Empty` の案内が `m` の skip を指す（下の「失敗の案内文」）。
  **後の履歴が同じ箇所を変えていれば、空ではなくコンフリクトで止まる**（03 の表）ので、
  `Conflict` の案内になる。どちらでも HEAD は動かない。
  取り込み済みかどうかは HEAD からの到達だけでは決まらない（merge で取り込んだ変更も空になる）
  ので、判定を mewgit に写さない
- **1 本だけの pick は、失敗したらログを読み直さない**（上の「ログの読み直し」）

**印を付けたコミットがあれば、`y` はカーソルではなく印の全部を積む**（下の「複数のコミットの
cherry-pick / revert（印）」）。

### revert の開始（`v`）

**Graph の `v` で、カーソルの指すコミットを取り消すコミットを HEAD に積む**（印があれば印の全部。下の「複数のコミットの cherry-pick / revert（印）」）。
確認を挟むこと、WIP 行で断ること、進行中操作を手前で見ないこと、失敗でログを読み直さない
ことは `y`（上）と同じ。確認の本文は「`abc1234 <summary>` を取り消すコミットを `<ブランチ>`
に積みます。」。

- **merge コミットは残す側の親を選ばせてから確認へ進む**（`Modal::MainlinePicker`。
  2026-09-29 にユーザーと決めた）。git は `-m` が無いと exit 128 で断る（03 の
  「revert の開始」）。`-m 1` 固定にしなかったのは、**`-m 2`（main 側の変更を取り消す）を
  mewgit から打てなくなる**から。lazygit も親を選ばせる
- **選ぶのは「残す側」だと画面で言う。** `--mainline N` は N 番目の親を本流として残し、
  ほかの親から入った変更を取り消す（03 の実測で `-m 1` は topic 側、`-m 2` は main 側の
  ファイルを消した）。「親を選ぶ」だけだと、取り消す側を選ぶと読める。確認の本文にも
  「残す側は N 番目の親 `<oid>` です。」を足す
- **親の summary は読み込み済みのグラフからだけ引く**（`ParentChoice::summary`）。読み
  込んでいない親は oid だけ出す。**読み足さない** —— oid だけでも選べ、読み足しを待たせる
  ほうが重い。merge の 1 番目の親はふつう直下の行なので、たいてい summary が出る
- **開いた時点のコミットと親を写して持つ**（`RemotePicker` の `remotes` と同じ）。開いて
  いるあいだにログが読み直されても、選んだ番号が別のコミットの親を指さない
- **親が 1 つのコミットには mainline を付けない。** git は付けても通す（03 の表）が、
  選ばせる意味が無い
- **取り消す変更が無かった revert は `Warn` で理由を出す**（下の「失敗の案内文」）。
  git は何も残さず exit 1 で返るので、進行中操作にはならない
- **`m` のメニューの revert に skip を足した**（2026-09-29 にユーザーと決めた）。1 本だけの
  revert でも、**コンフリクトを HEAD 側の内容で解くと continue が空で断られ、`REVERT_HEAD`
  が残る**（03 の「revert の開始」）。continue では抜けられず、git の hint も `--skip` を
  指す。最初は「1 本だけの revert は空で止まらない」を根拠に足さないと決めていたが、
  それは開始だけを測った結論で、`/code-review` が continue の形で反証した
- **空で断られた continue / skip は「変更が空で止まった。m から skip」を `Warn` で出す**
  （下の「失敗の案内文」）。開始の「取り消し済み」とは分ける —— 同じ `NoChanges` でも、
  こちらは進行中操作が残っている

### 複数のコミットの cherry-pick / revert（印）

**Graph の `Space` でカーソルの指すコミットに印を付け外しし、印があるときの `y` / `v` は
印の全部を 1 回の `git cherry-pick` / `git revert` に渡す**（2026-10-01 にユーザーと決めた）。
印は飛び飛びに付けられる（トグル式。GitKraken の Ctrl+クリックに近い）。連続範囲
（起点からカーソルまで）にしなかったのは、Graph の行がレーンをまたいだ topo-order で、
範囲の中身が HEAD の祖先の列とは限らないから —— 範囲は「表示で隣り合う行」しか表せない。

```rust
pub struct GraphState {
    // ...
    pub marks: Vec<Oid>,   // 付けた順。並べ直しは y / v のとき
}
```

- **印が無ければいままでどおりカーソルの 1 本。** 印が 1 つなら、その 1 本をカーソルの
  1 本と同じ経路に通す（merge の cherry-pick は断り、merge の revert は `MainlinePicker`
  を開く）—— 1 つだけの印で挙動が変わると、印の有無が 2 つの意味を持つ
- **並べる向きは op で逆**（03 の「複数のコミットの cherry-pick / revert」の実測）。**git は
  個別に渡した oid を argv の順に積む**ので、順序の責任は全部 mewgit にある。
  **cherry-pick は古い順、revert は新しい順** —— revert を古い順に渡すと、後のコミットが
  同じ箇所を変えていれば 1 本目でコンフリクトする（git 自身も `A^..B` の revert は新しい順に
  積む）。**鍵は日付ではなく読み込み済みのグラフの行の並び**（topo-order、新しい順）。
  日付は rebase / amend で並びが崩れる
- **印のどれかが merge コミットなら、確認を開く前に断る**（cherry-pick でも revert でも）。
  git は `-m` の無い merge に当たるとその手前まで積んでから exit 128 で止まり、
  **`sequencer/` だけが残る半端な形**になる（03 の表）。`--mainline N` は全部の merge に
  同じ N が効くので、merge ごとに `MainlinePicker` を回す形も取れない
- **WIP 行の `Space` は断る**（通知で行を選ぶよう言う。`y` / `v` と同じ）
- **`Esc` で印を全部外す。** 印が無ければ何もしない
- **印はログの読み直し（`GraphState::restart`）で消える。** 複数の pick / revert は成否を
  見ずにログを読み直す（上の「ログの読み直し」）ので、発行したあとの印は自然に消える。
  1 本（印 1 つ）の pick / revert が失敗したときはログを読み直さないので、印は残る ——
  カーソルが残るのと同じで、同じ印のまま直してやり直せる。読み直しをまたいで持たないのは、
  印のコミットが読み込み済みの行に居ないと並べられないから（`restart` はカーソルも先頭に戻す）
- **確認の本文は本数と積む先と、並べたとおりの短縮 oid**（「3 本のコミットを `main` に古い順に
  積みます: `abc1234` `def5678` `0123abc`」。revert は「3 本のコミットを取り消すコミットを
  `main` に新しい順に積みます: …」）。summary は並べない —— 本数に上限が無く、確認の箱に入らない
- **描き方**は 07 の「Graph ペイン」（印の付いた行の短縮 oid を `theme::marked` で描き、
  枠のタイトルに本数を出す）
- **途中で止まった形**は、コンフリクトなら `Conflict`、cherry-pick が空なら `Empty` の案内
  （1 本だけのときと同じ）。**revert が途中で空に当たると `NoChanges` で、進行中のまま止まる**
  ので「m から skip」を出す（下の「失敗の案内文」）

### interactive rebase の開始（`i`）

**Graph の `i` で、カーソルの指すコミットから HEAD までを `Modal::RebaseEditor` に
並べる。** 選択コミットも範囲に入る（07 の「選択コミットまで」）—— `onto` は選択
コミットの第 1 親で、`git rebase -i <選択>^` と同じ範囲になる。

```rust
Modal::RebaseEditor {
    onto: Oid, head: Oid, plan: RebasePlan, cursor: usize,
    reword: RewordInput,
}

pub enum RewordInput {
    Closed,             // 一覧を出している
    Loading(Oid),       // `r` を押し、そのコミットのメッセージ全文を読んでいる
    Open(TextBuffer),   // カーソルの行のメッセージを打っている
}
```

- **plan は読み込み済みのグラフを HEAD から第 1 親で辿って組む。** 表示行の並びでは
  切らない —— 行はレーンをまたいだ topo-order なので、HEAD の行と選択の行のあいだに
  ある行が HEAD の祖先とは限らない。**読み足さない**（`MainlinePicker` と同じ）。
  辿れなかったら断る（下の表）
- **HEAD は `RepoInfo.head`**（`Branch` の `oid` か `Detached`）。mewgit の書き込みの
  あとは `Reload::Refs` で読み直される（上の「ログの読み直し」）。**端末で積んだ
  コミットには probe（下の「外部の変更の検知」）が拾うまで最大 2 秒追いつかない**。**追いつかない HEAD で組んだ
  plan は git に渡った時点で断られる** —— `Modal::RebaseEditor` が plan を組んだ HEAD を
  持ち、`GitOp::RebaseInteractive` で運ぶ。sequence editor が git の `orig-head` と比べる
  （03 の「Interactive rebase の実装」）。比べないと、plan に載らなかったコミットは git が
  黙って落とす（`rebase.missingCommitsCheck` の既定は `ignore`）。断られたときの案内は
  下の「失敗の案内文」の `HeadMoved`
- **断るのは開く前。** エディタで並べ替えた plan は、閉じたあとで断ると失われる
  （`y` / `v` の「確認は打った文字を持たない」の裏返し）。進行中操作と作業ツリーの
  断りは `AppState::refuse_write` の 1 本（作業ツリーは stash と同じ `refuse_by_held_values`
  の網羅 `match`）に置き、**確定（`Enter`）のときの `issue`
  にも同じものが効く**

| 断る形 | 理由 |
|---|---|
| WIP 行 | 並べるコミットが無い（`y` / `v` と同じ） |
| HEAD が無い（`Unborn`）か、まだ読めていない | 辿る起点が無い |
| 選択が root コミット | `onto` が無い。`GitOp::RebaseInteractive` は `--root` を表せない |
| HEAD から第 1 親で辿って選択に届かない | HEAD の祖先でない / まだ読み込んでいないページにいる。どちらでも範囲が決まらない |
| 範囲に merge コミットがある | git は既定の todo で merge を落とし、**topic 側のコミットを混ぜて 1 列に並べる**（下の実測）。第 1 親だけの plan を渡すと topic 側のコミットが消える。線形化を mewgit に写さない |
| 進行中操作がある | `refuse_in_progress` が断る（`RebaseInteractive` は HEAD を動かす） |
| tracked のファイルに変更がある（staged / unstaged / conflicted） | git が `cannot rebase: You have unstaged changes` で断る。untracked だけなら git は通す（下の実測）。鮮度の無い snapshot では status を読み直して「もう一度押して」（stash の push と同じ） |

- **`rebase.autoStash=true` の人にも断る。** git なら通るが、mewgit はユーザーの
  git 設定を読んでいない。断りの文言が「commit か stash して」と出口を言うので、
  失うものは無い
- **`Enter` は `RebasePlan::validate()` を門にする**（07 の「RebaseEditor」）。通らなければ
  閉じず、理由をエディタの中に赤で出す —— 描く側も同じ関数を読み、判定を 2 本書かない。
  **全部 `drop` だけは確認を挟む**（ブランチが `onto` まで戻る。03 が「確認は UI」と
  している）
- **一覧の `Esc` は確認なしで閉じる**（`CommitMessage` と同じ。上の「コミットメッセージの編集」）。
  reword の入力の `Esc` は閉じずに一覧へ戻る（下）
- **`r`（reword）はエディタの中で入力のモードに入る。** `modal` は 1 枠なので、
  メッセージを打つ入力を別のモーダルとして開くと、戻るときに `onto` / `head` / `plan` /
  `cursor` を丸ごと運んで組み直すことになる。**1 つのモーダルの中でモードを切る先例は
  `PrCreate` の `field`** で、逆に「別のモーダルを開いて前のモーダルへ戻る」先例は無い
  （`MainlinePicker` → `Confirm` も前へ進むだけ）。`Open` のあいだ、打っているのは
  カーソルの行のメッセージで、キーは入力が受ける（カーソルは動かない）

| キー（`Open`） | 動作 |
|---|---|
| 文字 / 改行 / 削除 / 移動 | `CommitMessage` と同じ（振り分けは `update::multiline_input_key` の 1 本） |
| `Enter`（修飾なし） | カーソルの行を `RebaseAction::Reword { message }` にして一覧へ戻る |
| `Esc` | 行を変えずに一覧へ戻る（**エディタは閉じない**） |

- **初期値はコミットのメッセージ全文**（件名 + 本文）。git の reword もエディタに全文を
  出す。**件名だけで始めると、件名の typo を直しただけで本文が黙って消える**
  （グラフの行が持つのは `Commit.summary` だけ）。全文は `Cmd::LoadCommitMessage(Oid)` で
  読み、返事（`Msg::CommitMessageLoaded { oid, result }`）で入力を開く。**既に `Reword` の
  行は読まない** —— 打ってあるメッセージをそのまま初期値にする
- **`Cmd::LoadCommitDetail` に相乗りしない。** あちらの返事はグラフの選択と突き合わせて
  「違えば頼み直す」（上の「コミット詳細の遅延取得」）ので、エディタの行の Oid を渡すと
  グラフの詳細の追いつきが回る。`JobKind` も種別単位なので、グラフの詳細が走っている
  あいだは reword の読みが落とされる
- **読んでいるあいだを状態に持つ（`Loading(Oid)`）。** 返事はその Oid と一致したとき
  だけ採る。エディタを閉じれば待ちごと消えるので、閉じたあとの返事は失敗も含めて黙って
  捨てる。**持たずに「エディタが開いているか・plan にその Oid があるか」で推し量ると、
  `r` → 閉じる → 同じ範囲で `i` と打ったときに頼んでいない入力が開く**（`/simplify` が
  拾った）。採るときはカーソルをその行へ移してから開く —— 待っているあいだに `j` / `k` を
  打たれても、`r` を押した行を打つ。**Oid の中身は変わらない**ので世代は見ない
  （`JobKind::region()` は `None`。`LoadCommitDetail` と同じ）
- **待っているあいだの `r` と一覧の `Enter` は何もしない**（1 回目の行が開く。読みは
  一瞬）。`Enter` を通すと、`r` を押した行を reword にしないまま実行する（`/code-review` が
  拾った）。**違う Oid の
  返事が届いたら、待っている Oid を頼み直す** —— 閉じる前の `r` の読みが走っている
  あいだに開き直したエディタで `r` を押すと、そちらの `Cmd` は種別単位で落とされていて、
  頼み直さないと `Loading` が残り続ける
- **空のメッセージは入力のまま断る**（`commit_submit` と同じ。空白だけも同じ）。
  `RebasePlan::validate()` の `EmptyMessage` に任せると、空の `Reword` を一度 plan に
  置くことになる。`validate()` 側は手で組んだ plan への守りとして残す
- **貼り付けは入力が開いているときだけ受ける**（`CommitMessage` と同じ `insert_text`）。
  一覧のあいだは捨てる —— 行き先が無い
- **`p` / `e` / `s` / `f` / `d` は `Reword` の行も上書きする**（打ったメッセージは消える）。
  git の todo で語を書き換えるのと同じで、確認は挟まない
実測（git 2.55.0、2026-09-30）:

| 形 | 結果 |
|---|---|
| tracked のファイルを変えたまま `rebase -i` | exit 1、`error: cannot rebase: You have unstaged changes.` |
| 同じ形で `-c rebase.autoStash=true` | exit 0（`Applied autostash.`） |
| untracked のファイルだけ | exit 0 |
| 範囲に merge コミット（`main` の B, C と topic の T を merge した M、その上の D）で `rebase -i HEAD~4` の既定の todo | `pick B` / `pick C` / `pick T` / `pick D`。**M は落ち、T が 1 列に混ざる** |

## ワーキングツリー

```rust
pub struct WorktreeState {
    pub snapshot: WorktreeSnapshot,   // staged / unstaged / untracked / conflicted（02）
    pub section: WorktreeSection,     // カーソルのいる区画
    pub cursor: usize,                // 区画の中の添字
    pub snapshot_epoch: Option<u64>,  // snapshot を読んだ時点の Region::Worktree の世代
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
  `DiscardAdded`、それ以外は `Discard`。**判断はここにしか無い。**`to_commands` は
  純粋関数でどちらも見られない。**`RepoInfo.head` では判断しない。** あれは起動時に
  1 回読むきりで外部のコミットで古くなり、古い `Unborn` を信じると HEAD にある
  ファイルを `git rm --force` で消してしまう。`FileStatus` なら `LoadStatus` が
  毎回運んでくる
- Conflicted の行では stage / unstage / discard を出さない。解消ビューは MVP 外で、
  中途半端に `git add` だけ通すと「解消した」と誤解させる

### 書き込みのあとの status

`ExecFinished` は成否を問わず `Cmd::LoadStatus` を出すが、**そのとき読み取りが走って
いれば `issue` に落とされる。**走っている読み取りは書き込みより前に頼まれたものなので、
届いた中身は必ず古い。

**`WorktreeSnapshot` には「いつの状態か」を示す鍵が無い**（`CommitDetailLoaded` は
届いた値の `oid` で鮮度を導出できる）。**そこを世代が埋める** —— 頼んだ時点の
`Region::Worktree` の世代をジョブに刻み、`status_loaded` が届いた返信の刻印で判じて、
古ければ `Cmd::LoadStatus` を出し直す（下の「実行中ジョブ › 世代」）。

**持っている一覧の刻印は `WorktreeState.snapshot_epoch` に入れる。** `None` は「一度も
読めていない」。**旗 2 本（`stale` / `loaded`）を 1 本に畳んだもの**で、`loaded` が
表していた「空の一覧が『変更が無い』なのか『まだ読めていない』なのか」は `None` と
`Some` の区別がそのまま担う。

- **頼み直すかは届いた返信の刻印で決める。** `snapshot_epoch` で決めると、読み取りが
  失敗し続けたときに「古い → 頼み直す → 失敗」を回す（失敗は一覧を入れ替えないので
  刻印が古いまま残る）
- **旗のままでは閉じられない窓があった。** `stale` は「読み直しが落とされたとき」しか
  立たないので、書き込みの時点で `LoadStatus` が走っていなければ、届くまでのあいだ
  古い一覧を新しいものとして扱っていた。`s` の直後に `c` を押すと、commit が
  「stage された変更がありません」と誤って断る（下の「commit」）

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

**`None` の理由 4 つのうち、Diff ペインを見せたまま起きるのは「一覧がまだ届いて
いない」だけ。** Conflicted と選択なしは `Diff` が巡回の候補から外れ、`Enter` も
断られる（上の「Pane と Modal」）。**ペインの文言を書くときはこれを数える** ——
残る 1 つはコミット側なので、理由はファイル一覧の側が持っている（07 の「Diff ペイン」）。

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

**書き込みの鮮度はコミットの diff にも同じように効く。** コミットの diff は書き込みで
変わらないので、書き込み直後に 1 通だけ捨てて読み直すのは無駄ではあるが、**単位ごとに
例外を作らない**（世代は「リポジトリが変わった」しか言わず、`DiffTarget` を見ない。
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

この 3 つにも `// 続きを頼む:` の印を置く（`update_inner` の doc コメントに読み方がある）。

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

**無効化の印は世代**（下の「実行中ジョブ › 世代」）。`DiffView` は読めた時点の
`Region::Worktree` の世代を持ち、いまの世代と同じなら「書き込みは挟まっていない」。
判定は `AppState::diff_is_current`（対象が合っていて、かつ刻印が最新）。

- **`ExecFinished` は対象を見ずに世代を進める。** 書き込みの時点では `clamp` がまだで、
  そのとき選ばれているファイルが読み直しの対象になるとは限らない。どの `GitOp` が
  どの diff を変えるかを表にするより、1 回余分に読むほうが安い
- **採るのは「対象が合っている」かつ「刻印が最新」の返信だけ。** 対象を見ないと正しく
  出ている画面が別のファイルの diff に化け、刻印を見ないと書き込み前の中身に戻る
- **外の編集は世代ではなく `DiffView::stamp` で拾う**（下の「外部の変更の検知 › 定期の
  status」）。世代を進めるのは mewgit の書き込みと外の ref の変化だけで、エディタで
  保存しただけでは進まない。判定は同じ `diff_is_current` の 1 本
- **描くときは刻印を見ない**（`fresh_view` は対象だけで濾す）。古い diff を消して回ると
  読み直しが届くまで「読み込み中…」が挟まる。**見るのは、キャッシュを短絡してよいかと
  ハンクからパッチを組んでよいかの 2 つ**で、どちらも同じ「いまの世代か」を問う
- **`load_diff` は `jobs.is_running(LoadDiff)` を見ない。** 走っていれば `issue` が
  落とし、いま飛んでいる要求は書き込みより前のものなので、その返信は捨てられて同じ
  `load_diff` から頼み直される
- そのため **`diff_loaded` は必ず最後に `load_diff` を通る。** 採ったときも、採らな
  かったとき（対象違い・鮮度不足）も、続きの判断は同じ 1 箇所で行う

**3 状態の `Freshness` はこれで消えた。** 「短絡してよいか（頼んだか）」と「パッチを
組んでよいか（届いたか）」で見る時点が違ったのは、印が**発行側の事情**（頼んだ／落と
された）を覚えていたから。**刻印は返信そのものに付いている**ので、どちらも「持っている
`view` の刻印が最新か」の 1 つの問いに落ちる。

**最新の `view` を持っていないあいだ、ハンク単位の stage / unstage / discard は
理由を出して断る。** 古い `view` から組んだパッチは、内容の同じハンクが 2 つある
ファイルで `git apply` のオフセット追従に拾われ、**別のハンクに当たりうる**。
#24 で「見えていないハンクに破壊的な操作を押させない」と決めたのと同じ理由。
**この判定は mewgit の書き込みしか見ない。** 外の編集のあと、読み直すまで（最大 10 秒）は
`view` が最新に見えたまま押せる。そこを塞ぐのは `git apply` の直前の照合（下の
「外部の変更の検知 › 古いハンクの守り」）。

- **ファイル単位の操作に落とさない**（`s` がファイル全体を stage することになる）
- **断るのはハンク単位のときだけ。** ペイン名（`focus == Pane::Diff`）で書くと、
  古いパッチを一切使わない操作（Diff ペインで untracked に `s`）まで塞ぐうえ、
  ハンクを出す場所が増えたときにガードだけが外れる
- 文言は「待っています」ではなく「最新の差分をまだ読めていません」。頼んだ読み直しが
  失敗すればそこからは何も来ない（`DiffState.failed` が同じ対象への再発行を止める）

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

**読み込みの発行は `update` の出口に畳まない**（M3 の世代のタスクで決めた）。4 つの印が
冪等になった時点では「導出できる形になった」と書いていたが、**世代を入れたことで採れ
なくなった** —— 落とされたぶんの追いつきは**届いた返信の刻印**で判じるもので、それが
読めるのは返信を受ける関数の中だけ。持っている値の刻印から導くと、読み取りが失敗し続け
たときに「古い → 頼み直す → 失敗」を回す（失敗は値を入れ替えないので刻印が古いまま
残る）。発行の一覧は `update_inner` の doc コメントに置く。

## ブランチ一覧

```rust
pub struct BranchState {
    pub branches: Vec<Branch>,   // 名前順。02 の branches() の戻り値そのまま
    pub cursor: usize,
    load: ListLoad,            // Pending | Loaded | Failed
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

## Stash ペイン

```rust
pub struct StashState {
    pub entries: Vec<StashEntry>,  // stash@{0} から順。02 の stashes() の戻り値そのまま
    pub cursor: usize,
    load: ListLoad,                // Pending | Loaded | Failed（BranchState と共有）
}

pub enum DiffSource { Selection, Stash(Oid) }  // DiffState.source

pub enum DiffOrigin { Worktree, Commit(Oid), Stash(Oid) }  // AppState::diff_origin() が解く
```

右列の上の 3 枚目のタブ（`Branches | Stash | Prs`。07 の「右上のタブ」）。

**3 本の PR に割った**（2026-09-30 にユーザーと決めた。`docs/plan.md` の M5）。
(1) 一覧と `Enter` で開く diff、(2) push（全体）/ apply / pop / drop と、進行中操作の
断りの数え直し、(3) 部分 stash（Worktree のカーソルの 1 ファイル）。**(1) を先に
出すのは、書き込みが 1 つも無くても端末で作った stash を見られるから**（07 の
「一覧と `Enter` の行き先は同じ PR にする」と同じ数え方で、一覧だけでは選ぶ先が無い
ので diff まで入れる）。(2) と (3) を割るのは、部分 stash だけが「どのファイルを
退避するか」を選ぶ入口（Worktree のキー）と `-u` の要否（下）を持ち込むから。

- **一覧は `ListLoad` の 3 状態**（「ブランチ一覧」と同じ理由）。stash が 1 つも無い
  のは正常で、「読み込み中…」と同じ見た目にしない。**2 つ目の利用者が出たので
  `BranchLoad` を `ListLoad` に改名して共有した**
- **読むのは起動時と、`Reload::RefsAndLog` の書き込みのあと。** **stash の書き込み
  でなくても `refs/stash` は動く** —— `merge.autoStash` / `rebase.autoStash` を設定して
  いると、mewgit の `p`（`pull --ff-only`）や `m` の rebase の continue が **exit 0 のまま
  `stash@{0}: autostash` を積み、コンフリクトを残す**（実測、git 2.55.0。
  `git -c merge.autoStash=true pull --ff-only` で `Created autostash` → exit 0 →
  `stash list` に `autostash`、`status` に `UU f`）。git は「変更は stash にある」と
  案内するので、一覧を読み直さないと退避された変更が再起動まで画面のどこにも出ない
  （`/code-review` が拾った。最初の版は「`refs/stash` を動かすのは stash の書き込み
  だけ」と書いて起動時しか読んでいなかった）。**autostash の起きうる op に絞らない**
  —— 払うのは reflog 1 本の読み取りで、`RefsAndLog` はそれより重いログの読み直しを
  既に払っている。**端末で作った stash は probe が `refs/stash` の差として拾う**（下の
  「外部の変更の検知」。最大 2 秒）。**stash の書き込みのあとは `Reload::Stashes`**（下）
- **`JobKind::LoadStashes` の領域は `Region::Stash`**（下の「実行中ジョブ › 世代」）。
  進めるのは stash の書き込み（`Reload::Stashes`）と `RefsAndLog`（autostash）で、
  読み込みの最中に pull が終われば返りは捨てられて頼み直される。**`Region::Refs` に
  相乗りさせない** —— 最初の版はそうしていて、`/simplify` が 2 つ拾った。stash の
  書き込みで `Refs` を進めると走っている PR 一覧の取得（GitHub の往復）まで捨てられ、
  逆に switch など `Reload::Refs` の書き込み（一覧を読み直さない）のあとは一覧が古いと
  判じられて、**最初の apply / pop / drop が必ず 1 回断られる**（下の「stash の
  書き込み」の鮮度の門）。**stash の書き込みは `Reload::Stashes`** —— `Worktree` と
  `Stash` を進め、`LoadStatus` と `LoadStashes` を出す（上の「ログの読み直し」の表）。**成否を見ない** —— pop はコンフリクトで exit 1 でも作業ツリーを
  書き換え（stash は残す）、apply も同じ。ラベルとログは読み直さない（`refs/stash` は
  グラフの walk に押していない。02 の「ログ列挙」）
- **`Cmd::LoadBranches` に乗せない。** 古くなる書き込みが違う —— `Reload::Refs` の
  書き込み（switch、ブランチの作成 / 改名、非 force の push）は `refs/stash` を動かさない
  （`switch` に autostash は無い）
- **カーソルは選んでいた stash の oid で座り直す。** **index では座り直さない** ——
  push は先頭に 1 つ足し、drop / pop は後ろを詰めるので、同じ index が別の stash を
  指す（02 の「stash 一覧」に実測）。**選んでいたものが消えたら同じ位置に留まる**
  （末尾を越えたら末尾）。消えた 1 つの次がそこへ詰めてくるので、続けて drop できる
- **`Enter` は左に stash 全体の diff を出す**（2026-09-30 にユーザーと決めた）。
  `DiffTarget::Stash(oid)` で、Diff ペインはファイルの見出しを挟んで
  全ファイルを縦に並べる（07 の「Diff ペイン」）。**右下には出さない** —— 右下は
  「グラフで選んでいるものの中身」で、Stash の選択では入れ替わらない（07 の
  「レイアウト」）。Stash ペインの中でファイル一覧へ潜る形も比べたが、ペインに 2 段の
  状態と `Esc` の戻り先がもう 1 つ増えるので採らなかった。**巨大な stash では全ハンクを
  組む**（コミットで `path: None` を出さない理由と同じコストを払う）のは承知のうえ ——
  stash は作業の途中を退避したもので、依存の更新のような巨大なコミットほど大きくならない
- **untracked も出す**（2026-09-30 にユーザーと決めた。02 の「stash の diff」）。
  出さないと、`-u` で退避したファイルが画面から消えて見える
- **どこから来たかは `DiffState.source` が持つ**（`Selection | Stash(Oid)`）。グラフの
  選択は Diff ペインに居るあいだも画面の状態として残るので導けるが、Stash ペインの
  どの行から来たかは残らない。**oid で持つ**（index は指す相手が変わる）
- **読む側は `DiffSource` を直に見ず、`AppState::diff_origin()` を網羅 `match` する。**
  `DiffSource` とグラフの選択を `Worktree | Commit(oid) | Stash(oid)` の 3 値に解いたもの
  で、読むのは出す diff（`wanted_diff`）・巡回の候補・Diff の `Esc` の戻り先・`s` / `u` /
  `x` の可否。**各所で `DiffSource` とグラフの選択を 2 段で見る形にしない** —— 最初の
  版はそうしていて、出どころを足したときにコンパイラが何も言わず、段の順序（stash を
  先に見る）を 4 箇所で守ることになっていた（`/simplify` の altitude が拾った）。
  **巡回の候補も同じ関数から出る** —— `wanted_diff` から `focus` の門を外した
  `diff_candidate` を候補の判定が読むので、stash の diff を出しているあいだ `Diff` は
  候補から外れない
- **Diff ペインを離れたら `Selection` に戻す。** 離れる経路は `Esc` / `Tab` /
  `Shift+Tab` / 座り直しと複数あるので、**`update` の出口の `reseat_focus` 1 箇所**で
  戻す（`right_tab` と同じ出口）。残ると、`Tab` で Diff に入り直したときにグラフの
  選択ではなく前に開いた stash の diff が出る
- **stash の diff を出しているあいだ、WIP 側の `Diff` を候補から外さない**（上の
  「Pane と Modal」。`diff_candidate` が `Some` を返すので外れない）。外すと、変更の無い
  ワーキングツリーで `Enter` した瞬間に座り直しが Graph へ戻す
- **stash の diff では `s` / `u` / `x` を断る**（「stash の差分は変更できません」）。
  グラフの選択だけを見ると、WIP 行を選んだまま Stash から開いた diff では素通りし、
  **stash のハンクを見ながらワーキングツリーの選択行を stage する**。見るのは
  `diff_origin()` で、Diff ペインを離れれば `Worktree` か `Commit` に戻るので、Worktree
  ペインの `s` はここで断られない

### stash の書き込み

**キーは `z`（push。Worktree と Stash）、Stash ペインの `a`（apply）/ `Z`（pop）/
`d`（drop）、Worktree の `Z`（カーソルの 1 ファイルを push）**（2026-09-30 にユーザーと
決めた。07 のキー表）。apply / pop / drop の相手はカーソル行。**Worktree の `Z` は pop
ではない** —— `stash@{0}` の pop を置くと、どれを pop するかがその画面に出ていない。
`z` の対として部分 stash に使い、キーマップがペインごとに割れているので Stash の `Z`
（pop）とは衝突しない。

- **部分 stash はカーソルの 1 ファイル**（2026-09-30 にユーザーと決めた）。複数の選択は
  持たない（Worktree に選択の状態が無い）。**パスは入力を開いた時点のもので持つ**
  （`InputIntent::StashPush { path }`）—— 確定までに一覧が読み直されてカーソルが
  動いても、退避する相手は変わらない。staged と unstaged の両方に出るファイルは、
  どちらの行で押しても両方ぶんが入る（git はパス単位。03 の「stash push のパス指定」）
- **部分 stash にも `--include-untracked` を付ける** —— 無いと untracked のパスは
  `pathspec did not match` で落ち、付けてもパスで絞ればほかの untracked は退避されない
  （03 の「stash push のパス指定」）
- **git が 1 ファイルでは割ってしまう形は断る**（「この変更は 1 ファイルでは stash
  できません。z で全体を」）。rename（新しいパスでも元のパスでも）、staged の削除、`AD`。
  どれも stash を作ってから exit 1 で終わるか、rename が割れて作業ツリーに片側が残る
  （03 の「stash push のパス指定」。判定は `AppState::splits_alone`）
- **鮮度のある snapshot にそのパスが無ければ「退避する変更がありません」で断る**（全体の
  「空なら断る」と同じ門）。開いたあとに端末で commit した形
- **unmerged のファイルがあれば、全体・部分とも断る**（「コンフリクトを解消してから stash
  して」。2026-09-30 にユーザーと決めた）。git はどちらも `could not write index` で断り、
  理由（`needs merge`）は stdout にしか出ない。**進行中操作の門は拾わない** ——
  `stash pop` のコンフリクトは進行中操作にならない
- **部分 stash は index 全体を記録する**（git の仕様。03 の「stash push のパス指定」）。
  ほかに staged のファイルがあると stash の中身にも入るが、断らない —— Stash ペインの
  diff に出るので見える

- **push は常に `--include-untracked`**（2026-09-30 にユーザーと決めた）。「全体」は
  Worktree ペインに出ているもの全部で、untracked もそこに出る。付けないと、untracked
  だけの作業ツリーで `git stash push` は **exit 0 のまま `No local changes to save` を
  stdout に出して何もしない**（実測、git 2.55.0）—— 成功の通知は無いので、押しても
  何も起きないように見える
- **push は `Modal::Input` でメッセージを聞く**（2026-09-30 にユーザーと決めた）。
  **空の確定を通す** —— 空なら `--message` を付けず、git の既定（`WIP on <branch>:
  <sha> <件名>`）になる。ブランチ名の入力は空を断るので、空を断るかは `InputIntent`
  が答える（`requires_text`）
- **push は鮮度のある snapshot でしか通さない**（2026-09-30 にユーザーと決めた）。
  古ければ「状態を読み直しています。もう一度」で断って `LoadStatus` を出す。
  **ここが「進行中操作があるときの書き込み」の門の例外** —— あちらは古い snapshot では
  git に任せるが、**解消済みの cherry-pick / merge の途中の `stash push` は git が
  受けて `CHERRY_PICK_HEAD` / `MERGE_HEAD` を消す**（03 の表）。書き込みの直後の
  status 待ちの窓でそれを踏ませない。**外で始めた操作を mewgit が見ていない窓は
  狭まるが塞がらない**（probe が gitdir の差として拾うまで最大 2 秒。下の「外部の変更の検知」）
- **鮮度のある snapshot が空なら「退避する変更がありません」で断る**（`c` の
  「stage された変更がありません」と同じ門）。git に渡すと上の `No local changes to
  save` で、何も起きないように見える。`-u` を付けるので、untracked だけでも空ではない
- **apply / pop / drop は、鮮度のある stash 一覧でしか通さない。** 渡すのは
  `stash@{index}` で、**index は読んだ時点のものでしかない**（02 の「stash 一覧」）。
  push の直後は一覧が届くまで全部が 1 つずれていて、そこで `d` を押すと**確認に出した
  のとは別の stash を drop する**。古ければ「stash の一覧を読み直しています。もう一度」で
  断って `LoadStashes` を出す。**oid では渡せない** —— `git stash drop <oid>` / `pop
  <oid>` は `is not a stash reference` で断られる（実測、git 2.55.0。apply だけは通る）
- **判定は `AppState::refusal` に置く**（`AppState::refuse_by_held_values`。順は「古い →
  unmerged がある → 退避する行が無い → 1 ファイルでは割れる」。進行中操作の門と
  並べて `AppState::refuse_write` が束ねる）。キー・確認の `y`・入力の確定のどれから
  来ても同じ門を通る。**入力を開く前も `refuse_write` を問う**（`update::input`）——
  打ち終えてから断られない。開く前の判定を別に並べると、片方にだけ足した門で本文が消える
- **drop は `Modal::Confirm`**（不変ルール 5）。本文に `stash@{n}` とメッセージを出す。
  **開く前に `refusal` 全体を問う** —— git が走っているあいだに確認を開けると、
  その書き込み（autostash の `pull`）が終わって一覧がずれたあとに `y` が通る。
  走っていなければ、確認を開いているあいだに始まる書き込みは無い（キーはモーダルが受ける）
- **pop / apply は確認を挟まない。** 作業ツリーに足すだけで、pop がコンフリクトしても
  stash は残る（git が `The stash entry is kept` と言う。実測）
- **pop / apply のコンフリクトは `Warn` で案内する**（「失敗の案内文」）。git は
  `CONFLICT` を stdout に出して exit 1 で、stderr は空（実測、git 2.55.0）。
  **進行中操作にはならない**（continue が無い）ので `m` は案内しない
- **stash の書き込みでなくても `refs/stash` は動く**（上の「Stash ペイン」の autostash）。
  **`merge --abort` も積む** —— `merge.autoStash` で始めた merge（`pull --no-rebase`）が
  コンフリクトし、作業ツリーを触ってから abort すると、autostash を戻すところで
  コンフリクトして **exit 0 のまま `stash@{0}: autostash` を積む**（実測、git 2.55.0。
  既存の stash は `stash@{1}` にずれる）。HEAD は動かないので `RefsAndLog` ではなく
  `Reload::Stashes`（`/code-review` が拾った。最初の版は `Nothing` で、鮮度の門が
  古い一覧を通し、`d` が autostash —— ユーザーの未コミットの変更 —— を消していた）
- **進行中操作の途中の apply / pop は断ったまま**（`progress::refusal`）。未解消なら
  git も `could not write index` で断る。解消済みなら git は受けて `*_HEAD` も
  消さないが、**解消の途中に無関係な変更を混ぜる**（03 の表）

## PR ペイン

右列の上を `Branches | PRs` のタブにする（07 の「右上のタブ」）。`PrState` が持つのは
**このリポジトリで開いている PR の一覧**で、対象（`RepoInfo.target`）とアカウントが
決まってから引く。

```rust
pub struct PrState {
    pub list: Vec<PullRequest>,
    pub cursor: usize,
    load: PrLoad,
    details: HashMap<u64, PrDetailEntry>,   // 下の「集計」
}

// 集計は private な表に持ち、`PrState::detail(number) -> Option<&Result<PrDetail, GithubError>>` だけを貸す

pub enum PrLoad { Pending, Loaded, Failed(GithubError) }
```

- **`BranchState` と同じ形（値 + 段階）にする。** 引けた値そのものを 3 状態の enum に
  入れると、**読み直しが 1 回転けただけで読めていた一覧が画面から消える** ——
  一覧を引き直すのは ref を動かす書き込みのたびなので、この差は毎コミット効く
  （`BranchState::failed` の「捨てると画面から消えるだけになる」）
- **旗を `bool` 2 つにしない。** 「まだ届いていないのに失敗した」が表現できてしまう
  （`ListLoad` と同じ理由）
- **`Failed` だけ理由を持つ。** `ListLoad` は値を持たないが、GitHub の失敗は
  401 / 403 / レート制限で**次に何をするかが違う**ので `GithubError` を運んで `ui` に
  出させる（07）
- **鍵を持たない。** 一覧は「いまのリポジトリの open な PR 全部」で、**ブランチにも
  コミットにも紐づかない**。HEAD のブランチの PR 1 件を持っていたころの鍵は、
  一覧には対応する問いが無いので消えた。追いつきは世代
  （`JobKind::FetchPrList` の `region()` は `Region::Refs`）だけで足りる
- **一覧はアカウントに紐づく。** `*` / `R` の印は login で決まるので、`A` で選び直す
  あいだと degraded に落ちたときは一覧ごと落とす（`PrState::forget`）。残すと
  **印だけが黙って消えた前のアカウントの一覧**が出たままになる。**`Degraded` では
  `reload` が何も出さない**ので、落とす経路は `account_resolved` の `Err` と
  `resolve_again` の 2 つしかない
- **カーソルは PR の番号で座り直す。** 一覧は `sort=updated` で引くので、**ref を
  動かす書き込みのたびに並びが変わる** —— 添字で座り直すと、自分が commit した
  瞬間にカーソルが別の PR を指す。見つからなければ（マージされて open から消えた）
  先頭。Branches ペインが名前で座り直すのと同じ理由

### 何を引くか

**材料は 2 つ。** `TargetState::Resolved` と `AccountState::Resolved` の両方が揃って
いれば引く。**HEAD は材料ではない** —— 一覧はブランチに紐づかないので、detached でも
unborn でも同じものが出る。

**頼む場所は、ref（`Msg::RepoInfoLoaded`）と、どのアカウントで引いたか
（`Msg::AccountResolved`）と、`r`（下の「引き直す（`r`）」）と、PR を作ったとき
（下の「PR 作成（`N`）」）。** ref を動かさない PR 作成が引き金に要るのは、
**GitHub 側の一覧だけが変わる**ため。前の 2 つは
**どちらが先に届くかは決まらない**ので、両方の順序を回す
テストを書く。#63 の `RepoInfo.target` はまさにこの 2 本の順序で答えが変わり、
latch したせいで二度と直らなかった。**こちらは latch しない。**

**`Msg::RepoInfoLoaded` の側は `Ok` の腕だけが頼む。** `Err` の直後は `head` を
更新していないので、引き直しても同じ答えを 1 往復ぶん買うだけになる
（`resolve_target` を `Ok` の側だけで呼ぶのと同じ理由）。

**それでも、ref を動かさない書き込みでは引きすぎる。** `BranchCreate` / `BranchDelete`
のように GitHub 側の PR が変わりようのない操作でも 1 往復払う。**引き金を `GitOp` の
判る `exec_finished` へ移す形は採っていない** —— `f`（fetch）と `P`（push）は
**ローカルの HEAD を動かさない**のに GitHub 側の PR とその CI は動くので、`Reload` の
3 値からは選べず、`GitOp` ごとの一覧をもう 1 本作ることになる。`core` の予算は
5000/時（05 の実測表）なので、いまはこの無駄を選んでいる。

**返信に鍵は要らないが、世代は要る。** 鍵が答えるのは「選択が動いたか」で、一覧は
選択に紐づかないから要らない。**世代が答えるのは別のこと** —— 書き込みの最中に飛んで
いた要求の答え（書き込み前の GitHub の状態）が採られ、そのあとの引き直しが
`AppState::issue` に落とされたまま誰も拾わない形は、鍵の有無と関係なく残る
（`FetchPrList` は落ちても通知が出ない）。

**アカウントが決まっていないことを `Fetch` に持たせない。** `AccountState` から
導ける。持たせると `Unasked` の理由が状態にも写り、どちらが正かが決まらなくなる。

### 集計（チェックとレビュー）

一覧の各行について `checks` と `reviews` を引き、行に出す（07 の「PRs ペイン」）。

- **扇形でも `Cmd` は 1 本**（`Cmd::FetchPrDetails { targets: Vec<(番号, head sha)> }`）。
  `JobTracker` は種別単位なので、PR ごとに `Cmd` を出すと 2 本目以降を
  `AppState::issue` が黙って落とす。同時本数を抑えるのは `github` の中（05 の
  「`Semaphore`」）
- **答えは 1 件ずつ届く**（`Msg::PrDetailLoaded`）。60 件を同時 4 本で引くと全部
  揃うまで数秒かかるので、揃うのを待たない。**ジョブを畳むのは終端**
  （`Msg::PrDetailsFinished`）のほう
- **「まだ引いていない」を状態に書かない。** 表に**居ないこと**がそれ。書くと、
  `issue` に落とされた `Cmd` のぶんが「頼んだのに誰も返さない行」として残る ——
  落ちるのは扇形が走っているあいだに新しい一覧が届いたときで、**そのときは走って
  いるほうが終端で残りを数え直す**
- **どの失敗で止めるかは `app` も `runtime` も判じない。** `GithubError` に答えられる
  問いなので `github` が持つ（`GithubError::repeats_for_every_target`）。`app` は
  終端が運んできた「打ち切った理由」を受け取るだけ
- **引くのは PRs タブが見えているあいだだけ**（`AppState::wanted_details`）。既定の
  タブは Branches なので、門が無いと**PRs を一度も開かない起動でも一覧の行数ぶんの
  往復を払う**（数は 05）。`wanted_diff` と同じ形 —— **見えているペインのぶんだけ
  引き、引き金は `update::focus`**（フォーカスを動かすキーが全部通る入口）。
  **出口（`reseat_focus`）には足さない**（「失敗の記憶」で採らないと決めた形）
- **門は `right_tab` を直に読まない。** あれを書くのは `reseat_focus` ——
  `update` の**出口** —— の 1 箇所なので、`Tab` で PRs に入ったその回はまだ
  `Branches` のまま。直に読むと**切り替えたその回だけ門が閉じ**、行が `…` のまま
  次に一覧が届くまで埋まらない。**「いま見えているタブ」は
  `AppState::visible_right_tab` の 1 本**（`focus` が右列ならそちら、居ないときだけ
  憶えているタブ）で、**揃える側（`reseat_focus`）と読む側（`wanted_details`）が
  同じ式を使う** —— 不変条件（`reseat_focus` を通ったあとは `right_tab` がこれと
  等しい）が代入 1 本として読める形にしてある
- **latch（一度開いたら以後ずっと引く）にはしない。** タブを Branches に戻している
  あいだも払い続けることになり、門で減らしたかった往復がそのまま残る。**離れている
  あいだに増えた行は、戻ってきたときに `update::focus` が拾い直す**
- **ペインが見えていることと門が開いていることは同値。** 描くのは `reseat_focus` の
  あとなので、`ui` が読む `right_tab` は `focus` と揃っている —— つまり**画面に
  出ている PRs ペインの行は、必ず門の開いた側にある**。07 の「集計が居ない＝まだ
  引いていない（待ちの印）」はそのまま生きていて、門が「誰も引かない行」を画面に
  出すことは無い
- **`Tab` で通過するだけでも開いたことになる。** 巡回の並び（`Pane::WIP_FOCUS` /
  `COMMIT_FOCUS`）では `Prs` が下の領域より手前にあるので、**`Tab` で下へ向かうと
  途中で 1 回踏む**（`Shift+Tab` で戻れば踏まない。並びは実物を見る —— ここに
  写すと腐る）。つまり門が減らすのは**「PRs を一度も通らない起動」**で、通る人の
  払う額は門の前と同じ。並びを変えて避ける形は UX の話なので、ここでは採らない
- **行単位の門（見えている行だけ引く）は採らない。** 窓を導いているのは
  `ui::render_rows`（07）なので、`core` が行単位で門にするには**PRs ペインの行数を
  `Msg::Layout` に足し、窓の導出を `core` にもう 1 本置く**ことになる（`ui` と
  二重化する）。そのうえで**スクロールが新しい引き金**になり、`j` を押し続けた
  ぶんだけ扇形が飛ぶ。1 + 3N は依然としてタブを開く人が払う（open 60 件で 181
  リクエスト。05）ので、**塞ぐならタスクとして立てる**（plan.md に未チェックの行）
- **鍵は `(番号, head sha)` で、世代は見ない。** チェックはコミットに紐づくので、
  push で head が進んだ PR の答えは前のコミットのもの。`PrState::detail_loaded` が
  **いまの一覧**と突き合わせて捨てる。**世代を足しても増える守りが無い** ——
  古いと判じたところで、集計を憶えている行は `wanted_details` が拾わないので
  頼み直しが出ない（`JobKind::region` にも書いてある）
- **持っている行は引き直さない。** 1 件ぶんの往復が PR の数だけ積むので、毎回
  全件引くと `core` の予算に対して重すぎる（数は 05 の「扇形は毎回引き直さない」に
  実測で置いてある）。落とすのは head sha が動いた行（`PrState::loaded`）、
  アカウントが変わったとき（`PrState::forget`）、`r` で名指しされた行
  （`PrState::forget_detail`。下の「引き直す（`r`）」）
- **`reviews` は sha に紐づかないので、この鍵では古くなる。** sha が動かないまま
  レビューが付いても、鍵は「引き直せ」と言わない。**鍵に答えられない問いなので、
  答えるのは人のほう** —— 下の「引き直す（`r`）」がその経路で、自動の引き金は
  いまも sha しか見ていない
- **打ち切ったら、頼んだのに答えの返らなかった行を理由ごと埋める**
  （`PrState::details_aborted`）。埋めないとその行は「まだ引いていない」のまま
  `wanted_details` に拾われ、**同じ失敗を買い直す輪**になる（上の「`Err` から
  再発行してよいのは対象が違うときだけ」）。
  **埋める相手は頼んだ対象で、いまの一覧ではない** —— 一覧を舐めると、扇形が
  走っているあいだに増えた行（`AppState::issue` に落とされて頼まれていない）まで
  塞ぎ、次の `fan_out` も空になって**一度も叩いていない行が二度と引かれない**。
  そのため `Msg::PrDetailsFinished` が理由と一緒に対象を運ぶ。
- **引けなかった記憶は、次に一覧が届いたときに落とす**（`PrState::loaded` が `Err` の
  行を捨てる）。落とさないと `wanted_details` が二度と拾わず、**回線が 1 回切れた
  だけでセッション中ずっと `!` のまま**になる。**1 件ごとの失敗は通知に出ない**
  （出るのは行の印だけ。07）ので、落とせるのはここと、`r` で名指しする経路
  （下の「引き直す（`r`）」）。**輪にはならない** ——
  1 回の扇形の中では答えが入って `wanted_details` が空になり、次に頼むのは
  新しい一覧が届いたときだけ
- **世代（`PrState::epoch`）も要る。鍵だけでは足りない。** アカウントを切り替えても
  PR の番号も head も変わらないので、**前のトークンで飛んでいた扇形の答えが新しい
  一覧にそのまま当たる**（`forget` が落とすと言っている守りを、走っている扇形が
  外から破る）。`forget` が世代を進め、返信が持ち帰った世代と違えば**効果だけ**を
  捨てる —— **ジョブは世代に関係なく畳む**（畳まないと以後の `Cmd::FetchPrDetails`
  が `AppState::issue` に落とされ続ける）。
  **`JobTracker` の世代とは別物** —— あちらは git の書き込みが何を古くするかを
  数えるもので、アカウントの切り替えでは進まない
- **打ち切ったときだけ通知を出す。** **1 件ごとの失敗では出さない** —— fork が
  消えた PR の 404 は行ごとの事情で、一覧ぶん通知を積んでも次に何をするかが
  変わらない。
  **理由は要約行ではなく `Notice.detail` に載せる** —— `GithubError` の `Display` を
  要約行に混ぜると 1 行目の予算（上の「失敗の案内文」）を超え、末尾が `…` に切れる。
  `InvalidHost` はホスト名をそのまま持ち、**ホスト名に上限が無い**ので、予算をいくつに
  してもこれは変わらない（`-bad` の 4 文字でちょうど 72 セル。実測 2026-09-18。
  当時の予算が 72 だったのは `q:quit` と同じ行だったため）。`exec_finished` が argv と stderr を
  `detail` に載せるのと同じ形で、**読めるのは `L` の履歴**（上の「通知」）。
  **行き止まりにはならない** —— 引けなかった記憶は次に一覧が届いたときに落ちるので、
  `f` で引き直せる（上の項目）

### 引き直す（`r`）

PRs ペインの `r` で、**カーソル行の集計を落として一覧を引き直す**。返すのは
`prs::reload` の `Cmd` 1 本で、落とした行を引き直すのは**一覧が届いたときの既存の出口**
（`pr_list_loaded` が出す `fan_out`）。

- **自動の引き金では拾えないものがある。** 集計の鍵は `(番号, head sha)` なので
  **sha が動かない変化**（レビューが付いた / チェックが回り直した）を拾えず、一覧の
  引き金（上の「何を引くか」）には**タブを開く操作が入っていない**ので、他人が開いた
  PR は `f` を押すまで出ない。**どちらも「いつ変わるかを mewgit が知らない」形**
  なので、引き金を増やすのではなく人に押させる
- **その場で扇形を頼まない。** `Cmd::batch([reload, fan_out])` にすると、**これから
  引き直す一覧を待たずに手元の sha で 3 本張る**ことになる —— head が動いていた回は
  `detail_loaded` か `loaded` がその答えを捨てて新しい sha でもう 3 本払い、merge されて
  いれば捨てるだけで頼み直しも無い。**一覧の返信を待てばどの分岐でも 1 + 3 に収まり、
  扇形を頼む場所も増えない**
- **落とすのはカーソル行の集計だけ。** 全件落とすと、押すたびに一覧の行数ぶんの
  往復を払う（open 60 件で 181 リクエスト。05）。**見ている行が引き直したい行**で、
  ほかの行は `j` / `k` で寄ってから押せばよい。**実測（2026-09-19、open PR 6 件の
  `sharkdp/diskus` を指す一時リポジトリ。1 つのプロセスの中でヘッダの `[api: N]` が
  落ち着いた値を録画から拾った）** —— 1 回だけ押した回は、扇形が終わって 4314、
  押して 4310（**4**）。3 回押した回は 4370 → 4365 / 4361 / 4357（5 / 4 / 4 ——
  最初の 1 回が 5 なのは扇形の返信がまだ残っていたから）。**何も飛んでいなければ
  1 回 4 リクエスト**（一覧 1 本 + 行 1 つの集計 3 本。飛んでいるときは上の
  「終端が一覧より先に届くと」）で、全件落としていれば 3N を
  もう 1 回払う（同じ回の扇形は 4388 → 4370 の 18 = 3 × 6）。
  **測り方に 2 つ罠がある** —— 起動をまたいだ差では測れない（`auth::resolve` の
  probe は残量をヘッダに出さないので、次の起動の初期値がその手前で減っている）。
  ヘッダは**桁ごとに部分再描画される**ので、生の録画からは落ち着いた値だけを拾う
  （途中の値が混ざると 1 桁違う数が並ぶ）
- **一覧のほうは丸ごと引き直す。** 一覧は 1 往復で済み、しかも**他人が開いた PR を
  知る経路がほかに無い**（`f` も一覧を引き直すが、あちらは fetch が走る）
- **世代（`PrState::epoch`）は進めない。** 進めるのは `PrState::forget`
  （アカウントが変わった）で、`r` はアカウントを変えない —— 飛んでいる扇形の答えは
  **いまのアカウントの答え**なので、捨てる理由が無い
- **扇形が走っている行には、落とすものが無い。** 扇形の対象は `wanted_details`
  （集計を**持っていない**行）なので、**答えが飛んでいる行は集計を持たない** ——
  そこで `r` を押しても `forget_detail` は空振りし、答えは普通に `detail_loaded` で
  入る。**「落とした行に飛んでいた答えが入る」形は組めない**（2026-09-19、
  `/code-review` が 3 箇所の記述を反証した）
- **「一覧の sha で引く」は、扇形の終端が一覧より先に届くと破れる。** `r` が本当に
  落とすのは**集計を持っている行**で、そこで**別の行**の扇形が走っていると、
  `pr_details_finished` の数え直しが**手元の sha**で 3 本張る。head が動いていれば
  その答えは `detail_loaded` の `in_list` に捨てられ、届いた一覧の sha でもう 3 本
  —— **その順序のときだけ 1 + 3 + 3**。**行き止まりにはならない**（どちらの順序でも
  最後は正しい sha の集計が入る）ので、**数え直しから「`r` が落とした行」を外す形は
  採らない** —— 外すには「まだ引いていない」と「`r` が落とした」を状態で割ることに
  なり、上の「『まだ引いていない』を状態に書かない」を破る
- **一覧が飛んでいるあいだに押すと `Cmd` が落ちる。** `AppState::issue` が同種を
  落とすので `r` は `Cmd::None` を返す。**それでよい** —— 走っているほうが新しい一覧を
  返し、その出口の `fan_out` が落とした行を拾う。**`r` が「押しても何も起きない」に
  見えるのはこの 1 通りだけ**で、そのときは既にスピナーが回っている
- **失敗の記憶もここで落ちる。** 上の「引けなかった記憶」は次に一覧が届くまで
  落ちないので、`r` は行の `!` を人の意思で消せる経路でもある
- **引けない理由は `AppState::github_block` が値で答える。** `wanted_pr_list` は
  その `is_none()` で、**理由を呼び出し側が消去法で組み直さない**。組み直すと
  `AccountState::Degraded` が「アカウントが決まっていない」に潰れ、**同じ瞬間に
  30 セル隣のペインが出している「`GitHub: …（A）`」と食い違う**（`Degraded` でも
  Prs は巡回候補に残るので今日踏める）。`TargetState` の `Unasked`（待てば変わる）と
  `Unavailable`（恒久）が潰れるのも同じ形で、**どちらも状態が既に持っている区別**。
  材料を 1 つ足した瞬間に `else` の側が黙って嘘になる形でもある
  （`.claude/rules/rust-coding.md` の「離れた場所に『いま N ある』を書かない」）
- **`A` の通知（`account_view` の `Err`）も同じ値から文言を採る。** どちらも通知 1 行で
  **予算が同じ**なので、#73 の「置かれる場所の予算が違うなら別の文言でよい」は
  当たらない。**枠の内側 30 セルのペイン（07）とは分けてよい**
- **理由の字は core で展開しない。** `DegradedReason` を字にする `ui::degraded` は
  `mewgit-tui` にしかないので（不変ルール 6）、core の通知は「`A` で見られる」までに
  する。**ペインに `TargetState` を読ませて文言を割るのは M6**（plan.md の
  「既定リモートが GitHub でないときの PRs ペインの文言」）—— そちらは
  `github_block` をそのまま読めるので、残るのは 30 セルに畳む仕事だけ

### 残量（レートリミット）

GitHub API の残量をヘッダに出す（07 の「ヘッダ」）。**産出元は PR の 2 本だけ**
（一覧と扇形）で、**採るのは自分が出したリクエストの応答ヘッダ**（05）。

- **置き場所は `PrState`。** 落とす引き金（`PrState::forget`）と、突き合わせる世代
  （`PrState::epoch`）が両方そこにある。`AppState` の直下に置くと、**落とす責務だけが
  別の場所に離れる** —— アカウントが変わったときに落とすものが 2 つになった時点で
  共有先は 1 つにする（`.claude/rules/rust-coding.md`）
- **世代で弾く。** 残量は**そのトークンの予算**なので、アカウントを切り替えたあとに
  前のトークンの扇形が返す数は嘘になる。`(reset, used)` は「どちらの応答が新しいか」
  しか答えられない（#89 の「鍵が答えられる問いを数える」）。`Msg::RateLimitSeen` が
  世代を運び、`forget` が残量を落として世代を進める
- **届いた順で上書きしない。** 一覧と扇形は同時に走り、扇形の中でも 4 本が並ぶので、
  `Msg` の届く順は応答の新しさの順ではない。畳むのは `RateLimit::newer`（05）
- **`auth::resolve` の probe の残量は運ばない。** クライアント自身は応答ヘッダから
  憶えるが（`get_page` は全員が通る）、`Msg::RateLimitSeen` を送るのはランタイムの
  PR の腕だけで、probe のクライアントは `auth` の中で使い捨てになる。**運ぶと
  「選ばれなかった人の予算」がヘッダに出る** —— probe は**まだ誰のトークンか
  決まっていない候補**を 1 件ずつ試すので
- **ジョブは持たない。** 走っているのは一覧か扇形のジョブで、残量はその途中に何度でも
  流れる。**扇形は 1 件ごとに送る** —— 60 件の扇形は数秒かかるので、終端まで待つと
  **いちばん減っているあいだ**ヘッダが古い数を出し続ける

### 現ブランチの PR

**一覧の 1 行として出す。** `RepoInfo.head` がブランチに居て、`head.repo` が
**そのブランチの head が在りうるリポジトリ**と一致し、`head.branch` が**そのリポジトリでの
ブランチ名**と一致する PR がそれ（名前は候補ごとに決まる。下の「比べる名前」）。
**起動直後のカーソルはその行に座る。**

**候補は 1 つに決まらない。git は「どこへ push したか」を憶えていない**（git 2.55、
`env -i` の一時リポジトリで実測。2026-09-20）。

| 手順 | `branch.<name>.remote` | 実際の push 先 |
|---|---|---|
| `checkout -b fix bob/fix` → `push origin fix` | `bob` | origin |
| `remote.pushDefault=bob` で `push` | **未設定** | bob |
| `push -u fork feat/x`（既存の `origin` を上書き） | `fork` | fork |
| `gh pr checkout <n>`（fork からの PR） | **fork の URL**（リモート名ではない） | fork |

だから**追跡先 → `target` の順に当てる**。追跡先が先なのは、fork 運用で upstream 側の
同名ブランチから出た**他人の PR** より自分の fork の PR を採るため。`target` にも
落とすのは、1 行目の形（他人の fork を基に作って origin へ push した）で
**一覧に出ている自分の PR に座らなくなる**のを防ぐため。

- **状態に持たない。** `list` と `RepoInfo.head` から毎回導ける。持つと、どちらかが
  更新されたときに片方だけ古くなる（**導出できる値を、元から離れた場所に置かない**）
- **追跡先は `HeadInfo::Branch.upstream` の `remote`（`branch.<HEAD>.remote`）の URL から
  出す。** 一覧には fork からの PR も出る（05）ので、`target` とだけ比べると
  **fork のブランチでは合致しない**。**値はリモート名でも URL でもよい**（git の仕様）
  ので、`RepoInfo.remotes` で引けたらその URL を、引けなければ値そのものを
  `RemoteUrl` に通して `owner` / `repo` を取る。**URL の腕を落とすと
  `gh pr checkout` の形が当たらない**（上の表）。**規則を持つのは `app`** ——
  `git_read` が返すのは git が持っている値（リモート**名**）までで、名前を URL に直し、
  GitHub の形として読めるかを判断するのは `auth::url` を使う側の仕事。04 の「既定
  リモート」が同じ理由で `app` に置いている。**00 が禁じているからではない** ——
  `auth` は `git_read` の下段なので、`git_read` から `auth::url` を呼ぶこと自体は
  下向きで許される（`github` が `auth::url::valid_host` を使っているのと同じ）
- **比べる名前は、追跡先が URL の腕だけ `merge` から取る**（`refs/heads/` を外した
  もの）。PR の取り出し（`Enter`）は fork のブランチが既定ブランチや base と同名なら
  ローカルを `bob/main` にし、`merge` に `refs/heads/main` を書く（05 の「ブランチ名」）
  ので、ローカルの名前で比べると**取り出したその PR に座らず、`N` の「既にあります」も
  素通りして作り直しに行く**。URL を書くのは取り出し（mewgit と `gh pr checkout`）と
  `git push -u <URL> <local>:<remote>`（`merge` は `refs/heads/<remote>`。実測
  2026-09-23、git 2.55）で、どれも `merge` は「その URL に在るブランチ」そのもの。
  **名前の追跡先と `target` の腕はローカルの名前のまま** —— 名前の追跡先の `merge` は「何を基にしたか」で、
  `git checkout -b feat upstream/main` は `merge` を `refs/heads/main` にする。そこで
  `merge` を使うと upstream の `main` から出た PR に座り、`N` がそれを「既にある」として
  断る。`refs/heads/` の下に無い `merge`（`refs/pull/<n>/head`）の URL の腕は候補に
  しない —— 取り出しはその `merge` を URL と一緒には書かない（05 の「追跡先」）。
  **読み方は `RepoInfo::url_tracked` の 1 本**で、送る側の `head` の候補（下の「head の
  組み立て」）も同じものを通る。片方だけ直すと「取り出した PR に座らない」と「fork に
  無いブランチを送る」が片側だけで起きる
- **追跡先で当たらなければ `target` でも当てる。** 読めない追跡設定（設定が無い /
  `.`（ローカル追跡）/ その名前のリモートも URL でもない / GitHub の形でない）は
  候補にしない。**理由は分けない** —— 扱いはどれも同じで、出す先も無い
  （座る行が変わるだけで通知は出さない）。**残る穴**は、fork 運用で upstream 側にも
  同名のブランチの PR があり、しかも**自分の fork の PR が無い**とき ——
  そのとき他人の行に座る
- **材料は `Cmd::LoadRepoInfo` に載せる**（02 の「head」）。`BranchState` にも同じ
  設定があるが、**座り直しは 1 回きり**（一覧が 1 度届くと以後は選んでいる番号で
  座るので、この導出は二度と走らない）なので、**別のジョブから届く値を混ぜると
  到着順で答えが変わり、二度と直らない**。`RepoInfo.target` が `default_remote` を
  呼ばないのと同じ数え方
- **host は比べない。** `target.host` は `ssh -G` で正規化済み、こちらは URL に書かれた
  ままなので、生のまま比べると **fork だけ別のエイリアス（`gh-me:owner/repo`）を使う
  多アカウント運用**——04 が想定している形——で逆に落ちる。**正規化をもう 1 回頼むと、
  カーソルの位置のために `ssh -G` が 1 本増える**ので採らない。**残る穴**は、
  別ホストの同名 `owner/repo` に同名のブランチがあり、そこから出た PR が一覧に
  居る場合で、そのとき**カーソルが他人の行に座る**
- **`branch.<name>.pushRemote` と `remote.pushDefault` は読まない。** git の push 規則は
  fetch と違う（下の「push の行き先」に送ってある）。**読んでも「どこへ push したか」は
  出ない** —— 上の表のとおり config はそれを憶えていないので、精度が上がるのは
  `pushRemote` を明示している形だけ。`gh pr checkout` はそれを書くが、
  `branch.<name>.remote` にも同じ URL を書くので、いまの読み方で当たる
- **マージ済み / クローズ済みの PR は出ない。** 一覧は `state=open` だけを引くので、
  **PR がマージされた瞬間にその行は消える**。HEAD のブランチの PR を `MERGED` として
  出していた経路（`pr_for_branch` と `Cmd::FetchPr`）は、一覧に変えたときに消した。
  `o` もカーソル行しか開けないので、**mewgit から閉じた PR を見る手段は無い** ——
  **受け入れた穴**で、一覧に `state=all` を混ぜると辿るページが 2 桁増え、2/2 の扇形
  （PR ごとに checks / reviews を引く）が現実的な大きさに収まらない

### `o`（ブラウザで開く）

`gh browse <番号> -R <host>/<owner>/<repo>` を起動する。**開くのはカーソル行の PR**。
**URL を自分で組んで `open` / `xdg-open` を呼ばない** —— 不変ルール 7 が外部プロセスを
`git` `gh` `ssh` に限っている。

- **`gh browse` が出すのは issue の URL**（実測 2026-09-16、gh 2.100.0:
  `gh browse -n 123 -R github.com/cli/cli` → `https://github.com/cli/cli/issues/123`）。
  GitHub がそれを `302` で `https://github.com/cli/cli/pull/123` に飛ばす
  （`curl -sI` の `location` で実測）。**GHES で同じかは確かめていない**（手元に無い）
- **`PullRequest.url` は使わない。** 値としては持っているが、`gh browse` は URL を
  引数に取らない（`<number> | <path> | <commit-sha>` だけ。`gh browse --help`）
- `Cmd::OpenPr { number }` と `JobKind::OpenPr`。**`Cmd::Exec` に相乗りさせない**
  —— git の実行中に押せなくなるうえ、`Ctrl-C` の中断（下の「キャンセル」）が
  git 以外のプロセスに効くことになる
- **`gh browse` は起動したブラウザが終わるまで返らない**（実測 2026-09-16、
  gh 2.100.0: `BROWSER` に 5 秒眠るスクリプトを置くと 5.06 秒）。既定の `open` 経由なら
  すぐ返るが、端末ブラウザや自分で抜けないラッパーを `BROWSER` にしている環境では
  `JobKind::OpenPr` がそのあいだ走り続ける。**待っているあいだの `o` は黙って
  落とさない** —— `AppState::notify_dropped` が「ブラウザの起動を待っています」を出す
  （#63 の「無反応よりまし」で止めない）
- 失敗は通知に出す。`AuthError::GhFailed` が argv と出力を持っている（04）。
  **gh がそのホストにログインしていないと落ちる**（実測: `env -i` で
  `gh browse -n 123 -R github.com/cli/cli` が exit 4、
  `To get started with GitHub CLI, please run:  gh auth login`）
- **一覧が空のときは `N` を案内する。** 開ける PR が無いのは PR を作る場面なので、
  「開ける PR がありません」だけで止めると行き止まりになる（#63 の「案内が求める
  操作を、そもそも自分が持っているか」）。**キーの案内を出せるのは `N` が入ってから**
  で、それが下の節

### PR 作成（`N`）

PRs ペインの `N` で `Modal::PrCreate` を開き、確定で `POST /repos/{o}/{r}/pulls`（05）を
1 本出す。`Cmd::CreatePr` / `Msg::PrCreated` / `JobKind::CreatePr`。

**欄は title / body / base / head / draft**。`Tab` / `Shift+Tab` で巡回し、
どの欄を編集しているかは `Modal::PrCreate.field`（`PrField`）が持つ。
**`base` も `TextBuffer`** —— ここにはずっと `base: String` と書いてあったが、
要件（`docs/requirements.md`）がベースブランチの指定を求めており、編集できる欄は
`TextBuffer` でないとカーソルを持てない。

**キーは `Modal::CommitMessage` に揃える。** 修飾なしの `Enter` はどの欄でも確定、
修飾付きの `Enter` と `Ctrl-J` は body 欄でだけ改行、`Esc` で破棄。**改行と行移動の
規則は `update::multiline_edit_key` の 1 本**（2 人目が出た時点で `commit_message_key`
から割った）—— 写すと、端末ごとの改行キーを直したときに片方だけ古くなり、
**どちらのテストも通る**。

**貼り付け（`Msg::Paste`）も欄で変わる。** body は複数行、title と base は 1 行に
つなぐ（`Modal::Input` と同じ。上の「1 行入力」）。**`update::paste` の `match` は
網羅にする** —— 受け皿の `_` にしていたせいで、このモーダルを足した回に**貼り付けだけが
黙って無反応**になっていた。
**`head` の欄では `←` / `→` が候補を選び直す**（下の「head の組み立て」）。文字は
打てない —— 候補の外の値を打たせると、推定を人の目に通す意味が無くなる。
**`draft` の欄では `Space` がトグル** —— `Enter` に載せると「決定」と「切替」が
同じキーになる。**確定を `Alt+Enter` だけにしない** —— `CommitMessage` も `Input` も
`Enter` で確定するので、モーダルごとに `Enter` の意味が変わる形になる。

#### 初期値

- **title と body は空。** HEAD のコミットの要約を入れる形も書けるが、それには
  `Cmd::LoadHeadMessage` の答えが要り、**別のジョブから届く値を初期値に混ぜる**ことに
  なる（#63 の `RepoInfo.target`）。押した瞬間に開くほうを採る
- **base は `refs/remotes/<既定リモート>/HEAD` が指すブランチ。**
  `git_read::Remote` が `head_branch` として運ぶ（02）ので、**往復を増やさない**。
  `GET /repos/{o}/{r}` の `default_branch` でも出せるが、モーダルを開くのが非同期に
  なり「引いている最中」「引けなかった」の腕が増える。**symref が無い形はある** ——
  `git remote add` しただけで一度も fetch していないとき（clone 直後と `git fetch` の
  あとには在る。実測 2026-09-21、git 2.55、`env -i` の一時リポジトリ）。そのときは
  **空で開いて人に打たせる**
- **draft は `false`**

#### head の組み立て

`head` は**モーダルの `Head` 欄で人が選んだ候補**。候補は `N` を押した時点で
`RepoInfo` と ref の索引から組み（`AppState::head_candidates`）、欄に `<owner>:<branch>` として
見せる。**送るまで人の目を通す** —— 推定が外れても黙っては送られない。

**なぜ推定だけで決めないか。git は「どこへ push したか」を憶えていない**（上の
「現ブランチの PR」の表）。`@{push}` も決め手にならない —— 実測（2026-09-23、
git 2.55、`env -i` の一時リポジトリ。`push.default` は既定の `simple`）:

| 手順 | `branch.<b>.remote` | `git rev-parse --abbrev-ref <b>@{push}` |
|---|---|---|
| `checkout -b feat upstream/main`（未 push） | upstream | `fatal: cannot resolve 'simple' push to a single destination` |
| そのあと `push origin feat`（`-u` 無し） | upstream | **同じ fatal**（`refs/remotes/origin/feat` は在る） |
| `remote.pushDefault=origin` にして `git push` | upstream | **同じ fatal**（push 自体は origin へ通る） |
| `push -u origin feat3` | origin | `origin/feat3` |

**gh も推定だけでは送らない**（gh 2.101.0 の `pkg/cmd/pr/create/create.go` と
`pkg/cmd/pr/shared/find_refs_resolution.go` を読んだ）。push 先の設定
（`@{push}` → `pushRemote` → `remote.pushDefault` → `branch.<b>.remote`）を当て、
その `refs/remotes/<r>/<b>` が **HEAD と同じ SHA** なら採る。だめなら各リモートの
同名 ref を HEAD の SHA と照合し、それでも決まらなければ「どこへ push するか」を
プロンプトで聞く。

**候補**（`AppState::head_candidates`。座り直しの `head_homes` の隣に置く ——
同じ材料の寛容な読み手と断定する読み手を並べ、#101 で割った理由を見失わないため）:

- **同名の remote-tracking ref が在るリモート** —— 各リモートの
  `refs/remotes/<r>/<branch>` を `graph.refs`（`RefIndex::remote_oid`。02 の「refs と
  ラベル」）から引く。**読み取りを足さない** —— 索引は `repo.head` と同じ
  `Msg::RepoInfoLoaded` で届くので、2 つのあいだに順序は無い。**在ることは
  「そのリモートにそのブランチが在った」の証拠**で、`branch.<name>.remote`（何を
  基にしたか）と違って push の結果そのものが残る（上の表の 2 行目で `origin/feat` が
  在るのはこれ）
- **追跡設定の URL**（`branch.<name>.remote` が URL のとき）—— `gh pr checkout` と
  mewgit の取り出し（`Enter`）は fork の URL を書き、**remote-tracking ref を作らない**
  （05）。ref の腕だけにすると、**取り出した fork の PR に push し直して PR を作り直す**
  いちばんふつうの形が候補から漏れる。**ブランチ名は `merge` から取る** —— 取り出しは
  衝突を避けてローカルを `bob/main` にし、`merge` に `refs/heads/main` を書く（05 の
  「ブランチ名」）ので、ローカルの名前で組むと fork に無い `bob:bob/main` を指す
  （`/code-review` が拾った）。`refs/heads/` の下に無い `merge`（`refs/pull/<n>/head`）は
  ブランチではないので候補にしない。材料は `HeadInfo::Branch.upstream`（remote と
  merge の対。02 の「head」）。**読み方は座り直しと同じ `RepoInfo::url_tracked`**
  （上の「比べる名前」）
- **GitHub の形として読めないリモートは候補にしない**（`RemoteUrl::parse`）。
  **host は比べない**（上の「現ブランチの PR」と同じ理由）
- **送る値（owner とブランチ名）で 1 つに畳む。** `POST /pulls` の `head` が運べるのは
  `<owner>:<branch>` までで、同じ owner の 2 つのリポジトリ（`acme/app` と
  `acme/app-fork`）は区別できない
  （`head_repo` を送る形は組まない —— 踏むリポジトリが出てから）

**並びと既定。** HEAD と同じ oid の ref を持つ候補を先に置き（gh が採るのと同じ
「そこまで push 済み」の証拠）、残りはリモートの並び順、URL の候補は最後。**既定は
先頭** —— #101 の「候補の一覧の先頭を断定値に使わない」を破らないのは、**先頭が
画面に出て人が確定する**から。欄では `←` / `→` で選び直す。

**送る値。** 選んだ候補の owner が `target` の owner と同じなら**ブランチ名だけ**、
違えば `<owner>:<branch>`（gh の `QualifiedHeadRef` と同じ）。

**候補が 1 つも無ければ `N` を押した時点で断る**（下の「押した時点で断る形」。
2026-09-23 にユーザーと決めた）。**ブランチ名だけで送る経路は残さない** ——
owner を付けない `head` は**base リポジトリの中のブランチ**として解決されるので、
`target` 側に同名のブランチがあれば**他人のブランチから PR が開く**（**実 API では
確かめていない** —— 確かめると PR ができてしまう）。**代償** —— 別の端末から
push しただけで一度も `f` していないと、remote-tracking ref が無いので `f` するまで
断られる。**断りの文言が `f` を案内する**のはそのため。

**確定のときに組み直す。** モーダルを開いているあいだに `f` や外の checkout で
`RepoInfo` が読み直されると候補が変わる。**写しを送らない** —— 確定の時点で
`head_candidates` を引き直し、選んでいた候補（owner とブランチ名）が残っていなければ
**閉じずに欄を新しい候補で入れ替え、確かめ直させる**（本文は残る）。

**push 済みかは見ない。** ref が HEAD より遅れていても候補にする（既定の並びが
後ろになるだけ）—— `head` が指すのは**リモートのブランチ**なので、正しいかは
「どのリポジトリか」で決まる。ローカルにしか無いコミットは push の問題で、PR を
作るかどうかとは別（遅れた head で GitHub が断るかは**実 API では確かめていない**）。
**追跡設定（`Branch::upstream_config`）は読まない** —— あの `None` には
理由が 4 つあり（#47）、ここで要るのは「どこに在るか」のほうなので ref を読む。

#### 押した時点で断る形

どれも**リクエストを組めない**か、**組んでも通らない**もの。

| 状態 | 出す案内 |
|---|---|
| GitHub が使えない（`AppState::github_block`） | `github_block` が値で答える文言（`r` と同じ。上の「引き直す（`r`）」） |
| HEAD がブランチでない（detached / unborn） | head に載せる名前が無い |
| このブランチの PR が既にある（`AppState::current_pr`） | その番号を出す |
| `head` の候補が 1 つも無い（上の「head の組み立て」） | どのリモートにも無い。push したなら `f` |

**3 つ目は `AppState::current_pr` の答えをそのまま使う。** 別の判定を書くと、座り直し
（上の「現ブランチの PR」）と食い違う画面が作れる —— **カーソルが座っているその行の
PR を「無い」と言って作りに行く**形になる。**行き止まりにはならない** ——
一覧を落とす経路（`f` / `A` / `r`）がそのままこの断りを落とす経路になっている
（#53 の「状態を消すのが『その状態を根拠に断つキー』の先にあるなら、断つ前に消し方を
数える」）。

**断る側と送る側で読む材料が違う。** `current_pr` は `head_homes()` を通す**寛容な
照合**で、`head` は**remote-tracking ref と追跡設定の URL から組み、人が選ぶ**
（上の「head の組み立て」）。そのため
**追跡先が fork を指していて、その fork から比べる名前（上の「比べる名前」）の
ブランチの PR が出ている**とき、
その PR が自分のものでなくても `N` は「既にあります（#n）」で断る。**受け入れた穴**
——「このブランチから出た PR」の定義は #98 の照合規則そのもので、断る側だけ別の規則を
持たせると座り直しと食い違う（上）。`o` でその PR を開けば自分のものかは判る。

#### 確定と、そのあと

- **`Cmd` が落とされるなら閉じない**（`update::submit`）。打った本文がそのまま消える
  ため。**種別は `Cmd::job()` から引く** —— 引数で渡させると
  `submit(state, JobKind::Exec, Cmd::CreatePr(..))` という食い違った対が型で組め、
  組むと**走っていない種別を見て閉じ、そのあと `issue` が本当の種別で落とす**
  （「導出できる値を引数に取らせない」）
- **title が空なら断る。** GitHub も 422 を返すが、モーダルを閉じてからでは打った
  本文が戻らない（`commit_submit` と同じ理由）
- 成功したら通知に `#<番号>` を出し、**一覧を引き直す**（上の「何を引くか」に足した
  引き金）
- **カーソルは作った PR に座る。** `PrState` が作った番号を 1 回だけ憶え、届いた一覧で
  その行へ座る。**憶えないと座らない** —— `PrState::loaded` は「いま選んでいる PR の
  番号」を優先するので、作る前に別の行を選んでいると、新しい行が一覧の先頭に入っても
  カーソルはそのまま残る
- 失敗は通知に出す。**要約行は通知の予算に収め、`GithubError` の `Display` と 422 の
  本文は `Notice.detail` へ**（上の「失敗の案内文」）—— 422 の本文にも上限が無い
- **`JobKind::CreatePr` の `region()` は `None`。** 返るのは**作った PR そのもの**で、
  書き込みに追い越されて古くなるという問いが無い（`OpenPr` と同じ）

### PR チェックアウト（`Enter`）

PRs ペインの `Enter` でカーソル行の PR をローカルのブランチに取り出す。組む
手順・ブランチ名・追跡先は 05 の「PR チェックアウト」にあるので複製しない。
`app` が持つのは**どの場面か**を決める部分だけ（`update::prs::checkout_plan`）。

- **押した時点で断るのは 3 つ。** 一覧が空（`o` と同じく `N` を案内する）、
  fetch するリモートが決まらない（`conventional_remote` の文言。PR 作成の base と
  同じリモート）、ブランチ一覧がまだ届いていない（「在るか」を決められない。
  05）。**GitHub の状態は見ない** —— 叩くのは git だけで、一覧が画面に出ている
  以上、材料は揃っている
- **確認モーダルは出さない。** 取り出しはどの手順も上書きしない（fetch は `+` 無しで
  ローカルのブランチを早送りするだけ、`merge` は `--ff-only`）ので、不変ルール 5 の
  破壊的操作に当たらない。分かれていれば git が断り、その 1 本の argv と stderr が
  通知に出る（03 の「複数の手順を持つ書き込み」）
- **案内文は `non_fast_forward_advice` に足す。** fetch の拒否も `merge --ff-only` の
  拒否も `NonFastForward` に分類される（03）ので、「ローカルのブランチが PR と
  分かれています」を出す
- **読み直しは `Reload::RefsAndLog`**（ブランチを作り、HEAD を動かし、ログに新しい
  コミットが入る）
- **カーソルは動かさない。** 押した行がそのまま「現ブランチの PR」になるが、
  座り直しは 1 回きりなので導出は走らない（上の「現ブランチの PR」）

**PR 作成の `head`（上の「head の組み立て」）は 2 本目に割った**
（2026-09-23 にユーザーと決めた）。plan.md は「取り出しと同じ材料になる」と
書いていたが、**同じ材料ではなかった** —— 取り出しは PR（owner が判っている）から
ブランチを作る向きで、PR 作成はブランチから owner を探す逆向き。しかも
remote-tracking ref だけでは、**`gh pr checkout` で取り出した fork のブランチ
（remote-tracking ref が無い。05）を拾えず**、`origin` と `upstream` の両方に
同名がある形では「候補の一覧の先頭を断定値に使わない」（#101）へ戻る。2 本目は
**候補を人に選ばせる**ことでこれを解いた（上）。

### 右上のタブ

`Branches` / `Stash` / `Prs` は**どれも巡回候補**に入り、`focus` がそのどれかに来たときだけ
`right_tab` が追随する。`focus` が左や下にあるあいだは、最後に見ていたタブが残る。
並びは `Pane` の宣言順（`Branches | Stash | Prs`）。

- **`focus` だけで決めない。** 左の領域（Graph / Diff）は `focus` で決める（07）が、
  右上で同じにすると**チェックの結果を見ながら `j` でグラフを辿った瞬間に消える**。
  07 が右下ペインについて「`focus` で決めると Graph にフォーカスした瞬間に右が消える」と
  断ったのと同じ形
- **`right_tab` は `Pane` をそのまま持たない**（`enum RightTab { Branches, Stash, Prs }`）。
  `Pane` で持つと、読む側が「ほかの値は来ない」という注釈付きの `_ =>` を置くことに
  なり、タブを足したときにコンパイラが何も言わない（`Branches` が黙って描かれる）。
  **3 枚目の `Stash` を足したときは、読む側の網羅 `match` が全部コンパイルエラーに
  なった**（予告どおり）。写像は `RightTab::of(Pane)` の 1 本だけで、置くのは
  `reseat_focus`。**注釈をコメントではなく型にする**
- **書き込む場所は座り直しと同じ 1 箇所**（`update` の出口）。`focus ∈ {Branches, Stash, Prs}`
  なら `right_tab = focus` を必ず通すので、`focus` と食い違ったまま残らない
- **タブの集合を非同期の状態に依らせない。** `Prs` が候補から落ちるのは
  `config.github.enabled = false` のときだけで、`AccountState` では変えない。変えると
  **起動直後に解決が返った瞬間に `Tab` の行き先が増える**（WIP 行を変更の有無で
  隠さないのと同じ理由。07）

### GitHub 機能を切る（`config.github.enabled = false`）

**`AccountState` に 4 つ目の腕 `Disabled` を足す。** `AppState::new` が
`config.github.enabled` を見て入れ、以後この値は変わらない（08。実行中は設定を
読み直さない）。

- **`Resolving` のまま放置しない。** あれは「まだ頼んでいない / 走っている最中」なので、
  `A` の「解決中です」も右端のスピナーの説明も嘘になる
- **旗を読み手ごとに見に行かない。** `AccountState` を網羅 `match` している読み手
  （ヘッダの `account_span`、`A`、`Modal::AccountPicker`、`resolve_again`）が、腕が
  増えた時点でそれぞれ何を出すかを選ばされる
- 止まるのは `Cmd::ResolveAccount` の発行（**`gh` を一度も起動しない**）、ヘッダの
  `[gh: …]`、`Prs` の巡回候補入りとタブ見出し
- **`Cmd::ResolveTarget` は止めない。** `RepoInfo.target` はヘッダの `owner/repo` が
  読んでいて（07 の「ヘッダ」）、GitHub API を一度も叩かない（`ssh -G` だけ。
  下の「`RepoInfo.target` を埋める」）

## リモート操作（fetch / pull / push）

`f` / `p` / `P` はペインに依らない共通キー（07 のキー表）。`update_key` では
フォーカスの `match` **より先に**見る。`Tab` と同じ位置で、ペインごとの腕に書くと
あとから足したペインで押せなくなる。

**照合は `Key` 全体で行う**（`q` と同じ形）。`key.code()` で見ると `Ctrl-f` や `Alt-p`
まで拾い、しかもこの段は全ペインを覆うので、07 が移動に予約している `Ctrl-d` /
`Ctrl-u` の隣で修飾付きのキーを先に食う。

| キー | 出す `GitOp` |
|---|---|
| `f` | `Fetch { remote: Some(<既定リモート>), prune: false }`（下の「既定リモート」） |
| `p` | `Pull { mode: FastForwardOnly }` |
| `P` | `Push { .. }`（下の「push の行き先」） |

- **`f` はリモートを明示する。** git に決めさせると、**複数あって `origin` も
  `branch.<name>.remote` も無いときに何もせず exit 0 で返る**（git 2.55 で実測。
  `remote.pushDefault` は push 専用で fetch には効かない）。stderr が空なので
  不変ルール 4 の「stderr を UI まで届ける」経路も効かず、**`f` が無反応になる**。
  [`default_remote`](#既定リモート) が決まらないときは `GitOp` を出さずに通知で断る
- **`p` は選ばない。** 同じ状態でも `git pull --ff-only` は
  `There is no tracking information for the current branch.` を stderr に書いて
  **exit 1** で終わる（同じ一時リポジトリで実測）。**既存の通知経路がそのまま効く**
  ので、こちらに `default_remote` を掛ける理由が無い。掛けると、git が出す
  「`git branch --set-upstream-to=<remote>/<branch> main` で設定できます」という
  **具体的な回復手順を、こちらの短い文言で置き換える**ことになる
- **pull は `--ff-only`。** MVP にコンフリクト解消ビューが無い（requirements）ので、
  merge / rebase が衝突すると mewgit の中では抜けられない。`--ff-only` は早送りできな
  ければ何も変えずに終わる。モードを選ばせるのは continue / abort が揃う M5
- **prune は付けない。** 消えた remote-tracking ref を消す操作で、`f` が黙って ref を
  消すのは「fetch は読むだけ」という期待と食い違う。出すなら M6 の設定から

### 既定リモート

**関数は 2 本。** `default_remote`（`git fetch` が取るもの）と `conventional_remote`
（名前の慣習だけで選ぶもの）。規則そのものと実測の表、どちらがどれを呼ぶかの表は
04 の「既定リモート」にあるので複製しない。要点だけ。

```
default_remote      = branch.<HEAD>.remote → conventional_remote
conventional_remote = リモートが 1 本ならそれ → origin → 決まらない
```

**`RepoInfo.target` が `default_remote` を呼ばない理由は 04 にある**（返信の到着順で
答えが割れ、latch されるので直らない）。ここで数えておくのは**引き受ける側の非対称**
だけ —— `f` はキーなので、遅れて届いたブランチ一覧は次に押したときに効く。`target` は
1 回きりなので効かない。

- **HEAD のブランチは `BranchState` から取る**（push の行き先と同じ理由。
  `RepoInfo.head` は起動時に読んだきりで古い）。まだ届いていなければ
  `branch.<HEAD>.remote` の段だけを飛ばす —— **決まらないに落とさない** ——
  リモートが 1 本のふつうのリポジトリで、起動直後の `f` が断られてしまう。
  **この寛容さは `f` にしか成り立たない**（上の非対称）
- **逆順も数える。** ブランチ一覧だけが先に届く瞬間もある（`LoadRepoInfo` と
  `LoadBranches` は別のジョブ）。リモート一覧が空のまま 1 段目を通すと、追跡先が
  見つからないのは当たり前なので「追跡先の origin がリモートにありません」と、
  **設定が壊れていると言ってしまう**。空のあいだは 1 段目に入らず
  `conventional_remote` の「まだ読めていません」に落とす
- **`.`（ローカル追跡）は決まらない。** push が既に断っている形で、
  fetch に渡すと git が「そんなリモートは無い」で落ちる
- **`branch.<HEAD>.remote` が一覧に無い名前を指していたら、その段を飛ばさず
  決まらないにする。** 飛ばすと `origin` へ落ちるが、ユーザーが書いた行き先とは
  別のリモートを黙って fetch することになる
- **ただし URL なら `conventional_remote` へ落とす。** git の仕様で値は URL でもよく、
  **`gh pr checkout` と mewgit の PR チェックアウトは fork の URL を書く**（05）。
  git 自身はそのとき `merge` の 1 本だけを `FETCH_HEAD` へ取り、**remote-tracking ref を
  1 つも動かさない**（04 の表）—— `f` の役目（グラフのラベルと `↓n` を動かす）を
  果たさないので、git に合わせる理由が無い。決まらないにすると、PR を取り出す
  たびに `f` がリモート選択を開く。**URL かどうかは git の値として決める**
  （`:` を含むか `/` 始まり。リモート名は `:` を含められない）—— GitHub の形かで
  決めると、`/srv/git/x.git` のような URL が「消えたリモート名」に化ける。
  **読み方は `RepoInfo::tracked` の 1 本**で、現ブランチの PR の `head_homes` も通る

#### 決まらないときの逃げ道

**`default_remote` が決まらないと `f` は行き止まりだった。** 通知は「追跡先を
設定してください」と出るのに、**mewgit にその操作が無い** —— リモートを指定する
キーも Remotes ペインも無く、`P` も同じ `conventional_remote` を通るので `-u` の
提案すら出ない（実機で確認）。外で `git branch --set-upstream-to=…` しても、
ブランチ一覧を読み直すのは ref を動かす書き込みのあとだけなので**再起動するまで
効かなかった**（いまは probe が `config` の差として拾う。下の「外部の変更の検知」）。`.claude/rules/rust-coding.md` の「案内にキーを書いたら、その案内が
出ている画面でそのキーが効くかを数える」の一般形 —— **案内が求める操作を自分が
持っているかを数える** —— を踏み外していた。

`f` が決まらないときは [`Modal::RemotePicker`](07) を開く。**「選ばせる相手がいるか」は
`default_remote` が返す型（`NoRemote::Nobody` / `NoRemote::Ask`）で運ぶ** —— 文字列だけを
返すと、呼ぶ側がリモート一覧を数え直して同じ判断を 2 本目に書くことになる。

- **候補が 1 本でも選ばせる。** `default_remote` が `Err` を返すのは「黙って選ぶと
  危ない」形（`origin` が無い / 追跡先がローカル / 追跡先が一覧に無い）で、**危ないのは
  mewgit が選ぶこと**。人が選ぶなら危なくないので、1 本しか無くても素通しにしない
- **リモートが 0 本のときは開かない。** 「まだ読めていません」と「登録されていません」は
  選ばせる相手がいないので通知のまま
- **追跡設定は書かない。その回限りの `GitOp::Fetch { remote: Some(..) }`。**
  `.git/config` を黙って変えないため。書くなら `-u` と同じ形（確認モーダルで行き先を
  見せる）になるが、それは push の話なので混ぜない
- **セッションのあいだ憶える案も数えた。採らない。** 憶えれば毎回出る煩わしさは消え、
  `.git/config` も触らない。**落とす経路が要るのが理由** —— 憶えた名前のリモートが
  外で消えても、`f` はそれを出し続ける。#53 の「状態を消すのが『その状態を根拠に
  断つキー』の先にあるなら、断つ前に消し方を数える」と同じ形になる。**煩わしさは
  リモートが複数あって `origin` が無いリポジトリにしか出ない**ので、消し方を決める
  コストのほうが先に立つ。**外部の変更の検知（下の節）で消し方の材料（`config` の差）は
  できたが、そのタスク（2026-10-01）では扱っていない** —— 煩わしさが出る形の狭さは変わらない
- **`P` の行き止まりは塞がない。** push の行き先を人に選ばせるのは不変ルール 5 の
  手前の判断で、fetch と同じ扱いにはできない（fetch は読むだけ）。**M6 で `-u` の提案を
  git の push の規則に揃えた**（下の「push の行き先」）が、**救われるのは
  `branch.<name>.pushRemote` か `remote.pushDefault` を書いたリポジトリだけ** —— この形の
  典型（追跡設定がまったく無いブランチ）では `pushRemote` も未設定なので、読んでも
  決まらない。**残る穴として数えておく**（`/simplify` が「確かめていない断定」として拾った）
- **理由をモーダルの本文に出す。** 「なぜ選ばされているか」が無いと、毎回出るのか
  設定が壊れているのかが判らない
- **指示を足すのは、その操作を持たない側。** `NoRemote` が運ぶのは**診断だけ**で
  （「origin がありません（fork, upstream）」）、「。追跡先を設定してください」は
  `P` の通知が足す（`NoRemote::into_notice`）。モーダルの本文に足すと、**その操作を
  今まさに提供しているモーダルの中で、外でやれと言う**ことになる —— `.claude/rules/rust-coding.md` の
  「案内が求める操作を自分が持っているかを数える」の裏返し（`/code-review` が拾った）。
  **`Nobody` には足さない** —— 待つか `git remote add` するかで、こちらから出せる
  「次に何をするか」が無く、「追跡先を設定してください」はリモートが 1 本も無い相手には
  嘘になる
- **本文はすぐ下に並ぶリモート名を繰り返す。** `origin` が無い形の診断は名前を並べる
  ため。`P` の通知（一覧が出ない場所）が同じ診断を使うので、分けると文言が 2 本になる。
  **繰り返しのほうを取った**（`/simplify` の 2 人が逆を向き、実害の小さい側を採った）

**`-u` の提案（`propose_upstream`）は `default_remote` に畳まない。** git 自身が
fetch と push で違う規則を持っているため。理由は 04 の「既定リモート」の末尾にある。
ただし「1 本ならそれ、複数なら `origin`」の枝は共有する（`conventional_remote`）——
同じ規則を 2 本書かないため。push の規則の前の 2 段（`Branch.push_remote`）は
`push_plan` が先に見る（下の「push の行き先」の 2）。

### `RepoInfo.target` を埋める

`RepoInfo.target` は `RepoInfoLoaded` の中では決まらない。**`ssh -G` が非同期で、
リーダースレッド（git2）の外にある**ため。1 往復増やす。

```
Msg::RepoInfoLoaded → update が default_remote で URL を選び RemoteUrl::parse
  → Cmd::ResolveTarget(RemoteUrl)   （ssh なら ssh -G、https ならそのまま）
  → Msg::TargetResolved { .. }      → RepoInfo.target = Some(..)
```

- **`RepoInfoLoaded` が `Err` のときは頼まない。** 失敗の直後は `remotes` が前のまま
  （起動時なら空）なので、そこで決めると**読めなかっただけの一回**を「決められな
  かった」として latch し、隣の行が出す読み直しが成功しても `Unasked` ではなくなって
  いて二度と頼まない。`TargetState::Unavailable` は**読めなかったこと**ではない
- **`Cmd::ResolveTarget` は `RemoteUrl` を運ぶ**（`Remote` の生 URL ではない）。
  解析は純粋関数なので `update` の中で済み、**読めない URL のときに `Cmd` を
  出さずに済む**（`InvalidRemoteUrl` はそのまま通知にする）
- **https でも `Cmd` を通す。** ssh -G が要らないぶん往復は無駄だが、
  **`target` を書く場所を 1 つに保つ**ほうが強い（2 本にすると、片方だけ
  `valid_host` を通す・片方だけ通知を出す、が起きる。#18 の「共有は半分だけに
  なりやすい」）
- **`ssh -G` が失敗しても `target` は埋まる。** `host_raw` を使い、理由を通知に
  出す（04）。`Msg` は `Result` ではなく**解決済みの `RepoTarget` と
  「正規化できなかった理由」の `Option<AuthError>`**（`auth::ResolvedTarget`）を運ぶ
  —— `Result` にすると、呼び出し側が `Err` で `target` を落としてしまい、04 が用意した
  「degraded ではなく正規化だけを諦める」経路が消える。**`Box` に載せる**のは、`Msg` の
  サイズが最大変種で決まり、1 打鍵ごとに作られる `Msg::Key` まで大きくなるため
  （`msg_stays_small` が 128 バイトで見張っている）

**読み手は 2 人。** 1 人目は**ヘッダ**で、決まっていれば
`mewgit  yshrsmz/mewgit` のように `owner/repo` を並べる（07）。2 人目は
`Cmd::ResolveAccount`（04 の「タスクを 2 本に割った」）で、**`target_resolved` が
そのまま出す**。

**`Cmd::ResolveAccount` を `init` からは出さない。**
`auth::resolve` は `RepoTarget` を要るので、起動時にはまだ材料が無い ——
`Msg::RepoInfoLoaded` → `Cmd::ResolveTarget` → `Msg::TargetResolved` と 2 往復
待ってからでないと出せない。`Cmd` に `RepoTarget` を載せてあるのはこのため
（下の「Cmd」）。

**ここは最初の発行元で、唯一ではない。** もう一方は `resolve_again`（下の
「アカウントの選び直し」）で、`GitOp::SetAccount` / `UnsetAccount` が終わったあとの
`exec_finished` と `Modal::AccountPicker` の `r` がそこへ入る。**どれも
`.git/config` を書いたあと、あるいは外の世界が変わったあとで、材料はもう
`state.repo.target` にある** —— `RepoTarget` は latch されるので、2 度目も同じ対象を
渡す。**一覧はここに書かない**（`Cmd::ResolveAccount` で grep すれば出る）。

**`TargetState::Unavailable` に落ちたリポジトリでは一度も出さない。** GitHub の
リポジトリとして特定できないので解決する対象が無く、`account` は `Resolving` の
ままになる。ヘッダは `Resolving` のあいだ `[gh: …]` を出さないので（07）、
GitHub と関係のないリポジトリで degraded の理由が並ぶことはない。

### アカウントの選び直し

`A`（共通キー）で [`Modal::AccountPicker`](07) を開く。**`f` / `p` / `P` と同じく
フォーカスの `match` より先に見る** —— アカウントはリポジトリ単位の話で、どのペインに
居るかと関係が無い。ペイン専用にすると「案内にキーを書いたら、その案内が出ている画面で
そのキーが効くか」で詰む（#53）。

**開くのは `Degraded` と `Resolved` の 2 つ。** 通知に落ちるのは `Resolving` だけで、
あれは待てば変わるので出す中身が 1 行しかない。

| `state.account` | `A` |
|---|---|
| `Degraded(_)` | モーダルを開く（内訳を出し、`Ambiguous` なら選ばせる。`r` で解決し直す、`ConfiguredUnavailable` が `.git/config` を指すなら `u` で解除も） |
| `Resolved(_)` | モーダルを開く（選び直し。`GitConfig` なら `u` で解除も） |
| `Resolving` かつ `target` が `Unavailable` | 通知「GitHub のリポジトリとして特定できません」 |
| `Resolving`（それ以外） | 通知「アカウントを解決しています」 |

**`Resolved` を出どころで割らない。** `Config` 由来のときだけ開く形にすると、
`Single` / `Probe` で決まった相手は mewgit から変えられないまま残る。塞ぎたいのは
「保存した指定が外せない」ではなく**「mewgit が選んだアカウントを mewgit から
変えられない」**のほうで、出どころで変わるのは**外せるかどうか**だけ（下の `u`）。
腕を分けると、分けた意味を持たない `Single` / `Probe` に行き止まりが残る。

**これが 04 の送った「`AccountState::Resolving` を割るか」の答え。割らない。**
`Resolving` が兼ねている 2 つ（走っている最中 / 永久に頼まれない）を見分けるのは
`state.repo.target` で、あちらは**まさにその理由で 3 値**（`Unasked` / `Resolved` /
`Unavailable`）にしてある。割ると同じ事実が 2 箇所に出て、`TargetState::Unavailable`
を立てた側と `AccountState` を割った側が食い違いうる。

**モーダルは `Modal::AccountPicker { view, cursor }`。** `view` が 2 つの中身を割る。

- `AccountView::Degraded(DegradedReason)` —— **理由そのものを持つ。** 選べる候補
  （`Ambiguous` の `candidates`）も内訳（`NoAccess` の `attempts`、`Unusable` の
  `message`）もそこから引ける。**`Vec<GhLogin>` を写さない** —— 写すと同じ候補が
  `state.account` と `state.modal` の 2 箇所にでき、`ui` と `update` がどちらを読むかで
  答えが割れうる（「導出できる値を引数に取らせない」）。**ここも写しが古くなりうる**
  —— `Enter` で保存したあと、`git` が走っているあいだに `A` を押せば、消える運命の
  `DegradedReason` を持ったモーダルが開き直せる（`Repick` と同じ経路）。下の
  「写しの寿命」が同じ 1 箇所で両方を閉じる。**その開き直した画面では `r` も
  押せる**ので、あちらは別に断る（下の「外で直したあとの再解決」）。**かつて「開いているあいだ
  `state.account` は動かない」と書いていたが嘘で、`Repick` 側だけ直して
  ここに前提が生き残っていた**（独立したレビューが拾った。#54 の「節をまたぐと、
  直した節の隣で前提が生き残る」）
- `AccountView::Repick(Repick)` —— いまのアカウント（`login` / `host` / `source`）と、
  取り直した候補（下）。候補の `AccountListing` は `Waiting` /
  `Listed(Vec<GhLogin>)` / `Failed(String)` の 3 値。
  **`login` / `host` / `source` は `AppState::account` の写し** —— `ui` のモーダル
  描画は `Modal` しか受け取らないので、ここに無い値は画面に出せない
  （`Modal::RemotePicker` が `remotes` を写しているのと同じ形）。**写しが古くなる
  瞬間は在るので、下の「写しの寿命」で切る**

**選べる候補は `view` の 1 本の述語から出す**（`AccountView::candidates`）。`Degraded`
なら `DegradedReason::candidates`、`Repick` なら `Listed` のときだけ中身で、残りは空。
読み手は `ui`（行とタイトルとキーの案内）、カーソル移動、`Enter` の確定の 3 人。

**`Enter` で選べるのは、その述語が空でないときだけ。** `NoAccess` は理由を並べるが
選ばせない（どれも動かない状態でアカウントを選んでも何も直らない。04 の手順 4）。
`Waiting` と `Failed` も同じ扱いで、`Enter` は何もしない —— キーの案内から
`[j/k]` と `[Enter]` が落ちる（残るのは、その画面で効くほかのキー ——
degraded なら `[r]`、`.git/config` 由来なら `[u]`、どちらも無ければ `[Esc]` だけ）。

#### 候補の取り直し

`Resolved` で `A` を押したとき、**候補は state のどこにも無い** —— `Ambiguous` が
運んでいた `Vec<GhLogin>` は `Resolved` には無く、`Account` は決まった 1 つしか
持たない。`gh auth status` をもう一度叩く（`Cmd::ListAccounts(host)` →
`Msg::AccountsListed`）。

- **モーダルを先に開く。** `gh auth status` は 0.95 秒かかる（#61 の実測）ので、
  返信を待ってから開くと `A` が 1 秒無反応になる。先に開いて `Waiting` を出し、
  返信で差し替える
- **運ぶのはホスト名だけ。** `Backend::logins` が要るのは `host` で、`RepoTarget` を
  渡すと `owner` / `repo` を読まない引数が付く（「導出できる値を引数に取らせない」）
- **取り直した候補は probe しない。** `resolve` の手順 4 と違って断られる login も
  並ぶ —— ここで選ぶのは**上書きの指定**で、mewgit が絞り込んだ結果ではない。probe を
  通すと候補ごとに 1 往復増えるうえ、「403 で断られたが SSO を承認すれば通る」相手を
  選べなくなる。**gh の検証結果（`LoginState`）は行に出す**ので、選ぶ材料はある
- **`gh auth status` の失敗を `DegradedReason` に畳まない。** 畳むと `resolve` の外に
  `AccountState` の産出元がもう 1 つでき、04 が「degraded の理由を作るのは `resolve`
  だけ」と決めた境目が消える（`Backend` の TLS 初期化を `Unusable` に落とさないのと
  同じ数え方）。`Failed(String)` として本文に出すだけにして、`state.account` は動かさない。
  **`AuthError` のまま持たない**のも同じ形 —— 変種で分岐する読み手が UI に無く
  （直し方の違いは `Display` に出ている）、`DegradedReason::Unusable` が
  「3 つを分けない」と決めたのと同じ理由になる。**代わりに次の一手を本文に書く**
  （`Esc` で閉じてから `A` で引き直す）—— 引き直しは mewgit が持っている操作なので、
  行き止まりにしない（#65）。**`A` はモーダル表示中には効かない**ので、閉じてからと
  書く（#53）
- **返信を採るのは、モーダルがまだ `Repick` のときだけ。** 0.95 秒のあいだに
  `Esc` で閉じられるし、別のモーダルも開く。**もう一度 `A` を押された形も通る** ——
  2 度目の `Cmd` は `AppState::issue` が落とすので、遅れて届いた 1 通が**新しいほうの
  モーダル**を埋める。だから見るのは**モーダルの形**であって「自分が頼んだか」ではない。
  **「もう埋まっていないか」は見ない** —— この `Cmd` を出すのは `A` の 1 箇所だけで、
  そこは必ず `Waiting` を置く。踏めない状態を見張るコードは置かない

選んだ login は `.git/config` の `mewgit.user` に書く（`GitOp::SetAccount`。03）。
`u` で外す（`GitOp::UnsetAccount`）。

- **`u` が効くのは `AccountSource::GitConfig` のときだけ。** `[[repos]]` は mewgit の
  設定ファイルにあり、`git config --unset` では外せない（08 が「`config` モジュールは
  `.git/config` を見ない」と割った境目の裏側）。**キーの案内も同じ述語から出す** ——
  別々に見ると、効かない画面に `[u]` が出る（#53）
- **`RepoRule` でも選び直しはできる。** 手順 1 の優先順は `.git/config` >
  `[[repos]]` なので、書けば勝つ。本文には `[[repos]]` を mewgit からは外せないことを
  出す —— 外での直し方を書いてよいのは、**その操作を持たない側**だから（#65）
- **確認モーダルは挟まない**（`Enter` も `u` も）。不変ルール 5 の破壊的操作ではなく、
  書き換わるのは `mewgit.user` の 1 行だけ。**ただし書き込み先は本文に出す** ——
  「`.git/config` を黙って変えない」（上の「決まらないときの逃げ道」）は
  `f` が追跡設定を書かない理由でもあり、**書くなら行き先を見せる**がその裏返し
- **`submit_exec`（1 行入力と同じ確定の経路）を通す。** ほかの `git` が走って
  いるあいだは**モーダルを閉じない**（閉じてから `Cmd` が落とされると、選んだのに
  何も起きない）
- **`account` を動かすときに、開いている `AccountPicker` を閉じる**（`resolve_again`）。
  **「開いているあいだ `account` は動かない」は嘘だった**（`/code-review` が拾った）
  —— `submit_exec` は `git` を走らせる**前に**モーダルを閉じ、`A` には実行中の
  断りが無いので、走っているあいだに**書き込み前の写しを持ったモーダルを開き直せる**。
  閉じないと、外した直後の画面が `[u] 指定を解除` を出したままになり、押すと既に
  無いキーへ `git config --unset` が飛ぶ。**そのとき出るのは argv だけの赤い通知**
  —— `exec_finished` は `Err` を必ず通知に積むが、`exec_advice` は
  `ExecErrorKind::Other` に案内を持たず、**exit 5 は stderr が空**（03 の実測表）
  なので、`ExecError::Failed` の `Display`（= 実行した argv）しか残らない。
  **「通知が出ない」ではなく「理由が出ない」** —— 不変ルール 4 の経路は効いていて、
  効いていないのは案内のほう。**写しを禁じるのではなく、写しの寿命を切る** ——
  `ui` のモーダル描画は `Modal` しか受け取らないので写し自体は要る
- **一覧が 0 件で返ることがある。** `gh auth status` は 0 件を失敗にせず `Ok` で返し、
  空を `NotLoggedIn` に畳むのは `auth::resolve` の仕事なので、`Cmd::ListAccounts` は
  そこを通らない（起動後に外で `gh auth logout` すると踏める）。**保存先だけを出すと、
  選べる行が 1 つも無い画面が「選ぶと保存します」と言う**ので、`NotLoggedIn` の
  degraded と同じ案内（`gh auth login --hostname <host>`）に落とす
- **書き込みのあとは結果に関わらず `Cmd::ResolveAccount` を出し、`account` を
  `Resolving` に戻す。** 成功だけを見ない —— 中断（`ExecError::Canceled`）では
  **書けたかどうか判らない**（#56 の「失敗なら ref は元のまま」が中断では成り立たない、と
  同じ形）。**`UnsetAccount` も同じ** —— 外そうとしたキーが既に無ければ
  `git config --unset` は **exit 5 を stderr 空**で返す（03 の実測表）ので、成否から
  「いま何が設定されているか」は読めない。読み直せば `.git/config` の実物が答えるので、
  どちらも成否で分岐する理由が無い
- 保存後は 04 の手順 1（明示指定）に当たるので `Resolved(source: GitConfig)` になり、
  **以後は probe を通らない**（起動のたびの 1 往復も消える）。外したあとは手順 2 以降へ
  落ちるので、`NotLoggedIn` / `Single` / `Probe` / `Ambiguous` / `NoAccess` のどれかに戻る

#### 外で直したあとの再解決

`Degraded` の画面で `r` を押すと、もう一度 `resolve` の手順 1〜4 を通す
（`resolve_again`）。**`gh auth login` や `gh` の入れ替えは mewgit の外で起きる**ので、
それを知る手掛かりが mewgit には無い —— 押されたときに読み直す以外にない。

- **`Degraded` にだけ置く。理由は 2 つで、どちらも `Repick` の側の事情。**
  1 つ目 —— **候補が在る `Repick` には手順 1〜4 を通す入口が既にある**（`Enter` で
  保存すると書き込みのあとの `resolve_again` を通る）ので、行き止まりではない。
  **候補が空の `Repick`（待ちと `Failed` と 0 件）には、その入口が無い** ——
  あそこで効くのは `Esc` だけで、本文が案内する `A` の押し直しは一覧を引き直すだけ。
  ただし**あの状態でも `account` は `Resolved` のまま**なので GitHub 機能は動いており、
  塞ぐ必要のある行き止まりではない（`Enter` の入口は `mewgit.user` を書く副作用付き
  なので、「解決し直すだけ」の入口とは別物だという点だけは残る。`docs/plan.md` の M6）。
  2 つ目 —— **案内が箱に入らない。** `[u] 指定を解除` と `[r] 解決し直す` と
  選ばせる案内を並べると 74 セルで、箱の内側 57 セルを超える（`modals::WIDTH` は
  端末幅に関わらず 60）。**いま超えていないのは、`[u]` を出す唯一の degraded
  （`ConfiguredUnavailable`）が候補を運ばないから**で、これは 2 つの無関係な述語が
  たまたま重ならないという以上のものではない —— 幅の見張りがその組み合わせを回して
  いる（07）。**`ListAccounts` では塞がらないのと同じ区別を、ここでは持ち出さない**
  —— 候補の取り直しは一覧を引き直すだけで、`Single` で決まった相手が外のログイン
  追加で `Ambiguous` に変わったことは拾えない。**その穴は `docs/plan.md` の M6 へ
  送った**（案内の行を割るか詰めるかを決める必要がある）。
  **効く条件とキーの案内は `AccountView` の 1 本の述語から出す**（#53。
  `is_unsettable` の隣）
- **押したらモーダルを閉じる。** `resolve_again` をそのまま呼ぶ ——
  **`account` を動かすときに開いている `AccountPicker` を閉じる 1 箇所を増やさない**。
  開いたまま「解決しています」に差し替える形にすると、`Repick` で塞いだ
  「書き込み前の写しを持ったモーダル」の罠（上の「写しの寿命」）を degraded 側に
  開け直すことになる。閉じたあとの手掛かりはヘッダで、`Resolving` のあいだ
  `[gh: …]` が消え、決まると戻る（07）
- **ほかの `git` が走っているあいだは断る**（`submit_exec` と同じ `exec_busy`。
  **モーダルは閉じない**）。`r` は**書き込みの最中に `Cmd::ResolveAccount` を
  飛ばせる初めての経路**で、断らないと `Ambiguous` で `Enter` →（`git config` が
  走っているあいだに）`A` → `r` で**書き込み前の `.git/config` を読む解決**が先に
  走る。`exec_finished` の `resolve_again` が出す 2 通目は `AppState::issue` が
  同種として落とすので、**古い結果がそのまま最終になり、選んだ `mewgit.user` が
  黙って無視される**。#51 の「どの順序なら拾えるか」と同じ形 ——
  合図を出した順ではなく、**届く順**で決まる
- **`r` が読み直すのは `gh` と `.git/config` だけ。** mewgit の設定ファイルは起動時に
  固まる（08。`config` は `AppState` が持ち、`Cmd::ResolveAccount` はその写しを使う）
  ので、`general.gh_bin` と `[[repos]]` を外で直しても `r` では拾えない
  （pty で実測）。**その 2 つを案内する本文には、再起動が要ることも書く**（07）——
  書かないと、`[r] 解決し直す` を出した画面が**押しても変わらない直し方**だけを
  案内することになる
- **通知は出さない。** `Enter` / `u` のあとも通知なしで閉じて解決し直しており、
  degraded はヘッダに出る（`account_resolved` が「2 箇所で言わない」と決めたのと
  同じ数え方）。断ったときだけ `exec_busy` が「ほかの git 実行が終わるまで」を出す

### push の行き先

`git push`（引数なし）ではなく `<remote> <src>[:<dst>]` を明示する。force push が
`--force-with-lease=<dst>:<lease>` に remote 側の名前を要ることと、`-u` の提案が
行き先を確認モーダルに出すためで、**どちらも「git が選んだもの」では文面を書けない**。
`src` / `dst` に割って持つ理由は 03 の「argv への変換」（refspec と lease で同じ名前を
別の位置に置くので、1 本の文字列に畳むと lease が壊れる）。

決めるのは `app` の純粋関数。**HEAD のブランチは `BranchState` から取る** ——
行き先の材料（`branch.<name>.merge`、追跡 ref の位置、ahead / behind）を運ぶのは
`Branch` の行だけで、`RepoInfo.head` が持つのは名前と oid と `branch.<name>.remote`
だけ（#22 の「キャッシュした状態から破壊的操作を選ばない」）。**鮮度の差ではない**
—— どちらも ref を動かす書き込みのたびに読み直される（`Reload::Refs` が両方を
同じバッチで出す）。

1. HEAD のブランチが無い（detached / unborn）→ 通知して何も出さない
2. **`Branch.push_remote`（`pushRemote` / `pushDefault`）があり、追跡先と違うなら、
   そこへ同名で送る**（下の表の A〜E、H、K）。`set_upstream: false` で**確認モーダルも
   通さない** —— 素の `git push` と同じで、行き先はユーザーが書いたもの。追跡先と
   同じなら 3 へ進む（`dst` と force は追跡設定から組む。`gh pr checkout` と PR の
   取り出しは `remote` と `pushRemote` に同じ値を書くのでこちら）
   - **追跡設定の経路へ進まない理由。** 進むと `checkout -b e origin/main` +
     `pushRemote=fork` のブランチで **`git push origin e:main`** を確認なしに撃つ ——
     git なら fork の `e` へ行く push が upstream の `main` へ流れ込む
   - **`-u` を付けない。** 付けると `branch.<b>.remote` が push 先に書き換わり、
     以後の `p` がそこから取り込む —— upstream から pull して fork へ push する運用の
     意図を書き換える（2026-10-03 にユーザーと決めた。`/simplify` の altitude が拾った）
   - **force には上げない。** lease（`Branch.upstream` の位置）と `ahead == 0` の判定は
     追跡先に対する値で、行き先が違うと比べる相手がいない。**非 FF で断られても記憶を
     立てない**（`update::remote::pushed_elsewhere`。op の形だけでは追跡設定の push と
     見分けられないので、押したブランチの追跡先と行き先を比べる）。立てて `P` で断ると
     `Push` が出なくなり、push 先のブランチが外で消えても断り続ける（`/code-review` が
     拾った）。**案内も分ける** —— 追跡設定の push 向けの「`p` で取り込むか `P` を
     もう一度」は、`p` が追跡先から取り込み `P` が force に上がらないので、どちらも
     出口にならない。mewgit に出口が無いことを言い、外の git へ送る
   - **値は名前でも URL でもそのまま渡す。`.` と一覧に無い名前は断る**（表の J / L / M。
     `origin` へ落とさない —— ユーザーが書いた行き先とは別のリモートへ送る）。値は
     **文字列のまま追跡先と比べる**ので、名前と URL で同じリポジトリを指していれば 2 の
     経路になる（同名で送るので、`merge` と名前が違うときだけ 3 と行き先がずれる）
   - **`.` の判定（3）より先に見る。** `remote = .` でも git は `pushRemote` へ送る
3. `Branch.upstream_config`（`branch.<name>.remote` / `.merge`）があるなら、それが
   行き先。`set_upstream: false` で、**確認モーダルは通さない** —— 行き先はユーザーが
   書いたもので、推測していない
   - **`Branch.upstream`（短縮名）では決めない。** あちらは remote-tracking ref から
     作るので、`fetch --prune` で ref が消えただけのブランチも `None` になる。そこで
     4 に落ちると、**`P` を押しただけで追跡先が別のリモートへ向け替わる**（02 の
     「ブランチ一覧」がこの `None` の理由を 4 つ数えている）
   - **ローカル名と `merge` のブランチ名が違うときだけ `dst` を埋める**（argv は
     `<local>:<remote 側>`）。
     `push <remote> <local>` が動かすのは remote 側の**同名**の ref なので、`x` が
     `refs/heads/y` を追跡している状態では `origin/x` を新しく作ってしまう。git の
     `push.default = simple`（既定）がこの形を断るのも同じ理由。ブランチ名に `:` は
     使えないので、普通のブランチ名と読み違えられる余地は無い
   - **`refs/heads/` の下に無い `merge` は完全な ref のまま渡す。** 短縮できない名前を
     短縮したことにすると、別の ref を動かす
   - **`remote` が `.`（ローカル追跡）なら断る。** `git branch --set-upstream-to=main
     topic` はこれを書く。渡すと `git push . topic:main` になり、**ネットワークに
     出ないままローカルの `main` を早送りして exit 0 で返る**（git 2.55 で確認）。
     素の `git push` が `push.default = simple` で断る場面で、**こちらが組む refspec が
     その守りを外している** —— 「危ない引数を渡していないか」ではなく「危ない解釈を
     される余地があるか」（04 と同じ数え方）。**4 の `-u` 提案にも落とさない。**
     あちらは `branch.<name>.remote` を上書きするので、ユーザーが書いた `.` が黙って消える
4. 追跡設定も push 先も無いなら `-u` を提案する。リモートは 1 本ならそれ、複数なら
   `origin`、どちらでもなければ通知して断る（git の push の規則の 3 段目 = fetch の規則。
   `conventional_remote`）。**`Modal::Confirm` に行き先を出してから実行する**（不変
   ルール 5 が求める破壊的操作ではないが、行き先を推測しているのはこの経路だけ）

**git の push の規則は `branch.<name>.pushRemote` > `remote.pushDefault` > fetch の
規則**で、追跡設定があっても前の 2 つが勝つ。実測（2026-10-03、git 2.56.0、一時
リポジトリにリモート `origin` / `fork` / `up`。`push.default` は既定の `simple`）:

| | 設定 | `git push` が送った先 | `git push -u` が書く `branch.<b>.remote` |
|---|---|---|---|
| A | 追跡設定無し、`pushRemote=fork` | fork の `a` | `fork` |
| B | 追跡設定無し、`pushDefault=up` | up の `b` | `up` |
| C | `origin/c` を追跡、`pushRemote=fork` | **fork** の `c` | — |
| D | `origin/d` を追跡、`pushDefault=up` | **up** の `d` | — |
| E | `origin/main` を追跡（`checkout -b e origin/main`）、`pushRemote=fork` | **fork の `e`**（`main` ではない） | — |
| H | 追跡設定無し、`pushRemote=fork` + `pushDefault=up` | fork（`pushRemote` が勝つ） | — |
| J | 追跡設定無し、`pushRemote=.` | 何もしない（`Everything up-to-date`、exit 0） | — |
| K | 追跡設定無し、`pushRemote=<URL>` | その URL | **URL**（`gh pr checkout` と同じ形） |
| L / M | `pushRemote` / `pushDefault` が無い名前 | `fatal: 'nosuch' does not appear to be a git repository` | — |

- **`push_remote` は `git_read` が前の 2 段を畳んで運ぶ**（理由は 02 の「ブランチ一覧」）。
  3 段目（fetch の規則）は `conventional_remote` が持つ
- fetch の [`default_remote`](#既定リモート) には畳まない —— **git 自身が fetch と push で
  違う規則を持っている**ので、畳むと fetch の既定へ push することになる（04 の
  「既定リモート」）

**拒否の出口は `push_plan` の中に 1 本。** 「一覧がまだ届いていない」を呼び出し側の
早期 return にすると、同じ「Warn を出して `Cmd::None`」が 2 箇所に割れる。

**「無い」と「まだ届いていない」はブランチ一覧とリモート一覧の両方で区別する。**
`LoadRepoInfo` と `LoadBranches` は別のジョブなので、一覧だけ先に届いた瞬間がある。
`BranchState` には `load` があるが `RepoInfo` には印が無いので、リモート側は
`JobKind::LoadRepoInfo` が走っているかを見る。**空の `Vec` を片方だけ区別すると、
起動直後の一瞬に押した `P` が「リモートが登録されていません」と嘘を言う。**

### force push

**専用のキーは作らない**（07 のキー表に force push の行が無いのはそのため）。
通常の `P` が `ExecErrorKind::NonFastForward` で断られたときだけ、次の `P` が確認モーダル
経由の force push になる。

憶えるのは `AppState.push_rejected` 1 つ。

```rust
pub struct PushRejection {
    pub branch: String,  // 断られたときの HEAD のブランチ名
    pub oid: Oid,        // そのときのローカルの先端
}
```

- **立てるのは `exec_finished` の 1 箇所**、`GitOp::Push { lease: None, .. }` が
  `NonFastForward` で落ちたときだけ。**`NonFastForward` は git が非 FF を表す 3 つの語
  （`(non-fast-forward)` / `(fetch first)` / `(stale info)`）でしか立たない**（03）——
  `rejected` を広く拾うと、hook や保護ブランチの拒否まで force の確認を出す。**force push（`lease: Some`）の失敗では立てない**
  —— lease が古いときの `! [rejected] ... (stale info)` も `rejected` を含むので
  `NonFastForward` に落ち（git 2.55 で確認）、立て直すと同じ古い `lease` で force を
  撃ち続ける輪になる。落とすのは、それ以外の `GitOp::Push` の完了すべて
- **読むのは `P` の 1 箇所。** ブランチ名**と** `oid` の両方がいまの HEAD と一致した
  ときだけ force に上げる。**名前だけで憶えると、`↓n` で断られた直後に `p` で早送り
  しても force の確認が出る**（behind だけのブランチも `! [rejected] (non-fast-forward)`
  で断られる。git 2.55 で確認）。pull / commit / rebase はどれもローカルの先端を動かすので、
  `oid` まで見れば黙って失効する
- **上げるのは追跡設定を読めた経路だけ。** `-u` を提案する経路（`propose_upstream`）は
  追跡 ref を持たないので lease の値が無く、そちらで force を出すと「素の
  `--force-with-lease`」（03 が禁じた形）しか組めない。**なので `-u` の拒否は憶えない** ——
  読む側が使えない印を持つと、あとから追跡設定が外で入ったときに、追跡していない push の
  拒否を根拠に force の腕が立つ。憶えるかどうかは「次を force に上げられる経路か」
  （`upgradable`）の 1 つの問いで、force の失敗を憶えないのと同じ場所で決める
- **`ahead == 0` なら force に上げず pull を勧める。** `↓n` だけのブランチも
  非 FF で断られる（git 2.55 で確認）ので記憶は立つ。そこで `f` を踏むと lease が
  リモートの位置に追いつき、**force が通ってしまう** —— 出すものが 1 つも無いのに
  他人のコミットを消す。**lease は「リモートが動いていないこと」しか守らない**ので、
  fetch のあとは守りにならない。意図的な巻き戻し（`reset --hard HEAD~1` してから
  force）もここで断るが、その経路は M6 の設定へ送る
- **憶えるのは押したブランチ**（`GitOp::Push` の `src`）。一覧の読み直しは別のジョブ
  なので、`Switch` の直後に押した push が返るころには HEAD が別のブランチになって
  いることがある。HEAD で憶えると、押していないブランチに force の腕が立つ

`lease` に渡すのは `Branch.upstream` の位置（02）。**追跡 ref が無ければ force push は断る**
—— 値を作れないなら撃たない。`fetch --prune` のあとがこの形。

**force push は fetch を挟まないと lease に断られる。** 断られた時点の `origin/x` は
他人が進める前の位置なので、そのまま lease に渡すと `stale info` で落ちる（git 2.55 で
確認）。**これは守りが効いている形**で、ユーザーは `f` でリモートの位置を見てから `P` を
押す（**fetch はローカルの先端を動かさないので記憶は生きたまま**。`P` は 1 回でよい）。**手前で fetch を代行しない** —— `f` が別のキーとして在り、fetch は
ラベルとログを動かす（`Reload::RefsAndLog`）ので、`P` の中で黙って走らせると
画面が押した覚えのない形で作り替わる。

確認モーダルには remote / 行き先 / lease の短縮 Oid を出す。**`y` を押した時点で
読み直さない**（上の「確認モーダルを組む時点の値を `GitOp` に閉じ込める」）。

### 失敗の案内文

`git` の失敗は `exec_finished` が通知に積む。**`message` は上から順に決める** ——
(1) 案内文（`ExecErrorKind` と `GitOp` から決める純粋関数 `exec_advice`）、
(2) 案内が無ければ git の出力から選んだ理由の行（`ExecError::reason`。選び方は 03 の
「エラー分類」の「理由の行」）—— stderr があれば git の形の行、stderr が空なら
**stdout の最後の行**、
(3) 選べなくても出力があれば固定の文言「git が断りました。理由は L の履歴で」、
(4) 出力も無ければ `ExecError` の `Display`（`git の実行に失敗しました: git <argv>`）。
`detail` は argv、stderr、stdout の順に**全文**を並べる（空のものは飛ばす）。**stderr が
stdout より前**なのは、履歴のモーダルが入りきらない `detail` を `…` で切る（07）ため ——
長くなりうる stdout（複数の cherry-pick が積んだ行、status の一覧）を前に置くと、stderr の
理由が箱から押し出される。

**置き場所は `app`。** 答えるのが「mewgit のどのキーを押すか」で、`f` / `p` / `P` を
知っているのは `app` だけ（`git_exec` に置くと `moves_refs()` と同じ、層をまたぐ述語に
なる）。分類そのものは `git_exec` のまま（03 のエラー分類）。

| kind / op | 案内 |
|---|---|
| `SamlSsoRequired` | GitHub で鍵を Authorize する |
| `SshAuthFailed` | ssh-agent と鍵の登録を確認する |
| `HttpsAuthFailed` | `gh auth setup-git` を実行する |
| `NonFastForward` + `Push { lease: Some }` | `f` で取得してから `P` を 2 回押す |
| `NonFastForward` + `Push { set_upstream: true }` | Branches で改名する |
| `NonFastForward` + `Push { .. }` | `p` で取り込むか `P` をもう一度 |
| `NonFastForward` + `Pull` | 履歴が分かれている（merge / rebase は M5） |
| `Conflict` + rebase / cherry-pick / revert（開始・continue・skip）/ `pull --rebase` | 解消して `git add` したら `m` で continue する（**`Warn`**） |
| `Empty` + cherry-pick（開始・continue・skip） | `m` から skip する（**`Warn`**） |
| `NoChanges` + revert（開始、1 本だけ） | 取り消す変更が無い（**`Warn`**） |
| `NoChanges` + revert（continue・skip・複数の開始） | `m` から skip する（**`Warn`**） |
| `HeadMoved` + `RebaseInteractive` | 組んだあとに履歴が動いた。`i` から開き直す（**`Warn`**、48 セル） |
| `HeadMoved` + `RebaseContinue { reword: Some }` | 止まったあとに HEAD が動いた。`m` から開き直す（**`Warn`**、48 セル） |
| `Unmerged` + rebase / cherry-pick / revert の continue | 未解消のまま。`git add` したら `m` から continue（**`Warn`**、44 セル） |
| `Unmerged` + `Commit` | 未解消のまま。`git add` してから commit（**`Warn`**、37 セル） |
| `Unmerged` + `StashPush` | コンフリクトを解消してから stash（**`Warn`**。手前の断りと同じ文） |
| `Unmerged` + `StashApply` / `StashPop` | コンフリクトを解消してから stash を戻す（**`Warn`**、41 セル） |
| それ以外 | 無し（git の出力から選んだ理由の行。選べなければ固定の文言か `Display`） |

- **非 FF は op で分ける。** 同じ `(non-fast-forward)` でも、force push が
  `stale info` で断られた形は fetch を勧める側（lease は fetch してからでないと通らない。
  上の「force push」）、`-u` の提案から入った形は force に上げる経路が無い側（force は
  `PushPlan::Tracked` にしか無い）。kind だけで文を選ぶと、どちらかに嘘を言う
- **`f` のあとの `P` は 2 回。** force の失敗では記憶を落とす（上の「force push」）ので、
  1 回目の `P` は素の push になり、もう一度非 FF で断られてから force の確認が出る
  （一時リポジトリで確認）。**「押し直して」と 1 回に読める形で書くと、途中で出る
  「`p` で取り込むか、`P` をもう一度」と食い違う**
- **通常の push には「`P` をもう一度」とだけ書く。** 次の `P` が force の確認を出すかは
  `push_plan` が決めていて（`ahead == 0` と追跡 ref の有無で断る）、ここで先読みすると
  **同じ判定が 2 箇所に割れる**。押した結果が断りでも、出るのは `PushPlan::Refused` の
  正しい文言なので、案内が嘘にはならない
- **`-u` の経路は mewgit の中で行き止まり。** 追跡設定が無いので `p` も通らず
  （`git pull` が「どのブランチか」を決められない）、force にも上げられない。残る
  出口は改名だけ（リモートに同名の ref が無くなり、`-u` の push が通る）。
  「できません」で終える案内は、踏める手が実際に 1 つも無いことを数えてから書く
- **出すのはキーではなくペインの名前。** `r` が効くのは Branches ペインの中だけで
  （07 のキー表）、既定のフォーカスである Graph で押しても何も起きない。しかも `r` は
  **カーソル行**を改名するので、押した本人のブランチとは限らない。**押すキーを案内に
  書くときは、その案内が出ている画面でそのキーが効くかを数える。** グローバルな改名か
  HEAD 起点の改名を入れるかは M6 のキーマップのタスクへ送る
- **`P` は断らない。** `-u` の経路で `push_plan` が記憶を見て断ると、`P` が `Push` を
  二度と出さなくなり、記憶を落とす経路（`exec_finished`）へ入れなくなる。リモート側の
  同名ブランチが外で消えても断り続けることになるので、**代わりに憶えないほうを直した**
  （上の「force push」）
- **端末の幅から時刻の 9 セル（`HH:MM:SS` と空白）を引いた 71 セルに収める**（80 桁の
  端末で）。フッターは 2 行あり、**1 行目は進行中操作のバッジと通知が使う**（キーヒントは
  2 行目。バッジが出ているあいだは通知が短く切れるが、予算は縮めない。07 の
  「ステータスバー」）。溢れると末尾が `…` に置き換わるので、**消えるのは案内の
  「どうすればよいか」の側**。`…` は上限の無い値（パス・ホスト名）を埋めた文言が
  溢れたときに切れたことを見せる受け皿で、**固定の文言を予算に収める理由は消えていない**。
  **見張りは 80 桁で描いて、案内の末尾が見えることを見る**（`contains` だけでは切れても
  通る）。**見張りは複数あって、新しい産出元を自分では拾わない** —— core 側の産出元は
  `test_support::NOTICE_CELLS` で grep すれば出る。**実際に描いて確かめるのは `ui` 側の
  `あ×35` / `あ×36`**（バッジの隣は `あ×24` / `あ×25`）で、**リテラルのまま置いてある**
  —— 予算が変わったときに赤くして数え直させるため。算出に変えない。**予算の経緯** —— 72（同じ行の右端に `q:quit` が居た）→ 80（ヒントを 2 行目へ
  移した）→ 71（時刻を前に添えた。2026-09-26）
- **`Empty` も `Warn` で出す。** cherry-pick の変更が空で止まった形で、`CHERRY_PICK_HEAD` が
  残ってバッジが出る。**continue は同じ理由で断られる**ので、案内するのは skip（03 の
  「continue / skip / abort の終わり方」）。文は「変更が空で止まった。m から skip」
  （31 セル。バッジの隣の 48 に収まる）。**cherry-pick の系統の op だけに出す** —— revert が
  空で止まった形は stderr が空で `NoChanges` に落ちる（下）。rebase で空の pick に当たったときの出力は確かめていない。ほかで出たら
  案内の無い `Display` に落とすほうが嘘を言わない
- **`NoChanges` も `Warn` で出す。** 1 本だけの revert で取り消す変更が無かった形で、git は
  `REVERT_HEAD` を残さず exit 1 で返り、**stderr は空**（03 の「revert の開始」）。案内が
  無いと argv だけの赤い通知になり、理由が見えない。文は「取り消す変更がありません
  （取り消し済み）」。**開始の 1 本だけの revert に限る** —— continue / skip と複数の
  revert は同じ文言で**進行中のまま**止まる（03）ので、「取り消し済み」と言うと進行中の
  操作を隠す。continue / skip と複数の開始には `Empty` と同じ「変更が空で止まった。m から skip」を
  出す（複数の途中が空の形は `sequencer/` だけが残り、continue でも skip でも抜けられる。
  案内は skip に揃える）。**`git commit` も同じ文言を出すが、案内は付けない** —— stderr が
  空なので、要約行には stdout の結び（`no changes added to commit …`）がそのまま出る（下）
- **`Conflict` は `Warn` で出す。** continue / skip のあとに次のコミットでまた止まった
  形で（03 の「continue / skip / abort の終わり方」）、進行中操作は残ってバッジの数が
  進む。**想定どおりの停止を赤で出すと、壊れたように読める。** 案内は「`git add` したら」
  —— **Conflicted の行は mewgit では stage できない**（上の「ワーキングツリー」）ので、
  解消の印は端末で付ける。**op で絞る**のは、merge のコンフリクト（M5 で merge を始める
  経路が入ったとき）を結ぶのは continue ではなく commit だから
- **この文と、進行中操作の断り（下の「進行中操作があるときの書き込み」）は必ずバッジと
  並ぶので、予算は 71 ではなく 48 セル**（`test_support::BESIDE_BADGE_CELLS`）。いちばん
  長いバッジ `CHERRY-PICK abc1234` の隣で切れずに出る幅で、`ui` 側の
  `a_notice_beside_the_longest_badge_is_cut_short_not_pushed_off` が `あ×24` を描いて通す。
  71 に収めただけの 59 セルの文は、80 桁で `CHERRY-PICK … したら m か…` まで削られ、
  **押すキー（`m`）が消えた**（`/code-review` が描いて拾った。その前に pty で撮ったのは
  `REBASE 2/2` の隣だけで、文を縮めたあとに撮り直していなかった）。文は
  「コンフリクト。git add したら m から continue」（44 セル）
- **`HeadMoved` も `Warn` で出す。** `i` で plan を組んだあとに端末でコミットを積むなどして
  HEAD が動き、sequence editor が断った形（03 の「Interactive rebase の実装」）。git は
  rebase を始めずに `rebase-merge/` を消すので、**壊れたものも進行中操作も無い**（`NoChanges`
  と同じ数え方）。失敗のあとは `RefsAndLog` でログが読み直されているので、案内は
  「`i` から開き直して」で足りる。**予算はバッジの隣の 48 セル** —— 端末で cherry-pick /
  revert を止めたまま（`CHERRY_PICK_HEAD` / `sequencer/` が残ったまま）確定すると、git は
  rebase を断らずに sequence editor を起こす（git 2.56.0 で実測）。その cherry-pick が HEAD を
  動かしていればこの kind になり、失敗のあとの status でバッジが出る。**そのときも `i` を
  指したままでよい** —— `i` は開く前に進行中操作で断られ（下の「進行中操作があるときの
  書き込み」）、断りの通知が何の途中かと出口を言う。当初「バッジとは並ばない」と書いて
  71 セルの文にしており、`/code-review` が踏んだ
  **打った reword のメッセージは戻らない** —— エディタは確定で閉じており、開き直すと
  plan は読み直したグラフから組み直される（上の「interactive rebase の開始（`i`）」）。
  文は「組んだあとに履歴が動いた。i から開き直して」（43 セル）
  **reword を持つ continue の照合が外れた形も同じ kind**（03 の「Interactive rebase の
  実装」）。こちらは rebase が止まったままなので必ずバッジと並ぶ。失敗のあとの status が
  見分け直すので、**開き直したメニューはいまの HEAD で組まれる**（端末で amend し直して
  いれば素の continue）。文は「止まったあとに HEAD が動いた。m から開き直して」（46 セル）
- **stderr が空の失敗は stdout の最後の行を要約にする**（2026-10-03）。git は理由を
  stdout にだけ書くことがあり（何も stage していない `git commit`、未解消のままの
  `rebase --continue`、複数の revert が空で止まった形。03 の「エラー分類」）、
  `Display` に落とすと **argv だけの赤い通知になって理由が画面に出なかった**。
  **最後の行**なのは、git の stdout が status の一覧 → 結びの順に並ぶから。
  英語のまま出す —— 訳すと文言の数だけ表が要り、表に無い文言では結局 argv に戻る。
  71 セルを超えれば `…` で切れるが、`detail` に全文がある。
  **stderr があれば stdout は使わない**（stdout は進捗や status の一覧。03 の
  「理由の行」）。
- **stderr にだけ理由がある失敗は、stderr から理由の行を 1 本選ぶ**（2026-10-03）。
  いちばんよく出る赤い通知（`switch` の `would be overwritten`、`branch -d` の
  `not fully merged`、`add` の `pathspec did not match`）が argv だけになり、理由は `L`
  でしか読めなかった。**選ぶのは `git_exec`**（`ExecError::reason`。規則と op ごとの
  実測は 03 の「エラー分類」）—— 「どの行が理由か」は git の出力の形の問いで、
  `classify` と同じ知識（行頭の形、写されたコミットの行）を使う。`app` が持つのは
  「案内が無ければそれを要約にする」ことだけ。**`error:` / `fatal:` の接頭辞は剥ぐ** ——
  通知は既に赤く（`Error`）、`would be overwritten by checkout:` の行は接頭辞込みで
  82 セル・剥いで 75 セル（71 を超えるのはどちらも同じだが、剥ぐと `by chec…` まで残る）。
  **英語のまま出す**のは stdout と同じ理由。
  **`L` でしか読めないものは残る**: `would be overwritten …:` の行は末尾が `:` で、
  **どのファイルかは次の行**にある。`branch -d` の `-D` の案内も `hint:` なので要約に
  出ない（07 の「Branches ペイン」）
- **git の形をしていない出力しか無い失敗は、行を選ばずに `L` を案内する**（2026-10-03 に
  ユーザーと決めた）。hook の文言（lefthook の出力は 1 行目が枠、最後が所要時間）、
  ssh の行、リモートの文言がここに来る。文は「git が断りました。理由は L の履歴で」
  （35 セル）。lazygit の commit と同じ考え方（03 の「理由の行」に他の TUI の実測）。
  **出力が空なら案内しない** —— `L` にも argv しか無いので、`Display` に落とす
  （`git config --unset` の exit 5 など。上の「候補の取り直し」）。`L` はどのペインでも効く
  共通キー（07 のキー表）。失敗の出力は端末の制御シーケンスを剥いで届く（03）ので、
  `L` の履歴に色の残骸は出ない
- **`Unmerged` は「解消してから」を案内する**（2026-10-03）。未解消のファイルがあると、
  stash の 3 つは stdout に `<path>: needs merge`・stderr に `could not write index` だけを
  書き、`rebase --continue` は stdout に `needs merge` と結びを書いて **stderr は空**、
  cherry-pick / revert の continue と `commit` は stderr に `Committing is not possible
  because you have unmerged files` を書く（03 の「エラー分類」）。**入口で文言が割れても
  同じ状態なので、案内も揃える** —— rebase だけ案内が出て cherry-pick は赤い argv、では
  同じ `m` を押した結果が食い違う。stash の push は鮮度のある snapshot なら手前で断る
  （上の「stash の書き込み」）ので、ここに来るのは外で作った unmerged を読む前だけ。
  **`Warn`** —— 何も書き込まれず、解消すれば済む。continue の文は進行中操作のバッジと
  並ぶので 48 セルに収める
- **argv は画面から退く。** これまでステータスバーに出ていた `git <argv>` は
  `detail` の 1 行目に移り、`L` の履歴（上の「通知」）で読む。**1 行目を 2 つで分けない**
  —— 案内文は端末の幅いっぱいまで使う前提で書いてある（上）ので、argv を前に置くと
  「どうすればよいか」の側が末尾から `…` に削られる

## 進行中操作があるときの書き込み

どの `GitOp` を断るかは 03 の「進行中に通す書き込み」（実測の表がある）。ここは `app` が
**いつ・どこで**断るか。

**判定は `update::progress::refusal` の 1 本**（網羅 `match` で、`GitOp` に変種を足したら
断るか通すかを選ばせる）。**置き場所は `app`** —— 答えに「どのキーを押すか」が入り、
「HEAD を動かすか」は git の事実でも、どこまでを断るかは UI の方針（`Reload` と同じ
数え方。`.claude/rules/rust-coding.md` の「層をまたぐ述語」）。

**信じるのは鮮度のある snapshot だけ**（`AppState::worktree_is_current()` が真のとき）。
`c` の「stage された変更がありません」と同じ門で、**古いときは断らずに git に任せる**。
書き込みの直後は status が届くまで古く、そこで持っている進行中操作を信じると、abort した
直後の `Enter` を「rebase の途中です」で断る。逆向き（まだ `None` なのに実は始まっている）は
git が自分で断る —— 03 の表で断らないのは stash だけで、**stash の push は古い
snapshot でも断る**（上の「stash の書き込み」）。apply / pop は解消済みの途中でも git が
受けるが `*_HEAD` を消さない（03 の表）ので、古いあいだは git に任せる。

**3 つ目の向き —— 外で終えたのに「鮮度あり・途中」のまま残る。** 世代を進めるのは mewgit の
書き込みだけなので、端末で `git rebase --continue` / `--abort` しても snapshot は古く
ならない。**mewgit から始められるのは cherry-pick だけで、rebase / revert / merge / am は
必ず外で始まる**ので普通に踏む。
断ると git を起動しないぶん読み直しも起きず、断り続ける（`/code-review` が拾い、pty で
再現した）。**断ったら代わりに `Cmd::LoadStatus` を出す** —— 1 回目は断られるが、外で
終わっていれば 2 回目は通る。**1 回目も通るのは、押す前に probe が拾ったとき**（gitdir と
`index` の差。最大 2 秒。下の「外部の変更の検知」）。
**監視は `.git/HEAD` だけでは足りない**（実測 2026-09-27、git 2.55.0。コンフリクトで止めて
から外で終え、前後の mtime を比べた）——

| 外での終え方 | `HEAD` | `index` | gitdir |
|---|---|---|---|
| rebase → `--abort` / 解消して `--continue` | 変わる | 変わる | 変わる |
| cherry-pick → `--abort` / `--continue` / 解消して `commit` | **変わらない** | 変わる | 変わる |
| merge → `--abort` / 解消して `commit` | **変わらない** | 変わる | 変わる |
| revert → `--abort` | **変わらない** | 変わる | 変わる |

ブランチに居たまま終わる操作は HEAD ファイルを書き換えない。gitdir のディレクトリの mtime は
`MERGE_HEAD` などの印ができたり消えたりすると動く。
**git の実行中に断ったとき**は、この読み直しが書き込みより前の世代で刻まれ、書き込みの
あとに `exec_finished` が出す `LoadStatus` のほうが同種実行中で落とされる。古い返りは
`status_loaded` の追いつき（届いた刻印が古ければ頼み直す）が拾う

**断るのは `AppState::issue`**（`update` の出口）。同種のジョブが走っているときに落とすのと
同じ `AppState::refusal` の中で、**進行中操作を先に診る**（git の実行が終わっても断られる
ので、待てと言うより出口を言うほうが先に要る）。`Cmd::Exec` を返す経路はキー・確認
モーダルの `on_yes`・入力の確定と多く、経路ごとに書くと足し忘れが静かに通る。

**打った文字を守るために、同じ判定を前倒しで問う場所が 2 つある。**

- **`submit`**（入力モーダルの確定）は `AppState::refusal` を呼び、断られるなら閉じずに
  断る。出口で断るとモーダルはもう閉じていて、打った名前が消える
- **`Modal::Input` を開く入口**（`update::branches::input`）は、確定で出す `GitOp`
  （`InputIntent::op` に空の名前）を開く前に問う。**名前を打ち終えてから断られない**。
  確定と同じ `GitOp` から判じるので、開き手ごとに「断るか」を数えない（`r` の改名は
  通る操作なので開く。**ただし rebase 中の対象ブランチの改名は開く前に断る** ——
  git が `is being rebased` で断るので、開くと打った名前が消える。どのブランチかは
  `InProgressOp::Rebase.branch`（02）。削除（`d` / `D`）も同じ判定で断る。03 の表）

**通知の文言は進行中操作の種類で決まる**（**バッジの隣の 48 セル**の予算。上の「失敗の案内文」）。何の途中かと、
出口を言う —— rebase / cherry-pick / revert は「`m` から continue か abort」、merge は
「commit で結ぶか `m` から abort」、am は端末。rebase 中のブランチの改名・削除は
「rebase 中は改名・削除できません。m から終えて」。**`m` を案内するのはここだけ**（下の
「進行中操作のメニュー（`m`）」）。

### 進行中操作のメニュー（`m`）

**`m` で開く `Modal::ProgressMenu` に、進行中の操作で打てる手を並べる**（lazygit と同じ
キー）。**専用の「rebase モード」は持たない** —— 止まっていることはバッジが出し、断る
書き込みは上の `refusal` が断るので、モードで分けるものが残っていない。

| 進行中操作 | 項目 |
|---|---|
| `Rebase` | continue / skip / abort（**hook が reword を断った形は説明と continue の中身が変わる**。下） |
| `CherryPick` | continue / skip / abort |
| `Revert` | continue / skip / abort |
| `Merge` | abort（結ぶのは `c` の commit。03 の表） |
| `Am` | **開かない**。端末で `git am --continue` / `--abort` を案内する |
| 無い | **開かない**。通知を出して `Cmd::LoadStatus` を返す |

- **共通キー**（どのペインでも効く。`L` と同じ段）。押す場面はコンフリクトの解消の途中で、
  どのペインに居るかは決まっていない
- **項目は `(語, 説明, GitOp)` の列。** 何を出すかは `update::progress::progress_menu` の
  網羅 `match` 1 本で決め、**開いた時点で組んだものをモーダルが持つ**（開いているあいだに
  status が届いても、カーソルの下の項目が動かない）。確定は `GitOp` をそのまま出す。**abort は `Modal::Confirm` を挟む**
  （不変ルール 5）。**skip は挟まない** —— 止まったコミットを捨てるが、`CLAUDE.md` の
  確認対象（force push、`branch -D`、discard、abort 系）に入っておらず、lazygit も挟まない。
  確認するかは項目に持たせず `GitOp` から導く（`is_abort`。持たせると同じ判断が 2 つになる）
- **確定は `submit_exec`**（1 行入力と同じ）。git が走っていれば閉じずに断る
- **hook が reword を断って止まった rebase**（`Rebase.refused_reword` が `Some`。02 の
  「断られた reword」）では、continue が `GitOp::RebaseContinue { reword: Some(..) }` を出し、
  新しいメッセージを amend してから進める（03 の「Interactive rebase の実装」）。素の continue
  では git が元のメッセージのまま進むので、**同じ「continue」の語で別の中身**になる。本文と
  説明もこの形のときだけ変える —— 本文は「新しいメッセージが断られ、元のメッセージで
  止まっています」、continue は「新しいメッセージを当て直して続ける」、skip は「元の
  メッセージのまま続ける」。**skip はこの形では止まったコミットを捨てない**（pick は
  コミット済み。03 の表）ので、いつもの「止まったコミットを捨てて続ける」は嘘になる
- **開くときの snapshot の鮮度は問わない。** 古い snapshot の `Rebase` で開いて、実は
  終わっていれば git が `No rebase in progress?` で断り、`exec_finished` の `LoadStatus` が
  バッジを直す。**無いときに `LoadStatus` を返すのは、mewgit から始められない進行中操作が
  あるから** —— 端末で rebase を始めた直後に `m` を押すのがいちばんよく踏む形で、snapshot は
  まだ「無い」。1 回目は通知で終わるが、2 回目は開く（上の「3 つ目の向き」と同じ逃げ方）
- **フッターの 2 行目には出さない**（2026-09-27 にユーザーと決めた）。`m` が要るのは進行中の
  あいだだけで、そのときは断りの通知かバッジが必ず目に入る。いまは共通キー自体をフッターに
  出さない（07 の「ステータスバー」）。**`?` の一覧（`Modal::Help`）には載せた**

## 実行中ジョブ

`JobKind` は `Cmd` の変種と 1 対 1 なので、列挙を二重に持たず `Cmd` を単一の真実にする。

```rust
impl Cmd {
    /// この `Cmd` が生む非同期ジョブ —— 種別と、スピナーに添える説明。
    /// 副作用の無い `Cmd`（`None` / `Batch` / `Quit` / `CancelExec`）は `None`。
    pub fn job(&self) -> Option<(JobKind, &'static str)>;
    pub fn job_kind(&self) -> Option<JobKind>;   // job() の前半
}

impl AppState {
    /// `update` が返す `Cmd` を発行してよい形にする。同種が走っていれば `Cmd::None`、
    /// 進行中操作で断る書き込みなら `Cmd::LoadStatus`（下の「進行中操作があるときの書き込み」）。
    pub fn issue(&mut self, cmd: Cmd) -> Cmd;
    /// `issue` が断るなら、理由を通知に積んで代わりに出す `Cmd` を返す。`submit` も同じ関数で問う。
    pub(crate) fn refusal(&mut self, cmd: &Cmd) -> Option<Cmd>;
}
```

網羅 `match` で書くので、`Cmd` に変種を足したら実装漏れがコンパイルエラーになる。

登録は `update` の出口（`issue`）で行う。ランタイムの `dispatch` が `AppState` に
書いてよいのは `notices` だけなので、そちらでは登録できない。`Batch` は中身を 1 つずつ
判定する。まとめて弾くと、同種が走っている `Cmd` 1 つのために他の `Cmd` まで落ちる。

### スピナー

**走っているものがあるあいだ、ヘッダの右端に駒と説明を出す**（`⠋ fetch`。07 の
ヘッダ）。駒を進めるのは `Msg::Tick`（250ms）で、`AppState.tick` を 1 つ増やす（`Tick` が運ぶ
時刻は `AppState.now` に写すだけで、駒の選択には使わない。07 のヘッダ）。
`ui` はその剰余で駒を選ぶ。

- **時刻ではなく回数を持つ。** `ui` は `AppState` を描くだけ（不変ルール 2）なので、
  駒を時刻から決めると描画のたびに時計を読むことになり、`TestBackend` で「回った」ことを
  見張れない
- **説明は種別と一緒に `Cmd::job()` が返す。** `Exec` だけは走らせる `GitOp` で変わり
  （`GitOp::verb()`）、ほかは変種ごとの固定語。**1 つの網羅 `match` から出す** ——
  別々の関数に割ると、ペイロードで説明が変わる変種を足したときに説明の側だけ足りない形が
  組める（受け皿の `_` があるとコンパイルも通り、スピナーが静的な語のまま黙る）。
  `verb()` は `to_commands` が返す列のうち時間を使う 1 本の `argv[0]` と同じ語を返す（見張りは
  `git_exec::argv` の `verb_matches_the_argv`。03 の「複数の手順を持つ書き込み」）—— スピナーに
  `git` とだけ出しても、何が走っているかは判らない
- **ステータスバーではなくヘッダに出す。** 07 は当初こちらをステータスバーに置いて
  いたが、フッターの 1 行目は通知が幅いっぱい使い（下の「失敗の案内文」）、2 行目は
  キーヒントが埋める（07 の「ステータスバー」）。**2 行になっても譲る席は増えていない**
  —— ジョブが 1 つ増えるたびに案内かヒントが末尾から削られる

### キャンセル

**中断できるのは `Cmd::Exec` だけ。** 読み取りの future は途中で捨てられない
（`log_page` を捨てるとそのページのコミットがどこにも現れなくなる。02 の「スレッド」）。

- **`Ctrl-C` は `Exec` が走っているときだけ中断、それ以外は終了**（下の「終了経路」）。
  「ジョブがあれば中断」にすると、スクロール中はほぼ常に `LoadLog` が走っているので
  終了できない瞬間ができる
- **中断は新しいジョブではない。** `Cmd::CancelExec` は `job_kind()` が `None`。
  登録すると、`Exec` が終わったときに誰も畳まないものが `JobTracker` に残る
- **タスクを外から落とさない**（`JoinHandle::abort` を使わない）。落とすと
  `Msg::ExecFinished` が誰からも来ず、`JobKind::Exec` が走ったまま残って以後の書き込みが
  全部「ほかの git 実行が終わるまで待ってください」で落ちる。**合図を受けたタスク自身が、
  中断した結果を `Msg` にして返す**（ランタイムが `oneshot` の送り手を持つ）
- **殺し方はプロセスグループ宛ての SIGTERM**（03 の「キャンセル」）。`ExecError::Canceled`
  で返り、通知は `Error` ではなく `Info`。押した本人が止めたものを `Error` で出すと、
  頼んでいないことが壊れたように読める
- **中断した push は force を arm しない。** 憶えるのは git が非 FF で断ったときだけ
  （上の「force push」）で、中断は git の判断ではない
- **中断を頼んだかどうかは `app` が持つ**（`JobTracker::request_cancel`。刻みは `Job` に
  付き、ジョブが畳まれると一緒に消える）。**走っているかどうかで決めない** —— git が
  SIGTERM で死なないあいだ `Ctrl-C` が中断を返し続け、**モーダルを開いていると脱出口が
  1 つも無くなる**（`q` は入力文字）。2 回目の `Ctrl-C` は終了になる
- **頼んだ時点で「2 回目が何をするか」を通知に出す。** 頼んだ直後は画面が何も変わらない
  ので、出さないと効いていないように見える（上の「失敗の案内文」と同じ方針）
- ランタイムの `Cmd::CancelExec` は**合図を撃つだけ**。方針を持つのは `app` の側で、
  `dispatch` が書くのは `notices` だけ（下の「ランタイム」の規則どおり。1 回目で
  終われなかったときの案内をここから出す）

### 世代

**粒度が種別単位であることの影響は 2 つあり、別々の問題として扱う。**

| | 問題 | 解 |
|---|---|---|
| (a) | 同種が走っていると**要求が落ちる** | 返信が届いた場所で必ず頼み直す |
| (b) | 書き込みで**持っている値が古くなる** | 世代（epoch） |

**世代が解くのは (b) だけ。** (a) を解くには `JobKind` が対象（`DiffTarget` など）まで
持つ必要があり、それは別の変更。**ただし追いつき（(a)）の判定材料は世代が与える** ——
届いた返信が書き込みより前に頼まれたものかは、刻印を比べれば判る。

```rust
pub enum Region { Worktree, Refs, Log, Stash }

pub struct JobTracker {
    running: Vec<Job>,
    epochs: [u64; Region::ALL.len()],
}

impl JobTracker {
    /// 書き込みが 1 つ終わり、この領域の値が古くなった。
    pub fn advance(&mut self, region: Region);
    /// この領域のいまの世代。
    pub fn epoch(&self, region: Region) -> u64;
    /// 完了を記録し、**そのジョブを頼んだ時点の世代**を返す（走っていなければ `None`）。
    pub fn finish(&mut self, kind: JobKind) -> Option<u64>;
    /// `asked` の刻印が、`kind` が読む領域のいまの世代と同じか。
    pub fn is_fresh(&self, kind: JobKind, asked: Option<u64>) -> bool;
}

impl JobKind {
    /// この種別が読む値が、どの領域の世代で古くなるか。
    pub fn region(self) -> Option<Region>;
}
```

- **カウンタは領域ごとに 1 本。** 1 本にまとめると、index しか触らない `stage` でもログの
  世代が進み、飛んでいる `LoadLog` を毎回捨てて `LogSession` の作り直し（履歴全体を
  辿り直す。02）を払う。**どの書き込みがどこを古くするかは `update::Reload` が既に
  持っている**ので、その値を領域の集合に写す。どれも `Worktree` を含む（書き込みは
  必ず index / ワークツリーを古くする。`Account` だけは何も含まない）。**`Stash` は
  `refs/stash` の一覧だけ**で、`Refs` とは別に持つ（上の「Stash ペイン」）。
- **刻むのは `JobTracker::start`**（= `AppState::issue` の中）。`Job.epoch` は非公開で、
  組み立てる側は入れられない。入れられる形にすると、刻む場所が `start` の外に増える
- **進めるのは `exec_finished` と外部の変更の検知（下の節）。`Cmd` を返す前に進める。** `issue` は `update` の出口
  なので、返してからでは遅い。逆にすると書き込みの直後に頼んだ読み直しまで「古い要求」
  になり、届いた時点で捨てて頼み直す往復を延々と繰り返す
- **失敗した書き込みでも進める。** `git apply` が途中で落ちても index は動いている
  ことがある。どこを進めるかは `Reload::after` が成否ごと決める
- **リーダースレッドには何も足さない。** 世代は `Msg` ではなく `JobTracker` が持ち、
  `finish` が返す。`Cmd` / `Msg` の形は変わらない
- **判じるのは届いた返信の刻印。** 持っている値の刻印で判じると、読み取りが失敗し続けた
  ときに「古い → 頼み直す → 失敗」を回す。持っている値の刻印を見るのは「この値を根拠に
  してよいか」を問うとき（`AppState::diff_is_current` / `worktree_is_current`）だけ
- **領域は `JobKind::region()` から引く。** `is_fresh` に領域を渡させると、
  `finish(LoadStatus)` の刻印を `Region::Log` と比べる形が組めてしまい、常に「古い」に
  なって頼み直しを回す（`.claude/rules/rust-coding.md` の「旗と値を割らない」が 1 段上がった形）

**畳んだもの**: `DiffState.freshness`（→ `DiffView.epoch`)、`WorktreeState.stale` と
`loaded`（→ `snapshot_epoch: Option<u64>`）、`GraphState.reload_pending`（→ `Region::Log`
の世代）。**どれも「発行側の事情」を覚える印だった** —— 頼んだか、落とされたか。刻印は
返信そのものに付いているので、受け取り側が 1 つの問いで判じられる。

**塞いだ穴**: `LoadRepoInfo` / `LoadBranches` / `LoadHeadMessage` は書き込みのあとに出る
のに追いつきを持っていなかった（`issue` に落とされると、`switch` のあともラベルと
ブランチ一覧が切り替え前のまま残る）。3 つとも `Region::Refs`。

**刻印が拾えるのは「読みが飛んでいるあいだに書き込みが終わった」形だけ。** 逆順
（書き込みが走っているあいだに読みを頼み、読みのほうが先に返る）は刻印では捕まらない
—— そのとき世代はまだ進んでいないので、返信は「最新」に見える。`LoadHeadMessage` は
まさにこの順序になりやすい（git2 の読みは commit の hook や署名より先に返る）ので、
**押した時点で断る**（下の「commit」の `C`）。

**追いつきを 1 つの関数にはまとめない。** 判じ方（`is_fresh`）は 1 つだが、判じたあとの
形が 2 通りある —— `status_loaded` / `repo_info_loaded` / `branches_loaded` は**採った
うえで頼み直す**（古い索引でもラベルは出せるので、届くまで前のものを出したままにする）。
`head_message_loaded` と `log_loaded` は**中身を見る前に返す**（古い本文で入力欄を開くと
あとから届く返信では取り消せず、古いページは積むと消えたはずの行が残る）。関数 1 本に
すると、この違いが引数の分岐になって戻ってくる。

**畳まないもの**: 「失敗の記憶」の 5 つ（上の節）は鮮度ではない。`Msg::CommitDetailLoaded`
が `oid` を、`Msg::DiffLoaded` が `target` を運ぶ形も残す。**値そのものが鍵を持っている
なら、そちらのほうが強い**（世代は「リポジトリが変わった」しか言えず、「選択が動いた」を
言えない）。だから `LoadCommitDetail` / `LoadCommitFiles` は `region()` が `None`。
**`FetchPrList` は逆に、鍵が無くて世代だけ要る** —— 一覧は選択に紐づかないので
「選択が動いた」を言う鍵がそもそも無く、**世代だけが「書き込みで中身が変わった」を
言う**。**コミットの中身は書き込みで変わらないが、PR の中身は変わる**（上の
「PR ペイン › 何を引くか」）。`OpenPr` と `CreatePr` は `None` —— 前者は値を読まず、後者が返すのは**作った PR
そのもの**なので、書き込みに追い越されて古くなるという問いが無い。

**残るもの**: 落ちた要求の追いつき（(a)）は世代では解けない。`JobKind` の粒度を対象まで
含めるかは、`LoadDiff` の要求が実際に落ちて困る形が出てから決める。

## キー入力

crossterm の `KeyEvent` は core に入れられないので、core 側に等価な型を持つ。
`crossterm::event::KeyEvent → app::Key` の変換は `mewgit-tui` 側で行う。

フィールドは非公開にし、コンストラクタを唯一の入口にする。同じ打鍵が 1 通りの表現に
なるよう正規化するため。正規化しないと `Key` の `Eq` / `Hash` が壊れ、`keymap`（08、MVP 後）
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

### 案内に出すキーの表

**キーとその説明の一覧は `app/keys.rs` の 1 本。** ステータスバーの 2 行目（07 の
「ステータスバー」）と `Modal::Help`（07 の「Help モーダル」）と、あとから入る
`config.keymap` の上書き（08）が**同じ表を読む**。2 本に分けると、上書きしたときにヒントだけが既定の字を出す。

```rust
pub struct KeyHint { /* 非公開 */ }

impl KeyHint {
    pub fn keys(&self) -> &[Key];        // 案内に出すキー（`j` と `k` のように複数）
    pub fn label(&self) -> &'static str; // 何が起きるか
}

/// `?`（キーの一覧）と `q`。フッターに必ず出し、幅が足りなくても落とさない（07）。
pub const PINNED: &[KeyHint];
/// どのペインでも効くキーのうち、**フッターには出さないもの**。`Modal::Help` だけが読む（07）。
pub const COMMON: &[KeyHint];
/// `Modal::Help` の行（見出しとキー）。そのペインのキー → どこでも効くキーの順。
pub fn help(pane: Pane) -> Vec<HelpRow>;
/// **いま**押せるペインのキー。`None` は「この表のどれも押せない」（モーダル表示中）。
pub fn live(state: &AppState) -> Option<&'static [KeyHint]>;
/// そのペインで押せるキー。網羅 `match` なので、`Pane` を足すと実装漏れが落ちる。
pub fn pane(pane: Pane) -> &'static [KeyHint];
```

- **持つのは `Key` で、`j/k` や `Enter` という字は `ui` が `KeyCode` から組む。**
  表示の字を表にも書くと、導出できる値のコピーになる（`.claude/rules/rust-coding.md`）
- **ラベルは core が持つ。** 通知の文言と同じ扱いで、`ui` は並べるだけ（不変ルール 2）
- **同じ動作の別名は入れない。** `j` / `k` の代わりに効く `Down` / `Up` は表に無い
  —— 案内に出すものの一覧なので、出さないものを入れると「表 → 実際に押せる」の
  見張りが通るだけで案内は変わらない。**逆向き（`match` の腕が全部表にあるか）は
  `match` から導けない**ので、そこは見張れない
- **`m` / `Tab` / `Shift+Tab` / `Ctrl-C` は `COMMON` に入っている**（`Modal::Help` で
  足した）。読むのは `?` の一覧だけで、フッターには出ない
- **押して無反応なキーを表に入れない見張り**は `every_hinted_key_does_something`
  （`update` を通して 1 打鍵ずつ叩き、`Cmd` か `AppState` が動くことを見る）。
  `update_key` の `match` に対する 2 本目の一覧なので、**表だけが古くなる形を塞ぐ**
- **「いま押せるか」に答えるのは [`live`] のほう。** モーダル表示中はキーを全部モーダルが
  受けるので（上の「キー入力」）、ペインのキーも `COMMON` も `PINNED` も効かない。
  **`ui` に `modal.is_some()` を書かせない** —— 書くと振り分けの規則の写しが 2 本になる
- **表はまだ「どのキーか」と「何と書くか」しか持っていない。** dispatch と突き合わせる
  **アクションの同一性が無い**ので、上の見張りが「表 → dispatch」の片側しか見られない。
  `config.keymap`（08）はアクション名（`fetch = "f"`）で書くので、**`label` は鍵にならない**
  （`移動` / `切替` / `破棄` はどのアクション名とも一致しない）。**上書きを入れるより先に
  表へアクションを足す**（`docs/plan.md` の `config.keymap` の行）。足せば
  `Key → Action` の引きと `match action` の網羅で両方向が見られるようになり、この節の
  見張りと `hinted_state` は要らなくなる

## Msg

```rust
pub enum Msg {
    Key(Key), Resize { term: TermSize }, Layout { graph_rows: usize }, Tick(Timestamp),
    FocusGained,                       // 端末が前面に戻った（下の「外部の変更の検知」）
    // git_read
    Paste(String),                     // bracketed paste。編集中のモーダルだけが受ける
    LogLoaded { restart: bool, result: Result<LogPage, GitReadError> },
    CommitDetailLoaded { oid: Oid, result: Box<Result<CommitDetail, GitReadError>> },
    CommitFilesLoaded { oid: Oid, result: Box<Result<Vec<FileEntry>, GitReadError>> },
    HeadMessageLoaded(Result<Option<String>, GitReadError>),  // amend の初期値。None は unborn
    CommitMessageLoaded { oid: Oid, result: Box<Result<String, GitReadError>> },  // reword の初期値
    RepoInfoLoaded(Box<Result<RepoSnapshot, GitReadError>>),  // { remotes, head, refs }
    StatusLoaded(Box<Result<WorktreeSnapshot, GitReadError>>),
    FingerprintLoaded(Box<Result<RepoFingerprint, GitReadError>>),  // 下の「外部の変更の検知」
    BranchesLoaded(Box<Result<Vec<Branch>, GitReadError>>),   // ローカルブランチの一覧
    DiffLoaded { target: DiffTarget, result: Box<Result<Vec<FileDiff>, GitReadError>> },
    // git_exec
    ExecFinished(Box<ExecDone>),       // { op, result }
    // auth / github
    TargetResolved(Box<ResolvedTarget>),  // { target, unnormalized }。上の「RepoInfo.target を埋める」
    AccountResolved(Result<Account, DegradedReason>),
    AccountsListed(Result<Vec<GhLogin>, AuthError>),  // 選び直しの候補。上の「候補の取り直し」
    PrListLoaded(Result<Vec<PullRequest>, GithubError>),  // 開いている PR の一覧。上の「PR ペイン」
    PrDetailLoaded { epoch: u64, number: u64, sha: String, result: Result<PrDetail, GithubError> },  // 上の「集計」
    PrDetailsFinished { epoch: u64, aborted: Option<(GithubError, Vec<(u64, String)>)> },  // 扇形の終端
    RateLimitSeen { epoch: u64, seen: RateLimit },    // API の残量。下の「残量」
    PrCreated(Result<u64, GithubError>),              // 作った PR の番号。上の「PR 作成（N）」
    OpenPrFinished(Result<(), AuthError>),            // gh browse の結果。Err は通知に出す
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

`AccountResolved` は 04 の `resolve` の戻り値をそのまま運ぶ
（`Result<Account, DegradedReason>`）。`Result<Account, AuthError>` にしないのは、
`AuthError` と `DegradedReason` が同じ失敗を二重に表現し、`update` に手書きの
マッピングが生えるため。`AuthError` は `gh` / `ssh` 実行の内部エラーに留め、
`resolve` の中で `DegradedReason` に畳む。

`AccountsListed` は `auth::Backend::logins` の戻り値をそのまま運ぶ。**`Err` を
`DegradedReason` に畳まない** —— 畳むと `resolve` の外に `AccountState` の産出元が
でき、04 が決めた境目が消える（上の「候補の取り直し」）。受けるのはモーダルだけで、
`state.account` は動かさない。

**`AccountState` は運ばない**（04 の「送った判断」をここで畳んだ）。あの型には
`Resolving`（まだ頼んでいない）があり、`resolve` はそれを返さない ——
運ぶ型にしておくと、その約束を見張るのは doc の 1 文だけになる。`update` は
`Ok` を `Resolved`、`Err` を `Degraded` に写して `state.account` に入れる。
**2 つの型が残るのは、状態には「まだ頼んでいない」が要るから**で、
`Cmd::ResolveAccount` を出していないリポジトリ（`TargetState::Unavailable`）が
それに当たる（上の「`RepoInfo.target` を埋める」）。

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

大きい payload は `Box` に入れる。入れないと `Msg` が数百バイトになり、1 打鍵ごとに
作られる `Msg::Key`（実データ 12 バイト）まで同じ大きさで動く。**どれを `Box` するかを
ここに並べない** —— 並べた一覧は実物と割れる（実際に `BranchesLoaded` と
`TargetResolved` が漏れたまま残っていた）。**見張りは `msg.rs` のテスト（128 バイト）**
で、超えた変種はそこで落ちる。

## Cmd

```rust
pub enum Cmd {
    None, Batch(Vec<Cmd>),
    LoadLog { count: usize, restart: bool },  // 位置は LogSession が持つ。restart で捨てる（02）
    LoadCommitDetail(Oid),             // 選択コミットのメッセージ全文など（02）
    LoadCommitFiles(Oid),              // 同じコミットのファイル一覧（02）。詳細より重いので別
    LoadHeadMessage,                   // amend の初期値。HEAD の解決ごとリーダーで行う
    LoadCommitMessage(Oid),            // RebaseEditor の reword の初期値（上の「interactive rebase の開始」）
    LoadStatus, LoadDiff(DiffTarget),
    LoadRepoInfo,                      // remotes / head / refs。起動時と ref 変更後（M3）
    LoadBranches,                      // ローカルブランチ一覧。RepoInfo とは別（02）
    Probe,                             // 外の変更を見張る指紋（下の「外部の変更の検知」）
    Exec(GitOp),                       // git CLI 実行（03 参照）
    CancelExec,                        // 走っている Exec を中断する（上の「キャンセル」）
    ResolveTarget(RemoteUrl),          // 既定リモートの URL → RepoTarget（上の「RepoInfo.target を埋める」）
    ResolveAccount(RepoTarget),        // どのリポジトリのアカウントを決めるか（04 の手順 1〜4）
    ListAccounts(String),              // そのホストの login 一覧だけを取り直す（上の「候補の取り直し」）
    FetchPrList,                       // 開いている PR の一覧（上の「PR ペイン › 何を引くか」）
    FetchPrDetails { epoch: u64, targets: Vec<(u64, String)> },  // PR ごとの集計（上の「PR ペイン › 集計」）
    CreatePr(Box<CreatePr>),           // POST /pulls。上の「PR 作成（N）」
    OpenPr { number: u64 },            // gh browse。上の「o（ブラウザで開く）」
    Quit,
}
```

**`ResolveAccount` は対象を運ぶ。** ランタイムに `state.repo.target` を読ませると、
`TargetState` の 3 つのうち `Resolved` 以外を受けたときの腕を `dispatch` に書くことに
なり、**どのテストからも踏めない腕**が 2 つ増える（`update` はその 2 つでは `Cmd` を
出さない）。**`Box` には載せない** —— `Msg` と違って `Cmd` の大きさを決めているのは
`Exec(GitOp)`（96 バイト）で、`RepoTarget`（80 バイト）を箱に入れても 96 のまま
（実測）。`ResolveTarget` が運ぶ `RemoteUrl`（80 バイト）も同じ理由で裸で載せている。

**`CreatePr` だけは箱に入れる。** `String` を 4 本持つので 104 バイトあり、裸で載せると
`Cmd` が **96 → 112 バイト**になる（実測 2026-09-21、64bit）。分け目は
「`GitOp`（96）より大きいか」で、上の 2 つと同じ 1 本の規則から出る。

**`ListAccounts` が運ぶのはホスト名だけ。** `auth::Backend::logins` が要るのが `host`
だけで、`RepoTarget` を渡すと `owner` / `repo` を読まない引数が付く（「導出できる値を
引数に取らせない」）。**`ResolveAccount` とは別の `Cmd`** —— あちらは手順 1〜4 を
通して `AccountState` を作り直すもので、こちらは一覧を引くだけ。畳むと、選び直しの
モーダルを開くたびにアカウントが `Resolving` に戻る。

## 外部の変更の検知

**外で起きた変更（端末での commit / fetch / abort、エディタでの保存）を拾う。** 拾い方は
2 通り ——

- **ref・追跡設定・stash の変化は、mewgit の書き込みが 1 つ終わったのと同じ形で読み直す。**
  世代（上の「実行中ジョブ › 世代」）を進め、`Reload`（上の「ログの読み直し」）の腕と
  同じ読み直しを出す
- **ワークツリーの変化は status を読むだけで、世代を進めない。** 古くなったかは、届いた
  status を持っている値と比べて決める（下の「定期の status」）。ワークツリー側の mtime は
  外の `git status` 1 回でも動く（下の表）ので、世代を進める根拠にならない

**決めたこと**（2026-10-01 にユーザーと決めた）——

| 入口 | 間隔 | 何を読むか | 変わっていたら |
|---|---|---|---|
| probe | 2 秒（固定） | 指紋（下の `RepoFingerprint`） | 変わった領域だけを `Reload` で |
| 定期の status | 10 秒（`general.refresh_interval`。0 で止める。08） | `LoadStatus` | 一覧を入れ替え、diff の stamp が違えば diff を読み直す |
| `FocusGained` | 端末が前面に戻ったとき | probe と `LoadStatus` を両方すぐ | 上の 2 行と同じ |
| `R` | 押したとき | 指紋を見ずに全部（`Reload::RefsAndLog`） | — |

**ファイルの監視（`notify`）は採らない。** lazygit は fsnotify を外し（#2865。「file
handle を使いすぎる」）、gitui は notify を既定から外した（#1657。「大きなリポジトリで
問題になる」）。`.gitignore` を見る除外が無く `target/` まで見張り、Linux では inotify の
上限に当たる。**いまの lazygit の既定も上の表と同じ形**（フォーカス復帰で全体、2 秒ごとに
ref の指紋、10 秒ごとに files。`pkg/gui/background.go`、2026-09-30 の ff375b1）。
**フォーカスの報告（DECSET 1004）を送らない端末がある**（tmux は `focus-events` が既定で
off、GNU screen は 1004 を捨てる）ので、`FocusGained` は来なくても成り立つ形にする ——
来なければ 10 秒の status と 2 秒の probe が拾う。

### 指紋

```rust
/// 外の変更を見張る指紋。リーダースレッドが git2 と stat で組む（git を起動しない）。
pub struct RepoFingerprint {
    /// HEAD（symbolic なら ref 名も）と、`refs/stash` を除く全 ref の名前と指す先のハッシュ。
    pub refs: u64,
    /// `refs/stash` の指す先と、reflog（stash の一覧）の件数。
    pub stash: (Option<Oid>, usize),
    /// commondir の `config` の mtime（追跡設定・リモート一覧）。
    pub config: Option<SystemTime>,
    /// ワークツリーごとの gitdir の `index` と、gitdir そのものの mtime（進行中操作の印）。
    pub worktree: (Option<SystemTime>, Option<SystemTime>),
}
```

**ref は stat ではなく git2 で列挙する。** 決まったファイルの mtime を見る形（tig の
`src/watch.c`）は、**入れ子のブランチを外で作ったのを取りこぼす** —— 実測（2026-10-01、
git 2.56.0、一時リポジトリ。外での操作ごとに gitdir の各所の mtime を比べた）:

| 外での操作 | mtime が動くもの |
|---|---|
| ファイルの編集（未変更 → `M`、`M` → `M`） | **無し** |
| `git add` | `index`、gitdir |
| commit / `reset --hard` | `index`、gitdir、`refs/heads`、`logs/HEAD` |
| ブランチ作成 `x` | `refs/heads` |
| **ブランチ作成 `feat/y`** | **`refs/heads/feat` だけ**（gitdir も `refs/heads` も動かない） |
| `git fetch` | gitdir、`refs/remotes/origin`、`FETCH_HEAD` |
| stash push / drop | gitdir、`refs`、`refs/stash` ほか |
| `git branch --set-upstream-to` | gitdir（`config` の書き換え） |
| **変更の無い `git status`**（2 回とも） | **`index`、gitdir**（`index.lock` を作って消す） |
| `M` がある状態の `git status`（2 回目） | gitdir |
| `git --no-optional-locks status` | 無し |

**下の 3 行は `/simplify` の efficiency が拾った**（最初の版は gitdir と `index` の mtime の差を
`Reload::Nothing` に写していた）。シェルのプロンプトや IDE が `git status` を叩くたびに
status と diff の読み直しが 2 秒ごとに走り、diff の世代が進むたびにハンク単位の
`s` / `u` / `x` が断られる窓が開いていた。**mtime からは「外で `git status` を叩いた」と
「外で `git add` した」を区別できない**ので、ワークツリー側の差は status を読む
きっかけにだけ使う（下の表の最後の行）。

**stash は指す先だけでは足りない** —— 上以外の stash を外で drop / pop しても
`refs/stash` は動かない（実測 2026-10-01、git 2.56.0。3 つ積んで `git stash drop stash@{1}`
の前後で同じ oid、一覧は 3 → 2 件）。読み直さないと、mewgit の `d` / `a` / `Z` が
`stash@{n}` で別の stash に効く（`/code-review` が拾った）。reflog の件数を一緒に持つ。

**列挙のコスト**（release、git2 0.21.0、2 回目以降）: ref 1 万本（loose 5000 を
`refs/heads/feat/g*/` に入れ子 + packed 5000）で **70ms**、手元でいちばん ref の多い
リポジトリ（192 本）で **0.25ms**。`git for-each-ref` は 1 万本で 225ms。リーダー
スレッドは直列なので、1 万本のリポジトリでは 2 秒ごとに `LoadLog` を最大 70ms 待たせる。

**指紋の差から何を読み直すかを選ぶ**（`update::watch::Changed::between`。`Reload` は
`app` の型なので `git_read` の `RepoFingerprint` には置けない）——

| 変わったもの | 出すもの |
|---|---|
| `refs`（HEAD を含む） | `Reload::RefsAndLog`（ref が動けば walk に入るコミットが変わりうる。`Stash` も含む） |
| `config` だけ | `Reload::Refs`（`RepoInfo` のリモート一覧と、ブランチ一覧の追跡設定） |
| `stash` だけ（指す先か件数） | `Reload::Stashes` |
| `worktree` だけ | **`Cmd::LoadStatus` だけ**（世代を進めない。進行中操作は status に載る。02） |

複数が同時に変わったら、全部を覆う腕を選ぶ（`config` と `stash` なら `RefsAndLog`）。
**`RefsAndLog` はグラフの位置を先頭へ戻す**（`GraphState::restart`）が、mewgit の `f` も
同じ腕なので、外の fetch と見え方が揃う。**何も変わっていなければ何も出さない** ——
フォーカス復帰のたびにカーソルが飛ぶ形にしない。

### mewgit 自身の書き込みと分ける

書き込みのあとは指紋が必ず変わる。それを外の変更と読むと、読み直しが 2 周する。

- **書き込みの実行中は probe も定期の status も出さない**（lazygit の
  `PauseBackgroundRefreshes` と同じ）。書き込みの途中の値を外の変更と読む形を塞ぐ
- **`exec_finished` は持っている指紋を捨てる。** 次の probe（最大 2 秒後）の返りは
  基準にするだけで、比べない（比べる相手が無い）。書き込みの終わりから次の probe が
  読むまでのあいだに外で起きた変更は拾わない（受け入れた穴。次の変更で拾う）。
  **probe をその場で頼まない** —— `exec_finished` の返す `Cmd` は書き込みごとに読み直しの
  組を決めており（上の「ログの読み直し」）、そこに probe を混ぜると読み直しの表と
  関係の無い 1 本が全部の腕に付く
- **書き込みの実行中に届いた probe の返りは比べない。** probe がリーダースレッドで重い
  読み取りの後ろに並ぶと、出すときには走っていなかった書き込みの最中に返ることがあり、
  そのときはまだ刻印で見分けられない（世代を進めるのは `exec_finished`）。基準は
  `exec_finished` が捨てるので、次の probe が取り直す（`/code-review` が拾った順序）
- **書き込みより前に頼んだ probe の返りは捨てて頼み直す**（`status_loaded` の追いつきと
  同じ形）。**`JobKind::Probe` の領域は `Region::Worktree`** —— `Worktree` を進めるのは
  書き込みと外の ref の変化だけなので、刻印で書き込みを挟んだかが判る。**status の差は
  世代を進めない**（下の「定期の status」）ので、外で commit したときに status の返りが先に
  届いても、飛んでいる probe は捨てられない（`an_external_edit_seen_by_the_status_first_does_not_hide_the_refs`）。
  **例外は `Account`**（`.git/config` の 1 行。どの領域も進めない）で、その書き込みと同時に
  飛んでいた probe は古いまま基準になり、次の probe が `config` の差として `Refs` を
  1 回余分に読み直す（害は読み直し 1 回）。
  **最初の版は `Region::Write`（書き込みだけを数える領域）を足していた** —— status の差で
  `Worktree` を進める形だったので、それに巻き込まれないための特例だった。`/simplify` の
  altitude が拾い、status の差を世代の外（`DiffView::stamp`）で扱う形にして消した

### 定期の status

**status の一覧だけでは、既に `M` のファイルを外で書き換えたのを拾えない**（一覧は
`M` のまま変わらない。上の表の 1 行目）。そのため **snapshot にワークツリー側の
`(mtime, len)` を載せ**（`WorktreeSnapshot.stamps`。02 の「ステータス」）、届いた snapshot を持っている snapshot と
比べる。

- **コンフリクト中のファイルを保存しただけでは拾わないのが正しい**（実測 2026-09-27、
  git 2.55.0 —— 保存では `index` も gitdir も mtime が動かず、git 自身も `UU` のまま）。
  解消は `git add` が index を動かすので probe が拾う。`stamps` の差で diff は読み直す
- **diff は `DiffView::stamp` で比べる。** diff を採った時点の `snapshot.stamps[path]` を
  view に添え、`AppState::diff_is_current` が今の snapshot の stamp と比べる。届いた
  status の stamp が違えば `status_loaded` の出口の `load_diff` が読み直す。比べるのは
  ワークツリーか index を読む対象（`IndexVsWorktree` / `HeadVsIndex`）
- **stamp はワークツリーの `(mtime, len)` と index の blob の組**（`PathStamp`）。unstaged の
  diff は index にも依存するので、外で `git add -p` すると、ワークツリーのファイルは動かない
  まま diff だけが変わる（`/code-review` が拾った。最初の版はワークツリー側しか見ず、
  古い diff の `x` が index に残ったハンクの逆変更をワークツリーに入れうる形だった）。index 側の
  blob は status の delta が既に持っているので、index を別に読まない。**HEAD 側の blob も
  持つ**（staged の diff の基準。下の「古いハンクの守り」で外の commit を拾うために足した）
- **世代は進めない。** 進めると、外で 1 ファイル保存しただけで status まで古い扱いになり、
  届いた値の刻印を書き換える特例が要った（最初の版。`/simplify` の altitude が拾った）。
  **10 秒ごとに無条件に古くする形も採らない** —— そのたびに diff を読み直し、届くまで
  ハンク単位の `s` / `u` / `x` が「最新の差分をまだ読めていません」で断られる
- **どの経路の status でも、定期の期限を数え直す**（`status_loaded`）。数え直さないと、
  書き込みや外の変更のあとの読み直しの直後に、定期のぶんが同じものをもう一度読む

**読み直す前に押せる窓は残る**（外で書き換えてから最大 10 秒）。そこで押された
ハンク操作は、下の「古いハンクの守り」が `git apply` の手前で断る。

### 古いハンクの守り

**ハンク操作は、ハンクを読んだ時点の指紋と `git apply` の直前の指紋が違えば断る。**
守りが無いと、外で書き換えたあとに古いハンクの `s` を押すと **index に HEAD にも
ワークツリーにも無い内容が入る**（exit 0。実測は 03 の「古いハンクの守り」）。

- **比べる値は「ハンクを読んだ時点」の指紋**（`FileDiff::stamp`。02 の「diff とハンク」）。
  diff の読み取りが**ハンクを組む前に**採る。**`DiffView::stamp`（status の stamp）は使わない**
  —— あちらは diff を採った時点の status の値で、status のあとに書き換えてから diff を
  読むと、view は新しい中身を古い stamp で持つ。**2 つは役目が違うので別に持つ**:
  `DiffView::stamp` は「読み直すか」を status と比べて決める値（上の「定期の status」）、
  `FileDiff::stamp` は「見えているハンクがいまのファイルと合っているか」を確かめる値。
  片方に寄せると、status が diff より古いときに「違う → 読み直す → また違う」が次の
  status まで回る（1 本目の `/simplify` の altitude が時点のずれを拾い、2 本目で決めた）
- **比べるのは `git_exec` の `Runner`**（2026-10-02 にユーザーと決めた）。`update` は
  I/O をしないので、指紋を `GitOp` の中身（`HunkApply`）の `expect` に載せ、`Runner` が git を起こす直前に
  採り直す。**reader 経由の往復**（`Cmd` → `Msg` で採り直してから `Exec` を出す）は
  採らなかった —— `AppState` に返事待ちのハンク操作が増え、リーダーが `LoadLog` の
  後ろに並ぶと照合から実行までの隙間が広がる。`x` は確認モーダルを挟むので、押してから
  `y` までの間の編集もこの形なら拾える
- **指紋を組むのは `git_read` の `path_stamp` の 1 本**（00 のレイヤーの例外）。diff の側と
  `Runner` の側で組み方が割れると、照合が常に一致か常に不一致にずれる
- **断ったら `ExecError::Stale`**（03）。通知は `Warn`（git を起こしておらず、何も壊れて
  いない）、要約行は案内だけで**パスは詳細に出す**（上限が無く、要約行の予算を食う。
  上の「失敗の案内文」）。読み直しは他の失敗と同じく `exec_finished` が出す
  （`Reload::Nothing` が `Region::Worktree` を進め、status と diff が届く）ので、
  押し直せば読み直したハンクが当たる
- **指紋が無いハンクは断る**（`update/worktree.rs` の `expect_of`）。ワークツリーの diff は
  必ずパスを絞るので起きないが、ファイル単位に落とすとハンク 1 つのつもりでファイル全体を
  stage することになる

**外部の変更の検知とは別の PR にした。** 検知（見張りと読み直し）と、diff の読み取りと
書き込みの前提に手を入れるこちらは関心事が違う（`docs/plan.md`）。

### 間隔とフォーカス

- **間隔は `update` が `Msg::Tick` の時刻で決める**（`WatchState` に次の期限を持つ）。
  ランタイムに別のタイマーを持たせない —— 期限の判断がテストできる側に残る
- **`Tick` から `Cmd` を出すのは期限を過ぎたときだけ**（2 秒・10 秒）。毎回は出さない
  （250ms ごとに git2 で ref を列挙することになる）
- **`FocusGained` は probe と `LoadStatus` を出し、両方の期限を今から数え直す**（status の
  期限は返りの `status_loaded` が数え直す）。
  `FocusLost` は捨てる。**有効にした直後に今のフォーカスを送ってくる端末がある**
  （lazygit の `pkg/gocui/gui.go` が状態の変化だけを見ている理由）が、指紋が変わって
  いなければ何も読み直さないので、起動直後の 1 回は害が無い
- **フォーカスの報告は `main.rs` が有効にする**（`EnableFocusChange`。ratatui の
  `try_init` は有効にしない）。終了時に `DisableFocusChange` を送る。**パニックの経路では
  送らない**（`EnableBracketedPaste` と同じ扱い。ratatui のパニックフックは raw mode と
  代替画面しか戻さない）
- **時計が後ろへ跳んだら期限を引き戻す**（`WatchState::clamp_to`）。期限は壁時計の秒
  （`Timestamp::now`）なので、NTP の補正や VM の復帰で戻ると、跳んだぶんだけ見張りが
  止まる（`/code-review` が拾った）。`Tick` ごとに「今から 1 周期先」を上限にする
- **probe の失敗は最初の 1 回だけ通知する**（`WatchState.failed`。成功で落とす）。
  2 秒ごとに積むと通知の履歴が埋まる。上の「失敗の記憶」の形
- **ヘッダのスピナーは `Tick` を 1 つまたいで走っているジョブだけを描く**（`Job::since`。
  07 の「ヘッダ」）。probe（1ms 未満）と小さなリポジトリの status が 2 秒・10 秒ごとに
  1 フレームだけ駒を光らせないため。最初の版は probe だけを種別で外していた（`JobKind::spins`）
  が、status のちらつきが残っていた（`/simplify` の altitude が拾った）

### `R`

**指紋を見ずに `Reload::RefsAndLog` と同じ読み直しを出す**（世代も進める）。書き込みの
実行中も通す（押した人が頼んだ読み直しで、一緒に頼む probe が書き込みより前の値を読めば
刻印で捨てる）。
検知が外れる形（NFS、指紋に入れていない場所の変更）からの逃げ道なので、比べる
処理を挟まない。**グラフの位置は先頭へ戻る**（押した人が頼んだ読み直し）。
共通キーで、フッターには出ない（07 の「ステータスバー」。共通キーは `?` の一覧だけ）。

**拾わないもの**: `.git/config` の `mewgit.user`（アカウントの指定）を外で書き換えても
解決し直さない（`config` の差は `Refs` に写すだけ）。`A` で選び直せる。

## 起動時の読み込み

```rust
pub fn init(state: &mut AppState) -> Cmd;
```

`runtime::run` がループに入る前に 1 回呼ぶ。`main.rs` から呼ぶと、テストを書かない
配線の側に処理が出てしまう。出すのは `LoadRepoInfo`、
`LoadLog { count: config.graph.page_size, restart: false }`、`LoadStatus`、
`LoadBranches`、`LoadStashes`。
**`ResolveTarget` と `ResolveAccount` は `init` からは出さない** —— 前者はリモート
一覧が要るので `Msg::RepoInfoLoaded` を、後者は `RepoTarget` が要るので
`Msg::TargetResolved` を待ってから出す（上の「`RepoInfo.target` を埋める」）。起動時はまだ何も読んでいないので
`restart` は偽でよい（`LogSession` が無ければ `restart` の有無に関わらず先頭から開く）。

**進行中操作は `LoadStatus` の返りに載ってくる**（`WorktreeSnapshot.progress`。02 の
「進行中操作の検出」）。起動時の専用の読み取りは持たない —— 起動時・書き込みのあと・
外部の変更の検知（上の節）が同じ `LoadStatus` を通るので、**古い返りを捨てるのは
`Region::Worktree` の世代の 1 本**で済む（上の「実行中ジョブ › 世代」）。`conflicted` の
一覧と進行中操作が同じ時点の値になり、書き込みの拒否（上の「進行中操作があるときの書き込み」）も `c` と同じ
「鮮度のある snapshot だけを信じる」（`WorktreeState::snapshot_epoch`）で判じられる。

## update の原則

- `update` は `AppState` を直接変更し、`Cmd` を返す。I/O を呼ばない。返す `Cmd` は
  出口で `AppState::issue` を通し、同種のジョブが走っていれば発行しない（進行中操作で断る
  書き込みは `Cmd::LoadStatus` に差し替える）
- キー入力は `focus` と `modal` を見て、対応する pane / modal の handler に委譲する。
  ペインが `Cmd` を組み立てるのは `update` の中だけで、`ui` からは呼ばない
- `ExecFinished` は成功・失敗・中断を問わず `LoadStatus` を発行する。**例外は
  `GitOp::SetAccount` / `UnsetAccount`（`.git/config` の 1 行だけを書く）は出さない —— 進行中操作は動かない**（`update::exec` の `Reload::Account`）。進行中操作は
  その返りで入れ替わる（03 の「進行中操作との整合」）。外部の変更の検知（上の節）も、
  進行中操作のための専用の `Msg` を足さずに `LoadStatus` を出す
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
        _ = tick.tick() => Msg::Tick(Timestamp::now()),
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
書かない例外は端末の raw mode 切替・復元と `main.rs` の配線だけ（`.claude/rules/testing.md`）。

- `event_to_msg` は `Option<Msg>`。マウス・フォーカス・ペーストは対応する `Msg` が無いので落とす。
  実装は `mewgit-tui/src/input.rs`（`keymap` の名前は 08 のキーバインド設定に取っておく）
- ストリームのエラーは握りつぶさず `?` で上げる（不変ルール 4）
- キー変換は押下（`KeyEventKind::Press`）だけを通す。kitty プロトコルが有効な端末では
  離鍵も飛んでくるため、1 回の打鍵が 2 つの `Msg::Key` になる
- `dispatch` は `ControlFlow` を返す。`Cmd::Quit` で `Break`、`Cmd::Batch` は再帰する
- 非同期 `Cmd` は tokio タスクにし、結果を `Msg` にして mpsc に流す。**ログ読み込みの
  タスクは abort しない。** `ReaderHandle::log_page` の future を途中で捨てると、
  リーダースレッドは walk を進めたあとで返り先を失い、そのページのコミットがどの
  ページにも現れなくなる（02 の「スレッド」）。中断が効くのは `Cmd::Exec` だけ
  （上の「キャンセル」）
- `dispatch` の `match` でも `_` を使わず全 `Cmd` 変種を列挙する。`update` / `Cmd::job_kind`
  と同じ理由で、変種を足したときに実装漏れをコンパイルエラーにするため
- 未実装の `Cmd` は黙って捨てず `Notice::Warn` に積む。開発中に落ちているのが見えるように
- **`dispatch` が `AppState` に書いてよいのは `notices` だけ**（と、未実装 `Cmd` の
  ジョブを畳むところ）。それ以外の状態変化は `Msg` にして `update` に戻す。状態遷移の
  入口を `update` 1 つに保つため
- **読むほうは制限しない。** 起動時に確定して変わらない値（`repo.path`、`config`）は
  `dispatch` が `state` から取る —— `Runtime` に写しを持たせると出どころが 2 本になり、
  テストが本体と違う値を掴んだまま緑になる（`Cmd::ResolveAccount` で実際に起きた）。
  **`Cmd` に載せて運ぶのは、載せないと踏めない腕が増えるものだけ**（`RepoTarget` は
  `TargetState` の残り 2 つを `dispatch` に持ち込まないために運ぶ）
- 初期の端末サイズは `Terminal::size()` で取り、`Msg::Resize` にして `update` に流す。
  起動時に `Resize` イベントは飛んでこない
- **描画の直前に毎回 `ui::graph_rows(state.term)` と `state.graph.viewport` を比べ、違えば
  `Msg::Layout` を流す。** これがレイアウトの変化を core に伝える唯一の経路になる。
  いま行数を動かすのは端末のリサイズだけだが（07 の「CommitDetail ペイン」）、比べる
  場所を 1 つにしておくと、狭いレイアウト（M6）でペインを隠すときも同じ経路で通る
- Git CLI 実行中も入力を受け付けるが、書き込み系 `GitOp` は同時に 1 つのみ。
  **キューには積まず、`AppState::issue` が落として通知を出す。** `JobKind::Exec` の
  粒度は種別単位なので 2 本目は必ず落ちる。黙って落とすと「押したのに何も起きない」に
  なるので、落としたことと、待たずに済む手（`Ctrl-C`）を伝える。
  **キューにしないと決めた**（実行中ジョブのスピナーとキャンセルのタスク）—— 確認モーダルを
  通した破壊的操作を待たせると、**流れる時点の状態は押したときの状態ではない**
  （`x` で確認したファイルの区画が、先に走っている `pull` で変わっている）。
  中断が入ったので「終わるまで待つ」から抜ける手もある
- `Tick` は 250ms。**ランタイムが読んだ時刻を載せる**（`update` は時計を読まない。
  上の「通知」）。`AppState.tick` を進めてスピナーを回すのと、外部の変更の検知の期限（上の「外部の
  変更の検知 › 間隔とフォーカス」）に使う。**`Tick` から `Cmd` を出すのは期限を過ぎた
  ときだけ** —— 毎回出すと 1 秒に 4 回 git2 で ref を列挙することになる
- `Cmd::Exec` のタスクには中断の受け口（`oneshot`）を渡し、送り手はランタイムが持つ。
  `Cmd::CancelExec` はそれを撃つだけで、`Msg` を返すのはタスク自身（上の「キャンセル」）

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
- **`Ctrl-C` は `Cmd::Exec` が走っているあいだだけ中断を優先する**（上の「キャンセル」）。
  読み取りが走っているかは見ない —— スクロール中はほぼ常に `LoadLog` が走っている
- 編集中のモーダルを `Ctrl-C` で捨てる際も確認は挟まない。`modal` は 1 つしか持てず、
  確認を重ねるにはモーダルのスタックが要る（上の「コミットメッセージの編集」）
