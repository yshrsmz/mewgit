# 02. git_read: git2 による読み取り層

## 責務

- コミットログの列挙（グラフ用）
- ステータス（staged / unstaged / untracked）
- diff（コミット間、インデックス vs ワークツリー、ハンク分割）
- ref 一覧（ブランチ・タグ・リモート追跡）
- stash 一覧
- 進行中操作の検出

書き込みは一切行わない。ステージ操作も含めて `git_exec` に委譲する（ハンク単位のステージは `git apply --cached` を使うため）。

## API

```rust
pub struct RepoReader {
    repo: git2::Repository,
    /// `open` で bare を弾いたあとのワークツリーのルート。`workdir()` が
    /// `Option` を返さずに済むよう、不変条件を open で 1 回だけ確立する。
    workdir: PathBuf,
}

impl RepoReader {
    pub fn open(path: &Path) -> Result<Self, GitReadError>;
    pub fn workdir(&self) -> &Path;                                 // ワークツリーのルート
    pub fn remotes(&self) -> Result<Vec<Remote>, GitReadError>;     // name, url（生の値）, head_branch
    pub fn refs(&self) -> Result<RefIndex, GitReadError>;           // Oid → Vec<RefLabel>
    pub fn head(&self) -> Result<HeadInfo, GitReadError>;           // branch name or detached Oid
    pub fn branches(&self) -> Result<Vec<Branch>, GitReadError>;    // ローカルブランチ + upstream の ahead/behind
    pub fn log(&self) -> Result<LogSession<'_>, GitReadError>;   // ページングは LogSession が持つ
    pub fn commit_detail(&self, oid: Oid) -> Result<CommitDetail, GitReadError>;  // 選択時に遅延取得
    pub fn commit_files(&self, oid: Oid) -> Result<Vec<FileEntry>, GitReadError>;  // 同上（一覧だけ）
    pub fn status(&self) -> Result<WorktreeSnapshot, GitReadError>;
    pub fn diff(&self, target: DiffTarget) -> Result<Vec<FileDiff>, GitReadError>;
    pub fn stashes(&self) -> Result<Vec<StashEntry>, GitReadError>;  // 下の「stash 一覧」
}
```

git2 の呼び出しはどれも壊れたリポジトリや I/O エラーで失敗しうるので、参照系も含めてすべて `Result` を返す。
進行中操作の検出（`in_progress`）は自由関数で、`Result` を返さない。`.git/` のファイルの有無を見るだけで、読めないことと「進行中の操作は無い」を区別しても出し分けが無いため（下の「進行中操作の検出」）。`status()` の中で呼ばれる。

### open

- `Repository::discover` で探す。リポジトリのサブディレクトリを渡されても開けるようにするため
- bare リポジトリは `GitReadError::BareRepository` で拒否する。status / diff / checkout がいずれも
  ワークツリー前提で、開けても何もできない
- `workdir()` は `discover` が見つけたワークツリーのルート。`app` の `RepoInfo.path` と
  `git_exec` の cwd はこれを使う（引数で渡されたパスそのものではない）

### remotes

`Remote` は `{ name, url, head_branch }`。URL の解析（host / owner / repo）は `auth`（04）の担当で、
`ssh -G` によるホスト別名の展開が要る。`git_read` から `auth` は参照できない（00 の依存の向き）ので、
ここでは `.git/config` の生の文字列をそのまま返す。

`head_branch` は `refs/remotes/<name>/HEAD` が指すブランチの短縮名で、**そのリモートが
言う既定ブランチ**。読み手は `Modal::PrCreate` の base の初期値（01 の「PR 作成（`N`）」）と、
fork の PR を取り出すときのブランチ名（05 の「ブランチ名」）。

- **`Option`。** symref は clone 直後と `git fetch` のあとには在るが、`git remote add` を
  しただけで一度も fetch していないリポジトリには無い（実測 2026-09-21、git 2.55、
  `env -i` の一時リポジトリ）。**どちらの読み手も無いことを理由にしない** ——
  PR 作成は空欄で開いて人に打たせ、取り出しは PR の base とも比べる
- **`refs/remotes/<name>/` を剥がして返す。** 生の `refs/remotes/origin/main` は
  ブランチ名ではないので、そのまま base に載せると GitHub が 422 を返す。剥がせない
  値（symref が別の場所を指している）は `None`
- **リモートごとに持つ。** どのリモートを既定とするかを答えられるのは `app`
  （`update::conventional_remote`）で、`git_read` はリポジトリの状態しか知らない
  （`.claude/rules/rust-coding.md` の「層をまたぐ述語は、その名前が約束している問いに誰が答えられるかで
  置き場所を決める」）

### refs とラベル

```rust
pub struct RefIndex { /* Oid → Vec<RefLabel> */ }

impl RefIndex {
    pub fn labels(&self, oid: Oid) -> &[RefLabel];
    pub fn remote_oid(&self, name: &str) -> Option<Oid>;   // "origin/main" → Oid
}

impl FromIterator<(Oid, RefLabel)> for RefIndex { /* 並べ替えは new を通る */ }

pub enum RefLabel {
    Local { name: String, is_head: bool },   // "main"
    Remote { name: String },                 // "origin/main"
    Tag { name: String },                    // "v1.0"
}
```

- `Remote` は remote 名とブランチ名を分けず `origin/main` の短縮形 1 つで持つ。06 は
  `{origin/main}` と一続りで描くだけなので、分けても描画側で連結し直すだけになる
- **`remote_oid` は名前から位置を引く。** 読み手は PR 作成の `head` の候補（01 の
  「head の組み立て」）で、**「どのリモートにそのブランチが在るか」を git が残して
  いるのは remote-tracking ref だけ**（追跡設定は「何を基にしたか」しか答えない）。
  **別の読み取りを足さない** —— 索引は `head` と同じ `Cmd::LoadRepoInfo` で届くので、
  2 つのあいだに順序が無い。引く側は**リモートの一覧から `<remote>/<branch>` を組む**
  （ref 名からリモート名を切り出すと、`/` を含むリモート名で割り方が決まらない）。
  索引は Oid で引く形なので全件を舐めるが、引くのは `N` と確定のときだけ
- annotated tag は `target()` が tag オブジェクトの Oid なので、commit まで peel してから引く。
  peel しないとラベルがどのグラフ行にも乗らない
- 載せるのはローカルブランチ・リモート追跡ブランチ・タグだけ（ホワイトリスト）。
  `refs/stash` や `refs/notes/*` は他のペインの担当なのでグラフのラベルにしない。
  加えて symbolic ref（`refs/remotes/*/HEAD`）は種別を問わず落とす。解決すると
  指す先の ref とラベルが 2 重になる
