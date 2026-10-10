//! 外の変更の検知。設計は `docs/design/01-app-core.md` の「外部の変更の検知」。
//!
//! ref 側の変化は、mewgit の書き込みが 1 つ終わったのと同じ形で読み直す（[`reload`]）。
//! ワークツリー側は status を読むだけで、古くなったかは `status_loaded` と
//! `diff_is_current` が持っている値と比べて決める。鮮度の仕組みは足さない。

use crate::git_read::{GitReadError, RepoFingerprint};

use super::exec::{reload, Reload};
use crate::app::{AppState, Cmd, JobKind, NoticeLevel, WatchState};

/// `Tick` で期限を見る。過ぎていれば probe と定期の status を出す。
///
/// **書き込みの実行中は出さない**（01 の「mewgit 自身の書き込みと分ける」）。読んでも
/// 書き込みの途中の値で、書き込みのあとの読み直しが同じものを読む。期限は進めないので、
/// 書き込みが終わった次の `Tick` で出る。
pub(super) fn tick(state: &mut AppState) -> Cmd {
    if state.jobs.is_running(JobKind::Exec) {
        return Cmd::None;
    }
    let now = state.now;
    // **時計が後ろへ跳んだら期限を引き戻す**（NTP の補正、VM の復帰）。期限は壁時計の
    // 秒なので、引き戻さないと跳んだぶんだけ見張りが止まる（`/code-review` が拾った）。
    state.watch.clamp_to(now, &state.config);

    let probe = if now.seconds >= state.watch.next_probe {
        state.watch.restart_probe(now);
        Cmd::Probe
    } else {
        Cmd::None
    };
    // 期限を数え直すのは `status_loaded`（どの経路の status でも数え直す）。
    let status = match WatchState::status_interval(&state.config) {
        Some(_) if now.seconds >= state.watch.next_status => {
            state.watch.restart_status(now, &state.config);
            Cmd::LoadStatus
        }
        _ => Cmd::None,
    };
    Cmd::batch([probe, status])
}

/// 端末が前面に戻った。probe と status をすぐ出し、probe の期限を今から数え直す
/// （status の期限は `status_loaded` が数え直す）。
///
/// **指紋が変わっていなければ何も読み直さない**（[`fingerprint_loaded`]）ので、
/// 有効にした直後に今のフォーカスを送ってくる端末でも害が無い（01）。
pub(super) fn focus_gained(state: &mut AppState) -> Cmd {
    if state.jobs.is_running(JobKind::Exec) {
        return Cmd::None;
    }
    state.watch.restart_probe(state.now);
    Cmd::batch([Cmd::Probe, Cmd::LoadStatus])
}

/// `R`。指紋を見ずに全部を読み直す（01 の「外部の変更の検知 › `R`」）。
///
/// **検知が外れる形からの逃げ道**なので、比べる処理を挟まない。指紋は捨てて、
/// 読み直しと一緒に頼む probe の返りを基準にし直す。**書き込みの実行中も通す** ——
/// 押した人が頼んだ読み直しで、その probe が書き込みより前の値を読めば
/// [`fingerprint_loaded`] が刻印で捨てる。
pub(super) fn reload_all(state: &mut AppState) -> Cmd {
    state.watch.fingerprint = None;
    state.watch.restart_probe(state.now);
    Cmd::batch([reload(state, Reload::RefsAndLog), Cmd::Probe])
}

