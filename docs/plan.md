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
      引き直さない** —— 1 件ぶんの往復が PR の数だけ積むので、毎回引くと `core` の
      予算に対して重すぎる（数は 05 の実測）。**記号は `ui::pick_charset` を通し、
      `CheckState` の腕を畳まない**（07 の「PRs ペイン」。1 本目でチェック行ごと
      消えたので、制約はそちらに書き戻してある）。**`CheckSummary` を腕ごとの
      カウンタに広げるのもここ** —— 定義が誰にも組まれず誰にも読まれないまま
      置いてあり、07 の「畳まない」と最初から食い違っている
- [x] `ui::panes::prs`（2/2 の 2 本目 b）レートリミット残量の受け渡しとヘッダ表示 —
      **残量は自分が出したリクエストの `x-ratelimit-*` から採る**（`/rate_limit` は
      別の数を返す。05 の実測）。**バケットは `core` だけ**（`search` を使わないと
      決めたのでバケットが 1 本になった。05 の実測表）。**常に出すか、閾値を割った
      ときだけ出すかは、ヘッダの余りを測ってから決める** —— 1 行を `owner/repo` と
      `[gh: …]` とスピナーで分け合っている（07 の「ヘッダ」。測り方は #73）
- [x] PRs タブを開いていない起動で集計を引かない — 既定のタブは Branches なので、
      **PRs を一度も開かない起動でも一覧の行数ぶんの往復を払っていた**（数は 05 の実測）。
      **門は `AppState::wanted_details`**（`wanted_diff` と同じ形で「見えているぶんだけ
      引く」）、**引き金は `update::focus`** —— フォーカスを動かすキーが全部通る入口で、
      出口（`reseat_focus`）には足さない（01 の「失敗の記憶」で採らないと決めた形）。
      **門が読むのは `right_tab` そのものではない** —— あれを書くのは出口なので、
      タブに入ったその回はまだ `Branches` のまま（01 の「PR ペイン › 集計」）。
      実測（2026-09-19、open PR 43 件の `sharkdp/hyperfine` を指す一時リポジトリ、
      **連続した起動のヘッダ `[api: N]` の差**）—— タブを開かない起動は **3 リクエスト**
      （4950 → 4947、別の回も 4817 → 4814）、`Tab` で開く起動は **130**（4947 → 4817）
- [ ] 見えている行だけ集計を引く — タブの門（上の行）は**開いた瞬間に一覧の行数ぶん**
      を払う（open 60 件で 181 リクエスト。05）。行単位に落とすには PRs ペインの行数を
      `Msg::Layout` に足し、窓の導出を `core` にも置く（`ui::render_rows` と二重化する）
      うえ、**スクロールが新しい引き金になる**。踏むリポジトリが出てから着手する
      （01 の「PR ペイン › 集計」に採らなかった理由がある）
- [x] PR の集計と一覧を明示的に引き直す経路 — **PRs ペインの `r`**（01 の
      「引き直す（`r`）」）。a が `(番号, head sha)` で憶えるので、**sha が動かない
      変化（レビューが付いた / チェックが回り直した）は自動では画面に出ない** ——
      `f` は ref を動かすが PR の head sha は変えない。**同じキーで一覧も引き直す。**
      一覧を引く引き金は起動と、ref を動かす書き込みと、`A`（01 の「何を引くか」）
      なので、**タブを開く操作では引き直らず**、他人が開いた新しい PR は `f` を押す
      まで出なかった。**タブを開いた回に一覧も引く形は採らなかった**
      （2026-09-19 にユーザーと確認）—— `Tab` で下へ向かうと途中で Prs を踏むので、
      通過のたびに一覧 1 往復を払うことになる。**落とすのはカーソル行の集計だけ**
      （全件だと押すたびに 1 + 3N。05）。**引けない理由は `AppState::github_block` が
      値で答える** —— 述語を消去法で打ち消すと `Degraded` が潰れてペインと食い違う。
      フッターの Prs はキーが 1 つ増えても 80 桁で共通キーまで出る側に**残っている**
      （セル数は 07 の実測）
