//! 描画。設計は `docs/design/07-ui.md`。
//!
//! `render` は [`AppState`] のみを参照する（不変ルール 2）。git2 / CLI / HTTP を
//! ここから呼ばない。
//!
//! 本文は幅 100 から左右に割り、狭い端末では `focus` のペインだけを描く（07 の
//! 「レイアウト」）。

pub mod lanes;
pub mod theme;
pub mod time;

use std::borrow::Cow;
use std::fmt::Write as _;
use std::ops::Range;
use std::path::Path;

use crate::{modals, panes};
use mewgit_core::app::{keys, AppState, GraphSelection, Key, KeyCode, KeyHint, Pane, TermSize};
use mewgit_core::auth::{gh, AccountState, DegradedReason};
use mewgit_core::git_read::{short_oid, FileEntry, FileStatus, InProgressOp, Step};
use mewgit_core::github::RateLimit;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Padding, Paragraph, Wrap};
use ratatui::Frame;

/// 1 フレーム描く。
pub fn render(frame: &mut Frame, state: &AppState) {
    let [header, body, notice, hints] = split(frame.area());

    frame.render_widget(header_line(state, header.width), header);
    // **スピナーは同じ領域に右寄せで重ねる。** `Paragraph` は既定のスタイルで
    // 領域を塗り直すだけなので、左に描いたリポジトリ名は消えない。
    if let Some(spinner) = spinner_line(state) {
        frame.render_widget(Paragraph::new(spinner).right_aligned(), header);
    }
    match split_body(body) {
        Some([left, right]) => {
            render_left(frame, left, state);
            // **右列の上は Branches / Stash / PRs のタブ。** 左の選択では入れ替わらないので、
            // 上を先に置いて残りを下のペインに渡す（07 の「レイアウト」）。
            // **組むのはここ 1 回**で、高さの決定と描画の両方に回す。
            let top_pane = panes::TopPane::build(state, right.width);
            let [top, below] = split_right(state, &top_pane, right);
            top_pane.render(frame, top, state);
            render_below(frame, below, state);
        }
        // **狭い端末では `focus` のペインだけを本文いっぱいに描く**（07 の「レイアウト」）。
        // 巡回の候補は幅で変えないので、`Tab` で移った先がそのまま全画面になる。
        None => match Region::of(state.focus) {
            Region::Left => render_left(frame, body, state),
            Region::Top => panes::TopPane::build(state, body.width).render(frame, body, state),
            Region::Below => render_below(frame, body, state),
        },
    }
    frame.render_widget(status_line(state, notice.width), notice);
    frame.render_widget(hint_line(state, hints.width), hints);

    // モーダルは最後。下のペインを覆って描く（07）。
    if let Some(modal) = &state.modal {
        modals::render(frame, frame.area(), modal, state);
    }
}

/// 本文の領域。どのペインもどれか 1 つを差し替えで使う（07 の「レイアウト」）。
#[derive(Clone, Copy)]
enum Region {
    /// Graph か Diff。
    Left,
    /// 右列の上のタブ（Branches / Stash / PRs）。
    Top,
    /// 右列の下（Working tree かコミット詳細）。
    Below,
}

impl Region {
    /// **ペインと領域の対応はここ 1 つ。** 狭い端末で `focus` のペインを描く `render` と、
    /// テストの [`pane_area`] が同じものを読む。2 本にすると、テストは写しのほうを正として
    /// 緑のまま通る。
    fn of(pane: Pane) -> Self {
        match pane {
            Pane::Graph | Pane::Diff => Region::Left,
            Pane::Branches | Pane::Stash | Pane::Prs => Region::Top,
            Pane::Worktree | Pane::CommitDetail => Region::Below,
        }
    }
}

/// 左の領域（Graph か Diff）。**どちらを描くかは `focus` だけで決まる**（07）。
/// 「いまどちらを見せているか」を別に持つと、`focus` と食い違う状態
/// （Diff を見せているのにキーは Graph が受ける）が表現できてしまう。
fn render_left(frame: &mut Frame, area: Rect, state: &AppState) {
    if state.focus == Pane::Diff {
        panes::diff::render(frame, area, state);
    } else {
        panes::graph::render(frame, area, state);
    }
}

/// 右列の下（Working tree かコミット詳細）。**どちらを描くかはグラフの選択だけで
/// 決まる**（07）。`focus` で決めると、Graph にフォーカスした瞬間に右が消える。
fn render_below(frame: &mut Frame, area: Rect, state: &AppState) {
    match state.graph.selection() {
        GraphSelection::Wip => panes::worktree::render(frame, area, state),
        GraphSelection::Commit(line) => panes::commit::render(frame, area, state, &line.commit),
    }
}

/// 本文を左右に割れる最小の幅（07 の「レイアウト」）。
///
/// 下回ると右列を出さず、`focus` のペインを本文いっぱいに描く。60/40 で割ると
/// 80 桁の右列は 32 桁（枠の内側 30 セル）しか無く、PR の行や見出しのタブが切れる。
pub const SPLIT_MIN_WIDTH: u16 = 100;

/// 本文を左右に割る。幅が [`SPLIT_MIN_WIDTH`] に足りなければ `None`（割らない）。
fn split_body(area: Rect) -> Option<[Rect; 2]> {
    (area.width >= SPLIT_MIN_WIDTH).then(|| {
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(area)
    })
}

/// 左の領域。割らないときは本文そのもの。
fn left_area(body: Rect) -> Rect {
    split_body(body).map_or(body, |[left, _]| left)
}

/// 右列を上のタブ（Branches / Stash / PRs）と、その下のペイン（Working tree / コミット詳細）に
/// 割る。
///
/// **割る位置を決めるのは上のペイン自身**（`panes::TopPane::height`）。組んだ行から
/// 出すので、ここで高さを書くと規則が 2 本になる。
fn split_right(state: &AppState, top: &panes::TopPane, right: Rect) -> [Rect; 2] {
    let rows = top.height(state, right.height);
    Layout::vertical([Constraint::Length(rows), Constraint::Min(0)]).areas(right)
}

/// ペインの枠。
///
/// **フォーカスのあるペインは枠を強調する**（07）。どちらがキーを受けるか判らないと、
/// `s` や `x` が別のペインに飛んだように見える。**線の形（太線）と色の両方を変える**
/// —— 修飾だけだと、太字を描かない端末で差が出ない。枠の内側の大きさは強調の有無で
/// 変わらない（太線も 1 セル）ので、レイアウトの計算は `focused` を偽で呼んでよい。
pub fn pane_block(title: impl Into<Line<'static>>, focused: bool) -> Block<'static> {
    // **`&'static str` も受ける。** グラフの枠は行数を測るのに `runtime::layout` から
    // ループのたびに組まれるので、そこで毎回 `String` を確保しない。
    // **`Line` も受ける**のは、右上のタブが見出しの中で 2 枚を描き分けるため
    // （`panes::right_tabs`。07 の「右上のタブ」）。
    let block = Block::default().borders(Borders::ALL).title(title.into());
    if focused {
        block
            .border_type(BorderType::Thick)
            .border_style(theme::focused())
    } else {
        block
    }
}

/// 折り返す `Paragraph` の右に空ける桁数。
///
/// **ratatui の折り返しは 1 セルだけはみ出す。** `WordWrapper` は行が幅を超えるかを
/// **次の文字を足す前**に見る（`>=` 比較）ので、最後の 1 文字が全角だと行が
/// `幅 + 1` セルで確定する（`ratatui-widgets` 0.3.2 の `reflow.rs`）。`Paragraph` は
/// 枠より後に描き、`render_line` は領域の幅で切らないので、**はみ出した 1 セルが
/// 枠の縦線を上書きしてその行だけ箱が開く**（同 `paragraph.rs`）。1 桁空けておけば
/// そこに落ちる。はみ出しが 1 セルで収まるのは、最後の文字を足す前の幅が `幅 - 1`
/// 以下だから（07 の「モーダル」）。
///
/// **折り返す描画の側に置く。** はみ出すのは `Wrap` の経路だけで、折り返さない一覧や
/// diff は領域の幅で切れる。枠（`pane_block`）に載せると、折り返さないペインまで
/// 1 桁ずつ削る。
const WRAP_MARGIN: u16 = 1;

/// 折り返す `Paragraph`。右に [`WRAP_MARGIN`] を空けて描く。
///
/// **`Wrap` を付けるのはここだけ。** 余白を忘れた折り返しが 1 本でも残ると、そこで
/// 枠が開く。行数も要るなら [`wrapped`]。
pub fn wrapping<'a>(text: impl Into<Text<'a>>) -> Paragraph<'a> {
    Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .block(wrap_margin())
}

/// [`WRAP_MARGIN`] を載せた枠なしの [`Block`]。描く側と測る側が同じものを使う。
fn wrap_margin() -> Block<'static> {
    Block::default().padding(Padding::right(WRAP_MARGIN))
}

/// 折り返す `Paragraph` と、それを幅 `width` に描いたときの行数（07 の「モーダル」）。
///
/// **測る `Paragraph` はここでしか組まない。** 測るときと描くときで `Wrap` の設定と
/// 余白がずれないようにするため。`Paragraph::line_count` は枠の上下を行数に足す一方、
/// 左右ぶんを幅からは引かない（`ratatui-widgets` 0.3.2 の `paragraph.rs`）ので、
/// 余白の [`Block`] を付けたまま `width` で測ると、余白の無い幅で折り返した行数が返り、
/// 本文の末尾が切れる。**測る幅も `Block::inner` から出す**（[`WRAP_MARGIN`] を
/// 手で引くと、余白を変えたときに測る側だけ古くなる）。外側の枠のぶんは呼び出し側が
/// `Block::inner` から出す。
pub fn wrapped<'a>(text: impl Into<Text<'a>>, width: u16) -> (Paragraph<'a>, u16) {
    let inner = wrap_margin().inner(Rect::new(0, 0, width, 1)).width;
    let paragraph = wrapping(text);
    let rows = u16::try_from(paragraph.line_count(inner)).unwrap_or(u16::MAX);
    (paragraph, rows)
}

/// 一覧のうち描く行の範囲。`must_show` が窓に入るまでずらす。
///
/// **カーソル行 1 行ではなく「必ず見せたい塊」を受ける。** カーソルが 1 行を指す
/// ペイン（Worktree）は `cursor..cursor + 1` を渡し、塊を指すペイン（Diff のハンク）は
/// 見出しから本文の末尾までを渡す。カーソル行だけを見る形にすると、ハンクの見出しが
/// ちょうど窓の下端に来て**選んだハンクの本文が 1 行も見えない**（07 の「Diff ペイン」）。
///
/// 塊が窓より高いときは先頭を優先して残りを切る。
///
/// **スクロール位置を状態に持たない。** グラフ（01 の `scroll`）と違ってカーソルが
/// 下端に貼り付く癖が残るが、受け入れた（07 の「Worktree ペイン」）。右列のペインは
/// 高さが右上のタブの行数で動くので、`Msg::Layout` で運ぶと一覧の到着ごとに流れる。
pub fn window(len: usize, must_show: Range<usize>, height: usize) -> Range<usize> {
    if height == 0 {
        return 0..0;
    }
    // 塊の末尾が入る位置まで下げ、塊の先頭より下へは行かない。
    let start = must_show
        .start
        .min(must_show.end.saturating_sub(height))
        .min(len);
    start..(start + height).min(len)
}

