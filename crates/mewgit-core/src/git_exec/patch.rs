//! ハンクを `git apply` に渡すパッチへ戻す純粋関数。
//!
//! 設計は `docs/design/03-git-exec.md` の「ハンクからパッチを組む」。持ち方は
//! `docs/design/02-git-read.md` の「ハンクの持ち方」。

use std::path::Path;

use super::ArgvError;
use crate::git_read::{FileDiff, FileStatus, Hunk};

/// 改行で終わらない行の後ろに置くマーカー。git の出力と同じ綴り。
const NO_EOL: &[u8] = b"\\ No newline at end of file\n";

/// [`hunk_patch`] でハンク単位のパッチを組めるファイルか。
///
/// **述語をパッチを組む関数の隣に置く。** 呼び出し側（`app`）に置くと、表せる範囲を
/// 広げたときに呼び出し側が取り残され、テストは fallback を期待したまま緑で通る。
///
/// いまは `Modified` の非バイナリだけ。それ以外は「ハンク = ファイル全体」で、
/// パッチに `--- /dev/null`（Added / Deleted）や `rename from` / `rename to`
/// （Renamed / Copied）のヘッダが要る。呼び出し側はファイル単位の操作に落とす（07）。
pub fn supports_hunk_patch(file: &FileDiff) -> bool {
    file.status == FileStatus::Modified && !file.binary
}

/// `path` の `hunk` 1 つだけを含むパッチ。
///
/// `GitOp::StageHunk` / `UnstageHunk` / `DiscardHunk` の `patch` はこれで作る。
/// 戻り値が `Vec<u8>` なのは、非 UTF-8 の行をそのまま通すため（02）。
///
/// **ハンクのヘッダは数え直さずそのまま載せる。** `git apply` はヘッダの行数と実際の
/// 行数が食い違うと `corrupt patch` で落ちるので、数え直しは「元は正しかったものを
/// 壊す」側にしか働かない。開始行は 3 方向とも当てたい側の位置が入っている
/// （stage は `-` 側 = index、unstage は `+` 側 = index、discard は `+` 側 =
/// ワークツリー）ので、2 つ目以降のハンクだけを送っても当たる。
pub fn hunk_patch(path: &Path, hunk: &Hunk) -> Result<Vec<u8>, ArgvError> {
    let name = patch_path(path)?;

    // `index <blob>..<blob> <mode>` の行は出さない。blob の hash を持っておらず、
    // 無くても `git apply` は通る（`--3way` を使わないため。03）。
    let mut patch =
        format!("diff --git a/{name} b/{name}\n--- a/{name}\n+++ b/{name}\n").into_bytes();
    patch.extend_from_slice(hunk.header.as_bytes());
    patch.push(b'\n');

    for line in &hunk.lines {
        let mut mark = [0u8; 4];
        patch.extend_from_slice(line.origin.mark().encode_utf8(&mut mark).as_bytes());
        // `content` は改行を落として持っているので、ここで足す。`\r` は中身の一部
        // として残っているので、CRLF のファイルがそのまま戻る（02）。
        patch.extend_from_slice(&line.content);
        patch.push(b'\n');
        if line.origin.is_no_eol() {
            patch.extend_from_slice(NO_EOL);
        }
    }

    Ok(patch)
}

