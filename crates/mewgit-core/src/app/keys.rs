//! 案内に出すキーとその説明の表。設計は 01 の「案内に出すキーの表」。
//!
//! **キーの一覧はここ 1 本。** 読むのはステータスバーの 2 行目（07 の「ステータスバー」）と
//! `Modal::Help`（07 の「Help モーダル」）で、あとから入る `config.keymap` の上書き（08）も
//! ここを読む。2 本に分けると、上書きしたときにヒントだけが既定の字を出す。
//!
//! **表が dispatch を兼ねる。** 1 打鍵ごとに [`Action`] を持ち、`update_key` はここを
//! 引いてから各ペインの `*_key` で網羅 `match` する（01 の「表が dispatch を兼ねる」）。
//! `config.keymap`（08）はアクション名で書くので、**`label` は鍵にならない**。
//!
//! **表示の字は持たない。** `j/k` や `Enter` という並びは `ui` が [`KeyCode`] から
//! 組む（導出できる値を離れた場所に置かない。`.claude/rules/rust-coding.md`）。
//!
//! **モーダル自身のキーの案内は入っていない** —— あちらは箱の中に自前の `HINT` を
//! 持っている（`modals::*`。予算が違うので表に寄せない。07 の「ステータスバー」）。

use super::{AppState, Key, KeyCode, Pane};

/// 表の 1 打鍵が引く動作。ペインごとの enum を包む。
///
/// **移動だけはペインの外**（[`Action::Down`] / [`Action::Up`]）。どのペインにもあり、
/// 何のカーソルを動かすかは `focus` が決める（01 の「表が dispatch を兼ねる」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Down,
    Up,
    /// [`COMMON`] と [`PINNED`]。ペインの表より先に引く。
    Common(CommonAction),
    Graph(GraphAction),
    Branches(BranchesAction),
    Stash(StashAction),
    Prs(PrsAction),
    Worktree(WorktreeAction),
    CommitDetail(CommitDetailAction),
    Diff(DiffAction),
}

/// どのペインでも効く動作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommonAction {
    Help,
    Quit,
    Fetch,
    Pull,
    Push,
    Account,
    NoticeLog,
    ProgressMenu,
    Reload,
    NextPane,
    PrevPane,
    ToGraph,
    ToBranches,
    ToStash,
    ToPrs,
    /// 右下（Working tree かコミット詳細。グラフの選択で決まる）。
    ToRight,
    /// 走っている git の中断、無ければ終了。**モーダルより先に引く**（`update_key`）。
    Interrupt,
}

/// Graph ペインの動作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphAction {
    Detail,
    Branch,
    CherryPick,
    Revert,
    Rebase,
    Mark,
    Unmark,
}

/// Branches ペインの動作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchesAction {
    Checkout,
    Branch,
    Rename,
    Delete,
    ForceDelete,
    Back,
}

/// Stash ペインの動作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StashAction {
    Diff,
    Stash,
    Apply,
    Pop,
    Drop,
    Back,
}

/// PRs ペインの動作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrsAction {
    Checkout,
    Open,
    Refresh,
    Create,
    Back,
}

/// Worktree ペインの動作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeAction {
    Diff,
    Stage,
    Unstage,
    Discard,
    Commit,
    Amend,
    Stash,
    StashFile,
    Back,
}

/// コミット詳細ペインの動作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitDetailAction {
    Diff,
    Back,
}

/// Diff ペインの動作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffAction {
    Stage,
    Unstage,
    Discard,
    Back,
}

/// 案内 1 つ —— 押すキーと、押すと何が起きるか。
///
/// **動作は行ではなく打鍵ごとに持つ**（`j/k:移動` は 1 行で 2 つ）。行の束ね方は 07 の
/// 80 桁の実測が前提なので、同一性のために割らない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyHint {
    binds: &'static [(Key, Action)],
    label: &'static str,
}

