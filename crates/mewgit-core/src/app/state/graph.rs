use crate::git_read::{Commit, CommitDetail, FileEntry, Oid, RefIndex};
use crate::graph::{GraphRow, LaneState};

use super::{seated_index, stepped_index, Pane};

/// グラフの表示行が指しているもの。
///
/// **先頭の WIP 行はコミットではない**ので `GraphState::lines` に入れず、表示行の
/// 添字を解くここで場合分けする（01 の「グラフの行」）。
///
/// コミット側が [`GraphLine`]（コミット + 罫線）なのは、`ui` が罫線を描くのに要るため。
/// [`GraphState::cursor_commit`] がコミットだけを取り出す薄い包み。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphSelection<'a> {
    /// ワーキングツリーの変更（グラフ先頭の `◇ WIP` 行）。
    Wip,
    Commit(&'a GraphLine),
}

impl GraphSelection<'_> {
    /// 右の領域に出るペイン。
    ///
    /// **選択とペインの対応はここ 1 つ。** `Enter` の行き先（`update::graph_key`）と
    /// 巡回の候補（[`AppState::focus_candidates`](super::AppState::focus_candidates)）が別々に決めると、画面はコミット詳細
    /// なのにキーは Working tree が受ける状態を作れてしまう。`ui::render` だけは
    /// 中身（`&GraphLine`）を取り出す必要があるので変種で分岐するが、そちらが
    /// この対応と食い違っていないことはテストで見張る。
    pub fn right_pane(self) -> Pane {
        match self {
            GraphSelection::Wip => Pane::Worktree,
            GraphSelection::Commit(_) => Pane::CommitDetail,
        }
    }
}

