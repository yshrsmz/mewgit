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

### `gh auth status --json hosts` の形（v2.100.0 のソースで確認）

`hosts` は**ホストをキーにしたオブジェクト**で、値がエントリの配列。エントリは
`{ state, error?, active, host, login, tokenSource, token?, scopes?, gitProtocol }`。

- **`state` は `success` / `timeout` / `error` の 3 値。`gh` の層では絞らない。**
  `GhLogin.state` に載せてそのまま運ぶ。**`resolve` も絞らない**と決めた
  （下の「`LoginState` で候補を絞らない」）ので、`state` が効くのは表示だけ
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
- **port は scheme に畳む。** `RemoteScheme::Ssh` / `RemoteScheme::Https { port }` の
  形にして、**ssh の port が API のベース URL に届く組み合わせを型で消す**（#49 の
  「旗と値を割らない」と同じ数え方）。`ssh://git@ghe.example.com:2222/o/r` の 2222 は
  **ssh の port であって API の port ではない** —— GHES を非既定の ssh port で運んで
  いても、API は `https://ghe.example.com/api/v3/` のままなので、平らな
  `port: Option<u16>` を持つと `https://ghe.example.com:2222/api/v3/` という
  **動くが違う場所を指す**ベースが組める（`base_url` が `foo/bar.ghe.com` で
  作りかけたのと同じ形）
- **既定ポートは `None` に畳む。** https の 443 と ssh の 22 は書いてあっても
  書いていないのと同じなので、`https://github.com:443/o/r` が `api.github.com:443` に
  ならないようにする。port が u16 として読めない形（`https://h:x/o/r`）は
  `InvalidRemoteUrl` で拒む —— ベース URL を組めないうえ、git 側でも動かない
- ベース URL を組むのは `github`（05）で、`GithubClient::new(host, port, token)` が
  port を受ける。**`api.{host}` の形（github.com と `*.ghe.com`）に port が付く形は
  確かめていない** —— どちらも SaaS で 443 固定なので実物を用意できない。書かれて
  いれば素直に載せる（`https://api.foo.ghe.com:8443/`）
- scheme が ssh のときのみ `ssh -G` を実行し、`hostname` 行を実ホストとする
- 結果は `app` が `RepoInfo.target.host` として保持する（例: `github.com`、`acme.ghe.com`）
- `ssh` が無い、または解決失敗なら `host_raw` をそのまま使い、通知に出す
- **`resolve_ssh_host` の戻り値を `valid_host` に通す**（産出側で検査する）。
  `hostname_from_dump` が拾った値をそのまま返すと、不正なホストが
  `RepoTarget.host` まで進み、`GithubClient::new` が `InvalidHost` で落として
  **「GitHub 機能が使えない」だけが残る**。産出側で落とせば上の「解決失敗なら
  `host_raw` を使って通知に出す」経路にそのまま乗り、**degraded ではなく
  正規化だけを諦める**形になる。`Err` の条件はこれで **4 つ**（下の「ssh -G の実測」）
- **`github::client::base_url` の検査は残す。** 04 は当初「産出側でやれば `github` 側の
  検査が要らなくなる」と書いていたが、**2 つは別の問いに答えている** —— 産出側は
  「正規化が成功したか」（失敗しても `host_raw` で続けられる**回復可能な**判断）、
  `base_url` は「`pub fn GithubClient::new` に渡された任意のホストを補間してよいか」
  （回復先の無い**入口の守り**）。消すと、`https://api.foo/bar.ghe.com/` という
  **動くが違う場所を指す**ベースを作る経路が `new` の呼び出し側の数だけ開く。
  述語は 1 本（`auth::url::valid_host`）のままなので、「同じ判定を 2 箇所に書かない」
  には触れない —— 呼ぶ場所が 2 つあるだけで、規則は 1 本

### ssh -G の実測（OpenSSH 10.3p1）

- **未定義の alias でも exit 0** で `hostname <alias>` を返す。「エイリアスが無かった」は
  エラーではなく「正規化しても同じだった」。`Err` にするのは **4 つ** — spawn 失敗、
  非 0 終了、出力に `hostname` の行が無いこと（ssh が形を変えたときにしか起きないが、
  正規化していない値を正規化済みとして下流に渡さないために落とす）、**その値が
  `valid_host` を通らないこと**（上の「産出側で検査する」）
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

