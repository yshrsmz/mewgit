//! GitHub REST API のレスポンスをそのまま受ける型と、値型への写像。
//!
//! **API の形と `github` の値型を分ける。** API は使わないフィールドのほうが多く、
//! `Option` の意味も API 側の都合（fork が削除されると `head.repo` が `null` になる、
//! など）なので、そのまま公開すると呼ぶ側が API の事情を知ることになる。
//!
//! 未知のフィールドは無視する（`serde` の既定）。GitHub は互換のあるフィールド追加を
//! 予告なく行うので、`deny_unknown_fields` にはしない。

use serde::Deserialize;
use std::collections::HashMap;

use super::{Check, CheckState, PrStatus, PullRequest, RefSpec, RepoMeta, RepoRef, ReviewSummary};

#[derive(Debug, Deserialize)]
pub(super) struct WireRepo {
    pub name: String,
    pub owner: WireUser,
    pub default_branch: String,
}

impl From<WireRepo> for RepoMeta {
    fn from(w: WireRepo) -> Self {
        RepoMeta {
            owner: w.owner.login,
            repo: w.name,
            default_branch: w.default_branch,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct WireUser {
    pub login: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct WirePull {
    pub number: u64,
    pub title: String,
    /// `"open"` / `"closed"`。merge されたかは `merged_at` でしか判らない。
    pub state: String,
    pub merged_at: Option<String>,
    pub draft: bool,
    pub html_url: String,
    pub user: Option<WireUser>,
    pub head: WirePullRef,
    pub base: WirePullRef,
    /// **一覧（`GET /repos/{o}/{r}/pulls`）にも入っている。** レビュー依頼の印は
    /// ここから出す（07 の「PRs ペイン」）。GHES の版差で欠けても落ちないよう
    /// `default` を付ける。
    #[serde(default)]
    pub requested_reviewers: Vec<WireUser>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WirePullRef {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub sha: String,
    /// **fork が削除されると `null` になる。** [`RefSpec::repo`] も `Option` なので
    /// そのまま運ぶ。空文字を入れると「消えた fork」と「owner が空」を
    /// 区別できなくなり、05 の PR チェックアウトが**同一リポジトリかどうかを
    /// owner の比較で決めている**ので、消えた fork を fork 経路へ落とす。
    pub repo: Option<WireRepoRef>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WireRepoRef {
    pub name: String,
    pub owner: WireUser,
}

impl From<WirePullRef> for RefSpec {
    fn from(w: WirePullRef) -> Self {
        RefSpec {
            repo: w.repo.map(|r| RepoRef {
                owner: r.owner.login,
                repo: r.name,
            }),
            branch: w.ref_name,
            sha: w.sha,
        }
    }
}

impl WirePull {
    /// PR の状態。**`state` だけでは merge と close を区別できない**ので
    /// `merged_at` を先に見る（GitHub は merge した PR も `state: "closed"` で返す）。
    pub(super) fn status(&self) -> PrStatus {
        if self.merged_at.is_some() {
            PrStatus::Merged
        } else if self.state == "open" {
            PrStatus::Open
        } else {
            PrStatus::Closed
        }
    }
}

impl From<WirePull> for PullRequest {
    fn from(w: WirePull) -> Self {
        let state = w.status();
        PullRequest {
            number: w.number,
            title: w.title,
            state,
            draft: w.draft,
            head: w.head.into(),
            base: w.base.into(),
            url: w.html_url,
            // Ghost user（アカウント削除）では `user` が `null` になる。
            author: w.user.map(|u| u.login).unwrap_or_default(),
            requested_reviewers: w.requested_reviewers.into_iter().map(|u| u.login).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct WireCheckRuns {
    pub check_runs: Vec<WireCheckRun>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WireCheckRun {
    pub name: String,
    /// `"queued"` / `"in_progress"` / `"completed"`。
    pub status: String,
    /// `status` が `"completed"` のときだけ入る。
    pub conclusion: Option<String>,
    pub html_url: Option<String>,
}

impl From<WireCheckRun> for Check {
    fn from(w: WireCheckRun) -> Self {
        Check {
            name: w.name,
            state: check_run_state(&w.status, w.conclusion.as_deref()),
            url: w.html_url,
        }
    }
}

/// check-run の状態。**`status` が `completed` でないあいだ `conclusion` は
/// `null`** なので、先に `status` を見ないと「結論なし」を失敗と誤読する。
fn check_run_state(status: &str, conclusion: Option<&str>) -> CheckState {
    if status != "completed" {
        return CheckState::Pending;
    }
    match conclusion {
        Some("success") => CheckState::Success,
        Some("skipped") => CheckState::Skipped,
        Some("neutral") => CheckState::Neutral,
        // `cancelled` / `timed_out` / `action_required` / `startup_failure` と、
        // GitHub が今後足す結論はすべて失敗に寄せる。**未知を成功にしない。**
        Some(_) => CheckState::Failure,
        // `completed` なのに結論が無い形は API では起きないが、起きたら保留にする
        // （失敗として赤くするより、次の取得で埋まるほうに賭ける）。
        None => CheckState::Pending,
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct WireCombinedStatus {
    pub statuses: Vec<WireStatus>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WireStatus {
    pub context: String,
    /// `"success"` / `"failure"` / `"error"` / `"pending"`。
    pub state: String,
    pub target_url: Option<String>,
}

impl From<WireStatus> for Check {
    fn from(w: WireStatus) -> Self {
        Check {
            name: w.context,
            state: match w.state.as_str() {
                "success" => CheckState::Success,
                "pending" => CheckState::Pending,
                // `failure` / `error` と未知の値。check-run と同じ理由で
                // 未知は成功にしない。
                _ => CheckState::Failure,
            },
            url: w.target_url,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct WireReview {
    pub user: Option<WireUser>,
    /// `"APPROVED"` / `"CHANGES_REQUESTED"` / `"COMMENTED"` / `"DISMISSED"` /
    /// `"PENDING"`。
    pub state: String,
}

/// レビューをユーザーごとに集約する。
///
/// **GitHub の数え方に合わせる。** `COMMENTED` は approve / changes-requested を
/// 上書きしない（コメントを足しても承認は残る）。`DISMISSED` は取り消しなので
/// そのユーザーの状態を落とす。`PENDING`（まだ submit していない自分の下書き）は
/// 数えない。
///
/// 配列は古い順に来るので、後に来たものが勝つ。
pub(super) fn summarize_reviews(reviews: Vec<WireReview>) -> ReviewSummary {
    /// 1 人のユーザーの現在の状態。数え上げる 3 つだけを持つ。
    enum Verdict {
        Approved,
        ChangesRequested,
        Commented,
    }

    let mut latest: HashMap<String, Verdict> = HashMap::new();
    for review in reviews {
        // Ghost user（アカウント削除）のレビューは誰のものか判らないので数えない。
        let Some(user) = review.user else { continue };
        match review.state.as_str() {
            "APPROVED" => {
                latest.insert(user.login, Verdict::Approved);
            }
            "CHANGES_REQUESTED" => {
                latest.insert(user.login, Verdict::ChangesRequested);
            }
            "DISMISSED" => {
                latest.remove(&user.login);
            }
            // `or_insert` が「既にあるなら上書きしない」そのもの。
            "COMMENTED" => {
                latest.entry(user.login).or_insert(Verdict::Commented);
            }
            // `PENDING`（未 submit の下書き）と未知の状態。
            _ => {}
        }
    }

    let mut summary = ReviewSummary::default();
    for verdict in latest.into_values() {
        match verdict {
            Verdict::Approved => summary.approved += 1,
            Verdict::ChangesRequested => summary.changes_requested += 1,
            Verdict::Commented => summary.commented += 1,
        }
    }
    summary
}
