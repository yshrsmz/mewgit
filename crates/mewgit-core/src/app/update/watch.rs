//! 外の変更の検知。設計は `docs/design/01-app-core.md` の「外部の変更の検知」。
//!
//! 外で起きた変更を、mewgit の書き込みが 1 つ終わったのと同じ形で読み直す
//! （[`reload`]）。鮮度の仕組みは足さない。

use crate::git_read::{GitReadError, RepoFingerprint};

use super::exec::{reload, Reload};
use crate::app::{AppState, Cmd, JobKind, NoticeLevel};

/// probe の間隔（秒）。lazygit の `externalChangeCheckInterval` と同じ（01）。
pub(crate) const PROBE_INTERVAL: i64 = 2;

/// `Tick` で期限を見る。過ぎていれば probe と定期の status を出す。
///
/// **書き込みの実行中は出さない**（01 の「mewgit 自身の書き込みと分ける」）。期限は
/// 進めないので、書き込みが終わった次の `Tick` で出る。
pub(super) fn tick(state: &mut AppState) -> Cmd {
    if state.jobs.is_running(JobKind::Exec) {
        return Cmd::None;
    }
    let now = state.now.seconds;

    let probe = if now >= state.watch.next_probe {
        state.watch.next_probe = now.saturating_add(PROBE_INTERVAL);
        Cmd::Probe
    } else {
        Cmd::None
    };
    let status = match status_interval(state) {
        Some(interval) if now >= state.watch.next_status => {
            state.watch.next_status = now.saturating_add(interval);
            Cmd::LoadStatus
        }
        _ => Cmd::None,
    };
    Cmd::batch([probe, status])
}

/// 端末が前面に戻った。probe と status をすぐ出し、両方の期限を今から数え直す。
///
/// **指紋が変わっていなければ何も読み直さない**（[`fingerprint_loaded`]）ので、
/// 有効にした直後に今のフォーカスを送ってくる端末でも害が無い（01）。
pub(super) fn focus_gained(state: &mut AppState) -> Cmd {
    if state.jobs.is_running(JobKind::Exec) {
        return Cmd::None;
    }
    restart_deadlines(state);
    Cmd::batch([Cmd::Probe, Cmd::LoadStatus])
}

/// `R`。指紋を見ずに全部を読み直す（01 の「外部の変更の検知 › `R`」）。
///
/// **検知が外れる形からの逃げ道**なので、比べる処理を挟まない。指紋は捨てて、
/// 読み直しと一緒に頼む probe の返りを基準にし直す。
pub(super) fn reload_all(state: &mut AppState) -> Cmd {
    state.watch.fingerprint = None;
    restart_deadlines(state);
    Cmd::batch([reload(state, Reload::RefsAndLog), Cmd::Probe])
}

/// probe の返り。基準と比べ、変わった領域だけを読み直す。
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

    // **書き込みより前に頼んだ返りは基準にもしない**（01 の「mewgit 自身の書き込みと
    // 分ける」）。書き込みの途中の値かもしれない。基準を捨てたまま次の probe を待つ。
    //
    // 続きを頼む: 書き込みより前に頼んだ probe の頼み直し。
    if !state.jobs.is_fresh(JobKind::Probe, asked) {
        state.watch.fingerprint = None;
        return Cmd::Probe;
    }

    match state.watch.fingerprint.replace(fingerprint) {
        // 起動直後と書き込みのあと。比べる相手が無い。
        None => Cmd::None,
        Some(before) => {
            let after = state.watch.fingerprint.as_ref().expect("いま入れた");
            match reload_for(&before, after) {
                Some(changed) => reload(state, changed),
                None => Cmd::None,
            }
        }
    }
}

/// 指紋の差から、読み直しの腕を選ぶ（01 の「外部の変更の検知 › 指紋」の表）。
///
/// **複数が同時に変わったら、全部を覆う腕を選ぶ。** `Reload` は累積なので
/// （`Refs` は `Worktree` を含む）、上から順に見て最初に当たったものが答えになる
/// —— ただし `config` と `stash` が両方変わったときは、両方を覆うのが `RefsAndLog`
/// しか無い。
fn reload_for(before: &RepoFingerprint, after: &RepoFingerprint) -> Option<Reload> {
    let refs = before.refs != after.refs;
    let config = before.config != after.config;
    let stash = before.stash != after.stash;
    let worktree = before.worktree != after.worktree;

    if refs || (config && stash) {
        Some(Reload::RefsAndLog)
    } else if config {
        Some(Reload::Refs)
    } else if stash {
        Some(Reload::Stashes)
    } else if worktree {
        Some(Reload::Nothing)
    } else {
        None
    }
}

