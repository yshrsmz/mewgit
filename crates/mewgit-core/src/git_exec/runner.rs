//! `git` サブプロセスの実行。設計は `docs/design/03-git-exec.md` の「Runner」。

use std::ffi::OsString;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};

use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::oneshot::{self, error::TryRecvError};

use super::rebase::{sh_quote, EDITOR_STDERR_PREFIX, HEAD_MOVED_MARKER};
use super::{to_commands, ExecError, ExecErrorKind, ExecOutput, GitCommand, GitOp};
use crate::auth::ssh::SshCommand;
use crate::git_read::RepoReader;

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
    /// `GIT_SEQUENCE_EDITOR` に入れる値。自バイナリを引用して `--sequence-editor` を
    /// 足したもの（03 の「Interactive rebase の実装」）。
    ///
    /// **`RebaseInteractive` に限らず、どのコマンドにも載せる。** mewgit の書き込みで
    /// sequence editor を起こすのはあれだけで（`pull` はモードを必ず argv で渡すので
    /// `pull.rebase=interactive` でも起きない。03 の実測）、ほかで起きたら plan が無い
    /// ので editor が非 0 で断る —— 既定の todo を黙って採るより閉じた側に倒れる。
    sequence_editor: OsString,
    /// `GIT_EDITOR` に入れる値。自バイナリに `--commit-editor` を足したもの。
    ///
    /// **これもどのコマンドにも載せる。** 止まった reword を `m` から continue した
    /// ときにも git はこれを起こすので、`RebaseInteractive` だけに載せると停止点より
    /// 後ろの reword が元のメッセージのまま通る（03 の実測）。reword 以外で起こされた
    /// ときは何もしない（`GIT_EDITOR=true` と同じ）。
    commit_editor: OsString,
    /// 2 つのエディタが起こす自バイナリそのもの。エディタの失敗のあとに、まだ在るかを
    /// 見る（[`ExecErrorKind::EditorGone`]。03 の「エラー分類」）。
    mewgit_bin: PathBuf,
    /// 実行時に重ねる環境変数。テストの隔離に使う。
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
    /// 経路を作らないため。`mewgit_bin` も同じ（Linux では `/proc/<pid>/exe`、ほかは
    /// `std::env::current_exe()`。`mewgit-tui` の `runtime::editor_bin`）。
    pub fn new(
        repo_root: impl Into<PathBuf>,
        git_bin: impl Into<PathBuf>,
        ssh: SshCommand,
        mewgit_bin: impl Into<PathBuf>,
    ) -> Self {
        let mewgit_bin = mewgit_bin.into();
        Self {
            repo_root: repo_root.into(),
            git_bin: git_bin.into(),
            sequence_editor: editor(&mewgit_bin, "--sequence-editor"),
            commit_editor: editor(&mewgit_bin, "--commit-editor"),
            mewgit_bin,
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

    /// `git` を実行して結果を返す。**キャンセルできない形。**
    ///
    /// 送り手を作らずに [`run_cancelable`](Self::run_cancelable) へ渡す。送り手が
    /// 落ちた受け側は二度と合図を返さない（`CancelSignal::wait`）ので、この呼び方
    /// ではキャンセルの腕が選ばれない。
    pub async fn run(&self, op: &GitOp) -> Result<ExecOutput, ExecError> {
        let (_, cancel) = oneshot::channel();
        self.run_cancelable(op, cancel).await
    }

    /// `git` を実行し、`cancel` が届いたらプロセスグループごと中断する。
    ///
    /// argv を組めなかったときは `git` を起動せずに [`ExecError::NoResult`] で返す。
    ///
    /// **中断は「殺して忘れる」ではなく、殺してから終了を待つ。** 待たずに future を
    /// 捨てると、`git` の後始末（lock ファイルの削除）が済む前に呼び出し側が次の
    /// 書き込みを出せてしまう。
    pub async fn run_cancelable(
        &self,
        op: &GitOp,
        cancel: oneshot::Receiver<()>,
    ) -> Result<ExecOutput, ExecError> {
        let commands = to_commands(op).map_err(|err| ExecError::NoResult {
            argv: Vec::new(),
            reason: err.to_string(),
        })?;

        // **古いハンクを当てない**（03 の「古いハンクの守り」）。git を起こす前に、
        // ハンクを読んだ時点の指紋と比べる。
        self.check_fresh(op, &commands[0].argv).await?;

        // **列を先頭から順に叩き、非 0 で止まる**（03 の「複数の手順を持つ書き込み」）。
        // 1 本の変種では 1 周で抜けるので、ここまでの形と変わらない。
        let mut cancel = CancelSignal::Waiting(cancel);
        let mut last = None;
        for (index, command) in commands.into_iter().enumerate() {
            // **手順のあいだに届いた中断は、次を起こさずに返す。** 起こしてから
            // `select!` で拾うと、1 本ぶん git を起こして殺すことになる。
            if index > 0 && cancel.arrived() {
                return Err(ExecError::Canceled { argv: command.argv });
            }
            last = Some(self.run_one(command, &mut cancel).await?);
        }
        // `to_commands` は空の列を返さない。
        last.ok_or_else(|| ExecError::NoResult {
            argv: Vec::new(),
            reason: "git の呼び出しが 1 つもありません".to_string(),
        })
    }

    /// `op` がハンク操作なら、ハンクを読んだ時点の指紋（[`HunkApply::expect`](super::HunkApply::expect)）と
    /// いまの指紋を比べ、外れていれば [`ExecError::Stale`] で断る。`argv` は起こすはずだった
    /// 1 本目で、どちらの失敗にも同じものを載せる。
    ///
    /// **指紋を組むのは `git_read` の [`RepoReader::path_stamp`]**（00 のレイヤーの例外）。
    /// ここで組み直すと、diff の側と組み方が割れて、照合が常に一致か常に不一致に
    /// ずれる。git2 は同期なので `spawn_blocking` に載せる。**ハンク操作のたびに
    /// リポジトリを開く**（コストは 03 の「古いハンクの守り」）。
    ///
    /// **比べてから git がファイルを読むまでの隙間は残る**（03。受け入れた穴）。
    async fn check_fresh(&self, op: &GitOp, argv: &[String]) -> Result<(), ExecError> {
        let Some(hunk) = op.hunk() else {
            return Ok(());
        };
        let (root, path) = (self.repo_root.clone(), hunk.path.clone());
        let unreadable = |reason: String| ExecError::NoResult {
            argv: argv.to_vec(),
            reason: format!("ハンクの対象を確かめられませんでした: {reason}"),
        };
        let now = tokio::task::spawn_blocking(move || {
            RepoReader::open(&root).and_then(|reader| reader.path_stamp(&path))
        })
        .await
        .map_err(|err| unreadable(err.to_string()))?
        .map_err(|err| unreadable(err.to_string()))?;
        if op.stale_at(&now) {
            return Err(ExecError::Stale {
                argv: argv.to_vec(),
                path: hunk.path.clone(),
            });
        }
        Ok(())
    }

    /// 1 本の `git` を実行し、中断の合図が届いたらプロセスグループごと止める。
    async fn run_one(
        &self,
        command: GitCommand,
        cancel: &mut CancelSignal,
    ) -> Result<ExecOutput, ExecError> {
        let mut child = self.spawn(&command)?;

        // **殺す相手はここで控える。** `wait_with_output` が `child` を取るので、
        // 中断が決まってからでは pid を引けない。
        let pid = child.id();

        let pipe = child.stdin.take();
        let write = async {
            if let (Some(mut pipe), Some(bytes)) = (pipe, command.stdin.as_ref()) {
                // 書き込みの失敗は握る。git が先に落ちると BrokenPipe になるが、
                // 本当の原因は git の stderr にある。こちらを返すと隠れる。
                let _ = pipe.write_all(bytes).await;
                let _ = pipe.shutdown().await;
            }
        };
        // stdin への書き込みと終了待ちを同時に進める。順に行うと、パイプのバッファを
        // 超えるパッチで git 側が stdout を書けずに止まり、こちらも書き込みで止まる。
        let run = async {
            let (_, waited) = tokio::join!(write, child.wait_with_output());
            waited
        };
        tokio::pin!(run);

        // `select!` を抜けたところで `&mut run` の借用が切れるので、中断した側は
        // このあと同じ future を待ち直せる。**待ち直すために `Option` に受ける** ——
        // 腕の中で `run.await` すると借用が重なる。
        // **「中断した」を別の旗で持たない。** 持つと「殺したのに待ち直さない」という
        // 組み合わせが型で書ける。
        let mut finished = None;
        tokio::select! {
            waited = &mut run => finished = Some(waited),
            // 送り手が落ちた（キャンセルできない呼び方）なら `wait` は返らない。
            () = cancel.wait() => terminate(pid),
        }
        // **これは「合図を撃った」であって「中断で終わった」ではない**（下の分岐で
        // 終了状態まで確かめる）。
        let signaled = finished.is_none();
        let waited = match finished {
            Some(waited) => waited,
            None => run.await,
        };

        let output = waited.map_err(|err| ExecError::NoResult {
            argv: command.argv.clone(),
            reason: format!("git の終了を待てませんでした: {err}"),
        })?;

        // **「合図を撃った」だけでは中断と決めない。** `select!` は両方 ready なら
        // 無作為に腕を選ぶので、**git が自分の理由で非 0 になったのと `Ctrl-C` が
        // 同じ瞬間に来た**形では、終わっているのに中断の腕が採られる。そのまま
        // `Canceled` にすると argv も stderr も落ち（不変ルール 4）、push では
        // 非 FF の記憶まで消えて force の経路が黙って消える。**SIGTERM で落ちたことを
        // 終了状態で確かめる。**
        //
        // **成功していれば成功として返す。** signal が届く前に git が終わっていた形で、
        // 書き込みの結果はリポジトリに残っている。`Canceled` にすると、済んだ commit を
        // 「中断した」と案内したうえに読み直しの経路まで変わる（`update::Reload` は
        // 中断を「動いたかもしれない」側に置いている）。
        //
        // **中断した出力は読まない。** stderr と exit code は signal で落ちたことの
        // 言い換えにしかならず、案内は「中断した」で足りる。fetch は progress を
        // 数十 KB 吐いてから殺されるので、捨てるものを UTF-8 検証に通さない。
        if canceled_by(signaled, &output.status) {
            return Err(ExecError::Canceled { argv: command.argv });
        }

        let stdout = lossy(output.stdout);
        let stderr = lossy(output.stderr);

        if output.status.success() {
            Ok(ExecOutput {
                argv: command.argv,
                stdout,
                stderr,
            })
        } else {
            // **失敗の出力は端末の制御シーケンスを剥いでから運ぶ。** hook（lefthook など）は
            // 端末でなくても色を付ける。描く側（ratatui）は ESC の 1 文字しか落とさないので、
            // 要約にも `L` の履歴にも `[38;2;56;56;56m` が残る（03 の「理由の行」）。
            let stdout = strip_escapes(stdout);
            let stderr = strip_escapes(stderr);
            Err(ExecError::Failed {
                argv: command.argv,
                code: output.status.code(),
                kind: self.kind_of_failure(&stdout, &stderr),
                stdout,
                stderr,
            })
        }
    }

    /// 失敗の分類。**エディタを起こせなかった失敗だけは、出力ではなく自バイナリの
    /// パスで決める**（理由は 03 の「エラー分類」の `EditorGone`）。
    fn kind_of_failure(&self, stdout: &str, stderr: &str) -> ExecErrorKind {
        // 大文字小文字は無視する。git 2.39 は `There was …` と書く（03）。
        if stderr
            .to_ascii_lowercase()
            .contains("there was a problem with the editor")
            && !self.mewgit_bin.exists()
        {
            ExecErrorKind::EditorGone
        } else {
            classify(stdout, stderr)
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
            // 端末のエディタを起こさない。どちらも自バイナリの隠しサブコマンドで、
            // interactive rebase 以外では何もしないか断る（03）。
            .env("GIT_EDITOR", &self.commit_editor)
            .env("GIT_SEQUENCE_EDITOR", &self.sequence_editor)
            .envs(comment_char_config(std::env::var("GIT_CONFIG_COUNT").ok()))
            .stdin(if command.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // **git を自分のプロセスグループの長にする。** キャンセルはこのグループ
            // 宛てに SIGTERM を送る（[`terminate`]）。git だけに送っても、git が
            // 起こした ssh や hook は孤児として生き残る（実測。03 の「キャンセル」）。
            //
            // **`kill_on_drop` は付けない。** あれが送るのは SIGKILL で、git は
            // sigchain の後始末をせずに死ぬ —— `index.lock` が残り、以後の書き込みが
            // 全部落ちる（実測）。future を捨てるのは mewgit 自身が終わるときだけ
            // なので、そのまま走らせて git に自分で終わらせるほうが安全。
            .process_group(0);
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

/// メッセージの行頭の `#` を、git にコメントとして読ませないためのコメント文字
/// （US、`\x1f`）。メッセージの行頭にまず現れない制御文字を選んだ。
///
/// **`core.commentString`（2.45 以降）ではなく `core.commentChar` に 1 文字で渡す。**
/// 古い git は `commentString` を未知のキーとして読み飛ばし、`commentChar` の 2 文字以上は
/// どのコマンドも断る（2.43.0 で実測）。1 文字なら 2.39.5 / 2.43.0 / 2.47.3 のどれでも
/// `#` の行が残る（03 の「git の最低版」）。
const COMMENT_CHAR: &str = "\x1f";

/// `core.commentChar` を [`COMMENT_CHAR`] にする `GIT_CONFIG_*`。
///
/// **git はエディタを通したメッセージを cleanup にかけ、コメント文字で始まる行を消す。**
/// reword のメッセージ（`--commit-editor` が書く）、squash でつなぐメッセージ、
/// コンフリクトで止まった pick / cherry-pick を continue したときの元のメッセージが
/// どれも通る —— 既定の `#` のままだと `#123 is closed` が黙って消え、`#42` だけの
/// メッセージは空として断られる（`/code-review` が拾った。03 の実測）。git 自身が書く
/// コメント（squash の `This is a combination…` など）も同じ文字で書かれて消えるので、
/// 揃ったまま `#` だけが本文に残る。`--cleanup` は reword に効かない。
///
/// **どのコマンドにも載せる。** エディタと同じで、止まった reword を continue したときも
/// 要る。**`-c` を argv に足さない** —— `L` の履歴と argv の表に毎回ノイズが載る。
/// ユーザーが `GIT_CONFIG_COUNT` を使っていれば、その後ろに足す（上書きしない）。
fn comment_char_config(count: Option<String>) -> [(String, String); 3] {
    let index = count.and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
    [
        (
            format!("GIT_CONFIG_KEY_{index}"),
            "core.commentChar".to_string(),
        ),
        (
            format!("GIT_CONFIG_VALUE_{index}"),
            COMMENT_CHAR.to_string(),
        ),
        ("GIT_CONFIG_COUNT".to_string(), (index + 1).to_string()),
    ]
}

/// `GIT_EDITOR` / `GIT_SEQUENCE_EDITOR` の値。git は `sh -c` で起こし、ファイルの
/// パスを末尾に足す（03 の実測）。
fn editor(mewgit_bin: &std::path::Path, subcommand: &str) -> OsString {
    let mut editor = sh_quote(mewgit_bin.as_os_str());
    editor.push(" ");
    editor.push(subcommand);
    editor
}

/// 中断の合図の受け側。**手順をまたいで 1 本を使い回す**（03 の「複数の手順を
/// 持つ書き込み」）。
///
/// **3 状態を持つ。** tokio の `oneshot::Receiver` は値（か送り手が落ちたこと）を
/// 返したあとに待ち直すと panic するので、終わった受け側は捨てる。**捨てるときに
/// 「届いた」と「もう来ない」を分ける** —— 1 つの `None` に畳むと、合図が届いた
/// その回に git が先に成功していた（`canceled_by` が偽を返す）とき、**次の手順の
/// 前の [`arrived`](Self::arrived) が「届いていない」と答え、止めたはずの手順が
/// 走る**（`/code-review` が拾った）。
enum CancelSignal {
    Waiting(oneshot::Receiver<()>),
    Arrived,
    /// 送り手が落ちた（キャンセルできない呼び方）。
    Closed,
}

impl CancelSignal {
    /// 合図が既に届いているか。待たない。
    fn arrived(&mut self) -> bool {
        if let CancelSignal::Waiting(receiver) = self {
            *self = match receiver.try_recv() {
                Ok(()) => CancelSignal::Arrived,
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Closed) => CancelSignal::Closed,
            };
        }
        matches!(self, CancelSignal::Arrived)
    }

    /// 合図を待つ。**合図が来たときだけ返る** —— 送り手が落ちていれば永久に待つので、
    /// `select!` の腕は git の終了のほうが必ず採られる。届いたあとは何度呼んでも
    /// すぐ返る。
    async fn wait(&mut self) {
        if let CancelSignal::Waiting(receiver) = self {
            *self = match receiver.await {
                Ok(()) => CancelSignal::Arrived,
                Err(_) => CancelSignal::Closed,
            };
        }
        if matches!(self, CancelSignal::Closed) {
            std::future::pending::<()>().await;
        }
    }
}

/// 中断で終わったか。`signaled` は「こちらが合図を撃ったか」。
///
/// **撃っただけでは決めない。** `select!` は両方 ready なら無作為に腕を選ぶので、
/// **git が自分の理由で非 0 になったのと `Ctrl-C` が同じ瞬間に来た**形では、終わって
/// いるのに合図の腕が採られる。そのまま `Canceled` にすると argv も stderr も落ち
/// （不変ルール 4）、push では非 FF の記憶まで消えて force の経路が黙って消える。
///
/// **逆に、こちらが撃っていない SIGTERM は中断ではない**（外から `kill` された）。
/// 押した本人が止めたものだけを `Canceled` にする。
fn canceled_by(signaled: bool, status: &ExitStatus) -> bool {
    signaled && status.signal() == Some(libc::SIGTERM)
}

/// プロセスグループごと SIGTERM を送る。
///
/// **グループ宛て（負の pid）で送る。** git 単体に送ると、git が起こした ssh や
/// hook が孤児として残る（実測。03 の「キャンセル」）。残った ssh は端末を掴んだまま
/// 眠り続け、mewgit を終えても居座る。
///
/// **SIGKILL ではなく SIGTERM。** git は SIGTERM では sigchain の後始末が走り、
/// `index.lock` を消してから終わる。SIGKILL だと残る（実測）。
///
/// `pid` が `None`（既に終わって刈られている）なら何もしない。呼ぶのは
/// [`Runner::run_cancelable`] が `child` を握ったままのあいだだけなので、pid が
/// 別のプロセスに再利用されている窓は無い。
fn terminate(pid: Option<u32>) {
    let Some(pid) = pid else {
        return;
    };
    // SAFETY: `kill` は失敗しても errno を返すだけで、こちらのメモリには触らない。
    // 相手が既に居なければ ESRCH が返るだけ。
    unsafe {
        libc::kill(-(pid as i32), libc::SIGTERM);
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

/// 端末の制御シーケンスを落とす。含まないとき（ふつうの場合）は複製しない。
///
/// - CSI（`ESC [`）: 引数と中間の文字のあと、`@`〜`~` の 1 文字で終わる
/// - OSC（`ESC ]`）: BEL か `ESC \` で終わる。**改行でも打ち切る**（改行は残す）——
///   終わらない OSC が出力の残りを、その後ろの git の `error:` 行ごと食べないように
/// - それ以外（`ESC ( B` など）: 中間の文字（空白〜`/`）のあとの 1 文字で終わる。
///   `tput sgr0` は `ESC ( B ESC [ m` を出す
///
/// **`ESC` の直後の制御文字（改行など）は食べない。** 行がつながる。
fn strip_escapes(text: String) -> String {
    if !text.contains('\x1b') {
        return text;
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                while let Some(c) = chars.next_if(|c| !c.is_control()) {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                while let Some(c) = chars.next_if(|&c| c != '\n') {
                    if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            Some(_) => {
                while chars.next_if(|c| (' '..='/').contains(c)).is_some() {}
                chars.next_if(|c| !c.is_control());
            }
            None => {}
        }
    }
    out
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
///
/// **stdout から、git が写したコミットの行を先に除く**（[`echoes_a_commit`]）。複数の
/// cherry-pick / revert は積んだぶんの `[main <oid>] <件名>` を stdout に並べるので、件名が
/// `fix: nothing to commit` のようなコミットを積んでから別の理由で止まると、語ごとに
/// 取り違える（03 の「エラー分類」）。語ごとに守らず入口で 1 度だけ除く。
fn classify(stdout: &str, stderr: &str) -> ExecErrorKind {
    let stdout = stdout
        .lines()
        .filter(|line| !echoes_a_commit(line))
        .collect::<Vec<_>>()
        .join("\n");
    let stdout = stdout.as_str();
    let has = |needle: &str| stderr.contains(needle) || stdout.contains(needle);
    let starts_a_line = |prefix: &str| {
        stdout
            .lines()
            .chain(stderr.lines())
            .any(|line| line.starts_with(prefix))
    };

    // **印は stderr の行頭でだけ見る。** git はコミットの件名を自分の出力に写す
    // （`error: could not apply <oid>... <件名>`）ので、`has` で探すと件名が
    // `[head-moved] …` のコミットのコンフリクトを取り違える（`/code-review` が踏んだ）。
    // 件名は行の途中にしか出ないので、隠しサブコマンドが書く行頭と合わせて見る。
    if stderr.lines().any(|line| {
        line.strip_prefix(EDITOR_STDERR_PREFIX)
            .is_some_and(|rest| rest.starts_with(HEAD_MOVED_MARKER))
            // 断られた reword を持つ continue の照合（`update-ref --stdin` の `verify`）。
            // **`cannot lock ref` だけで拾わない** —— lock ファイルの取り合いでも出る。
            || line
                .strip_prefix("fatal: cannot lock ref 'HEAD': is at ")
                .is_some_and(|rest| rest.contains(" but expected "))
    }) {
        ExecErrorKind::HeadMoved
    } else if has("SAML SSO") || has("organization has enabled or enforced SAML") {
        ExecErrorKind::SamlSsoRequired
    } else if has("Permission denied (publickey)") {
        ExecErrorKind::SshAuthFailed
    } else if has("Authentication failed") {
        ExecErrorKind::HttpsAuthFailed
    } else if starts_a_line("CONFLICT (") || starts_a_line("error: could not apply") {
        // **コンフリクトの印も行頭で見る。** 複数の cherry-pick は積んだぶんの
        // `[main <oid>] <件名>` を stdout に並べるので、件名が `CONFLICT` を含むコミットの
        // あとで空に当たると、行のどこでも拾う形では `Empty` を取り違える（`/code-review`
        // が踏んだ）。行頭の形は cherry-pick / revert / merge / rebase / pull で同じ
        // （git 2.56.0 で実測）。
        ExecErrorKind::Conflict
    } else if has("is now empty") {
        ExecErrorKind::Empty
    } else if has("nothing to commit")
        || has("nothing added to commit")
        || has("no changes added to commit")
    {
        // 作業ツリーの状態で 3 通りに割れる（clean / untracked がある / tracked が
        // dirty。03 の「revert の開始」）。**`Empty` より後ろ** —— 空で止まった
        // cherry-pick の stdout にも status が出るので、先に見ると進行中を見落とす。
        ExecErrorKind::NoChanges
    } else if stdout.lines().any(|line| line.ends_with(": needs merge"))
        || has("Committing is not possible because you have unmerged files")
    {
        ExecErrorKind::Unmerged
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

/// git が理由のあとに足す締めの行（接頭辞を剥いだ形）。どれも「何が」を言わない ——
/// push の失敗はどの ref がなぜか、リモートの失敗は ssh の何が、cherry-pick / revert の
/// 失敗はどのファイルが、を前の行（か stdout）に書く（03 の「理由の行」）。**選ばない
/// ためだけに照合する** —— git が文言を変えて外れても、この行が理由に選ばれるだけで、
/// 別の行を取り違えはしない。
const WRAP_UPS: [&str; 4] = [
    "failed to push some refs",
    "Could not read from remote repository.",
    "cherry-pick failed",
    "revert failed",
];

/// 失敗の理由の行（03 の「エラー分類」の「理由の行」。[`ExecError::reason`]）。
///
/// stderr が空なら stdout の最後の行（[`classify`] と同じく、git が写したコミットの行は
/// 除く）。stderr があれば ` ! [` の行 → 締め（[`WRAP_UPS`]）ではない最初の `error:` /
/// `fatal:` の行。**git の形をしていない行（hook の文言、ssh の行）は選ばない** ——
/// hook の出力は枠や所要時間の行を持ち、どの行が理由かを形で決められない（lefthook は
/// 1 行目が枠、最後が所要時間）。**行頭は trim する前に見る** —— ファイルの一覧はタブで
/// 字下げされるので、trim してから見ると `error: ` や `! [` で始まるパスを取り違える。
pub(super) fn reason<'a>(stdout: &'a str, stderr: &'a str) -> Option<&'a str> {
    if stderr.trim().is_empty() {
        return stdout
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty() && !echoes_a_commit(line));
    }
    stderr
        .lines()
        .find(|line| line.starts_with(" ! ["))
        .map(str::trim)
        .or_else(|| {
            stderr.lines().find_map(|line| {
                line.strip_prefix("error: ")
                    .or_else(|| line.strip_prefix("fatal: "))
                    .map(str::trim)
                    .filter(|rest| {
                        !rest.is_empty()
                            && !WRAP_UPS.iter().any(|wrap_up| rest.starts_with(wrap_up))
                    })
            })
        })
}

/// git がコミットを積んだときに stdout に写す行か（`[main 1a2b3c4] <件名>`、
/// `[detached HEAD 1a2b3c4] …`、`[main (root-commit) 1a2b3c4] …`）。
///
/// **`[` で始まるだけでは決めない。** パスも `[` で始まりうる —— `[slug].txt` が未解消だと
/// git は `[slug].txt: needs merge` と書く（git 2.56.0 で実測。`core.quotePath` でも
/// 引用されない）。括弧の中の最後の語が oid（16 進）の形だけを写しと読む。
fn echoes_a_commit(line: &str) -> bool {
    line.strip_prefix('[')
        .and_then(|rest| rest.split_once("] "))
        .and_then(|(head, _)| head.rsplit_once(' '))
        .is_some_and(|(_, oid)| oid.len() >= 4 && oid.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reason_is_picked_from_stderr_by_the_shape_of_the_line() {
        // 03 の「エラー分類」の「理由の行」。stderr は git 2.56.0（`LC_ALL=C`）の出力を
        // そのまま写した（同じ節の `Other` の表）。
        let cases: [(&str, &str); 5] = [
            // 最初の `error:` の行。下のパスの字下げはタブ。
            (
                "error: Your local changes to the following files would be overwritten by checkout:\n\t! [x].txt\nPlease commit your changes or stash them before you switch branches.\nAborting\n",
                "Your local changes to the following files would be overwritten by checkout:",
            ),
            // `hint:` は飛ばす。
            (
                "error: the branch 'side' is not fully merged\nhint: If you are sure you want to delete it, run 'git branch -D side'\nhint: Disable this message with \"git config set advice.forceDeleteBranch false\"\n",
                "the branch 'side' is not fully merged",
            ),
            // **最後ではなく最初。** 最後は締めの `cherry-pick failed` / `Could not read from remote`。
            (
                "error: The following untracked working tree files would be overwritten by merge:\n\tg\nPlease move or remove them before you merge.\nAborting\nfatal: cherry-pick failed\n",
                "The following untracked working tree files would be overwritten by merge:",
            ),
            (
                "fatal: 'nope' does not appear to be a git repository\nfatal: Could not read from remote repository.\n\nPlease make sure you have the correct access rights\nand the repository exists.\n",
                "'nope' does not appear to be a git repository",
            ),
            // push は ` ! [` の行。`error:` の `failed to push some refs` はどの ref がなぜかを言わない。
            (
                "remote: protected branch        \nTo /tmp/o.git\n ! [remote rejected] main -> main (pre-receive hook declined)\nerror: failed to push some refs to '/tmp/o.git'\n",
                "! [remote rejected] main -> main (pre-receive hook declined)",
            ),
        ];
        for (stderr, expected) in cases {
            assert_eq!(reason("", stderr), Some(expected), "stderr: {stderr}");
        }
    }

    #[test]
    fn a_line_that_is_not_shaped_like_git_is_never_the_reason() {
        // hook の文言と ssh の行は選ばない（03 の「理由の行」）。締めの行も選ばない。
        // stdout があっても、stderr があれば見ない。
        let cases = [
            // ssh の層の失敗。理由の行に接頭辞が無く、`fatal:` は締めの行だけ。
            "ssh: Could not resolve hostname nonexistent.invalid: nodename nor servname provided, or not known\nfatal: Could not read from remote repository.\n\nPlease make sure you have the correct access rights\nand the repository exists.\n",
            "Warning: Permanently added 'github.com' (ED25519) to the list of known hosts.\nERROR: Repository not found.\nfatal: Could not read from remote repository.\n",
            // pre-push hook に断られた push（hook の文言は stdout 側）。
            "error: failed to push some refs to '/tmp/o.git'\n",
            // lefthook の pre-commit（剥いだあと。1 行目が枠、最後が所要時間）。
            "╭──────╮\n│ 🥊 lefthook v2.1.15 hook: pre-commit │\n╰──────╯\n┃  lint ❯\nsrc/a.rs:1:1: error: unused variable x\n2 errors found\n\nexit status 1\nsummary: (done in 0.02 seconds)\n🥊 lint (0.00 seconds)\n",
            "hint: 何か\n",
        ];
        for stderr in cases {
            assert_eq!(
                reason("lint failed (stdout)\n", stderr),
                None,
                "stderr: {stderr}"
            );
        }
    }

    #[test]
    fn the_reason_falls_back_to_the_last_line_of_stdout_only_when_stderr_is_empty() {
        // 何も stage していない `git commit`（git 2.56.0）。末尾の空行は飛ばす。
        let stdout = "On branch main\nno changes added to commit (use \"git add\" and/or \"git commit -a\")\n\n";
        assert_eq!(
            reason(stdout, ""),
            Some("no changes added to commit (use \"git add\" and/or \"git commit -a\")")
        );
        // git が写したコミットの行は理由ではない（`classify` と同じく除く）。
        assert_eq!(reason("[main 1a2b3c4] fix\n", ""), None);
    }

    #[test]
    fn escapes_are_stripped_and_plain_text_is_kept_as_is() {
        // lefthook の枠（truecolor）と、OSC のハイパーリンク。
        assert_eq!(
            strip_escapes("\x1b[38;2;56;56;56m╭──╮\x1b[m\n\x1b[1mpre-commit\x1b[0m \x1b]8;;https://x\x07link\x1b]8;;\x1b\\\n".to_string()),
            "╭──╮\npre-commit link\n"
        );
        assert_eq!(strip_escapes("plain\n".to_string()), "plain\n");
        // `tput sgr0`（`ESC ( B ESC [ m`）。中間の文字のあとの 1 文字まで落とす。
        assert_eq!(
            strip_escapes("\x1b[31mfail\x1b(B\x1b[m\n".to_string()),
            "fail\n"
        );
        // 終わらない OSC は改行で打ち切る。後ろの git の行を食べない。
        assert_eq!(
            strip_escapes("x\x1b]0;title\nerror: rest\n".to_string()),
            "x\nerror: rest\n"
        );
        // `ESC` の直後の改行も、終わらない CSI の後ろの改行も残す（行がつながらない）。
        assert_eq!(
            strip_escapes("a\x1b\nb\x1b[3\nc\n".to_string()),
            "a\nb\nc\n"
        );
    }

    #[test]
    fn the_comment_char_is_appended_after_the_users_config() {
        assert_eq!(
            comment_char_config(None),
            [
                (
                    "GIT_CONFIG_KEY_0".to_string(),
                    "core.commentChar".to_string()
                ),
                ("GIT_CONFIG_VALUE_0".to_string(), "\x1f".to_string()),
                ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
            ]
        );
        // ユーザーの 0 番と 1 番を残して 2 番に足す。
        assert_eq!(
            comment_char_config(Some("2".into()))[0].0,
            "GIT_CONFIG_KEY_2"
        );
    }

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
    fn an_empty_cherry_pick_is_not_a_conflict() {
        // HEAD の祖先を cherry-pick した形（git 2.55.0 で実測した出力そのまま）。
        // **stdout の `all conflicts fixed` は小文字**なので `CONFLICT` には当たらない。
        // 当たると「git add したら continue」を案内し、continue は同じ理由で断られる。
        let stdout = "On branch main\nYou are currently cherry-picking commit 13100af.\n  (all conflicts fixed: run \"git cherry-pick --continue\")\n  (use \"git cherry-pick --skip\" to skip this patch)\n";
        let stderr = "The previous cherry-pick is now empty, possibly due to conflict resolution.\nIf you wish to commit it anyway, use:\n\n    git commit --allow-empty\n\nOtherwise, please use 'git cherry-pick --skip'\n";
        assert_eq!(classify(stdout, stderr), ExecErrorKind::Empty);
    }

    #[test]
    fn an_empty_cherry_pick_stays_empty_though_its_status_says_nothing_to_commit() {
        // git 2.55.0 で実測した出力そのまま。
        let stdout = "On branch main\nYou are currently cherry-picking commit 5ac5991.\n  (all conflicts fixed: run \"git cherry-pick --continue\")\n  (use \"git cherry-pick --skip\" to skip this patch)\n  (use \"git cherry-pick --abort\" to cancel the cherry-pick operation)\n\nnothing to commit, working tree clean\n";
        let stderr =
            "The previous cherry-pick is now empty, possibly due to conflict resolution.\n";
        assert_eq!(classify(stdout, stderr), ExecErrorKind::Empty);
    }

    #[test]
    fn an_empty_stop_after_a_commit_whose_subject_says_conflict_is_still_empty() {
        // 複数の cherry-pick が 1 本目を積んでから空に当たった形（git 2.56.0 で実測した
        // 出力そのまま）。積んだコミットの件名が stdout に写る。
        let stdout = "[main 68dd71f] fix CONFLICT marker parsing\n Date: Thu Oct 1 01:23:53 2026 +0900\n 1 file changed, 1 insertion(+)\nOn branch main\nYou are currently cherry-picking commit 7a0213e.\n  (all conflicts fixed: run \"git cherry-pick --continue\")\n\nnothing to commit, working tree clean\n";
        let stderr =
            "The previous cherry-pick is now empty, possibly due to conflict resolution.\n";
        assert_eq!(classify(stdout, stderr), ExecErrorKind::Empty);
    }

    #[test]
    fn a_failed_head_check_before_amending_a_refused_reword_is_head_moved() {
        // `update-ref --stdin` の `verify` が外れた 1 行（git 2.56.0 で実測した形）。
        let stderr = "fatal: cannot lock ref 'HEAD': is at 224dc0f106d8df8393093a1636281b693bafeeba but expected b57753cc22b307408ee1039232da80d68d6354bb\n";
        assert_eq!(classify("", stderr), ExecErrorKind::HeadMoved);
        // **lock の取り合いは HEAD が動いたことではない**（同じ `cannot lock ref` で始まる）。
        let contended =
            "fatal: cannot lock ref 'HEAD': Unable to create '/r/.git/HEAD.lock': File exists.\n";
        assert_eq!(classify("", contended), ExecErrorKind::Other);
        // 件名に同じ字を含むコミットの出力は、行頭ではないので拾わない。
        let subject = "error: could not apply abc1234... fatal: cannot lock ref 'HEAD': is at x but expected y\n";
        assert_eq!(classify("", subject), ExecErrorKind::Conflict);
    }

    #[test]
    fn a_sequence_editor_refusing_a_moved_head_is_head_moved() {
        // 隠しサブコマンドが書く 1 行（`mewgit: <Display>`）に、git が自分の行を並べる。
        // **印は `Display` から採る** —— 文言を直しても分類が外れないことを見る。
        use crate::git_exec::EditorError;
        use crate::test_util::fake_oid;

        let err = EditorError::HeadMoved {
            expected: fake_oid(7),
            actual: fake_oid(0).to_string(),
        };
        let stderr = format!(
            "{EDITOR_STDERR_PREFIX}{err}\nerror: there was a problem with the editor 'mewgit --sequence-editor'\n"
        );
        assert_eq!(classify("", &stderr), ExecErrorKind::HeadMoved);
    }

    #[test]
    fn a_vanished_editor_is_recognized_in_either_case_git_writes_it() {
        let runner = Runner::new(
            "/nonexistent",
            "git",
            SshCommand::default(),
            "/nonexistent/mewgit",
        );
        // 2.47 以降は小文字、2.39.5 は大文字で書く（03 の「git の最低版」）。
        for stderr in [
            "error: there was a problem with the editor ''/x/mewgit' --sequence-editor'\n",
            "error: There was a problem with the editor ''/x/mewgit' --sequence-editor'.\n",
        ] {
            assert_eq!(
                runner.kind_of_failure("", stderr),
                ExecErrorKind::EditorGone,
                "{stderr}"
            );
        }
    }

    #[test]
    fn a_subject_that_looks_like_the_marker_is_still_a_conflict() {
        // git は件名を出力に写す（git 2.56.0 で実測した形）。印を行のどこでも拾うと、
        // コンフリクトで止まった rebase に「i から開き直して」と言う。
        let stdout = "Could not apply 9799162... # [head-moved] tweak\n";
        let stderr = "error: could not apply 9799162... [head-moved] tweak\nhint: Resolve all conflicts manually\n";
        assert_eq!(classify(stdout, stderr), ExecErrorKind::Conflict);
        // 件名が行頭に来る出力（hook が件名を写すなど）でも、隠しサブコマンドの接頭辞が無い。
        assert_eq!(classify("", "[head-moved] tweak\n"), ExecErrorKind::Other);
    }

    #[test]
    fn a_revert_with_nothing_to_undo_is_no_changes_in_every_worktree_state() {
        // 1 本だけの revert で変更が空になった形。stderr は空で、stdout の結びが
        // 作業ツリーの状態で割れる（git 2.55.0 で実測。03 の「revert の開始」）。
        for stdout in [
            "On branch main\nnothing to commit, working tree clean\n",
            "On branch main\nUntracked files:\n  (use \"git add <file>...\" to include in what will be committed)\n\tu\n\nnothing added to commit but untracked files present (use \"git add\" to track)\n",
            "On branch main\nChanges not staged for commit:\n\tmodified:   a\n\nno changes added to commit (use \"git add\" and/or \"git commit -a\")\n",
        ] {
            assert_eq!(classify(stdout, ""), ExecErrorKind::NoChanges, "{stdout}");
        }
    }

    #[test]
    fn unresolved_files_are_unmerged_whichever_stream_says_so() {
        // 未解消のファイルを残したまま書き込んだ形（git 2.56.0 で実測。03 の「エラー分類」）。
        // stash の 3 つは stdout に理由、stderr に `could not write index` だけ。
        assert_eq!(
            classify("b: needs merge\n", "error: could not write index\n"),
            ExecErrorKind::Unmerged
        );
        assert_eq!(
            classify(
                "b: needs merge\nThe stash entry is kept in case you need it again.\n",
                "error: could not write index\n"
            ),
            ExecErrorKind::Unmerged
        );
        // **パスが `[` で始まっても**写しの行と読まない（`/code-review` が踏んだ）。
        assert_eq!(
            classify(
                "[slug].txt: needs merge\n",
                "error: could not write index\n"
            ),
            ExecErrorKind::Unmerged
        );
        // rebase の continue は stderr が空。
        assert_eq!(
            classify(
                "a: needs merge\nYou must edit all merge conflicts and then\nmark them as resolved using git add\n",
                ""
            ),
            ExecErrorKind::Unmerged
        );
        // cherry-pick / revert の continue と commit は stderr に理由。
        assert_eq!(
            classify(
                "U\ta\n",
                "error: Committing is not possible because you have unmerged files.\nhint: Fix them up in the work tree, and then use 'git add/rm <file>'\nhint: as appropriate to mark resolution and make a commit.\nfatal: Exiting because of an unresolved conflict.\n"
            ),
            ExecErrorKind::Unmerged
        );
    }

    #[test]
    fn a_subject_echoed_by_git_is_not_read_as_the_reason() {
        // 複数の cherry-pick がコミットを積んでから、`-m` の無い merge で止まった形（stderr は
        // 03 の「複数のコミットの cherry-pick / revert」の実測）。積んだコミットの件名が
        // stdout に写る。
        let stderr = "error: commit 5d6e7f8 is a merge but no -m option was given.\nfatal: cherry-pick failed\n";
        for subject in [
            "fix: needs merge",
            "fix: nothing to commit",
            "x is now empty",
        ] {
            for head in [
                "main 1a2b3c4",
                "detached HEAD 93fb6f8",
                "main (root-commit) 1a2b3c4",
            ] {
                let stdout = format!("[{head}] {subject}\n 1 file changed, 1 insertion(+)\n");
                assert_eq!(
                    classify(&stdout, stderr),
                    ExecErrorKind::Other,
                    "{head} {subject}"
                );
            }
        }
    }

    #[test]
    fn each_alternative_of_a_pattern_pair_is_enough() {
        // 実際の git / GitHub の文面は両方の語を含むことが多い。片方だけを含む
        // 入力（一部は作り物）を通して、選択肢の両側が生きていることを見る。
        //
        // **`Conflict` の 2 択はここには無い。** `CONFLICT` 側は
        // `merge_conflict_is_found_on_stdout` が、`could not apply` 側は
        // `classifies_by_pattern` が既に片方だけの入力で踏んでいる。
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

    /// wait(2) の終了状態。下位 7 bit が signal、その上が exit code。
    fn status(raw: i32) -> ExitStatus {
        ExitStatus::from_raw(raw)
    }

    #[tokio::test]
    async fn a_cancel_seen_while_waiting_is_still_seen_before_the_next_step() {
        // 合図を `wait` で受けたが git が先に成功していた回（`canceled_by` が偽）。
        // 次の手順の前の `arrived` がそれを憶えていないと、止めた手順が走る。
        let (send, receive) = oneshot::channel();
        let mut cancel = CancelSignal::Waiting(receive);
        send.send(()).expect("送れませんでした");
        // **時間を区切る。** 届いた合図を「もう来ない」に畳む形では `wait` が永久に
        // 待つので、落ちる代わりに固まる。
        tokio::time::timeout(std::time::Duration::from_secs(5), cancel.wait())
            .await
            .expect("届いた合図で返りませんでした");
        assert!(cancel.arrived());
        assert!(cancel.arrived(), "2 回目も届いたまま");

        // 送り手が落ちたら届かない。
        let (send, receive) = oneshot::channel::<()>();
        let mut cancel = CancelSignal::Waiting(receive);
        drop(send);
        assert!(!cancel.arrived());
    }

    #[test]
    fn only_our_own_sigterm_counts_as_a_cancel() {
        // 撃った合図で SIGTERM に落ちた。これが中断。
        assert!(canceled_by(true, &status(libc::SIGTERM)));

        // **合図と同じ瞬間に git が自分の理由で落ちた形。** `select!` はどちらも
        // ready なら無作為に腕を選ぶので起こりうる。`Canceled` にすると argv も
        // stderr も落ち、push では非 FF の記憶まで消える。
        assert!(!canceled_by(true, &status(1 << 8)));

        // 撃っていない SIGTERM（外から kill された）は中断ではない。
        assert!(!canceled_by(false, &status(libc::SIGTERM)));
    }
}
