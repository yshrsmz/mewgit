# 03. git_exec: git CLI 実行層

## 責務

すべての書き込み操作を `git` サブプロセスで実行する。ssh 設定、credential helper、hook、`GIT_*` 環境変数が自動で効くようにするため。

## GitOp

```rust
pub enum GitOp {
    Stage(Vec<PathBuf>), Unstage(Vec<PathBuf>),
    StageHunk(Box<HunkApply>), UnstageHunk(Box<HunkApply>), DiscardHunk(Box<HunkApply>),
    Discard(Vec<PathBuf>), DiscardUntracked(Vec<PathBuf>), DiscardAdded(Vec<PathBuf>),
    Commit { message: String, amend: bool },
    BranchCreate { name, start: Option<String>, switch: bool },
    Switch(String), BranchDelete { name, force }, BranchRename { from, to },
    Fetch { remote: Option<String>, prune: bool },
    Pull { mode: PullMode },
    Push { remote, src, dst: Option<String>, set_upstream, lease: Option<Oid> },
    RebaseInteractive { onto: String, plan: RebasePlan },
    RebaseContinue { reword: Option<RefusedReword> },   // 複数の手順になりうる（下の「複数の手順を持つ書き込み」）
    RebaseSkip, RebaseAbort,
    CherryPick(Vec<Oid>), CherryPickContinue, CherryPickSkip, CherryPickAbort,
    Revert { oids: Vec<Oid>, mainline: Option<u32> }, RevertContinue, RevertAbort,
    MergeAbort,
    StashPush { message, include_untracked, paths: Option<Vec<PathBuf>> },
    StashApply(usize), StashPop(usize), StashDrop(usize),
    SetAccount { login }, UnsetAccount,
    CheckoutPr(Box<PrCheckout>),   // 複数の手順（下の「複数の手順を持つ書き込み」）
}
```

- **`patch` は `Vec<u8>`。** `git apply --cached` に戻す入力なので、lossy 変換すると
  非 UTF-8 のファイルをステージした結果が元のファイルと変わる（02 の「ハンクの持ち方」と
  同じ理由）
- **ハンクの 3 操作は同じ `HunkApply { path, patch, expect }` を箱に入れて持つ。**
  `expect` はハンクを読んだ時点の指紋（02 の `FileDiff::stamp`）で、argv には載らず、
  `Runner` が git を起こす前に照合する（下の「古いハンクの守り」）。3 変種にフィールドを
  並べると、足すたびに 3 箇所を揃えることになり、しかも `GitOp` が 96 から 104 バイトに
  伸びて `Cmd` の大きさの規則（01 の「`Cmd`」）が数え直しになる（実測 2026-10-03、64bit）。
  最初の版は `expect` だけを箱に入れていた（`/simplify` の simplification と altitude が
  独立に拾った）
- **ハンクの 3 操作は `--whitespace=nowarn` を必ず付ける。** 既定はユーザーの
  `apply.whitespace` を読む。`fix`（よく勧められる設定）だと git が行末の空白を
  落としてから index に入れるので、**画面に出したハンクと index の中身が静かに
  食い違い**、`git status` は `MM` のまま残る。`error` だと逆に操作ごと落ちる。
  どちらも「見えているハンクをそのまま index へ」を壊す（git 2.55 で確認）。
  argv の組み立ては 3 変種で 1 本の関数に通し、変種を足したときに付け忘れない
  ようにする。`apply.ignoreWhitespace` も同じ性質を持つが、**結果が変わるケースを
  作れなかったので旗は足していない**（作れたら足す）
- **ハンクの discard だけ `--cached` が付かない。** `StageHunk` / `UnstageHunk` は
  index を書き換えるが、`DiscardHunk` はワークツリーのファイルを書き換える。
  **index とワークツリーの両方に適用する変種は作らない。** `git apply` は 1 回で
  片方にしか効かず（`--index` は「両方」だが index とワークツリーが一致していることを
  要求するので、staged な変更があると `does not match index` で落ちる。git 2.55 で
  確認）、2 回に割ると
  片方だけ通った中途半端な状態が残る。staged なハンクの discard は「先に unstage して
  ください」で断る（07 の Diff ペイン）
- **discard は tracked / untracked / 「HEAD に無い」の 3 変種に分ける。** untracked な
  ファイルは `git restore` では消せず `git clean` が要る。HEAD に無いファイル
  （`FileStatus::Added`）は、コミットが 1 つも無いと `restore --source=HEAD` 自体が
  落ちるので `git rm` が要る。`to_commands` は純粋関数で index もファイルシステムも
  見られないので、どれを出すかは区画と変更種別を知っている呼び出し側
  （`WorktreeSnapshot` を持つ `app`）が決める

## argv への変換

`GitOp` から argv と stdin を作るのは純粋関数 `to_commands`。argv だけでなく stdin も
一緒に返すのは、「どの引数で何を標準入力に流すか」が 1 つの判断だから（`git apply --cached -`）。
2 つの関数に割ると、変種を足したときに片方だけ直る。

```rust
pub struct GitCommand { pub argv: Vec<String>, pub stdin: Option<Vec<u8>> }

pub fn to_commands(op: &GitOp) -> Result<Vec<GitCommand>, ArgvError>;
```

**返すのは呼び出しの列。** 下の表の変種はほとんどが 1 本で、複数を返すのは `CheckoutPr` と
`RebaseContinue { reword: Some }` だけ（下の「複数の手順を持つ書き込み」）。

`argv` に `git` 自身は含めない（`Runner` が付ける）。`Result` なのは非 UTF-8 のパスを
argv に載せられないため。`git_read` が非 UTF-8 のパスを一覧ごと落とすので実際には来ないが、
`GitOp` は公開型なのでここでも塞ぐ（黙って lossy 変換すると別のファイルを操作してしまう）。

| GitOp | argv |
|---|---|
| `Stage(paths)` | `add -- <paths>` |
| `Unstage(paths)` | `reset --quiet -- <paths>` |
| `StageHunk { patch }` | `apply --cached --whitespace=nowarn -`（patch を stdin） |
| `UnstageHunk { patch }` | `apply --cached --reverse --whitespace=nowarn -` |
| `DiscardHunk { patch }` | `apply --reverse --whitespace=nowarn -`（`--cached` は付けない） |
| `Discard(paths)` | `restore --source=HEAD --staged --worktree -- <paths>` |
| `DiscardUntracked(paths)` | `clean --force -- <paths>` |
| `DiscardAdded(paths)` | `rm --force --quiet -- <paths>` |
| `Commit { message, amend }` | `commit [--amend] --message <message>` |
| `BranchCreate { switch: true }` | `switch --create <name> [<start>]` |
| `BranchCreate { switch: false }` | `branch <name> [<start>]` |
| `Switch(name)` | `switch <name>` |
| `BranchDelete { name, force }` | `branch --delete [--force] <name>` |
| `BranchRename { from, to }` | `branch --move -- <from> <to>` |
| `Fetch { remote, prune }` | `fetch [--prune] [<remote>]` |
| `Pull { mode }` | `pull --no-rebase` / `--rebase` / `--ff-only` |
| `Push { .. }` | `push [--force-with-lease=<dst\|src>:<lease>] [--set-upstream] <remote> <src>[:<dst>]` |
| `RebaseInteractive { onto, plan }` | `rebase --interactive --no-update-refs <onto>`（`plan` は JSON にして stdin へ。下の「Interactive rebase の実装」） |
| `RebaseContinue { reword: None }` / `Skip` / `Abort` | `rebase --continue` / `--skip` / `--abort` |
| `RebaseContinue { reword: Some }` | `update-ref --stdin`（stdin `verify HEAD <head>`）→ `commit --amend`（メッセージは commit editor が差し込む）→ `rebase --continue`（下の「Interactive rebase の実装」） |
| `CherryPick(oids)` | `cherry-pick <oids>` |
| `CherryPickContinue` / `Skip` / `Abort` | `cherry-pick --continue` / `--skip` / `--abort` |
| `Revert { oids, mainline }` | `revert [--mainline <n>] <oids>` |
| `RevertContinue` / `Abort` | `revert --continue` / `--abort` |
| `MergeAbort` | `merge --abort` |
| `StashPush { .. }` | `stash push [--include-untracked] [--message <m>] [-- <paths>]` |
| `StashApply(i)` / `Pop` / `Drop` | `stash apply` / `pop` / `drop` `stash@{i}` |
| `SetAccount { login }` | `config --local -- mewgit.user <login>` |
| `UnsetAccount` | `config --local --unset -- mewgit.user` |
| `CheckoutPr(plan)` | 05 の「mewgit の手順」の表（場面ごとに複数） |

決めたことと理由:

- **`SetAccount` はアカウント解決の結果を `.git/config` に憶える**（`Modal::AccountPicker`
  の `Enter`。01 の「アカウントの選び直し」）。**`git` CLI を通す理由に不変ルール 1 を
  引かない** —— あのルールの理由は **hook が走らないこと**で、名指しされているのは
  commit / checkout / rebase / stash。`git config --local` は hook を 1 つも走らせない
  ので、引くと「config の書き込みは `GitOp` でなければならない」と読めてしまう。
  ここで CLI なのは**「読みは git2、書きは CLI」の分担**と、失敗の argv と git の出力が
  `ExecError` で UI まで届く経路（不変ルール 4）に乗せるため。
  **キーの名前は `auth::MEWGIT_USER` の 1 本**から取る —— 読む側
  （`auth::resolve` の git2）と別々に書くと、片方を直したときに黙って別のキーを
  読み書きする。実測（git 2.55、`env -i` の一時リポジトリ）:

  | 確かめたこと | 結果 |
  |---|---|
  | `config --local mewgit.user <値>`（`--` 無し） | 値が `--global` / `--unset` / `--list` / `-4` でも**そのまま値として書かれる**（exit 0） |
  | `config --local -- mewgit.user <値>` | **同じ結果。** `git config` はキーより後ろをオプションとして読まない |
  | linked worktree での `--local` | 共有の `.git/config` に書く（`config.worktree` は作られない）= 読み側の git2 `ConfigLevel::Local` と同じ段 |
  | `mewgit.user` に値が 2 つあるとき `config --local -- mewgit.user <値>` | **exit 5**（`cannot overwrite multiple values with a single value`）。mewgit は `--add` しないので手で書いたときだけ。**畳まない**（`--replace-all` にすると、手で書いた別の値を黙って消す） |

  **`--` は守りになっていない**（上の 2 行目）。それでも置くのは、外したときの根拠が
  「いまの git はそう読まない」だけになるから —— `git branch --move` はまさにそれで
  足をすくわれた（下の項）。**「守っている」と書かない**のが `--` を置くことより大事で、
  書くと `-` 始まりの login をここで弾いたつもりになる
- **`UnsetAccount` は `.git/config` から `mewgit.user` を外す**（`Modal::AccountPicker`
  の `u`。01 の「アカウントの選び直し」）。**`--` は `--unset` より後ろ**に置く。
  実測（git 2.55、`env -i` の一時リポジトリ）:

  | 確かめたこと | 結果 |
  |---|---|
  | `config --local --unset -- mewgit.user` | exit 0。値が 1 つだけ残っていた `[mewgit]` は**節ごと消える** |
  | `config --local -- --unset mewgit.user` | `error: key does not contain a section: --unset` で **exit 2**。`--` より後ろは全部キーと値として読まれる |
  | 設定されていないキーに `--unset` | **exit 5、stderr は空** |
  | 値が 2 つあるキーに `--unset` | exit 5（`warning: mewgit.user has multiple values`）。`--unset-all` にはしない —— 手で書いた別の値を黙って消す |

  **3 行目が 01 の「成否で分岐しない」の根拠。** 断りの文言が出ないので、失敗から
  「いま何が設定されているか」は読めない —— 読み直して `.git/config` の実物に
  答えさせる。**`--` は `SetAccount` と形を揃えて置くだけ**で、ここでも守っていない
  （`UnsetAccount` は値を取らず、人が打った文字も外から来た名前も positional に載らない）
- **パスの前には必ず `--` を置く。** `-n` や `HEAD` という名前のファイルがオプションや
  revision として解釈されるのを防ぐ
- **人が打った文字や外から来た名前が positional に載るところにも `--` を置く。**
  いまは `BranchRename` の `to`（`Modal::Input` が受けた文字がそのまま来る）と
  `SetAccount` の `login`（上。**こちらは実測では効いていない**）の 2 つ。
  `git branch` は parse-options なので、`git branch --move topic --force` は
  「`--force` へ改名」ではなく**「HEAD を `topic` へ強制改名」**と読まれ、元の
  `topic` が黙って消えて **exit 0 で返る**（git 2.55 で確認）。`--` があれば
  `fatal: '--force' is not a valid branch name` で断られる。
  **`BranchCreate` には置かない** — `switch --create` は次の argv を無条件に名前として
  食うので `-` 始まりでも fatal になり、逆に `--create --` と書くと `--` 自体が名前に
  なって `fatal: invalid reference` で壊れる（git 2.55 で確認）
- **unstage は `restore --staged` ではなく `reset`。** コミットが 1 つも無いリポジトリ
  （unborn HEAD）で `git restore --staged` は `fatal: could not resolve 'HEAD'` で落ちる。
  `git reset -- <paths>` は同じ状態でも通る（git 2.55 で確認）。`--quiet` は
  「Unstaged changes after reset:」の一覧を出させないため
