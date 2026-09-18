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
      鮮度（01 の「diff のキャッシュと鮮度」）
- [x] `Modal::CommitMessage` と commit / amend。`TextBuffer` の編集操作、Worktree の
      `c` / `C`、`GitOp::Commit` の配線。**commit したあとログを読み直す経路も
      ここで入れる**（`Cmd::LoadLog { restart }` と、落とされたぶんの追いつき。01 の
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
      「`RepoInfo.target` を埋める」。04 の「既定リモート」）
- [x] `f` / `p` / `P` の配線 — `GitOp::Fetch` / `Pull { FastForwardOnly }` / `Push` を
      共通キーから出す。push の行き先の決定と `-u` 提案（`Modal::Confirm`）、
      ssh のパスフレーズで固まる件（実効 ssh に `-o BatchMode=yes` を重ねる。03 の
      「Runner」）。**`GitOp` と `to_command` は M2 の `git_exec` のタスクで済んでいる**
      ので、ここで足すのは `app` の配線と `Runner` の環境変数だけ
- [x] force push — `P` が `NonFastForward` で断られたあとの再 `P` を確認モーダル経由の
      force push にする。`git_read::Branch` に upstream の Oid を足し、
      `--force-with-lease=<dst>:<lease>` に渡す（01 の「force push」）
- [x] `JobTracker` に世代を持たせ、`DiffState.freshness` と `WorktreeState.stale` /
      `loaded` と `GraphState.reload_pending` を畳む（01 の「実行中ジョブ › 世代」）。
      **カウンタは領域ごとに 3 本**（`Worktree` / `Refs` / `Log`）。1 本にすると
      index しか触らない `stage` でもログの世代が進み、`LogSession` の作り直しを
      毎回払う。**`LoadRepoInfo` / `LoadBranches` / `LoadHeadMessage` の追いつきも
      ここで塞いだ。** fetch で ref が変わったあとのログの読み直しは #47 で
      `Reload::RefsAndLog` に乗って済んでいる
- [x] `SamlSsoRequired` / `SshAuthFailed` / `NonFastForward` の検出と案内文。
      **force push が `stale info` で断られた形は `f` を勧める**（lease は fetch して
      からでないと通らない。01 の「force push」）。**追跡設定の無いブランチが非 FF で
      断られた形は mewgit の中で抜けられない** —— force に上げるのは `PushPlan::Tracked`
      だけなので、`-u` の提案から入った経路には案内文が要る。
      **`pull --ff-only` の拒否も分類に足した** —— `Not possible to fast-forward` は
      `rejected` も他の 3 語も含まないので `Other` に落ちていた（01 の
      「失敗の案内文」、03 のエラー分類）
- [x] 実行中ジョブのスピナーとキャンセル。ヘッダ右端に駒 + 説明（`⠋ fetch`）、
      `Ctrl-C` で走っている `Cmd::Exec` を中断する（01 の「スピナー」「キャンセル」）。
      **殺し方はプロセスグループ宛ての SIGTERM**（`kill_on_drop` の SIGKILL だと
      `index.lock` が残り、git だけに送ると ssh が孤児として生き残る。実測の表は 03 の
      「キャンセル」）。**書き込みのキューは入れないと決めた**（01 の「ランタイム」）

- [x] `Modal::RemotePicker` — `f` が既定リモートを決められないときに一覧から選ばせる。
      **#63 で開いた穴を塞ぐ**（通知が「追跡先を設定してください」と言うのに、mewgit に
      その操作が無かった。01 の「決まらないときの逃げ道」）

fetch / pull / push を 2 本に割った理由。1 本目は新しい読み取りを 1 つも足さず、
既にある `GitOp` を `app` から出すだけで**画面から触れるものが増える**（`f` で
`origin/x` のラベルと `↓n` が動く）。2 本目は `git_read::Branch` に upstream の Oid を
足す新しい読み取りと、`NonFastForward` を憶える状態が要る。**BatchMode は 1 本目に
入れる** — `f` を配線した時点でパスフレーズ待ちの経路が UI から到達するので、
分けると「fetch を押すと固まる」状態を一度出荷することになる（03 の「Runner」が
「それまでこの経路は UI から到達しない」と書いていたのは、まさにここまでの話）。