/// probe の返り。基準と比べ、変わったものを読み直す。
pub(super) fn fingerprint_loaded(
    state: &mut AppState,
    result: Result<RepoFingerprint, GitReadError>,
) -> Cmd {
    let asked = state.jobs.finish(JobKind::Probe);

    let fingerprint = match result {
        Ok(fingerprint) => fingerprint,
        Err(err) => {
            // **最初の 1 回だけ通知する。** 2 秒ごとに積むと通知の履歴が埋まる
            // （01 の「失敗の記憶」の形）。落とすのは次の成功。
            if !state.watch.failed {
                state.watch.failed = true;
                state.push_notice(
                    NoticeLevel::Error,
                    format!("外の変更を確かめられません: {err}"),
                    None,
                );
            }
            return Cmd::None;
        }
    };
    state.watch.failed = false;

    // **書き込みの実行中に届いた返りは比べない**（01 の「mewgit 自身の書き込みと
    // 分ける」）。書き込みの途中の値かもしれず、刻印ではまだ見分けられない（世代を
    // 進めるのは `exec_finished`）。基準は `exec_finished` が捨て、次の probe が取り直す。
    if state.jobs.is_running(JobKind::Exec) {
        return Cmd::None;
    }
    // **書き込みより前に頼んだ返りは基準にもしない。** 基準は `exec_finished` が
    // 捨てている。
    //
    // 続きを頼む: 書き込みより前に頼んだ probe の頼み直し。
    if !state.jobs.is_fresh(JobKind::Probe, asked) {
        return Cmd::Probe;
    }

    // 基準が無い（起動直後と書き込みのあと）なら比べずに基準にするだけ。
    let changed = state
        .watch
        .fingerprint
        .as_ref()
        .map(|before| Changed::between(before, &fingerprint));
    state.watch.fingerprint = Some(fingerprint);
    match changed {
        Some(Changed::Repo(changed)) => reload(state, changed),
        // **ワークツリー側は status を読むだけで、世代を進めない**（01 の「指紋」）。
        // `index` と gitdir の mtime は外の `git status` 1 回でも動く（実測）ので、
        // 古くなったかは届いた status を持っている値と比べて決める。
        Some(Changed::Worktree) => Cmd::LoadStatus,
        Some(Changed::Nothing) | None => Cmd::None,
    }
}

/// 指紋の差が何を古くしたか（01 の「外部の変更の検知 › 指紋」の表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Changed {
    /// ref・追跡設定・stash のどれか。書き込みと同じ読み直しを出す。
    Repo(Reload),
    /// ワークツリーごとの gitdir の mtime だけ。status を読むきっかけにすぎない。
    Worktree,
    Nothing,
}