- **ブランチの切替は `checkout` ではなく `switch`。** `git checkout <名前>` は同名の
  ブランチとファイルがあるとブランチを優先するが、**ファイルだけが存在する名前**では
  「そのファイルを index から取り出す」と解釈してワークツリーの変更を黙って捨てる。
  `switch` は pathspec を取らないので `fatal: invalid reference: <名前>` で止まる
  （git 2.55 で確認）。`--` を置いて `checkout` を守る形と違い、`--` を落としたときに
  黙って危なくなる余地が無い。**`Checkout` が受けるのはブランチ名だけ**で、commit を
  detached で取り出すには `switch --detach` が要る（要るようになったら別の変種にする）
- **変種名は `Checkout` から `Switch` に改めた**（ブランチの作成とリネームのタスクで
  一緒に決めた）。argv が `switch` なのに変種名が `checkout` だと、同じ「作ってから
  切り替える」を 2 つの語彙で書くことになる。`BranchCreate { checkout: bool }` も
  `switch: bool` に揃えた。**`InputIntent`（01）は真偽値を持たない** — UI からの作成は
  常に切り替えるので、`switch: false` を組む経路が無い（`branch <name>` の側は
  `to_commands` のテストでだけ踏む）
- **force push は `--force-with-lease`。`--force` は他人のコミットを消せるので使わない。**
  **値（`=<dst>:<lease>`）を必ず添える。** 省くと lease は remote-tracking ref
  （`origin/main`）と比べるだけになり、`git fetch` を挟むと他人が進めた位置がそこに入って
  検査を素通りする（git 2.55 で確認）。「push が弾かれた → fetch して中身を見る →
  やはり force する」は UI が普通に通す道なので、素の lease は守りにならない。
  `lease` には確認モーダルで見せた位置をそのまま渡す
- **`force: bool` と `expected: Option<Oid>` に割らず、`lease: Option<Oid>` 1 つで表す。**
  2 つに割ると `force: true, expected: None` が組めてしまい、それがまさに上で
  「守りにならない」と書いた裸の `--force-with-lease` になる。**旗と値を 1 つにすれば
  その組み合わせは型として存在しない。** 前の形は `force` を出す経路が無いあいだだけ
  安全で、force push を配線した時点で踏める腕になっていた
- **行き先は `src` と `dst` に割る。** refspec（`<src>[:<dst>]`）と lease の refname は
  **同じ「リモート側の ref 名」を要求するが、置き場所が違う** —— refspec では右辺、lease
  では先頭。1 本の文字列に畳むと、`x` が `refs/heads/y` を追跡している形で
  `--force-with-lease=x:y:<oid>` という壊れた値になる。`dst` が `None`（ローカル名と
  同じ）なら refspec は `src` だけ、lease の refname も `src`。**lease の refname は
  短縮名（`main`）でも完全な ref 名（`refs/heads/main`）でも効く**ので、refspec の右辺に
  渡すのと同じ文字列をそのまま使う（git 2.55 で確認）
- **pathspec が空の `GitOp` は `ArgvError::EmptyPathspec` で拒否する。** `--` だけを
  置くと git は「指定なし」と読み、対象がリポジトリ全体に広がる。`reset --quiet --` は
  index 全体を戻し、`clean --force --` は untracked を全部消す（git 2.55 で確認）。
  「0 件を discard します」と確認して全部消えることになる。`StashPush { paths: None }` は
  「全部」を表す正当な指定なので、`Some(vec![])` とは区別する
- **HEAD に無いファイルの discard は `rm --force`。** `restore --source=HEAD --staged
  --worktree` は HEAD が無いと `fatal: could not resolve 'HEAD'` で落ちる。
  `git rm --force -- <paths>` は unborn でも通り、index からもワークツリーからも消える。
  これは HEAD があるときに staged な `Added` へ `restore` を掛けた結果（ファイルごと
  消える）と同じで、ワークツリー側が既に消えている staged 削除でも通る（git 2.55 で確認）。
  **`Discard` の代わりに無条件で使ってはいけない。** HEAD にあるファイルへ掛けると、
  変更ではなくファイルそのものが消える。**「HEAD に無いか」は `RepoInfo.head` では
  判断しない。** あれは起動時に 1 回読むきりで、外部でコミットされると古くなる。
  `FileStatus::Added` は `LoadStatus` が毎回運んでくるので、そちらで決める
- **untracked の discard に `-d` は付けない。** `git_read::status` が
  `recurse_untracked_dirs(true)` で列挙するので、渡すのは常にファイル。ディレクトリごと
  消す余地を残さない
- `switch` / `restore` を使うので git 2.23 以降が要る

### stash push のパス指定

部分 stash（Worktree の `Z`。01 の「stash の書き込み」）は `StashPush { paths: Some([1 つ]) }`
で、**常に `--include-untracked` を付ける**。パスで絞ればほかの untracked は退避されない
ので、付けて困る形が無い。**実測（git 2.55.0、2026-09-30。削除と rename の元のパスの行は `/code-review` の指摘で測り直した）** —— `a` / `b` / `c` を
コミットした一時リポジトリで、状態ごとに作り直して `stash push [-u] -- <path>` を叩いた:

| 状態 | 渡したパス | 結果 |
|---|---|---|
| unstaged の modified（ほかに untracked `u`） | `-u -- a` | exit 0。stash は `M a` だけ、`u` は残る |
| staged だけ / staged と unstaged の両方 | `-u -- a` | exit 0。両方の区画ぶんが 1 つの stash に入る |
| untracked（ほかに untracked `v`） | `-u -- u` | exit 0。stash は `A u` だけ、`v` は残る |
| untracked | `-- u`（`-u` 無し） | **exit 1**、`pathspec ':(prefix:0)u' did not match any file(s) known to git` |
| staged の追加（`A`）/ `AM` / unstaged の削除（` D`）/ `MD` / `MM` / staged・unstaged の typechange | `-u -- <path>` | exit 0 |
| staged の削除（`git rm b`） | `-u -- b` | **stash を作ってから exit 1**（`fatal: pathspec ':(prefix:0)b' did not match any files`）。作業ツリーは `D  b` のまま —— 同じ変更が 2 箇所に残る |
| `rm --cached b`（`D  b` と中身が同じ `?? b`） | `-u -- b` | **stash を作り作業ツリーも片付けてから exit 1**（`error: No valid patches in input (allow with "--allow-empty")`） |
| `git rm b` のあと中身の違う `b` を置く（`D  b` と `?? b`） | `-u -- b` | exit 0 —— 上と同じ形が中身次第で割れる |
| `AD`（staged の追加を作業ツリーから消す） | `-u -- n` | **stash を作り作業ツリーも片付けてから exit 1**（`No valid patches in input`） |
| staged の rename `a → a2` | `-u -- a2` | exit 0。stash は `R100 a a2` だが、**作業ツリーに `D  a` が staged のまま残る** |
| 同上 | `-u -- a a2` | **stash を作ってから exit 1**（`fatal: pathspec ':(prefix:0)a' did not match any files`）。作業ツリーは `R  a -> a2` のまま |
| 同上 | `-u -- a` | **stash を作ってから exit 1**（同じ `pathspec … did not match`）。作業ツリーは `R  a -> a2` のまま |
| staged の rename のあと元のパスに新しい `a` を置く（`R  a -> a2` と `?? a`） | `-u -- a` | exit 0 だが **rename が割れて `A  a2` が残る**。できた stash は `stash show --include-untracked` が `duplicate entries: a` で exit 128、`pop` は `could not restore untracked files` で exit 1 |
| `a` が `UU`（`stash pop` のコンフリクト）| `-u -- a`、`-u -- b`、パス無し | **どれも exit 1**、stdout に `a: needs merge`、stderr に `error: could not write index` |
| `a` を staged、`b` を unstaged で変更 | `-u -- b` | exit 0。**stash には `M a` も入る**（下） |

- **rename は 1 ファイルでは退避させない**（2026-09-30 にユーザーと決めた）。どう渡しても
  中途半端になる（上の rename の 4 行）。ワークツリー上の rename（`status` が対にした
  削除と untracked）も同じ `FileStatus::Renamed` で断る。**元のパスの行でも断る** ——
  `WorktreeState::entries_at` は `old_path` でも当たる
- **staged の削除と `AD` も 1 ファイルでは退避させない**（rename を断ると決めたのと同じ
  理由。`/code-review` が拾った —— 最初の表は unstaged の削除（`rm b`）しか測って
  おらず、「削除は exit 0」と書いていた）。stash を作ってから exit 1 で終わり、
  失敗の通知の裏で stash が 1 つ増える。`D  b` と `?? b` の組は中身次第で exit 0 にも
  なるので、staged の削除があれば区別せず断る。**`MD` と unstaged の削除と `A` だけは
  通す**（exit 0 で片付く）。判定は `AppState::splits_alone`
- **unmerged のパスがあれば、全体でも部分でも断る**（2026-09-30 にユーザーと決めた）。
  進行中操作の門（`progress::refusal`）は `stash pop` のコンフリクトを拾わない ——
  あれは進行中操作ではない（continue が無い）。git の理由は stdout にしか出ず、
  stderr の `could not write index` からは読めない。**門が見るのは mewgit の snapshot** なので、
  外で作った unmerged を読む前は git まで届く。そのときは `classify` が `needs merge` を
  `Unmerged` に分け、同じ断りの文を出す（下の「エラー分類」）
- **部分 stash は index 全体を記録する**（git の仕様）。ほかに staged のファイルがあると、
  それも stash の中身に入り（作業ツリーでは staged のまま残る）、後で `apply` すると
  そのファイルの変更も戻る（上の表の最後の行を `reset --hard` してから `apply` すると
  ` M a` / ` M b`）。**断らない** —— stash の diff（Stash ペインの `Enter`）には両方が
  出るので、入ったものは見える

## ハンクからパッチを組む

`GitOp` の `patch` を作るのも純粋関数。`git_read` が持つ 1 つの `Hunk`（02 の
「ハンクの持ち方」）を `git apply` に渡せるパッチにする。

```rust
pub fn hunk_patch(path: &Path, hunk: &Hunk) -> Result<Vec<u8>, ArgvError>;
```

出力はこの 4 つを並べたもの。

```
diff --git a/<path> b/<path>
--- a/<path>
+++ b/<path>
<hunk.header>
<行>...
```

- **`index <blob>..<blob> <mode>` の行は出さない。** blob の hash を `git_read` が
  持っておらず、無くても `git apply` は通る（git 2.55 で確認）。3-way merge
  （`--3way`）は使わないので落ちる経路も無い
- **ハンクのヘッダ（`@@ -a,b +c,d @@`）はそのまま載せる。** 数を組み直さない。
  `git apply` はヘッダの行数と実際の行数が食い違うと `corrupt patch` で落ちるので、
  数え直しは「元は正しかったものを壊す」側にしか働かない。開始行のほうは
  3 方向とも当てたい側の位置が入っている（stage は `-` 側 = index、unstage は
  `+` 側 = index、discard は `+` 側 = ワークツリー）ので、2 つ目以降のハンクだけを
  送っても探索なしで当たる
- **行は `<記号><content>` に `\n` を足したもの。** `content` は改行を落として
  持っているので（02）、`\r` はそのまま残り CRLF のファイルが壊れない
- **`*NoEol` の行の後ろにだけ `\ No newline at end of file` を足す。** マーカーは
  ヘッダの行数に数えない（git の出力もそうなっている。git 2.55 で確認）
- **パスは quote しない。** 非 ASCII も空白も、そのままの UTF-8 バイト列で
  `git apply` が受ける（git 2.55 で確認）。`git diff` の出力は
  `core.quotePath` の既定で非 ASCII を `"a/\346\227\245.txt"` と quote するが、
  受け側は両方を読める。`diff --git a/x b/y.txt b/x b/y.txt` のように `diff --git`
  行だけでは割り切れない名前でも、`---` / `+++` の 2 行から決め直すので当たる
- **ただし制御文字を含むパスと `"` で始まるパスは `ArgvError::UnquotablePath`
  で拒否する。** quote しない以上、tab は名前の終わりと読まれ（`---` の行は tab で
  区切る決まり）、`\r` は CRLF のパッチとして落とされ、`"` で始まる名前は quote
  されたものとして unescape される。どれも**別のファイルに patch を当てる**形の
  失敗なので、静かに通さない。禁じる文字を数え上げるのではなく「制御文字か」で
  弾くのは、受け側の細かい規則を写し取らずに済ませるため。
  ファイル単位の stage / unstage / discard はパスを argv に載せるだけなので、
  こういう名前でも通る
- 非 UTF-8 のパスは `to_commands` と同じ `ArgvError::NonUtf8Path`。`git_read` が
  一覧ごと落とすので実際には来ない

## Runner

```rust
pub struct Runner { repo_root: PathBuf, git_bin: PathBuf, sequence_editor: OsString, commit_editor: OsString, env: Vec<(OsString, OsString)>, .. }

impl Runner {
    pub fn new(repo_root: impl Into<PathBuf>, git_bin: impl Into<PathBuf>, ssh: SshCommand,
               mewgit_bin: impl Into<PathBuf>) -> Self;
    /// 実行時に重ねる環境変数。テストの隔離に使う。
    pub fn with_env(self, vars: ...) -> Self;
    pub async fn run(&self, op: &GitOp) -> Result<ExecOutput, ExecError>;
    /// 中断できる形（下の「キャンセル」）。
    pub async fn run_cancelable(&self, op: &GitOp, cancel: oneshot::Receiver<()>)
        -> Result<ExecOutput, ExecError>;
}

pub struct ExecOutput { pub argv: Vec<String>, pub stdout: String, pub stderr: String }

pub enum ExecError {
    /// git の終了状態を得られなかった。argv を組めなかった / 起動できなかった /
    /// 終了を待てなかった。
    NoResult { argv: Vec<String>, reason: String },
    /// ユーザーが中断した（下の「キャンセル」）。
    Canceled { argv: Vec<String> },
    /// ハンクを読んだあとに対象が変わっていた。git は起動していない（下の「古いハンクの守り」）。
    Stale { argv: Vec<String>, path: PathBuf },
    /// git が非 0 で終わった。
    Failed { argv: Vec<String>, code: Option<i32>, stdout: String, stderr: String, kind: ExecErrorKind },
}
```

