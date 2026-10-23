use std::collections::HashMap;

use crate::github::{GithubError, PrDetail, PullRequest, RateLimit};

use super::{seated_index, stepped_index};

/// PR ペインの状態（01 の「PR ペイン」）。
///
/// **中身はこのリポジトリで開いている PR の一覧**で、ブランチにもコミットにも
/// 紐づかない。**鍵を持たない**のはそのため —— 鍵が答えるのは「選択が動いたか」で、
/// 一覧には対応する問いが無い。追いつきは世代（[`JobKind::FetchPrList`](super::JobKind::FetchPrList) の
/// `region()`）だけで足りる。
///
/// **形は [`BranchState`](super::BranchState) に揃えてある**（値 + 段階）。引けた値そのものを 3 状態の
/// enum に入れると、**読み直しが 1 回転けただけで読めていた一覧が画面から消える**
/// （`BranchState::failed` の doc が言う「捨てると画面から消えるだけになる」）。
/// 一覧を引き直すのは ref を動かす書き込みのたびなので、この差は毎コミット効く。
#[derive(Debug, Default)]
pub struct PrState {
    /// 開いている PR。API が返した順（更新の新しい順）そのまま。
    ///
    /// **書くのは [`PrState::loaded`] と [`PrState::forget`] だけ。** 外から差し替えると、
    /// `loaded` が揃える `load` と `details`（`(番号, head sha)` で一覧と突き合わせる）が
    /// 古いまま残る。
    list: Vec<PullRequest>,
    /// 一覧のカーソル。書くのは `loaded`（番号で座り直す）・`forget` と
    /// `move_cursor` / [`PrState::set_cursor`]（端で丸める）だけ。
    cursor: usize,
    /// 読み込みの段階。空の一覧と「まだ届いていない」を出し分けるのに要る。
    load: PrLoad,
    /// PR ごとの集計。鍵は番号（01 の「PR ペイン › 集計」）。
    details: HashMap<u64, PrDetailEntry>,
    /// どのアカウントの一覧に対する集計か。[`PrState::forget`] が進める。
    ///
    /// **`(番号, head sha)` の鍵では足りない。** アカウントを切り替えても PR の番号も
    /// head も変わらないので、**前のトークンで飛んでいた扇形の答えが新しい一覧に
    /// そのまま当たる**（`forget` の doc が落とすと言っている守りを、走っている扇形が
    /// 外から破る）。鍵が答えるのは「どの行に当てるか」で、**「どのアカウントで
    /// 引いたか」には答えられない**。
    ///
    /// **[`JobTracker`](super::JobTracker) の世代とは別物。** あちらは git の書き込みが何を古くするかを
    /// 数えるもので、アカウントの切り替えでは進まない。
    epoch: u64,
    /// GitHub API の残量。ヘッダに出す（07 の「ヘッダ」）。
    ///
    /// **置き場所がここなのは、落とす引き金と突き合わせる世代が両方ここにあるから。**
    /// 残量は**そのトークンの予算**なので、アカウントが変われば嘘になる ——
    /// 落とすのは [`PrState::forget`]、飛んでいる扇形の答えを弾くのは同じ `epoch`。
    /// `AppState` の直下に置くと、落とす責務だけが別の場所に離れる。
    ///
    /// **ここへ届くのは PR の 2 本だけ**（一覧と扇形）。`auth::resolve` の probe が
    /// 使うクライアントも応答ヘッダから残量を憶えるが、**運ぶ経路が無い** ——
    /// `Msg::RateLimitSeen` を送るのはランタイムの PR の腕だけで、probe の
    /// クライアントは `auth` の中で使い捨てになる。**まだ誰のトークンか決まって
    /// いない候補の予算**を出さないため（05 の「残量の受け渡し」）。
    rate_limit: Option<RateLimit>,
    /// **いま作ったばかりで、まだ一覧に出ていない PR の番号**（01 の
    /// 「PR 作成（`N`）」）。届いた一覧でその行に座らせ、そこで落とす。
    ///
    /// **憶えないと座らない。** [`PrState::loaded`] は「いま選んでいる PR の番号」を
    /// 優先するので、作る前に別の行を選んでいると、新しい行が一覧の先頭に入っても
    /// カーソルはそのまま残る。
    ///
    /// **材料は `Msg::PrCreated` だけ。** 別のジョブから届く値を混ぜると、到着順で
    /// 答えが変わる（#63 の `RepoInfo.target`）。
    ///
    /// **当たったときだけ落とす**（[`PrState::loaded`]）。作成より前に頼まれた
    /// 一覧が先に届くことがあり（[`PrState::list_behind`]）、そこで落とすと
    /// **二度と座らない**。当たらないまま残るのは次の一覧までで、そこで
    /// [`PrState::forget_created`] が落とす。
    created: Option<u64>,
    /// **作った直後に頼んだ一覧が [`AppState::issue`](super::AppState::issue) に落とされた。**
    ///
    /// `r` や書き込みで `FetchPrList` が飛んでいる最中に POST のほうが先に返ると、
    /// `pr_created` が返す `Cmd::FetchPrList` は同種実行中なので落ちる。そこへ届く
    /// 一覧は**作成より前に頼まれたもの**なので、作った PR は入っていない。
    ///
    /// **`JobTracker::is_fresh` では拾えない。** あれは領域の世代で「書き込みに
    /// 追い越されたか」を見るが、**PR 作成はローカルの ref を動かさない**ので
    /// 世代が進まない（`JobKind::CreatePr::region()` が `None`）。
    ///
    /// **1 回きり。** 当たらないたびに頼み直すと、作った PR が即座に閉じられた
    /// ときに**同じ往復を回し続ける**（01 の「`Err` から再発行してよいのは対象が
    /// 違うときだけ」と同じ形）。
    list_behind: bool,
    /// 一覧を落としたとき（[`PrState::forget`]）に飛んでいた `FetchPrList` の返事を
    /// 捨てる印（01 の「一覧はアカウントに紐づく」）。
    ///
    /// **`JobTracker::is_fresh` では拾えない。** アカウントの解決し直しは
    /// `Region::Refs` を進めないので、前のトークンで頼んだ一覧が新しいものとして通る。
    /// **[`PrState::forget`] では落とさない** —— 立てるのはその直後で、落とすのは
    /// 返事が届いたときだけ。
    list_orphaned: bool,
}

