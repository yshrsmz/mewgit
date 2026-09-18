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
    /// レビューを依頼されている login。**チーム宛ての依頼（`requested_teams`）は
    /// 入らない** —— チームの一員かどうかを答えるにはチームの数だけ membership を
    /// 叩くことになる（07 の「PRs ペイン」で受け入れた穴）。
    pub requested_reviewers: Vec<String>,
}

/// PR 1 件ぶんの集計（チェックとレビュー）。[`GithubClient::pr_detail`] が返す。
///
/// **[`PullRequest`] には入れない。** 一覧（`pulls`）はこれを返さないので、
/// `Option` で持たせると「まだ引いていない」と「引いたが失敗した」が同じ `None` に
/// 落ちる（`CLAUDE.md` の「`None` の理由を 2 つ以上持たせない」）。
/// 段階を持つのは `app::PrState` の側で、失敗の理由（[`GithubError`]）もそちらが運ぶ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrDetail {
    pub checks: CheckSummary,
    pub review: ReviewSummary,
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

/// チェックの集計。PRs ペインの各行に出す（07 の「PRs ペイン」）。
///
/// **[`CheckState`] の腕ごとに 1 つ数える。畳まない** —— `Skipped` と `Neutral` を
/// 成功か失敗のどちらかに足すと**どちらも嘘になる**（07）。数える側で畳むと、
/// 畳んだことが `ui` から見えないまま「全部緑」に見える。
///
/// 腕を足したときにここも足させるのは [`CheckSummary::add`] の網羅 `match` で、
/// **enforcement はそこ 1 つ**。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CheckSummary {
    pub success: usize,
    pub failure: usize,
    pub pending: usize,
    pub skipped: usize,
    pub neutral: usize,
}

impl CheckSummary {
    /// チェック 1 件を数える。
    pub fn add(&mut self, state: CheckState) {
        let slot = match state {
            CheckState::Success => &mut self.success,
            CheckState::Failure => &mut self.failure,
            CheckState::Pending => &mut self.pending,
            CheckState::Skipped => &mut self.skipped,
            CheckState::Neutral => &mut self.neutral,
        };
        *slot += 1;
    }
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

impl GithubError {
    /// **この失敗は、同じトークンで叩くどの対象でも同じように起きるか。**
    ///
    /// 扇形（[`GithubClient::pr_details`]）が 1 件目で受けたときに残りを頼むかを
    /// これで決める。**答えられるのは `github` だけ**なので、呼ぶ側で
    /// `matches!(err, GithubError::RateLimited { .. })` と書かない（`CLAUDE.md` の
    /// 「層をまたぐ述語は、その名前が約束している問いに誰が答えられるかで置き場所を
    /// 決める」）。**腕を足したときに選ばせる**ため網羅 `match` にしてある。
    ///
    /// 真にするのは**プロセス全体の条件**だけ。`NotFound` は「その PR の fork が
    /// 消えた」のような行ごとの事情なので偽 —— 1 件で全部を諦めると、1 つの消えた
    /// fork が一覧ぜんぶの集計を落とす。
    pub fn repeats_for_every_target(&self) -> bool {
        match self {
            // 待てば直るが、いま叩き続けると二次制限を踏みに行く（05）。
            GithubError::RateLimited { .. } => true,
            // トークンの問題。残りを叩いても同じ答えを買うだけ。
            GithubError::Unauthorized | GithubError::Forbidden { .. } => true,
            // クライアントを組めない。どの対象でも組めない。
            GithubError::InvalidHost { .. } => true,
            // 対象ごとに違いうる。`Network` と `Decode` も**その 1 本だけ**の事故と
            // して扱う —— 回線が落ちていれば残りも同じように落ちるが、それは通知に
            // 出るだけで、諦めても叩いても払う往復は変わらない。
            GithubError::NotFound
            | GithubError::Network { .. }
            | GithubError::Decode { .. }
            | GithubError::Api { .. } => false,
        }
    }
}
