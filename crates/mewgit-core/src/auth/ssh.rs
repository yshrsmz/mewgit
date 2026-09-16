//! SSH エイリアスの正規化。設計は `docs/design/04-auth-account.md` の
//! 「SSH エイリアスの正規化」。
//!
//! `.git/config` のリモート URL に書かれたホストは `~/.ssh/config` のエイリアス
//! （`gh-work` など）のことがある。`ssh -G` に解決させて実ホスト名にする。

use std::ffi::OsString;
use std::path::Path;

use super::AuthError;

/// 何も設定されていないときに git が起動する ssh。[`effective_ssh`] の既定値。
///
/// [`SshCommand::Opaque`] のときに [`resolve_ssh_host`] が落ちる先でもある
/// （04 の「git が使う ssh と、ここで叩く ssh」の表）。
const SSH_BIN: &str = "ssh";

/// [`SshCommand::Shell`] を起動するシェル。git が `core.sshCommand` を渡すのと同じ。
const SHELL_BIN: &str = "sh";

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
/// `git_exec::Runner` が `-o BatchMode=yes` を重ねるのに使い、[`resolve_ssh_host`] は
/// これを `sh -c` 経由で起動する（04 の「git が使う ssh と、ここで叩く ssh」）。
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
/// `ssh` は **git が実際に使う ssh**（[`effective_ssh_from_env`]）。PATH の素の `ssh`
/// を叩くと、`core.sshCommand` / `GIT_SSH_COMMAND` を別の config を読む ssh に向けて
/// いる環境で**解決と接続が食い違う**（04）。
///
/// **`ssh -G` は未定義のエイリアスでも exit 0 で引数をそのまま返す**（04 の実測）。
/// つまり「エイリアスが無かった」は成功。`Err` になるのは 4 つ — ssh を起動できな
/// かったとき、ssh が非 0 で終了したとき、出力に `hostname` の行が無かったとき、
/// その値が [`valid_host`](super::url::valid_host) を通らなかったとき。後ろの 2 つは
/// ssh が出力の形を変えたときにしか起きないが、**黙って別の値を返すより落ちたほうが
/// いい**（正規化していない値を正規化済みとして下流に渡さない）。呼び出し側は `Err` の
/// ときに `host_raw` をそのまま使い、理由を通知に出す。
///
/// **`-o BatchMode=yes` は重ねない。** `ssh -G` は設定を書き出すだけで接続しないので、
/// パスフレーズや `known_hosts` のプロンプトが出る経路が無い（`Runner` が重ねるのは
/// 実際に繋ぐから。03）。
pub async fn resolve_ssh_host(ssh: &SshCommand, alias: &str) -> Result<String, AuthError> {
    resolve_with(ssh, None, alias).await
}