- **`Failed` は stdout も運ぶ。** git は失敗の理由を stdout にだけ書くことがある
  （何も stage していない `commit`、未解消の `rebase --continue`、複数の revert が空で
  止まった形。下の「エラー分類」）。分類（`classify`）だけが読んで捨てていたので、
  通知には argv しか出なかった。`app` は stdout を `detail` に載せ、stderr と stdout から
  選んだ理由の行を要約にする（下の「理由の行」）

- argv と stdin の組み立ては純粋関数 `to_commands()`（「argv への変換」）
- `git_bin` は `config` の `general.git_bin`（既定は `"git"`、絶対パスも書ける。08）から渡す。
  引数にしているのは、設定を読んだのに渡し忘れる経路を作らないため
- `mewgit_bin` は interactive rebase の sequence editor として起こす自バイナリ
  （Linux では `/proc/<pid>/exe`、ほかは `std::env::current_exe()`。下の「Interactive
  rebase の実装」の「消えた自バイナリ」）。`git_bin` と同じ理由で
  引数にする。core は自分がどのバイナリに入っているかを知らない。`new` が引用して
  `GIT_SEQUENCE_EDITOR` と `GIT_EDITOR` の値に組んでおく。**パスそのものも持つ** ——
  エディタの失敗のあとに、そのパスがまだ在るかを見る（下の「エラー分類」の `EditorGone`）
- **`ExecError` を割るのは、失敗の種類で埋まるフィールドが違うため。** 1 つの
  struct にすると「起動していないときは `code` が `None`、`stderr` に git ではない文言、
  `kind` は分類表に載らない値」という約束を、読む側が毎回思い出すことになる。
  `Canceled` も同じ理由で 3 つ目にした（中断した git の `code` と `stderr` は
  signal の言い換えでしかない）。`Stale` も同じで、git を起こしていないので `code` も
  `stderr` も無く、代わりに古くなったパスを持つ
- 環境変数: `GIT_TERMINAL_PROMPT=0`、`LC_ALL=C`（エラー文の検出を安定させる）、`GIT_EDITOR='<mewgit_bin>' --commit-editor` と `GIT_SEQUENCE_EDITOR='<mewgit_bin>' --sequence-editor`（端末のエディタを起こさない）、`GIT_CONFIG_*` で `core.commentString=#mewgit#`（**3 つともどのコマンドにも載せる**。下の「Interactive rebase の実装」）。`with_env` の指定はこの後に重ねるので、テストは上書きできる
- **`GIT_TERMINAL_PROMPT=0` では ssh のパスフレーズ入力を止められない。** `ssh` は
  stdin ではなく `/dev/tty` を直接読むので、agent に載っていない鍵だと止まる
  （タイムアウトは設けない方針。抜けるのは下の「キャンセル」）。**画面は壊れない** ——
  git を自分のプロセスグループで起こしている（下の「キャンセル」）ため、そこから
  `/dev/tty` を読む子孫は SIGTTIN で停止する（別グループの子が `/dev/tty` を読むと
  `stat` が `T`、同じグループなら `S+` で入力待ちになることを実測）。**プロンプトが
  出ないまま止まる**ので、押した本人にはなお何が起きているか判らない。
  **実効 ssh の後ろに `-o BatchMode=yes` を重ねて塞ぐ**（下の「BatchMode の重ね方」）

### 古いハンクの守り

**ハンクの 3 操作は、git を起こす前に対象のファイルの指紋を採り直し、`expect` と
違えば起こさずに `ExecError::Stale` で返す**（`GitOp::stale_at`）。どこで比べるかと
なぜ 2 つの指紋を持つかは 01 の「外部の変更の検知 › 古いハンクの守り」。

守りが無いときの実測（2026-10-03、git 2.56.0。一時リポジトリで `line2 → two-A` の
差分を `git diff` で採り、ワークツリーを `two-B` に書き換えてから当てた。2026-09-27 の
git 2.55.0 の pty の実測も同じ結果）:

| 操作 | 結果 |
|---|---|
| stage（`git apply --cached`） | **exit 0。index に `two-A`（HEAD にもワークツリーにも無い）が入り `MM`** |
| discard（`git apply --reverse`） | exit 1（`patch does not apply`）。ワークツリーは無傷 |

- **指紋はワークツリーの `(mtime, len)`、index の blob、HEAD のツリーの blob**（02 の `PathStamp`）。採るのは `git_read` の
  `RepoReader::path_stamp` で、diff の側と同じ 1 本（00 のレイヤーの例外）。git2 は同期
  なので `spawn_blocking` に載せ、**ハンク操作のたびにリポジトリを開く**。コスト
  （2026-10-03、release、macOS、200 回の平均。`/simplify` の efficiency が測った）:

  | リポジトリ | index | 開いて採る（いまの形） | 開いたままの reader で採る | git を 1 本起こす |
  |---|---|---|---|---|
  | mewgit | 13 KB | 333 µs | 12 µs | 3.3 ms |
  | 中規模（Android アプリ） | 184 KB | 856 µs | 15 µs | 3.3 ms |
  | 合成した 5 万ファイル | 3.6 MB | 11.2 ms | 62 µs | 3.4 ms |

  キーを 1 回押すたびに 1 回なので払う。リーダースレッドの reader を使い回せば安いが、
  `LoadLog` の後ろに並ぶ（照合から実行までが開く）うえ、`Runner` に `ReaderHandle` を
  持たせることになる
- **比べるのは、パッチの基準と当てる先だけ**（`GitOp::stale_at`）——

  | 操作 | パッチ | 比べる側 | 比べない側 |
  |---|---|---|---|
  | stage / discard | index → ワークツリー（`IndexVsWorktree`） | ワークツリー、index | HEAD（外の commit でもパッチは変わらない） |
  | unstage | HEAD → index（`HeadVsIndex`）、当てる先は index | index、HEAD | ワークツリー（編集を続けながら staged なハンクを外す形を断らない） |

  **unstage は HEAD も見る。** 外の `git commit` は index もワークツリーも動かさず HEAD
  だけを動かすので、最初の版（index だけを比べていた）は、commit のあとの古い staged な
  ハンクの `u` を通し、**exit 0 で index に `line2`（HEAD にもワークツリーにも無い）を
  入れた**（`/code-review` が拾った。git 2.56.0 で再現 —— `two-A` を stage して
  `git diff --cached` を採り、commit してから `git apply --cached --reverse` で `MM`）
- **discard も守る。** 上の実測で git が断ったのは、書き換えがハンクのコンテキストに
  掛かった形だけ。古いパッチは、内容の同じハンクが 2 つあるファイルでオフセット追従に
  拾われて別のハンクに当たりうる（01 の「diff のキャッシュと鮮度」。#24）ので、git が
  断ることを守りの根拠にしない。3 操作を同じ規則で通す
- **受け入れた穴が 2 つ。** (1) 照合してから git がファイルを読むまでの隙間（ミリ秒）。
  (2) **同じ長さで mtime も同じ**書き換え（git 自身の racy-git と同じ形）。APFS と ext4 は
  mtime をナノ秒で持つので、同じ刻みに 2 回書かない限り起きない。秒単位のファイル
  システムでは 1 秒の中の書き換えを取りこぼす
- 断ったあとの読み直しと通知の出し方は 01（`Warn`、要約行にパスを入れない）

### キャンセル

```rust
pub async fn run(&self, op: &GitOp) -> Result<ExecOutput, ExecError>;
pub async fn run_cancelable(&self, op: &GitOp, cancel: oneshot::Receiver<()>)
    -> Result<ExecOutput, ExecError>;
```

**`git` は自分のプロセスグループの長として起動し（`process_group(0)`）、中断は
そのグループ宛てに SIGTERM を送る。** 実測で決めた形（macOS の git 2.55、`index.lock` を
握ったまま止まる clean filter で確認）。

| 送り方 | `index.lock` | git の子（ssh / hook / filter） |
|---|---|---|
| SIGKILL（`kill_on_drop`） | **残る**。以後の書き込みが全部落ちる | 孤児として生き残る |
| git 本体だけに SIGTERM | 消える | 孤児として生き残る |
| **グループ宛てに SIGTERM** | 消える | 一緒に死ぬ |

- **`kill_on_drop` は付けない。** あれが送るのは SIGKILL で、git は sigchain の後始末を
  せずに死ぬ。future を捨てるのは mewgit 自身が終わるときだけなので、そのまま走らせて
  git に自分で終わらせるほうが安全
- **殺したあと、終了を待ってから返す。** 待たずに捨てると、lock の削除が済む前に
  呼び出し側が次の書き込みを出せてしまう
- **待つのは「git が終わるまで」ではなく「pipe が閉じるまで」**（`wait_with_output` は
  `wait` と両方の `read_to_end` を join する。tokio 1.53 の実装で確認）。グループ宛ての
  signal は送った瞬間のメンバーに届くので、**直後に fork された子孫を取り逃がすと、
  その子孫が git の pipe を握っているぶん `ExecFinished` が遅れる**。短く打ち切らない
  のは、lock の削除より先に次の書き込みを出さないため。**その間の脱出口は 2 回目の
  `Ctrl-C`（終了）**（01 の「キャンセル」）
- **中断は失敗と型で分ける**（`ExecError::Canceled { argv }`）。git の exit code と stderr は
  signal で落としたことの言い換えにしかならず、`ExecErrorKind` の分類表にも載らない。
  UI は「押した本人が止めた」ものとして扱う（01 の「キャンセル」）。**出力は読まずに
  捨てる** —— fetch は progress を数十 KB 吐いてから殺されるので、使わないものを UTF-8
  検証に通さない
- **合図が届いたあとでも、git が成功していれば `Ok` を返す。** signal より先に終わって
  いた形で、書き込みの結果はリポジトリに残っている。`Canceled` にすると、済んだ commit を
  「中断した」と案内したうえに読み直しの経路まで変わる（`update::Reload` は成否で分かれる）
- `run` は送り手を作らずに `run_cancelable` へ渡す。受け側の `Ok(())` パターンが外れた枝は
  `select!` が無効にするので、その呼び方では中断の腕が二度と選ばれない

### 複数の手順を持つ書き込み

`GitOp::CheckoutPr`（05 の「mewgit の手順」）と、hook が断った reword を持つ
`GitOp::RebaseContinue`（下の「Interactive rebase の実装」）は git を続けて叩く。
**`Runner` が順に叩き、1 本でも非 0 で終わればそこで止めて、その 1 本の
`ExecError` を返す**（残りは叩かない）。成功なら**最後の 1 本の `ExecOutput`** を返す。

- **1 つの `GitOp` にまとめる理由は 05 の「一連の手順を 1 つの書き込みにする」。**
  `app` から見ると書き込みは 1 回で、`JobKind::Exec` も `Reload` も 1 回
- **止まった手順が何だったかは `argv` で判る。** `ExecError` はどの変種も
  `argv` を持っているので、通知の `detail` にはその 1 本の argv と git の出力が出る
  （不変ルール 4）。**どこまで済んだかは運ばない** —— 済んだ手順の結果は
  リポジトリに残っており、読み直しがそれを拾う（`update::Reload` は
  `CheckoutPr` を `RefsAndLog` に置く）
- **中断は手順のあいだでも効く。** 合図は 1 本の `oneshot` で、叩く前に
  「もう届いているか」を見る（届いていれば次を起こさずに `Canceled` を返す。
  `argv` は起こさなかった 1 本のもの）。走っている最中に届けば 1 本だけの
  書き込みと同じ（上の「キャンセル」）。**届いたことは憶えておく** —— 走っている
  1 本が合図より先に成功していた回（`canceled_by` が偽）に受け側を「もう来ない」と
  同じに畳むと、次の手順の前の確認が「届いていない」と答え、止めたはずの手順が
  走る（`/code-review` が拾った。`CancelSignal` の 3 状態）
- **`verb()` は時間のほとんどを使う 1 本の `argv[0]`**（`CheckoutPr` は `fetch`、reword を
  持つ `RebaseContinue` は `rebase`）。スピナーに出る語なので、**最初の 1 本とは限らない** ——
  reword の照合（`update-ref`）は一瞬で、残りの時間は `rebase --continue` が使う。最初の 1 本に
  揃えていたころは `CheckoutPr` でたまたま一致していた。見張りは `verb_matches_the_argv`
  （複数の変種はどの 1 本かもテストに書く。「列のどれかと一致」では、語が別の手順へずれても通る）

### BatchMode の重ね方

素の `GIT_SSH_COMMAND="ssh -o BatchMode=yes"` を置くと、ユーザーの
`GIT_SSH_COMMAND` / `core.sshCommand` を**捨てて**しまう。実効値を先に求めてから、
その後ろにオプションを足す。

```
実効 = $GIT_SSH_COMMAND ?? core.sshCommand ?? ($GIT_SSH があれば「触らない」) ?? "ssh"
GIT_SSH_COMMAND = "<実効> -o BatchMode=yes"
```

- **git は ssh コマンドをシェル経由で起動し、引数を後ろに足す**ので、末尾に書いた
  `-o` は host より前に並ぶ。`core.sshCommand='sh dump.sh -o BatchMode=yes'` で argv を
  ダンプして確認した（git 2.55）。優先順は `GIT_SSH_COMMAND` > `core.sshCommand` で、
  こちらが環境変数に書けば config 側は読まれない（同じ方法で確認）
