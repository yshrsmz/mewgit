use crate::git_read::{Oid, StashEntry};

use super::{seated_index, stepped_index, ListLoad};

/// Stash ペインの状態（01 の「Stash ペイン」）。
#[derive(Debug, Default)]
pub struct StashState {
    /// `stash@{0}` から順。`git_read::stashes` の戻り値そのまま。
    ///
    /// **書くのは [`StashState::loaded`] だけ。** 外から差し替えると `load` / `epoch` が
    /// 古いまま残り、apply / pop / drop に渡す index を信じてよいかの根拠が崩れる。
    entries: Vec<StashEntry>,
    /// [`StashState::entries`] の添字。書くのは `loaded`（oid で座り直す）と
    /// `move_cursor` / [`StashState::set_cursor`]（端で丸める）だけ。
    cursor: usize,
    /// 読み込みの段階。空の一覧と「まだ届いていない」を出し分ける（[`BranchState`](super::BranchState) と
    /// 同じ理由）。
    load: ListLoad,
    /// いつの一覧か（届いた返信の刻印）。`None` はまだ読めていない。
    /// [`WorktreeState::snapshot_epoch`](super::WorktreeState::snapshot_epoch) と同じ形で、apply / pop / drop に渡す index を
    /// 信じてよいかの根拠（01 の「stash の書き込み」）。
    epoch: Option<u64>,
}

impl StashState {
    /// 届いた一覧で置き換える。
    ///
    /// **カーソルは選んでいた stash の oid で座り直す**（01 の「Stash ペイン」）。
    /// **index では座り直さない** —— push は先頭に 1 つ足し、drop / pop は後ろを
    /// 1 つずつ詰めるので、同じ index が別の stash を指す（git 2.55 で確認）。
    /// 選んでいたものが消えたら（drop / pop）、**同じ位置に留まる**（末尾を越えたら
    /// 末尾）。消えた 1 つの次の stash がそこへ詰めてくるので、続けて drop できる。
    pub(in crate::app) fn loaded(&mut self, entries: Vec<StashEntry>, epoch: Option<u64>) {
        let selected = self.selected().map(|stash| stash.oid);
        self.cursor = selected
            .and_then(|oid| entries.iter().position(|stash| stash.oid == oid))
            .unwrap_or_else(|| self.cursor.min(entries.len().saturating_sub(1)));
        self.entries = entries;
        self.load = ListLoad::Loaded;
        self.epoch = epoch;
    }

    /// 読み込みが失敗した。読めていた一覧は捨てない（[`BranchState::failed`](super::BranchState::failed) と同じ）。
    pub(in crate::app) fn failed(&mut self) {
        self.load = ListLoad::Failed;
    }

    /// いつの一覧か。読むのは `AppState::stashes_are_current` だけ（書き込みの刻印と
    /// 比べる）。
    pub(super) fn epoch(&self) -> Option<u64> {
        self.epoch
    }

    /// 読み込みの段階（[`BranchState::load`](super::BranchState::load) と同じ）。
    pub fn load(&self) -> ListLoad {
        self.load
    }

    /// `stash@{0}` から順の一覧。
    pub fn entries(&self) -> &[StashEntry] {
        &self.entries
    }

    /// カーソルの行（[`StashState::entries`] の添字）。
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// カーソルの指す stash。一覧が空なら `None`。
    pub fn selected(&self) -> Option<&StashEntry> {
        self.entries.get(self.cursor)
    }

    /// `oid` の stash。一覧に無ければ `None`（Diff ペインの見出しに `stash@{n}` を
    /// 出すのに使う。07 の「Diff ペイン」）。
    pub fn find(&self, oid: Oid) -> Option<&StashEntry> {
        self.entries.iter().find(|stash| stash.oid == oid)
    }

    /// カーソルを `delta` 行動かす。端では止まる（[`BranchState::move_cursor`](super::BranchState::move_cursor) と同じ）。
    pub(in crate::app) fn move_cursor(&mut self, delta: isize) {
        self.cursor = stepped_index(self.cursor, self.entries.len(), delta);
    }

    /// カーソルを `row` 行目へ跳ばす（[`BranchState::set_cursor`](super::BranchState::set_cursor) と同じ）。
    pub fn set_cursor(&mut self, row: usize) {
        self.cursor = seated_index(self.cursor, self.entries.len(), row);
    }
}
