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

/// `r` の案内。**出すのは [`AccountView::is_reresolvable`] が真のときだけ**（同上）。
const HINT_RERESOLVE: &str = "[r] 解決し直す";

/// 外での直し方が mewgit の設定ファイルを指すときに添える 1 行。
///
/// **`r` が読み直すのは `gh` と `.git/config` だけ。** 設定ファイルは起動時に固まる
/// （08。`config` は `AppState` が持ち、`Cmd::ResolveAccount` はその写しを使う）ので、
/// `general.gh_bin` や `[[repos]]` を直しても `r` では拾えない（pty で実測）。
/// **`[r] 解決し直す` を出した画面で、効かない直し方だけを案内しないため**
/// （07 の「外での直し方が設定ファイルを指すなら」）。
const RESTART_FOR_CONFIG: &str = "設定ファイルを直したときは mewgit の再起動が要ります";

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
    let candidates = view.candidates();

    picker::render(
        frame,
        area,
        Picker {
            title: title(view),
            body: body(view),
            rows: candidates.iter().map(candidate_line).collect(),
            cursor,
            hint: &hint(view),
        },
    );
}

/// 枠のタイトル。**元になる述語は [`hint`] や行の有無と同じ「候補が空か」**
/// —— 別々に数えると、片方だけが「選べる」ふりをする（07）。
///
/// **例外は 1 つ、待っているあいだ。** そこだけは候補が空でも「選ぶ」と書く ——
/// 0.95 秒で候補が出るものを「選べません」と名乗って書き換えるほうが読めない（07）。
/// **`[hint]` は例外を持たない**ので、待っているあいだの案内は `[Esc] 閉じる` に
/// なる（押せないキーを書かないほう）。**引けなかったときは書き換える** ——
/// あれは待てば変わる状態ではないので、選べる相手がいないことを枠でも言う。
fn title(view: &AccountView) -> &'static str {
    if !view.candidates().is_empty() || waiting(view) {
        TITLE_PICK
    } else {
        TITLE_SHOW
    }
}

/// 候補の取り直しを待っているか。**「まだ答えが出ていない」を言うのはここだけ。**
fn waiting(view: &AccountView) -> bool {
    matches!(
        view,
        AccountView::Repick(Repick {
            logins: AccountListing::Waiting,
            ..
        })
    )
}

/// キーの案内。**`u` と `r` はそれぞれの述語が真のときだけ足す**
/// （[`AccountView::is_unsettable`] / [`AccountView::is_reresolvable`]）——
/// 出す条件と `update::account_key` が受ける条件が同じ 1 本の述語から出る（#53）。
///
/// **`pick` を引数に取らない。** 呼ぶ側が計算して渡す形にすると、候補が無いのに
/// `[Enter] 保存` を出す組み合わせが型で作れる（「導出できる値を引数に取らせない」）。
fn hint(view: &AccountView) -> String {
    let base = if view.candidates().is_empty() {
        HINT_SHOW
    } else {
        HINT_PICK
    };
    // **その画面にしか無いキーを先に置く**（07）。`[Esc]` はどこでも効くので末尾。
    let mut parts = Vec::new();
    if view.is_unsettable() {
        parts.push(HINT_UNSET);
    }
    if view.is_reresolvable() {
        parts.push(HINT_RERESOLVE);
    }
    parts.push(base);
    parts.join("   ")
}

