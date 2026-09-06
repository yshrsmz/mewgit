//! レーン割り当て。設計は `docs/design/06-graph.md` の「アルゴリズム」。

use crate::git_read::{Commit, Oid};

use super::{Cell, ColorId, GraphRow, Vertical};

/// 1 レーンが次に待つコミットと、その色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Lane {
    oid: Oid,
    color: ColorId,
}

/// 各レーンが次に待つコミット。末尾行の直後の状態だけを保持し、追加ページの計算を
/// そこから再開する（06 の「ページングとの整合」）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaneState {
    /// 左から順。`None` は空きレーン。
    lanes: Vec<Option<Lane>>,
    next_color: u16,
}

impl LaneState {
    /// 続きのコミット列にレーンを割り当て、行を返す。状態は末尾行の直後まで進む。
    ///
    /// 渡すのは topo-order のコミット列（`git_read::LogSession` が返す順）。
    /// 途中から渡すときは、直前のページを処理した `LaneState` をそのまま使う。
    pub fn extend(&mut self, commits: &[Commit]) -> Vec<GraphRow> {
        commits.iter().map(|commit| self.row(commit)).collect()
    }

    /// コミット 1 つぶん進める。
    fn row(&mut self, commit: &Commit) -> GraphRow {
        // 続いているレーンを先に写す。この行で動かす前の状態がそのまま縦線になる。
        let mut cells = Vec::with_capacity(self.lanes.len() + 1);
        for (index, occupied) in self.lanes.iter().enumerate() {
            if let Some(occupied) = occupied {
                cell_mut(&mut cells, index).vertical = Vertical::Through(occupied.color);
            }
        }

        let waiting: Vec<usize> = self.waiting_for(commit.oid).collect();
        let (lane, color) = match waiting.first() {
            Some(&index) => (index, self.lane_color(index)),
            // どのレーンも待っていない = ブランチの先頭。右端に新しいレーンを開く。
            None => {
                self.lanes.push(None);
                (self.lanes.len() - 1, self.take_color())
            }
        };

        // 最左以外の待ちレーンは、このコミットへ合流して空になる。
        for &index in waiting.iter().skip(1) {
            let merged = self.lanes[index].take().expect("待ちレーンは埋まっている");
            cell_mut(&mut cells, index).vertical = Vertical::Up(merged.color);
            connect(&mut cells, lane, index, merged.color);
        }

        let mut parents = commit.parents.iter();
        // 第 1 親は同じレーンを色ごと引き継ぐ。親が無ければレーンが空く。
        self.lanes[lane] = parents.next().map(|&oid| Lane { oid, color });

        for &oid in parents {
            let waits = self.waiting_for(oid).next();
            match waits {
                // 既に待っているレーンがあればそこへ引くだけ。レーン自体は続く。
                Some(target) => {
                    let color = self.lane_color(target);
                    connect(&mut cells, lane, target, color);
                }
                None => {
                    let color = self.take_color();
                    let target = self.open_lane(lane, &waiting);
                    self.lanes[target] = Some(Lane { oid, color });
                    cell_mut(&mut cells, target).vertical = Vertical::Down(color);
                    connect(&mut cells, lane, target, color);
                }
            }
        }

        // コミット本体は最後に置く。第 1 親を置いたレーンには `Through` が入っている。
        let merge = commit.parents.len() > 1;
        cell_mut(&mut cells, lane).vertical = Vertical::Commit { color, merge };

        while self.lanes.last().is_some_and(Option::is_none) {
            self.lanes.pop();
        }
        // セルを伸ばすのは `cell_mut` と `connect` だけで、どちらも伸ばした先を必ず
        // 埋める。末尾が空になる経路は無い（06 の「末尾の空セルは落とす」）。
        debug_assert!(cells.last().is_none_or(|cell: &Cell| !cell.is_empty()));

        GraphRow {
            oid: commit.oid,
            lane,
            cells,
            color,
        }
    }