`resolve_ssh_host` は **git が実際に使う ssh** を起動する。PATH の素の `ssh` を叩くと、
`GIT_SSH_COMMAND` / `core.sshCommand` を別の ssh（別の config を読むもの）に向けている
環境で**解決と接続が食い違う**。

**実効値を求める関数はここ（`auth::ssh`）に置く**（03 の「BatchMode の重ね方」）。
`git_exec::Runner` はそれを使って `-o BatchMode=yes` を重ねる。`git_exec` から `auth` への
依存は層の向きに沿う（00）。

**`resolve_ssh_host` は実効値に向けた**（`RepoInfo.target` を埋める PR）。実効値は
シェル文字列なので `Command::new(ssh)` では起動できず、次の形で叩く。

```
sh -c '<実効 ssh> "$@"' ssh -G -- <alias>
```

**`alias` を `sh` の文字列に埋めない。** `"$@"` の位置引数に載せるので、シェルは
その中身を再解釈しない（`$(touch PWNED)` を alias に渡してもファイルは作られず、
ssh が `hostname contains invalid characters` で落ちる。実測）。`sh -c` の直後の
`ssh` は `$0` に入るだけの捨て値で、起動されるのは実効値のほう。

| 形 | 叩くもの |
|---|---|
| `SshCommand::Shell(cmd)` | `sh -c '<cmd> "$@"' ssh -G -- <alias>` |
| `SshCommand::Opaque`（`GIT_SSH`） | `ssh -G -- <alias>`（PATH の素の ssh） |

**`Opaque` で `$GIT_SSH` を叩かない。** `GIT_SSH` はシェルを通さず
`<prog> [-p port] [user@]host <command>` の argv で呼ばれる規約なので、**`-G` を
解する保証が無い**（`-G` を無視するラッパがそのあと何をするかは確かめていない。
接続に行くなら `ssh -G` は接続しないから速い、という前提が消える）。素の `ssh` に
落とすと、その環境では正規化が食い違いうるが、**食い違っても `host_raw` を使うのと
同じ結果に落ちる**。03 が `Opaque` を「触らない」と決めたのと同じ数え方。

### 既定リモート

**問いは 1 つではない。** `app` に関数が 2 本ある（01 の「既定リモート」）。
`auth` に置かないのは、材料が `git_read::Remote` と `Branch.upstream_config` で、
どちらも `auth` の**上段**の型だから（00）。

| 関数 | 答える問い | 読み手 |
|---|---|---|
| `update::default_remote` | `git fetch`（引数なし）はどれを取るか | `f` |
| `update::conventional_remote` | 名前の慣習だけで選ぶならどれか | `default_remote` の 2 段目、`-u` の提案、`RepoInfo.target` |

```
default_remote      = branch.<HEAD>.remote → conventional_remote
conventional_remote = リモートが 1 本ならそれ → origin → 決まらない
```

**`RepoInfo.target` は `default_remote` を呼ばない。** ブランチの追跡設定を混ぜると、
`Msg::BranchesLoaded` と `Msg::RepoInfoLoaded` の**どちらが先に届くかで答えが割れる**
——`init` が両方を別々のジョブとして出すので順序は決まらず、しかも `target` は
1 回きり決めて latch するので二度と直らない。`CLAUDE.md` が「ループテストは非同期の
返信の順序に依存できない」と書いている、その順序に production が乗ってしまう。
**ブランチを切り替えても `target` は変わらない**という事実とも噛み合わない。
`/simplify` の altitude が拾った。

git 2.55 で実測した（`env -i` の一時リポジトリ）。

| 状態 | `git fetch` |
|---|---|
| 単一リモート `upstream`（`origin` 無し、`branch.*.remote` 無し） | **その 1 本を fetch する**（`refs/remotes/upstream/main` ができる） |
| リモート 2 本、どちらも `origin` でない、`branch.*.remote` 無し | **何もせず exit 0**（ref が 1 つも増えない） |

「1 本ならそれ → 複数なら `origin`」と「`origin` があればそれ → 無ければ 1 本のとき
だけ」は**同じ結果**になる（食い違うのは「単一かつ `origin` でない」場合だけで、
どちらもその 1 本を選ぶ）。既にある `propose_upstream` が前者の順で書かれているので
そちらに合わせる。**これが `conventional_remote`。**

