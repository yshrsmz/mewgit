//! アカウント選択モーダルの描画。設計は `docs/design/07-ui.md` の
//! 「AccountPicker モーダル」と 04 の「内訳を出すのは `Modal::AccountPicker`」。
//!
//! **箱の中の割り方は `modals::picker` と共有する**（`RemotePicker` と 2 人）。
//! ここに残るのはタイトル・キーの案内・本文・行の組み立て。

use mewgit_core::app::{AccountListing, AccountView, Repick};
use mewgit_core::auth::{
    gh, AccountSource, AttemptFailure, DegradedReason, GhLogin, LoginState, ProbeAttempt,
    MEWGIT_USER,
};
use ratatui::layout::Rect;
use ratatui::text::{Line, Text};
use ratatui::Frame;

use crate::modals::picker::{self, Picker};

/// 選ばせるときの枠のタイトル。
const TITLE_PICK: &str = "GitHub アカウントを選ぶ";

/// 内訳を出すだけのときの枠のタイトル。**「選ぶ」と書かない** —— 選べる相手がいない。
const TITLE_SHOW: &str = "GitHub アカウント";

/// キーの案内。`update::picker_key` と `update::account_picked` が受けるキーと揃える。
const HINT_PICK: &str = "[j/k] 選ぶ   [Enter] 保存   [Esc] やめる";

/// 選べないときの案内。**`j/k` も `Enter` も書かない** —— どれも効かない。
const HINT_SHOW: &str = "[Esc] 閉じる";

/// `u` の案内。**出すのは [`AccountView::is_unsettable`] が真のときだけ** ——
/// `update::account_key` が受けるかどうかを同じ述語から出す（#53）。
const HINT_UNSET: &str = "[u] 指定を解除";

/// 保存先。**黙って `.git/config` を変えない**（01 の「アカウントの選び直し」）。
///
/// **キーの名前は [`MEWGIT_USER`] から引く。** 書く側（`GitOp::SetAccount` の argv）
/// と読む側（`auth::resolve` の git2）が 1 本の定数を見ているのに、**画面だけが
/// literal だと、キーを変えたときにここが古い場所を案内する**（`.git/config` しか
/// 見ていないテストでは落ちない）。
fn saves_to() -> String {
    format!("選ぶと .git/config の {MEWGIT_USER} に保存します")
}

/// アカウント解決の内訳を出し、選べるなら選ばせる。
pub fn render(frame: &mut Frame, area: Rect, view: &AccountView, cursor: usize) {
    // **選べるかどうかは 1 つの述語から出す。** タイトル・案内・行を別々に見ると、
    // 片方だけが「選べる」ふりをする（07）。
    let candidates = view.candidates();
    let pick = !candidates.is_empty();
    // **`Repick` は待っているあいだも「選ぶ」。** 0.95 秒で候補が出るので、途中で
    // 枠のタイトルが書き換わるほうが読めない（07）。
    let title = match view {
        AccountView::Repick(_) => TITLE_PICK,
        AccountView::Degraded(_) if pick => TITLE_PICK,
        AccountView::Degraded(_) => TITLE_SHOW,
    };

    picker::render(
        frame,
        area,
        Picker {
            title,
            body: body(view),
            rows: candidates.iter().map(candidate_line).collect(),
            cursor,
            hint: &hint(view, pick),
        },
    );
}

/// キーの案内。**`u` は [`AccountView::is_unsettable`] が真のときだけ足す** ——
/// 出す条件と `update::account_key` が受ける条件が同じ 1 本の述語から出る（#53）。
fn hint(view: &AccountView, pick: bool) -> String {
    let base = if pick { HINT_PICK } else { HINT_SHOW };
    if view.is_unsettable() {
        format!("{HINT_UNSET}   {base}")
    } else {
        base.to_string()
    }
}

/// 本文。[`AccountView`] の 2 つで分かれる。
fn body(view: &AccountView) -> Text<'static> {
    match view {
        AccountView::Degraded(reason) => lines(degraded_body(reason)),
        AccountView::Repick(repick) => lines(repick_body(repick)),
    }
}

fn lines(lines: Vec<String>) -> Text<'static> {
    Text::from(lines.into_iter().map(Line::from).collect::<Vec<_>>())
}

