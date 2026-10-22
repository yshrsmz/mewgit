/// 世代を数える領域。**書き込みが何を古くするか**で分ける。
///
/// **1 本のカウンタにしない。** index だけを触る `stage` でもログの世代が進み、
/// 飛んでいる `LoadLog` を毎回捨てて `LogSession` の作り直し（履歴全体を辿り直す。
/// 02 の「セッションは作り直さず使い回す」）を払うことになる。どの書き込みがどこを
/// 古くするかは `update::Reload` が持っている（01 の「実行中ジョブ › 世代」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// index とワークツリー。[`JobKind::LoadStatus`] と [`JobKind::LoadDiff`]。
    Worktree,
    /// ref の索引（ラベル・HEAD・ブランチ一覧・HEAD のメッセージ）。
    Refs,
    /// 積んだログの行。[`JobKind::LoadLog`]。
    Log,
    /// stash の一覧（`refs/stash` の reflog）。[`JobKind::LoadStashes`]。
    ///
    /// **`Refs` に相乗りさせない。** stash の書き込みで `Refs` を進めると、走っている
    /// PR 一覧の取得（GitHub の往復）とブランチ一覧が巻き添えで捨てられ、逆に switch
    /// などの `Refs` の書き込み（一覧を読み直さない）のあとは、一覧が古いと判じられて
    /// 最初の apply / pop / drop が必ず 1 回断られる（01 の「stash の書き込み」）。
    Stash,
}

impl Region {
    /// 世代を持つ領域すべて。[`JobTracker`] は `region as usize` でカウンタを引き、
    /// その配列の幅をここの長さで取る（並びは問わない）。
    ///
    /// 腕を足してここに足し忘れると添字が範囲外になるので、`all_variants!` の網羅
    /// `match` でコンパイルエラーにする。
    pub const ALL: &[Region] = &all_variants!(Region {
        Worktree,
        Refs,
        Log,
        Stash,
    });
}

/// 実行中の非同期タスク。スピナー表示と `Cmd` の重複発行防止に使う。
///
/// **世代（epoch）も持つ。** 走らせた時点の世代をジョブに刻み、[`finish`](Self::finish)
/// が返すので、届いた返信が直近の書き込みより後に頼まれたものかを受け取り側で判じ
/// られる（01 の「実行中ジョブ › 世代」）。リーダースレッドには何も足さない
/// （`Cmd` / `Msg` は世代を運ばない）。
#[derive(Debug, Default)]
pub struct JobTracker {
    running: Vec<Job>,
    /// 領域ごとの世代。添字は `region as usize`、幅は [`Region::ALL`] の長さ。
    epochs: [u64; Region::ALL.len()],
}

impl JobTracker {
    /// この種別のタスクが動いているか。同種の `Cmd` を二重に投げないための判定。
    pub fn is_running(&self, kind: JobKind) -> bool {
        self.running.iter().any(|job| job.kind == kind)
    }

    /// この領域のいまの世代。持っている値がこれと同じなら最新。
    pub fn epoch(&self, region: Region) -> u64 {
        self.epochs[region as usize]
    }

    /// 書き込みが 1 つ終わり、この領域の値が古くなった。
    ///
    /// **[`AppState::issue`](super::AppState::issue) より先に呼ぶ。** 進めたあとに発行した `Cmd` が新しい世代で
    /// 刻まれる。逆にすると、書き込みの直後に頼んだ読み直しまで「古い要求」になり、
    /// 届いた時点で捨てて頼み直す往復を延々と繰り返す。
    ///
    /// **失敗した書き込みでも進める。** `git apply` が途中で落ちても index は動いて
    /// いることがある。どこを進めるかは `update::Reload` が成否ごと決める。
    pub fn advance(&mut self, region: Region) {
        self.epochs[region as usize] += 1;
    }

    /// 開始を記録する。同種が既に動いていれば何もせず `false` を返す。
    pub fn start(&mut self, mut job: Job) -> bool {
        if self.is_running(job.kind) {
            return false;
        }
        job.epoch = job.kind.region().map(|region| self.epoch(region));
        self.running.push(job);
        true
    }