**`branch.<HEAD>.remote` が `.`（ローカル追跡）や、存在しないリモートを指すときは
決まらない**（`None`）。`.` は 01 の push が既に断っている形で、存在しない名前は
git 自身も fetch でエラーにする。

送られていた 2 つのうち、片方だけを引き受ける。

- **`f` が無反応になる形 → 直した。** `default_remote` が決まらないときに
  `GitOp::Fetch` を出さず、**リモート名を並べた通知**で断る。決まるなら
  `remote: Some(name)` を渡す。git が黙る経路（stderr が空のまま exit 0）へ
  そもそも入らない
- **`-u` の提案が git の規則の部分集合 → 引き受けない。** これは**別の規則**で、
  git 自身が fetch と push で違う規則を持っている（push は
  `branch.<name>.pushRemote` > `remote.pushDefault` > fetch の規則）。
  `default_remote` に畳むと、**fetch の既定へ push する**という git がしない挙動に
  なる。「規則が 2 本並ぶ」を避けたいのは *target* の規則が 2 本になることで、
  fetch と push を 1 本にすることではない。push 側を git に揃えるのは M6 へ送る
  （`Branch.pushRemote` の読み取りが要る。02）

## アカウント解決

**タスクを 2 本に割った**（plan.md も割ってある）。1 本目が `RepoInfo.target` を
埋めるところまで、2 本目が `resolve` 本体。

- **1 本目**（`RepoInfo.target`）— 上の「既定リモート」「port は scheme に畳む」
  「産出側で `valid_host`」「実効 ssh」。**port と target 埋めは同じ PR でないと
  片方が宙に浮く**（`RemoteUrl` に port を持たせても運ぶ先が無い）ので、この 4 つが
  最小の単位になる
- **2 本目**（`resolve`）— 下の手順 1〜4 と `Backend`。**1 本目が終わるまで
  `RepoTarget` が作れない**ので順序は入れ替えられない

`RepoTarget` は port を運ぶ。`RemoteUrl` から作るときに **https の port だけ**を
載せる（ssh の port は API の port ではない。上の「port は scheme に畳む」）。

```rust
// 状態の側。`resolve` が返すのは下の `Result<Account, DegradedReason>` で、
// `Resolving` は `Cmd::ResolveAccount` を出す前に置く値（01）。
pub enum AccountState {
    Resolving,                         // まだ頼んでいない。`resolve` は返さない（型でそうなっている）
    Resolved(Account),                 // { host, login, token, source: GitConfig | RepoRule | Single | Probe }
    Degraded(DegradedReason),
}

pub enum DegradedReason {
    GhMissing,
    GhTooOld { found: String },
    Unusable { message: String },                  // gh は居るのに答えが返らない
    NotLoggedIn { host: String },
    ConfiguredUnavailable { login, message },      // 明示指定の login のトークンが取れない
    NoAccess { attempts: Vec<ProbeAttempt> },
    Ambiguous { candidates: Vec<GhLogin> },        // probe を通った候補だけ
}

pub struct ProbeAttempt { pub login: String, pub failure: AttemptFailure }

pub enum AttemptFailure {
    Denied { status: u16 },        // HTTP で断られた。このアカウントでは無理
    Undetermined { message: String },  // 回線 / TLS / レート制限。もう一度なら通りうる
    NoToken { message: String },   // gh からトークンを取れず、probe まで行っていない
}

pub struct RepoTarget {
    pub host: String,
    /// https の非既定ポート。既定（443）は `None`。**ssh の port は入らない。**
    pub port: Option<u16>,
    pub owner: String,
    pub repo: String,
}

pub trait Backend {
    fn logins(&self, host: &str) -> impl Future<Output = Result<Vec<GhLogin>, AuthError>> + Send;
    fn token(&self, host: &str, login: &str)
        -> impl Future<Output = Result<SecretString, AuthError>> + Send;
    fn probe(&self, target: &RepoTarget, token: &SecretString)
        -> impl Future<Output = Result<(), AttemptFailure>> + Send;
}

pub async fn resolve<B: Backend + Sync>(
    target: &RepoTarget,
    repo_path: &Path,          // .git/config の mewgit.user を読むため
    backend: &B,
    cfg: &Config,
) -> Result<Account, DegradedReason>
```

