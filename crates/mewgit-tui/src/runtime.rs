//! イベントループと [`Cmd`] のディスパッチ。設計は `docs/design/01-app-core.md`。

use std::ops::ControlFlow;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use futures::{Stream, StreamExt};
use mewgit_core::app::{init, update, AppState, Cmd, Msg, NoticeLevel, RepoInfo, TermSize};
use mewgit_core::config::Config;
use mewgit_core::git_read::{GitReadError, ReaderHandle};
use ratatui::backend::Backend;
use ratatui::crossterm::event::Event;
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::input::event_to_msg;
use crate::ui;

/// `Tick` の間隔。進行中ジョブのスピナーと外部変更検知に使う。
const TICK: Duration = Duration::from_millis(250);

/// 非同期タスクとイベントループをつなぐもの。
///
/// [`ReaderHandle`] は `Cmd` ごとに起こす tokio タスクへ渡すのでクローンできる必要が
/// あり、結果は `tx` に流して `run` の `select!` が拾う。
pub struct Runtime {
    reader: ReaderHandle,
    tx: mpsc::UnboundedSender<Msg>,
}

/// リポジトリを開き、起動時に読めるものを読んで初期状態を作る。
///
/// 開けなければ起動できないので `Err` を返す。リモートや HEAD の読み取りだけが
/// 失敗したときは通知に積んで続ける。ref が無くてもグラフは出せる。
pub async fn open(path: &Path, config: Config) -> Result<(AppState, ReaderHandle), GitReadError> {
    let reader = ReaderHandle::spawn(path)?;

    let mut repo = RepoInfo {
        path: reader.workdir().to_path_buf(),
        ..RepoInfo::default()
    };
    let mut warnings = Vec::new();

    match reader.remotes().await {
        Ok(remotes) => repo.remotes = remotes,
        Err(err) => warnings.push(err),
    }
    match reader.head().await {
        Ok(head) => repo.head = Some(head),
        Err(err) => warnings.push(err),
    }

    let mut state = AppState::new(repo, config);
    match reader.refs().await {
        Ok(refs) => state.graph.refs = refs,
        Err(err) => warnings.push(err),
    }
    for warning in warnings {
        state.push_notice(NoticeLevel::Warn, warning.to_string(), None);
    }

    Ok((state, reader))
}

/// 描画 → 入力待ち → `update` → `dispatch` を回す。
///
/// `Backend` とイベントストリームをジェネリックに取るのは、`TestBackend` と
/// `futures::stream::iter` を渡してループ全体をテストできるようにするため。
pub async fn run<B, S>(
    terminal: &mut Terminal<B>,
    events: S,
    state: &mut AppState,
    reader: ReaderHandle,
) -> Result<()>
where
    B: Backend,
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let mut events = events;
    let mut tick = tokio::time::interval(TICK);
    let (runtime, mut msgs) = Runtime::new(reader);

    // 起動時の読み込み。ループの前に 1 回だけ。
    let cmd = init(state);
    if runtime.dispatch(state, cmd).is_break() {
        return Ok(());
    }

    loop {
        terminal.draw(|frame| ui::render(frame, state))?;

        let msg = tokio::select! {
            event = events.next() => match event {
                Some(event) => match event_to_msg(event?) {
                    Some(msg) => msg,
                    // 対応する Msg が無いイベントは捨てる。
                    None => continue,
                },
                // 入力が尽きた（端末が閉じた、テストのストリームが終わった）。
                // ここで抜けないと Tick だけで永久に回り続ける。
                None => return Ok(()),
            },
            // 送り手は `runtime` が持っているので、この分岐が閉じることはない。
            Some(msg) = msgs.recv() => msg,
            _ = tick.tick() => Msg::Tick,
        };

        let cmd = update(state, msg);
        if runtime.dispatch(state, cmd).is_break() {
            return Ok(());
        }
    }
}

