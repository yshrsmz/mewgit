//! SSH エイリアスの正規化。設計は `docs/design/04-auth-account.md` の
//! 「SSH エイリアスの正規化」。
//!
//! `.git/config` のリモート URL に書かれたホストは `~/.ssh/config` のエイリアス
//! （`gh-work` など）のことがある。`ssh -G` に解決させて実ホスト名にする。

use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

use super::AuthError;

/// 何も設定されていないときに git が起動する ssh。[`effective_ssh`] の既定値。
///
/// **`resolve_ssh_host` はいまこれを直に叩いている。** git の通信は
/// `core.sshCommand` / `GIT_SSH_COMMAND` に従うので、そこを別の ssh（別の config を
/// 読むもの）に向けている環境では、**解決と接続で食い違う**。寄せ先は
/// [`effective_ssh`] で決まったが、配線は M4 —— 実効値はシェル文字列なので
/// `Command::new(ssh)` ではなく `sh -c` 経由の起動に変える必要があり、いま
/// `resolve_ssh_host` を呼ぶ経路が無い（04 の「git が使う ssh と、ここで叩く ssh」）。
const SSH_BIN: &str = "ssh";

/// git がリモートとの通信に使う ssh。
///
/// 変種で分けるのは、**「オプションを足せるか」が呼び出し側の分岐そのもの**だから
/// （`Option<String>` の `None` に「触るな」と「既定の ssh」の 2 つの意味を持たせない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshCommand {
    /// git がシェル経由で起動する文字列。後ろに引数を足せる。
    Shell(String),
    /// `GIT_SSH` が設定されている。**触らない。**
    ///
    /// `GIT_SSH` はシェルを通さず argv を直に組むので同じ形でオプションを足せず、
    /// `GIT_SSH_COMMAND` を置くと git はそちらを優先するので、重ねるつもりが
    /// 差し替えになる（03 の「BatchMode の重ね方」）。
    Opaque,
}

impl Default for SshCommand {
    /// 何も設定されていない環境で git が起動するもの。
    fn default() -> Self {
        Self::Shell(SSH_BIN.to_string())
    }
}

/// git が使う ssh を、環境変数と `core.sshCommand` から決める。
///
/// 優先順は git と同じで `GIT_SSH_COMMAND` > `core.sshCommand` > `GIT_SSH` > `ssh`
/// （前の 2 つは環境変数を置くと config が読まれないことを実測。git 2.55）。
/// **空白だけの値は未設定として扱う。**
///
/// `git_exec::Runner` が `-o BatchMode=yes` を重ねるのに使う。`resolve_ssh_host` が
/// PATH の `ssh` を直に叩いているのをここへ寄せるのは M4（04 の「git が使う ssh と、
/// ここで叩く ssh」）。
fn effective_ssh(
    git_ssh_command: Option<&str>,
    core_ssh_command: Option<&str>,
    git_ssh: Option<&str>,
) -> SshCommand {
    if let Some(command) = set(git_ssh_command).or_else(|| set(core_ssh_command)) {
        return SshCommand::Shell(command.to_string());
    }
    if set(git_ssh).is_some() {
        return SshCommand::Opaque;
    }
    SshCommand::default()
}

/// プロセスの環境変数と `core.sshCommand` から実効 ssh を決める。
///
/// **環境変数を読むのはここだけ。** [`effective_ssh`] を純粋関数のままにしておくと、
/// 実行環境に `GIT_SSH_COMMAND` が置かれていてもテストの結果が動かない。
pub fn effective_ssh_from_env(core_ssh_command: Option<&str>) -> SshCommand {
    effective_ssh(
        std::env::var("GIT_SSH_COMMAND").ok().as_deref(),
        core_ssh_command,
        std::env::var("GIT_SSH").ok().as_deref(),
    )
}

