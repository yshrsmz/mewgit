//! イベントループと [`Cmd`] のディスパッチ。設計は `docs/design/01-app-core.md`。

use std::ops::ControlFlow;

use anyhow::Result;
use crossterm::event::EventStream;
use futures::StreamExt;
use mewgit_core::app::{update, AppState, Cmd, Msg, Notice, NoticeLevel};
use tokio::sync::mpsc;

use crate::keymap::event_to_msg;
use crate::terminal::Tui;
use crate::ui;

/// `Tick` の間隔。進行中ジョブのスピナーと外部変更検知に使う。
const TICK: std::time::Duration = std::time::Duration::from_millis(250);

/// 描画 → 入力待ち → `update` → `dispatch` を回す。
pub async fn run(terminal: &mut Tui, state: &mut AppState) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(TICK);

    loop {
        terminal.draw(|frame| ui::render(frame, state))?;

        let msg = tokio::select! {
            Some(event) = events.next() => match event_to_msg(event?) {
                Some(msg) => msg,
                // 対応する Msg が無いイベント。描画し直す必要も無いが、
                // select! を抜けるので次の周回で draw される。
                None => continue,
            },
            Some(msg) = rx.recv() => msg,
            _ = tick.tick() => Msg::Tick,
        };

        let cmd = update(state, msg);
        if dispatch(state, cmd, &tx).is_break() {
            return Ok(());
        }
    }
}

/// [`Cmd`] を実行する。終了なら [`ControlFlow::Break`]。
///
/// `_tx` は非同期タスクが [`Msg`] を返すためのチャネル。M1 以降で使う。
pub fn dispatch(
    state: &mut AppState,
    cmd: Cmd,
    _tx: &mpsc::UnboundedSender<Msg>,
) -> ControlFlow<()> {
    match cmd {
        Cmd::None => ControlFlow::Continue(()),
        Cmd::Batch(cmds) => {
            for cmd in cmds {
                dispatch(state, cmd, _tx)?;
            }
            ControlFlow::Continue(())
        }
        Cmd::Quit => ControlFlow::Break(()),

        // M1 以降で実装する。黙って捨てず notice に積む。開発中に落ちているのが
        // 見えるようにするため。
        cmd => {
            state.notices.push_back(Notice {
                level: NoticeLevel::Warn,
                message: format!("未実装のコマンドです: {cmd:?}"),
                detail: None,
            });
            ControlFlow::Continue(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mewgit_core::app::RepoInfo;

    fn fixture() -> (AppState, mpsc::UnboundedSender<Msg>) {
        let (tx, _rx) = mpsc::unbounded_channel();
        (AppState::new(RepoInfo::default()), tx)
    }

    #[test]
    fn quit_breaks_the_loop() {
        let (mut state, tx) = fixture();
        assert!(dispatch(&mut state, Cmd::Quit, &tx).is_break());
    }

    #[test]
    fn none_continues() {
        let (mut state, tx) = fixture();
        assert!(dispatch(&mut state, Cmd::None, &tx).is_continue());
        assert!(state.notices.is_empty());
    }

    #[test]
    fn batch_containing_quit_breaks() {
        let (mut state, tx) = fixture();
        let batch = Cmd::Batch(vec![Cmd::LoadStatus, Cmd::Quit]);
        assert!(dispatch(&mut state, batch, &tx).is_break());
    }

    #[test]
    fn batch_stops_at_quit() {
        let (mut state, tx) = fixture();
        // Quit の後ろは実行しない。未実装 notice が 1 件だけであることで確かめる。
        let batch = Cmd::Batch(vec![Cmd::LoadStatus, Cmd::Quit, Cmd::FetchPr]);
        assert!(dispatch(&mut state, batch, &tx).is_break());
        assert_eq!(state.notices.len(), 1);
    }

    #[test]
    fn unimplemented_cmd_warns_instead_of_disappearing() {
        let (mut state, tx) = fixture();
        assert!(dispatch(&mut state, Cmd::LoadStatus, &tx).is_continue());

        assert_eq!(state.notices.len(), 1);
        let notice = &state.notices[0];
        assert_eq!(notice.level, NoticeLevel::Warn);
        assert!(notice.message.contains("LoadStatus"));
    }

    #[test]
    fn nested_batch_is_dispatched() {
        let (mut state, tx) = fixture();
        let nested = Cmd::Batch(vec![Cmd::Batch(vec![Cmd::LoadStatus]), Cmd::FetchPr]);
        assert!(dispatch(&mut state, nested, &tx).is_continue());
        assert_eq!(state.notices.len(), 2);
    }
}
