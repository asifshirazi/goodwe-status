//! Station-local wall-clock times. SEMS sends times with no zone, and the Mac may be in
//! another zone, so the app never mixes them with the Mac's clock: a `LocalTs` is seconds
//! since 1970-01-01 00:00 *on the station's wall clock*, and time moves on by the app's own
//! monotonic clock (see `Monitor::station_now`).

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct LocalTs(pub i64);

impl LocalTs {
    /// "MM/DD/YYYY HH:MM:SS" (classic `info.time`) or "YYYY-MM-DD HH:MM:SS" (SEMS+ `tp`).
    pub fn parse(s: &str) -> Option<LocalTs> {
        let (date, time) = s.trim().split_once(' ')?;
        let (y, m, d) = if date.contains('/') {
            let mut it = date.split('/');
            let (m, d, y) = (it.next()?, it.next()?, it.next()?);
            (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?)
        } else {
            let mut it = date.split('-');
            (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?)
        };
        let mut t = time.split(':');
        let h: i64 = t.next()?.parse().ok()?;
        let mi: i64 = t.next()?.parse().ok()?;
        let sec: i64 = t.next().unwrap_or("0").parse().ok()?;
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
            return None;
        }
        Some(LocalTs(days_from_civil(y, m, d) * 86_400 + h * 3600 + mi * 60 + sec))
    }

    pub fn day(self) -> i64 {
        self.0.div_euclid(86_400)
    }
    pub fn minute_of_day(self) -> u32 {
        (self.0.rem_euclid(86_400) / 60) as u32
    }
    /// "08:20", 24 h.
    pub fn hhmm(self) -> String {
        let m = self.minute_of_day();
        format!("{:02}:{:02}", m / 60, m % 60)
    }
    /// "1:36 PM", 12 h, no leading zero: what the user sees. A no-break space keeps AM/PM with
    /// its time when a row wraps.
    pub fn hhmm12(self) -> String {
        let m = self.minute_of_day();
        let h = m / 60;
        format!("{}:{:02}\u{a0}{}", match h % 12 { 0 => 12, x => x }, m % 60, if h < 12 { "AM" } else { "PM" })
    }
    /// "2026-10-18", for the SEMS+ chart call.
    pub fn date(self) -> String {
        let (y, m, d) = civil_from_days(self.day());
        format!("{y:04}-{m:02}-{d:02}")
    }
    /// "18 Oct", for the chart read-out.
    pub fn day_month(self) -> String {
        let (_, m, d) = civil_from_days(self.day());
        let mon = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][(m - 1) as usize];
        format!("{d} {mon}")
    }
    pub fn plus_secs(self, s: i64) -> LocalTs {
        LocalTs(self.0 + s)
    }
    /// "Saturday, 17 October 2026" (Stats date button).
    pub fn long_date(self) -> String {
        format!("{}, {}", WEEKDAYS[self.weekday()], self.day_month_year())
    }
    /// "18 September 2026".
    pub fn day_month_year(self) -> String {
        format!("{} {}", self.day_month_long(), civil_from_days(self.day()).0)
    }
    /// "17 October".
    pub fn day_month_long(self) -> String {
        let (_, m, d) = civil_from_days(self.day());
        format!("{d} {}", MONTHS[(m - 1) as usize])
    }
    /// "Wed" (a midnight cut's other day).
    pub fn weekday_short(self) -> &'static str {
        &WEEKDAYS[self.weekday()][..3]
    }
    /// "October 2026" (calendar head).
    pub fn month_year(self) -> String {
        let (y, m, _) = civil_from_days(self.day());
        format!("{} {y}", MONTHS[(m - 1) as usize])
    }
    /// The day number of the 1st of `day`'s month, `step` months on (calendar).
    pub fn month_start(day: i64, step: i64) -> i64 {
        let (y, m, _) = civil_from_days(day);
        let t = y * 12 + m - 1 + step;
        days_from_civil(t.div_euclid(12), t.rem_euclid(12) + 1, 1)
    }
    /// 0 = Sunday.
    pub fn weekday(self) -> usize {
        (self.day() + 4).rem_euclid(7) as usize // 1970-01-01 was a Thursday
    }
}

const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

// Howard Hinnant's civil date algorithms (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_formats_to_the_same_instant() {
        let a = LocalTs::parse("10/18/2026 08:21:38").unwrap();
        let b = LocalTs::parse("2026-10-18 08:21:38").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.hhmm(), "08:21");
        assert_eq!(a.minute_of_day(), 8 * 60 + 21);
        assert_eq!(a.date(), "2026-10-18");
    }

    #[test]
    fn crosses_midnight_and_month_ends() {
        let t = LocalTs::parse("12/31/2026 23:58:00").unwrap().plus_secs(180);
        assert_eq!((t.date(), t.hhmm()), ("2027-01-01".into(), "00:01".into()));
        assert_eq!(LocalTs::parse("2028-02-29 00:00:00").unwrap().date(), "2028-02-29");
        assert_eq!(LocalTs::parse("1970-01-01 00:00:00"), Some(LocalTs(0)));
    }

    #[test]
    fn twelve_hour_text() {
        let t = |s: &str| LocalTs::parse(&format!("2026-10-18 {s}:00")).unwrap().hhmm12();
        assert_eq!(t("00:00"), "12:00\u{a0}AM");
        assert_eq!(t("00:59"), "12:59\u{a0}AM");
        assert_eq!(t("01:05"), "1:05\u{a0}AM");
        assert_eq!(t("11:59"), "11:59\u{a0}AM");
        assert_eq!(t("12:05"), "12:05\u{a0}PM");
        assert_eq!(t("13:36"), "1:36\u{a0}PM");
        assert_eq!(t("23:58"), "11:58\u{a0}PM");
        assert!(!t("13:36").contains(' '), "AM/PM must not wrap away from its time");
    }

    #[test]
    fn stats_dates() {
        let t = |s: &str| LocalTs::parse(&format!("{s} 12:00:00")).unwrap();
        assert_eq!(t("2026-10-17").long_date(), "Saturday, 17 October 2026");
        assert_eq!(t("2026-09-18").long_date(), "Friday, 18 September 2026");
        assert_eq!(t("2026-10-14").weekday_short(), "Wed");
        assert_eq!(t("2028-02-29").long_date(), "Tuesday, 29 February 2028");
        assert_eq!(t("1969-12-31").weekday_short(), "Wed");
        assert_eq!(t("2026-10-01").day_month_long(), "1 October");
        // calendar months: lengths, leap years, year ends, weekday of the 1st
        let len = |s: &str| LocalTs::month_start(t(s).day(), 1) - LocalTs::month_start(t(s).day(), 0);
        assert_eq!([len("2026-10-19"), len("2026-09-01"), len("2026-02-10"), len("2028-02-10"), len("2100-02-01"), len("2000-02-01")], [31, 30, 28, 29, 28, 29]);
        assert_eq!(LocalTs::month_start(t("2026-12-31").day(), 1), t("2027-01-01").day());
        assert_eq!(LocalTs::month_start(t("2026-01-15").day(), -1), t("2025-12-01").day());
        assert_eq!((t("2026-10-01").weekday(), t("2026-09-01").weekday()), (4, 2)); // Thu, Tue
        assert_eq!(t("2026-10-19").month_year(), "October 2026");
    }

    #[test]
    fn rejects_garbage() {
        for s in ["", "10/18/2026", "13/01/2026 00:00:00", "2026-10-18 25:00:00", "x y"] {
            assert_eq!(LocalTs::parse(s), None, "{s}");
        }
    }
}