- 列挙は `references()` の 1 パスで、種別は `is_branch()` / `is_remote()` / `is_tag()`
  （どれも ref 名の接頭辞判定）で振り分ける。`branches()` も内部では全 ref を列挙して
  から接頭辞で捨てるので、種別ごとに呼ぶと同じ走査を繰り返すことになる
- `is_head` は `HEAD` の symbolic target と ref 名の比較で決める。`Branch::is_head()` は
  呼ぶたびに HEAD を解決し直すのでブランチの本数だけファイル読みが増える。unborn は
  symbolic target の指す ref がまだ無く、detached は HEAD が symbolic ではないので、
  どちらも「どのブランチも HEAD ではない」に落ちる
- ラベルの並べ替え（種別 Local → Remote → Tag、同種内は名前順）は `RefIndex` を作る
  ところに置く。git2 の列挙順は安定しておらず、索引を組む経路が増えたときに
  並べ替えを忘れると描画順とテストの期待値だけが静かにずれる
- `FromIterator` を実装して `refs()` を通さずにも索引を組めるようにする。`ui` のテストが
  ラベルの描画を実リポジトリ無しで表現できるようにするため。どちらの経路でも
  並べ替えは同じところを通る

### ブランチ一覧

```rust
pub struct Branch {
    pub name: String,            // "feat/x"
    pub oid: Oid,
    pub upstream: Option<Upstream>,  // 追跡 ref から読んだ名前と位置
    pub upstream_config: Option<UpstreamConfig>,  // push の行き先
    pub ahead: usize,            // upstream に対する ahead / behind
    pub behind: usize,
    pub is_head: bool,
}

pub struct Upstream {
    pub name: String,  // "origin/feat/x"。表示用
    pub oid: Oid,      // force push の lease に渡す位置
}

pub struct UpstreamConfig {
    pub remote: String,  // branch.<name>.remote。ローカル追跡なら "."
    pub merge: String,   // branch.<name>.merge。完全な ref 名（"refs/heads/main"）
}
```

Branches ペイン（07）が使う。`refs()` が返すのは Oid → ラベルの索引で、**ラベルには
upstream も ahead / behind も無い**ので、そちらからは組めない。

- **列挙は `references_glob("refs/heads/*")`。** `references()` で全部辿ってから捨てると、
  loose ref のリポジトリで `refs/remotes/*` と `refs/tags/*` のファイルまで読む
  （libgit2 は接頭辞のあるグロブでディレクトリの走査を絞る。`refdb_fs.c` の
  `iter_load_optimize_prefix`）。`*` は `/` も跨ぐので `feat/x` も入る。
  git2 の `branches()` は使わない — 内部は絞り込みの無い `references()` と同じ
- **`is_head` は `HEAD` の symbolic target との比較**で決め、HEAD は 1 回だけ解決する
  （`git2::Branch::is_head()` は呼ぶたびに `.git/HEAD` を読み直す）。unborn と detached は
  どちらも「どのブランチも HEAD ではない」に落ちる。`refs()` と同じ形
- **非 UTF-8 のブランチ名は落とす**（`refs()` と同じ。識別子は `git_exec` の argv に
  載せられないので、出しても切替も削除もできない行になる）
- **並べ替えは名前順**。git2 の列挙順は安定していない（`RefIndex` と同じ理由）
- **upstream が引けないブランチは `upstream: None` にして一覧に残す。** 理由は 4 つある。
  `upstream()` が未設定で `NotFound`（class=Config）、設定はあるが追跡 ref が消えていて
  `NotFound`（class=Reference。`fetch --prune` のあと。どちらも git2 0.21 で確認）、
  名前が非 UTF-8、symbolic な remote-tracking ref。**`None` の理由が 2 つ以上あるときは、
  呼び出し側が同じ扱いをしてよいかを数える**（#31 の `hunk_scope` は「ハンクで表せない」と
  「まだ届いていない」を同じ `None` にして、ファイル全体を stage する事故になった）。
  数えた結果、**この一覧の読み手（Branches ペイン）にとっては 4 つとも同じ**
  ——「ahead / behind を出す相手がいない」。`NotFound` 以外の `Err` はそのまま返す
  （壊れたリポジトリなので `refs()` と同じ扱い）
- **`Branch.upstream` から push の `-u` を決めない。** 「upstream が無い」と
  「upstream はあるがこの一覧では出せない」が同じ `None` に落ちているので、`-u` を
  付けるかは `branch.<name>.merge` を読んで決める。ここは表示のための値
- **その読み取りが `upstream_config`**（M3 の `f` / `p` / `P` の配線で入れた）。
  `git2::Repository::branch_upstream_remote` / `branch_upstream_merge` は config だけを
  見るので、**追跡 ref が消えていても引ける**。`upstream` が `None` に落ちる 4 つの
  理由のうち「設定が無い」だけを取り出せる。短縮名で代用すると、**`fetch --prune` の
  あとに `P` を押しただけで追跡先が別のリモートへ向け替わる**（`git push -u
  <推測したリモート>` になる）
- **`remote` と `merge` は 2 つ揃って初めて `Option` を出る。** 片方だけ書かれた設定から
  push の行き先は決まらないので、揃っていなければ `None`。呼び出し側に組み合わせを
  数えさせない
- **`merge` は完全な ref 名のまま持つ**（`refs/heads/main`）。`refs/heads/` の下に無い
  値も書けるので、短縮を読み取り側で決め打つと別の ref を動かす。短縮するのは argv を
  組む側（01 の「push の行き先」）
- **ahead / behind は `graph_ahead_behind(local, upstream)`。** upstream があるときだけ
  引く。無いブランチで 0 / 0 を入れると「同期している」と読めてしまう
- **名前と位置は 1 つの `Option` に入れる**（`Upstream`）。どちらも remote-tracking ref から
  作り、ref が引けなければ両方無い。別々の `Option` にすると「名前は出せるが位置は無い」と
  いう起こらない形が型で組めて、揃っていることを散文で約束することになる
  （`GitOp::Push` の `lease` と同じ理由。03）。位置は force push の
  `--force-with-lease=<dst>:<lease>` に渡す（01）
- **`upstream_config` があっても `upstream` が無い形はある**（`fetch --prune` のあと）。
  そのときは追跡先の位置を知らないので、**force push は断る**（01）。config だけで
  lease を組むと、比べる相手がいないまま `--force` と同じものを撃つことになる
