//! `graph` モジュール。設計は `docs/design/06-graph.md` を参照。
//!
//! コミット列（topo-order）からレーン割り当てと罫線セルを生成する。文字と色は持たず、
//! `ui` が `Cell` を文字に写す（ASCII フォールバックも `ui` 側）。
//!
//! 現時点では `app` の `AppState` が参照する型だけを定義している。レーン割り当ての
//! アルゴリズムは M1 で実装する。

use crate::git_read::{Commit, Oid};

/// レーンの色。実際の色は `ui` が `ColorId % palette.len()` で写す。
///
/// `usize` ではなく `u16`。`Cell::Cross` が色を 2 つ持つため、`usize` だと `Cell` が
/// 24 バイトになる（`u16` なら 6 バイト）。`cells` は行 × レーン数ぶん並び毎フレーム
/// 読まれるので、ここのサイズがそのままキャッシュに効く。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorId(pub u16);

/// グラフ 1 行分のレーン情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphRow {
    pub oid: Oid,
    /// このコミットが置かれた列。
    pub lane: usize,
    /// 左から順の罫線セル。
    pub cells: Vec<Cell>,
    pub color: ColorId,
}

impl GraphRow {
    /// レーン計算をまだ通していない行。`cells` が空なので `ui` は罫線を描かない。
    fn unlaned(oid: Oid) -> Self {
        Self {
            oid,
            lane: 0,
            cells: Vec::new(),
            color: ColorId(0),
        }
    }
}

/// 罫線 1 マス。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    Empty,
    /// `│`
    Vertical(ColorId),
    /// `●` / merge は `○`
    Commit(ColorId),
    /// `├` `╮` など右へ分岐
    BranchOut(ColorId),
    /// `╯` `╰` など左へ合流
    MergeIn(ColorId),
    /// `─`
    Horizontal(ColorId),
    /// `┼`。縦と横で色が違う。
    Cross(ColorId, ColorId),
}

/// 各レーンが次に待つコミット。行ごとに保存し、追加ページの計算を再開できるようにする。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaneState {
    pub lanes: Vec<Option<Oid>>,
}

impl LaneState {
    /// 続きのコミット列にレーンを割り当て、行を返す。状態は末尾行の直後まで進む。
    ///
    /// **中身はまだ実装していない。** レーン割り当てを入れるまでは、罫線の無い行を
    /// コミットと同じ数だけ返す。`app` は結果を並べるだけなので、アルゴリズムを
    /// 入れるときに触るのは `graph` だけで済む。
    pub fn extend(&mut self, commits: &[Commit]) -> Vec<GraphRow> {
        commits
            .iter()
            .map(|commit| GraphRow::unlaned(commit.oid))
            .collect()
    }
}