impl Runtime {
    fn new(reader: ReaderHandle) -> (Self, mpsc::UnboundedReceiver<Msg>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { reader, tx }, rx)
    }

    /// [`Cmd`] を実行する。終了なら [`ControlFlow::Break`]。
    ///
    /// `AppState` に書いてよいのは `notices` だけ。それ以外の状態変化は [`Msg`] にして
    /// `update` に戻す。状態遷移の入口を `update` 1 つに保つため（ジョブの登録も
    /// `update` の出口の [`AppState::issue`] が済ませている）。
    ///
    /// `match` で `_` を使わないのは、`Cmd` に変種を足したときに実装漏れをコンパイル
    /// エラーにするため（`update` や `Cmd::job_kind` と同じ）。
    pub fn dispatch(&self, state: &mut AppState, cmd: Cmd) -> ControlFlow<()> {
        match cmd {
            Cmd::None => ControlFlow::Continue(()),
            Cmd::Batch(cmds) => {
                for cmd in cmds {
                    self.dispatch(state, cmd)?;
                }
                ControlFlow::Continue(())
            }
            Cmd::Quit => ControlFlow::Break(()),

            Cmd::LoadLog { count } => {
                let reader = self.reader.clone();
                let tx = self.tx.clone();
                // このタスクは abort しない。`log_page` の future を途中で捨てると、
                // リーダースレッドは walk を進めたあとで返り先を失い、そのページの
                // コミットがどのページにも現れなくなる（02 の「スレッド」）。
                tokio::spawn(async move {
                    let _ = tx.send(Msg::LogLoaded(reader.log_page(count).await));
                });
                ControlFlow::Continue(())
            }

            // M2 以降で実装する。
            Cmd::LoadStatus => not_implemented(state, "LoadStatus"),
            Cmd::LoadDiff(_) => not_implemented(state, "LoadDiff"),
            Cmd::Exec(_) => not_implemented(state, "Exec"),
            Cmd::ResolveAccount => not_implemented(state, "ResolveAccount"),
            Cmd::FetchPr => not_implemented(state, "FetchPr"),
            Cmd::FetchChecks => not_implemented(state, "FetchChecks"),
            Cmd::FetchPrList => not_implemented(state, "FetchPrList"),
        }
    }
}

/// 未実装の `Cmd` を通知に積む。黙って捨てると開発中に気付けない。
fn not_implemented(state: &mut AppState, name: &str) -> ControlFlow<()> {
    state.push_notice(
        NoticeLevel::Warn,
        format!("未実装のコマンドです: {name}"),
        None,
    );
    ControlFlow::Continue(())
}

