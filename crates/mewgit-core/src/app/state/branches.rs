use crate::git_read::{Branch, BranchList, Head};

use super::{stepped_index, ListLoad};

/// ブランチペインの状態（01 の「ブランチ一覧」）。HEAD の状態もここが持つ（02 の「head」）。
#[derive(Debug, Default)]
pub struct BranchState {
    /// `git_read::branches` の戻り値そのまま。`None` はまだ一度も読めていない。
    ///
    /// **HEAD を別の置き場に写さない。** 元は `RepoInfo.head` に判別を分けて持っていたので、
    /// 「`Branch` なのに HEAD の行が無い」組が `app` で組め、読み手ごとに「行を先に読み、
    /// 無ければ `RepoInfo.head`」と書いていた。[`BranchList`] のまま持てば、組は型で
    /// 作れない（02 の「head」）。
    list: Option<BranchList>,
    /// [`BranchState::branches`] の添字。書くのは [`BranchState::loaded`]（名前で座り直す）と
    /// `move_cursor` / `set_cursor`（端で丸める）だけ。
    cursor: usize,
    /// 読み込みの段階。空の一覧と「まだ届いていない」を出し分けるのに要る。
    load: ListLoad,
}

impl BranchState {
    /// 届いた一覧で置き換える。
    ///
    /// **カーソルは選んでいたブランチの名前で座り直す**（01 の「ブランチ一覧」）。
    /// 読み直しは commit のたびに来るので、HEAD へ戻すだけだと別のブランチを見ながら
    /// commit した人のカーソルが毎回飛ぶ。**添字では座り直さない** — 一覧は名前順
    /// なので、ブランチが 1 本増えるだけで同じ添字が別のブランチを指す。
    ///
    /// **`pub(crate)` なのは `crate::test_util` が状態を組むため**（`app` の外）。
    pub(crate) fn loaded(&mut self, list: BranchList) {
        let selected = self.selected().map(|branch| branch.name.clone());
        let branches = list.branches();
        self.cursor = selected
            .and_then(|name| branches.iter().position(|branch| branch.name == name))
            // 選んでいたものが消えた（削除 / リネーム）か、まだ何も選んでいない。
            .or_else(|| list.head_row())
            // detached / unborn では HEAD の行が無い（`head_row` が `None`）ので先頭に座る。
            .unwrap_or(0);
        self.list = Some(list);
        self.load = ListLoad::Loaded;
    }

    /// 読み込みが失敗した。**読めていた一覧は捨てない**（`StatusLoaded` と同じ理由。
    /// 捨てると画面から消えるだけになる）。
    pub(in crate::app) fn failed(&mut self) {
        self.load = ListLoad::Failed;
    }

    /// 読み込みの段階。一覧が空のときに `ui` が出す文言をここから決める
    /// （07 の「Branches ペイン」）。
    ///
    /// **`jobs.is_running` の否定から消去法で導かない**（01 の「失敗の記憶」）。
    pub fn load(&self) -> ListLoad {
        self.load
    }

    /// カーソルの指すブランチ。一覧が空なら `None`。
    ///
    /// **切替・削除の対象はここからしか取らない。** `branches[cursor]` を呼び出し側で
    /// 書くと、一覧が空のとき（読み込み中・失敗・`git init` の直後）に添字が外れる腕が
    /// 増える。
    pub fn selected(&self) -> Option<&Branch> {
        self.branches().get(self.cursor)
    }

    /// 名前順の一覧。まだ届いていなければ空。
    pub fn branches(&self) -> &[Branch] {
        self.list.as_ref().map_or(&[], BranchList::branches)
    }

