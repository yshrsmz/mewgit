# 05. github: API クライアント

## 責務

- PR・チェック・レビュー状態の取得
- PR 作成
- PR チェックアウトに必要な head ref / fork 情報の取得

`gh api` は使わず、`reqwest` で直接呼ぶ。理由: 型付きレスポンス、エラーヘッダの取得、並列呼び出しの制御。

## クライアント

```rust
pub struct GithubClient { http: reqwest::Client, base: Url, token: SecretString }

impl GithubClient {
    pub fn new(host: &str, port: Option<u16>, token: SecretString) -> Result<Self>;      // 実装済み
    pub async fn repo(&self, owner, repo) -> Result<RepoMeta>;                          // 実装済み（probe 兼用）
    pub async fn checks(&self, owner, repo, sha: &str) -> Result<Vec<Check>>;           // 実装済み
    pub async fn reviews(&self, owner, repo, number: u64) -> Result<ReviewSummary>;     // 実装済み
    pub async fn pr_list(&self, owner, repo) -> Result<Vec<PullRequest>>;               // 実装済み
    pub async fn create_pr(&self, owner, repo, input: &CreatePr) -> Result<PullRequest>;     // 実装済み
}
```

- **`new` は `Result`。** ベース URL を組めないホストがある（下の「ベース URL」）。
- **`pr_list` は絞り込みを引数に取らない。** `state=open` / `sort=updated` /
  `direction=desc` は常に同じで、**誰の PR か**の振り分けは `app` が
  `user.login` と `requested_reviewers` から行う（07 の「PRs ペイン」）。
  設計に書いていた `PrFilter` は**作らない** —— 絞り込みがサーバ側に無い以上、
  引数にすると「API に渡る」と読める名前だけが残る
- **`pr_for_branch` は消した**（`ui::panes::prs` の 2/2）。PRs ペインが
  「HEAD のブランチの PR 1 件」から「open な PR の一覧」に変わって読み手が
  いなくなったため。**fork の PR は `pr_list` にそのまま出る** ——
  `/repos/{o}/{r}/pulls` は fork からの PR も返し、`head.repo.owner` が fork 側の
  owner を運ぶ。`head` を `{owner}:{branch}` に組む場所ごと消えたので、
  `head_owner` を引数で運ぶ話も無くなった
- **`create_pr` だけが POST。** リクエストを送る 1 箇所（`send`）はそのままで、
  `RequestBuilder` を組むところだけが違う。**permit（`PERMITS`）も残量の記録も
  そこに在る**ので、POST を別の経路で送ると扇形と同時に飛んだときに同時本数の
  上限をすり抜け、残量も採れなくなる。
- **`create_pr` は返った `PullRequest` をそのまま返す。** 一覧（`pulls`）と同じ形が
  返るので `WirePull` を使い回す。**読む側は番号しか使わない**（01 の
  「PR 作成（`N`）」がカーソルを座らせる）が、**`()` に落とさない** —— 落とすと、
  作った PR が一覧に出るまで画面に何も出せない。
- **重複した PR と、リモートに無いブランチは 422。** どちらも `GithubError::Api`
  （`status: 422` と本文の先頭 200 文字）に落ちる。**腕を足さない** ——
  GitHub は同じ 422 に十数通りの `errors[].code` を載せるので、腕にすると
  「どれが来たか」を写した表を持つことになり、載らなかった形が黙って
  `Api` へ落ちる。**本文は握りつぶさず通知の `detail` に出す**（不変ルール 4 と
  同じ考え方）。
- **`repo` は probe 兼用**なので、返り値を捨てて「届いたか」だけを見る呼び出しがある。
  probe 専用に `()` を返す関数は作らない。**`auth::resolve` が直に呼ぶのではない**
  —— `github` は `auth` の上段なので、`auth::Backend` を実装する側
  （`github::AccountBackend`）がここを呼んで `Result<(), auth::AttemptFailure>` に落とす。
  `GithubError` の各腕をどちらに割るかの表は 04 にある。**`Decode` は `Ok` 側**
  （`check_status` を通ったあとにしか出ないので 2xx が返っている）。
- `RepoMeta` は `{ owner, repo, default_branch }` の 3 つだけ。`private` / `fork` /
  `parent` は読む側が出てから足す。

- ヘッダ: `Authorization: Bearer`、`Accept: application/vnd.github+json`、`X-GitHub-Api-Version`、`User-Agent: mewgit/<ver>`
### ベース URL

**形が 2 つある。** github.com とデータレジデンシー（`*.ghe.com`）は
`https://api.{host}`、self-hosted の GHES は `https://{host}/api/v3`
（`gh` のバイナリに `https://%s/api/v3/` があるのを `strings` で確認）。

