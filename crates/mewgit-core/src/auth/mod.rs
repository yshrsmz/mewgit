//! `auth` モジュール。設計は `docs/design/04-auth-account.md` を参照。
//!
//! `gh` から認証状態とトークンを取得し、リポジトリごとに使うアカウントを決める。
//! トークンはメモリ上のみで保持する（不変ルール 3）。
//!
//! リモート URL の解析（[`url`]）、エイリアスの正規化（[`ssh`]）、`gh` の呼び出し
//! （[`gh`]）、アカウント解決（[`resolve`]）はここにある。この `mod.rs` に置くのは
//! `app` の `AppState` / `Msg` が参照する値型だけ。
//!
//! [`Backend`] の production 実装は `github::AccountBackend`（04 の
//! 「`Backend` の production 実装は `github` に置く」）。`auth` は `github` を
//! 名指しできない（00 のレイヤー則）ので、ここには trait だけがある。
//!
//! **どのリモートの URL を見るかは決まった**（04 の「既定リモート」）。規則を持つのは
//! `app` で、材料の [`git_read::Remote`] と `Branch.upstream_config` がどちらも
//! `auth` の上段の型だから（00）。[`RepoTarget`] を組むのに使うのは
//! `update::conventional_remote`（名前の慣習だけで選ぶほう）で、`f` が使う
//! `update::default_remote` とは**別の問いに答える**。
//!
//! [`git_read::Remote`]: crate::git_read::Remote

pub mod gh;
pub(crate) mod probe;
mod resolve;
pub mod ssh;
pub mod url;

pub use resolve::{resolve, Backend};

use secrecy::SecretString;

/// `.git/config` に憶えるアカウントのキー（08 の `[mewgit]`）。
///
/// **読む側（[`resolve`] の git2）と書く側（`GitOp::SetAccount` の argv。03）で
/// 1 本にする。** 別々に書くと、片方を直したときに黙って別のキーを読み書きする。
pub const MEWGIT_USER: &str = "mewgit.user";

/// アカウント解決の対象。`app` の `RepoInfo` は上位層の型なので受け取らない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoTarget {
    /// `ssh -G` で正規化済みのホスト名。
    pub host: String,
    /// API のベース URL に載せる非既定ポート。既定（443）は `None`。
    ///
    /// **ssh の port は入らない。** `ssh://git@ghe.example.com:2222/o/r` の 2222 は
    /// ssh の port で、API は `https://ghe.example.com/api/v3/` のまま。
    /// [`url::RemoteScheme`] が https のときだけ値を持つので、ここへ載る経路も
    /// https のリモートだけになる（04 の「port は scheme に畳む」）。
    pub port: Option<u16>,
    pub owner: String,
    pub repo: String,
}

/// [`RepoTarget::resolve`] の結果。
///
/// **タプルで返さない。** 呼び出し側が並び順を覚えることになる
/// （`git_read::repo::Tracking` と同じ判断）。`app` は
/// [`Msg::TargetResolved`](crate::app::Msg::TargetResolved) で `Box` に載せて運ぶ
/// —— `Msg` のサイズは最大変種で決まり、1 打鍵ごとに作られる `Msg::Key` まで
/// 大きくなる（`msg_stays_small`）。
#[derive(Debug)]
pub struct ResolvedTarget {
    pub target: RepoTarget,
    /// 正規化できなかった理由。**`target` は埋まっている**（`host_raw` のまま）。
    pub unnormalized: Option<AuthError>,
}