/// グラフペインの状態。
#[derive(Debug, Default)]
pub struct GraphState {
    /// 表示行。コミットとレーン情報を 1 本にまとめ、添字のずれを型で消す。
    pub lines: Vec<GraphLine>,
    /// 末尾行を処理した直後のレーン状態。追加ページはここから計算を再開する（06 参照）。
    /// 行ごとに持たないのは、途中の行の `LaneState` を読む場面が無いため。
    pub lanes: LaneState,
    /// 行の右に並べる ref ラベル（02）。引くのはグラフペインだけなのでここに置く。
    /// fetch 後の読み直しは M3 で決める。
    pub refs: RefIndex,
    /// カーソルが指す**表示行**の添字。`0` は先頭の WIP 行、`1..` が `lines`。
    ///
    /// 解くのは [`GraphState::selection`] の 1 つ。各所で `lines.get(cursor - 1)` と
    /// 書くと、`- 1` の書き忘れが 1 行ずれとして静かに通る（01 の「グラフの行」）。
    ///
    /// **private にして、書くのは [`set_cursor`](Self::set_cursor) と
    /// [`restart`](Self::restart) だけにする**（`move_cursor` は `set_cursor` の上に組んである）。
    /// `viewport` と同じ理由で、直書きを許すと `scroll` が追わずにカーソルが窓の外に
    /// 出る（01 の「グラフの行」）。カーソルを跳ばす経路（HEAD へ移る、oid で座り直す
    /// 等）は `set_cursor` を通す。
    cursor: usize,
    /// 画面最上部に出す行の添字。カーソルだけからは決まらない（カーソルを上に
    /// 動かしたとき、見えているうちは view を動かさない）ので状態として持つ。
    ///
    /// private なのは `cursor` と同じ理由。書くのは [`scroll_into_view`](Self::scroll_into_view) と [`restart`](Self::restart) だけ。
    scroll: usize,
    /// グラフに使える行数。`ui` がレイアウトから出して [`Msg::Layout`](crate::app::Msg::Layout) で運んでくる
    /// （`crate::app::Msg::Layout`）。core はヘッダや枠の高さを知らない。
    ///
    /// **private にして、書くのは [`set_viewport`](Self::set_viewport) だけにする。**
    /// 窓を変えたら `scroll` を詰め直す必要があり、直書きを許すと、窓を変える経路を
    /// 足したときにそこだけ詰め直しが抜けて、広げたのに下に空白が残る（01 の「グラフの行」）。
    viewport: usize,
    /// 遅延取得した詳細（02）。どのコミットのものかは `detail.oid` で判る。
    /// カーソルの指すコミットと食い違っていれば、まだ届いていないか失敗したという
    /// こと。「読み込み中」のフラグを別に持たない（01 の「コミット詳細の遅延取得」）。
    pub detail: Option<CommitDetail>,
    /// 遅延取得したファイル一覧（02 の「コミットのファイル一覧」）。
    ///
    /// **`Option` で持つ。** `Vec` 1 本にすると、まだ届いていないのと
    /// `--allow-empty` のコミット（変更が 1 つも無い）が同じ見た目になる
    /// （`WorktreeState::loaded` と同じ理由。01 の「コミットのファイル一覧」）。
    pub files: Option<CommitFiles>,
    /// 直前に詳細の読み込みが失敗したコミット。**同じコミットには出し直さない。**
    ///
    /// 出すと、リポジトリが壊れているあいだ「失敗 → 再読込 → 失敗」を回す。
    /// **`Option<Oid>` なのは、対象が違えば一度も試していないから**（`bool` にすると、
    /// 別のコミットの失敗のあとに乗ったコミットまで塞ぐ）。`DiffState::failed` と
    /// 同じ形。落とすのは読み込みの成功とカーソル移動（`scrolled`）で、
    /// **移動して戻ってくれば 1 回だけ試し直せる**。
    pub detail_failed: Option<Oid>,
    /// 直前にファイル一覧の読み込みが失敗したコミット。[`detail_failed`] と同じ形。
    ///
    /// **詳細と別に持つ。** 片方だけが失敗することがあり（一覧はツリーの比較で、
    /// 本文の読み出しとは別の失敗をする）、1 本にすると成功したほうまで塞ぐ。
    ///
    /// [`detail_failed`]: Self::detail_failed
    pub files_failed: Option<Oid>,
    /// これ以上読むコミットが無い。
    pub loaded_all: bool,
    /// 直近のログ読み込みが失敗した。
    ///
    /// 立っているあいだ追加読み込みを出さない。失敗すると `lines` を捨てるので、
    /// 「残りが 1 画面以下」の条件が必ず真になり、以後のレイアウト変化のたびに
    /// 読み直しを出してしまう。次の読み込みはユーザーの操作から始める
    /// （01 の「追加ページの読み込み」）。
    pub load_failed: bool,
    /// `Space` で印を付けたコミット。印があると `y` / `v` は印の全部を相手にする
    /// （01 の「複数のコミットの cherry-pick / revert（印）」）。**並びに意味は無い** ——
    /// git に渡す順は [`GraphState::marked_lines`] がグラフの並びから決める。
    pub marks: Vec<Oid>,
}

impl GraphState {
    /// 積んだ行を捨てて、先頭から読み直せる状態にする。
    ///
    /// ログ読み込みの失敗（02 の契約により、失敗したセッションは捨てられ次は先頭から
    /// 来る）と、commit のあとの読み直しで同じことをする。**1 本にまとめてあるのは、
    /// 片方に足し忘れると `lines` に無いコミットの詳細やレーンの状態が残るため。**
    /// `load_failed` はここでは触らない（呼び出し元で値が違う。失敗の側だけが立てる）。
    pub fn restart(&mut self) {
        self.lines.clear();
        // もう `lines` に無いコミットを指したままにしない。
        self.lanes = LaneState::default();
        self.detail = None;
        self.files = None;
        self.detail_failed = None;
        self.files_failed = None;
        self.cursor = 0;
        self.scroll = 0;
        self.loaded_all = false;
        // 並べ直すには印のコミットが `lines` に居る必要がある（01）。
        self.marks.clear();
    }

    /// `oid` の印を付け外しする。
    pub fn toggle_mark(&mut self, oid: Oid) {
        match self.marks.iter().position(|mark| *mark == oid) {
            Some(at) => {
                self.marks.remove(at);
            }
            None => self.marks.push(oid),
        }
    }

    /// `oid` に印が付いているか。
    pub fn is_marked(&self, oid: Oid) -> bool {
        self.marks.contains(&oid)
    }

