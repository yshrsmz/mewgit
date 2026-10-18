# SessionEnd の hook。target/ が大きくなったときだけ cargo clean する。
#
# 毎回は消さない —— 並行して動いている別のセッションや rust-analyzer が同じ target/ で
# ビルドしている最中に消すと、そちらが落ちる。1 回のフルビルドは 2GB 弱で（2026-10-19 に
# 実測。test --no-run + clippy で 1.8GB、空から組み直して 38 秒）、cargo は古い成果物を
# 消さないので、依存やツールチェーンを上げるたびに積もる。
#
# CLEAN_TARGET_LIMIT_KB で閾値を変えられる（確かめるとき用）。

set -u

dir="${CLAUDE_PROJECT_DIR:-$(pwd)}"
target="$dir/target"
limit_kb="${CLEAN_TARGET_LIMIT_KB:-$((20 * 1024 * 1024))}"

[ -d "$target" ] || exit 0

size_kb=$(du -sk "$target" | cut -f1)
[ "$size_kb" -gt "$limit_kb" ] || exit 0

# 誰かがビルドしているあいだは消さない（rust-analyzer の cargo check も cargo として見える）。
if pgrep -x cargo >/dev/null || pgrep -x rustc >/dev/null; then
  exit 0
fi

cd "$dir" && cargo clean --quiet
