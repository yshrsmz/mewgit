//! アカウント選択モーダルの描画。設計は `docs/design/07-ui.md` の
//! 「AccountPicker モーダル」と 04 の「内訳を出すのは `Modal::AccountPicker`」。
//!
//! **箱の中の割り方は `modals::picker` と共有する**（`RemotePicker` と 2 人）。
//! ここに残るのはタイトル・キーの案内・本文・行の組み立て。

use mewgit_core::auth::{gh, AttemptFailure, DegradedReason, GhLogin, LoginState, ProbeAttempt};
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use crate::modals::picker::{self, Picker};

/// 選ばせるときの枠のタイトル。
const TITLE_PICK: &str = "GitHub アカウントを選ぶ";

/// 内訳を出すだけのときの枠のタイトル。**「選ぶ」と書かない** —— 選べる相手がいない。
const TITLE_SHOW: &str = "GitHub アカウント";

/// キーの案内。`update::account_picker_key` が受けるキーと揃える。
const HINT_PICK: &str = "[j/k] 選ぶ   [Enter] 保存   [Esc] やめる";

/// 選べないときの案内。**`j/k` も `Enter` も書かない** —— どれも効かない。
const HINT_SHOW: &str = "[Esc] 閉じる";

/// 保存先。**黙って `.git/config` を変えない**（01 の「アカウントの選び直し」）。
const SAVES_TO: &str = "選ぶと .git/config の mewgit.user に保存します";

/// アカウント解決の内訳を出し、選べるなら選ばせる。
pub fn render(frame: &mut Frame, area: Rect, reason: &DegradedReason, cursor: usize) {
    // **選べるかどうかは 1 つの述語から出す。** タイトル・案内・行を別々に見ると、
    // 片方だけが「選べる」ふりをする（07）。
    let candidates = reason.candidates();
    let pick = !candidates.is_empty();

    picker::render(
        frame,
        area,
        Picker {
            title: if pick { TITLE_PICK } else { TITLE_SHOW },
            body: &body(reason),
            rows: candidates.iter().map(candidate_line).collect(),
            cursor,
            hint: if pick { HINT_PICK } else { HINT_SHOW },
        },
    );
}

/// 本文。1 行目が何が起きているかで、以降が内訳と直し方。
///
/// **`ui::degraded`（ヘッダの 1 行）は呼べない。** あちらは `[gh: …]` の中に置く
/// 前提で主語が `gh` なので、単体で読むと文にならない（`GhMissing` は「ありません」）。
/// 04 の表がヘッダとここの分担を持っている。
///
/// **内訳を一覧側に置かない**のは、一覧が折り返さないため —— `Unusable` の
/// `message` は 1 行に収まらず、このモーダルが在る理由そのものが右端で切れる（07）。
fn body(reason: &DegradedReason) -> String {
    let lines: Vec<String> = match reason {
        DegradedReason::GhMissing => vec![
            "gh が見つかりません".to_string(),
            "PATH に gh を入れるか、設定の general.gh_bin にパスを書いてください".to_string(),
        ],
        DegradedReason::GhTooOld { found } => vec![
            format!("gh {found} は古すぎます（{} 以上が必要です）", gh::REQUIRED),
            "gh を更新してください".to_string(),
        ],
        // 畳んだ 3 つ（起動できない / 非 0 終了 / 解釈できない）を見分ける手掛かりが
        // `message`。**ヘッダに出せなかったのがこれ**（04）。
        DegradedReason::Unusable { message } => {
            vec!["gh を実行できません".to_string(), message.clone()]
        }
        DegradedReason::NotLoggedIn { host } => vec![
            format!("{host} にログインしていません"),
            format!("gh auth login --hostname {host} でログインしてください"),
        ],
        DegradedReason::ConfiguredUnavailable { login, message } => vec![
            format!("設定が指す {login} のトークンがありません"),
            message.clone(),
            ".git/config の mewgit.user か、設定ファイルの [[repos]] を直してください".to_string(),
        ],
        // **「権限が無い」と読ませない。** 全部が「確かめられなかった」形もここに来る
        // （04）ので、1 件ずつの理由をそのまま並べる。
        DegradedReason::NoAccess { attempts } => {
            let mut lines = vec![format!(
                "届いたアカウントがありません（{} 件）",
                attempts.len()
            )];
            lines.extend(attempts.iter().map(attempt_line));
            lines
        }
        // **外での直し方を書かない。** 選ばせるのはこのモーダル自身で、書くと
        // 「いま提供している操作を外でやれ」になる（#65）。
        DegradedReason::Ambiguous { candidates } => vec![
            format!("アカウントを 1 つに絞れません（{} 件）", candidates.len()),
            SAVES_TO.to_string(),
        ],
    };
    lines.join("\n")
}