impl RepoTarget {
    /// リモート URL からアカウント解決の対象を組む（01 の
    /// 「`RepoInfo.target` を埋める」）。
    ///
    /// ssh のときだけ [`ssh::resolve_ssh_host`] を通す。**失敗しても `Ok` 側に落ちる**
    /// —— `host_raw` を使い、理由を第 2 要素で返す。呼び出し側はそれを通知に出す。
    /// `Result` にしないのは、`Err` を受けた側が `target` を捨ててしまい、04 の
    /// 「degraded ではなく正規化だけを諦める」経路が消えるため。
    ///
    /// **理由は [`AuthError`] のまま返す。** ここで `String` に潰すと、`auth` の中で
    /// エラー型を畳むことになり（規約は「各モジュールで `thiserror`、`app` 層で集約」）、
    /// `SshResolveFailed { alias, message }` が持っている argv 相当の内訳が M6 の
    /// `Notice.detail` へ届かなくなる。
    ///
    /// **port は scheme から取る。** https のときだけ値があり、ssh の port は
    /// [`url::RemoteScheme`] の時点で捨ててある（04 の「port は scheme に畳む」）。
    pub async fn resolve(ssh: &ssh::SshCommand, url: url::RemoteUrl) -> ResolvedTarget {
        let (port, host, unnormalized) = match url.scheme {
            url::RemoteScheme::Https { port } => (port, url.host_raw, None),
            url::RemoteScheme::Ssh => match ssh::resolve_ssh_host(ssh, &url.host_raw).await {
                Ok(host) => (None, host, None),
                Err(err) => (None, url.host_raw, Some(err)),
            },
        };
        ResolvedTarget {
            target: RepoTarget {
                host,
                port,
                owner: url.owner,
                repo: url.repo,
            },
            unnormalized,
        }
    }
}

/// そのホストの 1 アカウント。
///
/// **`gh auth status` の 1 エントリではない。** 同じ login が複数のエントリで出る
/// （実機で `GITHUB_TOKEN` と `keyring` の 2 つが並んだ）ので、`(host, login)` で
/// 畳んである（04）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhLogin {
    pub host: String,
    pub login: String,
    /// gh の現在のアカウントか。**環境変数のトークンを含む** —— `GITHUB_TOKEN` を
    /// export していると、gh はそのエントリを active と報告する（04）。
    pub active: bool,
    /// **gh の設定上**（keyring / hosts.yml）で現在のアカウントか。
    ///
    /// **`gh auth refresh` / `gh auth switch` が相手にするのはこちら。**
    /// [`active`](Self::active) と分けるのは、環境変数のトークンがあると gh が
    /// そちらを active と報告し、設定上のエントリを**全部**非 active にするため
    /// （実測 2026-09-26、gh 2.101.0）。`active` で案内を組むと `gh auth switch` を
    /// 省き、環境変数を外して打ち直した refresh が**別のアカウントにスコープを
    /// 足す**（`/code-review` が拾った）。
    pub stored_active: bool,
    pub scopes: Vec<String>,
    /// gh がそのトークンを検証した結果。
    ///
    /// **絞り込みには使わない。** [`resolve`] は `state` で候補を落とさないと
    /// 決めた（04 の「`LoginState` で候補を絞らない」）—— `Single` は検証せずに
    /// 確定するので、gh の判定で落とすと **probe を一度も通さずに別のアカウントを
    /// 選ぶ**形が作れる。運ぶのは表示のため。
    pub state: LoginState,
}

/// `gh auth status` がそのアカウントに付けた状態（`status.go` の 3 値）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginState {
    /// トークンが通った。
    Success,
    /// ホストに届かなかった。**トークンが悪いとは限らない。**
    Timeout,
    /// gh がトークンを拒まれた。**それでも probe の候補には残る**（上の `state`）。
    Error,
}

/// 使用するアカウント。`token` は `Debug` に出ない（`SecretString`）。
#[derive(Debug)]
pub struct Account {
    pub host: String,
    pub login: String,
    pub token: SecretString,
    pub source: AccountSource,
}

