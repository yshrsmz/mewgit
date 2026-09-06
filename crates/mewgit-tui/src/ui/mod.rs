//! 描画。設計は `docs/design/07-ui.md`。
//!
//! `render` は [`AppState`] のみを参照する（不変ルール 2）。git2 / CLI / HTTP を
//! ここから呼ばない。
//!
//! いまはヘッダ・グラフ・ワーキングツリー・ステータスバー。右上の
//! `Branches | Stash | PRs` は、そのタブを実装するタスクで足す（07）。

pub mod lanes;
pub mod theme;
pub mod time;

use crate::{modals, panes};
use mewgit_core::app::{AppState, TermSize};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// 1 フレーム描く。
pub fn render(frame: &mut Frame, state: &AppState) {
    let [header, body, status] = split(frame.area());
    let [left, right] = split_body(body);

    frame.render_widget(header_line(state), header);
    panes::graph::render(frame, left, state);
    panes::worktree::render(frame, right, state);
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

/// ペインの枠。
///
/// **フォーカスのあるペインは枠を強調する**（07）。どちらがキーを受けるか判らないと、
/// `s` や `x` が別のペインに飛んだように見える。枠の内側の大きさは強調の有無で
/// 変わらないので、レイアウトの計算は `focused` を偽で呼んでよい。
pub fn pane_block(title: String, focused: bool) -> Block<'static> {
    let block = Block::default().borders(Borders::ALL).title(title);
    if focused {
        block.border_style(Style::default().add_modifier(Modifier::BOLD))
    } else {
        block
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
/// 引数は端末サイズと「詳細を出すか」の 2 つだけ。`&AppState` を丸ごと受けると、
/// あとから詳細の中身（メッセージの行数など）を見る余地が残る。そうすると詳細が
/// 届いた瞬間に行数が変わってグラフが跳ねる（07）。判定は
/// [`panes::graph::shows_detail`]。
pub fn graph_rows(term: TermSize, shows_detail: bool) -> usize {
    let [_, body, _] = split(Rect::new(0, 0, term.width, term.height));
    let [left, _] = split_body(body);
    panes::graph::areas(left, shows_detail).0.height as usize
}

/// ヘッダ。リポジトリと、解決できていればアカウントを出す。
fn header_line(state: &AppState) -> Paragraph<'_> {
    let mut spans = vec![Span::styled(
        repo_label(state),
        Style::default().add_modifier(Modifier::BOLD),
    )];

    if let Some(target) = &state.repo.target {
        spans.push(Span::raw(format!("  {}/{}", target.owner, target.repo)));
    }

    Paragraph::new(Line::from(spans))
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
    use super::{split, split_body};
    use crate::test_util::{draw, state_at, terminal_with};
    use mewgit_core::app::{NoticeLevel, Pane, TermSize};
    use ratatui::layout::Rect;
    use ratatui::style::Modifier;

    /// ペインの枠の左上。座標は実際のレイアウトから出す。手で書くと、割り方を
    /// 変えたときにテストだけが古い桁を見て緑のまま無意味になる。
    fn corner(pane: Pane, size: TermSize) -> (u16, u16) {
        let [_, body, _] = split(Rect::new(0, 0, size.width, size.height));
        let [left, right] = split_body(body);
        let area = match pane {
            Pane::Worktree => right,
            _ => left,
        };
        (area.x, area.y)
    }

    #[test]
    fn the_focused_pane_is_the_only_one_with_a_bold_border() {
        // どちらがキーを受けるか判らないと、`s` や `x` が別のペインに飛んだように見える。
        let size = TermSize::default();
        for focus in [Pane::Graph, Pane::Worktree] {
            let mut state = state_at("/repo");
            state.focus = focus;
            let terminal = terminal_with(&state, size);
            let buffer = terminal.backend().buffer();

            for pane in [Pane::Graph, Pane::Worktree] {
                let bold = buffer[corner(pane, size)].modifier.contains(Modifier::BOLD);
                assert_eq!(bold, pane == focus, "focus={focus:?} pane={pane:?}");
            }
        }
    }

    #[test]
    fn header_shows_the_repository_name() {
        let lines = draw(&state_at("/home/user/mewgit"), TermSize::default());
        assert!(lines[0].contains("mewgit"), "header: {:?}", lines[0]);
    }

    #[test]
    fn status_bar_shows_the_quit_hint_when_there_is_no_notice() {
        let size = TermSize::default();
        let lines = draw(&state_at("/repo"), size);
        let last = &lines[size.height as usize - 1];
        assert!(last.contains("q:quit"), "status: {last:?}");
    }

    #[test]
    fn status_bar_shows_the_latest_notice_alongside_the_hint() {
        let mut state = state_at("/repo");
        state.push_notice(NoticeLevel::Warn, "古い通知".to_string(), None);
        state.push_notice(NoticeLevel::Error, "新しい通知".to_string(), None);

        let size = TermSize::default();
        let lines = draw(&state, size);
        let last = &lines[size.height as usize - 1];

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
        let mut state = state_at("/repo");
        state.push_notice(
            NoticeLevel::Warn,
            "設定ファイルの構文エラー
  |
2 | ascii = 
  | ^"
            .to_string(),
            None,
        );

        let size = TermSize::default();
        let lines = draw(&state, size);
        let last = &lines[size.height as usize - 1];

        assert!(
            last.contains("設定ファイルの構文エラー"),
            "status: {last:?}"
        );
        assert!(!last.contains('|'), "status: {last:?}");
        assert!(last.contains("q:quit"), "status: {last:?}");
    }

    #[test]
    fn repo_label_falls_back_to_the_path_when_there_is_no_file_name() {
        // "." や "/" は file_name() が None。空ヘッダにしない。
        let lines = draw(&state_at("."), TermSize::default());
        assert!(lines[0].contains('.'), "header: {:?}", lines[0]);
    }

    #[test]
    fn renders_at_other_terminal_sizes() {
        // リサイズ後のサイズでも落ちないこと。
        for (width, height) in [(120, 40), (40, 10)] {
            let lines = draw(&state_at("/repo"), TermSize { width, height });
            assert_eq!(lines.len(), height as usize);
            assert!(lines[0].contains("repo"), "header: {:?}", lines[0]);

            let last = &lines[height as usize - 1];
            assert!(last.contains("q:quit"), "status: {last:?}");
        }
    }
}
