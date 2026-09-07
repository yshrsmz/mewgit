//! `RepoReader` を専用スレッドに所有させる。設計は `docs/design/02-git-read.md` の「スレッド」。

use std::path::{Path, PathBuf};

use tokio::sync::{mpsc, oneshot};

use super::{
    CommitDetail, DiffTarget, FileDiff, FileEntry, GitReadError, HeadInfo, LogPage, LogSession,
    Oid, RefIndex, Remote, RepoReader, WorktreeSnapshot,
};

/// リーダースレッドへの窓口。
///
/// `git2::Repository` は `Send` だが `Sync` ではないので、`RepoReader` は 1 本のスレッドに
/// 置き、要求をチャネルで送って結果を受け取る。`spawn_blocking` で毎回 open し直さない
/// のは、[`LogSession`] がスレッドのローカル変数として生き続ける必要があるため
/// （トポロジカル順の walk は途中からやり直せない。02 の「ログ列挙」）。
///
/// `Clone` できる。ランタイムは `Cmd` ごとに tokio タスクを起こし、その中から呼ぶ。
#[derive(Debug, Clone)]
pub struct ReaderHandle {
    tx: mpsc::UnboundedSender<Request>,
    /// `open` が確定させたワークツリーのルート。スレッドに聞き直さずに済むよう、
    /// 起動時に 1 回だけ受け取って持つ。
    workdir: PathBuf,
}

/// スレッドに送る要求。返り先の `oneshot` を各変種が持つ。
///
/// `Cmd` / `Msg` と並ぶ 3 本目の公開語彙にしないため非公開にし、外からは
/// [`ReaderHandle`] の型付きメソッドだけを見せる。
enum Request {
    LogPage {
        count: usize,
        /// 進行中のセッションを捨てて先頭から読み直す（02 の「スレッド」）。
        restart: bool,
        reply: Reply<LogPage>,
    },
    /// 状態を持たないので、返り先が消えても失うものは無い（M3 のキャンセルで
    /// abort してよい側。`LogPage` だけが例外）。
    CommitDetail {
        oid: Oid,
        reply: Reply<CommitDetail>,
    },
    CommitFiles {
        oid: Oid,
        reply: Reply<Vec<FileEntry>>,
    },
    Refs(Reply<RefIndex>),
    Head(Reply<HeadInfo>),
    HeadMessage(Reply<Option<String>>),
    Remotes(Reply<Vec<Remote>>),
    Status(Reply<WorktreeSnapshot>),
    Diff {
        target: DiffTarget,
        reply: Reply<Vec<FileDiff>>,
    },
}

/// 要求 1 件の返り先。
type Reply<T> = oneshot::Sender<Result<T, GitReadError>>;

impl ReaderHandle {
    /// リポジトリを開き、それを持つスレッドを起こす。
    ///
    /// `open` は呼び出し側のスレッドで行う。`git2::Repository` は `Send` なので、開いた
    /// ものをそのままスレッドへ渡せる。bare や存在しないパスはここで `Err` になるので、
    /// 呼び出し側は起動時に気付ける。`discover` の分だけ呼び出し側が止まるが、起動時の
    /// 1 回だけなのでそのままにしている。
    pub fn spawn(path: &Path) -> Result<Self, GitReadError> {
        let reader = RepoReader::open(path)?;
        let workdir = reader.workdir().to_path_buf();
        let (tx, rx) = mpsc::unbounded_channel();

        // スレッドを作れないのはプロセスが立ち上がれない状態なので、`GitReadError` に
        // 混ぜずここで落とす（`std::thread::spawn` の既定の挙動と同じ）。名前を付けるのは
        // デバッガやプロファイラでどのスレッドか分かるようにするため。
        std::thread::Builder::new()
            .name("mewgit-reader".to_string())
            .spawn(move || reader_loop(reader, rx))
            .expect("リーダースレッドを起動できませんでした");

        Ok(Self { tx, workdir })
    }

