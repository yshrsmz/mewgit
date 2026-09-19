//! 案内に出すキーとその説明の表。設計は 01 の「案内に出すキーの表」。
//!
//! **キーの一覧はここ 1 本。** いま読むのはステータスバーの 2 行目（07 の
//! 「ステータスバー」）で、あとから入る `Modal::Help` と `config.keymap` の上書き（08）も
//! ここを読む。2 本に分けると、上書きしたときにヒントだけが既定の字を出す。
//!
//! **表示の字は持たない。** `j/k` や `Enter` という並びは `ui` が [`KeyCode`] から
//! 組む（導出できる値を離れた場所に置かない。`CLAUDE.md`）。
//!
//! **`Tab` / `Shift+Tab` / `Ctrl-C` は入っていない。** いま読むのが 2 行目だけなので、
//! そこに描かないものを入れても誰も読まない行になる。足すのは `Modal::Help` のタスク
//! （`docs/plan.md` の M6）。**モーダル自身のキーの案内も入っていない** —— あちらは
//! 箱の中に自前の `HINT` を持っている（`modals::*`。07 の「ステータスバー」）。
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

/// 終了。**幅が足りなくても落とさない**（07）。
pub const QUIT: KeyHint = hint(&[Key::char('q')], "quit");

/// どのペインでも効くキー（`update_key` がペインの `match` より先に受ける）。
///
/// **出すのは幅が余るペインだけ**（07）。落とす順が「共通キー → ペインのキーの末尾」
/// なのは、ペインのキーが「いまここで何ができるか」の唯一の案内だから。
pub const COMMON: &[KeyHint] = &[
    hint(&[Key::char('f')], "fetch"),
    hint(&[Key::char('p')], "pull"),
    hint(&[Key::char('P')], "push"),
    hint(&[Key::char('A')], "account"),
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

const PRS: &[KeyHint] = &[MOVE, hint(&[Key::char('o')], "ブラウザ"), BACK];

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

/// **いま**押せるペインのキー。`None` は「この表のどれも押せない」。
///
/// **モーダル表示中が `None`。** そのあいだキーは全部モーダルが受けるので
/// （`update_key`）、ペインのキーも [`COMMON`] も [`QUIT`] も効かない。
/// **`ui` に `modal.is_some()` を書かせない**ため、ここが答える —— あちらに書くと
/// 振り分けの規則の写しが 2 本になる。**モーダル自身のキーの案内は各モーダルが
/// 持っている**（`modals::*` の `HINT`。表に入れるのは `Modal::Help` のタスク）。
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
