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
- `git+ssh://` も ssh として受ける（git が ssh 経路に流すことを argv のダンプで確認）。
  **大文字の scheme は受けない** — `SSH://` は git 自身が `remote-SSH` ヘルパを探して落ち、
  `HTTPS://` は dumb-http に落ちて GitHub に拒否されるので、どちらも git で動かない
- **`owner` / `repo` も `host` と対称に検査する。** この 2 つは `GET /repos/{owner}/{repo}`
  に補間される（05）ので、`..` を通すと `/repos/../users` になり、`?` や `#` を通すと
  query / fragment になる。英数と `.` `-` `_` だけを受け、`.` と `..` は拒む
- **`InvalidRemoteUrl` に載せる URL は資格情報を伏せる。** `https://x-access-token:<PAT>@host/…`
  は CI の checkout が書く形で、解析に失敗した URL は通知にもログにも出る（不変ルール 3）
- port は捨てる。`RemoteUrl` は host / owner / repo しか持たない。**非既定ポートの
  GHES では API のベース URL が違う**が、ベース URL を組むのは `github`（05）。
  **パスの形（`api.{host}` / `{host}/api/v3`）は 05 で実装済みで、port だけが残って
  いる。** 足すのは `RemoteUrl` に `port` を持たせるのと同じ PR、つまり下の
  「既定リモートを決めるのは M4」（`auth::resolve`）のタスク —— 片方だけ入れても
  `RepoInfo.target` に運ぶ経路が無く宙に浮く。`github::GithubClient::new` は
  port 付きのホストを [`GithubError::InvalidHost`] で拒む
- scheme が ssh のときのみ `ssh -G` を実行し、`hostname` 行を実ホストとする
- 結果は `app` が `RepoInfo.target.host` として保持する（例: `github.com`、`acme.ghe.com`）
- `ssh` が無い、または解決失敗なら `host_raw` をそのまま使い、通知に出す

### ssh -G の実測（OpenSSH 10.3p1）

- **未定義の alias でも exit 0** で `hostname <alias>` を返す。「エイリアスが無かった」は
  エラーではなく「正規化しても同じだった」。`Err` にするのは 3 つ — spawn 失敗、非 0 終了、
  出力に `hostname` の行が無いこと（ssh が形を変えたときにしか起きないが、正規化して
  いない値を正規化済みとして下流に渡さないために落とす）
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
- **`Host` の照合は大小文字を区別する。** `Host GH-Work` の設定に `ssh -G gh-work` は
  当たらない（当たらなければ引数がそのまま返るので、失敗せずに正規化だけが効かない）。
  したがって **`RemoteUrl` は `host_raw` を小文字化しない。** https 側だけ小文字化すると
  2 つの経路で別の文字列が下流に入るので、ホスト比較の正規化は `ssh -G` を通したあとの
  値に対して M4 で決める

### alias は argv の positional に載る

`.git/config` の URL はユーザーが打ったものとは限らない（clone してきたリポジトリの値が
そのまま入る）。`git@-oProxyCommand=...:a/b.git` のような URL を素直に渡すと
`ssh -G -oProxyCommand=...` になり、**ホスト名ではなくオプションとして解釈される**。
2 段で塞ぐ。

- パーサが `-` で始まる `host_raw` を `InvalidRemoteUrl` で拒む。第一にホスト名の構文
  として受けないから（`https://-4/a/b` もこれで落ちる）、第二に `RemoteUrl` が `user@` を
  捨てるから
- `resolve_ssh_host` は `ssh -G -- <alias>` と `--` を置く（03 の「argv への変換」と同じ
  数え方）。**`--` の有無で結果が変わる** — 無ければ ssh は usage を出して落ち、有れば
  「ホスト名として不正」で落ちる（この拒否は OpenSSH 7.6 以降）

**git と同じ判断ではない。** git 2.55 が拒むのは `user@host` **全体**が `-` で始まるとき
だけで、`git@-4:a/b.git` も `git@-oProxyCommand=id:a/b.git` も素通りする（`core.sshCommand`
で argv をダンプして確認）。git が安全なのは `git@-4` を 1 語のまま ssh に渡すからで、
`user@` を捨てる mewgit だけが裸の `-4` を作る。**受け継いだ危険ではなく、こちらが作った
危険**なので、git に合わせて緩めない。

「危ない引数を渡していないか」ではなく「危ない解釈をされる余地があるか」で数える
（`git branch --move` の `to` と同じ形）。

### git が使う ssh と、ここで叩く ssh

`resolve_ssh_host` は PATH の `ssh` を直に起動する。git のほうは
`GIT_SSH_COMMAND` / `core.sshCommand` に従うので、そこを別の ssh（別の config を読むもの）に
向けている環境では**解決と接続で食い違う**。

**実効値を求める関数はここ（`auth::ssh`）に置く**（03 の「BatchMode の重ね方」）。
`git_exec::Runner` はそれを使って `-o BatchMode=yes` を重ねる。`git_exec` から `auth` への
依存は層の向きに沿う（00）。

**`resolve_ssh_host` をその実効値に向けるのは M4。** いまこれを呼ぶ経路は無く
（`RepoInfo.target` を埋めるのが下の `auth::resolve`）、実効値はシェル文字列なので
`Command::new(ssh)` ではなく `sh -c` 経由の起動に変える必要がある。踏めない腕を
先に入れず、`auth::resolve` を書くタスクで一緒に配線する。

### 既定リモートを決めるのは M4

`RepoInfo.target` を埋めるには「どのリモートの URL を見るか」を決める必要があるが、
`origin` を採るのか現在のブランチの upstream の remote を見るのかは、どの設計書にも
書いていない。**`auth::resolve` を書く M4 のタスクで決める。** それまで `target` は
`None` のままで、この節が用意するのは `RemoteUrl::parse` と `resolve_ssh_host` だけ。

**同じ問いが M3 から 2 つ送られている**（01 の「リモート操作」）。どちらも「既定の
リモートはどれか」で、別々に決めると規則が 2 本並ぶ。

- **`f` が無反応になる形。** リモートが複数あって `origin` も `branch.<name>.remote` も
  無いと、`git fetch` は何もせず exit 0 で返る（git 2.55 で確認）。git が黙るので
  stderr を通知に出す経路も効かない
- **`-u` の提案が git の規則の部分集合。** `branch.<name>.pushRemote` と
  `remote.pushDefault` を読んでいない。読むこと自体は `git_read` にできる
  （`Branch.upstream_config` と同じ形。02）

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

**`api: &GithubClient` は成り立たない。このタスクで形を決め直す。** `GithubClient` は
**トークンを 1 本抱える**（05。`new(host, token)`）ので、手順 4 の「各トークンで
`GET /repos/{owner}/{repo}` を並列に試す」を 1 つのクライアントでは回せない。
候補は「ホストだけ渡して `resolve` の中でトークンごとに組む」「`GhLogin` から
クライアントを作る関数を渡す」の 2 つ。**`GithubClient::new` が `Result` を返す**
（ベース URL を組めないホストがある）ので、組む場所によって `Degraded` の理由が
1 つ増えるかどうかが変わる —— そこまで含めて決める。

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
