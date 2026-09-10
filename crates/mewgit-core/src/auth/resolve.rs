//! アカウント解決。設計は `docs/design/04-auth-account.md` の「アカウント解決」。
//!
//! 明示指定 → 単一 → probe → Ambiguous の 4 手順。**外の世界（`gh` と GitHub API）は
//! [`Backend`] の 3 つに畳んである** —— 00 のレイヤー図で `github` は `auth` の**上段**
//! なので、`resolve` が `GithubClient` を名指しすると依存が上を向く。`GhCli` のほうは
//! 下向きで許されるが、**同じ trait に載せる**（04）—— 実 `gh` では Single / Probe /
//! Ambiguous を踏めない（隔離した `GH_CONFIG_DIR` では 0 件、偽トークンを置くと実
//! ホストへ検証に出る。#61 で確認）ので、trait の外に残すと手順 1〜4 を見張れない。

use std::future::Future;
use std::path::Path;

use secrecy::SecretString;

use crate::config::Config;

use super::{
    Account, AccountSource, AccountState, AttemptFailure, AuthError, DegradedReason, GhLogin,
    ProbeAttempt, RepoTarget,
};

/// [`resolve`] が外の世界に頼む 3 つ。
///
/// **返り値に `Send` を要求する。** ランタイムは `Cmd::ResolveAccount` を
/// `tokio::spawn` に載せるので、`resolve` の future 全体が `Send` でないと積めない
/// （`async fn` を trait に書くと `Send` が付かない）。
pub trait Backend {
    /// そのホストのログイン一覧（`gh auth status --hostname <host> --json hosts`）。
    fn logins(&self, host: &str) -> impl Future<Output = Result<Vec<GhLogin>, AuthError>> + Send;

    /// その login のトークン（`gh auth token --hostname <host> --user <login>`）。
    fn token(
        &self,
        host: &str,
        login: &str,
    ) -> impl Future<Output = Result<SecretString, AuthError>> + Send;

    /// そのトークンで `GET /repos/{owner}/{repo}` に届いたか。
    ///
    /// **成功側は空。** 呼ぶ側が見るのは「このアカウントで通ったか」だけで、
    /// リポジトリのメタ情報は捨てる（05 の「`repo` は probe 兼用」）。
    fn probe(
        &self,
        target: &RepoTarget,
        token: &SecretString,
    ) -> impl Future<Output = Result<(), AttemptFailure>> + Send;
}