- [x] fork のブランチで現ブランチの PR を当てる — `branch.<HEAD>.remote` を
      `HeadInfo::Branch.upstream`（当時は `upstream_remote`）として読み、**リモート名なら `RepoInfo.remotes`
      で URL に直し、引けなければ値そのものを** `RemoteUrl` に通して、その
      `owner` / `repo` と `head.repo` を比べる（**`gh pr checkout` が書くのは URL**。
      実測表は 01）。**材料は `Cmd::LoadRepoInfo` に載せた** —— `BranchState` の同じ
      設定を使うと、**座り直しが 1 回きり**なので `Msg::BranchesLoaded` と一覧の
      どちらが先に届くかで答えが変わり、二度と直らない（`RepoInfo.target` が
      `default_remote` を呼ばないのと同じ形）。**当てる順は追跡先 → `target`** ——
      git は「どこへ push したか」を憶えていないので、追跡先で打ち切ると main では
      座っていた行に座らなくなる（01 の表の 1 行目）。**host は比べない**
      （`target.host` は `ssh -G` で正規化済み、こちらは生。生で比べると fork だけ別の
      エイリアスを使う多アカウント運用で逆に落ちる）ので、**別ホストの同名
      `owner/repo` に同名のブランチがある形は受け入れた穴**（01 の「現ブランチの PR」）
- [ ] 閉じた PR を見る経路 — 一覧は `state=open` だけを引くので、**マージされた
      瞬間にその行は消え、mewgit から辿る手段が無い**（01 の「現ブランチの PR」で
      受け入れた穴）。塞ぐなら「一覧に `state=all` を混ぜる」ではなく、**別の入口**
      （番号を入れて開く / 閉じたものを出すトグル）にする —— 全件を引くと辿るページが
      2 桁増え、PR ごとの扇形が現実的な大きさに収まらない
- [x] `Modal::PrCreate` と `create_pr` — PRs ペインの `N` で title / body /
      base / draft の欄を持つモーダルを開き、`POST /repos/{o}/{r}/pulls` を 1 本出す
      （01 の「PR 作成（`N`）」）。**base の既定は `refs/remotes/<既定リモート>/HEAD`**
      —— `git_read::Remote.head_branch` として `Cmd::LoadRepoInfo` に載せた（02）ので
      **往復を増やさない**。symref が無い形（一度も fetch していないリモート）は
      空欄で開く。**head はこの時点では plain な branch 名**だった（fork の
      `<owner>:<branch>` は「PR チェックアウト（2/2）」で塞いだ。`branch.<name>.remote`
      から owner を組むと fork 運用で外れる。01 に実測）。
      **push 済みかは手前で判定しない** —— 判定の材料（追跡設定）は #47 と #98 で
      二度不確かだと判っているので、422 の本文を通知の `detail` に出す。
      **`N` を足したぶん、Prs のキーヒントは 80 桁で共通キーを落とす側に移った**
      （07 の実測）
- [x] PR チェックアウト（1/2）— PRs ペインの `Enter` でカーソル行の PR を取り出す。
      **`gh pr checkout` と同じ形**（fork は `refs/pull/<n>/head` から取り、
      `branch.<b>.remote` / `pushRemote` に fork の URL を書く。2026-09-23 に
      ユーザーと決めた。実測の表は 05 の「PR チェックアウト」）。手順は
      `GitOp::CheckoutPr` 1 つに載せ、`Runner` が順に叩く（03 の「複数の手順を持つ
      書き込み」）。**`f` が URL の追跡先を扱えない穴も塞ぐ** —— gh で取り出した
      ブランチでも「追跡先の https://… がリモートにありません」でリモート選択が
      開いていた（01 / 04 の「既定リモート」）
- [x] PR チェックアウト（2/2）— `Modal::PrCreate` が受け入れた穴を塞ぐ。fork に在る
      ブランチから PR を作るには `head` を `<owner>:<branch>` にする必要があるが、
      **どのリモートに在るかを git は憶えていない**（01 の「head の組み立て」に実測。
      `@{push}` も fork 運用のふつうの形で fatal になる）。**モーダルに `Head` 欄を
      足し、候補を人に選ばせる**（2026-09-23 にユーザーと決めた）—— 候補は各リモートの
      `refs/remotes/<r>/<branch>`（`RefIndex::remote_oid`。02）と追跡設定の
      URL（gh / mewgit が取り出した fork）。**候補が無ければ `N` で断る**（ブランチ名
      だけで送ると base リポジトリの同名ブランチとして解決される）。1/2 と材料が
      違った理由は 01 の「PR チェックアウト（`Enter`）」
