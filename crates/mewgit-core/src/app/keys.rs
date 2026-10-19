//! 案内に出すキーとその説明の表。設計は 01 の「案内に出すキーの表」。
//!
//! **キーの一覧はここ 1 本。** 読むのはステータスバーの 2 行目（07 の「ステータスバー」）と
//! `Modal::Help`（07 の「Help モーダル」）と dispatch（`update_key`）で、`config.keymap` の
//! 上書き（08）もここに当たる。2 本に分けると、上書きしたときにヒントだけが既定の字を出す。
//!
//! **表が dispatch を兼ねる。** 1 打鍵ごとに [`Action`] を持ち、`update_key` はここを
//! 引いてから各ペインの `*_key` で網羅 `match` する（01 の「表が dispatch を兼ねる」）。
//! `config.keymap`（08）はアクション名（[`Action::name`]）で書くので、**`label` は鍵にならない**。
//!
//! **既定の表は `const`、引くのは [`Keymap`]。** `AppState::new` が既定の表に上書きを当てて
//! 画面（ペイン）ごとに組み、`AppState.keymap` に持つ。
//!
//! **表示の字は持たない。** キー 1 つの字は [`Key`] の `Display`、`j/k` という並びは `ui` が
//! 組む（導出できる値を離れた場所に置かない。`.claude/rules/rust-coding.md`）。文言に
//! キーを書く側は動作で引いて [`spell`] で組む（08 の「文言に書いたキーの字」）。
//!
//! **モーダル自身のキーの案内は入っていない** —— あちらは箱の中に自前の `HINT` を
//! 持っている（`modals::*`。予算が違うので表に寄せない。07 の「ステータスバー」）。

use std::borrow::Cow;
use std::collections::HashMap;

use super::{AppState, Key, KeyCode, Pane, ParseKeyError};

/// 表の 1 打鍵が引く動作。ペインごとの enum を包む。
///
/// **移動だけはペインの外**（[`Action::Down`] / [`Action::Up`]）。どのペインにもあり、
/// 何のカーソルを動かすかは `focus` が決める（01 の「表が dispatch を兼ねる」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Down,
    Up,
    /// どこでも効く表（[`COMMON`] と [`PINNED`]）。引くときはペインの表と 1 つにまとめてある
    /// （[`PaneKeys`]）。
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

impl Action {
    /// 動作の持ち主のペイン。移動と共通の動作は `None`（どのペインのものでもない）。
    ///
    /// **文言に書くキーを引く表を決める**（[`key`]）。**網羅 `match`** —— `Action` に
    /// ペインを足すと、ここで書き忘れが落ちる。表のテストもこの対応を引く（ペインと動作の
    /// enum の対応を 2 箇所に書かない）。
    pub fn pane(self) -> Option<Pane> {
        match self {
            Self::Down | Self::Up | Self::Common(_) => None,
            Self::Graph(_) => Some(Pane::Graph),
            Self::Branches(_) => Some(Pane::Branches),
            Self::Stash(_) => Some(Pane::Stash),
            Self::Prs(_) => Some(Pane::Prs),
            Self::Worktree(_) => Some(Pane::Worktree),
            Self::CommitDetail(_) => Some(Pane::CommitDetail),
            Self::Diff(_) => Some(Pane::Diff),
        }
    }

    /// `config.keymap` に書く名前（08 の「キーマップの解決」）。
    ///
    /// **名前の空間は平坦。** 同じ意味の動作は複数のペインで名前を共有し、上書きはその
    /// 全部に効く（`stage` は Worktree と Diff、`back` は右列の `Esc`）。意味が違うものは
    /// 分ける（PRs の `r` は `refresh`、Branches の `r` は `rename`）。**同じ画面で 2 つの
    /// 動作が名前を共有しない**のは `no_name_is_shared_on_one_screen` が見る —— 共有すると
    /// 1 つの上書きが同じ画面に同じキーを 2 本置く。
    ///
    /// **stage / unstage / discard を共有の enum にはしない**（`docs/plan.md` の 2/2 で
    /// 数え直した）。名前の字が 2 箇所に出るだけで、共有の enum にすると `all_variants!` の
    /// 網羅が `missing` の形になる。共有の取り違えは上のテストが落とす。
    pub fn name(self) -> &'static str {
        match self {
            Self::Down => "down",
            Self::Up => "up",
            Self::Common(action) => match action {
                CommonAction::Help => "help",
                CommonAction::Quit => "quit",
                CommonAction::Fetch => "fetch",
                CommonAction::Pull => "pull",
                CommonAction::Push => "push",
                CommonAction::Account => "account",
                CommonAction::NoticeLog => "notice_log",
                CommonAction::ProgressMenu => "progress_menu",
                CommonAction::Reload => "reload",
                CommonAction::NextPane => "next_pane",
                CommonAction::PrevPane => "prev_pane",
                CommonAction::ToGraph => "focus_graph",
                CommonAction::ToBranches => "focus_branches",
                CommonAction::ToStash => "focus_stash",
                CommonAction::ToPrs => "focus_prs",
                CommonAction::ToRight => "focus_right",
                CommonAction::Interrupt => INTERRUPT_NAME,
            },
            Self::Graph(action) => match action {
                GraphAction::Detail => "detail",
                GraphAction::Branch => "branch",
                GraphAction::CherryPick => "cherry_pick",
                GraphAction::Revert => "revert",
                GraphAction::Rebase => "rebase",
                GraphAction::Mark => "mark",
                GraphAction::Unmark => "unmark",
            },
            Self::Branches(action) => match action {
                BranchesAction::Checkout => "checkout",
                BranchesAction::Branch => "branch",
                BranchesAction::Rename => "rename",
                BranchesAction::Delete => "delete",
                BranchesAction::ForceDelete => "force_delete",
                BranchesAction::Back => "back",
            },
            Self::Stash(action) => match action {
                StashAction::Diff => "diff",
                StashAction::Stash => "stash",
                StashAction::Apply => "apply",
                StashAction::Pop => "pop",
                StashAction::Drop => "drop",
                StashAction::Back => "back",
            },
            Self::Prs(action) => match action {
                PrsAction::Checkout => "checkout",
                PrsAction::Open => "browse",
                PrsAction::Refresh => "refresh",
                PrsAction::Create => "create_pr",
                PrsAction::Back => "back",
            },
            Self::Worktree(action) => match action {
                WorktreeAction::Diff => "diff",
                WorktreeAction::Stage => "stage",
                WorktreeAction::Unstage => "unstage",
                WorktreeAction::Discard => "discard",
                WorktreeAction::Commit => "commit",
                WorktreeAction::Amend => "amend",
                WorktreeAction::Stash => "stash",
                WorktreeAction::StashFile => "stash_file",
                WorktreeAction::Back => "back",
            },
            Self::CommitDetail(action) => match action {
                CommitDetailAction::Diff => "diff",
                CommitDetailAction::Back => "back",
            },
            Self::Diff(action) => match action {
                DiffAction::Stage => "stage",
                DiffAction::Unstage => "unstage",
                DiffAction::Discard => "discard",
                DiffAction::Back => "back",
            },
        }
    }
}