    /// 完了を記録し、**そのジョブを頼んだ時点の世代**を返す。
    ///
    /// 走っていなければ `None`。領域を持たない種別（[`JobKind::region`] が `None`）も
    /// `None` で、そちらは返り値を読まない。
    pub fn finish(&mut self, kind: JobKind) -> Option<u64> {
        let at = self.running.iter().position(|job| job.kind == kind)?;
        self.running.remove(at).epoch
    }

    /// `asked` の刻印が、`kind` が読む領域のいまの世代と同じか。
    ///
    /// **領域は [`JobKind::region`] から引く。** 呼び出し側に渡させると、
    /// `finish(LoadStatus)` の刻印を `Region::Log` と比べる形が組めてしまい、
    /// 常に「古い」になって頼み直しを回す（`.claude/rules/rust-coding.md` の「旗と値を割らない」が
    /// 1 段上がった形）。
    ///
    /// **`None` は「刻印が無い」**（走っていなかった）。領域を持つ種別では「古い」に
    /// 倒す —— 一度だけ頼み直して、その返信は必ず刻印を持つ。領域を持たない種別は
    /// 書き込みで古くならないので真。
    ///
    /// 引数に取るのは**届いた返信の刻印**か、[`WorktreeState::snapshot_epoch`](super::WorktreeState::snapshot_epoch) のような
    /// **持っている値の刻印**。追いつき（頼み直すか）を決めるときは前者で判じる ——
    /// 後者で判じると、読み取りが失敗し続けたときに「古い → 頼み直す → 失敗」を回す
    /// （失敗は値を入れ替えないので刻印が古いまま残る）。
    pub fn is_fresh(&self, kind: JobKind, asked: Option<u64>) -> bool {
        match kind.region() {
            None => true,
            Some(region) => asked == Some(self.epoch(region)),
        }
    }

    /// 走っているこの種別のジョブに「中断を頼んだ」と刻む。
    ///
    /// **まだ頼んでいなければ真。** 走っていないときと、既に頼んだあとは偽 ——
    /// `Ctrl-C` はそこで終了に落ちる（01 の「キャンセル」）。刻みを持たないと、
    /// SIGTERM で死なない git のあいだ `Ctrl-C` が中断を返し続け、モーダル表示中は
    /// 脱出口が 1 つも無くなる。
    ///
    /// **刻みはジョブと一緒に消える。** [`finish`](Self::finish) が `Job` ごと
    /// 落とすので、次の書き込みは頼んでいない状態から始まる。
    pub fn request_cancel(&mut self, kind: JobKind) -> bool {
        let Some(job) = self.running.iter_mut().find(|job| job.kind == kind) else {
            return false;
        };
        if job.canceling {
            return false;
        }
        job.canceling = true;
        true
    }

    /// この種別のジョブに中断を頼んであるか。走っていなければ偽。
    ///
    /// 読むのは案内文の側（[`AppState::notify_dropped`](super::AppState::notify_dropped)）。頼んだかどうかで
    /// 「次に押すキー」が変わる。
    pub fn cancel_requested(&self, kind: JobKind) -> bool {
        self.running
            .iter()
            .any(|job| job.kind == kind && job.canceling)
    }

    /// 表示用。実行中のタスクを登録順に返す。
    pub fn iter(&self) -> impl Iterator<Item = &Job> {
        self.running.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.running.is_empty()
    }
}

