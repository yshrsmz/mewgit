//! モーダルの描画。`ui::render` が最後に呼び、下のペインを覆って描く。
//!
//! キー処理は `app::update` にある。`ui` は `Cmd` を作らない（不変ルール 2）。

pub mod confirm;

use mewgit_core::app::Modal;
use ratatui::layout::Rect;
use ratatui::Frame;

/// 表示中のモーダルを描く。
///
/// `match` で `_` を使わないのは、`Modal` に変種を足したときに描画漏れを
/// コンパイルエラーにするため（`update` や `Cmd::job_kind` と同じ）。
pub fn render(frame: &mut Frame, area: Rect, modal: &Modal) {
    match modal {
        Modal::Confirm { title, body, .. } => confirm::render(frame, area, title, body),
        // 描画はそれぞれのモーダルのタスクで足す。キー処理も同じタスクで入る。
        Modal::Input { .. }
        | Modal::CommitMessage { .. }
        | Modal::RebaseEditor { .. }
        | Modal::PrCreate { .. }
        | Modal::AccountPicker { .. }
        | Modal::Help => {}
    }
}