/// 選び直しの本文。1 行目が「いま誰で、なぜそれになったか」。
///
/// **ヘッダの `[gh: login @ host]` は「なぜ」を落としている**（04 の表）ので、
/// `A` を押した人が最初に知りたいものがここにしか無い。
fn repick_body(repick: &Repick) -> Vec<String> {
    let Repick {
        login,
        host,
        source,
        logins,
    } = repick;
    let mut lines = vec![format!(
        "{login} @ {host} を使用（{}）",
        source_label(*source)
    )];
    match logins {
        // **待っていることを出す。** 出さないと、0.95 秒のあいだ「候補が 1 つも
        // 無い」と読める（`Failed` と同じ見た目になる）。
        AccountListing::Waiting => lines.push("候補を読み込んでいます…".to_string()),
        AccountListing::Listed(_) => lines.push(saves_to()),
        // **`gh` の失敗をここで出す。** `DegradedReason` に畳まないと決めてあるので
        // （01 の「候補の取り直し」）、`state.account` は `Resolved` のまま。
        AccountListing::Failed(message) => {
            lines.push("候補を読み込めませんでした".to_string());
            lines.push(message.clone());
        }
    }
    // **外せない側にだけ外での直し方を書く**（#65 の「指示を足すのは、その操作を
    // 持たない側」）。外せる側には `[u] 指定を解除` が出ているので書かない。
    if matches!(source, AccountSource::RepoRule) {
        lines.push("設定ファイルの [[repos]] は mewgit からは外せません".to_string());
    }
    lines
}

/// [`AccountSource`] を画面に出す語（04 の手順 1〜4）。
///
/// **明示指定の 2 つを別々に出す。** どちらを直せば変わるかが違ううえ、
/// `.git/config` のほうは `u` でその場で外せる（`AccountSource` の doc）。
fn source_label(source: AccountSource) -> &'static str {
    match source {
        AccountSource::GitConfig => ".git/config の mewgit.user",
        AccountSource::RepoRule => "設定ファイルの [[repos]]",
        AccountSource::Single => "ログインが 1 つだけ",
        AccountSource::Probe => "届いたのが 1 つだけ",
    }
}

