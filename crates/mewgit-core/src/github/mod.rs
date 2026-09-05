//! `github` モジュール。設計は `docs/design/05-github-api.md` を参照。
//!
//! `gh api` は使わず `reqwest` で直接呼ぶ。型付きレスポンス・エラーヘッダの取得・
//! 並列制御のため。
//!
//! 現時点では `app` の `AppState` / `Msg` が参照する型だけを定義している。
//! `GithubClient` は M4 で実装する。

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
    pub review: ReviewSummary,
    /// チェックは別リクエストなので、未取得なら `None`。
    pub checks: Option<CheckSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrStatus {
    Open,
    Closed,
    Merged,
}

/// PR の head / base。fork の場合 `owner` が元リポジトリと異なる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefSpec {
    pub owner: String,
    pub repo: String,
    pub branch: String,
    pub sha: String,
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
        reset_at: i64,
    },
    #[error("GitHub API: 通信に失敗しました: {message}")]
    Network { message: String },
    #[error("GitHub API: {status}: {body_head}")]
    Api { status: u16, body_head: String },
}