/// 実行中のタスク 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub kind: JobKind,
    /// ヘッダのスピナーに添える短い説明。どの `Cmd` から来たかで決まるので、
    /// [`Cmd::job`](crate::app::Cmd::job) が種別と一緒に返す（`Exec` は `fetch` / `push` のように
    /// 走らせるサブコマンド）。
    pub label: &'static str,
    /// 発行した時点の [`AppState::tick`](super::AppState::tick)。**スピナーは `Tick` を 1 つまたいで走っている
    /// ジョブだけを描く**（07 の「ヘッダ」）—— 1 フレームで終わるもの（2 秒ごとの probe、
    /// 小さなリポジトリの status）まで描くと、そのたびに駒が 1 フレームだけ光り、
    /// `[api: …]` がその幅を譲って消える。
    pub since: u64,
    /// 走らせた時点の、この種別の領域の世代。
    ///
    /// **非公開で、[`JobTracker::start`] だけが入れる。** 組み立てる側に持たせると、
    /// 刻む場所が `start` の外に増える（`issue` の外で `Cmd` を発行する経路ができた
    /// ときに、世代の無いジョブが黙って混ざる）。領域を持たない種別
    /// （[`JobKind::region`] が `None`）では `None` のまま。
    epoch: Option<u64>,
    /// 中断を頼んだか（[`JobTracker::request_cancel`]）。**2 回目の `Ctrl-C` を
    /// 終了に落とすために持つ。** 持たないと、SIGTERM で死なない git のあいだ
    /// `Ctrl-C` が中断を返し続け、モーダル表示中は脱出口が 1 つも無くなる
    /// （`q` は入力文字。01 の「キャンセル」）。
    canceling: bool,
}

impl Job {
    /// スピナーの説明を添えて組む。世代は [`JobTracker::start`] が刻む。
    pub fn new(kind: JobKind, label: &'static str, since: u64) -> Self {
        Self {
            kind,
            label,
            since,
            epoch: None,
            canceling: false,
        }
    }
}

/// タスクの種類。`Cmd` の変種と 1 対 1 で、対応は [`Cmd::job_kind`](crate::app::Cmd::job_kind) が持つ。
///
/// **粒度は種別単位で、対象（Oid やパス）は持たない。** `LoadCommitDetail` では、
/// カーソルが動いたぶんの追いつきを `Msg::CommitDetailLoaded` の側で行うことにした
/// （01 の「コミット詳細の遅延取得」）。Oid まで含めて同一性を取ると、スクロール中の
/// 全コミットぶんの要求が同時に走る。`LoadDiff` も同じ方針で M2 に持ち越す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    LoadLog,
    LoadCommitDetail,
    LoadCommitFiles,
    LoadHeadMessage,
    LoadCommitMessage,
    LoadRepoInfo,
    LoadStatus,
    LoadStashes,
    LoadDiff,
    Exec,
    ResolveTarget,
    ResolveAccount,
    ListAccounts,
    FetchPrList,
    FetchPrDetails,
    CreatePr,
    OpenPr,
    /// 外の変更を見張る指紋（01 の「外部の変更の検知」）。
    Probe,
}

