---
name: renovate-pr
description: mewgit の Renovate PR を確かめてマージする手順（1 本ずつビルド、古い main の確かめ方と取り込み、Cargo.lock の衝突、新規 crate の公開日の数え方）。Renovate の PR を見る・直す・マージするときに使う。
---

# Renovate の PR を処理する

**先に `.claude/rules/dependencies.md` を読む**（14 日ルール、`cargo update` の範囲、crates.io の叩き方）。

- **Renovate の PR は 1 本ずつビルドして確かめる。「マージできる」と「壊れない」は別。**
  依存更新の PR は互いに依存することがある —— #26（crossterm 0.29）と #28（ratatui
  0.30）は**どちらを単独でマージしてもビルドが落ちた**（片方は feature が別バージョンに
  付いて `ratatui::crossterm::event::EventStream` が消え、もう片方は lock に crossterm が
  2 版並んだ）。手順は `gh pr checkout` → `git merge origin/main` →
  `cargo build --workspace --locked`
- **Renovate のブランチが古い main の上にあることがある。** Renovate は main が進むと
  自分でリベースするので**いつもではない** —— 古いのは、そのリベースより先にこちらが
  触ったとき。テスト数が減っていたらそれを疑う。**評価の前に確かめ**
  （`git merge-base --is-ancestor origin/main HEAD` が失敗したら古い）、古いときだけ
  `git merge origin/main`（rebase / force-push はしない。Renovate の元コミットを
  改変しないため）。`Cargo.lock` が衝突したら**main の lock を取り直して
  引き直す** —— `git checkout origin/main -- Cargo.lock && cargo update --offline
  --workspace`。`Cargo.toml` 側に新しい版が書いてあるので正しく入り直る。競合マーカーを
  手で消さない
