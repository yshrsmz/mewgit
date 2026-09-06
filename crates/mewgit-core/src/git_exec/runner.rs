//! `git` サブプロセスの実行。設計は `docs/design/03-git-exec.md` の「Runner」。

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};

use super::{to_command, ExecError, ExecErrorKind, ExecOutput, GitCommand, GitOp};

/// `RebaseInteractive` を拒否する理由。
///
/// todo を差し替えずに `git rebase -i` を走らせると、`GIT_EDITOR=true` によって
/// todo がそのまま採用され、全 `pick` の rebase が実行される。`plan` を黙って
/// 捨てた破壊的操作になるので、配線が済む M5 まで起動しない。
const REBASE_INTERACTIVE_UNSUPPORTED: &str =
    "interactive rebase はまだ実装していません（M5）。plan を渡す経路が無いまま実行すると、\
     指定した todo を無視した rebase になります";

/// `GitOp` を `git` サブプロセスとして実行する。
///
/// `git` は PATH から引く。パスを設定で差し替える要求は今のところ無い。
#[derive(Debug, Clone)]
pub struct Runner {
    repo_root: PathBuf,
    /// 実行時に重ねる環境変数。テストが実行環境の設定を持ち込まないために使う。
    env: Vec<(OsString, OsString)>,
}

impl Runner {
    pub fn new(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
            env: Vec::new(),
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
        let command = to_command(op).map_err(|err| ExecError {
            argv: Vec::new(),
            code: None,
            stderr: err.to_string(),
            kind: ExecErrorKind::NotAttempted,
        })?;

        if matches!(op, GitOp::RebaseInteractive { .. }) {
            return Err(ExecError {
                argv: command.argv,
                code: None,
                stderr: REBASE_INTERACTIVE_UNSUPPORTED.to_string(),
                kind: ExecErrorKind::NotAttempted,
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

        let output = waited.map_err(|err| ExecError {
            argv: command.argv.clone(),
            code: None,
            stderr: format!("git の終了を待てませんでした: {err}"),
            kind: ExecErrorKind::Other,
        })?;

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        if output.status.success() {
            Ok(ExecOutput {
                argv: command.argv,
                stdout,
                stderr,
            })
        } else {
            Err(ExecError {
                argv: command.argv,
                code: output.status.code(),
                kind: classify(&stdout, &stderr),
                stderr,
            })
        }
    }

    fn spawn(&self, command: &GitCommand) -> Result<Child, ExecError> {
        let mut cmd = Command::new("git");
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
        cmd.envs(self.env.iter().map(|(k, v)| (k.as_os_str(), v.as_os_str())));

        cmd.spawn().map_err(|err| ExecError {
            argv: command.argv.clone(),
            code: None,
            stderr: format!("git を起動できませんでした: {err}"),
            kind: ExecErrorKind::NotAttempted,
        })
    }
}

/// 出力のパターンからエラー種別を決める。**上から順に見て、先に当たったものを採る。**
///
/// stdout も見るのは、`git merge`（`pull --no-rebase` の経路）がコンフリクトを
/// stdout にしか書かないため。stderr だけだと `Other` に落ちる。
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
    } else if has("non-fast-forward") || has("rejected") {
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
    fn auth_patterns_win_over_rejected() {
        // push の失敗は `rejected` を含みがちなので、順序を変えると認証の失敗まで
        // NonFastForward に落ちる。
        let stderr = "git@github.com: Permission denied (publickey).\n\
                      fatal: Could not read from remote repository.\n\
                      error: failed to push some refs (rejected)";
        assert_eq!(classify("", stderr), ExecErrorKind::SshAuthFailed);

        let saml = "ERROR: The `acme' organization has enabled or enforced SAML SSO. \
                    Access rejected.";
        assert_eq!(classify("", saml), ExecErrorKind::SamlSsoRequired);
    }
}
