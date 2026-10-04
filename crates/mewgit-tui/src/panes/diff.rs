//! Diff ペインの描画。設計は `docs/design/07-ui.md` の「Diff ペイン」。
//!
//! 左の領域に Graph と差し替えで描く。1 行はハンクの見出し（`@@ ...`）か、
//! ` ` / `-` / `+` で始まる本文の行。

use std::ops::Range;

use mewgit_core::app::{AppState, DiffView, Pane};
use mewgit_core::git_read::{DiffTarget, FileStatus, HunkLine, LineOrigin};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::ui::{self, theme};

/// Diff ペインを描く。
pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    // **対象は 1 フレームに 1 回だけ導く。** `wanted_diff` は `PathBuf` を作るので、
    // タイトル・鮮度・文言でそれぞれ呼ぶと 1 フレームに 3 個確保することになる。
    let wanted = state.wanted_diff();
    // **`state.diff.view` を直に見ない。** 別のファイルの diff が残っていることがある
    // （01 の「diff の遅延取得」）。
    let view = state.diff.fresh_view(wanted.as_ref());

    let block = ui::pane_block(
        title(state, view, wanted.as_ref()),
        state.focus == Pane::Diff,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(view) = view else {
        let message = pending_message(state, wanted.as_ref());
        frame.render_widget(Paragraph::new(message), inner);
        return;
    };

    let (lines, cursor) = lines(view);
    if lines.is_empty() {
        frame.render_widget(Paragraph::new(no_hunks(view)), inner);
        return;
    }

    ui::render_rows(frame, inner, lines, cursor);
}

/// 枠のタイトル。**対象を必ず出す**（07）。
///
/// 左が Graph から差し替わっているので、出さないと何の diff かを画面から辿れない。
/// 届いていれば rename の旧い名前まで出せる。まだなら選んでいるパスだけ出す。
///
/// **対象全体（`path` が `None`）なら、対象そのものを名乗る**（stash は `stash@{n}`）。
/// ファイル名はそれぞれの見出しに出る（[`names_file`]）。
fn title(state: &AppState, view: Option<&DiffView>, wanted: Option<&DiffTarget>) -> String {
    if let Some(target) = wanted.filter(|target| target.path().is_none()) {
        return whole(state, target);
    }
    match view.map(|view| view.files.as_slice()) {
        Some([file]) => format!(
            "Diff: {}",
            ui::path_label(&file.path, file.old_path.as_deref())
        ),
        Some(files) if !files.is_empty() => format!("Diff ({} files)", files.len()),
        _ => match wanted.and_then(DiffTarget::path) {
            Some(path) => format!("Diff: {}", path.display()),
            None => "Diff".to_string(),
        },
    }
}

/// diff がまだ無いときの文言。
///
/// **「読みに行っていない」「失敗した」「待っている」を消去法で導かない。** 失敗した
/// ことは `DiffState.failed` が事実として持っている。`is_running` の否定から導くと、
/// `Cmd` の発行経路を変えたときに片方だけ直しても通ってしまう。
///
/// **`wanted` が `None` のままここへ来るのはコミット側だけ。** ワーキングツリーの行で
/// 対象が決まらないとき（変更が無い、conflicted）は `Diff` が巡回の候補から外れ、
/// `Enter` も断られる（01 の「Pane と Modal」）ので、ペインごと出ない。**残るのは
/// ファイル一覧が決まらない形だけ**なので、理由は一覧の側が持っている文言
/// （[`ui::files_placeholder`]）をそのまま使う。書き直すと 3 通りのどれかが落ちる。
fn pending_message(state: &AppState, wanted: Option<&DiffTarget>) -> &'static str {
    if wanted.is_none() {
        return ui::files_placeholder(state);
    }
    if state.diff.has_failed(wanted) {
        "差分を読み込めませんでした"
    } else {
        ui::LOADING
    }
}