- **`upstream()` はブランチ 1 本ごとにリモートを引き直す。** libgit2 1.9.7 の
  `branch.c: git_branch_upstream_name` は、呼ばれるたびに config の snapshot を取り、
  `branch.<name>.remote` が `.` でなければ `git_remote_lookup` する（ソースで確認）。
  ブランチが多いリポジトリでは、ここが `graph_ahead_behind` の walk より重くなる。
  **畳むなら config を 1 回だけ読み、リモートを名前で覚えて refspec 変換を自分でかける**
  ことになるが、`branch.<name>.remote = "."`（ローカル追跡）まで自前で再現することに
  なる。**M3 の世代のタスクで「畳まない」と決めた** —— 世代は読み直しの**回数**を
  変えず（書き込みのあとに読み直す判断は `update::Reload` のまま）、増えたのは
  落とされたぶんの追いつき 1 通だけ。libgit2 の再実装を先に払う根拠が無い。
  **払う場面が見えてから決める** —— ブランチが多いリポジトリで一覧の読み込みが
  体感できたとき（M6 の「端末幅 < 100」と同じ仕上げの並び）。
  **`upstream_config` は config の snapshot をさらに 2 回取る**（`branch_upstream_remote`
  と `branch_upstream_merge`）ので、畳むときはこの 2 本も一緒に数える

**`refs()` / `head` / `remotes` と 1 通にまとめない**（`Cmd::LoadRepoInfo` に乗せない）。
ブランチの本数だけ upstream の解決と `graph_ahead_behind` の walk が要り、グラフの
ラベルと HEAD だけが欲しい経路にそのコストを払わせることになる。`commit_files` を
`commit_detail` と分けたのと同じ理由（01 の「コミットのファイル一覧」）。**古くなる
時点は同じ**なので、ref を動かす書き込みの後は両方を読み直す。**その配線は切替と削除の
タスク**（01 の `update::Reload`。一覧を出すだけのタスクには ref を動かす書き込みが
無いので、足しても踏めない）。

### stash 一覧

```rust
pub struct StashEntry {
    pub index: usize,   // stash@{index} の index
    pub message: String, // "WIP on main: 6dd895c init" / "On main: <-m で付けたもの>"
    pub oid: Oid,        // stash コミット
}
```

Stash ペイン（01 の「Stash ペイン」）が使う。

- **`refs/stash` の reflog を読む。** `git stash list` が辿るのと同じもの
  （`git log --walk-reflogs refs/stash`）で、reflog の位置（新しい順）がそのまま
  `stash@{index}` の index になる。**`git2::Repository::stash_foreach` は使わない** ——
  git2 0.21 では `&mut self` を取り、読み取りを `&self` に揃えている `RepoReader` に
  合わない
- **stash が 1 つも無い（`refs/stash` が無い）ときは空の `Vec`**。libgit2 は reflog の
  ファイルが無ければ空の reflog を返す。`Err` にすると、stash をしたことの無い
  リポジトリで Stash ペインが毎回失敗になる
- **メッセージは reflog の本文**。実測（git 2.55.0、2026-09-30）では、`-m` 無しが
  `WIP on <branch>: <短縮 sha> <HEAD の summary>`、`-m` 付きが `On <branch>: <m>`。
  **非 UTF-8 でも行ごと落とさない**（lossy にする）—— 落とすと後ろの index がずれ、
  apply / drop が別の stash に当たる。表示にしか使わない文字列なので「非 UTF-8 の扱い」
  の後者に入る
- **drop / pop すると後ろが 1 つずつ詰まる**（`stash@{1}` を drop すると、それまでの
  `stash@{2}` が `stash@{1}` になる。実測）。push は先頭に 1 つ足すので全部が 1 つ
  ずれる。**index は読んだ時点のものでしかない**ので、上位は stash を oid で見分ける
  （01 の「Stash ペイン」）

### stash の diff

`DiffTarget::Stash(oid)` は stash 1 件の中身（下の「diff とハンク」の対象の 1 つ）。
**`path` を持たない** —— 絞る経路が無い（Stash ペインは全ファイルを 1 枚に並べる。
01 の「Stash ペイン」）。持たせると、組む側が毎回 `None` を書き、読む側は「1 ファイルに
絞る経路がある」と読む。

- **stash コミットの親は「stash した時点の HEAD」「index」「untracked」の順**で、3 つ目は
  `--include-untracked` を付けたときだけある（実測。`git rev-list --parents` で 3 つか
  4 つ）。**中身は第一親との差に、第 3 親のツリーを空ツリーとの差として混ぜたもの**
  （`Diff::merge`）
- **`DiffTarget::Commit` で代用しない。** 第一親との差には untracked が出ない（実測。
  `-u` で退避した `new.txt` が `git diff stash@{0}^1 stash@{0}` に出ず、
  `git ls-tree stash@{0}^3` にだけある）。代用すると、mewgit で `-u` 付きで退避した
  ファイルが画面から消えて見える。`git stash show` も既定では出さないが、あちらは
  `--include-untracked` で出せる
- **untracked の種別は `Added`**（空ツリーとの差なので）。`Untracked` と名乗らせると
  「いまある未追跡のファイル」と読めるが、退避したあとの作業ツリーには無い
- **index 側（第 2 親）は出さない。** `git stash show` の既定と同じで、stage していたか
  どうかは apply の `--index` の話になる
- **`find_similar` は混ぜたあとに 1 回だけ**（呼び出し側の `diff()` が掛ける）。混ぜる前に
  掛けると、tracked の削除と untracked の追加が rename として対にならない
- **第 3 親は数で確かめてから引く**（`parent_count() > 2`）。`parent(2)` の `Err` を
  「無い」と読むと、壊れたリポジトリで引けなかったものが黙って落ちる

### 非 UTF-8 の扱い

git は ref 名・パス・メッセージにどんなバイト列も許すので、層の入口で方針を 1 つに決める。

- **識別子**（ref 名、リモート名、パス）が非 UTF-8 なら、その ref / リモート / ファイルを
  一覧から落とす。`git_exec` の argv に載せられず、UI で選べても操作できないため
- **表示にしか使わない文字列**（リモート URL、コミットの summary や署名）は lossy 変換して
  残す。読めない文字が混ざるほうが、その項目ごと消えるより情報が多い
- 例外は HEAD が指すブランチ名で、これだけは落とさず `GitReadError::NonUtf8Head` にする。
  落とすと detached と区別が付かなくなり、上位が別の状態として扱ってしまう

### head

- 通常は `Branch { name, oid, upstream }`、detached HEAD は `Detached(oid)`
- コミットが 1 つも無い（unborn）ときは `repo.head()` が `ErrorCode::UnbornBranch` を返すので、
  `HEAD` の symbolic target からブランチ名を取って `Unborn { name }` にする
