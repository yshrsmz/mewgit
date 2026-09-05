//! `RepoReader` を専用スレッドに所有させる。設計は `docs/design/02-git-read.md` の「スレッド」。

use std::path::{Path, PathBuf};

use tokio::sync::{mpsc, oneshot};

use super::{GitReadError, HeadInfo, LogPage, LogSession, RefIndex, Remote, RepoReader};

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
    LogPage { count: usize, reply: Reply<LogPage> },
    Refs(Reply<RefIndex>),
    Head(Reply<HeadInfo>),
    Remotes(Reply<Vec<Remote>>),
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
    pub async fn log_page(&self, count: usize) -> Result<LogPage, GitReadError> {
        self.request(|reply| Request::LogPage { count, reply })
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

    /// リモート一覧。
    pub async fn remotes(&self) -> Result<Vec<Remote>, GitReadError> {
        self.request(Request::Remotes).await
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

/// ログ列挙の状態。`RepoReader` の隣にローカル変数として置く。
///
/// `LogSession<'repo>` は `RepoReader` を借りるので、同じ構造体には入れられない
/// （自己参照になる）。
enum Log<'repo> {
    /// まだ開いていない。最初の要求で開く。
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
        // 他のハンドルはまだ生きている。
        match request {
            Request::LogPage { count, reply } => {
                let _ = reply.send(next_page(&reader, &mut log, count));
            }
            Request::Refs(reply) => {
                let _ = reply.send(reader.refs());
            }
            Request::Head(reply) => {
                let _ = reply.send(reader.head());
            }
            Request::Remotes(reply) => {
                let _ = reply.send(reader.remotes());
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
    let mut session = match std::mem::replace(log, Log::Idle) {
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