## M4: GitHub 統合

- [x] `reqwest` を 0.13 に上げる。**GithubClient を書くタスクと同じ PR で行う。**
      0.13 は `rustls-tls` feature が消えて `rustls`（crypto は aws-lc-rs、ルート証明書は
      `rustls-platform-verifier` = OS のトラストストア）になり、`query` が opt-in に
      なった（ページネーションに要る）。**どの feature が要るかは呼ぶコードが無いと
      検証できない**ので、宣言だけの状態で上げない（05 の「TLS とルート証明書」）
- [x] `auth::gh::GhCli` — detect / auth_status / token。**テキスト出力は解析しない**
      と決めた（gh 2.81.0 未満は `GhTooOld` で degraded。04 の「最低版は gh 2.81.0」に
      理由）
- [x] `github::GithubClient` — `repo` / `pr_for_branch` / `checks` / `reviews` + wiremock テスト
- [x] `RepoInfo.target` を埋める — 既定リモートの規則（`f` の無反応もここで塞ぐ）、
      `RemoteUrl` の port を `RemoteScheme::Https { port }` に畳む、`GithubClient::new`
      が port を受ける、`ssh -G` の結果を産出側で `valid_host`、`resolve_ssh_host` を
      実効 ssh（`sh -c`）に向ける。**`auth::resolve` から割った 1 本目**（04 の
      「タスクを 2 本に割った」）。port と target 埋めは同じ PR でないと片方が宙に浮く
- [x] `auth::resolve` — 明示設定 → 単一 → probe → Ambiguous の流れ + テスト。
      **2 本目。** `auth` が `github` を名指ししない狭い trait（`auth::Backend`）で
      probe を受ける（`api: &GithubClient` は 00 のレイヤー則で不可。04）。
      **配線は入れていない** —— `Backend` の production 実装（`github::AccountBackend`）と
      `Cmd::ResolveAccount` の発行は次のタスクで、読み手が無いまま出すと起動のたびに
      「未実装のコマンドです」が出る（01 の「`RepoInfo.target` を埋める」）
- [x] ヘッダの `[gh: login @ host]` 表示と Degraded 表示。**`auth::Backend` の
      production 実装（`github::AccountBackend`）と `Cmd::ResolveAccount` の配線も
      ここ**（04 の「`Backend` の production 実装は `github` に置く」）。
      **`Modal::AccountPicker` より先に置いた** —— 配線はどちらにも要る前提で、
      こちらは `AccountState` を出すだけなので読み手として小さい
- [x] `Modal::AccountPicker` と `.git/config` への保存。**`NoAccess` / `Ambiguous` の
      内訳（どの login がなぜ落ちたか）を並べるのもここ** —— ヘッダは 1 行なので
      件数しか出していない（04 の「解決結果は画面上部に」）。**開くのは共通キー `A`**
      で、`Degraded` 以外は通知に落とす（01 の「アカウントの選び直し」）。保存は
      `GitOp::SetAccount`（`git config --local`）で、書いたあとに解決をやり直す
- [x] 保存したアカウントの選び直しと指定の解除。**`A` は `Resolved` では通知しか
      出せず、`.git/config` に書いた `mewgit.user` を mewgit から外せない**
      （`/code-review` が拾った。01 の「アカウントの選び直し」）。要るのは
      **候補一覧の取り直し**（`gh auth status` をもう一度）と、
      **`AccountSource::Config` を出どころで割ること**（`.git/config` は `--unset`
      できるが設定ファイルの `[[repos]]` は外せないので、同じ案内を出せない）
- [x] degraded を外で直したあとの再解決。`Modal::AccountPicker` の `Degraded` 側に
      `r` を足し、`resolve_again` をそのまま呼ぶ。**決めたことは 01 の
      「外で直したあとの再解決」**（`Repick` に出さない理由、モーダルを閉じる理由、
      `git` 実行中に断る理由）