impl KeyHint {
    /// 案内に出すキー。同じ行に束ねたものが複数あれば全部（`j` と `k`）。
    ///
    /// **別名は入れていない** —— `j` の代わりに効く `Down` は表に無い。ここは案内に
    /// 出すものの一覧で、出さないものを足しても案内は変わらない（01）。
    ///
    /// **`'static` を約束しない。** `config.keymap` の上書き（08）で差し替えたキーは
    /// `'static` では持てない。**先に外したのはここだけ**で、[`pane`] / [`live`] /
    /// [`help`] の `'static` は上書きを入れるタスク（`docs/plan.md` の 2/2）で外す。
    pub fn keys(&self) -> impl Iterator<Item = Key> + '_ {
        self.binds.iter().map(|(key, _)| *key)
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
const fn hint(binds: &'static [(Key, Action)], label: &'static str) -> KeyHint {
    KeyHint { binds, label }
}

/// 修飾なしの文字キーと動作の組。
const fn ch(c: char, action: Action) -> (Key, Action) {
    (Key::char(c), action)
}

/// 文字を持たないキーと動作の組。
const fn code(code: KeyCode, action: Action) -> (Key, Action) {
    (Key::new(code), action)
}

/// キーの一覧（`Modal::Help`）を開く。
pub const HELP: KeyHint = hint(&[ch('?', Action::Common(CommonAction::Help))], "help");

/// 中断 / 終了の打鍵。**表を引かずにこの定数と比べる**（`update_key` の先頭）。
///
/// モーダル表示中も効く唯一の脱出口なので、`config.keymap` の上書き（08）が表をどう
/// 書き換えても消えないよう、表の中身ではなく置き場所で守る。表の行（[`COMMON`] の
/// 「中断 / 終了」）は案内に出すためだけにある。
pub const INTERRUPT: Key = Key::ctrl('c');

/// 終了。
pub const QUIT: KeyHint = hint(&[ch('q', Action::Common(CommonAction::Quit))], "quit");

/// **フッターに必ず出し、幅が足りなくても落とさない**キー（07 の「ステータスバー」）。
/// どのペインでも効く。
pub const PINNED: &[KeyHint] = &[HELP, QUIT];

/// どのペインでも効くキーのうち、**フッターには出さないもの**（`Modal::Help` だけが読む。
/// 07 の「ステータスバー」）。`update_key` がペインの表より先に引く。
pub const COMMON: &[KeyHint] = &[
    // **1 行に束ねる。** 分けると `R` を足した時点で `?` の一覧が 80x24 に収まらない
    // （07 の「Help モーダル」。Worktree の `z/Z` と同じ理由）。
    hint(
        &[
            ch('f', Action::Common(CommonAction::Fetch)),
            ch('p', Action::Common(CommonAction::Pull)),
            ch('P', Action::Common(CommonAction::Push)),
        ],
        "fetch / pull / push",
    ),
    hint(&[ch('A', Action::Common(CommonAction::Account))], "account"),
    hint(&[ch('L', Action::Common(CommonAction::NoticeLog))], "通知"),
    hint(
        &[ch('m', Action::Common(CommonAction::ProgressMenu))],
        "進行中操作のメニュー",
    ),
    // 外の変更の検知が外れたときの逃げ道（01 の「外部の変更の検知 › `R`」）。
    hint(&[ch('R', Action::Common(CommonAction::Reload))], "読み直し"),
    hint(
        &[
            code(KeyCode::Tab, Action::Common(CommonAction::NextPane)),
            code(KeyCode::BackTab, Action::Common(CommonAction::PrevPane)),
        ],
        "ペイン移動",
    ),
    // 番号は画面の場所に付ける（07 の「ペインとフォーカス」）。`5` は右下。
    hint(
        &[
            ch('1', Action::Common(CommonAction::ToGraph)),
            ch('2', Action::Common(CommonAction::ToBranches)),
            ch('3', Action::Common(CommonAction::ToStash)),
            ch('4', Action::Common(CommonAction::ToPrs)),
            ch('5', Action::Common(CommonAction::ToRight)),
        ],
        "Graph/Branches/Stash/PRs/右下へ",
    ),
    // git が走っていれば中断、そうでなければ終了。**引くのは表ではなく [`INTERRUPT`]**。
    hint(
        &[(INTERRUPT, Action::Common(CommonAction::Interrupt))],
        "中断 / 終了",
    ),
];

/// 上下移動。**ペインをまたいで同じ 1 つを使う**（説明が割れないように）。
const MOVE: KeyHint = hint(&[ch('j', Action::Down), ch('k', Action::Up)], "移動");

/// 表に出さない別名。**案内に出すものだけを表に入れる**ので（[`KeyHint::keys`]）、
/// 引く側だけが持つ。どのペインにも [`MOVE`] があるので、ペインを問わない。
const ALIASES: &[(Key, Action)] = &[
    code(KeyCode::Down, Action::Down),
    code(KeyCode::Up, Action::Up),
];

/// 左のグラフへ戻る（Diff だけは `Enter` で来た側 —— Working tree、コミット詳細、Stash）。
///
/// **「1 つ前のペイン」ではない。** `Esc` の行き先は各ペインが決めていて、右列のペインは
/// どれも無条件に `Pane::Graph` を指す（巡回でどこから来ても同じ）。ラベルを「戻る」に
/// してあるのは、どちらでも嘘にならない語だから。
const BACK: &str = "戻る";

const GRAPH: &[KeyHint] = &[
    MOVE,
    // 行き先は選択で変わる（Working tree かコミット詳細）ので、どちらでも嘘に
    // ならない語にする。
    hint(
        &[code(KeyCode::Enter, Action::Graph(GraphAction::Detail))],
        "詳細",
    ),
    // **git の語のまま**（`y:cherry-pick` と同じ）。「ブランチ作成」だと `i` を足したときに
    // 80 桁を 1 セル超えた（07 の「Help モーダル」の実測）。
    hint(&[ch('n', Action::Graph(GraphAction::Branch))], "branch"),
    hint(
        &[ch('y', Action::Graph(GraphAction::CherryPick))],
        "cherry-pick",
    ),
    hint(&[ch('v', Action::Graph(GraphAction::Revert))], "revert"),
    // 何をするか（interactive）はエディタの枠のタイトルが言う。
    hint(&[ch('i', Action::Graph(GraphAction::Rebase))], "rebase"),
    // **末尾に置き、80 桁では落ちるのを受けた**（2026-10-01 にユーザーと決めた。07 の
    // 「Help モーダル」）—— 落ちるのはこの 2 つだけで、`?` の一覧には出る。
    hint(&[ch(' ', Action::Graph(GraphAction::Mark))], "mark"),
    hint(
        &[code(KeyCode::Esc, Action::Graph(GraphAction::Unmark))],
        "unmark",
    ),
];

const BRANCHES: &[KeyHint] = &[
    MOVE,
    hint(
        &[code(
            KeyCode::Enter,
            Action::Branches(BranchesAction::Checkout),
        )],
        "切替",
    ),
    hint(&[ch('n', Action::Branches(BranchesAction::Branch))], "作成"),
    hint(&[ch('r', Action::Branches(BranchesAction::Rename))], "改名"),
    hint(&[ch('d', Action::Branches(BranchesAction::Delete))], "削除"),
    hint(
        &[ch('D', Action::Branches(BranchesAction::ForceDelete))],
        "強制削除",
    ),
    hint(
        &[code(KeyCode::Esc, Action::Branches(BranchesAction::Back))],
        BACK,
    ),
];

const STASH: &[KeyHint] = &[
    MOVE,
    // Worktree / コミット詳細の `Enter` と同じ語。どれも左に Diff を出す。
    hint(
        &[code(KeyCode::Enter, Action::Stash(StashAction::Diff))],
        "差分",
    ),
    // 退避する相手は作業ツリーで、Worktree の `z` と同じ（あちらは `Z` と束ねた行）。
    hint(&[ch('z', Action::Stash(StashAction::Stash))], "stash"),
    hint(&[ch('a', Action::Stash(StashAction::Apply))], "apply"),
    hint(&[ch('Z', Action::Stash(StashAction::Pop))], "pop"),
    hint(&[ch('d', Action::Stash(StashAction::Drop))], "drop"),
    hint(
        &[code(KeyCode::Esc, Action::Stash(StashAction::Back))],
        BACK,
    ),
];

const PRS: &[KeyHint] = &[
    MOVE,
    // **Branches の `Enter` と同じ語。** どちらも「そのブランチに居る状態にする」で、
    // PR のほうは無ければ作り、在れば早送りする（05 の「PR チェックアウト」）。
    hint(
        &[code(KeyCode::Enter, Action::Prs(PrsAction::Checkout))],
        "切替",
    ),
    hint(&[ch('o', Action::Prs(PrsAction::Open))], "ブラウザ"),
    // **Branches の `r`（改名）とは別のキー。** キーマップはペインごとに割れて
    // いるので衝突しない（`.claude/rules/rust-coding.md` の「『押せる』は画面ごとに数える」）。
    hint(&[ch('r', Action::Prs(PrsAction::Refresh))], "更新"),
    hint(&[ch('N', Action::Prs(PrsAction::Create))], "PR 作成"),
    hint(&[code(KeyCode::Esc, Action::Prs(PrsAction::Back))], BACK),
];

const WORKTREE: &[KeyHint] = &[
    MOVE,
    hint(
        &[code(KeyCode::Enter, Action::Worktree(WorktreeAction::Diff))],
        "差分",
    ),
    hint(&[ch('s', Action::Worktree(WorktreeAction::Stage))], "stage"),
    hint(
        &[ch('u', Action::Worktree(WorktreeAction::Unstage))],
        "unstage",
    ),
    hint(
        &[ch('x', Action::Worktree(WorktreeAction::Discard))],
        "破棄",
    ),
    hint(
        &[ch('c', Action::Worktree(WorktreeAction::Commit))],
        "commit",
    ),
    hint(&[ch('C', Action::Worktree(WorktreeAction::Amend))], "amend"),
    // **`z` と `Z` を 1 行に束ねる**（`j/k` と同じ形）。分けると `?` の一覧が 80x24 に
    // 収まらない（07 の「Help モーダル」）。`Z` は Stash ペインの `Z`（pop）とは別の動き
    // —— キーマップはペインごとに割れている（PRs の `r` と同じ）。
    hint(
        &[
            ch('z', Action::Worktree(WorktreeAction::Stash)),
            ch('Z', Action::Worktree(WorktreeAction::StashFile)),
        ],
        "stash 全体/1 ファイル",
    ),
    hint(
        &[code(KeyCode::Esc, Action::Worktree(WorktreeAction::Back))],
        BACK,
    ),
];

const COMMIT_DETAIL: &[KeyHint] = &[
    MOVE,
    hint(
        &[code(
            KeyCode::Enter,
            Action::CommitDetail(CommitDetailAction::Diff),
        )],
        "差分",
    ),
    hint(
        &[code(
            KeyCode::Esc,
            Action::CommitDetail(CommitDetailAction::Back),
        )],
        BACK,
    ),
];

const DIFF: &[KeyHint] = &[
    MOVE,
    hint(&[ch('s', Action::Diff(DiffAction::Stage))], "stage"),
    hint(&[ch('u', Action::Diff(DiffAction::Unstage))], "unstage"),
    hint(&[ch('x', Action::Diff(DiffAction::Discard))], "破棄"),
    hint(&[code(KeyCode::Esc, Action::Diff(DiffAction::Back))], BACK),
];

/// どのペインでも効く表（[`COMMON`] と [`PINNED`]）。
pub(crate) fn anywhere() -> impl Iterator<Item = &'static KeyHint> {
    COMMON.iter().chain(PINNED)
}

