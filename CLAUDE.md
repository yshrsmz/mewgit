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
- 依存を新規追加するときは crates.io の公開日を確認し、14 日以上経過した版を採用する。既存依存の更新は Renovate（`yshrsmz/renovate-config` の `minimumReleaseAge: "2 weeks"`）が担保する

## コマンド

```
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo run -p mewgit -- <repo-path>
```

CI 相当のチェック: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`

TUI を非対話で動かして確認するには pty が要る。パイプ渡しだと `enable_raw_mode` が
`Device not configured (os error 6)` で落ちる。`script` は pty のサイズを設定しないので、
`stty` で与えないと 0x0 になって何も描画されない。

```
(sleep 1; printf 'q'; sleep 1) | script -q /dev/null \
  sh -c 'stty rows 24 cols 80; exec ./target/debug/mewgit <repo-path>' > out.txt 2>&1
```

## コーディング規約

- Rust 2021、`rustfmt` 既定設定。`clippy` の警告ゼロを維持
- エラーは各モジュールで `thiserror`、`app` 層で `AppError` に集約。`anyhow` は `main.rs` とテストのみ
- 非同期は tokio。git2 呼び出しは `spawn_blocking` またはリーダースレッド経由
- crossterm は `use ratatui::crossterm::` 経由で使う。直接 `use crossterm::` にすると、
  Renovate が片方だけ上げたときに `Event` / `KeyEvent` が別クレートの型に割れる。
  `Cargo.toml` の直接依存は `event-stream` feature のために残す
- 公開 API にはドキュメントコメント。内部は必要なところだけ
- 一つの PR / コミットは一つの関心事。設計書の変更とコード変更は同じコミットで
- **`AppState` にコールバック（関数ポインタ・クロージャ）を持たせない。** 確定後に何を
  するかは `Cmd` かデータの enum で表す。関数ポインタは環境をキャプチャできず文脈を
  運べないうえ、状態が `PartialEq` を失って遷移をテストできなくなる

## テスト

**テストなしで機能を追加しない。** 実装と同じコミットにテストを含める。`docs/plan.md` のタスクにチェックを付ける条件は、そのタスクの振る舞いを覆うテストが通っていること。

- テストが書きにくい設計になっていたら、テストを省くのではなく設計を直す（`docs/design/` を先に更新する）
- `mewgit-core` の git 系は `tempfile` に実リポジトリを作って統合テスト。helper は
  `crates/mewgit-core/tests/common/mod.rs` の `TestRepo`。リポジトリは `git` CLI で作り、
  `GIT_CONFIG_NOSYSTEM` / `GIT_CONFIG_GLOBAL` / `HOME` で実行環境の設定を持ち込まない
- `Path` 同士の `assert_eq!` は末尾の区切りを無視するので、パスの正規化はテストできない。
  git2 の `workdir()` / `path()` は `/…/repo/` と末尾を付けて返すため、正規化を確かめる
  ときは `to_string_lossy()` で文字列に直して見る
- `graph` は固定のコミット列に対する `cells` の期待値テストを必ず追加する
- GitHub API は `wiremock` でモック。実 API は叩かない
- `git_exec::to_argv` は純粋関数なのでユニットテストで全 `GitOp` をカバーする
- `mewgit-tui` の描画は ratatui の `TestBackend` にレンダリングしてバッファを検証する。
  取り出しは `terminal.backend().to_string()` を使う。セルを自前で走査すると全角文字の
  2 セル目が空白として混ざる（「新しい通知」→「新 し い 通 知」）
- 型だけを定義するタスクでも、その型で実際の操作を 1 つ組み立てるテストを書く。
  組み立てられないなら型が間違っている（「この確認モーダルに入れる値を作れるか」など）
- テストを書かない例外は、端末の raw mode 切替・復元と `main.rs` の配線だけ。実端末が必要で単体テストできないため

## Git 運用

- ブランチ: `feat/<topic>`、`fix/<topic>`。`github:git-workflow` スキルは `<type>-<subject>` を指示するが、こちらを優先する。main へは PR 経由
- コミットメッセージ: Conventional Commits（`feat(graph): ...`、`fix(git_exec): ...`）。scope はモジュール名
- `docs/plan.md` のタスクを完了したらチェックを付けて同じコミットに含める

### PR を作る前に

`gh pr create` の前に、CI 相当のチェックに加えて必ずこの順で実行する。

1. `/simplify` — 重複・冗長・不要な抽象を削る。指摘の適用まで含む
2. `/code-review medium` — バグを探す。指摘があれば直してから次へ
3. 修正が出たら CI 相当のチェックを流し直し、別コミットとして積む（amend しない）

どちらも指摘ゼロなら何もコミットせずに PR へ進む。レビュー指摘に「直さない」判断をした
場合は、その理由を PR 本文に書く。

どちらもバックグラウンドのエージェントとして走り、完了はハーネスが通知する。待機のために
no-op のコマンドを回さない（`echo waiting` の空回しなど）。ターンを終えて通知を待つ。

## やらないこと

- Windows 対応（MVP 外）
- コンフリクト解消ビュー（MVP 外。continue / abort のみ）
- GraphQL API（REST で足りる）
- 独自のトークン取得フロー（`gh` に任せる）
