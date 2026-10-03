//! 読み取り専用の一発 probe。`ssh -G` と `gh`、起動時の `git --version`
//! （`git_exec::version`）に共通の土台。
//!
//! **`ssh.rs` が「`GhCli` が 3 本目になったら数え直す」と送っていた宿題の答え。**
//! 数え直すと 3 本ではなく「同形が 2 本 + 別物が 1 本」だった。
//!
//! - `git_exec::Runner::spawn` は**別物**。`process_group(0)` で起こし、
//!   `kill_on_drop` を敢えて付けず（SIGKILL だと `index.lock` が残る。03 の表）、
//!   stdin をパイプにし、キャンセルを受ける
//! - `ssh -G` と `gh` は**同形**。lock を握らない読み取り専用の一発なので
//!   `kill_on_drop(true)` が安全で、待つのは終了だけ
//!
//! エラー型が違う（`SshResolveFailed` と `GhFailed`）ので、ここは**素材**を返し、
//! 文言は呼ぶ側が組む。

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

/// probe が失敗した理由。呼ぶ側が自分のエラー型に包み直す。
#[derive(Debug)]
pub(crate) enum ProbeError {
    /// 起動できなかった。**`kind` を捨てない** —— 「PATH に無い」と
    /// 「あるが実行できない」を同じ案内にすると、権限の問題を探せなくなる。
    Spawn {
        kind: io::ErrorKind,
        message: String,
    },
    /// 起動はしたが非 0 で終わった。
    Failed { code: Option<i32>, stderr: String },
}

impl ProbeError {
    /// 「なぜ失敗したか」を 1 行にする。呼ぶ側の案内文に埋める。
    pub(crate) fn describe(&self) -> String {
        match self {
            ProbeError::Spawn { message, .. } => message.clone(),
            ProbeError::Failed { code, stderr } => {
                let code = code.map_or_else(
                    || "シグナルで終了".to_string(),
                    |code| format!("exit {code}"),
                );
                if stderr.is_empty() {
                    code
                } else {
                    format!("{code}: {stderr}")
                }
            }
        }
    }
}

/// `bin` を起動して stdout を返す。
///
/// `LC_ALL=C` を先に置くので、`env` から上書きできる（テストの隔離のため）。
pub(crate) async fn output<I, S>(
    bin: &Path,
    args: I,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Result<String, ProbeError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // エラー文の検出を安定させる（`git_exec::Runner` と同じ理由）。
        .env("LC_ALL", "C")
        .envs(env.iter().map(|(k, v)| (k, v)))
        // 読み取り専用なので SIGKILL で困る lock が無い。
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|err| ProbeError::Spawn {
            kind: err.kind(),
            message: format!("{} を起動できませんでした: {err}", bin.display()),
        })?;

    if !output.status.success() {
        return Err(ProbeError::Failed {
            code: output.status.code(),
            stderr: lossy(output.stderr).trim().to_string(),
        });
    }
    Ok(lossy(output.stdout))
}

/// 出力を表示用の `String` にする。
///
/// `String::from_utf8_lossy(..).into_owned()` は、妥当な UTF-8（ふつうの場合）でも
/// バッファを丸ごと複製する。`from_utf8` なら `Vec<u8>` をそのまま転用できる。
///
/// **`git_exec::runner::lossy` と同じ中身だが共有しない。** 00 の依存の向きで
/// `git_exec` は `auth` の**上段**なので、こちらから呼ぶと上向きになる。
fn lossy(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into_owned())
}