/// PR 1 行ぶんの集計と、どの head sha について引いたか。
///
/// **外へは出さない**（[`PrState::detail`] が `result` だけを貸す）。`sha` を持つのは
/// 一覧と突き合わせるためで、読むのは [`in_list`] を呼ぶところだけ。一覧は
/// `sort=updated` で並びが変わり、PR の head も push で動くので、番号だけを鍵に
/// すると push で sha が進んだ PR に**前のコミットのチェック結果**が残る。
#[derive(Debug, Clone, PartialEq, Eq)]
struct PrDetailEntry {
    sha: String,
    /// **`Result` のまま持つ。** 失敗の理由は PR ごとに違う（404 は fork の削除、
    /// 403 はスコープ、レート制限は待てば直る）ので、畳むと `ui` が案内を選べない。
    result: Result<PrDetail, GithubError>,
}

/// その `(番号, head sha)` がこの一覧に居るか。
///
/// **集計の鍵の定義はここ 1 つ。** 憶える側（[`PrState::detail_loaded`]）と捨てる側
/// （[`PrState::loaded`]）が別々に書くと、片方に条件を足したときに**食い違ったまま
/// 両方のテストが緑になる**（`.claude/rules/rust-coding.md` の「同じ判定を 2 箇所に書かない」）。
fn in_list(list: &[PullRequest], number: u64, sha: &str) -> bool {
    list.iter()
        .any(|pr| pr.number == number && pr.head.sha == sha)
}

/// PR 一覧の読み込みがどこまで進んだか。
///
/// **`bool` を 2 つにしない**（[`ListLoad`](super::ListLoad) と同じ理由）。
///
/// **[`ListLoad`](super::ListLoad) と違って `Failed` が理由を持つ。** 案内に出すのが「読み込めません
/// でした」だけでは足りない —— 401 / 403 / レート制限は**次に何をするかが違う**ので、
/// [`GithubError`] をそのまま運んで `ui` に出させる（07 の「PRs ペイン」）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum PrLoad {
    /// まだ届いていない。
    #[default]
    Pending,
    Loaded,
    Failed(GithubError),
}