/// [`CommonAction::Interrupt`] の名前。**上書きを断る**（08。脱出口は [`INTERRUPT`]）。
const INTERRUPT_NAME: &str = "interrupt";

/// 案内 1 つ —— 押すキーと、押すと何が起きるか。
///
/// **動作は行ではなく打鍵ごとに持つ**（`j/k:移動` は 1 行で 2 つ）。行の束ね方は 07 の
/// 80 桁の実測が前提なので、同一性のために割らない。
///
/// **打鍵は `Cow`。** 既定の表は `const` で借り、[`Keymap`] が組んだ行は持つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyHint {
    binds: Cow<'static, [(Key, Action)]>,
    label: &'static str,
}

impl KeyHint {
    /// 案内に出すキー。同じ行に束ねたものが複数あれば全部（`j` と `k`）。
    ///
    /// **別名は入れていない** —— `j` の代わりに効く `Down` は表に無い。ここは案内に
    /// 出すものの一覧で、出さないものを足しても案内は変わらない（01）。
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
    KeyHint {
        binds: Cow::Borrowed(binds),
        label,
    }
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
const HELP: KeyHint = hint(&[ch('?', Action::Common(CommonAction::Help))], "help");

/// 中断 / 終了の打鍵。**表を引かずにこの定数と比べる**（`update_key` の先頭）。
///
/// モーダル表示中も効く唯一の脱出口なので、`config.keymap` の上書き（08）が表をどう
/// 書き換えても消えないよう、表の中身ではなく置き場所で守る。表の行（[`COMMON`] の
/// 「中断 / 終了」）は案内に出すためだけにある。
pub const INTERRUPT: Key = Key::ctrl('c');

/// 終了。
const QUIT: KeyHint = hint(&[ch('q', Action::Common(CommonAction::Quit))], "quit");

/// **フッターに必ず出し、幅が足りなくても落とさない**キー（07 の「ステータスバー」）。
/// どのペインでも効く。
const PINNED: &[KeyHint] = &[HELP, QUIT];

/// どのペインでも効くキーのうち、**フッターには出さないもの**（`Modal::Help` だけが読む。
/// 07 の「ステータスバー」）。どのペインの [`PaneKeys`] にも入る。
const COMMON: &[KeyHint] = &[
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

/// どのペインでも効く既定の表（[`COMMON`] と [`PINNED`]）。
fn anywhere() -> impl Iterator<Item = &'static KeyHint> {
    COMMON.iter().chain(PINNED)
}

/// そのペインの既定の表。
///
/// **網羅 `match`。** `Pane` に変種を足すと実装漏れがコンパイルエラーになる
/// （キーを受ける先を作ってから巡回に足す、という順序をここでも強いる）。
fn table(pane: Pane) -> &'static [KeyHint] {
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

/// 表の打鍵を平たく並べる。
fn binds<'a>(
    hints: impl IntoIterator<Item = &'a KeyHint>,
) -> impl Iterator<Item = &'a (Key, Action)> {
    hints.into_iter().flat_map(|hint| hint.binds.iter())
}