- **`GIT_SSH` だけが設定されている環境では何も足さない。** `GIT_SSH` は引数の渡し方が
  違い（シェルを通さない）、`GIT_SSH_COMMAND` を置くと git はそちらを優先するので、
  重ねるつもりが差し替えになる。**その環境では守りが無く、しかもユーザーには何も
  出ない** —— 止まって初めて分かる（プロンプトすら出ない。上の `GIT_TERMINAL_PROMPT` の
  項）。抜ける手は実行中ジョブのキャンセル（下の「キャンセル」）で、**グループごと殺すので
  停止した ssh も一緒に死ぬ**（停止中のプロセスにも SIGTERM は届く。実測）。
  **通知を出す形にはしない** — 起動時に「あなたの環境は守られていません」と出しても、
  fetch を押すまで意味が無い
- **ユーザーが明示した `BatchMode=no` は勝つ。** ssh は同じ項目を**先に現れた値**で
  決めるので（`ssh -o BatchMode=no -o BatchMode=yes -G` が `batchmode no` を返す。
  OpenSSH 10.3 で確認）、実効値の中に書かれた設定はこちらの追記より前に来る。
  **黙って上書きしない方針と向きが揃っている**
- **`core.sshCommand` を読むのは `git_read`**（不変ルール 1。読み取りは git2）。
  `ReaderHandle` が `workdir` と同じく起動時に 1 回だけ読む。実行のたびには読み直さない
  ので、mewgit を起動したまま設定を変えても効かない
- **`Runner` が受け取るのは実効値（`SshCommand`）で、生の設定値ではない。** 決めたのに
  渡し忘れる経路を作らないよう `git_bin` と同じくコンストラクタの引数にする。
  **環境変数を読むのは `auth::ssh::effective_ssh_from_env` の 1 箇所だけ** —
  実効値を決める `effective_ssh` を純粋関数のままにしておくと、実行環境に
  `GIT_SSH_COMMAND` が置かれていてもテストの結果が動かない
- 実効値を決める関数は `auth::ssh` に置く。「git が実際に使う ssh はどれか」は
  `resolve_ssh_host` と同じ問いで、`git_exec` から `auth` への依存は層の向きに沿う（00）。
  **`resolve_ssh_host` をその実効値に向けるのは M4。** いまは呼び出す経路が無く
  （`RepoInfo.target` を埋めるのが M4）、実効値はシェル文字列なので `sh -c` 経由の
  起動に変える必要がある。踏めない腕を先に入れない（04 の「SSH エイリアスの正規化」）
- stdin は閉じる。例外はハンクの 3 操作（`StageHunk` / `UnstageHunk` / `DiscardHunk`）と
  `RebaseInteractive`。前者は `git apply … -` に patch を、後者は plan の JSON を渡す
  （git は読まず、起こした sequence editor が継ぐ）
- stdout / stderr は表示専用なので lossy で `String` にする
- タイムアウトは設けない（fetch の長さは予測不能）。代わりに UI から中断できるようにする
  （下の「キャンセル」）
終了状態を得られない経路（`to_commands` の失敗、古いハンクの守りが指紋を採れなかった、
起動の失敗、終了待ちの失敗）は、どれも `ExecError::NoResult` で返し、`reason` には git の
stderr ではなく mewgit の説明文を入れる。**中断はここに入れない** —— 終了状態は
得られており（signal で落ちた）、区別したいのは「失敗か、止めたか」だから
（`ExecError::Canceled`。上の「キャンセル」）。**守りが断ったのも入れない**
（`ExecError::Stale`）—— 確かめられなかったのではなく、確かめて古かった。

## エラー分類（ExecErrorKind）

出力のパターンで分類し、UI が案内文を出せるようにする。**判定は上から順**で、
先に当たったものを採る。非 FF は語が広くなりやすいので、認証系より後ろに置く。

**`rejected` だけで非 FF と読まない。** hook や保護ブランチが断った push も
`! [remote rejected] main -> main (pre-receive hook declined)` と書く（git 2.55 で確認）。
`NonFastForward` にすると `app` が「次の `P` は force push」と憶える（01 の
「force push」）ので、履歴と関係の無い拒否に「リモートを上書きします」の確認が出る。
git が非 FF を表すのは 4 つ —— push が断る `(non-fast-forward)`、`(fetch first)`、
lease が古いときの `(stale info)` と、`pull --ff-only` が断る
`fatal: Not possible to fast-forward, aborting.`。

**4 つ目は `rejected` を含まない。** `pull --ff-only` は履歴が分かれているとこの
`fatal:` だけを書き、上の 3 語をどれも出さない（git 2.55 で確認）。**拾わないと
`p` の失敗が `Other` に落ち、案内文の側から消える** —— まさに案内が要る場面なのに
「git の実行に失敗しました」しか出ない。`hint:` の行は `advice.diverging false` で
消えるので、見るのは `fatal:` の側。

**広げたので読み手を数え直す。** `NonFastForward` を見るのは 2 人。`push_rejection`
（force を arm する。01 の「force push」）は `GitOp::Push` で門番しているので
`Pull` の失敗では立たない。もう 1 人は案内文（01 の「失敗の案内文」）で、そちらも
op で分けている。**#49 の「分類の読み手が増えたら偽陽性が何を引くかを数える」の
裏返し** —— 語を増やす側でも同じ数え方が要る。

**見るのは stderr だけではなく stdout も。** `git merge`（`pull --no-rebase` の経路）は
コンフリクトのとき `CONFLICT (content): ...` と `Automatic merge failed` を **stdout** に
出し、stderr には何も書かない。stderr だけで分類すると `Other` に落ちる
（git 2.55 で確認）。cherry-pick / rebase は `CONFLICT` が stdout、
`error: could not apply` が stderr と分かれる。**コンフリクトの印は行頭で見る** ——
複数の cherry-pick は積んだぶんの `[main <oid>] <件名>` を stdout に並べるので、件名に
`CONFLICT` を含むコミットを積んでから空に当たると、行のどこでも拾う形では `Empty` を
`Conflict` と取り違え、continue を案内する（`/code-review` が踏んだ）。行頭の形は
cherry-pick / revert / merge / rebase / `pull --rebase` / `pull --no-rebase` で同じ
（git 2.56.0、2026-10-01 に実測）。

| パターン | kind |
|---|---|
| stderr に `there was a problem with the editor` があり、**`Runner` の `mewgit_bin` が無い**（`classify` の外で `Runner` が見る） | `EditorGone` |
| stderr の行頭の `mewgit: [head-moved]`（mewgit の sequence editor が出す印） | `HeadMoved` |
| stderr の行頭の `fatal: cannot lock ref 'HEAD': is at ` で、同じ行に ` but expected `（reword を持つ continue の照合。下の「Interactive rebase の実装」） | `HeadMoved` |
| `SAML SSO` / `organization has enabled or enforced SAML` | `SamlSsoRequired` |
| `Permission denied (publickey)` | `SshAuthFailed` |
| `Authentication failed` | `HttpsAuthFailed` |
| 行頭の `CONFLICT (` / `error: could not apply` | `Conflict` |
| `is now empty` | `Empty` |
| `nothing to commit` / `nothing added to commit` / `no changes added to commit` | `NoChanges` |
| stdout の行末の `: needs merge` / `Committing is not possible because you have unmerged files` | `Unmerged` |
| `non-fast-forward` / `fetch first` / `stale info` / `Not possible to fast-forward` | `NonFastForward` |
| その他 | `Other` |

**`HeadMoved` は先頭。** 印は mewgit 自身が出す ASCII の定数（`git_exec::rebase` の
`HEAD_MOVED_MARKER`）で、`EditorError::HeadMoved` の `Display` が先頭に埋め、隠しサブコマンドが
`EDITOR_STDERR_PREFIX`（`mewgit: `）を付けて stderr に書く。**日本語の文言を鍵にしない** ——
文言を直すたびに分類が外れる。**照合は stderr の行頭だけ** —— git はコミットの件名を出力に
写す（`error: could not apply <oid>... <件名>`、rebase の stdout にも `# <件名>`。git 2.56.0 で
実測）ので、行のどこでも拾うと件名が `[head-moved] …` のコミットのコンフリクトを取り違える
（`/code-review` が踏んだ）。件名は行の途中にしか出ないので、行頭で合わせれば先頭に置いてよい。**照合の断りは
`cannot lock ref` だけで拾わない** —— 同じ語は lock ファイルの取り合いでも出る（そちらは
「HEAD が動いた」ではない）。`is at … but expected` まで揃ったときだけ `HeadMoved` にする。

**`EditorGone` は起動中に mewgit のバイナリが消えた形**（下の「Interactive rebase の
実装」の「消えた自バイナリ」）。ほかのどの kind より先に採る —— エディタが起きられなければ、
git がその先で何を書いても原因はそこにある（reword の行で止まると stderr に `You can amend
the commit now` も並ぶ）。見分けは 2 つ揃えたときだけ:

- **`there was a problem with the editor` だけでは決めない。** 自バイナリのエディタが自分で
  断った形（HEAD の照合の `[head-moved]`、plan の無い `--sequence-editor`）も同じ行を書く
- **shell の文言では照合しない。** 起こせなかったときの行は shell で割れる —— macOS の `sh` は
  `<path>: No such file or directory`、Debian の dash は `1: <path>: not found`（実測は下の表）
- **パスを見るのは失敗のあとだけ。** op ごとに起こす前に見ると、エディタを起こさない大半の
  書き込みにも stat が載る。`Runner` は変種を見ない（上の「Runner」）ので、どの op が
  エディタを起こすかで分けることもできない

`classify` は出力だけの純粋関数のまま残し、`Runner` が結果を上書きする。

**`NoChanges` は `Empty` より後ろ。** 空で止まった cherry-pick の stdout の末尾にも
`nothing to commit, working tree clean` が出る（git 2.55.0 で実測）ので、先に見ると
進行中操作が残っているのに「何も始まらなかった」と読む。**3 つの語は作業ツリーの状態で
割れる同じ結び**（clean / untracked がある / tracked が dirty。下の「revert の開始」）。
**`git commit` も同じ語を出す**（stage したものが無いとき）ので `Commit` の失敗も
`NoChanges` になるが、案内は op で分けてあり、commit には出さない（01 の「失敗の案内文」）。

**`Unmerged` は未解消のファイルが残っているのに書き込もうとした形**で、入口で文言が割れる
（git 2.56.0、2026-10-03 に一時リポジトリで実測。UU / AA の状態で叩いた）。

| 操作 | exit | stdout / stderr |
|---|---|---|
| `stash push`（全体・パス指定）/ `stash apply` | 128 | stdout `<path>: needs merge`、stderr `error: could not write index` |
| `stash pop` | 128 | 同上に、stdout `The stash entry is kept in case you need it again.` |
| `rebase --continue` | 1 | stdout `<path>: needs merge` / `You must edit all merge conflicts and then` / `mark them as resolved using git add`、**stderr 空** |
| `cherry-pick --continue` / `revert --continue` / `commit` | 128 | stdout `U	<path>`、stderr `error: Committing is not possible because you have unmerged files.` + `hint:` + `fatal: Exiting because of an unresolved conflict.` |

- **`classify` は stdout から `[` で始まる行を先に除く。** 複数の cherry-pick / revert は
  積んだぶんの `[main <oid>] <件名>` を stdout に並べる（上のコンフリクトの印と同じ）ので、
  件名が `fix: needs merge` や `fix: nothing to commit` のコミットを積んだあとで merge に
  当たって止まる形（下の「複数のコミットの cherry-pick / revert」の `-m` 無し）を、語ごとに
  取り違える。**語ごとに守らず入口で 1 度だけ除く。** 除くのは括弧の中の最後の語が oid の
  行だけ —— **パスも `[` で始まりうる**（`[slug].txt` が未解消だと stdout は
  `[slug].txt: needs merge`。git 2.56.0 で実測、`core.quotePath` でも引用されない。
  `[` だけで除いた版は `/code-review` が踏んだ）。stderr の写し（`could not apply <oid>... <件名>`）は行の途中なので、
  行頭で見る印（`HeadMoved` / `Conflict`）だけが守られる
- **`NoChanges` の後ろ。** どちらの出力も相手の語を含まないので順は効かないが、
  語が広いほうを後ろに置く（`NonFastForward` と同じ数え方）
- **`switch` / `pull` / `rebase -i` の開始などは別の文言で断る**（`you need to resolve your
  current index first` など。どれも stderr）ので `Unmerged` にしない —— 案内を出す op を
  数えていない（01 の「失敗の案内文」）

**案内文は 01 の「失敗の案内文」が持つ。** ここに文言まで並べると、同じ kind でも
op で変わる分（非 FF は 4 通り）を表に書けないうえ、「次に何のキーを押すか」は
`git_exec` が答えられる問いではない（`moves_refs()` を `app` へ移したのと同じ数え方。
`.claude/rules/rust-coding.md` の「層をまたぐ述語」）。

**`SshAuthFailed` の案内にホストは入れていない。** `ssh -T <host>` を出すには
「どのリモートで失敗したか」が要るが、`GitOp::Pull` はリモート名を運ばず、
`RemoteUrl` は `user@` を捨てる（04）ので `git@` を書き足すと推測になる。
**M4 で `RepoInfo.target` が埋まったので、材料は届いている**（04 の「既定リモート」）。
ただし `target.host` は**既定リモートのホスト**で、失敗した `GitOp` が使ったリモートとは
限らない（`Push` は自分でリモート名を運ぶ）。**案内にホストを入れるなら、`GitOp` 側から
辿る**（#49 の「完了時に `state` から取る値は、発行時と同じである保証を数える」）。
**入れないと決めた**（2026-09-26、通知の履歴のタスク）。stderr は `Notice.detail` に
残って `L` の履歴で読め、**ssh は認証の失敗で自分からホストを名乗る** ——
`git@github.com: Permission denied (publickey).`（鍵を渡さずに `git ls-remote
git@github.com:octocat/Hello-World.git` を叩いて実測。git 2.55.0 / OpenSSH 10.3p1）。
失敗した `GitOp` から辿る配線を足さなくても、どのホストで断られたかは画面に出る。