/// `must_show` が入る窓を決め、**窓の中の行だけを `row` で組んで**描く。
///
/// 行の組み立てはペインごとに違うが、**切り出して描くところまでが 1 つ**。
/// [`window`] だけを共有すると、その戻り値の使い方が各ペインに写る。
///
/// **組んでから捨てない**（07 の「見えない行の組み立て」）。描画は `Tick` で毎秒 4 回
/// 回るので、500 ファイルのコミットや長い diff の全行を組んでは大半を捨てることになる。
/// 呼ぶ側は行の数と、添字から 1 行を組む関数を渡す。**添字を数える規則は呼ぶ側の 1 本**
/// —— 見出しを挟むペインは、描く行の軽い記述（借用だけで `String` を持たない）を先に
/// 1 回で並べ、`len` とカーソルの行を同じ走査で出す。
///
/// **例外は `modals::edit_area`** で、そちらは [`window`] だけを使う（編集中の
/// モーダルはみな、あの 1 本を通る）。編集中はカーソルが枠の右端を越えるので横に
/// ずらす必要があり、ここに横位置を足すと、「長い行は右で切れるだけ」を意図している
/// diff と一覧まで一般化することになる。前提の成り立たない利用者を共有基盤に
/// 押し込まない（`.claude/rules/rust-coding.md`）。
///
/// **折り返さない**（`Wrap` を付けない）。diff は桁の揃いが意味を持ち、一覧のパスも
/// 折り返すと行とカーソルの対応が崩れる。
pub fn render_rows<'a>(
    frame: &mut Frame,
    area: Rect,
    len: usize,
    must_show: Range<usize>,
    row: impl FnMut(usize) -> Line<'a>,
) {
    let window = window(len, must_show, area.height as usize);
    frame.render_widget(Paragraph::new(window.map(row).collect::<Vec<_>>()), area);
}

/// 組み終えた行を [`render_rows`] で描く。
///
/// **全部組むしかない一覧のための口。** 右上のタブは割く高さを行数から出し、PRs は桁の
/// 幅を一覧ぜんぶから決める（`panes::TopPane`）。picker と通知の履歴の一覧は本数に
/// 上限がある（候補・通知 100 件）。どれも行を組まずに数を出す形にすると、数える規則が
/// 2 本になる。
pub fn render_built(
    frame: &mut Frame,
    area: Rect,
    mut lines: Vec<Line<'_>>,
    must_show: Range<usize>,
) {
    let len = lines.len();
    render_rows(frame, area, len, must_show, |at| {
        std::mem::take(&mut lines[at])
    });
}

/// UTF-8 と ASCII の字を選ぶ。`config.graph.ascii` に従う（08）。
///
/// **選ぶところは 1 本。** キーは `[graph]` の下にあるが、既定は locale から決まる
/// ので意味は「この端末が非 ASCII の字を出せるか」。グラフのレーンだけに効かせると、
/// 同じ端末で Branches ペインの矢印だけが化ける（07）。
pub fn pick_charset(glyphs: (&'static str, &'static str), ascii: bool) -> &'static str {
    if ascii {
        glyphs.1
    } else {
        glyphs.0
    }
}

/// ペインを描く領域。テスト用。
///
/// **座標を手で書かせない。** 実際の割り方（[`split`] / [`split_body`] /
/// [`split_right`]）をそのまま通すので、割り方を変えたときにテストだけが古い桁を
/// 見て緑のまま無意味になることがない。
#[cfg(test)]
pub fn pane_area(pane: Pane, size: TermSize, state: &AppState) -> Rect {
    let [_, body, _, _] = split(Rect::new(0, 0, size.width, size.height));
    // **狭い端末では `focus` のペインしか出ていない。** 出ていないペインに本文の
    // `Rect` を返すと、そこに描かれた別のペインを読んで `contains` が黙って通る。
    let Some([left, right]) = split_body(body) else {
        assert_eq!(
            state.focus, pane,
            "幅 {} では {pane:?} は出ていない（focus のペインだけを描く）",
            size.width
        );
        return body;
    };
    // 右列は上のタブとその下に割れている。割る位置も実際の関数から出す。
    let [top, below] = split_right(state, &panes::TopPane::build(state, right.width), right);
    // **同じ領域を差し替えで使うペインには同じ `Rect` を返す**（Graph と Diff、
    // Branches / Stash / PRs。07 の「レイアウト」「右上のタブ」）。
    match Region::of(pane) {
        Region::Left => left,
        Region::Top => top,
        Region::Below => below,
    }
}

/// 一覧に割く行数。必要なぶん、ただし `available` の半分まで（07）。
///
/// **上限が要るのは、一覧が伸びると同じ領域を分け合う相手が潰れるため。**
/// コミット詳細ではメッセージが、右列では Working tree / コミット詳細が相手になる。
/// 逆に固定で割かないのは、行が少ないときに空行を抱えるため。
///
/// **2 つのペインが同じ規則を使うので 1 本にする。** 写すと、片方だけ上限を変えた
/// ときに気付けない。
pub fn capped_rows(needed: usize, available: u16) -> u16 {
    u16::try_from(needed).unwrap_or(u16::MAX).min(available / 2)
}

/// ファイルの表示名。rename は元の名前も出す。
///
/// `FileEntry`（Worktree ペイン）と `FileDiff`（Diff ペイン）は別の型だが、パスの
/// 出し方は同じ。2 本に分けると片方だけ rename を落とす。
pub fn path_label(path: &Path, old_path: Option<&Path>) -> String {
    match old_path {
        Some(old) => format!("{} -> {}", old.display(), path.display()),
        None => path.display().to_string(),
    }
}

/// 非同期の読み取りを待っているあいだの文言。
///
/// **ペインごとに書かない。** グラフ・ワーキングツリー・diff・コミット詳細・
/// ファイル一覧・PR が同じ状態を出すので、散らすと文言を変えたときに揃わなくなる。
/// 何を読んでいるかは枠のタイトルが示す。
pub const LOADING: &str = "読み込み中…";

/// 変更が 1 つも無いときの文言。ワーキングツリーとコミットのファイル一覧が使う。
pub const NO_CHANGES: &str = "変更はありません";

/// コミットのファイル一覧が出せないときの文言。
///
/// **読み手は 2 つ。** CommitDetail ペイン（一覧そのもの）と Diff ペイン（一覧が
/// 決まらないと対象が決まらない）。分けて書くと、届く前・失敗・空の 3 通りのうち
/// どれかが片方だけ古くなる —— 実際、Diff 側に書き直したときに失敗の腕が落ちて、
/// **一覧の読み込みに失敗すると Diff ペインが永久に「読み込み中…」を出していた**。
pub fn files_placeholder(state: &AppState) -> &'static str {
    if state.graph.fresh_files().is_some() {
        // 届いていて空。`--allow-empty` のコミット。
        NO_CHANGES
    } else if state.graph.has_files_failed() {
        "ファイル一覧を読み込めませんでした"
    } else {
        LOADING
    }
}

/// ファイル一覧の 1 行。`M src/main.rs`、rename は `R old.txt -> new.txt`。
///
/// **Worktree ペインとコミット詳細のファイル一覧が同じものを使う**（07）。区画の
/// 見出しを挟むかどうかはペインごとに違うが、行そのものが 2 本あると種別の字だけが
/// 片方で古くなる。
///
/// **`width` に入らないパスは左を切る**（[`ellipsize_left`]。07 の「Worktree ペイン」）。
/// 右で切るとファイル名が消える。
pub fn file_row(entry: &FileEntry, width: u16) -> String {
    let lead = format!(" {} ", mark(entry.status));
    let room = width.saturating_sub(u16::try_from(lead.len()).unwrap_or(u16::MAX));
    let label = path_label(&entry.path, entry.old_path.as_deref());
    lead + &ellipsize_left(&label, room)
}

/// `path` を `cells` セルに収める。溢れるときは先頭の成分から落として `…/` にする。
///
/// **`/` の区切りで落とす**（07 の「Worktree ペイン」）。どこまで落としてもファイル名が
/// 入らないときは、書記素で左を落として `…` + 末尾にする。測るのは [`ellipsize`] と
/// 同じく書記素ごと。
fn ellipsize_left(path: &str, cells: u16) -> Cow<'_, str> {
    if fits(path, cells) {
        return Cow::Borrowed(path);
    }
    // `…/` の後ろに残す形を、長いほうから試す。
    for (at, _) in path.match_indices('/') {
        let rest = &path[at..];
        if fits(rest, cells.saturating_sub(1)) && at > 0 {
            return Cow::Owned(format!("…{rest}"));
        }
    }
    if cells == 0 {
        return Cow::Borrowed("");
    }
    // `…` の 1 セルを先に取っておき、末尾から詰める。
    let mut left = usize::from(cells - 1);
    let span = Span::raw(path);
    let graphemes: Vec<&str> = span
        .styled_graphemes(Style::default())
        .map(|grapheme| grapheme.symbol)
        .collect();
    let mut start = graphemes.len();
    for (index, symbol) in graphemes.iter().enumerate().rev() {
        let w = Span::raw(*symbol).width();
        if w > left {
            break;
        }
        left -= w;
        start = index;
    }
    Cow::Owned(format!("…{}", graphemes[start..].concat()))
}

/// 変更種別の 1 文字。`git status --short` と同じ字を使う。
fn mark(status: FileStatus) -> char {
    match status {
        FileStatus::Added => 'A',
        FileStatus::Modified => 'M',
        FileStatus::Deleted => 'D',
        FileStatus::Renamed => 'R',
        FileStatus::Copied => 'C',
        FileStatus::Typechange => 'T',
        FileStatus::Untracked => '?',
        FileStatus::Conflicted => 'U',
    }
}

/// 画面をヘッダ / 本文 / フッター 2 行（通知・キーヒント）に割る。
///
/// **フッターは固定 2 行**（07 の「ステータスバー」）。**中身で高さを変えない** ——
/// 変えると [`graph_rows`] が動き、通知が 1 件出るだけでログの読み直しが走る（01 の
/// 「グラフの行」）。
///
/// **フッターの中は算術で割る。** `Layout` に 1 行ずつ割らせると、**高さが 2 に足りない
/// 端末で後ろ（キーヒント）が 0 行になり、`q:quit` が画面から消える** —— 07 は
/// 「幅が足りないときに落とすのはペインのキーのほう」と決めているのに、**高さでは
/// その守りが効いていなかった**（リサイズ中の一瞬に踏む。80x3 で実測）。譲るのは
/// 通知のほう。
fn split(area: Rect) -> [Rect; 4] {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(FOOTER_ROWS),
    ])
    .areas(area);
    // 末尾の 1 行をキーヒントに、残りを通知に渡す（0 行なら描かれない）。
    let hints = Rect {
        y: footer.bottom().saturating_sub(1),
        height: footer.height.min(1),
        ..footer
    };
    let notice = Rect {
        height: footer.height.saturating_sub(1),
        ..footer
    };
    [header, body, notice, hints]
}

/// フッターの行数。**ここ 1 箇所だけが決める**（内訳は [`split`] が算術で割る）。
const FOOTER_ROWS: u16 = 2;

/// フッターの 2 行（1 行目が通知、2 行目がキーヒント）。テスト用。
///
/// **行番号を手で書かせない**（[`pane_area`] と同じ理由）。`height - 2` を各テストが
/// 持つと、フッターの高さや置き場所を変えたときに一斉に嘘になる。
#[cfg(test)]
pub fn footer(term: TermSize) -> [Rect; 2] {
    let [_, _, notice, hints] = split(Rect::new(0, 0, term.width, term.height));
    [notice, hints]
}

/// グラフの行に使える行数。
///
/// `app` がスクロール位置を決めるのに要る（01 の「グラフの行」）。core にレイアウトを
/// 知らせないよう、実際に描くのと同じ `split` と枠から出して `Msg::Layout` で渡す。
///
/// **引数は端末サイズだけ。** `&AppState` を丸ごと受けると、あとから状態を見る余地が
/// 残る。M1 ではコミット詳細をグラフの下部に展開していたので「詳細を出すか」も
/// 引数にあり、`Enter` を押すだけで行数が変わっていた。詳細を右ペインへ移して
/// この依存は消えている（07 の「CommitDetail ペイン」）。
pub fn graph_rows(term: TermSize) -> usize {
    let [_, body, _, _] = split(Rect::new(0, 0, term.width, term.height));
    panes::graph::area(left_area(body)).height as usize
}