/// `config.keymap` を読んで見つかった問題（08 の「キーマップの解決」）。どれも
/// **その上書きを捨てるか負けさせるだけ**で、起動は止めない。`AppState::new` が `Warn` の
/// 通知として積む。
///
/// **`ConfigWarning` に入れない** —— あちらは `config` の型で、`config` は最下段（00）なので
/// [`Key::parse`] の失敗を運べない。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeymapWarning {
    #[error("[keymap] {name}: その名前の動作はありません")]
    UnknownAction { name: String },
    #[error("[keymap] {name} = \"{value}\": {reason}")]
    BadKey {
        name: String,
        value: String,
        reason: ParseKeyError,
    },
    /// `interrupt` の名前。脱出口は表を引かない（[`INTERRUPT`]）ので、書いても効かない。
    #[error("[keymap] interrupt は変えられません（Ctrl-C は中断 / 終了の脱出口です）")]
    ReservedAction,
    /// 値が `Ctrl-C`。**捨てないと、案内に `Ctrl-C:fetch` と出ながら押すと終了する**（08）。
    #[error("[keymap] {name} = \"{value}\": Ctrl-C は中断 / 終了に使うので割り当てられません")]
    ReservedKey { name: String, value: String },
    /// 同じ画面で 1 つのキーが 2 つの動作に当たった。`winner` が効き、`loser` の打鍵は
    /// その画面の表から外れる。
    #[error("[keymap] \"{value}\" が {winner} と {loser} に重なるので {winner} を使います")]
    Conflict {
        value: String,
        winner: &'static str,
        loser: &'static str,
    },
    /// 上書きが表に出さない別名（矢印の `Down` / `Up`）を奪った。`j` / `k` は残るので、
    /// [`Conflict`](Self::Conflict) の文言では何が効かなくなったかが読めない。
    #[error("[keymap] \"{value}\" は {loser} の矢印キーなので、矢印で {loser} は効かなくなり {winner} を使います")]
    AliasTaken {
        value: String,
        winner: &'static str,
        loser: &'static str,
    },
}

/// 打鍵の出どころ。**宣言の順が強さの順**（別名 < 既定 < 上書き。上書き同士は名前の昇順で
/// 後ろほど強い —— [`Keymap::new`] が名前で並べてから番号を振る）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    Alias,
    Default,
    /// 上書きの番号。
    Override(usize),
}

/// 上書きを当てた案内の 1 行（[`PaneKeys::build`] の途中の形）。
type Row = (Vec<(Key, Action, Source)>, &'static str);

/// 読めた上書き 1 つ。
struct Override<'a> {
    name: &'a str,
    value: &'a str,
    key: Key,
}

/// 上書きを当てて画面（ペイン）ごとに組んだ表。`AppState.keymap` が持つ（08 の
/// 「キーマップの解決」）。
///
/// **画面ごとに組む。** 衝突は「共通キー ∪ そのペインのキー ∪ 別名」で見るので（08）、
/// 負けた打鍵を外す先も画面ごとに違う —— 共通キーの `f` が Worktree でだけ `stage` の
/// 上書きに負けると、Worktree の表からだけ `f:fetch` が消える。**外さないと、案内に出ている
/// キーを押して別の動作が起きる。**
#[derive(Debug, Clone)]
pub struct Keymap {
    graph: PaneKeys,
    branches: PaneKeys,
    stash: PaneKeys,
    prs: PaneKeys,
    worktree: PaneKeys,
    commit_detail: PaneKeys,
    diff: PaneKeys,
}

impl Default for Keymap {
    /// 上書きの無い表。
    fn default() -> Self {
        Self::new(&HashMap::new()).0
    }
}

impl Keymap {
    /// 既定の表に `overrides`（`config.keymap`）を当てる。
    ///
    /// **衝突は後勝ち。** 上書きは既定の打鍵より強く、上書き同士は**名前の昇順で後ろが
    /// 勝つ** —— `config.keymap` は `HashMap` で、ファイルに書いた順は残っていない（08）。
    pub fn new(overrides: &HashMap<String, String>) -> (Self, Vec<KeymapWarning>) {
        let mut warnings = Vec::new();
        let mut entries: Vec<(&String, &String)> = overrides.iter().collect();
        entries.sort();
        let mut parsed = Vec::new();
        for (name, value) in entries {
            if name == INTERRUPT_NAME {
                warnings.push(KeymapWarning::ReservedAction);
                continue;
            }
            // **名前は既定の表から引く**（全変種が表に在ることは `every_action_is_bound_in_its_table`）。
            let known = Pane::ALL
                .iter()
                .flat_map(|&pane| binds(table(pane)))
                .chain(binds(anywhere()))
                .any(|(_, action)| action.name() == name);
            if !known {
                warnings.push(KeymapWarning::UnknownAction { name: name.clone() });
                continue;
            }
            match Key::parse(value) {
                Err(reason) => warnings.push(KeymapWarning::BadKey {
                    name: name.clone(),
                    value: value.clone(),
                    reason,
                }),
                Ok(key) if key == INTERRUPT => warnings.push(KeymapWarning::ReservedKey {
                    name: name.clone(),
                    value: value.clone(),
                }),
                Ok(key) => parsed.push(Override { name, value, key }),
            }
        }
        let mut conflicts = Vec::new();
        let mut build = |pane| PaneKeys::build(pane, &parsed, &mut conflicts);
        let keymap = Self {
            graph: build(Pane::Graph),
            branches: build(Pane::Branches),
            stash: build(Pane::Stash),
            prs: build(Pane::Prs),
            worktree: build(Pane::Worktree),
            commit_detail: build(Pane::CommitDetail),
            diff: build(Pane::Diff),
        };
        warnings.extend(conflicts);
        (keymap, warnings)
    }

