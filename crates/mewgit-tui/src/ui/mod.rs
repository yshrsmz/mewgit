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
use mewgit_core::git_read::{FileEntry, FileStatus};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

/// 1 フレーム描く。
pub fn render(frame: &mut Frame, state: &AppState) {
    let [header, body, status] = split(frame.area());
    let [left, right] = split_body(body);

    frame.render_widget(header_line(state), header);
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
    // **右列の上は Branches。** 左の選択でも `focus` でも入れ替わらないので、
    // 一覧を先に置いて残りを下のペインに渡す（07 の「レイアウト」）。
    // **組むのはここ 1 回**で、高さの決定と描画の両方に回す。
    let view = panes::branches::lines(state);
    let [branches, right] = split_right(state, &view, right);
    panes::branches::render(frame, branches, state, view);
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

/// 右列を Branches と、その下のペイン（Working tree / コミット詳細）に割る。
///
/// **割る位置を決めるのは Branches ペイン自身**（`panes::branches::rows`）。一覧の
/// 行数から出すので、ここで高さを書くと規則が 2 本になる。
fn split_right(state: &AppState, view: &panes::branches::View, right: Rect) -> [Rect; 2] {
    let rows = panes::branches::rows(view, state, right.height);
    Layout::vertical([Constraint::Length(rows), Constraint::Min(0)]).areas(right)
}

/// ペインの枠。
///
/// **フォーカスのあるペインは枠を強調する**（07）。どちらがキーを受けるか判らないと、
/// `s` や `x` が別のペインに飛んだように見える。枠の内側の大きさは強調の有無で
/// 変わらないので、レイアウトの計算は `focused` を偽で呼んでよい。
pub fn pane_block(title: impl Into<Cow<'static, str>>, focused: bool) -> Block<'static> {
    // **`&'static str` も受ける。** グラフの枠は行数を測るのに `runtime::layout` から
    // ループのたびに組まれるので、そこで毎回 `String` を確保しない。
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
    // 右列は Branches とその下に割れている。割る位置も実際の関数から出す。
    let [branches, below] = split_right(state, &panes::branches::lines(state), right);
    match pane {
        Pane::Branches => branches,
        Pane::Worktree | Pane::CommitDetail => below,
        // **Graph と Diff は同じ領域を差し替えで使う**（07）ので同じ Rect を返す。
        // Stash / PRs はまだ描かないので、来たら左を返すだけ。
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
/// ファイル一覧が同じ状態を出すので、散らすと文言を変えたときに揃わなくなる。
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
fn header_line(state: &AppState) -> Paragraph<'_> {
    let mut spans = vec![Span::styled(
        repo_label(state),
        Style::default().add_modifier(Modifier::BOLD),
    )];

    if let Some(target) = state.repo.target.resolved() {
        spans.push(Span::raw(format!("  {}/{}", target.owner, target.repo)));
    }

    Paragraph::new(Line::from(spans))
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
    use super::{file_row, pane_area, window, wrapped};
    use crate::test_util::{draw, state_at, state_showing_diff, state_with_status, terminal_with};
    use mewgit_core::app::{AppState, Cmd, NoticeLevel, Pane, TermSize};
    use mewgit_core::git_exec::GitOp;
    use mewgit_core::test_util::fake_snapshot;
    use ratatui::style::Modifier;

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
    fn the_right_column_stacks_the_branches_over_the_selected_pane() {
        // 左と違って右は差し替えではない。ブランチ一覧はグラフの選択と無関係なので、
        // Working tree（またはコミット詳細）と同時に出る（07 の「レイアウト」）。
        let state = state_at("/repo");

        let text = draw(&state, TermSize::default()).join("\n");

        assert!(text.contains("Branches"), "{text}");
        assert!(text.contains("Working tree"), "{text}");
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
