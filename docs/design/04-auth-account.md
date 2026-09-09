# 04. auth: gh 連携・アカウント解決・SSH 正規化

## 責務

- `gh` から認証状態とトークンを取得する
- リポジトリごとに使用する GitHub アカウントを決定する
- SSH エイリアスを実ホスト名に正規化する
- トークンをメモリ上のみで保持する

## gh との連携

```rust
pub struct GhCli { bin: PathBuf, env: Vec<(OsString, OsString)> }

impl GhCli {
    pub async fn detect(bin: impl Into<PathBuf>) -> Result<Self, AuthError>;   // gh --version
    pub async fn auth_status(&self, host: &str) -> Result<Vec<GhLogin>, AuthError>;
    pub async fn token(&self, host: &str, user: &str) -> Result<SecretString, AuthError>;
}

pub struct GhLogin { pub host, pub login, pub active: bool, pub scopes: Vec<String>, pub state: LoginState }
pub enum LoginState { Success, Timeout, Error }
```

- **`bin` は `config` の `general.gh_bin` から渡す。** 直書きにすると、設定・既定値・
  `MEWGIT_GH_BIN` の上書きとそのテストが実装に到達しない。#21 が `Runner` に
  `git_bin` を**戻した**のと同じ理由で、渡し忘れを作らないよう引数で受ける。
  08 の「相対パスを許すと任意コード実行になる」は `git_bin` と `gh_bin` の
  2 つに掛かる
- **作れる経路は `detect` だけ。** 「gh があり最低版を満たすことの証」を型で保つには、
  検査を通らない構築経路を置かない（`Default` も公開のコンストラクタも無し）
- **`env` はテストが実行環境から切り離すために要る。** `gh` は設定ディレクトリを
  `GH_CONFIG_DIR` でしか受けず、`ssh -F` に当たる argv が無い。あわせて
  `GH_TOKEN` / `GITHUB_TOKEN` も空で上書きしないと、隔離した config に無い
  ログインが 1 件生える
- **`auth_status` は `--hostname` で絞る。** 付けないと gh は**全ホストのトークンを
  検証**し、到達できないホストが 1 つあるだけで待ち時間がそれに引きずられる
  （実測: 絞ると 0.24 秒、絞らないと **30.2 秒**。gh に待ち時間を縮めるフラグは無い）。
  下の手順 2 の「`target.host` で絞る」を、結果ではなく argv でやる。**config に
  無いホストでも exit 0 で `{"hosts":{}}` が返る**ので「0 件なら `NotLoggedIn`」は変わらない

### 最低版は gh 2.81.0。テキスト出力は解析しない