`RepoInfo`（01）は `app` 層の型なので受け取らない。`auth` は `app` より下にあり、
依存の向きは上から下のみ（00 参照）。呼び出し側が `RepoInfo` から `RepoTarget` を作る。

### 外の世界は `Backend` の 3 つに畳む

**`api: &GithubClient` は成り立たない。** 理由は 2 つあり、**先に効くのは 00 の
レイヤー則**のほう。

1. **依存の向き。** 00 の図で `github` は `auth` の**上段**なので、`auth` が
   `github` の型を名指しするのは上向きで、`RepoInfo` を受け取らないのと同じ理由で
   できない。**「ホストだけ渡して `resolve` の中で組む」も「クライアントを作る関数を
   渡す」も、同じ理由で不可。** 成立するのは、probe を `auth` の外へ出す
   （`app` が候補ごとに `github::repo` を叩き、結果だけ `resolve` に渡す）か、
   `auth` 側が `github` の型を名指ししない狭い結果型で受けるか、の 2 つ。
2. **トークンが 1 本。** `GithubClient` はトークンを抱える（05。`new(host, port, token)`）
   ので、手順 4 の「各トークンで `GET /repos/{owner}/{repo}` を並列に試す」を
   1 つのクライアントでは回せない。**`reqwest::Client` のほうは共有される**
   （`github` がプロセスで 1 つ持つ）ので、候補ごとに `GithubClient` を作っても
   トラストストアの読み直しは起きない。

**`&GhCli` を `resolve` の引数にするとテストできない**（1 本目で数えた）。#61 の
テストは JSON を解析する純粋関数を直に叩いており、`GhCli::auth_status` の非同期経路は
**空 config（0 件）しか通していない** —— 偽トークンを置いた `GH_CONFIG_DIR` では
`gh` が実ホストへ検証に出るので、Single / Probe / Ambiguous を実 `gh` では踏めない。
「テストが書きにくい設計になっていたらテストを省くのではなく設計を直す」に当たるので、
**上の 2 案のうち後者（`auth` が `github` の型を名指ししない狭い trait）を採り、
`gh` 側も同じ trait に載せた。**

**返り値に `Send` を要求する。** ランタイムは `Cmd::ResolveAccount` を `tokio::spawn`
に載せるので、`resolve` の future 全体が `Send` でないと積めない。trait に
`async fn` と書くと付かないので、`-> impl Future<Output = ..> + Send` で書く。

### `Backend` の production 実装は `github` に置く（`github::AccountBackend`）

- **置き場所は `github`。** `auth::Backend` と `GithubClient` の両方を名指しできる
  最下段がここ（`auth` は `github` を名指しできない）。`struct AccountBackend { gh: Result<GhCli, AuthError> }`
- **`GhCli::detect` の失敗は `Backend` の中に持つ。** ランタイムが畳むと、01 が
  「`AuthError` は `resolve` の中で `DegradedReason` に畳む」と決めた変換が 2 箇所に
  なる。`logins` / `token` が抱えた `Err` を clone して返せば
  （`AuthError` は `Clone`）、手順 2 がそのまま `GhMissing` / `GhTooOld` に落とす
- **`probe` は `GithubClient::new(target.host, target.port, token)` → `repo(owner, repo)`。**
  `new` の失敗（`shared_http()` の TLS 初期化。`InvalidHost` は 1 本目で届かなくなった）も
  下の表を通す
- **表を通すのは `Result` 全体で、`Err` だけではない。** `Err(Decode)` が `Ok(())` に
  なる行があるので、「成功したか」を先に数えると 1 行落ちる（`to_argv` と同じく
  純粋関数にして、表の行を 1 つずつ見張る）

