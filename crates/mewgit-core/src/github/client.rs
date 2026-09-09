//! GitHub REST API クライアント。設計は `docs/design/05-github-api.md`。

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use reqwest::{Response, StatusCode, Url};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::auth::split_scopes;
use crate::auth::url::valid_host;

use super::wire::{
    summarize_reviews, WireCheckRuns, WireCombinedStatus, WirePull, WireRepo, WireReview,
};
use super::{Check, GithubError, PrStatus, PullRequest, RepoMeta, ReviewSummary};

/// `X-GitHub-Api-Version`。GitHub は「日付で固定せよ」と言っている。
const API_VERSION: &str = "2022-11-28";

/// 一覧系の 1 ページあたり件数（API の上限）。
const PER_PAGE: &str = "100";

/// エラー本文として残す長さ。**文字数で切る** —— バイトで切ると多バイト文字の
/// 途中で落ちて panic する。
const BODY_HEAD_CHARS: usize = 200;

/// 1 リクエストの上限。**reqwest の既定は無制限**なので、GHES が固まると
/// OS の TCP タイムアウトまで（分単位）回り続ける。#56 のスピナーは出るが、
/// `Ctrl-C` が止められるのは `Cmd::Exec`（git）だけで HTTP は止まらない。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 接続確立の上限。TLS ハンドシェイクが返らない相手を長く待たない。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 辿るページ数の上限。`Link` が輪を作っても止まるようにする。
/// 1 ページ 100 件なので 1000 件。
const MAX_PAGES: usize = 10;

/// API のベース URL を組む。
///
/// **形が 2 つある。**
///
/// - `github.com` とデータレジデンシー（`*.ghe.com`）は `https://api.{host}`
/// - self-hosted の GHES は `https://{host}/api/v3`
///   （`gh` のバイナリに `https://%s/api/v3/` があるのを `strings` で確認。05）
///
/// **port はパスの形と直交する。** `{host}/api/v3` 側なら
/// `https://ghe.example.com:8443/api/v3/`、`api.{host}` 側なら
/// `https://api.foo.ghe.com:8443/`。後者は確かめていない（github.com も `*.ghe.com` も
/// SaaS で 443 固定なので実物を用意できない）。**既定の 443 は
/// [`auth::url::RemoteUrl`](crate::auth::url::RemoteUrl) が `None` に畳んでいる**ので、
/// `:443` がここへ来ることはない。05 が「パスの形を落として port だけ足すと
/// self-hosted では全部 404 になる」と警告していたのは、**パスの形を先に入れたことで
/// 解けている**。
///
/// **分類に port を混ぜない。** `.ghe.com` で終わるかを見るのは port を外した
/// `host` のほうで、これは下の「分類より先にホスト名だけか」と同じ順序の話。
///
/// ホストの大小は**比較のときだけ**無視する。組んだ URL の小文字化は
/// [`Url`] が行う（WHATWG URL の host 正規化）ので、ここでは触らない
/// —— 二重に正規化すると出どころが 2 本になる。
fn base_url(host: &str, port: Option<u16>) -> Result<Url, GithubError> {
    let invalid = || GithubError::InvalidHost {
        host: host.to_string(),
    };
    let lower = host.to_ascii_lowercase();
    // **分類より先に「ホスト名だけか」を確かめる。** 順序が逆だと
    // `foo/bar.ghe.com` が `.ghe.com` で終わって真になり、
    // `https://api.foo/bar.ghe.com/` という**動くが違う場所を指す**ベースになる。
    //
    // 述語は `auth` のものを呼ぶ。**別に書くと、`-` 始まりを弾く理由
    // （`ssh -G` の positional に裸の `-oProxyCommand=id` が載る。04）が
    // 片方にしか残らない** —— 実際、自前で `Url::parse` して突き合わせる形は
    // `-4` も `[::1]` も `.` も通していた。
    if !valid_host(&lower) {
        return Err(invalid());
    }
    // 分類は port を外したホストで済ませてある。ここから先は authority を組むだけ。
    let authority = match port {
        Some(port) => format!("{lower}:{port}"),
        None => lower.clone(),
    };
    let raw = if lower == "github.com" || lower.ends_with(".ghe.com") {
        format!("https://api.{authority}/")
    } else {
        format!("https://{authority}/api/v3/")
    };
    Url::parse(&raw).map_err(|_| invalid())
}

/// GitHub REST API クライアント。
///
/// トークンは [`SecretString`] のまま持つので `Debug` には出ない（不変ルール 3）。
/// `auth::Account` と同じ形。見張りは `debug_does_not_leak_the_token`。
#[derive(Debug)]
pub struct GithubClient {
    http: reqwest::Client,
    base: Url,
    token: SecretString,
}

