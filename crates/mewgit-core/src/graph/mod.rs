//! `graph` モジュール。設計は `docs/design/06-graph.md` を参照。
//!
//! コミット列（topo-order）からレーン割り当てと罫線セルを生成する。文字と色は持たず、
//! `ui` が `Cell` を文字に写す（ASCII フォールバックも `ui` 側）。
//!
//! 現時点では `app` の `AppState` が参照する型だけを定義している。レーン割り当ての
//! アルゴリズムは M1 で実装する。

use crate::git_read::Oid;

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
    /// レーン計算をまだ通していない行。
    ///
    /// `cells` が空なので `ui` は罫線を描かない。`graph::lane` を実装するまでの
    /// 暫定で、**レーン割り当ての PR でこの関数と呼び出し側を消す**。ここに置いて
    /// あるのは、消す場所を 1 つにするため。
    pub fn unlaned(oid: Oid) -> Self {
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
