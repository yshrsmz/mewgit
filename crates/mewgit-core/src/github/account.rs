//! [`auth::Backend`] の production 実装。設計は 04 の「`Backend` の production 実装は
//! `github` に置く」。
//!
//! **ここに置くのは、`auth::Backend` と [`GithubClient`] の両方を名指しできる最下段が
//! `github` だから**（`auth` は `github` を名指しできない。00 のレイヤー則）。
//!
//! [`auth::Backend`]: crate::auth::Backend

use std::path::PathBuf;

use secrecy::{ExposeSecret, SecretString};

use crate::auth::gh::GhCli;
use crate::auth::{AttemptFailure, AuthError, Backend, GhLogin, RepoTarget};

use super::{GithubClient, GithubError, RepoMeta};

/// `gh` と GitHub API を [`auth::resolve`](crate::auth::resolve) に渡す形にしたもの。
#[derive(Debug)]
pub struct AccountBackend {
    /// `gh` を探した結果。**失敗を抱えたまま持つ**（04）。
    ///
    /// ランタイムが畳むと、01 が「`AuthError` は `resolve` の中で `DegradedReason` に
    /// 畳む」と決めた変換が 2 箇所になる。抱えた `Err` を clone して
    /// [`logins`](Backend::logins) / [`token`](Backend::token) から返せば、04 の手順 2 と
    /// 手順 1 がそれぞれ `GhMissing` / `GhTooOld` に落とす。
    gh: Result<GhCli, AuthError>,
}

impl AccountBackend {
    /// `gh` を探す。`bin` には `config.general.gh_bin` を渡す。
    ///
    /// **`Result` を返さない。** 見つからなかったことも [`Backend`] の答えとして
    /// 運ぶ（上の `gh`）。
    pub async fn detect(bin: impl Into<PathBuf>) -> Self {
        AccountBackend {
            gh: GhCli::detect(bin).await,
        }
    }

    fn gh(&self) -> Result<&GhCli, AuthError> {
        self.gh.as_ref().map_err(Clone::clone)
    }
}

// **`async fn` で書く。** trait 側は `-> impl Future<..> + Send` と書いてある
// （`async fn` を trait に書くと `Send` が付かない。04）が、impl の側は `async fn` で
// よく、`Send` を満たしているかはコンパイラが確かめる。手で `async move { .. }` を
// 返すと `clippy::manual_async_fn` に当たる。
impl Backend for AccountBackend {
    async fn logins(&self, host: &str) -> Result<Vec<GhLogin>, AuthError> {
        self.gh()?.auth_status(host).await
    }

    async fn token(&self, host: &str, login: &str) -> Result<SecretString, AuthError> {
        self.gh()?.token(host, login).await
    }

    async fn probe(&self, target: &RepoTarget, token: &SecretString) -> Result<(), AttemptFailure> {
        // **`new` の失敗も同じ関数を通す。** ベース URL を組めない（`InvalidHost`）、
        // TLS バックエンドを初期化できない（`shared_http`）はどちらも
        // 「断られてはいないが確かめられなかった」で、`attempt` の最後の 3 行と同じ扱い。
        let result = match client(target, token) {
            Ok(client) => client.repo(&target.owner, &target.repo).await,
            Err(err) => Err(err),
        };
        attempt(result)
    }
}

/// 候補 1 件ぶんのクライアント。
///
/// **トークンは中身を写して渡す。** secrecy 0.10 の `SecretString` は
/// `SecretBox<str>` で `Clone` を持たない（`str` が `CloneableSecret` でない）ので、
/// `expose_secret()` から作り直すしかない。`resolve` は probe に使ったトークンを
/// そのまま `Account` に載せる（04 の手順 4）ので、こちらが借りるだけでは足りない。
fn client(target: &RepoTarget, token: &SecretString) -> Result<GithubClient, GithubError> {
    GithubClient::new(
        &target.host,
        target.port,
        SecretString::from(token.expose_secret().to_string()),
    )
}

