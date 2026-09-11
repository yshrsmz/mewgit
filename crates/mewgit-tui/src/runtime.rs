//! イベントループと [`Cmd`] のディスパッチ。設計は `docs/design/01-app-core.md`。

use std::ops::ControlFlow;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use futures::{Stream, StreamExt};
use mewgit_core::app::{
    init, update, AppState, Cmd, ExecDone, JobKind, Msg, NoticeLevel, RepoInfo, RepoSnapshot,
    TermSize,
};
use mewgit_core::auth::ssh::{effective_ssh_from_env, SshCommand};
use mewgit_core::auth::{self, RepoTarget};
// `logins` を呼ぶためだけの import。**名前は入れない** —— `ratatui::backend::Backend`
// と衝突する。
use mewgit_core::auth::Backend as _;
use mewgit_core::config::Config;
use mewgit_core::git_exec::Runner;
use mewgit_core::git_read::{GitReadError, ReaderHandle};
use mewgit_core::github::AccountBackend;
use ratatui::backend::Backend;
use ratatui::crossterm::event::Event;
use ratatui::Terminal;
use tokio::sync::{mpsc, oneshot};

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
    /// `git` CLI の実行（03）。`Cmd::Exec` ごとに `clone` してタスクへ渡す。
    runner: Runner,
    tx: mpsc::UnboundedSender<Msg>,
    /// 走っている [`Cmd::Exec`] への中断の合図（01 の「実行中ジョブのキャンセル」）。
    ///
    /// **`JoinHandle::abort` は使わない。** タスクを外から落とすと
    /// [`Msg::ExecFinished`] が誰からも来ず、`JobKind::Exec` が走ったまま残って
    /// 以後の書き込みが全部「ほかの git 実行が終わるまで待ってください」で落ちる。
    /// 合図を受けたタスク自身が、中断した結果を `Msg` にして返す。
    ///
    /// 書き込みは同時に 1 つだけ（`AppState::issue`）なので 1 本で足りる。使い終えた
    /// 送り手が残ることはあるが、`Exec` が走っていなければ `update` が
    /// [`Cmd::CancelExec`] を出さないので撃たれない。
    cancel: Option<oneshot::Sender<()>>,
    /// git が実際に使う ssh（04）。[`Cmd::ResolveTarget`] が `ssh -G` をこれで叩く。
    ///
    /// **`Runner` と同じ値を持つ。** `Runner::new` が受け取るのは
    /// `-o BatchMode=yes` を重ねたあとの文字列なので、そこからは取り出せない。
    ssh: SshCommand,
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
    // ratatui 0.30 で `Backend::Error` が関連型になり、`io::Error` 固定ではなくなった。
    // `?` で `anyhow::Error` に載せるので、`anyhow` が `From` を張れる境界を要求する。
    B::Error: std::error::Error + Send + Sync + 'static,
    S: Stream<Item = std::io::Result<Event>> + Unpin,
{
    let mut tick = tokio::time::interval(TICK);
    // `git_bin` は設定から渡す（03 の「Runner」）。cwd はワークツリーのルートで、
    // 引数に渡されたサブディレクトリではない（02 の `workdir`）。
    // 実効 ssh は `Runner` が受け取る（03 の「BatchMode の重ね方」）。
    let ssh = effective_ssh_from_env(reader.ssh_command());
    let runner = Runner::new(
        state.repo.path.clone(),
        &state.config.general.git_bin,
        ssh.clone(),
    );
    let (mut runtime, mut msgs) = Runtime::new(reader, runner, ssh);

    // 起動時の読み込み。ループの前に 1 回だけ。**サイズを入れるより先に出す。**
    // 逆にすると、サイズを知った時点で `load_more` が 1 ページ目を頼み、`init` の
    // `Cmd::LoadLog` が `issue` に落とされる。最初のページを頼むのが `init` でなく
    // なり、テストと設計書（01）が主張する経路とバイナリの経路が食い違う。
    // `terminal.size()` が失敗しても読める、という保険もこの順序で効く。
    let cmd = init(state);
    if runtime.dispatch(state, cmd).is_break() {
        return Ok(());
    }

    // 起動時に `Resize` は飛んでこないので、最初のサイズは自分で入れる。`Resize` が
    // するのは `state.term` の更新だけなので `Cmd` は必ず `None`。グラフの行数は
    // このあとループ先頭の `Msg::Layout` が入れるので、`terminal.size()` が失敗しても
    // 既定のサイズぶんは入る。
    if let Ok(size) = terminal.size() {
        let _ = update(
            state,
            Msg::Resize {
                term: TermSize {
                    width: size.width,
                    height: size.height,
                },
            },
        );
    }

    loop {
        // レイアウトの変化を core に伝える唯一の経路。端末のリサイズも `Enter` の
        // 展開も同じところを通るので、片方だけ更新を忘れることがない（01）。
        let cmd = layout(state);
        if runtime.dispatch(state, cmd).is_break() {
            return Ok(());
        }

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

/// グラフに使える行数が変わっていたら [`Msg::Layout`] を流す。
///
/// 行数を決めるのは `ui` だけ（不変ルール 2 と 07）。`AppState` に直接書かず `update`
/// を通すのは、`viewport` の計算経路を 1 本に保つため。
fn layout(state: &mut AppState) -> Cmd {
    let graph_rows = ui::graph_rows(state.term);
    if graph_rows == state.graph.viewport {
        return Cmd::None;
    }
    update(state, Msg::Layout { graph_rows })
}

impl Runtime {
    fn new(
        reader: ReaderHandle,
        runner: Runner,
        ssh: SshCommand,
    ) -> (Self, mpsc::UnboundedReceiver<Msg>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                reader,
                runner,
                tx,
                cancel: None,
                ssh,
            },
            rx,
        )
    }

    /// [`Cmd`] を実行する。終了なら [`ControlFlow::Break`]。
    ///
    /// `AppState` に書いてよいのは `notices` だけ。それ以外の状態変化は [`Msg`] にして
    /// `update` に戻す。状態遷移の入口を `update` 1 つに保つため（ジョブの登録も
    /// `update` の出口の [`AppState::issue`] が済ませている）。
    ///
    /// `match` で `_` を使わないのは、`Cmd` に変種を足したときに実装漏れをコンパイル
    /// エラーにするため（`update` や `Cmd::job_kind` と同じ）。
    pub fn dispatch(&mut self, state: &mut AppState, cmd: Cmd) -> ControlFlow<()> {
        match cmd {
            Cmd::None => ControlFlow::Continue(()),
            Cmd::Batch(cmds) => {
                for cmd in cmds {
                    self.dispatch(state, cmd)?;
                }
                ControlFlow::Continue(())
            }
            Cmd::Quit => ControlFlow::Break(()),

            Cmd::LoadLog { count, restart } => {
                let reader = self.reader.clone();
                let tx = self.tx.clone();
                // このタスクは abort しない。`log_page` の future を途中で捨てると、
                // リーダースレッドは walk を進めたあとで返り先を失い、そのページの
                // コミットがどのページにも現れなくなる（02 の「スレッド」）。
                tokio::spawn(async move {
                    // 要求した `restart` をそのまま返す。届いたページが先頭からの
                    // ものか続きかは `LogPage` からは判らない（01）。
                    let result = reader.log_page(count, restart).await;
                    let _ = tx.send(Msg::LogLoaded { restart, result });
                });
                ControlFlow::Continue(())
            }

            Cmd::LoadHeadMessage => {
                let reader = self.reader.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send(Msg::HeadMessageLoaded(reader.head_message().await));
                });
                ControlFlow::Continue(())
            }

            Cmd::LoadCommitDetail(oid) => {
                let reader = self.reader.clone();
                let tx = self.tx.clone();
                // `log_page` と違って状態を持たない要求なので、このタスクは
                // 途中で捨てても失うものが無い（02 の「スレッド」）。
                tokio::spawn(async move {
                    let result = Box::new(reader.commit_detail(oid).await);
                    let _ = tx.send(Msg::CommitDetailLoaded { oid, result });
                });
                ControlFlow::Continue(())
            }

            Cmd::LoadCommitFiles(oid) => {
                let reader = self.reader.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let result = Box::new(reader.commit_files(oid).await);
                    let _ = tx.send(Msg::CommitFilesLoaded { oid, result });
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

            Cmd::LoadBranches => {
                let reader = self.reader.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send(Msg::BranchesLoaded(Box::new(reader.branches().await)));
                });
                ControlFlow::Continue(())
            }

            Cmd::LoadStatus => {
                let reader = self.reader.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send(Msg::StatusLoaded(Box::new(reader.status().await)));
                });
                ControlFlow::Continue(())
            }

            Cmd::Exec(op) => {
                let runner = self.runner.clone();
                let tx = self.tx.clone();
                // 中断の合図。**受け側はこのタスクの中**で、殺したあとの結果も
                // このタスクが `Msg` にして返す。
                let (cancel, canceled) = oneshot::channel();
                self.cancel = Some(cancel);
                tokio::spawn(async move {
                    let result = runner.run_cancelable(&op, canceled).await;
                    // 進行中操作の再検出は M5（03）。それまではここで `None` を入れる。
                    let done = ExecDone {
                        op,
                        result,
                        progress: None,
                    };
                    let _ = tx.send(Msg::ExecFinished(Box::new(done)));
                });
                ControlFlow::Continue(())
            }

            Cmd::LoadDiff(target) => {
                let reader = self.reader.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    // 要求した対象をそのまま返す。`update` はこれと `wanted_diff()` を
                    // 比べて、届いた diff がいま選んでいるファイルのものかを見る（01）。
                    let result = Box::new(reader.diff(target.clone()).await);
                    let _ = tx.send(Msg::DiffLoaded { target, result });
                });
                ControlFlow::Continue(())
            }

            Cmd::CancelExec => {
                match self.cancel.take() {
                    Some(cancel) => {
                        // 受け側が落ちている（タスクが既に終わった）ときの `Err` は
                        // 握る。`Msg::ExecFinished` がもう飛んでいるということなので、
                        // 伝えることが無い。
                        let _ = cancel.send(());
                    }
                    // 1 回目の中断で git が終われなかった形。**黙らない** ——
                    // `Ctrl-C` を押したのに画面が変わらないのと同じに見える。
                    None => state.push_notice(
                        NoticeLevel::Info,
                        "中断を頼んでいます。git の終了を待っています".to_string(),
                        None,
                    ),
                }
                ControlFlow::Continue(())
            }

            // `ssh -G` は `auth` が起動する。ここは待ってその結果を `Msg` にするだけ
            // （01 の「`RepoInfo.target` を埋める」）。
            Cmd::ResolveTarget(url) => {
                let ssh = self.ssh.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let resolved = RepoTarget::resolve(&ssh, url).await;
                    let _ = tx.send(Msg::TargetResolved(Box::new(resolved)));
                });
                ControlFlow::Continue(())
            }

            // `gh` の起動と GitHub API は `AccountBackend` の中（04）。ここは待って
            // 結果を `Msg` にするだけ。**`GhCli::detect` もタスクの中で走らせる** ——
            // 最初のフレームの前に `gh --version` の往復を置かずに済むうえ、
            // **`Runtime` に写しを持たないので、外で `gh` を入れ直した回に古い
            // `Err(GhMissing)` を掴んだまま固まらない**（下の `ListAccounts` も
            // 同じ理由で毎回 detect する。実測 0.03〜0.04 秒で、待っている
            // `gh auth status` の 0.95 秒に比べれば小さい）。
            Cmd::ResolveAccount(target) => {
                // **材料は `AppState` から取る。** 起動時に決まってから変わらない値
                // （08。実行中は設定を読み直さない）なので `Runtime` に写しを持つことも
                // できるが、持つと出どころが 2 本になり、テストが本体と違う設定を
                // 掴んだまま緑になる。`dispatch` に禁じられているのは `notices` 以外を
                // **書く**ことで、読むことではない（01 のランタイム）。
                let repo_path = state.repo.path.clone();
                let config = state.config.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let backend = AccountBackend::detect(&config.general.gh_bin).await;
                    let result = auth::resolve(&target, &repo_path, &backend, &config).await;
                    let _ = tx.send(Msg::AccountResolved(result));
                });
                ControlFlow::Continue(())
            }

            // 選び直しの候補（01 の「候補の取り直し」）。**`auth::resolve` は通さない**
            // —— 引くのは一覧だけで、degraded の理由を作り直すのは
            // `Cmd::ResolveAccount` の仕事（畳むと `AccountState` の産出元が増える。04）。
            Cmd::ListAccounts(host) => {
                // **`detect` は上の腕と同じく毎回走らせる**（理由もあちらに書いてある）。
                let gh_bin = state.config.general.gh_bin.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let backend = AccountBackend::detect(&gh_bin).await;
                    let _ = tx.send(Msg::AccountsListed(backend.logins(&host).await));
                });
                ControlFlow::Continue(())
            }

            // M4 以降で実装する。
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
    use mewgit_core::auth::{AccountState, DegradedReason};
    use mewgit_core::git_exec::GitOp;
    use mewgit_core::git_read::HeadInfo;
    use mewgit_core::test_util::{fake_target, wait_for_pid, BareRepo, TestRepo};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// リポジトリを開いた状態とハンドル。
    fn opened(repo: &TestRepo) -> (AppState, ReaderHandle) {
        open(repo.path(), offline_config()).expect("リポジトリを開けませんでした")
    }

    /// テスト用のランタイム。`git` は隔離した環境で走らせる
    /// （実行環境の `~/.gitconfig` を持ち込まない）。
    fn runtime_for(
        repo: &TestRepo,
        reader: ReaderHandle,
    ) -> (Runtime, mpsc::UnboundedReceiver<Msg>) {
        Runtime::new(reader, repo.runner(), SshCommand::default())
    }

    /// **実物の `gh` を叩かない設定。** `general.gh_bin` を存在しないパスに向けると、
    /// `AccountBackend` は `GhMissing` を抱えたまま作られる（04）。実行環境の
    /// keyring と GitHub への往復をテストに持ち込まないため、`opened` が作る
    /// `AppState` はこれを持つ（`Cmd::ResolveAccount` は `state.config` を読む）。
    fn offline_config() -> Config {
        let mut config = Config::default();
        config.general.gh_bin = "/nonexistent/gh".to_string();
        config
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
        let (mut runtime, mut msgs) = runtime_for(&repo, reader);

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
        let (mut runtime, mut msgs) = runtime_for(&repo, reader);

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
    async fn load_status_reaches_the_state_through_the_reader() {
        let repo = TestRepo::init();
        repo.commit_file("a.txt", "first");
        repo.write_worktree_file("b.txt", b"new");
        let (mut state, reader) = opened(&repo);
        let (mut runtime, mut msgs) = runtime_for(&repo, reader);

        let cmd = state.issue(Cmd::LoadStatus);
        assert!(runtime.dispatch(&mut state, cmd).is_continue());
        let msg = msgs.recv().await.expect("Msg が返りませんでした");
        let _ = update(&mut state, msg);

        assert_eq!(state.worktree.snapshot.untracked.len(), 1);
        assert!(!state.jobs.is_running(JobKind::LoadStatus));
    }

    #[tokio::test]
    async fn load_diff_reaches_the_state_through_the_reader() {
        let repo = TestRepo::init();
        repo.commit_file("a.txt", "one\ntwo\n");
        repo.write_worktree_file("a.txt", b"one\nTWO\n");
        let (mut state, reader) = opened(&repo);
        let (mut runtime, mut msgs) = runtime_for(&repo, reader);

        // ワーキングツリーを読み、Diff ペインを開くところまでを実際の経路で通す。
        let cmd = state.issue(Cmd::LoadStatus);
        assert!(runtime.dispatch(&mut state, cmd).is_continue());
        let msg = msgs.recv().await.expect("Msg が返りませんでした");
        let _ = update(&mut state, msg);
        state.focus = Pane::Worktree;

        // `KeyCode` は crossterm 側と名前が衝突するので完全修飾で書く。
        let enter = mewgit_core::app::Key::new(mewgit_core::app::KeyCode::Enter);
        let cmd = update(&mut state, Msg::Key(enter));
        assert!(runtime.dispatch(&mut state, cmd).is_continue());
        let msg = msgs.recv().await.expect("Msg が返りませんでした");
        let _ = update(&mut state, msg);

        let view = state.diff.view.expect("diff が届いていません");
        assert_eq!(view.files.len(), 1);
        assert_eq!(view.files[0].path, std::path::Path::new("a.txt"));
        assert_eq!(view.files[0].hunks.len(), 1);
        assert!(!state.jobs.is_running(JobKind::LoadDiff));
    }

    #[tokio::test]
    async fn exec_runs_git_and_the_reload_shows_the_result() {
        // `Cmd::Exec` から `git` を回して、そのあとの `LoadStatus` までを通す。
        let repo = TestRepo::init();
        repo.commit("first");
        repo.write_worktree_file("b.txt", b"new");
        let (mut state, reader) = opened(&repo);
        let (mut runtime, mut msgs) = runtime_for(&repo, reader);

        let cmd = state.issue(Cmd::Exec(GitOp::Stage(vec!["b.txt".into()])));
        assert!(runtime.dispatch(&mut state, cmd).is_continue());
        let msg = msgs.recv().await.expect("Msg が返りませんでした");

        // 実行が終わったら status を読み直す。
        let cmd = update(&mut state, msg);
        assert_eq!(cmd, Cmd::LoadStatus);
        assert!(runtime.dispatch(&mut state, cmd).is_continue());
        let msg = msgs.recv().await.expect("Msg が返りませんでした");
        let _ = update(&mut state, msg);

        assert_eq!(state.worktree.snapshot.staged.len(), 1);
        assert!(state.worktree.snapshot.untracked.is_empty());
        assert!(state.notices.is_empty(), "{:?}", state.notices);
    }

    #[tokio::test]
    async fn canceling_lets_the_next_write_through() {
        // **中断したタスク自身が `Msg::ExecFinished` を返す。** `abort` で落とすと
        // 誰も `JobKind::Exec` を畳まず、以後の書き込みが全部
        // 「ほかの git 実行が終わるまで待ってください」で落ちる。
        let repo = TestRepo::init();
        repo.commit("first");
        let (op, pid_file) = repo.slow_stage();
        let (mut state, reader) = opened(&repo);
        let (mut runtime, mut msgs) = runtime_for(&repo, reader);

        let cmd = state.issue(Cmd::Exec(op));
        assert!(runtime.dispatch(&mut state, cmd).is_continue());
        wait_for_pid(&pid_file).await;

        // `Ctrl-C` が出すのはこれ（`update` 側で見張っている）。
        assert!(runtime.dispatch(&mut state, Cmd::CancelExec).is_continue());
        let msg = msgs.recv().await.expect("Msg が返りませんでした");
        let _ = update(&mut state, msg);

        assert!(!state.jobs.is_running(JobKind::Exec));
        // **「中断で終わった」ことまで見る。** 合図が届いていないと、フィルタが
        // 眠り終えた 30 秒後に成功が返ってきて、ジョブだけ見る見張りは緑になる。
        let notice = state.notices.back().expect("通知がありません");
        assert!(notice.message.contains("中断"), "{notice:?}");
        assert!(!repo.index_lock().exists(), "index.lock が残っています");
        // 次の書き込みが通る（落とされない）。
        let cmd = state.issue(Cmd::Exec(GitOp::Stage(vec!["slow.txt".into()])));
        assert!(matches!(cmd, Cmd::Exec(_)), "{cmd:?}");
    }

    #[tokio::test]
    async fn a_failed_exec_reaches_the_notices_with_its_argv_and_stderr() {
        // Git CLI のエラーは握りつぶさない（不変ルール 4）。
        let repo = TestRepo::init();
        repo.commit("first");
        let (mut state, reader) = opened(&repo);
        let (mut runtime, mut msgs) = runtime_for(&repo, reader);

        let cmd = state.issue(Cmd::Exec(GitOp::Stage(vec!["missing.txt".into()])));
        assert!(runtime.dispatch(&mut state, cmd).is_continue());
        let msg = msgs.recv().await.expect("Msg が返りませんでした");
        let _ = update(&mut state, msg);

        let notice = state.notices.back().expect("通知がありません");
        assert_eq!(notice.level, NoticeLevel::Error);
        let detail = notice.detail.as_deref().expect("detail がありません");
        assert!(detail.contains("git add -- missing.txt"), "{detail}");
        assert!(detail.contains("pathspec"), "{detail}");
        assert!(!state.jobs.is_running(JobKind::Exec));
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
        let (mut runtime, _msgs) = runtime_for(&repo, reader);
        assert!(runtime.dispatch(&mut state, Cmd::Quit).is_break());
    }

    #[tokio::test]
    async fn none_continues() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo);
        let (mut runtime, _msgs) = runtime_for(&repo, reader);
        assert!(runtime.dispatch(&mut state, Cmd::None).is_continue());
        assert!(state.notices.is_empty());
    }

    #[tokio::test]
    async fn batch_stops_at_quit() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo);
        let (mut runtime, _msgs) = runtime_for(&repo, reader);

        // Quit の後ろは実行しない。未実装 notice が 1 件だけであることで確かめる。
        let batch = Cmd::Batch(vec![Cmd::FetchPr, Cmd::Quit, Cmd::FetchChecks]);
        assert!(runtime.dispatch(&mut state, batch).is_break());
        assert_eq!(state.notices.len(), 1);
    }

    #[tokio::test]
    async fn unimplemented_cmd_warns_instead_of_disappearing() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo);
        let (mut runtime, _msgs) = runtime_for(&repo, reader);

        assert!(runtime.dispatch(&mut state, Cmd::FetchPr).is_continue());

        assert_eq!(state.notices.len(), 1);
        let notice = &state.notices[0];
        assert_eq!(notice.level, NoticeLevel::Warn);
        assert!(notice.message.contains("FetchPr"));
    }

    #[test]
    fn an_unimplemented_cmd_does_not_hold_on_to_its_job() {
        // 掴んだままだと、2 回目以降の同じ `Cmd` が `issue` に落とされて
        // 通知も出ない（「1 回だけ警告が出てあとは無反応」になる）。
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo);
        let (mut runtime, _msgs) = runtime_for(&repo, reader);

        let cmd = state.issue(Cmd::FetchPr);
        assert!(runtime.dispatch(&mut state, cmd).is_continue());
        assert!(!state.jobs.is_running(JobKind::FetchPr));

        let again = state.issue(Cmd::FetchPr);
        assert!(runtime.dispatch(&mut state, again).is_continue());
        assert_eq!(state.notices.len(), 2);
    }

    /// `Cmd::ResolveAccount` が `AccountBackend` を通って `Msg` で返る（04）。
    ///
    /// **`gh` は存在しないパスに向けてある**ので、答えは `GhMissing` に決まる。
    /// ここで見張るのは配線 —— 手順 1〜4 の分岐は `auth::resolve` のテストが
    /// `Backend` の向こう側を組んで踏んでいる（実 `gh` では踏めない。04）。
    #[tokio::test]
    async fn resolve_account_comes_back_through_the_backend() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo);
        let (mut runtime, mut msgs) = runtime_for(&repo, reader);

        let cmd = state.issue(Cmd::ResolveAccount(fake_target()));
        assert!(runtime.dispatch(&mut state, cmd).is_continue());
        let msg = msgs.recv().await.expect("Msg が返りませんでした");
        let _ = update(&mut state, msg);

        assert!(
            matches!(
                state.account,
                AccountState::Degraded(DegradedReason::GhMissing)
            ),
            "{:?}",
            state.account
        );
        // 畳んでいないと、2 回目の `Cmd::ResolveAccount` が `issue` に落とされる。
        assert!(!state.jobs.is_running(JobKind::ResolveAccount));
    }

    #[tokio::test]
    async fn nested_batch_is_dispatched() {
        let repo = TestRepo::init();
        let (mut state, reader) = opened(&repo);
        let (mut runtime, _msgs) = runtime_for(&repo, reader);

        let nested = Cmd::Batch(vec![Cmd::Batch(vec![Cmd::FetchPr]), Cmd::FetchChecks]);
        assert!(runtime.dispatch(&mut state, nested).is_continue());
        assert_eq!(state.notices.len(), 2);
    }

    #[tokio::test]
    async fn load_commit_detail_reaches_the_state_through_the_reader() {
        let repo = TestRepo::init();
        repo.commit("subject");
        repo.git(&["commit", "--amend", "-q", "-m", "subject\n\nbody"]);
        let oid = repo.head_oid();
        let (mut state, reader) = opened(&repo);
        let (mut runtime, mut msgs) = runtime_for(&repo, reader);

        // 行が無いとカーソルの指すコミットが決まらず、届いた詳細は捨てられる。
        // 先に 1 ページ積む。
        let cmd = state.issue(Cmd::LoadLog {
            count: 10,
            restart: false,
        });
        assert!(runtime.dispatch(&mut state, cmd).is_continue());
        let msg = msgs.recv().await.expect("Msg が返りませんでした");
        let _ = update(&mut state, msg);
        // 表示行 0 は WIP 行。1 件目のコミットはその 1 つ下。
        state.graph.cursor = 1;

        let cmd = state.issue(Cmd::LoadCommitDetail(oid));
        assert!(runtime.dispatch(&mut state, cmd).is_continue());
        let msg = msgs.recv().await.expect("Msg が返りませんでした");
        let _ = update(&mut state, msg);

        let detail = state.graph.detail.as_ref().expect("詳細が入っていません");
        assert_eq!(detail.oid, oid);
        assert_eq!(detail.message, "subject\n\nbody");
        assert!(!state.jobs.is_running(JobKind::LoadCommitDetail));
    }

    #[tokio::test]
    async fn enter_reaches_the_state_and_the_viewport_follows() {
        // `Enter` を打ってループを 1 本走らせる。カーソルは WIP 行にあるので、
        // `Enter` は右ペイン（Working tree）へ移る。`viewport` は常に `ui` が出す値と
        // 一致していること（一致しないとカーソルが画面の外に出る）。
        let repo = TestRepo::init();
        repo.commit("first");
        let (mut state, reader) = opened(&repo);
        let events = vec![key(KeyCode::Enter), key(KeyCode::Char('q'))];

        run_with(events, &mut state, reader).await.unwrap();

        assert_eq!(state.focus, Pane::Worktree);
        assert_eq!(state.graph.viewport, crate::test_util::graph_rows(&state));
    }

    #[test]
    fn the_viewport_does_not_move_when_the_selection_changes() {
        // コミット詳細は右ペインに出るので、選択が変わってもグラフの高さは動かない
        // （07 の「CommitDetail ペイン」）。動くと `Msg::Layout` が毎回流れ、そのたびに
        // 追加読み込みの判定まで走る。
        let mut state = crate::test_util::state_with_commits(&["first"]);
        let on_wip = state.graph.viewport;

        state.graph.cursor = 1;

        assert_eq!(layout(&mut state), Cmd::None);
        assert_eq!(state.graph.viewport, on_wip);
    }

    #[tokio::test]
    async fn the_viewport_is_filled_in_before_the_first_frame() {
        // `Msg::Resize` は端末サイズしか運ばない。行数を入れるのはループ先頭の
        // `Msg::Layout` で、そこが抜けると `viewport` が 0 のままスクロールが壊れる。
        let repo = TestRepo::init();
        repo.commit("first");
        let (mut state, reader) = opened(&repo);

        run_with(vec![key(KeyCode::Char('q'))], &mut state, reader)
            .await
            .unwrap();

        assert_eq!(state.graph.viewport, crate::test_util::graph_rows(&state));
        assert!(state.graph.viewport > 0);
    }

    #[tokio::test]
    async fn the_graph_pane_has_focus_at_startup() {
        let repo = TestRepo::init();
        let (state, _reader) = opened(&repo);
        assert_eq!(state.focus, Pane::Graph);
    }
}
