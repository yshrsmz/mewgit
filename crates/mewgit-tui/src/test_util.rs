//! テスト用のヘルパ。`mewgit-tui` は 1 つのバイナリクレートなので、単体テストは
//! すべて同じコンパイル単位に入る。feature で切り分ける必要はない。

use mewgit_core::app::{update, AppState, Cmd, Key, KeyCode, Modal, Msg, Pane, RepoInfo, TermSize};
use mewgit_core::config::Config;
use mewgit_core::git_read::{Commit, FileEntry, GitReadError, LogPage, WorktreeSnapshot};
use mewgit_core::test_util::{fake_branch, fake_commit, fake_commit_with_parents, reply};
use ratatui::backend::TestBackend;
use ratatui::layout::Position;
use ratatui::style::Modifier;
use ratatui::text::Line;
use ratatui::Terminal;

use crate::ui;

/// 描画結果を行ごとの文字列にする。
///
/// `TestBackend` の `Display` を使う。自前でセルを走査すると全角文字の 2 セル目が
/// 空白として混ざる（「新しい通知」→「新 し い 通 知」）が、`Display` は幅を見て
/// 正しく詰めてくれる。スタイルを見たいときは [`terminal_with`] を使う。
///
/// `Display` は 1 行を `"` で囲むので外す。付いたままだと文字列上の桁が
/// バッファの x 座標と 1 つずれ、桁を見るテストが読みにくくなる。
pub fn draw(state: &AppState, size: TermSize) -> Vec<String> {
    terminal_with(state, size)
        .backend()
        .to_string()
        .lines()
        .map(|line| unquote(line).to_string())
        .collect()
}

/// 囲みの `"` と、そのうしろの注記を落とす。
///
/// 全角文字があると `TestBackend` は隠れたセルの一覧を閉じ `"` のあとに足す
/// （`"…" Hidden by multi-width symbols: [(1, " ")]`）。注記の中にも `"` が
/// 入るので、末尾から探さずに注記の開始で切る。
fn unquote(line: &str) -> &str {
    let body = line.strip_prefix('"').unwrap_or(line);
    match body.split_once(HIDDEN_CELLS) {
        Some((head, _)) => head,
        None => body.strip_suffix('"').unwrap_or(body),
    }
}

/// `TestBackend` が隠れたセルを報告するときの区切り。閉じ `"` を含む。
const HIDDEN_CELLS: &str = "\" Hidden by multi-width symbols:";

/// 描いた通知の行（フッターの 1 行目）。
///
/// **取り出しはここ 1 本。** `height - 2` を各テストが持つと、フッターの高さや置き場所を
/// 変えたときに一斉に嘘になる。**行番号は `ui::footer` から出す**（`ui::pane_area` と
/// 同じ理由）。**`ui` の `mod tests` ではなくここに置く** —— `runtime` のテストも
/// 通知の行を見る（テストの共有先は `test_util`。`CLAUDE.md`）。
pub fn notice_bar(state: &AppState, size: TermSize) -> String {
    footer_row(state, size, 0)
}

/// 描いたキーヒントの行（フッターの 2 行目）。
pub fn hint_bar(state: &AppState, size: TermSize) -> String {
    footer_row(state, size, 1)
}

fn footer_row(state: &AppState, size: TermSize, row: usize) -> String {
    // **その行が在ることまで見る。** 低い端末ではフッターに 2 行渡らず、通知の行が
    // 0 行になる（`ui::split` が足りないぶんを通知から削る）。`y` だけを信じると
    // **別の行を通知として読む**か、範囲外で理由の無い panic になる。
    let area = ui::footer(size)[row];
    assert!(
        area.height > 0,
        "{size:?} にはフッターの {row} 行目が無い（低すぎる端末）"
    );
    draw(state, size)[area.y as usize].clone()
}

/// `needle` を含む行が何行目か。無ければ panic する。
///
/// 桁や行を直に書かない。レイアウトを変えたときにテストだけ古い座標を持つのを避ける
/// （[`graph_rows`] と同じ理由）。
pub fn row_of(lines: &[String], needle: &str) -> u16 {
    lines
        .iter()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle} を含む行がありません: {lines:?}")) as u16
}

