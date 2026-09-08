# 実装計画

各タスクは Claude Code に 1 回で渡せる粒度を目安にしている。完了したら `[x]` にする。

## M0: 土台

- [x] workspace がビルドできる状態にする（依存追加、`lib.rs` のモジュール宣言、空の `main.rs`）
- [x] `mewgit-core::config` — `Config::load()`、既定値、`[[repos]]` の glob マッチ、テスト
- [x] `mewgit-core::app` — `AppState` / `Msg` / `Cmd` の型定義と、何もしない `update`
- [x] `mewgit-tui` — ターミナル初期化・復元、イベントループ、`q` で終了、リサイズ対応
- [x] CI（GitHub Actions）: fmt / clippy / test を Linux + macOS で

## M1: 読み取り専用でグラフを出す

- [x] `git_read::RepoReader::open` / `remotes` / `refs` / `head`
- [x] `git_read` のログ列挙（`LogSession`、ページング）+ tempfile リポジトリでのテスト
- [x] `graph::lane` — レーン割り当てアルゴリズム + 固定ケースのテスト（直線 / 分岐 / マージ / オクトパス / 複数ルート）
- [x] `ui::panes::graph` — `GraphRow` の描画、罫線 / ASCII フォールバック、ラベル、色
- [x] コミット詳細の遅延取得と展開表示
- [x] スクロール追随のページ追加ロード

## M2: ワーキングツリーとブランチ

- [x] `git_read::status` / `diff`（ハンク分割）
- [x] `git_exec::Runner` と `to_command` の全 `GitOp` テスト、`ExecErrorKind` の分類テスト
- [x] `ui::panes::worktree` — フォーカス巡回、`Modal::Confirm`、`Runner` の配線、
      ファイル単位の stage / unstage / discard
- [x] `ui::panes::diff` — 左の領域を Graph と差し替えて diff を出す経路と、その表示
      （07 の「レイアウト」）。`Cmd::LoadDiff` の配線、`Msg::DiffLoaded` の `Result` 化、
      `path` で絞った diff の rename（02）
- [x] ハンク単位の stage / unstage / discard — `GitOp::DiscardHunk`、ハンク → パッチの
      組み立て（`git apply` への round-trip テスト）、`view` のキャッシュと
      `Freshness`（01）
- [x] `Modal::CommitMessage` と commit / amend。`TextBuffer` の編集操作、Worktree の
      `c` / `C`、`GitOp::Commit` の配線。**commit したあとログを読み直す経路も
      ここで入れる**（`Cmd::LoadLog { restart }` と `GraphState.reload_pending`。01 の
      「ログの読み直し」）。無いと commit しても左のグラフが古い先頭のまま動かない
- [x] コミット詳細を右ペインへ移す。**グラフ先頭の WIP 行と、そこを選んだときの
      右ペイン（staging）も一緒に入れる。** グラフ下の展開を廃止し、`shows_detail` /
      `graph_rows` / `Msg::Layout` を単純にする。フォーカスの候補を選択（WIP か
      コミットか）から決める形にする（01 の「ペインとフォーカス」）。
      **WIP 行を commit のタスクに入れない。** 右ペインが「WIP かコミットか」で
      中身を変えるので、コミット詳細の移設と同じタスクでないと、コミットを選んだ
      ときに詳細を出す先が無くなる（07 の「どこまで実装したか」）
- [x] コミットに含まれるファイル一覧を右ペインに並べ、選ぶと左が `DiffTarget::Commit`
      の diff に差し替わる（07）。**上と 2 本に割ってある。** 上は置き場所を移すだけで
      新しい読み取りを足さず、こちらは `git_read::commit_files` の追加と
      `AppState::wanted_diff` の拡張が要る（割った理由は 07 の「どこまで実装したか」）。
      **一覧は `diff(DiffTarget::Commit { path: None })` では作らない**（02）