- [x] 取り出した fork のブランチの名前がずれていても現ブランチの PR に座る —— 取り出しは
      衝突を避けてローカルを `bob/main` にする（05 の「ブランチ名」）が、座り直し
      （`state::current_row`）は**ローカルの名前**を PR の `head.branch`（`main`）と
      比べていたので座らず、**`N` の「既にあります」も同じ関数を読むので素通りしていた**。
      **追跡先が URL の腕だけ**、比べる名前を `HeadInfo::Branch.upstream` の `merge` から
      取る。**名前の追跡先には広げない** —— そこでの `merge` は「何を基にしたか」で、
      `checkout -b feat upstream/main` が upstream の `main` の PR に座る（01 の
      「現ブランチの PR」）
- [x] 足りないスコープの案内 —— 行の名前は「403 の `needed_scopes` 案内」だったが、
      **GitHub はスコープ不足を 404 で返す**（05 の実測）ので、ステータスで分ける前に
      `X-Accepted-Oauth-Scopes` / `X-Oauth-Scopes` を見る（`GithubError::MissingScope`）。
      ヘッダが付くのは mewgit の API では `repos/{o}/{r}` と commit status だけなので、
      **出す先は `A` の `NoAccess` の内訳**（`gh auth refresh -h <host> -s <scope>`。
      アクティブでない login には `gh auth switch` を先に書く。04 の「権限不足の案内」）。
      SAML の 403 は `AttemptFailure::SsoRequired` に割って「SSO の承認が要ります」
- [ ] SAML の承認 URL を案内に載せる — `X-GitHub-SSO` ヘッダが何を返すかを確かめて
      いない（SAML を強制する org が手元に無い）。**踏む org が出てから**、ヘッダの値を
      実測して `AttemptFailure::SsoRequired` に URL を持たせ、`A` の内訳に出す
      （04 の「権限不足の案内」）。送り先だった「通知の履歴」は、承認 URL を運ぶ値が
      無いまま済んだので、ここへ移した
- [ ] 既定リモートが GitHub でないときの PRs ペインの文言 — いまは
      `TargetState::Unavailable` でも `Cmd::ResolveAccount` が出ないため
      `AccountState::Resolving` のままで、ペインが**永久に「読み込み中…」**になる
      （理由を出せるのは `A` だけ）。塞ぐには枠の内側 30 セル（80x24）に収まる文言が
      要るので、**M6 の「端末幅 < 100 のレイアウト」で案内を畳む形を決めてから**
      （01 の「PR ペイン」）。**理由そのものは `AppState::github_block` が値で持って
      いる**ので（`r` の通知が読んでいる）、残るのは枠の内側 30 セルに畳む仕事だけ
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

- [x] `git_read::in_progress` の検出とステータスバー表示。**`ProgressDetected` と
      `ExecDone.progress` は消した** —— 検出は `status()` が同じ読み取りで行い、
      `WorktreeSnapshot.progress` として `LoadStatus` の返りに載る（起動時も書き込みの
      あとも同じ入口。古い返りは世代が捨てる。01 の「起動時の読み込み」、03）。
      **検出表は実測で作り直した**（02）—— `-i` の有無は区別できない
      ので `Rebase.interactive` を消し、`git am`（同じ `rebase-apply/`）を `Am` に分け、
      `sequencer/` だけが残る形を拾う。バッジに止まった理由（`(edit)`）は出さない（07）
- [ ] `--sequence-editor` / `--commit-editor` 隠しサブコマンド
- [ ] `RebasePlan::to_todo` + テスト、`Modal::RebaseEditor`
- [x] 進行中操作があるときの書き込み `GitOp` 拒否。**continue / abort より先に入れる**
      （2026-09-27 に順序を入れ替えた）。**断るのは HEAD か作業ツリー全体を動かすもの
      だけ**（03 の「進行中に通す書き込み」）
