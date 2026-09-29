//! 案内に出すキーとその説明の表。設計は 01 の「案内に出すキーの表」。
//!
//! **キーの一覧はここ 1 本。** 読むのはステータスバーの 2 行目（07 の「ステータスバー」）と
//! `Modal::Help`（07 の「Help モーダル」）で、あとから入る `config.keymap` の上書き（08）も
//! ここを読む。2 本に分けると、上書きしたときにヒントだけが既定の字を出す。
//!
//! **表示の字は持たない。** `j/k` や `Enter` という並びは `ui` が [`KeyCode`] から
//! 組む（導出できる値を離れた場所に置かない。`.claude/rules/rust-coding.md`）。
//!
//! **モーダル自身のキーの案内は入っていない** —— あちらは箱の中に自前の `HINT` を
//! 持っている（`modals::*`。予算が違うので表に寄せない。07 の「ステータスバー」）。
//!
//! **まだ持っていないのはアクションの同一性。** キーと表示の文言しか無いので、この表は
//! `update_key` の `match` の手書きの写しで、見張れるのは「表 → dispatch」の片側だけ
//! （`every_hinted_key_does_something`）。`config.keymap`（08）はアクション名で書くので
//! **`label` は鍵にならない** —— 足すのはそのタスク（`docs/plan.md` に何が要るか書いた）。

use super::{AppState, Key, KeyCode, Pane};

/// 案内 1 つ —— 押すキーと、押すと何が起きるか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyHint {
    keys: &'static [Key],
    label: &'static str,
}

impl KeyHint {
    /// 案内に出すキー。同じ動作をするものが複数あれば全部（`j` と `k`）。
    ///
    /// **別名は入れていない** —— `j` の代わりに効く `Down` は表に無い。ここは案内に
    /// 出すものの一覧で、出さないものを足しても案内は変わらない（01）。
    ///
    /// **返すのは `&'static` ではなく借用。** `config.keymap` の上書き（08）で差し替えた
    /// キーは `'static` では持てないので、いま `'static` を約束すると、そのタスクで
    /// **呼び手ごと書き換えることになる**。
    pub fn keys(&self) -> &[Key] {
        self.keys
    }

    /// 押すと何が起きるか。
    ///
    /// **git / `gh` の語はそのまま、画面の操作は日本語**（`s:stage` と `j/k:移動`）。
    /// 例外は `x:破棄` で、`discard` にすると Worktree のヒントが 80 桁に収まらない
    /// （07 の実測）。
    pub fn label(&self) -> &'static str {
        self.label
    }
}

/// 表に並べるときの短縮。
const fn hint(keys: &'static [Key], label: &'static str) -> KeyHint {
    KeyHint { keys, label }
}

/// 終了。**フッターに必ず出し、幅が足りなくても落とさない**（07）。
pub const QUIT: KeyHint = hint(&[Key::char('q')], "quit");

/// キーの一覧（`Modal::Help`）。**[`QUIT`] と同じく必ず出す** —— フッターから落ちた
/// キーと共通キーを辿る入口はここだけ（07）。
pub const HELP: KeyHint = hint(&[Key::char('?')], "help");

/// どのペインでも効くキー（`update_key` がペインの `match` より先に受ける）。
///
/// **フッターには出さず、`Modal::Help` だけが読む**（2026-09-30 にユーザーと決めた。
/// 07 の「ステータスバー」）。フッターに出していた頃は、ペインにキーを足すたびに
/// ここが 1 つずつ見えなくなった。
pub const COMMON: &[KeyHint] = &[
    hint(&[Key::char('f')], "fetch"),
    hint(&[Key::char('p')], "pull"),
    hint(&[Key::char('P')], "push"),
    hint(&[Key::char('A')], "account"),
    hint(&[Key::char('L')], "通知"),
    hint(&[Key::char('m')], "進行中操作のメニュー"),
    hint(
        &[Key::new(KeyCode::Tab), Key::new(KeyCode::BackTab)],
        "ペイン移動",
    ),
    HELP,
    // git が走っていれば中断、そうでなければ終了（`update_key` の先頭）。
    hint(&[Key::ctrl('c')], "中断 / 終了"),
    QUIT,
];

/// 上下移動。**ペインをまたいで同じ 1 つを使う**（説明が割れないように）。
const MOVE: KeyHint = hint(&[Key::char('j'), Key::char('k')], "移動");

/// 左のグラフへ戻る（Diff だけは `Enter` で来た側 —— Working tree かコミット詳細）。
///
/// **「1 つ前のペイン」ではない。** `Esc` の行き先は各ペインが決めていて、右列のペインは
/// どれも無条件に `Pane::Graph` を指す（巡回でどこから来ても同じ）。ラベルを「戻る」に
/// してあるのは、どちらでも嘘にならない語だから。
const BACK: KeyHint = hint(&[Key::new(KeyCode::Esc)], "戻る");

const GRAPH: &[KeyHint] = &[
    MOVE,
    // 行き先は選択で変わる（Working tree かコミット詳細）ので、どちらでも嘘に
    // ならない語にする。
    hint(&[Key::new(KeyCode::Enter)], "右へ"),
    hint(&[Key::char('n')], "ブランチ作成"),
    hint(&[Key::char('y')], "cherry-pick"),
    hint(&[Key::char('v')], "revert"),
];