/// `NoAccess` の 1 件。
fn attempt_line(attempt: &ProbeAttempt) -> String {
    let ProbeAttempt { login, failure } = attempt;
    match failure {
        AttemptFailure::Denied { status } => format!("{login}: HTTP {status} で断られました"),
        // **「断られた」と混ぜない。** もう一度なら通りうる（04）。
        AttemptFailure::Undetermined { message } => {
            format!("{login}: 確かめられませんでした（{message}）")
        }
        AttemptFailure::NoToken { message } => {
            format!("{login}: トークンを取れませんでした（{message}）")
        }
    }
}

/// 選ばせる 1 行。**選ぶ材料を出す** —— どれが gh の現在のアカウントか、
/// どのスコープを持っているか、gh の検証が通ったか。
///
/// `state` を出すのは、[`resolve`] が**それで候補を落とさない**と決めてあるため
/// （04 の「`LoginState` で候補を絞らない」）。落とさない代わりに、選ぶ人に見せる。
///
/// [`resolve`]: mewgit_core::auth::resolve
fn candidate_line(login: &GhLogin) -> Line<'static> {
    // `*` は gh の active なアカウント。Branches ペインの HEAD と同じ印。
    let mark = if login.active { "* " } else { "  " };
    let scopes = if login.scopes.is_empty() {
        "scopes なし".to_string()
    } else {
        login.scopes.join(", ")
    };
    let state = match login.state {
        LoginState::Success => "",
        LoginState::Timeout => "  gh: 応答なし",
        LoginState::Error => "  gh: 拒否",
    };
    Line::from(format!("{mark}{}  {scopes}{state}", login.login))
}

#[cfg(test)]
mod tests {
    use crate::test_util::{draw, row_is_reversed, state_with_modal};
    use mewgit_core::app::{AppState, Modal, TermSize};
    use mewgit_core::auth::{AttemptFailure, DegradedReason, LoginState, ProbeAttempt};
    use mewgit_core::test_util::fake_login;

    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    fn state_with_reason(reason: DegradedReason) -> AppState {
        state_with_modal(Modal::AccountPicker { reason, cursor: 0 })
    }

    fn ambiguous(logins: Vec<mewgit_core::auth::GhLogin>) -> DegradedReason {
        DegradedReason::Ambiguous { candidates: logins }
    }

    #[test]
    fn choosing_shows_the_candidates_the_target_file_and_the_keys() {
        let mut active = fake_login("taro_acme");
        active.active = true;
        active.scopes = vec!["repo".to_string(), "read:org".to_string()];
        let mut plain = fake_login("taro");
        plain.scopes = Vec::new();
        let state = state_with_reason(ambiguous(vec![active, plain]));

        let text = draw(&state, SIZE).join("\n");

        assert!(
            text.contains("アカウントを 1 つに絞れません（2 件）"),
            "{text}"
        );
        // **保存先を出す。** `.git/config` を黙って変えない（01）。
        assert!(text.contains(".git/config"), "{text}");
        assert!(text.contains("* taro_acme  repo, read:org"), "{text}");
        assert!(text.contains("scopes なし"), "{text}");
        assert!(text.contains("[Enter] 保存"), "{text}");
    }

