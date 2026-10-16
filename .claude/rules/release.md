---
paths:
  - "release-please-config.json"
  - ".release-please-manifest.json"
  - ".github/workflows/release.yml"
---

# リリース

`CLAUDE.md` の「Git 運用」にある「リリースは release-please の release PR をマージして切る」の続き。
決めた理由と実測は `docs/plan.md` M6 の「README、リリースワークフロー」の行にある。

- **`release-type: rust` に戻さない。** ルートの `Cargo.toml` に `[package]` が無いと
  `CargoToml` が throw し、メンバーの `version.workspace = true` は書き換え先が値でない
  （release-please 17.11.2 のソース）。`simple` + `extra-files` の toml で書き換えている
- **`Cargo.lock` の jsonpath は `@.name.value=='…'`。** release-please は値に位置を付けた
  オブジェクトとして TOML を読むので、素直な `@.name=='…'` は**警告だけ出して何も当たらない**
- **release PR とタグは GitHub App のトークンで作る。** `GITHUB_TOKEN` で作った PR には
  `pull_request` の CI が走らず、作ったタグの push も他のワークフローを起こさない。
  ビルドを同じワークフローの後段（`release_created`）に置いているのも後者のため。
  App の資格情報は **2 つとも secret**（`RELEASE_APP_CLIENT_ID` / `RELEASE_APP_PRIVATE_KEY`）
- **release PR では `Cargo.toml` の差分を見る。** 版の行は 3 つ（`[workspace.package]` と
  lock の `mewgit` / `mewgit-core`）。`Cargo.toml` 側だけが外れると、`--version` は
  古い版をエラー無しに出す（lock 側が外れれば `--locked` が落ちて気付く）
- **設定を変えたら、main に入れる前に release-please を dry-run する。** release-please の
  リポジトリを scratch に clone して `npm ci --ignore-scripts` でビルドし、
  `node build/src/bin/release-please.js release-pr --dry-run --repo-url yshrsmz/mewgit
  --target-branch <ブランチ> --token "$(gh auth token)"` を叩く。設定は対象ブランチから
  読まれるので、先に push しておく。`release.yml` を触る PR では build の matrix が走るので、
  runner の都合はそこで先に分かる
- **Release は draft で作り、添付と attestation が揃ってから公開する。** release-please の `draft` + `force-tag-creation`（draft のままだとタグが打たれないので後者が要る）。途中で落ちたら Release は draft のまま残るので、`release.yml` を `workflow_dispatch` でタグを指定して流し直す。公開済みのタグを渡すと、添付を差し替えずに tap への知らせだけを送り直す
- **公開の最後に homebrew-tap へ `repository_dispatch`（`mewgit-released`）を送る。** トークンは release の App から `owner: yshrsmz` / `repositories: homebrew-tap` に絞って発行するので、**App を homebrew-tap にも install しておく**（外すと公開の後でトークンの発行が落ちる）。tap 側の `update-mewgit.yml` は attestation の署名元を `release.yml` の `refs/heads/main` に固定して確かめるので、**ワークフローのファイル名を変えたら tap 側も揃える**
- **Formula の `url` / `sha256` は `if OS.mac?` で分ける。** `on_macos` / `on_linux` の中に置くと `brew audit --strict` が断り、`version` を書くと URL と重複すると断る。tap 側は版を URL から読むので、URL の形（`/releases/download/v<版>/`）を変えたら tap 側の読み方も揃える。雛形は `packaging/homebrew/mewgit.rb.template`、確かめ方は `release.yml` の formula ジョブと同じ（一時 tap に置いて `brew style` / `brew audit --strict`）
