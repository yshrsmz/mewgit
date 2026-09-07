//! SSH エイリアスの正規化。設計は `docs/design/04-auth-account.md` の
//! 「SSH エイリアスの正規化」。
//!
//! `.git/config` のリモート URL に書かれたホストは `~/.ssh/config` のエイリアス
//! （`gh-work` など）のことがある。`ssh -G` に解決させて実ホスト名にする。

use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

use super::AuthError;

/// 実行する ssh。`git_bin` / `gh_bin` と違って設定で差し替えられない。
///
/// ssh を呼ぶのはホスト名の正規化だけで、Git の通信自体は `git` が
/// `core.sshCommand` / `GIT_SSH_COMMAND` に従って行う（03）。ここに設定を足すと、
/// 「どちらの ssh で解決したか」と「どちらの ssh で繋いだか」が食い違いうる。
const SSH_BIN: &str = "ssh";

/// エイリアスを実ホスト名に正規化する。
///
/// **`ssh -G` は未定義のエイリアスでも exit 0 で引数をそのまま返す**（04 の実測）。
/// つまり「エイリアスが無かった」は成功であって、`Err` になるのは ssh を起動できな
/// かったときと、ssh が非 0 で終了したときだけ。呼び出し側は `Err` のときに
/// `host_raw` をそのまま使い、理由を通知に出す。
pub async fn resolve_ssh_host(alias: &str) -> Result<String, AuthError> {
    resolve_with(Path::new(SSH_BIN), None, alias).await
}

/// `ssh -G` を実行して `hostname` の値を返す。
///
/// `config_file` は `-F` に渡す ssh_config。**テストを実行環境から切り離すためだけに
/// あり**、本番は `None` で `~/.ssh/config` と `/etc/ssh/ssh_config` をそのまま使う。
async fn resolve_with(
    ssh_bin: &Path,
    config_file: Option<&Path>,
    alias: &str,
) -> Result<String, AuthError> {
    let failed = |message: String| AuthError::SshResolveFailed {
        alias: alias.to_string(),
        message,
    };

    let mut cmd = Command::new(ssh_bin);
    if let Some(path) = config_file {
        cmd.arg("-F").arg(path);
    }
    cmd.arg("-G")
        // **`alias` は positional に載る。** `.git/config` の URL は clone してきた
        // 値がそのまま入るので、`-oProxyCommand=...` をオプションとして解釈させない
        // （04 の「alias は argv の positional に載る」）。
        .arg("--")
        .arg(alias)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // エラー文を安定させる（`git_exec::Runner` と同じ）。
        .env("LC_ALL", "C")
        .kill_on_drop(true);

    let output = cmd.output().await.map_err(|err| {
        failed(format!(
            "{} を起動できませんでした: {err}",
            ssh_bin.display()
        ))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        let code = output.status.code().map_or_else(
            || "シグナルで終了".to_string(),
            |code| format!("exit {code}"),
        );
        return Err(failed(if stderr.is_empty() {
            code
        } else {
            format!("{code}: {stderr}")
        }));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    hostname_from_dump(&stdout)
        .map(str::to_string)
        .ok_or_else(|| failed("ssh -G の出力に hostname の行がありませんでした".to_string()))
}

/// `ssh -G` の出力から `hostname` の値を取る。
///
/// **`host <alias>` の行が `hostname <実ホスト>` より先に来る**ので、`host` の prefix で
/// 見ると最初の行を取ってしまう。キーは完全一致で見る（04 の実測）。
fn hostname_from_dump(dump: &str) -> Option<&str> {
    dump.lines().find_map(|line| {
        let value = line.strip_prefix("hostname ")?.trim();
        (!value.is_empty()).then_some(value)
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;

    /// `-F` に渡す ssh_config を書く。`TempDir` は落とすと消えるので呼び出し側で持つ。
    fn ssh_config(dir: &TempDir, body: &str) -> PathBuf {
        let path = dir.path().join("config");
        let mut file = std::fs::File::create(&path).expect("ssh_config を作れなかった");
        file.write_all(body.as_bytes())
            .expect("ssh_config を書けなかった");
        path
    }

    async fn resolve(config: &Path, alias: &str) -> Result<String, AuthError> {
        resolve_with(Path::new(SSH_BIN), Some(config), alias).await
    }

    #[test]
    fn takes_the_hostname_line_not_the_host_line() {
        // 実際の並び。`host` が先に来る。
        let dump = "host gh-work\nuser git\nhostname github.com\nport 22\n";
        assert_eq!(hostname_from_dump(dump), Some("github.com"));
    }

    #[test]
    fn ignores_dumps_without_a_hostname() {
        assert_eq!(hostname_from_dump("host gh-work\nuser git\n"), None);
        // 値の無い行は無いものとして扱う（あれば次の行を見る）。
        assert_eq!(
            hostname_from_dump("hostname \nhostname a.example\n"),
            Some("a.example")
        );
    }

    #[test]
    fn trims_the_line_ending() {
        assert_eq!(
            hostname_from_dump("hostname github.com\r\n"),
            Some("github.com")
        );
    }

    #[tokio::test]
    async fn resolves_an_alias_to_its_hostname() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = ssh_config(&dir, "Host gh-work\n  HostName github.com\n  User git\n");
        assert_eq!(resolve(&config, "gh-work").await.unwrap(), "github.com");
    }

    #[tokio::test]
    async fn an_unknown_alias_resolves_to_itself() {
        // `ssh -G` は未定義のエイリアスでも exit 0 で引数をそのまま返す（04 の実測）。
        // 「解決できなかった」を `Err` にすると、エイリアスを使っていないふつうの
        // リモートが全部エラーになる。
        let dir = tempfile::tempdir().expect("tempdir");
        let config = ssh_config(&dir, "Host gh-work\n  HostName github.com\n");
        assert_eq!(
            resolve(&config, "acme.ghe.com").await.unwrap(),
            "acme.ghe.com"
        );
    }

    #[tokio::test]
    async fn an_option_like_alias_is_not_read_as_an_option() {
        // **`--` の見張り。** 外すと ssh が `-4` をオプションとして読み、ホストが
        // 無いまま usage を出して落ちる。`--` があればホスト名として不正だと言う。
        // どちらも `Err` なので、見張れるのは理由のほうだけ。
        let dir = tempfile::tempdir().expect("tempdir");
        let config = ssh_config(&dir, "Host gh-work\n  HostName github.com\n");
        let err = resolve(&config, "-4").await.unwrap_err();
        let message = err.to_string();
        assert!(
            !message.to_lowercase().contains("usage"),
            "ssh がオプションとして読んでいる: {message}"
        );
    }

    #[tokio::test]
    async fn a_broken_config_is_reported_with_the_stderr() {
        // 不変ルール 4 の趣旨。ssh が断った理由を握りつぶさない。
        let dir = tempfile::tempdir().expect("tempdir");
        let config = ssh_config(&dir, "Host gh-work\n  NoSuchKeyword yes\n");
        let err = resolve(&config, "gh-work").await.unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("nosuchkeyword") || message.contains("NoSuchKeyword"),
            "stderr が案内に入っていない: {message}"
        );
    }

    #[tokio::test]
    async fn a_missing_ssh_is_reported() {
        let err = resolve_with(Path::new("/nonexistent/ssh"), None, "gh-work")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("起動できませんでした"), "{err}");
    }
}