**分類より前に「ホスト名だけか」を確かめる。** 順序が逆だと `foo/bar.ghe.com` が
`.ghe.com` で終わってデータレジデンシー扱いになり、`https://api.foo/bar.ghe.com/`
という**動くが違う場所を指す**ベースになる。

**述語は `auth::url::valid_host` を呼ぶ。書き直さない。** 00 の依存の向きで
`github` は `auth` の上段なので参照してよい。自前で `Url::parse` して host / port /
path を突き合わせる形も書けるが、それは `-4` も `-oProxyCommand=id` も `[::1]` も
通す —— **`-` 始まりを弾くのが眼目**だと 04 が名指ししている、まさにその形。
`InvalidHost` がここに要る理由（`ssh -G` の `hostname` は `RemoteUrl` の検査を
通っていない）と、その穴を塞ぐ述語は同じ 1 本でなければ噛み合わない。

**ホストの大小は比較のときだけ無視する。** 組んだ URL の小文字化は `Url` が行う
（WHATWG URL の host 正規化）ので、こちらでは触らない —— 二重に正規化すると
出どころが 2 本になる。`RemoteUrl` は host を小文字化していない。

**port は `new(host, port, token)` で受ける。** `auth::RemoteUrl` が
`RemoteScheme::Https { port }` として運ぶようになった（04）。**受け取るのは
https の port だけ** —— ssh の port は API の port ではないので、`RemoteUrl` の
段階で型から消してある。`Option<u16>` の `None` は「既定（443）」で、
`RemoteUrl` が 443 を `None` に畳んでいるので `:443` が URL に出ることはない。

port はパスの形と直交する。`{host}/api/v3` の側に付ければ
`https://ghe.example.com:8443/api/v3/`、`api.{host}` の側なら
`https://api.foo.ghe.com:8443/` になる。**後者は確かめていない**（github.com も
`*.ghe.com` も SaaS で 443 固定なので実物を用意できない）。**「パスの形を落として
port だけ足すと self-hosted では全部 404」という警告は、パスの形を先に入れた
ことで解けている。**

### TLS とルート証明書

**`reqwest` は 0.13.5**（#81 の lock file maintenance で 0.13.4 から上がった）。
feature は `json` / `query` / `rustls`。

- **`rustls`**（aws-lc-rs + `rustls-platform-verifier` = OS のトラストストア）。
  0.12 の `rustls-tls`（同梱の webpki ルート）は消えた。**同梱ルートには戻さない**
  —— GHES の社内 CA が効くのは OS のトラストストアのほうで、
  `webpki-root-certs` + `ClientBuilder::tls_certs_only` にすると社内 CA で落ちる
- **`query`** は `?per_page=100` に要る（0.13 で opt-in になった）
- **`system-proxy` は入れない。** 環境変数のプロキシ（`HTTP_PROXY` / `HTTPS_PROXY` /
  `ALL_PROXY` / `NO_PROXY`）は feature に関係なく効く（hyper-util 0.1.20 の
  `client/proxy/matcher.rs` の `Builder::from_system` が常に読み、feature が足すのは
  macOS / Windows の **OS 設定**だけ。実装を読んで確認）。入れると 6 crate 増え、
  MVP 外の Windows 向け `windows-registry` と `core-foundation` の 2 版目が入る。
  **macOS / Windows の OS 設定のプロキシが要る形が出てきたら足す**

実測（2026-09-09）。

| 測ったもの | 値 |
|---|---|
| lock に増えた crate | 26（`rustls-platform-verifier` が OS ごとの実装を連れてくる） |
| macOS のビルド（依存だけ再コンパイル） | 17.2 秒 |
| Linux（`ubuntu-latest`）の CI ジョブ | 1 分 45 秒。**`aws-lc-sys` の build.rs は追加セットアップなしに通る** |

**`aws-lc-rs` / `aws-lc-sys` の版は lock でしか固定していない。** `Cargo.toml` に
書いているのは `reqwest` だけで、そこから引かれる `aws-lc-*` は `^1.x` / `^0.x` の
範囲。14 日ルールを手で数えたのは**この PR で新規に入ったときだけ**で、以後の更新は
Renovate（`minimumReleaseAge: "2 weeks"`）に任せる。**人が
`cargo update --workspace` を叩いた場合は誰も担保しない。**

**任せた先にも穴がある。** Renovate の `lockFileMaintenance` は
`minimumReleaseAge` を継承しないので、**`Cargo.toml` に書いた直接依存でも、
キャレットの範囲内なら公開 14 日未満で lock に入る**。#81 が `reqwest` を
0.13.5（公開 2026-09-08 20:55 UTC）に上げたのがそれで、2026-09-17 の時点で
8.6 日だった。**担保しているのは「直接依存かどうか」ではなく、lock を書き換える
側の設定のほう。**

