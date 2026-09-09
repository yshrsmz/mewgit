//! `auth` モジュール。設計は `docs/design/04-auth-account.md` を参照。
//!
//! `gh` から認証状態とトークンを取得し、リポジトリごとに使うアカウントを決める。
//! トークンはメモリ上のみで保持する（不変ルール 3）。
//!
//! リモート URL の解析（[`url`]）、エイリアスの正規化（[`ssh`]）、`gh` の呼び出し
//! （[`gh`]）は実装済み。`resolve` は M4 の次のタスクで実装するので、ここには
//! `app` の `AppState` / `Msg` が参照する型を置いてある。
//!
//! **どのリモートの URL を見るかは M4 で決める**（04 の「既定リモートを決めるのは M4」）。
//! それまで `RepoInfo.target` は `None` のまま。

pub mod gh;
pub mod ssh;
pub mod url;

use secrecy::SecretString;

/// アカウント解決の対象。`app` の `RepoInfo` は上位層の型なので受け取らない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoTarget {
    /// `ssh -G` で正規化済みのホスト名。
    pub host: String,
    pub owner: String,
    pub repo: String,
}

/// `gh auth status` の 1 エントリ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhLogin {
    pub host: String,
    pub login: String,
    pub active: bool,
    pub scopes: Vec<String>,
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
    /// `gh` が古い。`gh auth status --json` は 2.81.0 で入った（04）。
    GhTooOld {
        /// 見つかった版。案内に出すので、解釈できなければ生の 1 行目を入れる。
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

/// `auth` 層のエラー。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("gh が見つかりませんでした")]
    GhMissing,
    #[error("gh {found} は古すぎます（{required} 以上が必要です）")]
    GhTooOld { found: String, required: String },
    #[error("gh の実行に失敗しました: {}: {stderr}", .argv.join(" "))]
    GhFailed { argv: Vec<String>, stderr: String },
    #[error("gh の出力を解釈できませんでした: {message}")]
    GhUnparsable { message: String },
    #[error("{host} にログインしていません")]
    NotLoggedIn { host: String },
    #[error("ssh -G {alias} に失敗しました: {message}")]
    SshResolveFailed { alias: String, message: String },
    #[error("リモート URL を解釈できませんでした: {url}")]
    InvalidRemoteUrl { url: String },
}
