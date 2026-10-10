//! GitHub REST API クライアント。設計は `docs/design/05-github-api.md`。

use futures::stream::{self, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use reqwest::{Response, StatusCode, Url};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Semaphore;

use crate::auth::split_scopes;
use crate::auth::url::valid_host;
use crate::auth::RepoTarget;

use super::wire::{
    summarize_reviews, WireCheckRuns, WireCombinedStatus, WireCreatePr, WirePull, WireRepo,
    WireReview,
};
use super::{
    Check, CheckSummary, CreatePr, GithubError, PrDetail, PullRequest, RateLimit, RepoMeta,
    ReviewSummary,
};

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

/// 認証付きリクエストの同時本数（05）。
///
/// **プロセス全体で 1 つ。** [`GithubClient`] のフィールドにすると、`runtime` が
/// dispatch のたびにクライアントを組み直すぶんだけ上限が増える（一覧の引き直しと
/// PR ごとの扇形が並ぶと 2 倍になる）。
///
/// **permit を取るのは HTTP が実際に出るところ（[`GithubClient::send`]）だけ。**
/// GET も POST もそこを通る。上の段でも取ると
/// **デッドロックする** —— [`GithubClient::checks`] は `try_join!` で 2 本張るので、
/// 外側の permit を握ったまま内側が permit を待つ形になる（05）。
static PERMITS: Semaphore = Semaphore::const_new(4);

/// 同時に走らせる PR の本数（[`GithubClient::pr_details`]）。
///
/// **これは HTTP の上限ではない**（そちらは [`PERMITS`]）。1 件が複数のリクエストを
/// 張るので、ここを大きくしても `PERMITS` の手前に並ぶだけ。**有界にするのは、
/// 打ち切ったときに「まだ始めていない」ぶんを実際に頼まずに済ませるため** ——
/// 全件を先に起こしてしまうと、止める合図はどれにも間に合わない。
const FAN_OUT: usize = 4;

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
    // **分類は port を外したホストで済ませる。** ここで先に決めておくと、下の
    // authority を組むときに `lower` を move できる（`clone` は「あとで要る」と読める
    // ので、要らないなら置かない）。
    let api_subdomain = lower == "github.com" || lower.ends_with(".ghe.com");
    let authority = match port {
        Some(port) => format!("{lower}:{port}"),
        None => lower,
    };
    let raw = if api_subdomain {
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
    /// 直近の応答が言ったレートリミット残量（05 の「残量の受け渡し」）。
    ///
    /// **`&self` のまま書けるように `Mutex` を噛ませる。** 扇形は 1 つの
    /// クライアントから同時 4 本まで飛び（`PERMITS`）、どれが新しいかは届く順では
    /// 決まらないので、畳むのは [`RateLimit::newer`]。
    ///
    /// **持ち主はジョブ 1 本。** [`GithubClient`] は `Cmd` を捌くたびに組み直すので
    /// （`runtime::github_for`）、ジョブをまたいで新しさを比べるのは受け取る側
    /// （`app::PrState`）の仕事になる。
    rate: Mutex<Option<RateLimit>>,
}

/// 既定ヘッダとタイムアウトを載せた builder。
///
/// **プロセスで 1 つのクライアント（[`shared_http`]）と、テストが 1 件ずつ組む
/// 使い捨てのクライアント（`tests::client`）が同じ設定を読む。** 分けると、
/// テストが見ているヘッダと出荷されるヘッダが割れる
/// （`repo_sends_the_auth_and_version_headers`）。
fn http_builder() -> reqwest::ClientBuilder {
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
    HTTP.get_or_init(|| http_builder().build().map_err(network))
        .clone()
}

impl GithubClient {
    /// 決まった対象とトークンからクライアントを作る。
    ///
    /// **トークンは中身を写す。** secrecy 0.10 の `SecretString` は `SecretBox<str>` で
    /// `Clone` を持たない（`str` が `CloneableSecret` でない）ので、`expose_secret()`
    /// から作り直すしかない。
    ///
    /// **[`new`](Self::new) の引数 3 つを [`RepoTarget`] から引く形をここに寄せてある。**
    /// 呼ぶ側が 2 つある（`auth::Backend` の probe と、ランタイムの PR 取得）ので、
    /// 写し方と引数の対応を 2 箇所に書かない。
    pub fn with_token(target: &RepoTarget, token: &SecretString) -> Result<Self, GithubError> {
        Self::new(
            &target.host,
            target.port,
            SecretString::from(token.expose_secret().to_string()),
        )
    }

    /// ホスト名からクライアントを作る。
    ///
    /// `port` は **https の非既定ポート**（`auth::RepoTarget::port`）。ssh の port は
    /// ここへ来ない（04 の「port は scheme に畳む」）。
    pub fn new(host: &str, port: Option<u16>, token: SecretString) -> Result<Self, GithubError> {
        Self::with_base_url(base_url(host, port)?, token)
    }

    /// ベース URL を直に与える。テストがモックサーバを指すための seam。
    fn with_base_url(base: Url, token: SecretString) -> Result<Self, GithubError> {
        Ok(Self::with_http(shared_http()?, base, token))
    }

    /// HTTP クライアントごと与える。**テストが接続の使い回しを切るための seam。**
    fn with_http(http: reqwest::Client, base: Url, token: SecretString) -> Self {
        GithubClient {
            http,
            base,
            token,
            rate: Mutex::new(None),
        }
    }

    /// リポジトリのメタ情報。`auth::resolve` の probe 兼用。
    pub async fn repo(&self, owner: &str, repo: &str) -> Result<RepoMeta, GithubError> {
        let url = self.endpoint(&["repos", owner, repo]);
        let wire: WireRepo = self.get(url, &[]).await?;
        Ok(wire.into())
    }

    /// 開いている PR の一覧。更新の新しい順。
    ///
    /// **絞り込みは引数に取らない**（05 の「クライアント」）。誰の PR かの振り分けは
    /// 呼ぶ側が [`PullRequest::author`] と [`PullRequest::requested_reviewers`] から
    /// 行う。
    ///
    /// **`/search/issues` は使わない。** サーバ側で `author:@me` を絞れる代わりに
    /// 返るのが issue 形で `head` が無く、チェックを引く鍵（head sha）を作れない
    /// （05。実測 2026-09-17）。
    ///
    /// **閉じた PR は返らない。** `state=all` にすると辿るページが 2 桁増え、
    /// PR ごとの集計が現実的な大きさに収まらない（01 の「現ブランチの PR」）。
    pub async fn pr_list(&self, owner: &str, repo: &str) -> Result<Vec<PullRequest>, GithubError> {
        let url = self.endpoint(&["repos", owner, repo, "pulls"]);
        let wire: Vec<WirePull> = self
            .get_paged(
                url,
                &[
                    ("state", "open"),
                    ("sort", "updated"),
                    ("direction", "desc"),
                    ("per_page", PER_PAGE),
                ],
                |page: Vec<WirePull>| page,
            )
            .await?;
        Ok(wire.into_iter().map(Into::into).collect())
    }

    /// PR を作る（01 の「PR 作成（`N`）」）。
    ///
    /// **返った `PullRequest` をそのまま返す。** `pulls` と同じ形が返るので
    /// [`WirePull`] を使い回す。読む側が使うのは番号だけだが、**`()` に落とさない**
    /// —— 落とすと、作った PR が一覧に出るまで画面に何も出せない（05）。
    ///
    /// **重複した PR も、リモートに無いブランチも 422。** どちらも
    /// [`GithubError::Api`] に落ち、本文の先頭 200 文字が付く。**腕を足さない** ——
    /// GitHub は同じ 422 に十数通りの `errors[].code` を載せるので、腕にすると
    /// 載らなかった形が黙って `Api` へ落ちる（05）。
    pub async fn create_pr(
        &self,
        owner: &str,
        repo: &str,
        input: &CreatePr,
    ) -> Result<PullRequest, GithubError> {
        let url = self.endpoint(&["repos", owner, repo, "pulls"]);
        let body = WireCreatePr::from(input);
        let (body, _) = self.send(self.http.post(url).json(&body)).await?;
        let wire: WirePull = serde_json::from_str(&body).map_err(decode)?;
        Ok(wire.into())
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
        // **permit はここでは取らない。** 取ると外側で握ったまま内側の `send` が
        // 待つ形になり、扇形が上限まで並んだところで誰も返さなくなる（`PERMITS`）。
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

    /// PR 1 件ぶんの集計（チェックとレビュー）。
    ///
    /// **2 本は独立なので並べる。** permit を取るのは `send` の中だけなので、
    /// ここで `try_join!` しても入れ子で詰まらない（`PERMITS`）。
    ///
    /// **`sha` は PR の head。** チェックはコミットに紐づくので、呼ぶ側は
    /// `(番号, head sha)` を鍵に憶えて、変わっていない行を引き直さない
    /// （05 の「扇形は毎回引き直さない」）。**レビューは sha に紐づかない**ので、
    /// その鍵では古くなる —— 人が `r` を押したときだけ落とす（01 の「引き直す（`r`）」）。
    pub async fn pr_detail(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        sha: &str,
    ) -> Result<PrDetail, GithubError> {
        let (checks, review) = tokio::try_join!(
            self.checks(owner, repo, sha),
            self.reviews(owner, repo, number),
        )?;
        let mut summary = CheckSummary::default();
        for check in &checks {
            summary.add(check.state);
        }
        Ok(PrDetail {
            checks: summary,
            review,
        })
    }

    /// 一覧の各 PR の集計を引く。返るのは**打ち切った理由**（最後まで引けたら `None`）。
    ///
    /// **答えは届いた順に `on_each` へ渡す。** 60 件を引くと全部揃うまで数秒かかる
    /// ので、揃うのを待たない（01 の「PR ペイン › 集計」）。
    ///
    /// **1 件が「どの対象でも同じように起きる」失敗を返したら、そこで止める**
    /// （[`GithubError::repeats_for_every_target`]）。同じ失敗を N 回買ううえ、
    /// レート制限では二次制限を踏みに行く（05）。
    ///
    /// **止めているのは「抜けること」で、同時本数ではない。** ここを抜けると
    /// ストリームごと落ちるので、走っていない future は permit を取りに行かない。
    /// **`tokio::spawn` / `JoinSet` に全件を渡してから旗で止める形は 1 本も
    /// 止められない** —— 起こしたタスクは切り離されているので、旗を読み終えた
    /// あと permit の列で待ち、順番が来れば素直に叩く（実測。見張りは
    /// `a_fan_out_stops_asking_after_a_failure_that_repeats` で、**`return` を
    /// 外すと落ちる**）。
    ///
    /// **`FAN_OUT` で有界にするのは別の理由** —— 対象が多いときに future を
    /// 全部組み立てないためと、**permit の列を短く保つ**ため。tokio の
    /// [`Semaphore`] は FIFO（`tokio-1.53.1/src/sync/semaphore.rs` の
    /// 「permits are given out in the order they were requested」）なので、
    /// 扇形が全件ぶんの `acquire` を並べると、そのあとに押した `f` の一覧取得が
    /// 列の最後尾に付く。
    ///
    /// **`on_each` は同期。** 呼ぶ側がやるのは `Msg` を 1 本送ることだけで
    /// （`mpsc::UnboundedSender::send`）、`github` に `Msg` を知らせないためでもある。
    pub async fn pr_details(
        &self,
        owner: &str,
        repo: &str,
        targets: Vec<(u64, String)>,
        mut on_each: impl FnMut(u64, String, Result<PrDetail, GithubError>),
    ) -> Option<GithubError> {
        let mut running = stream::iter(targets)
            .map(|(number, sha)| async move {
                let result = self.pr_detail(owner, repo, number, &sha).await;
                (number, sha, result)
            })
            .buffer_unordered(FAN_OUT);
        while let Some((number, sha, result)) = running.next().await {
            let stop = match &result {
                Err(err) if err.repeats_for_every_target() => Some(err.clone()),
                _ => None,
            };
            on_each(number, sha, result);
            if stop.is_some() {
                return stop;
            }
        }
        None
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

    /// GET を 1 本送る。
    async fn get_page(
        &self,
        url: Url,
        query: &[(&str, &str)],
    ) -> Result<(String, Option<String>), GithubError> {
        self.send(self.http.get(url).query(query)).await
    }

    /// 組んだリクエストを 1 本送って、本文と `Link` ヘッダを返す。
    ///
    /// **同時本数を抑えるのはここだけ**（`PERMITS`）。**permit は応答を読み切る
    /// まで握る** —— 本文を読み終える前に返すと、次のリクエストが同時に飛ぶ。
    ///
    /// **メソッドで分けない。** GET と POST で違うのは `RequestBuilder` の組み方
    /// だけで、permit・トークン・残量の記録・状態の写しはどれも同じ（05 の
    /// 「`create_pr` だけが POST」）。**POST を別の経路で送ると**、扇形と同時に
    /// 飛んだときに上限をすり抜け、残量も採れない。
    async fn send(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<(String, Option<String>), GithubError> {
        // `Semaphore` を閉じないので `acquire` は失敗しない。
        let _permit = PERMITS
            .acquire()
            .await
            .expect("PERMITS は閉じないので acquire は成功する");
        let response = request
            .bearer_auth(self.token.expose_secret())
            .send()
            .await
            .map_err(network)?;
        // **状態を見る前に採る。** 残量が 0 の応答は 403 / 429 で返ってくるので、
        // [`check_status`] の後ろに置くと**いちばん出したい数だけが採れない**。
        self.note_rate_limit(&response);
        let response = check_status(response).await?;
        let link = header_str(&response, "link").map(str::to_string);
        let body = response.text().await.map_err(network)?;
        Ok((body, link))
    }

    /// 応答が言ったレートリミット残量を憶える。
    ///
    /// **新しいほうだけ残す**（[`RateLimit::newer`]）。同時に飛んだ 4 本は届く順が
    /// 新しさの順ではない。
    fn note_rate_limit(&self, response: &Response) {
        let Some(seen) = rate_limit_of(response) else {
            return;
        };
        let mut slot = self.rate.lock().expect("rate の Mutex は毒されない");
        RateLimit::keep_newer(&mut slot, seen);
    }

    /// このクライアントが見たうちで新しいほうの残量。1 度も見ていなければ `None`。
    ///
    /// **読むのはランタイム。** `github` は `Msg` を知らないので、`app` へ運ぶのは
    /// 呼ぶ側（05 の「残量の受け渡し」）。
    pub fn rate_limit(&self) -> Option<RateLimit> {
        *self.rate.lock().expect("rate の Mutex は毒されない")
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
/// （429 のこともある）ので、`Forbidden` / `MissingScope` として扱うと、待てば
/// 直るものを直らないものとして見せる。
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
    let missing = missing_scope(&response);
    // 401 / 404 でも本文は読み切る。読まずに落とすと hyper がその接続を
    // プールへ戻せない（GitHub のエラー本文は数十バイト）。
    let body = response.text().await.unwrap_or_default();
    let body_head: String = body.chars().take(BODY_HEAD_CHARS).collect();
    // **ステータスで分ける前に見る**（05）。足りないスコープは 403 ではなく 404 で
    // 返るので、`FORBIDDEN` の腕の中で見ると取りこぼす。
    if let Some(scope) = missing {
        return Err(GithubError::MissingScope {
            status: status.as_u16(),
            scope,
        });
    }
    Err(match status {
        StatusCode::UNAUTHORIZED => GithubError::Unauthorized,
        StatusCode::FORBIDDEN => GithubError::Forbidden {
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
///   これを見落とすと `Forbidden` / `MissingScope` に落ち、**待てば直るものを
///   直らないものとして**見せる。
///
/// どちらの印も無い 403 は `MissingScope` か `Forbidden` のまま（05）。
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

/// 応答のヘッダから判る「足りないスコープ」。判らなければ `None`。
///
/// **条件と gh との違いは 05 の「足りないスコープ」**（`api/client.go` の
/// `generateScopesSuggestion` に合わせ、受け付けるスコープを 1 つでも持っていれば
/// 案内しない点だけ変えた）。
fn missing_scope(response: &Response) -> Option<String> {
    let status = response.status();
    if !status.is_client_error() || status == StatusCode::UNPROCESSABLE_ENTITY {
        return None;
    }
    let held = header_str(response, "x-oauth-scopes").map(split_scopes)?;
    if held.is_empty() {
        return None;
    }
    let accepted = header_str(response, "x-accepted-oauth-scopes").map(split_scopes)?;
    if accepted.iter().any(|scope| holds(&held, scope)) {
        return None;
    }
    accepted.into_iter().next()
}

/// `held` のどれかが `scope` を含むか。
fn holds(held: &[String], scope: &str) -> bool {
    held.iter().any(|h| implies(h, scope))
}

/// スコープ `held` を持っていれば `scope` も持っているか。**含み方は gh と同じ**
/// （`repo` は `public_repo` などを、`admin:x` は `read:x` / `write:x` を、`write:x` は
/// `read:x` を含む）。
fn implies(held: &str, scope: &str) -> bool {
    if held == scope {
        return true;
    }
    match held {
        "repo" => {
            return matches!(
                scope,
                "repo:status"
                    | "repo_deployment"
                    | "public_repo"
                    | "repo:invite"
                    | "security_events"
            )
        }
        "user" => return matches!(scope, "read:user" | "user:email" | "user:follow"),
        "codespace" => return scope == "codespace:secrets",
        _ => {}
    }
    if let Some(rest) = held.strip_prefix("admin:") {
        return scope.strip_prefix("read:") == Some(rest)
            || scope.strip_prefix("write:") == Some(rest);
    }
    held.strip_prefix("write:")
        .is_some_and(|rest| scope.strip_prefix("read:") == Some(rest))
}

/// 応答の `x-ratelimit-*` から残量を組む。**どれか 1 つでも欠けたら `None`**。
///
/// **`x-ratelimit-resource` が `core` でなければ採らない。** 上限が 2 桁小さい
/// `search` の数をヘッダに出すと、残量が突然 30 になる（05 の実測表）。**いま mewgit が
/// 叩くエンドポイントは全部 `core`** なので、この腕を踏ませられるのは
/// wiremock のテストだけ。
///
/// **ヘッダごと無いこともある。** レートリミットを切った GHES がそれで、そのときは
/// 残量を出さない（`None`）だけで、エラーにはしない。
///
/// **読むのはここ。** 3 つとも同じ型の `Option<i64>` なので、[`RateLimit`] 側に
/// 引数で渡す形だと入れ替えても誰も気付かない（`.claude/rules/rust-coding.md` の「導出できる値を引数に
/// 取らせない」）。ヘッダの読み方は [`header_i64`] と同じ場所に置く。
fn rate_limit_of(response: &Response) -> Option<RateLimit> {
    if header_str(response, "x-ratelimit-resource").is_some_and(|bucket| bucket != "core") {
        return None;
    }
    Some(RateLimit {
        limit: u32::try_from(header_i64(response, "x-ratelimit-limit")?).ok()?,
        remaining: u32::try_from(header_i64(response, "x-ratelimit-remaining")?).ok()?,
        reset: header_i64(response, "x-ratelimit-reset")?,
    })
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
    // **テストからしか届かない名前はここに置く**（`cargo fix` がヘッダから落とす。
    // `.claude/rules/rust-coding.md` の「`cargo fix` は…」）。
    use crate::github::{CheckState, PrStatus, RepoRef};
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;
    use wiremock::matchers::{
        body_json, header, method, path, path_regex, query_param, query_param_is_missing,
    };
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    /// モックサーバを指すクライアント。
    ///
    /// **接続を使い回さない**（`pool_max_idle_per_host(0)`）。出荷される
    /// [`shared_http`] はプロセスで 1 つなので、**死んだ [`MockServer`] 宛ての
    /// idle 接続がプールに残り、OS が同じポートを次のサーバに割り当てると
    /// そこへ送って `error sending request` で落ちる**。実測（2026-09-18、macOS）
    /// —— 本文の変更を入れずにテストを 7 本足しただけで
    /// `pr_detail_counts_every_check_state_and_does_not_fold_them` が
    /// **15 回中 2 回**落ち、`pool_max_idle_per_host(0)` で 12 回中 0 回になった
    /// （main は 6 回中 0 回）。**サーバが死なない出荷側では起きない**ので、
    /// 切るのはこちらだけ。
    fn client(server: &MockServer) -> GithubClient {
        let base = Url::parse(&format!("{}/", server.uri())).expect("mock server の URI");
        let http = http_builder()
            .pool_max_idle_per_host(0)
            .build()
            .expect("テスト用のクライアント");
        GithubClient::with_http(http, base, SecretString::from("t0ken"))
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

    /// `/repos/o/r` が返す本文。**`repo` を代表に選んだテストが全部これを使う**
    /// （[`repo_err`] と同じ理由で、一番引数の少ない API が土台になっている）。
    fn repo_body() -> serde_json::Value {
        json!({
            "owner": { "login": "o" },
            "name": "r",
            "default_branch": "main",
        })
    }

    /// 応答にヘッダを載せる。
    fn with_headers(mut response: ResponseTemplate, headers: &[(&str, &str)]) -> ResponseTemplate {
        for (name, value) in headers {
            response = response.insert_header(*name, *value);
        }
        response
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
            .respond_with(ResponseTemplate::new(200).set_body_json(repo_body()))
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
    async fn pr_list_asks_for_the_open_ones_newest_first() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .and(query_param("state", "open"))
            .and(query_param("sort", "updated"))
            .and(query_param("direction", "desc"))
            .and(query_param("per_page", PER_PAGE))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!([pull(10, "open", None), pull(9, "open", None),])),
            )
            .mount(&server)
            .await;

        let prs = client(&server).pr_list("o", "r").await.unwrap();
        // **並べ替えない。** 並びは API が決める（07 の「PRs ペイン」）。
        assert_eq!(
            prs.iter().map(|pr| pr.number).collect::<Vec<_>>(),
            vec![10, 9]
        );
        assert_eq!(prs[0].state, PrStatus::Open);
    }

    #[tokio::test]
    async fn pr_list_comes_back_empty_when_there_is_no_open_pr() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;

        assert!(client(&server).pr_list("o", "r").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn create_pr_posts_every_field_and_returns_the_new_pull_request() {
        // **本文をまるごと突き合わせる。** 1 つでも欠けると GitHub は 422 を返すので、
        // `contains` では「送っているつもりの欄」を見張れない。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/repos/o/r/pulls"))
            .and(body_json(json!({
                "title": "feat: x",
                "body": "本文",
                "head": "fork-owner:feat/x",
                "base": "main",
                "draft": true,
            })))
            .respond_with(with_headers(
                ResponseTemplate::new(201).set_body_json(pull(7, "open", None)),
                &[
                    ("x-ratelimit-limit", "5000"),
                    ("x-ratelimit-remaining", "4321"),
                    ("x-ratelimit-reset", "1700000000"),
                ],
            ))
            .mount(&server)
            .await;

        let api = client(&server);
        let created = api
            .create_pr(
                "o",
                "r",
                &CreatePr {
                    title: "feat: x".to_string(),
                    body: "本文".to_string(),
                    head: "fork-owner:feat/x".to_string(),
                    base: "main".to_string(),
                    draft: true,
                },
            )
            .await
            .expect("create_pr");

        assert_eq!(created.number, 7);
        // **POST も GET と同じ経路（`send`）を通っている証拠。** 残量を採るのも
        // permit を取るのもそこ 1 箇所なので、別経路で送ると**上限をすり抜け、
        // 残量も採れない**（05）。ここが `None` なら経路が割れている。
        assert_eq!(
            api.rate_limit().map(|r| r.remaining),
            Some(4321),
            "POST が残量を採る経路を通っていません"
        );
    }

    #[tokio::test]
    async fn create_pr_hands_the_422_body_to_the_caller() {
        // 重複した PR も、リモートに無いブランチも 422（05）。**腕を足さずに
        // `Api` へ落とし、本文をそのまま残す** —— 通知の `detail` に出るのがこれで、
        // 握りつぶすと「何が悪いのか」を知る経路が無くなる（不変ルール 4 と同じ）。
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/repos/o/r/pulls"))
            .respond_with(ResponseTemplate::new(422).set_body_json(json!({
                "message": "Validation Failed",
                "errors": [{ "message": "A pull request already exists for me:feat/x." }],
            })))
            .mount(&server)
            .await;

        let err = client(&server)
            .create_pr(
                "o",
                "r",
                &CreatePr {
                    title: "t".to_string(),
                    body: String::new(),
                    head: "feat/x".to_string(),
                    base: "main".to_string(),
                    draft: false,
                },
            )
            .await
            .expect_err("422 のはず");

        let GithubError::Api { status, body_head } = err else {
            panic!("Api に落ちていません: {err:?}");
        };
        assert_eq!(status, 422);
        assert!(
            body_head.contains("A pull request already exists"),
            "{body_head}"
        );
    }

    #[tokio::test]
    async fn pr_list_carries_the_requested_reviewers() {
        // レビュー依頼の印（07 の `R`）はこれだけを見る。**チーム宛ての依頼
        // （`requested_teams`）は運ばない**ので、同じ本文に入れても出てこない。
        let server = MockServer::start().await;
        let mut body = pull(4, "open", None);
        body["requested_reviewers"] = json!([{ "login": "hanako" }, { "login": "taro" }]);
        body["requested_teams"] = json!([{ "slug": "reviewers" }]);
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([body])))
            .mount(&server)
            .await;

        let prs = client(&server).pr_list("o", "r").await.unwrap();
        assert_eq!(prs[0].requested_reviewers, vec!["hanako", "taro"]);
    }

    #[tokio::test]
    async fn a_merged_pr_is_merged_not_closed() {
        // `state` だけでは merge と close を区別できない（`WirePull::status`）。
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                pull(7, "closed", Some("2026-01-01T00:00:00Z")),
                pull(8, "closed", None),
            ])))
            .mount(&server)
            .await;

        let prs = client(&server).pr_list("o", "r").await.unwrap();
        assert_eq!(prs[0].state, PrStatus::Merged);
        assert_eq!(prs[1].state, PrStatus::Closed);
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

        let prs = client(&server).pr_list("o", "r").await.unwrap();
        let pr = &prs[0];
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
    async fn the_status_alone_decides_these_errors() {
        // **ヘッダを読まずにステータスだけで決まる腕をまとめて回す。** 1 本ずつ
        // 書くと、同じ `repo_err` に 1 回渡して `assert_eq!` するだけの関数が並び、
        // ステータスを 1 つ足すのに関数を 1 つ書くことになる。
        //
        // **速さのためではない。** `repo_err` は呼ばれるたびに `MockServer` を
        // 立てるので本数は畳んでも 4 つのままで、むしろハーネスの並列実行が
        // 直列になる（実測: 4 本 0.10 秒 / 1 本 0.10 秒。差は出ない）。
        //
        // ヘッダで分かれる 403 と、reset を持たない 429 は下の別のテストが持つ
        // （あちらは分類の根拠がステータスではないので混ぜない）。
        let cases = [
            (
                404,
                ResponseTemplate::new(404).set_body_json(json!({ "message": "Not Found" })),
                GithubError::NotFound,
            ),
            (401, ResponseTemplate::new(401), GithubError::Unauthorized),
            (
                429,
                ResponseTemplate::new(429).insert_header("x-ratelimit-reset", "42"),
                GithubError::RateLimited { reset_at: Some(42) },
            ),
            (
                500,
                ResponseTemplate::new(500).set_body_string("boom"),
                // 案内に出せる手掛かりが本文しか無いので、頭を残す。
                GithubError::Api {
                    status: 500,
                    body_head: "boom".into(),
                },
            ),
        ];

        for (status, response, want) in cases {
            assert_eq!(repo_err(response).await, want, "HTTP {status}");
        }
    }

    #[tokio::test]
    async fn a_saml_403_is_forbidden_with_the_saml_flag_even_with_scope_headers() {
        // **スコープの話ではない。** トークンは受け付けるスコープを持っているので、
        // ヘッダが付いていても `MissingScope` にしない（持っていれば理由は別にある）。
        let response = with_headers(
            ResponseTemplate::new(403)
                .set_body_string("Resource protected by organization SAML enforcement."),
            &[
                ("x-accepted-oauth-scopes", "repo"),
                ("x-oauth-scopes", "gist, repo"),
            ],
        );
        let err = repo_err(response).await;
        assert_eq!(err.to_string(), "GitHub API: 403 SAML SSO の承認が要ります");
        let GithubError::Forbidden { saml, body_head } = err else {
            panic!("Forbidden ではない: {err:?}");
        };
        assert!(saml);
        assert!(body_head.contains("SAML"));
    }

    #[tokio::test]
    async fn a_404_names_the_missing_scope_only_when_the_token_says_what_it_holds() {
        // **足りないスコープは 404 で返る**（05 の実測。`user/emails`）。
        let accepted = ("x-accepted-oauth-scopes", "user, user:email");
        let with_held = with_headers(
            ResponseTemplate::new(404),
            &[accepted, ("x-oauth-scopes", "gist, read:org, repo")],
        );
        assert_eq!(
            repo_err(with_held).await,
            GithubError::MissingScope {
                status: 404,
                scope: "user".to_string(),
            }
        );
        // **持っているスコープが判らなければ案内しない**（gh と同じ）。同じ 404 が
        // ただの `NotFound` に戻ることで、上の入力が効いていることを見る。
        let without_held = with_headers(ResponseTemplate::new(404), &[accepted]);
        assert_eq!(repo_err(without_held).await, GithubError::NotFound);
        // **ヘッダはあるが値が空**の形も同じ。GitHub は空のスコープヘッダを値の無い
        // まま送ってくる（05 の実測で `X-Accepted-Oauth-Scopes` がそうだった）。
        let empty_held = with_headers(
            ResponseTemplate::new(404),
            &[accepted, ("x-oauth-scopes", "")],
        );
        assert_eq!(repo_err(empty_held).await, GithubError::NotFound);
    }

    #[tokio::test]
    async fn holding_any_accepted_scope_through_implication_is_not_a_missing_scope() {
        // **gh と違えた 1 点。** `write:gpg_key` は `read:gpg_key` を含み、受け付ける
        // スコープはどれか 1 つで足りる。gh はここでも `admin:gpg_key` を足せと言う。
        let accepted = ("x-accepted-oauth-scopes", "admin:gpg_key, read:gpg_key");
        let implied = with_headers(
            ResponseTemplate::new(404),
            &[accepted, ("x-oauth-scopes", "repo, write:gpg_key")],
        );
        assert_eq!(repo_err(implied).await, GithubError::NotFound);
        // 含まないトークンなら先頭を案内する。
        let lacking = with_headers(
            ResponseTemplate::new(404),
            &[accepted, ("x-oauth-scopes", "repo")],
        );
        assert_eq!(
            repo_err(lacking).await,
            GithubError::MissingScope {
                status: 404,
                scope: "admin:gpg_key".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn scope_headers_do_not_override_422_or_the_rate_limit() {
        let scopes = [
            ("x-accepted-oauth-scopes", "repo"),
            ("x-oauth-scopes", "gist"),
        ];
        // 422 は本文の検証エラー（PR 作成の「既にある」など）で、スコープの話ではない。
        let unprocessable = with_headers(
            ResponseTemplate::new(422).set_body_string("invalid"),
            &scopes,
        );
        assert!(
            matches!(
                repo_err(unprocessable).await,
                GithubError::Api { status: 422, .. }
            ),
            "422 がスコープの案内に化けた"
        );
        // **レート制限を先に見る。** 待てば直るものを直らないものとして見せない。
        let limited = with_headers(
            with_headers(ResponseTemplate::new(403), &scopes),
            &[("x-ratelimit-remaining", "0")],
        );
        assert!(matches!(
            repo_err(limited).await,
            GithubError::RateLimited { .. }
        ));
    }

    #[test]
    fn a_top_level_scope_holds_the_ones_it_groups() {
        let held = |raw: &str| split_scopes(raw);
        assert!(holds(&held("repo"), "public_repo"));
        assert!(holds(&held("repo"), "repo:status"));
        assert!(holds(&held("user"), "user:email"));
        assert!(holds(&held("codespace"), "codespace:secrets"));
        assert!(holds(&held("admin:org"), "read:org"));
        assert!(holds(&held("admin:org"), "write:org"));
        assert!(holds(&held("write:org"), "read:org"));
        // 逆向きには含まない。
        assert!(!holds(&held("read:org"), "write:org"));
        assert!(!holds(&held("public_repo"), "repo"));
        // 接頭辞の後ろが違えば含まない。
        assert!(!holds(&held("admin:org"), "read:gpg_key"));
    }

    #[tokio::test]
    async fn a_403_is_rate_limited_only_when_no_quota_is_left() {
        // 残量ヘッダが 0 のときだけレート制限。無いか残っていれば `Forbidden` の 403。
        let response = ResponseTemplate::new(403).insert_header("x-ratelimit-remaining", "4999");
        assert!(
            matches!(repo_err(response).await, GithubError::Forbidden { .. }),
            "残量があるなら待っても直らない"
        );
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
        // `Forbidden` に落ち、待てば直るものを直らないものとして見せる。
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

    // ---- レートリミット残量 ----

    /// `x-ratelimit-*` の 3 つ。**欠けた形を踏むテストだけが手で並べる。**
    fn quota_headers(
        limit: &'static str,
        remaining: &'static str,
        reset: &'static str,
    ) -> [(&'static str, &'static str); 3] {
        [
            ("x-ratelimit-limit", limit),
            ("x-ratelimit-remaining", remaining),
            ("x-ratelimit-reset", reset),
        ]
    }

    /// `/repos/o/r` にこのヘッダを載せた 200 を返させる。
    async fn mount_repo(server: &MockServer, headers: &[(&str, &str)]) {
        Mock::given(method("GET"))
            .and(path("/repos/o/r"))
            .respond_with(with_headers(
                ResponseTemplate::new(200).set_body_json(repo_body()),
                headers,
            ))
            .mount(server)
            .await;
    }

    /// `repo` を 1 本叩いたあとにクライアントが憶えた残量。
    async fn rate_after_repo(headers: &[(&str, &str)]) -> Option<RateLimit> {
        let server = MockServer::start().await;
        mount_repo(&server, headers).await;
        let api = client(&server);
        api.repo("o", "r").await.expect("repo");
        api.rate_limit()
    }

    /// `repo` → `pr_list` と続けて叩いたあとに憶えた残量。**畳み方を見る形**で、
    /// 2 本目のほうが必ずあとに返る。
    async fn rate_after_repo_then_list(
        repo: &[(&str, &str)],
        pulls: &[(&str, &str)],
    ) -> Option<RateLimit> {
        let server = MockServer::start().await;
        mount_repo(&server, repo).await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls"))
            .respond_with(with_headers(
                ResponseTemplate::new(200).set_body_json(json!([])),
                pulls,
            ))
            .mount(&server)
            .await;
        let api = client(&server);
        api.repo("o", "r").await.expect("repo");
        api.pr_list("o", "r").await.expect("pr_list");
        api.rate_limit()
    }

    #[tokio::test]
    async fn the_remaining_quota_comes_from_the_response_headers() {
        // **`/rate_limit` は叩かない。** 同じセッションで連続して叩いても数が合わず、
        // `reset` は後ろを返した（05 の実測表）。
        let seen = rate_after_repo(&quota_headers("5000", "4984", "1789703295")).await;

        assert_eq!(
            seen,
            Some(RateLimit {
                limit: 5000,
                remaining: 4984,
                reset: 1789703295,
            })
        );
    }

    #[tokio::test]
    async fn a_response_without_the_headers_leaves_the_quota_unknown() {
        // レートリミットを切った GHES がこの形。**エラーにはしない** ——
        // 出さないだけ（07 のヘッダ）。
        assert_eq!(rate_after_repo(&[]).await, None);
        // 3 つのうち 1 つでも欠けたら組まない。
        assert_eq!(
            rate_after_repo(&[
                ("x-ratelimit-limit", "5000"),
                ("x-ratelimit-remaining", "4984"),
            ])
            .await,
            None
        );
    }

    #[tokio::test]
    async fn a_quota_from_another_bucket_is_not_recorded() {
        // `search` は上限が 2 桁小さい（05 の実測表）。混ぜると残量が突然 30 になる。
        // **いま叩くエンドポイントは全部 `core`** なので、この腕を踏めるのはここだけ。
        let mut headers = vec![("x-ratelimit-resource", "search")];
        headers.extend(quota_headers("30", "28", "1789703295"));

        assert_eq!(rate_after_repo(&headers).await, None);
    }

    #[tokio::test]
    async fn a_rate_limited_response_still_records_the_quota() {
        // **状態を見る前に採る。** 残量 0 は 403 / 429 で返ってくるので、
        // `check_status` の後ろで採ると**いちばん出したい数だけが採れない**。
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r"))
            .respond_with(with_headers(
                ResponseTemplate::new(403),
                &quota_headers("5000", "0", "1789703295"),
            ))
            .mount(&server)
            .await;
        let api = client(&server);

        let err = api.repo("o", "r").await.unwrap_err();

        assert!(matches!(err, GithubError::RateLimited { .. }), "{err:?}");
        assert_eq!(api.rate_limit().map(|seen| seen.remaining), Some(0));
    }

    #[tokio::test]
    async fn the_newer_of_two_responses_wins_even_when_it_comes_first() {
        // **届く順は新しさの順ではない**（扇形は同時 4 本）。あとから届いた
        // 「残量の多い」応答で上書きすると、ヘッダの数が増えたり減ったりする。
        let seen = rate_after_repo_then_list(
            &quota_headers("5000", "4900", "1789703295"),
            &quota_headers("5000", "4950", "1789703295"),
        )
        .await;

        assert_eq!(
            seen.map(|seen| seen.remaining),
            Some(4900),
            "あとから届いた古い応答で上書きした"
        );
    }

    #[tokio::test]
    async fn a_response_from_the_next_window_wins_even_with_more_left() {
        // 窓が変われば残量は戻る。**比べるのは `(reset, used)`** で、`reset` が先。
        let seen = rate_after_repo_then_list(
            &quota_headers("5000", "12", "1789703295"),
            &quota_headers("5000", "5000", "1789706895"),
        )
        .await;

        assert_eq!(seen.map(|seen| seen.remaining), Some(5000));
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

    #[tokio::test]
    async fn pr_detail_counts_every_check_state_and_does_not_fold_them() {
        // **[`CheckState`] の腕を畳まない**（07）。`Skipped` と `Neutral` を成功か
        // 失敗のどちらかに足すと**どちらも嘘になる**。
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits/abc/check-runs"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "check_runs": [
                    { "name": "a", "status": "completed", "conclusion": "success", "html_url": null },
                    { "name": "b", "status": "completed", "conclusion": "failure", "html_url": null },
                    { "name": "c", "status": "in_progress", "conclusion": null, "html_url": null },
                    { "name": "d", "status": "completed", "conclusion": "skipped", "html_url": null },
                    { "name": "e", "status": "completed", "conclusion": "neutral", "html_url": null },
                ]})),
            )
            .mount(&server)
            .await;
        // **commit status のぶんも同じ集計に入る**（片方だけ見ると、古い API しか
        // 使っていないリポジトリで 0 件になる）。
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits/abc/status"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "statuses": [
                    { "context": "legacy", "state": "success", "target_url": null },
                ]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls/7/reviews"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "user": { "login": "u1" }, "state": "APPROVED" },
                { "user": { "login": "u2" }, "state": "CHANGES_REQUESTED" },
            ])))
            .mount(&server)
            .await;

        let detail = client(&server).pr_detail("o", "r", 7, "abc").await.unwrap();
        assert_eq!(
            detail.checks,
            CheckSummary {
                success: 2,
                failure: 1,
                pending: 1,
                skipped: 1,
                neutral: 1,
            }
        );
        assert_eq!(detail.review.approved, 1);
        assert_eq!(detail.review.changes_requested, 1);
    }

    /// 遅らせて返しつつ、**リクエストが届いた時刻**を記録する。
    ///
    /// **`respond` は遅延の前に呼ばれる** —— wiremock 0.6.5 の `mock_set.rs` は
    /// `response_template()` を呼んでから `delay().map(sleep)` を組み、`hyper.rs` が
    /// ハンドラの最後で `delay.await` する（ソースで確認、2026-09-18）。
    struct Slow {
        seen: Arc<Mutex<Vec<Instant>>>,
        body: serde_json::Value,
    }

    impl Respond for Slow {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            self.seen
                .lock()
                .expect("毒されたロックはこのテストでは起きない")
                .push(Instant::now());
            ResponseTemplate::new(200)
                .set_body_json(self.body.clone())
                .set_delay(SLOW_DELAY)
        }
    }

    const SLOW_DELAY: Duration = Duration::from_millis(200);

    /// [`Slow`] を 1 本まとめる。**3 本とも同じ `seen` を共有する**ので、
    /// 届いた順が 1 本の列になる。
    async fn mount_slow(
        server: &MockServer,
        builder: wiremock::MockBuilder,
        seen: &Arc<Mutex<Vec<Instant>>>,
        body: serde_json::Value,
    ) {
        builder
            .respond_with(Slow {
                seen: Arc::clone(seen),
                body,
            })
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn the_fan_out_does_not_put_more_than_four_requests_in_flight() {
        // **上限は `PERMITS`。** 6 件 × 3 リクエスト（check-runs / status / reviews）
        // = 18 本を同時に頼み、**5 本目が 1 本目と同時には届かない**ことを見る。
        // permit を外すと 18 本が一斉に届いてここが落ちる。
        //
        // **「ちょうど 4 本に届く」は見ない。** `PERMITS` はプロセスに 1 つで、
        // **同じテストバイナリの他のテストも通る**ので、こちらが t0 に何本取れるかは
        // 他のテストの混み具合で変わる。ここで見るのは**上限のほう**だけ。
        //
        // **`try_join!` の入れ子で詰まる形は timeout が拾う。** permit を PR ごとにも
        // 取ると、外側が握ったまま内側が待って**誰も返さなくなる**（05）。
        let server = MockServer::start().await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        mount_slow(
            &server,
            Mock::given(method("GET")).and(path("/repos/o/r/commits/abc/check-runs")),
            &seen,
            json!({ "check_runs": [] }),
        )
        .await;
        mount_slow(
            &server,
            Mock::given(method("GET")).and(path("/repos/o/r/commits/abc/status")),
            &seen,
            json!({ "statuses": [] }),
        )
        .await;
        mount_slow(
            &server,
            Mock::given(method("GET")).and(path_regex(r"^/repos/o/r/pulls/\d+/reviews$")),
            &seen,
            json!([]),
        )
        .await;

        let api = client(&server);
        let calls = (1..=6u64).map(|number| api.pr_detail("o", "r", number, "abc"));
        let done =
            tokio::time::timeout(Duration::from_secs(20), futures::future::join_all(calls)).await;
        let results = done.expect("扇形が返らない（permit を握ったまま permit を待っている）");
        assert!(results.iter().all(|r| r.is_ok()), "{results:?}");

        let mut times = seen.lock().expect("ロック").clone();
        assert_eq!(times.len(), 18, "叩いた本数");
        times.sort();
        assert!(
            times[4].duration_since(times[0]) >= SLOW_DELAY / 2,
            "5 本目が 1 本目と同時に届いている（同時本数が抑えられていない）"
        );
    }

    /// 叩かれたパスを記録して同じ応答を返す。
    struct Recording {
        seen: Arc<Mutex<Vec<String>>>,
        status: u16,
    }

    impl Respond for Recording {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            self.seen
                .lock()
                .expect("毒されたロックはこのテストでは起きない")
                .push(request.url.path().to_string());
            ResponseTemplate::new(self.status)
                .insert_header("x-ratelimit-remaining", "0")
                .set_body_string("{}")
        }
    }

    #[tokio::test]
    async fn a_fan_out_stops_asking_after_a_failure_that_repeats() {
        // **「どの対象でも同じように起きる」失敗を受けたら残りは頼まない**（05）。
        // 全部を先に起こしてから旗で止める形では**1 本も止まらない** —— 起こした
        // ぶんは最初の応答が返るより前に旗を読み終えているため。`buffer_unordered` は
        // 次を必要になってから作るので、抜ければ残りは組み立てられもしない。
        let server = MockServer::start().await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        Mock::given(method("GET"))
            .respond_with(Recording {
                seen: Arc::clone(&seen),
                // 残量 0 の 403 = 一次レート制限（`is_rate_limited`）。
                status: 403,
            })
            .mount(&server)
            .await;

        let targets: Vec<(u64, String)> = (1..=8).map(|n| (n, format!("sha{n}"))).collect();
        let mut answers = Vec::new();
        let aborted = client(&server)
            .pr_details("o", "r", targets, |number, _sha, result| {
                answers.push((number, result));
            })
            .await;

        // **主張はこれ —— 後ろの対象を 1 度も叩かない。** 先に見る（下の 2 つは
        // どちらもこの腕と同じ原因で落ちるので、順番を逆にするとこの腕が踏まれない）。
        // 同時に走るのは `FAN_OUT` 件までなので、5 件目より後ろは組み立てられもしない。
        let seen = seen.lock().expect("ロック").clone();
        for n in (FAN_OUT + 1)..=8 {
            assert!(
                !seen.iter().any(|path| path.contains(&format!("sha{n}"))
                    || path.contains(&format!("pulls/{n}/"))),
                "{n} 件目を叩いている: {seen:?}"
            );
        }
        assert_eq!(answers.len(), 1, "打ち切ったのに答えを配り続けた");
        assert!(
            matches!(aborted, Some(GithubError::RateLimited { .. })),
            "{aborted:?}"
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