/// 定期の status の間隔（秒）。`general.refresh_interval` が 0 なら止める（08）。
fn status_interval(state: &AppState) -> Option<i64> {
    match state.config.general.refresh_interval {
        0 => None,
        seconds => Some(i64::try_from(seconds).unwrap_or(i64::MAX)),
    }
}

/// 両方の期限を今から数え直す。
fn restart_deadlines(state: &mut AppState) {
    let now = state.now.seconds;
    state.watch.next_probe = now.saturating_add(PROBE_INTERVAL);
    if let Some(interval) = status_interval(state) {
        state.watch.next_status = now.saturating_add(interval);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    use super::super::test_support::{
        diff_state, exec_done, fake_output, press, reload as log_reload, state, tick,
    };
    use super::super::update;
    use super::*;
    use crate::app::{Msg, Region};
    use crate::git_exec::GitOp;
    use crate::git_read::{FileStamp, Timestamp, WorktreeSnapshot};
    use crate::test_util::{fake_snapshot, reply, FAKE_NOW};

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
            stash: None,
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

    /// 外の変更で `Reload::RefsAndLog` が出す読み直し。
    fn refs_and_log(state: &AppState) -> Cmd {
        Cmd::Batch(vec![
            Cmd::LoadStatus,
            Cmd::LoadRepoInfo,
            Cmd::LoadBranches,
            Cmd::LoadStashes,
            log_reload(state),
        ])
    }

    #[test]
    fn the_probe_and_the_status_wait_for_their_own_deadlines() {
        let mut state = state();

        // 起動時は `init` が自分で頼むので、最初の `Tick` では出さない。
        assert_eq!(update(&mut state, tick()), Cmd::None);
        assert_eq!(update(&mut state, tick_after(PROBE_INTERVAL)), Cmd::Probe);
        // 同じ秒の `Tick` では出し直さない。
        let _ = reply(&mut state, Cmd::None, probed(print(0)));
        assert_eq!(update(&mut state, tick_after(PROBE_INTERVAL)), Cmd::None);

        // status は `general.refresh_interval`（既定 10 秒）ごと。
        let interval = i64::try_from(state.config.general.refresh_interval).unwrap();
        assert_eq!(
            update(&mut state, tick_after(interval)),
            Cmd::Batch(vec![Cmd::Probe, Cmd::LoadStatus])
        );
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
        let _ = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));

        assert_eq!(update(&mut state, tick_after(60)), Cmd::None);
        assert_eq!(update(&mut state, Msg::FocusGained), Cmd::None);

        // 期限は進めていないので、終わった次の `Tick` で出る。
        let _ = update(&mut state, exec_done(Ok(fake_output())));
        state.jobs.finish(JobKind::LoadStatus);
        assert_eq!(
            update(&mut state, tick_after(60)),
            Cmd::Batch(vec![Cmd::Probe, Cmd::LoadStatus])
        );
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
    fn each_difference_picks_the_smallest_reload_that_covers_it() {
        let base = print(1);
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        let config = RepoFingerprint {
            config: Some(at),
            ..base.clone()
        };
        let stash = RepoFingerprint {
            stash: Some(crate::test_util::fake_oid(7)),
            ..base.clone()
        };
        let worktree = RepoFingerprint {
            worktree: (Some(at), None),
            ..base.clone()
        };
        let both = RepoFingerprint {
            config: Some(at),
            stash: Some(crate::test_util::fake_oid(7)),
            ..base.clone()
        };

        assert_eq!(reload_for(&base, &config), Some(Reload::Refs));
        assert_eq!(reload_for(&base, &stash), Some(Reload::Stashes));
        assert_eq!(reload_for(&base, &worktree), Some(Reload::Nothing));
        assert_eq!(reload_for(&base, &both), Some(Reload::RefsAndLog));
        assert_eq!(reload_for(&base, &print(2)), Some(Reload::RefsAndLog));
        assert_eq!(reload_for(&base, &base), None);
    }

    #[test]
    fn a_write_drops_the_baseline_so_its_own_changes_are_not_external() {
        let mut state = watching(print(1));
        let _ = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));
        let _ = update(&mut state, exec_done(Ok(fake_output())));

        assert_eq!(state.watch.fingerprint, None);
        // 書き込みのあとの指紋は基準になるだけ。
        assert_eq!(reply(&mut state, Cmd::Probe, probed(print(2))), Cmd::None);
        assert_eq!(state.watch.fingerprint, Some(print(2)));
    }

    #[test]
    fn a_probe_asked_before_a_write_is_not_even_a_baseline() {
        let mut state = watching(print(1));
        let _ = state.issue(Cmd::Probe);
        let _ = state.issue(Cmd::Exec(GitOp::Stage(vec!["a.txt".into()])));
        let _ = update(&mut state, exec_done(Ok(fake_output())));

        // 書き込みの途中に読んだかもしれない値。捨てて頼み直す。
        assert_eq!(update(&mut state, probed(print(2))), Cmd::Probe);
        assert_eq!(state.watch.fingerprint, None);
    }

    #[test]
    fn an_external_edit_seen_by_the_status_first_does_not_hide_the_refs() {
        // 外で commit すると status も ref も変わる。status が先に届いて `Worktree` を
        // 進めても、飛んでいる probe は捨てない（`Region::Write`）。
        let mut state = watching(print(1));
        let _ = reply(
            &mut state,
            Cmd::LoadStatus,
            Msg::StatusLoaded(Box::new(Ok(fake_snapshot(&["a.txt"], &[], &[])))),
        );
        let _ = state.issue(Cmd::Probe);
        let worktree = state.jobs.epoch(Region::Worktree);
        let _ = reply(
            &mut state,
            Cmd::LoadStatus,
            Msg::StatusLoaded(Box::new(Ok(WorktreeSnapshot::default()))),
        );
        // status の差で `Worktree` が進んだ（前提）。
        assert_eq!(state.jobs.epoch(Region::Worktree), worktree + 1);

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
        // 期限は今から数え直す。
        assert_eq!(state.watch.next_probe, FAKE_NOW.seconds + PROBE_INTERVAL);
    }

    #[test]
    fn capital_r_reloads_everything_without_looking_at_the_fingerprint() {
        let mut state = watching(print(1));

        let cmd = press(&mut state, 'R');

        assert_eq!(
            cmd,
            Cmd::Batch(vec![
                Cmd::LoadStatus,
                Cmd::LoadRepoInfo,
                Cmd::LoadBranches,
                Cmd::LoadStashes,
                log_reload(&state),
                Cmd::Probe,
            ])
        );
        assert_eq!(state.watch.fingerprint, None);
    }

    #[test]
    fn an_edit_of_an_already_modified_file_reloads_the_diff() {
        // 一覧は ` M` のまま変わらない（01 の実測）。`stamps` の差で拾う。
        let mut state = diff_state(&["a.txt"], 1);
        assert!(state.diff_is_current());
        let edited = |secs| {
            let mut snapshot = fake_snapshot(&[], &["a.txt"], &[]);
            snapshot.stamps.insert(
                PathBuf::from("a.txt"),
                FileStamp {
                    mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
                    len: 6,
                },
            );
            Msg::StatusLoaded(Box::new(Ok(snapshot)))
        };
        let _ = reply(&mut state, Cmd::LoadStatus, edited(1));
        let target = state.wanted_diff().expect("対象がありません");
        let _ = update(
            &mut state,
            super::super::test_support::diff_ok(target.clone(), Vec::new()),
        );
        assert!(state.diff_is_current());

        // 同じ stamp なら何も読み直さない。
        assert_eq!(reply(&mut state, Cmd::LoadStatus, edited(1)), Cmd::None);
        assert!(state.diff_is_current());

        // 外で書き換えた。status は頼み直さず、diff だけを読み直す。
        let cmd = reply(&mut state, Cmd::LoadStatus, edited(2));
        assert_eq!(cmd, Cmd::LoadDiff(target));
        assert!(state.worktree_is_current());
    }
}
