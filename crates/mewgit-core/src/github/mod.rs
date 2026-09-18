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

/// レートリミットの残量。**自分が出したリクエストの応答ヘッダから採る**（05）。
///
/// **`/rate_limit` は産出元にしない。** 同じセッションで連続して叩いても数が合わず、
/// `reset` は後ろを返した（05 の実測表。2026-09-18）。1 往復を余分に払ったうえで
/// 嘘を出すことになる。
///
/// **`used` は持たない。** `limit - remaining` で出るので、持つと同じ値の置き場所が
/// 2 つになる（`CLAUDE.md` の「導出できる値を引数に取らせない」）。新しさの比較に
/// 要るのは [`RateLimit::newer`] の中だけ。
///
/// **バケットは `core` の 1 つ。** `search` を使う経路は無いと決めてある（05）ので、
/// 別のバケットの数が混ざる形は [`RateLimit::from_parts`] が入口で断る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    /// 窓あたりの上限。`x-ratelimit-limit`。
    pub limit: u32,
    /// 残り。`x-ratelimit-remaining`。
    pub remaining: u32,
    /// 窓が戻る時刻（UNIX 秒）。`x-ratelimit-reset`。
    pub reset: i64,
}

impl RateLimit {
    /// 応答ヘッダの 3 つから組む。**どれか 1 つでも欠けたら `None`**。
    ///
    /// **`resource` が `core` でなければ採らない。** 上限が 2 桁小さい `search` の
    /// 数をヘッダに出すと、残量が突然 30 になる（05 の実測表）。**いま mewgit が叩く
    /// エンドポイントは全部 `core`** なので、この腕を踏ませられるのは
    /// wiremock のテストだけ。
    ///
    /// **ヘッダごと無いこともある。** レートリミットを切った GHES がそれで、
    /// そのときは残量を出さない（`None`）だけで、エラーにはしない。
    pub fn from_parts(
        resource: Option<&str>,
        limit: Option<i64>,
        remaining: Option<i64>,
        reset: Option<i64>,
    ) -> Option<Self> {
        if resource.is_some_and(|resource| resource != "core") {
            return None;
        }
        Some(RateLimit {
            limit: u32::try_from(limit?).ok()?,
            remaining: u32::try_from(remaining?).ok()?,
            reset: reset?,
        })
    }

    /// 2 つのうち新しいほう。
    ///
    /// **並行に飛んだ応答は届く順が新しさの順ではない**（扇形は同時 4 本。05）。
    /// 比べるのは `(reset, used)` で、`used` は `limit - remaining` ——
    /// 同じ窓のあいだ残量は減る一方なので、**残量の少ないほうが新しい**。
    /// 窓が変われば `reset` が先へ動く。
    ///
    /// **判定はここ 1 本。** 同時に飛んだ応答を畳む [`GithubClient`] と、ジョブを
    /// またいで届く返信を畳む `app::PrState` の両方がこれを呼ぶ（`CLAUDE.md` の
    /// 「同じ判定を 2 箇所に書かない」）。
    pub fn newer(self, other: Self) -> Self {
        let key = |limit: &Self| (limit.reset, limit.limit.saturating_sub(limit.remaining));
        if key(&other) > key(&self) {
            other
        } else {
            self
        }
    }

    /// 残量が乏しいか（上限の 10% 以下）。ヘッダの色を決める（07）。
    ///
    /// **割合で見る。** 上限は GHES や App のトークンで変わるので、固定の本数だと
    /// 上限 60 のトークンでは最初から乏しく、5000 では最後まで点かない。
    pub fn is_low(&self) -> bool {
        self.remaining <= self.limit / 10
    }
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
            // して扱う —— 回線が落ちていれば残りも同じように落ちるが、諦めても
            // 叩いても払う往復は変わらない。
            //
            // **1 件ごとの失敗は通知に出ない。** 出るのは行の印だけ（07 で受け入れた
            // 穴）。行き止まりにならないのは、**引けなかった記憶が次に一覧を引いた
            // ときに落ちる**から（`app::PrState::loaded`）—— `f` を押せば引き直す。
            GithubError::NotFound
            | GithubError::Network { .. }
            | GithubError::Decode { .. }
            | GithubError::Api { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_failures_that_are_the_whole_process_stop_the_fan_out() {
        // **腕ごとに理由が違う。** `NotFound` は「その PR の fork が消えた」のような
        // 行ごとの事情なので偽 —— 真にすると、**1 つの消えた fork が一覧ぜんぶの
        // 集計を落とす**。`Unauthorized` / `Forbidden` はトークンの問題なので真で、
        // 偽にすると同じ 401 を行の数だけ買う。
        //
        // **腕を 1 つずつ並べる。** 真偽をまとめて 2 本の `assert` にすると、腕を
        // 足したときに**どちらにも入れないまま**通る。
        let cases: [(GithubError, bool); 8] = [
            (GithubError::RateLimited { reset_at: None }, true),
            (GithubError::Unauthorized, true),
            (
                GithubError::Forbidden {
                    needed_scopes: Vec::new(),
                    saml: false,
                    body_head: String::new(),
                },
                true,
            ),
            (
                GithubError::InvalidHost {
                    host: "-bad".to_string(),
                },
                true,
            ),
            (GithubError::NotFound, false),
            (
                GithubError::Network {
                    message: "eof".to_string(),
                },
                false,
            ),
            (
                GithubError::Decode {
                    message: "eof".to_string(),
                },
                false,
            ),
            (
                GithubError::Api {
                    status: 500,
                    body_head: String::new(),
                },
                false,
            ),
        ];
        for (err, wanted) in cases {
            assert_eq!(
                err.repeats_for_every_target(),
                wanted,
                "{err:?} の判断が違う"
            );
        }
    }
}
