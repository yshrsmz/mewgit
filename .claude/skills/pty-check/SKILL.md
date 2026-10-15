---
name: pty-check
description: mewgit の TUI を pty で非対話に起動し、キーを送って画面を復元して確かめる手順。「実物で確かめる」「pty で動かす」「画面を撮る」「報告の前にバイナリを動かす」とき、キー入力の送り方や screen.py の読み方が要るときに使う。
---

# TUI を pty で動かして確かめる

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
送る（`update` の先頭で拾っていて、モーダル表示中でも終了する）。**`git` が走って
いるあいだの `Ctrl-C` は中断で、終了しない**（#56）ので、そのときは 2 回送る。`Esc` で
閉じてから `q` を送ると、`screen.py` が出すのは閉じたあとの 1 枚になる。

**`\t`（Tab）は効く。ペインをまたぐ操作も pty で踏める。** ここには以前
「送ってもフォーカスが移らない」「`Tab` → `s`（stage）を送っても何も起きない」と
書いてあったが、**移らなかったのではなく行き先が違っていた** —— 巡回順は
`AppState::focus_candidates()`（`Pane::WIP_FOCUS` の並び）で、**Graph の次は
Branches**。`Tab` 1 回で `s` を押しても Worktree には居ないので何も起きない。
実測（2026-09-17、80x24、WIP 行を選択、`config.github.enabled` は既定）—— `Tab` を
**3 回**送ると Worktree に届き、`s` が `git add` を実行した
（`git status --short` が ` M a.txt` → `M  a.txt`）。`Tab` 2 回で PRs ペインに入り、
`j` でカーソルの反転が追随することも生出力の SGR で確かめた。
**何回送るかは `focus_candidates()` の並びから数える** —— 選んでいる行（WIP /
コミット）と `config.github.enabled` で候補が変わるので、固定の回数を書かない。

**`screen.py` は色を落とす。** 反転や太字を確かめたいときは `out.txt` の生の
エスケープを見る（`\033[...m` のうち `7` を含む区間が反転）。

**`screen.py` の出力は桁が端末と揃う。** 全角の 2 桁目には幅 0 の印を置くので、連結した行の表示幅がそのまま端末の桁になる。以前は全角の 2 桁目で別の全角の 1 桁目を潰したときに印を消し残し、以降が 1 桁左へ詰まっていた（`x:破棄c:commit`・`Esc:戻る?:help`、枠の右端の `┃` も 1 桁左へずれた）。直したあとも、桁が合わないと疑ったら生の `ESC[<行>;<桁>H` と突き合わせる（`grep` ではなく python で `re.findall(r'\x1b\[24;(\d+)H([^\x1b]*)', data)` が速い）。`screen.py` を触ったら `python3 -m unittest discover -s scripts` を流す。

**確認用の一時リポジトリは `env -i` で環境を切って作る。** `GIT_CONFIG_NOSYSTEM` などの
ために `HOME` をそのリポジトリに向けたまま python などを走らせると、macOS が
`Library/Caches/...` を作って untracked に何十件も混ざり、画面の確認ができなくなる。

```
env -i PATH="$PATH" HOME=/nonexistent GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null \
  sh -c "cd <dir> && git init -q -b main . && ..."
```

**他の TUI（lazygit / gitui / tig）を撮るときは `HOME` を scratchpad に向ける**（設定と
状態を持ち込まない・書き込ませない）。**lazygit は初回起動でダイアログが出る**ので、
`Enter` で閉じてから撮る。**`q` で終わらずに止まることがある**（2026-09-30、lazygit
0.65.1 で 120 秒のタイムアウトまで返らなかった）ので、キーの列の最後に `Ctrl-C` を
2 回足す。**複数のサイズを 1 つのコマンドで回さない** —— 1 つが止まると全部が
バックグラウンドへ回され、残ったプロセスを `pkill` で片付けることになる。

```
(sleep 2.5; printf '\r'; sleep 1.5; printf 'q'; sleep 0.7; printf '\003'; sleep 0.5; printf '\003') \
  | script -q /dev/null sh -c "stty rows 24 cols 80; cd <repo>; exec env HOME=<scratchpad>/home lazygit" > out.txt 2>&1
```
