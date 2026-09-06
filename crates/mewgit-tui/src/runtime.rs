//! イベントループと [`Cmd`] のディスパッチ。設計は `docs/design/01-app-core.md`。

use std::ops::ControlFlow;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use futures::{Stream, StreamExt};
use mewgit_core::app::{
    init, update, AppState, Cmd, JobKind, Msg, NoticeLevel, RepoInfo, RepoSnapshot, TermSize,
};
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

/// リポジトリを開いて初期状態を作る。
///
/// ここでやるのは `open` だけ。bare や存在しないパスは起動できないので `Err` を返す
/// （端末を初期化する前に出す）。リモート・HEAD・ref は `Cmd::LoadRepoInfo` として
/// ループの中で読む。ref の列挙は本数ぶんの object 読み出しになるので、待つと
/// 最初の 1 フレームが出るまで画面が真っ黒になる。
pub fn open(path: &Path, config: Config) -> Result<(AppState, ReaderHandle), GitReadError> {
    let reader = ReaderHandle::spawn(path)?;
    let repo = RepoInfo {
        path: reader.workdir().to_path_buf(),
        ..RepoInfo::default()
    };

    Ok((AppState::new(repo, config), reader))
}

/// 描画 → 入力待ち → `update` → `dispatch` を回す。
///
/// `Backend` とイベントストリームをジェネリックに取るのは、`TestBackend` と
/// `futures::stream::iter` を渡してループ全体をテストできるようにするため。
pub async fn run<B, S>(
    terminal: &mut Terminal<B>,
    mut events: S,
    state: &mut AppState,
    reader: ReaderHandle,
) -> Result<()>
where
    B: Backend,
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let mut tick = tokio::time::interval(TICK);
    let (runtime, mut msgs) = Runtime::new(reader);

    // 起動時の読み込み。ループの前に 1 回だけ。**サイズを入れるより先に出す。**
    // 逆にすると、サイズを知った時点で `load_more` が 1 ページ目を頼み、`init` の
    // `Cmd::LoadLog` が `issue` に落とされる。最初のページを頼むのが `init` でなく
    // なり、テストと設計書（01）が主張する経路とバイナリの経路が食い違う。
    // `terminal.size()` が失敗しても読める、という保険もこの順序で効く。
    let cmd = init(state);
    if runtime.dispatch(state, cmd).is_break() {
        return Ok(());
    }

    // 起動時に `Resize` は飛んでこないので、最初のサイズは自分で入れる。
    // 戻ってきた `Cmd` も流す（`init` が読んでいる最中なら `issue` が落とす）。
    if let Ok(size) = terminal.size() {
        let msg = crate::input::resized(TermSize {
            width: size.width,
            height: size.height,
        });
        let cmd = update(state, msg);
        if runtime.dispatch(state, cmd).is_break() {
            return Ok(());
        }
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

            Cmd::LoadRepoInfo => {
                let reader = self.reader.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send(Msg::RepoInfoLoaded(Box::new(repo_info(&reader).await)));
                });
                ControlFlow::Continue(())
            }

            // M2 以降で実装する。
            Cmd::LoadStatus => not_implemented(state, JobKind::LoadStatus, "LoadStatus"),
            Cmd::LoadDiff(_) => not_implemented(state, JobKind::LoadDiff, "LoadDiff"),
            Cmd::Exec(_) => not_implemented(state, JobKind::Exec, "Exec"),
            Cmd::ResolveAccount => {
                not_implemented(state, JobKind::ResolveAccount, "ResolveAccount")
            }
            Cmd::FetchPr => not_implemented(state, JobKind::FetchPr, "FetchPr"),
            Cmd::FetchChecks => not_implemented(state, JobKind::FetchChecks, "FetchChecks"),
            Cmd::FetchPrList => not_implemented(state, JobKind::FetchPrList, "FetchPrList"),
        }
    }
}

/// リモート・HEAD・ref をまとめて読む。
///
/// リーダースレッドは 1 本なので、並行にしても待ち時間は変わらない。
async fn repo_info(reader: &ReaderHandle) -> Result<RepoSnapshot, GitReadError> {
    Ok(RepoSnapshot {
        remotes: reader.remotes().await?,
        head: reader.head().await?,
        refs: reader.refs().await?,
    })
}

