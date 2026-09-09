//! `gh` CLI の呼び出し。設計は `docs/design/04-auth-account.md` の「gh との連携」。
//!
//! **JSON しか読まない。** `gh auth status --json hosts` は 2.81.0 で入った。
//! それ未満は [`AuthError::GhTooOld`] に落とし、テキスト出力は解析しない（04 に理由）。

use std::ffi::OsString;

use secrecy::SecretString;
use serde::Deserialize;
use tokio::process::Command;

use super::{AuthError, GhLogin};

/// `gh auth status --json` と `gh auth token --user` が揃う最低版。
const REQUIRED: Version = Version(2, 81, 0);

/// `gh` があり、最低版を満たすことの証。
///
/// **バイナリのパスは持たない。** 設定で差し替える要求が無く、#21 で
/// `Runner.git_bin` を同じ理由で消してある。PATH 解決は `Command` に任せる。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GhCli {
    env: Vec<(OsString, OsString)>,
}

/// `major.minor.patch`。比較にしか使わないので `semver` は足さない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version(u32, u32, u32);

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

impl GhCli {
    /// 追加の環境変数を重ねる。`git_exec::Runner::with_env` と同じ形。
    ///
    /// **テストが `GH_CONFIG_DIR` で実行環境から切り離すために要る。** `gh` は
    /// 設定ディレクトリを環境変数でしか指定できず（`gh help environment`。
    /// 相当するフラグは無い）、`ssh` の `-F` のように argv で渡せない。
    #[must_use]
    pub fn with_env<I, K, V>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        self.env
            .extend(vars.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    /// `gh --version` を叩いて、存在と最低版を確かめる。
    pub async fn detect() -> Result<Self, AuthError> {
        Self::default().detected().await
    }

    /// [`detect`](Self::detect) と同じだが、重ねた環境変数を保ったまま確かめる。
    pub async fn detected(self) -> Result<Self, AuthError> {
        let out = self.run(&["--version"]).await?;
        // 版を読めないのは「古すぎる」ではなく「知らない形」だが、**通す理由が無い**
        // ので同じ扱いにする。案内には生の 1 行目を出して、何が来たか判るようにする。
        let first = out.lines().next().unwrap_or("").trim();
        let found = parse_version(first);
        match found {
            Some(v) if v >= REQUIRED => Ok(self),
            _ => Err(AuthError::GhTooOld {
                found: found.map_or_else(|| first.to_string(), |v| v.to_string()),
                required: REQUIRED.to_string(),
            }),
        }
    }

    /// ログイン一覧。**`--show-token` は付けない**（付けると JSON に生のトークンが入る。
    /// 不変ルール 3）。
    pub async fn auth_status(&self) -> Result<Vec<GhLogin>, AuthError> {
        let out = self.run(&["auth", "status", "--json", "hosts"]).await?;
        let status: WireStatus =
            serde_json::from_str(&out).map_err(|e| AuthError::GhUnparsable {
                message: e.to_string(),
            })?;
        Ok(merge_logins(status))
    }

    /// 指定したアカウントのトークン。
    pub async fn token(&self, host: &str, user: &str) -> Result<SecretString, AuthError> {
        let out = self
            .run(&["auth", "token", "--hostname", host, "--user", user])
            .await?;
        Ok(SecretString::from(out.trim()))
    }

    /// `gh` を起動して stdout を返す。
    ///
    /// **stderr は [`AuthError::GhFailed`] に載せる**（不変ルール 4 と同じ扱い）。
    /// argv にトークンは載らない（[`token`](Self::token) が受けるのは host と login だけ）。
    async fn run(&self, args: &[&str]) -> Result<String, AuthError> {
        let out = Command::new("gh")
            .args(args)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|_| AuthError::GhMissing)?;
        if !out.status.success() {
            return Err(AuthError::GhFailed {
                argv: std::iter::once("gh")
                    .chain(args.iter().copied())
                    .map(str::to_string)
                    .collect(),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// `gh version 2.100.0 (2026-09-03)` から版を取る。
fn parse_version(line: &str) -> Option<Version> {
    let rest = line.strip_prefix("gh version ")?;
    let number = rest.split_whitespace().next()?;
    let mut parts = number.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    // パッチは無い形（`2.81`）も受ける。比較で 0 として効く。
    let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some(Version(major, minor, patch))
}

#[derive(Debug, Deserialize)]
struct WireStatus {
    /// ホストをキーにしたオブジェクト。値はそのホストのエントリの配列。
    hosts: std::collections::HashMap<String, Vec<WireEntry>>,
}

#[derive(Debug, Deserialize)]
struct WireEntry {
    /// `success` / `timeout` / `error`（v2.100.0 の `status.go` で確認）。
    state: String,
    active: bool,
    login: String,
    /// カンマ区切りの 1 本の文字列。**fine-grained PAT では丸ごと欠ける**ので
    /// `Option`（`omitempty`）。
    scopes: Option<String>,
}

/// エントリを [`GhLogin`] に畳む。
///
/// **`(host, login)` で 1 件にまとめる。** 同じ login が複数のエントリで出る
/// （実機で `GITHUB_TOKEN` と `keyring` の 2 つが並んだ）。`GhLogin` は
/// `tokenSource` を持たないので**見分けられない重複**になり、`resolve` の
/// 「1 件なら `Single`」が誤爆する。`token(host, user)` は login で引くので、
/// 畳んでも取り出せるトークンは変わらない。
///
/// **採るのは `state == "success"` だけ。** gh 自身が「このトークンは通らない」と
/// 言っているものを probe の候補にしない。`timeout`（ホストに届かなかった）を
/// `error` と同じに畳んでよいかは `auth::resolve` のタスクで決める（04）。
fn merge_logins(status: WireStatus) -> Vec<GhLogin> {
    let mut merged: Vec<GhLogin> = Vec::new();
    for (host, entries) in status.hosts {
        for entry in entries {
            if entry.state != "success" {
                continue;
            }
            let scopes = entry
                .scopes
                .as_deref()
                .unwrap_or("")
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            match merged
                .iter_mut()
                .find(|l| l.host == host && l.login == entry.login)
            {
                // どちらかが active なら active。スコープは先に見たほうを残す
                // （同じアカウントなので同じはず。違っても片方を選ぶしかない）。
                Some(existing) => existing.active |= entry.active,
                None => merged.push(GhLogin {
                    host: host.clone(),
                    login: entry.login,
                    active: entry.active,
                    scopes,
                }),
            }
        }
    }
    // `HashMap` の反復順は不定なので、呼ぶ側とテストのために並べる。
    merged.sort_by(|a, b| (&a.host, &a.login).cmp(&(&b.host, &b.login)));
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logins(json: &str) -> Vec<GhLogin> {
        merge_logins(serde_json::from_str(json).expect("JSON"))
    }

    /// 実行環境の gh 設定から切り離した `GhCli`。
    ///
    /// **`ssh` のテストが `-F` で `/etc/ssh/ssh_config` ごと無視するのと同じ狙い。**
    /// `gh` は設定ディレクトリを環境変数でしか受けないので `GH_CONFIG_DIR` を使い、
    /// **トークンの環境変数も空で上書きする** —— `GH_TOKEN` / `GITHUB_TOKEN` が
    /// 効いていると、隔離したはずの config に無いログインが 1 件生える。
    fn isolated(config_dir: &std::path::Path) -> GhCli {
        GhCli::default().with_env([
            ("GH_CONFIG_DIR", config_dir.as_os_str()),
            ("GH_TOKEN", "".as_ref()),
            ("GITHUB_TOKEN", "".as_ref()),
            ("GH_ENTERPRISE_TOKEN", "".as_ref()),
        ])
    }

    #[tokio::test]
    async fn detect_accepts_the_gh_on_this_machine() {
        // 実物を叩く。CI にも開発機にも `gh` は入っている前提（GitHub 連携が
        // 動く環境でしか意味の無いモジュールなので、無い環境は想定しない）。
        GhCli::detect()
            .await
            .expect("gh が 2.81.0 以上で入っていること");
    }

    #[tokio::test]
    async fn auth_status_on_an_empty_config_is_not_an_error() {
        // ログインが 0 件なのは失敗ではない。`NotLoggedIn` に畳むのは `resolve` の仕事。
        // **`--json` を付けると認証に問題があっても exit 0** で返る（04）ので、
        // 素の `gh auth status` の exit 1 に引きずられない。
        let dir = tempfile::tempdir().expect("一時ディレクトリ");
        let logins = isolated(dir.path())
            .auth_status()
            .await
            .expect("空の config でも Ok");
        assert_eq!(logins, Vec::new());
    }

    #[tokio::test]
    async fn a_failing_gh_carries_its_argv_and_stderr() {
        // 不変ルール 4 と同じ扱い。握りつぶさず UI まで届ける。
        let dir = tempfile::tempdir().expect("一時ディレクトリ");
        let err = isolated(dir.path())
            .token("github.com", "nobody")
            .await
            .expect_err("ログインしていないので失敗する");
        let AuthError::GhFailed { argv, stderr } = err else {
            panic!("GhFailed ではない: {err:?}");
        };
        assert_eq!(
            argv,
            vec![
                "gh",
                "auth",
                "token",
                "--hostname",
                "github.com",
                "--user",
                "nobody"
            ]
        );
        assert!(!stderr.is_empty(), "stderr を落とさない");
    }

    #[test]
    fn reads_the_version_line() {
        // 実機の `gh --version` の 1 行目。
        assert_eq!(
            parse_version("gh version 2.100.0 (2026-09-03)"),
            Some(Version(2, 100, 0))
        );
        // パッチが無い形も受ける。
        assert_eq!(parse_version("gh version 2.81"), Some(Version(2, 81, 0)));
    }

    #[test]
    fn rejects_lines_that_are_not_a_version() {
        for line in ["", "gh version", "git version 2.55.0", "gh version x.y.z"] {
            assert_eq!(parse_version(line), None, "{line:?} は読めないはず");
        }
    }

    #[test]
    fn the_required_version_is_the_one_that_added_json_output() {
        // 2.81.0 で `gh auth status --json` が入った（04）。境界を固定する。
        assert!(Version(2, 81, 0) >= REQUIRED);
        assert!(Version(2, 80, 9) < REQUIRED);
        // 数字で比べる。文字列だと "2.100" < "2.81" になる。
        assert!(Version(2, 100, 0) >= REQUIRED);
    }

    #[test]
    fn the_same_login_from_two_sources_is_one_account() {
        // 実機の形。`GITHUB_TOKEN`（active）と `keyring`（非 active）が並ぶ。
        let merged = logins(
            r#"{"hosts":{"github.com":[
                {"state":"success","active":true,"host":"github.com","login":"me",
                 "tokenSource":"GITHUB_TOKEN","scopes":"gist, repo","gitProtocol":"https"},
                {"state":"success","active":false,"host":"github.com","login":"me",
                 "tokenSource":"keyring","scopes":"gist, repo","gitProtocol":"https"}
            ]}}"#,
        );
        assert_eq!(merged.len(), 1, "同じ login は 1 件に畳む: {merged:?}");
        assert!(merged[0].active, "どちらかが active なら active");
        assert_eq!(merged[0].scopes, vec!["gist", "repo"]);
    }

    #[test]
    fn the_active_flag_survives_whichever_order_the_entries_come_in() {
        // 上の裏返し。非 active が先に来ても落とさない。
        let merged = logins(
            r#"{"hosts":{"github.com":[
                {"state":"success","active":false,"host":"github.com","login":"me",
                 "tokenSource":"keyring","gitProtocol":"https"},
                {"state":"success","active":true,"host":"github.com","login":"me",
                 "tokenSource":"GITHUB_TOKEN","gitProtocol":"https"}
            ]}}"#,
        );
        assert_eq!(merged.len(), 1);
        assert!(merged[0].active);
    }

    #[test]
    fn different_logins_stay_separate() {
        let merged = logins(
            r#"{"hosts":{"github.com":[
                {"state":"success","active":true,"host":"github.com","login":"b",
                 "tokenSource":"keyring","gitProtocol":"https"},
                {"state":"success","active":false,"host":"github.com","login":"a",
                 "tokenSource":"keyring","gitProtocol":"https"}
            ]}}"#,
        );
        let names: Vec<&str> = merged.iter().map(|l| l.login.as_str()).collect();
        assert_eq!(names, vec!["a", "b"], "並べて返す（HashMap の順は不定）");
    }

    #[test]
    fn a_fine_grained_pat_has_no_scopes_field_at_all() {
        // `scopes` は omitempty。欠けても落ちず、空で返す（04 の EMU の節）。
        let merged = logins(
            r#"{"hosts":{"github.com":[
                {"state":"success","active":true,"host":"github.com","login":"me",
                 "tokenSource":"keyring","gitProtocol":"https"}
            ]}}"#,
        );
        assert_eq!(merged[0].scopes, Vec::<String>::new());
    }

    #[test]
    fn entries_that_gh_could_not_verify_are_not_candidates() {
        // `error` も `timeout` も落とす。gh 自身が通らないと言っているトークンを
        // probe の候補にしない。
        let merged = logins(
            r#"{"hosts":{"github.com":[
                {"state":"error","active":true,"host":"github.com","login":"bad",
                 "error":"authentication failed","tokenSource":"keyring","gitProtocol":"https"},
                {"state":"timeout","active":false,"host":"github.com","login":"slow",
                 "tokenSource":"keyring","gitProtocol":"https"},
                {"state":"success","active":false,"host":"github.com","login":"ok",
                 "tokenSource":"keyring","gitProtocol":"https"}
            ]}}"#,
        );
        let names: Vec<&str> = merged.iter().map(|l| l.login.as_str()).collect();
        assert_eq!(names, vec!["ok"]);
    }

    #[test]
    fn several_hosts_come_back_together() {
        let merged = logins(
            r#"{"hosts":{
                "github.com":[{"state":"success","active":true,"host":"github.com",
                 "login":"me","tokenSource":"keyring","gitProtocol":"https"}],
                "acme.ghe.com":[{"state":"success","active":true,"host":"acme.ghe.com",
                 "login":"me","tokenSource":"keyring","gitProtocol":"https"}]
            }}"#,
        );
        let hosts: Vec<&str> = merged.iter().map(|l| l.host.as_str()).collect();
        assert_eq!(hosts, vec!["acme.ghe.com", "github.com"]);
    }

    #[test]
    fn no_hosts_is_not_an_error() {
        // ログインしていない環境。`NotLoggedIn` に畳むのは `resolve` の仕事。
        assert_eq!(logins(r#"{"hosts":{}}"#), Vec::<GhLogin>::new());
    }
}