const BRANCHES: &[KeyHint] = &[
    MOVE,
    hint(&[Key::new(KeyCode::Enter)], "切替"),
    hint(&[Key::char('n')], "作成"),
    hint(&[Key::char('r')], "改名"),
    hint(&[Key::char('d')], "削除"),
    hint(&[Key::char('D')], "強制削除"),
    BACK,
];

const PRS: &[KeyHint] = &[
    MOVE,
    // **Branches の `Enter` と同じ語。** どちらも「そのブランチに居る状態にする」で、
    // PR のほうは無ければ作り、在れば早送りする（05 の「PR チェックアウト」）。
    hint(&[Key::new(KeyCode::Enter)], "切替"),
    hint(&[Key::char('o')], "ブラウザ"),
    // **Branches の `r`（改名）とは別のキー。** キーマップはペインごとに割れて
    // いるので衝突しない（`.claude/rules/rust-coding.md` の「『押せる』は画面ごとに数える」）。
    hint(&[Key::char('r')], "更新"),
    hint(&[Key::char('N')], "PR 作成"),
    BACK,
];

const WORKTREE: &[KeyHint] = &[
    MOVE,
    hint(&[Key::new(KeyCode::Enter)], "差分"),
    hint(&[Key::char('s')], "stage"),
    hint(&[Key::char('u')], "unstage"),
    hint(&[Key::char('x')], "破棄"),
    hint(&[Key::char('c')], "commit"),
    hint(&[Key::char('C')], "amend"),
    BACK,
];

const COMMIT_DETAIL: &[KeyHint] = &[MOVE, hint(&[Key::new(KeyCode::Enter)], "差分"), BACK];

const DIFF: &[KeyHint] = &[
    MOVE,
    hint(&[Key::char('s')], "stage"),
    hint(&[Key::char('u')], "unstage"),
    hint(&[Key::char('x')], "破棄"),
    BACK,
];

/// [`help`] の 1 行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpRow {
    /// 段の見出し（「このペイン」「どこでも」）。
    Heading(&'static str),
    Key(&'static KeyHint),
}

/// `Modal::Help` の行。**そのペインのキー → どこでも効くキー**の 2 段（07 の
/// 「Help モーダル」）。ほかのペインのキーは出さない —— 押せるのは今いるペインのものだけ。
///
/// **キーの無いペインでは見出しごと出さない**（空の段を見せない）。
pub fn help(pane: Pane) -> Vec<HelpRow> {
    let here = self::pane(pane);
    let mut rows = Vec::with_capacity(here.len() + COMMON.len() + 2);
    if !here.is_empty() {
        rows.push(HelpRow::Heading("このペイン"));
        rows.extend(here.iter().map(HelpRow::Key));
    }
    rows.push(HelpRow::Heading("どこでも"));
    rows.extend(COMMON.iter().map(HelpRow::Key));
    rows
}

/// **いま**押せるペインのキー。`None` は「この表のどれも押せない」。
///
/// **モーダル表示中が `None`。** そのあいだキーは全部モーダルが受けるので
/// （`update_key`）、ペインのキーも [`COMMON`] も [`QUIT`] も効かない。
/// **`ui` に `modal.is_some()` を書かせない**ため、ここが答える —— あちらに書くと
/// 振り分けの規則の写しが 2 本になる。**モーダル自身のキーの案内は各モーダルが
/// 持っている**（`modals::*` の `HINT`。表には寄せない。07）。
pub fn live(state: &AppState) -> Option<&'static [KeyHint]> {
    if state.modal.is_some() {
        return None;
    }
    Some(pane(state.focus))
}

/// そのペインで押せるキー。
///
/// **網羅 `match`。** `Pane` に変種を足すと実装漏れがコンパイルエラーになる
/// （キーを受ける先を作ってから巡回に足す、という順序をここでも強いる）。
pub fn pane(pane: Pane) -> &'static [KeyHint] {
    match pane {
        Pane::Graph => GRAPH,
        Pane::Branches => BRANCHES,
        Pane::Prs => PRS,
        Pane::Worktree => WORKTREE,
        Pane::CommitDetail => COMMIT_DETAIL,
        Pane::Diff => DIFF,
        // **Stash はまだ無い**（M5 のペイン）。巡回候補にも入っていないので、
        // フォーカスがここに来ることはない。
        Pane::Stash => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_help_has_the_pane_keys_then_the_keys_that_work_anywhere() {
        let rows = help(Pane::Graph);
        assert_eq!(rows[0], HelpRow::Heading("このペイン"));
        let anywhere = rows
            .iter()
            .position(|row| *row == HelpRow::Heading("どこでも"))
            .expect("どこでもの見出しがありません");
        assert_eq!(anywhere, 1 + pane(Pane::Graph).len());
        assert_eq!(rows.len(), anywhere + 1 + COMMON.len());
    }

    #[test]
    fn a_pane_without_keys_shows_no_empty_heading() {
        // Stash はまだキーを持たない。空の段を見せない。
        let rows = help(Pane::Stash);
        assert_eq!(rows[0], HelpRow::Heading("どこでも"));
        assert!(!rows.contains(&HelpRow::Heading("このペイン")));
    }
}