- [x] `ui::panes::branches` の一覧 — `git_read::branches`、`Cmd::LoadBranches`、
      右列を Branches と（Working tree / コミット詳細）の上下に割る、`Tab` の巡回に
      `Pane::Branches` を足す。**読み取りだけ。** 書き込みは下の 2 本
- [x] ブランチの切替と削除 — `Enter` で `GitOp::Checkout`、`d` / `D` で
      `GitOp::BranchDelete`（`Modal::Confirm` を通す）。**`Checkout` の argv を
      `switch <target>` に直す**（03 は既に `switch` / `restore` 前提で、
      `checkout <target> --` だけが残っていた）。`update::Reload` のブランチ一覧の
      読み直しは #40 で入っているので追加はなし
- [x] ブランチの作成とリネーム — `n` / `r` で `Modal::Input`（`InputIntent`）、
      `GitOp::BranchCreate { switch: true }` / `GitOp::BranchRename`。
      **`Modal::Input` の最初の利用者**なので `modals/input.rs` と、1 行入力での
      貼り付けの扱いをここで決める。**グラフの `n`（カーソルの指すコミットを起点に
      作る）も同じ本に入れる** — 起点を選ぶ経路がどこにも無いとタグや過去のコミット
      からブランチを作れず、`InputIntent::BranchCreate { start }` の `Some` 側が
      どのテストからも踏めない（07 の「Graph ペイン」）。**`GitOp::Checkout` は
      `Switch` に改名する**（03 がこのタスクへ送っていた。`BranchCreate` の
      `checkout: bool` も `switch: bool` に揃える）

`ui::panes::branches` を 3 本に割った理由。1 本目は新しい読み取り（`git_read::branches`）と
右列のレイアウト変更で、書き込みを 1 つも足さない。2 本目は書き込みだが既存の
`Modal::Confirm` しか使わず、3 本目は `Modal::Input` という**新しいモーダル型**の最初の
利用者になる（`modals::place` の 3 人目）。**どれも単独で画面から触れるものが増える。**
詳しくは 07 の「どこまで実装したか」。

## M3: リモート

- [x] `auth::ssh::resolve_ssh_host`（`ssh -G`）と `RemoteUrl` パーサ + テスト。
      **どのリモートの URL を見るかは決めない**（`RepoInfo.target` を埋めるのは M4 の
      `auth::resolve`。04 の「既定リモートを決めるのは M4」）
- [ ] `f` / `p` / `P` の配線 — `GitOp::Fetch` / `Pull { FastForwardOnly }` / `Push` を
      共通キーから出す。push の行き先の決定と `-u` 提案（`Modal::Confirm`）、
      ssh のパスフレーズで固まる件（実効 ssh に `-o BatchMode=yes` を重ねる。03 の
      「Runner」）。**`GitOp` と `to_command` は M2 の `git_exec` のタスクで済んでいる**
      ので、ここで足すのは `app` の配線と `Runner` の環境変数だけ
- [ ] force push — `P` が `NonFastForward` で断られたあとの再 `P` を確認モーダル経由の
      force push にする。`git_read::Branch` に upstream の Oid を足し、
      `--force-with-lease=<branch>:<expected>` に渡す（01 の「force push」）
- [ ] `JobTracker` に世代を持たせ、`DiffState.freshness` と `WorktreeState.stale` と
      `GraphState.reload_pending` を畳む。fetch で ref が変わったあとのログの読み直しは
      M2 で入れた `Cmd::LoadLog { restart }` に乗せる（01 の「実行中ジョブ › 世代」）。
      **カウンタを 1 本にするとログまで毎回読み直すことになるので、領域ごとに
      分けるかはここで決める**
- [ ] `SamlSsoRequired` / `SshAuthFailed` / `NonFastForward` の検出と案内文
- [ ] 実行中ジョブのスピナーとキャンセル

