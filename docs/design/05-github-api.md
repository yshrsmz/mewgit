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
    pub async fn pr_list(&self, owner, repo) -> Result<Vec<PullRequest>>;               // `ui::panes::prs`（2/2）
    pub async fn create_pr(&self, owner, repo, input: CreatePr) -> Result<PullRequest>;      // `Modal::PrCreate` のタスク
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

- **認証付きリクエストは同時 4 本まで（`Semaphore`）。まだ入れていない。**
  いま並ぶのは `checks`（check-runs と commit status）だけで、**上限 4 の
  内側なので `Semaphore` が働く場面がまだ無い**。扇形に広がるのは
  `ui::panes::prs`（2/2）の集計（PR ごとに checks / reviews を引く）なので、そこで入れる。
  **置くのはクライアントの中で、`Cmd` を N 本出す形にはできない** —— `JobTracker` は
  種別単位なので、同じ `JobKind` の 2 本目以降は `AppState::issue` が黙って落とす
  （01 の「実行中ジョブ」）。**扇形は 1 ジョブ**（対象の `Vec<(number, sha)>` を運ぶ
  `Cmd` 1 本）にして、その中で `Semaphore` が同時本数を抑える。`checks` は
  `try_join!` で 2 本取るが、上限 4 の内側なので入れ子で詰まることはない
- **一覧は `Link` の `rel="next"` を辿って全ページ集める。** `per_page=100` で
  止めない。**切り捨てが答えを変えるものがある** —— `reviews` の配列は古い順なので、
  1 ページ目だけ見ると「変更要求のあと承認した人」を `changes_requested` と数える。
  check-runs もマトリクスビルドが 100 を超えると、2 ページ目の失敗が見えないまま
  緑に見える。`Link` が輪を作っても止まるよう 10 ページ（1000 件）で打ち切る
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
  **表は過去形の記録として残す** —— `search` を使う経路が戻ってきたときに、
  2 桁小さい上限を数え直さずに済む
- **レートリミット残量の受け渡しはまだ。** ヘッダの `x-ratelimit-*` は
  [`GithubError::RateLimited`] を組むときにしか読んでいない。**「ヘッダの Degraded
  表示」のタスクへ送っていたが、そこでは足さなかった** —— あのタスクで API を叩くのは
  `auth::resolve` の probe だけで、しかも**候補が複数あるリポジトリの起動時 1 回**。
  常時出す残量の産出元にならず、`AppState` に「いつのものか判らない数」を 1 つ
  増やすだけになる。**送り先は `ui::panes::prs`（2/2）** —— PR / checks / reviews を
  引き始めると残量が実際に動き、読む側（ヘッダ）に出す意味が出る。
  **タスクは 2 本に割れた**（plan.md）ので、送り先は **2/2（PR ごとの集計）**。
  1 本目（一覧を出すだけ）で叩くのは `pulls` 1 本で、扇形に広がらず `Semaphore` も
  働かない。**出す数は `core` の 1 つ**（上の「`/search/issues` は使わない」）

## エンドポイント対応

| 機能 | API |
|---|---|
| probe / メタ | `GET /repos/{o}/{r}` |
| PR 一覧 | `GET /repos/{o}/{r}/pulls?state=open&sort=updated&direction=desc`（fork からの PR も返る） |
| チェック | `GET /repos/{o}/{r}/commits/{sha}/check-runs` + `GET /commits/{sha}/status`（両方をマージ） |
| レビュー | `GET /repos/{o}/{r}/pulls/{n}/reviews` を最新レビュー/ユーザーで集約 |
| PR 作成 | `POST /repos/{o}/{r}/pulls` |

GraphQL は MVP では使わない（1 画面に必要な情報は REST 数本で足りる）。

## モデル

```rust
pub struct PullRequest { number, title, state: PrStatus, draft, head: RefSpec, base: RefSpec, url, author, requested_reviewers: Vec<String>, review: Option<ReviewSummary>, checks: Option<CheckSummary> }
pub enum PrStatus { Open, Closed, Merged }   // app::PrState（画面の状態）と紛らわしいので Status
pub struct RefSpec { repo: Option<RepoRef>, branch: String, sha: String }
pub struct RepoRef { owner: String, repo: String }
pub enum CheckState { Success, Failure, Pending, Skipped, Neutral }
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

1. `pr.head` が同一リポジトリなら `Fetch { remote: Some("origin") }` → `BranchCreate { name: <branch>, start: Some("origin/<branch>"), checkout: true }`（= `switch --create <branch> origin/<branch>`）。追跡設定は `branch.autoSetupMerge` が既定なら自動で入るが、**`false` にしている環境では入らない**（git 2.55 で確認。`--track` を付ければ入る）。上流の無いブランチができると Branches ペインに ahead/behind が出ず、push でも `-u` が要る。`GitOp::BranchCreate` に `track` を足すかは**このタスクで決める**
2. fork なら `git fetch origin pull/<n>/head:<branch>` でローカルブランチを作り、`.git/config` に `branch.<b>.remote/merge` を書いて後の push 先を明示する
3. いずれも `GitOp` として `git_exec` に渡す