    /// 印の付いた行を**グラフの並び（新しい順）**で返す。cherry-pick は逆にして古い順に
    /// 使う（03 の「複数のコミットの cherry-pick / revert」）。鍵が日付でなく行の並びなのは、
    /// 日付が rebase / amend で並びと食い違うから。
    pub fn marked_lines(&self) -> Vec<&GraphLine> {
        self.lines
            .iter()
            .filter(|line| self.is_marked(line.commit.oid))
            .collect()
    }

    /// 表示行の数。先頭の WIP 行を含むので必ず 1 以上。
    ///
    /// **WIP 行は変更が無くても出す**（07 の「Graph ペイン」）。件数で出し入れすると
    /// status が届くたびに表示行数が変わり、カーソルが指すコミットを oid で座り直させる
    /// 必要が出る。常に 1 行あるなら `1 + lines.len()` の定数関係で済む。
    pub fn rows(&self) -> usize {
        self.lines.len() + 1
    }

    /// `index` 番目の表示行。[`rows`](Self::rows) の外なら `None`。
    ///
    /// **表示行から `lines` の添字への変換はここだけ。** `ui` が窓を切り出すときも
    /// ここを通す。各所で `lines.get(index - 1)` と書くと、`- 1` の書き忘れが 1 行ずれ
    /// として静かに通り、反転する行と右ペインの中身が食い違う（01 の「グラフの行」）。
    pub fn row(&self, index: usize) -> Option<GraphSelection<'_>> {
        match index.checked_sub(1) {
            Some(at) => self.lines.get(at).map(GraphSelection::Commit),
            None => Some(GraphSelection::Wip),
        }
    }

    /// カーソルが指しているもの。
    ///
    /// カーソルは常に表示行の中にいる（[`set_cursor`](Self::set_cursor) が丸める）ので
    /// `Wip` に落ちるのは本当に WIP 行を指しているときだけ。
    pub fn selection(&self) -> GraphSelection<'_> {
        self.row(self.cursor).unwrap_or(GraphSelection::Wip)
    }

    /// カーソルが指しているコミット。WIP 行を指していれば `None`。
    pub fn cursor_commit(&self) -> Option<&Commit> {
        match self.selection() {
            GraphSelection::Commit(line) => Some(&line.commit),
            GraphSelection::Wip => None,
        }
    }

    /// カーソルの指すコミットの詳細。別のコミットのものしか無ければ `None`。
    ///
    /// 「読み込み中」のフラグを持たず oid の一致で鮮度を見る（01 の「コミット詳細の
    /// 遅延取得」）。その判定をここに置くのは、`update`（次に何を読むか）と
    /// `ui`（何を描くか）が同じものを使うため。両方に書くと、片方だけ直したときに
    /// 古い詳細を出したままになってもコンパイルは通る。
    pub fn fresh_detail(&self) -> Option<&CommitDetail> {
        let oid = self.cursor_commit()?.oid;
        self.detail.as_ref().filter(|detail| detail.oid == oid)
    }

    /// カーソルの指すコミットのファイル一覧。別のコミットのものしか無ければ `None`。
    ///
    /// [`fresh_detail`](Self::fresh_detail) と同じ形。`update`（次に何を読むか）と
    /// `ui`（何を描くか）が同じものを使う。
    pub fn fresh_files(&self) -> Option<&CommitFiles> {
        let oid = self.cursor_commit()?.oid;
        self.files.as_ref().filter(|files| files.oid == oid)
    }

    /// [`fresh_files`](Self::fresh_files) の可変版。カーソルを動かすのに使う。
    ///
    /// **見えていない一覧のカーソルを動かさない。** 描くのも `wanted_diff` が対象を
    /// 決めるのも `fresh_files` を通したものだけなので、濾さずに動かすと画面に出て
    /// いない一覧のカーソルが黙って動く（[`DiffState::fresh_view_mut`](super::DiffState::fresh_view_mut) と同じ形）。
    pub fn fresh_files_mut(&mut self) -> Option<&mut CommitFiles> {
        let oid = self.cursor_commit()?.oid;
        self.files.as_mut().filter(|files| files.oid == oid)
    }

    /// カーソルの指すコミットの詳細を読みに行って失敗したままか。
    ///
    /// 「読み込み中」と「読み込めなかった」の出し分けに使う。**`ui` は
    /// `jobs.is_running` の否定から消去法で導かない。** `Cmd` の発行経路を変えた
    /// ときに片方だけ直しても通ってしまう（[`DiffState::has_failed`](super::DiffState::has_failed) と同じ形）。
    pub fn has_detail_failed(&self) -> bool {
        self.failed_here(self.detail_failed)
    }

    /// カーソルの指すコミットのファイル一覧を読みに行って失敗したままか。
    /// [`has_detail_failed`](Self::has_detail_failed) と同じ形。
    pub fn has_files_failed(&self) -> bool {
        self.failed_here(self.files_failed)
    }

    /// 失敗の印がカーソルの指すコミットのものか。
    fn failed_here(&self, failed: Option<Oid>) -> bool {
        failed.is_some() && self.cursor_commit().map(|commit| commit.oid) == failed
    }

    /// カーソルの指すコミットの一覧で選んでいるファイル。
    ///
    /// 一覧がまだ届いていないときと、変更が 1 つも無いコミットのときは `None`。
    /// [`AppState::wanted_diff`](super::AppState::wanted_diff) と `ui` の反転がここを通る。
    pub fn selected_file(&self) -> Option<&FileEntry> {
        let files = self.fresh_files()?;
        files.entries.get(files.cursor)
    }

    /// カーソルが指す表示行の添字。
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// 画面最上部に出す行の添字。
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// カーソルを `delta` 行動かす。端では止まる。
    pub fn move_cursor(&mut self, delta: isize) {
        self.set_cursor(self.cursor.saturating_add_signed(delta));
    }

    /// カーソルを表示行 `row` へ跳ばす。[`rows`](Self::rows) の外なら最終行に丸め、
    /// 窓の外へ出たならスクロールが追う。
    pub fn set_cursor(&mut self, row: usize) {
        // 行が 1 つも積まれていなくても WIP 行はあるので、`rows()` は 1 以上（据え置く腕には入らない）。
        self.cursor = seated_index(self.cursor, self.rows(), row);
        self.scroll_into_view();
    }

    /// グラフに使える行数。
    pub fn viewport(&self) -> usize {
        self.viewport
    }

    /// 窓を `rows` 行にし、その窓に合わせてスクロールを寄せ直す。
    pub fn set_viewport(&mut self, rows: usize) {
        self.viewport = rows;
        self.scroll_into_view();
    }

    /// カーソルが見える位置までスクロールを寄せる。
    ///
    /// カーソルが窓の中にいる限り動かさない。カーソルと `viewport` から毎回導くと、
    /// 下端から上へ戻るときにカーソルが最下行に貼り付いたまま中身だけが流れる。
    /// 毎回、カーソルを追う寄せの前に、`scroll` が上限（`rows - viewport`）を超えていれば
    /// 詰める。効くのは窓が広がったときだけで、広げる前に窓の中にいたカーソルは広げた後も
    /// 窓の中に残るので、寄せは動かず、詰めなければ下に空白が残る（01）。
    fn scroll_into_view(&mut self) {
        if self.viewport == 0 {
            return;
        }
        // 先に詰める（理由は doc）。
        self.scroll = self.scroll.min(self.rows().saturating_sub(self.viewport));
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + self.viewport {
            self.scroll = self.cursor - self.viewport + 1;
        }
    }
}

/// 遅延取得したコミットのファイル一覧と、その中の選択位置。
///
/// どのコミットのものかは `oid` で判る（`CommitDetail` と同じ形）。「読み込み中」の
/// フラグを別に持たない（01 の「コミットのファイル一覧」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitFiles {
    pub oid: Oid,
    /// 02 の `commit_files()` の戻り値そのまま。空なら変更の無いコミット。
    pub entries: Vec<FileEntry>,
    /// 一覧の選択位置。左の Diff ペインの対象を決める。
    pub cursor: usize,
}

impl CommitFiles {
    /// カーソルを `delta` 行動かす。端では止まる。
    ///
    /// 行が 1 つも無ければ据え置く（選べるものが無い）。
    pub fn move_cursor(&mut self, delta: isize) {
        self.cursor = stepped_index(self.cursor, self.entries.len(), delta);
    }
}

/// グラフの 1 行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphLine {
    pub commit: Commit,
    pub row: GraphRow,
}
