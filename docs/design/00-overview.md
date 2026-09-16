# 00. 全体アーキテクチャ

## レイヤー構成

```
┌──────────────────────────────────────────────┐
│ ui        ratatui ウィジェット・レイアウト・入力の変換 │
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

同じ段にある `graph` / `git_read` / `git_exec` / `github` の間では、**`git_read` の値型
（`Oid`、`InProgressOp`、`Hunk` など）だけは他の 3 つから参照してよい**。git の概念を
表す語彙を 4 回定義し直さないため。逆向き（`git_read` が他の 3 つを使う）と、
`graph` / `git_exec` / `github` の相互参照はしない。進行中操作の**検出**は `git_read`
の責務で、`git_exec` は実行後にそれを呼んで結果に添える（02・03 参照）。

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
  mewgit-core/src/
    app/        state.rs  msg.rs  cmd.rs  key.rs
      update/   mod.rs  graph.rs  worktree.rs  diff.rs  branches.rs
                remote.rs  account.rs  modal.rs  exec.rs
                test_support.rs（`#[cfg(test)]`）
    graph/      lane.rs
    git_read/   repo.rs   log.rs  diff.rs  status.rs  thread.rs
    git_exec/   argv.rs   runner.rs patch.rs  〔rebase.rs〕
    github/     client.rs wire.rs  account.rs
    auth/       gh.rs     resolve.rs probe.rs  ssh.rs  url.rs
    config/     mod.rs
  mewgit-tui/src/
    main.rs   runtime.rs  input.rs
    ui/       mod.rs    lanes.rs  theme.rs  time.rs
    panes/    graph.rs  worktree.rs  diff.rs  commit.rs  branches.rs  prs.rs
    modals/   confirm.rs  input.rs  commit_message.rs  picker.rs
              remote_picker.rs  account_picker.rs
```

**〔〕はまだ無いファイル**（`rebase.rs` は M5）。**印を付けるのは、実在する行と
混ざると木全体が信用できなくなるため** —— 「この構成に合わせて置く」の意味と
「ここに在る」の意味が同じ字で書いてあると、読む側は確かめるまでどちらか判らない。

**`github/` に `pr.rs` / `checks.rs` を置く予定は取り下げた。** PR とチェックの
取得は `client.rs`（リクエストの組み立て）と `wire.rs`（応答の型）に既に入って
いて（#59）、**割る理由になる関心事が増えていない**。`ui::panes::prs` で増えるのは
描く側で、そちらは `mewgit-tui/src/panes/prs.rs`。

**`update` を割ってあるのは関心事の数のほうで、行数ではない**（Rust に行数の目安は
無く、`clippy::too_many_lines` は `allow` 既定の**関数**単位）。1 ファイル = 1 つの
privacy 境界なので、割ると「どれが本当に他から呼ばれているか」が `pub(super)` として
可視になる —— 平坦なままだと全部が private で、共有の意図が読めない。
**`pub(super)` は、他のモジュールが実際に名指ししているものだけに付ける。**
例外は 1 つだけで、`remote::NoRemote` は `pub(super)` な関数の返り値型なので狭めると
`private_interfaces` が `-D warnings` で落ちる（**数はここに書かない**。数えるなら
`test_support.rs` を除く —— あれは `#[cfg(test)]` の共有土台で、下の段落が別扱いに
している）。**名指ししているのが他モジュールの
テストだけ、というものもある**（`worktree::COMMIT_DIFF` と `NOT_LATEST`）——
Diff ペインのキーが `worktree::act` を通る以上、断りの文言は worktree が持ち主で、
見張りは Diff 側に要る。**`mod.rs` の項目には付けない** ——
あそこの `super` は `app` なので、`pub(super)` は「`app` から呼べる」という別のことを
言ってしまう（子モジュールは private のままでも見える）。
`state.rs` を割っていないのは、**`update.rs` を割った時点で workspace 最長になった
（2269 行）あとも**中身が `AppState` とその部品という 1 つの関心事だから（`docs/plan.md` の M6 に、割る
引き金つきで送ってある）。

**テストは対象と同じモジュールに置く。** private を叩くテストが多く、1 本に残すと
そのために `pub(super)` を増やすことになる。2 つ以上のモジュールが呼ぶヘルパだけ
`update/test_support.rs` に寄せる（`crate::test_util` とは別。あちらは `test-util`
feature で外のクレートにも配る土台）。

## 横断的関心事

- エラー: `thiserror` で層ごとの Error を定義し、`app` で `AppError` に集約。UI にはメッセージ + 実行コマンド + stderr を渡す
- ログ: `tracing` + ファイル出力（`~/.local/state/mewgit/log`）。トークンはログに出さない
- テスト: `core` は tempdir に実リポジトリを作って統合テスト。API は `wiremock` でモック。`tui` は ratatui の `TestBackend` にレンダリングしてバッファを検証する

各コンポーネントの詳細は 01〜08 を参照。