/// ヘッダ。リポジトリと、解決できていればアカウントを出す。
///
/// **幅を受けるのは残量のため**（下の `[api: …]`）。同じ領域に重ねて描くスピナーの
/// ぶんを引かないと、「余っているか」を数えられない。
fn header_line(state: &AppState, width: u16) -> Paragraph<'_> {
    let mut spans = vec![Span::styled(
        repo_label(state),
        Style::default().add_modifier(Modifier::BOLD),
    )];

    if let Some(target) = state.repo.target.resolved() {
        spans.push(Span::raw(format!("{SEP}{}/{}", target.owner, target.repo)));
    }

    if let Some(account) = account_span(&state.account) {
        spans.push(Span::raw(SEP));
        spans.push(account);
    }

    if let Some(limit) = state.prs.rate_limit() {
        let quota = rate_limit_span(limit);
        // **入らなければ出さない。** 右端のスピナーは同じ領域に重ねて描くので、
        // はみ出した `[api: 4984]` は **`[api: 49` として残り、桁の減った数が
        // そのまま読める**（実測。07 のヘッダに幅の表がある）。**切れた数は
        // 「無い」より悪い。**
        // **譲るのは新しく足したほう** —— `owner/repo` と `[gh: …]` が食われる形は
        // 足す前からあり、落とす順序を全体で決めるのは M6（07）。
        let spinner = spinner_line(state).as_ref().map_or(0, Line::width);
        if quota_fits(&spans, &quota, spinner, width) {
            spans.push(Span::raw(SEP));
            spans.push(quota);
        }
    }

    Paragraph::new(Line::from(spans))
}

/// 残量を足しても、右端のスピナーとのあいだに [`SEP`] が残るか。
///
/// **数えるのは余白まで。** **`contains` だけの見張りでは余白ゼロが通る**（#73）ので、
/// 「入った」の基準を隣の語と地続きにならないところに置く。
fn quota_fits(spans: &[Span<'_>], quota: &Span<'_>, spinner: usize, width: u16) -> bool {
    // **区切りもセル幅で数える。** `SEP.len()` はバイト数なので、いまの `"  "` では
    // 同じ値になるが、区切りを非 ASCII にした瞬間に組む側とずれる。
    let sep = Span::raw(SEP).width();
    let left: usize = spans.iter().map(Span::width).sum();
    left + sep + quota.width() + sep + spinner <= width as usize
}

/// 1 行に並べる要素の区切り。**1 セルだと隣の語と地続きに見える。**
///
/// 組む側（`Span::raw(SEP)`）と数える側（[`quota_fits`]）が同じものを読む ——
/// 幅だけを定数にすると、区切りを広げたときに数えるほうが 2 のまま残る。
///
/// **ヘッダとフッターで同じものを使う** —— ヘッダの `[api: …]` の手前と、キーヒントの
/// 群（ペインのキー / 共通キー / `q:quit`）のあいだ。1 行の中で 2 通りの空きがあると、
/// 幅を詰める判断がどちらの側の話か読めなくなる。
const SEP: &str = "  ";

/// ヘッダの `[api: …]`。GitHub API の残量（07 の「ヘッダ」）。
///
/// **1 度でも応答が届いたら出し続ける。** 閾値を割ったときだけ出す形は、**出ていない
/// あいだ減り具合が見えない** —— 一覧 1 本と PR ごとの扇形で 1 回の引き直しに
/// 100 往復を超えることがあり（05 の実測）、出たときにはもう遅い。
///
/// **乏しいときだけ色を付ける。** 割合で見るのは [`RateLimit::is_low`] で、上限は
/// トークンの種類で変わる。
///
/// **上限は添えない。** `[api: 4984/5000]` は 18 セルで、80 桁では `owner/repo` が
/// 現実的な長さでもスピナーに重なる（実測。07 のヘッダ）。
///
/// **`[gh: …]` の degraded とは同時に出ない。** degraded なら [`PrState::forget`] が残量を
/// 落としており、`Resolving` では一度も引いていない —— 出るのは
/// `[gh: login @ host]` の隣だけ。
fn rate_limit_span(limit: RateLimit) -> Span<'static> {
    let text = format!("[api: {}]", limit.remaining);
    if limit.is_low() {
        Span::styled(text, theme::warn())
    } else {
        Span::raw(text)
    }
}

/// ヘッダの `[gh: …]`。まだ決まっていなければ `None`（07）。
///
/// **見出しは `gh`。** 誰として GitHub に喋るかを出す欄で、その誰かを決めるのも
/// トークンを出すのも `gh` CLI（04）。degraded の理由も `gh` の話が多いので、
/// [`degraded`] の側では `gh` を繰り返さない（`[gh: gh がありません]` にしない）。
///
/// **`Resolving` のあいだは何も出さない。** 走っていることは右端のスピナー
/// （`account`）が出しているので、左にも「解決中」を出すと同じことを 2 箇所で言う。
/// 既定リモートが無いなどで一度も頼まないリポジトリ（`TargetState::Unavailable`）も
/// ここに来るので、GitHub と関係のないリポジトリには何も出ない（01）。
fn account_span(account: &AccountState) -> Option<Span<'_>> {
    match account {
        // **`Disabled` でも何も出さない**（07 の「ヘッダ」）。自分で切った設定を毎
        // フレーム言い返すことになるうえ、1 行しかないヘッダで `owner/repo` と
        // スピナーの予算を食う。`A` を押せば理由が通知に出る。
        AccountState::Resolving | AccountState::Disabled => None,
        AccountState::Resolved(account) => Some(Span::raw(format!(
            "[gh: {} @ {}]",
            account.login, account.host
        ))),
        AccountState::Degraded(reason) => Some(Span::styled(degraded_label(reason), theme::warn())),
    }
}

/// ヘッダに出す degraded の 1 行（`[gh: …]` ごと）。
///
/// **どの degraded にも `A` を添える。** `A` は `f` / `p` / `P` と同じ共通キーなので、
/// どのペインで見ていても押せる（#53 の「案内にキーを書いたら、その案内が出ている
/// 画面でそのキーが効くかを数える」）。**選べない degraded でも押す価値がある** ——
/// ヘッダが 1 行に詰めて落とした内訳（`NoAccess` の `attempts`、`Unusable` の
/// `message`）と外での直し方を出すのは `Modal::AccountPicker` だけで（04 の
/// 「内訳を出すのは `Modal::AccountPicker`」）、そこへ行く手掛かりが画面に無いと
/// **出す先を用意したのに誰も辿り着けない**。
///
/// 文言が 2 つに割れるのは「押すと何が起きるか」が違うから。判定は
/// [`DegradedReason::candidates`] の 1 本で、選ばせる側（モーダルのタイトル・
/// キーの案内・`Enter`）と同じものを見る。
///
/// **`[gh: …]` ごと組むのは、長さの見張り（`every_degraded_reason_fits_the_header`）に
/// 出荷される形をそのまま渡すため。** `degraded` だけを渡すと、キーのぶんが
/// 見張りから外れる。
fn degraded_label(reason: &DegradedReason) -> String {
    let hint = if reason.candidates().is_empty() {
        "。A で詳細"
    } else {
        "。A で選ぶ"
    };
    format!("[gh: {}{hint}]", degraded(reason))
}

/// degraded の理由を 1 行にする。
///
/// **診断だけを書く。** 押すキーを添えるのは [`degraded_label`] で、ここは
/// 「何が起きているか」だけを返す。
///
/// **1 行の予算に合わせて詰めてある。** リポジトリ名とスピナーと同じ 1 行を
/// 分け合うので（07 のヘッダ）、`Modal::AccountPicker` は**同じ事実を別の文言**で
/// 書く（あちらは箱の中で折り返せる）。揃えると、ここを詰めるたびにあちらが痩せる。
/// **内訳も出さない。** `NoAccess` の `attempts` は 1 件ずつ理由を持っているが、
/// 並べるのは `Modal::AccountPicker`（04）。
pub(crate) fn degraded(reason: &DegradedReason) -> Cow<'static, str> {
    match reason {
        DegradedReason::GhMissing => "ありません".into(),
        // 要求版は定数から引く。`GhTooOld` は値として持っていない（04）。
        DegradedReason::GhTooOld { found } => {
            format!("{found} は古い（{} 以上）", gh::REQUIRED).into()
        }
        // **`message` は出さない。** `gh` が起動できない理由は 1 行に収まらないので、
        // 出す先は `A` で開く `Modal::AccountPicker`（04）。
        DegradedReason::Unusable { .. } => "実行できません".into(),
        DegradedReason::NotLoggedIn { host } => format!("{host} に未ログイン").into(),
        DegradedReason::ConfiguredUnavailable { login, .. } => {
            format!("{login} のトークンがありません").into()
        }
        // **「権限が無い」と読ませない。** 回線が無いだけの回もここに来る（04）。
        DegradedReason::NoAccess { attempts, .. } => {
            format!("どれも届きません（{} 件）", attempts.len()).into()
        }
        DegradedReason::Ambiguous { candidates } => {
            format!("1 つに絞れません（{} 件）", candidates.len()).into()
        }
    }
}

/// スピナーの駒。[`Msg::Tick`](mewgit_core::app::Msg::Tick) ごとに 1 つ進む。
const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

/// ヘッダ右端の「いま何が走っているか」。走っているものが無ければ `None`。
///
/// **駒と一緒に説明も出す**（`⠋ fetch status`）。07 は当初これをステータスバーに
/// 置いていたが、フッターの 1 行目は通知が幅いっぱい使い（01 の「失敗の案内文」）、
/// 2 行目はキーヒントが埋める（07 の「ステータスバー」）。**2 行になっても譲る席は
/// 増えていない** —— ジョブが 1 つ増えるたびに案内かヒントが末尾から削られるので、
/// 空いているヘッダへ移した。
fn spinner_line(state: &AppState) -> Option<Line<'static>> {
    // **`Tick` を 1 つまたいで走っているジョブだけを描く**（`Job::since`。07 の
    // 「ヘッダ」）。1 フレームで終わるもの（2 秒ごとの probe、小さなリポジトリの
    // status）まで描くと、そのたびに駒が 1 フレームだけ光る。
    let mut labels = state
        .jobs
        .iter()
        .filter(|job| job.since < state.tick)
        .map(|job| job.label)
        .peekable();
    labels.peek()?;
    // 駒は `Tick` の回数で選ぶ。`jobs` の中身では選ばない —— 走っているものが
    // 変わらないあいだ止まって見える。
    let frame = SPINNER[(state.tick % SPINNER.len() as u64) as usize];
    // 説明は `&'static str` なので、組むのは `Vec<Span>` 1 本だけ。描画は入力が
    // 無くても `Tick` で回る（1 秒に 4 回）ので、中間の `String` を作らない。
    let mut spans = vec![Span::raw(frame)];
    for label in labels {
        spans.push(Span::raw(" "));
        spans.push(Span::raw(label));
    }
    Some(Line::from(spans))
}

/// フッターの 1 行目。進行中操作のバッジと、直近の通知を積まれた時刻を添えて出す。
///
/// **幅いっぱい使ってよい**（キーヒントは 2 行目）。時刻の 9 セルを引いた残りに
/// 案内文が収まることは `app` 側が見張る（01 の「失敗の案内文」）。**キー入力で
/// 下ろした通知は出さない**（[`AppState::shown_notice`]）が、**バッジは下ろさない**
/// —— 進行中操作は continue / abort するまで終わらない（07 の「ステータスバー」）。
///
/// **溢れたら末尾を `…` にする**（07 の「ステータスバー」）。予算を見張れるのは
/// 固定の文言だけで、パスやホスト名を埋めた文言には上限が無い。黙って切ると、
/// 切れたことが画面から判らない。**バッジが出ているあいだは、そのぶん短く切れる**
/// （予算は縮めない。07）。
///
/// 出すのは先頭行だけ。toml のパースエラーのように複数行のメッセージがあり、そのまま
/// 流すと改行が消えて読めなくなる。全文は `L` の履歴で読む（`modals::notice_log`）。
fn status_line(state: &AppState, width: u16) -> Paragraph<'_> {
    let mut lead = Vec::new();
    if let Some(op) = &state.worktree.snapshot.progress {
        lead.push(Span::styled(
            format!(" {} ", progress_label(op)),
            theme::in_progress(),
        ));
        lead.push(Span::raw(" "));
    }
    let Some(notice) = state.shown_notice() else {
        return Paragraph::new(Line::from(lead));
    };
    lead.push(Span::styled(
        format!("{} ", time::clock(notice.at)),
        theme::muted(),
    ));
    Paragraph::new(lead_line(lead, &notice.message, width))
}