/// リポジトリで使うアカウントを決める（04 の手順 1〜4）。
///
/// **`Resolving` は返さない。** あれは「まだ頼んでいない」で、`Cmd` を出した側が
/// 置く値（[`AccountState::default`]）。
///
/// `repo_path` はワークツリーのルート。手順 1 の `mewgit.user` を読むために要る
/// （`config` モジュールは `.git/config` を見ない。08）。
pub async fn resolve<B: Backend + Sync>(
    target: &RepoTarget,
    repo_path: &Path,
    backend: &B,
    cfg: &Config,
) -> AccountState {
    // 手順 1。**明示指定があれば一覧を引かない** —— `gh auth status` は全候補の
    // トークンをホストへ検証しに行くので、答えが決まっているのに 0.24 秒払うことになる。
    if let Some(login) = explicit_login(target, repo_path, cfg).await {
        return match backend.token(&target.host, &login).await {
            Ok(token) => resolved(target, login, token, AccountSource::Config),
            Err(err) => AccountState::Degraded(configured_failed(login, err)),
        };
    }

    // 手順 2。
    let mut logins = match backend.logins(&target.host).await {
        Ok(logins) => logins,
        Err(err) => return AccountState::Degraded(reason_for(err)),
    };
    if logins.is_empty() {
        return AccountState::Degraded(DegradedReason::NotLoggedIn {
            host: target.host.clone(),
        });
    }

    // 手順 3。**probe しない**（04）。1 件しかないなら選びようが無く、起動のたびに
    // 1 往復払う理由が無い。
    if logins.len() == 1 {
        let login = logins.remove(0).login;
        return match backend.token(&target.host, &login).await {
            Ok(token) => resolved(target, login, token, AccountSource::Single),
            Err(err) => AccountState::Degraded(DegradedReason::NoAccess {
                attempts: vec![ProbeAttempt {
                    login,
                    failure: AttemptFailure::NoToken {
                        message: err.to_string(),
                    },
                }],
            }),
        };
    }

    // 手順 4。**候補ごとの「token → probe」を 1 本の future にして並列に回す。**
    // 逐次だと N×(トークン取得 + RTT) を起動のたびに払う。
    let candidates = futures::future::join_all(logins.into_iter().map(|login| async move {
        let token = match backend.token(&target.host, &login.login).await {
            Ok(token) => token,
            Err(err) => {
                return (
                    login,
                    Err(AttemptFailure::NoToken {
                        message: err.to_string(),
                    }),
                )
            }
        };
        (login, backend.probe(target, &token).await.map(|()| token))
    }))
    .await;

    let mut passed = Vec::new();
    let mut unchecked = Vec::new();
    let mut attempts = Vec::new();
    for (login, result) in candidates {
        match result {
            Ok(token) => passed.push((login, token)),
            // **`Undetermined` は「除外できた」ではない。** 断られていない候補は
            // 下の fold で数え直す（`AttemptFailure` に 3 つ目を足したので、
            // 出口も 3 つで数える。`CLAUDE.md`）。
            Err(failure @ AttemptFailure::Undetermined { .. }) => {
                attempts.push(ProbeAttempt {
                    login: login.login.clone(),
                    failure,
                });
                unchecked.push(login);
            }
            Err(failure) => attempts.push(ProbeAttempt {
                login: login.login,
                failure,
            }),
        }
    }

    match (passed.len(), unchecked.is_empty()) {
        // **確かめられなかった候補が 1 つも無いときだけ「1 件に絞れた」と言う。**
        // 混ぜると、回線やレート制限で 1 本落ちただけの回に**別のアカウントを黙って
        // 選んで latch する** —— EMU と個人アカウントの両方がそのリポジトリの
        // collaborator なら、以後の PR は違う名前で立つ。**トークンを取り直さない**
        // （probe に使ったものをそのまま渡す）。
        (1, true) => {
            let (login, token) = passed.remove(0);
            resolved(target, login.login, token, AccountSource::Probe)
        }
        // 届いたことを確かめられた候補が無い。**理由は `attempts` に全部ある** ——
        // 全部が `Undetermined`（起動時に回線が無い）の形もここに来るので、
        // 読む側は変種の名前を「権限が無い」と読まない。**選ばせない**のは、
        // どれも動かない状態でアカウントを選んでも何も直らないため。
        (0, _) => AccountState::Degraded(DegradedReason::NoAccess { attempts }),
        // **除外できなかった候補を並べる。** 断られたもの（401 / 404）は落とす ——
        // UI が「選べるアカウント」として届かないログインを出すことになる。
        // **`auth_status` を呼び直さないで済むよう `GhLogin` ごと運ぶ**
        // （呼び直すと 0.95 秒を再度払う。#61 の実測）。
        _ => AccountState::Degraded(DegradedReason::Ambiguous {
            candidates: passed
                .into_iter()
                .map(|(login, _)| login)
                .chain(unchecked)
                .collect(),
        }),
    }
}

/// 手順 1 の明示指定。`.git/config` の `mewgit.user` が設定ファイルより強い。
async fn explicit_login(target: &RepoTarget, repo_path: &Path, cfg: &Config) -> Option<String> {
    if let Some(login) = configured_user(repo_path).await {
        return Some(login);
    }
    cfg.account_for(&target.host, &target.owner, &target.repo)
        .map(str::to_string)
}

/// `.git/config` の `mewgit.user`。
///
/// **`Local` の段だけを読む。** 全段（`Repository::config`）だと `~/.gitconfig` の
/// `mewgit.user` が全リポジトリに効いてしまい、08 が「`config` モジュールは
/// `.git/config` を見ない」と割った境目が消える（04 の「`.git/config` の読み方」に
/// 実測の表。**テストは書いていない** —— libgit2 に global の段を差し替えさせるには
/// プロセス全体の `git2::opts::set_search_path` が要る）。**リンクされたワークツリーでも
/// `Local` はリポジトリ共有の `config`** で、同じリポジトリなら同じアカウントに
/// なる（`Worktree` の段は読まない）。
///
/// **失敗はすべて `None`。** 「設定されていない」と「読めなかった」を分けても、
/// 手順 2 へ落ちるという同じ結果にしかならない。
async fn configured_user(repo_path: &Path) -> Option<String> {
    let path = repo_path.to_path_buf();
    // git2 は同期なので `spawn_blocking` に載せる（規約）。git2 の型は await をまたがない。
    let user = tokio::task::spawn_blocking(move || read_mewgit_user(&path))
        .await
        .ok()??;
    // **空文字は「設定されていない」と同じに扱う。** `gh auth token --user ""` を
    // 叩いても答えは返らないので、設定を消し忘れた形で degraded にしない。
    let user = user.trim();
    (!user.is_empty()).then(|| user.to_string())
}

