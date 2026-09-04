//! mewgit エントリポイント。
//!
//! 隠しサブコマンド `--sequence-editor <path>` / `--commit-editor <path>` は
//! interactive rebase 用（`docs/design/03-git-exec.md`）。

mod modals;
mod panes;
mod ui;

fn main() -> anyhow::Result<()> {
    println!("mewgit: not implemented yet. See docs/plan.md");
    Ok(())
}