fetch / pull / push を 2 本に割った理由。1 本目は新しい読み取りを 1 つも足さず、
既にある `GitOp` を `app` から出すだけで**画面から触れるものが増える**（`f` で
`origin/x` のラベルと `↓n` が動く）。2 本目は `git_read::Branch` に upstream の Oid を
足す新しい読み取りと、`NonFastForward` を憶える状態が要る。**BatchMode は 1 本目に
入れる** — `f` を配線した時点でパスフレーズ待ちの経路が UI から到達するので、
分けると「fetch を押すと固まる」状態を一度出荷することになる（03 の「Runner」が
「それまでこの経路は UI から到達しない」と書いていたのは、まさにここまでの話）。

## M4: GitHub 統合

- [ ] `reqwest` を 0.13 に上げる。**GithubClient を書くタスクと同じ PR で行う。**
      0.13 は `rustls-tls` feature が消えて `rustls`（crypto は aws-lc-rs、ルート証明書は
      `rustls-platform-verifier` = OS のトラストストア）になり、`query` が opt-in に
      なった（ページネーションに要る）。**どの feature が要るかは呼ぶコードが無いと
      検証できない**ので、宣言だけの状態で上げない（05 の「TLS とルート証明書」）
- [ ] `auth::gh::GhCli` — detect / auth_status（JSON とテキスト両対応）/ token
- [ ] `github::GithubClient` — `repo` / `pr_for_branch` / `checks` / `reviews` + wiremock テスト
- [ ] `auth::resolve` — 明示設定 → 単一 → probe → Ambiguous の流れ + テスト
- [ ] `Modal::AccountPicker` と `.git/config` への保存
- [ ] ヘッダの `[acct: login @ host]` 表示と Degraded 表示
- [ ] `ui::panes::prs` — 現ブランチ PR、チェック一覧、一覧タブ、ブラウザで開く
- [ ] `Modal::PrCreate` と `create_pr`
- [ ] PR チェックアウト（同一リポジトリ / fork）
- [ ] 403 の `needed_scopes` 案内

## M5: 履歴操作

- [ ] `git_read::in_progress` の検出と `ProgressDetected`、ステータスバー表示
- [ ] `--sequence-editor` / `--commit-editor` 隠しサブコマンド
- [ ] `RebasePlan::to_todo` + テスト、`Modal::RebaseEditor`
- [ ] rebase の continue / skip / abort
- [ ] cherry-pick（単一 / 複数）と continue / abort
- [ ] revert（mainline 選択）と continue / abort
- [ ] `ui::panes::stash` — 一覧 / diff / push / apply / pop / drop、部分 stash
- [ ] 進行中操作があるときの書き込み `GitOp` 拒否

## M6: 仕上げ

- [ ] `Modal::Help` とキーマップ上書き。**ステータスバーのキーヒント
      （フォーカス中のペインで押せるキーだけ）も同じタスク**。キーの一覧を 2 本に
      すると `config.keymap` の上書きがヒントに効かない（07 の「ステータスバー」）
- [ ] ステータスバー — 通知の消え方（dismiss / 期限切れ）と履歴表示。
      **いまは一度出た通知が次の通知まで消えない。** 成功した操作は通知を出さないので、
      「stage 済みに `s`」の警告が `u` の成功後も出たままになる（01 の「通知」）。
      **`Notice.detail`（argv と stderr）を出すのもここ。** いまは要約行しか描かないので、
      git が断った理由（`would be overwritten`、未マージのときの `-D` のヒント）が
      画面に出ない。不変ルール 4 が「UI まで届ける」と言っているのはこの詳細のこと
- [ ] 端末幅 < 100 のレイアウト
- [ ] ロケール（`LC_ALL` / `LC_CTYPE` / `LANG`）から `graph.ascii` の既定を決める
      （曖昧幅を全角扱いする端末は UTF-8 でもずれる。06 の「ASCII フォールバック」）
- [ ] `.git/HEAD` mtime 監視での外部変更検知
- [ ] README、リリースワークフロー（`cargo dist` or release-please）
