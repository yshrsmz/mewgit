//! リポジトリを開いて ref / リモート / HEAD を読む。設計は `docs/design/02-git-read.md`。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use git2::{ErrorCode, Reference, ReferenceType, Repository};

use super::{
    lossy, signature, Branch, CommitDetail, GitReadError, HeadInfo, Oid, RefIndex, RefLabel,
    Remote, Upstream, UpstreamConfig,
};

/// git2 による読み取り専用のリポジトリハンドル。
///
/// `git2::Repository` は `Send` だが `Sync` ではないので、`app` からは専用スレッド越しに
/// 使う（02 の「スレッド」節）。ここはその内側の同期 API。
pub struct RepoReader {
    /// `git_read` の他のサブモジュール（log / diff / status）も読む。守る不変条件が
    /// 無いのでアクセサは挟まない。
    pub(super) repo: Repository,
    workdir: PathBuf,
}

/// `git2::Repository` が `Debug` を実装していないので手で書く。`Result<RepoReader, _>` を
/// `unwrap_err` するテストとログ出力のために `Debug` 自体は要る。`AppState` は
/// `RepoReader` を持たない（02 の「スレッド」節のとおり `app` には handle を渡す）。
impl std::fmt::Debug for RepoReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepoReader")
            .field("workdir", &self.workdir)
            .finish_non_exhaustive()
    }
}

/// [`RepoReader::tracking`] の戻り値。
///
/// **3 つを 1 つの値にまとめる。** どれも同じ「追跡 ref が引けたか」で決まり、
/// タプルで返すと呼び出し側が並び順を覚えることになる。既定は「比べる相手がいない」。
#[derive(Default)]
struct Tracking {
    upstream: Option<Upstream>,
    ahead: usize,
    behind: usize,
}

impl RepoReader {
    /// リポジトリを開く。サブディレクトリを渡されても上に辿って探す。
    ///
    /// bare リポジトリは拒否する。status / diff / checkout がいずれもワークツリー前提で、
    /// 開けても何もできないため。
    pub fn open(path: &Path) -> Result<Self, GitReadError> {
        let repo = Repository::discover(path).map_err(|source| GitReadError::Open {
            path: path.to_path_buf(),
            source,
        })?;
        // 見つかったリポジトリのパスを添える。引数のサブディレクトリを返すと
        // 「そのディレクトリが bare だ」と読めてしまう。
        let workdir = repo
            .workdir()
            .ok_or_else(|| GitReadError::BareRepository {
                path: repo.path().components().collect(),
            })?
            // libgit2 は末尾に区切りを付けた `/…/repo/` を返す。そのまま持つと
            // 表示が `mewgit/` になり、設定や引数のパスとの文字列比較も外れる。
            .components()
            .collect();

        Ok(Self { repo, workdir })
    }

    /// ワークツリーのルート。`open` に渡したパスではなく、実際に見つかった方を返す。
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// `core.sshCommand` の値。設定されていなければ `None`。
    ///
    /// **読むのは起動時の 1 回だけ。** `Runner` がここから受けた値に
    /// `-o BatchMode=yes` を重ねる（03 の「BatchMode の重ね方」）。実行のたびに
    /// 読み直さないので、mewgit を起動したまま設定を変えても効かない。
    ///
    /// **`Err` を返さない。** config を読めないのは「設定されていない」と区別しても
    /// 呼び出し側の分岐が同じ（既定の `ssh` を使う）で、起動を止める理由にはならない。
    pub fn ssh_command(&self) -> Option<String> {
        let config = self.repo.config().ok()?;
        config.get_string("core.sshCommand").ok()
    }