- [x] `ui::panes::prs`（1/2）現ブランチ PR — `PrState.current` / `checks` を埋め、
      右上を `Branches | PRs` のタブにし、`o` で `gh browse`。**`config.github.enabled`
      が何を止める旗かもここで決める**（`AccountState::Disabled`。04）。
      **実装済みの API だけで組む** —— `pr_for_branch` / `checks` / `reviews` と
      `Link` のページ辿りは #59 で入っている
- [x] `ui::panes::prs`（2/2 の 1 本目）PRs ペインを open な PR の一覧にする —
      `GithubClient::pr_list`（`GET /repos/{o}/{r}/pulls?state=open&sort=updated&direction=desc`）、
      `PrState` を `Fetch<Vec<PullRequest>>` 1 本にし、行頭の印（`*` 自分の PR /
      `R` レビュー依頼）まで。**`/search/issues` は使わないと決めた** —— 返るのが
      issue 形で `head` が無く、チェックを引く鍵を作れない（05。実測 2026-09-17）。
      **`pr_for_branch` と `Cmd::FetchPr` / `Cmd::FetchChecks` は消す**（01 の
      「現ブランチの PR」）
- [x] `ui::panes::prs`（2/2 の 2 本目 a）PR ごとのチェック集計 — 一覧の各 PR に
      `checks` / `reviews` を引く扇形と、認証付きリクエストの同時本数を抑える
      `Semaphore`。**扇形は 1 ジョブにする** —— `JobTracker` は種別単位なので、
      `Cmd` を N 本出すと `AppState::issue` が 2 本目以降を黙って落とす（05 の
      「`Semaphore`」）。**permit を取るのは HTTP を出す 1 箇所だけ** —— PR ごとにも
      取ると、`checks` の `try_join!` が内側で permit を待つあいだ外側が握ったままで
      詰まる（05）。**集計は `(番号, head sha)` を鍵に憶え、変わっていない行は
      引き直さない** —— 1 件あたり 3 リクエストなので、毎回引くと ref を動かす書き込み
      1 回が `3N + 1` になる（05 の実測）。**記号は `ui::pick_charset` を通し、
      `CheckState` の 5 値を 3 つに畳まない**（07 の「PRs ペイン」。1 本目で
      チェック行ごと消えたので、制約はそちらに書き戻してある）。
      **`CheckSummary` を 5 値に広げるのもここ** —— 定義が 3 フィールドのまま誰にも
      組まれず誰にも読まれておらず、07 の「3 つに畳まない」と最初から食い違っている
- [ ] `ui::panes::prs`（2/2 の 2 本目 b）レートリミット残量の受け渡しとヘッダ表示 —
      **残量は自分が出したリクエストの `x-ratelimit-*` から採る**（`/rate_limit` は
      別の数を返す。05 の実測）。**バケットは `core` だけ**（`search` を使わないと
      決めたのでバケットが 1 本になった。05 の実測表）。**常に出すか、閾値を割った
      ときだけ出すかは、ヘッダの余りを測ってから決める** —— 1 行を `owner/repo` と
      `[gh: …]` とスピナーで分け合っている（07 の「ヘッダ」。測り方は #73）
- [ ] PR の集計を明示的に引き直す経路 — a が `(番号, head sha)` で憶えるので、
      **sha が動かない変化（レビューが付いた / チェックが回り直した）は画面に出ない**。
      `f` は ref を動かすが PR の head sha は変えないので、いまの引き金では拾えない。
      塞ぐならキーを 1 つ足して、カーソル行の記憶を落として引き直す（a で受け入れた穴）
- [ ] fork のブランチで現ブランチの PR を当てる — **一覧には fork からの PR も出る**
      （`/repos/{o}/{r}/pulls` が返す）が、起動時にカーソルを座らせる照合が
      `head.repo.owner == target.owner` なので、**fork のブランチでは合致せず先頭に
      座る**（01 の「現ブランチの PR」）。塞ぐにはブランチの push 先リモート
      （`branch.<name>.remote`）を `RemoteUrl` に通して owner を出す経路が要る
