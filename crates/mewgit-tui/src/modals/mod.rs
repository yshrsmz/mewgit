//! モーダルの描画。`ui::render` が最後に呼び、下のペインを覆って描く。
//!
//! キー処理は `app::update` にある。`ui` は `Cmd` を作らない（不変ルール 2）。

pub mod commit_message;
pub mod confirm;

use mewgit_core::app::Modal;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::Frame;

/// 表示中のモーダルを描く。
///
/// `match` で `_` を使わないのは、`Modal` に変種を足したときに描画漏れを
/// コンパイルエラーにするため（`update` や `Cmd::job_kind` と同じ）。
pub fn render(frame: &mut Frame, area: Rect, modal: &Modal) {
    match modal {
        Modal::Confirm { title, body, .. } => confirm::render(frame, area, title, body),
        Modal::CommitMessage { buf, amend } => commit_message::render(frame, area, buf, *amend),
        // 描画はそれぞれのモーダルのタスクで足す。キー処理も同じタスクで入る。
        Modal::Input { .. }
        | Modal::RebaseEditor { .. }
        | Modal::PrCreate { .. }
        | Modal::AccountPicker { .. }
        | Modal::Help => {}
    }
}

/// 画面の中央に `width` x `height` を取る。画面が小さければそのぶん縮める。
///
/// 中央寄せの算術は書かず ratatui の [`Flex::Center`] に任せる。`Max` にしているのは、
/// 端末が小さいときに縮めるため（`Length` だとはみ出す）。
///
/// **2 つ目のモーダルが出た時点でここへ移した。** `confirm` と `commit_message` が
/// 同じ中央寄せを持つと、片方だけ直したときに箱の位置がずれる。
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [area] = Layout::horizontal([Constraint::Max(width)])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Max(height)])
        .flex(Flex::Center)
        .areas(area);
    area
}