- **認証付きリクエストは同時 4 本まで（`Semaphore`）。**
  **置くのはクライアントの中で、`Cmd` を N 本出す形にはできない** —— `JobTracker` は
  種別単位なので、同じ `JobKind` の 2 本目以降は `AppState::issue` が黙って落とす
  （01 の「実行中ジョブ」）。**扇形は 1 ジョブ**（対象の `Vec<(number, sha)>` を運ぶ
  `Cmd` 1 本）にして、その中で `Semaphore` が同時本数を抑える。
- **permit を取るのは HTTP が実際に出る 1 箇所（`get_page`）だけ。** PR ごとにも
  取ると**デッドロックする** —— `checks` は `try_join!` で check-runs と commit status
  の 2 本を張るので、外側の permit を 4 本が握ったまま内側の 2 本が permit を待ち、
  誰も返さない。**「上限 4 の内側だから入れ子で詰まらない」は成り立たない**
  （詰まるのは本数ではなく、**permit を握ったまま permit を待つ**形のほう）。
  取る場所が 1 箇所なら、`try_join!` の 2 本はどちらも**何も握らずに**待つ。
  **実測（2026-09-18）** —— `pr_detail` の先頭にも `acquire()` を足すと、6 件 ×
  3 リクエストの扇形が **20 秒のタイムアウトまで 1 件も返らなかった**
  （`the_fan_out_does_not_put_more_than_four_requests_in_flight`）。
  permit を外したほうの変異では 18 本が一斉に届いて同じ見張りが落ちる。
  **どちらも「そうなるはず」ではなく、実際に踏んで確かめてある。**
- **`Semaphore` はプロセス全体に 1 つ**（`shared_http()` の隣に `static`）。
  `tokio` 1.53.1 の `Semaphore::const_new` は `#[cfg(not(all(loom, test)))]` だけが
  付いた `pub const fn` なので `static` にそのまま置ける（`OnceLock` は要らない。
  実測 2026-09-18、`tokio-1.53.1/src/sync/semaphore.rs`）。
  **`GithubClient` のフィールドにしない** —— `runtime::github_for` が dispatch の
  たびにクライアントを組み直すので、1 ジョブぶんしか抑えられない（一覧の引き直しと
  扇形が並ぶと 4 + 4 本になる）
- **扇形は毎回引き直さない。鍵は `(番号, head sha)`。** PR 1 件に 3 リクエスト
  （check-runs / commit status / reviews）かかるので、ref を動かす書き込みのたびに
  全件引くと **1 + 3N** になる。open PR が 60 件のリポジトリ（`cli/cli` は 62 件。
  下の実測）では 1 回の commit が **181 リクエスト**で、`core` の 5000/時は
  **27 回で尽きる**。`f` を 1 分おきに押すだけで超える。**checks は sha に紐づく**
  ので、`(番号, head sha)` が変わっていない行は憶えた集計をそのまま使う。
  **そもそも引くのは PRs タブが見えているあいだだけ**（門は 01 の「PR ペイン ›
  集計」）—— ここの 3N は**タブを開いている人が払う額**で、開かない起動で出るのは
  一覧の 1 本だけ（起動そのものにはアカウント解決ぶんの往復も乗る。実測は plan.md の
  「PRs タブを開いていない起動で集計を引かない」の行）。
- **reviews は sha に紐づかないので、この記憶では古くなる。** sha が動かないまま
  レビューが付いても、自動の引き金は動かない —— `f` は ref を動かすが PR の head sha は
  変えないので、鍵は「引き直せ」と言わない。**塞いだのは `r`**（PRs ペインでカーソル行の
  記憶を落として引き直す。01 の「引き直す（`r`）」）。**落とすのがカーソル行だけなのは
  ここの 1 + 3N が理由** —— 全件落とすと、押すたびに上と同じ 181 リクエストを払う。
- **「どの対象でも同じように起きる」失敗を受けたら、残りは頼まない**（扇形は
  `GithubClient::pr_details`）。同じ失敗を N 回買ううえ、レート制限では下の
  「二次制限」を踏みに行く。打ち切ったことは残りの行に理由として出す（黙って
  「まだ引いていない」のまま残さない）。
  **どれがその失敗かを答えるのは `github`**（`GithubError::repeats_for_every_target`）
  —— レート制限だけでなく 401 / 403 もそう。呼ぶ側で `matches!` を書くと、腕を
  足したときに選び直す機会が無い。
- **止めているのは「抜けること」で、同時本数ではない。** 対象を
  `tokio::spawn` / `JoinSet` に全部渡してから旗で止める形は**1 本も止められない**
  —— 切り離されたタスクは、旗を読み終えたあと permit の列で待ち、順番が来れば
  素直に叩く（実測 2026-09-18。60 件で `issued=60 / skipped=0`）。有界な
  `buffer_unordered` を抜ければ、走っていない future は permit を取りに行かない。