/// 進行中操作のバッジの文言。git の語を大文字で（07 の「ステータスバー」）。
///
/// **止まった理由（`edit` など）は出さない** —— 検出側が持っていない（02）。
fn progress_label(op: &InProgressOp) -> String {
    let step = |step: &Option<Step>| step.map(|step| format!("{}/{}", step.current, step.total));
    let (name, detail) = match op {
        InProgressOp::Rebase { step: s, .. } => ("REBASE", step(s)),
        InProgressOp::Am { step: s } => ("AM", step(s)),
        InProgressOp::CherryPick { oid } => ("CHERRY-PICK", oid.as_ref().map(short_oid)),
        InProgressOp::Revert { oid } => ("REVERT", oid.as_ref().map(short_oid)),
        InProgressOp::Merge { oid } => ("MERGE", Some(short_oid(oid))),
    };
    match detail {
        Some(detail) => format!("{name} {detail}"),
        None => name.to_string(),
    }
}

/// `lead`（時刻など）の後ろに `message` の 1 行目を置き、`width` に収まるように
/// 末尾を `…` にした 1 行。
///
/// **ステータスバーと通知の履歴の一覧が同じ組み方をする**（07）。前置きの幅を引いて
/// 残りに詰める規則を 2 本に写すと、溢れ方を直したときに片方だけ古くなる。
pub(crate) fn lead_line<'a>(mut lead: Vec<Span<'a>>, message: &'a str, width: u16) -> Line<'a> {
    let used: usize = lead.iter().map(Span::width).sum();
    let room = width.saturating_sub(u16::try_from(used).unwrap_or(u16::MAX));
    lead.push(Span::raw(ellipsize(first_line(message), room)));
    Line::from(lead)
}

/// タブ幅。`\t` を次のタブストップまで開く。
const TAB_WIDTH: usize = 8;

/// タブを開くのに使う空白。[`TAB_WIDTH`] 桁ぶんを切り出して使う。
const SPACES: &str = "        ";

/// `text` を `out` に足す。`\t` は [`TAB_WIDTH`] 桁のタブストップまで空白に開き、
/// `\r` は落とす。桁は `\n` で 0 に戻す。
///
/// **ratatui はタブを描かない**（幅 0 の字として落ちる）。1 個の空白に潰すと
/// インデントが崩れ、diff では上下の行の対応が、git の stderr では見出しと字下げした
/// ファイル名の区別が読めなくなる。**Diff ペインと通知の履歴が同じ規則を使う** ——
/// 同じ git の出力を 2 通りに描かない。
///
/// `\r` は落とさないと CRLF のファイルで制御文字が画面に出る。桁は文字数で数えるので、
/// タブの手前に全角文字があるとタブストップがずれる（タブはほぼインデントに使われる
/// ので、実際には手前は空白だけになる）。
pub fn push_expanded(out: &mut String, text: &str) {
    let mut column = 0;
    for ch in text.chars() {
        match ch {
            '\t' => {
                let pad = TAB_WIDTH - column % TAB_WIDTH;
                out.push_str(&SPACES[..pad]);
                column += pad;
            }
            '\r' => {}
            '\n' => {
                out.push(ch);
                column = 0;
            }
            ch => {
                out.push(ch);
                column += 1;
            }
        }
    }
}

/// `text` を `cells` セルに収める。溢れるときは末尾を `…` に置き換える。
fn ellipsize(text: &str, cells: u16) -> Cow<'_, str> {
    if fits(text, cells) {
        return Cow::Borrowed(text);
    }
    if cells == 0 {
        return Cow::Borrowed("");
    }
    // `…` の 1 セルを先に取っておく。
    let mut left = usize::from(cells - 1);
    let mut kept = String::new();
    // **書記素ごとに測る。** 描くときの単位がそれで、文字ごとに足すと異体字セレクタ付きの
    // 絵文字（`❤️` は 2 文字で 2 セル、文字ごとだと 1）を少なく数えて `…` が溢れる。
    for grapheme in Span::raw(text).styled_graphemes(Style::default()) {
        let w = Span::raw(grapheme.symbol).width();
        if w > left {
            break;
        }
        left -= w;
        kept.push_str(grapheme.symbol);
    }
    kept.push('…');
    Cow::Owned(kept)
}

/// 最初の行。改行が無ければ全体。
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

/// フッターの 2 行目。いま押せるキー（07 の「ステータスバー」）。
///
/// **「いま押せるか」を答えるのは core**（[`keys::live`]）。モーダル表示中に全部が
/// 効かなくなるのは `update_key` の振り分けの話で、ここで `modal.is_some()` を見ると
/// その知識の写しが `ui` にできる。
fn hint_line(state: &AppState, width: u16) -> Paragraph<'static> {
    let Some(pane) = keys::live(state) else {
        return Paragraph::new("");
    };
    // **`Line::raw(String)` に渡さない。** あちらは `Cow::Owned` を `Span` に割るときに
    // 中身をもう 1 本作り直す（`ratatui-core` の `cow_to_spans` で確認）。
    Paragraph::new(Line::from(Span::raw(hint_text(pane, width))))
}

/// `width` に収まるヒントの並び。`ペインのキー  ?:help q:quit` で、**落とすのはペインの
/// キーの末尾から**。[`keys::PINNED`] は落とさない（07 の「ステータスバー」）。
///
/// **収まるかは組んだ字から数える。** 幅だけを別に算術で出すと、組む側と数える側で
/// 規則が 2 本になる（`.claude/rules/rust-coding.md` の「出力を 1 本にしても入力が 2 本なら結果はずれる」）。
/// そのぶん幅が足りないときは落とす回数だけ組み直す（描画は 250ms ごと）。
fn hint_text(pane: &[KeyHint], width: u16) -> String {
    let pinned = joined(keys::PINNED);
    for keep in (1..=pane.len()).rev() {
        let text = format!("{}{SEP}{pinned}", joined(&pane[..keep]));
        if fits(&text, width) {
            return text;
        }
    }
    pinned
}

/// 案内をキーの空白で繋いだ字（`j/k:移動 Enter:差分`）。
fn joined(hints: &[KeyHint]) -> String {
    hints.iter().fold(String::new(), |mut out, hint| {
        if !out.is_empty() {
            out.push_str(KEY_GAP);
        }
        push_hint(&mut out, hint);
        out
    })
}

/// 案内 1 つの字（`j/k:移動`）を `out` に足す。フッターの 2 行目が使う。
fn push_hint(out: &mut String, hint: &KeyHint) {
    push_keys(out, hint);
    out.push(':');
    out.push_str(hint.label());
}

/// 案内のキーの字（`j/k`）を `out` に足す。
///
/// **表（`app::keys`）はキーしか持たない**ので、字にするのはここ（01）。フッターの 2 行目と
/// `?` の一覧（`modals::help`）が使う。**モーダルは自前の `HINT` を持っている**
/// （`modals::*`。あちらは箱の中で予算が違う）ので、綴りが 2 通りある（07）。
pub(crate) fn push_keys(out: &mut String, hint: &KeyHint) {
    for (i, key) in hint.keys().iter().enumerate() {
        if i > 0 {
            out.push('/');
        }
        push_key(out, key);
    }
}

/// キー 1 つの字。
///
/// **`Key` 全体を見る**（`KeyCode` だけでは `Ctrl-C` と `c` が同じ字になる）。
fn push_key(out: &mut String, key: &Key) {
    let mods = key.mods();
    if mods.ctrl {
        out.push_str("Ctrl-");
    }
    if mods.alt {
        out.push_str("Alt-");
    }
    match key.code() {
        // **`Ctrl` と組む文字は大文字で書く**（`Ctrl-C`）。端末の慣習で、小文字だと
        // `c` のキーと見分けにくい。
        KeyCode::Char(c) if mods.ctrl => out.push(c.to_ascii_uppercase()),
        // 空白のままだと案内の区切りと見分けられない。
        KeyCode::Char(' ') => out.push_str("Space"),
        KeyCode::Char(c) => out.push(c),
        KeyCode::Enter => out.push_str("Enter"),
        KeyCode::Esc => out.push_str("Esc"),
        KeyCode::Tab => out.push_str("Tab"),
        KeyCode::BackTab => out.push_str("Shift+Tab"),
        // **表に入っているのは上の字だけ**（`app::keys`）。ほかを足したら字もここに足す
        // —— 書き忘れると `Debug` の字が出るので、画面で気付ける。
        other => {
            let _ = write!(out, "{other:?}");
        }
    }
}

/// 案内どうしの空白。
const KEY_GAP: &str = " ";

/// 幅 `width` に収まるか。
///
/// **`Line` を通さない。** あちらは幅を測るためだけに `Vec<Span>` を 1 本確保する
/// （`modals::cursor_col` と同じ理由）。
fn fits(text: &str, width: u16) -> bool {
    Span::raw(text).width() <= width as usize
}

