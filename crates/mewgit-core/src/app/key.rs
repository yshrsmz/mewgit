//! UI ライブラリに依存しないキー入力の表現。設計は `docs/design/01-app-core.md`。
//!
//! crossterm の `KeyEvent` は `mewgit-tui` 側でこの型に変換する。core に crossterm を
//! 入れないため（不変ルール 6）。

/// 1 打鍵。
///
/// フィールドは非公開。コンストラクタが唯一の入口で、そこで [`KeyCode::Char`] の
/// `shift` を落として正規化する。crossterm は `P` に `SHIFT` を立てて渡してくるので、
/// 正規化しないと同じ打鍵が 2 通りの `Key` になり、`Eq` / `Hash` が壊れる
/// （`keymap` の引き当てが静かに外れる。08 参照）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    code: KeyCode,
    mods: Modifiers,
}

/// キーの種類。
///
/// `Char` には shift 適用後の文字を入れる（`P` は `Char('P')` であって
/// `Char('p')` + shift ではない）。crossterm の挙動に合わせている。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Char(char),
    Enter,
    Esc,
    Tab,
    /// Shift+Tab。
    BackTab,
    Backspace,
    Delete,
    Insert,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

/// 修飾キー。
///
/// `shift` は `Tab` や矢印など、文字を持たないキーの修飾にだけ使う。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Key {
    /// 修飾付きのキー。`Char` なら `shift` を落として正規化する。
    ///
    /// crossterm からの変換はここを通す。
    pub fn with_mods(code: KeyCode, mods: Modifiers) -> Self {
        let mods = match code {
            // 文字は shift 適用後の値が入っているので、shift は情報を持たない。
            KeyCode::Char(_) => Modifiers {
                shift: false,
                ..mods
            },
            _ => mods,
        };
        Self { code, mods }
    }

    /// 修飾なしのキー。
    pub fn new(code: KeyCode) -> Self {
        Self::with_mods(code, Modifiers::default())
    }

    /// 修飾なしの文字キー。
    pub fn char(c: char) -> Self {
        Self::new(KeyCode::Char(c))
    }

    /// `Ctrl` + 文字。
    pub fn ctrl(c: char) -> Self {
        Self::with_mods(
            KeyCode::Char(c),
            Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
        )
    }

    pub fn code(&self) -> KeyCode {
        self.code
    }

    pub fn mods(&self) -> Modifiers {
        self.mods
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_keys_drop_shift() {
        // crossterm は `P` に SHIFT を立てて渡してくる。正規化しないと
        // `Key::char('P')` と等しくならず、keymap の引き当てが外れる。
        let from_terminal = Key::with_mods(
            KeyCode::Char('P'),
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        );
        assert_eq!(from_terminal, Key::char('P'));
        assert!(!from_terminal.mods().shift);
    }

    #[test]
    fn shift_is_kept_for_non_char_keys() {
        // 矢印や Tab の shift は情報を持つので落とさない。
        let mods = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        let key = Key::with_mods(KeyCode::Left, mods);
        assert!(key.mods().shift);
        assert_ne!(key, Key::new(KeyCode::Left));
    }

    #[test]
    fn ctrl_and_alt_survive_normalization() {
        let key = Key::with_mods(
            KeyCode::Char('d'),
            Modifiers {
                ctrl: true,
                alt: true,
                shift: true,
            },
        );
        assert!(key.mods().ctrl);
        assert!(key.mods().alt);
        assert!(!key.mods().shift);
    }

    #[test]
    fn ctrl_char_differs_from_plain_char() {
        assert_ne!(Key::ctrl('q'), Key::char('q'));
    }
}
