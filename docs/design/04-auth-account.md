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
pub async fn resolve_ssh_host(alias: &str) -> Result<String, AuthError>   // ssh -G -- alias → hostname
```

- リモート URL を `RemoteUrl { scheme, host_raw, owner, repo }` に解析（`auth/url.rs`）。
  受けるのは scp 形式 `git@alias:owner/repo.git`、`ssh://git@alias:2222/owner/repo`、
  `https://host/owner/repo` の 3 つ。`git://` / `http://` / `file://` / ローカルパスと、
  `owner/repo` の 2 段にならないパスは `InvalidRemoteUrl` で拒む。**拒んでも Git 操作は
  動く**（GitHub 機能が degraded になるだけ）ので、読めない形を無理に通さない
- port は捨てる。`RemoteUrl` は host / owner / repo しか持たない。**非既定ポートの
  GHES では API のベース URL が違う**が、ベース URL を組むのは `github`（05）なので
  そちらのタスクで決める
- scheme が ssh のときのみ `ssh -G` を実行し、`hostname` 行を実ホストとする
- 結果は `app` が `RepoInfo.target.host` として保持する（例: `github.com`、`acme.ghe.com`）
- `ssh` が無い、または解決失敗なら `host_raw` をそのまま使い、通知に出す

### ssh -G の実測（OpenSSH 10.3p1）

- **未定義の alias でも exit 0** で `hostname <alias>` を返す。「エイリアスが無かった」は
  エラーではなく「正規化しても同じだった」。`Err` にするのは spawn 失敗と非 0 終了だけ
- 出力は `host <alias>` の行が `hostname <実ホスト>` の行より先に来る。**`hostname ` の
  完全一致**で拾う（`host` の prefix で見ると最初の行を取ってしまう）
- `HostName %h.example.com` のトークンは `-G` の時点で展開済みの値が出る
- 構文エラーのある設定は exit 255 で、stderr に `line N: Bad configuration option`
- stdin が端末でないと `Pseudo-terminal will not be allocated` が stderr に出るが、
  **これが出るのは成功する経路だけ**（設定エラーもホスト名エラーも、その手前で落ちる）。
  成功時の stderr は使わないので `-T` で止める必要は無い。**止めても観測できる差が
  無く、見張りを書けない**ので入れない
- `-F <file>` を渡すと `/etc/ssh/ssh_config` も含めて無視するので、実 ssh を使った
  テストを環境から切り離せる（テストのための stub は要らない）

### alias は argv の positional に載る

`.git/config` の URL はユーザーが打ったものとは限らない（clone してきたリポジトリの値が
そのまま入る）。`git@-oProxyCommand=...:a/b.git` のような URL を素直に渡すと
`ssh -G -oProxyCommand=...` になり、**ホスト名ではなくオプションとして解釈される**。
2 段で塞ぐ。

- パーサが `-` で始まる `host_raw` を `InvalidRemoteUrl` で拒む（git 自身と同じ判断）
- `resolve_ssh_host` は `ssh -G -- <alias>` と `--` を置く。**`--` の有無で結果が変わる** —
  無ければ ssh は usage を出して落ち、有れば「ホスト名として不正」で落ちる。公開関数なので
  パーサを通らない呼び出しもありうる

「危ない引数を渡していないか」ではなく「危ない解釈をされる余地があるか」で数える
（`git branch --move` の `to` と同じ形）。

### 既定リモートを決めるのは M4

`RepoInfo.target` を埋めるには「どのリモートの URL を見るか」を決める必要があるが、
`origin` を採るのか現在のブランチの upstream の remote を見るのかは、どの設計書にも
書いていない。**`auth::resolve` を書く M4 のタスクで決める。** それまで `target` は
`None` のままで、この節が用意するのは `RemoteUrl::parse` と `resolve_ssh_host` だけ。

## アカウント解決

```rust
pub enum AccountState {
    Resolving,
    Resolved(Account),                 // { host, login, token, source: Config | Probe | Single }
    Degraded(DegradedReason),          // GhMissing | NotLoggedIn(host) | NoAccess | Ambiguous(Vec<GhLogin>)
}

pub struct RepoTarget { pub host: String, pub owner: String, pub repo: String }

pub async fn resolve(
    target: &RepoTarget,
    repo_path: &Path,          // .git/config の mewgit.user を読むため
    gh: &GhCli,
    cfg: &Config,
    api: &GithubClient,
) -> AccountState
```

`RepoInfo`（01）は `app` 層の型なので受け取らない。`auth` は `app` より下にあり、
依存の向きは上から下のみ（00 参照）。呼び出し側が `RepoInfo` から `RepoTarget` を作る。

手順:

1. 明示指定があれば、その login のトークンを取得して `Resolved(source: Config)`。優先順は
   1. `.git/config` の `mewgit.user`。`auth` が git2 の `Config` で読む（`config` モジュールは `.git/config` を見ない。08 参照）
   2. 設定ファイルの `[[repos]]` マッチ。`cfg.account_for(&target.host, &target.owner, &target.repo)`
2. `auth_status()` を `target.host` で絞る。0 件なら `Degraded(NotLoggedIn)`
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
