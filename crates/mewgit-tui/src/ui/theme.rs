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

/// フォーカスのあるペインの枠と見出し（07 の「ペインとフォーカス」）。
///
/// **色は ANSI の名前付き色に限る。** 実際の RGB は端末のカラースキームが決めるので、
/// light / dark を切り替えれば追随する。色が出ない端末のために、枠の線の形も変える
/// （`ui::pane_block`）。
pub fn focused() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

/// 右上のタブで、いま出ているほう（07 の「右上のタブ」）。
///
/// `selected` と同じ反転だが、**あちらは一覧のカーソル行で、こちらはタブ**。
/// `[theme]` にキーを足すときに別々に変えられるよう分けておく。
pub fn current_tab() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

/// フッターの 1 行目の、進行中操作のバッジ（07 の「ステータスバー」）。
///
/// **反転に色を足す。** 反転だけだとタブやカーソル行と同じ見た目になり、通知の
/// 並びの中で「いまの状態」だと読めない。黄は `warn` と同じ「止まっているが壊れては
/// いない」の色。
pub fn in_progress() -> Style {
    Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::REVERSED | Modifier::BOLD)
}

/// 見出しなど、本文より弱く出すもの。
pub fn muted() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// diff の追加行。
///
/// `selected` / `muted` と同じで、いまは `[theme]` にキーが無い。**ペインに色名を
/// 直接書かないため**にここへ置く（07）。設定できるようにするときの受け口でもある。
pub fn added() -> Style {
    Style::default().fg(Color::Green)
}

/// diff の削除行。
pub fn removed() -> Style {
    Style::default().fg(Color::Red)
}

/// 効かなくなっている機能の注意書き（ヘッダの degraded なアカウントなど。04 / 07）。
///
/// **エラーではない。** git の操作は動くので `removed` の赤とは分ける。
pub fn warn() -> Style {
    Style::default().fg(Color::Yellow)
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