    /// そのペインに居るときの表。
    pub fn pane(&self, pane: Pane) -> &PaneKeys {
        match pane {
            Pane::Graph => &self.graph,
            Pane::Branches => &self.branches,
            Pane::Stash => &self.stash,
            Pane::Prs => &self.prs,
            Pane::Worktree => &self.worktree,
            Pane::CommitDetail => &self.commit_detail,
            Pane::Diff => &self.diff,
        }
    }
}

/// 1 つのペインに居るときに効くキー —— そのペインの表、どこでも効く表、別名。
///
/// **案内の表と引く表は同じものから組む**（[`PaneKeys::build`]）。衝突に負けた打鍵は
/// どちらからも外れている。
#[derive(Debug, Clone)]
pub struct PaneKeys {
    hints: Vec<KeyHint>,
    common: Vec<KeyHint>,
    pinned: Vec<KeyHint>,
    actions: HashMap<Key, Action>,
}

impl PaneKeys {
    /// `pane` の既定の表に `overrides` を当てて組む。衝突は `conflicts` に足す（重複は畳む。
    /// 共通キーの衝突は全部のペインで起きる）。
    fn build(pane: Pane, overrides: &[Override], conflicts: &mut Vec<KeymapWarning>) -> Self {
        // 上書きを当てた表。行の束ね方はそのまま持つ（負けた打鍵を外して案内に戻すため）。
        let apply = |hints: &'static [KeyHint]| -> Vec<Row> {
            hints
                .iter()
                .map(|hint| {
                    let binds = hint
                        .binds
                        .iter()
                        .map(|&(key, action)| {
                            match overrides.iter().position(|o| o.name == action.name()) {
                                Some(i) => (overrides[i].key, action, Source::Override(i)),
                                None => (key, action, Source::Default),
                            }
                        })
                        .collect();
                    (binds, hint.label)
                })
                .collect()
        };
        let (here, common, pinned) = (apply(table(pane)), apply(COMMON), apply(PINNED));

        let mut ranked: Vec<(Key, Action, Source)> = ALIASES
            .iter()
            .map(|&(key, action)| (key, action, Source::Alias))
            .chain(
                [&here, &common, &pinned]
                    .into_iter()
                    .flatten()
                    .flat_map(|(binds, _)| binds.iter().copied()),
            )
            .collect();
        ranked.sort_by_key(|(_, _, source)| *source);

        let mut bound: HashMap<Key, (Action, Source)> = HashMap::new();
        for (key, action, source) in ranked {
            let Some((loser, lost)) = bound
                .insert(key, (action, source))
                .filter(|(loser, _)| *loser != action)
            else {
                continue;
            };
            // **警告するのは上書きが勝ったときだけ。** 既定の表と別名は互いに重ならず、それは
            // `the_default_tables_keep_every_bind_on_every_screen` が負けた打鍵の数で見る。
            let Source::Override(i) = source else {
                continue;
            };
            let (value, winner, loser) =
                (overrides[i].value.to_string(), action.name(), loser.name());
            let conflict = if lost == Source::Alias {
                KeymapWarning::AliasTaken {
                    value,
                    winner,
                    loser,
                }
            } else {
                KeymapWarning::Conflict {
                    value,
                    winner,
                    loser,
                }
            };
            if !conflicts.contains(&conflict) {
                conflicts.push(conflict);
            }
        }

        let actions: HashMap<Key, Action> = bound
            .into_iter()
            .map(|(key, (action, _))| (key, action))
            .collect();

        // 負けた打鍵を外す。打鍵が 1 つも残らない行は落とす。
        let resolve = |rows: Vec<Row>| -> Vec<KeyHint> {
            rows.into_iter()
                .filter_map(|(binds, label)| {
                    let kept: Vec<(Key, Action)> = binds
                        .into_iter()
                        .map(|(key, action, _)| (key, action))
                        .filter(|(key, action)| actions.get(key) == Some(action))
                        .collect();
                    (!kept.is_empty()).then_some(KeyHint {
                        binds: Cow::Owned(kept),
                        label,
                    })
                })
                .collect()
        };
        Self {
            hints: resolve(here),
            common: resolve(common),
            pinned: resolve(pinned),
            actions,
        }
    }

    /// そのペインの表。
    pub fn hints(&self) -> &[KeyHint] {
        &self.hints
    }

    /// **フッターに必ず出し、幅が足りなくても落とさない**キー（`?` と `q`。07 の
    /// 「ステータスバー」）。
    pub fn pinned(&self) -> &[KeyHint] {
        &self.pinned
    }

    /// どのペインでも効く表（フッターに出さないものと [`pinned`](Self::pinned)）。
    pub fn anywhere(&self) -> impl Iterator<Item = &KeyHint> {
        self.common.iter().chain(&self.pinned)
    }

    /// `key` が引く動作。**比べるのは `Key` 全体**（修飾付きのキーを拾わない。01）。
    pub fn action(&self, key: Key) -> Option<Action> {
        self.actions.get(&key).copied()
    }

    /// `action` に効くキー。表に載っていない別名（`Down` など）は返さない。
    pub fn key_for(&self, action: Action) -> Option<Key> {
        binds(self.hints.iter().chain(self.anywhere()))
            .find(|(_, bound)| *bound == action)
            .map(|(key, _)| *key)
    }

    /// `Modal::Help` の行。**そのペインのキー → どこでも効くキー**の 2 段（07 の
    /// 「Help モーダル」）。ほかのペインのキーは出さない —— 押せるのは今いるペインのものだけ。
    ///
    /// **キーの無いペインは無い**（Stash がキーを持った時点で最後の 1 つが消えた）ので、
    /// 「空の段を見せない」分岐は置かない —— 踏めない腕はテストで見張れずに腐る。
    pub fn help(&self) -> Vec<HelpRow<'_>> {
        let mut rows =
            Vec::with_capacity(self.hints.len() + self.common.len() + self.pinned.len() + 2);
        rows.push(HelpRow::Heading(HelpSection::Pane));
        rows.extend(self.hints.iter().map(HelpRow::Key));
        rows.push(HelpRow::Heading(HelpSection::Anywhere));
        rows.extend(self.anywhere().map(HelpRow::Key));
        rows
    }
}