- **`upstream` は `branch.<HEAD>.remote` / `.merge` の対**（`UpstreamConfig`）。読み手は
  `app` の「現ブランチの PR」（**fork のブランチから出した PR を当てる**）と
  PR 作成の `head` の候補。**どちらも `merge` を読むのは追跡先が fork の URL のときで**、
  そこに在るブランチの名前はローカルの名前ではなく `merge` のほう（01 の「現ブランチの
  PR」の「比べる名前」）。`upstream_config` をそのまま
  呼ぶので、**片方でも欠けていれば `None`** —— 同じ設定の読み方を 2 通り持たないため
- **ブランチ一覧が読む `Branch::upstream_config` と同じ値を、別のジョブがもう一度読む。**
  重複を承知でこちらに載せるのは、**読み手が 1 回きりの座り直し**だから ——
  `Cmd::LoadBranches` の返信と PR 一覧のどちらが先に届くかで答えが変わる形になる
  （01 の「現ブランチの PR」。`RepoInfo.target` が `default_remote` を呼ばないのと
  同じ数え方）。`head` と `remotes` は同じ `Cmd::LoadRepoInfo` に乗るので、
  この 2 つのあいだには順序が無い

## ログ列挙

```rust
pub struct LogSession<'repo> { /* Revwalk<'repo> */ }

impl LogSession<'_> {
    pub fn next_page(&mut self, n: usize) -> Result<LogPage, GitReadError>;
}

pub struct LogPage {
    pub commits: Vec<Commit>,
    /// これ以上読むコミットが無い。
    pub done: bool,
}
```

- `Revwalk` に `refs/heads/*` `refs/remotes/*` `refs/tags/*` と HEAD を push、
  `Sort::TOPOLOGICAL | Sort::TIME`。リモート追跡ブランチも push するのは、
  ローカルより進んでいる `origin/main` の行が出ないと `{origin/main}` のラベルを
  載せる行が無くなるため
- push は `push_glob` を使う。単体の `push_ref` と違い、commit まで peel できない ref
  （tree を指すタグなど）を黙って飛ばすので、その 1 本のために列挙全体が失敗しない。
  `refs()` が peel できない ref を落とすのと同じ扱いになる
- HEAD の push は `head_detached()` が真のときだけにする。ブランチに乗っていれば
  `refs/heads/*` で既に入っているので、個別に要るのは detached だけ。
  `Revwalk::push_head` を無条件に呼ぶと、コミットが 1 つも無いリポジトリで返る
  エラーが `GenericError`（"reference 'refs/heads/main' not found"）になり、握りつぶす
  対象を絞り込めない。`head_detached` は HEAD が symbolic なら解決しないので unborn を
  踏まない。代わりに `refs/heads/*` の外を指す symbolic HEAD は押されないが、git が
  作らない状態なので許容する
- ページングの位置は API に持たせない。トポロジカル順の walk は途中の Oid からやり直せない
  （他のブランチとの交互配置が失われる）ため、`Revwalk` を `LogSession` として保持し、
  `next_page(n)` で続きを取る
- **セッションは作り直さず使い回す。** `Sort` を指定した `Revwalk` は libgit2 側で
  `limited` になり、最初の `next_page` で push した ref から到達可能なコミットを全部
  読んでから整列する。つまりセッション生成 + 1 ページ目は履歴全体のコストで、2 ページ目
  以降はほぼタダ。スクロールのたびに `log()` を呼び直すと毎回それを払い直すことになる。
  1 ページ目は必ずリーダースレッド側で読む
- `done` は `Revwalk` を `Peekable` にして 1 つ先読みして決める。「ちょうど n 件で終わった」
  ときに `done: false` を返して空のページをもう 1 回読ませないため
- `LogSession<'repo>` は `RepoReader` を借りる。`Repository` と同じ構造体には入れられない
  （自己参照になる）ので、リーダースレッドではループのローカル変数として
  `RepoReader` の隣に置く（状態の持ち方は「スレッド」を参照）
- **先頭から読み直す経路は `LogSession` を捨てることで表す。** セッションを持ったまま
  巻き戻す API は作らない（`Revwalk::reset` は push した ref も落とすので、結局
  作り直しと同じコストになる）。上位から見える形は `log_page` の `restart`（「スレッド」）
- `next_page` が `Err` を返したらセッションを捨てる。そのページのために walk から取り出した
  Oid は失われており、同じセッションで読み直すとその分がどのページにも現れない。グラフには
  親の見つからない行が残り、2 回目は成功するので UI にも異常が出ない
- 1 コミットあたり `Commit { oid, parents, summary, author, time }` のみ抽出。本文は選択時に
  `commit_detail` で、ファイル一覧は `commit_files` で遅延取得する（下の「コミットのファイル一覧」）
- `Commit::time` は author 日時だが、行の並びは `Sort::TIME`（= committer 日時）で決まる。
  rebase / cherry-pick された履歴では表示日時が並び順と逆転しうる。`git log` と同じ挙動なので
  そのままにする
- summary と署名は表示にしか使わないので、非 UTF-8 でも lossy 変換して残す（「非 UTF-8 の扱い」）

## コミットの詳細

グラフの行が持たない値（メッセージ全文、committer、日時のタイムゾーン）を 1 コミットぶん読む。
グラフで選択されたコミットについて `app` が要求する（01 の `Cmd::LoadCommitDetail`）。

```rust
pub struct CommitDetail {
    pub oid: Oid,
    pub parents: Vec<Oid>,
    /// メッセージ全文（summary + 本文）。末尾の改行は落とす。
    pub message: String,
    pub author: Signature,
    pub author_time: Timestamp,
    pub committer: Signature,
    pub committer_time: Timestamp,
}

pub struct Timestamp { pub seconds: i64, pub offset_minutes: i32 }
```

- **`Timestamp` はコミット自身のタイムゾーンのオフセットを持つ。** `Commit::time` のような
  裸の秒数だけだと、表示が見る人の環境のタイムゾーンに寄ってしまう。`git show` は
  コミットに刻まれたオフセットで出すので、それに合わせる。`Commit::time` は行の並びと
  1 行表示にしか使わないのでそのまま
- メッセージは表示にしか使わないので、非 UTF-8 でも lossy で残す（「非 UTF-8 の扱い」）
- ファイル一覧は含めない。`commit_files`（下）の担当で、そちらはコミットの本文より
  重い。1 つの要求にまとめると本文だけ見たいときも一覧のコストを払うことになる

## コミットのファイル一覧

コミットが変更したファイルを、中身（ハンク）抜きで並べる。右ペインのコミット詳細に出し、
選んだ 1 件が `diff(DiffTarget::Commit { oid, path: Some(..) })` で左の Diff ペインに出る（07）。

```rust
pub fn commit_files(&self, oid: Oid) -> Result<Vec<FileEntry>, GitReadError>;
```

