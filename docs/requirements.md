# TUI 版 GitKraken 要件定義書

- 作成日: 2026-09-02
- ステータス: ドラフト v0.1

## 1. 目的・コンセプト

ターミナル上で Git のコミットグラフ・ステージング・ブランチ操作・履歴操作・GitHub PR 連携を、キーボード中心に一画面で扱えるツールを作る。

- 位置づけ: lazygit（操作の軽さ）と GitKraken（グラフの視認性・GitHub 統合）の中間
- 認証情報を自前で持たず、ローカルの `~/.ssh` と `gh` CLI に完全委譲する
- GitHub EMU（Enterprise Managed Users）環境で確実に動作する

## 2. スコープ

### 2.1 MVP に含める

- リポジトリ読込・コミットグラフ（tig 風レーン描画）・コミット詳細
- ワーキングツリー操作（ハンク単位のステージ／アンステージ、diff）
- コミット（メッセージ入力、amend）
- ブランチ操作（作成・切替・削除・リネーム）
- fetch / pull / push（トラッキング設定込み）
- 履歴操作: interactive rebase、cherry-pick、revert、stash
- GitHub 統合: PR 表示・作成・チェックアウト・一覧、CI チェック表示

### 2.2 MVP に含めない（拡張候補）

- コンフリクト解消の専用ビュー（MVP では conflict 検出と continue / abort のみ）
- 複数リポジトリ切替、submodule、worktree
- コマンドパレット、キーバインドカスタマイズ、テーマ
- Issue 連携、レビューコメントの投稿
- Windows 対応

## 3. 認証・接続要件

### 3.1 Git 操作（fetch / pull / push）

- `git` CLI をサブプロセスとして実行する。これにより `~/.ssh/config`（Host エイリアス、IdentityFile、ssh-agent）と `git credential` ヘルパーが自動で効く
- libgit2（git2 クレート）は読み取り専用で使い、ssh 認証を再実装しない
- HTTPS リモートは `git credential` に委譲する（`gh auth setup-git` 済みなら gh がヘルパーになる）

### 3.2 SSH エイリアスの正規化

- EMU アカウントの SSH 鍵は個人アカウントと別鍵になるため、`~/.ssh/config` の Host エイリアス運用（例: `github.com-work`）を前提とする
- リモート URL のホスト部がエイリアスの場合、`ssh -G <alias>` で `hostname` を引き、GitHub API 用の実ホスト名に正規化する
- SSH 鍵の SAML SSO 承認漏れは、`git fetch` の stderr に含まれる `SAML SSO` エラー文を検出し、承認手順を案内する

### 3.3 GitHub API トークン

- 起動時に `gh auth status` を解析し、ホストごと・ユーザーごとのログイン状況を取得する
- トークンは `gh auth token --hostname <host> --user <login>` で取得し、アクティブアカウントに依存しない
- トークンはメモリ上のみ保持し、ファイルやログに書かない
- トークンの由来（OAuth / fine-grained PAT / classic PAT）は問わない。EMU では OAuth App 制限により PAT を `gh auth login --with-token` で入れているケースが多いため
- `gh` 未インストール／**版が古い**（`gh auth status --json` が入った 2.81.0 以上が要る。04）／未ログイン／トークン解決失敗の場合、GitHub パネルのみ degraded 表示にし、Git 機能は必ず動作させる

### 3.4 アカウント解決（EMU 対応の中核）

同一ホスト（github.com）に個人アカウントと EMU アカウントが同時ログインされている前提で、リポジトリごとに使用アカウントを決定する。

優先順位:

1. ユーザーの明示設定（`.git/config` または設定ファイルの `user = <login>`）
2. リモート URL のホスト（`github.com` / `<sub>.ghe.com` のデータレジデンシー環境）で候補を絞る
3. 候補トークンで `GET /repos/{owner}/{repo}` を試し、成功したものを採用
4. 解決不能なら GitHub パネルを degraded にし、選択 UI を出す

解決結果（ログイン名とホスト）は画面上部に常時表示する。

### 3.5 権限・ネットワーク制約への対処

- API の 403 / 404 は握りつぶさず、HTTP ステータスとレスポンス本文の先頭を表示する
- 権限不足時は必要スコープと `gh auth refresh -h <host> -s <scope>` を案内する。自前のトークン取得フローは持たない
- 必要スコープの目安: `repo`、`read:org`、CI 状態取得のための `workflow` 相当
- IP アローリスト等で API が失敗する環境でも Git 機能は継続する

## 4. 機能要件

### 4.1 コミットグラフ

- tig 風のレーン描画。各コミットにレーンを割り当て、親との接続を罫線（`│ ├ ╯ ╮ ┼` 等）で表現する
- ソートは topo-order + 日付（`git log --topo-order` 相当）
- 画面外のコミットは計算しない。ウィンドウ単位でレーン状態を保持し、スクロールに合わせて追加計算する
- ブランチごとに色を割り当てる
- 同一行にラベルを描画: ローカルブランチ / リモートブランチ / タグ / HEAD / 紐づく PR 番号
- Unicode 罫線が使えない端末向けに ASCII フォールバック（`| / \ *`）

