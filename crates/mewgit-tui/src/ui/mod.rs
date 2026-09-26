//! 描画。設計は `docs/design/07-ui.md`。
//!
//! `render` は [`AppState`] のみを参照する（不変ルール 2）。git2 / CLI / HTTP を
//! ここから呼ばない。
//!
//! いまはヘッダ・グラフ・ブランチ一覧・ワーキングツリー（またはコミット詳細）・
//! ステータスバー。`Stash` と `PRs` は、そのタブを実装するタスクで足す（07）。

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
use mewgit_core::git_read::{FileEntry, FileStatus};
use mewgit_core::github::RateLimit;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

/// 1 フレーム描く。
pub fn render(frame: &mut Frame, state: &AppState) {
    let [header, body, notice, hints] = split(frame.area());
    let [left, right] = split_body(body);

    frame.render_widget(header_line(state, header.width), header);
    // **スピナーは同じ領域に右寄せで重ねる。** `Paragraph` は既定のスタイルで
    // 領域を塗り直すだけなので、左に描いたリポジトリ名は消えない。
    if let Some(spinner) = spinner_line(state) {
        frame.render_widget(Paragraph::new(spinner).right_aligned(), header);
    }
    // **左は Graph と Diff の差し替え。** どちらを描くかは `focus` だけで決まる（07）。
    // 「いまどちらを見せているか」を別に持つと、`focus` と食い違う状態
    // （Diff を見せているのにキーは Graph が受ける）が表現できてしまう。
    if state.focus == Pane::Diff {
        panes::diff::render(frame, left, state);
    } else {
        panes::graph::render(frame, left, state);
    }
    // **右列の上は Branches と PRs のタブ。** 左の選択では入れ替わらないので、
    // 上を先に置いて残りを下のペインに渡す（07 の「レイアウト」）。
    // **組むのはここ 1 回**で、高さの決定と描画の両方に回す。
    let top_pane = panes::TopPane::build(state);
    let [top, right] = split_right(state, &top_pane, right);
    top_pane.render(frame, top, state);
    // **右列の下は Working tree とコミット詳細の差し替え。** どちらを描くかはグラフの
    // 選択だけで決まる（07）。`focus` で決めると、Graph にフォーカスした瞬間に
    // 右が消える。
    match state.graph.selection() {
        GraphSelection::Wip => panes::worktree::render(frame, right, state),
        GraphSelection::Commit(line) => panes::commit::render(frame, right, state, &line.commit),
    }
    frame.render_widget(notice_line(state, notice.width), notice);
    frame.render_widget(hint_line(state, hints.width), hints);

    // モーダルは最後。下のペインを覆って描く（07）。
    if let Some(modal) = &state.modal {
        modals::render(frame, frame.area(), modal);
    }
}

/// 本文を左右に割る。
///
/// 幅 < 100 で右ペインを隠す分岐は M6（07）。全画面切替が同じタスクなので、
/// 隠すほうだけ先に入れると 80 桁の端末から右ペインへ到達できなくなる。
fn split_body(area: Rect) -> [Rect; 2] {
    Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(area)
}

