//! `[theme]`（08）の文字列を ratatui のスタイルにする。
//!
//! ペインをまたいで使うのでここに置く。個々のペインで色名を直接書かない（07）。

use ratatui::style::{Color, Modifier, Style};

/// 空白区切りのスタイル指定を読む。
///
/// トークンは色名（`green`、`#ff00aa` など ratatui が読める表記）か修飾
/// （`bold` `dim` `italic` `underlined` `reversed`）。色は最後に書いたものが効く。
/// 読めないトークンは黙って無視する。設定の 1 語のミスで起動を止めない（08）。
pub fn style(spec: &str) -> Style {
    let mut style = Style::default();
    for token in spec.split_whitespace() {
        if let Some(modifier) = modifier(token) {
            style = style.add_modifier(modifier);
        } else if let Some(color) = color(token) {
            style = style.fg(color);
        }
    }
    style
}

/// カーソル行。ペインをまたいで同じ見た目にする。
///
/// ここに置くのは、2 つ目のペイン（worktree）が出た時点で `graph` と同じ式を
/// 書き写すことになったため。3 つ目（branches / stash）で 3 度目を書かない。
/// `[theme]` にキーを足すときの受け口でもある。
pub fn selected() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

/// 見出しなど、本文より弱く出すもの。
pub fn muted() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// 色名 1 つ。読めなければ `None`。
///
/// `[graph] colors` の palette もここを通す。「文字列 → 色」の解釈を 1 箇所に置く。
pub fn color(token: &str) -> Option<Color> {
    token.parse().ok()
}

/// 修飾の名前。`ratatui` の `Modifier` に `FromStr` は無い。
fn modifier(token: &str) -> Option<Modifier> {
    match token {
        "bold" => Some(Modifier::BOLD),
        "dim" => Some(Modifier::DIM),
        "italic" => Some(Modifier::ITALIC),
        "underlined" => Some(Modifier::UNDERLINED),
        "reversed" => Some(Modifier::REVERSED),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_modifier_name_is_not_read_as_a_color() {
        // `graph_head` の既定値は "bold"。色として読むと既定色に落ちて、
        // 強調がまるごと消える。
        let style = style("bold");
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(style.fg, None);
    }

    #[test]
    fn a_color_name_becomes_a_foreground() {
        assert_eq!(style("green").fg, Some(Color::Green));
    }

    #[test]
    fn colors_and_modifiers_can_be_mixed() {
        let style = style("bold green");
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(style.fg, Some(Color::Green));
    }

    #[test]
    fn unknown_tokens_are_ignored() {
        // 設定の 1 語のミスで起動を止めない（08）。
        let style = style("sparkly bold");
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(style.fg, None);
    }

    #[test]
    fn an_empty_spec_is_the_default_style() {
        assert_eq!(style(""), Style::default());
    }
}