impl PrState {
    /// 開いている PR。API が返した順（更新の新しい順）そのまま。まだ届いていなければ空。
    pub fn list(&self) -> &[PullRequest] {
        &self.list
    }

    /// カーソルの行（[`PrState::list`] の添字）。
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// カーソルを動かす。行が 1 つも無ければ据え置く。
    pub(in crate::app) fn move_cursor(&mut self, delta: isize) {
        self.cursor = stepped_index(self.cursor, self.list.len(), delta);
    }

    /// カーソルを `row` 行目へ跳ばす（[`BranchState::set_cursor`](super::BranchState::set_cursor) と同じ）。
    pub fn set_cursor(&mut self, row: usize) {
        self.cursor = seated_index(self.cursor, self.list.len(), row);
    }

    /// いまカーソルが指している PR。
    pub fn selected(&self) -> Option<&PullRequest> {
        self.list.get(self.cursor)
    }

    /// 読み込みの段階。一覧が空のときに `ui` が出す文言をここから決める
    /// （07 の「PRs ペイン」）。
    ///
    /// **`ui` が見るのは一覧が空のときだけ。** 中身があるならそれを描く ——
    /// 読み直しが失敗しても読めていた一覧は捨てない（[`BranchState::load`](super::BranchState::load) と同じ）。
    pub fn load(&self) -> &PrLoad {
        &self.load
    }

    /// 届いた一覧で置き換える。
    ///
    /// **カーソルは PR の番号で座り直す。** 一覧は `sort=updated` で引くので、
    /// **ref を動かす書き込みのたびに並びが変わる** —— 添字を保つと、自分が commit
    /// した瞬間にカーソルが別の PR を指す（`BranchState::loaded` が名前で座り直すのと
    /// 同じ理由）。
    ///
    /// **座り直す先は 3 つで、強い順に**：いま作った PR（[`PrState::created`]）、
    /// いま選んでいる PR の番号、`fallback` が新しい一覧から選ぶ番号（起動直後は
    /// 「いま居るブランチの PR」。01 の「現ブランチの PR」）。どれも見つからなければ
    /// 先頭。**`fallback` が新しい一覧を受け取るのは、その答えが一覧の中身から
    /// 決まるから** —— 呼ぶ側に座り直しの手順を出すと、添字のまま座る形が外から
    /// 書けるようになる。
    pub(in crate::app) fn loaded(
        &mut self,
        list: Vec<PullRequest>,
        fallback: impl FnOnce(&[PullRequest]) -> Option<u64>,
    ) {
        // **作ったばかりの PR がいちばん強い**（01 の「PR 作成（`N`）」）。人が
        // いま出したものなので、直前に選んでいた行より先に見せる。
        //
        // **当たったときだけ落とす。** 作成より前に頼まれた一覧が先に届くことが
        // あり（[`PrState::list_behind`]）、そこで無条件に落とすと**作った PR に
        // 二度と座らない**。当たらないまま残るのは次の一覧までで、そこで
        // [`PrState::forget_created`] が落とす。
        let wanted = self
            .created
            .take_if(|number| list.iter().any(|pr| pr.number == *number))
            .or_else(|| self.selected().map(|pr| pr.number))
            .or_else(|| fallback(&list));
        self.cursor = wanted
            .and_then(|number| list.iter().position(|pr| pr.number == number))
            // 選んでいたものが open から消えた（マージ / クローズ）か、まだ何も
            // 選んでいない。
            .unwrap_or(0);
        // **残すのは「引けた」集計のうち、`(番号, head sha)` が一致する行だけ。**
        // push で head が進んだ行のチェックは前のコミットのもので、一覧から消えた行の
        // ものは誰も読まない（05 の「扇形は毎回引き直さない」）。
        //
        // **引けなかった行（`Err`）はここで落とす。** 残すと `wanted_details` が
        // 二度と拾わず、**回線が 1 回切れただけでセッション中ずっと `!` のまま**に
        // なる。一覧を引き直すのは `f` と書き込みのあとなので、ここが
        // 「失敗からの出口」（01 の「何を引くか」が `reload` について言っているのと
        // 同じ形。**人が `r` を押しても落ちる** —— `forget_detail`）。
        // **輪にはならない** —— 1 回の扇形の中では答えが入って
        // `wanted_details` が空になり、次に頼むのは新しい一覧が届いたときだけ。
        self.details
            .retain(|number, entry| entry.result.is_ok() && in_list(&list, *number, &entry.sha));
        self.list = list;
        self.load = PrLoad::Loaded;
    }