/// 未実装の `Cmd` を通知に積む。黙って捨てると開発中に気付けない。
///
/// 登録済みのジョブもここで畳む。`update` の出口（`AppState::issue`）が登録したまま
/// 誰も完了させないと、2 回目以降の同じ `Cmd` が通知も出さずに捨てられ、
/// 「1 回だけ警告が出てあとは無反応」になる。`dispatch` が `notices` 以外に書く
/// 唯一の場所で、`Cmd` を実装するたびに 1 行ずつ消えていく。
fn not_implemented(state: &mut AppState, kind: JobKind, name: &str) -> ControlFlow<()> {
    state.jobs.finish(kind);
    state.push_notice(
        NoticeLevel::Warn,
        format!("未実装のコマンドです: {name}"),
        None,
    );
    ControlFlow::Continue(())
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

    /// リポジトリを開いた状態とハンドル。
    fn opened(repo: &TestRepo) -> (AppState, ReaderHandle) {
        open(repo.path(), Config::default()).expect("リポジトリを開けませんでした")
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

    #[test]
    fn open_finds_the_worktree_without_reading_anything_else() {
        // ref の列挙は本数ぶんの object 読み出しになる。ここで待つと最初の 1 フレームが
        // 出るまで画面が真っ黒になるので、`Cmd::LoadRepoInfo` に回している。
        let repo = TestRepo::init();
        repo.commit("first");

        let (state, _reader) = opened(&repo);

        assert_eq!(state.repo.path, repo.path());
        assert!(state.repo.remotes.is_empty());
        assert!(state.repo.head.is_none());
        assert!(state.notices.is_empty());
    }

    #[tokio::test]
    async fn load_repo_info_fills_in_the_remotes_head_and_ref_labels() {
        let repo = TestRepo::init();
        let oid = repo.commit("first");
        repo.git(&["remote", "add", "origin", "git@github.com:owner/repo.git"]);
        let (mut state, reader) = opened(&repo);
        let (runtime, mut msgs) = Runtime::new(reader);

        assert!(runtime
            .dispatch(&mut state, Cmd::LoadRepoInfo)
            .is_continue());
        let msg = msgs.recv().await.expect("Msg が返りませんでした");
        let _ = update(&mut state, msg);

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
            .map(|label| label.name())
            .collect();
        assert_eq!(labels, ["main"]);
    }

    #[test]
    fn open_fails_on_a_repository_that_cannot_be_used() {
        let repo = BareRepo::init();

        let err = open(repo.path(), Config::default()).unwrap_err();

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
        let (mut state, reader) = opened(&repo);
        let (runtime, mut msgs) = Runtime::new(reader);

        let cmd = init(&mut state);
        assert!(runtime.dispatch(&mut state, cmd).is_continue());

        // 非同期タスクが返した Msg をループの代わりに受け取る。
        let msg = msgs.recv().await.expect("Msg が返りませんでした");
        let _ = update(&mut state, msg);

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
        let (mut state, reader) = opened(&repo);
        run_with(vec![key(KeyCode::Char('q'))], &mut state, reader)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn ctrl_c_ends_the_loop() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo);
        let ctrl_c = Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        run_with(vec![ctrl_c], &mut state, reader).await.unwrap();
    }

    #[tokio::test]
    async fn resize_reaches_the_state_before_quitting() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo);
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
        let (mut state, reader) = opened(&repo);
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
        let (mut state, reader) = opened(&repo);
        run_with(vec![Ok(Event::Resize(100, 30))], &mut state, reader)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn stream_errors_are_not_swallowed() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo);
        let err = Err(std::io::Error::other("端末の読み取りに失敗"));

        let result = run_with(vec![err], &mut state, reader).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn quit_breaks_the_loop() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo);
        let (runtime, _msgs) = Runtime::new(reader);
        assert!(runtime.dispatch(&mut state, Cmd::Quit).is_break());
    }

    #[tokio::test]
    async fn none_continues() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo);
        let (runtime, _msgs) = Runtime::new(reader);
        assert!(runtime.dispatch(&mut state, Cmd::None).is_continue());
        assert!(state.notices.is_empty());
    }

    #[tokio::test]
    async fn batch_stops_at_quit() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo);
        let (runtime, _msgs) = Runtime::new(reader);

        // Quit の後ろは実行しない。未実装 notice が 1 件だけであることで確かめる。
        let batch = Cmd::Batch(vec![Cmd::LoadStatus, Cmd::Quit, Cmd::FetchPr]);
        assert!(runtime.dispatch(&mut state, batch).is_break());
        assert_eq!(state.notices.len(), 1);
    }

    #[tokio::test]
    async fn unimplemented_cmd_warns_instead_of_disappearing() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo);
        let (runtime, _msgs) = Runtime::new(reader);

        assert!(runtime.dispatch(&mut state, Cmd::LoadStatus).is_continue());

        assert_eq!(state.notices.len(), 1);
        let notice = &state.notices[0];
        assert_eq!(notice.level, NoticeLevel::Warn);
        assert!(notice.message.contains("LoadStatus"));
    }

    #[test]
    fn an_unimplemented_cmd_does_not_hold_on_to_its_job() {
        // 掴んだままだと、2 回目以降の同じ `Cmd` が `issue` に落とされて
        // 通知も出ない（「1 回だけ警告が出てあとは無反応」になる）。
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo);
        let (runtime, _msgs) = Runtime::new(reader);

        let cmd = state.issue(Cmd::LoadStatus);
        assert!(runtime.dispatch(&mut state, cmd).is_continue());
        assert!(!state.jobs.is_running(JobKind::LoadStatus));

        let again = state.issue(Cmd::LoadStatus);
        assert!(runtime.dispatch(&mut state, again).is_continue());
        assert_eq!(state.notices.len(), 2);
    }

    #[tokio::test]
    async fn nested_batch_is_dispatched() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo);
        let (runtime, _msgs) = Runtime::new(reader);

        let nested = Cmd::Batch(vec![Cmd::Batch(vec![Cmd::LoadStatus]), Cmd::FetchPr]);
        assert!(runtime.dispatch(&mut state, nested).is_continue());
        assert_eq!(state.notices.len(), 2);
    }

    #[tokio::test]
    async fn the_graph_pane_has_focus_at_startup() {
        let repo = TestRepo::init();
        let (state, _reader) = opened(&repo);
        assert_eq!(state.focus, Pane::Graph);
    }
}