/// 端末サイズを [`TermSize`] にする。起動時に `Resize` は飛んでこないので自分で取る。
pub fn term_size<B: Backend>(terminal: &Terminal<B>) -> Option<TermSize> {
    terminal.size().ok().map(|size| TermSize {
        width: size.width,
        height: size.height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use mewgit_core::app::{Pane, TermSize};
    use mewgit_core::git_read::HeadInfo;
    use mewgit_core::test_util::{BareRepo, TestRepo};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// コミットを 1 つ持つリポジトリと、それを開いた状態・ハンドル。
    async fn opened(repo: &TestRepo) -> (AppState, ReaderHandle) {
        open(repo.path(), Config::default())
            .await
            .expect("リポジトリを開けませんでした")
    }

    fn key(code: KeyCode) -> std::io::Result<Event> {
        Ok(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    /// 与えたイベントを順に流してループを 1 本走らせる。
    async fn run_with(
        events: Vec<std::io::Result<Event>>,
        state: &mut AppState,
        reader: ReaderHandle,
    ) -> Result<()> {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        run(&mut terminal, stream::iter(events), state, reader).await
    }

    #[tokio::test]
    async fn open_fills_in_the_repository_and_the_ref_labels() {
        let repo = TestRepo::init();
        let oid = repo.commit("first");
        repo.git(&["remote", "add", "origin", "git@github.com:owner/repo.git"]);

        let (state, _reader) = opened(&repo).await;

        assert_eq!(state.repo.path, repo.path());
        assert_eq!(state.repo.remotes.len(), 1);
        assert_eq!(
            state.repo.head,
            Some(HeadInfo::Branch {
                name: "main".to_string(),
                oid
            })
        );
        let labels: Vec<&str> = state
            .graph
            .refs
            .labels(oid)
            .iter()
            .map(|l| l.name())
            .collect();
        assert_eq!(labels, ["main"]);
        assert!(state.notices.is_empty());
    }

    #[tokio::test]
    async fn open_fails_on_a_repository_that_cannot_be_used() {
        let repo = BareRepo::init();

        let err = open(repo.path(), Config::default()).await.unwrap_err();

        assert!(
            matches!(err, GitReadError::BareRepository { .. }),
            "bare を拒否していません: {err:?}"
        );
    }

    #[tokio::test]
    async fn load_log_reaches_the_state_through_the_reader() {
        let repo = TestRepo::init();
        repo.commit("first");
        repo.commit("second");
        let (mut state, reader) = opened(&repo).await;
        let (runtime, mut msgs) = Runtime::new(reader);

        let cmd = init(&mut state);
        assert!(runtime.dispatch(&mut state, cmd).is_continue());

        // 非同期タスクが返した Msg をループの代わりに受け取る。
        let msg = msgs.recv().await.expect("Msg が返りませんでした");
        update(&mut state, msg);

        let summaries: Vec<&str> = state
            .graph
            .lines
            .iter()
            .map(|line| line.commit.summary.as_str())
            .collect();
        assert_eq!(summaries, ["second", "first"]);
        assert!(state.graph.loaded_all);
    }

    #[tokio::test]
    async fn q_ends_the_loop() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo).await;
        run_with(vec![key(KeyCode::Char('q'))], &mut state, reader)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn ctrl_c_ends_the_loop() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo).await;
        let ctrl_c = Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        run_with(vec![ctrl_c], &mut state, reader).await.unwrap();
    }

    #[tokio::test]
    async fn resize_reaches_the_state_before_quitting() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo).await;
        let events = vec![Ok(Event::Resize(120, 40)), key(KeyCode::Char('q'))];

        run_with(events, &mut state, reader).await.unwrap();
        assert_eq!(
            state.term,
            TermSize {
                width: 120,
                height: 40
            }
        );
    }

    #[tokio::test]
    async fn unmapped_events_are_skipped_without_ending_the_loop() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo).await;
        let events = vec![
            Ok(Event::FocusGained),
            key(KeyCode::CapsLock),
            key(KeyCode::Char('q')),
        ];

        run_with(events, &mut state, reader).await.unwrap();
    }

    #[tokio::test]
    async fn exhausted_event_stream_ends_the_loop() {
        // 終了キーが来なくても、入力が尽きたら抜ける。抜けないと Tick だけで
        // 永久に回り、テストがハングする。
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo).await;
        run_with(vec![Ok(Event::Resize(100, 30))], &mut state, reader)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn stream_errors_are_not_swallowed() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo).await;
        let err = Err(std::io::Error::other("端末の読み取りに失敗"));

        let result = run_with(vec![err], &mut state, reader).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn quit_breaks_the_loop() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo).await;
        let (runtime, _msgs) = Runtime::new(reader);
        assert!(runtime.dispatch(&mut state, Cmd::Quit).is_break());
    }

    #[tokio::test]
    async fn none_continues() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo).await;
        let (runtime, _msgs) = Runtime::new(reader);
        assert!(runtime.dispatch(&mut state, Cmd::None).is_continue());
        assert!(state.notices.is_empty());
    }

    #[tokio::test]
    async fn batch_stops_at_quit() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo).await;
        let (runtime, _msgs) = Runtime::new(reader);

        // Quit の後ろは実行しない。未実装 notice が 1 件だけであることで確かめる。
        let batch = Cmd::Batch(vec![Cmd::LoadStatus, Cmd::Quit, Cmd::FetchPr]);
        assert!(runtime.dispatch(&mut state, batch).is_break());
        assert_eq!(state.notices.len(), 1);
    }

    #[tokio::test]
    async fn unimplemented_cmd_warns_instead_of_disappearing() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo).await;
        let (runtime, _msgs) = Runtime::new(reader);

        assert!(runtime.dispatch(&mut state, Cmd::LoadStatus).is_continue());

        assert_eq!(state.notices.len(), 1);
        let notice = &state.notices[0];
        assert_eq!(notice.level, NoticeLevel::Warn);
        assert!(notice.message.contains("LoadStatus"));
    }

    #[tokio::test]
    async fn nested_batch_is_dispatched() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo).await;
        let (runtime, _msgs) = Runtime::new(reader);

        let nested = Cmd::Batch(vec![Cmd::Batch(vec![Cmd::LoadStatus]), Cmd::FetchPr]);
        assert!(runtime.dispatch(&mut state, nested).is_continue());
        assert_eq!(state.notices.len(), 2);
    }

    #[tokio::test]
    async fn the_graph_pane_has_focus_at_startup() {
        let repo = TestRepo::init();
        let (state, _reader) = opened(&repo).await;
        assert_eq!(state.focus, Pane::Graph);
    }
}
