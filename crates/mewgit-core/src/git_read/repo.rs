//! リポジトリを開いて ref / リモート / HEAD を読む。設計は `docs/design/02-git-read.md`。

use std::path::{Path, PathBuf};

use git2::{ErrorCode, Reference, ReferenceType, Repository};

use super::{
    lossy, signature, Branch, BranchList, CommitDetail, GitReadError, Oid, RefIndex, RefLabel,
    Remote, RowlessHead, StashEntry, Upstream, UpstreamConfig, STASH_REF,
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

    /// ワークツリーごとの gitdir。linked worktree では `.git/worktrees/<name>/` で、
    /// 進行中操作のファイルはこちらに置かれる（`status()` が見る。02 の「進行中操作の検出」）。
    pub fn git_dir(&self) -> &Path {
        self.repo.path()
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

    /// Oid → ラベルの索引（リモート追跡ブランチとタグ）。グラフの各行の右に出す ref を
    /// 引くのに使う（06）。
    ///
    /// **`refs/heads/*` は読まない。** ローカルブランチのラベルは `ui` が
    /// [`RepoReader::branches`] の行から引く（02 の「refs とラベル」）。
    pub fn refs(&self) -> Result<RefIndex, GitReadError> {
        let mut labels = Vec::new();
        // **接頭辞ごとのグロブで読む**（`branches()` の列挙と同じ理由）。種別もグロブで
        // 決まるので、ラベルの作り方を対にして回す。
        type Label = fn(String) -> RefLabel;
        let kinds: [(&str, Label); 2] = [
            ("refs/remotes/*", |name| RefLabel::Remote { name }),
            ("refs/tags/*", |name| RefLabel::Tag { name }),
        ];
        for (glob, label) in kinds {
            for reference in self.repo.references_glob(glob)? {
                let reference = reference?;
                let Some(name) = label_name(&reference) else {
                    continue;
                };
                // annotated tag は tag オブジェクトを指すので、commit まで peel してから
                // 引く。peel できない ref（tree や blob を指すタグ）はグラフのどの行にも
                // 乗らないので落とす。ラベルが 1 つ減るだけなので全体は失敗させない。
                let Ok(commit) = reference.peel_to_commit() else {
                    continue;
                };
                labels.push((commit.id(), label(name)));
            }
        }

        Ok(labels.into_iter().collect())
    }

    /// ローカルブランチの一覧（名前順）と HEAD の状態。
    ///
    /// グラフのローカルブランチのラベルも `ui` がこの行から引く（02 の「refs とラベル」）。
    ///
    /// **HEAD はここで 1 回だけ解決し、`Branch` かどうかは一覧の行から決める**（02 の
    /// 「head」）。HEAD を別の読み取りで解決すると、あいだに外で HEAD が動いたとき
    /// 「`Branch` なのに HEAD の行が無い」組が届く。
    pub fn branches(&self) -> Result<BranchList, GitReadError> {
        // `git2::Branch::is_head()` は呼ぶたびに `.git/HEAD` を読み直すので、ブランチの
        // 本数だけファイル読みが増える。HEAD はここで 1 回だけ解決する。
        let head = self.repo.find_reference("HEAD")?;
        // git2 0.21 から `Err`（target が非 UTF-8）と `Ok(None)`（symbolic でない）が
        // 分かれた。**前者はエラーにする** —— 他の ref と同じく行を落とすと、ブランチに
        // 居るのに HEAD の行が無くなり、detached と区別が付かない（02 の「非 UTF-8 の扱い」）。
        let head_target = head
            .symbolic_target()
            .map_err(|_| GitReadError::NonUtf8Head)?;
        // **symbolic ref の連鎖は最後まで辿る**（02 の「head」）。`refs/heads/alias` →
        // `refs/heads/main` を指す HEAD を、git は `On branch main` と読む。1 段で止めると
        // `alias` の行が HEAD になり、強調も追跡設定も別のブランチのものになる。
        let head_target = head_target
            .map(|target| self.symbolic_end(target))
            .transpose()?;
        let head_target = head_target.as_deref();

        // **`refs/heads/*` だけを列挙する。** `references()` で全部辿ってから捨てると、
        // loose ref のリポジトリで `refs/remotes/*` と `refs/tags/*` のファイルまで
        // 読むことになる（libgit2 は接頭辞のあるグロブでディレクトリの走査を絞る。
        // `refdb_fs.c` の `iter_load_optimize_prefix`）。`*` は `/` も跨ぐので
        // `feat/x` のような名前も入る（統合テストで確かめている）。
        // `pushRemote` はブランチごとに読むので、snapshot を 1 回だけ取る（live の
        // config で引くと 1 回ごとに config ファイルを stat し直す。libgit2 1.9.7 の
        // `config_file_refresh`）。非 UTF-8 の値は `get_string` が `Err` にする。
        let config = self.repo.config()?.snapshot()?;
        let push_default = config.get_string("remote.pushDefault").ok();

        let mut branches = Vec::new();
        for reference in self.repo.references_glob("refs/heads/*")? {
            let reference = reference?;
            // 非 UTF-8 のブランチ名は識別子として使えない（`git_exec` の argv にも
            // 載せられない）ので落とす。切替も削除もできない行を出さないため。
            let Some(name) = reference.shorthand().ok().map(str::to_string) else {
                continue;
            };
            // 写しを持つ。`reference` は下で `git2::Branch` に包むので借りたままにできない
            // （reflog を引くのはその後。`pointed_at`）。
            let full = reference.name().ok().map(str::to_string);
            // 指す先が commit でない ref（ありえないが、壊れたリポジトリでは起きる）は
            // 落とす。`refs` が peel できないタグを落とすのと同じ扱い。HEAD の行を落とした
            // ときは `rowless_head` が peel し直して `Err` にする。
            let Ok(oid) = reference.peel_to_commit().map(|commit| commit.id()) else {
                continue;
            };

            // **追跡設定は remote-tracking ref とは別に読む。** `upstream()` は ref が
            // 無ければ引けないが、`branch.<name>.remote` / `.merge` は config に残って
            // いる。push の `-u` はこちらで決める（02 の「ブランチ一覧」）。
            let upstream_config = full.as_deref().and_then(|full| self.upstream_config(full));
            let push_remote = config
                .get_string(&format!("branch.{name}.pushRemote"))
                .ok()
                .or_else(|| push_default.clone());

            let branch = git2::Branch::wrap(reference);
            // **upstream が引けないブランチも一覧に残す。** 未設定なら
            // `NotFound`（class=Config）、設定はあるが追跡 ref が消えていれば
            // `NotFound`（class=Reference。git2 0.21 で確認）。**理由を問わず
            // `(None, 0, 0)` に落とすのは、1 本の壊れた追跡 ref で一覧全体を
            // 失わせないため**（他のブランチはどれも読めている。02）。
            let tracking = match branch.upstream() {
                Ok(upstream) => self.tracking(full.as_deref(), oid, &upstream),
                Err(_) => Tracking::default(),
            };

            branches.push(Branch {
                name,
                oid,
                upstream: tracking.upstream,
                upstream_config,
                push_remote,
                ahead: tracking.ahead,
                behind: tracking.behind,
            });
        }

        // git2 の列挙順は安定していない（`RefIndex::from_iter` と同じ理由）。
        branches.sort_by(|a, b| a.name.cmp(&b.name));
        // **どの行が HEAD かは名前で決める**（`Branch` に旗を持たせない。02 の「head」）。
        // 列挙は `refs/heads/*` だけなので、行の名前は target から接頭辞を外したものと
        // 比べられる。外れなければ（`refs/heads/` の外を指す symref）どの行でもない。
        // symbolic でない HEAD は名前を持たないので、必ず `rowless` へ落ちる。
        let head_name = head_target.and_then(|target| target.strip_prefix("refs/heads/"));
        BranchList::new(branches, head_name, || match head_target {
            None => Ok(RowlessHead::Detached(head.peel_to_commit()?.id())),
            Some(target) => rowless_head(&head, target),
        })
    }

    /// symbolic ref `target` から、`refs/heads/` の中で連鎖を辿った先の ref 名。行き止まり
    /// （指す先の ref が無い）ならそこで止まる —— git は HEAD → `alias` → 無い ref を
    /// `On branch <無い名前>` と読む（unborn。実測 2026-10-10、git 2.56.0）。
    ///
    /// **`refs/heads/` の外へ出る段は辿らない。** `refs/heads/alias` →
    /// `refs/remotes/origin/main` は `alias` の行のままにする（02 の「head」の表）。
    ///
    /// **輪になった連鎖でも止まるように段数を打ち切る。** そこで止まった名前の行は peel
    /// できずに一覧から落ちているので、HEAD は `rowless_head` が `Err` にする（git も
    /// `git branch` が `failed to resolve HEAD` で落ちる）。
    fn symbolic_end(&self, target: &str) -> Result<String, GitReadError> {
        let mut name = target.to_string();
        for _ in 0..SYMBOLIC_DEPTH {
            let reference = match self.repo.find_reference(&name) {
                Ok(reference) => reference,
                Err(err) if err.code() == ErrorCode::NotFound => break,
                Err(err) => return Err(err.into()),
            };
            match reference
                .symbolic_target()
                .map_err(|_| GitReadError::NonUtf8Head)?
            {
                Some(next) if next.starts_with("refs/heads/") => name = next.to_string(),
                _ => break,
            }
        }
        Ok(name)
    }

    /// stash の一覧。`stash@{0}`（いちばん新しいもの）から順に並ぶ。
    ///
    /// **`refs/stash` の reflog を読む。** `git stash list` が辿るのと同じもの
    /// （`git log --walk-reflogs refs/stash`）で、reflog の位置がそのまま
    /// `stash@{index}` の index になる。**`git2::Repository::stash_foreach` を使わない**
    /// のは `&mut self` を取るからで、読み取りを `&self` に揃えている `RepoReader` には
    /// 合わない（git2 0.21。02 の「stash 一覧」）。
    ///
    /// stash が 1 つも無い（`refs/stash` が無い）ときは空の `Vec`。libgit2 は reflog の
    /// ファイルが無ければ空の reflog を返す。
    pub fn stashes(&self) -> Result<Vec<StashEntry>, GitReadError> {
        let reflog = self.repo.reflog(STASH_REF)?;
        Ok(reflog
            .iter()
            .enumerate()
            .map(|(index, entry)| StashEntry {
                index,
                // 非 UTF-8 のメッセージも行ごと落とさない。落とすと後ろの index が
                // ずれ、apply / drop が別の stash に当たる。
                message: entry.message_bytes().map(lossy).unwrap_or_default(),
                oid: entry.id_new(),
            })
            .collect())
    }

    /// 追跡 ref から読めた値と、それに対する ahead / behind。
    ///
    /// **出せないときは [`Tracking::default`]。`Err` を返さない。** 追跡 ref が壊れている
    /// （中断した fetch のあとなど）ブランチが 1 本あるだけで一覧全体が消えると、
    /// 読めているブランチまで画面から無くなる。名前が非 UTF-8（識別子として扱えない）、
    /// symbolic な remote-tracking ref（ラベルと同じく解決しない）、`ahead_behind` が
    /// 引けない（指す先のオブジェクトが無い）のどれも、**一覧の読み手にとっては同じ
    /// 「比べる相手がいない」**（02）。
    fn tracking(
        &self,
        full_ref: Option<&str>,
        local: Oid,
        upstream: &git2::Branch<'_>,
    ) -> Tracking {
        let (Ok(Some(name)), Some(oid)) = (upstream.name(), upstream.get().target()) else {
            return Tracking::default();
        };
        match self.repo.graph_ahead_behind(local, oid) {
            Ok((ahead, behind)) => Tracking {
                upstream: Some(Upstream {
                    name: name.to_string(),
                    oid,
                    rewound: ahead == 0
                        && behind > 0
                        && full_ref.is_some_and(|full| self.pointed_at(full, oid)),
                }),
                ahead,
                behind,
            },
            Err(_) => Tracking::default(),
        }
    }

    /// ブランチ `full_ref` が `oid` を一度指していたか。reflog のどれかの項目の新しい側に
    /// `oid` があれば真（02 の「ブランチ一覧」の `rewound`）。
    ///
    /// **reflog が無い・読めないときは偽。** 読み手は force push を通すかをこれで決める
    /// （01 の「force push」）ので、判らないときは断る側に倒す。
    fn pointed_at(&self, full_ref: &str, oid: Oid) -> bool {
        self.repo
            .reflog(full_ref)
            .is_ok_and(|reflog| reflog.iter().any(|entry| entry.id_new() == oid))
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

    /// HEAD のコミットメッセージ全文。コミットが 1 つも無ければ `None`。
    ///
    /// amend の初期値に使う（01 の「commit」）。**HEAD の解決とメッセージの取得を
    /// 1 回で行う。** 呼び出し側が HEAD の解決（[`RepoReader::branches`]）と
    /// [`RepoReader::commit_detail`] を 2 回に分けて呼ぶと、そのあいだに HEAD が
    /// 動く隙ができ、1 つ前のコミットのメッセージで HEAD を上書きすることになる。
    ///
    /// unborn を `Err` にしないのは、「読めなかった」と「amend する相手がいない」で
    /// 上位の出し分けが違うため。
    pub fn head_message(&self) -> Result<Option<String>, GitReadError> {
        let oid = match self.repo.head() {
            Ok(head) => head.peel_to_commit()?.id(),
            Err(err) if err.code() == ErrorCode::UnbornBranch => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        Ok(Some(self.commit_detail(oid)?.message))
    }
}

/// [`RepoReader::symbolic_end`] が辿る段数の上限。
const SYMBOLIC_DEPTH: usize = 10;

/// HEAD が symbolic（target は連鎖を辿った先の `target`）なのに、一覧に HEAD の行が
/// 無いときの状態。
///
/// **HEAD を `resolve()` で辿る**（symbolic の連鎖も辿る。`repo.head()` と同じ）。辿った
/// 先が無ければ unborn —— `refs/heads/a` → `refs/heads/missing` のような連鎖も含む。
/// 在るのにコミットまで peel できなければ `Err` —— 一覧が HEAD の行を落とした形で、
/// 通すと「ブランチに居るのに HEAD の行が無い」が `Err` にならずに届く（02 の「head」）。
/// **peel できれば `Detached`**。一覧の行と綴りが合わない HEAD がここに落ちる ——
/// `refs/heads/` の外を指す（`git symbolic-ref HEAD refs/remotes/origin/main` は通る）、
/// 大文字小文字を区別しない FS で `git switch Main` した（`main` の loose ref に当たる）、
/// 列挙のあとに外でブランチが作られた、のどれも行を持たない点で detached と同じに扱い、
/// **行と判別は食い違わない**（実測は 02 の「head」。git 2.56.0）。
fn rowless_head(head: &Reference<'_>, target: &str) -> Result<RowlessHead, GitReadError> {
    match head.resolve() {
        Ok(resolved) => Ok(RowlessHead::Detached(resolved.peel_to_commit()?.id())),
        Err(err) if err.code() == ErrorCode::NotFound => Ok(RowlessHead::Unborn {
            name: target.strip_prefix("refs/heads/").unwrap_or(target).into(),
        }),
        Err(err) => Err(err.into()),
    }
}

/// グラフのラベルに載せる ref の名前。載せないものは `None`。
fn label_name(reference: &Reference<'_>) -> Option<String> {
    // symbolic ref（`refs/remotes/origin/HEAD`）は解決すると指す先のラベルと重複する。
    if reference.kind() != Some(ReferenceType::Direct) {
        return None;
    }
    // 非 UTF-8 の ref 名は識別子として扱えないので落とす（02 の「非 UTF-8 の扱い」）。
    // git2 0.21 の `shorthand()` は `Err` が非 UTF-8 そのもの。落とすのがここの方針
    // （HEAD だけは `branches()` でエラーにする）なので `.ok()?` が移行先。
    Some(reference.shorthand().ok()?.to_string())
}