    /// その oid を待っているレーンの添字。左から順。
    fn waiting_for(&self, oid: Oid) -> impl Iterator<Item = usize> + '_ {
        self.lanes
            .iter()
            .enumerate()
            .filter(move |(_, lane)| lane.is_some_and(|lane| lane.oid == oid))
            .map(|(index, _)| index)
    }

    fn lane_color(&self, index: usize) -> ColorId {
        self.lanes[index]
            .expect("埋まっているレーンの色を引く")
            .color
    }

    /// `after` より右にある空きレーン。無ければ右端に開く。
    ///
    /// この行で空けたレーン（`waiting` の合流先）は避ける。使うと、ずっと続いていた
    /// レーンに見えてしまう（06 の「アルゴリズム」2 の 3 つ目）。
    fn open_lane(&mut self, after: usize, waiting: &[usize]) -> usize {
        let reusable = self
            .lanes
            .iter()
            .enumerate()
            .skip(after + 1)
            .find(|(index, lane)| lane.is_none() && !waiting.contains(index))
            .map(|(index, _)| index);

        reusable.unwrap_or_else(|| {
            self.lanes.push(None);
            self.lanes.len() - 1
        })
    }

    /// 新しい色を採番する。`ui` が palette に写すので、ここでは単調に増やすだけ。
    fn take_color(&mut self) -> ColorId {
        let color = ColorId(self.next_color);
        self.next_color = self.next_color.wrapping_add(1);
        color
    }
}

/// `index` のマス。足りない分は空セルで伸ばす。
fn cell_mut(cells: &mut Vec<Cell>, index: usize) -> &mut Cell {
    if cells.len() <= index {
        cells.resize(index + 1, Cell::default());
    }
    &mut cells[index]
}