/// 届いてはいるがハンクが 1 つも無いときの文言。
///
/// バイナリ・mode だけの変更・内容の同じ rename・中身が空のファイルは、どれも
/// `hunks` が空になる（02 の「diff とハンク」）。**一括りにしない。** すべてを
/// 「mode だけの変更」と出すと、`touch` しただけの空ファイルにも mode の話をする。
fn no_hunks(view: &DiffView) -> &'static str {
    match view.files.as_slice() {
        [] => "差分はありません",
        [file] if file.binary => "バイナリファイルです",
        [file] => match file.status {
            // 追加・削除なのに行が無い = 中身が空。
            FileStatus::Added | FileStatus::Deleted | FileStatus::Untracked => "空のファイルです",
            // 中身は同じまま名前だけ変わった。
            FileStatus::Renamed | FileStatus::Copied => "名前が変わっただけで、内容は同じです",
            // 内容が同じで mode だけ変わった。
            _ => "内容の変更はありません（mode だけの変更）",
        },
        // 複数ファイル（stash）で 1 つもハンクが無い形。
        _ => "内容の変更はありません",
    }
}

/// 描く行と、**カーソルの指すハンクが占める行の範囲**を返す。
///
/// 行番号を別の関数で数え直さないのは Worktree ペインと同じ理由で、見出しを何行入れるか
/// の規則が 2 本になると、反転する行と窓の基準行が静かにずれる。
///
/// **返すのは見出しの行番号ではなくハンク全体の範囲。** 見出し 1 行だけを窓に入れる
/// 形にすると、それが窓の下端に来て本文が 1 行も見えない（07 の「Diff ペイン」）。
///
/// **ファイルの見出しは、枠のタイトルがファイルを名乗っていないときだけ**
/// （[`names_file`]）。名乗っていれば、同じ名前を 2 度並べても行を消費するだけになる。
fn lines(view: &DiffView) -> (Vec<Line<'_>>, Range<usize>) {
    let mut lines = Vec::new();
    let mut cursor = 0..0;
    let headed = !names_file(view);

    for (file_index, file) in view.files.iter().enumerate() {
        if headed {
            let label = ui::path_label(&file.path, file.old_path.as_deref());
            lines.push(Line::from(Span::styled(label, theme::muted())));
        }

        for (hunk_index, hunk) in file.hunks.iter().enumerate() {
            let start = lines.len();
            let header = Line::from(Span::styled(hunk.header.as_str(), theme::muted()));
            let selected = view.cursor.file == file_index && view.cursor.hunk == hunk_index;

            lines.push(if selected {
                header.style(theme::selected())
            } else {
                header
            });
            lines.extend(hunk.lines.iter().map(hunk_line));

            if selected {
                cursor = start..lines.len();
            }
        }
    }

    (lines, cursor)
}

/// 対象全体の diff の枠のタイトル（[`title`]）。
fn whole(state: &AppState, target: &DiffTarget) -> String {
    match target {
        // 一覧から消えていれば（読み直しで drop を見た）番号は名乗れない。
        DiffTarget::Stash(oid) => match state.stashes.find(*oid) {
            Some(stash) => format!("Diff: {}", stash.name()),
            None => "Diff: stash".to_string(),
        },
        // `path: None` で組む経路は stash だけ（`AppState::wanted_diff`）。
        _ => "Diff".to_string(),
    }
}

/// 枠のタイトルがファイルを名乗るか（[`title`]）。
///
/// **対象が `path` で 1 ファイルに絞られているときだけ。** 絞っていない diff は、
/// 1 ファイルしか無くても対象そのもの（`stash@{n}`）を名乗るので、ファイル名は見出しに
/// 出す —— 出さないと、どのファイルの差分かが画面のどこにも無い。「stash なら」と
/// 変種で書くと、`path: None` の対象を足すたびにここへ足すことになる。
fn names_file(view: &DiffView) -> bool {
    view.target.path().is_some()
}