### 4.2 ワーキングツリー・コミット

- 変更ファイル一覧（staged / unstaged / untracked）
- ファイル単位・ハンク単位のステージ／アンステージ
- diff 表示（side-by-side は拡張）
- コミットメッセージ入力（複数行）、amend
- `.gitignore` 追加、discard（確認付き）

### 4.3 ブランチ

- 作成・切替・削除・リネーム
- リモートブランチからのチェックアウト、トラッキング設定
- 削除時、未マージなら警告

### 4.4 リモート操作

- fetch（全リモート / 指定リモート、prune）
- pull（merge / rebase の選択）
- push（トラッキング未設定なら `-u` を提案、force push は確認）
- 実行コマンドと stderr をそのまま表示する

### 4.5 履歴操作

**Interactive rebase**

- コミット一覧で pick / reword / edit / squash / fixup / drop を編集し、並べ替えも可能
- `GIT_SEQUENCE_EDITOR` に自前スクリプトを渡して `git rebase -i` を実行する
- 途中停止（edit / conflict）からの continue / skip / abort を UI で扱う

**cherry-pick**

- 単一・範囲・複数選択
- conflict 時の continue / abort

**revert**

- 単一・複数
- merge commit は親番号（`-m`）を選択

**stash**

- 一覧・内容 diff 表示
- 作成（メッセージ、`--include-untracked`、部分 stash）
- apply / pop / drop

**進行中状態の復元**

- 起動時に `.git/rebase-merge`、`.git/rebase-apply`、`CHERRY_PICK_HEAD`、`REVERT_HEAD`、`MERGE_HEAD` を検出し、対応するモードで起動する
- ステータスバーに進行中操作を常時表示する

### 4.6 GitHub 統合

- 現ブランチに紐づく PR の表示: 番号・タイトル・状態・レビュー状況・CI チェック一覧（成功 / 失敗 / 進行中）
- PR 作成: タイトル・本文・ベースブランチ・draft 指定
- PR チェックアウト（`gh pr checkout` 相当。fork からの PR も対応）
- PR 一覧: 自分が作成 / レビュー依頼されているもの
- PR をブラウザで開く

## 5. 非機能要件

- 起動 1 秒以内
- 数万コミットのリポジトリでもグラフ描画がスクロールに追随（遅延ロード）
- Git 操作の失敗を握りつぶさず、実行コマンドと stderr を表示する
- 破壊的操作は確認ダイアログ: force push、`branch -D`、`reset --hard`、discard、rebase / cherry-pick / revert の abort
- 対応 OS: macOS / Linux
- ターミナルサイズ変更に追従。256 色 / TrueColor 対応。Nerd Font なしでも崩れない
- 長時間処理（fetch、API 呼び出し、大規模ログ読込）は UI スレッドをブロックしない

## 6. 技術構成

### 6.1 スタック

| 領域 | クレート | 用途 |
|---|---|---|
| TUI | ratatui + crossterm | 描画・入力・リサイズ |
| Git 読み取り | git2（libgit2） | ログ走査・diff・ステータス |
| Git 書き込み | `git` CLI（tokio::process） | rebase / cherry-pick / push など。ssh 設定と credential helper を自動で効かせる |
| GitHub | reqwest + serde | REST / GraphQL。`gh` は token 取得と `auth status` 解析にのみ使用 |
| 非同期 | tokio | fetch・API・大規模ログ読込の分離 |
| 設定 | toml + directories | `~/.config/mewgit/config.toml` |

### 6.2 方針

- 読み取りは git2、書き込みは CLI の二層構成。EMU の SSH 鍵切替や SAML エラー検出は CLI 経由の方が確実
- GitHub API は `gh api` ではなく直接呼ぶ（レスポンスの型付けとエラー処理を自前で制御するため）

### 6.3 画面構成

```
┌ Repo: owner/name  [acct: user_shortcode @ github.com] ─────┐
│ Graph（左・広）        │ 右上: Branches / Stash / PRs      │
│                        │ 右下: Working tree / Commit詳細   │
├──────────────── Status bar: 進行中の rebase / 選択操作 ────┤
```

- モーダル: rebase 編集、PR 作成、確認ダイアログ
- ステータスバー: 進行中操作、使用アカウント、直近のエラー

## 7. 開発順序

1. リポジトリ読込 + tig 風グラフ + コミット詳細（読み取り専用で完成させる）
2. ワーキングツリー・ステージング・コミット・ブランチ操作
3. fetch / pull / push（SSH エイリアス正規化と SAML エラー検出をここで実装）
4. `gh` 連携: アカウント解決 → PR 表示 → PR 作成・チェックアウト
5. interactive rebase・cherry-pick・revert・stash と進行中状態の復元

## 8. 未決事項

- プロダクト名（設定ディレクトリ名・バイナリ名に影響）
- GitHub Enterprise Server（`ghe.com` 以外の自社ホスト）への対応範囲
- CI チェックの取得元: Checks API のみか、Commit Status API も含めるか
- グラフのレーン割り当てアルゴリズムの詳細（別途設計）
