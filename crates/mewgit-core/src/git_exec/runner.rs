//! `git` サブプロセスの実行。設計は `docs/design/03-git-exec.md` の「Runner」。

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};

use super::{to_command, ExecError, ExecErrorKind, ExecOutput, GitCommand, GitOp};
use crate::auth::ssh::SshCommand;

/// `RebaseInteractive` を拒否する理由。
///
/// todo を差し替えずに `git rebase -i` を走らせると、`GIT_EDITOR=true` によって
/// todo がそのまま採用され、全 `pick` の rebase が実行される。`plan` を黙って
/// 捨てた破壊的操作になるので、配線が済む M5 まで起動しない。
const REBASE_INTERACTIVE_UNSUPPORTED: &str =
    "interactive rebase はまだ実装していません（M5）。plan を渡す経路が無いまま実行すると、\
     指定した todo を無視した rebase になります";

/// ssh がパスフレーズを訊きに `/dev/tty` を掴むのを止めるオプション。
///
/// 実効 ssh の**後ろ**に足す。ssh は同じ項目を先に現れた値で決めるので、ユーザーが
/// 自分のコマンドに `BatchMode=no` と書いていればそちらが勝つ（OpenSSH 10.3 で確認）。
/// **黙って上書きしない方針と向きが揃っている**（03 の「BatchMode の重ね方」）。
const BATCH_MODE: &str = " -o BatchMode=yes";

/// `GitOp` を `git` サブプロセスとして実行する。
#[derive(Debug, Clone)]
pub struct Runner {
    repo_root: PathBuf,
    /// 実行する `git`。`config` の `general.git_bin`（既定は `"git"` で PATH から
    /// 引く。絶対パスも書ける）から渡す。引数にしているのは、設定を読んだのに
    /// 渡し忘れる経路を作らないため。
    git_bin: PathBuf,
    /// 実行時に重ねる環境変数。テストの隔離と、M5 の `GIT_SEQUENCE_EDITOR` /
    /// `GIT_EDITOR` の差し替えに使う。
    env: Vec<(OsString, OsString)>,
    /// `GIT_SSH_COMMAND` に入れる値。**生の設定値ではなく、実効 ssh に
    /// [`BATCH_MODE`] を足したもの。** `None` は「触らない」（`GIT_SSH` の環境。
    /// 03 の「BatchMode の重ね方」）。
    ///
    /// 素の `GIT_SSH_COMMAND="ssh -o BatchMode=yes"` を置くと、ユーザーの
    /// `GIT_SSH_COMMAND` / `core.sshCommand` を捨ててしまう。
    git_ssh_command: Option<String>,
}

impl Runner {
    /// `ssh` は git が実際に使う ssh（`auth::ssh::effective_ssh_from_env`）。
    /// **`git_bin` と同じくコンストラクタの引数にしている**のは、決めたのに渡し忘れる
    /// 経路を作らないため。
    pub fn new(
        repo_root: impl Into<PathBuf>,
        git_bin: impl Into<PathBuf>,
        ssh: SshCommand,
    ) -> Self {
        Self {
            repo_root: repo_root.into(),
            git_bin: git_bin.into(),
            env: Vec::new(),
            git_ssh_command: match ssh {
                SshCommand::Shell(command) => Some(command + BATCH_MODE),
                SshCommand::Opaque => None,
            },
        }
    }

    /// 追加の環境変数を重ねる。`GIT_TERMINAL_PROMPT` などの既定より後に適用されるので、
    /// テストからは上書きもできる。
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

    /// `git` を実行して結果を返す。
    ///
    /// `git` を起動しないまま失敗する経路が 2 つある（argv を組めなかったときと、
    /// `RebaseInteractive`）。どちらも [`ExecErrorKind::NotAttempted`] で返す。
    pub async fn run(&self, op: &GitOp) -> Result<ExecOutput, ExecError> {
        let command = to_command(op).map_err(|err| ExecError::NoResult {
            argv: Vec::new(),
            reason: err.to_string(),
        })?;

        if matches!(op, GitOp::RebaseInteractive { .. }) {
            return Err(ExecError::NoResult {
                argv: command.argv,
                reason: REBASE_INTERACTIVE_UNSUPPORTED.to_string(),
            });
        }

        let mut child = self.spawn(&command)?;

        // stdin への書き込みと終了待ちを同時に進める。順に行うと、パイプのバッファを
        // 超えるパッチで git 側が stdout を書けずに止まり、こちらも書き込みで止まる。
        let pipe = child.stdin.take();
        let write = async {
            if let (Some(mut pipe), Some(bytes)) = (pipe, command.stdin.as_ref()) {
                // 書き込みの失敗は握る。git が先に落ちると BrokenPipe になるが、
                // 本当の原因は git の stderr にある。こちらを返すと隠れる。
                let _ = pipe.write_all(bytes).await;
                let _ = pipe.shutdown().await;
            }
        };
        let (_, waited) = tokio::join!(write, child.wait_with_output());

        let output = waited.map_err(|err| ExecError::NoResult {
            argv: command.argv.clone(),
            reason: format!("git の終了を待てませんでした: {err}"),
        })?;

        let stdout = lossy(output.stdout);
        let stderr = lossy(output.stderr);

        if output.status.success() {
            Ok(ExecOutput {
                argv: command.argv,
                stdout,
                stderr,
            })
        } else {
            Err(ExecError::Failed {
                argv: command.argv,
                code: output.status.code(),
                kind: classify(&stdout, &stderr),
                stderr,
            })
        }
    }