/// どうやってアカウントが決まったか。UI の説明に使う。
///
/// **明示指定の 2 つ（[`GitConfig`](Self::GitConfig) /
/// [`RepoRule`](Self::RepoRule)）を畳まない。** どちらも 04 の手順 1 で決まるが、
/// **mewgit から外せるのは前者だけ** —— `.git/config` は `GitOp::UnsetAccount` で
/// 消せて、設定ファイルの `[[repos]]` は `git config` の手が届かない（08 が
/// 「`config` モジュールは `.git/config` を見ない」と割った境目の裏側）。1 本に
/// 畳んだまま両方の置き場所を案内に並べる形は #73 で出荷していて、「選び直せない」を
/// `/code-review` に拾われた（01 の「アカウントの選び直し」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountSource {
    /// `.git/config` の `mewgit.user`。**`u` で外せる唯一の出どころ。**
    GitConfig,
    /// 設定ファイルの `[[repos]]` マッチ。**mewgit からは外せない。**
    RepoRule,
    /// そのホストのログインが 1 つだけだった。
    Single,
    /// 候補トークンで `GET /repos/{owner}/{repo}` を試して 1 つに絞れた。
    Probe,
}

impl AccountSource {
    /// `GitOp::UnsetAccount` で外せるか（01 の「アカウントの選び直し」）。
    ///
    /// **キーの案内（`[u] 指定を解除`）と `u` の受け口が同じ述語を読む。** 別々に
    /// `matches!` を書くと、効かない画面に案内だけが出る（#53）。
    pub fn is_unsettable(self) -> bool {
        matches!(self, AccountSource::GitConfig)
    }
}

/// アカウント解決の状態。`Degraded` でも Git 機能は動かす。
///
/// **[`resolve`] の戻り値ではない。** あちらは `Result<Account, DegradedReason>` を
/// 返し、`Resolving` を返せないことが型になっている（04 の「`resolve` の戻り値」）。
/// `Resolving` はここにだけある値で、**`Cmd::ResolveAccount` をまだ出していない**
/// ことを表す —— 走っている最中と、既定リモートが無いなどで
/// [`RepoTarget`] を作れず一度も頼まないリポジトリの両方がこれになる（01）。
#[derive(Debug, Default)]
pub enum AccountState {
    #[default]
    Resolving,
    Resolved(Account),
    Degraded(DegradedReason),
    /// `config.github.enabled = false`。**`AppState::new` だけが入れる**（01 の
    /// 「GitHub 機能を切る」）。
    ///
    /// **[`DegradedReason`] に腕を足さない。** あちらは「GitHub を使いたいのに
    /// 使えない」理由の一覧で、直し方を案内する相手が居る。切ってあるのはそう設定した
    /// 結果なので、混ぜると `Modal::AccountPicker` が直し方を並べる相手を失う（04）。
    ///
    /// **[`Resolving`](Self::Resolving) で代用しない。** あれは「まだ頼んでいない /
    /// 走っている最中」なので、`A` の「解決しています」も右端のスピナーの説明も嘘に
    /// なる。
    Disabled,
}

impl AccountState {
    /// GitHub 機能が設定で切ってあるか（01 の「GitHub 機能を切る」）。
    ///
    /// **網羅 `match` の腕とは別の問い。** あちらは「この状態で何を出すか」を読み手
    /// ごとに選ばせるためのもので、こちらは「GitHub を使うか」の 1 つの問い。読むのは
    /// 巡回候補（`Pane::Prs` を入れるか）と `Cmd::ResolveAccount` の発行の 2 人。
    pub fn is_disabled(&self) -> bool {
        matches!(self, AccountState::Disabled)
    }
}