- **戻り値は `FileEntry`**（ステータスの区画と同じ型）。`ui` の行の組み立ても `FileStatus` の
  1 文字もそのまま使える。`FileDiff` を空の `hunks` で返す形にすると、一覧の行が
  「ハンクの無いファイル」（バイナリ・mode だけの変更）と区別できなくなる
- **比べる 2 つのツリーと rename 検出の設定は `diff()` と同じものを使う**（第一親、ルート
  コミットは空ツリー、`find_similar` は `status` に合わせた設定）。実装でも同じ関数を通す。
  **出口（delta → `FileEntry` の写像）だけを共有しても、入口の設定が 2 本あれば結果はずれる。**
  ずれは「一覧に出ているファイルを選んだのに diff が空」という形で出る
- **`Patch` は作らない。** 一覧に要るのはパスと種別だけで、そこがこの読み取りを
  `diff(DiffTarget::Commit { path: None })` で代用しない理由でもある。代用すると一覧を出す
  ためだけにコミット全体のハンクを組むことになり、依存の更新のような巨大なコミットで
  リーダースレッドが止まる（01 の「コミットのファイル一覧」に app 側の理由もある）
- 変更が 1 つも無いコミット（`--allow-empty`）では空の `Vec`。**「まだ読めていない」との
  区別は `app` が付ける**（`Option` で持つ。01）
- **1 ファイルの diff でもコミット全体の delta 生成と `find_similar` が毎回走る。**
  `path` は `DiffOptions` に渡していない（渡すと rename が壊れる。上の「diff とハンク」）
  ので、一覧を出すのに 1 回、選んだファイルの diff にもう 1 回、同じツリー比較を
  リーダースレッドで直列に走らせる。ファイルを選び直すたびに 1 回ずつ増える
  （`DiffState` のキャッシュは対象 1 件ぶん）。ディレクトリごと移動したコミットのように
  add / delete が数百あると、`find_similar` の総当たりを毎回やり直しながら `LoadLog` や
  `LoadStatus` を待たせる。**塞ぎ方は判っている** — `DiffTarget::Commit` に `old_path` を
  持たせ、新旧のパスを両方 `opts.pathspec(..)` に渡したうえで
  `disable_pathspec_match(true)` を立てる。rename は対のまま残り、旗を立てると
  pathspec がイテレータの `pathlist` になって walk 自体が絞られる（旗が無いと libgit2 は
  pathspec の**共通接頭辞**しか枝刈りに使わないので、`old.txt` / `new.txt` のように
  接頭辞を共有しない対では効かない。git2 0.21 / libgit2 1.9 の
  `diff_generate.c` の `diff_prepare_iterator_opts` で確認）。`DiffTarget` の形が変わり
  01 の `wanted_diff` も一覧の `old_path` を運ぶことになる。**M3 の世代のタスクでは
  やらないと決めた** —— 世代が変えたのは「持っている diff をいつ捨てるか」で、
  1 通あたりのコストには触っていない（捨てる通数も変わっていない）。同じ PR に入れると
  `DiffTarget` の形の変更が混ざる。**M5 でコミットの diff を出す場面が増えるとき**
  （rebase / cherry-pick の対象を見る）に、そこで決める

## ステータス

```rust
pub struct WorktreeSnapshot {
    pub staged: Vec<FileEntry>,
    pub unstaged: Vec<FileEntry>,
    pub untracked: Vec<FileEntry>,
    pub conflicted: Vec<FileEntry>,
    pub progress: Option<InProgressOp>,   // 区画ではない。下の「進行中操作の検出」
}

pub struct FileEntry {
    pub path: PathBuf,
    /// rename のときだけ入る元のパス。
    pub old_path: Option<PathBuf>,
    pub status: FileStatus,
}
```

- `StatusOptions` で untracked を含め、`recurse_untracked_dirs` を有効化する。無効だと
  新しいディレクトリが 1 行にまとまり、中のファイルを個別に stage できない。ignored は含めない
- rename 検出は `renames_head_to_index` / `renames_index_to_workdir` を有効にする。切ると
  `git mv` が「削除 + 追加」の 2 行になり、`git status` の見え方と食い違う
- 区画は delta の有無で決める。`head_to_index()` があれば staged、`index_to_workdir()` が
  あれば unstaged（種別が `Untracked` ならそちら）。`CONFLICTED` が立っている entry だけは
  例外で、conflicted にだけ入れて他の区画には出さない（解消するまで stage も unstage も
  できない）。**同じファイルが staged と unstaged の両方に出るのは正しい**（一部だけ
  stage した状態）
- **変更種別は `Status` のビットではなく delta から決める。** ビットは同時に複数立つ
  （`git mv` してから編集すると `RENAMED | MODIFIED`）が、どちらを採るかは libgit2 が
  delta 側で既に解決している。ビットから決め直すと優先順の表が `diff` の写像と 2 本になり、
  片方だけ直したときに静かにずれる
- conflicted の delta は `index_to_workdir()` を先に見て、無ければ `head_to_index()` を使う。
  どちらか一方しか無いことがある
- **パスは `StatusEntry::path()` からは取らない。** rename のとき `path()` は旧パスを返すので、
  staged の一覧に新しい名前が出なくなる。delta の `new_file` を `path`、rename と copy の
  ときだけ `old_file` を `old_path` に入れる
- `old_path` を持つのは、`FileStatus::Renamed` が旧パス無しでは表示にならず、staged rename の
  unstage が旧パスと新パスの両方を argv に載せる必要があるため（03）
- パスが非 UTF-8 のファイルは一覧から落とす（「非 UTF-8 の扱い」）。旧パスだけが非 UTF-8 の
  rename も落とす。新しい名前は出せても unstage できない
- **delta から `FileEntry` を組み立てるのは `status` と `diff` で同じ 1 本。** `FileDiff` は
  `FileEntry` と同じ 3 つを頭に持つ。別々に書くと、非 UTF-8 の落とし方と `old_path` を入れる
  種別がずれる
- 並びは区画ごとに `path` 順へ自分で並べ替える。libgit2 の列挙順に依存しない
  （`RefIndex` のラベルと同じ理由）。区画を手で並べず分解束縛で回し、区画を足したときに
  並べ替え漏れがコンパイルエラーになるようにする

## diff とハンク