- [ ] rebase の continue / skip / abort。**`m` で開くメニュー**（lazygit と同じ）に
      進行中操作の種類で絞った項目を出し、abort は `Modal::Confirm` を通す。**merge の
      abort（`GitOp::MergeAbort`）もここで足す** —— 書き込みの拒否が switch / pull を断るので、
      無いと merge の途中から mewgit で抜ける手が commit しか無い。am は端末の
      `git am --continue` / `--abort` を案内するだけ（2026-09-27 にユーザーと決めた。01 の
      「進行中操作があるときの書き込み」）。**#120 が残した穴もここで塞ぐ** ——
      rebase 中の対象ブランチの改名（`r`）は git が `is being rebased` で断り、打った名前が
      消える。`InProgressOp::Rebase` に元のブランチ（`rebase-merge/head-name` /
      `rebase-apply/head-name`。どちらも `refs/heads/<name>` の 1 行、git 2.55.0 で確認）を
      持たせ、`refusal` がそのブランチの `BranchRename` / `BranchDelete` を断る
- [ ] cherry-pick（単一 / 複数）と continue / abort
- [ ] revert（mainline 選択）と continue / abort
- [ ] `ui::panes::stash` — 一覧 / diff / push / apply / pop / drop、部分 stash

## M6: 仕上げ

- [x] ステータスバーのキーヒント。**M4 の残りより先にやる**（2026-09-19 に決めた）。
      フッターを **2 行**にして 1 行目を通知、2 行目をフォーカス中のペインのキーにする。
      **キーの一覧は `app` に置く唯一の表**で、下の `Modal::Help` と `config.keymap` が
      後からその同じ表を読む —— 07 が「ヒントを先に入れない」と書いているのは
      **一覧をアドホックに書くと 2 本目になる**からなので、**表を先に作れば束は解ける**。
      **着手したらまず 07 の「ステータスバー」と 01 の「失敗の案内文」を直す**
      （設計書が先。`CLAUDE.md`）。決めたこと ——
      - **フッターは固定 2 行。** 高さを内容で変えると `Msg::Layout` 経由でグラフの
        行数が動き、読み直しが走る。本文は 1 行減る（80x24 でグラフ 20 → 19 行）
      - **共通キー（`f` `p` `P` `A` = 31 セル）は幅が余るペインでだけ出す。**
        着手前の見積りは `x:discard` 形のラベルで測っていて（Diff 53 / Branches 59）、
        **出荷したラベルでは違う**（Diff 50 / Branches 68）。**実測の表は 07 の
        「ステータスバー」の 1 本だけに置いた** —— 2 箇所に数を書くと片方が腐る
      - **`?:help` はまだ出さない**（押しても何も起きない案内を先に出さない。07）
      - **案内文の予算は 72 → 80（= 端末の幅）にした。** 72 の根拠は「同じ行の右端に
        `q:quit` がいるから」で、ヒントを 2 行目に移した時点で消えている。
        `exec_advice` / `A` の通知 / PR の集計が回す `<= 72` は
        `test_support::NOTICE_CELLS` の 1 本に寄せ、`ui` 側の見張りは
        `a_notice_of_the_full_width_fits_on_its_own_line`（リテラルで 40 文字）に直した
      - **表は `update/*.rs` の `match key.code()` に対する 2 本目の一覧になる。**
        嘘にならないよう、**表の全キーをそのペインで実際に押して無反応でないことを見る**
        テストを見張りに置く（`every_hinted_key_does_something`。表に無いキーを足す /
        `match` の腕を消す、の両方向で落ちることを確かめた）
      - **`Tab` / `Shift+Tab` / `Ctrl-C` は表に入れなかった。** 2 行目に描かないものを
        入れると誰も読まない行になるので、下の `Modal::Help` の行へ送った
- [ ] `Modal::Help` — 上の表の全部を出す。**`?:help` をフッターに出すのもここ**。
      **`app::keys` の表に `Tab` / `Shift+Tab` / `Ctrl-C` を足すのもここ** —— いまの表は
      フッターの 2 行目に描くぶん（ペインのキー・共通キー・`q`）しか持っていない。
      `Ctrl-C` は「`Exec` が走っていれば中断、そうでなければ終了」の 2 つを兼ねるので、
      1 行の説明をどう書くかもここで決める