/// 表の打鍵を平たく並べる。
fn binds<'a>(
    hints: impl IntoIterator<Item = &'a KeyHint>,
) -> impl Iterator<Item = &'a (Key, Action)> {
    hints.into_iter().flat_map(|hint| hint.binds)
}

/// `binds` で `key` を引く。**比べるのは `Key` 全体**（修飾付きのキーを拾わない。01）。
fn find<'a>(binds: impl IntoIterator<Item = &'a (Key, Action)>, key: Key) -> Option<Action> {
    binds
        .into_iter()
        .find(|(bound, _)| *bound == key)
        .map(|(_, action)| *action)
}

/// どのペインでも効く表で `key` を引く。
pub(crate) fn common_action(key: Key) -> Option<CommonAction> {
    match find(binds(anywhere()), key)? {
        Action::Common(action) => Some(action),
        // 共通の表にペインの動作は置かない（`the_tables_hold_only_their_own_actions`）。
        _ => None,
    }
}

/// `pane` の表で `key` を引き、無ければ別名（[`ALIASES`]）を引く。
pub(crate) fn pane_action(pane: Pane, key: Key) -> Option<Action> {
    find(binds(self::pane(pane)).chain(ALIASES), key)
}

/// [`help`] の段。見出しの字は `ui` が持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpSection {
    /// 開いたときのペインのキー。
    Pane,
    /// どのペインでも効くキー（[`COMMON`] と [`PINNED`]）。
    Anywhere,
}