```rust
pub enum DiffTarget {
    IndexVsWorktree(Option<PathBuf>),
    HeadVsIndex(Option<PathBuf>),
    Commit { oid: Oid, path: Option<PathBuf> },
    Range { from: Oid, to: Oid, path: Option<PathBuf> },
    Stash(Oid),  // 上の「stash の diff」。path を持たない
}

impl DiffTarget {
    /// 絞り込みのパス。全変種が持つので、変種ごとの `match` を呼び出し側に配らない。
    pub fn path(&self) -> Option<&Path>;
}

pub struct FileDiff {
    pub path: PathBuf,
    pub old_path: Option<PathBuf>,
    pub status: FileStatus,
    /// バイナリと判定された。`hunks` は空になる。
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

pub struct Hunk { pub header: String, pub lines: Vec<HunkLine> }
pub struct HunkLine { pub origin: LineOrigin, pub content: Vec<u8> }
```

- `Commit` は**第一親との diff**。ルートコミットは空ツリーと比べる。マージコミットも
  combined diff ではなく第一親と比べる（`git show` の既定はマージに何も出さないが、
  それではペインが常に空になる）
- `HeadVsIndex` は HEAD が unborn なら空ツリーと比べる。最初のコミットを作る前でも
  staged の中身が見える
- `IndexVsWorktree` は untracked の中身も出す（`include_untracked` +
  `recurse_untracked_dirs` + `show_untracked_content`）。出さないと worktree ペインで
  untracked を選んでも diff が空になる
- **`path` で絞るのは diff を作ったあと。pathspec は使わない。** pathspec は diff の
  生成時に効くので、`find_similar` が対にすべき旧パスの delta が先に落ちる。1 ファイルに
  絞った rename は `Renamed` ではなく「全行追加の `Added`」になり、`status` の一覧
  （`Renamed`）と食い違う。`git diff -M -- <新パス>` も同じ挙動なので CLI とはずれないが、
  **`StageHunk` のパッチをこの diff から組むと、rename ではなく「ファイルの新規追加」を
  index に適用することになる**。代わりに pathspec による walk の枝刈り（libgit2 は
  `disable_pathspec_match` のとき pathspec を iterator の pathlist に渡すので、走査
  そのものが縮む）を失うが、ワーキングツリーの diff を 1 ファイルぶん出すのに払う値段
  としては rename が壊れないほうを採る。**pathspec を使わないので
  `disable_pathspec_match`（glob 扱いを止めて `[x].txt` のような名前を守るもの）も要らない**
- **絞る位置は `Patch::from_diff` の手前。** delta のパス（`old_file()` / `new_file()`）は
  `Patch` を作る前でも正しいので、そこで見て飛ばす。出口の `Vec<FileDiff>` で `retain`
  すると、`IndexVsWorktree` は `show_untracked_content` を立てているぶん**全 untracked
  ファイルの中身を読んでから捨てる**ことになる。`find_similar` は diff 全体に掛けたあと
  なので、手前で飛ばしても rename の対は残る。**`old_path` の側でも一致を見る**
  （新パスで絞ったときに rename の行が消えないようにするため）
- rename 検出の `find_similar()` は `Patch` を作る前に呼ぶ。あとから呼んでも、取り出し済みの
  delta には効かない
- **`find_similar()` の設定は `status` と揃える。既定（`None`）は使わない。**
  `for_untracked` を立てないと、ワークツリー上の rename（まだ `add` していない `git mv`）が
  「index から消えた + untracked で増えた」の 2 つの delta のままになる。`status` は
  `renames_index_to_workdir` で 1 行の `Renamed` にするので、同じファイルの見え方が食い違う。
  `renames` も明示する。既定は `diff.renames` の設定を読むが `status` 側は無条件に有効なので、
  設定で切っている環境だけ 2 つがずれる
- `Delta::Unmodified` / `Ignored` / `Unreadable` の delta は落とす。`FileStatus` に対応する
  変種が無く、この UI では出しても操作できない。`Patch::from_diff` が `None` を返すのも
  libgit2 が「出さない」と判断した delta だけなので、パッチが作れない delta はそのまま飛ばす。
  ただし libgit2 のその判定には「`INCLUDE_UNTRACKED` が無いときの `Untracked`」も入る。
  untracked を出す `IndexVsWorktree` が必ずそのフラグを立てているから飛ばして安全であって、
  無条件に安全なわけではない
- **`binary` は `Patch` を作ったあとの `patch.delta()` から読む。** libgit2 はバイナリ判定を
  パッチ生成のときに行うので、`Diff::get_delta` を先に読むと立っていないことがある。
  バイナリと「mode だけの変更」はどちらも `hunks` が空になるので、区別はこのフラグで付ける

### ハンクの持ち方

`git_exec` が `git apply` に渡すパッチへ戻せる形で持つ。戻すのは
`git_exec::hunk_patch`（03 の「ハンクからパッチを組む」）。`diff --git` などの
ファイルヘッダは `path` から組み立て直せるので持たない。

- `header` は `@@ -1,4 +1,6 @@ fn foo() {` の行から末尾の改行を落としたもの。`@@` の後ろの
  関数コンテキストも残す。ここは非 UTF-8 でも lossy でよい。`git apply` は 2 つ目の `@@` より
  後ろを読まないので、置換文字が混ざってもパッチとしては壊れない
- **`content` は `Vec<u8>` で持ち、lossy 変換しない。** ハンクは表示専用ではなく
  `git apply --cached` に戻す入力でもある（「非 UTF-8 の扱い」の例外）。非 UTF-8 のファイルを
  lossy で通すと、ステージした結果が元のファイルと変わる。UI が描くときにその場で lossy にする
- `content` は行末の改行を落として持つ。改行で終わらない行は `origin` を `*NoEol` にする。
  パッチへ戻すときは、`*NoEol` の行の後ろにだけ `\ No newline at end of file` を足す
- **libgit2 の `\ No newline at end of file` は独立した 1 行として来る。** 直前の行の内容を
  運ぶのではなく、`origin` が `=` / `>` / `<`、`content` が
  `"\n\\ No newline at end of file\n"` の行になる。しかも `>`（`ADD_EOFNL`）が付くのは
  `-` 行の後ろで、名前と出る場所が合っていない。**マーカー行は捨て、改行の有無は内容の行が
  `\n` で終わるかどうかで判定する**（git 2.55 / libgit2 1.9 で確認）

## 進行中操作の検出

```rust
pub fn in_progress(git_dir: &Path) -> Option<InProgressOp>;   // git2 を使わない自由関数

pub enum InProgressOp {
    Rebase { step: Option<Step>, branch: Option<String> },   // Step { current, total }
    Am { step: Option<Step> },
    CherryPick { oid: Option<Oid> },
    Revert { oid: Option<Oid> },
    Merge { oid: Oid },
}
```