    /// ワークツリーのルート。`open` が確定させた値で、引数に渡したパスとは限らない。
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// ログの続きを最大 `count` 件読む。
    ///
    /// セッションはスレッド側が持ち回る。呼ぶたびに続きが返り、先頭からは読み直さない。
    /// 読み切ったあとは空のページを `done` 付きで返す。
    ///
    /// **`Err` を返したあとの次の呼び出しは先頭から読み直す。** 失敗したセッションは
    /// 捨てるしかない（02 の「ログ列挙」）ので、エラーを通知として出しつつスクロールを
    /// 続けさせると 1 ページ目が二重に積まれる。呼び出し側は積んだ行を捨ててから
    /// 読み直すこと。
    ///
    /// **この future は cancel-safe ではない。** 完了前に drop すると、スレッド側は
    /// walk を進めたあとで返り先を失い、そのページのコミットはどのページにも現れなく
    /// なる（グラフに親の見つからない行が残る）。ランタイムはログ読み込みのタスクを
    /// abort しないこと。M3 で入れるキャンセルは `Cmd::Exec`（git CLI の実行）に効かせる。
    /// **`restart` はセッションを捨ててから読む。** commit や fetch で HEAD が動くと
    /// 積んだ行が丸ごと古くなり、続きを読む経路では直せない（新しいコミットは末尾では
    /// なく先頭に付く）。別のメソッドに分けないのは、「セッションを取り出して成功した
    /// ときだけ戻す」契約を 2 箇所で守ることにならないようにするため（02）。
    pub async fn log_page(&self, count: usize, restart: bool) -> Result<LogPage, GitReadError> {
        self.request(|reply| Request::LogPage {
            count,
            restart,
            reply,
        })
        .await
    }

    /// コミット 1 件の詳細。グラフで選択されたときに読む（02 の「コミットの詳細」）。
    ///
    /// `log_page` と違って状態を持たないので、この future は drop してよい。
    pub async fn commit_detail(&self, oid: Oid) -> Result<CommitDetail, GitReadError> {
        self.request(|reply| Request::CommitDetail { oid, reply })
            .await
    }

    /// コミット 1 件のファイル一覧（02 の「コミットのファイル一覧」）。
    ///
    /// `commit_detail` と分けているのは、こちらがツリー同士の比較でより重いため。
    /// 1 通にまとめると、本文しか見えていない時点でも一覧のコストを払う（01）。
    pub async fn commit_files(&self, oid: Oid) -> Result<Vec<FileEntry>, GitReadError> {
        self.request(|reply| Request::CommitFiles { oid, reply })
            .await
    }

    /// ref の索引（Oid → ラベル）。
    pub async fn refs(&self) -> Result<RefIndex, GitReadError> {
        self.request(Request::Refs).await
    }

    /// HEAD の状態。
    pub async fn head(&self) -> Result<HeadInfo, GitReadError> {
        self.request(Request::Head).await
    }

    /// HEAD のコミットメッセージ全文。コミットが 1 つも無ければ `None`。
    ///
    /// amend の初期値に使う。`head` と `commit_detail` に分けないのは、そのあいだに
    /// HEAD が動く隙を作らないため（[`RepoReader::head_message`]）。
    pub async fn head_message(&self) -> Result<Option<String>, GitReadError> {
        self.request(Request::HeadMessage).await
    }

    /// リモート一覧。
    pub async fn remotes(&self) -> Result<Vec<Remote>, GitReadError> {
        self.request(Request::Remotes).await
    }

    /// ワーキングツリーの状態。`commit_detail` と同じく状態を持たないので、この
    /// future は drop してよい。
    pub async fn status(&self) -> Result<WorktreeSnapshot, GitReadError> {
        self.request(Request::Status).await
    }

    /// 対象の diff。`status` と同じく drop してよい。
    pub async fn diff(&self, target: DiffTarget) -> Result<Vec<FileDiff>, GitReadError> {
        self.request(|reply| Request::Diff { target, reply }).await
    }