/// [`help`] の 1 行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpRow {
    Heading(HelpSection),
    Key(&'static KeyHint),
}

/// `Modal::Help` の行。**そのペインのキー → どこでも効くキー**の 2 段（07 の
/// 「Help モーダル」）。ほかのペインのキーは出さない —— 押せるのは今いるペインのものだけ。
///
/// **キーの無いペインは無い**（Stash がキーを持った時点で最後の 1 つが消えた）ので、
/// 「空の段を見せない」分岐は置かない —— 踏めない腕はテストで見張れずに腐る。
pub fn help(pane: Pane) -> Vec<HelpRow> {
    let here = self::pane(pane);
    let mut rows = Vec::with_capacity(here.len() + COMMON.len() + PINNED.len() + 2);
    rows.push(HelpRow::Heading(HelpSection::Pane));
    rows.extend(here.iter().map(HelpRow::Key));
    rows.push(HelpRow::Heading(HelpSection::Anywhere));
    rows.extend(anywhere().map(HelpRow::Key));
    rows
}

/// **いま**押せるペインのキー。`None` は「この表のどれも押せない」。
///
/// **モーダル表示中が `None`。** そのあいだキーは全部モーダルが受けるので
/// （`update_key`）、ペインのキーも [`COMMON`] も [`PINNED`] も効かない。
/// **`ui` に `modal.is_some()` を書かせない**ため、ここが答える —— あちらに書くと
/// 振り分けの規則の写しが 2 本になる。**モーダル自身のキーの案内は各モーダルが
/// 持っている**（`modals::*` の `HINT`。表には寄せない。07）。
pub fn live(state: &AppState) -> Option<&'static [KeyHint]> {
    if state.modal.is_some() {
        return None;
    }
    Some(pane(state.focus))
}

