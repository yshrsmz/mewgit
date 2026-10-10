//! `gh` CLI の呼び出し。設計は `docs/design/04-auth-account.md` の「gh との連携」。
//!
//! **出力は JSON しか読まない。** `gh auth status --json hosts` は [`REQUIRED`] で入った。
//! それ未満は [`AuthError::GhTooOld`] に落とし、テキスト出力は解析しない（04 に理由）。
//! 例外は [`GhCli::browse`] で、あれは**出力を読まない**（ブラウザを開くだけ）。

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

use secrecy::SecretString;
use serde::Deserialize;

use super::probe::{self, ProbeError};
use super::{split_scopes, AuthError, GhLogin, LoginState};

/// `gh auth status --json` と `gh auth token --user` が揃う最低版。
///
/// `ui` の案内文もここを読む（[`AuthError::GhTooOld`] は要求版を値として持たない）。
pub const REQUIRED: &str = "2.81.0";

/// `gh` があり、[`REQUIRED`] を満たすことの証。
///
/// **作れる経路は [`detect`](Self::detect) だけ。** `Default` も公開の
/// コンストラクタも置かないので、「値がある = 検査済み」が構築経路の数と一致する。
#[derive(Debug)]
pub struct GhCli {
    /// `config` の `general.gh_bin`。**直書きしない** —— #21 が `Runner` に
    /// `git_bin` を戻したのと同じ理由で、設定・既定値・`MEWGIT_GH_BIN` の上書きと
    /// そのテストが実装に到達しなくなる。渡し忘れを作らないよう引数で受ける。
    bin: PathBuf,
    /// テストが実行環境から切り離すための上書き。
    env: Vec<(OsString, OsString)>,
}

impl GhCli {
    /// `gh --version` を叩いて、存在と最低版を確かめる。
    ///
    /// `bin` には `config.general.gh_bin` を渡す。
    pub async fn detect(bin: impl Into<PathBuf>) -> Result<Self, AuthError> {
        Self::detect_with_env(bin, Vec::new()).await
    }

    /// [`detect`](Self::detect) に環境変数の上書きを添える。
    ///
    /// **テストが `GH_CONFIG_DIR` で実行環境から切り離すために要る。** `gh` は
    /// 設定ディレクトリを環境変数でしか受けず（`gh help environment`）、
    /// `ssh` の `-F` に当たる argv が無い。公開しないのは、production から
    /// 使う場面が無いため。
    async fn detect_with_env(
        bin: impl Into<PathBuf>,
        env: Vec<(OsString, OsString)>,
    ) -> Result<Self, AuthError> {
        let cli = GhCli {
            bin: bin.into(),
            env,
        };
        check_version(cli.run(&["--version"]).await?.lines().next().unwrap_or(""))?;
        Ok(cli)
    }

    /// そのホストのログイン一覧。
    ///
    /// **`--hostname` で絞る。** 付けないと gh は**全ホストのトークンを検証**し、
    /// 到達できないホストが 1 つ混ざっているだけで待ち時間がそれに引きずられる
    /// （実測: 絞ると 0.24 秒、絞らないと **30.2 秒**）。04 の手順 2 が
    /// 「`auth_status()` を `target.host` で絞る」と言っている、その絞り込みを
    /// 結果ではなく argv でやる。**config に無いホストでも exit 0 で
    /// `{"hosts":{}}` が返る**ので、「0 件なら `NotLoggedIn`」の経路は変わらない。
    ///
    /// **`--show-token` は付けない**（付けると JSON に生のトークンが入る。不変ルール 3）。
    pub async fn auth_status(&self, host: &str) -> Result<Vec<GhLogin>, AuthError> {
        let out = self
            .run(&["auth", "status", "--hostname", host, "--json", "hosts"])
            .await?;
        let status: WireStatus =
            serde_json::from_str(&out).map_err(|e| AuthError::GhUnparsable {
                message: e.to_string(),
            })?;
        Ok(merge_logins(status))
    }

