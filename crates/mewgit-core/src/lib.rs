//! mewgit core: UI に依存しないロジック。
//!
//! レイヤー構成は `docs/design/00-overview.md` を参照。
//! - 読み取りは [`git_read`]（git2）、書き込みは [`git_exec`]（git CLI）。
//! - [`ui`] 相当は `mewgit-tui` クレート側にあり、ここには含めない。

/// enum の変種の名前の並び 1 つから、網羅 `match` と全変種の一覧を作る。並びは書いた
/// 順のまま（宣言順と違ってよい）。
///
/// - `all_variants!(Ty { A, B })` —— unit 変種だけの enum。変種の配列 `[Ty::A, Ty::B]` を返す
/// - `all_variants!(missing Ty { A, B } in values)` —— フィールドを持つ変種がある enum
///   （値を作れない）。テストが手で組んだ値の一覧 `values`（`&Ty` を返すイテレータ）に
///   1 つも無い変種の名前を返す。パターンは `Ty::A { .. }` なので、タプル・構造体・unit の
///   どの変種にも当たる
///
/// **一覧を手で書くと、網羅 `match` が強いるのは腕だけで一覧の足し忘れは通る**
/// （`.claude/rules/rust-coding.md` の「離れた場所に『いま N ある』を書かない」）。同じ
/// 並びから両方を作るので、名前を書き忘れると E0004、同じ名前を 2 度書くと
/// `unreachable_patterns`（CI の `clippy -D warnings` で落ちる）になる。
macro_rules! all_variants {
    (missing $ty:ident { $($name:ident),* $(,)? } in $values:expr) => {{
        fn variant(value: &$ty) -> &'static str {
            match value {
                $($ty::$name { .. } => stringify!($name),)*
            }
        }
        let covered: Vec<&'static str> = $values.into_iter().map(variant).collect();
        [$(stringify!($name)),*]
            .into_iter()
            .filter(|name| !covered.contains(name))
            .collect::<Vec<&'static str>>()
    }};
    ($ty:ident { $($name:ident),* $(,)? }) => {{
        #[allow(dead_code)]
        fn exhaustive(value: $ty) {
            match value {
                $($ty::$name => {})*
            }
        }
        [$($ty::$name),*]
    }};
}

pub mod app;
pub mod auth;
pub mod config;
pub mod git_exec;
pub mod git_read;
pub mod github;
pub mod graph;

/// テスト用の一時リポジトリ。`test-util` feature でのみコンパイルされる。
///
/// `mewgit-tui` のテストからも使うので `tests/` ではなくここに置く。
#[cfg(feature = "test-util")]
pub mod test_util;