/// `action` に**いま**効くキー。`None` は「いまは押せない」。引くのはペインの表
/// （[`live`]）と、どこでも効く表（[`COMMON`] と [`PINNED`]）の両方。
///
/// **ペインの本文にキーを書くときに使う**（PRs ペインの「開いている PR はありません（N）」。
/// 07 の「PRs ペイン」）。`ui` が `focus` やモーダルを見て自分で決めると、振り分けの規則の
/// 写しが 2 本になり、モーダル表示中に効かない案内が出る（[`live`] の doc と同じ理由）。
/// **字ではなく動作で聞く** —— 字で聞くと、`config.keymap`（08）で上書きした瞬間に
/// 押せないキーを案内する。表に載っていない別名（`Down` など）は返さない。
pub fn live_key(state: &AppState, action: Action) -> Option<Key> {
    binds(live(state)?.iter().chain(anywhere()))
        .find(|(_, bound)| *bound == action)
        .map(|(key, _)| *key)
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
        Pane::Stash => STASH,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Modal, RepoInfo};
    use crate::config::Config;
    use crate::test_util::FAKE_NOW;

    #[test]
    fn an_action_has_a_live_key_only_in_its_pane_and_never_under_a_modal() {
        let mut state = AppState::new(RepoInfo::default(), Config::default(), FAKE_NOW);
        let create = Action::Prs(PrsAction::Create);
        state.focus = Pane::Graph;
        assert_eq!(
            live_key(&state, create),
            None,
            "Graph では PR 作成は効かない"
        );
        state.focus = Pane::Prs;
        assert_eq!(live_key(&state, create), Some(Key::char('N')));
        // 別名は案内に出さないので、表のキー（`j`）を返す。
        assert_eq!(live_key(&state, Action::Down), Some(Key::char('j')));
        // **どこでも効く表も引く**（`A` を案内に書く側が同じ関数で引ける）。
        let account = Action::Common(CommonAction::Account);
        assert_eq!(live_key(&state, account), Some(Key::char('A')));
        // **モーダル表示中はキーを全部モーダルが受ける**（`live`）。
        state.modal = Some(Modal::Help {
            pane: Pane::Prs,
            cursor: 0,
        });
        assert_eq!(live_key(&state, create), None, "モーダルの下で効くと言った");
        assert_eq!(
            live_key(&state, account),
            None,
            "モーダルの下で効くと言った"
        );
    }

    /// 表の打鍵が引く動作を全部。
    fn bound<'a>(hints: impl IntoIterator<Item = &'a KeyHint>) -> Vec<Action> {
        binds(hints).map(|(_, action)| *action).collect()
    }

    /// 動作の持ち主のペイン。移動と共通の動作は `None`（どのペインのものでもない）。
    ///
    /// **網羅 `match`** —— `Action` にペインを足すと、ここで書き忘れが落ちる。下の 2 本の
    /// テストはどちらもこの対応を引く（ペインと動作の enum の対応を 2 箇所に書かない）。
    fn owner(action: Action) -> Option<Pane> {
        match action {
            Action::Down | Action::Up | Action::Common(_) => None,
            Action::Graph(_) => Some(Pane::Graph),
            Action::Branches(_) => Some(Pane::Branches),
            Action::Stash(_) => Some(Pane::Stash),
            Action::Prs(_) => Some(Pane::Prs),
            Action::Worktree(_) => Some(Pane::Worktree),
            Action::CommitDetail(_) => Some(Pane::CommitDetail),
            Action::Diff(_) => Some(Pane::Diff),
        }
    }

    /// **動作の enum の全変種が表に在る**（01 の「表が dispatch を兼ねる」）。dispatch の
    /// 網羅 `match` が「表 → 腕」を強いるので、こちらが逆向き（「変種 → 表」）を見る。
    /// 表に無い変種は、腕を書いても押す手が無い。
    #[test]
    fn every_action_is_bound_in_its_table() {
        let anywhere = bound(anywhere());
        for action in all_variants!(CommonAction {
            Help,
            Quit,
            Fetch,
            Pull,
            Push,
            Account,
            NoticeLog,
            ProgressMenu,
            Reload,
            NextPane,
            PrevPane,
            ToGraph,
            ToBranches,
            ToStash,
            ToPrs,
            ToRight,
            Interrupt,
        }) {
            let action = Action::Common(action);
            assert!(anywhere.contains(&action), "{action:?} が表にありません");
        }

        let pane_actions: Vec<Action> = [
            all_variants!(GraphAction {
                Detail,
                Branch,
                CherryPick,
                Revert,
                Rebase,
                Mark,
                Unmark,
            })
            .map(Action::Graph)
            .to_vec(),
            all_variants!(BranchesAction {
                Checkout,
                Branch,
                Rename,
                Delete,
                ForceDelete,
                Back,
            })
            .map(Action::Branches)
            .to_vec(),
            all_variants!(StashAction {
                Diff,
                Stash,
                Apply,
                Pop,
                Drop,
                Back,
            })
            .map(Action::Stash)
            .to_vec(),
            all_variants!(PrsAction {
                Checkout,
                Open,
                Refresh,
                Create,
                Back,
            })
            .map(Action::Prs)
            .to_vec(),
            all_variants!(WorktreeAction {
                Diff,
                Stage,
                Unstage,
                Discard,
                Commit,
                Amend,
                Stash,
                StashFile,
                Back,
            })
            .map(Action::Worktree)
            .to_vec(),
            all_variants!(CommitDetailAction { Diff, Back })
                .map(Action::CommitDetail)
                .to_vec(),
            all_variants!(DiffAction {
                Stage,
                Unstage,
                Discard,
                Back,
            })
            .map(Action::Diff)
            .to_vec(),
        ]
        .concat();
        for &action in &pane_actions {
            let pane = owner(action).unwrap_or_else(|| panic!("{action:?} の持ち主がありません"));
            assert!(
                bound(self::pane(pane)).contains(&action),
                "{pane:?} の表に {action:?} がありません"
            );
        }
        // **ペインの一覧も網羅で回す**（`.claude/rules/testing.md` の「見張りが回す範囲を
        // 実装側の集合から導いていると…」）。上に並べ忘れたペインと、移動の無いペインを落とす。
        for &pane in Pane::ALL {
            assert!(
                pane_actions
                    .iter()
                    .any(|action| owner(*action) == Some(pane)),
                "{pane:?} の動作が並んでいません"
            );
            // **移動はどのペインにもある**（別名の矢印もこれを前提にペインを問わず引く）。
            let table = bound(self::pane(pane));
            for action in [Action::Down, Action::Up] {
                assert!(
                    table.contains(&action),
                    "{pane:?} の表に {action:?} がありません"
                );
            }
        }
    }

    /// **表はそのペインの動作だけを持つ。** 別のペインの動作が混ざると、`update_key` は
    /// それを別のペインの `*_key` に渡し、フォーカスと違うペインの状態を動かす。
    #[test]
    fn the_tables_hold_only_their_own_actions() {
        for action in bound(anywhere()) {
            assert!(matches!(action, Action::Common(_)), "共通の表に {action:?}");
        }
        for &pane in Pane::ALL {
            for action in bound(self::pane(pane)) {
                let own =
                    matches!(action, Action::Down | Action::Up) || owner(action) == Some(pane);
                assert!(own, "{pane:?} の表に {action:?}");
            }
        }
    }

    /// **1 つのキーが同じ画面で 2 つの動作を引かない。** 数えるのは「共通キー ∪ そのペインの
    /// キー ∪ 別名」—— 共通キーはペインより先に引くので、重なるとペインのキーが押せない（08）。
    #[test]
    fn no_key_is_bound_twice_on_one_screen() {
        for &pane in Pane::ALL {
            let mut seen: Vec<Key> = Vec::new();
            let on_screen = binds(anywhere().chain(self::pane(pane))).chain(ALIASES);
            for (key, action) in on_screen {
                assert!(
                    !seen.contains(key),
                    "{pane:?} で {key:?} が 2 度（{action:?}）"
                );
                seen.push(*key);
            }
        }
    }

    #[test]
    fn keys_are_looked_up_whole_and_the_arrows_are_hidden_aliases() {
        let shift = |code| {
            Key::with_mods(
                code,
                crate::app::Modifiers {
                    shift: true,
                    ..Default::default()
                },
            )
        };
        assert_eq!(pane_action(Pane::Diff, Key::char('j')), Some(Action::Down));
        assert_eq!(
            pane_action(Pane::Diff, Key::new(KeyCode::Down)),
            Some(Action::Down)
        );
        assert_eq!(
            pane_action(Pane::Diff, Key::new(KeyCode::Up)),
            Some(Action::Up)
        );
        // **修飾付きは別のキー**（`match key.code()` だったころは `Ctrl-j` も移動した）。
        assert_eq!(pane_action(Pane::Diff, Key::ctrl('j')), None);
        assert_eq!(pane_action(Pane::Diff, shift(KeyCode::Down)), None);
        // 別名は案内に出さない。
        assert!(!pane(Pane::Diff)
            .iter()
            .any(|hint| hint.keys().any(|key| key == Key::new(KeyCode::Down))));

        assert_eq!(common_action(Key::char('f')), Some(CommonAction::Fetch));
        assert_eq!(common_action(Key::ctrl('f')), None);
        // ペインのキーは共通の表では引けない。
        assert_eq!(common_action(Key::char('s')), None);
    }

    #[test]
    fn the_help_has_the_pane_keys_then_the_keys_that_work_anywhere() {
        let rows = help(Pane::Graph);
        assert_eq!(rows[0], HelpRow::Heading(HelpSection::Pane));
        let anywhere = rows
            .iter()
            .position(|row| *row == HelpRow::Heading(HelpSection::Anywhere))
            .expect("どこでもの見出しがありません");
        assert_eq!(anywhere, 1 + pane(Pane::Graph).len());
        assert_eq!(rows.len(), anywhere + 1 + COMMON.len() + PINNED.len());
    }
}
