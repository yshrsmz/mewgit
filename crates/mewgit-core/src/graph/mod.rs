//! `graph` モジュール。設計は `docs/design/06-graph.md` を参照。
//!
//! コミット列（topo-order）からレーン割り当てと罫線セルを生成する。文字と色は持たず、
//! `ui` が `Cell` を文字に写す（ASCII フォールバックも `ui` 側）。

mod lane;

use crate::git_read::Oid;

pub use lane::LaneState;

/// レーンの色。実際の色は `ui` が `ColorId % palette.len()` で写す。
///
/// `usize` ではなく `u16`。`Cell` は色を 2 つ持つので、`usize` にすると 32 バイト、
/// `u16` なら 8 バイトになる。`cells` は行 × レーン数ぶん並び毎フレーム読まれるので、
/// ここのサイズがそのままキャッシュに効く。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorId(pub u16);

/// グラフ 1 行分のレーン情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphRow {
    pub oid: Oid,
    /// このコミットが置かれた列。
    pub lane: usize,
    /// 左から順の罫線セル。末尾の空セルは含まない。
    pub cells: Vec<Cell>,
    pub color: ColorId,
}

/// 罫線 1 マス。
///
/// 縦と横を別に持つ。「右へ分岐」のような向きの名前だと `╮`（上が無い）と `┤`
/// （上から続く）を区別できず、`ui` が隣の行を覗いて文字を決めることになるため
/// （06 の「なぜ『縦 × 横』なのか」）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cell {
    pub vertical: Vertical,
    pub horizontal: Horizontal,
}

impl Cell {
    /// 線が 1 本も通っていない。`ui` は空白を描く。
    pub fn is_empty(self) -> bool {
        self == Self::default()
    }
}

/// マスの縦成分。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Vertical {
    #[default]
    None,
    /// 上下に貫く（`│`）。
    Through(ColorId),
    /// 上から来てこの行で終わる（`╯` の縦側）。
    Up(ColorId),
    /// この行から下へ伸びる（`╮` の縦側）。
    Down(ColorId),
    /// コミット本体。`merge` が真なら親が 2 つ以上（`ui` は `○`、他は `●`）。
    Commit { color: ColorId, merge: bool },
}

/// マスの横成分。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Horizontal {
    #[default]
    None,
    Left(ColorId),
    Right(ColorId),
    /// 左右に通り抜ける。
    Both(ColorId),
}

impl Horizontal {
    /// 左右の有無と色から作る。
    fn new(left: bool, right: bool, color: ColorId) -> Self {
        match (left, right) {
            (true, true) => Self::Both(color),
            (true, false) => Self::Left(color),
            (false, true) => Self::Right(color),
            (false, false) => Self::None,
        }
    }

    /// 左のマスへ線が伸びている。`ui` が隣のレーンとの間を `─` で埋めるのに使う。
    pub fn goes_left(self) -> bool {
        matches!(self, Self::Left(_) | Self::Both(_))
    }

    /// 右のマスへ線が伸びている。
    pub fn goes_right(self) -> bool {
        matches!(self, Self::Right(_) | Self::Both(_))
    }

    /// 横線の色。線が無ければ `None`。
    pub fn color(self) -> Option<ColorId> {
        match self {
            Self::None => None,
            Self::Left(color) | Self::Right(color) | Self::Both(color) => Some(color),
        }
    }

    /// このマスに線を 1 本重ねる。
    ///
    /// 1 マスに出せる色は 1 つなので、色は先に引いた線のものを残す。引く順は
    /// 合流線 → `p2..` の順（06 の「色」）。
    fn add(self, left: bool, right: bool, color: ColorId) -> Self {
        Self::new(
            self.goes_left() || left,
            self.goes_right() || right,
            self.color().unwrap_or(color),
        )
    }
}
