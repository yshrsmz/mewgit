# 06. graph: tig 風レーン描画

## 責務

コミット列（topo-order）を受け取り、各行のレーン割り当てと罫線セルを生成する。描画はしない（セルの列を返し、`ui` が文字と色に変換する）。

## データ構造

```rust
pub struct LaneState { lanes: Vec<Option<Oid>> }   // 各レーンが次に待つコミット

pub struct GraphRow {
    pub oid: Oid,
    pub lane: usize,                    // このコミットの列
    pub cells: Vec<Cell>,               // 左から順に
    pub color: ColorId,
}

pub enum Cell {
    Empty,
    Vertical(ColorId),                  // │
    Commit(ColorId),                    // ● / ○(merge)
    BranchOut(ColorId),                 // ├ ╮ など右へ分岐
    MergeIn(ColorId),                   // ╯ ╰ など左へ合流
    Horizontal(ColorId),                // ─
    Cross(ColorId, ColorId),            // ┼ 縦と横が別色
}
```

## アルゴリズム（1 行ずつ、状態を引き継ぐ）

各コミット `c`（親 `p1..pn`）について:

1. `lanes` から `c.oid` を待っているレーンを全部探す
   - 0 個: 新しいレーン（右端）に置く。ルート側の新規ブランチ先頭
   - 1 個以上: 最左を `c.lane` とし、残りは合流線（MergeIn）を引いて空ける
2. 親の配置
   - `p1` を `c.lane` に置く（同じ色を継承）
   - `p2..` は既に待っているレーンがあればそこへ Horizontal で接続、なければ右側の空きレーンに置いて BranchOut。新しい色を割り当てる
3. 空になった末尾レーンを詰める（中間の空きは残し、次のコミットで再利用）
4. `cells` を `lanes` の長さ分生成する

merge commit は `Commit` を白抜きにし、`p2` への線を分岐色で描く。

## 色

- ColorId は `p1` 連鎖で継承し、分岐時に新規採番
- 実際の色は `ui` で `ColorId % palette.len()` に写像。隣接レーンが同色にならないよう、採番時に左右レーンの色を避ける

## ページングとの整合

- `LaneState` は行ごとの結果と一緒に保存する（`Vec<(GraphRow, LaneState)>`）。追加ページ計算は最後の状態から再開できる
- 上方向のスクロールは既に計算済みなので再計算不要
- ref の変更（fetch 後）は先頭から再計算する。数千行までは 10ms 程度で終わる想定

## ラベル

- `RefIndex`（02）から `oid → Vec<RefLabel>` を引き、`GraphRow` の右に並べる
- 種別ごとの見た目: ローカル `[main]`、リモート `{origin/main}`、タグ `<v1.0>`、HEAD は太字、PR は `#123` を PR 状態の色で
- 幅が足りないときはラベル → summary の順で省略

## ASCII フォールバック

`config.graph.ascii = true` または端末が UTF-8 でないとき、`ui` 側で `│→|`、`╮→\`、`╯→/`、`●→*`、`┼→+` に置換する。`graph` 層は文字を持たない。

## テスト

- 直線 / 単純分岐 / マージ / オクトパスマージ / 複数ルート の固定ケースを `cells` の期待値と比較
- `git log --graph --topo-order` の出力と行数・親子関係が一致することをスナップショットで確認