/// 本文。[`AccountView`] の 2 つで分かれる。
fn body(view: &AccountView) -> Text<'static> {
    match view {
        AccountView::Degraded(reason) => degraded_body(reason),
        AccountView::Repick(repick) => repick_body(repick),
    }
    .into_iter()
    .collect()
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
        // **0 件は `Ok` で返る**（`gh auth status` は 0 件を失敗にしない。
        // 空を `NotLoggedIn` に畳むのは `auth::resolve` の仕事で、`Cmd::ListAccounts`
        // はそこを通らない）。**起動後に外で `gh auth logout` すると踏める** ——
        // 保存先だけを出すと、**選べる行が 1 つも無い画面が「選ぶと保存します」と
        // 言う**（`/code-review` が拾った）。`NotLoggedIn` の degraded と同じ案内に
        // 落とす。
        AccountListing::Listed(logins) if logins.is_empty() => {
            lines.push(format!("{host} にログインしていません"));
            lines.push(format!(
                "gh auth login --hostname {host} でログインしてください"
            ));
        }
        AccountListing::Listed(_) => lines.push(saves_to()),
        // **`gh` の失敗をここで出す。** `DegradedReason` に畳まないと決めてあるので
        // （01 の「候補の取り直し」）、`state.account` は `Resolved` のまま。
        // **次の一手を書く。** 引けなかった理由（gh が消えた・答えない）は外で
        // 直すものだが、**引き直しは mewgit が持っている操作**なので、そこまで
        // 案内する。**`A` はモーダル表示中には効かない**ので閉じてから、と書く
        // （#53 の「案内にキーを書いたら、その案内が出ている画面でそのキーが
        // 効くかを数える」）。
        AccountListing::Failed(message) => {
            lines.push("候補を読み込めませんでした".to_string());
            lines.push(message.clone());
            lines.push("Esc で閉じてから A で引き直せます".to_string());
        }
    }
    // **外せない側にだけ外での直し方を書く**（#65 の「指示を足すのは、その操作を
    // 持たない側」）。外せる側には `[u] 指定を解除` が出ている。
    lines.extend(cannot_remove(*source));
    lines
}

/// mewgit から外せない**指定**なら、その 1 行。
///
/// **`Single` / `Probe` は `None`。** [`AccountSource::is_unsettable`] が偽なのは
/// 同じだが、あちらは mewgit が選んだ結果で**直す場所そのものが無い** ——
/// `is_unsettable` を条件に使うと「ログインが 1 つだけ は外せません」と出る。
///
/// **網羅 `match` で書く**（`modals::render` が `_` を使わないのと同じ理由）。
/// `_ =>` を置くと、`AccountSource` を 1 つ足したときにどちらの側へ落ちても
/// コンパイルエラーにならない。
fn cannot_remove(source: AccountSource) -> Option<String> {
    match source {
        AccountSource::RepoRule => Some(format!(
            "{} は mewgit からは外せません",
            source_label(source)
        )),
        AccountSource::GitConfig | AccountSource::Single | AccountSource::Probe => None,
    }
}

