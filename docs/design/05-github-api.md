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
pub struct PullRequest { number, title, state: Open|Closed|Merged, draft, head: RefSpec, base: RefSpec, url, author, review: ReviewSummary, checks: Option<CheckSummary> }
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

1. `pr.head` が同一リポジトリなら `git fetch origin <branch>` → `git checkout -b <branch> --track origin/<branch>`
2. fork なら `git fetch origin pull/<n>/head:<branch>` でローカルブランチを作り、`.git/config` に `branch.<b>.remote/merge` を書いて後の push 先を明示する
3. いずれも `GitOp` として `git_exec` に渡す