**TLS 初期化の失敗は候補ごとの `Undetermined` のままにする**（送っていた
「実装するタスクで数え直す」の答え）。1 回だけ判定して `Unusable` に落とす形は 2 つの
理由で採らない。**`resolve` の外に `AccountState` の産出元がもう 1 つできる** ——
畳むのはランタイムになり、01 が「`update` に手書きのマッピングを生やさない」と決めた
境目に、`resolve` を通らない degraded の経路が並ぶ。そして**見張れない** ——
`shared_http()` は `OnceLock` で、失敗させるには TLS バックエンドの初期化を折る
必要がある。**プロセス全体の条件が候補ごとの服を着る**形（手順 1 の `gh` と同じ）で
あることは変わらないが、こちらは**全候補が同じ `message` で並ぶだけ**で、手順 1 の
ように「設定を直してください」と誤った直し方を案内する腕には落ちない。

| `GithubClient::repo` の結果 | `probe` の返り値 | 理由 |
|---|---|---|
| `Ok(RepoMeta)` | `Ok(())` | |
| `Err(Decode)` | **`Ok(())`** | `check_status` を通ったあとにしか出ない = **2xx が返っている**。GHES の版差で本文の形が違うだけで、アクセス権はある。`Failed` に落とすと動いている環境が `NoAccess` になる |
| `Err(Unauthorized)` | `Err(Denied { 401 })` | |
| `Err(Forbidden)` | `Err(Denied { 403 })` | **SAML の 403 もここ。** 「SSO 承認が要る」は直せる状態だが、サーバは**答えを返している**（このトークンでは通らない）ので `Undetermined`（確かめられなかった）ではない。**`saml` の印はここで落ちる。載せないと決めた**（ピッカーのタスクで数え直した結果）—— 403 の候補は `Ambiguous` に並ばず落ちるので、印が出るのは `NoAccess` の内訳だけ。そこで直し方まで書くには bool ではなく**どの org を承認するか**（`body_head`）と `needed_scopes` が要り、それは M4 の「403 の `needed_scopes` 案内」が要る材料と同じ。**そちらで 1 回作る。** `Denied` に `saml: bool` を足すと `Denied { 404, saml: true }` が型で組める（「旗と値を割らない」） |
| `Err(NotFound)` | `Err(Denied { 404 })` | 権限が無ければ GitHub は 404 を返す |
| `Err(Api { status: 4xx, .. })` | `Err(Denied { status })` | |
| `Err(Api { status: 5xx, .. })` | **`Err(Undetermined)`** | **サーバが一時的に答えられないだけ**で、断られてはいない。`Denied` に畳むと手順 4 が「除外できた」と数え、GHES のメンテナンスや 502 が 1 回混じった回に**別のアカウントを黙って選んで latch する**（`/code-review` が拾った。`unchecked` の見張りが塞いだはずの形） |
| `Err(RateLimited)` | `Err(Undetermined)` | **ステータスを載せない。** 05 の `RateLimited` は 429 だけでなく**印の付いた 403** からも来る |
| `Err(Network)` | `Err(Undetermined)` | |
| `Err(InvalidHost)` | `Err(Undetermined)` | 届かないはず（1 本目で `valid_host` を通している）。`unreachable!()` にしない |

手順:

1. 明示指定があれば、その login のトークンを取得して `Resolved`。優先順は
   1. `.git/config` の `mewgit.user` → `AccountSource::GitConfig`。`auth` が git2 の `Config` で読む（`config` モジュールは `.git/config` を見ない。08 参照）
   2. 設定ファイルの `[[repos]]` マッチ → `AccountSource::RepoRule`。`cfg.account_for(&target.host, &target.owner, &target.repo)`
   - **どちらで当たったかを `AccountSource` に残す。** 1 本の `Config` に畳むと、
     `A` のモーダルが「mewgit から外せるか」を答えられない —— `.git/config` は
     `git config --unset` で外せるが、設定ファイルの `[[repos]]` は外せない
     （08 の境目の裏側）。**畳んだまま両方の置き場所を案内に並べる形は #73 で出荷して
     いて、`/code-review` に「選び直せない」を拾われた**（01 の「アカウントの選び直し」）
   - **一覧を引かない。** `gh auth status` は候補のトークンをホストへ検証しに行くので、
     答えが決まっているのに 0.24 秒払うことになる
   - **トークンが取れなかったら手順 2 へ落とさない。** `ConfiguredUnavailable` で止める
     —— 指定を無視して別のアカウントを選ぶと、EMU と個人アカウントを取り違える。
     直すのは gh ではなく設定のほうなので、`Unusable` にも畳まず login を持たせる
   - **ただし `ConfiguredUnavailable` にするのは login 固有の失敗（`GhFailed`）だけ。**
     手順 1 は手順 2 を**意図的に飛ばす**ので、**`gh` の失敗の入口はここにもある**
     —— `gh` が居ない・古いをまとめて畳むと、「`mewgit.user` を直してください」と
     案内しながら `GhTooOld` の腕（「2.81.0 以上に上げてください」）が一度も
     発火しない。**プロセス全体の条件が、login ごとの設定ミスの服を着る**
     （`/simplify` の altitude が拾った）
   - **空文字（`user = ` が残った形）は「指定なし」と同じ。** `gh auth token --user ""`
     を叩いても答えは返らない
