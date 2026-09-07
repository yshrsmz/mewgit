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
    pub fn new(host: &str, token: SecretString) -> Self;
    pub async fn repo(&self, owner: &str, repo: &str) -> Result<RepoMeta>;              // probe 兼用
    pub async fn pr_for_branch(&self, owner, repo, head: &str) -> Result<Option<PullRequest>>;
    pub async fn pr_list(&self, owner, repo, filter: PrFilter) -> Result<Vec<PullRequest>>;
    pub async fn checks(&self, owner, repo, sha: &str) -> Result<Vec<Check>>;
    pub async fn reviews(&self, owner, repo, number: u64) -> Result<ReviewSummary>;
    pub async fn create_pr(&self, owner, repo, input: CreatePr) -> Result<PullRequest>;
}
```

- ヘッダ: `Authorization: Bearer`、`Accept: application/vnd.github+json`、`X-GitHub-Api-Version`、`User-Agent: mewgit/<ver>`

### TLS とルート証明書

**`reqwest` は 0.12 のまま据え置いてある。** 宣言だけで、まだ 1 行も呼んでいない。

0.13 で `rustls-tls` feature が消えた。0.12 のそれは「rustls + 同梱の webpki ルート」
だったが、0.13 はルート証明書の feature 自体を廃止し、既定を
`rustls-platform-verifier`（OS のトラストストア）にした。同梱ルートに戻すには
`webpki-root-certs` を足して `ClientBuilder::tls_certs_only` を呼ぶコードが要る。
`query`（`?per_page=100` に使う）も opt-in になった。

**どの feature が要るかは、このクライアントを書いてからでないと決められない。**
宣言だけの状態で上げると、検証する手段が無いまま feature の組み合わせと
`aws-lc-sys`（重い build.rs を持つ crypto provider）を選ぶことになる。上げるのは
`GithubClient` を書くタスクと同じ PR で行う（`docs/plan.md` の M4）。

- 認証付きリクエストは同時 4 本まで（`Semaphore`）
- レートリミット残量をヘッダから読み、`app` に渡してステータスバーに表示

## エンドポイント対応

| 機能 | API |
|---|---|
| probe / メタ | `GET /repos/{o}/{r}` |
| ブランチの PR | `GET /repos/{o}/{r}/pulls?head={o}:{branch}&state=all`（fork の場合 `head` に fork owner） |
| PR 一覧 | `GET /search/issues?q=is:pr+repo:{o}/{r}+author:@me` / `review-requested:@me` |
| チェック | `GET /repos/{o}/{r}/commits/{sha}/check-runs` + `GET /commits/{sha}/status`（両方をマージ） |
| レビュー | `GET /repos/{o}/{r}/pulls/{n}/reviews` を最新レビュー/ユーザーで集約 |
| PR 作成 | `POST /repos/{o}/{r}/pulls` |

GraphQL は MVP では使わない（1 画面に必要な情報は REST 数本で足りる）。

## モデル

```rust
pub struct PullRequest { number, title, state: PrStatus, draft, head: RefSpec, base: RefSpec, url, author, review: ReviewSummary, checks: Option<CheckSummary> }
pub enum PrStatus { Open, Closed, Merged }   // app::PrState（画面の状態）と紛らわしいので Status
pub struct RefSpec { owner: String, repo: String, branch: String, sha: String }
pub enum CheckState { Success, Failure, Pending, Skipped, Neutral }
pub struct Check { name, state: CheckState, url: Option<String> }
```

## エラー

```rust
pub enum GithubError {
    Unauthorized,                                  // 401
    Forbidden { needed_scopes: Vec<String>, body: String },  // 403（SAML / スコープ / IP allowlist）
    NotFound,                                      // 404（権限不足でも返る）
    RateLimited { reset_at },
    Network(reqwest::Error),
    Api { status, body_head: String },             // 本文先頭 200 文字
}
```

403 の本文に `SAML` / `Resource protected by organization SAML enforcement` が含まれる場合は `Forbidden` に `saml: true` を付け、案内を「トークンの SSO 承認」に切り替える。

## PR チェックアウト

1. `pr.head` が同一リポジトリなら `git fetch origin <branch>` → `git switch --create <branch> --track origin/<branch>`（切替は `checkout` を使わない。03 の argv の表）
2. fork なら `git fetch origin pull/<n>/head:<branch>` でローカルブランチを作り、`.git/config` に `branch.<b>.remote/merge` を書いて後の push 先を明示する
3. いずれも `GitOp` として `git_exec` に渡す