/// プロセスで 1 つの [`reqwest::Client`]。
///
/// **クライアントごとに組まない。** トークンは `bearer_auth` でリクエスト単位に
/// 載るので、`reqwest::Client` はトークンにもベース URL にも依存しない。組むたびに
/// Linux では `rustls-platform-verifier` が `/etc/ssl/certs` を読み直し
/// （`rustls-platform-verifier-0.7.0/src/verification/others.rs` の
/// `load_native_certs`）、コネクションプールも分かれる。`auth::resolve` は
/// トークン候補ごとにクライアントを作るので、そこで人数分払うことになる。
///
/// 失敗（TLS バックエンドを初期化できない）も憶える。**プロセス全体の条件**なので
/// 呼び直しても変わらない。
fn shared_http() -> Result<reqwest::Client, GithubError> {
    static HTTP: OnceLock<Result<reqwest::Client, GithubError>> = OnceLock::new();
    HTTP.get_or_init(|| {
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static(API_VERSION),
        );
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(concat!("mewgit/", env!("CARGO_PKG_VERSION"))),
        );
        reqwest::Client::builder()
            .default_headers(headers)
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(network)
    })
    .clone()
}

impl GithubClient {
    /// ホスト名からクライアントを作る。
    ///
    /// `port` は **https の非既定ポート**（`auth::RepoTarget::port`）。ssh の port は
    /// ここへ来ない（04 の「port は scheme に畳む」）。
    pub fn new(host: &str, port: Option<u16>, token: SecretString) -> Result<Self, GithubError> {
        Self::with_base_url(base_url(host, port)?, token)
    }

    /// ベース URL を直に与える。テストがモックサーバを指すための seam。
    fn with_base_url(base: Url, token: SecretString) -> Result<Self, GithubError> {
        Ok(GithubClient {
            http: shared_http()?,
            base,
            token,
        })
    }

    /// リポジトリのメタ情報。`auth::resolve` の probe 兼用。
    pub async fn repo(&self, owner: &str, repo: &str) -> Result<RepoMeta, GithubError> {
        let url = self.endpoint(&["repos", owner, repo]);
        let wire: WireRepo = self.get(url, &[]).await?;
        Ok(wire.into())
    }

    /// ブランチに紐づく PR を 1 件返す。無ければ `None`。
    ///
    /// `head` は API の求める `{owner}:{branch}` の形で、**fork なら fork 側の
    /// owner**。`:` を挟むのをここ 1 箇所にするため、呼ぶ側は 2 つに分けて渡す。
    ///
    /// `state=all` で引くので閉じた PR も返る。**開いているものを優先**し、
    /// 無ければ API が返した最初のものを採る。
    pub async fn pr_for_branch(
        &self,
        owner: &str,
        repo: &str,
        head_owner: &str,
        branch: &str,
    ) -> Result<Option<PullRequest>, GithubError> {
        let url = self.endpoint(&["repos", owner, repo, "pulls"]);
        let head = format!("{head_owner}:{branch}");
        let wire: Vec<WirePull> = self
            .get_paged(
                url,
                &[
                    ("head", head.as_str()),
                    ("state", "all"),
                    ("sort", "updated"),
                    ("direction", "desc"),
                    ("per_page", PER_PAGE),
                ],
                |page: Vec<WirePull>| page,
            )
            .await?;
        // **選んでから変換する。** 全件を値型にすると、採らない PR の `String` まで
        // 確保したうえで採った 1 件を clone することになる。
        let at = wire
            .iter()
            .position(|w| w.status() == PrStatus::Open)
            .unwrap_or(0);
        Ok(wire.into_iter().nth(at).map(Into::into))
    }

    /// コミットのチェック一覧。
    ///
    /// **2 本の API をマージする。** GitHub Actions などは check-run、外部 CI が
    /// 古い commit status API を使うと statuses に出る。片方だけ見ると、
    /// もう片方しか使っていないリポジトリでチェックが 0 件になる。
    pub async fn checks(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
    ) -> Result<Vec<Check>, GithubError> {
        let runs_url = self.endpoint(&["repos", owner, repo, "commits", sha, "check-runs"]);
        let status_url = self.endpoint(&["repos", owner, repo, "commits", sha, "status"]);
        // **2 本は独立なので並べる。** 順に待つと RTT がそのまま倍になる。
        // 同時 2 本は 05 の上限（4）の内側なので、`Semaphore` はまだ要らない。
        let (runs, statuses) = tokio::try_join!(
            self.get_paged(runs_url, &[("per_page", PER_PAGE)], |p: WireCheckRuns| p
                .check_runs),
            self.get_paged(
                status_url,
                &[("per_page", PER_PAGE)],
                |p: WireCombinedStatus| p.statuses
            ),
        )?;
        Ok(runs
            .into_iter()
            .map(Check::from)
            .chain(statuses.into_iter().map(Check::from))
            .collect())
    }