- [ ] `config.keymap` の上書き — パーサの解析先は crossterm の `KeyEvent` ではなく
      core の `app::Key`（08）。**上の表とキーの dispatch が同じ上書きを読む**
      （2 本に割ると、ヒントだけが既定の字を出す。07 の「ステータスバー」）。
      **塞ぐのに要るのは、表へアクションの同一性を足すこと** —— いまの `app::keys` は
      キーと表示の文言しか持っておらず、TOML のアクション名（`fetch = "f"`）と
      突き合わせる鍵が無い（**`label` は鍵にならない**。`移動` / `破棄` はどの
      アクション名とも一致しない）。ペインごとの `enum Action` を表に持たせ、
      `*_key` を「表で `Key → Action` を引いて `match action`」にすると、
      **キーヒントの見張り（`every_hinted_key_does_something`）も
      `hinted_state` も要らなくなる**（表と dispatch が 1 本になるので、網羅 `match` が
      両方向を見る）。`KeyHint::keys` が `&'static` を返さないのはこのため ——
      上書きしたキーは `'static` では持てない。
      **いまの見張りが拾えない形も同じここで消える** —— キーの動作が丸ごと通知に
      退化した形（`x` が理由を出すだけになる）や誤配線（`x` が stage をする）は
      「無反応でない」で通る（レビューが変異で確認した。受け入れた穴）
- [x] フォーカス中のペインとタブの選択中表示をわかりやすくする。**いまはペインが
      枠線の `Modifier::BOLD` だけ（`ui::pane_block`）、右上のタブは非選択側を
      `DIM` にするだけ（`panes::tab_style`）で、どちらも「選択中を強調する」形に
      なっていない。** `BOLD` を太字にしない端末や `DIM` を無視する端末では差が出ず、
      **ユーザーから「判りづらい。bold にするだけじゃ足りない」と指摘が出ている**
      （2026-09-09）。一覧のカーソル行は `theme::selected`（`REVERSED`）なので、
      **強め方を足すなら受け口は `theme`**（色名をペインに直接書かない。07）。
      **#54 の見張り（`the_focused_pane_is_the_only_one_with_a_bold_border`）は
      枠の太さを巡回候補ぜんぶで見る**ので、見せ方を変えるならあの腕が何を守って
      いたかを読んでから書き換える。**上の「ステータスバーのキーヒント」が入ると、
      フッターの 2 行目がフォーカス中のペインを別の形でも示す**ので、どちらを先に
      やっても要求が変わらないかを突き合わせる（キーヒントのほうが先に入る）
      - **決めた形**（2026-09-26 にユーザーと決めた。理由は 07 の「ペインとフォーカス」と
        「右上のタブ」）—— フォーカス中のペインは**太線（`BorderType::Thick`）+
        `theme::focused()`（シアン + 太字）**、タブは出ているほうを**前後 1 空白ごと
        反転**（`theme::current_tab()`）、出ていないほうは `DIM` のまま。**色は ANSI の
        名前付き色**なので端末の light / dark に追随する（`ESC[38;5;6m`。生出力で確認）
      - #54 の見張りは `the_focused_pane_is_the_only_one_with_a_thick_border` に改め、
        **線の字・色・太字**を見る（1 つずつ外して落ちることを確かめた。期待値を
        `theme::focused()` から取ると色を外しても通るので直書き —— `/code-review` が拾った）
      - モーダルの枠（`modals::block`）は `BOLD` のまま。表示中はキーを全部受けるので
        見分ける必要が無い
- [x] ステータスバー（1/2）— 通知を次の入力で下ろし、積まれた時刻を添える。
      **2 本に割った**（2026-09-26）。1 本目だけで「古いメッセージが残り続けて判り
      づらい」（2026-09-09 の要望）は解け、履歴は表示の器（モーダル）を足す別の関心事
      なので。**時間で消す形・未読バッジ・常駐の履歴エリアは採らなかった**（理由は
      01 の「通知」）。時刻は `HH:MM:SS` を前に添えるので、**案内文の予算は 80 → 71 セル**
      （溢れたら `…`。07 の「ステータスバー」）