分類するのは git の出力があるとき、つまり `ExecError::Failed` だけ。`NoResult` には
`kind` が無い（分類関数を通らない）。

### 理由の行（`ExecError::reason`）

案内の無い失敗の要約行（01 の「失敗の案内文」）に出す 1 行を、git の出力から選ぶ。
**置き場所は `git_exec`**（`runner::reason`、`classify` の隣）—— 「どの行が理由か」は
git の出力の形の問いで、写されたコミットの行（`echoes_a_commit`）を除くのも `classify` と
同じ。上から順に ——

1. **stderr が空なら stdout の最後の行。** git は理由を stdout にだけ書くことがある
   （何も stage していない `commit`、未解消のままの `rebase --continue`、複数の revert が
   空で止まった形）。stdout は status の一覧 → 結びの順に並ぶ
2. **push の ` ! [` の行**（`! [remote rejected] main -> main (pre-receive hook declined)`）
3. **締めではない最初の `error:` / `fatal:` の行**から接頭辞を剥いだもの

どれも選べなければ `None`。**git の形をしていない行は選ばない**（2026-10-03 にユーザーと
決めた）—— hook の文言、ssh の行（`ssh: Could not resolve hostname …`）、リモートが書く
文言（GitHub の `ERROR: Repository not found.`）。`app` は出力があれば固定の文言で `L` を
案内し、出力が空なら `Display` に落とす（01 の「失敗の案内文」）。

- **hook の出力は形で選べない。** lefthook 2.1.15 で pre-commit の 1 command を失敗させる
  と、出力は**1 行目が枠**（`╭──…╮`、色付き）・見出し・各 command の出力・**最後が所要
  時間**（`🥊 lint (0.00 seconds)`）で、理由（`src/a.rs:1:1: error: unused variable x`）は
  真ん中にある（実測。行頭は `src/…` なので 3 にも当たらない）。最初の行も最後の行も
  理由にならない。他の TUI も選んでいない（2026-10-03 に実測。lazygit 0.65.1 は commit
  なら固定の文言 `Git command failed. Check command log for details`、push なら stderr の
  全文、gitui 0.28.1 は全文のポップアップ、tig 2.6.1 は端末に生で流す）
- **「締めの手前の最後の行」も採らなかった。** ssh の行と stderr に書く hook は拾えるが、
  締めの行を**区切り**に使うので、git が文言を変えて照合が外れると手前の行を取り違える
  （ssh なら案内文の `and the repository exists.`）。いまの形では、締めの行は**選ばない
  ため**だけに照合するので、外れても締めの行（薄いが事実の行）が要約に出るだけ
- pre-commit の hook の出力は stdout に書いても git が stderr に合流させる。pre-push では
  合流させない（lefthook は自分の出力をすべて stdout に書く）

**締めの行**（`runner::WRAP_UPS`）は、git が理由のあとに足す「何が」を言わない行 ——
`failed to push some refs`（push。どの ref がなぜかは ` ! [` の行か hook の文言）、
`Could not read from remote repository.`（fetch / pull / push。何がは前の行）、
`cherry-pick failed` / `revert failed`（どのファイルかは前の `error:`）。ssh の失敗と
pre-push の hook では、`error:` / `fatal:` の行が締めの行だけになる（`/code-review` が
踏んだ）ので、飛ばさないとそれが要約に出る。

**stderr があれば stdout は見ない** —— 下の表の形では、stdout は進捗や status の一覧
だった（`pull` の `Updating …`、stash の apply の status）。例外の `needs merge`
（stderr は `could not write index` だけ）は `Unmerged` の案内が先に拾う。
**2 と 3 は trim する前の行頭で見る** —— `would be overwritten` の下のファイル一覧は
タブで字下げされるので、trim してから見ると `error: ` や `! [` で始まるパスを
取り違える。

**失敗の出力は端末の制御シーケンスを剥いで運ぶ**（`runner::strip_escapes`。`Failed` を
組むとき）。hook は端末でなくても色を付け（lefthook は truecolor の枠）、描く側の ratatui
（`ratatui-core` 0.1.2 の `Buffer::set_stringn`）は ESC の 1 文字しか落とさないので、
要約にも `L` の履歴にも `[38;2;56;56;56m` が残る。gitui 0.28.1 は剥がさずに描いて画面が
崩れた（実測）。成功の出力は剥がない（読む側が無い）。CSI / OSC のほかに `ESC ( B`
（`tput sgr0` が出す）も剥ぐ。**終わらない OSC は改行で打ち切る** —— 出力の残りを、
その後ろの git の `error:` 行ごと食べない（`/code-review` が踏んだ）。

**`Other` に落ちる失敗の stderr**（git 2.56.0、`LC_ALL=C`、2026-10-03 に一時リポジトリで
実測）。

| 操作 | exit | stderr（タブの字下げは `⇥`） |
|---|---|---|
| `switch`（ワークツリーの変更が上書きされる） | 1 | `error: Your local changes to the following files would be overwritten by checkout:` / `⇥f` / `Please commit …` / `Aborting` |
| `switch`（無い ref）/ merge の途中の `switch` | 128 | `fatal: invalid reference: nope` / `fatal: cannot switch branch while merging` + 案内 1 行 |
| `branch` / `switch --create` / `branch --move`（同名がある） | 128 | `fatal: a branch named 'side' already exists` |
| `branch`（不正な名前） | 128 | `fatal: 'a..b' is not a valid branch name` + `hint:` 2 行 |
| `branch --delete`（未マージ） | 1 | `error: the branch 'side' is not fully merged` + `hint:` 2 行（`-D` の案内と `advice.forceDeleteBranch`） |
| `branch --delete`（worktree が使っている） | 1 | `error: cannot delete branch 'main' used by worktree at '<path>'` |
| `add` / `rm`（無いパス） | 128 | `fatal: pathspec 'nope' did not match any files` |
| `restore`（無いパス） | 1 | `error: pathspec 'nope' did not match any file(s) known to git` |
| `apply --cached`（当たらないパッチ） | 1 | `error: patch failed: f:1` / `error: f: patch does not apply` |
| `fetch`（無いリモート / 無いパス） | 128 | `fatal: '<remote>' does not appear to be a git repository` / **`fatal: Could not read from remote repository.`** / 空行 / 案内 2 行 |
| `pull --ff-only`（上書きされる） | 1 | `error: Your local changes … would be overwritten by merge:` / `⇥f` / … / `Aborting`（stdout `Updating <a>..<b>`） |
| `push`（hook / 保護ブランチが断った） | 1 | `remote: <hook の文言>` / `To <url>` / **` ! [remote rejected] main -> main (pre-receive hook declined)`** / `error: failed to push some refs to '<url>'` |
| `cherry-pick` / `revert`（上書きされる） | 128 | `error: The following untracked working tree files would be overwritten by merge:`（tracked なら `Your local changes …`）/ `⇥g` / … / `Aborting` / **`fatal: cherry-pick failed`**（`revert failed`） |
| `stash apply`（上書きされる） | 128 | `error: Your local changes … would be overwritten by merge:` / … / `Aborting`（stdout に status の一覧） |
| `stash apply` / `drop`（無いエントリ） | 128 | `error: stash@{0} is not a valid reference` / `fatal: log for 'stash' only has 1 entries` |
| `rebase --interactive`（ワークツリーが汚れている） | 1 | `error: cannot rebase: You have unstaged changes.` / `error: Please commit or stash them.` |
| `rebase --abort` / `cherry-pick --abort` / `merge --abort`（何も無い） | 128 | `fatal: no rebase in progress` / `error: no cherry-pick or revert in progress` + `fatal: cherry-pick failed` / `fatal: There is no merge to abort (MERGE_HEAD missing).` |
| `commit`（空のメッセージ） | 1 | `Aborting commit due to empty commit message.`（接頭辞が無い） |
| `commit`（pre-commit / commit-msg の hook が断った） | 1 | **hook が書いた文言だけ**（git は何も足さない） |
| `push`（pre-push の hook が断った） | 1 | `error: failed to push some refs to '<url>'` だけ。**hook の文言は hook が書いた側**（stdout に書けば stdout、stderr に書けば締めの行の前） |
| `fetch`（初めて繋ぐホストで、リモートが無い。github.com） | 128 | `Warning: Permanently added 'github.com' (ED25519) to the list of known hosts.` / `ERROR: Repository not found.` / **`fatal: Could not read from remote repository.`** / 空行 / 案内 2 行 |
| `fetch`（ssh の名前解決 / 接続拒否） | 128 | `ssh: Could not resolve hostname …`（`ssh: connect to host … Connection refused`）/ **`fatal: Could not read from remote repository.`** / 空行 / 案内 2 行 |

- **太字が締めの行。** cherry-pick / revert と fetch は理由のあとに情報の薄い `fatal:` を
  足し、push の `error:` はどの ref がなぜ断られたかを言わない
- **素の hook の行は 1 行の形で測った**（commit-msg の hook は `hook.<名前>.command` の
  後ろにメッセージファイルのパスが足されるので、`exit 1` だけのコマンドは
  `exit: too many arguments` を 2 行目に書いた。hook の書き方の都合で、git の文言ではない）
- 非 FF の push（`(fetch first)` / `(non-fast-forward)`）と `pull --ff-only` の
  `Not possible to fast-forward` は `NonFastForward` に分類されて案内が付くので、ここの
  対象ではない

## Interactive rebase の実装

`git rebase -i` は todo ファイルをエディタで開く。これを自バイナリで置き換える。

```rust
pub struct RebasePlan { pub entries: Vec<RebaseEntry> }
pub struct RebaseEntry { pub action: RebaseAction, pub oid: Oid, pub summary: String }
pub enum RebaseAction { Pick, Reword { message: String }, Edit, Squash, Fixup, Drop }
```

1. UI で `RebasePlan` を作る（07 の「RebaseEditor」）
2. `to_commands` が `plan.validate()` を通し、plan と `head` を JSON にして stdin に載せる。通らない
   plan は `ArgvError` で、git を起こさない
3. `Runner` はいつもどおり `git rebase --interactive --no-update-refs <onto>` を起こす。
   **`Runner` は変種を見ない** —— `GIT_SEQUENCE_EDITOR='<自バイナリ>' --sequence-editor` と
   `GIT_EDITOR='<自バイナリ>' --commit-editor` はどのコマンドにも載っている
4. git は todo のパスを末尾に足してシェル経由で sequence editor を起こし、**自分の stdin を
   そのまま継がせる**（下の表）。`--sequence-editor` は stdin から plan と plan を組んだ
   HEAD を読み、**HEAD が git の `rebase-merge/orig-head` と同じことを確かめてから**todo を
   書き、reword のメッセージを todo と同じ `rebase-merge/` に `mewgit-reword-<Oid>` として置く
5. git は reword の行ごとに `<gitdir>/COMMIT_EDITMSG` を渡して `GIT_EDITOR` を起こす。
   `--commit-editor` は `<gitdir>/rebase-merge/done` の最終行（`reword <Oid> …`）から
   Oid を引き、置いてあるメッセージを書き込む
6. 停止したら、書き込みのあとの `LoadStatus` が運ぶ進行中操作（`WorktreeSnapshot.progress`）がバッジに出て、`m` のメニューから continue / skip / abort を打つ（01 の「進行中操作のメニュー（`m`）」。専用のモードは持たない）

決めたことと理由:

- **2 つのエディタはどのコマンドにも載せる。** `RebaseInteractive` のときだけ重ねると、
  **止まった rebase を `m` の continue で再開したときに `GIT_EDITOR` が届かず**、止まった
  reword も停止点より後ろの reword も元のメッセージのまま通る（continue は別の `GitOp`）。
  - `--commit-editor` は reword 以外で起こされたら何もしない（squash、revert / cherry-pick
    の continue、端末で始めた rebase の reword）。以前の `GIT_EDITOR=true` と同じ振る舞い
  - `--sequence-editor` を起こすのは `RebaseInteractive` だけで（`pull` はモードを argv で
    必ず渡すので、`pull.rebase=interactive` でも起きない。下の表）、ほかで起きたら stdin に
    plan が無いので非 0 で断る。**載せないと既定の todo を黙って採る**ので、閉じた側に
    倒れるほうを取った
  - `RebaseInteractive` のときだけ `Runner` が重ねる形（最初の実装）は、`to_commands` と
    `Runner` の 2 箇所が「git に何を渡すか」を決めることになり、一時ファイルとその寿命の
    管理も `Runner` に要った（`/simplify` の指摘で改めた）
- **reword は git 自身の `reword` のまま書く。`pick` + `exec git commit --amend -F` の
  2 行にしない。** 一度その形で実装したが、`exec` は直前の pick がどうなったかを見ずに
  HEAD を amend するので、**止まった reword の pick を skip すると別のコミットの
  メッセージを書き換える**（1 本前の pick、先頭なら `onto` の先端。下の表）。todo に
  `git` と直書きするので `general.git_bin` も効かない（どちらも `/code-review` が拾った）