    /// PR のレビュー集計。数え方は `wire::summarize_reviews`。
    pub async fn reviews(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<ReviewSummary, GithubError> {
        let url = self.endpoint(&[
            "repos",
            owner,
            repo,
            "pulls",
            &number.to_string(),
            "reviews",
        ]);
        let wire: Vec<WireReview> = self
            .get_paged(url, &[("per_page", PER_PAGE)], |page: Vec<WireReview>| page)
            .await?;
        Ok(summarize_reviews(wire))
    }

    /// ベース URL にパスを足す。
    ///
    /// **`Url::join` は使わない。** 相対パスの解決になるので、ベースの末尾や
    /// セグメントに `.` `..` が来たときに登る。`path_segments_mut` なら
    /// セグメントとして percent-encode される。
    fn endpoint(&self, segments: &[&str]) -> Url {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("base は https なので path segments を持つ")
            .pop_if_empty()
            .extend(segments);
        url
    }

    async fn get<T: DeserializeOwned>(
        &self,
        url: Url,
        query: &[(&str, &str)],
    ) -> Result<T, GithubError> {
        let (body, _) = self.get_page(url, query).await?;
        serde_json::from_str(&body).map_err(decode)
    }

    /// `Link` の `rel="next"` を辿って全ページを集める。
    ///
    /// **1 ページ目で止めない。** GitHub の一覧は既定 30 件、`per_page` を効かせても
    /// 100 件で切れる。切り捨てが**答えを変える**ものがある —— `reviews` の配列は
    /// 古い順なので、1 ページ目だけ見ると「変更要求のあと承認した人」を
    /// `changes_requested` と数える。check-runs もマトリクスビルドが 100 を超えると、
    /// 2 ページ目の失敗が見えないまま緑に見える。
    ///
    /// `take` は 1 ページぶんの応答から項目を取り出す。裸の配列を返す API と、
    /// `{ check_runs: [...] }` のように包む API があるため。
    async fn get_paged<Page, Item>(
        &self,
        url: Url,
        query: &[(&str, &str)],
        take: impl Fn(Page) -> Vec<Item>,
    ) -> Result<Vec<Item>, GithubError>
    where
        Page: DeserializeOwned,
    {
        let mut items = Vec::new();
        let mut next = Some(url);
        // クエリを載せるのは 1 ページ目だけ。`Link` の URL が全部持っている。
        let mut query = query;
        for _ in 0..MAX_PAGES {
            let Some(url) = next.take() else { break };
            let (body, link) = self.get_page(url, query).await?;
            query = &[];
            let page: Page = serde_json::from_str(&body).map_err(decode)?;
            items.extend(take(page));
            next = link.as_deref().and_then(next_page);
        }
        Ok(items)
    }

    /// 1 リクエスト送って、本文と `Link` ヘッダを返す。
    async fn get_page(
        &self,
        url: Url,
        query: &[(&str, &str)],
    ) -> Result<(String, Option<String>), GithubError> {
        let response = self
            .http
            .get(url)
            .bearer_auth(self.token.expose_secret())
            .query(query)
            .send()
            .await
            .map_err(network)?;
        let response = check_status(response).await?;
        let link = header_str(&response, "link").map(str::to_string);
        let body = response.text().await.map_err(network)?;
        Ok((body, link))
    }
}

/// `Link` ヘッダから `rel="next"` の URL を取る。
///
/// 形は `<https://…?page=2>; rel="next", <https://…?page=9>; rel="last"`。
fn next_page(link: &str) -> Option<Url> {
    link.split(',').find_map(|part| {
        let (target, params) = part.split_once(';')?;
        if !params.contains("rel=\"next\"") {
            return None;
        }
        let target = target.trim().strip_prefix('<')?.strip_suffix('>')?;
        Url::parse(target).ok()
    })
}

fn decode(err: serde_json::Error) -> GithubError {
    GithubError::Decode {
        message: err.to_string(),
    }
}

fn network(err: reqwest::Error) -> GithubError {
    GithubError::Network {
        // **URL を含めない。** `reqwest::Error` の `Display` は URL を出すことがあり、
        // ベース URL にトークンは載らないが、通知に出す文字列を短く保つ。
        message: err.without_url().to_string(),
    }
}

/// ステータスコードを [`GithubError`] に写す。
///
/// **レート制限を先に見る。** GitHub は使い切った要求に **403** を返す
/// （429 のこともある）ので、`Forbidden` として扱うと「スコープが足りない」の
/// 案内が出て、待てば直るものを直らないものとして見せる。
async fn check_status(response: Response) -> Result<Response, GithubError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    if is_rate_limited(&response) {
        return Err(GithubError::RateLimited {
            reset_at: rate_limit_reset(&response),
        });
    }
    // **本文を読む前にヘッダを取る。** `text()` が `response` を消費する。
    let scopes = needed_scopes(&response);
    // 401 / 404 でも本文は読み切る。読まずに落とすと hyper がその接続を
    // プールへ戻せない（GitHub のエラー本文は数十バイト）。
    let body = response.text().await.unwrap_or_default();
    let body_head: String = body.chars().take(BODY_HEAD_CHARS).collect();
    Err(match status {
        StatusCode::UNAUTHORIZED => GithubError::Unauthorized,
        StatusCode::FORBIDDEN => GithubError::Forbidden {
            needed_scopes: scopes,
            saml: body.contains("SAML"),
            body_head,
        },
        StatusCode::NOT_FOUND => GithubError::NotFound,
        _ => GithubError::Api {
            status: status.as_u16(),
            body_head,
        },
    })
}