- [x] ステータスバー（2/2）— 通知の履歴を見るモーダル。**時刻は日付まで**
      （`ui::time::format`）出し、**`Notice.detail`（argv と stderr）もここで出す。**
      いまは要約行しか描かないので、git が断った理由（`would be overwritten`、
      未マージのときの `-D` のヒント）が画面に出ない。不変ルール 4 が「UI まで届ける」と
      言っているのはこの詳細のこと。**`Modal` に変種を足したら `_` の腕を持つ `match` を
      grep する**（`update::paste` が受け皿を持っていた。`.claude/rules/rust-coding.md`）。開くキーは
      フッターの表（`app::keys`）に載せ、`every_hinted_key_does_something` を通す
      - **`L` を共通キーの末尾に足した**（2026-09-26 にユーザーと決めた）。あわせて
        **共通キーを丸ごとではなく末尾から 1 つずつ落とす**形にした —— 丸ごとだと 80 桁の
        Graph から `f` / `p` / `P` / `A` まで消えた（`/simplify` の altitude が拾った）。
        80 桁の Graph で落ちるのは `L` だけで、`L` の案内が 80 桁で見えるのは
        CommitDetail だけ（実測は 07 の「ステータスバー」）
      - **stderr のタブは空白に展開する**（ratatui が描かないので字下げが消える。実物を
        描いて判った。07 の「NoticeLog モーダル」）
      - **中身のスクロールは入れていない**（入りきらなければ最下行を `…`）。受け入れた穴
- [ ] HEAD の追跡設定を 1 本にする。**いま `branch.<HEAD>.remote` は 2 箇所にある**
      —— `RepoInfo.head`（`HeadInfo::Branch.upstream`。PR の座り先と PR 作成の
      `head` が読む）と `BranchState::head()` の `Branch.upstream_config`（`f` と push の
      行き先が読む）。**`HeadInfo` に `UpstreamConfig` を対で載せるところまでは
      PR チェックアウト（2/2）で済んだ**（`merge` の読み手が来たため）。残るのは
      `f` / push の読み手を寄せること。
      **見返りは `update::default_remote` の 2 つのガード** ——「リモート一覧が空のあいだ
      1 段目を飛ばす」「ブランチ一覧がまだなら飛ばす」——で、どちらも追跡設定を
      `remotes` と別のジョブから取っていることだけが理由なので消えるはず
      （`/simplify` の altitude が挙げた。**こちらでは数え直していない**）。
      **`Branch.upstream_config` を消せるかは別の問い** —— あちらの読み手は ahead /
      behind と追跡 ref も同じ行から取っており、出どころを混ぜると到着順の問いが
      戻るかを数えていない
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
      **`render_rows` を見える行だけ組む形にするのもここ**（07 の「見えない行の
      組み立て」）。`NoticeLog` は一覧に加えて**中身（上限の無い stderr）の全文**も
      毎フレーム組むので、中身も「見える行 + 1 行」で打ち切る（detail 5000 行で
      1 フレーム 7.1ms。実測は 07）
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
      いまの `A`（候補の取り直し）では拾えない**。
      **モーダルを読むテストを枠線の字から外すのもここ** —— `account_picker` /
      `notice_log` / `pr_create` と `ui` のフッターのテストは、端末 1 行を `│` で割るか
      剥がすかして本文を取り出しており、**このタスクで `pane_block` に `Padding` を足す・
      `modals::block` を変えると割れる**（#117 で `┃` を足して継いだのが 1 回目）。
      落ちるときは assert が落ちるので黙っては通らないが、無関係なテストを毎回直すことに
      なる。塞ぐのに要るのは、`test_util::pane_text` のモーダル版（内側の `Rect` で
      バッファを切り出す `modal_text`）。**内側の `Rect` を状態から出す関数がまだ無い**
      —— 位置は各モーダルの `render` の中で `modals::place` が本文の行数から決めるので、
      `place` が返す `body` をテストから拾える形（`render` が `Rect` を返す等）が先に要る
      **ヘッダの落とす順序もここで決める**（07 の「ヘッダ」）—— 1 行を取り合うのは
      `mewgit` / `owner/repo` / `[gh: …]` / `[api: …]` と、同じ領域に右寄せで重ねる
      スピナーの 5 つ。**`[api: …]` だけは既に自分で譲る**（`ui::quota_fits`。隙間を
      2 セル取れなければ丸ごと落とす）が、`owner/repo` と `[gh: …]` はスピナーに
      食われたまま切れる。塞ぐのに要るのは (a) 譲る順序、(b) `[gh: …]` を丸ごと
      落とすのか短い形（host を落として login だけ）にするのか、(c) 重ねるのをやめて
      領域を割るのか。**測る材料は 07 のヘッダにある幅の表**（左群の長さごとに、
      出るのに要る幅・切れた数が読める幅・`]` だけ欠ける幅）。
      **`[api: …]` が譲る相手はそのフレームのスピナーなので、数が動いているあいだ
      だけ消える帯がある** —— 左群 70 セルなら幅 85〜95 で、`pr checks` が走って
      いるあいだ消えて終わると出る（実測 2026-09-18。07 の「ヘッダ」）。
      **(c) で領域を固定幅に割れば消える**（現在のスピナーに合わせて割る形では、
      ジョブが走った瞬間に左が縮むので帯は残る。**確かめていない** —— (c) 自体が
      まだ形になっていない）。順序だけを決めて終わりにしない
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
      そこで数え直す。**通知履歴では数え直して割らなかった** —— 増えたのは
      `Modal::NoticeLog` の変種 1 つと、`Modal::move_cursor` を `AppState::move_modal_cursor`
      へ移したことだけで（一覧の長さを `AppState.notices` が持つため）、`Modal` が
      独立して育ったとは言えない