fn read_mewgit_user(path: &Path) -> Option<String> {
    git2::Repository::open(path)
        .ok()?
        .config()
        .ok()?
        .open_level(git2::ConfigLevel::Local)
        .ok()?
        .get_string("mewgit.user")
        .ok()
}

/// `gh` の失敗を degraded の理由に畳む（01 の「`AccountResolved`」）。
///
/// **網羅 `match` で書く。** `_ =>` を置くと、新しい [`AuthError`] が既存の理由に
/// 化けて通知が嘘をつく（「gh が見つかりませんでした」と出して PATH を探させる形）。
fn reason_for(err: AuthError) -> DegradedReason {
    match err {
        AuthError::GhMissing { .. } => DegradedReason::GhMissing,
        AuthError::GhTooOld { found } => DegradedReason::GhTooOld { found },
        AuthError::NotLoggedIn { host } => DegradedReason::NotLoggedIn { host },
        // gh は居るのに答えが返らない 3 つ。**分けない** —— 直し方の違いは
        // `Display` の中身（起動できない / argv と exit / 解釈できなかった JSON）に
        // 出ており、これで分岐する読み手が UI に無い。
        AuthError::GhUnrunnable { .. } | AuthError::GhFailed { .. } | AuthError::GhUnparsable { .. }
        // `ssh` と URL 由来の 2 つは [`Backend`] からは来ないが、**受け皿を置く**。
        // `unreachable!()` にすると、実装した誰かが返した瞬間に落ちる。
        | AuthError::SshResolveFailed { .. } | AuthError::InvalidRemoteUrl { .. } => {
            DegradedReason::Unusable {
                message: err.to_string(),
            }
        }
    }
}

/// 手順 1 の `token()` が失敗した理由を畳む。
///
/// **login 固有の失敗だけを [`DegradedReason::ConfiguredUnavailable`] にする。**
/// `gh` が居ない・古いは**プロセス全体の条件**で、設定を直しても変わらない ——
/// 一緒くたにすると「`mewgit.user` を直してください」と案内しながら、UI の
/// `GhTooOld`（「2.81.0 以上に上げてください」）の腕が一度も発火しなくなる。
/// **手順 1 は手順 2 を意図的に飛ばすので、`gh` の失敗の入口はここにもある。**
fn configured_failed(login: String, err: AuthError) -> DegradedReason {
    match err {
        // `gh auth token --user <login>` が非 0 で終わったもの。実測の文言は
        // `no oauth token found for <host> account <login>`（04 の表）。
        AuthError::GhFailed { .. } => DegradedReason::ConfiguredUnavailable {
            login,
            message: err.to_string(),
        },
        other => reason_for(other),
    }
}