    #[test]
    fn a_login_that_gh_could_not_verify_says_so() {
        // **`resolve` は `LoginState` で候補を落とさない**（04）。落とさない代わりに、
        // 選ぶ人に見せる。
        let mut timed_out = fake_login("taro");
        timed_out.state = LoginState::Timeout;
        let state = state_with_reason(ambiguous(vec![fake_login("taro_acme"), timed_out]));

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("gh: 応答なし"), "{text}");
    }

    #[test]
    fn the_cursor_row_is_highlighted() {
        let state = state_with_modal(Modal::AccountPicker {
            reason: ambiguous(vec![fake_login("taro_acme"), fake_login("taro")]),
            cursor: 1,
        });

        assert!(
            row_is_reversed(&state, SIZE, "  taro  repo"),
            "カーソル行が反転していない"
        );
        assert!(
            !row_is_reversed(&state, SIZE, "taro_acme"),
            "選んでいない行が反転している"
        );
    }

    #[test]
    fn nothing_reached_the_repository_lists_the_reasons_and_offers_no_choice() {
        // **ヘッダは件数しか出せない**（1 行なので。07）。1 件ずつ並べるのがここ（04）。
        let state = state_with_reason(DegradedReason::NoAccess {
            attempts: vec![
                ProbeAttempt {
                    login: "taro_acme".to_string(),
                    failure: AttemptFailure::Denied { status: 404 },
                },
                ProbeAttempt {
                    login: "taro".to_string(),
                    failure: AttemptFailure::Undetermined {
                        message: "接続できませんでした".to_string(),
                    },
                },
            ],
        });

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("taro_acme: HTTP 404"), "{text}");
        // **「断られた」と混ぜない。** もう一度なら通りうる（04）。
        assert!(text.contains("確かめられませんでした"), "{text}");
        // 選べないのに選ぶキーを出さない。
        assert!(text.contains("[Esc] 閉じる"), "{text}");
        assert!(!text.contains("[Enter]"), "{text}");
    }

    #[test]
    fn the_message_the_header_cannot_show_lands_here() {
        // ヘッダは「実行できません」の 1 行だけ（07）。**畳んだ 3 つを見分ける
        // 手掛かりが `message`** で、出す先はここだと 04 が決めている。
        let state = state_with_reason(DegradedReason::Unusable {
            message: "gh の出力を解釈できませんでした: unexpected end of JSON".to_string(),
        });

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("unexpected end of JSON"), "{text}");
    }

    #[test]
    fn a_reason_that_only_the_outside_can_fix_says_how() {
        // **指示を足すのは、その操作を持たない側**（#65）。mewgit は `gh auth login` を
        // 提供していないので、ここに書く。
        let state = state_with_reason(DegradedReason::NotLoggedIn {
            host: "ghe.example.com".to_string(),
        });

        let text = draw(&state, SIZE).join("\n");

        assert!(
            text.contains("gh auth login --hostname ghe.example.com"),
            "{text}"
        );
    }

    #[test]
    fn the_cursor_row_stays_visible_when_the_list_is_taller_than_the_box() {
        // 見えていない候補を `Enter` で保存しない（`RemotePicker` と同じ守り。
        // **産出元が 2 つになったので両方で数える**）。
        let logins: Vec<_> = (0..30)
            .map(|i| fake_login(&format!("login{i:02}")))
            .collect();
        let state = state_with_modal(Modal::AccountPicker {
            reason: ambiguous(logins),
            cursor: 29,
        });

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("login29"), "{text}");
    }

    #[test]
    fn a_body_that_fills_the_box_is_not_cut_by_the_empty_list() {
        // **一覧が空のときは縦に割らない**（`picker`）。割ると `Min(1)` が 1 行取り、
        // 箱が端末の高さに丸められた回に**本文の最後の行が消える** —— 消えるのは
        // 直し方の行（いちばん下）なので、このモーダルが在る理由が欠ける。
        // **境目は実測で 80x9（内訳 5 件）。** それより高いと `place` が空ける余白から
        // 1 行を取るだけなので**同じ内訳でも踏めない**（80x10 では直す前も通る）。
        let attempts: Vec<_> = (0..5)
            .map(|i| ProbeAttempt {
                login: format!("login{i:02}"),
                failure: AttemptFailure::Denied { status: 404 },
            })
            .collect();
        let state = state_with_reason(DegradedReason::NoAccess { attempts });

        let text = draw(
            &state,
            TermSize {
                width: 80,
                height: 9,
            },
        )
        .join("\n");

        assert!(text.contains("login04"), "{text}");
    }

    #[test]
    fn a_tiny_terminal_does_not_panic() {
        // リサイズの途中では枠より小さい端末が来る。**選べない側も回す** ——
        // 一覧が空のときだけ通る腕がある（`picker` の縦割りを飛ばす）。
        for (width, height) in [(1, 1), (10, 3), (20, 5)] {
            for reason in [
                ambiguous(vec![fake_login("taro")]),
                DegradedReason::GhMissing,
            ] {
                let state = state_with_reason(reason);
                let lines = draw(&state, TermSize { width, height });
                assert_eq!(lines.len(), height as usize);
            }
        }
    }
}
