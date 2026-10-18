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
    /// "2026-10-18", for the SEMS+ chart call.
    pub fn date(self) -> String {
        let (y, m, d) = civil_from_days(self.day());
        format!("{y:04}-{m:02}-{d:02}")
    }
    pub fn plus_secs(self, s: i64) -> LocalTs {
        LocalTs(self.0 + s)
    }
}

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
    fn rejects_garbage() {
        for s in ["", "10/18/2026", "13/01/2026 00:00:00", "2026-10-18 25:00:00", "x y"] {
            assert_eq!(LocalTs::parse(s), None, "{s}");
        }
    }
}