- [ ] `git_exec::argv` の `VARIANT_COUNT` を消す。**`variant()` の網羅 `match` から
      離れた場所に変種の本数のコピーが置いてある**（`.claude/rules/rust-coding.md` の「離れた場所に、
      コードの『いま N ある』を書かない」）。**ただの削除では済まない** ——
      `covers_every_variant` は「`to_argv` は全 `GitOp` をカバーする」（同じく `.claude/rules/testing.md`）
      の見張りなので、消すとカバー要求のほうが破れる。塞ぐ形は、各変種の代表値を返す
      網羅 `match`（`fn every_variant() -> Vec<GitOp>`）を 1 本置き、`cases()` が
      その集合を全部含むことを `BTreeSet` で比べること —— 変種を足すとそちらが
      コンパイルエラーになるので、数を書かずに漏れが出る
- [ ] `scripts/screen.py` が全角文字のあとで 1 桁落とすのを直す。**桁を根拠にする
      確認（幅の予算・余白・隣との隙間）で嘘になる** —— 空白 1 つが消えて隣の語と
      地続きに見える（実測は `.claude/skills/pty-check/SKILL.md`）。いまは生の
      `ESC[<行>;<桁>H` を読んで回避している
- [ ] 外部変更検知。**進行中操作も `LoadStatus` を出せば入れ替わる**
      （`Msg::ProgressDetected` は消した。01 の「update の原則」）。**見るのは
      `.git/HEAD` だけでは足りない** —— cherry-pick / merge / revert を外で終えても
      HEAD の mtime は変わらない。**ワークツリーごとの gitdir（`RepoReader::git_dir()`）
      と `index` の mtime** なら 8 通りとも変わる（実測の表は 01 の「進行中操作が
      あるときの書き込み」）。**これで #120 が残した「外で終えた直後の 1 回目は断られる」
      が塞がる** —— 見張りは「外で `rebase --abort` / `merge --abort` したあと、キーを
      押す前に監視の `LoadStatus` が届き、1 回目の `Enter` が通る」
- [ ] テストで status を届ける 3 行（`reply(state, Cmd::LoadStatus, Msg::StatusLoaded(..))`）を
      `test_support` のヘルパ 1 本に寄せ、`worktree_state` もそれを呼ぶ形にする。いまは
      diff / graph / exec / worktree / mod / progress の各テストにインラインで散っている
      （#120 の `/simplify` が拾い、範囲外として見送った）。`WorktreeSnapshot` に
      フィールドが増えたときに直す場所が 1 つになる
- [ ] README、リリースワークフロー（`cargo dist` or release-please）
