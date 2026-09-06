# 実装計画

各タスクは Claude Code に 1 回で渡せる粒度を目安にしている。完了したら `[x]` にする。

## M0: 土台

- [x] workspace がビルドできる状態にする（依存追加、`lib.rs` のモジュール宣言、空の `main.rs`）
- [x] `mewgit-core::config` — `Config::load()`、既定値、`[[repos]]` の glob マッチ、テスト
- [x] `mewgit-core::app` — `AppState` / `Msg` / `Cmd` の型定義と、何もしない `update`
- [x] `mewgit-tui` — ターミナル初期化・復元、イベントループ、`q` で終了、リサイズ対応
- [x] CI（GitHub Actions）: fmt / clippy / test を Linux + macOS で

## M1: 読み取り専用でグラフを出す

- [x] `git_read::RepoReader::open` / `remotes` / `refs` / `head`
- [x] `git_read` のログ列挙（`LogSession`、ページング）+ tempfile リポジトリでのテスト
- [x] `graph::lane` — レーン割り当てアルゴリズム + 固定ケースのテスト（直線 / 分岐 / マージ / オクトパス / 複数ルート）
- [x] `ui::panes::graph` — `GraphRow` の描画、罫線 / ASCII フォールバック、ラベル、色
- [ ] コミット詳細の遅延取得と展開表示
- [ ] スクロール追随のページ追加ロード

## M2: ワーキングツリーとブランチ

- [ ] `git_read::status` / `diff`（ハンク分割）
- [ ] `git_exec::Runner` と `to_argv` の全 `GitOp` テスト、`ExecErrorKind` の分類テスト
- [ ] `ui::panes::worktree` / `diff` — ファイル・ハンク単位の stage / unstage / discard
- [ ] `Modal::CommitMessage` と commit / amend
- [ ] `ui::panes::branches` — 一覧、作成 / 切替 / 削除 / リネーム、`Modal::Confirm`

## M3: リモート

- [ ] `auth::ssh::resolve_ssh_host`（`ssh -G`）と `RemoteUrl` パーサ + テスト
- [ ] fetch / pull / push の `GitOp`、`-u` 提案、force push 確認
- [ ] `SamlSsoRequired` / `SshAuthFailed` / `NonFastForward` の検出と案内文
- [ ] 実行中ジョブのスピナーとキャンセル

## M4: GitHub 統合

- [ ] `auth::gh::GhCli` — detect / auth_status（JSON とテキスト両対応）/ token
- [ ] `github::GithubClient` — `repo` / `pr_for_branch` / `checks` / `reviews` + wiremock テスト
- [ ] `auth::resolve` — 明示設定 → 単一 → probe → Ambiguous の流れ + テスト
- [ ] `Modal::AccountPicker` と `.git/config` への保存
- [ ] ヘッダの `[acct: login @ host]` 表示と Degraded 表示
- [ ] `ui::panes::prs` — 現ブランチ PR、チェック一覧、一覧タブ、ブラウザで開く
- [ ] `Modal::PrCreate` と `create_pr`
- [ ] PR チェックアウト（同一リポジトリ / fork）
- [ ] 403 の `needed_scopes` 案内

## M5: 履歴操作

- [ ] `git_read::in_progress` の検出と `ProgressDetected`、ステータスバー表示
- [ ] `--sequence-editor` / `--commit-editor` 隠しサブコマンド
- [ ] `RebasePlan::to_todo` + テスト、`Modal::RebaseEditor`
- [ ] rebase の continue / skip / abort
- [ ] cherry-pick（単一 / 複数）と continue / abort
- [ ] revert（mainline 選択）と continue / abort
- [ ] `ui::panes::stash` — 一覧 / diff / push / apply / pop / drop、部分 stash
- [ ] 進行中操作があるときの書き込み `GitOp` 拒否

## M6: 仕上げ

- [ ] `Modal::Help` とキーマップ上書き
- [ ] 端末幅 < 100 のレイアウト
- [ ] `.git/HEAD` mtime 監視での外部変更検知
- [ ] README、リリースワークフロー（`cargo dist` or release-please）
