//! mewgit core: UI に依存しないロジック。
//!
//! レイヤー構成は `docs/design/00-overview.md` を参照。
//! - 読み取りは [`git_read`]（git2）、書き込みは [`git_exec`]（git CLI）。
//! - [`ui`] 相当は `mewgit-tui` クレート側にあり、ここには含めない。

/// unit 変種だけの enum について、変種の名前の並び 1 つから網羅 `match` と変種の配列を
/// 作る。並びは書いた順のまま（宣言順と違ってよい）。
///
/// **一覧を手で書くと、網羅 `match` が強いるのは腕だけで一覧の足し忘れは通る**
/// （`.claude/rules/rust-coding.md` の「離れた場所に『いま N ある』を書かない」）。同じ
/// 並びから両方を作るので、名前を書き忘れると E0004、同じ名前を 2 度書くと
/// `unreachable_patterns`（CI の `clippy -D warnings` で落ちる）になる。
macro_rules! all_variants {
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