impl Changed {
    /// **複数が同時に変わったら、全部を覆う腕を選ぶ。** `Reload` は累積なので
    /// （`Refs` は `Worktree` を含む）上から順に見る —— ただし `config` と `stash` が
    /// 両方変わったときは、両方を覆うのが `RefsAndLog` しか無い。
    fn between(before: &RepoFingerprint, after: &RepoFingerprint) -> Self {
        let refs = before.refs != after.refs;
        let config = before.config != after.config;
        let stash = before.stash != after.stash;

        if refs || (config && stash) {
            Changed::Repo(Reload::RefsAndLog)
        } else if config {
            Changed::Repo(Reload::Refs)
        } else if stash {
            Changed::Repo(Reload::Stashes)
        } else if before.worktree != after.worktree {
            Changed::Worktree
        } else {
            Changed::Nothing
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    use super::super::test_support::{
        diff_ok, diff_state, exec_done, fake_output, finish_exec, press, refs_and_log, state, tick,
    };
    use super::super::update;
    use super::*;
    use crate::app::{Msg, Region};
    use crate::git_exec::GitOp;
    use crate::git_read::{FileStamp, PathStamp, Timestamp, WorktreeSnapshot};
    use crate::test_util::{deliver_status, fake_oid, fake_snapshot, reply, FAKE_NOW};

    const PROBE: i64 = WatchState::PROBE_INTERVAL;

    /// `FAKE_NOW` から `seconds` 秒あとの `Tick`。
    fn tick_after(seconds: i64) -> Msg {
        Msg::Tick(Timestamp {
            seconds: FAKE_NOW.seconds + seconds,
            ..FAKE_NOW
        })
    }

    fn print(refs: u64) -> RepoFingerprint {
        RepoFingerprint {
            refs,
            stash: (None, 0),
            config: None,
            worktree: (None, None),
        }
    }

    fn probed(fingerprint: RepoFingerprint) -> Msg {
        Msg::FingerprintLoaded(Box::new(Ok(fingerprint)))
    }

    /// 基準の指紋を持った状態。
    fn watching(base: RepoFingerprint) -> AppState {
        let mut state = state();
        assert_eq!(reply(&mut state, Cmd::Probe, probed(base)), Cmd::None);
        state
    }

    fn stage() -> GitOp {
        GitOp::Stage(vec!["a.txt".into()])
    }

    #[test]
    fn the_probe_and_the_status_wait_for_their_own_deadlines() {
        let mut state = state();

        // 起動時は `init` が自分で頼むので、最初の `Tick` では出さない。
        assert_eq!(update(&mut state, tick()), Cmd::None);
        assert_eq!(update(&mut state, tick_after(PROBE)), Cmd::Probe);
        // 同じ秒の `Tick` では出し直さない。
        let _ = update(&mut state, probed(print(0)));
        assert_eq!(update(&mut state, tick_after(PROBE)), Cmd::None);

        // status は `general.refresh_interval`（既定 10 秒）ごと。
        let interval = i64::try_from(state.config.general.refresh_interval).unwrap();
        assert_eq!(
            update(&mut state, tick_after(interval)),
            Cmd::Batch(vec![Cmd::Probe, Cmd::LoadStatus])
        );
    }

    #[test]
    fn any_status_restarts_the_periodic_one() {
        // 書き込みのあとの読み直しの直後に、定期のぶんが同じものを読まない。
        let mut state = state();
        let interval = i64::try_from(state.config.general.refresh_interval).unwrap();
        state.now.seconds += interval - 1;
        let _ = deliver_status(&mut state, Ok(WorktreeSnapshot::default()));

        assert_eq!(update(&mut state, tick_after(interval)), Cmd::Probe);
    }

    #[test]
    fn a_refresh_interval_of_zero_stops_only_the_status() {
        let mut state = state();
        state.config.general.refresh_interval = 0;

        assert_eq!(update(&mut state, tick_after(3600)), Cmd::Probe);
    }

    #[test]
    fn nothing_is_probed_while_a_write_runs() {
        // 書き込みの途中の値を外の変更と読まない（01 の「mewgit 自身の書き込みと分ける」）。
        let mut state = state();
        let _ = state.issue(Cmd::Exec(stage()));

        assert_eq!(update(&mut state, tick_after(60)), Cmd::None);
        assert_eq!(update(&mut state, Msg::FocusGained), Cmd::None);

        // 期限は進めていないので、終わった次の `Tick` で出る（status は書き込みの
        // あとの読み直しが走っているので落ちる）。
        let _ = update(&mut state, exec_done(Ok(fake_output())));
        assert_eq!(update(&mut state, tick_after(60)), Cmd::Probe);
    }

    #[test]
    fn the_first_fingerprint_is_only_a_baseline() {
        let mut state = state();
        assert_eq!(reply(&mut state, Cmd::Probe, probed(print(1))), Cmd::None);
        assert_eq!(state.watch.fingerprint, Some(print(1)));
    }

    #[test]
    fn an_unchanged_fingerprint_reloads_nothing() {
        let mut state = watching(print(1));
        assert_eq!(reply(&mut state, Cmd::Probe, probed(print(1))), Cmd::None);
    }

    #[test]
    fn moved_refs_reload_like_a_write_that_moves_them() {
        let mut state = watching(print(1));
        let log = state.jobs.epoch(Region::Log);

        let cmd = reply(&mut state, Cmd::Probe, probed(print(2)));

        assert_eq!(cmd, refs_and_log(&state));
        // 書き込みと同じく世代を進める（飛んでいる読み取りの返りを捨てるため）。
        assert_eq!(state.jobs.epoch(Region::Log), log + 1);
        assert_eq!(state.watch.fingerprint, Some(print(2)));
    }

    #[test]
    fn a_moved_worktree_mtime_only_reads_the_status() {
        // 外の `git status` 1 回でも動く（01 の実測）ので、世代は進めない。
        let mut state = watching(print(1));
        let worktree = state.jobs.epoch(Region::Worktree);
        let moved = RepoFingerprint {
            worktree: (Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1)), None),
            ..print(1)
        };

        assert_eq!(
            reply(&mut state, Cmd::Probe, probed(moved)),
            Cmd::LoadStatus
        );
        assert_eq!(state.jobs.epoch(Region::Worktree), worktree);
    }