    /// リモート一覧。`url` は `.git/config` の生の値で、host / owner / repo の解析は
    /// `auth` の担当（00 の依存の向き）。
    ///
    /// **`head_branch` も一緒に読む**（02 の「remotes」）。同じ `Cmd::LoadRepoInfo` に
    /// 乗るので、読み手（`Modal::PrCreate` の base）が別のジョブの到着を待たずに済む。
    pub fn remotes(&self) -> Result<Vec<Remote>, GitReadError> {
        let names = self.repo.remotes()?;
        let mut remotes = Vec::with_capacity(names.len());

        for name in names.iter() {
            // 名前が非 UTF-8 のリモートは `git_exec` の argv にも載せられないので落とす。
            // git2 0.21 から `Err`（非 UTF-8）と `Ok(None)`（NULL 要素）が分かれたが、
            // どちらも「名前として使えない」なので同じく落とす。
            let Ok(Some(name)) = name else { continue };
            let remote = self.repo.find_remote(name)?;
            // URL は表示と `auth` の解析にしか使わないので、非 UTF-8 でもリモート自体は
            // 消さずに lossy で残す。
            let url = lossy(remote.url_bytes());
            let head_branch = self.remote_head_branch(name);
            remotes.push(Remote {
                name: name.to_string(),
                url,
                head_branch,
            });
        }

        remotes.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(remotes)
    }

    /// `refs/remotes/<name>/HEAD` が指すブランチの短縮名。
    ///
    /// **`refs/remotes/<name>/` を剥がして返す。** 生の `refs/remotes/origin/main` は
    /// ブランチ名ではないので、そのまま PR の base に載せると GitHub が 422 を返す
    /// （02 の「remotes」）。**剥がせない値は `None`** —— symref が
    /// `refs/heads/...` のような別の場所を指している形で、ブランチ名として使えない。
    ///
    /// **`Err` を返さない。** symref が無いのはふつうの状態（一度も fetch して
    /// いないリモート）で、読み手は空欄で開くだけ。リポジトリを開けない理由には
    /// ならない。
    fn remote_head_branch(&self, name: &str) -> Option<String> {
        let head = self
            .repo
            .find_reference(&format!("refs/remotes/{name}/HEAD"))
            .ok()?;
        let target = head.symbolic_target().ok().flatten()?;
        target
            .strip_prefix(&format!("refs/remotes/{name}/"))
            .map(str::to_string)
    }

    /// Oid → ラベルの索引。グラフの各行の右に出す ref を引くのに使う（06）。
    pub fn refs(&self) -> Result<RefIndex, GitReadError> {
        // HEAD は 1 回だけ解決する。`Branch::is_head()` は呼ぶたびに `.git/HEAD` と
        // その参照先を読み直すので、ブランチの本数だけファイル読みが増える。
        // unborn なら symbolic target の指す ref が存在せず、detached なら HEAD が
        // symbolic ではないので、どちらも「どのブランチも HEAD ではない」に落ちる。
        let head = self.repo.find_reference("HEAD")?;
        // git2 0.21 から `Err`（target が非 UTF-8）と `Ok(None)`（symbolic でない）が
        // 分かれた。**ここでは畳んでよい。** 使い道は下の `ref_label` での ref 名との
        // 一致判定だけで、比較相手は UTF-8 の `&str` なので、非 UTF-8 の target は
        // どの ref とも一致しない。つまり「どのブランチも HEAD ではない」に落ちる点で
        // unborn / detached と同じ。
        let head_target = head.symbolic_target().ok().flatten();

        // `refs/*` を 1 パスで走査して種別に振り分ける。`branches()` も内部では全 ref を
        // 列挙してから接頭辞で捨てるので、種別ごとに呼ぶと同じ走査を繰り返すことになる。
        let mut labels: HashMap<Oid, Vec<RefLabel>> = HashMap::new();
        for reference in self.repo.references()? {
            let reference = reference?;
            let Some(label) = ref_label(&reference, head_target) else {
                continue;
            };
            // annotated tag は tag オブジェクトを指すので、commit まで peel してから引く。
            // peel できない ref（tree や blob を指すタグ）はグラフのどの行にも乗らないので
            // 落とす。ラベルが 1 つ減るだけなので `refs()` 全体は失敗させない。
            let Ok(commit) = reference.peel_to_commit() else {
                continue;
            };
            labels.entry(commit.id()).or_default().push(label);
        }

        Ok(RefIndex::new(labels))
    }