`gh auth status --json hosts` は **2.81.0（2025-10-01 リリース）で入った**
（[cli/cli#11544](https://github.com/cli/cli/pull/11544)、マージ 2025-09-25。
`gh api` でリリースを引いて確認）。`gh auth token --user` は同じ版に既にある
（v2.81.0 の `pkg/cmd/auth/token/token.go` を読んで確認）。**それ未満は
`AuthError::GhTooOld` で degraded に落とし、テキスト出力は解析しない。**
**版を読めない形も同じ `GhTooOld` に畳む**（通す理由が無い）。そのとき `found` には
`gh --version` の**生の 1 行目**が入るので、UI は何が来たかを出せる。

理由は「書式を確かめられないまま書くことになる」から。手元の 2.100.0 は
`✓ Logged in to github.com account <login> (<source>)` / `- Active account: true` /
スコープはクォート付き、という形だが、**古い版は `as <login>` でクォート無し、
`- Active account:` の行自体が無い**。再現できない書式に対してパーサを書くのは
`.claude/rules/verifying-assumptions.md` に反する。`gh` はもともと必須依存なので、
要求版を案内するほうが行動可能（`brew upgrade gh`）。**古い gh の利用者が実際に出て
きたら、その人の実出力をフィクスチャにして足す**ほうが確実。

`bin: PathBuf` は持たない。バイナリのパスを設定で差し替える要求が無く、#21 で
`Runner.git_bin` を同じ理由で消してある。`Command::new("gh")` の PATH 解決に任せる。

### `gh auth status --json hosts` の形（v2.100.0 のソースで確認）

`hosts` は**ホストをキーにしたオブジェクト**で、値がエントリの配列。エントリは
`{ state, error?, active, host, login, tokenSource, token?, scopes?, gitProtocol }`。

- **`state` は `success` / `timeout` / `error` の 3 値。`gh` の層では絞らない。**
  `GhLogin.state` に載せてそのまま運ぶ。**`timeout`（ホストに届かなかった）を
  `error` と同じに畳んでよいかは `auth::resolve` のタスクで決める**ので、ここで
  落とすと**送った先に判断の材料が届かない**（回線の一時的な失敗を「ログインして
  いない」に化けさせる）
- **`--show-token` を付けない。** 付けると `token` フィールドが JSON に入る（不変ルール 3）
- **`scopes` はカンマ区切りの 1 本の文字列**（`"gist, read:org, repo"`）で、`omitempty`。
  fine-grained PAT では**丸ごと欠ける**ので、無ければ空にする（下の EMU の節が
  「`scopes` が空でも動作させる」と言っているのはこの形）
- **同じ `login` が複数のエントリで出る。** 実機で `GITHUB_TOKEN`（環境変数）と
  `keyring` の 2 つが、どちらも同じ login で並んだ。`GhLogin` は `tokenSource` を
  持たないので**見分けられない重複**になり、下の手順 3「1 件なら `Single`」が誤爆する。
  **`(host, login)` で畳み、`active` は OR、`state` は一番良かったもの
  （`Success` > `Timeout` > `Error`）を採る。** `token(host, user)` は login で
  引くので、畳んでも取り出せるトークンは変わらない。**スコープは `Success` な
  エントリのものを採る** —— 出どころが違えば別のトークンなのでスコープも違いうる
  （同じだとは確かめていない）
- `--json` を付けると**認証に問題があっても exit 0** で返る（`gh auth status --help`
  に明記）。素の `gh auth status` は exit 1 になるので、この差に依存しない

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
- **`resolve_ssh_host` の戻り値は `valid_host` を通っていない。** いまは
  `github::client::base_url` が同じ述語（`auth::url::valid_host`）を呼んで塞いでいる
  が、**より深い形は産出側で検査すること** —— `hostname_from_dump` の結果に
  `valid_host` を掛ければ、上の「解決失敗なら `host_raw` を使って通知に出す」経路に
  そのまま乗り、`github` 側の検査が要らなくなる。採るなら `Err` になる条件の数
  （下の「ssh -G の実測」の 3 つ）と `hostname_from_dump` の「`None` の理由を 1 つに
  保つ」を同じコミットで数え直す。**このタスク（`auth::resolve`）で決める。**

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
    Degraded(DegradedReason),          // GhMissing | GhTooOld | NotLoggedIn(host) | NoAccess | Ambiguous(Vec<GhLogin>)
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

**`api: &GithubClient` は成り立たない。このタスクで形を決め直す。** 理由は 2 つあり、
**先に効くのは 00 のレイヤー則**のほう。

1. **依存の向き。** 00 の図で `github` は `auth` の**上段**なので、`auth` が
   `github` の型を名指しするのは上向きで、`RepoInfo` を受け取らないのと同じ理由で
   できない。**「ホストだけ渡して `resolve` の中で組む」も「クライアントを作る関数を
   渡す」も、同じ理由で不可。** 成立するのは、probe を `auth` の外へ出す
   （`app` が候補ごとに `github::repo` を叩き、結果だけ `resolve` に渡す）か、
   `auth` 側が `github` の型を名指ししない狭い結果型で受けるか、の 2 つ。
2. **トークンが 1 本。** `GithubClient` はトークンを抱える（05。`new(host, token)`）
   ので、手順 4 の「各トークンで `GET /repos/{owner}/{repo}` を並列に試す」を
   1 つのクライアントでは回せない。**`reqwest::Client` のほうは共有される**
   （`github` がプロセスで 1 つ持つ）ので、候補ごとに `GithubClient` を作っても
   トラストストアの読み直しは起きない。

`GithubClient::new` は `Result`（ベース URL を組めないホストがある）なので、
組む場所によって `Degraded` の理由が 1 つ増えるかどうかが変わる。そこまで含めて決める。

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