/// 空文字列と空白だけの値を「未設定」に畳む。
///
/// `config` の `MEWGIT_GIT_BIN`（08）も同じ判断をしているが、あちらは `trim` しない。
/// **揃えないのは、こちらの値がシェルに渡るコマンド文字列だから** —— 空白だけの実効値に
/// `-o BatchMode=yes` を足すと、git は先頭のオプションをコマンド名として起動する。
fn set(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// エイリアスを実ホスト名に正規化する。
///
/// **`ssh -G` は未定義のエイリアスでも exit 0 で引数をそのまま返す**（04 の実測）。
/// つまり「エイリアスが無かった」は成功。`Err` になるのは 3 つ — ssh を起動できな
/// かったとき、ssh が非 0 で終了したとき、出力に `hostname` の行が無かったとき。
/// 3 つ目は ssh が出力の形を変えたときにしか起きないが、**黙って別の値を返すより
/// 落ちたほうがいい**（正規化していない値を正規化済みとして下流に渡さない）。
/// 呼び出し側は `Err` のときに `host_raw` をそのまま使い、理由を通知に出す。
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
        // **`alias` は positional に載る**ので `--` を置く（03 の「argv への変換」と
        // 同じ数え方）。`.git/config` の URL は clone してきた値がそのまま入り、
        // `RemoteUrl` の `host_of` が `user@` を捨てるので、`git@-oProxyCommand=...`
        // は裸の `-oProxyCommand=...` になってここへ来る（04）。
        .arg("--")
        .arg(alias)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // ここから下の 2 つと下の spawn エラーの文言は `git_exec::Runner::spawn` と
        // 同じ（エラー文の安定と、キャンセルで子を残さないこと）。**共有先はまだ
        // 作らない** — 抽出できるのは `Command` の組み立てだけで、エラー型が違うので
        // 呼び出し側で包み直しが要る。M4 の `GhCli` が 3 本目になったら数え直す。
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
///
/// 値を `trim` しないのは、`str::lines` が CRLF の `\r` を自分で落とすため。空値を
/// 飛ばす腕も足さない — `None` の理由を「`hostname ` の行が無い」1 つに保つ
/// （呼び出し側の案内はそう言っている）。
fn hostname_from_dump(dump: &str) -> Option<&str> {
    dump.lines().find_map(|line| line.strip_prefix("hostname "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `-F` に渡す ssh_config を書いて、そこで `ssh -G` を回す。
    ///
    /// `TempDir` はこの中で作って呼び出しのあいだだけ持つ。テストごとに書くと
    /// 「`TempDir` を束縛し続ける」注意書きが人数分に増える。
    async fn resolve(ssh_config: &str, alias: &str) -> Result<String, AuthError> {
        let (_dir, path) = crate::test_util::temp_file("config", ssh_config);
        resolve_with(Path::new(SSH_BIN), Some(&path), alias).await
    }

    /// エイリアスが 1 つだけ書かれた ssh_config。
    const GH_WORK: &str = "Host gh-work\n  HostName github.com\n  User git\n";

    #[test]
    fn the_environment_wins_over_the_config() {
        // git と同じ優先順（環境変数を置くと `core.sshCommand` は読まれない。
        // git 2.55 で確認）。
        assert_eq!(
            effective_ssh(Some("ssh -i env"), Some("ssh -i config"), None),
            SshCommand::Shell("ssh -i env".to_string())
        );
    }

    #[test]
    fn the_config_is_used_when_the_environment_is_unset() {
        assert_eq!(
            effective_ssh(None, Some("ssh -i config"), None),
            SshCommand::Shell("ssh -i config".to_string())
        );
    }

    #[test]
    fn git_ssh_alone_is_opaque() {
        // `GIT_SSH` はシェルを通さないので、同じ形でオプションを足せない。
        // `GIT_SSH_COMMAND` を置くと git はそちらを優先するので、重ねるつもりが
        // 差し替えになる。
        assert_eq!(
            effective_ssh(None, None, Some("/usr/bin/ssh")),
            SshCommand::Opaque
        );
    }

    #[test]
    fn git_ssh_is_ignored_when_a_shell_command_is_set() {
        // git も `GIT_SSH_COMMAND` / `core.sshCommand` を先に見る。
        assert_eq!(
            effective_ssh(None, Some("ssh -i config"), Some("/usr/bin/ssh")),
            SshCommand::Shell("ssh -i config".to_string())
        );
    }

    #[test]
    fn nothing_set_falls_back_to_plain_ssh() {
        assert_eq!(
            effective_ssh(None, None, None),
            SshCommand::Shell("ssh".to_string())
        );
    }

    #[test]
    fn blank_values_count_as_unset() {
        // `core.sshCommand = ` と書いただけの設定を「そういう名前のコマンド」に
        // しない。空白だけの実効値に `-o BatchMode=yes` を足すと、git は先頭の
        // オプションをコマンド名として起動しようとする。
        assert_eq!(
            effective_ssh(Some("  "), Some("\t"), Some(" ")),
            SshCommand::default()
        );
    }

    #[test]
    fn takes_the_hostname_line_not_the_host_line() {
        // 実際の並び。`host` が先に来る。
        let dump = "host gh-work\nuser git\nhostname github.com\nport 22\n";
        assert_eq!(hostname_from_dump(dump), Some("github.com"));
    }

    #[test]
    fn reports_no_hostname_when_the_line_is_missing() {
        assert_eq!(hostname_from_dump("host gh-work\nuser git\n"), None);
    }

    #[test]
    fn reads_a_crlf_dump() {
        // 値を `trim` していないのは `str::lines` が `\r` を落とすから。
        // ここが落ちたら `trim` を戻すのではなく、`lines` の前提を疑う。
        assert_eq!(
            hostname_from_dump("host gh-work\r\nhostname github.com\r\n"),
            Some("github.com")
        );
    }

    #[tokio::test]
    async fn resolves_an_alias_to_its_hostname() {
        assert_eq!(resolve(GH_WORK, "gh-work").await.unwrap(), "github.com");
    }

    #[tokio::test]
    async fn an_unknown_alias_resolves_to_itself() {
        // `ssh -G` は未定義のエイリアスでも exit 0 で引数をそのまま返す（04 の実測）。
        // 「解決できなかった」を `Err` にすると、エイリアスを使っていないふつうの
        // リモートが全部エラーになる。
        assert_eq!(
            resolve(GH_WORK, "acme.ghe.com").await.unwrap(),
            "acme.ghe.com"
        );
    }

    #[tokio::test]
    async fn an_option_like_alias_is_not_read_as_an_option() {
        // **`--` の見張り。** 外すと ssh が `-4` をオプションとして読み、ホストが
        // 無いまま usage を出して落ちる。`--` があればホスト名として不正だと言う
        // （この拒否は OpenSSH 7.6 以降）。どちらも `Err` なので、見張れるのは理由のほう。
        let err = resolve(GH_WORK, "-4").await.unwrap_err();
        let message = err.to_string();
        assert!(
            !message.to_lowercase().contains("usage"),
            "ssh がオプションとして読んでいる: {message}"
        );
    }

    #[tokio::test]
    async fn a_broken_config_is_reported_with_the_stderr() {
        // 不変ルール 4 の趣旨。ssh が断った理由を握りつぶさない。
        let err = resolve("Host gh-work\n  NoSuchKeyword yes\n", "gh-work")
            .await
            .unwrap_err();
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