    fn spawn(&self, command: &GitCommand) -> Result<Child, ExecError> {
        let mut cmd = Command::new(&self.git_bin);
        cmd.current_dir(&self.repo_root)
            .args(&command.argv)
            // 認証を求めて端末を掴まないようにする。
            .env("GIT_TERMINAL_PROMPT", "0")
            // エラー文のパターン照合を安定させる。
            .env("LC_ALL", "C")
            // 意図しないエディタ起動を防ぐ。
            .env("GIT_EDITOR", "true")
            .stdin(if command.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // 実行中ジョブのキャンセル（M3）で子プロセスを残さないため。
            .kill_on_drop(true);
        // **`GIT_TERMINAL_PROMPT=0` は ssh に効かない**（ssh は stdin ではなく
        // `/dev/tty` を直に読む）。
        if let Some(ssh) = &self.git_ssh_command {
            cmd.env("GIT_SSH_COMMAND", ssh);
        }
        cmd.envs(self.env.iter().map(|(k, v)| (k.as_os_str(), v.as_os_str())));

        cmd.spawn().map_err(|err| ExecError::NoResult {
            argv: command.argv.clone(),
            reason: format!("{} を起動できませんでした: {err}", self.git_bin.display()),
        })
    }
}

/// git の出力を表示用の `String` にする。
///
/// `String::from_utf8_lossy(..).into_owned()` は、妥当な UTF-8（ふつうの場合）でも
/// バッファを丸ごと複製する。`from_utf8` なら `Vec<u8>` をそのまま `String` に
/// 転用でき、不正なバイトを含むときだけ複製が要る。
fn lossy(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into_owned())
}