/// リポジトリの表示名。パスの末尾を使い、取れなければパスそのもの。
fn repo_label(state: &AppState) -> String {
    let path = &state.repo.path;
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        degraded_label, ellipsize, ellipsize_left, file_row, hint_text, pane_area, progress_label,
        push_expanded, render_rows, window, wrapped, SPACES, SPLIT_MIN_WIDTH, TAB_WIDTH,
    };
    use crate::test_util::{
        draw, footer_area, hint_bar, notice_bar, state_at, state_showing_diff, state_with_status,
        tab_to, terminal_with, SPLIT_NARROWEST,
    };
    use mewgit_core::app::{
        keys, update, AppState, Cmd, JobKind, Key, KeyCode, Modal, Msg, NoticeLevel, Pane,
        TargetState, TermSize,
    };
    use mewgit_core::auth::{AccountSource, AccountState, DegradedReason, RepoTarget};
    use mewgit_core::git_exec::GitOp;
    use mewgit_core::git_read::{short_oid, InProgressOp, Step};
    use mewgit_core::github::GithubError;
    use mewgit_core::test_util::{
        fake_account, fake_ambiguous, fake_no_access, fake_oid, fake_rate_limit, fake_snapshot,
        fetch_pr_details,
    };
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier};
    use ratatui::widgets::Widget;

    #[test]
    fn a_file_row_puts_the_status_letter_before_the_path() {
        // **書式の見張りはここ 1 本。** 2 つのペインが同じ関数を通るので、ペインの
        // 側でそれぞれ字を見ると、種別を足したときにどちらのテストも通ってしまう。
        use mewgit_core::git_read::FileStatus;
        use mewgit_core::test_util::fake_entry;

        let row = |status| file_row(&fake_entry("a.txt", status), 80);

        assert_eq!(row(FileStatus::Added), " A a.txt");
        assert_eq!(row(FileStatus::Modified), " M a.txt");
        assert_eq!(row(FileStatus::Deleted), " D a.txt");
        assert_eq!(row(FileStatus::Copied), " C a.txt");
        assert_eq!(row(FileStatus::Typechange), " T a.txt");
        assert_eq!(row(FileStatus::Untracked), " ? a.txt");
        assert_eq!(row(FileStatus::Conflicted), " U a.txt");

        // rename は元の名前も出す（`path_label`）。
        let mut renamed = fake_entry("new.txt", FileStatus::Renamed);
        renamed.old_path = Some("old.txt".into());
        assert_eq!(file_row(&renamed, 80), " R old.txt -> new.txt");
    }

    #[test]
    fn a_file_row_that_does_not_fit_keeps_the_end_of_the_path() {
        // **右で切るとファイル名が消える**（07 の「Worktree ペイン」）。種別の 3 セルを
        // 除いた残りに、`…/` と入るだけの末尾の成分を置く。
        use mewgit_core::git_read::FileStatus;
        use mewgit_core::test_util::fake_entry;

        let entry = fake_entry(
            "crates/mewgit-tui/src/panes/worktree.rs",
            FileStatus::Modified,
        );
        // 幅 24 なら種別の 3 セルを除いて 21 セル。`…/panes/worktree.rs`（19 セル）は入り、
        // `…/src/panes/worktree.rs`（23 セル）は入らない。
        assert_eq!(file_row(&entry, 24), " M …/panes/worktree.rs");
        assert_eq!(file_row(&entry, 21), " M …/worktree.rs");
        // 入る幅では切らない。
        assert_eq!(
            file_row(&entry, 42),
            " M crates/mewgit-tui/src/panes/worktree.rs"
        );
    }

    #[test]
    fn a_left_cut_falls_back_to_graphemes_when_the_name_alone_is_too_long() {
        // ファイル名だけでも入らなければ、区切りを諦めて末尾を書記素で残す。
        assert_eq!(ellipsize_left("dir/abcdefgh.rs", 6), "…gh.rs");
        // 区切りが無いパスも同じ。
        assert_eq!(ellipsize_left("abcdefgh.rs", 4), "….rs");
        // **全角は 2 セルで数える。** 文字数で数えると `…` が枠を越える。
        assert_eq!(ellipsize_left("ディレクトリ/ファイル", 7), "…ァイル");
        assert_eq!(ellipsize_left("a/b", 0), "");
    }

    #[test]
    fn a_left_cut_of_a_rename_drops_the_old_name_first() {
        // 1 本の文字列として左から切る（07 で受け入れた穴）。`R` の印で rename は読める。
        assert_eq!(
            ellipsize_left("src/old_name.rs -> src/new_name.rs", 20),
            "…/new_name.rs"
        );
    }

    #[test]
    fn render_rows_builds_only_the_rows_in_the_window() {
        // **組んでから捨てない**（07 の「見えない行の組み立て」）。行数のテストだけでは、
        // 全部組んで窓の外を捨てる形に戻しても通るので、呼ばれた添字を数える。
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 4)).unwrap();
        let mut built = Vec::new();
        terminal
            .draw(|frame| {
                render_rows(frame, frame.area(), 1000, 500..501, |at| {
                    built.push(at);
                    ratatui::text::Line::raw(at.to_string())
                });
            })
            .unwrap();

        assert_eq!(built, vec![497, 498, 499, 500]);
    }

    #[test]
    fn the_spaces_used_for_a_tab_are_wide_enough() {
        // `SPACES[..pad]` は `pad` が `TAB_WIDTH` まで伸びる。短いと panic する。
        assert_eq!(SPACES.len(), TAB_WIDTH);
    }

    #[test]
    fn a_tab_stop_restarts_on_every_line() {
        // 桁を `\n` で戻さないと、2 行目以降のタブストップが前の行の長さでずれる
        // （git の stderr は複数行を 1 本の文字列で持つ）。**前の行は 8 の倍数に
        // しない** —— 8 桁ちょうどだと、戻さなくても次のタブストップが同じになる。
        let mut out = String::new();
        push_expanded(&mut out, "error:\n\ta.txt\r");
        assert_eq!(out, "error:\n        a.txt");
    }

    #[test]
    fn a_wrapped_line_that_just_overflows_keeps_the_border() {
        // **ratatui の折り返しは 1 セルはみ出す**（[`super::WRAP_MARGIN`]）。踏むには
        // `内側 - 1` セルの後ろに全角 1 文字を置く —— 足す前の幅が内側に届いていないので
        // そのまま足し、行が `内側 + 1` セルで確定する。ペインで折り返すのは CommitDetail
        // だけなので、枠付きのペインと同じ形（`pane_block` の内側に描く）で見る。
        let area = Rect::new(0, 0, 12, 4);
        let block = super::pane_block("t", false);
        let inner = block.inner(area);
        let text = format!("{}あ", "x".repeat(usize::from(inner.width) - 1));
        let (paragraph, rows) = wrapped(text, inner.width);

        let mut buffer = Buffer::empty(area);
        block.render(area, &mut buffer);
        paragraph.render(inner, &mut buffer);

        // **枠の記号ではなく、内側の右端が空いているかを見る。** はみ出した全角は内側の
        // 最後の桁から始まり、2 セル目が枠に掛かるが、`Buffer` の上では枠の桁の記号が
        // `│` のまま残る（実端末では重なって描かれる）ので、枠を見ても通ってしまう。
        let last = inner.right() - 1;
        assert_eq!(buffer[(last, inner.y)].symbol(), " ", "全角が枠に掛かった");
        // **はみ出す手前で折れたことも見る** —— 1 行に収まったなら踏めていない。
        assert_eq!(rows, 2);
    }

    #[test]
    fn wrapped_counts_the_line_that_word_wrapping_adds() {
        // 30 桁の語 3 つで 92 桁。幅 58 で割った切り上げなら 2 行だが、語の途中では
        // 折れないので実際は 3 行になる。ここを見積もると本文の末尾が切れる。
        let words = format!("{} {} {}", "a".repeat(30), "b".repeat(30), "c".repeat(30));

        assert_eq!(wrapped(words, 58).1, 3);
    }

    #[test]
    fn wrapped_is_zero_rows_at_zero_width() {
        // リサイズの途中で幅 0 が来る。`Constraint::Max(0)` に落ちるだけで済ませる。
        assert_eq!(wrapped("何か", 0).1, 0);
    }

    #[test]
    fn the_window_follows_a_single_row_past_the_bottom() {
        // 窓に入っているあいだは先頭から。入らなくなったら見えるまでずらす。
        assert_eq!(window(10, 0..1, 4), 0..4);
        assert_eq!(window(10, 3..4, 4), 0..4);
        assert_eq!(window(10, 4..5, 4), 1..5);
        assert_eq!(window(10, 9..10, 4), 6..10);
        // 行が窓より少ないときは全部出す。
        assert_eq!(window(2, 1..2, 4), 0..2);
        // 高さ 0 でも破綻しない（リサイズ中に起きる）。
        assert_eq!(window(10, 5..6, 0), 0..0);
    }

    #[test]
    fn the_window_shows_the_whole_block_not_just_its_first_row() {
        // ハンクの見出しだけを窓に入れると、選んだハンクの本文が 1 行も見えない。
        // 塊の末尾まで入る位置までずらす。
        assert_eq!(window(20, 5..9, 4), 5..9);
        assert_eq!(window(20, 8..12, 4), 8..12);
        // 塊が窓より高いときは先頭を優先して残りを切る。
        assert_eq!(window(20, 8..18, 4), 8..12);
    }

    /// ペインを描く領域の左上。**Graph と Diff は同じ領域**なので同じ座標になる（07）。
    fn corner(pane: Pane, size: TermSize, state: &AppState) -> (u16, u16) {
        let area = pane_area(pane, size, state);
        (area.x, area.y)
    }

    #[test]
    fn the_focused_pane_is_the_only_one_with_a_thick_border() {
        // どちらがキーを受けるか判らないと、`s` や `x` が別のペインに飛んだように見える。
        // **線の形と色の両方を見る**（07）。形は色も修飾も出ない端末のための強調で、
        // 色だけを見ると `BorderType` を落としても通る。
        let size = SPLIT_NARROWEST;
        // 巡回の候補はグラフの選択で変わる（01）。WIP 側で回す。
        //
        // **差分で表せる行を 1 つ置く。** 変更が無いと `Diff` が候補から外れ（01 の
        // 「Pane と Modal」）、この見張りが**黙って 1 ペインぶん減る** —— 下の
        // 「Graph と Diff は同じ領域」の腕がまさに踏まれなくなる（#40 と同じ形）。
        let base = || state_with_status(fake_snapshot(&[], &["a.txt"], &[]));
        let candidates = base().focus_candidates();
        assert!(candidates.contains(&Pane::Diff), "{candidates:?}");
        for &focus in candidates {
            let mut state = base();
            state.focus = focus;
            // **`focus` を置いたら `reseat_focus` を通す。** 右上のタブは `focus` と
            // 揃っていなければならず、揃えるのは `update` の出口の 1 箇所（01 の
            // 「右上のタブ」）。直に置くだけだと `focus == Prs` なのに Branches を
            // 描いている**実物では出ない画面**を見張ることになる（隣の
            // `the_left_area_swaps_between_the_graph_and_the_diff` と同じ理由）。
            state.reseat_focus();
            let terminal = terminal_with(&state, size);
            let buffer = terminal.backend().buffer();

            for &pane in candidates {
                let at = corner(pane, size, &state);
                let cell = &buffer[at];
                // **`pane == focus` では見られない。** Graph と Diff は同じ領域なので、
                // Diff にフォーカスがあるとき「Graph の枠」の座標には Diff の枠が
                // 描かれている。太いかどうかはその領域にフォーカスがあるかで決まる。
                let focused = at == corner(focus, size, &state);
                // **期待値は `theme::focused()` から取らない。** 取ると、あちらから色を
                // 消したときに期待値も一緒に消えて通る（`/code-review` が変異で拾った）。
                let (symbol, fg) = if focused {
                    ("┏", Color::Cyan)
                } else {
                    ("┌", Color::Reset)
                };
                let why = format!("focus={focus:?} pane={pane:?}");
                assert_eq!(cell.symbol(), symbol, "{why}");
                assert_eq!(cell.fg, fg, "{why}");
                assert_eq!(cell.modifier.contains(Modifier::BOLD), focused, "{why}");
            }
        }
    }

    #[test]
    fn the_left_area_swaps_between_the_graph_and_the_diff() {
        let state = state_at("/repo");
        let text = draw(&state, SPLIT_NARROWEST).join("\n");
        assert!(text.contains("Graph"), "{text}");

        // **`focus` を直に置かない。** 対象の無い `Diff` は候補から外れる（01）ので、
        // 直に置くと実物では出ない画面を見張ることになる。
        let state = state_showing_diff(fake_snapshot(&[], &["a.txt"], &[]));
        let text = draw(&state, SPLIT_NARROWEST).join("\n");

        // 差し替えであって分割ではない。両方出すと左の 60% ではどちらも読めない。
        assert!(!text.contains("Graph"), "{text}");
        assert!(text.contains("Diff"), "{text}");
        // **右のワーキングツリーは出したまま。** どのファイルを見ているかが一覧の側でも
        // 判るようにする（07）。
        assert!(text.contains("Working tree"), "{text}");
    }

    #[test]
    fn the_right_area_swaps_between_the_worktree_and_the_commit_detail() {
        // 右ペインの中身はグラフの選択だけで決まる。`focus` では決めない（07）。
        let state = crate::test_util::state_with_commits(&["first"]);
        let text = draw(&state, SPLIT_NARROWEST).join("\n");
        assert!(text.contains("Working tree"), "{text}");
        assert!(!text.contains("Commit "), "{text}");

        let state = crate::test_util::state_on_first_commit(&["first"]);
        let text = draw(&state, SPLIT_NARROWEST).join("\n");

        assert!(!text.contains("Working tree"), "{text}");
        assert!(text.contains("Commit "), "{text}");
    }

    #[test]
    fn the_right_area_follows_the_selection_even_while_the_graph_has_focus() {
        // `focus` で決めると、Graph にフォーカスした瞬間に右が消える（07）。
        let mut state = crate::test_util::state_on_first_commit(&["first"]);
        state.focus = Pane::Graph;

        let text = draw(&state, SPLIT_NARROWEST).join("\n");

        assert!(text.contains("Commit "), "{text}");
    }

    /// 本文の 1 行目（ヘッダの直下）に並ぶ、ペインの枠の左上の桁。
    fn pane_corners(state: &AppState, size: TermSize) -> Vec<u16> {
        let terminal = terminal_with(state, size);
        let buffer = terminal.backend().buffer();
        (0..size.width)
            .filter(|&x| matches!(buffer[(x, 1)].symbol(), "┌" | "┏"))
            .collect()
    }

    #[test]
    fn the_body_splits_from_the_minimum_width_and_not_below() {
        // **境目の両側を見る。** 99 桁で割ると右列は 39 桁（内側 37 セル）になり、
        // 割らない側の 80 桁だけを見ても閾値がずれたことは判らない。
        let state = state_at("/repo");
        let narrow = TermSize {
            width: SPLIT_MIN_WIDTH - 1,
            ..SPLIT_NARROWEST
        };

        assert_eq!(pane_corners(&state, narrow), [0]);
        // 左（Graph）と右列の上（Branches）の 2 枚。
        assert_eq!(pane_corners(&state, SPLIT_NARROWEST).len(), 2);
    }

    #[test]
    fn a_narrow_terminal_shows_every_candidate_alone_across_the_full_width() {
        // **巡回の候補は幅で変えない**（07 の「レイアウト」）。狭い端末では `Tab` で
        // 移った先が本文いっぱいに出る。候補のどれかが描かれないと、80 桁の端末から
        // そのペインへ届かない。
        let size = TermSize::default();
        assert!(size.width < SPLIT_MIN_WIDTH, "{size:?}");
        let wip = || state_with_status(fake_snapshot(&[], &["a.txt"], &[]));
        let commit = || crate::test_util::state_on_first_commit(&["first"]);
        // **回す範囲を先に確かめる。** 候補は production の集合から導くので、
        // 狙ったペインが落ちると見張りが黙って減る（`.claude/rules/testing.md`）。
        let wip_candidates = wip().focus_candidates();
        let commit_candidates = commit().focus_candidates();
        for pane in [Pane::Branches, Pane::Worktree, Pane::Diff] {
            assert!(wip_candidates.contains(&pane), "{wip_candidates:?}");
        }
        assert!(commit_candidates.contains(&Pane::CommitDetail));

        for (base, candidates) in [
            (&wip as &dyn Fn() -> AppState, wip_candidates),
            (&commit, commit_candidates),
        ] {
            for &pane in candidates {
                let mut state = base();
                // **`Tab` を通す。** 直に置くと右上のタブが `focus` と揃わない。
                tab_to(&mut state, pane);
                let terminal = terminal_with(&state, size);
                let buffer = terminal.backend().buffer();
                let why = format!("focus={pane:?}");

                // 枠は左端から右端まで 1 枚で、フォーカスの太線。**太線を見るのは、
                // 描いたのが `focus` のペインであることの証拠を兼ねるため** —— 別の
                // ペインを描くと、そのペインの枠はフォーカスが無いので細線になる。
                assert_eq!(buffer[(0, 1)].symbol(), "┏", "{why}");
                assert_eq!(buffer[(size.width - 1, 1)].symbol(), "┓", "{why}");
                assert_eq!(pane_corners(&state, size), [0], "{why}");
            }
        }
    }

    #[test]
    fn tab_on_a_narrow_terminal_swaps_the_graph_for_the_branches() {
        // 隠したペインへ届く手段は `Tab` だけ。押す前と後で、描かれる中身が
        // 丸ごと入れ替わることを見る。
        let size = TermSize::default();
        let mut state = state_at("/repo");
        let before = draw(&state, size).join("\n");
        assert!(before.contains("Graph"), "{before}");
        assert!(!before.contains("Branches"), "{before}");

        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Tab)));
        assert_eq!(state.focus, Pane::Branches);
        let after = draw(&state, size).join("\n");

        assert!(after.contains("Branches"), "{after}");
        assert!(!after.contains("Graph"), "{after}");
    }

    /// 解決済みのアカウントを持つ状態。
    fn state_with_account(account: AccountState) -> AppState {
        let mut state = state_at("/repo");
        state.account = account;
        state
    }

    fn resolved_account() -> AccountState {
        AccountState::Resolved(fake_account("user_shortcode"))
    }

    /// ヘッダの `needle` が始まるセルの前景色。
    ///
    /// **バッファを見る。** `draw` の文字列にはスタイルが残らないので、黄色で出して
    /// いるかは色を読まないと判らない（`row_is_reversed` と同じ理由でここに置く）。
    fn header_color_at(state: &AppState, needle: &str) -> Color {
        header_color_at_size(state, needle, TermSize::default())
    }

    /// 幅を指定して [`header_color_at`] と同じことをする。
    fn header_color_at_size(state: &AppState, needle: &str, size: TermSize) -> Color {
        let line = header(state, size);
        let col = line
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} がヘッダにありません: {line:?}"));
        // バイト位置をセル列として使うので、手前が ASCII であることだけ確かめる。
        assert!(line[..col].is_ascii(), "{line:?}");
        let terminal = crate::test_util::terminal_with(state, size);
        terminal.backend().buffer()[(col as u16, 0)].fg
    }

    #[test]
    fn header_shows_the_resolved_account() {
        let line = header(&state_with_account(resolved_account()), TermSize::default());

        assert!(
            line.contains("[gh: user_shortcode @ github.com]"),
            "header: {line:?}"
        );
    }

    #[test]
    fn header_says_nothing_while_the_account_is_unresolved() {
        // **`Resolving` は出さない**（07）。走っていることは右端のスピナーが出しており、
        // 既定リモートの無いリポジトリはここから動かない（01）。
        let line = header(
            &state_with_account(AccountState::Resolving),
            TermSize::default(),
        );

        assert!(!line.contains("[gh:"), "header: {line:?}");
    }

    #[test]
    fn a_degraded_account_is_shown_in_the_warn_style() {
        let state = state_with_account(AccountState::Degraded(DegradedReason::GhMissing));

        let line = header(&state, TermSize::default());
        // 押すキーの案内は `degraded_label` が足す（`every_degraded_account_says_
        // which_key_to_press`）。ここで見るのは理由の文言と色。
        assert!(line.contains("[gh: ありません。"), "header: {line:?}");
        assert_eq!(header_color_at(&state, "[gh:"), Color::Yellow);
        // 解決できているアカウントは色を付けない（注意を引くのは degraded だけ）。
        assert_eq!(
            header_color_at(&state_with_account(resolved_account()), "[gh:"),
            Color::Reset
        );
    }

    #[test]
    fn every_degraded_reason_fits_the_header() {
        // **7 つを 1 つずつ回す。** 理由を足したときに、文言の無い腕と、ヘッダから
        // はみ出す長さの両方をここで落とす（通知の予算と同じ形の見張り。01）。
        //
        // **出荷される形のヘッダで測る**（`/code-review` が拾った）。`[gh: …]` が
        // 出るのは `Cmd::ResolveAccount` を出したリポジトリだけで、それを出すのは
        // `target_resolved` —— **`owner/repo` が必ず左隣にある**。走っているジョブの
        // スピナーも右端に重なる。素の `state_at("/repo")` で測ると、実際には
        // 存在しない短いヘッダを見張ることになる。
        //
        // **幅は 100。** 07 の目標のレイアウトがその幅で、`< 100` は M6 のタスク。
        //
        // **`owner/repo` と login の長さは fixture 任せ。** どちらも上限が無いので
        // 「いちばん長い形」を作れない —— 31 桁の `owner/repo` を実測したら
        // `A` の案内が落ちた（07 のヘッダに残した）。ここが見張るのは
        // 「現実的な名前で、目標幅に収まって隙間が残るか」まで。
        let reasons = [
            DegradedReason::GhMissing,
            DegradedReason::GhTooOld {
                found: "2.80.0".to_string(),
            },
            DegradedReason::Unusable {
                message: "起動できませんでした".to_string(),
            },
            DegradedReason::NotLoggedIn {
                host: "github.com".to_string(),
            },
            DegradedReason::ConfiguredUnavailable {
                login: "taro".to_string(),
                source: AccountSource::GitConfig,
                message: "no oauth token found".to_string(),
            },
            fake_no_access(Vec::new()),
            // **候補を空にしない。** 空だと `A` の案内が付かず、**出荷される
            // `Ambiguous` の形（いちばん長い）を測らないまま緑になる**。
            fake_ambiguous(&["user_shortcode", "taro"]),
        ];
        let size = TermSize {
            width: 100,
            height: 24,
        };
        for reason in reasons {
            // **末尾に空白まで見る。** `contains` だけだと、スピナーと隙間なく
            // 並んだ状態（余白ゼロ）で通ってしまい、**リポジトリ名が 1 桁伸びた
            // 瞬間に案内が落ちる**のを見張れない（`/code-review` が拾った。
            // 実測で `Ambiguous` がちょうどその状態だったので文言を詰めた）。
            let want = format!("{} ", degraded_label(&reason));
            let mut state = shipping_header_state();
            state.account = AccountState::Degraded(reason);
            let line = header(&state, size);
            assert!(line.contains(&want), "はみ出している: {line:?} / {want}");
        }
    }

    #[test]
    fn every_degraded_account_says_which_key_to_press() {
        // **選べない degraded でも `A` を案内する。** ヘッダが落とした内訳と外での
        // 直し方を出す画面は `Modal::AccountPicker` だけなので（04）、案内が無いと
        // 出す先を用意したのに誰も辿り着けない。**文言は押すと何が起きるかで割る。**
        let size = TermSize {
            width: 100,
            height: 24,
        };
        let mut state = shipping_header_state();

        state.account = AccountState::Degraded(fake_ambiguous(&["taro", "taro_acme"]));
        let pickable = header(&state, size);
        assert!(pickable.contains("A で選ぶ"), "header: {pickable:?}");

        state.account = AccountState::Degraded(fake_no_access(Vec::new()));
        let shown = header(&state, size);
        assert!(shown.contains("A で詳細"), "header: {shown:?}");
    }

    /// 実際に `[gh: …]` が出るときのヘッダの状態。
    ///
    /// 長めのリポジトリ名（`claude-code`）と `owner/repo`、それに走っているジョブの
    /// スピナーを一緒に出す。**この 3 つは `[gh: …]` と同じ 1 行を分け合う。**
    fn shipping_header_state() -> AppState {
        let mut state = state_at("/tmp/claude-code");
        state.repo.target = TargetState::Resolved(RepoTarget {
            host: "github.com".to_string(),
            port: None,
            owner: "anthropics".to_string(),
            repo: "claude-code".to_string(),
        });
        let _ = state.issue(Cmd::LoadBranches);
        state
    }

    /// 残量まで出ている出荷形の状態（アカウント解決済み + 扇形が走っている最中）。
    ///
    /// **`[api: …]` が出るのはこの形だけ。** degraded では [`PrState::forget`] が
    /// 残量を落としており、`Resolving` では一度も引いていない。**スピナーも
    /// 扇形のもの**（`pr checks`）—— 数が動くのはそのあいだだけなので、同時に
    /// 出るのはこの組み合わせ（#73 の「出荷される形を測る」）。
    fn state_with_quota(remaining: u32) -> AppState {
        let mut state = state_fetching_details();
        let epoch = state.prs.epoch();
        let _ = update(
            &mut state,
            Msg::RateLimitSeen {
                epoch,
                seen: fake_rate_limit(remaining, 1789703295),
            },
        );
        state
    }

    /// 残量が届く前の同じ形。**足したことで他が変わっていないか**を比べる相手。
    fn state_fetching_details() -> AppState {
        let mut state = shipping_header_state();
        state.jobs.finish(JobKind::LoadBranches);
        state.account = resolved_account();
        // **世代は `fetch_pr_details` に書かせる**（`PrState::forget` が進めたあとの
        // 状態でも「古い返信」を組めるようにするため）。答えは届けないので、ここが
        // 要るのは「扇形が走っている」ことだけ —— スピナーが `pr checks` になる。
        let ask = fetch_pr_details(&state, &[(1, "sha", Err(GithubError::NotFound))]);
        let _ = state.issue(ask);
        // `Tick` を 1 つまたぐ。またがないジョブはスピナーに出ない（`Job::since`）。
        // 1 周（駒の数）進めるので、描かれる駒は `tick` が 0 のときと同じ。
        state.tick += super::SPINNER.len() as u64;
        state
    }

    #[test]
    fn the_header_shows_the_api_quota_next_to_the_account() {
        // **出荷形で測る。** 左に `owner/repo` と `[gh: …]`、右端に扇形のスピナーが
        // 重なる（07 のヘッダ）。**左の区切りごと綴じる** —— `SEP` を落とすと
        // `github.com][api: 4984]` と地続きになるが、`[api: 4984]` だけを見る形では
        // それが通る（実測 2026-09-18、`SEP` を外して 226 本とも緑だった）。
        //
        // **右の余白はここでは見張れない。** 幅 100 ではスピナーとのあいだが 6 セル
        // 余っているので、`quota_fits` が取る隙間を 2 セルから 0 に緩めても通る。
        // 隙間を見張るのは `a_quota_that_does_not_fit_is_dropped_whole_not_cut` の
        // 境目（95 / 96）。
        let size = TermSize {
            width: 100,
            height: 24,
        };
        let line = header(&state_with_quota(4984), size);

        assert!(
            line.contains("[gh: user_shortcode @ github.com]  [api: 4984]"),
            "header: {line:?}"
        );
        assert!(line.contains("pr checks"), "スピナーが出ていない: {line:?}");
    }

    #[test]
    fn the_quota_turns_warn_when_little_is_left() {
        // **割合で見る**（`RateLimit::is_low`）。上限はトークンの種類で変わるので、
        // 固定の本数だと上限 60 のトークンでは最初から点く。
        let size = TermSize {
            width: 100,
            height: 24,
        };

        assert_eq!(
            header_color_at_size(&state_with_quota(4984), "[api:", size),
            Color::Reset
        );
        assert_eq!(
            header_color_at_size(&state_with_quota(500), "[api:", size),
            Color::Yellow,
            "上限の 10% は乏しい"
        );
    }

    #[test]
    fn the_header_says_nothing_about_the_quota_before_the_first_response() {
        // **応答が 1 度も届いていなければ出さない。** レートリミットを切った GHES は
        // ヘッダごと返さないので、ここから動かない（05）。
        let mut state = shipping_header_state();
        state.account = resolved_account();

        let line = header(&state, TermSize::default());

        assert!(!line.contains("[api:"), "header: {line:?}");
    }

    #[test]
    fn a_quota_that_does_not_fit_is_dropped_whole_not_cut() {
        // **切れた数は「無い」より悪い。** 右端にはスピナーが重なるので、はみ出した
        // `[api: 4984]` は `[api: 49` として残り、**桁の減った数がそのまま読める**
        // 形になる（#89 の「桁を足した瞬間に予算が自分へ返る」の、返り先が
        // 自分の数だった版）。
        //
        // **幅は実測で選ぶ**（2026-09-18、扇形のスピナー `pr checks` が走っている
        // 最中）—— `anthropics/claude-code` + `user_shortcode` の左群は 70 セル、
        // スピナーは 11 セル。**80 では測れない** —— 残量の始まる 72 桁目が
        // スピナーの下（69 桁目から）に丸ごと隠れるので、**門を外しても画面が
        // 変わらない**（この見張りは最初その幅で書いて、変異で素通りした）。
        // **切れた数が読めるのは幅 90〜92**（`[api: 4`／`[api: 49`／`[api: 498`）。
        // 幅 84〜89 は `[api: ` までで桁が 1 つも出ず、幅 93〜95 は数が全部読める
        // （うち 93 だけは `]` も食われ、94〜95 は隙間が 2 セルに足りないだけ）。
        // 出るのは**幅 96 以上**（隙間 2 セルを両側に取るため。`quota_fits`）。
        let narrow = TermSize {
            width: 90,
            height: 24,
        };
        let line = header(&state_with_quota(4984), narrow);

        assert!(!line.contains("[api:"), "切れた数を出した: {line:?}");
        // **足したことで他の要素が 1 セルも動かない。** 幅が足りない回は、残量が
        // 届く前とまったく同じヘッダになる。
        assert_eq!(
            line,
            header(&state_fetching_details(), narrow),
            "残量を落とすのに他がずれた"
        );

        // **境目の両側を踏む。** 幅 90 の側だけでは「隙間 2 セル」を見張れない ——
        // `quota_fits` から右の `sep` を外すと門は幅 94 へ下がるが、幅 90 では
        // 相変わらず落ち、幅 96 では相変わらず出るので**両方とも通る**
        // （実測 2026-09-18、外して 226 本とも緑）。1 セル手前の 95 を踏む。
        let just_short = TermSize {
            width: 95,
            height: 24,
        };
        assert!(
            !header(&state_with_quota(4984), just_short).contains("[api:"),
            "スピナーとの隙間が 1 セルしか無いのに出した"
        );

        let wide = TermSize {
            width: 96,
            height: 24,
        };
        assert!(
            header(&state_with_quota(4984), wide).contains("[api: 4984]"),
            "余地があるのに出さない"
        );
    }

    #[test]
    fn header_shows_the_repository_name() {
        let line = header(&state_at("/home/user/mewgit"), TermSize::default());
        assert!(line.contains("mewgit"), "header: {line:?}");
    }

    /// 描いたヘッダ（最上行）。
    ///
    /// **取り出しはここ 1 本。** `test_util::notice_bar` と同じ理由で、ヘッダの位置や
    /// 高さが変わる（M6 の狭い幅での畳み方）と、各テストが持っている `[0]` が一斉に嘘になる。
    fn header(state: &AppState, size: TermSize) -> String {
        draw(state, size)[0].clone()
    }

    /// ジョブを 1 つ走らせた状態で描いたヘッダ。
    ///
    /// **`tick` の 1 周あとで描く** —— `Tick` をまたがないジョブはスピナーに出ない
    /// （`Job::since`）。1 周ずらしても駒は `tick` のときと同じ。
    fn header_while_fetching(tick: u64) -> String {
        let mut state = state_at("/repo");
        let _ = state.issue(Cmd::Exec(GitOp::Fetch {
            remote: None,
            prune: false,
        }));
        state.tick = tick + super::SPINNER.len() as u64;
        header(&state, TermSize::default())
    }

    #[test]
    fn the_header_spins_while_a_job_runs() {
        // 走っているものが無ければ出さない。出しっぱなしだと「何か動いている」の
        // 意味が消える。
        let idle = header(&state_at("/repo"), TermSize::default());
        assert!(!idle.contains('⠋'), "header: {idle:?}");

        let header = header_while_fetching(0);

        // 駒と一緒に「何が」走っているかも出す（07 のヘッダ）。
        assert!(header.contains("⠋ fetch"), "header: {header:?}");
        // リポジトリ名は消えない（同じ領域に右寄せで重ねている）。
        assert!(header.contains("repo"), "header: {header:?}");
    }

    #[test]
    fn a_job_spins_only_after_it_outlives_a_tick() {
        // 1 フレームで終わるもの（2 秒ごとの probe、小さなリポジトリの status）を描くと、
        // そのたびに駒が 1 フレームだけ光る（`Job::since`。07 の「ヘッダ」）。
        let mut state = state_at("/repo");
        let _ = state.issue(Cmd::Probe);
        let fresh = header(&state, TermSize::default());
        assert!(!fresh.contains("probe"), "header: {fresh:?}");

        // 同じジョブが `Tick` をまたいだら出す。肯定側も同じ状態で見る。後から
        // 発行したものは、まだ出ない。
        state.tick += 1;
        let _ = state.issue(Cmd::LoadStatus);
        let later = header(&state, TermSize::default());
        assert!(later.contains(" probe"), "header: {later:?}");
        assert!(!later.contains("status"), "header: {later:?}");
    }

    #[test]
    fn the_spinner_frame_follows_the_tick() {
        // `Tick` ごとに駒が変わること。変わらないと「止まっている」と読める。
        assert_ne!(header_while_fetching(0), header_while_fetching(1));
        // 一周して戻る。溢れても止まらない（`tick` は `wrapping_add`）。
        assert_eq!(header_while_fetching(0), header_while_fetching(8));
    }

    /// 通知を 1 件だけ積んで描いた通知の行。既定サイズ（80x24）で見る。
    /// 時刻は `FAKE_NOW`（10:03:26）。
    fn notice_bar_with(message: &str) -> String {
        let mut state = state_at("/repo");
        state.push_notice(NoticeLevel::Error, message.to_string(), None);
        notice_bar(&state, TermSize::default())
    }

    #[test]
    fn a_notice_leads_with_the_time_it_was_raised() {
        let bar = notice_bar_with("push に失敗しました");
        assert!(
            bar.trim_start_matches(['│', ' '])
                .starts_with("10:03:26 push に失敗しました"),
            "notice: {bar:?}"
        );
    }

    #[test]
    fn a_notice_taken_down_by_a_key_is_not_drawn() {
        let mut state = state_at("/repo");
        state.push_notice(NoticeLevel::Warn, "古い通知".to_string(), None);
        assert!(notice_bar(&state, TermSize::default()).contains("古い通知"));

        let _ = update(&mut state, Msg::Key(mewgit_core::app::Key::char('j')));

        assert!(
            !notice_bar(&state, TermSize::default()).contains("古い通知"),
            "下ろした通知が出ています"
        );
    }

    #[test]
    fn ellipsize_cuts_at_the_cell_budget_and_marks_the_cut() {
        assert_eq!(ellipsize("abc", 3), "abc");
        assert_eq!(ellipsize("abcd", 3), "ab…");
        // 全角は 2 セル。半端な 1 セルは空けて、`…` を跨がせない。
        assert_eq!(ellipsize("あいう", 5), "あい…");
        assert_eq!(ellipsize("あいう", 4), "あ…");
        assert_eq!(ellipsize("abc", 0), "");
        // 異体字セレクタ付きの絵文字は 1 書記素で 2 セル（文字ごとに足すと 1）。
        let heart = "a❤️bcd";
        assert_eq!(ratatui::text::Span::raw(heart).width(), 6);
        assert_eq!(ellipsize(heart, 4), "a❤️…");
        assert_eq!(ellipsize(heart, 3), "a…");
    }

    /// `pane` にフォーカスを置いて描いたヒントの行。
    fn hint_bar_of(pane: Pane, width: u16) -> String {
        // **`focus` を置いたら `reseat_focus` を通す**（隣の
        // `the_focused_pane_is_the_only_one_with_a_thick_border` と同じ理由）——
        // 候補外の `focus` は実物では出ない画面。ワーキングツリーに 1 ファイル置くのは、
        // Worktree と Diff を候補に入れるため。
        let mut state = state_with_status(fake_snapshot(&[], &["a.txt"], &[]));
        state.focus = pane;
        state.reseat_focus();
        assert_eq!(state.focus, pane, "巡回候補に入っていません");
        hint_bar(
            &state,
            TermSize {
                width,
                ..TermSize::default()
            },
        )
    }

    #[test]
    fn the_hint_line_shows_the_quit_key_when_there_is_no_notice() {
        let last = hint_bar(&state_at("/repo"), TermSize::default());
        assert!(last.contains("q:quit"), "hints: {last:?}");
    }

    #[test]
    fn the_notice_line_shows_the_latest_notice_and_the_hints_stay_below() {
        let mut state = state_at("/repo");
        state.push_notice(NoticeLevel::Warn, "古い通知".to_string(), None);
        state.push_notice(NoticeLevel::Error, "新しい通知".to_string(), None);

        let notice = notice_bar(&state, TermSize::default());
        let hints = hint_bar(&state, TermSize::default());

        assert!(notice.contains("新しい通知"), "notice: {notice:?}");
        assert!(!notice.contains("古い通知"), "notice: {notice:?}");
        // **通知とヒントは行が別。** 取り合わないことがフッターを 2 行にした理由
        // （07 の「ステータスバー」）。
        assert!(!notice.contains("q:quit"), "notice: {notice:?}");
        assert!(hints.contains("q:quit"), "hints: {hints:?}");
    }

    #[test]
    fn the_hint_line_shows_the_keys_of_the_focused_pane() {
        // ペインごとに違うものが出ること。同じなら「フォーカス中のペインのキー」で
        // あることを誰も見ていない。
        let worktree = hint_bar_of(Pane::Worktree, 80);
        assert!(worktree.contains("s:stage"), "hints: {worktree:?}");
        assert!(worktree.contains("C:amend"), "hints: {worktree:?}");

        let branches = hint_bar_of(Pane::Branches, 80);
        assert!(branches.contains("D:強制削除"), "hints: {branches:?}");
        assert!(!branches.contains("s:stage"), "hints: {branches:?}");
    }

    #[test]
    fn the_footer_ends_with_help_and_quit_and_never_shows_the_common_keys() {
        // 共通キーは `?` の一覧にだけ出す（07 の「ステータスバー」）。**幅が余っても出さない**
        // ので無限の幅で見る —— 余った幅で出す旧い規則に戻すと、ここが落ちる。
        for pane in [
            Pane::Graph,
            Pane::Branches,
            Pane::Prs,
            Pane::Worktree,
            Pane::CommitDetail,
            Pane::Diff,
        ] {
            let hints = hint_text(keys::pane(pane), u16::MAX);
            assert!(hints.ends_with("  ?:help q:quit"), "{pane:?}: {hints:?}");
            assert!(!hints.contains("f:fetch"), "{pane:?}: {hints:?}");
        }
    }

    #[test]
    fn the_pane_keys_drop_from_the_end_at_the_width_where_they_would_be_cut() {
        // **境目を跨ぐ**（`.claude/rules/testing.md`）。**幅は組んだ字から導く** —— 数を写すと、
        // キーを足すたびに揃える先が増える（07）。Worktree はペインのキーがいちばん長く、
        // 80 桁で末尾が落ちる側の代表（07 の実測）。
        let full = ratatui::text::Span::raw(hint_text(keys::pane(Pane::Worktree), u16::MAX)).width()
            as u16;
        assert!(full > 80, "80 桁で落ちる前提が崩れています: {full}");
        let fits = hint_bar_of(Pane::Worktree, full);
        assert!(fits.contains("Esc:戻る"), "hints: {fits:?}");

        // 1 セル狭いと**末尾の 1 つだけ**落ち、`?:help` と `q:quit` は残る。
        let over = hint_bar_of(Pane::Worktree, full - 1);
        assert!(!over.contains("Esc:戻る"), "hints: {over:?}");
        assert!(over.contains("C:amend"), "hints: {over:?}");
        assert!(over.contains("?:help q:quit"), "hints: {over:?}");
    }

    #[test]
    fn every_pane_but_worktree_and_graph_shows_all_of_its_keys_at_80() {
        // 80 桁でペインのキーが全部出ることを見張る（落ちる側は Worktree と Graph。07 の実測）。
        for pane in [Pane::Branches, Pane::Prs, Pane::CommitDetail, Pane::Diff] {
            assert_eq!(
                hint_text(keys::pane(pane), 80),
                hint_text(keys::pane(pane), u16::MAX),
                "{pane:?} が 80 桁で落ちています"
            );
        }
    }

    #[test]
    fn the_graph_drops_only_its_mark_keys_at_80() {
        // `Space` / `Esc` は末尾に置き、80 桁で落ちるのを受けた（2026-10-01 にユーザーと
        // 決めた。07 の「Help モーダル」）。落ちるのはその 2 つだけで、`i` までは残る。
        let hints = hint_bar_of(Pane::Graph, 80);
        assert!(
            hints.contains("i:rebase  ?:help q:quit"),
            "hints: {hints:?}"
        );
        assert!(!hints.contains("Space:mark"), "hints: {hints:?}");
        let wide = hint_bar_of(Pane::Graph, u16::MAX);
        assert!(wide.contains("Space:mark Esc:unmark"), "hints: {wide:?}");
    }

    #[test]
    fn a_narrow_terminal_keeps_quit_and_drops_pane_keys_from_the_end() {
        // 幅が足りないときに落とすのはペインのキーで、末尾から（07）。終了の入口が
        // 落ちると、そもそも抜けられない。
        let narrow = hint_bar_of(Pane::Worktree, 30);
        assert!(narrow.contains("?:help q:quit"), "hints: {narrow:?}");
        assert!(narrow.contains("j/k:移動"), "hints: {narrow:?}");
        assert!(!narrow.contains("Esc:戻る"), "hints: {narrow:?}");
    }

    #[test]
    fn a_modal_hides_the_hints_but_not_the_notice() {
        // モーダル表示中は `q` もペインのキーも効かない（キーはモーダルが受ける）。
        // 案内だけ残すと、押しても何も起きないキーを案内することになる（07）。
        let mut state = state_at("/repo");
        state.push_notice(NoticeLevel::Error, "失敗しました".to_string(), None);
        state.modal = Some(Modal::Confirm {
            title: "変更を破棄".to_string(),
            body: "a.txt".to_string(),
            on_yes: Cmd::None,
        });

        let notice = notice_bar(&state, TermSize::default());
        let hints = hint_bar(&state, TermSize::default());

        assert!(notice.contains("失敗しました"), "notice: {notice:?}");
        assert!(!hints.contains("q:quit"), "hints: {hints:?}");
    }

    #[test]
    fn body_is_drawn_between_header_and_status() {
        let lines = draw(&state_at("/repo"), TermSize::default());
        assert!(lines[1].contains("Graph"), "body: {:?}", lines[1]);
    }

    #[test]
    fn tiny_terminal_does_not_panic() {
        // ヘッダ 1 + 本文 1 + フッター 2 の合計（4）に満たない高さ。リサイズ中に
        // 一瞬こうなることがあるので、Layout が破綻しないことを固定しておく。
        for (width, height) in [(1, 1), (1, 3), (20, 2)] {
            let lines = draw(&state_at("/repo"), TermSize { width, height });
            assert_eq!(lines.len(), height as usize);
        }
    }

    #[test]
    fn a_short_terminal_keeps_the_quit_key_and_drops_the_notice_instead() {
        // **フッターが 2 行に足りないときに落とすのは通知のほう**（07）。`Layout` に
        // 1 行ずつ割らせると後ろのキーヒントが 0 行になり、**`q:quit` が画面から消える**。
        // **境目を跨ぐ**（高さ 3 でフッターは 1 行、4 で 2 行）。
        let mut state = state_at("/repo");
        state.push_notice(NoticeLevel::Error, "失敗しました".to_string(), None);

        for height in [3, 4] {
            let size = TermSize { width: 80, height };
            let lines = draw(&state, size).join("\n");
            assert!(lines.contains("q:quit"), "{height} 行: {lines:?}");
        }

        // 2 行あるときは通知も出る（落とすのは足りないときだけ）。
        let both = draw(
            &state,
            TermSize {
                width: 80,
                height: 4,
            },
        )
        .join("\n");
        assert!(both.contains("失敗しました"), "{both:?}");
        // 1 行しか無いときは通知が消える。
        let only_hints = draw(
            &state,
            TermSize {
                width: 80,
                height: 3,
            },
        )
        .join("\n");
        assert!(!only_hints.contains("失敗しました"), "{only_hints:?}");
    }

    #[test]
    fn the_notice_line_shows_only_the_first_line_of_a_notice() {
        // toml のパースエラーは複数行。そのまま流すと改行が消えて読めなくなる。
        let last = notice_bar_with(
            "設定ファイルの構文エラー
  |
2 | ascii = 
  | ^",
        );

        assert!(
            last.contains("設定ファイルの構文エラー"),
            "notice: {last:?}"
        );
        assert!(!last.contains('|'), "notice: {last:?}");
    }

    #[test]
    fn a_notice_of_the_full_width_fits_on_its_own_line() {
        // **案内文の予算（端末の幅 − 時刻の 9 セル）はここから来る**（01 の「失敗の
        // 案内文」）。ヒントは 2 行目なので、80 桁の端末では時刻の後ろに 71 セル残る。
        // `contains` を見るテストは切れても通るので、**収まることを直に見る**。
        //
        // **35 と 36 はリテラルのまま置く。** 幅から算出すると、予算が変わったときに
        // このテストが黙って追随し、`app` 側の「予算に収める」だけが古くなる
        // （`a_failure_puts_the_next_key_on_the_summary_line`）。**リテラルで
        // あること自体が、2 つの見張りを噛み合わせている。**

        let fits = "あ".repeat(35);
        let last = notice_bar_with(&fits);
        assert!(last.contains(&fits), "notice: {last:?}");
        assert!(!last.contains('…'), "notice: {last:?}");

        // 1 文字増やすと 72 セルになり、時刻の後ろに入らない。**切れたことを `…` で
        // 見せる**（07 の「ステータスバー」）。
        let over = "あ".repeat(36);
        let last = notice_bar_with(&over);
        assert!(!last.contains(&over), "notice: {last:?}");
        assert!(last.contains(&format!("{fits}…")), "notice: {last:?}");
    }

    #[test]
    fn the_progress_badge_leads_the_line_and_outlives_the_notice() {
        // 通知は次の入力で下ろすが、進行中操作は continue / abort するまで終わらない
        // （07 の「ステータスバー」）。**通知が無い行にも出る**ことまで見る。
        let mut state = state_at("/repo");
        state.worktree.snapshot.progress = Some(InProgressOp::Rebase {
            step: Some(Step {
                current: 3,
                total: 7,
            }),
            branch: Some("topic".to_string()),
            refused_reword: None,
        });
        state.push_notice(NoticeLevel::Error, "push に失敗しました".to_string(), None);
        let bar = notice_bar(&state, TermSize::default());
        assert!(
            bar.starts_with(" REBASE 3/7  10:03:26 push に失敗しました"),
            "notice: {bar:?}"
        );

        state.dismiss_notice();
        let bar = notice_bar(&state, TermSize::default());
        assert_eq!(bar.trim_end(), " REBASE 3/7", "notice: {bar:?}");
    }

    #[test]
    fn the_progress_badge_is_set_apart_from_the_notice() {
        // 反転だけだとタブやカーソル行と同じ見た目になる（`theme::in_progress`）。
        let mut state = state_at("/repo");
        state.worktree.snapshot.progress = Some(InProgressOp::Merge { oid: fake_oid(1) });
        let terminal = terminal_with(&state, TermSize::default());
        let y = footer_area(TermSize::default(), 0).y;
        let buffer = terminal.backend().buffer();

        let badge = &buffer[(1, y)];
        assert_eq!(badge.symbol(), "M");
        assert_eq!(badge.fg, Color::Yellow);
        assert!(badge.modifier.contains(Modifier::REVERSED));
        // 前後の空白 1 つずつまでがバッジ。区切りの空白は素のまま。
        let label = format!(" MERGE {} ", short_oid(&fake_oid(1)));
        let width = u16::try_from(label.len()).unwrap();
        assert!(buffer[(0, y)].modifier.contains(Modifier::REVERSED));
        assert!(buffer[(width - 1, y)].modifier.contains(Modifier::REVERSED));
        assert!(!buffer[(width, y)].modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn each_operation_has_its_git_name_on_the_badge() {
        let oid = fake_oid(1);
        let short = short_oid(&oid);
        let step = Some(Step {
            current: 1,
            total: 2,
        });
        for (op, label) in [
            (
                InProgressOp::Rebase {
                    step,
                    branch: None,
                    refused_reword: None,
                },
                "REBASE 1/2".to_string(),
            ),
            (
                InProgressOp::Rebase {
                    step: None,
                    branch: None,
                    refused_reword: None,
                },
                "REBASE".to_string(),
            ),
            (InProgressOp::Am { step }, "AM 1/2".to_string()),
            (
                InProgressOp::CherryPick { oid: Some(oid) },
                format!("CHERRY-PICK {short}"),
            ),
            (
                InProgressOp::CherryPick { oid: None },
                "CHERRY-PICK".to_string(),
            ),
            (
                InProgressOp::Revert { oid: Some(oid) },
                format!("REVERT {short}"),
            ),
            (InProgressOp::Revert { oid: None }, "REVERT".to_string()),
            (InProgressOp::Merge { oid }, format!("MERGE {short}")),
        ] {
            assert_eq!(progress_label(&op), label, "{op:?}");
        }
    }

    #[test]
    fn a_notice_beside_the_longest_badge_is_cut_short_not_pushed_off() {
        // **予算（71 セル）は縮めない**ので、いちばん長いバッジ（`CHERRY-PICK
        // abc1234`、前後の空白と区切りで 22 セル）の隣では 80 − 22 − 9 = 49 セルで
        // 切れる（07 の「ステータスバー」）。**24 と 25 はリテラルのまま置く** ——
        // 上の `a_notice_of_the_full_width_fits_on_its_own_line` と同じ理由。
        let mut state = state_at("/repo");
        state.worktree.snapshot.progress = Some(InProgressOp::CherryPick {
            oid: Some(fake_oid(1)),
        });

        let fits = "あ".repeat(24);
        state.push_notice(NoticeLevel::Error, fits.clone(), None);
        let bar = notice_bar(&state, TermSize::default());
        assert!(bar.contains(&fits), "notice: {bar:?}");
        assert!(!bar.contains('…'), "notice: {bar:?}");

        let over = "あ".repeat(25);
        state.push_notice(NoticeLevel::Error, over.clone(), None);
        let bar = notice_bar(&state, TermSize::default());
        assert!(!bar.contains(&over), "notice: {bar:?}");
        assert!(bar.contains(&format!("{fits}…")), "notice: {bar:?}");
    }

    #[test]
    fn repo_label_falls_back_to_the_path_when_there_is_no_file_name() {
        // "." や "/" は file_name() が None。空ヘッダにしない。
        let line = header(&state_at("."), TermSize::default());
        assert!(line.contains('.'), "header: {line:?}");
    }

    #[test]
    fn renders_at_other_terminal_sizes() {
        // リサイズ後のサイズでも落ちないこと。
        for (width, height) in [(120, 40), (40, 10)] {
            let size = TermSize { width, height };
            let lines = draw(&state_at("/repo"), size);
            assert_eq!(lines.len(), height as usize);
            assert!(
                header(&state_at("/repo"), size).contains("repo"),
                "header: {:?}",
                lines[0]
            );

            let last = hint_bar(&state_at("/repo"), size);
            assert!(last.contains("q:quit"), "hints: {last:?}");
        }
    }
}