fn resolved(
    target: &RepoTarget,
    login: String,
    token: SecretString,
    source: AccountSource,
) -> AccountState {
    AccountState::Resolved(Account {
        host: target.host.clone(),
        login,
        token,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::LoginState;
    use crate::test_util::{fake_target, TestRepo};
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::time::Duration;

    /// 手順 1〜4 を踏むための [`Backend`]。
    ///
    /// **実 `gh` では踏めない**（04 の「`&GhCli` を `resolve` の引数にすると
    /// テストできない」）ので、trait の向こう側はここで組む。
    struct Fake {
        logins: Result<Vec<GhLogin>, AuthError>,
        /// login ごとの probe の結果。無い login は届かなかったことにする。
        probes: BTreeMap<String, Result<(), AttemptFailure>>,
        /// トークンを取れない login と、そのときの `gh` の失敗。**種類を持つ** ——
        /// 手順 1 は login 固有の失敗（`GhFailed`）とプロセス全体の条件
        /// （`GhMissing` など）を分けるので、1 種類しか作れないと片方を踏めない。
        no_token: BTreeMap<String, AuthError>,
        calls: Mutex<Vec<String>>,
        /// 全候補が揃うまで probe を返さない関所。**逐次に回すと開かない。**
        gate: Option<tokio::sync::Barrier>,
    }

    impl Fake {
        fn new(logins: Vec<GhLogin>) -> Self {
            Fake {
                logins: Ok(logins),
                probes: BTreeMap::new(),
                no_token: BTreeMap::new(),
                calls: Mutex::default(),
                gate: None,
            }
        }

        fn failing(err: AuthError) -> Self {
            Fake {
                logins: Err(err),
                ..Fake::new(Vec::new())
            }
        }

        fn probing(mut self, login: &str, outcome: Result<(), AttemptFailure>) -> Self {
            self.probes.insert(login.to_string(), outcome);
            self
        }

        fn without_token(self, login: &str) -> Self {
            self.failing_token(
                login,
                AuthError::GhFailed {
                    argv: vec!["gh".to_string(), "auth".to_string(), "token".to_string()],
                    message: format!("exit 1: no oauth token found for account {login}"),
                },
            )
        }

        fn failing_token(mut self, login: &str, err: AuthError) -> Self {
            self.no_token.insert(login.to_string(), err);
            self
        }

        fn gated(mut self, parties: usize) -> Self {
            self.gate = Some(tokio::sync::Barrier::new(parties));
            self
        }

        fn record(&self, call: String) {
            self.calls.lock().expect("記録").push(call);
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("記録").clone()
        }
    }

    impl Backend for Fake {
        fn logins(
            &self,
            host: &str,
        ) -> impl Future<Output = Result<Vec<GhLogin>, AuthError>> + Send {
            self.record(format!("logins:{host}"));
            let result = self.logins.clone();
            async move { result }
        }

        fn token(
            &self,
            _host: &str,
            login: &str,
        ) -> impl Future<Output = Result<SecretString, AuthError>> + Send {
            self.record(format!("token:{login}"));
            let result = match self.no_token.get(login) {
                Some(err) => Err(err.clone()),
                None => Ok(SecretString::from(format!("token-for-{login}"))),
            };
            async move { result }
        }

        fn probe(
            &self,
            _target: &RepoTarget,
            token: &SecretString,
        ) -> impl Future<Output = Result<(), AttemptFailure>> + Send {
            use secrecy::ExposeSecret;
            let login = token
                .expose_secret()
                .strip_prefix("token-for-")
                .expect("トークンは token() が作ったもの")
                .to_string();
            self.record(format!("probe:{login}"));
            let outcome = self.probes.get(&login).cloned().unwrap_or_else(|| {
                Err(AttemptFailure::Undetermined {
                    message: "テストが結果を用意していない".to_string(),
                })
            });
            let gate = self.gate.as_ref();
            async move {
                if let Some(gate) = gate {
                    gate.wait().await;
                }
                outcome
            }
        }
    }

    /// `state` は既定で `Success`。**絞り込みに使わないので**（04）、値が効くのは
    /// `gh_state_does_not_narrow_the_candidates` の 1 本だけ。
    fn login(name: &str) -> GhLogin {
        GhLogin {
            host: "github.com".to_string(),
            login: name.to_string(),
            active: false,
            scopes: Vec::new(),
            state: LoginState::Success,
        }
    }

    /// 候補 3 件。手順 4 を踏むテストが使う。
    fn three() -> Vec<GhLogin> {
        ["taro", "hanako", "jiro"]
            .iter()
            .map(|n| login(n))
            .collect()
    }

    fn denied(status: u16) -> Result<(), AttemptFailure> {
        Err(AttemptFailure::Denied { status })
    }

    /// 明示指定の無いリポジトリで手順 2 以降を踏む。**リポジトリを引数に取らない**
    /// —— `mewgit.user` を書くテストだけが `TestRepo` を自分で持つ。
    async fn resolve_plain(backend: &Fake) -> AccountState {
        let repo = TestRepo::init();
        resolve(&fake_target(), repo.path(), backend, &Config::default()).await
    }

    /// `mewgit.user = taro` を書いたリポジトリで解決する（手順 1）。
    async fn resolve_configured(backend: &Fake) -> AccountState {
        let repo = TestRepo::init();
        repo.git(&["config", "mewgit.user", "taro"]);
        resolve(&fake_target(), repo.path(), backend, &Config::default()).await
    }

    fn account(state: &AccountState) -> &Account {
        match state {
            AccountState::Resolved(account) => account,
            other => panic!("解決していない: {other:?}"),
        }
    }

    fn reason(state: &AccountState) -> &DegradedReason {
        match state {
            AccountState::Degraded(reason) => reason,
            other => panic!("degraded ではない: {other:?}"),
        }
    }

    fn secret(account: &Account) -> String {
        use secrecy::ExposeSecret;
        account.token.expose_secret().to_string()
    }

    /// `[[repos]]` が 1 本だけある設定。
    fn config_with_rule(pattern: &str, user: &str) -> Config {
        let text = format!("[[repos]]\nmatch = \"{pattern}\"\nuser = \"{user}\"\n");
        let (config, warnings) = Config::from_toml_str(&text);
        assert!(warnings.is_empty(), "{warnings:?}");
        config
    }

    #[tokio::test]
    async fn an_explicit_user_in_git_config_is_used_without_listing_logins() {
        let backend = Fake::new(vec![login("taro"), login("hanako")]);

        let state = resolve_configured(&backend).await;

        let account = account(&state);
        assert_eq!(account.login, "taro");
        assert_eq!(account.source, AccountSource::Config);
        assert_eq!(account.host, "github.com");
        assert_eq!(secret(account), "token-for-taro");
        // **一覧を引かない**（04）。`gh auth status` は候補のトークンをホストへ
        // 検証しに行くので、答えが決まっているのに 0.24 秒払うことになる。
        assert_eq!(backend.calls(), vec!["token:taro".to_string()]);
    }

    #[tokio::test]
    async fn the_config_file_supplies_the_login_when_git_config_has_none() {
        let repo = TestRepo::init();
        let backend = Fake::new(vec![login("taro")]);
        let config = config_with_rule("github.com/yshrsmz/*", "taro_acme");

        let state = resolve(&fake_target(), repo.path(), &backend, &config).await;

        let account = account(&state);
        assert_eq!(account.login, "taro_acme");
        assert_eq!(account.source, AccountSource::Config);
    }

    #[tokio::test]
    async fn git_config_beats_the_config_file() {
        let repo = TestRepo::init();
        repo.git(&["config", "mewgit.user", "taro"]);
        let backend = Fake::new(Vec::new());
        let config = config_with_rule("github.com/yshrsmz/*", "taro_acme");

        let state = resolve(&fake_target(), repo.path(), &backend, &config).await;

        assert_eq!(account(&state).login, "taro");
    }

    #[tokio::test]
    async fn an_empty_mewgit_user_falls_through_to_the_login_list() {
        // 設定を消し忘れた形（`user = ` が残る）。**degraded にしない** ——
        // 空の login では `gh auth token --user ""` を叩いても答えが返らない。
        let repo = TestRepo::init();
        repo.git(&["config", "mewgit.user", ""]);
        let backend = Fake::new(vec![login("hanako")]);

        let state = resolve(&fake_target(), repo.path(), &backend, &Config::default()).await;

        let account = account(&state);
        assert_eq!(account.login, "hanako");
        assert_eq!(account.source, AccountSource::Single);
    }

    #[tokio::test]
    async fn an_explicit_login_without_a_token_does_not_fall_through() {
        // **指定を無視して別のアカウントを選ばない。** EMU と個人アカウントを
        // 取り違える形になる。
        let backend = Fake::new(vec![login("hanako")]).without_token("taro");

        let state = resolve_configured(&backend).await;

        match reason(&state) {
            DegradedReason::ConfiguredUnavailable { login, message } => {
                assert_eq!(login, "taro");
                assert!(message.contains("no oauth token found"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(backend.calls(), vec!["token:taro".to_string()]);
    }

    #[tokio::test]
    async fn a_process_wide_gh_failure_is_not_dressed_as_a_setting_mistake() {
        // **手順 1 は手順 2 を飛ばすので、`gh` の失敗の入口はここにもある。**
        // 全部 `ConfiguredUnavailable` に畳むと「`mewgit.user` を直してください」と
        // 案内しながら、UI の `GhTooOld`（「2.81.0 以上に上げてください」）の腕が
        // 一度も発火しない。
        let backend = Fake::new(Vec::new()).failing_token(
            "taro",
            AuthError::GhTooOld {
                found: "2.80.0".to_string(),
            },
        );

        let state = resolve_configured(&backend).await;

        assert_eq!(
            reason(&state),
            &DegradedReason::GhTooOld {
                found: "2.80.0".to_string()
            }
        );
    }

    #[tokio::test]
    async fn no_login_for_the_host_is_not_logged_in() {
        let backend = Fake::new(Vec::new());

        let state = resolve_plain(&backend).await;

        assert_eq!(
            reason(&state),
            &DegradedReason::NotLoggedIn {
                host: "github.com".to_string()
            }
        );
    }

    #[tokio::test]
    async fn a_single_login_is_taken_without_probing() {
        let backend = Fake::new(vec![login("taro")]);

        let state = resolve_plain(&backend).await;

        let account = account(&state);
        assert_eq!(account.login, "taro");
        assert_eq!(account.source, AccountSource::Single);
        // **1 往復を払わない**（04 の手順 3）。選びようが無いので probe で判ることが無い。
        assert_eq!(
            backend.calls(),
            vec!["logins:github.com".to_string(), "token:taro".to_string()]
        );
    }

    #[tokio::test]
    async fn a_single_login_without_a_token_reports_the_attempt() {
        let backend = Fake::new(vec![login("taro")]).without_token("taro");

        let state = resolve_plain(&backend).await;

        match reason(&state) {
            // **`Denied { status: 0 }` にしない。** HTTP まで行っていない。
            DegradedReason::NoAccess { attempts } => match attempts.as_slice() {
                [attempt] => {
                    assert_eq!(attempt.login, "taro");
                    assert!(
                        matches!(&attempt.failure, AttemptFailure::NoToken { .. }),
                        "{:?}",
                        attempt.failure
                    );
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn the_only_login_that_reaches_the_repo_wins() {
        let backend = Fake::new(three())
            .probing("hanako", Ok(()))
            .probing("taro", denied(404))
            .probing("jiro", denied(403));

        let state = resolve_plain(&backend).await;

        let account = account(&state);
        assert_eq!(account.login, "hanako");
        assert_eq!(account.source, AccountSource::Probe);
        // **probe に使ったトークンをそのまま渡す。** 取り直すと 1 回分よけいに払う。
        assert_eq!(secret(account), "token-for-hanako");
        assert_eq!(
            backend
                .calls()
                .iter()
                .filter(|c| c.starts_with("token:"))
                .count(),
            3
        );
    }

    #[tokio::test]
    async fn several_reachable_logins_are_ambiguous() {
        let backend = Fake::new(three())
            .probing("taro", Ok(()))
            .probing("hanako", Ok(()))
            .probing("jiro", denied(404));

        let state = resolve_plain(&backend).await;

        match reason(&state) {
            // **通った候補だけが並ぶ。** 404 だった `jiro` を混ぜると、選べる
            // アカウントとして届かないログインを出すことになる。
            DegradedReason::Ambiguous { candidates } => {
                let names: Vec<&str> = candidates.iter().map(|c| c.login.as_str()).collect();
                assert_eq!(names, vec!["taro", "hanako"]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn an_unchecked_candidate_keeps_the_choice_open() {
        // **確かめられなかった候補は「除外できた」ではない。** 通ったのが 1 件でも、
        // 回線やレート制限で 1 本落ちただけの回に別のアカウントを黙って latch すると、
        // 以後の PR が違う名前で立つ（`/code-review` が拾った）。
        let backend = Fake::new(three())
            .probing("taro", Ok(()))
            .probing(
                "hanako",
                Err(AttemptFailure::Undetermined {
                    message: "レート制限に達しました".to_string(),
                }),
            )
            .probing("jiro", denied(404));

        let state = resolve_plain(&backend).await;

        match reason(&state) {
            // 断られた `jiro` は落ちる。残るのは通った `taro` と、確かめられなかった
            // `hanako` の 2 つ。
            DegradedReason::Ambiguous { candidates } => {
                let names: Vec<&str> = candidates.iter().map(|c| c.login.as_str()).collect();
                assert_eq!(names, vec!["taro", "hanako"]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn no_reachable_login_keeps_every_reason() {
        let backend = Fake::new(three())
            .probing("taro", denied(401))
            .probing(
                "hanako",
                Err(AttemptFailure::Undetermined {
                    message: "接続できません".to_string(),
                }),
            )
            .without_token("jiro");

        let state = resolve_plain(&backend).await;

        match reason(&state) {
            // **3 つとも別の腕。** `u16` 1 本では「HTTP 0」に潰れる。
            DegradedReason::NoAccess { attempts } => {
                let seen: Vec<(&str, &AttemptFailure)> = attempts
                    .iter()
                    .map(|a| (a.login.as_str(), &a.failure))
                    .collect();
                assert_eq!(seen.len(), 3);
                assert!(seen.contains(&("taro", &AttemptFailure::Denied { status: 401 })));
                assert!(seen.iter().any(|(login, failure)| *login == "hanako"
                    && matches!(failure, AttemptFailure::Undetermined { .. })));
                assert!(seen.iter().any(|(login, failure)| *login == "jiro"
                    && matches!(failure, AttemptFailure::NoToken { .. })));
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn gh_state_does_not_narrow_the_candidates() {
        // **`Single` は検証せずに確定する**ので、gh の判定で候補を落とすと
        // 「黙って別のアカウントを選ぶ」形が作れる。落とさなければ払うのは
        // 並列のリクエスト 1 本（04 の「候補の絞り込み」）。
        for dropped in [LoginState::Error, LoginState::Timeout] {
            let backend = Fake::new(vec![
                login("taro"),
                GhLogin {
                    state: dropped,
                    ..login("hanako")
                },
            ])
            .probing("taro", denied(404))
            .probing("hanako", Ok(()));

            let state = resolve_plain(&backend).await;

            let account = account(&state);
            assert_eq!(account.login, "hanako", "{dropped:?}");
            assert_eq!(account.source, AccountSource::Probe, "{dropped:?}");
            let probes: Vec<String> = backend
                .calls()
                .into_iter()
                .filter(|c| c.starts_with("probe:"))
                .collect();
            assert_eq!(probes.len(), 2, "{dropped:?}: {probes:?}");
        }
    }

    #[tokio::test]
    async fn candidates_are_probed_in_parallel() {
        // **逐次に回すと開かない関所。** 3 本が揃うまで `probe` が返らないので、
        // 1 本ずつ await する実装ではここで止まる（`timeout` が拾う）。
        let backend = Fake::new(three())
            .probing("taro", Ok(()))
            .probing("hanako", denied(404))
            .probing("jiro", denied(404))
            .gated(3);

        let state = tokio::time::timeout(Duration::from_secs(5), resolve_plain(&backend))
            .await
            .expect("probe が並列に走っていない");

        assert_eq!(account(&state).login, "taro");
    }

    #[tokio::test]
    async fn gh_failures_keep_their_own_reason() {
        let cases = [
            (
                AuthError::GhMissing {
                    bin: "gh".to_string(),
                },
                DegradedReason::GhMissing,
            ),
            (
                AuthError::GhTooOld {
                    found: "2.80.0".to_string(),
                },
                DegradedReason::GhTooOld {
                    found: "2.80.0".to_string(),
                },
            ),
            (
                AuthError::NotLoggedIn {
                    host: "ghe.example.com".to_string(),
                },
                DegradedReason::NotLoggedIn {
                    host: "ghe.example.com".to_string(),
                },
            ),
        ];
        for (err, want) in cases {
            let backend = Fake::failing(err.clone());
            let state = resolve_plain(&backend).await;
            assert_eq!(reason(&state), &want, "{err:?}");
        }

        // 残る 3 つは `Unusable` に畳む。**`GhMissing` に化けさせない** ——
        // 「gh が見つかりませんでした」と出すと PATH を探させることになる。
        let unusable = [
            AuthError::GhUnrunnable {
                message: "権限がありません".to_string(),
            },
            AuthError::GhFailed {
                argv: vec!["gh".to_string(), "auth".to_string()],
                message: "exit 1".to_string(),
            },
            AuthError::GhUnparsable {
                message: "expected value".to_string(),
            },
        ];
        for err in unusable {
            let backend = Fake::failing(err.clone());
            let state = resolve_plain(&backend).await;
            match reason(&state) {
                DegradedReason::Unusable { message } => {
                    assert_eq!(message, &err.to_string(), "{err:?}");
                }
                other => panic!("{err:?}: {other:?}"),
            }
        }
    }
}
