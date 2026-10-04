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
