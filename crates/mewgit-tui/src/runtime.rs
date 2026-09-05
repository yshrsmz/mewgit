//! イベントループと [`Cmd`] のディスパッチ。設計は `docs/design/01-app-core.md`。

use std::ops::ControlFlow;
use std::time::Duration;

use anyhow::Result;
use futures::{Stream, StreamExt};
use mewgit_core::app::{update, AppState, Cmd, Msg, NoticeLevel};
use ratatui::backend::Backend;
use ratatui::crossterm::event::Event;
use ratatui::Terminal;

use crate::input::event_to_msg;
use crate::ui;

/// `Tick` の間隔。進行中ジョブのスピナーと外部変更検知に使う。
const TICK: Duration = Duration::from_millis(250);

/// 描画 → 入力待ち → `update` → `dispatch` を回す。
///
/// `Backend` とイベントストリームをジェネリックに取るのは、`TestBackend` と
/// `futures::stream::iter` を渡してループ全体をテストできるようにするため。
///
/// 非同期タスクが [`Msg`] を返す mpsc チャネルは、最初の非同期 [`Cmd`] を実装する
/// M1 で `select!` に足す。今は送り手がいない。
pub async fn run<B, S>(terminal: &mut Terminal<B>, events: S, state: &mut AppState) -> Result<()>
where
    B: Backend,
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let mut events = events;
    let mut tick = tokio::time::interval(TICK);

    loop {
        terminal.draw(|frame| ui::render(frame, state))?;

        let msg = tokio::select! {
            event = events.next() => match event {
                Some(event) => match event_to_msg(event?) {
                    Some(msg) => msg,
                    // 対応する Msg が無いイベントは捨てる。
                    None => continue,
                },
                // 入力が尽きた（端末が閉じた、テストのストリームが終わった）。
                // ここで抜けないと Tick だけで永久に回り続ける。
                None => return Ok(()),
            },
            _ = tick.tick() => Msg::Tick,
        };

        let cmd = update(state, msg);
        if dispatch(state, cmd).is_break() {
            return Ok(());
        }
    }
}

/// [`Cmd`] を実行する。終了なら [`ControlFlow::Break`]。
///
/// `AppState` に書いてよいのは `notices` だけ。それ以外の状態変化は [`Msg`] にして
/// `update` に戻す。状態遷移の入口を `update` 1 つに保つため。
///
/// `match` で `_` を使わないのは、`Cmd` に変種を足したときに実装漏れをコンパイル
/// エラーにするため（`update` や `Cmd::job_kind` と同じ）。
pub fn dispatch(state: &mut AppState, cmd: Cmd) -> ControlFlow<()> {
    match cmd {
        Cmd::None => ControlFlow::Continue(()),
        Cmd::Batch(cmds) => {
            for cmd in cmds {
                dispatch(state, cmd)?;
            }
            ControlFlow::Continue(())
        }
        Cmd::Quit => ControlFlow::Break(()),

        // M1 以降で実装する。
        Cmd::LoadLog { .. } => not_implemented(state, "LoadLog"),
        Cmd::LoadStatus => not_implemented(state, "LoadStatus"),
        Cmd::LoadDiff(_) => not_implemented(state, "LoadDiff"),
        Cmd::Exec(_) => not_implemented(state, "Exec"),
        Cmd::ResolveAccount => not_implemented(state, "ResolveAccount"),
        Cmd::FetchPr => not_implemented(state, "FetchPr"),
        Cmd::FetchChecks => not_implemented(state, "FetchChecks"),
        Cmd::FetchPrList => not_implemented(state, "FetchPrList"),
    }
}

/// 未実装の `Cmd` を通知に積む。黙って捨てると開発中に気付けない。
fn not_implemented(state: &mut AppState, name: &str) -> ControlFlow<()> {
    state.push_notice(
        NoticeLevel::Warn,
        format!("未実装のコマンドです: {name}"),
        None,
    );
    ControlFlow::Continue(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use mewgit_core::app::{RepoInfo, TermSize};
    use mewgit_core::config::Config;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn state() -> AppState {
        AppState::new(RepoInfo::default(), Config::default())
    }

    fn key(code: KeyCode) -> std::io::Result<Event> {
        Ok(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    /// 与えたイベントを順に流してループを 1 本走らせる。
    async fn run_with(events: Vec<std::io::Result<Event>>, state: &mut AppState) -> Result<()> {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        run(&mut terminal, stream::iter(events), state).await
    }

    #[tokio::test]
    async fn q_ends_the_loop() {
        let mut state = state();
        run_with(vec![key(KeyCode::Char('q'))], &mut state)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn ctrl_c_ends_the_loop() {
        let mut state = state();
        let ctrl_c = Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        run_with(vec![ctrl_c], &mut state).await.unwrap();
    }

    #[tokio::test]
    async fn resize_reaches_the_state_before_quitting() {
        let mut state = state();
        let events = vec![Ok(Event::Resize(120, 40)), key(KeyCode::Char('q'))];

        run_with(events, &mut state).await.unwrap();
        assert_eq!(
            state.term,
            TermSize {
                width: 120,
                height: 40
            }
        );
    }

    #[tokio::test]
    async fn unmapped_events_are_skipped_without_ending_the_loop() {
        let mut state = state();
        let events = vec![
            Ok(Event::FocusGained),
            key(KeyCode::CapsLock),
            key(KeyCode::Char('q')),
        ];

        run_with(events, &mut state).await.unwrap();
    }

    #[tokio::test]
    async fn exhausted_event_stream_ends_the_loop() {
        // 終了キーが来なくても、入力が尽きたら抜ける。抜けないと Tick だけで
        // 永久に回り、テストがハングする。
        let mut state = state();
        run_with(vec![Ok(Event::Resize(100, 30))], &mut state)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn stream_errors_are_not_swallowed() {
        let mut state = state();
        let err = Err(std::io::Error::other("端末の読み取りに失敗"));

        let result = run_with(vec![err], &mut state).await;
        assert!(result.is_err());
    }

    #[test]
    fn quit_breaks_the_loop() {
        let mut state = state();
        assert!(dispatch(&mut state, Cmd::Quit).is_break());
    }

    #[test]
    fn none_continues() {
        let mut state = state();
        assert!(dispatch(&mut state, Cmd::None).is_continue());
        assert!(state.notices.is_empty());
    }

    #[test]
    fn batch_stops_at_quit() {
        let mut state = state();
        // Quit の後ろは実行しない。未実装 notice が 1 件だけであることで確かめる。
        let batch = Cmd::Batch(vec![Cmd::LoadStatus, Cmd::Quit, Cmd::FetchPr]);
        assert!(dispatch(&mut state, batch).is_break());
        assert_eq!(state.notices.len(), 1);
    }

    #[test]
    fn unimplemented_cmd_warns_instead_of_disappearing() {
        let mut state = state();
        assert!(dispatch(&mut state, Cmd::LoadStatus).is_continue());

        assert_eq!(state.notices.len(), 1);
        let notice = &state.notices[0];
        assert_eq!(notice.level, NoticeLevel::Warn);
        assert!(notice.message.contains("LoadStatus"));
    }

    #[test]
    fn nested_batch_is_dispatched() {
        let mut state = state();
        let nested = Cmd::Batch(vec![Cmd::Batch(vec![Cmd::LoadStatus]), Cmd::FetchPr]);
        assert!(dispatch(&mut state, nested).is_continue());
        assert_eq!(state.notices.len(), 2);
    }
}
