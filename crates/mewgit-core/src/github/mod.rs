//! `github` モジュール。設計は `docs/design/05-github-api.md` を参照。
//!
//! `gh api` は使わず `reqwest` で直接呼ぶ。型付きレスポンス・エラーヘッダの取得・
//! 並列制御のため。
//!
//! ベース URL の形は 2 つある（`client` の `base_url` のドキュメント）。**port は
//! https の非既定ポートだけ**が `auth::RepoTarget` から届く —— ssh の port は
//! `auth::url::RemoteScheme` の時点で捨ててある（04 の「port は scheme に畳む」）。

mod account;
mod client;
mod wire;

pub use account::AccountBackend;
pub use client::GithubClient;

/// リポジトリのメタ情報。`auth::resolve` の probe 兼用なので、
/// **存在するかどうか**（= `Ok` か [`GithubError::NotFound`] か）だけを見る
/// 呼び出しもある。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoMeta {
    pub owner: String,
    pub repo: String,
    pub default_branch: String,
}

/// PR 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub state: PrStatus,
    pub draft: bool,
    pub head: RefSpec,
    pub base: RefSpec,
    pub url: String,
    pub author: String,
    /// レビューは別リクエスト（`reviews`）なので、未取得なら `None`。
    /// **`ReviewSummary::default()` を入れない** —— 「レビュー 0 件」と
    /// 区別できなくなる。`checks` と対称にしてある。
    pub review: Option<ReviewSummary>,
    /// チェックは別リクエストなので、未取得なら `None`。
    pub checks: Option<CheckSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrStatus {
    Open,
    Closed,
    Merged,
}

/// PR の head / base。
///
/// **`repo` が `Option` なのは fork の削除を表すため。** GitHub は削除された
/// fork の `head.repo` を `null` で返す。空文字で埋めると「owner が空」と
/// 区別できなくなり、PR チェックアウト（05）が**同一リポジトリかどうかを owner の
/// 比較で決めている**ので、消えた fork を fork 経路へ落とす。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefSpec {
    /// fork が削除されていれば `None`。fork のときは元リポジトリと owner が異なる。
    pub repo: Option<RepoRef>,
    pub branch: String,
    pub sha: String,
}

/// リポジトリの指し先。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    pub owner: String,
    pub repo: String,
}

/// CI チェック 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub state: CheckState,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Success,
    Failure,
    Pending,
    Skipped,
    Neutral,
}

/// チェックの集計。ヘッダとブランチ一覧に出す。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CheckSummary {
    pub success: usize,
    pub failure: usize,
    pub pending: usize,
}

/// レビューの集計。ユーザーごとに最新のレビューだけを数える。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReviewSummary {
    pub approved: usize,
    pub changes_requested: usize,
    pub commented: usize,
}

/// GitHub API のエラー。403 / 404 は握りつぶさず本文の先頭を残す。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GithubError {
    /// ベース URL を組めないホスト。`RemoteUrl` は host を検査するが、
    /// **`ssh -G` が返す `hostname` は検査を通っていない**ので、ここに届きうる。
    #[error("GitHub API: ホスト名が URL になりません: {host}")]
    InvalidHost { host: String },
    #[error("GitHub API: 401 Unauthorized")]
    Unauthorized,
    #[error("GitHub API: 403 Forbidden")]
    Forbidden {
        needed_scopes: Vec<String>,
        /// 本文に SAML enforcement が含まれていた。案内を SSO 承認に切り替える。
        saml: bool,
        body_head: String,
    },
    #[error("GitHub API: 404 Not Found")]
    NotFound,
    #[error("GitHub API: レート制限に達しました")]
    RateLimited {
        /// 回復時刻。Unix epoch からの秒数。生の epoch を出しても読めないので
        /// `Display` には含めない。表示用の整形は `ui` が行う。
        ///
        /// **`None` は「いつ戻るか判らない」。** 0 を入れると 1970 年になり、
        /// 表示側は「もう明けている」と読んで即座に叩き直す。二次レート制限
        /// （`retry-after` だけが付き `x-ratelimit-reset` が無い）で実際に起きる。
        reset_at: Option<i64>,
    },
    #[error("GitHub API: 通信に失敗しました: {message}")]
    Network { message: String },
    /// 応答を型に落とせなかった。**`Network` と分ける** —— GHES の版差で
    /// フィールドが欠けた形がここに来るので、「通信に失敗」と出すと
    /// サーバ互換性の問題を回線の問題として調べさせることになる。
    #[error("GitHub API: 応答を解釈できませんでした: {message}")]
    Decode { message: String },
    #[error("GitHub API: {status}: {body_head}")]
    Api { status: u16, body_head: String },
}