- [ ] 閉じた PR を見る経路 — 一覧は `state=open` だけを引くので、**マージされた
      瞬間にその行は消え、mewgit から辿る手段が無い**（01 の「現ブランチの PR」で
      受け入れた穴）。塞ぐなら「一覧に `state=all` を混ぜる」ではなく、**別の入口**
      （番号を入れて開く / 閉じたものを出すトグル）にする —— 全件を引くと辿るページが
      2 桁増え、PR ごとの扇形が現実的な大きさに収まらない
- [ ] `Modal::PrCreate` と `create_pr`
- [ ] PR チェックアウト（同一リポジトリ / fork）
- [ ] 403 の `needed_scopes` 案内
- [ ] 既定リモートが GitHub でないときの PRs ペインの文言 — いまは
      `TargetState::Unavailable` でも `Cmd::ResolveAccount` が出ないため
      `AccountState::Resolving` のままで、ペインが**永久に「読み込み中…」**になる
      （理由を出せるのは `A` だけ）。塞ぐには枠の内側 30 セル（80x24）に収まる文言が
      要るので、**M6 の「端末幅 < 100 のレイアウト」で案内を畳む形を決めてから**
      （01 の「PR ペイン」）
- [ ] PR 一覧の打ち切りを知らせる — `get_paged` は 10 ページ（1000 件）で黙って
      打ち切り、`Ok` を返す。open PR がそれを超えるリポジトリでは**いちばん古い側が
      画面から消え**、`AppState::current_pr` も当たらなくなる（05 の「一覧は `Link` の
      `rel="next"` を辿る」）。**実際にそういうリポジトリを踏んでから**着手する

`ui::panes::prs`（2/2）を 2 本に割った理由。1 本目は**新しい往復を 1 つも増やさずに
画面から触れるものが増える** —— いままで HEAD のブランチの PR 1 件しか出なかった枠に、
open な PR が全部並ぶ。2 本目は PR ごとに `checks` / `reviews` を引く扇形で、
**同時本数（`Semaphore`）とレートリミット残量という別の関心事**が入る。1 本にすると
「一覧が出るか」と「扇形が何本走るか」が同じ差分に混ざる。

**2 本目をさらに a / b に割った理由。** a だけで**画面から触れるものが増える**
（各行にチェックの記号が出る）。b の残量は**a が入って初めて実際に動く**ので、
先に出しても動かない数をヘッダに置くことになる（05 の「残量の受け渡し」が
「読む側に出す意味が出る」と言っているのがこれ）。1 本にすると「扇形が何本走るか」と
「残量をどう出すか」が同じ差分に混ざり、`core` / `tui` / 設計書 3 本にまたがる。

**1/2（#83）が入れた「HEAD のブランチの PR 1 件」は、1 本目で一覧に置き換わる。**
`PrState.current` / `checks` と `pr_for_branch` は消える（01 の「現ブランチの PR」）。

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
- [ ] `-u` の提案を git の push の規則に揃える。**fetch の既定リモート
      （`update::default_remote`）とは別の規則**で、git は
      `branch.<name>.pushRemote` > `remote.pushDefault` > fetch の規則の順に見る。
      `git_read` に `Branch.push_remote` の読み取りが要る（04 の「既定リモート」、02）
- [ ] 意図的な巻き戻しの force push（`reset --hard HEAD~1` してから `P`）。**いまは
      `ahead == 0` でまとめて断っている** —— `↓n` だけのブランチが `f` のあとに force で
      通ってしまう形と同じ見た目なので、区別するには「巻き戻すつもりだ」を別の入口で
      受けるしかない（01 の「force push」）
