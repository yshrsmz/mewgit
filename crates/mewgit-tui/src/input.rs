//! crossterm のイベントを core の [`Msg`] に変換する。
//!
//! `Msg` は `mewgit-core` の型なので `From<crossterm::event::Event>` は生やせない
//! （孤児ルール、かつ不変ルール 6）。変換はこちら側の自由関数で行う。
//!
//! ファイル名を `keymap` にしないのは、`config.keymap`（キーとアクションの対応表、
//! 08 と M6）と紛らわしいため。ここが扱うのは端末イベントの変換だけ。
//!
//! crossterm は ratatui の再エクスポート経由で使う。ratatui との間で `Event` や
//! `KeyEvent` が別クレートの型に割れないようにするため。

use mewgit_core::app::{Key, KeyCode, Modifiers, Msg, TermSize};
use ratatui::crossterm::event::{
    Event, KeyCode as CtKeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};

/// 端末イベントを [`Msg`] にする。対応する `Msg` が無いイベントは `None`。
///
/// マウス・フォーカス・ペーストは今のところ使わないので落とす。
pub fn event_to_msg(event: Event) -> Option<Msg> {
    match event {
        Event::Key(key) => key_event_to_key(key).map(Msg::Key),
        Event::Resize(width, height) => Some(resized(TermSize { width, height })),
        Event::FocusGained | Event::FocusLost | Event::Mouse(_) | Event::Paste(_) => None,
    }
}

/// サイズ変更の [`Msg`]。グラフに使える行数はレイアウトから出して一緒に運ぶ。
///
/// core がヘッダや枠の高さを知っていると、07 のレイアウトを変えたときに黙ってずれる
/// （01 の「グラフの行」）。起動時は `Resize` が飛んでこないので、ランタイムが
/// 最初のサイズでこれを 1 回作る。
pub fn resized(term: TermSize) -> Msg {
    Msg::Resize {
        term,
        graph_rows: crate::ui::graph_rows(term),
    }
}

/// crossterm の [`KeyEvent`] を core の [`Key`] にする。
///
/// 押下だけを通す。kitty プロトコルが有効な端末では離鍵も飛んでくるので、
/// フィルタしないと 1 回の打鍵が 2 つの [`Msg::Key`] になる。
fn key_event_to_key(event: KeyEvent) -> Option<Key> {
    if event.kind != KeyEventKind::Press {
        return None;
    }

    let code = match event.code {
        CtKeyCode::Char(c) => KeyCode::Char(c),
        CtKeyCode::Enter => KeyCode::Enter,
        CtKeyCode::Esc => KeyCode::Esc,
        CtKeyCode::Tab => KeyCode::Tab,
        CtKeyCode::BackTab => KeyCode::BackTab,
        CtKeyCode::Backspace => KeyCode::Backspace,
        CtKeyCode::Delete => KeyCode::Delete,
        CtKeyCode::Insert => KeyCode::Insert,
        CtKeyCode::Left => KeyCode::Left,
        CtKeyCode::Right => KeyCode::Right,
        CtKeyCode::Up => KeyCode::Up,
        CtKeyCode::Down => KeyCode::Down,
        CtKeyCode::Home => KeyCode::Home,
        CtKeyCode::End => KeyCode::End,
        CtKeyCode::PageUp => KeyCode::PageUp,
        CtKeyCode::PageDown => KeyCode::PageDown,
        CtKeyCode::F(n) => KeyCode::F(n),
        // Null / CapsLock / Menu / メディアキーなど、キーマップに載せないものは捨てる。
        _ => return None,
    };

    let mods = Modifiers {
        ctrl: event.modifiers.contains(KeyModifiers::CONTROL),
        alt: event.modifiers.contains(KeyModifiers::ALT),
        shift: event.modifiers.contains(KeyModifiers::SHIFT),
    };

    // 正規化は Key::with_mods が行う（Char の shift 落とし、Shift+Tab → BackTab）。
    Some(Key::with_mods(code, mods))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: CtKeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn plain_char_becomes_a_char_key() {
        let key = key_event_to_key(press(CtKeyCode::Char('q'), KeyModifiers::NONE));
        assert_eq!(key, Some(Key::char('q')));
    }

    #[test]
    fn shifted_char_keeps_the_uppercase_letter_and_drops_shift() {
        // 端末は `P` を Char('P') + SHIFT で送ってくる。
        let key = key_event_to_key(press(CtKeyCode::Char('P'), KeyModifiers::SHIFT));
        assert_eq!(key, Some(Key::char('P')));
    }

    #[test]
    fn ctrl_char_keeps_ctrl() {
        let key = key_event_to_key(press(CtKeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(key, Some(Key::ctrl('c')));
    }

    #[test]
    fn back_tab_is_normalized_to_an_unmodified_key() {
        // crossterm は Shift+Tab を BackTab + SHIFT として送る。
        let key = key_event_to_key(press(CtKeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(key, Some(Key::new(KeyCode::BackTab)));
    }

    #[test]
    fn arrow_keeps_shift() {
        let key = key_event_to_key(press(CtKeyCode::Left, KeyModifiers::SHIFT));
        let mods = key.unwrap().mods();
        assert!(mods.shift);
    }

    #[test]
    fn function_key_is_mapped() {
        let key = key_event_to_key(press(CtKeyCode::F(5), KeyModifiers::NONE));
        assert_eq!(key, Some(Key::new(KeyCode::F(5))));
    }

    #[test]
    fn key_release_is_dropped() {
        // kitty プロトコルが有効な端末では離鍵も飛ぶ。通すと 1 打鍵が 2 回になる。
        let release = KeyEvent::new_with_kind(
            CtKeyCode::Char('q'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert_eq!(key_event_to_key(release), None);
    }

    #[test]
    fn unmapped_key_code_is_dropped() {
        assert_eq!(
            key_event_to_key(press(CtKeyCode::CapsLock, KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn resize_event_carries_the_new_size_and_the_graph_rows() {
        // グラフに使える行数はレイアウトから出す。core はヘッダや枠の高さを知らない。
        let term = TermSize {
            width: 120,
            height: 40,
        };
        let msg = event_to_msg(Event::Resize(term.width, term.height));

        let Some(Msg::Resize {
            term: got,
            graph_rows,
        }) = msg
        else {
            panic!("Resize になっていません: {msg:?}");
        };
        assert_eq!(got, term);
        assert_eq!(graph_rows, crate::ui::graph_rows(term));
    }

    #[test]
    fn events_without_a_msg_are_dropped() {
        assert!(event_to_msg(Event::FocusGained).is_none());
        assert!(event_to_msg(Event::FocusLost).is_none());
        assert!(event_to_msg(Event::Paste("text".to_string())).is_none());
    }
}