- **同時に走らせる PR も有界にする**（`FAN_OUT`）。理由は 2 つで、対象が多いときに
  future を全部組み立てないためと、**permit の列を短く保つ**ため —— tokio の
  `Semaphore` は FIFO（`tokio-1.53.1/src/sync/semaphore.rs` の「permits are given
  out in the order they were requested」）なので、扇形が全件ぶんの `acquire` を
  並べると、そのあとに押した `f` の一覧取得が列の最後尾に付く。
- **一覧は `Link` の `rel="next"` を辿る。** `per_page=100` で止めない。
  **切り捨てが答えを変えるものがある** —— `reviews` の配列は古い順なので、1 ページ目
  だけ見ると「変更要求のあと承認した人」を `changes_requested` と数える。check-runs も
  マトリクスビルドが 100 を超えると、2 ページ目の失敗が見えないまま緑に見える。
- **辿るのは 10 ページ（1000 件）まで。** `Link` が輪を作っても止まるための上限だが、
  **超えたことは呼ぶ側に伝わらない**（`get_paged` が黙って `Ok` を返す）。「全ページ
  集める」ではない —— open PR が 1000 件を超えるリポジトリでは**いちばん古い側が
  落ちる**（`pr_list` は `sort=updated&direction=desc`）ので、放置した自分の PR が
  一覧から消え、`AppState::current_pr` も当たらなくなる。**画面には何も出ない。**
  打ち切りを知らせる経路を足すかは、実際にそういうリポジトリが出てから決める
  （plan.md）
- **タイムアウトを付ける。** `reqwest` の既定は**無制限**で、GHES が固まると
  OS の TCP タイムアウトまで（分単位）回り続ける。#56 のスピナーは出るが、
  `Ctrl-C` が止められるのは `Cmd::Exec`（git）だけで HTTP は止まらない。
  1 リクエスト 30 秒、接続確立 10 秒
- **`o`（ブラウザで開く）はここを通らない。** PR を開くのは `gh browse` で、
  `PullRequest.url` は使わない（01 の「`o`（ブラウザで開く）」）。不変ルール 7 が
  外部プロセスを `git` `gh` `ssh` に限っているので、URL を自分で開く経路が無い
- **`/search/issues` は使わないと決めた**（`ui::panes::prs` の 2/2）。一覧の絞り込みを
  サーバ側でやれる代わりに、**返るのが issue 形で `head` が無い** ——
  `PullRequest.head.sha` が埋まらないので、チェックを引く鍵を作れない。
  `advanced_search=true` を付けても形は変わらない（実測 2026-09-17、`gh api`、
  github.com。`search_type` は `lexical` のまま、キー集合も同じ）。代わりに
  `/repos/{o}/{r}/pulls` を引き、**誰の PR かの振り分けは手元で行う**
  （`user.login` と `requested_reviewers` が本文に入っている）。
- **レートリミットのバケットは 1 つではない。** `core`（`GET /repos/…` などの REST）と
  `search`（`GET /search/issues`）は別に数え、どちらのものかは応答の
  `x-ratelimit-resource` に出る。実測（2026-09-16 / 2026-09-17、`gh api -i`、
  github.com の個人アカウント）。

  | 叩いたもの | `x-ratelimit-resource` | `x-ratelimit-limit` |
  |---|---|---|
  | `/rate_limit` | `core` | 5000 |
  | `/repos/{o}/{r}/pulls?state=open&per_page=1` | `core` | 5000 |
  | `/search/issues?q=repo:cli/cli+is:pr&per_page=1` | `search` | 30 |

  **いま mewgit が触るのは `core` だけ**なので、ヘッダに出す数も 1 つで足りる。
  **1 回の引き直しで払うのは 1 ページぶん**が普通（実測 2026-09-17: `cli/cli` の
  open PR は 62 件で `Link` の `next` が付かず 1 リクエスト）。100 件を超えると
  ページの数だけ**逐次**に増える。
  **表は過去形の記録として残す** —— `search` を使う経路が戻ってきたときに、
  2 桁小さい上限を数え直さずに済む
