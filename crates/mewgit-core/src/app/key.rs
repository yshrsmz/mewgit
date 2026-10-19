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

/// 案内に書くキーの字（`Ctrl-D`、`Space`、`Shift+Tab`）。
///
/// **core に置くのは、通知の文言もキーを書くから**（08 の「文言に書いたキーの字」）。
/// フッター・`?` の一覧・通知が同じ字を出す。**`Key` 全体を見る**（`KeyCode` だけでは
/// `Ctrl-C` と `c` が同じ字になる）。**どの `KeyCode` も綴る** —— `config.keymap` の上書きで
/// どのキーも表に入りうる。
impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mods = self.mods;
        if mods.ctrl {
            f.write_str("Ctrl-")?;
        }
        if mods.alt {
            f.write_str("Alt-")?;
        }
        // `Char` と `Tab` には立たない（`with_mods` が落とす）。
        if mods.shift {
            f.write_str("Shift-")?;
        }
        match self.code {
            // 空白のままだと案内の区切りと見分けられない。**修飾より先に見る**（`Ctrl-Space`
            // が `Ctrl- ` にならないように）。
            KeyCode::Char(' ') => f.write_str("Space"),
            // **`Ctrl` と組む文字は大文字で書く**（`Ctrl-C`）。端末の慣習で、小文字だと
            // `c` のキーと見分けにくい。
            KeyCode::Char(c) if mods.ctrl => write!(f, "{}", c.to_ascii_uppercase()),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::Enter => f.write_str("Enter"),
            KeyCode::Esc => f.write_str("Esc"),
            KeyCode::Tab => f.write_str("Tab"),
            KeyCode::BackTab => f.write_str("Shift+Tab"),
            KeyCode::Backspace => f.write_str("Backspace"),
            KeyCode::Delete => f.write_str("Delete"),
            KeyCode::Insert => f.write_str("Insert"),
            KeyCode::Left => f.write_str("Left"),
            KeyCode::Right => f.write_str("Right"),
            KeyCode::Up => f.write_str("Up"),
            KeyCode::Down => f.write_str("Down"),
            KeyCode::Home => f.write_str("Home"),
            KeyCode::End => f.write_str("End"),
            KeyCode::PageUp => f.write_str("PageUp"),
            KeyCode::PageDown => f.write_str("PageDown"),
            KeyCode::F(n) => write!(f, "F{n}"),
        }
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
            // CSI u を話さない端末では、`Tab` / `Enter` / `Esc` と同じバイト（0x09 / 0x0D /
            // 0x1B）で届き、crossterm はそちらのキーとして返す（`parse_event`）。
            if mods.ctrl && matches!(c, 'i' | 'm' | '[') {
                return Err(ParseKeyError::CtrlSameAsKey);
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
    #[error("ctrl-i / ctrl-m / ctrl-[ は端末によって Tab / Enter / Esc として届きます")]
    CtrlSameAsKey,
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
    fn every_key_is_spelled_with_its_modifiers() {
        // **どの `KeyCode` も綴る**（`config.keymap` の上書きでどのキーも表に入りうる）。
        // 以前の `ui::push_key` は表の既定のキーしか綴らず、ほかは `Debug` の字（`F(5)`）に、
        // 矢印の shift は落としていた。
        let with = |code, ctrl, alt, shift| Key::with_mods(code, Modifiers { ctrl, alt, shift });
        let cases = [
            (Key::char('j'), "j"),
            (Key::char('P'), "P"),
            (Key::char('?'), "?"),
            (Key::char(' '), "Space"),
            (Key::ctrl('c'), "Ctrl-C"),
            (with(KeyCode::Char('x'), false, true, false), "Alt-x"),
            (Key::new(KeyCode::Enter), "Enter"),
            (Key::new(KeyCode::Esc), "Esc"),
            (Key::new(KeyCode::Tab), "Tab"),
            (Key::new(KeyCode::BackTab), "Shift+Tab"),
            (Key::new(KeyCode::Backspace), "Backspace"),
            (Key::new(KeyCode::Delete), "Delete"),
            (Key::new(KeyCode::Insert), "Insert"),
            (Key::new(KeyCode::Left), "Left"),
            (Key::new(KeyCode::Right), "Right"),
            (Key::new(KeyCode::Up), "Up"),
            (Key::new(KeyCode::Down), "Down"),
            (Key::new(KeyCode::Home), "Home"),
            (Key::new(KeyCode::End), "End"),
            (Key::new(KeyCode::PageUp), "PageUp"),
            (Key::new(KeyCode::PageDown), "PageDown"),
            (Key::new(KeyCode::F(5)), "F5"),
            (with(KeyCode::Up, false, false, true), "Shift-Up"),
            (
                with(KeyCode::PageDown, true, true, false),
                "Ctrl-Alt-PageDown",
            ),
        ];
        for (key, text) in cases {
            assert_eq!(key.to_string(), text, "{key:?}");
        }
        // **書いた値がそのまま読める字になる**（`config.keymap` の値と案内の字が対応する）。
        assert_eq!(Key::parse("shift-up").unwrap().to_string(), "Shift-Up");
        assert_eq!(Key::parse("ctrl-space").unwrap().to_string(), "Ctrl-Space");
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
        for value in ["ctrl-i", "ctrl-m", "ctrl-["] {
            assert_eq!(
                Key::parse(value),
                Err(ParseKeyError::CtrlSameAsKey),
                "{value}"
            );
        }
        // `ctrl-j` は raw mode では `Enter` に化けない（crossterm の Issue #371 の扱い）。
        assert_eq!(Key::parse("ctrl-j"), Ok(Key::ctrl('j')));
        // 大文字が断られるのは ctrl だけ（`alt-P` は端末が `P` を送る形がある）。
        assert!(Key::parse("alt-P").is_ok());
    }
}
