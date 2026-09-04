# 04. auth: gh 連携・アカウント解決・SSH 正規化

## 責務

- `gh` から認証状態とトークンを取得する
- リポジトリごとに使用する GitHub アカウントを決定する
- SSH エイリアスを実ホスト名に正規化する
- トークンをメモリ上のみで保持する

## gh との連携

```rust
pub struct GhCli { bin: PathBuf }

impl GhCli {
    pub async fn detect() -> Option<Self>;                      // PATH から探す
    pub async fn auth_status(&self) -> Result<Vec<GhLogin>>;    // host, login, active, scopes
    pub async fn token(&self, host: &str, user: &str) -> Result<SecretString>;
}

pub struct GhLogin { pub host: String, pub login: String, pub active: bool, pub scopes: Vec<String> }
```

- `gh auth status --json hosts` が使えるバージョンでは JSON を使い、古い場合はテキスト出力を解析する（バージョンは `gh --version` で判定）
- `gh auth token --hostname H --user U` でユーザー指定取得。`--user` 非対応の古い gh では `active` のみ使える旨を degraded 理由に含める
- トークンは `secrecy::SecretString` で保持し、`Debug` 出力とログから除外する

## SSH エイリアスの正規化

```rust
pub async fn resolve_ssh_host(alias: &str) -> Result<String>   // ssh -G alias → hostname
```

- リモート URL を `RemoteUrl { scheme, host_raw, owner, repo }` に解析（`git@alias:owner/repo.git`、`ssh://git@alias/owner/repo`、`https://host/owner/repo` に対応）
- scheme が ssh のときのみ `ssh -G` を実行し、`hostname` 行を実ホストとする
- 結果は `RepoInfo.host` として保持（例: `github.com`、`acme.ghe.com`）
- `ssh` が無い、または解決失敗なら `host_raw` をそのまま使い、通知に出す

## アカウント解決

```rust
pub enum AccountState {
    Resolving,
    Resolved(Account),                 // { host, login, token, source: Config | Probe | Single }
    Degraded(DegradedReason),          // GhMissing | NotLoggedIn(host) | NoAccess | Ambiguous(Vec<GhLogin>)
}

pub async fn resolve(repo: &RepoInfo, gh: &GhCli, cfg: &Config, api: &GithubClient) -> AccountState
```

手順:

1. `cfg.account_for(&repo)`（`.git/config` の `mewgit.user` または設定ファイルの `[[repos]]` マッチ）があれば、その login のトークンを取得して `Resolved(source: Config)`
2. `auth_status()` を `repo.host` で絞る。0 件なら `Degraded(NotLoggedIn)`
3. 1 件なら `Resolved(source: Single)`
4. 複数なら各トークンで `GET /repos/{owner}/{repo}` を並列に試す
   - 成功が 1 件 → `Resolved(source: Probe)`
   - 成功が複数 → `Degraded(Ambiguous)`。UI で選ばせ、選択を `.git/config` に保存する
   - 成功が 0 件 → `Degraded(NoAccess)`。各アカウントの HTTP ステータスを保持して表示

解決結果は画面上部に `[acct: login @ host]` として常時表示する。`Degraded` は黄色で理由を表示。

## EMU 特有の考慮

- EMU の login は `user_shortcode` 形式。表示上の見分けを付けるため、`gh auth status` の login をそのまま使う
- 企業ポリシーで OAuth App が制限されている環境では PAT ログインが前提になる。トークンの種類は判定せず、`scopes` が空（fine-grained PAT）でも動作させる。スコープ不足は API の 403 で検出する
- データレジデンシー（`*.ghe.com`）は `host` が違うだけで同じ流れ。API ベース URL は `https://api.{host}`（github.com は `https://api.github.com`）

## 権限不足の案内

`GithubError::Forbidden { needed_scopes }` を受けたら、通知に以下を出す:

```
GitHub API: 403 Forbidden（PR 作成）
必要スコープ: repo
対処: gh auth refresh -h github.com -s repo
```

`needed_scopes` はレスポンスヘッダ `X-Accepted-OAuth-Scopes` から取り、無ければ操作ごとの既定値を使う。