/// [`PaneKeys::help`] の段。見出しの字は `ui` が持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpSection {
    /// 開いたときのペインのキー。
    Pane,
    /// どのペインでも効くキー。
    Anywhere,
}

/// [`PaneKeys::help`] の 1 行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpRow<'a> {
    Heading(HelpSection),
    Key(&'a KeyHint),
}

/// 文言に書く `action` のキー（08 の「文言に書いたキーの字」）。`None` は「その表で衝突に
/// 負けて外れた」。
///
/// **引く表は動作が決める。** ペインの動作（`i` の rebase、`N` の PR 作成）は**そのペインの
/// 表**、どこでも効く動作はフォーカスのペインの表。通知は非同期に届くので、`i` を押して
/// 別のペインへ移ったあとの失敗でも、フォーカスの表で引くと `i` が見つからない。
/// **モーダルを見ない**（[`live_key`] との違い）—— 通知はモーダル表示中にも届いて閉じた
/// あとも残り、ヘッダはキーの有無で段が変わる。
pub fn key(state: &AppState, action: Action) -> Option<Key> {
    state
        .keymap
        .pane(action.pane().unwrap_or(state.focus))
        .key_for(action)
}

/// 文言にキーの字を差し込む。`actions` のキーを [`key`] で引いて `text` に渡し、**どれか
/// 1 つでも引けなければ `keyless`** を返す（08 の「文言に書いたキーの字」）。
///
/// **キーが欠けたときの扱いをここ 1 本に置く。** 呼ぶ側ごとに書くと、キーを 2 つ書く
/// 文言で片方だけ欠けた形が割れる。
pub fn spell<const N: usize>(
    state: &AppState,
    actions: [Action; N],
    text: impl FnOnce([Key; N]) -> String,
    keyless: &str,
) -> String {
    let found = actions.map(|action| key(state, action));
    if found.iter().any(Option::is_none) {
        return keyless.to_string();
    }
    text(found.map(|key| key.expect("上で確かめた")))
}

/// **いま**押せるキー。`None` は「この表のどれも押せない」。
///
/// **モーダル表示中が `None`。** そのあいだキーは全部モーダルが受けるので
/// （`update_key`）、ペインのキーもどこでも効くキーも効かない。
/// **`ui` に `modal.is_some()` を書かせない**ため、ここが答える —— あちらに書くと
/// 振り分けの規則の写しが 2 本になる。**モーダル自身のキーの案内は各モーダルが
/// 持っている**（`modals::*` の `HINT`。表には寄せない。07）。
pub fn live(state: &AppState) -> Option<&PaneKeys> {
    if state.modal.is_some() {
        return None;
    }
    Some(state.keymap.pane(state.focus))
}

