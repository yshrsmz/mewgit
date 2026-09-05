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
    /// リポジトリを開くスレッドを起こす。
    ///
    /// `open` はスレッドの中で行い、結果だけを待つ。失敗（bare、存在しないパス）は
    /// ここで返るので、呼び出し側は起動時に気付ける。
    pub async fn spawn(path: PathBuf) -> Result<Self, GitReadError> {
        let (tx, rx) = mpsc::unbounded_channel();
        let (ready_tx, ready_rx) = oneshot::channel();

        // スレッドを作れないのはプロセスが立ち上がれない状態なので、`GitReadError` に
        // 混ぜずここで落とす（`std::thread::spawn` の既定の挙動と同じ）。名前を付けるのは
        // デバッガやプロファイラでどのスレッドか分かるようにするため。
        std::thread::Builder::new()
            .name("mewgit-reader".to_string())
            .spawn(move || reader_loop(&path, rx, ready_tx))
            .expect("リーダースレッドを起動できませんでした");

        let workdir = ready_rx.await.map_err(|_| GitReadError::ReaderGone)??;
        Ok(Self { tx, workdir })
    }

    /// ワークツリーのルート。`open` が確定させた値で、引数に渡したパスとは限らない。
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// ログの続きを最大 `count` 件読む。
    ///
    /// セッションはスレッド側が持ち回る。呼ぶたびに続きが返り、先頭からは読み直さない。
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
    /// 送信側・受信側のどちらが切れても [`GitReadError::ReaderGone`] にする。スレッドが
    /// 落ちたときに待ち続けると、`JobTracker` に登録されたジョブが永久に残る。
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

/// スレッドの本体。`RepoReader` と [`LogSession`] をローカル変数として持つ。
///
/// `LogSession<'repo>` は `RepoReader` を借りるので同じ構造体には入れられない
/// （自己参照になる）。ここでは `reader` の隣に置くことで、借用のまま持ち回れる。
fn reader_loop(
    path: &Path,
    mut rx: mpsc::UnboundedReceiver<Request>,
    ready: oneshot::Sender<Result<PathBuf, GitReadError>>,
) {
    let reader = match RepoReader::open(path) {
        Ok(reader) => reader,
        Err(err) => {
            let _ = ready.send(Err(err));
            return;
        }
    };
    if ready.send(Ok(reader.workdir().to_path_buf())).is_err() {
        // 呼び出し側が待つのをやめた。要求は来ないので畳む。
        return;
    }

    let mut session: Option<LogSession<'_>> = None;

    // 送信側（すべての `ReaderHandle`）が落ちたらループを抜けてスレッドを終える。
    while let Some(request) = rx.blocking_recv() {
        // 返り先が落ちていても続ける。要求元のタスクが先に消えただけで、
        // 他のハンドルはまだ生きている。
        match request {
            Request::LogPage { count, reply } => {
                let _ = reply.send(next_page(&reader, &mut session, count));
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
/// セッションを一度取り出し、成功したときだけ戻す。`next_page` が `Err` を返したら
/// 捨てるという 02 の契約を、型ではなくこの 1 箇所で守る。取り出し済みの Oid が
/// 失われたまま読み進めると、どのページにも現れないコミットが出る。
fn next_page<'repo>(
    reader: &'repo RepoReader,
    session: &mut Option<LogSession<'repo>>,
    count: usize,
) -> Result<LogPage, GitReadError> {
    let mut live = match session.take() {
        Some(live) => live,
        None => reader.log()?,
    };

    let page = live.next_page(count)?;
    *session = Some(live);
    Ok(page)
}