/// ハンクの 1 行。先頭に種別の記号を付ける。
/// 中身は所有した `String` なので、どの寿命の `Line` としても返せる。`'static` に
/// 固定すると、ヘッダを借用している `Vec<Line<'_>>` に `extend` できない
/// （`&mut Vec<T>` は `T` に対して不変）。
fn hunk_line<'a>(line: &HunkLine) -> Line<'a> {
    // **記号は `LineOrigin::mark()`。** パッチに戻す `git_exec::hunk_patch` と同じ
    // ものを使う。ここで別に書くと「画面では文脈行、パッチでは追加行」になりうる。
    // 色だけがこちらの持ち物（`theme` は core に無い）。
    let style = match line.origin {
        LineOrigin::Addition | LineOrigin::AdditionNoEol => theme::added(),
        LineOrigin::Deletion | LineOrigin::DeletionNoEol => theme::removed(),
        LineOrigin::Context | LineOrigin::ContextNoEol => Style::default(),
    };
    Line::from(Span::styled(
        marked(line.origin.mark(), &line.content),
        style,
    ))
}

/// 記号 1 文字と、その後ろに続く本文。
///
/// `content` は `Vec<u8>` なので**ここで lossy にする**（02 の「ハンクの持ち方」）。
/// パッチに戻す入力は `content` のままで、こちらは表示専用。記号と本文を 1 つの
/// `String` に組むのは、`format!` でもう一度丸ごとコピーしないため。
///
/// タブは [`ui::push_expanded`] で開く。桁は**本文の先頭から**数える —— 先頭の
/// `-` / `+` / ` ` を含めて数えると、変更行と文脈行でタブストップが 1 桁ずれるので、
/// 記号を足してから本文を別に渡す。
fn marked(mark: char, content: &[u8]) -> String {
    let mut out = String::with_capacity(content.len() + 1);
    out.push(mark);
    ui::push_expanded(&mut out, &String::from_utf8_lossy(content));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{
        col_of, deliver_commit_files, draw, pane_text, row_is_reversed, row_of, state_at,
        state_on_first_commit, state_showing_diff, state_with_status, tab_to, with_stashes,
    };
    use mewgit_core::app::{update, AppState, Cmd, Key, KeyCode, Msg, TermSize};
    use mewgit_core::git_read::{FileDiff, GitReadError, WorktreeSnapshot};
    use mewgit_core::test_util::{fake_entry, fake_file_diff, fake_hunk, fake_stash};

    /// 既定の 80x24。左は 46 桁になるので、テストはその幅で読める内容にする。
    const SIZE: TermSize = crate::test_util::SPLIT_NARROWEST;

    /// `a.txt` だけが変更されたワーキングツリー。
    fn one_file() -> WorktreeSnapshot {
        WorktreeSnapshot {
            unstaged: vec![fake_entry("a.txt", FileStatus::Modified)],
            ..WorktreeSnapshot::default()
        }
    }

    /// いま選んでいる対象の diff として `files` を届ける。
    fn deliver(state: &mut AppState, files: Vec<FileDiff>) {
        let target = state.wanted_diff().expect("対象が決まっていません");
        let _ = update(
            state,
            Msg::DiffLoaded {
                target,
                result: Box::new(Ok(files)),
            },
        );
    }

    /// `a.txt` を選び、`files` が届いた状態。
    fn state_with_diff(files: Vec<FileDiff>) -> AppState {
        let mut state = state_showing_diff(one_file());
        deliver(&mut state, files);
        state
    }

    /// 1 ファイル 1 ハンクの diff。
    fn one_hunk(path: &str, header: &str, lines: &[(LineOrigin, &str)]) -> Vec<FileDiff> {
        vec![fake_file_diff(path, vec![fake_hunk(header, lines)])]
    }

    #[test]
    fn hunks_and_lines_are_listed_with_their_marks() {
        let state = state_with_diff(one_hunk(
            "a.txt",
            "@@ -1,2 +1,2 @@ fn main() {",
            &[
                (LineOrigin::Context, "keep"),
                (LineOrigin::Deletion, "old"),
                (LineOrigin::Addition, "new"),
            ],
        ));

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("Diff: a.txt"), "{text}");
        assert!(text.contains("@@ -1,2 +1,2 @@ fn main() {"), "{text}");
        assert!(text.contains(" keep"), "{text}");
        assert!(text.contains("-old"), "{text}");
        assert!(text.contains("+new"), "{text}");
    }

    #[test]
    fn a_single_file_is_not_named_twice() {
        // 枠のタイトルに出ているので、本文に見出しを足すと行を消費するだけになる。
        let state = state_with_diff(one_hunk(
            "a.txt",
            "@@ -1 +1 @@",
            &[(LineOrigin::Context, "keep")],
        ));

        let lines = draw(&state, SIZE);
        let named = lines.iter().filter(|line| line.contains("a.txt")).count();

        // 枠のタイトルと、右のワーキングツリーの行の 2 つだけ。
        assert_eq!(named, 2, "{lines:?}");
    }

    #[test]
    fn the_cursor_hunk_header_is_highlighted() {
        let mut state = state_with_diff(vec![fake_file_diff(
            "a.txt",
            vec![
                fake_hunk("@@ -1 +1 @@ first", &[(LineOrigin::Context, "a")]),
                fake_hunk("@@ -9 +9 @@ second", &[(LineOrigin::Context, "b")]),
            ],
        )]);
        // `j` で 2 つ目のハンクへ。
        let _ = update(&mut state, Msg::Key(Key::char('j')));

        assert!(row_is_reversed(&state, SIZE, "second"));
        assert!(!row_is_reversed(&state, SIZE, "first"));
    }

    #[test]
    fn the_body_of_the_cursor_hunk_is_visible_not_just_its_header() {
        // 見出し 1 行だけを窓に入れると、見出しがちょうど下端に来て本文が 1 行も
        // 見えない。**選んだハンクに対して破壊的な操作を押すことになる**（07）。
        let body = |tag: &str| -> Vec<(LineOrigin, String)> {
            (0..10)
                .map(|n| (LineOrigin::Context, format!("{tag}{n}")))
                .collect()
        };
        let hunk = |header: &str, tag: &str| {
            let owned = body(tag);
            let lines: Vec<(LineOrigin, &str)> = owned
                .iter()
                .map(|(origin, text)| (*origin, text.as_str()))
                .collect();
            fake_hunk(header, &lines)
        };
        let mut state = state_with_diff(vec![fake_file_diff(
            "a.txt",
            vec![
                hunk("@@ HUNK1 @@", "one"),
                hunk("@@ HUNK2 @@", "two"),
                hunk("@@ HUNK3 @@", "three"),
            ],
        )]);

        // 2 つ目のハンクへ。ここまでで 22 行あり、ペインの高さ（20 行）を超える。
        let _ = update(&mut state, Msg::Key(Key::char('j')));

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("@@ HUNK2 @@"), "{text}");
        assert!(
            text.contains("two0"),
            "見出しだけで本文が見えていない: {text}"
        );
        assert!(text.contains("two9"), "本文の末尾まで見えていない: {text}");
    }

    #[test]
    fn a_diff_for_another_file_is_not_shown_while_the_new_one_loads() {
        // 届くまで前のファイルの diff を出したままにすると、右で b.txt を選んでいるのに
        // 左は a.txt という画面になる。次のタスクのハンク操作はこの `view` を読むので、
        // b.txt を選んだまま a.txt のハンクを stage することになる（01）。
        let snapshot = WorktreeSnapshot {
            unstaged: vec![
                fake_entry("a.txt", FileStatus::Modified),
                fake_entry("b.txt", FileStatus::Modified),
            ],
            ..WorktreeSnapshot::default()
        };
        let mut state = state_showing_diff(snapshot);
        deliver(
            &mut state,
            one_hunk("a.txt", "@@ AAA @@", &[(LineOrigin::Addition, "AAALINE")]),
        );

        // b.txt へ移って開き直す。まだ届いていない。
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Esc)));
        let _ = update(&mut state, Msg::Key(Key::char('j')));
        let cmd = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));
        assert!(matches!(cmd, Cmd::LoadDiff(_)), "{cmd:?}");

        let text = draw(&state, SIZE).join("\n");

        assert!(!text.contains("AAALINE"), "古い diff が残っている: {text}");
        assert!(
            !text.contains("Diff: a.txt"),
            "古いタイトルが残っている: {text}"
        );
        assert!(text.contains("Diff: b.txt"), "{text}");
        assert!(
            pane_text(&state, SIZE, Pane::Diff).contains("読み込み中"),
            "{text}"
        );
    }

    #[test]
    fn reopening_the_same_file_keeps_showing_it_while_it_reloads() {
        // 同じ対象なら鮮度は保たれているので「読み込み中…」を挟まない。
        let mut state = state_with_diff(one_hunk(
            "a.txt",
            "@@ AAA @@",
            &[(LineOrigin::Addition, "AAALINE")],
        ));

        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Esc)));
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));

        let text = draw(&state, SIZE).join("\n");
        assert!(text.contains("AAALINE"), "{text}");
    }

    #[test]
    fn a_tab_is_opened_to_the_tab_stop() {
        // 1 個の空白に潰すとインデントが崩れて、上下の行の対応が読めなくなる。
        // 桁は本文の先頭から数える（記号を含めると変更行と文脈行で 1 桁ずれる）。
        let state = state_with_diff(one_hunk(
            "a.txt",
            "@@ -1 +1 @@",
            &[
                (LineOrigin::Context, "\tone"),
                (LineOrigin::Addition, "\ttwo"),
            ],
        ));

        let lines = draw(&state, SIZE);
        let row = |needle: &str| lines[row_of(&lines, needle) as usize].clone();

        // 枠(1) + 記号(1) + タブ 8 桁。**枠の字では比べない** —— フォーカスの強調で
        // 線の種類が変わる（07 の「ペインとフォーカス」）のはこのテストの関心ではない。
        assert_eq!(col_of(&row("one"), "one"), 10, "{:?}", row("one"));
        assert_eq!(col_of(&row("two"), "two"), 10, "{:?}", row("two"));
        assert_eq!(col_of(&row("two"), "+"), 1, "{:?}", row("two"));
    }

    #[test]
    fn a_carriage_return_does_not_reach_the_screen() {
        // 落とさないと CRLF のファイルで制御文字が画面に出る。
        let state = state_with_diff(one_hunk(
            "a.txt",
            "@@ -1 +1 @@",
            &[(LineOrigin::Addition, "crlf\r")],
        ));

        let lines = draw(&state, SIZE);
        let row = &lines[row_of(&lines, "crlf") as usize];

        assert!(!row.contains('\r'), "{row:?}");
    }

    #[test]
    fn a_long_line_is_cut_instead_of_wrapped() {
        // 折り返すと、どの行がどの行に対応するのか読めなくなる（07）。
        let long = "x".repeat(200);
        let state = state_with_diff(one_hunk(
            "a.txt",
            "@@ -1,2 +1,2 @@",
            &[(LineOrigin::Addition, &long), (LineOrigin::Context, "TAIL")],
        ));

        let lines = draw(&state, SIZE);

        // 長い行の次の行がそのまま来る。折り返していれば 2 行以上ずれる。
        assert_eq!(
            row_of(&lines, "TAIL"),
            row_of(&lines, "xxx") + 1,
            "{lines:?}"
        );
    }

    #[test]
    fn the_title_shows_both_names_of_a_rename() {
        let mut renamed = fake_file_diff("new.txt", vec![fake_hunk("@@ -1 +1 @@", &[])]);
        renamed.old_path = Some("old.txt".into());
        renamed.status = FileStatus::Renamed;

        let state = state_with_diff(vec![renamed]);

        let text = draw(&state, SIZE).join("\n");
        assert!(text.contains("Diff: old.txt -> new.txt"), "{text}");
    }

    #[test]
    fn a_binary_file_says_so() {
        // バイナリと「mode だけの変更」はどちらもハンクが空になる。
        let mut binary = fake_file_diff("a.png", Vec::new());
        binary.binary = true;

        let state = state_with_diff(vec![binary]);

        let text = draw(&state, SIZE).join("\n");
        assert!(text.contains("バイナリ"), "{text}");
    }

    #[test]
    fn a_mode_only_change_is_not_called_binary() {
        let state = state_with_diff(vec![fake_file_diff("a.txt", Vec::new())]);

        let text = draw(&state, SIZE).join("\n");
        assert!(text.contains("mode だけの変更"), "{text}");
    }

    #[test]
    fn an_empty_file_is_not_called_a_mode_change() {
        // `touch` しただけのファイルもハンクが 0 個になる。一括りに「mode だけの
        // 変更」と出すと、mode を触っていないファイルに mode の話をすることになる。
        let mut empty = fake_file_diff("empty.txt", Vec::new());
        empty.status = FileStatus::Untracked;

        let state = state_with_diff(vec![empty]);

        let text = draw(&state, SIZE).join("\n");
        assert!(text.contains("空のファイル"), "{text}");
        assert!(!text.contains("mode"), "{text}");
    }

    #[test]
    fn a_rename_without_content_changes_says_so() {
        let mut renamed = fake_file_diff("new.txt", Vec::new());
        renamed.old_path = Some("old.txt".into());
        renamed.status = FileStatus::Renamed;

        let state = state_with_diff(vec![renamed]);

        let text = draw(&state, SIZE).join("\n");
        assert!(text.contains("名前が変わっただけ"), "{text}");
    }

    #[test]
    fn a_pending_diff_reads_as_loading() {
        // 起動直後に「読み込めませんでした」と出すと、まだ読んでいないだけなのに
        // 失敗したように見える。
        let state = state_showing_diff(one_file());

        // **このペインの中だけを見る。** 画面全体を見ると、まだ届いていない
        // ブランチ一覧が出した同じ文言で通ってしまう（`ui::LOADING` は共有）。
        let text = pane_text(&state, SIZE, Pane::Diff);

        assert!(text.contains("読み込み中"), "{text}");
        // 届く前でも、何の diff を待っているかは出す。
        assert!(text.contains("Diff: a.txt"), "{text}");
    }

    /// コミットを選び、`Enter` を 2 回で Diff ペインまで入った状態。
    ///
    /// **詳細もファイル一覧も届けない。** グラフの `Enter` は `right_pane()` へ移るだけ
    /// で詳細を待たないし、**コミット側は一覧が届く前でも `Diff` に入れる**（届いたら
    /// 頼み直す経路が別にある。01 の「コミットのファイル一覧」）。この状態は実物で踏める。
    fn commit_diff_pending() -> AppState {
        let mut state = state_on_first_commit(&["subject"]);
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));
        assert_eq!(state.focus, Pane::Diff, "Diff ペインに入れていません");
        state
    }

    #[test]
    fn a_commit_waiting_for_its_file_list_reads_as_loading() {
        // **「行が選ばれていません」ではない。** 一覧が届けば対象が決まるので、
        // 待っているだけ（01 の「コミットのファイル一覧」）。
        let state = commit_diff_pending();

        let text = pane_text(&state, SIZE, Pane::Diff);

        assert!(text.contains("読み込み中"), "{text}");
    }

    #[test]
    fn a_commit_without_files_uses_the_same_words_as_the_list() {
        // **一覧が決まらない理由は一覧の側が持っている**（`ui::files_placeholder`）。
        // 空（`--allow-empty`）と失敗を「読み込み中」に丸めない。
        for (result, expected) in [
            (Ok(Vec::new()), ui::NO_CHANGES),
            (
                Err(GitReadError::NonUtf8Head),
                "ファイル一覧を読み込めませんでした",
            ),
        ] {
            let mut state = commit_diff_pending();
            deliver_commit_files(&mut state, result);

            // **このペインの中だけを見る。** 文言は CommitDetail ペインも出すので、
            // 画面全体で見ると隣のペインが出した字で通る（#40）。
            let text = pane_text(&state, SIZE, Pane::Diff);

            assert!(text.contains(expected), "{text}");
        }
    }

    #[test]
    fn a_conflicted_row_does_not_swap_the_graph_for_an_empty_diff() {
        // **conflicted は 2 者の diff で表せない**（02）ので対象が決まらない。`Enter` は
        // 断られ、左はグラフのまま。以前はここで空の Diff がグラフを覆っていた。
        //
        // **`state_showing_diff` は使わない。** 押した結果が「開かない」ことを見る
        // テストなので、開いた状態を名乗るヘルパを通すと名前が中身と逆になる。
        let mut state = state_with_status(WorktreeSnapshot {
            conflicted: vec![fake_entry("c.txt", FileStatus::Conflicted)],
            ..WorktreeSnapshot::default()
        });
        state.focus = Pane::Worktree;
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("Graph"), "{text}");
        assert!(!text.contains("Diff"), "{text}");
    }

    #[test]
    fn a_failed_diff_says_so() {
        let mut state = state_showing_diff(one_file());
        let target = state.wanted_diff().unwrap();
        let cmd = update(
            &mut state,
            Msg::DiffLoaded {
                target,
                result: Box::new(Err(mewgit_core::git_read::GitReadError::NonUtf8Head)),
            },
        );
        assert_eq!(cmd, Cmd::None);

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("読み込めませんでした"), "{text}");
    }

    /// stash が 2 つ届いた一覧で `stash@{1}` を選び、`Enter` で左に diff を開いて
    /// `files` を届けた状態（01 の「Stash ペイン」）。
    fn state_showing_stash(files: Vec<FileDiff>) -> AppState {
        let mut state = state_at("/repo");
        with_stashes(&mut state, Ok(vec![fake_stash(0), fake_stash(1)]));
        tab_to(&mut state, Pane::Stash);
        let _ = update(&mut state, Msg::Key(Key::char('j')));
        let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));
        assert_eq!(state.focus, Pane::Diff);
        deliver(&mut state, files);
        state
    }

    #[test]
    fn a_stash_diff_names_the_stash_and_heads_even_a_single_file() {
        // タイトルが `stash@{n}` を名乗るので、1 ファイルでも見出しを挟まないと
        // どのファイルの差分かが画面のどこにも無い（07 の「Diff ペイン」）。
        let state = state_showing_stash(one_hunk(
            "a.txt",
            "@@ -1 +1 @@",
            &[(LineOrigin::Addition, "new")],
        ));

        let text = pane_text(&state, SIZE, Pane::Diff);

        assert!(text.contains("Diff: stash@{1}"), "{text}");
        assert!(text.contains("a.txt"), "{text}");
        assert!(text.contains("+new"), "{text}");
    }

    #[test]
    fn a_stash_diff_heads_every_file() {
        let state = state_showing_stash(vec![
            fake_file_diff("a.txt", vec![fake_hunk("@@ -1 +1 @@", &[])]),
            fake_file_diff("new.txt", vec![fake_hunk("@@ -0,0 +1 @@", &[])]),
        ]);

        let lines = draw(&state, SIZE);

        assert!(
            row_of(&lines, "a.txt") < row_of(&lines, "new.txt"),
            "{lines:?}"
        );
    }
}