    #[test]
    fn each_difference_picks_the_smallest_reload_that_covers_it() {
        let base = print(1);
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        let config = RepoFingerprint {
            config: Some(at),
            ..base.clone()
        };
        let stash = RepoFingerprint {
            stash: (Some(fake_oid(7)), 1),
            ..base.clone()
        };
        let both = RepoFingerprint {
            config: Some(at),
            stash: (Some(fake_oid(7)), 1),
            ..base.clone()
        };
        let all = RepoFingerprint {
            worktree: (Some(at), None),
            ..both.clone()
        };

        assert_eq!(
            Changed::between(&base, &config),
            Changed::Repo(Reload::Refs)
        );
        assert_eq!(
            Changed::between(&base, &stash),
            Changed::Repo(Reload::Stashes)
        );
        assert_eq!(
            Changed::between(&base, &both),
            Changed::Repo(Reload::RefsAndLog)
        );
        assert_eq!(
            Changed::between(&base, &print(2)),
            Changed::Repo(Reload::RefsAndLog)
        );
        // ref 側が変わっていれば、ワークツリー側はそちらの読み直しが覆う。
        assert_eq!(
            Changed::between(&base, &all),
            Changed::Repo(Reload::RefsAndLog)
        );
        assert_eq!(Changed::between(&base, &base), Changed::Nothing);
    }

    #[test]
    fn a_write_drops_the_baseline_so_its_own_changes_are_not_external() {
        let mut state = watching(print(1));
        finish_exec(&mut state, stage(), Ok(fake_output()));

        assert_eq!(state.watch.fingerprint, None);
        // 書き込みのあとの指紋は基準になるだけ。
        assert_eq!(reply(&mut state, Cmd::Probe, probed(print(2))), Cmd::None);
        assert_eq!(state.watch.fingerprint, Some(print(2)));
    }

    #[test]
    fn a_probe_asked_before_a_write_is_not_even_a_baseline() {
        let mut state = watching(print(1));
        let _ = state.issue(Cmd::Probe);
        finish_exec(&mut state, stage(), Ok(fake_output()));

        // 書き込みの途中に読んだかもしれない値。捨てて頼み直す。
        assert_eq!(update(&mut state, probed(print(2))), Cmd::Probe);
        assert_eq!(state.watch.fingerprint, None);
    }

    #[test]
    fn an_external_edit_seen_by_the_status_first_does_not_hide_the_refs() {
        // 外で commit すると status も ref も変わる。status が先に届いても、飛んでいる
        // probe は捨てない —— status の差は世代を進めない（`diff_is_current` が
        // `DiffView::stamp` で比べる）。
        let mut state = watching(print(1));
        let _ = deliver_status(&mut state, Ok(fake_snapshot(&["a.txt"], &[], &[])));
        let _ = state.issue(Cmd::Probe);
        let worktree = state.jobs.epoch(Region::Worktree);
        let _ = deliver_status(&mut state, Ok(WorktreeSnapshot::default()));
        assert_eq!(state.jobs.epoch(Region::Worktree), worktree);

        let cmd = update(&mut state, probed(print(2)));

        assert_eq!(cmd, refs_and_log(&state));
    }

    #[test]
    fn a_failing_probe_is_noticed_once_until_it_succeeds() {
        let mut state = watching(print(1));
        let failed = || {
            Msg::FingerprintLoaded(Box::new(Err(GitReadError::BareRepository {
                path: PathBuf::from("/x"),
            })))
        };

        let _ = reply(&mut state, Cmd::Probe, failed());
        let _ = reply(&mut state, Cmd::Probe, failed());
        assert_eq!(state.notices.len(), 1);

        // 成功で落とすので、次の失敗はまた出る。
        let _ = reply(&mut state, Cmd::Probe, probed(print(1)));
        let _ = reply(&mut state, Cmd::Probe, failed());
        assert_eq!(state.notices.len(), 2);
    }

    #[test]
    fn regaining_focus_probes_and_reads_the_status_at_once() {
        let mut state = state();
        assert_eq!(
            update(&mut state, Msg::FocusGained),
            Cmd::Batch(vec![Cmd::Probe, Cmd::LoadStatus])
        );
        // probe の期限は今から数え直す。
        assert_eq!(state.watch.next_probe, FAKE_NOW.seconds + PROBE);
    }