    /// 読み込みが失敗した。**読めていた一覧は捨てない**（`BranchState::failed` と
    /// 同じ理由。捨てると画面から消えるだけになる）。
    pub(in crate::app) fn failed(&mut self, err: GithubError) {
        self.load = PrLoad::Failed(err);
    }

    /// この PR の集計。
    ///
    /// **`None` は「まだ引いていない」。** 表に**居ないこと**がそれで、`Err` は
    /// 「引いたが読めなかった」。**同じ `None` に畳まない**（`.claude/rules/rust-coding.md` の
    /// 「`None` の理由を 2 つ以上持たせない」）—— 畳むと `ui` が「CI の無い PR」と
    /// 「CI を読めなかった PR」を区別できない。
    ///
    /// 居ない行に扇形が飛ぶ条件は `AppState::wanted_details`（01 の
    /// 「PR ペイン › 集計」）。**どの sha について引いたかは外へ出さない** ——
    /// 突き合わせるのは `detail_loaded` と `loaded` の中だけで、出すと「表示している
    /// 行と違う sha の集計」を読む形が外から書ける。
    pub fn detail(&self, number: u64) -> Option<&Result<PrDetail, GithubError>> {
        self.details.get(&number).map(|entry| &entry.result)
    }

    /// まだ集計を持っていない行の `(番号, head sha)`。
    ///
    /// **持っている行は引き直さない。** 1 件ぶんの往復が PR の数だけ積むので、
    /// ref を動かす書き込みのたびに全件引くと `core` の予算に対して重すぎる
    /// （数は 05 の「扇形は毎回引き直さない」）。憶えた集計を落とすのは
    /// [`loaded`](Self::loaded)（head sha が動いた行）、[`forget`](Self::forget)
    /// （アカウントが変わった）、[`forget_detail`](Self::forget_detail)（`r` で
    /// 名指しされた行）。
    ///
    /// **`app` へは出さない。** タブの門を通さずに扇形を組む形が書けてしまうので、
    /// 外から呼べるのは [`AppState::wanted_details`](super::AppState::wanted_details) だけにしてある（`pub(super)` は
    /// `AppState` の居る `state` までで、`update` からは見えない）。
    pub(super) fn wanted_details(&self) -> Vec<(u64, String)> {
        self.list
            .iter()
            .filter(|pr| !self.details.contains_key(&pr.number))
            .map(|pr| (pr.number, pr.head.sha.clone()))
            .collect()
    }

    /// 扇形の答えが 1 件届いた。
    ///
    /// **鍵は `(番号, sha)`。** 飛んでいるあいだに push で head が進んだ PR の答えは
    /// **前のコミットのもの**なので捨てる（捨てた行は集計を持たないままなので、
    /// 次の [`wanted_details`](Self::wanted_details) が拾い直す）。
    /// **一覧から消えた PR の答えも捨てる** —— 入れると、居ない行の集計が
    /// `details` に溜まる。
    pub(in crate::app) fn detail_loaded(
        &mut self,
        number: u64,
        sha: String,
        result: Result<PrDetail, GithubError>,
    ) {
        if !in_list(&self.list, number, &sha) {
            return;
        }
        self.details.insert(number, PrDetailEntry { sha, result });
    }