/// レート制限に当たった応答か。
///
/// 429 はそのまま。403 は 2 通りある。
///
/// - **一次制限**（時間あたりの総量）は `x-ratelimit-remaining` が 0 になる。
/// - **二次制限**（短時間に叩きすぎ）は**残量を減らさずに** `retry-after` を付ける。
///   これを見落とすと `Forbidden` に落ち、05 が「出してはいけない」と書いている
///   「スコープが足りない」の案内が、**待てば直るものに対して**出る。
///
/// どちらの印も無い 403 はスコープ不足なので `Forbidden` のまま。
fn is_rate_limited(response: &Response) -> bool {
    if response.status() == StatusCode::TOO_MANY_REQUESTS {
        return true;
    }
    response.status() == StatusCode::FORBIDDEN
        && (header_i64(response, "x-ratelimit-remaining") == Some(0)
            || header_str(response, "retry-after").is_some())
}

/// いつ回復するか（Unix epoch 秒）。判らなければ `None`。
///
/// **`retry-after` を優先する。** あれが付いているのは二次制限で、
/// `x-ratelimit-reset`（一次制限の窓。1 時間先のこともある）は無関係か、
/// そもそも付いていない。**0 で埋めない** —— 1970 年になり、表示側は
/// 「もう明けている」と読んで即座に叩き直し、制限を踏み直す。
fn rate_limit_reset(response: &Response) -> Option<i64> {
    if let Some(after) = header_i64(response, "retry-after") {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default();
        return Some(now + after);
    }
    header_i64(response, "x-ratelimit-reset")
}

/// 403 に添える「足りないスコープ」。GitHub は要求されるスコープを
/// `x-accepted-oauth-scopes` で返す。
fn needed_scopes(response: &Response) -> Vec<String> {
    // 割り方は `auth` と共有する（`gh auth status` の `scopes` が 2 人目の利用者）。
    header_str(response, "x-accepted-oauth-scopes")
        .map(split_scopes)
        .unwrap_or_default()
}

fn header_str<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response.headers().get(name)?.to_str().ok()
}

