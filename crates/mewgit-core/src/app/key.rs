//! UI ライブラリに依存しないキー入力の表現。設計は `docs/design/01-app-core.md`。
//!
//! crossterm の `KeyEvent` は `mewgit-tui` 側でこの型に変換する。core に crossterm を
//! 入れないため（不変ルール 6）。

/// 1 打鍵。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub mods: Modifiers,
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
    /// 修飾なしのキー。
    pub fn new(code: KeyCode) -> Self {
        Self {
            code,
            mods: Modifiers::default(),
        }
    }

    /// 修飾なしの文字キー。
    pub fn char(c: char) -> Self {
        Self::new(KeyCode::Char(c))
    }

    /// `Ctrl` + 文字。
    pub fn ctrl(c: char) -> Self {
        Self {
            code: KeyCode::Char(c),
            mods: Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
        }
    }

    /// 修飾なしでこの文字が押されたか。`Ctrl-q` を `q` と取り違えないための判定。
    pub fn is_char(&self, c: char) -> bool {
        self.code == KeyCode::Char(c) && !self.mods.ctrl && !self.mods.alt
    }
}