/// GitHub 機能を degraded にする理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DegradedReason {
    /// `gh` が PATH に無い。
    GhMissing,
    /// `gh` が古い。`gh auth status --json` は [`gh::REQUIRED`] で入った（04）。
    ///
    /// 要求版は案内に出すが、**値としては持たない** —— 定数から引ける
    /// （`ui` からは [`gh::REQUIRED`] を読む）。
    GhTooOld {
        /// 見つかった版。解釈できなければ `gh --version` の生の 1 行目が入る。
        found: String,
    },
    /// `gh` は居るのに答えが返らなかった（起動できない / 非 0 終了 / 出力を
    /// 解釈できない）。**3 つを分けない** —— 直し方の違いは `message` に出ており、
    /// これで分岐する読み手が UI に無い（[`resolve`] の `reason_for`）。
    ///
    /// **`message` は `Modal::AccountPicker` が出す**（04 の「内訳を出すのは
    /// `Modal::AccountPicker`」）。ヘッダは 1 行なので「gh を実行できません」しか
    /// 出さない（07）—— 畳んだ 3 つを見分ける手掛かりは `A` で開く画面のほうにある。
    Unusable {
        message: String,
    },
    NotLoggedIn {
        host: String,
    },
    /// `.git/config` の `mewgit.user` か設定ファイルの `[[repos]]` が指す login の
    /// トークンを取れなかった。
    ///
    /// **[`Unusable`](Self::Unusable) に畳まない。** 直すのは gh ではなく設定の
    /// ほうなので、どの login を指していたかを案内に出す。**手順 2 へ落とさない**
    /// のも同じ理由 —— 指定を無視して別のアカウントを選ぶと、EMU と個人アカウントを
    /// 取り違える。
    ///
    /// **[`AccountSource`] を運ぶ。** 直す先が `.git/config` か設定ファイルの
    /// `[[repos]]` かで案内が変わるうえ、**前者は `A` の `u` でその場で外せる** ——
    /// 運ばないと、壊れた指定を憶えたまま `A` が案内しか出せない行き止まりが残る
    /// （これは `Resolved` 側と同じ穴の、degraded に落ちた版）。
    ConfiguredUnavailable {
        login: String,
        source: AccountSource,
        message: String,
    },
    /// 届いたことを確かめられた候補が 1 つも無かった。
    ///
    /// **「権限が無い」と読まない。** 全部が [`AttemptFailure::Undetermined`]
    /// （起動時に回線が無い）の形もここに来る。**理由は `attempts` に 1 件ずつ
    /// 入っている**ので、案内はそちらから組む。
    NoAccess {
        attempts: Vec<ProbeAttempt>,
    },
    /// 候補を 1 つに絞れなかった。UI で選ばせる。
    ///
    /// **並ぶのは「除外できなかった」候補** —— probe を通ったものと、
    /// [`AttemptFailure::Undetermined`] で確かめられなかったもの。断られたもの
    /// （401 / 404）は落とす（選べるアカウントとして届かないログインを出さない）。
    /// **確かめられなかったものを混ぜる**のは、1 本が回線やレート制限で落ちただけの
    /// 回に、通った 1 件へ黙って latch しないため（04 の手順 4）。
    /// **[`GhLogin`] ごと運ぶ**ので、選び直すときに `gh auth status` を
    /// 呼び直さなくてよい（04）。
    Ambiguous {
        candidates: Vec<GhLogin>,
    },
}

impl DegradedReason {
    /// 選ばせてよい候補。[`Ambiguous`](Self::Ambiguous) 以外は空。
    ///
    /// **読み手が散っているので 1 本にする** —— `AccountView::candidates`（そこから
    /// `Modal::AccountPicker` の描画とカーソル移動と `Enter` の確定へ広がる。01 の
    /// 「アカウントの選び直し」）と、ヘッダが `A` に添える文言（`A で選ぶ` /
    /// `A で詳細`。07）。別々に `matches!` を書くと、`NoAccess` を選ばせない約束を
    /// 誰かが守り損ねる。
    ///
    /// **`NoAccess` を空にするのは意図**（04 の手順 4）。どれも届かない状態で
    /// アカウントを選んでも何も直らない。
    pub fn candidates(&self) -> &[GhLogin] {
        match self {
            DegradedReason::Ambiguous { candidates } => candidates,
            DegradedReason::GhMissing
            | DegradedReason::GhTooOld { .. }
            | DegradedReason::Unusable { .. }
            | DegradedReason::NotLoggedIn { .. }
            | DegradedReason::ConfiguredUnavailable { .. }
            | DegradedReason::NoAccess { .. } => &[],
        }
    }