2. `auth_status()` を `target.host` で絞る。0 件なら `Degraded(NotLoggedIn)`
3. 1 件なら `Resolved(source: Single)`。**probe しない**（選びようが無く、起動のたびに
   1 往復払う理由が無い）。トークンが取れなければ `NoAccess`（`NoToken` の attempt 1 件）
4. 複数なら各トークンで `GET /repos/{owner}/{repo}` を並列に試す。**出口は
   `AttemptFailure` と同じ 3 つで数える**（「二値だった結果に 3 つ目を足したら、
   入口と出口の両方を数える」）—— 断られた（`Denied` / `NoToken`）候補だけが
   除外でき、`Undetermined` は**除外できていない**
   - 成功が 1 件で、`Undetermined` が 0 件 → `Resolved(source: Probe)`。
     **トークンを取り直さない**（probe に使ったものをそのまま `Account` に載せる）
   - **成功が 1 件でも `Undetermined` が残っていれば `Ambiguous`。** 混ぜると、
     回線やレート制限で 1 本落ちただけの回に**別のアカウントを黙って選んで latch
     する** —— EMU と個人アカウントの両方がその repo の collaborator なら、以後の
     PR が違う名前で立ち、再起動するまで直らない（`/code-review` が拾った）
   - 成功が複数 → `Degraded(Ambiguous)`。`A` で開くモーダルが選ばせ、選んだ login を
     `.git/config` の `mewgit.user` に書く（`GitOp::SetAccount`。01 の「アカウントの
     選び直し」）。**書いたあとは解決をやり直す**ので、以後は手順 1 で
     `Resolved(source: GitConfig)` になり probe を通らない。
     **`Vec<GhLogin>` を運ぶので、`Ambiguous` から選ぶときは `auth_status` を
     呼び直さなくてよい**（実測 0.95 秒。#61）。**効くのはここだけ** ——
     決まったあと（`Resolved`）に `A` で選び直すときは候補が state のどこにも
     残っていないので、そちらは `Cmd::ListAccounts` で引き直す（01 の
     「候補の取り直し」）。**引き直した候補は probe しない**（同）。**並べるのは除外できなかった候補**（通ったもの +
     `Undetermined`）—— 断られたものを混ぜると、選べるアカウントとして 401 の
     ログインを出すことになる。**候補ごとの probe の結果は運んでいない。ピッカーのタスクで数え直して、
     据え置くと決めた** —— 運ぶこと自体は安い（`passed` と `unchecked` は
     `resolve` の中にある）が、**印を見た人に取れる行動が無い**。
     `Undetermined` は回線・レート制限・TLS 初期化を畳んだもので、行に書けるのは
     「確かめられませんでした」だけ。しかも**状況はプロセス共通**なので、
     ほとんどの回は全候補に同じ印が付いて何も分けない。**gh 側の検証結果
     （`LoginState`）は行に出す**ので、そちらで足りるかを先に見る
   - 成功が 0 件 → `Degraded(NoAccess)`。各アカウントの落ちた理由を保持して表示。
     **全部が `Undetermined`（起動時に回線が無い）もここ** —— 選ばせないのは、
     どれも動かない状態でアカウントを選んでも何も直らないため。変種の名前を
     「権限が無い」と読まない

**候補ごとの「token → probe」を 1 本の future にして `join_all` で回す。** 逐次だと
N×(トークン取得 + RTT) を起動のたびに払う。tokio には動的な本数を待つ組み合わせ子が
無く、`JoinSet` は `'static` を要求して `&Backend` を借りる future を載せられないので、
`futures` の `join_all` を使う（`mewgit-tui` が既に使っている版と同じ）。
**逐次に戻すと開かない関所**（`Barrier` + `timeout`）で見張る。