/// `ssh -G` を実行して `hostname` の値を返す。
///
/// `config_file` は `-F` に渡す ssh_config。**テストを実行環境から切り離すためだけに
/// あり**、本番は `None` で `~/.ssh/config` と `/etc/ssh/ssh_config` をそのまま使う。
async fn resolve_with(
    ssh: &SshCommand,
    config_file: Option<&Path>,
    alias: &str,
) -> Result<String, AuthError> {
    let failed = |message: String| AuthError::SshResolveFailed {
        alias: alias.to_string(),
        message,
    };

    // **`alias` は positional に載る**ので `--` を置く（03 の「argv への変換」と
    // 同じ数え方）。`.git/config` の URL は clone してきた値がそのまま入り、
    // `RemoteUrl` の `split_authority` が `user@` を捨てるので、
    // `git@-oProxyCommand=...` は裸の `-oProxyCommand=...` になってここへ来る（04）。
    let mut args: Vec<OsString> = Vec::new();
    if let Some(path) = config_file {
        args.push(OsString::from("-F"));
        args.push(path.into());
    }
    args.extend([
        OsString::from("-G"),
        OsString::from("--"),
        OsString::from(alias),
    ]);

    // `GIT_SSH` はシェルを通さない argv 規約なので `-G` を解する保証が無く、叩かない。
    // 落とし先は素の `ssh` = [`SshCommand::default`] と同じ（04 の表）。
    let command = match ssh {
        SshCommand::Shell(command) => command.as_str(),
        SshCommand::Opaque => SSH_BIN,
    };
    // `sh -c '<実効 ssh> "$@"' ssh -G -- <alias>`。**alias をシェルの文字列に埋めない**
    // —— `"$@"` の位置引数に載せるので、`$(touch x)` を渡してもシェルは展開せず、
    // ssh が「hostname として不正」で落ちる（04 の実測）。`-c` の次の `ssh` は `$0` に
    // 入るだけの捨て値で、起動されるのは実効値のほう。
    //
    // **`Opaque` を直起動する腕は置かない。** 落とし先が `Shell("ssh")` と同じ値なので、
    // 分けても**どちらの腕を通ったかを観測できるテストが書けない**（「テストで踏めない
    // 状態をコードに入れない」）。
    let mut shell = vec![
        OsString::from("-c"),
        OsString::from(format!("{command} \"$@\"")),
        OsString::from(SSH_BIN),
    ];
    shell.extend(args);

    // 起動の形は `gh` と共有している（`probe`）。**数え直した結果はそこの doc に
    // ある** — `git_exec::Runner::spawn` は前提が違うので入っていない。
    let stdout = super::probe::output(Path::new(SHELL_BIN), shell, &[])
        .await
        .map_err(|err| failed(err.describe()))?;
    let host = hostname_from_dump(&stdout)
        .ok_or_else(|| failed("ssh -G の出力に hostname の行がありませんでした".to_string()))?;
    // **産出側で検査する。** 通してしまうと不正なホストが `RepoTarget.host` まで
    // 進み、`GithubClient::new` が `InvalidHost` で落として「GitHub 機能が使えない」
    // だけが残る。ここで落とせば呼び出し側は `host_raw` で続けられる（04）。
    if !super::url::valid_host(host) {
        return Err(failed(format!(
            "ssh -G が返した hostname をホスト名として読めませんでした: {host}"
        )));
    }
    Ok(host.to_string())
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
    use crate::test_util::fake_ssh_host;

    /// `-F` に渡す ssh_config を書いて、そこで `ssh -G` を回す。
    ///
    /// `TempDir` はこの中で作って呼び出しのあいだだけ持つ。テストごとに書くと
    /// 「`TempDir` を束縛し続ける」注意書きが人数分に増える。
    async fn resolve(ssh_config: &str, alias: &str) -> Result<String, AuthError> {
        let (_dir, path) = crate::test_util::temp_file("config", ssh_config);
        resolve_with(&SshCommand::default(), Some(&path), alias).await
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
    async fn a_missing_ssh_is_reported_with_its_path() {
        // **`sh -c` 経由なので `ProbeError::Spawn` には落ちない** —— 起動に失敗するのは
        // `sh` のほうで、実効値が見つからないことは `sh` が exit 127 と stderr で言う。
        // 案内にパスが残っていることだけを見る（不変ルール 4 の趣旨）。
        let ssh = SshCommand::Shell("/nonexistent/ssh".to_string());
        let err = resolve_with(&ssh, None, "gh-work").await.unwrap_err();
        let message = err.to_string();
        assert!(message.contains("/nonexistent/ssh"), "{message}");
    }

    #[tokio::test]
    async fn the_effective_ssh_is_the_one_that_runs() {
        // **これが「実効値に向けた」の見張り。** PATH の素の `ssh` を叩いていたら
        // `gh-work` はそのまま返ってくるので、偽物の hostname は出ない。
        let (_dir, ssh) = fake_ssh_host("fake.example.com");
        assert_eq!(
            resolve_with(&ssh, None, "gh-work").await.unwrap(),
            "fake.example.com"
        );
    }

    #[tokio::test]
    async fn options_in_the_effective_ssh_reach_ssh() {
        // 実効値は「コマンド + オプション」の文字列で、後ろに `-G -- <alias>` が付く。
        // `-F` を実効値の側に書いても効くことを見る（`sh -c '<cmd> "$@"'` の形）。
        let (_dir, path) = crate::test_util::temp_file("config", GH_WORK);
        let ssh = SshCommand::Shell(format!("ssh -F {}", path.display()));
        assert_eq!(
            resolve_with(&ssh, None, "gh-work").await.unwrap(),
            "github.com"
        );
    }

    #[tokio::test]
    async fn the_alias_is_not_reinterpreted_by_the_shell() {
        // **`sh -c` を通すようになったので数え直した腕。** alias を実効値の文字列に
        // 埋めると `$(...)` がシェルに展開される。`"$@"` の位置引数に載せているので
        // 展開されず、ssh が「ホスト名として不正」で落ちる（04 の実測）。
        let (dir, ssh) = fake_ssh_host("github.com");
        let canary = dir.path().join("PWNED");
        let alias = format!("$(touch {})", canary.display());
        // 偽物は何を渡されても同じ hostname を返すので、見るのは canary の有無だけ。
        let _ = resolve_with(&ssh, None, &alias).await;
        assert!(!canary.exists(), "alias がシェルに展開された");
    }

    #[tokio::test]
    async fn a_hostname_that_is_not_a_host_is_refused() {
        // **産出側の検査**（04）。`-4` を通すと `RepoTarget.host` まで進み、
        // `GithubClient::new` が `InvalidHost` で落として degraded だけが残る。
        let (_dir, ssh) = fake_ssh_host("-4");
        let err = resolve_with(&ssh, None, "gh-work").await.unwrap_err();
        assert!(err.to_string().contains("-4"), "{err}");
    }

    #[tokio::test]
    async fn git_ssh_falls_back_to_plain_ssh() {
        // `Opaque` は値を持たない単位ヴァリアントなので、落とし先は素の `ssh`（04 の表）。
        // **見張れるのは「素の ssh と同じ結果になる」ことだけ** —— 分岐を残しても
        // どちらのランチャを通ったかは観測できないので、腕ごと畳んである。
        //
        // **自分自身と比べているのではない。** 比べているのは 2 つの**実行**で、
        // `Opaque` 側だけが独立に失敗しうる —— `resolve_with` の
        // `SshCommand::Opaque => SSH_BIN` を起動できない値に変えると、この
        // `unwrap()` が落ちる（実測。等価な定数の assert に置き換えたときは
        // **同じ破壊で 19 本とも緑のまま通った**）。
        let (_dir, path) = crate::test_util::temp_file("config", GH_WORK);
        assert_eq!(
            resolve_with(&SshCommand::Opaque, Some(&path), "gh-work")
                .await
                .unwrap(),
            resolve_with(&SshCommand::default(), Some(&path), "gh-work")
                .await
                .unwrap(),
        );
    }
}