/// 描いた行のなかで `needle` が始まる**桁**。
///
/// **バイト位置ではなく桁で測る。** 枠の `│` は 3 バイトなので、`str::find` の返す
/// 添字をそのまま桁として使うと**枠 1 本につき 2 ずれる**。編集中のモーダルの
/// カーソルを見張るテストがどれも同じ式を書いていたので、4 本目（`PrCreate` の
/// 2 欄）が出た時点でここへ移した。
///
/// [`row_of`] と対で使う（あちらが行、こちらが桁）。
pub fn col_of(line: &str, needle: &str) -> u16 {
    let head = &line[..line
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} がありません: {line:?}"))];
    u16::try_from(Line::from(head).width()).unwrap_or(u16::MAX)
}

/// 1 フレーム描いた `TestBackend` を返す。バッファのスタイルを見るとき用。
pub fn terminal_with(state: &AppState, size: TermSize) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(size.width, size.height)).unwrap();
    terminal.draw(|frame| ui::render(frame, state)).unwrap();
    terminal
}

/// 端末カーソルの位置。
///
/// **編集中のモーダルは端末側のカーソルを出す**（反転の升目を自前で描かない。07）ので、
/// どこに置いたかはバッファからは見えない。`CommitMessage` と `Input` の 2 人目が
/// 出た時点でここへ移した。
pub fn cursor(state: &AppState, size: TermSize) -> Position {
    terminal_with(state, size)
        .get_cursor_position()
        .expect("カーソル位置を取れませんでした")
}

/// `needle` を含む行が反転しているか（カーソル行の見張り）。
///
/// **描いて座標を突き合わせるところまでが 1 つ。** `terminal_with` と `row_of` を
/// 各ペインで組み合わせると、反転の見方（どのセルを見るか）がペインの数だけ増える。
/// 行の右側は余白でスタイルが付かないので、1 セルでも反転していれば真とする。
pub fn row_is_reversed(state: &AppState, size: TermSize, needle: &str) -> bool {
    let row = row_of(&draw(state, size), needle);
    let terminal = terminal_with(state, size);
    let buffer = terminal.backend().buffer();
    (0..size.width).any(|x| buffer[(x, row)].modifier.contains(Modifier::REVERSED))
}

/// `needle` を含む行が、**そのペインの中で**反転しているか。
///
/// **[`row_is_reversed`] は行の全幅を見る。** 右列のペインでは 1 行目が左のグラフの
/// カーソル行（反転している）と同じ端末行に来るので、**反転していなくても真になる**。
/// 左のペインを使う見張りは全幅で足りるが、右列では桁も絞らないと空振りする。
pub fn pane_row_is_reversed(state: &AppState, size: TermSize, pane: Pane, needle: &str) -> bool {
    let area = ui::pane_area(pane, size, state);
    let row = area.y
        + row_of(
            &pane_text(state, size, pane)
                .lines()
                .map(String::from)
                .collect::<Vec<_>>(),
            needle,
        );
    let terminal = terminal_with(state, size);
    let buffer = terminal.backend().buffer();
    (area.x..area.right()).any(|x| buffer[(x, row)].modifier.contains(Modifier::REVERSED))
}

/// そのペインの中で `needle` を含む 1 行。無ければ panic する。
///
/// **[`row_of`] と対にする**（あちらは索引、こちらは本文）。行の本文を見る見張りは
/// `pane_text(..).lines().find(..).expect(..)` を各テストが書き写すことになり、
/// 見つからなかったときの文言がテストの数だけ散る。
pub fn pane_row(state: &AppState, size: TermSize, pane: Pane, needle: &str) -> String {
    let text = pane_text(state, size, pane);
    text.lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{pane:?} に {needle} を含む行がありません:\n{text}"))
        .to_string()
}

