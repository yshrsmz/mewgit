//! git の版。設計は `docs/design/03-git-exec.md` の「git の最低版」。
//!
//! **版は能力として問う。** 版の数と比較はこのモジュールの中に閉じ、外に出すのは
//! 名前の付いた能力（[`GitVersion::keeps_comment_lines`]）と、案内に出す `Display` だけ。
//! 外から `>= 2.45` と書けないので、版と能力の対応はここの 1 箇所に集まる。
//!
//! **`Version` は `auth::gh` の同名の型と同じ形だが共有しない。** 版の読み方が意図して
//! 違い（下の `parse_version`）、共有できるのは 3 つの数と `Display` だけで、置き場所の
//! ために `auth` か新しい下段のモジュールへ版の概念を移すほどの量が無い。

use std::path::Path;

use crate::auth::probe::{self, ProbeError};

/// 起動の床。`rebase --no-update-refs` が入った版（03）。
const MINIMUM: Version = Version(2, 38, 0);

/// `core.commentString` を読む最初の版（03 の「Interactive rebase の実装」）。
const COMMENT_STRING: Version = Version(2, 45, 0);

/// [`GitVersion::keeps_comment_lines`] が真になる版。案内の文に出す（`GitVersion` は
/// 比べられないので、`app` が版で分岐する手段にはならない）。
pub const KEEPS_COMMENT_LINES_SINCE: GitVersion = GitVersion(COMMENT_STRING);

/// 起動の床を満たした git の版。
///
/// **作れる経路は [`detect`](Self::detect) と [`parse`](Self::parse) だけ**で、どちらも
/// 床を確かめる。`Ord` は付けない（上のモジュールの doc）。テストも `parse` で作る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitVersion(Version);

impl GitVersion {
    /// `<git_bin> --version` を叩き、床を満たすときだけ返す。
    ///
    /// `git_bin` には `config.general.git_bin` を渡す（`Runner` が起こすのと同じ git）。
    pub async fn detect(git_bin: &Path) -> Result<Self, VersionError> {
        let stdout =
            probe::output(git_bin, ["--version"], &[])
                .await
                .map_err(|err| match &err {
                    ProbeError::Spawn { .. } => VersionError::Unrunnable {
                        message: err.describe(),
                    },
                    ProbeError::Failed { .. } => VersionError::Failed {
                        bin: git_bin.display().to_string(),
                        message: err.describe(),
                    },
                })?;
        Self::parse(stdout.lines().next().unwrap_or(""))
    }

    /// `git --version` の 1 行目を読み、床を確かめる。
    ///
    /// **起動と分けてある。** 一緒にすると、古い git を持っていない限り
    /// [`VersionError::TooOld`] の腕を踏めない（`auth::gh` の `check_version` と同じ）。
    /// 版を読めない形も `TooOld` に畳む —— 通す理由が無い。案内には生の 1 行目を出す。
    pub fn parse(first_line: &str) -> Result<Self, VersionError> {
        let line = first_line.trim();
        match parse_version(line) {
            Some(version) if version >= MINIMUM => Ok(Self(version)),
            found => Err(VersionError::TooOld {
                found: found.map_or_else(|| line.to_string(), |v| v.to_string()),
            }),
        }
    }

    /// reword / squash のメッセージで、行頭のコメント文字の行を残せるか。
    ///
    /// 残せない git は `core.commentString` を読み飛ばし、打ったメッセージや squash で
    /// つなぐメッセージから行頭 `#` の行を exit 0 のまま消す（03 の実測）。
    pub fn keeps_comment_lines(self) -> bool {
        self.0 >= COMMENT_STRING
    }
}