/// 出力のパターンからエラー種別を決める。**上から順に見て、先に当たったものを採る。**
///
/// stdout も見るのは、`git merge`（`pull --no-rebase` の経路）がコンフリクトを
/// stdout にしか書かないため。stderr だけだと `Other` に落ちる。
///
/// **`rejected` だけでは広すぎる。** hook や保護ブランチが断った push も
/// `! [remote rejected] main -> main (pre-receive hook declined)` と書く（git 2.55 で
/// 確認）。これを `NonFastForward` にすると、`app` が「次の `P` は force push」と
/// 憶えてしまい（01 の「force push」）、履歴と関係の無い拒否に対して「リモートを
/// 上書きします」という確認が出る。git が非 FF を表す語は 4 つ ——
/// push が断る `(non-fast-forward)` / `(fetch first)` / `(stale info)`（lease が古い）と、
/// `pull --ff-only` が断る `Not possible to fast-forward`。
///
/// **4 つ目は `rejected` を含まない。** 履歴が分かれた `pull --ff-only` はこの
/// `fatal:` だけを書き、上の 3 語をどれも出さない（git 2.55 で確認）。拾わないと
/// `p` の失敗が `Other` に落ち、**案内が要るその場面で案内文の側から消える**
/// （01 の「失敗の案内文」）。`hint:` の行は `advice.diverging false` で消えるので、
/// 見るのは `fatal:` の側。**広げても force は arm されない** —— 憶える
/// `push_rejection` は `GitOp::Push` で門番している（01 の「force push」）。
fn classify(stdout: &str, stderr: &str) -> ExecErrorKind {
    let has = |needle: &str| stderr.contains(needle) || stdout.contains(needle);

    if has("SAML SSO") || has("organization has enabled or enforced SAML") {
        ExecErrorKind::SamlSsoRequired
    } else if has("Permission denied (publickey)") {
        ExecErrorKind::SshAuthFailed
    } else if has("Authentication failed") {
        ExecErrorKind::HttpsAuthFailed
    } else if has("CONFLICT") || has("could not apply") {
        ExecErrorKind::Conflict
    } else if has("non-fast-forward")
        || has("fetch first")
        || has("stale info")
        || has("Not possible to fast-forward")
    {
        ExecErrorKind::NonFastForward
    } else {
        ExecErrorKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_pattern() {
        assert_eq!(
            classify(
                "",
                "ERROR: The `acme' organization has enabled or enforced SAML SSO."
            ),
            ExecErrorKind::SamlSsoRequired
        );
        assert_eq!(
            classify("", "git@github.com: Permission denied (publickey)."),
            ExecErrorKind::SshAuthFailed
        );
        assert_eq!(
            classify(
                "",
                "fatal: Authentication failed for 'https://github.com/a/b.git/'"
            ),
            ExecErrorKind::HttpsAuthFailed
        );
        assert_eq!(
            classify("", "error: could not apply d0f6d2f... side"),
            ExecErrorKind::Conflict
        );
        assert_eq!(
            classify("", " ! [rejected]        main -> main (non-fast-forward)"),
            ExecErrorKind::NonFastForward
        );
        // `pull --ff-only` が履歴の分かれたブランチを断る形。**`rejected` も上の
        // 3 語も含まない**ので、この行を拾わないと `Other` に落ちる（git 2.55 で確認）。
        assert_eq!(
            classify("", "fatal: Not possible to fast-forward, aborting."),
            ExecErrorKind::NonFastForward
        );
        assert_eq!(
            classify("", "fatal: pathspec 'nope' did not match any files"),
            ExecErrorKind::Other
        );
    }

    #[test]
    fn merge_conflict_is_found_on_stdout() {
        // `git merge` はコンフリクトを stdout にしか書かない（git 2.55 で確認）。
        // stderr だけを見ていた頃はここが `Other` に落ちていた。
        let stdout = "Auto-merging f\nCONFLICT (content): Merge conflict in f\n\
                      Automatic merge failed; fix conflicts and then commit the result.\n";
        assert_eq!(classify(stdout, ""), ExecErrorKind::Conflict);
    }

    #[test]
    fn each_alternative_of_a_pattern_pair_is_enough() {
        // 実際の git / GitHub の文面は両方の語を含むことが多い。片方だけを含む
        // 入力（一部は作り物）を通して、選択肢の両側が生きていることを見る。
        assert_eq!(
            classify(
                "",
                "remote: The `acme' organization has enabled or enforced SAML."
            ),
            ExecErrorKind::SamlSsoRequired
        );
        assert_eq!(
            classify("", "remote: SAML SSO の承認が要ります"),
            ExecErrorKind::SamlSsoRequired
        );
        assert_eq!(
            classify("", " ! [rejected]        main -> main (fetch first)"),
            ExecErrorKind::NonFastForward
        );
        assert_eq!(
            classify("", "error: the update is a non-fast-forward"),
            ExecErrorKind::NonFastForward
        );
        assert_eq!(
            classify(
                "Auto-merging f\nCONFLICT (content): Merge conflict in f\n",
                ""
            ),
            ExecErrorKind::Conflict
        );
        assert_eq!(
            classify("", "error: could not apply d0f6d2f... side"),
            ExecErrorKind::Conflict
        );
    }

    #[test]
    fn non_utf8_output_is_shown_lossily() {
        assert_eq!(lossy(b"ok\n".to_vec()), "ok\n");
        assert_eq!(lossy(vec![0x61, 0xff, 0x62]), "a\u{fffd}b");
    }

    #[test]
    fn a_hook_rejection_is_not_a_non_fast_forward() {
        // **`rejected` だけを見ていた頃はここが `NonFastForward` に落ちていた。**
        // そうなると次の `P` が force push の確認になる（01）が、履歴とは関係の無い
        // 拒否なので force しても同じ hook に断られる。git 2.55 の実測の 1 行。
        let stderr = " ! [remote rejected] main -> main (pre-receive hook declined)\n\
                      error: failed to push some refs to '../remote.git'";
        assert_eq!(classify("", stderr), ExecErrorKind::Other);
    }

    #[test]
    fn auth_patterns_win_over_a_non_fast_forward() {
        // push の失敗は非 FF の語を含みがちなので、順序を変えると認証の失敗まで
        // NonFastForward に落ちる。
        let stderr = "git@github.com: Permission denied (publickey).\n\
                      fatal: Could not read from remote repository.\n\
                      error: failed to push some refs (non-fast-forward)";
        assert_eq!(classify("", stderr), ExecErrorKind::SshAuthFailed);

        let saml = "ERROR: The `acme' organization has enabled or enforced SAML SSO. \
                    Access rejected.";
        assert_eq!(classify("", saml), ExecErrorKind::SamlSsoRequired);
    }
}