- **レートリミット残量は応答ヘッダから採り、`Msg` で `app` へ運ぶ。**
  [`GithubClient`] が `get_page` のたびに `x-ratelimit-limit` / `-remaining` /
  `-reset` を読んで **`Mutex<Option<RateLimit>>`** に畳み、ランタイムが
  `Msg::RateLimitSeen { epoch, seen }` に載せる。**扇形は 1 件ごと、一覧はジョブの
  終わりに 1 回**（01 の「PR ペイン › 残量」）—— 60 件の扇形は数秒かかるので、
  終端まで待つと**いちばん減っているあいだ**ヘッダが古い数を出し続ける。
  **採るのは `check_status`
  の手前** —— 残量 0 の応答は 403 / 429 で返るので、後ろに置くと**いちばん出したい
  数だけが採れない**。
  - **畳み方は `(reset, used)` の大きいほう**（`RateLimit::newer`）。扇形は同時 4 本
    なので、届く順は新しさの順ではない。`used` は `limit - remaining` で出るので
    持たない（同じ窓では残量が減る一方）。**判定は 1 本**で、クライアント
    （同じジョブの中）と `app::PrState`（ジョブをまたぐ）の両方がこれを呼ぶ
  - **バケットが `core` でなければ採らない**（`x-ratelimit-resource`）。`search` は
    上限が 2 桁小さいので、混ざると残量が突然 30 になる（下の表）。**いま叩く
    エンドポイントは全部 `core`** なので、この腕を踏むのは wiremock のテストだけ
  - **ヘッダごと無いこともある**（レートリミットを切った GHES）。そのときは何も
    送らず、`app` は「まだ判らない」のまま（ヘッダに出さない。07）
  - **世代を添える。** 残量は**そのトークンの予算**なので、アカウントを切り替えた
    あとに前のトークンの扇形が返す数は嘘になる。`(reset, used)` は「どちらの応答が
    新しいか」しか答えない（01 の「PR ペイン › 残量」）
  - **運ぶのはランタイム。** `github` は `Msg` を知らない（`pr_details` の `on_each`
    が同期なのと同じ理由）。**見張れるのは「送らない」側だけ** ——
    1 度も見ていなければ何も送らないことは
    `runtime::tests::send_rate_limit_says_nothing_before_the_first_response` が踏む
    （クライアントを組むだけならリクエストは出ない）。**見た数を運ぶ側には見張りが
    無い** —— `GithubClient` は https のベース URL しか組まず（`base_url`）、
    `with_base_url` は `mewgit-core` の中の seam なので、`mewgit-tui` から
    モックサーバへ向けられない。
    両端（クライアントが憶える / `app` が畳んで弾く / `ui` が描く）は見張ってある
- **残量は自分が出したリクエストのヘッダから採る。`/rate_limit` は別の数を返す。**
  実測（2026-09-18、`gh api -i`、github.com の個人アカウント。**同じセッションで
  連続して叩いた**）——

  | 叩いたもの | `remaining` | `used` | `reset` |
  |---|---|---|---|
  | `/rate_limit` | 5000 | 0 | 1789703715 |
  | `/repos/cli/cli/pulls?state=open&per_page=1` | 4984 | 16 | 1789703295 |

  **どちらも `x-ratelimit-resource: core`** なのに数が合わず、`/rate_limit` のほうが
  `reset` を**後ろ**に返した。**`/rate_limit` を残量の産出元にしない**（1 往復を
  余分に払ったうえで嘘を出すことになる）。`x-ratelimit-used` は実在するので、
  並行に飛んだ応答のどれが新しいかは `(reset, used)` の大きいほうで決められる。
  **GHES で同じかは確かめていない**（手元に無い）

## エンドポイント対応

| 機能 | API |
|---|---|
| probe / メタ | `GET /repos/{o}/{r}` |
| PR 一覧 | `GET /repos/{o}/{r}/pulls?state=open&sort=updated&direction=desc`（fork からの PR も返る） |
| チェック | `GET /repos/{o}/{r}/commits/{sha}/check-runs` + `GET /commits/{sha}/status`（両方をマージ） |
| レビュー | `GET /repos/{o}/{r}/pulls/{n}/reviews` を最新レビュー/ユーザーで集約 |
| PR 作成 | `POST /repos/{o}/{r}/pulls` |

| PR ごとの集計 | 上の 2 つを `try_join!` で並べた `pr_detail`（`(番号, head sha)` で呼ぶ） |

GraphQL は MVP では使わない（1 画面に必要な情報は REST 数本で足りる）。

**集計を [`PullRequest`] に入れない。** 一覧（`pulls`）はこれを返さないので、
`Option` で持たせると「まだ引いていない」と「引いたが失敗した」が同じ `None` に
落ちる。段階を持つのは `app::PrState` の側で、失敗の理由（`GithubError`）もそちらが
運ぶ（01 の「PR ペイン › 集計」）。**以前は `review` / `checks` を `Option` で
持っていたが、誰も組まず誰も読まないまま置いてあった。**

## モデル

```rust
pub struct PullRequest { number, title, state: PrStatus, draft, head: RefSpec, base: RefSpec, url, author, requested_reviewers: Vec<String> }
pub struct CreatePr { title: String, body: String, head: String, base: String, draft: bool }  // create_pr の入力。head の組み立ては app（01）
pub struct PrDetail { checks: CheckSummary, review: ReviewSummary }  // pr_detail が返す。PullRequest には入れない
pub enum PrStatus { Open, Closed, Merged }   // app::PrState（画面の状態）と紛らわしいので Status
pub struct RefSpec { repo: Option<RepoRef>, branch: String, sha: String }
pub struct RepoRef { owner: String, repo: String }
pub enum CheckState { Success, Failure, Pending, Skipped, Neutral }
pub struct CheckSummary { success, failure, pending, skipped, neutral }  // CheckState の腕ごとに 1 つ
pub struct Check { name, state: CheckState, url: Option<String> }
```