    /// 指定したアカウントのトークン。
    pub async fn token(&self, host: &str, user: &str) -> Result<SecretString, AuthError> {
        let mut out = self
            .run(&["auth", "token", "--hostname", host, "--user", user])
            .await?;
        // 末尾を切り詰めて `String` ごと渡す。`trim()` の `&str` を渡すと、
        // トークンを載せた平のバッファがもう 1 本増える。
        out.truncate(out.trim_end().len());
        Ok(SecretString::from(out))
    }

    /// PR をブラウザで開く（01 の「`o`（ブラウザで開く）」）。
    ///
    /// **URL は渡せない。** `gh browse` の引数は `<number> | <path> | <commit-sha>` だけ
    /// （gh 2.100.0 の `--help`）なので、`PullRequest.url` があっても番号で頼む。
    /// 出すのは issue の URL（実測: `gh browse -n 123 -R github.com/cli/cli` →
    /// `https://github.com/cli/cli/issues/123`）だが、GitHub がそれを `302` で
    /// `/pull/123` に飛ばす（2026-09-16 に `curl -sI` で実測。**GHES では
    /// 確かめていない**）。
    ///
    /// **`-R` にホストを載せる。** `[HOST/]OWNER/REPO` の形で、載せないと gh は
    /// カレントディレクトリの git リポジトリを探す（実測: リポジトリの外では
    /// `unable to determine base repository`）。
    ///
    /// **`--` は置かない。** `gh browse` は `-R` の値も positional もオプションとして
    /// 読み直さないが、**それを守りだと書かない**ためにそもそも置かない
    /// （#73 で `git config` の `--` が何も守っていなかったのと同じ形）。ここへ来る
    /// `host` / `owner` / `repo` は [`RepoTarget`](super::RepoTarget) の値で、
    /// `auth::url::valid_host` を通っている。
    ///
    /// **どのブラウザを起動するかは gh が決める**（`GH_BROWSER` / `BROWSER`。
    /// `gh help environment`）。mewgit は端末を渡さない。
    pub async fn browse(
        &self,
        host: &str,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<(), AuthError> {
        let repo_flag = format!("{host}/{owner}/{repo}");
        let number = number.to_string();
        self.run(&["browse", &number, "-R", &repo_flag]).await?;
        Ok(())
    }

    /// `gh` を起動して stdout を返す。
    ///
    /// argv にトークンは載らない（[`token`](Self::token) が受けるのは host と login だけ）。
    async fn run(&self, args: &[&str]) -> Result<String, AuthError> {
        probe::output(&self.bin, args, &self.env)
            .await
            .map_err(|err| match &err {
                // 「PATH に無い」だけを `GhMissing` にする。実行権限が無い場合まで
                // 畳むと、「gh が見つかりませんでした」と案内して探させることになる。
                ProbeError::Spawn {
                    kind: io::ErrorKind::NotFound,
                    ..
                } => AuthError::GhMissing {
                    bin: self.bin.to_string_lossy().into_owned(),
                },
                ProbeError::Spawn { .. } => AuthError::GhUnrunnable {
                    message: err.describe(),
                },
                ProbeError::Failed { .. } => AuthError::GhFailed {
                    argv: std::iter::once(self.bin.to_string_lossy().into_owned())
                        .chain(args.iter().map(|a| (*a).to_string()))
                        .collect(),
                    message: err.describe(),
                },
            })
    }
}

/// `gh --version` の 1 行目が [`REQUIRED`] を満たすか。
///
/// **起動と分けてある。** 一緒にすると、古い gh を持っていない限り
/// [`AuthError::GhTooOld`] の腕を踏めない。
fn check_version(first_line: &str) -> Result<(), AuthError> {
    let line = first_line.trim();
    let found = parse_version(line);
    // 版を読めないのは「古すぎる」ではなく「知らない形」だが、**通す理由が無い**
    // ので同じ扱いにする。案内には生の 1 行目を出して、何が来たか判るようにする。
    match found {
        Some(v) if v >= required() => Ok(()),
        _ => Err(AuthError::GhTooOld {
            found: found.map_or_else(|| line.to_string(), |v| v.to_string()),
        }),
    }
}

fn required() -> Version {
    parse_number(REQUIRED).expect("REQUIRED は版として書いてある")
}

/// `major.minor.patch`。比較にしか使わないので `semver` は足さない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version(u32, u32, u32);

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// `gh version 2.100.0 (2026-09-03)` から版を取る。
fn parse_version(line: &str) -> Option<Version> {
    parse_number(
        line.strip_prefix("gh version ")?
            .split_whitespace()
            .next()?,
    )
}

/// `2.100.0` を割る。
///
/// **3 つ揃っていることは求める** —— `2.81` のような形を gh が出す実例は見ていないし、
/// 2 つだけだと「パッチが 0」なのか「読み違い」なのか決められない。
///
/// **一方、パッチのぶら下がり（`2.85.0+dfsg1`、`2.90.0-rc.1`）は許す。** こちらも
/// 実例は見ていないが、**外したときの害が非対称**。厳しくして外すと、十分新しい gh で
/// GitHub 機能を丸ごと失う。緩めて外しても、比較に使う 3 つの数は変わらない。
fn parse_number(number: &str) -> Option<Version> {
    let mut parts = number.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?;
    let digits = patch.len() - patch.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let version = Version(major, minor, patch[..digits].parse().ok()?);
    // **パッチが数字だけで終わっているのに 4 つ目が続く形（`2.81.0.1`）は受けない。**
    // 知らない採番なので、3 つ目までを版として読んでよい根拠が無い。ぶら下がりが
    // 付いていれば（`2.85.0-rc.1`）そこから先は接尾辞なので、`.` で割れていても数えない。
    if digits == patch.len() && parts.next().is_some() {
        return None;
    }
    Some(version)
}

#[derive(Debug, Deserialize)]
struct WireStatus {
    /// ホストをキーにしたオブジェクト。値はそのホストのエントリの配列。
    hosts: BTreeMap<String, Vec<WireEntry>>,
}

#[derive(Debug, Deserialize)]
struct WireEntry {
    /// `success` / `timeout` / `error`（v2.100.0 の `status.go` で確認）。
    state: String,
    active: bool,
    login: String,
    /// トークンの出どころ。設定に保存したものは `keyring` / `oauth_token`
    /// （hosts.yml）で、ほかは環境変数の名前（gh の `internal/config/config.go`）。
    #[serde(rename = "tokenSource")]
    token_source: Option<String>,
    /// カンマ区切りの 1 本の文字列。**fine-grained PAT では丸ごと欠ける**ので
    /// `Option`（`omitempty`）。
    scopes: Option<String>,
}

impl WireEntry {
    fn state(&self) -> LoginState {
        match self.state.as_str() {
            "success" => LoginState::Success,
            "timeout" => LoginState::Timeout,
            // `error` と、gh が今後足す状態。**未知を成功にしない。**
            _ => LoginState::Error,
        }
    }