- [ ] 端末幅 < 100 のレイアウト。**枠付きペインの折り返しが 1 セルはみ出す件もここ** ——
      `ui::pane_block` に `Padding::right(1)` を足す（`modals::block` は
      `Modal::AccountPicker` のタスクで入れた。07 の「モーダル」）。
      **モーダルのキーの案内が切れる境目もここで見る** —— `AccountPicker` の
      いちばん長い案内（`[u] 指定を解除` 付きと `[r] 解決し直す` 付き。どちらも
      14 セルなので並ぶ）は 57 セルで、箱の内側
      （`WIDTH` 60 − 枠 2 − `RIGHT_MARGIN` 1）と**ちょうど同じ**。`modals::place` は
      折り返さずに切るので、**実測で 52 桁から `[Esc]` が消える**（53 桁までは残る。
      `[u]` を足す前は 36 桁まで保っていた）。`Esc` は確認を挟まずに閉じるキーなので、
      狭い端末の案内をどう畳むかをこのタスクで決める。
      **`[u]` と `[r]` と選ばせる案内が同居する形は 74 セルで、80 桁の端末でも入らない**
      （実測）。いま出ないのは `[u]` を出す degraded が候補を運ばないからで、
      **`Repick` に `r` を出せない理由の 2 つ目がこれ**（01 の「外で直したあとの
      再解決」）。案内を畳む形を決めたら、`Repick` にも `r` を出せるか数え直す ——
      **`Single` で決まった相手が外のログイン追加で `Ambiguous` に変わったことは、
      いまの `A`（候補の取り直し）では拾えない**
- [ ] アカウント書き込み中の `A` を断る。**いまは書き込みが走っているあいだに
      `A` を押せて、`state.account` の書き込み前の写しを持ったモーダルが開き直せる**
      （閉じるのは `resolve_again` の側。01 の「写しの寿命」）。そのせいで `r` は
      `exec_busy` で断っており、**借りている通知が書き込み向けの文言**
      （「ほかの git 実行が終わるまで待ってください（Ctrl-C で中断）」）で、
      読み取りである `r` にはずれている。塞ぐ形は `account_picked` / `account_unset` が
      `Cmd::Exec` を返したときに `AccountState::Resolving` へ落とすこと ——
      `A` は `Resolving` では通知に落ちるので開き直せなくなり、`r` の `exec_busy` も
      要らなくなる。**#78 が足した見張り
      （`a_picker_reopened_while_the_write_runs_does_not_outlive_the_write`）の前提が
      消える**ので、あれが何を守っていたかを読んでから書き換える。
      **`JobKind::region()` のコメント「どちらも git の書き込みで動かない」も同じ行で
      直す** —— #78 以降は嘘で、`SetAccount` が `ResolveAccount` の読む鍵を書く
- [ ] ロケール（`LC_ALL` / `LC_CTYPE` / `LANG`）から `graph.ascii` の既定を決める
      （曖昧幅を全角扱いする端末は UTF-8 でもずれる。06 の「ASCII フォールバック」）
- [ ] `app/state.rs` を割るかを決める。**`update.rs` を割ったので workspace で
      いちばん長いファイルになった（2269 行。次は `update/graph.rs` の 1155 行）が、
      割る根拠は `update.rs` より弱い** —— 中身は `AppState` とその部品という 1 つの関心事で、
      大半が `pub` な型定義なので、割っても `pub(super)` として見えるものが増えない
      （00 の「ディレクトリ構成」）。**引き金は行数ではなく、`Modal` / `GraphState` /
      `WorktreeState` / `JobTracker` が独立して育ったかどうか。** M6 は `Modal::Help` と
      `RebaseEditor`、ステータスバーの通知履歴で `Modal` と `Notice` に手が入るので、
      そこで数え直す
- [ ] `git_exec::argv` の `VARIANT_COUNT` を消す。**`variant()` の網羅 `match` から
      離れた場所に変種の本数のコピーが置いてある**（CLAUDE.md の「離れた場所に、
      コードの『いま N ある』を書かない」）。**ただの削除では済まない** ——
      `covers_every_variant` は「`to_argv` は全 `GitOp` をカバーする」（同じく CLAUDE.md）
      の見張りなので、消すとカバー要求のほうが破れる。塞ぐ形は、各変種の代表値を返す
      網羅 `match`（`fn every_variant() -> Vec<GitOp>`）を 1 本置き、`cases()` が
      その集合を全部含むことを `BTreeSet` で比べること —— 変種を足すとそちらが
      コンパイルエラーになるので、数を書かずに漏れが出る
- [ ] `.git/HEAD` mtime 監視での外部変更検知
- [ ] README、リリースワークフロー（`cargo dist` or release-please）