/// 右列を上のタブ（Branches / PRs）と、その下のペイン（Working tree / コミット詳細）に
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
/// `s` や `x` が別のペインに飛んだように見える。枠の内側の大きさは強調の有無で
/// 変わらないので、レイアウトの計算は `focused` を偽で呼んでよい。
pub fn pane_block(title: impl Into<Line<'static>>, focused: bool) -> Block<'static> {
    // **`&'static str` も受ける。** グラフの枠は行数を測るのに `runtime::layout` から
    // ループのたびに組まれるので、そこで毎回 `String` を確保しない。
    // **`Line` も受ける**のは、右上のタブが見出しの中で 2 枚を描き分けるため
    // （`panes::right_tabs`。07 の「右上のタブ」）。
    let block = Block::default().borders(Borders::ALL).title(title.into());
    if focused {
        block.border_style(Style::default().add_modifier(Modifier::BOLD))
    } else {
        block
    }
}

/// 折り返す `Paragraph` と、それを幅 `width` で描いたときの行数（07 の「モーダル」）。
///
/// **測る `Paragraph` はここでしか組まない。** 測るときと描くときで `Wrap` の設定が
/// ずれないようにするためと、枠を付けたまま測らせないため。`Paragraph::line_count` は
/// 枠の上下を行数に足す一方、枠の左右ぶんを幅からは引かないので、枠付きで測ると
/// 外側の幅で折り返した行数が返り、本文の末尾が切れる。枠のぶんは呼び出し側が
/// `Block::inner` から出す。
pub fn wrapped<'a>(text: impl Into<Text<'a>>, width: u16) -> (Paragraph<'a>, u16) {
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
    let rows = u16::try_from(paragraph.line_count(width)).unwrap_or(u16::MAX);
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
/// 下端に貼り付く癖が残るが、そのために `Msg::Layout` にフィールドを足すのは M6
/// （狭いレイアウト）と一緒にする（07）。
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

/// `must_show` が入る窓を切り出して描く。
///
/// 行の組み立てはペインごとに違うが、**切り出して描くところまでが 1 つ**。
/// [`window`] だけを共有すると、その戻り値の使い方が各ペインに写る。
///
/// **例外は `modals::edit_area`** で、そちらは [`window`] だけを使う（編集中の
/// モーダルはみな、あの 1 本を通る）。編集中はカーソルが枠の右端を越えるので横に
/// ずらす必要があり、ここに横位置を足すと、「長い行は右で切れるだけ」を意図している
/// diff と一覧まで一般化することになる。前提の成り立たない利用者を共有基盤に
/// 押し込まない（`.claude/rules/rust-coding.md`）。
///
/// **折り返さない**（`Wrap` を付けない）。diff は桁の揃いが意味を持ち、一覧のパスも
/// 折り返すと行とカーソルの対応が崩れる。
pub fn render_rows(
    frame: &mut Frame,
    area: Rect,
    mut lines: Vec<Line<'_>>,
    must_show: Range<usize>,
) {
    let window = window(lines.len(), must_show, area.height as usize);
    frame.render_widget(
        Paragraph::new(lines.drain(window).collect::<Vec<_>>()),
        area,
    );
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
    let [left, right] = split_body(body);
    // 右列は上のタブとその下に割れている。割る位置も実際の関数から出す。
    let [top, below] = split_right(state, &panes::TopPane::build(state), right);
    match pane {
        // **Branches と PRs も同じ領域を差し替えで使う**（07 の「右上のタブ」）。
        Pane::Branches | Pane::Prs => top,
        Pane::Worktree | Pane::CommitDetail => below,
        // **Graph と Diff は同じ領域を差し替えで使う**（07）ので同じ Rect を返す。
        // Stash はまだ描かないので、来たら左を返すだけ。
        _ => left,
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
pub fn file_row(entry: &FileEntry) -> String {
    format!(
        " {} {}",
        mark(entry.status),
        path_label(&entry.path, entry.old_path.as_deref())
    )
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
    let [left, _] = split_body(body);
    panes::graph::area(left).height as usize
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
    if state.jobs.is_empty() {
        return None;
    }
    // 駒は `Tick` の回数で選ぶ。`jobs` の中身では選ばない —— 走っているものが
    // 変わらないあいだ止まって見える。
    let frame = SPINNER[(state.tick % SPINNER.len() as u64) as usize];
    // 説明は `&'static str` なので、組むのは `Vec<Span>` 1 本だけ。描画は入力が
    // 無くても `Tick` で回る（1 秒に 4 回）ので、中間の `String` を作らない。
    let mut spans = vec![Span::raw(frame)];
    for job in state.jobs.iter() {
        spans.push(Span::raw(" "));
        spans.push(Span::raw(job.label));
    }
    Some(Line::from(spans))
}

/// フッターの 1 行目。直近の通知だけを、積まれた時刻を添えて出す。
///
/// **幅いっぱい使ってよい**（キーヒントは 2 行目）。時刻の 9 セルを引いた残りに
/// 案内文が収まることは `app` 側が見張る（01 の「失敗の案内文」）。**キー入力で
/// 下ろした通知は出さない**（[`AppState::shown_notice`]）。
///
/// **溢れたら末尾を `…` にする**（07 の「ステータスバー」）。予算を見張れるのは
/// 固定の文言だけで、パスやホスト名を埋めた文言には上限が無い。黙って切ると、
/// 切れたことが画面から判らない。
///
/// 出すのは先頭行だけ。toml のパースエラーのように複数行のメッセージがあり、そのまま
/// 流すと改行が消えて読めなくなる。全文は `Notice.message` に残っている（履歴表示は M6）。
fn notice_line(state: &AppState, width: u16) -> Paragraph<'_> {
    let Some(notice) = state.shown_notice() else {
        return Paragraph::new("");
    };
    let at = Span::styled(format!("{} ", time::clock(notice.at)), theme::muted());
    let room = width.saturating_sub(u16::try_from(at.width()).unwrap_or(u16::MAX));
    let message = Span::raw(ellipsize(first_line(&notice.message), room));
    Paragraph::new(Line::from(vec![at, message]))
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

/// `width` に収まるヒントの並び。
///
/// 落とす順は**共通キー → ペインのキーの末尾**で、`q:quit` は落とさない（07）。
///
/// **収まるかは組んだ字から数える。** 幅だけを別に算術で出すと、組む側と数える側で
/// 規則が 2 本になる（`.claude/rules/rust-coding.md` の「出力を 1 本にしても入力が 2 本なら結果はずれる」）。
/// そのぶん 1 フレームで数十の `String` を捨てる（幅が足りないときは落とす回数だけ
/// 組み直す）が、描画は 250ms ごと（1 秒 4 回）。実測（release、10,000 回平均。
/// レビューで測った値）—— Graph 幅 80 で 342ns、Worktree 幅 80 で 711ns、
/// 落とす形の Worktree 幅 30 で 2.18µs。
fn hint_text(pane: &[KeyHint], width: u16) -> String {
    let line = |pane: &[KeyHint], common: &[KeyHint]| {
        let groups = [joined(pane), joined(common), joined(&[keys::QUIT])];
        groups
            .iter()
            .filter(|group| !group.is_empty())
            .fold(String::new(), |mut out, group| {
                if !out.is_empty() {
                    out.push_str(SEP);
                }
                out.push_str(group);
                out
            })
    };

    // 共通キーまで出せるなら出す。
    let full = line(pane, keys::COMMON);
    if fits(&full, width) {
        return full;
    }
    // 入らなければ共通キーを落とし、それでも入らなければペインのキーを末尾から。
    let mut keep = pane.len();
    loop {
        let text = line(&pane[..keep], &[]);
        if keep == 0 || fits(&text, width) {
            return text;
        }
        keep -= 1;
    }
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

/// 案内 1 つの字（`j/k:移動`）を `out` に足す。
///
/// **フッターの 2 行目を組むのはここだけ。** 表（`app::keys`）はキーしか持たない（01）。
/// **モーダルは自前の `HINT` を持っている**（`modals::*`。あちらは箱の中で予算が違う）ので、
/// 綴りが 2 通りある —— 揃えるのは `Modal::Help` のタスク（07 の「ステータスバー」）。
fn push_hint(out: &mut String, hint: &KeyHint) {
    for (i, key) in hint.keys().iter().enumerate() {
        if i > 0 {
            out.push('/');
        }
        push_key(out, key);
    }
    out.push(':');
    out.push_str(hint.label());
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
        KeyCode::Char(c) => out.push(c),
        KeyCode::Enter => out.push_str("Enter"),
        KeyCode::Esc => out.push_str("Esc"),
        // **表に入っているのはこの 3 つだけ**（`app::keys`）。`Tab` / `Ctrl-C` を足すのは
        // `Modal::Help` のタスクなので、**字を先に書かない** —— 書いても描かれないので
        // 間違っていても見えない（`Debug` の字が出る形にしてある）。
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
    use super::{degraded_label, ellipsize, file_row, fits, pane_area, window, wrapped};
    use crate::test_util::{
        draw, hint_bar, notice_bar, state_at, state_showing_diff, state_with_status, terminal_with,
    };
    use mewgit_core::app::{
        update, AppState, Cmd, JobKind, Modal, Msg, NoticeLevel, Pane, TargetState, TermSize,
    };
    use mewgit_core::auth::{AccountSource, AccountState, DegradedReason, RepoTarget};
    use mewgit_core::git_exec::GitOp;
    use mewgit_core::github::GithubError;
    use mewgit_core::test_util::{
        fake_account, fake_ambiguous, fake_no_access, fake_rate_limit, fake_snapshot,
        fetch_pr_details,
    };
    use ratatui::style::{Color, Modifier};

    #[test]
    fn a_file_row_puts_the_status_letter_before_the_path() {
        // **書式の見張りはここ 1 本。** 2 つのペインが同じ関数を通るので、ペインの
        // 側でそれぞれ字を見ると、種別を足したときにどちらのテストも通ってしまう。
        use mewgit_core::git_read::FileStatus;
        use mewgit_core::test_util::fake_entry;

        let row = |status| file_row(&fake_entry("a.txt", status));

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
        assert_eq!(file_row(&renamed), " R old.txt -> new.txt");
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
    fn the_focused_pane_is_the_only_one_with_a_bold_border() {
        // どちらがキーを受けるか判らないと、`s` や `x` が別のペインに飛んだように見える。
        let size = TermSize::default();
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
                let bold = buffer[at].modifier.contains(Modifier::BOLD);
                // **`pane == focus` では見られない。** Graph と Diff は同じ領域なので、
                // Diff にフォーカスがあるとき「Graph の枠」の座標には Diff の枠が
                // 描かれている。太いかどうかはその領域にフォーカスがあるかで決まる。
                let expected = at == corner(focus, size, &state);
                assert_eq!(bold, expected, "focus={focus:?} pane={pane:?}");
            }
        }
    }

    #[test]
    fn the_left_area_swaps_between_the_graph_and_the_diff() {
        let state = state_at("/repo");
        let text = draw(&state, TermSize::default()).join("\n");
        assert!(text.contains("Graph"), "{text}");

        // **`focus` を直に置かない。** 対象の無い `Diff` は候補から外れる（01）ので、
        // 直に置くと実物では出ない画面を見張ることになる。
        let state = state_showing_diff(fake_snapshot(&[], &["a.txt"], &[]));
        let text = draw(&state, TermSize::default()).join("\n");

        // 差し替えであって分割ではない。両方出すと 80 桁ではどちらも読めない。
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
        let text = draw(&state, TermSize::default()).join("\n");
        assert!(text.contains("Working tree"), "{text}");
        assert!(!text.contains("Commit "), "{text}");

        let state = crate::test_util::state_on_first_commit(&["first"]);
        let text = draw(&state, TermSize::default()).join("\n");

        assert!(!text.contains("Working tree"), "{text}");
        assert!(text.contains("Commit "), "{text}");
    }

    #[test]
    fn the_right_area_follows_the_selection_even_while_the_graph_has_focus() {
        // `focus` で決めると、Graph にフォーカスした瞬間に右が消える（07）。
        let mut state = crate::test_util::state_on_first_commit(&["first"]);
        state.focus = Pane::Graph;

        let text = draw(&state, TermSize::default()).join("\n");

        assert!(text.contains("Commit "), "{text}");
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
    /// 高さが変わる（M6 の狭いレイアウト）と、各テストが持っている `[0]` が一斉に嘘になる。
    fn header(state: &AppState, size: TermSize) -> String {
        draw(state, size)[0].clone()
    }

    /// ジョブを 1 つ走らせた状態で描いたヘッダ。
    fn header_while_fetching(tick: u64) -> String {
        let mut state = state_at("/repo");
        let _ = state.issue(Cmd::Exec(GitOp::Fetch {
            remote: None,
            prune: false,
        }));
        state.tick = tick;
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
        // `the_focused_pane_is_the_only_one_with_a_bold_border` と同じ理由）——
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
    fn the_common_keys_drop_where_the_pane_keys_fill_the_line() {
        // 幅 80 では Worktree のキーだけで 1 行が埋まる（**セル数の表は 07 の
        // 「ステータスバー」の 1 本**。ここに写すと腐る）。**出る側は下の境目の
        // テストが見る** —— 両方で見ると、同じことを 2 箇所で数えることになる。
        let worktree = hint_bar_of(Pane::Worktree, 80);
        assert!(!worktree.contains("f:fetch"), "hints: {worktree:?}");
        // 落とすのは共通キーのほうで、ペインのキーは残る（07）。
        assert!(worktree.contains("Esc:戻る"), "hints: {worktree:?}");
        assert!(worktree.contains("q:quit"), "hints: {worktree:?}");
    }

    #[test]
    fn the_common_keys_drop_at_the_width_where_they_would_be_cut() {
        // **境目を跨ぐ**（`.claude/rules/testing.md`）。Graph + 共通キー + `q:quit` は 75 セルなので、
        // 75 では出て 74 では落ちる。机上で選んだ幅では、どちらか片側しか踏めない。
        let fits = hint_bar_of(Pane::Graph, 75);
        assert!(fits.contains("f:fetch"), "hints: {fits:?}");
        assert!(fits.contains("n:ブランチ作成"), "hints: {fits:?}");

        let over = hint_bar_of(Pane::Graph, 74);
        assert!(!over.contains("f:fetch"), "hints: {over:?}");
        assert!(over.contains("n:ブランチ作成"), "hints: {over:?}");
        assert!(over.contains("q:quit"), "hints: {over:?}");
    }

    #[test]
    fn the_prs_pane_drops_the_common_keys_but_keeps_all_of_its_own() {
        // **`N:PR 作成` を足して落ちる側へ移った**（07 の実測: ペインのキーだけで
        // 43 → 53 セル。共通キーの 33 を足すと 86 で 80 に入らない）。ここが見張る
        // のは**どちら側に居るか**で、落とす仕組みの境目は上のテストが見る。
        let prs = hint_bar_of(Pane::Prs, 80);
        assert!(!prs.contains("f:fetch"), "hints: {prs:?}");

        // **落とすのは共通キーだけ。** ペインのキーが 1 つでも欠けると、
        // 「いまここで何ができるか」の唯一の案内が減る（07）。
        for key in [
            "j/k:移動",
            "Enter:切替",
            "o:ブラウザ",
            "r:更新",
            "N:PR 作成",
            "Esc:戻る",
            "q:quit",
        ] {
            assert!(prs.contains(key), "{key} が落ちた: {prs:?}");
        }

        // **余白まで見る**（#80）。ペインのキーだけで 80 桁にはまだ余る（セル数は
        // 07 の実測）—— ここを埋め切ると次に落ちるのは `Esc:戻る` のほうになる。
        assert!(fits(prs.trim_end(), 79), "80 桁で余白がありません: {prs:?}");
    }

    #[test]
    fn the_graph_pane_still_shows_the_common_keys_with_room_to_spare() {
        // **`Prs` が落ちる側へ移った回に、出る側まで一緒に落ちていないか。**
        // 上のテストと対になる —— あちらは落ちる側、こちらは出る側（07 の表）。
        // **`CommitDetail` は `hint_bar_of` では座れない** —— あのヘルパは WIP 行を
        // 選んだ状態なので候補に入らない（07 の「ペインとフォーカス」）。
        let graph = hint_bar_of(Pane::Graph, 80);
        assert!(graph.contains("f:fetch"), "hints: {graph:?}");
        // `contains` だけだと、80 桁をちょうど埋めた状態でも通る（#80）。
        assert!(fits(graph.trim_end(), 79), "余白がありません: {graph:?}");
    }

    #[test]
    fn a_narrow_terminal_keeps_quit_and_drops_pane_keys_from_the_end() {
        // 幅が足りないときに落とすのはペインのキーで、末尾から（07）。終了の入口が
        // 落ちると、そもそも抜けられない。
        let narrow = hint_bar_of(Pane::Worktree, 30);
        assert!(narrow.contains("q:quit"), "hints: {narrow:?}");
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
