# mewgit

TUI 版 GitKraken。Rust + ratatui。認証はローカルの `~/.ssh` と `gh` CLI に完全委譲する。

## まず読むもの

- `docs/requirements.md` — 何を作るか
- `docs/design/00-overview.md` — レイヤー構成とデータフロー。**変更前に必ず読む**
- `docs/design/0N-*.md` — 触るコンポーネントの設計
- `docs/plan.md` — マイルストーンとタスク。着手中のタスクはここで確認する

設計と実装がずれる判断をしたときは、コードではなく設計書を先に直してから実装する。

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
- GitHub Actions の `uses` はコミット SHA で固定し、行末コメントにタグを書く（Renovate が更新する）
- ワークフローを触ったら `mise x ghalint -- ghalint run` と `mise x actionlint -- actionlint` を通す

## コマンド

```
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo run -p mewgit -- <repo-path>
```

CI 相当のチェック: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`

## コーディング規約

- Rust 2021、`rustfmt` 既定設定。`clippy` の警告ゼロを維持
- エラーは各モジュールで `thiserror`、`app` 層で `AppError` に集約。`anyhow` は `main.rs` とテストのみ
- 非同期は tokio。git2 呼び出しは `spawn_blocking` またはリーダースレッド経由
- 公開 API にはドキュメントコメント。内部は必要なところだけ
- 一つの PR / コミットは一つの関心事。設計書の変更とコード変更は同じコミットで

## テスト

- `mewgit-core` の git 系は `tempfile` に実リポジトリを作って統合テスト（`tests/` に helper あり、なければ作る）
- `graph` は固定のコミット列に対する `cells` の期待値テストを必ず追加する
- GitHub API は `wiremock` でモック。実 API は叩かない
- `git_exec::to_argv` は純粋関数なのでユニットテストで全 `GitOp` をカバーする

## Git 運用

- ブランチ: `feat/<topic>`、`fix/<topic>`。main へは PR 経由
- コミットメッセージ: Conventional Commits（`feat(graph): ...`、`fix(git_exec): ...`）。scope はモジュール名
- `docs/plan.md` のタスクを完了したらチェックを付けて同じコミットに含める

## やらないこと

- Windows 対応（MVP 外）
- コンフリクト解消ビュー（MVP 外。continue / abort のみ）
- GraphQL API（REST で足りる）
- 独自のトークン取得フロー（`gh` に任せる）