- **`core.commentString` を `#mewgit#` にする。** reword のメッセージは `--commit-editor`
  が書くので、git はエディタの出力として cleanup にかけ、コメント文字で始まる行を消す
  —— `#123 is closed` の行が黙って消え、`#42` だけのメッセージは空として断られ rebase が
  止まる（`/code-review` が拾った。下の表）。`--cleanup` は reword に効かない。git 自身が
  書くコメント（squash の `# This is a combination…`）も同じ印で書かれて消えるので、
  揃ったまま `#` だけが本文に残る。**continue にも要る**のでエディタと同じくどのコマンドにも
  載せ、argv の `-c` ではなく `GIT_CONFIG_COUNT` / `KEY_n` / `VALUE_n` で渡す（`L` の履歴と
  argv の表に毎回載せない）。ユーザーが `GIT_CONFIG_COUNT` を使っていれば後ろに足す。
  **`core.commentString` を読むのは git 2.45 以降**（リリースノートより。手元は 2.55 だけで
  測った）で、それより古い git は未知のキーを黙って無視するので `#` の行が消える形に
  戻るはず。**確かめていない**（mewgit はまだ git の最低版を決めていない。M6）
- **消えた自バイナリ。** エディタが自バイナリなので、起動中に mewgit のバイナリが消えると、
  エディタを起こす git が落ちる（以前の `true` では起きない。**Linux では `/proc/<pid>/exe`
  を渡すので起きない** —— 下の 2 つ目）。**落ちるのは rebase の系統と
  `commit --amend` だけ**で、revert / cherry-pick は開始も continue もエディタを起こさない ——
  `Runner` は stdin を `Stdio::null()` か pipe にするので、git の `--edit` の既定（端末から
  起こされたとき）が外れる（下の表）。**落ちても壊れるものは無い** —— 開始は始まらず、
  continue は止まったまま stage した解消が残る。reword の行で止まった形は #145 の「断られた
  reword」と同じ形で、`mewgit-reword-<Oid>` も残るので、起動し直せば `m` の continue が
  新しいメッセージを入れる（下の表）。**案内は `EditorGone`**（上の「エラー分類」、文は 01 の
  「失敗の案内文」）
  - **`true` 相当に倒さない。** パスが無いときに `true` を渡すと、sequence editor は既定の
    todo を黙って採り（plan が効かない）、commit editor は reword のメッセージを黙って落とす
    （#145 で塞いだ形そのもの）。閉じた側に倒れるいまの形を採る
  - **Linux では `/proc/<pid>/exe` を渡して、消える形も版ずれも起こさない**（`/simplify` の
    altitude が出し、2026-10-03 にユーザーと決めた）。`current_exe()` は `/proc/self/exe` を
    読んで **symlink を解いた実体**のパスを返すので、Linuxbrew の keg のように実体の
    ディレクトリごと消えるとパスも消え、apt や `cargo install` のように同じパスへ新しい版を
    rename で置くと**新しい版のエディタが古い TUI の plan を読む**（版ずれ）。
    `/proc/<pid>/exe` は走っているプロセスの実体を指すので、**どちらでも起動した版そのものが
    起きる**（下の表）。`self` ではなく pid で書くのは、エディタを起こすのが git の子の `sh`
    だから。起動時に読めなければ（`/proc` の無い環境）か、`/proc/self/exe` と別の実体を指せば（PID
    名前空間で `/proc` をマウントし直していない形。`/code-review` が拾った）`current_exe()` に
    戻す（`mewgit-tui` の
    `runtime::editor_bin`）。**stat もこのリンクを辿るので、実体が消えても `EditorGone` には
    ならない** —— エディタは起きるので、それで正しい
  - **macOS には同じ仕組みが無い。** `current_exe()` は `_NSGetExecutablePath` で、**起動に
    使ったパスを symlink のまま**返す（相対パスで起こしても絶対パス）。Homebrew の
    `/opt/homebrew/bin/mewgit` は upgrade のあとも残って新しい keg を指すので、消えるのは
    手で消した形くらいで、そこを `EditorGone` の案内が拾う
  - **macOS の版ずれ（Homebrew の upgrade、`cargo install`）は塞いでいない。** 新しい版の
    `--sequence-editor` / `--commit-editor` が古い TUI の組んだ stdin の JSON と
    `mewgit-reword-<Oid>` を読む。受け渡しの形を変えない限り起きない。変えたときは
    sequence editor は JSON を読めずに非 0 で断る側に倒れるが、`mewgit-reword-<Oid>` の
    中身の形を変えると commit editor が黙って読み違えうる（2026-10-03 にユーザーと決めて
    タスクにしていない）
- **plan は一時ファイルではなく stdin で渡す。** git が editor に stdin を継がせる（下の表）
  ので、ハンクの patch と同じ経路に乗る。`tempfile` を本依存に上げずに済み、後始末も無い
- **メッセージのファイルは `rebase-merge/` の中に置き、Oid で引く。** 完了でも abort でも
  git がディレクトリごと消すので、mewgit が後始末を持たない。commit editor が知って
  いるのは渡された `COMMIT_EDITMSG` のパスと `done` の最終行だけなので、置き場は
  その隣（linked worktree でも `COMMIT_EDITMSG` と `rebase-merge/` は同じ gitdir に並ぶ。
  下の表）、鍵は Oid になる。**`GIT_DIR` は editor に渡らない**（下の表）
- **hook が reword を断って止まった rebase は、continue の前に新しいメッセージを amend する。**
  git は pick を元のメッセージでコミットしてから reword のエディタを起こすので、commit-msg
  hook が断ると**元のメッセージのコミットを残して止まり**、continue はエディタを起こし直さずに
  そのまま進む（下の表）。git 自身の挙動で、reword の渡し方を変えても塞がらない。そこで
  `status()` がこの形を見分けて `InProgressOp::Rebase.refused_reword` に載せ（02 の「断られた
  reword」）、`m` の continue が `GitOp::RebaseContinue { reword: Some(..) }` を出す。手順は 3 本 ——
  1. `update-ref --stdin` に `verify HEAD <見分けた時点の HEAD>` を流す。書き込みも reflog も
     残さずに HEAD を照合し、外れれば exit 128 で止まる（下の表）。**snapshot が古いと別の
     コミットを amend する** —— 端末で continue して次の停止に移っていても
     `mewgit-reword-<Oid>` は残るので、照合が無いと止まったコミットのメッセージで別の
     コミットを書き換える。断りは `HeadMoved` に分ける（上の「エラー分類」）
  2. `commit --amend`（メッセージを渡さない）。git がエディタを起こし、`--commit-editor` が
     `done` の最終行から `mewgit-reword-<X>` を差し込む —— **rebase の開始と同じ経路**なので、
     cleanup も `core.commentString` も同じに効き、行頭の `#` が残る（統合テストで確認）。
     メッセージを `--file -` で流し直す形（最初の版）は、同じメッセージを開始とやり直しで
     別の cleanup に通していた（`/simplify` の altitude が拾った）。**hook がまた断れば
     ここで止まり、rebase は止まったまま** —— 同じ形なので、hook を直して `m` から押し直せる。
     案内は足さない（git は hook の出力しか書かないので、分ける手掛かりが無い。通知の
     `detail` に hook の stderr が出る）
  3. `rebase --continue`
  - **照合を `Runner` に持たせなかった**（ハンクの指紋の形。上の「古いハンクの守り」）。
    `Runner` が `git_read` を呼ぶ例外を 00 に 2 つ目として足すことになり、`to_commands` と
    `Runner` の 2 箇所が「git に何を渡すか」を決める形にも戻る。`update-ref` の `verify` なら
    照合も git に渡す argv の一部で、`L` の履歴にも載る
  - **端末で別のメッセージに amend し直したら、素の continue に戻る。** 見分けは HEAD の
    メッセージが元のメッセージと同じことを条件にしている（02）ので、amend した時点で
    `refused_reword` は `None` になる
  - **この形の skip は止まったコミットを捨てない。** pick はもうコミット済みなので、
    元のメッセージのまま残る（下の表）。メニューの説明はこの形のときだけ変える（01 の
    「進行中操作のメニュー（`m`）」）
- **squash のメッセージは git が連結したものを使う。** `--commit-editor` は squash では
  何もしないので、squash した 2 本のメッセージを空行でつないだものがそのまま入る
  （下の表）。書き換えたいなら、あとから reword する。**`RebaseEntry` はメッセージを
  持たない** —— 持つのは `Reword` だけで、値の無い reword は型として作れない
  （`Option` にすると `Pick` に値がある形も作れてしまう）
- **sequence editor は、plan を組んだ HEAD が `rebase-merge/orig-head` と違えば断る。**
  plan は UI が読み込み済みのグラフから組む（01 の「interactive rebase の開始（`i`）」）
  ので、端末で積んだコミットがあると HEAD が古く、**plan に載らなかったコミットを git は
  黙って落とす**（`rebase.missingCommitsCheck` の既定は `ignore`）。`GitOp::RebaseInteractive`
  が plan を組んだ HEAD（`head`）を運び、stdin の JSON に plan と並べて載せる。`onto` は
  Oid で固定され、範囲の merge は UI が開く前に断るので、**HEAD が同じなら範囲も同じ**。
  断れば git は rebase を始めない（下の表の「sequence editor が非 0」）。**断りは
  `ExecErrorKind::HeadMoved` に分ける**（上の「エラー分類」。案内は 01 の「失敗の案内文」）。
  **見るのは開始だけでよい** —— continue / skip は sequence editor を起こさない（下の表）
  - **最初は git の既定の todo と plan のコミットを集まりとして突き合わせていた**
    （`/simplify` の altitude で改めた）。既定の todo は `core.abbrev` の短縮 Oid で、
    `rebase.abbreviateCommands` / `autoSquash` / `rebaseMerges` で行の形も変わる
    （下の表）ので、構文を追い続けることになる。`orig-head` は完全な Oid の 1 行
  - **`-c rebase.missingCommitsCheck=error` を argv に載せる形も採らなかった** —— 落ちる
    のは todo を書いたあとで、rebase が始まったまま止まり（`rebase-merge/` が残る）、
    `m` から abort させることになる。`GitOp::verb()` が argv の先頭と揃う約束も崩れる
- **`--no-update-refs` を必ず付ける。** `rebase.updateRefs=true` だと git は既定の todo に
  `update-ref refs/heads/<b>` の行を入れる（実測）。mewgit は todo を丸ごと書き換える
  ので、付けないとユーザーの設定が黙って効かなくなる。効かないことを argv に書いて
  おくほうが、`L` の履歴から読める
- **todo は完全な Oid で書く。** 既定の todo は短縮形だが、git は完全形も受ける。
  短縮形は mewgit が組んだ時点と git が読む時点で一意性が変わりうる
- **`validate` が断るもの**: 空の plan、最初の `drop` 以外の行が `squash` / `fixup`
  （git が `cannot 'squash' without a previous commit` で断る）、空白だけの reword の
  メッセージ（git が空のメッセージとしてコミットを断り、rebase が途中で止まる）。
  **全部 `drop` は断らない** —— git は通して、ブランチが `onto` まで戻る。確認を
  挟むのは UI（07）

実測（git 2.55.0、`LC_ALL=C` の一時リポジトリ、2026-09-30。`GIT_EDITOR` は断りが無ければ
`true`）:

| 形 | 結果 |
|---|---|
| `GIT_SEQUENCE_EDITOR="sh 'dump.sh' extra"`、`-c sequence.editor=false` | 環境変数が勝つ。argv は `extra <gitdir>/rebase-merge/git-rebase-todo`（**シェル経由で、todo のパスが末尾に付く**） |
| `printf 'drop A\npick B\n' \| GIT_SEQUENCE_EDITOR="sh -c 'cat > \"\$1\"' -" git rebase -i HEAD~2` | exit 0、A が落ちる（**git の stdin が editor に継がれる**） |
| `GIT_SEQUENCE_EDITOR=<呼ばれたら印を残す>`、`-c pull.rebase=interactive` で `pull --rebase` / `--no-rebase` / `--ff-only` / 素の `pull` | 起こされるのは**素の `pull` だけ**（mewgit はモードを必ず argv で渡す） |
| sequence editor の環境 | `GIT_EXEC_PATH` / `GIT_PREFIX` / `GIT_SEQUENCE_EDITOR` と呼び出し側の `GIT_*` だけ。**`GIT_DIR` は無い** |
| sequence editor が非 0 | exit 1、stderr `error: there was a problem with the editor '<editor>'`。HEAD は動かず `rebase-merge/` も残らない |
| `edit` で止めたあとの `rebase --continue` / `--skip`（sequence editor は呼ばれたら印を残す。git 2.56.0、2026-10-01） | どちらも**起こされない**（印は開始の 1 回だけ）。止まった理由は `git status` の `interactive rebase in progress` で確認 |
| todo が空 | exit 1、stderr `error: nothing to do` |
| 既定の todo の形（`fixup! C2` / `amend! C2` を含む 4 本を `rebase -i HEAD~4`、sequence editor は写して exit 1） | `pick <7 桁> # C2` の並び。`-c rebase.abbreviateCommands=true` で `p`、`rebase.autoSquash=true` で `fixup <Oid>` と `fixup -C <Oid>` に変わり、`rebase.instructionFormat=%an:%s` は `#` の後ろだけ、`rebase.rebaseMerges=true` は先頭に `label onto` / `reset onto`、`core.abbrev=12` で 12 桁。**どれも HEAD は動かず `rebase-merge/` も残らない** |
| sequence editor が起きた時点の `rebase-merge/`（ブランチ上 / detached HEAD / linked worktree の 3 形） | `git-rebase-todo`、`.backup`、`head-name`、`interactive`、`onto`、**`orig-head`**（rebase 前の HEAD の完全な Oid 1 行）などが**書き込み済み**。3 形とも同じ |
| `-c rebase.missingCommitsCheck=error` で、範囲の 1 本を載せない todo | exit 1、`Dropped commits` に落ちた件名。**`rebase-merge/` が残り、rebase は途中のまま**（HEAD は `onto`）。採らなかった形 |
| todo が全部 `drop` | exit 0、`Successfully rebased`。ブランチは `onto` まで戻る |
| `reword A <件名>` を `GIT_EDITOR` に印を残すスクリプトを置いて流す（linked worktree） | `<gitdir>/worktrees/<名前>/COMMIT_EDITMSG` を渡して起こされ、**`rebase-merge/` はその隣**。`done` の最終行は `reword <完全な Oid> <件名>` |
| `reword A` / `pick B`、A がコンフリクト → 解消して continue（`GIT_EDITOR` は印を読んで書き込むスクリプト） | continue で A のエディタが起こされ、新しいメッセージが入る |
| `pick A` / `reword B`、A がコンフリクト → 解消して continue（同上） | B に新しいメッセージが入る。**continue を `GIT_EDITOR=true` で打つと元のメッセージのまま** |
| `reword A` / `pick B`、A がコンフリクト → `rebase --skip` | A が落ち、`onto` の先端はそのまま |
| `pick A` / `exec git commit --amend -m NEW` / `pick B`、A がコンフリクト → `rebase --skip` | **exec が `onto` の先端を amend する**（先端の件名が NEW に化ける）。採らなかった形 |
| commit-msg hook が reword を断る（git 2.56.0、2026-10-03。hook は `hook.<名前>.command` で置いた。以下 7 行も同じ） | exit 1、stderr に hook の出力と `You can amend the commit now`。pick は**元のメッセージでコミット済み**で、`done` の最終行は `reword <X>`、`git status` は `editing a commit`、gitdir に `MERGE_MSG` は無い。**`rebase-merge/amend` は pick が fast-forward した形（HEAD = X、reflog `rebase: fast-forward`）にしか無い** —— `--onto` で載せ直す形では HEAD が X と別の新しいコミット（reflog `rebase (reword): <件名>`）で、`amend` が無い |
| ↑ の hook を外して continue | exit 0 だが**エディタは起こされず**、元のメッセージのまま進む |
| ↑ で `commit --amend -F <mewgit-reword-<X>>` → continue（`core.commentString` の有無の両方） | 新しいメッセージが入り、`#123 kept` の行も残る。エディタは開始の 1 回しか起こされない |
| ↑ の amend を hook がまた断る | exit 1、stderr は hook の出力だけ。`rebase-merge/` はそのまま |
| ↑ で `rebase --skip`（載せ直す形） | exit 0、**X は元のメッセージのまま残る**（捨てられない） |
| reword の pick がコンフリクト → 解消して continue（hook は新しいメッセージを断るもの） | exit 0、**hook は走らず**新しいメッセージが入る。止まっているあいだは `MERGE_MSG` が在り（`git add` したあとも）、`amend` は無い。止まったまま `git reset --hard` すると `MERGE_MSG` が消え、HEAD は 1 本前のコミットのまま |
| X を cherry-pick して中身だけ amend したコミット（メッセージも author の日時も X と同じ）を onto に積み、X を reword して載せ直す | X の pick がコンフリクトして止まり、HEAD はその双子。HEAD の reflog の先頭は `rebase (start): checkout <onto>`。解消して `git add` → `reset --hard` しても先頭は変わらない（`/code-review` が再現した形。2026-10-03） |
| `printf 'verify HEAD <oid>\n' \| git update-ref --stdin` | 一致なら exit 0 で、reflog も増えない。外れると exit 128、stderr `fatal: cannot lock ref 'HEAD': is at <いまの Oid> but expected <oid>` |
| `rebase-merge/` に自分のファイルを置いたまま完了 / `rebase --abort` | どちらも `rebase-merge/` ごと消える |
| `pick A` / `squash B`（`GIT_EDITOR=true`） | メッセージは `A の件名` + 空行 + `B の件名` |
| reword のメッセージ `Fix bug` + 空行 + `#123 is closed`（`--commit-editor`） | `#123` の行が消える。`-c core.commentString=#mewgit#` を付けると残り、同じ rebase の squash のメッセージにもコメントの行は残らない |
| `GIT_CONFIG_COUNT=2`（0 番はユーザーの設定、1 番が `core.commentString`）、reword 2 本の 1 本目がコンフリクト → 解消して continue | 2 本とも `#` の行が残る（`#42 only` だけのメッセージも通る）。0 番の設定も効いたまま |

消えた自バイナリの実測（2026-10-03。macOS の git 2.56.0 と、Debian trixie のコンテナの
git 2.47.3 で同じ結果。`LC_ALL=C`、stdin は `/dev/null`。エディタは「呼ばれたら印を残す」
スクリプトと、存在しないパス `'/nonexistent dir/mewgit' --commit-editor` の 2 通りで流した）:

| 操作 | エディタ | 存在しないパスのとき |
|---|---|---|
| `revert <X>` / コンフリクトを解いて `revert --continue` / 2 本の revert の continue | 起こされない | 影響なし |
| `cherry-pick <X>` / コンフリクトを解いて `cherry-pick --continue` | 起こされない | 影響なし |
| `pull --no-rebase`（マージコミットができる形） | 起こされない | 影響なし |
| `rebase --interactive` の開始 | sequence editor | exit 1、stderr `<shell の行>` + `error: there was a problem with the editor '…'`。HEAD は動かず `rebase-merge/` も残らない |
| pick のコンフリクトを解いて `rebase --continue` | commit editor | exit 1、`there was a problem with the editor` + `could not commit staged changes`。**rebase は止まったまま**で、stage した解消も残る |
| reword の行 | commit editor | exit 1、`there was a problem with the editor` + `You can amend the commit now`。pick は元のメッセージでコミット済み |
| `commit --amend`（メッセージを渡さない） | commit editor | exit 1、`there was a problem with the editor` + `Please supply the message using either -m or -F option.` |
| ↑ reword の行で止まった形を、mewgit の実バイナリで（plan は `--sequence-editor` に stdin で渡した）。早送りの形と `--onto` で載せ直す形 | — | 早送りは HEAD = X・`amend` 在り・reflog `rebase: fast-forward`、載せ直しは reflog `rebase (reword): A`・`amend` 無し。**どちらも `mewgit-reword-<X>` が残る**。エディタを戻して verify → `commit --amend` → continue で**新しいメッセージが入る** |
| shell の行 | — | macOS: `'…' --commit-editor: /nonexistent dir/mewgit: No such file or directory`、dash: `'…' --commit-editor: 1: /nonexistent dir/mewgit: not found` |
| `current_exe()`（rust 1.98.1。symlink `bin/exe` → `real/exe` で起こす） | — | macOS は `…/bin/exe`（相対パスで起こしても絶対パス）、Linux は `/opt/real/exe`。起動後に実体を消すと、Linux で読み直した値は `/opt/real/exe (deleted)`、macOS は消える前と同じ |
| Linux で、起動したプロセスが `sh -c "'/proc/<自分の pid>/exe' child"` を孫として起こす（Debian trixie のコンテナ、rust 1.98.1） | — | 何もしない / 起動後に実体を消す / 同じパスへ新しい版を rename で置く、の 3 形とも**起動した版が exit 0 で走る**。同じ 3 形を `current_exe()` のパスで起こすと、消した形は exit 127 `not found`、置いた形は**新しい版**が走る |

## 進行中操作との整合

- **`ExecFinished` は進行中操作を運ばない。** `exec_finished` が成功・失敗・中断を
  問わず出す `LoadStatus` の返り（`WorktreeSnapshot.progress`。02）で入れ替わる
  （`GitOp::SetAccount` / `UnsetAccount`（`.git/config` の 1 行だけを書く）は出さない —— 進行中操作は動かない）。
  **以前は「実行結果と 1 通に束ねる」と決めていた**（別々の `Msg` に割ると受け取る順序に
  依存するため）が、束ねると起動時・書き込みのあと・外部の変更の検知（01）で入口が 3 本に
  なり、それぞれに到着順の議論が要った。status に載せれば、古い返りを捨てるのは既存の
  世代の 1 本で済む（01 の「起動時の読み込み」）。**代わりにバッジは status が届くまで
  1 往復遅れる**が、そのあいだは `conflicted` の一覧も同じだけ遅れており、世代の門が
  その窓を「まだ信じない」と読むので食い違いにはならない（2026-09-26、`/simplify` の
  指摘を受けてユーザーと決めた）
- `Runner` は gitdir を知らない。中断した git の後始末（`CHERRY_PICK_HEAD` の削除など）は
  `run_cancelable` が終了を待ってから返るので、`LoadStatus` はそのあとを読む
- 進行中操作があるときは、**HEAD か作業ツリー全体を動かす書き込み**を `app` が
  git を起動する前に断る（下の「進行中に通す書き込み」。判定の置き場所は 01 の
  「進行中操作があるときの書き込み」）

### 進行中に通す書き込み

**以前は「continue / skip / abort 以外の書き込み `GitOp` を拒否する（UI 側でも無効化）」と
書いていたが、そのとおりに作るとコンフリクトから抜けられない。** 解消には stage
（`git add`）が要り、merge を結ぶのは `commit` で（merge の continue に当たる `GitOp` は
無い）、rebase の `edit` で止まったところで打つのは `commit --amend` だから。

**実測（git 2.55.0、2026-09-27。`stash apply` / `pop` の行は 2026-09-30）。** 状態 × 操作の組ごとに一時リポジトリを作り直して叩いた
（1 つのリポジトリで続けて叩くと、前の操作が状態を変える —— `stash push` が
`CHERRY_PICK_HEAD` を消したあとの `commit` は「cherry-pick の途中の commit」ではない）。
「解消済み」はコンフリクトしたファイルを `git add` したあと。

| 操作 | コンフリクト中（3 種とも） | 解消済みの rebase | 解消済みの cherry-pick / merge |
|---|---|---|---|
| `switch` | 断る（`cannot switch branch while rebasing` など） | 断る | 断る |
| `pull --ff-only` | 断る（unmerged files） | 断る（detached HEAD） | 断る（`You have not concluded your …`） |
| `cherry-pick` / `revert` | 断る（unmerged files） | 断る（local changes would be overwritten） | 断る |
| `rebase` の開始 | 断る | 断る（`already a rebase-merge directory`） | 断る（index contains uncommitted changes） |
| `stash push` | 断る（`could not write index`） | **通る**（解消した内容を stash が持っていく。`rebase-merge/` は残る） | **通り、`CHERRY_PICK_HEAD` / `MERGE_HEAD` が消える** |
| `stash apply` / `pop` | 断る（`could not write index`） | 通る（stash の変更が unstaged で混ざる。`rebase-merge/` は残る） | 通る（同左。**`*_HEAD` は残る**） |
| `commit` | 断る（unmerged files） | 通る（detached HEAD に積む） | 通り、**操作を結ぶ**（`*_HEAD` が消える） |
| `commit --amend` | 断る | 通る（`edit` の使い道そのもの） | 断る（`You are in the middle of a cherry-pick -- cannot amend`） |
| `fetch` / `push` | 通る | 通る | 通る |
| `branch -m` / `branch -D` | 通る（**rebase 中の対象ブランチは両方とも断る** —— `-m` は `is being rebased`、`-D` は `used by worktree`） | 同左 | 通る |

ここから決めたこと ——

- **断るのは HEAD か作業ツリー全体を動かす書き込み**（switch、pull、PR の取り出し、
  cherry-pick / revert / rebase の開始、stash の push / apply / pop など）。**git が自分で
  断るものも断る** —— 押すたびに赤い通知と argv を出すより、何の途中かと出口を先に言う
  ほうが読める。**`stash push` だけは git が受けて進行中操作を壊す**ので、これは案内の
  都合ではなく守り（古い snapshot のあいだも断る。01 の「stash の書き込み」）。apply / pop は
  解消済みなら git が受けて `*_HEAD` を残すが、**解消の途中に無関係な変更を混ぜる**ので断る
- **通すのは解消に要るもの**（stage / unstage / ハンク / discard、`Commit`。amend も ——
  cherry-pick / merge の途中の amend は git が断る）**と、実測で無害なもの**（fetch、push、
  ブランチの改名・削除 —— **ただし rebase 中の対象ブランチは断る**。git も断るが、`r` は
  打った名前が消えるので入力を開く前に断りたい。対象ブランチは `rebase-*/head-name` から
  読む（02 の「進行中操作の検出」））。**変種ごとの振り分けは `app::update::progress::refusal` の
  網羅 `match` が正**で、ここには写さない
- **continue / skip / abort は、進行中の操作と種類が合うときだけ通す**（cherry-pick の途中の
  `RebaseContinue` は断る）。**merge は `MergeAbort` だけ** —— merge の continue に当たる
  `GitOp` は無く、結ぶのは `Commit`。switch / pull を断るので、`MergeAbort` が無いと
  merge の途中から mewgit で抜ける手が commit しか無い。`rebase -r` の merge で止まった形は
  rebase として検出する（02）ので、`MergeAbort` は断り、rebase の abort を案内する。**am には通すものが無い** —— mewgit から am は始めず、
  `GitOp` にも無い。端末で `git am --continue` / `--abort` を打つよう案内する
- **UI 側でキーを無効化しない**（フッターのキーヒントも消さない）。押せば断りの理由が
  通知に出るので、消えたキーを探させるより読める。ヒントを状態で出し分けると、
  `app::keys` の表が 1 本でなくなる（07 の「ステータスバー」）