/// degraded の本文。1 行目が何が起きているかで、以降が内訳と直し方。
///
/// **`ui::degraded`（ヘッダの 1 行）とは別の文言。同じ事実を 2 通りに書く。**
/// あちらは `[gh: …]` の中に置く前提で主語が `gh` なので単体では文にならず
/// （`GhMissing` は「ありません」）、しかも**リポジトリ名とスピナーと 1 行を
/// 分け合う**ので詰めてある（`どれも届きません（2 件）`）。ここは箱の中で折り返せる
/// ので、詰める理由が無い。**揃えると、ヘッダを 1 桁詰めるたびにこちらが痩せる。**
/// 分担は 04 の表にある。
///
/// **内訳を一覧側に置かない**のは、一覧が折り返さないため —— `Unusable` の
/// `message` は 1 行に収まらず、このモーダルが在る理由そのものが右端で切れる（07）。
fn degraded_body(reason: &DegradedReason) -> Vec<String> {
    match reason {
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
        // **どちらの指定かを出す。** `AccountSource` を運ぶようにしたのはこのため
        // （両方を並べる形は #73 で出荷して「選び直せない」を拾われた）。
        // **`GitConfig` なら `u` で外せる**ので、外での直し方は書かない
        // （#65 の「指示を足すのは、その操作を持たない側」）。
        DegradedReason::ConfiguredUnavailable {
            login,
            source,
            message,
        } => vec![
            format!("設定が指す {login} のトークンがありません"),
            message.clone(),
            match source {
                AccountSource::GitConfig => {
                    format!("{MEWGIT_USER} は .git/config にあります")
                }
                _ => "設定ファイルの [[repos]] を直してください".to_string(),
            },
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
            saves_to(),
        ],
    }
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
    use mewgit_core::app::{AccountListing, AccountView, AppState, Modal, Repick, TermSize};
    use mewgit_core::auth::{
        AccountSource, AttemptFailure, DegradedReason, LoginState, ProbeAttempt,
    };
    use mewgit_core::test_util::fake_login;

    const SIZE: TermSize = TermSize {
        width: 80,
        height: 24,
    };

    fn state_with_reason(reason: DegradedReason) -> AppState {
        state_with_modal(Modal::AccountPicker {
            view: AccountView::Degraded(reason),
            cursor: 0,
        })
    }

    /// 決まっているアカウントを選び直す画面。
    fn state_with_repick(source: AccountSource, logins: AccountListing) -> AppState {
        state_with_modal(Modal::AccountPicker {
            view: AccountView::Repick(Repick {
                login: "taro".to_string(),
                host: "github.com".to_string(),
                source,
                logins,
            }),
            cursor: 0,
        })
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
    fn the_repick_says_who_is_in_use_and_waits_for_the_candidates() {
        // **待っていることを出す。** 0.95 秒のあいだ空の一覧を出すと、「候補が
        // 1 つも無い」と読める（01 の「候補の取り直し」）。
        let state = state_with_repick(AccountSource::GitConfig, AccountListing::Waiting);

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("taro @ github.com を使用"), "{text}");
        assert!(text.contains(".git/config の mewgit.user"), "{text}");
        assert!(text.contains("候補を読み込んでいます"), "{text}");
        // **待っているあいだは選べない**ので `[Enter]` を出さない（候補が空）。
        assert!(!text.contains("[Enter]"), "{text}");
    }

    #[test]
    fn the_repick_offers_u_only_where_the_setting_can_be_removed() {
        // **案内と `update::account_key` が同じ述語（`AccountView::is_unsettable`）を
        // 読む。** 別々に見ると、効かない画面に `[u]` が出る（#53）。
        let cases = [
            (AccountSource::GitConfig, true),
            (AccountSource::RepoRule, false),
            (AccountSource::Single, false),
            (AccountSource::Probe, false),
        ];

        for (source, offered) in cases {
            let state = state_with_repick(source, AccountListing::Listed(vec![fake_login("taro")]));
            let text = draw(&state, SIZE).join("\n");
            assert_eq!(
                text.contains("[u] 指定を解除"),
                offered,
                "{source:?}: {text}"
            );
        }
    }

    #[test]
    fn the_hint_with_u_still_fits_one_row_of_the_box() {
        // **`[u]` を足した案内がいちばん長い**（57 セル。箱の内側と同じ）。
        // 溢れると `modals::place` が案内に取る 1 行の右端で**切れて**、最後の
        // `[Esc]` が消える（確認を挟まずに閉じるキーなので、押す前に見えている
        // 必要がある）。**`contains` だけでは 1 行に収まったかを見ていない**ので、
        // 行そのものを取り出して確かめる（#73 の「余白が残るか」）。
        let state = state_with_repick(
            AccountSource::GitConfig,
            AccountListing::Listed(vec![fake_login("taro")]),
        );

        let rows = draw(&state, SIZE);
        // **探すのは `[u]` だけ。** 文言そのものを鍵にすると、案内を伸ばして
        // 折り返させたときに「行が見つからない」で落ちてしまい、**この見張りが
        // 幅を見ているのか綴りを見ているのか判らなくなる**。
        let hint = rows
            .iter()
            .find(|row| row.contains("[u]"))
            .unwrap_or_else(|| panic!("{}", rows.join("\n")));

        assert!(
            hint.contains("[Esc]"),
            "案内が 1 行に収まっていない: {hint}"
        );
    }

    #[test]
    fn a_setting_that_mewgit_cannot_remove_says_where_it_lives() {
        // **指示を足すのは、その操作を持たない側**（#65）。`[[repos]]` は
        // `git config` の手が届かないので、外での直し先を本文に書く。
        let state = state_with_repick(
            AccountSource::RepoRule,
            AccountListing::Listed(vec![fake_login("taro")]),
        );

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("[[repos]]"), "{text}");
        assert!(text.contains("外せません"), "{text}");
    }

    #[test]
    fn a_listing_that_failed_says_why_in_the_body() {
        // **`DegradedReason` に畳まない**ので、ここが理由を出す唯一の場所（01）。
        let state = state_with_repick(
            AccountSource::Probe,
            AccountListing::Failed("gh が見つかりませんでした".to_string()),
        );

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("候補を読み込めませんでした"), "{text}");
        assert!(text.contains("gh が見つかりませんでした"), "{text}");
        assert!(!text.contains("[Enter]"), "{text}");
    }

    #[test]
    fn a_broken_configured_login_says_which_of_the_two_settings_it_came_from() {
        // **`AccountSource` を運ぶようにした効き目。** 両方を並べる形は #73 で
        // 出荷していて、「選び直せない」を `/code-review` に拾われた。
        let state = state_with_reason(DegradedReason::ConfiguredUnavailable {
            login: "taro".to_string(),
            source: AccountSource::GitConfig,
            message: "no oauth token found".to_string(),
        });

        let text = draw(&state, SIZE).join("\n");

        assert!(text.contains("mewgit.user は .git/config"), "{text}");
        assert!(!text.contains("[[repos]]"), "{text}");
        // **外せるので `u` を出す**（`AccountView::is_unsettable` が degraded でも
        // 真になる唯一の腕）。
        assert!(text.contains("[u] 指定を解除"), "{text}");
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
            view: AccountView::Degraded(ambiguous(vec![
                fake_login("taro_acme"),
                fake_login("taro"),
            ])),
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
            view: AccountView::Degraded(ambiguous(logins)),
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