    /// 扇形が打ち切られた。**頼んだのに答えの返らなかった行を理由ごと埋める。**
    ///
    /// 埋めないと、その行は「まだ引いていない」のまま
    /// [`wanted_details`](Self::wanted_details) に拾われ、**同じ失敗を買い直す輪**に
    /// なる（01 の「`Err` から再発行してよいのは対象が違うときだけ」）。
    ///
    /// **埋める相手は `targets` であって、いまの一覧ではない。** 一覧のほうを舐めると、
    /// **その扇形が一度も対象にしていない行まで塞ぐ** —— 扇形が走っているあいだに
    /// commit などで一覧が増えると、増えた行は `AppState::issue` に落とされて頼まれて
    /// おらず、そこへ打ち切りの理由を書くと次の `fan_out` も空になって二度と引かれない。
    ///
    /// **いまの一覧に居ない対象は捨てる**（[`in_list`]）。頼んだあとに head が動いた
    /// 行を埋めても、その集計は次の [`loaded`](Self::loaded) が落とす。
    pub(in crate::app) fn details_aborted(&mut self, targets: &[(u64, String)], err: &GithubError) {
        for (number, sha) in targets {
            if !in_list(&self.list, *number, sha) {
                continue;
            }
            self.details
                .entry(*number)
                .or_insert_with(|| PrDetailEntry {
                    sha: sha.clone(),
                    result: Err(err.clone()),
                });
        }
    }

    /// いまの世代。扇形を頼むときに刻み、返信で突き合わせる。
    ///
    /// **`ui` は読まない。** 読むのは `update::prs`（頼むとき）と `test_util`
    /// （頼む `Cmd` を組むとき）。
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// その返信は、いまのアカウントの一覧に対するものか。
    pub(in crate::app) fn is_current(&self, epoch: u64) -> bool {
        self.epoch == epoch
    }

    /// いま判っている残量。ヘッダが読む（07）。
    pub fn rate_limit(&self) -> Option<RateLimit> {
        self.rate_limit
    }

    /// 応答が言った残量を憶える。
    ///
    /// **新しいほうだけ残す**（[`RateLimit::keep_newer`]）。ジョブは同時に走り、`Msg` が
    /// 届く順は応答の新しさの順ではない —— 一覧の答えが扇形の最中に返れば、扇形が
    /// もっと減らしたあとの数を上書きしてしまう。
    pub(in crate::app) fn rate_limit_seen(&mut self, seen: RateLimit) {
        RateLimit::keep_newer(&mut self.rate_limit, seen);
    }

    /// この行の集計を落とす。`Err` も落とす。
    ///
    /// **1 行だけなこと・世代を進めないことの理由は、呼ぶ側の 1 本**
    /// （`update::prs::refresh`。01 の「引き直す（`r`）」）。
    pub(in crate::app) fn forget_detail(&mut self, number: u64) {
        self.details.remove(&number);
    }

    /// アカウントが決まっていない状態に戻す。
    ///
    /// **一覧はどのアカウントで引いたかに紐づく。** `A` で選び直したり degraded に
    /// 落ちたりした後に前のトークンで引いた一覧を出したままにすると、`*` / `R` の印
    /// （login で決まる）だけが黙って消えた一覧が残る。**`Pending` に戻すのは、
    /// `AccountState` が `Resolved` に戻れば `prs::reload` が引き直すから**
    /// （01 の「何を引くか」）。
    pub(in crate::app) fn forget(&mut self) {
        self.list.clear();
        self.cursor = 0;
        self.load = PrLoad::Pending;
        // **集計も落とす。** 残すと、次のアカウントで引き直した一覧に**前の
        // トークンで見えた集計**が番号だけで当たる。
        self.details.clear();
        // **飛んでいる扇形の答えも当たらなくする。** 落とすだけでは足りない ——
        // 番号も head も変わらないので、前のトークンで頼んだ答えが新しい一覧に
        // そのまま入る（上の `epoch`）。
        self.epoch = self.epoch.wrapping_add(1);
        // **残量も落とす。** あれは**そのトークンの予算**で、次のアカウントの残量では
        // ない。残すと、ヘッダの `[gh: …]` だけが変わって数が前のまま残る。
        self.rate_limit = None;
        // **座らせる約束も落とす。** 一覧を捨てたので当てる先が無い。
        // **害が出るかは数えていない**（`target` は latch するので一覧は同じ
        // リポジトリのもので、同じ番号は同じ PR）—— 一覧を捨てるなら、それを
        // 指していた記憶も捨てる、という形を揃えているだけ。
        self.created = None;
        self.list_behind = false;
    }

