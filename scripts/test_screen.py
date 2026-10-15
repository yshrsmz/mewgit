"""`screen.py` の仮想画面が、実端末と同じ桁に文字を残すかを見る。

    python3 -m unittest discover -s scripts
"""

import unittest

from screen import replay


def row(data: str, width: int = 8) -> str:
    """1 行の画面に描き直し、連結した文字列で返す。

    全角の 2 桁目の印は幅を持たないので、連結した文字列の表示幅が端末の桁と揃う。
    """
    return "".join(replay(data, width, 1)[0])


def at(col: int, text: str) -> str:
    """1 始まりの桁 `col` へ移ってから `text` を書く。"""
    return f"\x1b[1;{col}H{text}"


class WideOverwriteTest(unittest.TestCase):
    def test_wide_tail_over_another_wide_head_clears_its_tail(self):
        # 「い」の 1 桁目を「名」の 2 桁目で潰す。実端末は「い」を丸ごと消すので
        # 4 桁目は空白。印が残ると以降が 1 桁左へ詰まり `名d` に見える。
        self.assertEqual(row(at(3, "い") + at(2, "名") + at(5, "d")), " 名 d   ")

    def test_narrow_over_wide_head_clears_its_tail(self):
        self.assertEqual(row(at(1, "詳細") + at(1, "S")), "S 細    ")

    def test_narrow_over_wide_tail_clears_its_head(self):
        self.assertEqual(row(at(1, "詳細") + at(2, "S")), " S細    ")

    def test_wide_over_wide_tail_clears_its_head(self):
        # 「詳」の 2 桁目を「名」の 1 桁目で潰す。1 桁目は空く。
        self.assertEqual(row(at(1, "詳") + at(2, "名")), " 名     ")

    def test_erasing_from_wide_tail_clears_its_head(self):
        self.assertEqual(row(at(1, "a詳") + at(3, "\x1b[K")), "a       ")

    def test_redrawing_shifted_wides_keeps_every_char(self):
        # 部分再描画で全角の並びが 1 桁ずれて描き直されても、1 文字おきに空白が
        # 入らない（前のフレームの印を、いま書いた全角の一部と取り違えない）。
        self.assertEqual(row(at(2, "あい") + at(1, "かき")), "かき    ")


if __name__ == "__main__":
    unittest.main()
