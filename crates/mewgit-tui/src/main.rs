//! mewgit エントリポイント。
//!
//! 隠しサブコマンド `--sequence-editor <todo>` / `--commit-editor <file>` は interactive
//! rebase 用。git が `GIT_SEQUENCE_EDITOR` / `GIT_EDITOR` として起こし、plan は git から
//! 継いだ標準入力で届く（`docs/design/03-git-exec.md` の
//! 「Interactive rebase の実装」）。本体は `mewgit_core::git_exec::write_todo`。
//!
//! ここは配線だけで、実端末が必要なためテストを書かない（`.claude/rules/testing.md` の例外）。
//! 分岐のある処理は `runtime` 側に置く。

mod input;
mod modals;
mod panes;
mod runtime;
#[cfg(test)]
mod test_util;
mod ui;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Result};
use mewgit_core::app::NoticeLevel;
use mewgit_core::config::Config;
use mewgit_core::git_exec;
use ratatui::crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, EventStream, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;

fn main() -> Result<ExitCode> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    // **端末にも設定にも触れる前に分ける。** git の子として起こされており、
    // 代替画面に入ると git の端末を壊す。
    match args.first().and_then(|arg| arg.to_str()) {
        Some("--sequence-editor") => {
            return Ok(editor(&args[1..], |todo| {
                git_exec::write_todo(std::io::stdin().lock(), todo)
            }))
        }
        Some("--commit-editor") => return Ok(editor(&args[1..], git_exec::write_commit_message)),
        _ => {}
    }
    tui(repo_path(args.first())?)?;
    Ok(ExitCode::SUCCESS)
}

/// エディタとして起こされたとき。git はファイルのパスを末尾に 1 つ足す。
///
/// 失敗は stderr に書いて非 0 で返す。git が `there was a problem with the editor` と
/// 並べて自分の stderr に出す（sequence editor なら rebase を始めずに終わる。03 の実測）。
fn editor(
    args: &[OsString],
    write: impl FnOnce(&Path) -> Result<(), git_exec::EditorError>,
) -> ExitCode {
    let [file] = args else {
        eprintln!("mewgit: エディタとしてはファイルのパスを 1 つ受け取ります");
        return ExitCode::FAILURE;
    };
    match write(file.as_ref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("mewgit: {err}");
            ExitCode::FAILURE
        }
    }
}

#[tokio::main]
async fn tui(repo: PathBuf) -> Result<()> {
    let (config, warnings) = Config::load();
    let (mut state, reader) = runtime::open(&repo, config)?;

    // 設定の警告は起動時に見せる。読み込み自体は失敗しない（08 参照）。
    for warning in warnings {
        state.push_notice(NoticeLevel::Warn, warning.to_string(), None);
    }

    // raw mode / 代替画面 / パニックフックは ratatui が持っている。
    let mut terminal = ratatui::try_init()?;
    push_keyboard_enhancement();
    // **貼り付けを 1 つの `Msg` で受け取る。** 要求しないと、貼り付けた文字が 1 文字
    // ずつキーとして届き、コミットメッセージを貼ると最初の改行で確定して、残りが
    // ペインのキーとして解釈される（`x` で discard の確認が開き、あとから来た `y` が
    // それを実行する）。
    let _ = execute!(std::io::stdout(), EnableBracketedPaste);

    let result = runtime::run(&mut terminal, EventStream::new(), &mut state, reader).await;

    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    // restore は () を返すので、ループのエラーを後始末のエラーで踏み潰さない。
    ratatui::restore();
    result
}

/// kitty keyboard protocol を要求する。
///
/// 有効になると `Shift+Enter` が `Enter` と区別して届く（`CSI 13;2u`）。コミット
/// メッセージの改行に使う（01 の「コミットメッセージの編集」）。対応しない端末では
/// 何も起きず、そこでは `Alt+Enter`（`ESC CR`）と `Ctrl-J` が改行になる。
///
/// **対応するかを問い合わせない。** `crossterm::terminal::supports_keyboard_enhancement`
/// は端末に `CSI ?u` を投げて応答を待ち、**応答しない端末では 2 秒待ってから諦める**
/// （crossterm 0.28.1 の `terminal/sys/unix.rs`）。実測で起動が 0.82 秒 → 2.04 秒に
/// なった。対応しない端末ほど黒い画面を長く見ることになる。`CSI > 1 u` は private な
/// CSI なので、解さない端末は読み飛ばす。押せたかを知る必要も無い（`Enter` の意味は
/// どちらでも変わらず、増えるのは受けられる改行キーだけ）。
///
/// 押す旗は `DISAMBIGUATE_ESCAPE_CODES` だけにする。`REPORT_EVENT_TYPES` まで押すと
/// 離鍵も届き、1 打鍵が 2 つの `Msg::Key` になる。
fn push_keyboard_enhancement() {
    let _ = execute!(
        std::io::stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
}

/// 開くリポジトリ。引数が無ければカレントディレクトリ。
///
/// 絶対パスに直すのは、`.` や `..` のままだと `file_name()` が `None` になり、
/// ヘッダに `.` と出てしまうため。
fn repo_path(arg: Option<&OsString>) -> Result<PathBuf> {
    let raw = match arg {
        Some(arg) => {
            // オプションは隠しサブコマンドしか解釈しないので、`--help` という名前の
            // リポジトリを開きにいかないよう弾く。
            if arg.to_string_lossy().starts_with('-') {
                bail!(
                    "オプションはまだ対応していません: {}",
                    arg.to_string_lossy()
                );
            }
            PathBuf::from(arg)
        }
        None => PathBuf::from("."),
    };

    Ok(std::fs::canonicalize(&raw).unwrap_or(raw))
}