/// [`AccountSource`] を画面に出す語（04 の手順 1〜4）。
///
/// **明示指定の 2 つを別々に出す。** どちらを直せば変わるかが違ううえ、
/// `.git/config` のほうは `u` でその場で外せる（`AccountSource` の doc）。
///
/// **置き場所を書くのはここ 1 本。** [`degraded_body`] の
/// `ConfiguredUnavailable` も [`repick_body`] もこれを呼ぶ —— 手で書くと、
/// `AccountSource` を 1 つ足したときに直す場所が増える（しかも
/// [`MEWGIT_USER`] を literal で書き直すことになる。[`saves_to`] の doc）。
fn source_label(source: AccountSource) -> String {
    match source {
        AccountSource::GitConfig => format!(".git/config の {MEWGIT_USER}"),
        AccountSource::RepoRule => "設定ファイルの [[repos]]".to_string(),
        AccountSource::Single => "ログインが 1 つだけ".to_string(),
        AccountSource::Probe => "届いたのが 1 つだけ".to_string(),
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
            RESTART_FOR_CONFIG.to_string(),
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
        DegradedReason::ConfiguredUnavailable {
            login,
            source,
            message,
        } => {
            let mut lines = vec![
                format!("設定が指す {login} のトークンがありません"),
                message.clone(),
                // **置き場所は [`source_label`] の 1 本から引く。** ここで手書きすると
                // `MEWGIT_USER` を literal で書くことになる（[`saves_to`] の doc）。
                format!("{} を直してください", source_label(*source)),
            ];
            // **`.git/config` には添えない。** `auth::resolve` が毎回 git2 で開き直すので
            // `r` で拾える —— 添えると、要らない再起動を案内することになる。
            if source == &AccountSource::RepoRule {
                lines.push(RESTART_FOR_CONFIG.to_string());
            }
            lines
        }
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
    use super::{hint, HINT_RERESOLVE};
    use crate::test_util::{draw, row_is_reversed, row_of, state_with_modal};
    use mewgit_core::app::{AccountListing, AccountView, AppState, Modal, Repick, TermSize};
    use mewgit_core::auth::{
        AccountSource, AttemptFailure, DegradedReason, LoginState, ProbeAttempt,
    };
    use mewgit_core::test_util::{fake_configured_unavailable, fake_login};

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

    /// 決まっているアカウントを選び直す画面の中身。
    ///
    /// **`AccountView` で返す。** 幅の見張りは `hint(&view)` を組み直すので、
    /// `AppState` まで包んだものからは取り出せない。
    fn repick(source: AccountSource, logins: AccountListing) -> AccountView {
        AccountView::Repick(Repick {
            login: "taro".to_string(),
            host: "github.com".to_string(),
            source,
            logins,
        })
    }

    /// 決まっているアカウントを選び直す画面。
    fn state_with_repick(source: AccountSource, logins: AccountListing) -> AppState {
        state_with_modal(Modal::AccountPicker {
            view: repick(source, logins),
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
        // **枠は「選ぶ」のまま。** 0.95 秒で候補が出るものを「選べません」と
        // 名乗って書き換えるほうが読めない（07）。
        assert!(text.contains("GitHub アカウントを選ぶ"), "{text}");
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
    fn the_longest_hints_still_fit_one_row_of_the_box() {
        // 案内が溢れると `modals::place` が案内に取る 1 行の右端で**切れて**、
        // 最後の `[Esc]` が消える（確認を挟まずに閉じるキーなので、押す前に見えて
        // いる必要がある）。**`contains` だけでは 1 行に収まったかを見ていない**ので、
        // 行そのものを取り出して確かめる（#73 の「余白が残るか」）。
        //
        // **`[u]` か `[r]` を足した案内が箱の内側とちょうど同じ 57 セル**
        // （`WIDTH` 60 から枠 2 と `RIGHT_MARGIN` 1 を引いた値）で、**余白は無い**。
        // `[u] 指定を解除` と `[r] 解決し直す` はどちらも 14 セルなので、選ばせる
        // 画面ではどちらを足しても同じ幅になる（実測）。
        //
        // **産出元が増えたら回す画面を足す**（共有した見張りは新しい産出元を自分では
        // 拾わない。`CLAUDE.md`）—— `[r]` を足したときに degraded 側が抜けていると、
        // `[u]` を出す画面だけで緑のままになる。
        //
        // **端末を狭めたときの境目は実測で 53 桁**（52 桁で `[Esc]` が切れる。
        // `[u]` を足す前は 36 桁まで保っていた）。いま見張るのは 80 桁だけで、
        // 狭い端末は M6 の「端末幅 < 100 のレイアウト」が持つ。
        //
        // **行を探すのは案内の先頭トークン、収まったかを見るのは [`hint`] が組んだ
        // 文字列そのもの。** 綴りを手で書くと、案内を伸ばして溢れさせたときに「行が
        // 見つからない」で落ちてしまい、**幅を見ているのか綴りを見ているのかが
        // 判らなくなる**。**`[Esc]` を `contains` で探す形では浅い** —— 切れるのは
        // 行末からなので、5 セル溢れても `[Esc] やめ` が残って素通りする（実測）。
        let cases = [
            // `[u]` + 選ばせる案内。
            repick(
                AccountSource::GitConfig,
                AccountListing::Listed(vec![fake_login("taro")]),
            ),
            // `[r]` + 選ばせる案内（`Ambiguous` は候補を運ぶので選ばせる側に出る）。
            AccountView::Degraded(ambiguous(vec![fake_login("taro_acme"), fake_login("taro")])),
            // **`[u]` と `[r]` が同居する形。** `ConfiguredUnavailable` は出どころを
            // 持つので `[u]` も出る。候補を運ばないので**いまは 46 セル**だが、
            // **運ぶようになれば 74 セルで入らなくなる** —— その回を拾うのはこの腕だけ。
            AccountView::Degraded(fake_configured_unavailable()),
        ];

        for view in cases {
            let want = hint(&view);
            // **鍵は `want` から導く**（「導出できる値を引数に取らせない」）。案内の
            // 並び順を変えても、直す場所が増えない。
            let marker = want.split(' ').next().expect("案内が空");
            let state = state_with_modal(Modal::AccountPicker { view, cursor: 0 });

            let rows = draw(&state, SIZE);
            let row = &rows[row_of(&rows, marker) as usize];

            assert!(row.contains(&want), "案内が 1 行に収まっていない: {row}");
        }
    }

    #[test]
    fn a_fix_that_lives_in_the_config_file_says_a_restart_is_needed() {
        // **`r` が読み直すのは `gh` と `.git/config` だけ。** 設定ファイルは起動時に
        // 固まる（08）ので、`[r] 解決し直す` を出した画面が `general.gh_bin` や
        // `[[repos]]` を直せと言うだけでは、**押しても変わらない案内**になる
        // （pty で実測: 走行中に `config.toml` を直して `r` を押しても
        // 「gh が見つかりません」のまま）。
        let needs_restart = [
            DegradedReason::GhMissing,
            DegradedReason::ConfiguredUnavailable {
                login: "taro".to_string(),
                source: AccountSource::RepoRule,
                message: "no oauth token found".to_string(),
            },
        ];
        for reason in needs_restart {
            let text = draw(&state_with_reason(reason.clone()), SIZE).join("\n");
            assert!(text.contains("再起動"), "{reason:?}: {text}");
        }

        // **`.git/config` には添えない。** `auth::resolve` が毎回 git2 で開き直すので
        // `r` で拾える —— 添えると要らない再起動を案内する。
        let text = draw(&state_with_reason(fake_configured_unavailable()), SIZE).join("\n");
        assert!(!text.contains("再起動"), "{text}");
    }

    #[test]
    fn r_is_offered_only_where_resolving_again_can_change_the_answer() {
        // **案内と `update::account_key` が同じ述語（`AccountView::is_reresolvable`）を
        // 読む**（#53）。`Repick` に出さないのは、あちらが行き止まりではないから ——
        // 選んで保存すれば書き込みのあとの `resolve_again` を通る（01 の
        // 「外で直したあとの再解決」）。
        let degraded = draw(&state_with_reason(DegradedReason::GhMissing), SIZE).join("\n");
        assert!(degraded.contains(HINT_RERESOLVE), "{degraded}");

        let repick = draw(
            &state_with_repick(
                AccountSource::GitConfig,
                AccountListing::Listed(vec![fake_login("taro")]),
            ),
            SIZE,
        )
        .join("\n");
        assert!(!repick.contains(HINT_RERESOLVE), "{repick}");
    }

    #[test]
    fn a_repick_with_no_logins_left_does_not_promise_to_save_anything() {
        // **0 件は `Ok` で返る**（`gh auth status` は 0 件を失敗にしない）。起動後に
        // 外で `gh auth logout` すると踏める。**保存先だけを出すと、選べる行が
        // 1 つも無い画面が「選ぶと保存します」と言う**（`/code-review` が拾った）。
        let state = state_with_repick(AccountSource::Probe, AccountListing::Listed(Vec::new()));

        let text = draw(&state, SIZE).join("\n");

        assert!(!text.contains("選ぶと"), "{text}");
        assert!(text.contains("github.com にログインしていません"), "{text}");
        assert!(text.contains("gh auth login"), "{text}");
        // 選べる相手がいないので枠も案内も「選ぶ」と名乗らない。
        assert!(!text.contains("GitHub アカウントを選ぶ"), "{text}");
        assert!(!text.contains("[Enter]"), "{text}");
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
        // **待ちとは枠が違う。** 引けなかったのは待てば変わる状態ではないので、
        // 「選ぶ」と名乗らない（`title` の doc）。
        assert!(!text.contains("GitHub アカウントを選ぶ"), "{text}");
        // **次の一手を出す。** 引き直しは mewgit が持っている操作なので、
        // 行き止まりにしない（#65）。`A` はモーダル表示中には効かないので、
        // 閉じてから、と書く（#53）。
        assert!(text.contains("Esc で閉じてから A で引き直せます"), "{text}");
    }

    #[test]
    fn a_broken_configured_login_says_which_of_the_two_settings_it_came_from() {
        // **`AccountSource` を運ぶようにした効き目。** 両方を並べる形は #73 で
        // 出荷していて、「選び直せない」を `/code-review` に拾われた。
        let state = state_with_reason(fake_configured_unavailable());

        let text = draw(&state, SIZE).join("\n");

        assert!(
            text.contains(".git/config の mewgit.user を直して"),
            "{text}"
        );
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