- **`requested_reviewers` は一覧の本文に入っている。** `/repos/{o}/{r}/pulls` が
  返すので、レビュー依頼の印（07 の `R`）に往復が要らない。**`requested_teams` は
  入れない** —— チーム宛ての依頼を自分宛てに読むには、チームの数だけ
  `/orgs/{org}/teams/{team}/memberships/{login}` を叩くことになる（07 で受け入れた穴）
- **`review` と `checks` はどちらも `Option`。** 別リクエストなので、PR 一覧を
  引いた直後はどちらも未取得。`review` に `ReviewSummary::default()` を入れると
  **「レビュー 0 件」と区別できない**ので、2 つは対称にする。
- **`RefSpec.repo` が `Option` なのは fork の削除を表すため。** GitHub は削除された
  fork の `head.repo` を `null` で返す。空文字で埋めると「owner が空」と区別できず、
  下の PR チェックアウトが**同一リポジトリかどうかを owner の比較で決めている**ので、
  消えた fork を fork 経路へ落とす。

## エラー

```rust
pub enum GithubError {
    InvalidHost { host },                          // ベース URL を組めないホスト
    Unauthorized,                                  // 401
    Forbidden { needed_scopes: Vec<String>, saml: bool, body_head: String },  // 403（SAML / スコープ / IP allowlist）
    NotFound,                                      // 404（権限不足でも返る）
    RateLimited { reset_at: Option<i64> },         // `None` = いつ戻るか判らない
    Network { message: String },
    Decode { message: String },                    // 応答を型に落とせない
    Api { status, body_head: String },             // 本文先頭 200 文字
}
```

- **`Decode` を `Network` と分ける。** GHES の版差でフィールドが欠けた形はここに
  来るので、「通信に失敗しました」と出すと**サーバ互換性の問題を回線の問題として
  調べさせる**ことになる。

- **`Network` は `reqwest::Error` を持てない。** `GithubError` は `Clone + PartialEq +
  Eq` を derive していて（通知に載せて比較するため）、`reqwest::Error` はどれも
  満たさない。文字列 1 本にし、**`without_url()` で URL を落としてから**入れる
  （`Display` が URL を出すことがある）。

403 の本文に `SAML` / `Resource protected by organization SAML enforcement` が含まれる場合は `Forbidden` に `saml: true` を付け、案内を「トークンの SSO 承認」に切り替える。

**レート制限を `Forbidden` より先に見る。** GitHub は使い切った要求に **403** を返す
ことがある（429 のこともある）ので、順序が逆だと「スコープが足りない」の案内が出て、
**待てば直るものを直らないものとして見せる**。

403 をレート制限と読む印は **2 つある**。

- **一次制限**（時間あたりの総量）は `x-ratelimit-remaining` が 0 になる。
- **二次制限**（短時間に叩きすぎ）は**残量を減らさずに** `retry-after` を付ける。
  片方だけ見ていると、二次制限が丸ごと `Forbidden` に落ちる。

どちらの印も無い 403 はスコープ不足なので `Forbidden` のまま。

**回復時刻は `retry-after` を優先する。** あれが付いているのは二次制限で、
`x-ratelimit-reset`（一次制限の窓。1 時間先のこともある）は無関係か、そもそも
付いていない。**どちらも無いときは `None`。0 で埋めない** —— 1970 年になり、
表示側は「もう明けている」と読んで即座に叩き直し、制限を踏み直す。

**`InvalidHost` は入口の守りとして残す。** `ssh -G` の `hostname` は産出側
（`auth::ssh`）で `valid_host` を通すようになったので、**mewgit の中の経路からは
もう届かない**（04）。それでも消さないのは、`GithubClient::new` が `pub` で、
ここが**回復先の無い最後の関門**だから —— 消すと `https://api.foo/bar.ghe.com/` を
組む経路が呼び出し側の数だけ開く。産出側の検査は「正規化が成功したか」（失敗しても
`host_raw` で続けられる）を答えており、**同じ述語で別の問いに答えている**。

**`body_head` は文字数で切る。** バイトで切ると多バイト文字の途中で落ちて panic する。

## PR チェックアウト

PRs ペインの `Enter` でカーソル行の PR をローカルのブランチに取り出す（01 の
「PR チェックアウト（`Enter`）」）。**API は 1 本も叩かない** —— 材料は一覧が既に
運んでいる `head`（branch / repo）と番号で足りる。

