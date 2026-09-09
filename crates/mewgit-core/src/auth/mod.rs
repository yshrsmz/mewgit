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
mod probe;
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
    #[error("gh が見つかりませんでした")]
    GhMissing,
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
