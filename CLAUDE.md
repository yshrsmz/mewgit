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

**同じ判断の理由を設計書・doc コメント・テストコメントの 3 箇所に書いたなら、直すときの
grep 対象にテストコメントも入れる。** #42 では、レビューを受けて 07 の理由を書き換えた
のに、同じ理由を書いたテストコメントだけが古いまま残った。テストコメントは設計書の語で
検索しても出にくい。

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
python3 scripts/screen.py out.txt 80 24
```

**`out.txt` はそのままでは読めない。** TUI は毎フレーム全画面を描き直すので、録画には
フレームが何枚も連なっている。エスケープを `sed` で落とすだけだと全部が 1 行に潰れ、
前のフレームの残骸と最新の描画が混ざる。`scripts/screen.py` がカーソル移動と消去を
解釈して仮想画面に描き直し、終了直前の 1 枚を出す。幅・高さは `stty` に与えた値と
同じにする。

キーは `printf` で送る。raw mode では Enter は `\r`（`\n` ではない）。**修飾付きも
同じように送れる** — `Alt+Enter` は `\033\r`（ESC CR）、`Ctrl-J` は `\012`。貼り付け
（bracketed paste）は本文を `printf '\033[200~<本文>\033[201~'` で囲む。実端末を
使わずに、モーダルの入力経路をひととおり踏める。

**モーダルを開いた画面を撮るときは `q` で終われない。** キーはモーダルが受けるので
`q` は落ち、プロセスが残ってコマンドがタイムアウトする。`Ctrl-C`（`printf '\003'`）を
送る（`update` の先頭で拾っていて、モーダル表示中でも終了する）。`Esc` で閉じてから
`q` を送ると、`screen.py` が出すのは閉じたあとの 1 枚になる。

**確認用の一時リポジトリは `env -i` で環境を切って作る。** `GIT_CONFIG_NOSYSTEM` などの
ために `HOME` をそのリポジトリに向けたまま python などを走らせると、macOS が
`Library/Caches/...` を作って untracked に何十件も混ざり、画面の確認ができなくなる。

```
env -i PATH="$PATH" HOME=/nonexistent GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null \
  sh -c "cd <dir> && git init -q -b main . && ..."
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
- **改行を含む文字列リテラルは 1 行で書く。行継続（`\` + 改行）を使わない。**
  `rustfmt` が畳むときに、次の行のインデントをリテラルの空白として残すことがある
  （確認モーダルの本文が「。」のあとに空白 17 個、という形で出る）。`contains` を見る
  テストは素通りするので、pty で動かすまで気付けない。テストのパッチも同じ理由で 1 行
- **`AppState` にコールバック（関数ポインタ・クロージャ）を持たせない。** 確定後に何を
  するかは `Cmd` かデータの enum で表す。関数ポインタは環境をキャプチャできず文脈を
  運べないうえ、状態が `PartialEq` を失って遷移をテストできなくなる
- **共有先に寄せるときは、その基盤の暗黙の前提が新しい利用者にも成り立つかを確かめる。**
  `ui::window` は「カーソルが自己完結した 1 行」を前提にしていて、ハンクという塊を指す
  Diff ペインでは見出しが窓の下端に来て本文が 1 行も見えなかった。**doc コメントに
  「2 つのペインで同じものを使う」と利点として書いたことが、前提が壊れている事実を
  隠していた。** 共有を主張する文を書くときは、そこに前提を並べて数える

## テスト

**テストなしで機能を追加しない。** 実装と同じコミットにテストを含める。`docs/plan.md` のタスクにチェックを付ける条件は、そのタスクの振る舞いを覆うテストが通っていること。

- テストが書きにくい設計になっていたら、テストを省くのではなく設計を直す（`docs/design/` を先に更新する）
- **共有した見張りは、新しい産出元を自分では拾わない。** 産出元が 2 つになったら、
  見張りが全部を通っているかを数える。#40 では `ui::LOADING`（複数のペインが出す文言）を
  画面全体の `contains` で見ていたので、新しいペインが出した時点で既存の見張りが 2 つ
  死んだ。#42 では確認モーダルの本文を組む場所が 2 つになったのに、畳み込みインデントの
  見張りは worktree の discard しか回していなかった。**どちらもテストは全部通っていた**
- **テストで踏めない状態をコードに入れない。** 設計書に書いた状態でも、いまの機能では
  到達経路が無いなら入れず、送り先と理由を設計書に書いて次のタスクへ送る（`DiffState.stale`
  はハンク単位の書き込みが入るまで踏めないので送った）
- **テストヘルパは、発行したジョブの返信も届けてから状態を渡す。** 届けないと
  `JobTracker` にジョブが残り、後続の同種の `Cmd` を `AppState::issue` が黙って落とす。
  #38 では、一覧の返信を届けないヘルパのせいで既存テストが `Cmd::LoadCommitDetail`
  単独を期待したまま通り、**一覧も頼んでいることを一つも見張れていなかった**。
  `Cmd` の期待値が「出なかった」なのか「落とされた」なのかは、`update` の戻り値
  だけからは読めない
- **「この見張りは本当に落ちるか」を確かめるとき、壊したつもりの置換が当たっていない
  ことがある。** `rustfmt` が畳んだ 1 行に対して複数行のパターンで置換すると、何も
  変わらないままテストが通り、**「見張りが効いていない」と誤読する**。置換したあとに
  `grep` で当たったことを確かめる
- **「この見張りは本当に落ちるか」を実装を壊して確かめるとき、`cargo test --workspace` は
  最初に失敗したターゲットで打ち切る。** 後続クレートの結果が出ないまま「落ちなかった」と
  誤読する。`--no-fail-fast` を付けるか、`-p <crate> --lib <テスト名>` で対象を絞る
- `mewgit-core` の git 系は `tempfile` に実リポジトリを作って統合テスト。helper は
  `crates/mewgit-core/src/test_util.rs` の `TestRepo`。`test-util` feature でのみコンパイル
  され、`mewgit-core` は自分自身を dev-dependency に書いて有効にしている。ここを使う
  クレートは dev-dependency 側で `features = ["test-util"]` を付ける（付けないと
  `cargo test -p <crate>` 単体で「feature の後ろに隠れている」と言われる）。
  リポジトリは `git` CLI で作り、
  `GIT_CONFIG_NOSYSTEM` / `GIT_CONFIG_GLOBAL` / `HOME` で実行環境の設定を持ち込まない
- `TestRepo` はコミット日時を実時間ではなく固定カウンタから与える（`git` を 1 回叩くごとに
  60 秒進む）。実時間だと連続したコミットが同じ秒に落ち、`Sort::TIME` での並びが揺れる。
  author 日時と committer 日時には同じ値が入るので、両者が食い違うケースを踏みたいときは
  `git commit --amend --date` で author 側だけずらす
- 非 UTF-8 のコミットメッセージは `-c i18n.commitEncoding=ISO-8859-1` を付けて `-F` で
  渡さないと作れない。git は「UTF-8 ではない」と判断したメッセージを latin-1 とみなして
  UTF-8 に変換するので、素直に作ると生バイト列がオブジェクトに残らない（git 2.55 で確認）。
  `-q` も要る。summary の echo が `TestRepo` の UTF-8 検証に当たる
- 非 UTF-8 の**ファイル名**はワークツリーに作れない。macOS の APFS が
  `Illegal byte sequence` で弾く。`git update-index --add --cacheinfo` で index にだけ
  入れれば作れて、しかも「staged で追加 / ワークツリーからは消えている」になるので
  staged 側と unstaged 側を 1 回で踏める（`TestRepo::add_raw_path_to_index`）。
  パスを argv に載せるので `TestRepo::git` は `AsRef<OsStr>` を取る
- **`mv` でファイルを戻すと cargo が再ビルドしない。** `mv` は元の mtime を持ち込むので
  ビルド成果物より古くなり、fingerprint が変わらずに古いバイナリが走る。「この見張りは
  本当に落ちるか」を確かめるために実装を一時的に書き換えたときは、戻したあと `touch`
  してから流し直す
- `Path` 同士の `assert_eq!` は末尾の区切りを無視するので、パスの正規化はテストできない。
  git2 の `workdir()` / `path()` は `/…/repo/` と末尾を付けて返すため、正規化を確かめる
  ときは `to_string_lossy()` で文字列に直して見る
- **`auth` のテストは実物の `ssh` を叩く。** `-F` に一時ファイルの ssh_config を渡すと
  `/etc/ssh/ssh_config` ごと無視するので、実行環境から切り離せる（stub は要らない）。
  設定ファイルを 1 枚書くだけのヘルパは `test_util::temp_file`
- `graph` は固定のコミット列に対する `cells` の期待値テストを必ず追加する
- GitHub API は `wiremock` でモック。実 API は叩かない
- `git_exec::to_argv` は純粋関数なのでユニットテストで全 `GitOp` をカバーする
- **`runtime::run` のループテストは、非同期の返信が届く順序に依存できない。**
  `stream::iter` で渡したキー入力は即座に ready なので、リーダースレッドの返信
  （`Msg::LogLoaded` など）より先に処理される。「1 ページ目が積まれている前提」の
  ループテストは書けない。状態を用意してから遷移だけを見たいときは、ループを回さずに
  `update` / `layout` を直に呼ぶ
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
- **1 つのタスクを複数の PR に割ると決めたら、その割り方を `docs/plan.md` に反映し、
  なぜそう割るかを設計書に書く。** 会話の中だけで決めると、次のセッションは plan.md の
  元の粒度を読んで再び 1 本にまとめる。割った理由が設計書にある制約から来ているなら
  （「この 2 つは同じタスクでないと片方が宙に浮く」など）、そこにも書いて突き合わせる

### PR を作る前に

`gh pr create` の前に、CI 相当のチェックに加えて必ずこの順で実行する。

1. `/simplify` — 重複・冗長・不要な抽象を削る。指摘の適用まで含む
2. `/code-review medium` — バグを探す。指摘があれば直してから次へ
3. 修正が出たら CI 相当のチェックを流し直し、別コミットとして積む（amend しない）

どちらも指摘ゼロなら何もコミットせずに PR へ進む。レビュー指摘に「直さない」判断をした
場合は、その理由を PR 本文に書く。

**この 2 つを通すのは Rust コードを含む PR だけ。** docs や CI 設定しか触らない PR では
省く。`/simplify` の 4 観点（既存ヘルパの再利用・冗長な状態・無駄な計算・修正の深さ）は
どれもコード前提で、Markdown や YAML には当たらない。省いたことと理由は PR 本文に書く。

どちらもバックグラウンドのエージェントとして走り、完了はハーネスが通知する。待機のために
no-op のコマンドを回さない（`echo waiting` の空回しなど）。ターンを終えて通知を待つ。

**エージェントが失敗したら、自己レビューで代替せずに立て直す。** セッションのレート上限で
落ちることがあり、エラーにリセット時刻が出る。自分で書いたコードを自分で見直しても
独立性が無く、この手順が効いている理由そのものが失われる。時刻を過ぎてから、落ちた
エージェントを同じ観点で立て直す。

**2 つの指摘が逆を向くことがある。実害がある側を採る。** #38 では `/simplify` の
「将来 `Range` を出したときにガードが外れる」という一般化を入れ、`/code-review` に
その場の回帰（コンフリクトの案内が別の文言に化ける）を拾われた。**将来のための
一般化は、いま踏める腕が無ければ「テストで踏めない状態をコードに入れない」に負ける。**
採らなかった側は、送り先と理由をコメントか設計書に書いて次のタスクへ送る。

## やらないこと

- Windows 対応（MVP 外）
- コンフリクト解消ビュー（MVP 外。continue / abort のみ）
- GraphQL API（REST で足りる）
- 独自のトークン取得フロー（`gh` に任せる）