/// パッチのヘッダに載せるパス。載せられない名前は拒否する。
///
/// **quote しない。** 非 ASCII も空白も、そのままの UTF-8 バイト列で `git apply` が
/// 受ける（git 2.55 で確認）。`diff --git a/x b/y.txt b/x b/y.txt` のように
/// `diff --git` 行だけでは割り切れない名前でも、`---` / `+++` の 2 行から決め直す。
///
/// **その代わり、受け側が名前の区切りと読む文字を含むものは通さない。** tab は名前の
/// 終わり、`\r` は CRLF のパッチの行末、先頭の `"` は quote された名前の始まりと
/// 読まれる。どれも**別のファイルに patch を当てる**形の失敗なので、静かに通さない。
/// 禁じる文字を数え上げず「制御文字か」で弾くのは、受け側の細かい規則を写し取らずに
/// 済ませるため。ファイル単位の stage / unstage / discard はパスを argv に載せる
/// だけなので、こういう名前でも通る。
fn patch_path(path: &Path) -> Result<String, ArgvError> {
    // 非 UTF-8 の判定は argv と同じところを通す。`ArgvError::NonUtf8Path` を組む
    // 場所が 2 つになると、片方だけ緩めたときに気付けない。
    let name = super::argv::path_arg(path)?;

    if name.starts_with('"') || name.chars().any(char::is_control) {
        return Err(ArgvError::UnquotablePath {
            path: path.to_path_buf(),
        });
    }

    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_read::LineOrigin;
    use crate::test_util::fake_hunk;

    /// パッチを行ごとの `String` にする。中身が UTF-8 のときだけ使う。
    fn lines(patch: &[u8]) -> Vec<String> {
        String::from_utf8(patch.to_vec())
            .expect("パッチが UTF-8 ではありません")
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn a_hunk_becomes_a_patch_with_the_file_header_in_front() {
        let hunk = fake_hunk(
            "@@ -12,3 +12,3 @@ fn main() {",
            &[
                (LineOrigin::Context, "12"),
                (LineOrigin::Deletion, "13"),
                (LineOrigin::Addition, "thirteen"),
                (LineOrigin::Context, "14"),
            ],
        );
        let patch = hunk_patch(Path::new("src/main.rs"), &hunk).expect("組めませんでした");

        assert_eq!(
            lines(&patch),
            [
                "diff --git a/src/main.rs b/src/main.rs",
                "--- a/src/main.rs",
                "+++ b/src/main.rs",
                // ヘッダは関数コンテキストごとそのまま。
                "@@ -12,3 +12,3 @@ fn main() {",
                " 12",
                "-13",
                "+thirteen",
                " 14",
            ]
        );
    }

    #[test]
    fn a_line_without_a_newline_gets_the_marker_after_it() {
        let hunk = fake_hunk(
            "@@ -1,2 +1,2 @@",
            &[
                (LineOrigin::Context, "a"),
                (LineOrigin::DeletionNoEol, "b"),
                (LineOrigin::AdditionNoEol, "B"),
            ],
        );
        let patch = hunk_patch(Path::new("n.txt"), &hunk).expect("組めませんでした");

        // マーカーは `-` / `+` の直後それぞれに 1 つずつ。ヘッダの行数には数えない。
        assert_eq!(
            lines(&patch)[4..],
            [
                " a",
                "-b",
                "\\ No newline at end of file",
                "+B",
                "\\ No newline at end of file",
            ]
        );
    }

    #[test]
    fn crlf_and_non_utf8_bytes_survive_unchanged() {
        // `content` は `Vec<u8>` のまま流す。lossy にすると、ステージした結果が
        // 元のファイルと変わる。
        let hunk = Hunk {
            header: "@@ -1 +1 @@".to_string(),
            lines: vec![
                crate::git_read::HunkLine {
                    origin: LineOrigin::Deletion,
                    content: b"caf\xe9\r".to_vec(),
                },
                crate::git_read::HunkLine {
                    origin: LineOrigin::Addition,
                    content: b"caf\xe8\r".to_vec(),
                },
            ],
        };
        let patch = hunk_patch(Path::new("a.txt"), &hunk).expect("組めませんでした");

        assert!(
            patch.ends_with(b"@@ -1 +1 @@\n-caf\xe9\r\n+caf\xe8\r\n"),
            "{patch:?}"
        );
    }

    #[test]
    fn a_path_with_a_space_or_non_ascii_goes_in_unquoted() {
        // `git diff` は `core.quotePath` の既定で非 ASCII を quote するが、
        // `git apply` は quote されていないものも読む（tests/hunk.rs で実際に叩く）。
        let hunk = fake_hunk("@@ -1 +1 @@", &[(LineOrigin::Context, "a")]);
        for path in ["sp ace.txt", "日本語.txt", "x b/y.txt"] {
            let patch = hunk_patch(Path::new(path), &hunk).expect("組めませんでした");
            assert_eq!(
                lines(&patch)[..3],
                [
                    format!("diff --git a/{path} b/{path}"),
                    format!("--- a/{path}"),
                    format!("+++ b/{path}"),
                ]
            );
        }
    }

    #[test]
    fn a_path_that_would_be_reparsed_as_another_file_is_refused() {
        let hunk = fake_hunk("@@ -1 +1 @@", &[(LineOrigin::Context, "a")]);
        // tab は名前の終わり、`\r` は行末、先頭の `"` は quote の始まりと読まれる。
        for path in ["a\tb.txt", "a\rb.txt", "a\nb.txt", "\"a.txt"] {
            let err = hunk_patch(Path::new(path), &hunk).expect_err("通ってはいけません");
            assert!(
                matches!(err, ArgvError::UnquotablePath { .. }),
                "{path:?} -> {err:?}"
            );
        }
    }

    #[test]
    fn a_non_utf8_path_is_refused() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let hunk = fake_hunk("@@ -1 +1 @@", &[(LineOrigin::Context, "a")]);
        let path = Path::new(OsStr::from_bytes(b"caf\xe9.txt"));
        let err = hunk_patch(path, &hunk).expect_err("通ってはいけません");
        assert!(matches!(err, ArgvError::NonUtf8Path { .. }), "{err:?}");
    }
}