/// `GET /repos/{owner}/{repo}` の結果を「このアカウントで届いたか」に落とす（04 の表）。
///
/// **`Result` 全体を受ける。** `Err(Decode)` が `Ok(())` になる行があるので、
/// 「成功したか」を先に数えて `Err` だけを写す形にすると 1 行落ちる。
///
/// 網羅 `match` で書く。`_ =>` を置くと、[`GithubError`] に変種が増えたときに
/// 黙って `Undetermined`（もう一度なら通りうる）に化ける。
fn attempt(result: Result<RepoMeta, GithubError>) -> Result<(), AttemptFailure> {
    match result {
        // **`Decode` は 2xx が返っている。** `check_status` を通ったあとにしか出ない
        // ので、GHES の版差で本文の形が違うだけでアクセス権はある。断ると、動いて
        // いる環境が `NoAccess` になる（04）。
        Ok(_) | Err(GithubError::Decode { .. }) => Ok(()),
        Err(GithubError::Unauthorized) => Err(denied(401)),
        Err(GithubError::Forbidden { .. }) => Err(denied(403)),
        // 権限が無ければ GitHub は 404 を返す。
        Err(GithubError::NotFound) => Err(denied(404)),
        Err(GithubError::Api { status, .. }) => Err(denied(status)),
        // **レート制限にステータスを載せない。** 05 の `RateLimited` は 429 だけでなく
        // 印の付いた 403 からも来るので、`Denied { status }` に畳むと嘘になる。
        // `InvalidHost` は 1 本目（#63）の `valid_host` で届かなくなったが、
        // `unreachable!()` にはしない。
        Err(
            err @ (GithubError::RateLimited { .. }
            | GithubError::Network { .. }
            | GithubError::InvalidHost { .. }),
        ) => Err(AttemptFailure::Undetermined {
            message: err.to_string(),
        }),
    }
}

fn denied(status: u16) -> AttemptFailure {
    AttemptFailure::Denied { status }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> RepoMeta {
        RepoMeta {
            owner: "owner".to_string(),
            repo: "repo".to_string(),
            default_branch: "main".to_string(),
        }
    }

    fn target(host: &str) -> RepoTarget {
        RepoTarget {
            host: host.to_string(),
            port: None,
            owner: "owner".to_string(),
            repo: "repo".to_string(),
        }
    }

    /// 04 の対応表を 1 行ずつ。**`Ok` の行を含めて全部回す** —— `Err` だけを写す
    /// 形にすると `Decode` の行が落ちる。
    #[test]
    fn every_row_of_the_probe_table_is_covered() {
        let cases: Vec<(Result<RepoMeta, GithubError>, Result<(), AttemptFailure>)> = vec![
            (Ok(meta()), Ok(())),
            (
                Err(GithubError::Decode {
                    message: "missing field".to_string(),
                }),
                Ok(()),
            ),
            (Err(GithubError::Unauthorized), Err(denied(401))),
            (
                Err(GithubError::Forbidden {
                    needed_scopes: vec!["repo".to_string()],
                    saml: false,
                    body_head: String::new(),
                }),
                Err(denied(403)),
            ),
            (Err(GithubError::NotFound), Err(denied(404))),
            (
                Err(GithubError::Api {
                    status: 502,
                    body_head: "bad gateway".to_string(),
                }),
                Err(denied(502)),
            ),
        ];
        for (result, want) in cases {
            let shown = format!("{result:?}");
            assert_eq!(attempt(result), want, "{shown}");
        }
    }

    /// 確かめられなかった 3 つ。**`message` に理由が残る** —— `NoAccess` の案内は
    /// これを 1 件ずつ並べる（04）。
    #[test]
    fn the_three_undetermined_rows_keep_their_reason() {
        let cases = [
            GithubError::RateLimited { reset_at: None },
            GithubError::Network {
                message: "接続できません".to_string(),
            },
            GithubError::InvalidHost {
                host: "-4".to_string(),
            },
        ];
        for err in cases {
            let want = err.to_string();
            match attempt(Err(err)) {
                Err(AttemptFailure::Undetermined { message }) => assert_eq!(message, want),
                other => panic!("確かめられなかったことになっていない: {other:?}"),
            }
        }
    }

    /// `gh` が見つからなければ、[`Backend`] の 2 つは同じ失敗を返す。
    ///
    /// **`detect` の `Err` を抱えたまま持つ**（04）ので、`resolve` の手順 1 と手順 2 が
    /// どちらもここから `DegradedReason` を組める。
    #[tokio::test]
    async fn a_missing_gh_comes_back_from_both_calls() {
        let backend = AccountBackend::detect("/nonexistent/gh").await;

        let logins = backend.logins("github.com").await;
        let token = backend.token("github.com", "taro").await;

        assert!(
            matches!(logins, Err(AuthError::GhMissing { .. })),
            "{logins:?}"
        );
        assert!(
            matches!(token, Err(AuthError::GhMissing { .. })),
            "{token:?}"
        );
    }

    /// ベース URL を組めないホストは、ネットワークへ出る前に
    /// [`AttemptFailure::Undetermined`] になる。
    ///
    /// **`new` の失敗が `attempt` を通っていること**の見張り。実際に API へ届く経路は
    /// ここでは踏めない（`base_url` は https 固定なので、モックサーバへ向けられない）。
    #[tokio::test]
    async fn probe_folds_a_client_that_cannot_be_built() {
        let backend = AccountBackend::detect("/nonexistent/gh").await;

        let result = backend
            .probe(&target("-4"), &SecretString::from("token".to_string()))
            .await;

        match result {
            Err(AttemptFailure::Undetermined { message }) => {
                assert!(message.contains("-4"), "{message}");
            }
            other => panic!("確かめられなかったことになっていない: {other:?}"),
        }
    }
}
