//! 状態遷移。設計は `docs/design/01-app-core.md`。
//!
//! `update` は同期・純粋（`AppState` の変更を除く）。I/O は呼ばず、必要な副作用は
//! [`Cmd`] として返す。
//!
//! 現時点では骨組みだけを実装している。ペインごとのキー処理は M1 以降で、対応する
//! ペインのタスクと一緒に足す。

use super::{AppState, Cmd, Key, KeyCode, Msg};

/// 1 つの [`Msg`] を状態に適用し、実行してほしい副作用を返す。
pub fn update(state: &mut AppState, msg: Msg) -> Cmd {
    match msg {
        Msg::Key(key) => update_key(state, key),
        Msg::Resize(size) => {
            state.term = size;
            Cmd::None
        }
        Msg::Tick => Cmd::None,

        // 以下は対応するマイルストーンのタスクで実装する。ここで `_` を使わないのは、
        // `Msg` に変種を足したときに実装漏れをコンパイルエラーにするため。
        Msg::LogLoaded { .. } => Cmd::None,
        Msg::StatusLoaded(_) => Cmd::None,
        Msg::DiffLoaded { .. } => Cmd::None,
        Msg::ExecFinished { .. } => Cmd::None,
        Msg::ProgressDetected(_) => Cmd::None,
        Msg::AccountResolved(_) => Cmd::None,
        Msg::PrLoaded(_) => Cmd::None,
        Msg::ChecksLoaded(_) => Cmd::None,
        Msg::PrListLoaded(_) => Cmd::None,
    }
}

/// キー入力を `modal` → `focus` の順に振り分ける。
fn update_key(state: &mut AppState, key: Key) -> Cmd {
    // モーダル表示中はすべてモーダルが受ける。個々のモーダルの処理は各マイルストーンで。
    if state.modal.is_some() {
        if key.code == KeyCode::Esc {
            state.modal = None;
        }
        return Cmd::None;
    }

    if key.is_char('q') {
        return Cmd::Quit;
    }

    Cmd::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Modal, Pane, TermSize, TextBuffer};

    #[test]
    fn q_quits() {
        let mut state = AppState::default();
        assert_eq!(update(&mut state, Msg::Key(Key::char('q'))), Cmd::Quit);
    }

    #[test]
    fn ctrl_q_does_not_quit() {
        let mut state = AppState::default();
        // `is_char` は修飾なしのみを見る。Ctrl-q で落ちると事故になる。
        assert_eq!(update(&mut state, Msg::Key(Key::ctrl('q'))), Cmd::None);
    }

    #[test]
    fn unhandled_key_does_nothing() {
        let mut state = AppState::default();
        assert_eq!(update(&mut state, Msg::Key(Key::char('z'))), Cmd::None);
        assert_eq!(state.focus, Pane::Graph);
        assert!(state.modal.is_none());
    }

    #[test]
    fn esc_closes_the_modal() {
        let mut state = AppState {
            modal: Some(Modal::Help),
            ..AppState::default()
        };

        assert_eq!(
            update(&mut state, Msg::Key(Key::new(KeyCode::Esc))),
            Cmd::None
        );
        assert!(state.modal.is_none());
    }

    #[test]
    fn modal_swallows_other_keys() {
        let mut state = AppState {
            modal: Some(Modal::Help),
            ..AppState::default()
        };

        // モーダル表示中の `q` は終了ではない。入力中に閉じられると困る。
        assert_eq!(update(&mut state, Msg::Key(Key::char('q'))), Cmd::None);
        assert!(state.modal.is_some());
    }

    #[test]
    fn resize_updates_the_terminal_size() {
        let mut state = AppState::default();
        let size = TermSize {
            width: 120,
            height: 40,
        };

        assert_eq!(update(&mut state, Msg::Resize(size)), Cmd::None);
        assert_eq!(state.term, size);
    }

    #[test]
    fn tick_does_nothing_yet() {
        let mut state = AppState::default();
        assert_eq!(update(&mut state, Msg::Tick), Cmd::None);
    }

    #[test]
    fn text_buffer_round_trips() {
        let buf = TextBuffer::from_text("first\nsecond");
        assert_eq!(buf.lines, ["first", "second"]);
        assert_eq!(buf.cursor, (1, 6));
        assert_eq!(buf.text(), "first\nsecond");
    }

    #[test]
    fn default_text_buffer_has_one_empty_line() {
        // cursor が指す行が必ず存在する必要がある。
        let buf = TextBuffer::default();
        assert_eq!(buf.lines, [""]);
        assert_eq!(buf.cursor, (0, 0));
        assert_eq!(buf.text(), "");
    }

    #[test]
    fn batch_flattens_empty_and_single() {
        assert_eq!(Cmd::batch([]), Cmd::None);
        assert_eq!(Cmd::batch([Cmd::None, Cmd::None]), Cmd::None);
        assert_eq!(Cmd::batch([Cmd::LoadStatus]), Cmd::LoadStatus);
        assert_eq!(
            Cmd::batch([Cmd::LoadStatus, Cmd::None, Cmd::Quit]),
            Cmd::Batch(vec![Cmd::LoadStatus, Cmd::Quit])
        );
    }
}