    /// HEAD の状態。**`None` は一覧を一度も読めていないこと**（まだ届いていないか、最初の
    /// 読み込みが失敗した。どちらかは [`BranchState::load`] が答える）。detached / unborn は
    /// `Some`。
    pub fn head(&self) -> Option<Head<'_>> {
        self.list.as_ref().map(BranchList::head)
    }

    /// HEAD の行の位置（[`BranchState::branches`] の添字）。detached / unborn / まだ
    /// 読めていないときは `None`。行ごとに印を付ける読み手のため（[`BranchList::head_row`]）。
    pub fn head_row(&self) -> Option<usize> {
        self.list.as_ref().and_then(BranchList::head_row)
    }

    /// HEAD が指しているブランチ。detached / unborn / まだ読めていないときは `None`。
    ///
    /// **push と fetch の行き先も、断られたことの記憶も、HEAD はここからしか取らない**
    /// （#22 の「キャッシュした状態から破壊的操作を選ばない」）。材料（追跡 ref の位置、
    /// ahead / behind、`push_remote`）を運ぶのは [`Branch`] の行だけ。
    pub fn head_branch(&self) -> Option<&Branch> {
        self.head().and_then(Head::branch)
    }

    /// カーソルの行（[`BranchState::branches`] の添字）。
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// カーソルを `delta` 行動かす。端では止まる。
    ///
    /// 行が 1 つも無ければ据え置く（[`stepped_index`]）。
    pub(in crate::app) fn move_cursor(&mut self, delta: isize) {
        self.cursor = stepped_index(self.cursor, self.branches().len(), delta);
    }

    /// カーソルを `row` 行目へ跳ばす。外なら最終行に丸め、行が 1 つも無ければ据え置く
    /// （`seated_index`）。
    ///
    /// **テストでだけ組む。** 本番に跳ばす経路は無く、カーソルは `loaded` が名前で座らせ、
    /// `move_cursor` が端で丸める。tui のテストが状態を組むので `test-util` でも出す。
    #[cfg(any(test, feature = "test-util"))]
    pub fn set_cursor(&mut self, row: usize) {
        self.cursor = super::seated_index(self.cursor, self.branches().len(), row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{fake_branch, fake_branch_list, fake_head_list};

    /// カーソルの指すブランチ名。
    fn cursor_name(branches: &BranchState) -> Option<&str> {
        branches.selected().map(|branch| branch.name.as_str())
    }

    #[test]
    fn a_loaded_branch_list_seats_the_cursor_on_the_head() {
        let mut branches = BranchState::default();

        branches.loaded(fake_branch_list(
            vec![
                fake_branch("another"),
                fake_branch("main"),
                fake_branch("topic"),
            ],
            Some("main"),
        ));

        assert_eq!(cursor_name(&branches), Some("main"));
        assert_eq!(branches.load(), ListLoad::Loaded);
    }

    #[test]
    fn a_detached_head_seats_the_branch_cursor_on_the_first_row() {
        // detached / unborn では HEAD の行が無い（02 の「head」）。
        let mut branches = BranchState::default();

        branches.loaded(fake_branch_list(
            vec![fake_branch("another"), fake_branch("main")],
            None,
        ));

        assert_eq!(cursor_name(&branches), Some("another"));
    }

    #[test]
    fn a_reload_keeps_the_cursor_on_the_branch_by_name() {
        // 読み直しは commit のたびに来る（01 の `update::Reload`）。HEAD へ戻すだけだと、
        // 別のブランチを見ながら commit した人のカーソルが毎回飛ぶ。
        let mut branches = BranchState::default();
        branches.loaded(fake_branch_list(
            vec![fake_branch("main"), fake_branch("topic")],
            Some("main"),
        ));
        branches.move_cursor(1);
        assert_eq!(cursor_name(&branches), Some("topic"));

        // ブランチが 1 本増えて添字がずれる（一覧は名前順）。
        branches.loaded(fake_branch_list(
            vec![
                fake_branch("alpha"),
                fake_branch("main"),
                fake_branch("topic"),
            ],
            Some("main"),
        ));

        assert_eq!(cursor_name(&branches), Some("topic"));
    }

    #[test]
    fn a_reload_that_loses_the_selected_branch_falls_back_to_the_head() {
        // 消えた（削除 / リネーム）ときの行き先。
        let mut branches = BranchState::default();
        branches.loaded(fake_branch_list(
            vec![fake_branch("main"), fake_branch("topic")],
            Some("main"),
        ));
        branches.move_cursor(1);

        branches.loaded(fake_head_list(fake_branch("main")));

        assert_eq!(cursor_name(&branches), Some("main"));
    }

    #[test]
    fn the_branch_cursor_stops_at_both_ends() {
        let mut branches = BranchState::default();
        branches.loaded(fake_branch_list(
            vec![fake_branch("a"), fake_branch("b")],
            None,
        ));

        branches.move_cursor(-1);
        assert_eq!(branches.cursor(), 0);
        branches.move_cursor(1);
        branches.move_cursor(1);
        assert_eq!(branches.cursor(), 1);
    }

    #[test]
    fn an_empty_branch_list_leaves_the_cursor_where_it_is() {
        // 選べるものが無い。`CommitFiles::move_cursor` と同じ。
        let mut branches = BranchState::default();

        branches.move_cursor(1);
        branches.set_cursor(3);

        assert_eq!(branches.cursor(), 0);
        assert_eq!(cursor_name(&branches), None);
    }

    #[test]
    fn setting_the_branch_cursor_past_the_end_lands_on_the_last_row() {
        let mut branches = BranchState::default();
        branches.loaded(fake_branch_list(
            vec![fake_branch("a"), fake_branch("b"), fake_branch("c")],
            Some("a"),
        ));

        branches.set_cursor(1);
        assert_eq!(cursor_name(&branches), Some("b"));
        branches.set_cursor(9);
        assert_eq!(cursor_name(&branches), Some("c"));
    }

    #[test]
    fn the_head_is_none_only_until_the_branch_list_arrives() {
        // `None` は「まだ届いていない」だけを言う。detached / unborn は `Some`（02 の「head」）。
        let mut branches = BranchState::default();
        assert_eq!(branches.head(), None);

        branches.loaded(fake_branch_list(vec![fake_branch("main")], None));

        assert!(matches!(branches.head(), Some(Head::Detached(_))));
        assert_eq!(branches.head_branch(), None);
    }

    #[test]
    fn a_failed_branch_list_keeps_what_was_already_there() {
        // 読めていた一覧は捨てない（`status_loaded` と同じ理由）。
        let mut branches = BranchState::default();
        branches.loaded(fake_head_list(fake_branch("main")));

        branches.failed();

        assert_eq!(branches.load(), ListLoad::Failed);
        assert_eq!(branches.branches().len(), 1);
    }
}