    /// ローカルブランチの一覧。名前順。
    ///
    /// [`RepoReader::refs`] とは別の読み取り（02 の「ブランチ一覧」）。あちらが返す
    /// [`RefIndex`] は Oid → ラベルの索引で、**upstream も ahead / behind も持たない**
    /// ので、Branches ペインはそこからは組めない。
    pub fn branches(&self) -> Result<Vec<Branch>, GitReadError> {
        // HEAD は 1 回だけ解決する。`git2::Branch::is_head()` は呼ぶたびに `.git/HEAD` を
        // 読み直すので、ブランチの本数だけファイル読みが増える（`refs` と同じ）。
        let head = self.repo.find_reference("HEAD")?;
        let head_target = head.symbolic_target().ok().flatten();

        // **`refs/heads/*` だけを列挙する。** `references()` で全部辿ってから捨てると、
        // loose ref のリポジトリで `refs/remotes/*` と `refs/tags/*` のファイルまで
        // 読むことになる（libgit2 は接頭辞のあるグロブでディレクトリの走査を絞る。
        // `refdb_fs.c` の `iter_load_optimize_prefix`）。`*` は `/` も跨ぐので
        // `feat/x` のような名前も入る（統合テストで確かめている）。
        let mut branches = Vec::new();
        for reference in self.repo.references_glob("refs/heads/*")? {
            let reference = reference?;
            // 非 UTF-8 のブランチ名は識別子として使えない（`git_exec` の argv にも
            // 載せられない）ので落とす。切替も削除もできない行を出さないため。
            let Some(name) = reference.shorthand().ok().map(str::to_string) else {
                continue;
            };
            let full = reference.name().ok();
            // `full` が無いときに `head_target` も無い（detached / unborn）と、
            // `None == None` で HEAD になってしまう。名前が引けた行だけを比べる。
            let is_head = full.is_some_and(|full| head_target == Some(full));
            // 指す先が commit でない ref（ありえないが、壊れたリポジトリでは起きる）は
            // 落とす。`refs` が peel できないタグを落とすのと同じ扱い。
            let Ok(oid) = reference.peel_to_commit().map(|commit| commit.id()) else {
                continue;
            };

            // **追跡設定は remote-tracking ref とは別に読む。** `upstream()` は ref が
            // 無ければ引けないが、`branch.<name>.remote` / `.merge` は config に残って
            // いる。push の `-u` はこちらで決める（02 の「ブランチ一覧」）。
            let upstream_config = full.and_then(|full| self.upstream_config(full));

            let branch = git2::Branch::wrap(reference);
            // **upstream が引けないブランチも一覧に残す。** 未設定なら
            // `NotFound`（class=Config）、設定はあるが追跡 ref が消えていれば
            // `NotFound`（class=Reference。git2 0.21 で確認）。**理由を問わず
            // `(None, 0, 0)` に落とすのは、1 本の壊れた追跡 ref で一覧全体を
            // 失わせないため**（他のブランチはどれも読めている。02）。
            let tracking = match branch.upstream() {
                Ok(upstream) => self.tracking(oid, &upstream),
                Err(_) => Tracking::default(),
            };

            branches.push(Branch {
                name,
                oid,
                upstream: tracking.upstream,
                upstream_config,
                ahead: tracking.ahead,
                behind: tracking.behind,
                is_head,
            });
        }

        // git2 の列挙順は安定していない（`RefIndex::new` と同じ理由）。
        branches.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(branches)
    }

