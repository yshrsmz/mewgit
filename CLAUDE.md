# mewgit

TUI 版 GitKraken。Rust + ratatui。認証はローカルの `~/.ssh` と `gh` CLI に完全委譲する。

## まず読むもの

- `docs/requirements.md` — 何を作るか
- `docs/design/00-overview.md` — レイヤー構成とデータフロー。**変更前に必ず読む**
- `docs/design/0N-*.md` — 触るコンポーネントの設計
- `docs/plan.md` — マイルストーンとタスク。着手中のタスクはここで確認する

設計と実装がずれる判断をしたときは、コードではなく設計書を先に直してから実装する。
設計書を 1 本直したら、同じ話題に触れている他の設計書も同じコミットで揃える。
`0N-*.md` は互いを参照しているので、片方だけ直すと設計書同士が矛盾したまま残る。

**設計書を書く・直すときの教訓は `.claude/rules/design-docs.md`**（`docs/` を読むと読み込まれる）。

**この `CLAUDE.md` は入口だけ。** 規約は `.claude/rules/`（path 付きで、触るファイルに応じて
読み込まれる）、手順は `.claude/skills/`。**教訓や過去の判断を探すときは、grep の対象に
`.claude/` も含める**（`docs/` だけを探すと、規約に移した教訓が見つからない）。

## 不変のルール

1. **読み取りは git2、書き込みは `git` CLI**。git2 で commit / checkout / rebase / stash を実行しない（hook が走らないため）。
2. **`ui` は `AppState` を描画するだけ**。`ui` から git2 / CLI / HTTP を直接呼ばない。副作用は `Cmd` で返す。
3. **トークンはメモリのみ**。`SecretString` で保持し、`Debug` 出力・ログ・エラーメッセージに含めない。
4. **Git CLI のエラーは握りつぶさない**。実行 argv と stderr を `ExecError` に入れて UI まで届ける。
5. **破壊的操作は `Modal::Confirm` を通す**（force push、`branch -D`、discard、abort 系）。
6. **`mewgit-core` は UI 非依存**。ratatui / crossterm への依存を core に入れない。
7. 外部プロセスは `git` `gh` `ssh` のみ。それ以外のバイナリに依存しない。

## 開発環境

Rust の toolchain は `rust-toolchain.toml` が SoT。rustup がこれを読んで自動で正しい版に切り替えるので、`rustup` が入っていれば追加のセットアップは不要。

- **mise で rust を管理しない。** mise の `rust` は rustup のラッパーで `RUSTUP_TOOLCHAIN` を環境変数として注入するため、`rust-toolchain.toml` が読まれなくなる。SoT が二重化するので使わない
- 依存バージョンは `Cargo.lock` をコミットして固定する。`Cargo.toml` はキャレット範囲のままでよく、`=` 固定はしない。CI は `--locked` を付けて lock からの逸脱を落とす
- 依存を追加・削除したら、`--locked` 付きのコマンドを叩く前に `cargo update --offline --workspace` で lock を更新する。`--locked` は lock の書き換えを拒否するので、先に流さないと `cannot update the lock file` で落ちる
- GitHub Actions の `uses` はコミット SHA で固定し、行末コメントにタグを書く（Renovate が更新する）
- ワークフローを触ったら `mise x ghalint -- ghalint run` と `mise x actionlint -- actionlint` を通す
- **依存の追加・更新の規則は `.claude/rules/dependencies.md`**（`Cargo.toml` / `Cargo.lock` を読むと読み込まれる）。**Renovate の PR を処理するときは `renovate-pr` スキル**

## コマンド

```
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo run -p mewgit -- <repo-path>
```

`crates/mewgit-tui` のパッケージ名は `mewgit`（`-p mewgit-tui` は `did not match any packages` で落ちる）。core は `-p mewgit-core`。