/// そのペインの領域だけを取り出した描画結果。
///
/// **画面全体の `contains` で見ない。** 文言はペインをまたいで共有しているので
/// （`ui::LOADING` はどのペインも出す）、全体を見ると**別のペインが出した同じ
/// 文字で通ってしまう**。Branches ペインを足した時点で、Worktree と Diff の
/// 「読み込み中…」の見張りが実際に死んだ（`draw` の全画面 `contains` が、まだ届いて
/// いないブランチ一覧の文言を拾っていた）。
///
/// **全角文字の 2 セル目は読み飛ばす。** ratatui は widget が覆ったセルを
/// `Cell::reset()`（= 空白 1 つ）に戻すので、素直に並べると「日 本 語」になり、
/// `contains` を見る見張りが静かに素通りする（`ratatui-core` の
/// `Buffer::set_stringn` で確認）。幅は ratatui 自身の `Line::width` から取る。
pub fn pane_text(state: &AppState, size: TermSize, pane: Pane) -> String {
    let area = ui::pane_area(pane, size, state);
    let terminal = terminal_with(state, size);
    let buffer = terminal.backend().buffer();

    let mut rows = Vec::with_capacity(area.height as usize);
    for y in area.y..area.bottom() {
        let mut row = String::new();
        let mut x = area.x;
        while x < area.right() {
            let symbol = buffer[(x, y)].symbol();
            row.push_str(symbol);
            x += u16::try_from(Line::raw(symbol).width()).unwrap_or(1).max(1);
        }
        rows.push(row);
    }
    rows.join("\n")
}

/// `runtime::layout` がその状態に入れるのと同じグラフの行数。
///
/// ランタイムのループを回さないテストが `viewport` を用意するのに使う。判定を
/// 手で書くと、レイアウトの決め方が変わったときにテストだけ古い値を持つ。
pub fn graph_rows(state: &AppState) -> usize {
    ui::graph_rows(state.term)
}

/// パスだけ入った状態。
pub fn state_at(path: &str) -> AppState {
    AppState::new(
        RepoInfo {
            path: path.into(),
            ..RepoInfo::default()
        },
        Config::default(),
    )
}

/// 与えた summary のコミットを新しい順に積んだ状態。親は持たない。
pub fn state_with_commits(summaries: &[&str]) -> AppState {
    let commits = summaries
        .iter()
        .enumerate()
        .map(|(index, summary)| fake_commit(index as u8, summary))
        .collect();
    state_with_log(commits)
}

/// 1 件目のコミットを選んだ状態。詳細はまだ届いていない。
///
/// 起動直後のカーソルは WIP 行なので、`j` を 1 回打って乗せる。**`cursor = 1` を
/// 直に書かない。** 表示行の構成（先頭が WIP 行）を知る場所を増やすと、構成を
/// 変えたときに片方だけ古くなる。`update` を通すので `JobTracker` も本番と同じ。
pub fn state_on_first_commit(summaries: &[&str]) -> AppState {
    let mut state = state_with_commits(summaries);
    let _ = update(&mut state, Msg::Key(Key::char('j')));
    state
}

/// ワーキングツリーの一覧が届いた状態。
///
/// **発行してから届ける。** 通さない返信は刻印を持たず、`status_loaded` が
/// 「書き込みより前に頼まれたもの」として頼み直す（01 の「実行中ジョブ › 世代」）。
pub fn state_with_status(snapshot: WorktreeSnapshot) -> AppState {
    let mut state = state_at("/repo");
    let _ = reply(
        &mut state,
        Cmd::LoadStatus,
        Msg::StatusLoaded(Box::new(Ok(snapshot))),
    );
    state
}

/// ワーキングツリーを読み、`Enter` で Diff ペインを開いた状態。
///
/// **`AppState` に直に書かず `Enter` を通す。** `focus` と `wanted_diff` が実際の経路で
/// 揃った形を描くため。`focus = Pane::Diff` を直に置くと、対象の無い状態
/// （`focus_candidates` が候補から外す形。01）を作れてしまい、実物では出ない画面を
/// 見張ることになる。
pub fn state_showing_diff(snapshot: WorktreeSnapshot) -> AppState {
    let mut state = state_with_status(snapshot);
    state.focus = Pane::Worktree;
    let _ = update(&mut state, Msg::Key(Key::new(KeyCode::Enter)));
    // **開けたことを確かめる。** `Enter` は差分で表せる行が無ければ断られるので、
    // 見ないと「Diff を出した状態」を名乗りながら Worktree のままの状態を返しうる。
    assert_eq!(state.focus, Pane::Diff, "Diff ペインが開いていません");
    state
}