**`gh pr checkout` と同じ形に揃える。** 同じリポジトリで両方を使ったときに、
片方が作ったブランチをもう片方が別物として扱わないため。実測（2026-09-23、
gh 2.101.0 / git 2.55、`sharkdp/hyperfine` の一時クローン。`GIT_TRACE=1` で
gh が叩いた git を拾った。`maintenance` と partial clone の遅延取得は除く）:

| 場面 | gh が叩く git | `.git/config` に書くもの |
|---|---|---|
| 同一リポジトリ、ローカルに無い | `fetch origin +refs/heads/<b>:refs/remotes/origin/<b> --no-tags` → `checkout -b <b> --track origin/<b>` | `--track` が書く `remote` / `merge` |
| 同一リポジトリ、ローカルに在る（いま居るかは問わない） | 同じ fetch → `checkout <b>` → `merge --ff-only refs/remotes/origin/<b>` | なし |
| fork、ローカルに無い | `fetch origin refs/pull/<n>/head:<b> --no-tags` → `checkout <b>` → `config` × 3 | `remote` と `pushRemote` に **fork の URL**、`merge = refs/heads/<b>` |
| fork、ローカルに在る（いま居ない） | 同じ fetch → `checkout <b>` | なし |
| fork、いま居る | `fetch origin refs/pull/<n>/head --no-tags` → `merge --ff-only FETCH_HEAD` | なし |
| fork、head が base の既定ブランチと同名（`master`） | ブランチ名を **`<owner>/master`** にして上と同じ | 下の「追跡先」 |

- **ローカルのブランチが PR と分かれていると止まる。** fetch の refspec に `+` が
  無いので `! [rejected] … (non-fast-forward)` で exit 1、gh もそこで止まる
  （以降の `checkout` は叩かない）。**上書きしない**のは、ローカルにしか無い
  コミットを黙って消さないため
- **いま居るブランチへは `<src>:<b>` で fetch できない**（`fatal: refusing to fetch
  into branch 'refs/heads/<b>' checked out at …`、exit 128。`--update-head-ok` を
  付けると通るが、ref だけ動いてワークツリーと index が置き去りになる）。
  **だから「いま居る」だけ `FETCH_HEAD` を早送りする形**に割れている

### mewgit の手順

gh の表から 3 つ変える。

- **`checkout` ではなく `switch`**（03 の「ブランチの切替は `checkout` ではなく
  `switch`」）。`checkout -b <b> --track <start>` は `switch --track --create <b>
  <start>` にする。**`--track` は明示する** —— `branch.autoSetupMerge` を `false`
  にしている環境では既定で入らず（git 2.55 で確認）、上流の無いブランチができると
  Branches ペインに ahead/behind が出ず、push でも `-u` が要る
- **fork の追跡設定は `switch` より前に書く。** gh は `checkout` のあとに書くので、
  **切替が断られると（ワークツリーの変更がぶつかる）追跡設定の無いブランチが残り**、
  次の回は「ローカルに在る」側へ入って二度と書かれない。先に書けば、切替が
  断られてもブランチは完成している
- **同一リポジトリで「在る」ときは、fetch の refspec でローカルのブランチも
  早送りする**（`+` を付けない 2 本目の refspec）。gh は切り替えてから
  `merge --ff-only` するので、**分かれていると切り替えたあとで止まり**、失敗の
  通知が出ているのに HEAD だけが動く（一時リポジトリで確かめた）。refspec なら
  分かれていれば fetch が断り、切り替えまで進まない（remote-tracking ref のほうは
  `+` 付きなので同じ fetch の中で更新される）。fork の「在る」と同じ形になる

| 場面 | 手順 |
|---|---|
| 同一リポジトリ、無い | `fetch --no-tags <r> +refs/heads/<b>:refs/remotes/<r>/<b>` → `switch --track --create <b> <r>/<b>` |
| 同一リポジトリ、在る | `fetch --no-tags <r> +refs/heads/<b>:refs/remotes/<r>/<b> refs/heads/<b>:refs/heads/<b>` → `switch <b>` |
| 同一リポジトリ、いま居る | 同じ fetch → `merge --ff-only refs/remotes/<r>/<b>` |
| fork、無い | `fetch --no-tags <r> refs/pull/<n>/head:refs/heads/<b>` → `config --local -- branch.<b>.remote <u>` → `… .pushRemote <u>` → `… .merge <m>` → `switch <b>` |
| fork、在る | 同じ fetch → `switch <b>` |
| fork、いま居る | `fetch --no-tags <r> refs/pull/<n>/head` → `merge --ff-only FETCH_HEAD` |

`<r>` は **`RepoInfo.target` を決めたリモート**（`conventional_remote`。PR 作成の
base と同じ）。gh は `origin` と書いてあるが、一覧は `target` のリポジトリの PR
なので、そのリポジトリを指しているリモートから取る。