    /// 追跡 ref から読めた値と、それに対する ahead / behind。
    ///
    /// **出せないときは [`Tracking::default`]。`Err` を返さない。** 追跡 ref が壊れている
    /// （中断した fetch のあとなど）ブランチが 1 本あるだけで一覧全体が消えると、
    /// 読めているブランチまで画面から無くなる。名前が非 UTF-8（識別子として扱えない）、
    /// symbolic な remote-tracking ref（ラベルと同じく解決しない）、`ahead_behind` が
    /// 引けない（指す先のオブジェクトが無い）のどれも、**一覧の読み手にとっては同じ
    /// 「比べる相手がいない」**（02）。
    fn tracking(&self, local: Oid, upstream: &git2::Branch<'_>) -> Tracking {
        let (Ok(Some(name)), Some(oid)) = (upstream.name(), upstream.get().target()) else {
            return Tracking::default();
        };
        match self.repo.graph_ahead_behind(local, oid) {
            Ok((ahead, behind)) => Tracking {
                upstream: Some(Upstream {
                    name: name.to_string(),
                    oid,
                }),
                ahead,
                behind,
            },
            Err(_) => Tracking::default(),
        }
    }

    /// `branch.<name>.remote` / `branch.<name>.merge` を読む。
    ///
    /// **どちらか片方でも欠けていれば `None`。** push の行き先は 2 つ揃って初めて
    /// 決まるので、片方だけ持たせると呼び出し側が組み合わせを数えることになる。
    /// 非 UTF-8 の値も `None`（argv に載せられない。ブランチ名と同じ扱い）。
    fn upstream_config(&self, full_ref: &str) -> Option<UpstreamConfig> {
        let remote = self.repo.branch_upstream_remote(full_ref).ok()?;
        let merge = self.repo.branch_upstream_merge(full_ref).ok()?;
        Some(UpstreamConfig {
            remote: remote.as_str().ok()?.to_string(),
            merge: merge.as_str().ok()?.to_string(),
        })
    }

    /// コミット 1 件の詳細。グラフの行が持たない値（メッセージ全文・committer・
    /// タイムゾーン付きの日時）を読む。
    ///
    /// ログの列挙で全コミットぶん読むと重いので、選択されたときに 1 件ずつ取る
    /// （02 の「コミットの詳細」）。ファイル一覧は含めない（`diff` の担当）。
    pub fn commit_detail(&self, oid: Oid) -> Result<CommitDetail, GitReadError> {
        let commit = self.repo.find_commit(oid)?;
        let author = commit.author();
        let committer = commit.committer();

        // メッセージは表示にしか使わないので非 UTF-8 でも lossy で残す。末尾の改行は
        // git が付けたもので、そのまま出すと詳細の最後に空行が出る。
        let mut message = lossy(commit.message_bytes());
        message.truncate(message.trim_end().len());

        Ok(CommitDetail {
            oid: commit.id(),
            parents: commit.parent_ids().collect(),
            message,
            author: signature(&author),
            author_time: author.when().into(),
            committer: signature(&committer),
            committer_time: committer.when().into(),
        })
    }

    /// HEAD の状態。
    ///
    /// **`branch.<HEAD>.remote` もここで読む**（02 の「head」）。`remotes` と同じ
    /// `Cmd::LoadRepoInfo` に乗せるため —— 読み手（01 の「現ブランチの PR」）は
    /// 1 回きり座り直すので、**別のジョブから届く値と混ぜると到着順で答えが変わる**。
    pub fn head(&self) -> Result<HeadInfo, GitReadError> {
        let head = match self.repo.head() {
            Ok(head) => head,
            // コミットが 1 つも無いリポジトリ。ブランチ名は HEAD の symbolic target から取る。
            Err(err) if err.code() == ErrorCode::UnbornBranch => {
                return Ok(HeadInfo::Unborn {
                    name: self.unborn_branch_name()?,
                });
            }
            Err(err) => return Err(err.into()),
        };

        let oid = head.peel_to_commit()?.id();
        if !head.is_branch() {
            return Ok(HeadInfo::Detached(oid));
        }
        // ブランチに乗っているのに名前を落とすと detached と区別が付かなくなるので、
        // ここだけは非 UTF-8 を握りつぶさずエラーにする（02 の「非 UTF-8 の扱い」）。
        // git2 0.21 の `shorthand()` は `Err` が非 UTF-8 そのものなので、`Option` を
        // 経由していた 0.20 より意図に近い。
        let name = head.shorthand().map_err(|_| GitReadError::NonUtf8Head)?;
        // `name()` の `Err` は名前が非 UTF-8 のときで、非 ASCII が入りうるのは
        // `refs/heads/` の後ろ——`shorthand()` が既に見た側——だけ。**それでも
        // 握り潰す**：ここで落ちても「追跡設定を引く鍵が無い」だけで、HEAD そのものは
        // 返せる（`upstream_config` が `None` を返す場合と同じ扱いになる）。
        let upstream = head.name().ok().and_then(|full| self.upstream_config(full));
        Ok(HeadInfo::Branch {
            name: name.to_string(),
            oid,
            upstream,
        })
    }