### `resolve` の戻り値は `Result<Account, DegradedReason>`

**ヘッダ表示のタスクで数え直して採った**（送っていたのは `/simplify` の altitude）。
`AccountState` を返す形だと「`Resolving` は返さない」を見張るのは doc の 1 文だけで、
`Result` にすればそれが型になる。01 の `Msg::AccountResolved` も同じ形にしたので、
「`resolve` の戻り値をそのまま運ぶ」は保たれている。`update` が `Ok` を
`Resolved`、`Err` を `Degraded` に写す。

**`AccountState` は状態の側に残る。** `Resolving`（まだ頼んでいない）が要るのは
`Cmd::ResolveAccount` を出していないリポジトリ —— 既定リモートが無い、URL を
GitHub の形として読めないリポジトリでは `RepoTarget` が作れず、解決を頼まない
（01 の「`RepoInfo.target` を埋める」）。

### `LoginState` で候補を絞らない

送られていた「`Timeout` を `Error` と同じに畳んでよいか」の答え。**どちらも落とさない**
ので、畳む / 畳まないの判断が要らない形になった。`state` は表示のためだけに運ぶ。

理由は**害が非対称**だから。手順 3 の `Single` は**検証せずに確定する**ので、gh の
判定で候補を落とすと「2 件のうち 1 件が `Error`」が `Single` に化け、**probe を
一度も通さずに別のアカウントを選ぶ**。落とさなければ払うのは並列のリクエスト 1 本。
`gh` の検証と probe は**別の問い**でもある（gh はトークンが生きているかを見るが、
こちらは*このリポジトリに*届くかを見る）。

全部が `Error` のときも、`NotLoggedIn`（ログインはしている、という意味で嘘）ではなく
`NoAccess` に落ちて 401 が並ぶ。

### `.git/config` の読み方（git2 で実測）

`Repository::config()` は**全段**を返すので、`~/.gitconfig` に `mewgit.user` を
書くと全リポジトリに効く。08 が「`config` モジュールは `.git/config` を見ない」と
割った境目が消えるので、**`open_level(ConfigLevel::Local)` で局所の段だけを読む**。
リンクされたワークツリーでも `Local` はリポジトリ共有の `config` なので、同じ
リポジトリなら同じアカウントになる（`Worktree` の段は読まない）。

| 読み方 | `~/.gitconfig` に `mewgit.user = global_taro` があるとき |
|---|---|
| `repo.config()` | `Some("global_taro")` |
| `repo.config().open_level(Local)` | `None` |

**この 2 行を回すテストは書いていない。** libgit2 に global の段を差し替えさせるには
`git2::opts::set_search_path`（プロセス全体・`unsafe`）が要り、並列に走る他のテストの
git2 まで巻き込む。上の表は手で叩いた結果（`git2` 0.21）。

**書くのは `git` CLI**（不変ルール 1、`GitOp::SetAccount`。03 に argv と実測）。
読みが git2、書きが CLI で割れるので、**キーの名前は `auth::MEWGIT_USER` の 1 本**に
する —— 別々に書くと、片方を直したときに黙って別のキーを読み書きする。
`git config --local` の書き先はリンクされたワークツリーでも共有の `.git/config` で、
上の `open_level(Local)` が読む段と同じ（03 の表）。

### `gh auth token` の実測（gh 2.100.0）

| 状況 | 結果 |
|---|---|
| ログイン 0 件で `--user nobody` | **exit 1** / `no oauth token found for github.com account nobody` |
| `GH_TOKEN` を立てて `--user nobody` | **同じ**（環境変数のトークンで代替されない） |

2 行目は手順 1 の安全のため —— `mewgit.user = B` が黙って A のトークンを B として
返す形は無い。

解決結果は画面上部に `[gh: login @ host]` として表示する。`Degraded` は黄色で理由を表示。
**決まるまでは何も出さない**（`Resolving` のあいだ。07 のヘッダ）。