impl JobKind {
    /// この種別が読む値が、どの領域の世代で古くなるか。
    ///
    /// `None` は「書き込みでは古くならない」か、**値そのものが鍵を持っていて世代より
    /// 強い**もの（01 の「実行中ジョブ › 世代」）。コミットの詳細とファイル一覧は
    /// `Msg` が運ぶ Oid で追いつくので、世代では判じない（amend で Oid が変わった
    /// 返信は、鮮度を見るまでもなく別のコミットのものとして捨てられる）。
    ///
    /// 網羅 `match` にしているのは、種別を足したときにどの領域なのかを選ばせるため。
    pub fn region(self) -> Option<Region> {
        match self {
            // index とワークツリー。どの書き込みでも古くなる。
            JobKind::LoadStatus | JobKind::LoadDiff => Some(Region::Worktree),
            // **指紋は書き込みより前の値なら比べてはいけない**（01 の「外部の変更の検知 ›
            // mewgit 自身の書き込みと分ける」）。`Worktree` を進めるのは書き込みと、外の
            // ref の変化（`watch`）だけなので、ここで書き込みを挟んだかが判る。**例外は
            // `Account`**（`.git/config` の 1 行。どの領域も進めない）で、そのときは
            // 次の probe が `config` の差として `Refs` を 1 回余分に読み直す。
            JobKind::Probe => Some(Region::Worktree),
            // ref から導かれる値。`LoadHeadMessage` もここ —— commit / amend が
            // 走っているあいだに `C` を押すと、書き込み前の本文で amend の入力欄が
            // 埋まる。
            // **PR の一覧もここ。** 一覧は選択に紐づかないので鍵が無く、**世代だけ**が
            // 「書き込みで中身が変わった」を言う（commit / push で head sha が進み、
            // CI が回り直し、マージすれば open から消える）。世代を見ないと、書き込みの
            // 最中に飛んでいた要求の答え（書き込み前の GitHub の状態）が採られ、
            // そのあとの引き直しは `AppState::issue` に落とされたまま誰も拾わない。
            JobKind::LoadRepoInfo
            | JobKind::LoadHeadMessage
            | JobKind::FetchPrList => Some(Region::Refs),
            // `refs/stash` の reflog。進めるのは stash の書き込みと、autostash の起きうる
            // `Reload::RefsAndLog`（01 の「Stash ペイン」）。
            JobKind::LoadStashes => Some(Region::Stash),
            // 積んだログの行。ref が動いて walk に入る集合が変わったときだけ古くなる。
            JobKind::LoadLog => Some(Region::Log),
            // **PR ごとの集計は鍵のほうが強い。** 答えは `(番号, head sha)` を
            // 運び、受け取り側が**いまの一覧**と突き合わせる（`PrState::detail_loaded`）
            // ので、書き込みの最中に飛んでいた答えは head sha が進んだ時点で捨てられる。
            // **世代を足しても増える守りが無い** —— 世代で「古い」と判じたところで、
            // 集計を憶えている行は `PrState::wanted_details` が拾わないので頼み直しが
            // 出ない。**sha が変わらないまま中身が変わる**（レビューが付く / CI が
            // 回り直す）のは世代でも鍵でも拾えないので、そこは人が `r` を押して
            // 記憶を落とす（01 の「引き直す（`r`）」）。
            JobKind::FetchPrDetails
            // 書き込みそのものと、Oid で追いつくもの、リポジトリの外から来るもの。
            | JobKind::Exec
            | JobKind::LoadCommitDetail
            | JobKind::LoadCommitFiles
            | JobKind::LoadCommitMessage
            // `ssh -G` はリポジトリの ref を読まない。読むのは `.git/config` の
            // リモート URL と `~/.ssh/config` で、どちらも git の書き込みで動かない。
            | JobKind::ResolveTarget
            // **`.git/config` の `mewgit.user` は `GitOp::SetAccount` / `UnsetAccount` が
            // 書く**が、世代は要らない —— その書き込みとは重ならず
            // （`update::account::submit_account_write`）、終われば必ず頼み直す。
            | JobKind::ResolveAccount
            // `gh auth status` が読むのは gh 自身の設定で、git の書き込みで動かない。
            | JobKind::ListAccounts
            // **`OpenPr` は値を読まない**（ブラウザに渡すだけ）。
            | JobKind::OpenPr
            // **`CreatePr` が返すのは作った PR そのもの。** 書き込みに追い越されて
            // 古くなるという問いが無い（番号は変わらない）。一覧のほうは
            // `FetchPrList` が世代で追いつく。
            | JobKind::CreatePr => None,
        }
    }
}

/// GitHub の材料（`RepoInfo.target` とアカウント）を使って走るジョブ。ランタイムが
/// 材料を取れずに走らせなかったときに、畳む種別と通知に出す名前を引く。
///
/// **`JobKind` から絞った別の型にした理由は 01 の「実行中ジョブ」。**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GithubJob {
    FetchPrList,
    FetchPrDetails,
    CreatePr,
    OpenPr,
}

impl GithubJob {
    /// 全変種。名前を回すテストが、足した変種を見落とさないための一覧。
    pub const ALL: &[GithubJob] = &all_variants!(GithubJob {
        FetchPrList,
        FetchPrDetails,
        CreatePr,
        OpenPr,
    });

    /// 畳むジョブの種別。
    pub fn kind(self) -> JobKind {
        match self {
            GithubJob::FetchPrList => JobKind::FetchPrList,
            GithubJob::FetchPrDetails => JobKind::FetchPrDetails,
            GithubJob::CreatePr => JobKind::CreatePr,
            GithubJob::OpenPr => JobKind::OpenPr,
        }
    }
}