**`status()` が同じ読み取りの中で呼び、`WorktreeSnapshot.progress` に載せる。** 別の
読み取り（起動時の同期読みや、書き込みの直後に `ExecFinished` へ添える形）にすると、
入口ごとに到着順の議論が要る —— `LoadStatus` に載せれば、古い返りを捨てるのは既存の
世代（01 の「実行中ジョブ › 世代」）の 1 本で、`conflicted` と進行中操作が同じ時点の
値になる。`git status` 自身も両方を 1 回で出す。**関数を gitdir を受ける自由関数に
しておくのは**、ファイルしか見ないのでテストで gitdir に見立てたディレクトリを渡せる
ようにするため。

**読む先はワークツリーごとの gitdir**（`RepoReader::git_dir()` = `git2::Repository::path()`）。
linked worktree では `MERGE_HEAD` も `rebase-merge/` も `.git/worktrees/<name>/` に置かれる
（下の実測）ので、`workdir/.git` を決め打ちすると別のワークツリーの状態を読む。

**判定は上から順で、最初に当たった 1 つを採る。** `git status` とは違う —— あちらは複数を同時に出す（`rebase -r` の merge の行がコンフリクトすると `rebase-merge/` と `MERGE_HEAD` が両方在り、両方を出す。下の実測）。**rebase を先に見る**のは、そのとき打つのが `git rebase --continue` / `--abort` だから（merge を採ると M5 の continue / abort が merge 側の argv を組む）。

| 判定 | InProgressOp | 進捗 |
|---|---|---|
| `rebase-merge/` 存在 | `Rebase`（`branch` は `head-name`） | `msgnum` / `end` |
| `rebase-apply/applying` 存在 | `Am` | `next` / `last` |
| `rebase-apply/` 存在（`applying` 無し） | `Rebase`（`branch` は `head-name`） | `next` / `last` |
| `MERGE_HEAD` | `Merge { oid }`（先頭行） | — |
| `CHERRY_PICK_HEAD` | `CherryPick { oid: Some }` | — |
| `REVERT_HEAD` | `Revert { oid: Some }` | — |
| `sequencer/todo` の先頭行が `pick` / `p` | `CherryPick { oid: None }` | — |
| `sequencer/todo` の先頭行が `revert` | `Revert { oid: None }` | — |

**実測（git 2.55.0、2026-09-26。一時リポジトリで各状態を作って `.git/` を読んだ）。**
表の形はここから決めた。

| 作った状態 | `.git/` に在ったもの | `git status` の見出し |
|---|---|---|
| `git rebase main`（`-i` 無し）でコンフリクト | `rebase-merge/`（`interactive` **も在る**）、`msgnum` 1 / `end` 2 | interactive rebase in progress |
| `git rebase -i main` で `edit` に止まる | `rebase-merge/`（`amend` が増える）、`done` の最終行 `edit …` | interactive rebase in progress |
| `git rebase -x false <base>`（2 コミット、コンフリクト無し） | `msgnum` 2 / **`end` 4**、`done` の最終行 `exec false` | — |
| `git rebase -i main` で `edit` にした pick がコンフリクト | `done` の最終行 `edit …`（止まった理由はコンフリクト） | interactive rebase in progress |
| `git rebase -r main` で merge の行がコンフリクト | `rebase-merge/` **と** `MERGE_HEAD` の両方 | interactive rebase と unmerged paths の両方を出す |
| `git rebase --apply main` でコンフリクト | `rebase-apply/`（`rebasing` が在る）、`next` / `last` | rebase in progress |
| `git am` でコンフリクト | `rebase-apply/`（**`applying` が在る**） | You are in the middle of an am session |
| `git cherry-pick A` でコンフリクト | `CHERRY_PICK_HEAD` | — |
| `git cherry-pick A B` でコンフリクト | `CHERRY_PICK_HEAD` + `sequencer/`（`todo` は `pick …` の 2 行） | — |
| ↑ を解いて `git commit` した直後 | **`sequencer/` だけ**（`CHERRY_PICK_HEAD` は消える） | Cherry-pick currently in progress |
| `git revert` でコンフリクト | `REVERT_HEAD` | — |
| `git merge` でコンフリクト / `--no-commit` | `MERGE_HEAD`（1 行） | — |
| linked worktree で `git merge` でコンフリクト | `MERGE_HEAD` は `.git/worktrees/<name>/` に在る | — |

ここから決めたこと ——

- **`Rebase` に `interactive` を持たせない。** `-i` の有無で `rebase-merge/` の中身は
  変わらず（2.26 以降の既定は merge backend）、`git status` 自身も両方を
  「interactive rebase」と呼ぶ。区別できない値を持つと、読み手が嘘の分岐を書く
- **`Am` を `Rebase` と分ける。** 同じ `rebase-apply/` だが、am の途中で
  `git rebase --continue` を打つと git が `It looks like 'git am' is in progress.
  Cannot rebase.` で断る（実測）。continue / abort の argv が違うので、同じ変種に
  すると M5 の continue で壊れる。**mewgit から am は始めない**が、外で始めた am の途中で
  開いたときに「進行中は無い」と読むと、書き込みの拒否（01 の「進行中操作があるときの書き込み」）が効かない
- **`CherryPick` / `Revert` の `oid` は `Option`。** 複数の pick の途中で 1 つを
  commit で解くと `sequencer/` だけが残る。`git status` はそれも進行中と呼ぶ
- **`git2::Repository::state()` は使わない。** libgit2 1.9 の実装（`repository.c`）は
  `rebase-merge/interactive` の有無で `REBASE_INTERACTIVE` を返すので上の区別が
  できず、`sequencer/todo` は `CHERRY_PICK_HEAD` が在るときしか見ない（`sequencer/`
  だけの形は `NONE`）。**ファイルが SoT**
- **進捗（`Step`）は todo の行数で、コミット数ではない。** `-x` は exec の行を足すので、
  2 コミットの rebase が `end` 4 になる。画面には git の数えたとおり出す
- **`Step` が読めないときは `None`**（ディレクトリは在るのに `msgnum` がまだ無い ——
  外で `git rebase -i` の todo を編集している最中など）。**進行中であることは落とさない**
- **止まった理由（`edit` / コンフリクト / exec の失敗）は持たない。** `done` の最終行は
  理由を表さない —— `edit` にした pick がコンフリクトすると、最終行は `edit` なのに
  止まったのはコンフリクトのため（実測）。07 のモックにあった `(edit)` はこの理由で出さない
- **`Rebase` は元のブランチ（`branch`）を持つ。** rebase 中の対象ブランチは git が
  改名（`is being rebased`）も削除（`used by worktree`）も断る（03 の表）ので、`app` が
  入力を開く前に断るのに要る。`head-name` は merge / apply の両 backend で
  `refs/heads/<name>` の 1 行、**detached HEAD で始めた rebase では `detached HEAD`** の
  1 行（実測 2026-09-27、git 2.55.0）。`refs/heads/` で始まらなければ `None`
  （対象のブランチが無い）。読めなくても `Rebase` であることは落とさない（`Step` と同じ）