    /// 要求を送って結果を待つ。
    ///
    /// 送信側・受信側のどちらのチャネルが切れても [`GitReadError::ReaderGone`] にする。
    /// 待ち続けると、この呼び出しを包む非同期タスクが二度と完了しない。
    async fn request<T>(
        &self,
        into_request: impl FnOnce(Reply<T>) -> Request,
    ) -> Result<T, GitReadError> {
        let (reply, result) = oneshot::channel();
        self.tx
            .send(into_request(reply))
            .map_err(|_| GitReadError::ReaderGone)?;
        result.await.map_err(|_| GitReadError::ReaderGone)?
    }
}

/// 進行中の walk。`RepoReader` の隣にローカル変数として置く。
///
/// `LogSession<'repo>` は `RepoReader` を借りるので、同じ構造体には入れられない
/// （自己参照になる）。
#[derive(Default)]
enum Log<'repo> {
    /// まだ開いていない。最初の要求で開く。
    #[default]
    Idle,
    Live(LogSession<'repo>),
    /// 読み切った。`Live` のまま持ち続けると、libgit2 が walk のために読んだコミットの
    /// プールがプロセスの寿命だけ残る（`git_revwalk_free` まで解放されない）。かといって
    /// `Idle` に戻すと、末尾までスクロールしたあとの 1 回で履歴全体を読み直してしまう。
    Exhausted,
}

/// スレッドの本体。`RepoReader` と [`Log`] を持ち、要求を 1 件ずつ処理する。
fn reader_loop(reader: RepoReader, mut rx: mpsc::UnboundedReceiver<Request>) {
    let mut log = Log::Idle;

    // 送信側（すべての `ReaderHandle`）が落ちたらループを抜けてスレッドを終える。
    while let Some(request) = rx.blocking_recv() {
        // 返り先が落ちていても続ける。要求元のタスクが先に消えただけで、
        // 他のハンドルはまだ生きている。`LogPage` だけは読み進めたページが失われるが、
        // それは要求元を捨てないことで防ぐ（[`ReaderHandle::log_page`] の注記）。
        match request {
            Request::LogPage {
                count,
                restart,
                reply,
            } => {
                // 捨てるのはここで 1 回だけ。`next_page` は「取り出して成功したときだけ
                // 戻す」ことに専念する。
                if restart {
                    log = Log::Idle;
                }
                let _ = reply.send(next_page(&reader, &mut log, count));
            }
            Request::CommitDetail { oid, reply } => {
                let _ = reply.send(reader.commit_detail(oid));
            }
            Request::CommitFiles { oid, reply } => {
                let _ = reply.send(reader.commit_files(oid));
            }
            Request::Refs(reply) => {
                let _ = reply.send(reader.refs());
            }
            Request::Head(reply) => {
                let _ = reply.send(reader.head());
            }
            Request::HeadMessage(reply) => {
                let _ = reply.send(reader.head_message());
            }
            Request::Remotes(reply) => {
                let _ = reply.send(reader.remotes());
            }
            Request::Status(reply) => {
                let _ = reply.send(reader.status());
            }
            Request::Diff { target, reply } => {
                let _ = reply.send(reader.diff(target));
            }
        }
    }
}

/// セッションを使い回してログの続きを読む。無ければここで開く。
///
/// セッションを一度取り出し、続きがあるときだけ戻す。`next_page` が `Err` を返したら
/// 捨てるという 02 の契約を、型ではなくこの 1 箇所で守る。取り出し済みの Oid が
/// 失われたまま読み進めると、どのページにも現れないコミットが出る。
fn next_page<'repo>(
    reader: &'repo RepoReader,
    log: &mut Log<'repo>,
    count: usize,
) -> Result<LogPage, GitReadError> {
    // 取り出した時点で `Idle` に落としておく。`?` で戻るときはセッションが捨てられる。
    let mut session = match std::mem::take(log) {
        Log::Live(session) => session,
        Log::Idle => reader.log()?,
        Log::Exhausted => {
            *log = Log::Exhausted;
            return Ok(LogPage {
                commits: Vec::new(),
                done: true,
            });
        }
    };

    let page = session.next_page(count)?;
    *log = if page.done {
        Log::Exhausted
    } else {
        Log::Live(session)
    };
    Ok(page)
}