    #[test]
    fn capital_r_reloads_everything_without_looking_at_the_fingerprint() {
        let mut state = watching(print(1));

        let cmd = press(&mut state, 'R');

        assert_eq!(cmd, Cmd::batch([refs_and_log(&state), Cmd::Probe]));
        assert_eq!(state.watch.fingerprint, None);
    }

    /// 指紋（ワークツリーの mtime が `worktree` 秒、index の blob が `index`）の付いた status。
    fn stamped(staged: &[&str], unstaged: &[&str], worktree: u64, index: u8) -> WorktreeSnapshot {
        let mut snapshot = fake_snapshot(staged, unstaged, &[]);
        snapshot.stamps.insert(
            PathBuf::from("a.txt"),
            PathStamp {
                worktree: Some(FileStamp {
                    mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(worktree),
                    len: 6,
                }),
                index: Some(fake_oid(index)),
                head: None,
            },
        );
        snapshot
    }

    #[test]
    fn an_edit_of_an_already_modified_file_reloads_the_diff() {
        // 一覧は ` M` のまま変わらない（01 の実測）。`stamps` の差で拾う。
        let mut state = diff_state(&["a.txt"], 1);
        let target = state.wanted_diff().expect("対象がありません");
        // stamp の無い snapshot から stamp のあるものへ変わったので読み直す（前提）。
        assert_eq!(
            deliver_status(&mut state, Ok(stamped(&[], &["a.txt"], 1, 1))),
            Cmd::LoadDiff(target.clone())
        );
        let _ = update(&mut state, diff_ok(target.clone(), Vec::new()));
        assert!(state.diff_is_current());

        // 同じ stamp なら何も読み直さない。
        let same = deliver_status(&mut state, Ok(stamped(&[], &["a.txt"], 1, 1)));
        assert_eq!(same, Cmd::None);
        assert!(state.diff_is_current());

        // 外で書き換えた。status は頼み直さず、diff だけを読み直す。
        let edited = deliver_status(&mut state, Ok(stamped(&[], &["a.txt"], 2, 1)));
        assert_eq!(edited, Cmd::LoadDiff(target));
        assert!(!state.diff_is_current());
        assert!(state.worktree_is_current());
    }

    #[test]
    fn staging_outside_reloads_the_unstaged_diff() {
        // 外で `git add -p` した。ワークツリーのファイルは動かず、index だけが動く
        // （`/code-review` が拾った形）。
        let mut state = diff_state(&["a.txt"], 2);
        let target = state.wanted_diff().expect("対象がありません");
        let _ = deliver_status(&mut state, Ok(stamped(&[], &["a.txt"], 1, 1)));
        let _ = update(&mut state, diff_ok(target.clone(), Vec::new()));
        assert!(state.diff_is_current());

        let staged = deliver_status(&mut state, Ok(stamped(&["a.txt"], &["a.txt"], 1, 2)));

        assert_eq!(staged, Cmd::LoadDiff(target));
    }

    #[test]
    fn a_probe_arriving_while_a_write_runs_is_not_compared() {
        // 書き込みの途中の値かもしれず、刻印ではまだ見分けられない（`/code-review` が
        // 拾った順序。probe がリーダースレッドで後ろに並び、書き込みの最中に返る）。
        let mut state = watching(print(1));
        let _ = state.issue(Cmd::Probe);
        let _ = state.issue(Cmd::Exec(stage()));

        assert_eq!(update(&mut state, probed(print(2))), Cmd::None);
        assert_eq!(state.watch.fingerprint, Some(print(1)));
    }

    #[test]
    fn a_clock_jumping_back_does_not_stop_the_watch() {
        let mut state = state();
        // 1 時間先から戻ってきた（NTP の補正）。期限は 1 時間先に残っている。
        state.watch.restart_probe(Timestamp {
            seconds: FAKE_NOW.seconds + 3600,
            ..FAKE_NOW
        });

        // 最初の `Tick` で今から 1 周期先へ引き戻し、その周期が来たら出す。引き戻さないと
        // 1 時間止まる。
        assert_eq!(update(&mut state, tick_after(PROBE)), Cmd::None);
        assert_eq!(update(&mut state, tick_after(2 * PROBE)), Cmd::Probe);
    }
}