    /// 明示指定が原因で degraded になったなら、その指定の出どころ。
    ///
    /// **網羅 `match` で書く**（[`candidates`](Self::candidates) と同じ形）。
    /// `_ =>` の受け皿を置くと、**出どころを運ぶ変種を将来足したときに黙って
    /// `None` になる** —— 読み手は `A` の `u`（指定の解除）が効くかどうかなので、
    /// 効くはずの画面で何も起きない形が、コンパイルエラーにならずに通る。
    pub fn configured_source(&self) -> Option<AccountSource> {
        match self {
            DegradedReason::ConfiguredUnavailable { source, .. } => Some(*source),
            // **`gh` が居ない・古い・答えないはプロセス全体の条件**で、
            // `mewgit.user` を外しても変わらない（`resolve` の `configured_failed`）。
            DegradedReason::GhMissing
            | DegradedReason::GhTooOld { .. }
            | DegradedReason::Unusable { .. }
            | DegradedReason::NotLoggedIn { .. }
            | DegradedReason::NoAccess { .. }
            | DegradedReason::Ambiguous { .. } => None,
        }
    }
}

/// `NoAccess` のとき、どのログインがなぜ落ちたか。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeAttempt {
    /// **[`GhLogin`] ごと運ぶ**（`Ambiguous` と同じ）。案内が読むのは
    /// `host`（`gh auth refresh -h` に渡すリポジトリのホスト）と `active`
    /// （`gh auth refresh` はアクティブなアカウントにしか効かないので、そうで
    /// なければ `gh auth switch` を先に書く）。04 の「権限不足の案内」。
    /// 1 フィールドずつ抜くと、案内の材料が増えるたびに型と詰め替えが増える。
    pub login: GhLogin,
    pub failure: AttemptFailure,
}

/// 候補 1 件がリポジトリに届かなかった理由。
///
/// **`status: u16` では足りない**（「二値の結果に 3 つ目を足したら入口と出口を
/// 数える」）。HTTP まで届かない失敗は**ステータスを持たない**ので、0 や 599 を
/// 埋めると案内が「HTTP 0 で断られました」になる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptFailure {
    /// HTTP のステータスで断られた（401 / 403 / 404）。**このアカウントでは無理**。
    Denied {
        status: u16,
        /// 応答のヘッダから判った、足りないスコープ（05 の `MissingScope`）。
        /// 判らなければ `None`（ヘッダが無い / トークンが既に持っている）。
        missing_scope: Option<String>,
    },
    /// SAML SSO の承認が要る 403。**トークンを org に承認すれば通る**ので、
    /// 案内が `Denied` と違う。
    ///
    /// **`Denied` に `saml: bool` を足さない**（04）—— `Denied { 404, saml: true }`
    /// が型で組める。SAML の拒否は常に 403 なので、ステータスを持たない腕にする。
    SsoRequired,
    /// 断られてはいないが確かめられなかった（回線、TLS バックエンドの初期化、
    /// レート制限）。**もう一度なら通りうる**ので `Denied` と分ける ——
    /// レート制限を `Denied { status: 429 }` に畳むと、待てば直るものを
    /// 「アクセス権がありません」として出すことになる（05 の `RateLimited` は
    /// 429 だけでなく**印の付いた 403** からも来るので、ステータスを載せても嘘になる）。
    Undetermined { message: String },
    /// `gh` からトークンを取れなかった。**probe まで行っていない。**
    NoToken { message: String },
}

