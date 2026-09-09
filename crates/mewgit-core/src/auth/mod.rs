//! `auth` モジュール。設計は `docs/design/04-auth-account.md` を参照。
//!
//! `gh` から認証状態とトークンを取得し、リポジトリごとに使うアカウントを決める。
//! トークンはメモリ上のみで保持する（不変ルール 3）。
//!
//! リモート URL の解析（[`url`]）、エイリアスの正規化（[`ssh`]）、`gh` の呼び出し
//! （[`gh`]）は実装済み。`resolve` は M4 の次のタスクで実装するので、ここには
//! `app` の `AppState` / `Msg` が参照する型を置いてある。
//!
//! **どのリモートの URL を見るかは決まった**（04 の「既定リモート」）。規則を持つのは
//! `app` で、材料の [`git_read::Remote`] と `Branch.upstream_config` がどちらも
//! `auth` の上段の型だから（00）。[`RepoTarget`] を組むのに使うのは
//! `update::conventional_remote`（名前の慣習だけで選ぶほう）で、`f` が使う
//! `update::default_remote` とは**別の問いに答える**。
//!
//! [`git_read::Remote`]: crate::git_read::Remote

pub mod gh;
mod probe;
pub mod ssh;
pub mod url;

use secrecy::SecretString;

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
    pub active: bool,
    pub scopes: Vec<String>,
    /// gh がそのトークンを検証した結果。
    ///
    /// **`gh` の層で絞らずに運ぶ。** 04 は「`Timeout` を `Error` と同じに畳んで
    /// よいか」を `auth::resolve` のタスクへ送っており、ここで落とすと**送った先に
    /// 判断の材料が届かない**。
    pub state: LoginState,
}

/// `gh auth status` がそのアカウントに付けた状態（`status.go` の 3 値）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginState {
    /// トークンが通った。probe の候補になる。
    Success,
    /// ホストに届かなかった。**トークンが悪いとは限らない。**
    Timeout,
    /// gh がトークンを拒まれた。
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountSource {
    /// `.git/config` の `mewgit.user` か設定ファイルの `[[repos]]`。
    Config,
    /// そのホストのログインが 1 つだけだった。
    Single,
    /// 候補トークンで `GET /repos/{owner}/{repo}` を試して 1 つに絞れた。
    Probe,
}

/// アカウント解決の状態。`Degraded` でも Git 機能は動かす。
#[derive(Debug, Default)]
pub enum AccountState {
    #[default]
    Resolving,
    Resolved(Account),
    Degraded(DegradedReason),
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
    NotLoggedIn {
        host: String,
    },
    /// どの候補トークンでもリポジトリにアクセスできなかった。
    NoAccess {
        attempts: Vec<ProbeAttempt>,
    },
    /// 複数の候補がアクセスできた。UI で選ばせる。
    Ambiguous {
        candidates: Vec<GhLogin>,
    },
}

/// `NoAccess` のとき、どのログインがどの HTTP ステータスで落ちたか。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeAttempt {
    pub login: String,
    pub status: u16,
}

/// カンマ区切りの OAuth スコープを割る。
///
/// **2 人の利用者がいる。** `gh auth status` の `scopes`（カンマ区切りの 1 本の
/// 文字列）と、GitHub API が 403 に添える `x-accepted-oauth-scopes`
/// （`github::client`）。00 の依存の向きで `github` は `auth` の上段なので、
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