- `MERGE_HEAD` は octopus では複数行になる。`Merge` が持つのは先頭行だけ
- ファイルが読めない・中身が壊れているときは、その行を「無い」と読んで次の判定へ進む。
  **戻り値が `Option` で `Result` でないのはこのため**（上の「API」）

## エラー

```rust
pub enum GitReadError {
    Open { path: PathBuf, source: git2::Error },
    BareRepository { path: PathBuf },
    NonUtf8Head,
    ReaderGone,
    Git(git2::Error),
}
```

`Open` は渡されたパスを添える（まだ何も見つかっていないので、それしか手掛かりが無い）。
`BareRepository` は `discover` が見つけたリポジトリのパスを添える。引数のサブディレクトリを
返すと「そのディレクトリが bare だ」と読めてしまうため。

`ReaderGone` はリーダースレッド（下記）が落ちているときに返る。待ち続けると `JobTracker` に
登録されたジョブが永久に残り、同種の `Cmd` が二度と発行されなくなる。

## スレッド

`git2::Repository` は `Send` だが `Sync` ではない。`RepoReader` は `spawn_blocking` 内で毎回 open するか、専用スレッドに所有させてチャネル経由で使う。後者を採用し、`ReaderHandle` をランタイムに渡す。

```rust
pub struct ReaderHandle { /* mpsc::UnboundedSender<Request>, workdir: PathBuf */ }

impl ReaderHandle {
    pub fn spawn(path: &Path) -> Result<Self, GitReadError>;
    pub fn workdir(&self) -> &Path;
    pub async fn log_page(&self, count: usize, restart: bool) -> Result<LogPage, GitReadError>;
    pub async fn commit_detail(&self, oid: Oid) -> Result<CommitDetail, GitReadError>;
    pub async fn commit_files(&self, oid: Oid) -> Result<Vec<FileEntry>, GitReadError>;
    pub async fn head_message(&self) -> Result<Option<String>, GitReadError>;
    pub async fn refs(&self) -> Result<RefIndex, GitReadError>;
    pub async fn branches(&self) -> Result<Vec<Branch>, GitReadError>;
    pub async fn stashes(&self) -> Result<Vec<StashEntry>, GitReadError>;
    pub async fn head(&self) -> Result<HeadInfo, GitReadError>;
    pub async fn remotes(&self) -> Result<Vec<Remote>, GitReadError>;
    pub async fn status(&self) -> Result<WorktreeSnapshot, GitReadError>;
    pub async fn diff(&self, target: DiffTarget) -> Result<Vec<FileDiff>, GitReadError>;
}
```

- `spawn_blocking` を毎回呼ぶ形にできないのは `LogSession` があるため。セッションはスレッドの
  ローカル変数として生き続ける必要がある（「ログ列挙」の「セッションは作り直さず使い回す」）
- スレッドの本体は `RepoReader` をローカル変数に置き、その隣に walk の状態を持つ。
  `LogSession<'repo>` が `RepoReader` を借りるので、同じ構造体には入れられない（自己参照になる）
- 要求は変種ごとに `oneshot::Sender<Result<T, GitReadError>>` を持つ非公開の enum で送る。
  `ReaderHandle` は型付きの async メソッドだけを見せる。公開の要求 / 応答 enum を作ると、
  `Cmd`（01）と `Msg`（01）に並ぶ 3 本目の語彙になり、変種を足すたびに 3 箇所直すことになる
- `open` は呼び出し側で行い、開いた `RepoReader` をスレッドへムーブする（`Repository` は
  `Send`）。bare や存在しないパスは `spawn` の `Err` として起動時に出る。`workdir()` は
  その値を持つので同期で読める。スレッドの中で open して結果を `oneshot` で返す形にすると、
  `?` で足りるところにチャネルが 1 本増える
- ログ列挙の状態は `Option<LogSession>` ではなく `Idle` / `Live` / `Exhausted` の 3 状態で持つ。
  読み切ったセッションを `Live` のまま抱えると、libgit2 が walk のために読んだコミットの
  プールがプロセスの寿命だけ残る（`git_revwalk_free` まで解放されない）。かといって `Idle` に
  戻すと、末尾までスクロールしたあとの 1 回で履歴全体を読み直す。`Exhausted` は空のページを
  `done` 付きで返す
- **`next_page` の `Err` でセッションを捨てる契約は、ここで実装として守る。** スレッドは
  セッションを一度取り出し、成功したときだけ戻す。契約を書ける場所が他に無い。
  捨てたあとの次の要求は先頭から読み直すので、上位は積んだ行を捨ててから読み直す
- **`log_page` の future は cancel-safe ではない。** 完了前に drop すると、スレッドは walk を
  進めたあとで返り先を失い、そのページのコミットがどのページにも現れなくなる。返り先が
  消えていたら読まない、という判定は競合を狭めるだけで無くせない（`send` が成功した直後に
  受け側が消える窓が残る）ので、ランタイム側の規約にする。**ログ読み込みのタスクは abort
  しない。** 中断が効くのは `Cmd::Exec` だけ（01 の「キャンセル」）。状態を持たない
  `refs` / `head` / `remotes` / `status` / `diff` は drop されても捨ててよい
- **`log_page(count, restart)` の `restart` はセッションを捨ててから読む。** commit や
  fetch で HEAD が動くと積んだ行が丸ごと古くなり、続きを読む経路では直せない（新しい
  コミットは末尾ではなく先頭に付く）。`Live` も `Exhausted` も `Idle` に落としてから
  読むので、先頭の 1 ページが返る。**別のメソッドに分けず引数にする。** 分けると
  「セッションを取り出して成功したときだけ戻す」契約を 2 箇所で守ることになる
- **`head_message` は HEAD の解決とメッセージの取得をスレッドの中で 1 回で行う。**
  amend の初期値に使う（01 の「commit」）。`head()` と `commit_detail()` の 2 回に
  分けると、そのあいだに HEAD が動く隙ができ、**1 つ前のコミットのメッセージで HEAD を
  上書きする**ことになる。コミットが 1 つも無い（unborn）ときは `Ok(None)`。
  「読めなかった」と「amend する相手がいない」は上位の出し分けが違う
- `ReaderHandle` は `Clone`。ランタイムは `Cmd` ごとに tokio タスクを起こし、その中から呼ぶ。
  スレッドは 1 本なので要求は直列に処理される
- 送信側・受信側のどちらのチャネルが切れても `ReaderGone` にする。ハンドルが全部 drop されたら
  スレッドはループを抜けて終わる
