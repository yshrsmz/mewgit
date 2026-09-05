//! `graph` モジュール。設計は `docs/design/06-graph.md` を参照。
//!
//! コミット列（topo-order）からレーン割り当てと罫線セルを生成する。文字と色は持たず、
//! `ui` が `Cell` を文字に写す（ASCII フォールバックも `ui` 側）。
//!
//! 現時点では `app` の `AppState` が参照する型だけを定義している。レーン割り当ての
//! アルゴリズムは M1 で実装する。

use crate::git_read::Oid;

/// レーンの色。実際の色は `ui` が `ColorId % palette.len()` で写す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorId(pub usize);

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