CI 相当のチェック: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`

**TUI を pty で動かして画面を確かめるときは `pty-check` スキル**（報告の前に実物を動かす。`.claude/rules/reporting.md`）。

## コーディング規約

- Rust 2021、`rustfmt` 既定設定。`clippy` の警告ゼロを維持
- エラーは各モジュールで `thiserror`、`app` 層で `AppError` に集約。`anyhow` は `main.rs` とテストのみ
- 非同期は tokio。git2 呼び出しは `spawn_blocking` またはリーダースレッド経由
- crossterm は `use ratatui::crossterm::` 経由で使う。直接 `use crossterm::` にすると、
  Renovate が片方だけ上げたときに `Event` / `KeyEvent` が別クレートの型に割れる。
  `Cargo.toml` の直接依存は `event-stream` feature のために残す
- 公開 API にはドキュメントコメント。内部は必要なところだけ
- 一つの PR / コミットは一つの関心事。設計書の変更とコード変更は同じコミットで

**Rust を書くときの規約（踏んだ形と PR 番号）は `.claude/rules/rust-coding.md`**（`crates/**/*.rs` を読むと読み込まれる）。
**`app` 層の不変条件（`Cmd` / `Msg` / ジョブの扱いで何度も破ったもの）は `.claude/rules/app-invariants.md`**（`app/` と `runtime.rs` を読むと読み込まれる）。

## テスト

**テストなしで機能を追加しない。** 実装と同じコミットにテストを含める。`docs/plan.md` のタスクにチェックを付ける条件は、そのタスクの振る舞いを覆うテストが通っていること。

- テストが書きにくい設計になっていたら、テストを省くのではなく設計を直す（`docs/design/` を先に更新する）

**テストの規約は `.claude/rules/testing.md`**（`crates/**/*.rs` を読むと読み込まれる）。

## Git 運用

- ブランチ: `feat/<topic>`、`fix/<topic>`。`github:git-workflow` スキルは `<type>-<subject>` を指示するが、こちらを優先する。main へは PR 経由。
  refactor / test / docs の変更も `feat/` か `fix/` に載せる —— commit の type とブランチの接頭辞は別（#181 で `refactor/` と付けて改名した）
- コミットメッセージ: Conventional Commits（`feat(graph): ...`、`fix(git_exec): ...`）。scope はモジュール名
- `docs/plan.md` のタスクを完了したらチェックを付けて同じコミットに含める
- **リリースは release-please の release PR をマージして切る。** タグを手で打たない、
  `Cargo.toml` の版を手で上げない（release PR が `Cargo.toml` と `Cargo.lock` を書き換える）。
  CHANGELOG はコミットメッセージから作られるので、Conventional Commits の type がそのまま節になる
- **1 つのタスクを複数の PR に割ると決めたら、その割り方を `docs/plan.md` に反映し、
  なぜそう割るかを設計書に書く。** 会話の中だけで決めると、次のセッションは plan.md の
  元の粒度を読んで再び 1 本にまとめる。割った理由が設計書にある制約から来ているなら
  （「この 2 つは同じタスクでないと片方が宙に浮く」など）、そこにも書いて突き合わせる
- **マージは `gh pr merge --squash --delete-branch`。** ローカルもリモートもブランチが
  残らない（追跡 ref は `git fetch --prune`）。取り残したマージ済みブランチの掃除は
  `github:branch-cleanup` スキルに任せる —— squash merge なので `git branch -d` も
  `--merged` も判定できず、マージ済み PR の head SHA と照合するしかない。自前で
  組み直さない。ユーザーに頼まれたときだけ走らせる

### PR を作る前に

**`gh pr create` の前に必ず `pre-pr` スキルを読んで従う**（`/simplify` → `/code-review medium`、指摘の裁き方、確認なしで push と PR まで）。

## やらないこと

- Windows 対応（MVP 外）
- コンフリクト解消ビュー（MVP 外。continue / abort のみ）
- GraphQL API（REST で足りる）
- 独自のトークン取得フロー（`gh` に任せる）
