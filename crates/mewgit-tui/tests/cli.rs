//! 本物の `mewgit` に `--version` / `--help` を渡す。端末にも設定にも触れずに
//! stdout へ書いて終わることを見る（`main.rs` は単体テストを持たないので、ここに置く）。

use std::process::{Command, Output};

fn mewgit(arg: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mewgit"))
        .arg(arg)
        .output()
        .expect("mewgit を起こせるはずです")
}

fn stdout(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).expect("stdout は UTF-8 のはずです")
}

#[test]
fn version_prints_the_package_version() {
    for arg in ["--version", "-V"] {
        let output = mewgit(arg);
        assert!(output.status.success(), "{arg}: {output:?}");
        assert_eq!(
            stdout(&output),
            format!("mewgit {}\n", env!("CARGO_PKG_VERSION")),
            "{arg}"
        );
        assert!(output.stderr.is_empty(), "{arg}: {output:?}");
    }
}

#[test]
fn help_prints_the_usage() {
    for arg in ["--help", "-h"] {
        let output = mewgit(arg);
        assert!(output.status.success(), "{arg}: {output:?}");
        let help = stdout(&output);
        assert!(
            help.starts_with(&format!("mewgit {}\n", env!("CARGO_PKG_VERSION"))),
            "{arg}: {help}"
        );
        assert!(help.contains("使い方: mewgit [repo-path]"), "{arg}: {help}");
        assert!(output.stderr.is_empty(), "{arg}: {output:?}");
    }
}

#[test]
fn an_unknown_option_is_refused_with_a_pointer_to_help() {
    let output = mewgit("--nope");
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("知らないオプションです: --nope"),
        "{stderr}"
    );
    assert!(stderr.contains("mewgit --help"), "{stderr}");
}
