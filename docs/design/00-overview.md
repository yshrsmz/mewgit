# 00. 全体アーキテクチャ

## レイヤー構成

```
┌──────────────────────────────────────────────┐
│ ui        ratatui ウィジェット・レイアウト・キー処理 │
├──────────────────────────────────────────────┤
│ app       状態（AppState）・Msg・update ループ      │
├────────────┬────────────┬────────────┬───────┤
│ graph      │ git_read   │ git_exec   │ github│
│ レーン計算 │ git2 読取  │ CLI 実行   │ API   │
├────────────┴────────────┴─────┬──────┴───────┤
│ auth  gh 連携 / アカウント解決 / SSH 正規化   │
├───────────────────────────────┴──────────────┤
│ config  設定ファイル・キーバインド定義          │
└──────────────────────────────────────────────┘
```

依存の向きは上から下のみ。`ui` は `app` の状態を描画するだけで、Git や API を直接呼ばない。

## クレート分割（Cargo workspace）

| クレート | 責務 | 主な依存 |
|---|---|---|
| `mewgit-core` | app / graph / git_read / git_exec / github / auth / config | git2, tokio, reqwest, serde |
| `mewgit-tui` | ui、バイナリのエントリポイント | ratatui, crossterm, mewgit-core |

`core` を UI 非依存にしておくことで、グラフ計算やアカウント解決を単体テストしやすくする。

## データフロー（Elm アーキテクチャ）

```
入力イベント ──▶ Msg ──▶ update(&mut AppState, Msg) ──▶ Cmd
                                    ▲                   │
                                    │                   ▼
                              Msg ◀── 非同期タスク（tokio）
```

- `update` は同期・純粋。副作用は `Cmd` として返し、ランタイムが tokio タスクに変換する
- 非同期タスク完了時は `Msg` を mpsc チャネルで戻す
- 描画は毎フレーム `AppState` から生成する（差分管理しない）

## ディレクトリ構成

```
crates/
  core/src/
    app/        state.rs  msg.rs  update.rs  cmd.rs
    graph/      lane.rs   render.rs
    git_read/   repo.rs   log.rs  diff.rs  status.rs
    git_exec/   runner.rs rebase.rs progress.rs
    github/     client.rs pr.rs   checks.rs
    auth/       gh.rs     resolve.rs ssh.rs
    config/     mod.rs    keymap.rs
  tui/src/
    main.rs   ui/  panes/  modals/  keymap.rs
```

## 横断的関心事

- エラー: `thiserror` で層ごとの Error を定義し、`app` で `AppError` に集約。UI にはメッセージ + 実行コマンド + stderr を渡す
- ログ: `tracing` + ファイル出力（`~/.local/state/mewgit/log`）。トークンはログに出さない
- テスト: `core` は tempdir に実リポジトリを作って統合テスト。API は `wiremock` でモック

各コンポーネントの詳細は 01〜08 を参照。
