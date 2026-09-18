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
use std::ops::Range;
use std::path::Path;

use crate::{modals, panes};
use mewgit_core::app::{AppState, GraphSelection, Pane, TermSize};
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
    let [header, body, status] = split(frame.area());
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
    frame.render_widget(status_line(state), status);

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
/// **例外は `modals::commit_message`** で、そちらは [`window`] だけを使う。編集中は
/// カーソルが枠の右端を越えるので横にずらす必要があり、ここに横位置を足すと、
/// 「長い行は右で切れるだけ」を意図している diff と一覧まで一般化することになる。
/// 前提の成り立たない利用者を共有基盤に押し込まない（CLAUDE.md）。
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
    let [_, body, _] = split(Rect::new(0, 0, size.width, size.height));
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

/// 画面をヘッダ / 本文 / ステータスバーに割る。
fn split(area: Rect) -> [Rect; 3] {
    Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area)
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
    let [_, body, _] = split(Rect::new(0, 0, term.width, term.height));
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

/// ヘッダの要素の区切り。**1 セルだと隣の語と地続きに見える。**
///
/// 組む側（`Span::raw(SEP)`）と数える側（[`quota_fits`]）が同じものを読む ——
/// 幅だけを定数にすると、区切りを広げたときに数えるほうが 2 のまま残る。
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
        DegradedReason::NoAccess { attempts } => {
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
/// 置いていたが、あちらは 1 行しかなく、`git` の失敗の案内文に 72 セルを取ってある
/// （01 の「失敗の案内文」）。ジョブが 1 つ増えるたびに案内が末尾から削られるので、
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

/// ステータスバー。直近の通知とキーヒントの両方を出す。
///
/// 通知で置き換えないのは、起動時の設定警告が 1 件出るとヒントが二度と戻らなくなるため。
fn status_line(state: &AppState) -> Paragraph<'_> {
    let mut spans = Vec::new();
    if let Some(notice) = state.notices.back() {
        // 1 行しか無いので先頭行だけ。toml のパースエラーのように複数行の
        // メッセージがあり、そのまま流すと改行が消えて読めなくなる。
        // 全文は notice.message に残っている（履歴表示は M6）。
        spans.push(Span::raw(first_line(&notice.message)));
        spans.push(Span::raw("  "));
    }
    spans.push(Span::raw(HINT));

    Paragraph::new(Line::from(spans))
}

/// 最初の行。改行が無ければ全体。
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

/// ステータスバー右側の固定ヒント。M6 の `Modal::Help` までの暫定。
const HINT: &str = "q:quit";