    /// 作った PR の番号を憶える。届いた一覧でその行に座る（01 の「PR 作成（`N`）」）。
    ///
    /// **一覧に足さない。** 作った PR を手で差し込むと、並び（`sort=updated`）も
    /// 集計の鍵も `ui` の出し分けも、**一覧を引いたときとは別の経路**で作られる。
    /// 番号だけ憶えて引き直すほうが、産出元が 1 本で済む。
    pub(in crate::app) fn seat_on_created(&mut self, number: u64, list_behind: bool) {
        self.created = Some(number);
        self.list_behind = list_behind;
    }

    /// 作った直後の一覧が落とされていたか。**読んだら落とす**（1 回きり）。
    pub(in crate::app) fn take_list_behind(&mut self) -> bool {
        std::mem::take(&mut self.list_behind)
    }

    /// 飛んでいる一覧の返事を捨てる印を立てる（[`PrState::list_orphaned`]）。
    pub(in crate::app) fn orphan_list(&mut self) {
        self.list_orphaned = true;
    }

    /// 届いた一覧の返事を捨てるかを答え、印を落とす。
    pub(in crate::app) fn take_list_orphaned(&mut self) -> bool {
        std::mem::take(&mut self.list_orphaned)
    }

    /// まだ座れていない作成の記憶があるか。
    pub(in crate::app) fn has_unseated_creation(&self) -> bool {
        self.created.is_some()
    }

    /// 座り先の記憶を落とす。**作成より後に頼んだ一覧に居なければ、もう出ない。**
    pub(in crate::app) fn forget_created(&mut self) {
        self.created = None;
    }
}

/// PR 作成の `head` の候補 1 つ（01 の「head の組み立て」）。
///
/// **owner だけを持ち、リポジトリ名は持たない。** `POST /pulls` の `head` が運べるのは
/// `<owner>:<branch>` までなので、同じ owner の 2 つのリポジトリは送る値の上で
/// 区別できない —— 持つと、区別できない 2 つを別の候補として並べることになる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrHead {
    pub owner: String,
    pub branch: String,
}

impl PrHead {
    /// 欄に出す字。**常に `<owner>:<branch>`** —— どのリポジトリから出すかを
    /// 人に確かめさせる欄なので、送る値（`PrHead::api_value`）と違って owner を
    /// 落とさない。
    pub fn label(&self) -> String {
        format!("{}:{}", self.owner, self.branch)
    }

    /// `POST /pulls` の `head`。**`target` と同じ owner ならブランチ名だけ**、違えば
    /// `<owner>:<branch>`（gh の `QualifiedHeadRef` と同じ形）。GitHub の login は
    /// 大文字小文字を区別しないので、比べるときもそうする。
    pub(in crate::app) fn api_value(&self, target_owner: &str) -> String {
        if self.owner.eq_ignore_ascii_case(target_owner) {
            self.branch.clone()
        } else {
            self.label()
        }
    }
}

/// [`Modal::PrCreate`](super::Modal::PrCreate) の `head` の候補と、選んでいる位置（01 の「head の組み立て」）。
///
/// **カーソルを持ち主の型に包む**（`BranchState::move_cursor` /
/// `CommitFiles::move_cursor` と同じ）。一覧とカーソルを `Modal` のフィールドに
/// 並べると、端の扱い（`stepped_index`）を `Modal` に置くことになり、
/// **どの欄に居るときだけ動かすか**の判断と割れる。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrHeads {
    list: Vec<PrHead>,
    cursor: usize,
}

impl PrHeads {
    /// 先頭を選んで開く。**既定が先頭でよいのは、欄に出て人が確定するから**
    /// （#101 の「候補の一覧の先頭を断定値に使わない」を破らない理由。01）。
    pub fn new(list: Vec<PrHead>) -> Self {
        Self { list, cursor: 0 }
    }

    pub fn list(&self) -> &[PrHead] {
        &self.list
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// 選んでいる候補。候補が空なら `None`（開いているあいだに消えた形）。
    pub fn selected(&self) -> Option<&PrHead> {
        self.list.get(self.cursor)
    }

    /// `←` / `→`。**端で止まる**（ペインのカーソルと同じ `stepped_index`）。
    pub fn move_cursor(&mut self, delta: isize) {
        self.cursor = stepped_index(self.cursor, self.list.len(), delta);
    }
}
