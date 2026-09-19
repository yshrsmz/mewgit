//! UI ライブラリに依存しないキー入力の表現。設計は `docs/design/01-app-core.md`。
//!
//! crossterm の `KeyEvent` は `mewgit-tui` 側でこの型に変換する。core に crossterm を
//! 入れないため（不変ルール 6）。

/// 1 打鍵。
///
/// フィールドは非公開。コンストラクタが唯一の入口で、そこで正規化する
/// （[`Key::with_mods`] 参照）。正規化しないと同じ打鍵が複数の `Key` になり、
/// `Eq` / `Hash` が壊れて `keymap` の引き当てが静かに外れる（08 参照）。
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
    /// Shift+Tab。端末は SHIFT 付きで送ってくるが、`Key` では修飾なしに正規化する。
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
/// `shift` は矢印や `Home` など、文字を持たず専用のキーコードも無いキーの修飾にだけ
/// 残る。`Char` と `Tab` は [`Key::with_mods`] が正規化するので `shift` は立たない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Modifiers {
    /// 修飾なし。
    ///
    /// **`Default` の const 版ではなく、`Default` がこちらを返す。** `const fn` から
    /// `Default::default()` は呼べず（trait のメソッド）、両方に値を書くと「修飾なし」が
    /// 2 箇所になる。
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
    };
}

impl Default for Modifiers {
    fn default() -> Self {
        Self::NONE
    }
}

impl Key {
    /// 修飾付きのキー。同じ打鍵が 1 通りの表現になるよう正規化する。
    ///
    /// crossterm からの変換はここを通す。正規化するのは 2 か所。
    ///
    /// - `Char` は shift 適用後の値が入っているので `shift` を落とす
    /// - Shift+Tab は端末が `BackTab` + SHIFT として送ってくる。`Tab` + shift と
    ///   `BackTab` + shift と `BackTab` の 3 通りができてしまうので、`BackTab`
    ///   （修飾なし）に寄せる
    ///
    /// **`const fn`。** 案内に出すキーの表（`app::keys`。01）を `const` で組むため。
    pub const fn with_mods(code: KeyCode, mods: Modifiers) -> Self {
        let without_shift = Modifiers {
            shift: false,
            ..mods
        };
        let (code, mods) = match code {
            KeyCode::Char(_) => (code, without_shift),
            KeyCode::Tab if mods.shift => (KeyCode::BackTab, without_shift),
            KeyCode::BackTab => (code, without_shift),
            _ => (code, mods),
        };
        Self { code, mods }
    }

    /// 修飾なしのキー。
    pub const fn new(code: KeyCode) -> Self {
        Self::with_mods(code, Modifiers::NONE)
    }

    /// 修飾なしの文字キー。
    pub const fn char(c: char) -> Self {
        Self::new(KeyCode::Char(c))
    }

    /// `Ctrl` + 文字。
    pub const fn ctrl(c: char) -> Self {
        Self::with_mods(
            KeyCode::Char(c),
            Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            },
        )
    }

    pub const fn code(&self) -> KeyCode {
        self.code
    }

    pub const fn mods(&self) -> Modifiers {
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
    fn shift_tab_normalizes_to_back_tab() {
        // 端末は Shift+Tab を BackTab + SHIFT として送ってくる。正規化しないと
        // Key::new(KeyCode::BackTab) と一致せず、ペインの逆順巡回が動かない。
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        let expected = Key::new(KeyCode::BackTab);

        assert_eq!(Key::with_mods(KeyCode::BackTab, shift), expected);
        assert_eq!(Key::with_mods(KeyCode::Tab, shift), expected);
        assert_ne!(Key::new(KeyCode::Tab), expected);
    }

    #[test]
    fn back_tab_keeps_other_modifiers() {
        let key = Key::with_mods(
            KeyCode::BackTab,
            Modifiers {
                ctrl: true,
                shift: true,
                alt: false,
            },
        );
        assert!(key.mods().ctrl);
        assert!(!key.mods().shift);
    }

    #[test]
    fn shift_is_kept_for_non_char_keys() {
        // 矢印の shift は情報を持つので落とさない。
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