/// コミットのファイル一覧の返信を届ける。**成功も失敗もここを通す。**
///
/// oid はカーソルの指すコミットから取る。手で書くと、選ぶ行を変えたときに返信が
/// 黙って別のコミットのものになり、テストが待ち状態のまま緑になる。
pub fn deliver_commit_files(state: &mut AppState, result: Result<Vec<FileEntry>, GitReadError>) {
    let oid = state
        .graph
        .cursor_commit()
        .expect("コミット行を選んでいません")
        .oid;
    let _ = update(
        state,
        Msg::CommitFilesLoaded {
            oid,
            result: Box::new(result),
        },
    );
}

/// モーダルを出した状態。下に何か描かれていること（覆いのテスト）が要るので、
/// コミットを 2 件積んである。
pub fn state_with_modal(modal: Modal) -> AppState {
    let mut state = state_with_commits(&["first", "second"]);
    state.modal = Some(modal);
    state
}

/// `(索引, 親の索引)` の並びからコミットを組んで積んだ状態。並びは新しい順。
///
/// レーンのある行を描くのに使う。oid は索引から作る（`fake_oid`）。
pub fn state_with_history(history: &[(u8, &[u8])]) -> AppState {
    let commits = history
        .iter()
        .map(|(index, parents)| fake_commit_with_parents(*index, parents, "summary"))
        .collect();
    state_with_log(commits)
}

/// コミット列を 1 ページとして積んだ状態。
///
/// 実リポジトリを作らない。描画のテストが `git` の挙動で落ちないようにするため。
/// `viewport` は 24 行の端末で描くときのグラフの行数に合わせる。
fn state_with_log(commits: Vec<Commit>) -> AppState {
    let mut state = state_at("/repo");
    state.graph.viewport = graph_rows(&state);

    let page = LogPage {
        commits,
        done: true,
    };
    let count = state.config.graph.page_size;
    let _ = reply(
        &mut state,
        Cmd::LoadLog {
            count,
            restart: false,
        },
        Msg::LogLoaded {
            restart: false,
            result: Ok(page),
        },
    );
    // **ブランチ一覧も届いた状態にする。** ここが作るのは「読み込みが済んだ
    // リポジトリ」なので、一覧だけ「読み込み中…」で残っていると画面が実際とずれる。
    // （文言が他のペインのテストに当たるのは別の問題で、そちらは `pane_text` が
    // ペインの中だけを見ることで塞いである。）
    let _ = reply(
        &mut state,
        Cmd::LoadBranches,
        Msg::BranchesLoaded(Box::new(Ok(vec![fake_branch("main", true)]))),
    );
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_text_keeps_wide_characters_and_stops_at_the_pane_edge() {
        // 全角の 2 セル目が空文字であることに乗っている。混ざると「読 み 込 み」に
        // なり、`contains` を見る見張りが静かに素通りする。
        let state = state_with_commits(&["日本語のコミットメッセージ"]);

        let text = pane_text(&state, TermSize::default(), Pane::Graph);

        assert!(text.contains("日本語のコミットメッセージ"), "{text}");
        // 右のペインの中身は入らない。
        assert!(!text.contains("Working tree"), "{text}");
        assert!(!text.contains("Branches"), "{text}");
    }

    #[test]
    fn a_row_with_wide_characters_comes_back_without_the_quotes() {
        // 全角があると `TestBackend` は閉じ `"` のうしろに隠れたセルの一覧を足す。
        // 末尾から `"` を探すと、その注記の中の `"` に当たって外れない。
        let state = state_with_commits(&["日本語のコミットメッセージ"]);

        let lines = draw(&state, TermSize::default());

        // 枠の中の 1 行目は WIP 行なので、コミットはその 1 つ下（07）。
        assert!(!lines[3].contains('"'), "{:?}", lines[3]);
        assert!(!lines[3].contains("Hidden by"), "{:?}", lines[3]);
        assert!(lines[3].contains("日本語のコミットメッセージ"));
    }
}