    /// HEAD のコミットメッセージ全文。コミットが 1 つも無ければ `None`。
    ///
    /// amend の初期値に使う（01 の「commit」）。**HEAD の解決とメッセージの取得を
    /// 1 回で行う。** 呼び出し側が [`RepoReader::head`] と
    /// [`RepoReader::commit_detail`] を 2 回に分けて呼ぶと、そのあいだに HEAD が
    /// 動く隙ができ、1 つ前のコミットのメッセージで HEAD を上書きすることになる。
    ///
    /// unborn を `Err` にしないのは、「読めなかった」と「amend する相手がいない」で
    /// 上位の出し分けが違うため。
    pub fn head_message(&self) -> Result<Option<String>, GitReadError> {
        let oid = match self.head()? {
            HeadInfo::Unborn { .. } => return Ok(None),
            HeadInfo::Branch { oid, .. } | HeadInfo::Detached(oid) => oid,
        };
        Ok(Some(self.commit_detail(oid)?.message))
    }

    /// unborn な HEAD が指しているブランチ名。
    ///
    /// unborn は「HEAD が symbolic で、指す先の ref がまだ無い」状態。git2 0.21 から
    /// `symbolic_target()` は `Err`（target が非 UTF-8）と `Ok(None)`（HEAD が symbolic
    /// でない）を分けて返すが、**呼ぶのは [`RepoReader::head`] が `UnbornBranch` を
    /// 受けたあとだけ**なので後者は起きない。腕を分けても踏めないので、
    /// 「ブランチ名を出せない」という同じ結果に畳む。
    fn unborn_branch_name(&self) -> Result<String, GitReadError> {
        let head = self.repo.find_reference("HEAD")?;
        let target = head
            .symbolic_target()
            .map_err(|_| GitReadError::NonUtf8Head)?
            .ok_or(GitReadError::NonUtf8Head)?;
        Ok(target.strip_prefix("refs/heads/").unwrap_or(target).into())
    }
}

/// グラフのラベルにする ref を選び、種別を決める。載せないものは `None`。
///
/// `head_target` は `HEAD` の symbolic target（`refs/heads/main`）。
fn ref_label(reference: &Reference<'_>, head_target: Option<&str>) -> Option<RefLabel> {
    // symbolic ref（`refs/remotes/origin/HEAD`）は解決すると指す先のラベルと重複する。
    if reference.kind() != Some(ReferenceType::Direct) {
        return None;
    }
    // 非 UTF-8 の ref 名は識別子として扱えないので落とす（02 の「非 UTF-8 の扱い」）。
    // git2 0.21 の `shorthand()` は `Err` が非 UTF-8 そのもの。落とすのがここの方針
    // （HEAD だけは `head()` でエラーにする）なので `.ok()?` が移行先。
    let name = reference.shorthand().ok()?.to_string();

    // 載せるのはローカル / リモート追跡ブランチとタグだけ。`refs/stash` や
    // `refs/notes/*` はそれぞれ別のペインの担当で、グラフのラベルにはしない。
    if reference.is_branch() {
        let is_head = reference.name().is_ok_and(|full| head_target == Some(full));
        Some(RefLabel::Local { name, is_head })
    } else if reference.is_remote() {
        Some(RefLabel::Remote { name })
    } else if reference.is_tag() {
        Some(RefLabel::Tag { name })
    } else {
        None
    }
}