/// カンマ区切りの OAuth スコープを割る。
///
/// **2 人の利用者がいる。** `gh auth status` の `scopes`（カンマ区切りの 1 本の
/// 文字列）と、GitHub API がエラーの応答に添える `x-accepted-oauth-scopes` /
/// `x-oauth-scopes`（`github::client`）。00 の依存の向きで `github` は `auth` の上段なので、
/// 下段のここに置いて上から呼ぶ。
pub(crate) fn split_scopes(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// `auth` 層のエラー。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    /// **パスを載せる。** `general.gh_bin` / `MEWGIT_GH_BIN` の書き間違いでも
    /// ここに来るので、「gh が見つかりませんでした」だけだと PATH を探させてしまう。
    #[error("{bin} が見つかりませんでした")]
    GhMissing { bin: String },
    /// `gh` はあるが起動できない（実行権限が無いなど）。**`GhMissing` と分ける** ——
    /// 「見つかりませんでした」と出すと、権限の問題を探せなくなる。
    #[error("{message}")]
    GhUnrunnable { message: String },
    /// **`required` は持たない。** 常に [`gh::REQUIRED`] なので、持たせると
    /// 誤った値を組める（「導出できる値を引数に取らせない」）。
    #[error("gh {found} は古すぎます（{} 以上が必要です）", gh::REQUIRED)]
    GhTooOld { found: String },
    /// **`message` は `stderr` ではない。** 終了コードも入る（stderr が空のまま
    /// 落ちる gh があるため）。argv は不変ルール 4 と同じ理由で残す。
    #[error("gh の実行に失敗しました: {}: {message}", .argv.join(" "))]
    GhFailed { argv: Vec<String>, message: String },
    #[error("gh の出力を解釈できませんでした: {message}")]
    GhUnparsable { message: String },
    #[error("{host} にログインしていません")]
    NotLoggedIn { host: String },
    #[error("ssh -G {alias} に失敗しました: {message}")]
    SshResolveFailed { alias: String, message: String },
    #[error("リモート URL を解釈できませんでした: {url}")]
    InvalidRemoteUrl { url: String },
}

#[cfg(test)]
mod tests {
    use super::url::RemoteUrl;
    use super::*;
    use crate::test_util::{fake_ssh, fake_ssh_host};

    #[tokio::test]
    async fn an_https_remote_keeps_its_port_and_does_not_run_ssh() {
        // **偽 ssh は `example.invalid` を返す。** https で `ssh -G` を回していたら
        // host がそちらに化けるので、これが「回していない」の見張りになる。
        let (_dir, ssh) = fake_ssh_host("example.invalid");
        let url = RemoteUrl::parse("https://ghe.example.com:8443/acme/tools.git").unwrap();
        let ResolvedTarget {
            target,
            unnormalized,
        } = RepoTarget::resolve(&ssh, url).await;

        assert_eq!(target.host, "ghe.example.com");
        assert_eq!(target.port, Some(8443));
        assert_eq!(target.owner, "acme");
        assert_eq!(target.repo, "tools");
        assert_eq!(unnormalized, None);
    }

    #[tokio::test]
    async fn an_ssh_remote_is_normalized_and_carries_no_port() {
        // **ssh の port は API の port ではない**ので、2222 は `RemoteScheme` の時点で
        // 捨てられている（04 の「port は scheme に畳む」）。
        let (_dir, ssh) = fake_ssh_host("github.com");
        let url = RemoteUrl::parse("ssh://git@gh-work:2222/yshrsmz/mewgit.git").unwrap();
        let ResolvedTarget {
            target,
            unnormalized,
        } = RepoTarget::resolve(&ssh, url).await;

        assert_eq!(target.host, "github.com");
        assert_eq!(target.port, None);
        assert_eq!(unnormalized, None);
    }

    #[tokio::test]
    async fn a_failed_normalization_keeps_the_raw_host_and_says_why() {
        // **degraded ではなく正規化だけを諦める**（04）。`Err` を返して呼び出し側に
        // `target` を捨てさせると、この経路が消える。
        let (_dir, ssh) = fake_ssh("echo 'no hostname here'\n");
        let url = RemoteUrl::parse("git@gh-work:yshrsmz/mewgit.git").unwrap();
        let ResolvedTarget {
            target,
            unnormalized,
        } = RepoTarget::resolve(&ssh, url).await;

        assert_eq!(target.host, "gh-work");
        assert_eq!(target.owner, "yshrsmz");
        // **どの alias で失敗したかを持ったまま返す**（`String` に潰さない）。
        let reason = unnormalized.expect("理由が付いていない");
        assert!(
            matches!(&reason, AuthError::SshResolveFailed { alias, .. } if alias == "gh-work"),
            "{reason:?}"
        );
    }
}