### continue / skip / abort の終わり方

**結果は 3 通り**（`.claude/rules/rust-coding.md` の「二値だった結果に 3 つ目を足したら」）。
当時の `Runner` と同じ `GIT_EDITOR=true LC_ALL=C` で、一時リポジトリを作り直して叩いた
（git 2.55.0、2026-09-27）。**エディタで止まることは無い**（解消したコミットは元の
メッセージのまま積まれる）。いまの `GIT_EDITOR`（`--commit-editor`）も mewgit が始めた
rebase の reword 以外では何もしないので、同じ結果になる（下の「Interactive rebase の
実装」）。

| 操作 | exit | stdout / stderr | 分類 |
|---|---|---|---|
| 解消して `rebase --continue` → 最後まで | 0 | stderr `Successfully rebased and updated refs/heads/<b>.` | 成功 |
| `edit` で止まったあと `rebase --continue` | 0 | 同上 | 成功 |
| 解消して `rebase --continue` / `rebase --skip` → **次のコミットでまたコンフリクト** | 1 | stdout `CONFLICT (content)`、stderr `could not apply` | `Conflict` |
| 解消して `cherry-pick --continue` → 次の pick でコンフリクト | 1 | 同上 | `Conflict` |
| 解消せずに `rebase --continue` | 1 | stdout `a: needs merge` / `You must edit all merge conflicts` | `Unmerged` |
| 解消せずに `cherry-pick --continue` | 128 | stderr `Committing is not possible because you have unmerged files.` | `Unmerged` |
| `rebase --abort` / `merge --abort` | 0 | 何も出さない | 成功 |
| 変更が空で止まった pick のあと `cherry-pick --continue` | 1 | stderr `The previous cherry-pick is now empty`（止まったときと同じ文） | `Empty` |
| 変更が空で止まった pick のあと `cherry-pick --skip` / `--abort` | 0 | 何も出さない | 成功 |

**空で止まった pick は continue では抜けられない**（上の表の 2 行。2026-09-29 に測った）。
`GIT_EDITOR=true` でも同じで、git は `commit --allow-empty` か `--skip` を求める。
**出口は skip か abort** なので、`m` のメニューの cherry-pick に skip を入れた（01 の
「進行中操作のメニュー（`m`）」）。**revert にも入れた** —— 解消した結果が空だと continue が
同じように断られる（下の「revert の開始」の表。stderr は空で、`Empty` ではなく
`NoChanges` に落ちる）。

**`Conflict` は失敗ではなく「次で止まった」** —— 進行中操作は残り、バッジの数が進む。
そこで赤を出すと壊れたように読めるので、`app` は `Warn` で次の手を案内する（01 の
「失敗の案内文」）。**解消していないまま continue した形も `Warn`**（`Unmerged`。上の
「エラー分類」）—— 当初は「git の文言（`needs merge`）がそのまま理由になる」として赤の
`Other` に残したが、rebase はその文言を **stdout にだけ**書くので、通知には argv しか
出ていなかった（2026-10-03）。

### cherry-pick の開始

**`GitOp::CherryPick` は印が無ければカーソルの 1 本で使う**（01 の「cherry-pick の開始（`y`）」。
複数は下の「複数のコミットの cherry-pick / revert」）。一時リポジトリを作り直して叩いた（git 2.55.0、
`GIT_EDITOR=true LC_ALL=C`、2026-09-29）。

| 選んだコミット・状態 | exit | stdout / stderr | 残るもの | 分類 |
|---|---|---|---|---|
| 別のブランチのコミット | 0 | stdout `[main c708635] t1` | 無し（HEAD が進む） | 成功 |
| 同上、detached HEAD | 0 | stdout `[detached HEAD d4b7235] t1` | 無し | 成功 |
| 同上、**別のファイル**が dirty | 0 | 同上 | dirty のまま | 成功 |
| 同上、**同じファイル**が dirty | 128 | stderr `Your local changes to the following files would be overwritten by merge` | 無し | `Other` |
| HEAD 自身・HEAD の祖先・merge で取り込み済みのコミット（後の履歴が同じ箇所を変えていない） | 1 | stderr `The previous cherry-pick is now empty` | **`CHERRY_PICK_HEAD`** | `Empty` |
| HEAD の祖先で、**後のコミットが同じ箇所を変えている** | 1 | stdout `CONFLICT (content)`、stderr `could not apply` | `CHERRY_PICK_HEAD`、`UU` | `Conflict` |
| コンフリクトするコミット | 1 | stdout `CONFLICT (content)`、stderr `could not apply` | `CHERRY_PICK_HEAD`、`UU` | `Conflict` |
| merge コミット | 128 | stderr `is a merge but no -m option was given` | 無し | `Other` |

- **どの失敗でも HEAD は動かない**（1 本だけのとき）。書き込みのあとの読み直し
  （01 の「ログの読み直し」）は、成功したときだけログまで読み直す。複数では途中まで
  積んでから止まるので、成否を見ずに読み直す
- **空で止まった形は `Empty` に分ける。** `Other` のままだと赤い `Display` に落ち、
  stdout の `all conflicts fixed: run "git cherry-pick --continue"` に従うと同じ理由で
  断られる。**`CONFLICT` は大文字で見る** —— この形の stdout にも小文字の `conflicts` が
  出る
- **`--empty=drop` は付けない。** 空の pick を落として exit 0 で返る（stderr に
  `dropping … patch contents already upstream`）が、**成功は通知に出さないので、押しても
  何も起きなかったように見える**
- **merge コミットは `app` が手前で断る**（`GitOp::CherryPick` は mainline を運ばない）。
  mainline を選ぶのは revert の開始（下）

### revert の開始

**`GitOp::Revert` は印が無ければカーソルの 1 本で使う**（01 の「revert の開始（`v`）」）。一時リポジトリを
作り直して叩いた（git 2.55.0、`GIT_EDITOR=true LC_ALL=C`、2026-09-29）。fixture は
`c1`（`a`）→ `c2`（`b`）→ `c3`（`a` を書き換え）の main に、`c1` から分けた topic（`t`）を
`--no-ff` で merge した形。

| 選んだコミット・状態 | exit | stdout / stderr | 残るもの | 分類 |
|---|---|---|---|---|
| 親が 1 つのコミット | 0 | stdout `[main 18dbc69] Revert "c2"` | 無し（HEAD が進む） | 成功 |
| 同上、**別のファイル**が dirty | 0 | 同上 | dirty のまま | 成功 |
| 同上、**同じファイル**が dirty | 128 | stderr `Your local changes to the following files would be overwritten by merge` | 無し | `Other` |
| merge コミット、`-m` 無し | 128 | stderr `is a merge but no -m option was given` | 無し | `Other` |
| merge コミット、`-m 1` | 0 | stdout `Revert "merge"`、**topic 側の `t` を消す** | 無し | 成功 |
| merge コミット、`-m 2` | 0 | stdout `Revert "merge"`、**main 側の `b` を消し `a` を戻す** | 無し | 成功 |
| merge コミット、`-m 3`（無い親） | 128 | stderr `does not have parent 3` | 無し | `Other` |
| 親が 1 つのコミットに `-m 1` | 0 | 成功と同じ | 無し | 成功 |
| 後の履歴が同じ箇所を変えている（`c1`） | 1 | stdout `CONFLICT (modify/delete)`、stderr `could not revert` | `REVERT_HEAD`、`UD`（**`sequencer/` は無し**） | `Conflict` |
| 取り消し済みのコミット | 1 | stdout `nothing to commit, working tree clean`、**stderr 空** | **無し**（`REVERT_HEAD` も `sequencer/` も残らない） | `NoChanges` |
| 同上、untracked がある | 1 | stdout `nothing added to commit but untracked files present` | 無し | `NoChanges` |
| 同上、tracked が dirty | 1 | stdout `no changes added to commit` | 無し | `NoChanges` |
| コンフリクトのあと `revert --skip` | 0 | 何も出さない | 無し（HEAD は元のまま） | 成功 |
| コンフリクトを **HEAD 側の内容で解いて** `revert --continue` | 1 | stdout `You are currently reverting commit …` + `nothing to commit`、**stderr 空** | **`REVERT_HEAD`**（進行中のまま） | `NoChanges` |
| ↑ のあと `revert --skip` | 0 | 何も出さない | 無し（HEAD は元のまま） | 成功 |
| 複数の revert で skip した先が空 | 1 | stdout `nothing to commit`、stderr 空 | **`sequencer/` だけ**（`todo` の先頭行は `revert …`） | `NoChanges` |
| 複数の revert で途中が空 | 1 | stdout `Revert currently in progress.` + 上の 3 つの結びのどれか | **`sequencer/` だけ**（`REVERT_HEAD` は無い） | `NoChanges` |

- **どの失敗でも HEAD は動かない**（1 本だけのとき）。cherry-pick と同じく、成功したときだけ
  ログまで読み直す（01 の「ログの読み直し」）
- **`--mainline N` は N 番目の親を残す。** `-m 1` と `-m 2` で消えるファイルが逆になる
  （表の 2 行）。画面では「残す側の親」と言う（01、07）
- **取り消す変更が無い形は `NoChanges` に分ける**（上の「エラー分類」）。stderr が空なので、
  案内が無いと要約行は stdout の結び（英語の `nothing to commit …`）になり、進行中操作が
  残ったかを言えない（01 の「失敗の案内文」）
- **1 本だけでも continue で空になる。** コンフリクトを HEAD 側の内容で解くと、開始と
  同じ文言で断られるが **`REVERT_HEAD` が残る**。**`NoChanges` は進行中操作が残ったかを
  言わない**ので、案内は op で分ける（開始なら「取り消し済み」、continue / skip なら
  「m から skip」。01 の「失敗の案内文」）。skip の先が空でも `sequencer/` が残って止まり、
  `todo` の先頭行が `revert` なので 02 の検出が `Revert` と読む（`m` の skip が届く）
- **複数の revert で途中が空だと止まる**（表の最後の行）。`sequencer/` だけが残る形で、
  `REVERT_HEAD` が無い。02 の検出は `sequencer/todo` の先頭行（`revert …`）で `Revert` と
  読む（`tests/git_read.rs` の fixture で固定）。抜け方は下の「複数のコミットの
  cherry-pick / revert」

### 複数のコミットの cherry-pick / revert

**印を付けた複数のコミットは 1 回の `cherry-pick <oid>…` / `revert <oid>…` に渡す**
（01 の「複数のコミットの cherry-pick / revert（印）」）。`GitOp::CherryPick(Vec<Oid>)` /
`GitOp::Revert { oids, .. }` は最初から複数を運べる形で、argv は oid を渡された順に並べる。
一時リポジトリで叩いた（git 2.56.0、`GIT_EDITOR=true LC_ALL=C`、2026-10-01）。

| 渡したもの・状態 | exit | stdout / stderr | 残るもの | 分類 |
|---|---|---|---|---|
| cherry-pick `t3 t1 t2`（topic の 3 本を順不同で） | 0 | — | 無し。**積まれた順は `t3` → `t1` → `t2`**（渡した順） | 成功 |
| cherry-pick `t1^..t3`（範囲） | 0 | — | 無し。古い順に積む | 成功 |
| cherry-pick `t2 t1 t3`、`t1` は HEAD に取り込み済み | 1 | stdout `[main …] t2` + `nothing to commit`、stderr `The previous cherry-pick is now empty` | `CHERRY_PICK_HEAD` + `sequencer/`（`todo` は `pick t1` / `pick t3`）。`t2` は積まれている | `Empty` |
| cherry-pick `o1 <merge>`（`-m` 無し） | 128 | stderr `commit … is a merge but no -m option was given.` | **`sequencer/` だけ**。**`o1` は積まれている** | `Other` |
| cherry-pick `-m 1 o1 <merge>` | 0 | — | 無し | 成功 |
| revert `m1 m2 m3`（同じファイルを順に書き換えた 3 本を古い順に） | 1 | stdout `CONFLICT (modify/delete)`、stderr `could not revert … m1` | `REVERT_HEAD` + `sequencer/` | `Conflict` |
| revert `m1^..m3`（範囲） | 0 | — | 無し。**新しい順（`m3` → `m1`）に積む** | 成功 |
| revert `ky hx m3`、`hx` は取り消し済み | 1 | stdout `[main …] Revert "ky"` + `Revert currently in progress.` + `nothing to commit`、stderr 空 | **`sequencer/` だけ**（`todo` の先頭行 `revert … hx`） | `NoChanges` |
| ↑ から `revert --continue` | 0 | — | 無し。**空の `hx` を落として `m3` まで積む** | 成功 |
| revert `zz hx ky`（`hx` は取り消し済み）で止まったところから `revert --skip` | 0 | stdout `[main …] Revert "ky"`（残りの `ky` を積む） | 無し | 成功 |

- **並べる向きは `app` が決める**（git は渡した順に積む）。cherry-pick は古い順、revert は
  新しい順（表の 1 行目と 6・7 行目）
- **merge が混ざると途中まで積んで止まる**（表の 4 行目）ので、`app` が手前で断る
- **複数では途中まで積んでから止まる**ので、成否を見ずにログを読み直す（上の「cherry-pick の
  開始」の「どの失敗でも HEAD は動かない」は 1 本だけのとき。01 の「ログの読み直し」）
- **途中が空の revert は continue でも skip でも抜けられる**（表の最後の 2 行）。1 本だけの
  continue が空で断られる形（上の「revert の開始」）と違い、`REVERT_HEAD` が無いので
  continue が空のコミットを作ろうとしない。案内は 1 本だけの形と同じ「m から skip」に
  揃える（01 の「失敗の案内文」）