**「同一リポジトリか」は `head.repo` と `target` の `owner` / `repo` の一致で決める**
（上の「モデル」の `RefSpec.repo`）。**大文字小文字は区別しない**（`RepoRef::same_as`。
URL の綴りと API の綴りが違うことがある）—— 現ブランチの PR（01）も同じ比べ方を
通るので、取り出した行に座らない形は作らない。**消えた fork（`head.repo` が `null`）は fork**。

**「在る / いま居る」は `BranchState` の一覧で決める。** 一覧がまだ届いていなければ
押した時点で断る。一覧が古くても**壊れない** —— 「無い」と読んで在った形は
`switch --create` が `already exists` で断り、fork の fetch は `+` が無いので
早送りできなければ断る。

**同じ名前のブランチが在るとき、それが同じ PR のものかは確かめない**（gh も
確かめない）。早送りできれば早送りして切り替える。

#### ブランチ名

**同一リポジトリは `head.branch` そのもの**（`--track` が `<r>/<b>` を追うので
名前がずれると追跡先と食い違う）。**fork も `head.branch`** だが、**base の既定
ブランチか PR の base ブランチと同名なら `<owner>/<branch>`**。gh が `<owner>/` を
付けたのは既定ブランチと同名の #871 で見た（gh の規則の全体は確かめていない）。こちらは既定ブランチを `refs/remotes/<r>/HEAD`（02 の
`Remote::head_branch`）でしか知らず、**一度も fetch していないリモートでは
分からない** —— そのとき fork の `main` がローカルの `main` を早送りすることになるので、
PR の base（ほぼ既定ブランチ）とも比べる。**owner が分からない消えた fork は
`pr-<n>/<branch>`**。

#### 追跡先

**fork の URL を書く**（2026-09-23 にユーザーと決めた）。`<u>` は `<r>` の URL の
`owner/repo` だけを `head.repo` のものに差し替えたもの、`<m>` は
`refs/heads/<head.branch>`。

- **URL は `<r>` の形を写す**（`auth::url::sibling`）。ssh なら ssh、エイリアスなら
  エイリアスのまま —— gh は自分の `git_protocol` で組むが、こちらには
  `target` のリモートを実際に通している形のほうが確か。**https の userinfo は写さない**
  （`https://x-access-token:<PAT>@…` を別のキーへ複製しない。不変ルール 3 の趣旨）
- **gh は fork の URL を書かないことがある。** 上の表の最後の行を取った #871 は
  `maintainer_can_modify: false` で、`remote = origin`、`merge = refs/pull/871/head`
  が書かれた（URL を書いた #865 は編集を許している PR）。**こちらはこの旗を見ない** —— 一覧
  （`GET /repos/{o}/{r}/pulls`）は **`maintainer_can_modify` を `null` で返す**
  （実測 2026-09-23。単体の `GET …/pulls/{n}` には入っている。gh は GraphQL で
  取っている）ので、見るには PR ごとに 1 往復増える。**見ないことの代償**は、
  push できない fork でも push 先が fork になること —— どちらを書いても push は
  断られる（`refs/pull/…` は GitHub が受けない）ので、差は `p`（pull）の取り元だけ
  で、**fork が読めない（private の fork）と `p` が認証で落ちる**
- **消えた fork は `<r>` と `refs/pull/<n>/head`**（gh と同じ）。URL を組む相手が無い
- **URL を組めない `<r>` も同じ形に落とす**（`RemoteUrl` として読めないリモート。
  そもそも `target` を決められないので、一覧が出ている時点では起きない）
- **mewgit の読み手は URL の追跡先を受ける。** 現ブランチの PR（01）は URL を
  そのまま `RemoteUrl` に通し、`P` は URL へ push する。**`f` は受けていなかった**
  （「追跡先の https://… がリモートにありません」でリモート選択を開いた。
  gh で取り出したブランチでも同じ）ので、同じ PR で直した（01 / 04 の
  「既定リモート」）
- **ahead / behind は出ない。** URL の追跡先には remote-tracking ref が無い
  （gh で取り出したブランチも同じ）

#### 一連の手順を 1 つの書き込みにする

手順は `GitOp::CheckoutPr` 1 つに載せ、`Runner` が順に叩く（03 の「複数の手順を
持つ書き込み」）。**`app` が手順ごとに `Cmd::Exec` を出して `exec_finished` で
次を出す形にしない。** その形だと (1) 「残りの手順」を `AppState` に持つことになり、
失敗・中断のたびにそれを落とす経路が要る（#89 の「憶える形は落とす経路とセット」）、
(2) 手順ごとに `Reload` が走り、ログの読み直しを手順の数だけ払う、(3) `Ctrl-C` が
「いま走っている 1 本」しか止めず、次の手順が出てしまう。1 つの書き込みにすれば
ジョブも読み直しも中断も 1 回で済む。
