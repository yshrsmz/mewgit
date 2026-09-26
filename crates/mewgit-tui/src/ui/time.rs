//! 日時を文字列にする。設計は `docs/design/07-ui.md` の「詳細の展開」と「ステータスバー」。
//!
//! **`Timestamp` が持っているオフセットで出す。** コミットの日時はコミットに刻まれた
//! タイムゾーン（`git show` と同じで、見る人の環境には寄せない）、通知の時刻は
//! 刻んだときのローカルのタイムゾーン（`Timestamp::now`）で、どちらも
//! 「秒数 + オフセット」から暦の日付を起こすことだけで足り、tz データベースは要らない。
//!
//! そのためだけに日時のクレートを足していない。ここでやることは純粋関数に収まり、
//! テストで固定できる。

use mewgit_core::git_read::Timestamp;

/// `2026-09-06 15:35:55 +0900` の形にする。
pub fn format(time: Timestamp) -> String {
    let (year, month, day, secs) = wall_clock(time);

    let sign = if time.offset_minutes < 0 { '-' } else { '+' };
    let offset = time.offset_minutes.unsigned_abs();

    format!(
        "{year:04}-{month:02}-{day:02} {} {sign}{:02}{:02}",
        hms(secs),
        offset / 60,
        offset % 60,
    )
}

/// `15:35:55` の形にする。ステータスバーの通知に添える（07 の「ステータスバー」）。
pub fn clock(time: Timestamp) -> String {
    hms(wall_clock(time).3)
}

/// オフセットを足した「そのタイムゾーンでの壁時計」に直して、`(年, 月, 日, その日の秒)`。
fn wall_clock(time: Timestamp) -> (i64, u32, u32, i64) {
    let local = time.seconds + i64::from(time.offset_minutes) * 60;
    let (year, month, day) = civil_from_days(local.div_euclid(SECONDS_PER_DAY));
    (year, month, day, local.rem_euclid(SECONDS_PER_DAY))
}

fn hms(secs: i64) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

const SECONDS_PER_DAY: i64 = 86_400;

/// Unix epoch からの日数を `(年, 月, 日)` にする。
///
/// Howard Hinnant の `civil_from_days`。3 月を年始とみなす暦に置き換えることで、
/// 閏日を年の末尾に寄せて場合分けを消している。400 年周期（`era`）で割ってから
/// 周期内の日数（`doe`）を数える。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // 0000-03-01 を起点にずらす。
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    // 周期内の年。100 年ごと・400 年ごとの閏の例外をここで戻す。
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    // 3 月始まりの月番号（0 = 3 月）。
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);

    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64, offset_minutes: i32) -> String {
        format(Timestamp {
            seconds,
            offset_minutes,
        })
    }

    #[test]
    fn the_clock_is_the_wall_clock_of_the_offset() {
        // 日付をまたぐ側で見る。UTC の 22 時は東京では翌朝 7 時。
        let time = |offset_minutes| Timestamp {
            seconds: 1_700_000_000,
            offset_minutes,
        };
        assert_eq!(clock(time(0)), "22:13:20");
        assert_eq!(clock(time(540)), "07:13:20");
    }

    #[test]
    fn epoch_in_utc() {
        assert_eq!(at(0, 0), "1970-01-01 00:00:00 +0000");
    }

    #[test]
    fn the_offset_shifts_the_wall_clock_and_the_date() {
        // 2023-11-14 22:13:20 UTC は東京では翌日の 07:13:20。
        assert_eq!(at(1_700_000_000, 0), "2023-11-14 22:13:20 +0000");
        assert_eq!(at(1_700_000_000, 540), "2023-11-15 07:13:20 +0900");
    }

    #[test]
    fn a_negative_offset_goes_backwards() {
        assert_eq!(at(1_700_000_000, -300), "2023-11-14 17:13:20 -0500");
        // 日付をまたいで前日に戻るケース。
        assert_eq!(at(1_700_000_000, -1380), "2023-11-13 23:13:20 -2300");
    }

    #[test]
    fn an_offset_that_is_not_a_whole_hour_keeps_its_minutes() {
        // インド（+05:30）やネパール（+05:45）のような 30 分・45 分刻み。
        assert_eq!(at(1_700_000_000, 330), "2023-11-15 03:43:20 +0530");
        assert_eq!(at(1_700_000_000, -210), "2023-11-14 18:43:20 -0330");
    }

    #[test]
    fn leap_days_land_on_february_29() {
        // 2024 は閏年、2100 は（400 で割り切れないので）閏年ではない。
        assert_eq!(at(1_709_164_800, 0), "2024-02-29 00:00:00 +0000");
        assert_eq!(at(4_107_542_400, 0), "2100-03-01 00:00:00 +0000");
    }

    #[test]
    fn dates_before_the_epoch_do_not_wrap() {
        // 秒数は符号付き。commit の日時に負の値が入っていても暦が壊れないこと。
        assert_eq!(at(-1, 0), "1969-12-31 23:59:59 +0000");
        assert_eq!(at(-86_400, 0), "1969-12-31 00:00:00 +0000");
    }

    #[test]
    fn the_year_boundary_is_not_off_by_one() {
        assert_eq!(at(1_735_689_599, 0), "2024-12-31 23:59:59 +0000");
        assert_eq!(at(1_735_689_600, 0), "2025-01-01 00:00:00 +0000");
    }
}
