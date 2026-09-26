#!/usr/bin/env python3
"""`script` が録った端末出力から最終フレームを復元する。

TUI は毎フレーム全画面を描き直すので、`script` の生出力にはフレームが何枚も
連なっている。エスケープを落としただけだと全部が 1 行に潰れて読めない
（前のフレームの残骸と最新の描画が混ざる）。カーソル移動と消去だけを解釈して
仮想画面に描き直すと、終了直前の 1 枚がそのまま出る。

使い方は `.claude/skills/pty-check/SKILL.md`を参照。

    python3 scripts/screen.py out.txt [幅] [高さ]

幅と高さは `stty` に与えた値と同じにする。既定は 80x24。
"""

import re
import sys
import unicodedata

# CSI シーケンス。解釈するのは H / J / K だけで、色などの SGR は捨てる。
#
# 中間バイトに `<` `>` `=` も許す。終了時に出る `\x1b[<1u`（キーボードプロトコルの
# pop）が外れると、そのまま画面に文字として落ちて最後のフレームに混ざる。
CSI = re.compile(r"\x1b\[([0-9;?<>=]*)([a-zA-Z])")
# 文字集合の切り替え（`\x1b(B` など）。ratatui は出さないが `script` の記録に混ざる。
CHARSET = re.compile(r"\x1b[()][A-Z0-9]")


def replay(data: str, width: int, height: int) -> list[list[str]]:
    """出力を仮想画面に描き直して、最後の状態を返す。"""
    screen = [[" "] * width for _ in range(height)]
    row = col = 0
    i = 0

    while i < len(data):
        char = data[i]

        if char == "\x1b":
            csi = CSI.match(data, i)
            if csi:
                args = [int(n) for n in csi.group(1).split(";") if n.isdigit()]
                final = csi.group(2)
                if final == "H":  # カーソル位置指定。引数は 1 始まり。
                    row = args[0] - 1 if args else 0
                    col = args[1] - 1 if len(args) > 1 else 0
                elif final == "J" and args and args[0] == 2:  # 画面消去
                    screen = [[" "] * width for _ in range(height)]
                elif final == "K":  # 行末まで消去
                    for x in range(col, width):
                        screen[row][x] = " "
                i = csi.end()
                continue

            charset = CHARSET.match(data, i)
            i = charset.end() if charset else i + 1
            continue

        if char == "\n":
            row += 1
            col = 0
        elif char == "\r":
            col = 0
        elif char >= " ":
            wide = unicodedata.east_asian_width(char) in "WF"
            if 0 <= row < height and 0 <= col < width:
                put(screen[row], col, char, wide)
            # 全角は 2 桁使う。1 桁で進めると以降の桁が全部ずれる。
            col += 2 if wide else 1

        i += 1

    return screen


# 全角の 2 桁目に置く印。表示幅を持たないので、行を連結したときに全角 1 文字ぶんの
# 幅になる。
WIDE_TAIL = ""


def put(line: list[str], col: int, char: str, wide: bool) -> None:
    """1 セル書く。全角の食い違いをここで解消する。

    ratatui は変わったセルだけを描き直すので、全角の上に半角が乗ることがある。
    実端末は残った半分を消すが、素朴に 1 セルずつ写すと消し残りが混ざる
    （「詳細」の上に別の文字が来て「詳S細a」のようになる）。
    """
    # 全角の 2 桁目に書き込むなら、残った 1 桁目は文字として成り立たないので空ける。
    # **左隣も 2 桁目なら消さない。** そこは直前に書いた全角の 2 桁目で、`line[col]` に
    # 残っているのは前のフレームの印。消すと、いま書いたばかりの全角が半分になる
    # （部分再描画で 1 文字おきに空白が入る）。
    if line[col] == WIDE_TAIL and col > 0 and line[col - 1] != WIDE_TAIL:
        line[col - 1] = " "

    line[col] = char
    if col + 1 < len(line):
        if wide:
            # 元がそこにあった文字の残骸を残さない。
            line[col + 1] = WIDE_TAIL
        elif line[col + 1] == WIDE_TAIL:
            # 全角の 1 桁目を半角で潰した。宙に浮いた 2 桁目も空ける。ここを
            # 忘れると、次の 1 文字がこの印を見て直前に書いたばかりの桁を消す
            # （1 文字おきに空白が入る）。
            line[col + 1] = " "


def main() -> None:
    path = sys.argv[1]
    width = int(sys.argv[2]) if len(sys.argv) > 2 else 80
    height = int(sys.argv[3]) if len(sys.argv) > 3 else 24

    with open(path, "rb") as f:
        data = f.read().decode("utf-8", "replace")

    for line in replay(data, width, height):
        print("".join(line).rstrip())


if __name__ == "__main__":
    main()