**ヘッダに出すのは 1 行に収まる診断だけ。** `NoAccess` の `attempts`
（「どの login がなぜ落ちたか」）は件数しか出さず
（`[gh: どれも届きません（2 件）。A で詳細]`）、`Unusable` と
`ConfiguredUnavailable` の `message` も出さない。上の `reason_for` が
`GhUnrunnable` / `GhFailed` / `GhUnparsable` を 1 つに畳んでよいと言う根拠は
「直し方の違いは `message`（`AuthError` の `Display`）に出ている」なので、
**その `message` を出す画面が要る**（起動できない理由は 1 行に収まらない）。

### 内訳を出すのは `Modal::AccountPicker`

**送ってあった「1 件ずつ並べる先」はここ。** `A` で開くモーダルが、解決の内訳を
並べる唯一の画面になる（01 の「アカウントの選び直し」、07 の「AccountPicker
モーダル」）。ヘッダの 1 行との分担は次のとおり。

**どの理由でもヘッダは `A` を案内する**（`A で選ぶ` / `A で詳細`）。**選べない
degraded でも案内する**のは、ヘッダが落としたものを出す画面がここしか無いから ——
案内が無いと、出す先を用意したのに誰も辿り着けない（07 のヘッダ）。

| | ヘッダ | `Modal::AccountPicker` |
|---|---|---|
| `Ambiguous` | 件数 + `A で選ぶ` | 候補を 1 件ずつ（login / scopes / `LoginState`）。`Enter` で選ぶ |
| `NoAccess` | 件数だけ + `A で詳細` | `attempts` を 1 件ずつ（login とその `AttemptFailure`）。**選ばせない** |
| `Unusable` / `ConfiguredUnavailable` | 畳んだ 1 行 + `A で詳細` | `message` そのまま |
| `GhMissing` / `GhTooOld` / `NotLoggedIn` | 1 行で足りる + `A で詳細` | 同じ 1 行 + 外での直し方 |
| `Resolved`（degraded ではない） | `login @ host` だけ | 決め方と、取り直した候補。`Enter` で選び直し、`GitConfig` なら `u` で解除（01） |

**degraded の行はどれも `r`（解決し直す）を出す。`Resolved` の行だけが出さない**
（理由は 01 の「外で直したあとの再解決」）。

**最後の行だけヘッダが `A` を案内しない。** degraded の 1 行はヘッダが情報を落として
いるので「続きはあちら」と言う必要があるが、`Resolved` の `login @ host` は落として
いない —— 添えると、**リポジトリ名とスピナーと分け合う 1 行**（#73 で詰めた側）が
毎回 `A で選び直す` のぶんだけ痩せる。キーの一覧は `Modal::Help`（M6）が出す。
**件数の文言はヘッダとモーダルで 1 本**（`ui::degraded`）。残る 5 腕は共有できない
—— あちらは `[gh: …]` の中に置く前提で主語が `gh` なので、単体で読むと文にならない
（`GhMissing` は「ありません」）。

**外での直し方を書くのはモーダルの側。** `gh auth login` も `gh` の入れ替えも
mewgit は提供していないので、#65 で決めた「**指示を足すのは、その操作を持たない側**」に
そのまま当たる。**外で直したあとに読み直す操作のほうは mewgit が持つ**（`r`。01 の
「外で直したあとの再解決」）ので、外での直し方を書いても行き止まりにはならない ——
**ただし `r` が読み直すのは `gh` と `.git/config` まで。** mewgit の設定ファイルは
起動時に固まるので、`general.gh_bin` と `[[repos]]` を指す案内には再起動が要ることも
添える（01 / 07）（`RemotePicker` が「追跡先を設定してください」を本文に書かないのは、
あちらが**その操作を提供している**ため。裏返しの形）。`Ambiguous` だけは mewgit が
選ばせられるので、本文に書くのは保存先（`.git/config` の `mewgit.user`）で、
外での指示は書かない。

**`AccountState::Resolving` は割らない**（送ってあった判断の答え）。`Resolving` が
兼ねている「走っている最中」と「**永久に頼まれない**」（`TargetState::Unavailable`）を
見分けるのは `state.repo.target` のほう —— あれは**まさにその理由で 3 値**にしてある
（#54 の「`None` に 2 つの意味」）。割ると同じ事実が 2 箇所に出て、`Unavailable` を
立てた側と食い違いうる。`A` が出す通知の文言はこの 2 つで変わる（01 の表）。

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