fn header_i64(response: &Response, name: &str) -> Option<i64> {
    header_str(response, name)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::{CheckState, RepoRef};
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> GithubClient {
        let base = Url::parse(&format!("{}/", server.uri())).expect("mock server の URI");
        GithubClient::with_base_url(base, SecretString::from("t0ken")).expect("クライアント")
    }

    /// `/repos/o/r` にこの応答を返させ、`repo` が返すエラーを取る。
    ///
    /// エラーの分類はステータスとヘッダだけで決まるので、どの API から呼んでも
    /// 同じ。**一番引数の少ない `repo` で代表させる。**
    async fn repo_err(response: ResponseTemplate) -> GithubError {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r"))
            .respond_with(response)
            .mount(&server)
            .await;
        client(&server).repo("o", "r").await.unwrap_err()
    }

    // ---- ベース URL ----

    /// port の無いふつうの形。port を見るテストだけが [`base_url`] を直に呼ぶ。
    fn base(host: &str) -> Result<Url, GithubError> {
        base_url(host, None)
    }

    #[test]
    fn github_com_and_data_residency_use_the_api_subdomain() {
        assert_eq!(
            base("github.com").unwrap().as_str(),
            "https://api.github.com/"
        );
        assert_eq!(
            base("acme.ghe.com").unwrap().as_str(),
            "https://api.acme.ghe.com/"
        );
    }

    #[test]
    fn self_hosted_uses_the_api_v3_path() {
        assert_eq!(
            base("ghe.example.com").unwrap().as_str(),
            "https://ghe.example.com/api/v3/"
        );
    }

    #[test]
    fn host_matching_ignores_case_and_the_url_lowercases_it() {
        // 比較は case-insensitive。組んだ URL の小文字化は `Url` に任せる。
        assert_eq!(
            base("GitHub.com").unwrap().as_str(),
            "https://api.github.com/"
        );
    }

    #[test]
    fn lookalike_hosts_are_treated_as_self_hosted() {
        // `github.com` を含むだけの別ホストを `api.` 側に落とさない。
        assert_eq!(
            base("github.com.evil.example").unwrap().as_str(),
            "https://github.com.evil.example/api/v3/"
        );
        // `.ghe.com` で終わらない `ghe.com` 単体も self-hosted 扱い。
        assert_eq!(base("ghe.com").unwrap().as_str(), "https://ghe.com/api/v3/");
    }

    #[test]
    fn a_self_hosted_host_named_api_keeps_its_own_name() {
        // ホスト名が `api.` で始まるだけの self-hosted を、`api.` を剥がして
        // 突き合わせない。剥がすと自分自身と一致せず、拒んでしまう。
        assert_eq!(
            base("api.internal.example").unwrap().as_str(),
            "https://api.internal.example/api/v3/"
        );
    }

    #[test]
    fn a_path_that_ends_with_the_hosted_suffix_is_not_hosted() {
        // 分類を先にすると `foo/bar.ghe.com` が `.ghe.com` で終わって真になり、
        // `https://api.foo/bar.ghe.com/` という「動くが違う場所」を指すベースになる。
        assert!(matches!(
            base("foo/bar.ghe.com"),
            Err(GithubError::InvalidHost { .. })
        ));
    }

    #[test]
    fn a_non_default_port_rides_on_both_shapes() {
        // self-hosted は `{host}:{port}/api/v3`。非既定ポートの GHES がこれ。
        assert_eq!(
            base_url("ghe.example.com", Some(8443)).unwrap().as_str(),
            "https://ghe.example.com:8443/api/v3/"
        );
        // `api.{host}` 側は**実物を用意できないので確かめていない**（github.com も
        // `*.ghe.com` も SaaS で 443 固定）。書かれていれば素直に載せる、という
        // こちらの判断を固定しておく（05）。
        assert_eq!(
            base_url("acme.ghe.com", Some(8443)).unwrap().as_str(),
            "https://api.acme.ghe.com:8443/"
        );
    }

    #[test]
    fn the_port_does_not_join_the_classification() {
        // `.ghe.com` で終わるかを見るのは port を外したホスト。混ぜると
        // `ghe.example.com:443` のような値が分類を変えうる。
        assert_eq!(
            base_url("github.com", Some(8443)).unwrap().as_str(),
            "https://api.github.com:8443/"
        );
    }

    #[test]
    fn hosts_that_are_not_bare_hostnames_are_rejected() {
        // **mewgit の中の経路からはもう届かない** —— `RemoteUrl` は host を検査し、
        // `ssh -G` の `hostname` も産出側（`auth::ssh`）で `valid_host` を通る（04）。
        // それでも残すのは、`GithubClient::new` が `pub` で、ここが**回復先の無い
        // 最後の関門**だから（05 の `InvalidHost`）。
        for host in [
            "ghe.example.com:8443",
            "ghe.example.com/extra",
            "user@ghe.example.com",
            "",
            " ",
            // `-` 始まりは `auth` が「弾くのが眼目」と書いている形（04）。
            // 自前で `Url::parse` して突き合わせる検査はこれを通していた。
            "-4",
            "-oProxyCommand=id",
            "[::1]",
        ] {
            assert!(
                matches!(base(host), Err(GithubError::InvalidHost { .. })),
                "{host} は拒むべき"
            );
        }
    }

    #[test]
    fn debug_does_not_leak_the_token() {
        let c = GithubClient::new("github.com", None, SecretString::from("s3cret")).unwrap();
        let shown = format!("{c:?}");
        assert!(
            !shown.contains("s3cret"),
            "トークンが Debug に出ている: {shown}"
        );
    }

    // ---- リクエストの形 ----

    #[tokio::test]
    async fn repo_sends_the_auth_and_version_headers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r"))
            .and(header("authorization", "Bearer t0ken"))
            .and(header("accept", "application/vnd.github+json"))
            .and(header("x-github-api-version", API_VERSION))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "r",
                "owner": { "login": "o" },
                "default_branch": "main",
            })))
            .mount(&server)
            .await;

        let meta = client(&server).repo("o", "r").await.unwrap();
        assert_eq!(
            meta,
            RepoMeta {
                owner: "o".into(),
                repo: "r".into(),
                default_branch: "main".into(),
            }
        );
    }

    #[tokio::test]
    async fn pr_for_branch_builds_the_owner_colon_branch_head() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .and(query_param("head", "fork-owner:feat/x"))
            .and(query_param("state", "all"))
            .and(query_param("per_page", PER_PAGE))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([pull(1, "open", None)])))
            .mount(&server)
            .await;

        let pr = client(&server)
            .pr_for_branch("o", "r", "fork-owner", "feat/x")
            .await
            .unwrap()
            .expect("PR が 1 件");
        assert_eq!(pr.number, 1);
        assert_eq!(pr.state, PrStatus::Open);
    }

    #[tokio::test]
    async fn pr_for_branch_reports_none_when_there_is_no_pr() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;

        assert!(client(&server)
            .pr_for_branch("o", "r", "o", "x")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn pr_for_branch_prefers_the_open_one_over_a_closed_one() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                pull(9, "closed", Some("2026-01-01T00:00:00Z")),
                pull(10, "open", None),
            ])))
            .mount(&server)
            .await;

        let pr = client(&server)
            .pr_for_branch("o", "r", "o", "x")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pr.number, 10, "開いているほうを採る");
        assert_eq!(pr.state, PrStatus::Open);
    }

    #[tokio::test]
    async fn a_merged_pr_is_merged_not_closed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([pull(
                7,
                "closed",
                Some("2026-01-01T00:00:00Z")
            )])))
            .mount(&server)
            .await;

        let pr = client(&server)
            .pr_for_branch("o", "r", "o", "x")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pr.state, PrStatus::Merged);
    }

    #[tokio::test]
    async fn a_deleted_fork_leaves_the_head_owner_empty() {
        let server = MockServer::start().await;
        let mut body = pull(3, "open", None);
        body["head"]["repo"] = json!(null);
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([body])))
            .mount(&server)
            .await;

        let pr = client(&server)
            .pr_for_branch("o", "r", "o", "x")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            pr.head.repo, None,
            "消えた fork は None で表す（空文字にしない）"
        );
        assert_eq!(pr.head.branch, "feat/x", "branch と sha は残る");
        assert_eq!(pr.head.sha, "deadbeef");
        assert_eq!(
            pr.base.repo,
            Some(RepoRef {
                owner: "o".into(),
                repo: "r".into()
            }),
            "base 側は消えていない"
        );
    }

    #[tokio::test]
    async fn checks_merge_check_runs_and_commit_statuses() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits/abc/check-runs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "check_runs": [
                    { "name": "build", "status": "completed", "conclusion": "success", "html_url": "https://x/1" },
                    { "name": "slow", "status": "in_progress", "conclusion": null, "html_url": null },
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits/abc/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "statuses": [
                    { "context": "legacy-ci", "state": "failure", "target_url": "https://x/2" },
                ]
            })))
            .mount(&server)
            .await;

        let checks = client(&server).checks("o", "r", "abc").await.unwrap();
        let seen: Vec<(&str, CheckState)> =
            checks.iter().map(|c| (c.name.as_str(), c.state)).collect();
        assert_eq!(
            seen,
            vec![
                ("build", CheckState::Success),
                // `completed` でないあいだ `conclusion` は `null`。失敗にしない。
                ("slow", CheckState::Pending),
                ("legacy-ci", CheckState::Failure),
            ]
        );
    }

    #[tokio::test]
    async fn an_unknown_conclusion_is_a_failure_not_a_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits/abc/check-runs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "check_runs": [
                    { "name": "future", "status": "completed", "conclusion": "exploded", "html_url": null },
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits/abc/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "statuses": [] })))
            .mount(&server)
            .await;

        let checks = client(&server).checks("o", "r", "abc").await.unwrap();
        assert_eq!(checks[0].state, CheckState::Failure);
    }

    #[tokio::test]
    async fn reviews_count_the_latest_verdict_per_user() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls/12/reviews"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                // 同じ人が変更要求 → 承認。後のほうが勝つ。
                { "user": { "login": "a" }, "state": "CHANGES_REQUESTED" },
                { "user": { "login": "a" }, "state": "APPROVED" },
                // 承認のあとのコメントは承認を落とさない。
                { "user": { "login": "a" }, "state": "COMMENTED" },
                // dismiss された承認は数えない。
                { "user": { "login": "b" }, "state": "APPROVED" },
                { "user": { "login": "b" }, "state": "DISMISSED" },
                // コメントだけの人。
                { "user": { "login": "c" }, "state": "COMMENTED" },
                // 未 submit の下書きは数えない。
                { "user": { "login": "d" }, "state": "PENDING" },
            ])))
            .mount(&server)
            .await;

        let summary = client(&server).reviews("o", "r", 12).await.unwrap();
        assert_eq!(
            summary,
            ReviewSummary {
                approved: 1,
                changes_requested: 0,
                commented: 1,
            }
        );
    }

    // ---- エラー ----

    #[tokio::test]
    async fn a_404_becomes_not_found() {
        let body = json!({ "message": "Not Found" });
        let response = ResponseTemplate::new(404).set_body_json(body);
        assert_eq!(repo_err(response).await, GithubError::NotFound);
    }

    #[tokio::test]
    async fn a_401_becomes_unauthorized() {
        assert_eq!(
            repo_err(ResponseTemplate::new(401)).await,
            GithubError::Unauthorized
        );
    }

    #[tokio::test]
    async fn a_saml_403_carries_the_scopes_and_the_saml_flag() {
        let response = ResponseTemplate::new(403)
            .insert_header("x-accepted-oauth-scopes", "repo, read:org")
            .set_body_string("Resource protected by organization SAML enforcement.");
        let err = repo_err(response).await;
        let GithubError::Forbidden {
            needed_scopes,
            saml,
            body_head,
        } = err
        else {
            panic!("Forbidden ではない: {err:?}");
        };
        assert!(saml);
        assert_eq!(
            needed_scopes,
            vec!["repo".to_string(), "read:org".to_string()]
        );
        assert!(body_head.contains("SAML"));
    }

    #[tokio::test]
    async fn a_403_with_no_quota_left_is_rate_limited_not_forbidden() {
        // 残量ヘッダが 0 のときだけレート制限。無ければスコープ不足の 403。
        let response = ResponseTemplate::new(403)
            .insert_header("x-ratelimit-remaining", "0")
            .insert_header("x-ratelimit-reset", "1789000000");
        assert_eq!(
            repo_err(response).await,
            GithubError::RateLimited {
                reset_at: Some(1_789_000_000)
            }
        );
    }

    #[tokio::test]
    async fn a_403_with_quota_left_stays_forbidden() {
        let response = ResponseTemplate::new(403).insert_header("x-ratelimit-remaining", "4999");
        assert!(
            matches!(repo_err(response).await, GithubError::Forbidden { .. }),
            "残量があるなら待っても直らない"
        );
    }

    #[tokio::test]
    async fn a_429_is_rate_limited() {
        let response = ResponseTemplate::new(429).insert_header("x-ratelimit-reset", "42");
        assert_eq!(
            repo_err(response).await,
            GithubError::RateLimited { reset_at: Some(42) }
        );
    }

    #[tokio::test]
    async fn other_statuses_keep_the_head_of_the_body() {
        let response = ResponseTemplate::new(500).set_body_string("boom");
        assert_eq!(
            repo_err(response).await,
            GithubError::Api {
                status: 500,
                body_head: "boom".into()
            }
        );
    }

    #[tokio::test]
    async fn the_body_head_is_cut_by_characters_not_bytes() {
        // 多バイト文字だけの本文。バイトで切ると境界で panic する。
        let body = "あ".repeat(BODY_HEAD_CHARS + 50);
        let err = repo_err(ResponseTemplate::new(500).set_body_string(body)).await;
        let GithubError::Api { body_head, .. } = err else {
            panic!("Api ではない: {err:?}");
        };
        assert_eq!(body_head.chars().count(), BODY_HEAD_CHARS);
    }

    #[tokio::test]
    async fn a_secondary_rate_limit_403_is_rate_limited_even_with_quota_left() {
        // 二次制限は残量を減らさずに `retry-after` を付ける。見落とすと
        // 「スコープが足りない」の案内が、待てば直るものに対して出る。
        let response = ResponseTemplate::new(403)
            .insert_header("x-ratelimit-remaining", "4999")
            .insert_header("retry-after", "60");
        let err = repo_err(response).await;
        let GithubError::RateLimited { reset_at } = err else {
            panic!("RateLimited ではない: {err:?}");
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let reset_at = reset_at.expect("retry-after があるので判る");
        assert!(
            (now + 55..=now + 65).contains(&reset_at),
            "retry-after の秒数を今から数える: {reset_at} vs {now}"
        );
    }

    #[tokio::test]
    async fn a_rate_limit_without_any_reset_header_is_unknown_not_epoch_zero() {
        // 0 を入れると 1970 年になり、表示側は「もう明けている」と読んで
        // 即座に叩き直す。
        let err = repo_err(ResponseTemplate::new(429)).await;
        assert_eq!(err, GithubError::RateLimited { reset_at: None });
    }

    #[tokio::test]
    async fn a_body_that_does_not_match_the_type_is_a_decode_error() {
        // GHES の版差でフィールドが欠けた形。「通信に失敗」と出すと、
        // サーバ互換性の問題を回線の問題として調べさせることになる。
        let response = ResponseTemplate::new(200).set_body_json(json!({ "name": "r" }));
        let err = repo_err(response).await;
        assert!(
            matches!(err, GithubError::Decode { .. }),
            "Decode ではない: {err:?}"
        );
    }

    // ---- ページング ----

    #[test]
    fn the_next_link_is_taken_from_the_link_header() {
        let link = "<https://api.github.com/x?page=2>; rel=\"next\", \
                    <https://api.github.com/x?page=9>; rel=\"last\"";
        assert_eq!(
            next_page(link).map(|u| u.to_string()),
            Some("https://api.github.com/x?page=2".to_string())
        );
        // `next` が無ければ終わり（最終ページは `prev` / `first` しか持たない）。
        assert!(next_page("<https://api.github.com/x?page=1>; rel=\"prev\"").is_none());
    }

    #[tokio::test]
    async fn reviews_follow_the_link_header_because_page_one_changes_the_answer() {
        // 配列は古い順。1 ページ目で止めると、変更要求のあと承認した人を
        // `changes_requested` と数える。
        let server = MockServer::start().await;
        let next = format!(
            "<{}/repos/o/r/pulls/12/reviews?page=2>; rel=\"next\"",
            server.uri()
        );
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls/12/reviews"))
            .and(query_param_is_missing("page"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", next.as_str())
                    .set_body_json(
                        json!([{ "user": { "login": "a" }, "state": "CHANGES_REQUESTED" }]),
                    ),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls/12/reviews"))
            .and(query_param("page", "2"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!([{ "user": { "login": "a" }, "state": "APPROVED" }])),
            )
            .mount(&server)
            .await;

        let summary = client(&server).reviews("o", "r", 12).await.unwrap();
        assert_eq!(
            summary,
            ReviewSummary {
                approved: 1,
                changes_requested: 0,
                commented: 0,
            },
            "2 ページ目まで読めば最新は承認"
        );
    }

    #[tokio::test]
    async fn check_runs_follow_the_link_header() {
        // マトリクスビルドが 100 を超えると、2 ページ目の失敗が見えないまま
        // 緑に見える。
        let server = MockServer::start().await;
        let next = format!(
            "<{}/repos/o/r/commits/abc/check-runs?page=2>; rel=\"next\"",
            server.uri()
        );
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits/abc/check-runs"))
            .and(query_param_is_missing("page"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", next.as_str())
                    .set_body_json(json!({ "check_runs": [
                        { "name": "green", "status": "completed", "conclusion": "success", "html_url": null },
                    ]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits/abc/check-runs"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "check_runs": [
                { "name": "red", "status": "completed", "conclusion": "failure", "html_url": null },
            ]})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits/abc/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "statuses": [] })))
            .mount(&server)
            .await;

        let checks = client(&server).checks("o", "r", "abc").await.unwrap();
        let seen: Vec<(&str, CheckState)> =
            checks.iter().map(|c| (c.name.as_str(), c.state)).collect();
        assert_eq!(
            seen,
            vec![("green", CheckState::Success), ("red", CheckState::Failure),]
        );
    }

    /// PR 1 件のレスポンス。
    fn pull(number: u64, state: &str, merged_at: Option<&str>) -> serde_json::Value {
        json!({
            "number": number,
            "title": "t",
            "state": state,
            "merged_at": merged_at,
            "draft": false,
            "html_url": format!("https://github.com/o/r/pull/{number}"),
            "user": { "login": "author" },
            "head": {
                "ref": "feat/x",
                "sha": "deadbeef",
                "repo": { "name": "r", "owner": { "login": "fork-owner" } },
            },
            "base": {
                "ref": "main",
                "sha": "cafe",
                "repo": { "name": "r", "owner": { "login": "o" } },
            },
        })
    }
}
