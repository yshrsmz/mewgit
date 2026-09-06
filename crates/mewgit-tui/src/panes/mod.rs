//! ペインごとの描画。`ui::render` から呼ばれる。
//!
//! 各ペインは `AppState` を読むだけで、`Cmd` を作らない（不変ルール 2）。

pub mod graph;
pub mod worktree;
