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

    pub fn code(&self) -> KeyCode {
        self.code
    }

    pub fn mods(&self) -> Modifiers {
        self.mods
    }

    /// `config.keymap` の値（`"ctrl-d"`, `"shift-tab"`, `"P"`）を解く。書き方は 08 の
    /// 「キーマップの解決」。
    ///
    /// 修飾は `ctrl-` / `alt-` / `shift-` を前に重ねる（大文字小文字を問わない）。残りは
    /// 名前（`enter` / `space` / `f5` など）か 1 文字。**1 文字は書いたまま** —— `P` は
    /// `Char('P')` で、ここで小文字に寄せると `p` と区別できない。
    ///
    /// **押しても届かない形は断る。** 黙って正規化すると、書いたキーと効くキーが割れる。
    pub fn parse(text: &str) -> Result<Self, ParseKeyError> {
        let mut mods = Modifiers::NONE;
        let mut rest = text;
        // 末尾の `-` はキーそのもの（`"-"` や `"ctrl--"`）なので、`-` の後ろが空なら止める。
        while let Some((head, tail)) = rest.split_once('-').filter(|(_, tail)| !tail.is_empty()) {
            match head.to_ascii_lowercase().as_str() {
                "ctrl" => mods.ctrl = true,
                "alt" => mods.alt = true,
                "shift" => mods.shift = true,
                _ => break,
            }
            rest = tail;
        }
        let code = match rest.to_ascii_lowercase().as_str() {
            "enter" => KeyCode::Enter,
            "esc" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "backspace" => KeyCode::Backspace,
            "delete" => KeyCode::Delete,
            "insert" => KeyCode::Insert,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            // 空白は字で書くと見えないので名前を持たせる（Graph の `Space`）。
            "space" => KeyCode::Char(' '),
            name => match function_key(name) {
                Some(n) => KeyCode::F(n),
                None => {
                    let mut chars = rest.chars();
                    match (chars.next(), chars.next()) {
                        (Some(c), None) => KeyCode::Char(c),
                        _ => return Err(ParseKeyError::Unknown),
                    }
                }
            },
        };
        if let KeyCode::Char(c) = code {
            // `with_mods` が落とすので、`shift-a` は `a` として効く。
            if mods.shift {
                return Err(ParseKeyError::ShiftOnChar);
            }
            // 端末は `Ctrl` + 英字を小文字で送る（crossterm 0.29 の `parse_event` は
            // 0x01〜0x1A を小文字に、CSI u は shift を外した字で返す）。
            if mods.ctrl && c.is_ascii_uppercase() {
                return Err(ParseKeyError::CtrlUppercase);
            }
        }
        Ok(Self::with_mods(code, mods))
    }
}

/// `f1` などの番号。`f` 1 文字は文字キーなので `None`。
fn function_key(name: &str) -> Option<u8> {
    name.strip_prefix('f')?.parse().ok().filter(|n| *n > 0)
}

/// [`Key::parse`] が値を断った理由。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseKeyError {
    #[error("キーとして読めません")]
    Unknown,
    #[error("文字キーに shift- は付けられません（大文字で書いてください）")]
    ShiftOnChar,
    #[error("ctrl- の後ろの英字は小文字で書いてください（端末は大文字を送りません）")]
    CtrlUppercase,
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn shift_is_dropped_but_ctrl_and_alt_survive() {
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

        // 落とすのは shift だけで、ctrl / alt は残す。
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

    #[test]
    fn parse_reads_modifiers_names_and_single_chars() {
        let alt = |code| {
            Key::with_mods(
                code,
                Modifiers {
                    alt: true,
                    ..Modifiers::NONE
                },
            )
        };
        assert_eq!(Key::parse("P"), Ok(Key::char('P')));
        assert_eq!(Key::parse("p"), Ok(Key::char('p')));
        assert_eq!(Key::parse("ctrl-d"), Ok(Key::ctrl('d')));
        assert_eq!(Key::parse("Ctrl-d"), Ok(Key::ctrl('d')));
        assert_eq!(Key::parse("alt-enter"), Ok(alt(KeyCode::Enter)));
        // 正規化は `with_mods` に任せる（Shift+Tab は `BackTab`）。
        assert_eq!(Key::parse("shift-tab"), Ok(Key::new(KeyCode::BackTab)));
        assert_eq!(Key::parse("backtab"), Ok(Key::new(KeyCode::BackTab)));
        assert_eq!(Key::parse("Enter"), Ok(Key::new(KeyCode::Enter)));
        assert_eq!(Key::parse("space"), Ok(Key::char(' ')));
        assert_eq!(Key::parse("f5"), Ok(Key::new(KeyCode::F(5))));
        // `f` 1 文字は F キーではない。
        assert_eq!(Key::parse("f"), Ok(Key::char('f')));
        // `-` はキーそのものにもなる。
        assert_eq!(Key::parse("-"), Ok(Key::char('-')));
        assert_eq!(Key::parse("ctrl--"), Ok(Key::ctrl('-')));
        // 矢印の shift は残る（`with_mods` の規則）。
        assert_eq!(
            Key::parse("shift-down"),
            Ok(Key::with_mods(
                KeyCode::Down,
                Modifiers {
                    shift: true,
                    ..Modifiers::NONE
                }
            ))
        );
    }

    #[test]
    fn parse_refuses_what_cannot_be_typed() {
        assert_eq!(Key::parse(""), Err(ParseKeyError::Unknown));
        assert_eq!(Key::parse("ab"), Err(ParseKeyError::Unknown));
        assert_eq!(Key::parse("hyper-a"), Err(ParseKeyError::Unknown));
        assert_eq!(Key::parse("f0"), Err(ParseKeyError::Unknown));
        assert_eq!(Key::parse("ctrl-"), Err(ParseKeyError::Unknown));
        assert_eq!(Key::parse("shift-a"), Err(ParseKeyError::ShiftOnChar));
        assert_eq!(Key::parse("ctrl-D"), Err(ParseKeyError::CtrlUppercase));
        // 大文字が断られるのは ctrl だけ（`alt-P` は端末が `P` を送る形がある）。
        assert!(Key::parse("alt-P").is_ok());
    }
}
