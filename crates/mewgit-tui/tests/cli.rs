//! 本物の `mewgit` に起動引数を渡す。`--version` / `--help` は端末にも設定にも触れずに
//! stdout へ書いて終わり、読めない引数は端末に入る前に断ることを見る（`main.rs` の
//! 配線の確認。引数の読み方の腕は `args.rs` の単体テストが持つ）。

use std::process::{Command, Output, Stdio};

fn mewgit(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mewgit"))
        .args(args)
        .output()
        .expect("mewgit を起こせるはずです")
}

fn refusal(args: &[&str]) -> String {
    let output = mewgit(args);
    assert!(!output.status.success(), "{args:?}: {output:?}");
    assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.contains("mewgit --help"), "{args:?}: {stderr}");
    stderr
}

fn version_line() -> String {
    format!("mewgit {}\n", env!("CARGO_PKG_VERSION"))
}

fn stdout(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).expect("stdout は UTF-8 のはずです")
}

#[test]
fn version_prints_the_package_version() {
    for args in [
        &["--version"][..],
        &["-V"],
        &[".", "--version"],
        &["a", "b", "-V"],
    ] {
        let output = mewgit(args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert_eq!(stdout(&output), version_line(), "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}: {output:?}");
    }
}

#[test]
fn help_prints_the_usage() {
    // 後ろに置いても、知らないオプションや余分な引数より先に拾う。
    for args in [
        &["--help"][..],
        &["-h"],
        &[".", "--help"],
        &["--nope", "a", "b", "-h"],
    ] {
        let output = mewgit(args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        let help = stdout(&output);
        assert!(help.starts_with(&version_line()), "{args:?}: {help}");
        assert!(
            help.contains("使い方: mewgit [options] [--] [repo-path]"),
            "{args:?}: {help}"
        );
        assert!(output.stderr.is_empty(), "{args:?}: {output:?}");
    }
}

#[test]
fn an_unknown_option_is_refused() {
    for args in [&["--nope"][..], &[".", "--nope"]] {
        let stderr = refusal(args);
        assert!(
            stderr.contains("知らないオプションです: --nope"),
            "{stderr}"
        );
    }
}

#[test]
fn a_second_path_is_refused() {
    let stderr = refusal(&[".", "other"]);
    assert!(stderr.contains("余分な引数です: other"), "{stderr}");
}

#[test]
fn a_reader_that_closed_first_does_not_make_it_panic() {
    // 読み手を先に閉じた pipe を渡すと、書き込みは必ず EPIPE になる（`| head -1` の
    // 読み手が先に終わった形を、競合なしに作る）。
    for arg in ["--version", "--help"] {
        let (reader, writer) = std::io::pipe().expect("pipe を作れるはずです");
        drop(reader);
        let output = Command::new(env!("CARGO_BIN_EXE_mewgit"))
            .arg(arg)
            .stdout(Stdio::from(writer))
            .output()
            .expect("mewgit を起こせるはずです");
        assert!(output.status.success(), "{arg}: {output:?}");
        assert!(output.stderr.is_empty(), "{arg}: {output:?}");
    }
}