/// `from` と `to` を横線でつなぐ。
fn connect(cells: &mut Vec<Cell>, from: usize, to: usize, color: ColorId) {
    let (left, right) = (from.min(to), from.max(to));
    if left == right {
        return;
    }
    cell_mut(cells, right);

    for (index, cell) in cells.iter_mut().enumerate().take(right + 1).skip(left) {
        cell.horizontal = cell.horizontal.add(index > left, index < right, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Horizontal;
    use crate::test_util::fake_commit;

    /// `parents` を持つコミット。oid は添字から作る（`fake_commit`）。
    fn commit(index: u8, parents: &[u8]) -> Commit {
        Commit {
            parents: parents.iter().map(|&index| oid(index)).collect(),
            ..fake_commit(index, "")
        }
    }

    fn oid(index: u8) -> Oid {
        fake_commit(index, "").oid
    }

    /// 行を文字にして並べる。期待値を読める形にするためのテスト専用レンダラ。
    ///
    /// 文字の割り当ては 06 の対応表、レーン間の埋め方は 07 の Graph ペインの規則
    /// （左右がつながっているときだけ `─`）。**同じ表が `ui` 側にも生える**ので、
    /// 変えるときは 06 / 07 / `ui::panes::graph` と一緒に直す。
    fn render(rows: &[GraphRow]) -> String {
        rows.iter()
            .map(|row| {
                let mut line = String::new();
                for (index, cell) in row.cells.iter().enumerate() {
                    if index > 0 {
                        let previous = row.cells[index - 1];
                        line.push(
                            if previous.horizontal.goes_right() && cell.horizontal.goes_left() {
                                '─'
                            } else {
                                ' '
                            },
                        );
                    }
                    line.push(glyph(*cell));
                }
                format!("{}\n", line.trim_end())
            })
            .collect()
    }

    fn glyph(cell: Cell) -> char {
        use Horizontal as H;
        use Vertical as V;
        match (cell.vertical, cell.horizontal) {
            (V::Commit { merge: true, .. }, _) => '○',
            (V::Commit { merge: false, .. }, _) => '●',
            (V::None, H::None) => ' ',
            (V::None, _) => '─',
            (V::Through(_), H::None) => '│',
            (V::Through(_), H::Left(_)) => '┤',
            (V::Through(_), H::Right(_)) => '├',
            (V::Through(_), H::Both(_)) => '┼',
            (V::Up(_), H::None) => '╵',
            (V::Up(_), H::Left(_)) => '╯',
            (V::Up(_), H::Right(_)) => '╰',
            (V::Up(_), H::Both(_)) => '┴',
            (V::Down(_), H::None) => '╷',
            (V::Down(_), H::Left(_)) => '╮',
            (V::Down(_), H::Right(_)) => '╭',
            (V::Down(_), H::Both(_)) => '┬',
        }
    }

    #[test]
    fn a_linear_history_stays_in_one_lane() {
        let commits = [commit(3, &[2]), commit(2, &[1]), commit(1, &[])];

        let rows = LaneState::default().extend(&commits);

        assert_eq!(render(&rows), "●\n●\n●\n");
        assert!(rows.iter().all(|row| row.lane == 0));
        // 第 1 親を辿るあいだ色は変わらない。
        assert!(rows.iter().all(|row| row.color == rows[0].color));
    }

    #[test]
    fn two_tips_meet_at_their_common_parent() {
        // 10 と 11 はどちらも 12 を親に持つ。11 の行では 10 のレーンが縦線で残る。
        let commits = [commit(10, &[12]), commit(11, &[12]), commit(12, &[])];

        let rows = LaneState::default().extend(&commits);

        assert_eq!(
            render(&rows),
            "\
●
│ ●
●─╯
"
        );
        assert_eq!(rows[1].lane, 1);
        assert_ne!(rows[1].color, rows[0].color);
        // 合流先は最左のレーン。色も最左のものを引き継ぐ。
        assert_eq!(rows[2].lane, 0);
        assert_eq!(rows[2].color, rows[0].color);
    }

    #[test]
    fn a_merge_opens_a_lane_for_its_second_parent() {
        let commits = [
            commit(1, &[2, 3]),
            commit(2, &[4]),
            commit(3, &[4]),
            commit(4, &[]),
        ];

        let rows = LaneState::default().extend(&commits);

        assert_eq!(
            render(&rows),
            "\
○─╮
● │
│ ●
●─╯
"
        );
    }

    #[test]
    fn the_cells_of_a_merge_row_carry_direction_and_colour() {
        // レンダラを通さずに `cells` そのものを固定する。レンダラ側が間違っていても
        // 気付けるようにするため。
        let rows = LaneState::default().extend(&[commit(1, &[2, 3])]);

        assert_eq!(
            rows[0].cells,
            vec![
                Cell {
                    vertical: Vertical::Commit {
                        color: ColorId(0),
                        merge: true
                    },
                    horizontal: Horizontal::Right(ColorId(1)),
                },
                Cell {
                    vertical: Vertical::Down(ColorId(1)),
                    horizontal: Horizontal::Left(ColorId(1)),
                },
            ]
        );
    }

    #[test]
    fn an_octopus_merge_opens_a_lane_per_extra_parent() {
        let commits = [
            commit(1, &[2, 3, 4]),
            commit(2, &[5]),
            commit(3, &[5]),
            commit(4, &[5]),
            commit(5, &[]),
        ];

        let rows = LaneState::default().extend(&commits);

        assert_eq!(
            render(&rows),
            "\
○─┬─╮
● │ │
│ ● │
│ │ ●
●─┴─╯
"
        );
    }

    #[test]
    fn a_second_parent_that_is_already_waiting_is_reached_by_a_line() {
        // 左向きの横線。feature ブランチ（レーン 1）が、レーン 0 で待っている
        // コミットを第 2 親に取り込む形。
        let commits = [
            commit(1, &[3]),
            commit(2, &[4, 3]),
            commit(4, &[3]),
            commit(3, &[]),
        ];

        let rows = LaneState::default().extend(&commits);

        assert_eq!(
            render(&rows),
            "\
●
├─○
│ ●
●─╯
"
        );
        // 第 2 親のレーンは増えない。既にあるレーンへ引くだけ。
        assert_eq!(rows[1].cells.len(), 2);
    }

    #[test]
    fn a_new_root_reuses_the_lane_that_was_freed() {
        let commits = [commit(1, &[2]), commit(3, &[]), commit(2, &[])];

        let mut lanes = LaneState::default();
        let rows = lanes.extend(&commits);

        assert_eq!(
            render(&rows),
            "\
●
│ ●
●
"
        );
        // ルートは右端に開き、親が無いのでその行で閉じる。
        assert_eq!(rows[1].lane, 1);
        assert_eq!(rows[2].lane, 0);
        // 最後のコミットも親を持たないので、レーンは 1 本も残らない。
        assert!(lanes.lanes.is_empty());
    }

    #[test]
    fn a_lane_freed_by_a_merge_is_not_reused_in_the_same_row() {
        // 1 の行ではレーン 1 が合流して空くが、第 2 親 3 はその跡地には置かない。
        // 置くと「ずっと続いていたレーン」に見える。
        let commits = [
            commit(10, &[1]),
            commit(11, &[1]),
            commit(12, &[13]),
            commit(1, &[2, 3]),
        ];

        let rows = LaneState::default().extend(&commits);

        assert_eq!(
            render(&rows),
            "\
●
│ ●
│ │ ●
○─┴─┼─╮
"
        );
        // 空いたレーン 1 は次のコミットから使う。この行では跡地を飛ばして右端に開く。
        assert_eq!(rows[3].cells.len(), 4);
        assert!(matches!(rows[3].cells[1].vertical, Vertical::Up(_)));
        assert!(matches!(rows[3].cells[3].vertical, Vertical::Down(_)));
        // 線が重なるマスの色は先に引いたほう。レーン 1 のマスは合流線（レーン 1 の色）が
        // 先に通り、第 2 親へ伸ばす線が後から重なる。重なっていないレーン 2 のマスは
        // 第 2 親の線の色のまま（06 の「色」）。
        let Vertical::Down(branch) = rows[3].cells[3].vertical else {
            panic!("第 2 親のレーンが開いていません: {:?}", rows[3].cells[3]);
        };
        assert_eq!(rows[3].cells[1].horizontal, Horizontal::Both(rows[1].color));
        assert_eq!(rows[3].cells[2].horizontal, Horizontal::Both(branch));
    }

    #[test]
    fn a_new_parent_takes_the_hole_an_earlier_row_left_behind() {
        // レーン 1 は 6 の行で空くが、レーン 2 が生きているので穴として残る。
        // 5 の行の第 2 親はその穴に入る（末尾に開くのではなく）。
        let commits = [
            commit(1, &[5]),
            commit(2, &[6]),
            commit(3, &[7]),
            commit(6, &[]),
            commit(5, &[8, 9]),
        ];

        let rows = LaneState::default().extend(&commits);

        assert_eq!(
            render(&rows),
            "\
●
│ ●
│ │ ●
│ ● │
○─╮ │
"
        );
        assert!(matches!(rows[4].cells[1].vertical, Vertical::Down(_)));
        assert_eq!(rows[4].cells.len(), 3, "右端に開いています");
    }

    #[test]
    fn splitting_the_commits_into_pages_gives_the_same_rows() {
        // 「ページングとの整合」。どこで切っても、続けて渡したときと同じ行になる。
        let commits = [
            commit(1, &[2, 3]),
            commit(2, &[4]),
            commit(3, &[5]),
            commit(4, &[6]),
            commit(5, &[6]),
            commit(6, &[7]),
            commit(8, &[7]),
            commit(7, &[]),
        ];

        let mut whole = LaneState::default();
        let expected = whole.extend(&commits);

        for split in 0..=commits.len() {
            let mut paged = LaneState::default();
            let mut rows = paged.extend(&commits[..split]);
            rows.extend(paged.extend(&commits[split..]));

            assert_eq!(rows, expected, "{split} 件目で分割したとき");
            assert_eq!(paged, whole, "{split} 件目で分割したとき");
        }
    }
}
