//! 起動引数の解釈。
//!
//! 隠しサブコマンド（`--sequence-editor` / `--commit-editor`）はここを通らない。git が
//! 必ず先頭に置くので、`main` が先頭だけを見て先に分ける。

use std::ffi::OsString;

/// 引数を読んだ結果。
#[derive(Debug, PartialEq)]
pub enum Launch {
    Version,
    Help,
    Tui(Option<OsString>),
}

/// 起動引数を読む。断るときは、ユーザーに見せる理由を返す。
///
/// **`--help` / `--version` は位置によらず拾う**（`mewgit <path> --help`）。どちらかが
/// あれば、ほかの引数の誤りより先に、先に現れたほうを出す。無ければ、知らないオプションと
/// 2 つ目のパスを断る —— 黙って捨てると、頼んだつもりのことが起きないまま TUI が開く。
/// `--` から後ろはオプションとして読まないので、`-` で始まる名前のディレクトリも開ける。
pub fn parse(args: &[OsString]) -> Result<Launch, String> {
    let mut path = None;
    let mut refusal = None;
    let mut options_done = false;
    for arg in args {
        let text = arg.to_string_lossy();
        if options_done || !text.starts_with('-') {
            if path.is_none() {
                path = Some(arg.clone());
            } else {
                refusal.get_or_insert_with(|| format!("余分な引数です: {text}"));
            }
        } else {
            match &*text {
                "--" => options_done = true,
                "--version" | "-V" => return Ok(Launch::Version),
                "--help" | "-h" => return Ok(Launch::Help),
                _ => {
                    refusal.get_or_insert_with(|| format!("知らないオプションです: {text}"));
                }
            }
        }
    }
    match refusal {
        Some(refusal) => Err(format!("{refusal}（mewgit --help で使い方を出します）")),
        None => Ok(Launch::Tui(path)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Launch, String> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        parse(&args)
    }

    fn tui(path: &str) -> Result<Launch, String> {
        Ok(Launch::Tui(Some(OsString::from(path))))
    }

    #[test]
    fn no_args_opens_the_current_directory() {
        assert_eq!(parse_strs(&[]), Ok(Launch::Tui(None)));
    }

    #[test]
    fn a_path_is_opened() {
        assert_eq!(parse_strs(&["repo"]), tui("repo"));
    }

    #[test]
    fn help_and_version_are_found_anywhere() {
        assert_eq!(parse_strs(&["repo", "--help"]), Ok(Launch::Help));
        assert_eq!(parse_strs(&["repo", "-V"]), Ok(Launch::Version));
    }

    #[test]
    fn help_and_version_win_over_refusals() {
        assert_eq!(parse_strs(&["--nope", "a", "b", "-h"]), Ok(Launch::Help));
        assert_eq!(parse_strs(&["a", "b", "--version"]), Ok(Launch::Version));
    }

    #[test]
    fn the_first_of_help_and_version_wins() {
        assert_eq!(parse_strs(&["-V", "--help"]), Ok(Launch::Version));
        assert_eq!(parse_strs(&["-h", "--version"]), Ok(Launch::Help));
    }

    #[test]
    fn an_unknown_option_is_refused_wherever_it_is() {
        for args in [&["--nope"][..], &["repo", "--nope"], &["-"]] {
            let refusal = parse_strs(args).unwrap_err();
            assert!(
                refusal.starts_with("知らないオプションです: "),
                "{args:?}: {refusal}"
            );
            assert!(refusal.contains("mewgit --help"), "{args:?}: {refusal}");
        }
    }

    #[test]
    fn a_second_path_is_refused() {
        let refusal = parse_strs(&["a", "b"]).unwrap_err();
        assert!(refusal.starts_with("余分な引数です: b"), "{refusal}");
    }

    #[test]
    fn the_first_refusal_is_reported() {
        let refusal = parse_strs(&["a", "--nope", "b", "--other"]).unwrap_err();
        assert!(
            refusal.starts_with("知らないオプションです: --nope"),
            "{refusal}"
        );
    }

    #[test]
    fn after_double_dash_a_leading_dash_is_a_path() {
        assert_eq!(parse_strs(&["--", "--help"]), tui("--help"));
        assert_eq!(parse_strs(&["--", "-dir"]), tui("-dir"));
        assert_eq!(parse_strs(&["--", "--"]), tui("--"));
    }

    #[test]
    fn a_second_path_after_double_dash_is_refused() {
        let refusal = parse_strs(&["a", "--", "-b"]).unwrap_err();
        assert!(refusal.starts_with("余分な引数です: -b"), "{refusal}");
    }
}