    /// 設定に保存したトークンか。**知らない出どころは保存していない側に数える** ——
    /// そうすると案内が `gh auth switch` を足す側に倒れ、別のアカウントに
    /// スコープを足させない。
    fn is_stored(&self) -> bool {
        matches!(
            self.token_source.as_deref(),
            Some("keyring" | "oauth_token")
        )
    }
}

/// エントリを [`GhLogin`] に畳む。
///
/// **`(host, login)` で 1 件にまとめる。** 同じ login が複数のエントリで出る
/// （実機で `GITHUB_TOKEN` と `keyring` の 2 つが並んだ）。`GhLogin` は
/// トークンの出どころを持たないので**見分けられない重複**になり、`resolve` の
/// 「1 件なら `Single`」が誤爆する。`token(host, user)` は login で引くので、
/// 畳んでも取り出せるトークンは変わらない。
///
/// 畳むときの規則は 2 つ。
///
/// - **状態は「一番良かったもの」を採る。** どれか 1 つでも通るなら、その login は
///   probe の候補になりうる。
/// - **スコープは `Success` なエントリのものを採る。** 出どころが違えばスコープも
///   違いうる（`GH_TOKEN` と keyring で別のトークンなので）。**同じだとは
///   確かめていない**ので、先に見たほうを残すのではなく「通ったほう」を選ぶ。
fn merge_logins(status: WireStatus) -> Vec<GhLogin> {
    let mut merged: BTreeMap<(String, String), Slot> = BTreeMap::new();
    for (host, entries) in status.hosts {
        for entry in entries {
            // **login の無いエントリは落とす。** 期限切れの `GH_TOKEN` を export した
            // ままだと `{"state":"error","active":true,"login":""}` が本物のログインの
            // 隣に並ぶ（実測）。`token(host, user)` は login で引くので**取り出す術が
            // 無く**、候補として数えると 04 の手順 3「1 件なら `Single`」が誤爆する。
            // `state` の絞り込みと違って、これは `resolve` の方針では変わらない
            // —— どの方針でも使えないため。
            if entry.login.is_empty() {
                continue;
            }
            let state = entry.state();
            // `entry.login` を下の閉包が持っていくので、先に読む。
            let stored = entry.is_stored();
            let slot = merged
                .entry((host.clone(), entry.login.clone()))
                .or_insert_with(|| Slot {
                    login: GhLogin {
                        host: host.clone(),
                        login: entry.login,
                        active: false,
                        stored_active: false,
                        scopes: Vec::new(),
                        // 最初の 1 件で上書きされるか、この値のまま残る。
                        state: LoginState::Error,
                    },
                    scopes_taken: false,
                });
            slot.login.active |= entry.active;
            slot.login.stored_active |= entry.active && stored;
            if better(state, slot.login.state) {
                slot.login.state = state;
            }
            // **`scopes.is_empty()` を「まだ埋めていない」の印にしない。** fine-grained
            // PAT は本当にスコープが空なので（04 の EMU の節）、印にすると
            // 「空だった 1 件目」を「未取得」と読んで、**別のトークンのスコープを
            // 混ぜ込む**。混ざると「足りないスコープ」の案内が嘘になる。
            if state == LoginState::Success && !slot.scopes_taken {
                slot.login.scopes = split_scopes(entry.scopes.as_deref().unwrap_or(""));
                slot.scopes_taken = true;
            }
        }
    }
    merged.into_values().map(|slot| slot.login).collect()
}

/// 畳んでいる途中の 1 件。`scopes_taken` は「`Success` なエントリから取ったか」で、
/// **「空かどうか」とは別の問い**。
struct Slot {
    login: GhLogin,
    scopes_taken: bool,
}

/// `a` のほうが望ましい状態か。`Success` > `Timeout` > `Error`。
///
/// `Timeout` を `Error` より上に置くのは、**届かなかっただけでトークンが悪いとは
/// 限らない**ため。**`resolve` はどちらでも候補を落とさない**と決めたので
/// （04 の「`LoginState` で候補を絞らない」）、この順序が効くのは表示だけ。
fn better(a: LoginState, b: LoginState) -> bool {
    fn rank(s: LoginState) -> u8 {
        match s {
            LoginState::Success => 2,
            LoginState::Timeout => 1,
            LoginState::Error => 0,
        }
    }
    rank(a) > rank(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn logins(json: &str) -> Vec<GhLogin> {
        merge_logins(serde_json::from_str(json).expect("JSON"))
    }

    /// 実行環境の gh 設定から切り離した `GhCli`。
    ///
    /// **`ssh` のテストが `-F` で `/etc/ssh/ssh_config` ごと無視するのと同じ狙い。**
    /// `gh` は設定ディレクトリを環境変数でしか受けないので `GH_CONFIG_DIR` を使い、
    /// **トークンの環境変数も空で上書きする** —— `GH_TOKEN` / `GITHUB_TOKEN` が
    /// 効いていると、隔離したはずの config に無いログインが 1 件生える。
    ///
    /// `TempDir` は返り値で持つ。テストごとに作ると「束縛し続ける」注意書きが
    /// 人数分に増える（`ssh` の `resolve` ヘルパと同じ形）。
    async fn isolated() -> (tempfile::TempDir, GhCli) {
        let dir = tempfile::tempdir().expect("一時ディレクトリ");
        let cli = GhCli::detect_with_env("gh", env_for(dir.path()))
            .await
            .expect("gh が入っていること");
        (dir, cli)
    }

    fn env_for(config_dir: &Path) -> Vec<(OsString, OsString)> {
        ["GH_TOKEN", "GITHUB_TOKEN", "GH_ENTERPRISE_TOKEN"]
            .into_iter()
            .map(|k| (OsString::from(k), OsString::new()))
            .chain(std::iter::once((
                OsString::from("GH_CONFIG_DIR"),
                config_dir.as_os_str().to_owned(),
            )))
            .collect()
    }

    // ---- 版 ----

    #[test]
    fn reads_the_version_line() {
        // 実機の `gh --version` の 1 行目。
        assert_eq!(
            parse_version("gh version 2.100.0 (2026-09-03)"),
            Some(Version(2, 100, 0))
        );
    }

    #[test]
    fn rejects_lines_that_are_not_a_version() {
        for line in [
            "",
            "gh version",
            "git version 2.55.0",
            "gh version x.y.z",
            // 3 つ揃っていない形は受けない（gh が出す実例を見ていない）。
            "gh version 2.81",
            "gh version 2.81.0.1",
            "gh version .81.0",
        ] {
            assert_eq!(parse_version(line), None, "{line:?} は読めないはず");
        }
    }

    #[test]
    fn a_patch_with_packaging_junk_still_reads() {
        // 実例は見ていないが、外したときの害が非対称。厳しくして外すと、十分新しい
        // gh で GitHub 機能を丸ごと失う。
        for line in [
            "gh version 2.85.0+dfsg1",
            "gh version 2.85.0-rc.1 (2026-01-01)",
        ] {
            assert_eq!(
                parse_version(line),
                Some(Version(2, 85, 0)),
                "{line:?} は 2.85.0 として読む"
            );
        }
    }

    #[test]
    fn the_required_version_is_the_one_that_added_json_output() {
        // 2.81.0 で `gh auth status --json` が入った（04）。境界を固定する。
        assert_eq!(required(), Version(2, 81, 0));
        // 数字で比べる。文字列だと "2.100" < "2.81" になる。
        assert!(Version(2, 100, 0) >= required());
    }

    #[test]
    fn a_gh_older_than_required_is_reported_with_its_version() {
        let err =
            check_version("gh version 2.80.9 (2025-09-01)").expect_err("2.81.0 未満なので断る");
        assert_eq!(
            err,
            AuthError::GhTooOld {
                found: "2.80.9".to_string()
            }
        );
        // 案内には要求版が出る。
        assert!(err.to_string().contains(REQUIRED), "{err}");
    }

    #[test]
    fn a_version_line_we_cannot_read_keeps_the_raw_line() {
        // 何が来たか判らないと直しようが無いので、生の 1 行目を案内に載せる。
        let err = check_version("  gh version wat  ").expect_err("読めないので断る");
        assert_eq!(
            err,
            AuthError::GhTooOld {
                found: "gh version wat".to_string()
            }
        );
    }

    // ---- 実物の gh ----

    #[tokio::test]
    async fn detect_accepts_the_gh_on_this_machine() {
        GhCli::detect("gh")
            .await
            .expect("gh が 2.81.0 以上で入っていること");
    }

    #[tokio::test]
    async fn the_configured_gh_binary_is_the_one_that_runs() {
        // `config` の `general.gh_bin` を渡し忘れていないことの見張り。直書きに
        // 戻すと、ここで本物の gh が走って成功してしまう（#21 の
        // `the_configured_git_binary_is_the_one_that_runs` と同じ形）。
        let dir = tempfile::tempdir().expect("一時ディレクトリ");
        let path = dir.path().join("no-such-gh");
        let err = GhCli::detect(&path)
            .await
            .expect_err("存在しないバイナリなので失敗する");
        let AuthError::GhMissing { bin } = &err else {
            panic!("GhMissing ではない: {err:?}");
        };
        // **パスを案内に出す。** `general.gh_bin` の書き間違いでもここに来るので、
        // 「gh が見つかりませんでした」だけだと PATH を探させてしまう。
        assert_eq!(bin, &path.to_string_lossy());
        assert!(err.to_string().contains("no-such-gh"), "{err}");
    }

    #[tokio::test]
    async fn a_gh_that_cannot_be_executed_is_not_reported_as_missing() {
        // 実行権限の無いファイル。「見つかりませんでした」と案内すると探させてしまう。
        let dir = tempfile::tempdir().expect("一時ディレクトリ");
        let path = dir.path().join("gh");
        std::fs::write(&path, b"#!/bin/sh\n").expect("書ける");
        let err = GhCli::detect(&path)
            .await
            .expect_err("実行できないので失敗する");
        let AuthError::GhUnrunnable { message } = err else {
            panic!("GhUnrunnable ではない: {err:?}");
        };
        assert!(message.contains("起動できませんでした"), "{message}");
    }

    #[tokio::test]
    async fn auth_status_on_an_empty_config_is_not_an_error() {
        // ログインが 0 件なのは失敗ではない。`NotLoggedIn` に畳むのは `resolve` の仕事。
        // **`--json` を付けると認証に問題があっても exit 0** で返る（04）ので、
        // 素の `gh auth status` の exit 1 に引きずられない。
        let (_dir, cli) = isolated().await;
        let logins = cli
            .auth_status("github.com")
            .await
            .expect("空の config でも Ok");
        assert_eq!(logins, Vec::new());
    }

    #[tokio::test]
    async fn a_failing_gh_carries_its_argv_and_the_reason() {
        // 不変ルール 4 と同じ扱い。握りつぶさず UI まで届ける。
        let (_dir, cli) = isolated().await;
        let err = cli
            .token("github.com", "nobody")
            .await
            .expect_err("ログインしていないので失敗する");
        let AuthError::GhFailed { argv, message } = err else {
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
        // 終了コードか stderr、少なくとも片方は残る。
        assert!(!message.is_empty(), "理由を落とさない");
    }

    // ---- 一覧の畳み込み ----

    #[test]
    fn the_same_login_from_two_sources_is_one_account() {
        // 実機の形。`GITHUB_TOKEN`（active）と `keyring`（非 active）が並ぶ。
        // 順序を入れ替えても同じ結果になること（`active` を落とさない）も見る。
        let pair = [
            r#"{"state":"success","active":true,"host":"github.com","login":"me",
                "tokenSource":"GITHUB_TOKEN","scopes":"gist, repo","gitProtocol":"https"}"#,
            r#"{"state":"success","active":false,"host":"github.com","login":"me",
                "tokenSource":"keyring","scopes":"gist, repo","gitProtocol":"https"}"#,
        ];
        for order in [[0, 1], [1, 0]] {
            let merged = logins(&format!(
                r#"{{"hosts":{{"github.com":[{},{}]}}}}"#,
                pair[order[0]], pair[order[1]]
            ));
            assert_eq!(merged.len(), 1, "同じ login は 1 件に畳む: {merged:?}");
            assert!(merged[0].active, "どちらかが active なら active");
            // **設定上は非 active。** 環境変数のエントリが active でも、`gh auth
            // refresh` が相手にするのは keyring のほう（`GhLogin::stored_active`）。
            assert!(
                !merged[0].stored_active,
                "環境変数の active を設定上の active にしない"
            );
            assert_eq!(merged[0].scopes, vec!["gist", "repo"]);
        }
    }

    #[test]
    fn only_a_stored_token_makes_the_login_active_in_gh_config() {
        // `gh auth refresh` / `switch` が相手にするのは設定上のアカウント。
        // **知らない出どころは保存していない側**（案内が `switch` を足す側に倒れる）。
        let merged = logins(
            r#"{"hosts":{"github.com":[
                {"state":"success","active":true,"login":"kr","tokenSource":"keyring"},
                {"state":"success","active":true,"login":"yml","tokenSource":"oauth_token"},
                {"state":"success","active":true,"login":"env","tokenSource":"GH_ENTERPRISE_TOKEN"},
                {"state":"success","active":true,"login":"none"},
                {"state":"success","active":false,"login":"off","tokenSource":"keyring"}
            ]}}"#,
        );
        let stored: Vec<(&str, bool)> = merged
            .iter()
            .map(|l| (l.login.as_str(), l.stored_active))
            .collect();
        for want in [
            ("kr", true),
            ("yml", true),
            ("env", false),
            ("none", false),
            ("off", false),
        ] {
            assert!(stored.contains(&want), "{want:?}: {stored:?}");
        }
    }

    #[test]
    fn a_login_that_gh_could_not_verify_is_still_reported() {
        // **落とさずに運ぶ。** `Timeout` を `Error` と同じに畳んでよいかは
        // `resolve` のタスクで決めるので、その材料をここで捨てない（04）。
        let merged = logins(
            r#"{"hosts":{"github.com":[
                {"state":"error","active":true,"login":"bad"},
                {"state":"timeout","active":false,"login":"slow"},
                {"state":"success","active":false,"login":"ok"}
            ]}}"#,
        );
        let seen: Vec<(&str, LoginState)> =
            merged.iter().map(|l| (l.login.as_str(), l.state)).collect();
        assert_eq!(
            seen,
            vec![
                ("bad", LoginState::Error),
                ("ok", LoginState::Success),
                ("slow", LoginState::Timeout),
            ]
        );
    }

    #[test]
    fn the_best_state_wins_and_its_scopes_come_with_it() {
        // 出どころが違えばスコープも違いうる。通ったほうのスコープを採る。
        let merged = logins(
            r#"{"hosts":{"github.com":[
                {"state":"error","active":true,"login":"me","scopes":"stale"},
                {"state":"success","active":false,"login":"me","scopes":"gist, repo"}
            ]}}"#,
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].state, LoginState::Success);
        assert_eq!(merged[0].scopes, vec!["gist", "repo"]);
    }

    #[test]
    fn an_entry_without_a_login_is_not_a_candidate() {
        // 期限切れの `GH_TOKEN` を export したままだと、本物のログインの隣に
        // login の無いエントリが並ぶ（実測）。`token(host, user)` は login で引くので
        // 取り出す術が無く、数えると「1 件なら Single」が誤爆する。
        let merged = logins(
            r#"{"hosts":{"github.com":[
                {"state":"error","active":true,"login":"","tokenSource":"GH_TOKEN"},
                {"state":"success","active":false,"login":"me","tokenSource":"keyring"}
            ]}}"#,
        );
        let names: Vec<&str> = merged.iter().map(|l| l.login.as_str()).collect();
        assert_eq!(names, vec!["me"], "名前の無い候補は数えない");
    }

    #[test]
    fn scopes_do_not_migrate_from_another_token_source() {
        // 1 件目が fine-grained PAT（スコープが本当に空）で、2 件目が別の
        // トークン。空を「未取得」の印にすると、持っていないスコープを報告する。
        let merged = logins(
            r#"{"hosts":{"github.com":[
                {"state":"success","active":true,"login":"me"},
                {"state":"success","active":false,"login":"me","scopes":"repo, gist"}
            ]}}"#,
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].scopes,
            Vec::<String>::new(),
            "1 件目の「本当に空」を 2 件目で埋めない"
        );
    }

    #[test]
    fn several_hosts_come_back_together() {
        let merged = logins(
            r#"{"hosts":{
                "github.com":[{"state":"success","active":true,"login":"me"}],
                "acme.ghe.com":[{"state":"success","active":true,"login":"me"}]
            }}"#,
        );
        let hosts: Vec<&str> = merged.iter().map(|l| l.host.as_str()).collect();
        assert_eq!(hosts, vec!["acme.ghe.com", "github.com"]);
    }
}
