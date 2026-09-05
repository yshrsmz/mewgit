//! コミットログの列挙。設計は `docs/design/02-git-read.md` の「ログ列挙」。

use std::iter::Peekable;

use git2::{Repository, Revwalk, Sort};

use super::{lossy, Commit, GitReadError, LogPage, Oid, RepoReader, Signature};

impl RepoReader {
    /// ログ列挙のセッションを開く。
    ///
    /// 位置は返り値の [`LogSession`] が持つ。トポロジカル順の walk は途中の Oid から
    /// やり直せない（他のブランチとの交互配置が失われる）ので、続きを読むには同じ
    /// セッションを使い回す。
    pub fn log(&self) -> Result<LogSession<'_>, GitReadError> {
        LogSession::new(&self.repo)
    }
}

/// 進行中のログ列挙。[`next_page`](LogSession::next_page) を呼ぶたびに続きを返す。
///
/// `Revwalk` は `Repository` を借りるので、`RepoReader` と同じ構造体には入れられない
/// （自己参照になる）。リーダースレッドでは `RepoReader` の隣にローカル変数として置く。
pub struct LogSession<'repo> {
    repo: &'repo Repository,
    /// 1 つ先読みできる形で持つ。「ちょうど n 件で終わった」ページに `done: true` を
    /// 立てるため。先読みしないと、続きが無いのにもう 1 回空のページを読むことになる。
    walk: Peekable<Revwalk<'repo>>,
}

impl<'repo> LogSession<'repo> {
    fn new(repo: &'repo Repository) -> Result<Self, GitReadError> {
        let mut walk = repo.revwalk()?;
        walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
        // リモート追跡ブランチも入れる。ローカルより進んでいる `origin/main` の行が
        // 出ないと、`RefIndex` が作った `{origin/main}` のラベルを載せる行が無くなる。
        //
        // `push_ref` ではなく `push_glob` を使う。glob は commit まで peel できない ref
        // （tree を指すタグなど）を黙って飛ばすので、その 1 本のために列挙全体が失敗しない。
        for glob in ["refs/heads/*", "refs/remotes/*", "refs/tags/*"] {
            walk.push_glob(glob)?;
        }
        // detached HEAD だけはどの ref 名前空間にも無いので個別に push する。ブランチに
        // 乗っているときは `refs/heads/*` で既に入っている。
        //
        // `push_head` を無条件に呼ばないのは、コミットが 1 つも無いリポジトリで返る
        // エラーが `GenericError`（"reference 'refs/heads/main' not found"）で、握りつぶす
        // 対象を絞り込めないため。`head_detached` は HEAD が symbolic なら解決しないので、
        // unborn はそもそもこの分岐に入らない。代わりに `refs/heads/*` の外を指す symbolic
        // HEAD が押されなくなるが、git が作らない状態なので許容する。
        if repo.head_detached()? {
            walk.push(repo.head()?.peel_to_commit()?.id())?;
        }

        Ok(Self {
            repo,
            walk: walk.peekable(),
        })
    }

    /// 続きを最大 `n` 件読む。
    pub fn next_page(&mut self, n: usize) -> Result<LogPage, GitReadError> {
        let mut commits = Vec::with_capacity(n);
        while commits.len() < n {
            let Some(oid) = self.walk.next().transpose()? else {
                break;
            };
            commits.push(self.read_commit(oid)?);
        }

        // 先読みが `Err` なら「まだ続きがある」に倒す。エラーは次の `next_page` が返す。
        let done = self.walk.peek().is_none();
        Ok(LogPage { commits, done })
    }

    /// グラフ 1 行分だけを取り出す。本文とファイル一覧は選択時に遅延取得する。
    fn read_commit(&self, oid: Oid) -> Result<Commit, GitReadError> {
        let commit = self.repo.find_commit(oid)?;
        let author = commit.author();
        Ok(Commit {
            oid,
            parents: commit.parent_ids().collect(),
            // summary と署名は表示にしか使わないので、非 UTF-8 でも行ごと落とさず
            // lossy で残す（02 の「非 UTF-8 の扱い」）。`summary_bytes` はメッセージが
            // 空のときに `None` を返す。
            summary: lossy(commit.summary_bytes().unwrap_or_default()),
            author: Signature {
                name: lossy(author.name_bytes()),
                email: lossy(author.email_bytes()),
            },
            time: author.when().seconds(),
        })
    }
}