/// リポジトリの表示名。パスの末尾を使い、取れなければパスそのもの。
fn repo_label(state: &AppState) -> String {
    let path = &state.repo.path;
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::{degraded_label, file_row, pane_area, window, wrapped};
    use crate::test_util::{draw, state_at, state_showing_diff, state_with_status, terminal_with};
    use mewgit_core::app::{
        update, AppState, Cmd, JobKind, Msg, NoticeLevel, Pane, TargetState, TermSize,
    };
    use mewgit_core::auth::{AccountSource, AccountState, DegradedReason, RepoTarget};
    use mewgit_core::git_exec::GitOp;
    use mewgit_core::github::GithubError;
    use mewgit_core::test_util::{
        fake_account, fake_ambiguous, fake_rate_limit, fake_snapshot, fetch_pr_details,
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
        // はみ出す長さの両方をここで落とす（通知の 72 セルと同じ形の見張り。01）。
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
            DegradedReason::NoAccess {
                attempts: Vec::new(),
            },
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

        state.account = AccountState::Degraded(DegradedReason::NoAccess {
            attempts: Vec::new(),
        });
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
        // 幅 84〜89 は `[api: ` までで桁が 1 つも出ず、幅 93〜95 は数が丸ごと残って
        // `]` と隙間だけが欠ける。出るのは**幅 96 以上**（隙間 2 セルを両側に取る
        // ため。`quota_fits`）。
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
    /// **取り出しはここ 1 本。** `status_bar` と同じ理由で、ヘッダの位置や高さが
    /// 変わる（M6 の狭いレイアウト）と、各テストが持っている `[0]` が一斉に嘘になる。
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

    /// 描いたステータスバー（最下行）。
    ///
    /// **取り出しはここ 1 本。** ステータスバーの置き場所が変わる（M6）と、各テストが
    /// 持っている `height - 1` が一斉に嘘になる。
    fn status_bar(state: &AppState, size: TermSize) -> String {
        draw(state, size)[size.height as usize - 1].clone()
    }

    /// 通知を 1 件だけ積んで描いたステータスバー。既定サイズ（80x24）で見る。
    fn status_bar_with(message: &str) -> String {
        let mut state = state_at("/repo");
        state.push_notice(NoticeLevel::Error, message.to_string(), None);
        status_bar(&state, TermSize::default())
    }

    #[test]
    fn status_bar_shows_the_quit_hint_when_there_is_no_notice() {
        let last = status_bar(&state_at("/repo"), TermSize::default());
        assert!(last.contains("q:quit"), "status: {last:?}");
    }

    #[test]
    fn status_bar_shows_the_latest_notice_alongside_the_hint() {
        let mut state = state_at("/repo");
        state.push_notice(NoticeLevel::Warn, "古い通知".to_string(), None);
        state.push_notice(NoticeLevel::Error, "新しい通知".to_string(), None);

        let last = status_bar(&state, TermSize::default());

        assert!(last.contains("新しい通知"), "status: {last:?}");
        assert!(!last.contains("古い通知"), "status: {last:?}");
        // 通知が出てもヒントは消えない。消すと二度と戻らない。
        assert!(last.contains("q:quit"), "status: {last:?}");
    }

    #[test]
    fn body_is_drawn_between_header_and_status() {
        let lines = draw(&state_at("/repo"), TermSize::default());
        assert!(lines[1].contains("Graph"), "body: {:?}", lines[1]);
    }

    #[test]
    fn tiny_terminal_does_not_panic() {
        // ヘッダ 1 + 本文 1 + ステータス 1 の合計に満たない高さ。リサイズ中に
        // 一瞬こうなることがあるので、Layout が破綻しないことを固定しておく。
        for (width, height) in [(1, 1), (1, 3), (20, 2)] {
            let lines = draw(&state_at("/repo"), TermSize { width, height });
            assert_eq!(lines.len(), height as usize);
        }
    }

    #[test]
    fn status_bar_shows_only_the_first_line_of_a_notice() {
        // toml のパースエラーは複数行。そのまま流すと改行が消えて読めなくなる。
        let last = status_bar_with(
            "設定ファイルの構文エラー
  |
2 | ascii = 
  | ^",
        );

        assert!(
            last.contains("設定ファイルの構文エラー"),
            "status: {last:?}"
        );
        assert!(!last.contains('|'), "status: {last:?}");
        assert!(last.contains("q:quit"), "status: {last:?}");
    }

    #[test]
    fn a_notice_of_72_cells_fits_beside_the_hint() {
        // **案内文の 72 セルという予算はここから来る**（01 の「失敗の案内文」）。
        // `q:quit` と空白 2 で 8 セル取るので、80 桁では通知に 72 セル残る。
        // `contains` を見るテストは切れても通るので、**収まることを直に見る**。
        //
        // **36 と 37 はリテラルのまま置く。** `HINT` から算出すると、M6 で `?:help` が
        // 付いて予算が下がったときにこのテストが黙って追随し、`app` 側の「72 セルに
        // 収める」だけが古くなる（`a_failure_puts_the_next_key_on_the_summary_line`）。
        // **リテラルであること自体が、2 つの見張りを噛み合わせている** —— 予算が
        // 変わればここが赤くなり、案内文の側も数え直すことになる。

        let fits = "あ".repeat(36);
        let last = status_bar_with(&fits);
        assert!(last.contains(&fits), "status: {last:?}");
        assert!(last.contains("q:quit"), "status: {last:?}");

        // 1 文字増やすと 74 セルになり、ヒントと合わせて 80 桁に入らない。**どちらが
        // 切られるかは見ない** —— 07 は通知を切ると決めていて、いまの実装は末尾
        // （ヒント）を落とす。直すのは M6 のステータスバーのタスク。
        let over = "あ".repeat(37);
        let last = status_bar_with(&over);
        assert!(
            !(last.contains(&over) && last.contains("q:quit")),
            "status: {last:?}"
        );
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

            let last = status_bar(&state_at("/repo"), size);
            assert!(last.contains("q:quit"), "status: {last:?}");
        }
    }
}