impl std::fmt::Display for GitVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// 起動を断る理由。`main` が端末に入る前に stderr へ出す。
#[derive(Debug, thiserror::Error)]
pub enum VersionError {
    /// 起動できなかった（`general.git_bin` が無い・実行できない）。
    #[error("{message}（general.git_bin を確かめてください）")]
    Unrunnable { message: String },
    #[error("{bin} --version が失敗しました（{message}）")]
    Failed { bin: String, message: String },
    #[error("mewgit には git {MINIMUM} 以上が要ります（見つかったのは {found}）")]
    TooOld { found: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version(u32, u32, u32);

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// `git version 2.54.0 (Apple Git-157)` から版を取る。
///
/// **major と minor は数字を要求し、patch は先頭の数字だけを取る**（無ければ 0）。
/// 4 つ目以降と後ろの括弧は読まない —— gh と違い、4 つ目が続く形が実在する
/// （Windows の `2.45.1.windows.1`、ソースから作った `2.47.0.123.gabcdef`）。
fn parse_version(line: &str) -> Option<Version> {
    let number = line
        .strip_prefix("git version ")?
        .split_whitespace()
        .next()?;
    let mut parts = number.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts
        .next()
        .and_then(|patch| patch.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|digits| digits.parse().ok())
        .unwrap_or(0);
    Some(Version(major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_forms_git_prints() {
        for (line, expected) in [
            ("git version 2.56.0", Version(2, 56, 0)),
            ("git version 2.54.0 (Apple Git-157)", Version(2, 54, 0)),
            ("git version 2.45.1.windows.1", Version(2, 45, 1)),
            ("git version 2.47.0.123.gabcdef", Version(2, 47, 0)),
            ("git version 2.47.0.rc1", Version(2, 47, 0)),
            ("git version 2.50", Version(2, 50, 0)),
            ("git version 3.0.0", Version(3, 0, 0)),
        ] {
            assert_eq!(
                GitVersion::parse(line).unwrap(),
                GitVersion(expected),
                "{line}"
            );
        }
    }

    #[test]
    fn refuses_below_the_floor_and_what_it_cannot_read() {
        for (line, found) in [
            // Ubuntu 22.04。`rebase --no-update-refs` が `unknown option` で落ちる（03）。
            ("git version 2.34.1", "2.34.1"),
            ("git version 2.37.9", "2.37.9"),
            ("hub version 2.14.2", "hub version 2.14.2"),
            ("git version two", "git version two"),
            ("", ""),
        ] {
            let Err(VersionError::TooOld { found: actual }) = GitVersion::parse(line) else {
                panic!("断るはずです: {line}");
            };
            assert_eq!(actual, found, "{line}");
        }
        assert!(GitVersion::parse("git version 2.38.0").is_ok());
    }

    #[test]
    fn comment_lines_survive_from_2_45() {
        let keeps = |line| GitVersion::parse(line).unwrap().keeps_comment_lines();
        // Debian 12 と Ubuntu 24.04 の既定。`#` の行が消える（03 の実測）。
        assert!(!keeps("git version 2.39.5"));
        assert!(!keeps("git version 2.43.0"));
        assert!(!keeps("git version 2.44.9"));
        assert!(keeps("git version 2.45.0"));
        assert!(keeps("git version 2.47.3"));
        assert!(keeps("git version 3.0.0"));
    }

    #[test]
    fn the_refusal_names_the_floor_and_what_was_found() {
        let err = GitVersion::parse("git version 2.34.1").unwrap_err();
        assert_eq!(
            err.to_string(),
            "mewgit には git 2.38.0 以上が要ります（見つかったのは 2.34.1）"
        );
    }

    #[tokio::test]
    async fn detect_accepts_the_git_on_this_machine() {
        GitVersion::detect(Path::new("git"))
            .await
            .expect("手元の git は床を満たすはずです");
    }

    #[tokio::test]
    async fn detect_reports_a_git_that_is_not_there() {
        let err = GitVersion::detect(Path::new("/nonexistent/mewgit-git"))
            .await
            .unwrap_err();
        assert!(matches!(err, VersionError::Unrunnable { .. }), "{err:?}");
    }
}
