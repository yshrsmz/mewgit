---
paths:
  - "**/Cargo.toml"
  - "Cargo.lock"
---

# 依存の追加と更新

`CLAUDE.md` の「開発環境」の続き。依存を足す・上げるとき、Renovate の PR を確かめるときに読む（`renovate-pr` スキルもここを読む）。

- 依存を新規追加するときは crates.io の公開日を確認し、14 日以上経過した版を採用する。既存依存の更新は Renovate（`yshrsmz/renovate-config` の `minimumReleaseAge: "2 weeks"`）が担保する
- **14 日ルールは間接依存にも当たるが、固定できるのは lock だけ。** `Cargo.toml` に
  書くのは直接依存だけで、そこから引かれる crate は範囲内で引き直せてしまう
  （#59 では `reqwest` の下の `aws-lc-rs` / `aws-lc-sys` が 7 日前の公開だった）。
  **手で数えるのは新規に入る回だけ**で、以後は Renovate に任せる —— `=` 固定は
  上の方針に反し、直接依存でないものを `Cargo.toml` に書く副作用のほうが大きい。
  人が `cargo update --workspace` を叩いた場合は誰も担保しない、という穴は残る。
  数え方は `git diff origin/main -- Cargo.lock` から新規の crate を出して 1 件ずつ
  crates.io に当たり、14 日未満は `cargo update -p <crate> --precise <版>` で落とす
- **`cargo update --offline --workspace` は lock 全体を範囲内で引き直す。** 狙った
  crate 以外も動くので、差分から新しく入った crate を出して公開日を数える（上の
  14 日ルール）。`git diff origin/main -- Cargo.lock | grep -E '^[+-]name = |^[+-]version = '`
- **crates.io の API は並列に叩くとレート制限で非 JSON を返す**
  （`JSONDecodeError: Expecting value: line 1 column 1` はそれ）。公開日を確かめるときは
  1 件ずつ `time.sleep(2)` を挟み、`User-Agent` を付ける
