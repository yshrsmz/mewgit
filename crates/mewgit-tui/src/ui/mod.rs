//! 描画。設計は `docs/design/07-ui.md`。
//!
//! `render` は [`AppState`] のみを参照する（不変ルール 2）。git2 / CLI / HTTP を
//! ここから呼ばない。
//!
//! M0 の時点ではヘッダ・本文プレースホルダ・ステータスバーだけ。07 のレイアウト
//! （グラフ / ブランチ / ワーキングツリー）は M1 以降で各ペインのタスクと一緒に足す。

use mewgit_core::app::AppState;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// 1 フレーム描く。
pub fn render(frame: &mut Frame, state: &AppState) {
    let [header, body, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    frame.render_widget(header_line(state), header);
    frame.render_widget(Block::default().borders(Borders::ALL).title("Graph"), body);
    frame.render_widget(status_line(state), status);
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

/// ステータスバー。直近の通知があればそれ、無ければキーヒント。
fn status_line(state: &AppState) -> Paragraph<'_> {
    match state.notices.back() {
        Some(notice) => Paragraph::new(Line::from(notice.message.clone())),
        None => Paragraph::new(Line::from("q:quit")),
    }
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
    use super::*;
    use mewgit_core::app::{Notice, NoticeLevel, RepoInfo, TermSize};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    /// 描画結果を行ごとの文字列にする。
    ///
    /// 端末のセル格子をそのまま読むので、全角文字は 2 セル目が空白として残る
    /// （「新しい通知」→「新 し い 通 知」）。非 ASCII を含む期待値と突き合わせる
    /// ときは [`without_spaces`] を通すこと。
    fn draw(state: &AppState, size: TermSize) -> Vec<String> {
        let backend = TestBackend::new(size.width, size.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, state)).unwrap();

        let buffer = terminal.backend().buffer();
        (0..size.height)
            .map(|y| {
                (0..size.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// 全角文字のセル分割を吸収する。
    fn without_spaces(s: &str) -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect()
    }

    fn state_at(path: &str) -> AppState {
        AppState::new(RepoInfo {
            path: PathBuf::from(path),
            ..RepoInfo::default()
        })
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
    fn status_bar_shows_the_latest_notice() {
        let mut state = state_at("/repo");
        state.notices.push_back(Notice {
            level: NoticeLevel::Warn,
            message: "古い通知".to_string(),
            detail: None,
        });
        state.notices.push_back(Notice {
            level: NoticeLevel::Error,
            message: "新しい通知".to_string(),
            detail: None,
        });

        let size = TermSize::default();
        let lines = draw(&state, size);
        let last = &lines[size.height as usize - 1];
        assert!(
            without_spaces(last).contains(&without_spaces("新しい通知")),
            "status: {last:?}"
        );
        assert!(!without_spaces(last).contains(&without_spaces("古い通知")));
    }

    #[test]
    fn body_is_drawn_between_header_and_status() {
        let lines = draw(&state_at("/repo"), TermSize::default());
        assert!(lines[1].contains("Graph"), "body: {:?}", lines[1]);
    }

    #[test]
    fn renders_at_a_wide_terminal() {
        // リサイズ後のサイズでも落ちないこと。
        let lines = draw(
            &state_at("/repo"),
            TermSize {
                width: 120,
                height: 40,
            },
        );
        assert_eq!(lines.len(), 40);
        assert!(lines[39].contains("q:quit"));
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
    fn renders_at_a_narrow_terminal() {
        let lines = draw(
            &state_at("/repo"),
            TermSize {
                width: 40,
                height: 10,
            },
        );
        assert_eq!(lines.len(), 10);
        assert!(lines[0].contains("repo"));
    }
}