/// `action` に**いま**効くキー。`None` は「いまは押せない」。引くのはペインの表
/// （[`live`]）と、どこでも効く表の両方。
///
/// **「いま押せるときだけ書く」文言に使う**（PRs ペインの「開いている PR はありません（N）」。
/// 07 の「PRs ペイン」）。ほかの文言は [`key`] から引く（モーダルを見ない）。`ui` が `focus` やモーダルを見て自分で決めると、振り分けの規則の
/// 写しが 2 本になり、モーダル表示中に効かない案内が出る（[`live`] の doc と同じ理由）。
/// **字ではなく動作で聞く** —— 字で聞くと、`config.keymap`（08）で上書きした瞬間に
/// 押せないキーを案内する。表に載っていない別名（`Down` など）は返さない。
pub fn live_key(state: &AppState, action: Action) -> Option<Key> {
    live(state)?.key_for(action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Modal, RepoInfo};
    use crate::config::Config;
    use crate::test_util::{keymap_with, keymap_without, FAKE_NOW};

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
        // **文言に書くキーはモーダルを見ず、ペインの動作はそのペインの表から引く**（`key`。
        // 08 の「文言に書いたキーの字」）。
        assert_eq!(key(&state, account), Some(Key::char('A')));
        state.focus = Pane::Graph;
        assert_eq!(
            key(&state, create),
            Some(Key::char('N')),
            "PRs の表から引いていない"
        );
    }

    #[test]
    fn spell_writes_the_keys_or_falls_back_when_any_is_missing() {
        // **キーを 2 つ書く文言は、片方でも欠ければキーを書かない形**（08 の「文言に書いた
        // キーの字」）。
        let pull = Action::Common(CommonAction::Pull);
        let push = Action::Common(CommonAction::Push);
        let fetch = Action::Common(CommonAction::Fetch);
        let both = |state: &AppState, actions| {
            spell(state, actions, |[a, b]| format!("{a} と {b}"), "キー無し")
        };
        let mut state = AppState::new(RepoInfo::default(), Config::default(), FAKE_NOW);
        assert_eq!(both(&state, [pull, push]), "p と P");
        state.keymap = keymap_with(&[("push", "ctrl-p")]);
        assert_eq!(both(&state, [pull, push]), "p と Ctrl-P");
        state.keymap = keymap_without(&["fetch"]);
        assert_eq!(both(&state, [fetch, push]), "キー無し");
        assert_eq!(both(&state, [push, fetch]), "キー無し");
    }

    /// 表の打鍵が引く動作を全部。
    fn bound<'a>(hints: impl IntoIterator<Item = &'a KeyHint>) -> Vec<Action> {
        binds(hints).map(|(_, action)| *action).collect()
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
            let pane = action
                .pane()
                .unwrap_or_else(|| panic!("{action:?} の持ち主がありません"));
            assert!(
                bound(table(pane)).contains(&action),
                "{pane:?} の表に {action:?} がありません"
            );
        }
        // **ペインの一覧も網羅で回す**（`.claude/rules/testing.md` の「見張りが回す範囲を
        // 実装側の集合から導いていると…」）。上に並べ忘れたペインと、移動の無いペインを落とす。
        for &pane in Pane::ALL {
            assert!(
                pane_actions
                    .iter()
                    .any(|action| action.pane() == Some(pane)),
                "{pane:?} の動作が並んでいません"
            );
            // **移動はどのペインにもある**（別名の矢印もこれを前提にペインを問わず引く）。
            let table = bound(table(pane));
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
            for action in bound(table(pane)) {
                let own =
                    matches!(action, Action::Down | Action::Up) || action.pane() == Some(pane);
                assert!(own, "{pane:?} の表に {action:?}");
            }
        }
    }

    /// **既定の表では、1 つのキーが同じ画面で 2 つの動作を引かない。** 数えるのは
    /// 「共通キー ∪ そのペインのキー ∪ 別名」。**警告では見ない** —— [`PaneKeys::build`] が
    /// 警告するのは上書きが勝ったときだけなので、既定どうしが重なると黙って片方が外れる。
    /// だから組んだ表に既定の打鍵が全部残っていることを見る（負けた打鍵は表から外れる）。
    #[test]
    fn the_default_tables_keep_every_bind_on_every_screen() {
        let keymap = Keymap::default();
        for &pane in Pane::ALL {
            let keys = keymap.pane(pane);
            let defaults: Vec<(Key, Action)> = binds(table(pane).iter().chain(anywhere()))
                .chain(ALIASES)
                .copied()
                .collect();
            for (key, action) in defaults {
                assert_eq!(
                    keys.action(key),
                    Some(action),
                    "{pane:?} で {key:?} が {action:?} を引きません"
                );
            }
            assert_eq!(
                screen_binds(keys).len(),
                binds(table(pane).iter().chain(anywhere())).count(),
                "{pane:?} の案内から打鍵が外れています"
            );
        }
        assert_eq!(Keymap::new(&HashMap::new()).1, Vec::new());
    }

    /// 重なる上書きは警告する。共通キーの衝突は全部の画面で起きるが、警告は 1 つに畳む。
    #[test]
    fn an_override_that_collides_is_warned_once() {
        let (_, warnings) = Keymap::new(&overrides(&[("fetch", "s")]));
        assert_eq!(
            warnings,
            vec![KeymapWarning::Conflict {
                value: "s".into(),
                winner: "fetch",
                loser: "stage",
            }],
            "Worktree と Diff の 2 画面で重なるが、警告は 1 つに畳む"
        );
        // **別名を奪ったときは別の文言**（`j` は残るので「down に重なる」では読めない）。
        let (keymap, warnings) = Keymap::new(&overrides(&[("fetch", "down")]));
        assert_eq!(
            warnings,
            vec![KeymapWarning::AliasTaken {
                value: "down".into(),
                winner: "fetch",
                loser: "down",
            }]
        );
        assert_eq!(
            keymap.pane(Pane::Graph).action(Key::char('j')),
            Some(Action::Down)
        );
    }

    /// **同じ画面で 2 つの動作が名前を共有しない**（[`Action::name`]）。共有すると、1 つの
    /// 上書きが同じ画面に同じキーを 2 本置き、片方が必ず負ける。
    #[test]
    fn no_name_is_shared_on_one_screen() {
        for &pane in Pane::ALL {
            let mut seen: Vec<(&str, Action)> = Vec::new();
            let on_screen = binds(anywhere().chain(table(pane)));
            for &(_, action) in on_screen {
                let name = action.name();
                assert!(
                    !seen.iter().any(|(n, a)| *n == name && *a != action),
                    "{pane:?} で {name} が 2 つの動作に付いています"
                );
                seen.push((name, action));
            }
        }
    }

    /// **名前を共有する動作の組は、意図したものだけ**（08 の「キーマップの解決」）。
    /// `name` の字の打ち間違いで共有が割れる形と、意図しない共有の両方を落とす。
    #[test]
    fn the_names_shared_across_panes_are_the_intended_ones() {
        let mut by_name: Vec<(&str, Vec<Action>)> = Vec::new();
        let all = Pane::ALL
            .iter()
            .flat_map(|&pane| binds(table(pane)))
            .chain(binds(anywhere()));
        for &(_, action) in all {
            match by_name.iter_mut().find(|(name, _)| *name == action.name()) {
                Some((_, actions)) if actions.contains(&action) => {}
                Some((_, actions)) => actions.push(action),
                None => by_name.push((action.name(), vec![action])),
            }
        }
        let shared: Vec<(&str, Vec<Action>)> = by_name
            .into_iter()
            .filter(|(_, actions)| actions.len() > 1)
            .collect();
        assert_eq!(
            shared,
            vec![
                (
                    "branch",
                    vec![
                        Action::Graph(GraphAction::Branch),
                        Action::Branches(BranchesAction::Branch)
                    ]
                ),
                (
                    "checkout",
                    vec![
                        Action::Branches(BranchesAction::Checkout),
                        Action::Prs(PrsAction::Checkout)
                    ]
                ),
                (
                    "back",
                    vec![
                        Action::Branches(BranchesAction::Back),
                        Action::Stash(StashAction::Back),
                        Action::Prs(PrsAction::Back),
                        Action::Worktree(WorktreeAction::Back),
                        Action::CommitDetail(CommitDetailAction::Back),
                        Action::Diff(DiffAction::Back),
                    ]
                ),
                (
                    "diff",
                    vec![
                        Action::Stash(StashAction::Diff),
                        Action::Worktree(WorktreeAction::Diff),
                        Action::CommitDetail(CommitDetailAction::Diff),
                    ]
                ),
                (
                    "stash",
                    vec![
                        Action::Stash(StashAction::Stash),
                        Action::Worktree(WorktreeAction::Stash)
                    ]
                ),
                (
                    "stage",
                    vec![
                        Action::Worktree(WorktreeAction::Stage),
                        Action::Diff(DiffAction::Stage)
                    ]
                ),
                (
                    "unstage",
                    vec![
                        Action::Worktree(WorktreeAction::Unstage),
                        Action::Diff(DiffAction::Unstage)
                    ]
                ),
                (
                    "discard",
                    vec![
                        Action::Worktree(WorktreeAction::Discard),
                        Action::Diff(DiffAction::Discard)
                    ]
                ),
            ]
        );
    }

    fn overrides(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    /// 画面の表の全打鍵。
    fn screen_binds(keys: &PaneKeys) -> Vec<(Key, Action)> {
        binds(keys.hints().iter().chain(keys.anywhere()))
            .copied()
            .collect()
    }

    #[test]
    fn an_override_moves_the_key_on_every_pane_that_shares_the_name() {
        let (keymap, warnings) = Keymap::new(&overrides(&[("stage", "S"), ("down", "ctrl-n")]));
        assert_eq!(warnings, Vec::new());
        let stage_on = |pane| keymap.pane(pane).action(Key::char('S'));
        assert_eq!(
            stage_on(Pane::Worktree),
            Some(Action::Worktree(WorktreeAction::Stage))
        );
        assert_eq!(stage_on(Pane::Diff), Some(Action::Diff(DiffAction::Stage)));
        for &pane in Pane::ALL {
            let keys = keymap.pane(pane);
            // **元のキーは効かなくなる**（足すのではなく差し替える）。
            assert_eq!(keys.action(Key::char('s')), None, "{pane:?}");
            assert_eq!(keys.action(Key::char('j')), None, "{pane:?}");
            assert_eq!(keys.action(Key::ctrl('n')), Some(Action::Down), "{pane:?}");
            // 表に出さない別名は残る。
            assert_eq!(
                keys.action(Key::new(KeyCode::Down)),
                Some(Action::Down),
                "{pane:?}"
            );
            // **案内も同じ表から出る**（ヒントだけが既定の字を出す形は作れない）。
            assert_eq!(keys.key_for(Action::Down), Some(Key::ctrl('n')), "{pane:?}");
        }
        assert_eq!(
            keymap
                .pane(Pane::Diff)
                .key_for(Action::Diff(DiffAction::Stage)),
            Some(Key::char('S'))
        );
        // 上書きの無い行は既定のまま（`k` は `j` と同じ行だが動かない）。
        assert_eq!(
            keymap.pane(Pane::Graph).action(Key::char('k')),
            Some(Action::Up)
        );
    }

    /// **衝突は後勝ちで、負けた打鍵はその画面の表から外れる**（08）。外さないと、`?` の
    /// 一覧が `s:stage` と案内しながら `s` で fetch が走る。
    #[test]
    fn a_conflicting_override_wins_and_the_loser_leaves_that_screen() {
        let (keymap, _) = Keymap::new(&overrides(&[("fetch", "s")]));
        let worktree = keymap.pane(Pane::Worktree);
        let stage = Action::Worktree(WorktreeAction::Stage);
        let fetch = Action::Common(CommonAction::Fetch);
        assert_eq!(worktree.action(Key::char('s')), Some(fetch));
        assert_eq!(worktree.key_for(stage), None);
        assert!(!screen_binds(worktree).contains(&(Key::char('s'), stage)));
        // **行ごと落ちる**（打鍵が 1 つも残らない）。
        assert!(!worktree.hints().iter().any(|hint| hint.label() == "stage"));
        // 重ならない画面では両方とも残る（Graph に `s` は無い）。
        let graph = keymap.pane(Pane::Graph);
        assert_eq!(graph.action(Key::char('s')), Some(fetch));
        assert_eq!(graph.key_for(fetch), Some(Key::char('s')));
        // 元の `f` は空く。
        assert_eq!(worktree.action(Key::char('f')), None);
    }

    /// **上書き同士は名前の昇順で後ろが勝つ**（`config.keymap` は `HashMap` で順序を持たない。08）。
    #[test]
    fn of_two_overrides_on_one_key_the_later_name_wins() {
        let (keymap, warnings) = Keymap::new(&overrides(&[("pull", "X"), ("fetch", "X")]));
        assert_eq!(
            warnings,
            vec![KeymapWarning::Conflict {
                value: "X".into(),
                winner: "pull",
                loser: "fetch",
            }]
        );
        for &pane in Pane::ALL {
            assert_eq!(
                keymap.pane(pane).action(Key::char('X')),
                Some(Action::Common(CommonAction::Pull)),
                "{pane:?}"
            );
            assert_eq!(
                keymap
                    .pane(pane)
                    .key_for(Action::Common(CommonAction::Fetch)),
                None,
                "{pane:?}"
            );
        }
    }

    /// **`Ctrl-C` は名前でも値でも上書きさせない**（08）。値の側は、捨てないと `?` の一覧が
    /// `Ctrl-C:fetch` と案内し、押すと先頭の [`INTERRUPT`] が受けて終了する。
    #[test]
    fn interrupt_can_be_neither_renamed_nor_taken() {
        let (keymap, warnings) =
            Keymap::new(&overrides(&[("interrupt", "ctrl-q"), ("fetch", "ctrl-c")]));
        assert_eq!(
            warnings,
            vec![
                KeymapWarning::ReservedKey {
                    name: "fetch".into(),
                    value: "ctrl-c".into(),
                },
                KeymapWarning::ReservedAction,
            ]
        );
        let graph = keymap.pane(Pane::Graph);
        let fetch = Action::Common(CommonAction::Fetch);
        // 捨てた上書きは当たらない —— `f` は fetch のまま。
        assert_eq!(graph.key_for(fetch), Some(Key::char('f')));
        assert_eq!(graph.action(Key::ctrl('q')), None);
        assert_eq!(
            graph.key_for(Action::Common(CommonAction::Interrupt)),
            Some(INTERRUPT)
        );
    }

    #[test]
    fn unknown_names_and_unreadable_values_are_warned_and_dropped() {
        let (keymap, warnings) = Keymap::new(&overrides(&[("stagee", "S"), ("stage", "ctrl-S")]));
        assert_eq!(
            warnings,
            vec![
                KeymapWarning::BadKey {
                    name: "stage".into(),
                    value: "ctrl-S".into(),
                    reason: ParseKeyError::CtrlUppercase,
                },
                KeymapWarning::UnknownAction {
                    name: "stagee".into()
                },
            ]
        );
        assert_eq!(
            keymap.pane(Pane::Worktree).action(Key::char('s')),
            Some(Action::Worktree(WorktreeAction::Stage))
        );
        assert_eq!(keymap.pane(Pane::Worktree).action(Key::char('S')), None);
    }

    /// **`AppState::new` が `config.keymap` を当て、警告を通知に積む**（08）。
    #[test]
    fn the_state_carries_the_overridden_table_and_the_warnings_as_notices() {
        let config = Config {
            keymap: overrides(&[("create_pr", "C"), ("nope", "x")]),
            ..Config::default()
        };
        let mut state = AppState::new(RepoInfo::default(), config, FAKE_NOW);
        state.focus = Pane::Prs;
        assert_eq!(
            live_key(&state, Action::Prs(PrsAction::Create)),
            Some(Key::char('C'))
        );
        let notices: Vec<_> = state
            .notices
            .iter()
            .map(|notice| (notice.level, notice.message.as_str()))
            .collect();
        assert_eq!(
            notices,
            vec![(
                crate::app::NoticeLevel::Warn,
                "[keymap] nope: その名前の動作はありません"
            )]
        );
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
        let keymap = Keymap::default();
        let diff = keymap.pane(Pane::Diff);
        assert_eq!(diff.action(Key::char('j')), Some(Action::Down));
        assert_eq!(diff.action(Key::new(KeyCode::Down)), Some(Action::Down));
        assert_eq!(diff.action(Key::new(KeyCode::Up)), Some(Action::Up));
        // **修飾付きは別のキー**（`match key.code()` だったころは `Ctrl-j` も移動した）。
        assert_eq!(diff.action(Key::ctrl('j')), None);
        assert_eq!(diff.action(shift(KeyCode::Down)), None);
        // 別名は案内に出さない。
        assert!(!diff
            .hints()
            .iter()
            .any(|hint| hint.keys().any(|key| key == Key::new(KeyCode::Down))));

        assert_eq!(
            diff.action(Key::char('f')),
            Some(Action::Common(CommonAction::Fetch))
        );
        assert_eq!(diff.action(Key::ctrl('f')), None);
        // ペインのキーは別のペインでは引けない。
        assert_eq!(keymap.pane(Pane::Graph).action(Key::char('s')), None);
    }

    #[test]
    fn the_help_has_the_pane_keys_then_the_keys_that_work_anywhere() {
        let keymap = Keymap::default();
        let rows = keymap.pane(Pane::Graph).help();
        assert_eq!(rows[0], HelpRow::Heading(HelpSection::Pane));
        let anywhere = rows
            .iter()
            .position(|row| *row == HelpRow::Heading(HelpSection::Anywhere))
            .expect("どこでもの見出しがありません");
        assert_eq!(anywhere, 1 + table(Pane::Graph).len());
        assert_eq!(rows.len(), anywhere + 1 + COMMON.len() + PINNED.len());
    }
}
