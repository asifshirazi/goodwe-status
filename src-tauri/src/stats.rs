//! Settings › Stats (T-015 hand-off § 3): one station-local day's power cuts, built from the
//! stored history when shown (§ 4.13: cuts are never stored). Every string is formatted here;
//! settings.js only lays them out.

use crate::clock::LocalTs;
use crate::monitor::Monitor;
use crate::outages::{self, DAY};
use crate::rules::{self, fmt_duration};
use serde::Serialize;
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StatsView {
    pub date: String,
    pub is_today: bool,
    pub prev: bool,
    pub next: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_tip: Option<String>,
    /// 'ok' | 'loading' | 'unsaved' | 'nobattery'.
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,
    pub summary: String,
    pub strip: Strip,
    pub rows: Vec<Row>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<[&'static str; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backfill: Option<Backfill>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<Problem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calendar: Option<Calendar>,
}

/// The calendar popover (§ 3 Calendar). Days are 1-based in the month; 0 = not in it.
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    pub month: String,
    pub letters: Vec<&'static str>,
    pub lead: usize,
    pub today: i64,
    pub selected: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus: Option<i64>,
    pub prev: bool,
    pub next: bool,
    /// 'cut' | 'clear' | 'todo' | 'none'; index 0 = day 1.
    pub days: Vec<&'static str>,
}

/// Minutes of the day. `from`: the first known minute (1440 = nothing known); `to`: now, today only.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Strip {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<u32>,
    /// This day's part of each cut, (first, last off minute); `cuts[i]` belongs to `rows[i]`.
    pub cuts: Vec<(u32, u32)>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub off: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub off_day: Option<&'static str>,
    pub on: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_day: Option<&'static str>,
    pub dur: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub running: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub off_unknown: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub on_unknown: bool,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Backfill {
    Count { done: usize, total: usize },
    Paused { paused: String },
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Problem {
    pub text: &'static str,
}

/// The Mac's first weekday, 0 = Sunday (D-O7): `read_first_weekday` sets it at launch.
static FIRST_WEEKDAY: AtomicUsize = AtomicUsize::new(0);

/// Reads the Mac's first weekday from NSCalendar (it applies both the Language & Region choice
/// and the region's own default), through JXA: no Objective-C crate for one number. Sunday if
/// it can't. shortcut: read once per launch; a change in System Settings shows after a relaunch.
pub fn read_first_weekday() -> Option<usize> {
    let out = std::process::Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript", "-e", "ObjC.import('Foundation'); $.NSCalendar.currentCalendar.firstWeekday"])
        .output()
        .ok()?;
    let wd = String::from_utf8_lossy(&out.stdout).trim().parse::<usize>().ok().filter(|d| (1..=7).contains(d))? - 1;
    FIRST_WEEKDAY.store(wd, Ordering::Relaxed);
    Some(wd)
}

const OFFLINE: &str = "Can’t reach SEMS. Showing saved history; new readings load when it’s back.";

fn hm(t: i64) -> String {
    LocalTs(t * 60).hhmm12()
}
fn date_of(d: i64) -> LocalTs {
    LocalTs(d * 86_400)
}

/// The day ← / → go to from `d` (§ 3): the next one with data or not loaded yet, skipping days
/// SEMS had nothing for, never before the first day or after today.
pub fn step_day(m: &Monitor, d: i64, step: i64, today: i64) -> Option<i64> {
    let first = first_day(m, today);
    let no_data = |x: i64| m.history_final.contains(&x) && m.history.get(&x).is_none_or(|g| g.newest.is_none());
    let mut x = d + step;
    while (first..=today).contains(&x) {
        if !no_data(x) {
            return Some(x);
        }
        x += step;
    }
    None
}

/// The station's set-up day; without one (another model), the oldest day loaded.
fn first_day(m: &Monitor, today: i64) -> i64 {
    m.first_day().or(m.history.keys().next().copied()).unwrap_or(today).min(today)
}

/// The panel's strip (T-020 § 3): today's Stats view, whatever day Settings shows, without the
/// date, calendar and footer. `None` without cut history: signed out or no battery.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Cuts {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,
    pub summary: String,
    pub strip: Strip,
    pub rows: Vec<Row>,
}

pub fn today(m: &Monitor, now: Instant) -> Option<Cuts> {
    let v = day_view(m, now, true);
    (v.status != "nobattery" && m.signed_in).then_some(Cuts { status: v.status, msg: v.msg, summary: v.summary, strip: v.strip, rows: v.rows })
}

/// The Stats state for the day it shows (`Monitor::stats_day`, else today).
pub fn view(m: &Monitor, now: Instant) -> StatsView {
    day_view(m, now, false)
}

/// `today_only`: the panel's view, today and no calendar.
fn day_view(m: &Monitor, now: Instant, today_only: bool) -> StatsView {
    let blank = |date: String, status, msg: String| StatsView {
        date,
        is_today: true,
        prev: false,
        next: false,
        prev_tip: None,
        status,
        msg: Some(msg),
        summary: "\u{a0}".into(),
        strip: Strip { from: Some(1440), ..Strip::default() },
        rows: vec![],
        retry: None,
        note: None,
        backfill: None,
        problem: None,
        calendar: None,
    };
    let at = m.station_now(now);
    // shortcut: before a reading there is no station clock, so the date is the UTC one; fine for these dimmed states.
    let utc = || LocalTs(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64));
    if !m.signed_in {
        let v = blank(utc().long_date(), "unsaved", "Sign in to SEMS Portal to see your power cuts.".into());
        return StatsView { retry: Some(["signin", "Sign In…"]), ..v };
    }
    let (Some(at), Some(last)) = (at, m.last.as_ref()) else {
        return blank(utc().long_date(), "loading", format!("Loading {} from SEMS…", utc().day_month_long()));
    };
    if last.battery.is_none() {
        let msg = "Power cuts are worked out from the battery’s readings, and this station has no battery.";
        return blank(at.long_date(), "nobattery", msg.into());
    }
    let today = at.day();
    let first = first_day(m, today);
    let d = if today_only { today } else { m.stats_day.unwrap_or(today).clamp(first, today) };
    let offline = m.failures >= rules::STALE_FAILURES;
    let prev = step_day(m, d, -1, today).is_some();
    let base = StatsView {
        date: date_of(d).long_date(),
        is_today: d == today,
        prev,
        next: step_day(m, d, 1, today).is_some(),
        prev_tip: (!prev).then(|| format!("History starts {}", date_of(first).day_month_year())),
        problem: offline.then_some(Problem { text: OFFLINE }),
        calendar: m.stats_cal.filter(|_| !today_only).map(|c| calendar(m, c, d, first, today, FIRST_WEEKDAY.load(Ordering::Relaxed))),
        backfill: match (m.walk.paused_until, m.walk.total) {
            (_, 0) => None,
            (Some(t), total) => Some(Backfill::Paused {
                paused: format!("SEMS asked the app to slow down. History loading resumes at {} ({} of {total} days).", t.hhmm12(), total - m.walk.left.min(total)),
            }),
            (None, total) => Some(Backfill::Count { done: total - m.walk.left.min(total), total }),
        },
        ..blank(String::new(), "ok", String::new())
    };
    let Some(g) = m.history.get(&d).filter(|g| g.newest.is_some()) else {
        let day = date_of(d).day_month_long();
        return match (m.history_final.contains(&d), offline) {
            // shortcut: ← → and the calendar never pick a day SEMS had nothing for; copy to confirm (Build › Open questions).
            (true, _) => StatsView { msg: Some("No readings on this day.".into()), ..base },
            (false, true) => StatsView { status: "unsaved", msg: Some(format!("{day} isn’t saved yet, and SEMS can’t be reached right now.")), ..base },
            (false, false) => StatsView { status: "loading", msg: Some(format!("Loading {day} from SEMS…")), ..base },
        };
    };

    let (d0, d1) = (d * DAY, (d + 1) * DAY);
    // The first day starts at its first reading: on this station SEMS+ has data from 4:48 PM though
    // `create_time` says 6:28 PM (live, 2026-10-19), so the strip and the note follow the data.
    let created = last.created.filter(|c| c.day() == d).map(|_| g.gaps.iter().find(|x| x.0 == 0).map_or(0, |x| x.1 + 1));
    let from = created.unwrap_or(0);
    let to = (d == today).then(|| last.time.0.div_euclid(60));
    let loaded = |x: i64| m.history.get(&x).is_some_and(|g| g.newest.is_some());
    let pending = |x: i64| !loaded(x) && !m.history_final.contains(&x) && x < today;
    // On the first day, the hours before the station's first minute are no gap: history starts there.
    let gaps: Vec<(u32, u32)> = g.gaps.iter().filter(|x| created.is_none() || (x.0 > 0 && x.1 >= from)).map(|x| (x.0.max(from), x.1)).collect();
    let gap_of = |t: i64, f: fn(&(u32, u32)) -> u32| {
        let day = t.div_euclid(DAY);
        m.history.get(&day).is_some_and(|g| g.gaps.iter().any(|x| f(x) as i64 == t - day * DAY))
    };
    let (mut rows, mut cuts, mut total) = (vec![], vec![], 0);
    let (mut at_least, mut midnight, mut edge_pending) = (false, false, false);
    let outs = outages::outages(&m.history, today);
    for o in outs.iter().filter(|o| o.start < d1 && o.end >= d0) {
        let end = if o.running { to.unwrap_or(o.end + 1).max(o.end + 1) } else { o.end + 1 }; // Grid on, or now
        let off_unknown = o.start == d0 && !loaded(d - 1);
        let on_midnight = !o.running && o.end == d1 - 1 && !loaded(d + 1);
        let on_gap = !o.running && gap_of(o.end + 1, |x| x.0);
        let after_gap = o.start > d0 && gap_of(o.start - 1, |x| x.1);
        edge_pending |= (off_unknown && pending(d - 1)) || (on_midnight && pending(d + 1));
        midnight |= o.start < d0 || end > d1 || off_unknown || on_midnight;
        let unknown = on_gap || after_gap;
        at_least |= unknown;
        let len = fmt_duration(end - o.start);
        let day_of = |t: i64| (t.div_euclid(DAY) != d).then(|| date_of(t.div_euclid(DAY)).weekday_short());
        rows.push(Row {
            off: if off_unknown { "Before midnight".into() } else { hm(o.start) },
            off_day: day_of(o.start).filter(|_| !off_unknown),
            on: match () {
                _ if o.running => String::new(),
                _ if on_midnight => "After midnight".into(),
                _ if on_gap => "No readings".into(),
                _ => hm(end),
            },
            on_day: day_of(end).filter(|_| !o.running && !on_midnight && !on_gap),
            dur: match () {
                _ if o.running => format!("{len} so far"),
                _ if unknown || off_unknown || on_midnight => format!("at least {len}"),
                _ => len,
            },
            running: o.running,
            off_unknown,
            on_unknown: on_midnight || on_gap,
        });
        let (a, b) = (o.start.max(d0), end.min(d1));
        // A running cut reaches now on the strip, a finished one its last off minute.
        cuts.push(((a - d0) as u32, ((b - d0) as u32 - u32::from(!o.running)).min(DAY as u32 - 1)));
        total += b - a;
    }
    let n = rows.len();
    let summary = match n {
        0 if d == today => "So far today: no power cuts.".into(), // shortcut: copy to confirm (Build › Open questions)
        0 => "No power cuts.".into(),
        _ => {
            let what = if n == 1 { "1 power cut".to_owned() } else { format!("{n} power cuts") };
            let tail = if midnight {
                " on this day"
            } else if n > 1 && !at_least && d != today {
                " in total"
            } else {
                ""
            };
            let head = if d == today { "So far today: " } else { "" };
            format!("{head}{what}, off for {}{}{tail}.", if at_least { "at least " } else { "" }, fmt_duration(total))
        }
    };
    let gap_min: u32 = gaps.iter().map(|x| x.1 + 1 - x.0).sum();
    let note = if edge_pending && !offline {
        "Loading the days before and after, to finish the cuts across midnight.".into()
    } else if gaps.len() == 1 {
        format!("No readings from {} to {}.", hm(d0 + gaps[0].0 as i64), hm(d0 + gaps[0].1 as i64 + 1))
    } else if gaps.len() > 1 {
        format!("No readings for {} in total.", fmt_duration(gap_min as i64))
    } else if let Some(c) = created {
        format!("History starts at {} on this day, when the station was set up.", hm(d0 + c as i64))
    } else if midnight {
        "Cuts across midnight show on both days; the total counts only this day’s part.".into()
    } else if d == today {
        format!("{} {}.", if offline { "Today as of" } else { "Updated" }, last.time.hhmm12())
    } else {
        "Cuts of 5 minutes or more, from the inverter’s minute history.".into()
    };
    StatsView {
        msg: (n == 0 && d == today).then(|| "No power cuts so far today.".into()),
        summary,
        strip: Strip { from: created, to: to.map(|t| (t - d0) as u32), cuts, gaps },
        rows,
        note: Some(note),
        ..base
    }
}

/// The month `month` (its 1st) with `sel` picked; `wd` = the first weekday, 0 = Sunday.
fn calendar(m: &Monitor, (month, focus): (i64, Option<i64>), sel: i64, first: i64, today: i64, wd: usize) -> Calendar {
    let next = LocalTs::month_start(month, 1);
    let outs = outages::outages(&m.history, today);
    let days = (month..next)
        .map(|x| match m.history.get(&x).filter(|g| g.newest.is_some()) {
            _ if x < first || x > today => "none",
            None if m.history_final.contains(&x) => "none",
            None => "todo",
            Some(_) if outs.iter().any(|o| o.start < (x + 1) * DAY && o.end >= x * DAY) => "cut",
            Some(_) => "clear",
        })
        .collect();
    let in_month = |x: i64| if (month..next).contains(&x) { x - month + 1 } else { 0 };
    Calendar {
        month: date_of(month).month_year(),
        letters: (0..7).map(|i| ["S", "M", "T", "W", "T", "F", "S"][(wd + i) % 7]).collect(),
        lead: (date_of(month).weekday() + 7 - wd) % 7,
        today: in_month(today),
        selected: in_month(sel),
        focus: focus.map(in_month).filter(|&f| f > 0),
        prev: month > LocalTs::month_start(first, 0),
        next: next <= today,
        days,
    }
}

/// Stats' controls (§ 3 actions: day, today, calendar, cal_month, cal_day, retry). True: the
/// day shown changed (or Try Again), so the walk should look at it first (§ 4.8).
pub fn act(m: &mut Monitor, a: &Value, now: Instant) -> bool {
    let Some(today) = m.station_now(now).map(LocalTs::day) else { return false };
    let first = first_day(m, today);
    let d = m.stats_day.unwrap_or(today).clamp(first, today);
    // Today is None, so the pane follows the station's day past midnight.
    let show = |m: &mut Monitor, x: i64| m.stats_day = (x != today).then_some(x);
    let step = a["step"].as_i64().unwrap_or(0).signum();
    match (a["id"].as_str().unwrap_or_default(), m.stats_cal) {
        ("day", _) if step != 0 => match step_day(m, d, step, today) {
            Some(x) => show(m, x),
            None => return false,
        },
        ("today", _) => m.stats_day = None,
        ("calendar", open) => {
            m.stats_cal = if open.is_some() { None } else { Some((LocalTs::month_start(d, 0), None)) };
            return false;
        }
        ("cal_month", Some((month, _))) if step != 0 => {
            let to = LocalTs::month_start(month, step);
            if to < LocalTs::month_start(first, 0) || to > today {
                return false;
            }
            // focus: a day in the new month; 0 or less counts back from its end (0 = the last day).
            let len = LocalTs::month_start(to, 1) - to;
            let focus = a["focus"].as_i64().map(|f| to - 1 + (if f <= 0 { len + f } else { f }).clamp(1, len));
            m.stats_cal = Some((to, focus));
            return false;
        }
        ("cal_day", Some((month, _))) => {
            let x = month + a["day"].as_i64().unwrap_or(0) - 1;
            if x < first || x > today || (m.history_final.contains(&x) && m.history.get(&x).is_none_or(|g| g.newest.is_none())) {
                return false;
            }
            show(m, x);
            m.stats_cal = None;
        }
        ("retry", _) => {}
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outages::GridDay;
    use crate::sems::{Battery, GridSignal, Reading};
    use serde_json::{json, Value};

    fn day(s: &str) -> i64 {
        LocalTs::parse(&format!("{s} 00:00:00")).unwrap().day()
    }
    /// A day with these off minutes (all loaded), readings up to `newest`, and these gaps.
    fn grid(off: &[(u32, u32)], newest: u32, gaps: Vec<(u32, u32)>) -> GridDay {
        GridDay { off: off.iter().flat_map(|&(a, b)| (a..=b).map(|m| (m, true))).collect(), gaps, newest: Some(newest) }
    }
    /// Signed in, battery, station set up 2026-09-18 18:28; today Monday 19 October 2026 2:14 PM.
    fn station(days: Vec<(&str, GridDay)>) -> Monitor {
        let mut m = Monitor::new(true);
        let r = Reading {
            time: LocalTs::parse("10/19/2026 14:14:00").unwrap(),
            grid: GridSignal::Off,
            vac1: 0.0,
            grid_status: None,
            battery: Some(Battery { soc: 80.0, watts: 900.0, capacity_kwh: Some(5.1) }),
            utc_offset_h: Some(5.0),
            inverter_c: None,
            battery_c: None,
            created: LocalTs::parse("09/18/2026 18:28:53"),
        };
        m.set_station(Some("station-a".into()));
        m.on_reading(r, Instant::now());
        let now = day("2026-10-19") * DAY + 854;
        for (d, g) in days {
            m.on_day(day(d), g, now);
        }
        m
    }
    fn show(m: &mut Monitor, d: &str) -> Value {
        m.stats_day = Some(day(d));
        serde_json::to_value(view(m, Instant::now())).unwrap()
    }
    fn row(off: &str, on: &str, dur: &str) -> Value {
        let t = |s: &str| s.replace(" AM", "\u{a0}AM").replace(" PM", "\u{a0}PM");
        json!({ "off": t(off), "on": t(on), "dur": dur })
    }
    fn with(mut r: Value, extra: Value) -> Value {
        r.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        r
    }

    #[test]
    fn past_day_with_cuts() {
        let mut m = station(vec![("2026-10-17", grid(&[(547, 722), (821, 865), (1195, 1225)], 1439, vec![]))]);
        let v = show(&mut m, "2026-10-17");
        assert_eq!(v["date"], "Saturday, 17 October 2026");
        assert_eq!((v["isToday"].as_bool(), v["prev"].as_bool(), v["next"].as_bool(), v["status"].as_str()), (Some(false), Some(true), Some(true), Some("ok")));
        assert_eq!(v["summary"], "3 power cuts, off for 4h 12m in total.");
        assert_eq!(v["strip"], json!({ "cuts": [[547, 722], [821, 865], [1195, 1225]] }));
        assert_eq!(v["rows"], json!([row("9:07 AM", "12:03 PM", "2h 56m"), row("1:41 PM", "2:26 PM", "45 min"), row("7:55 PM", "8:26 PM", "31 min")]));
        assert_eq!(v["note"], "Cuts of 5 minutes or more, from the inverter’s minute history.");
        assert!(v.get("msg").is_none() && v.get("problem").is_none() && v.get("backfill").is_none());
        // one cut; and none (the box's default line is settings.js's)
        let mut m = station(vec![("2026-10-12", grid(&[(600, 646)], 1439, vec![])), ("2026-10-11", grid(&[], 1439, vec![]))]);
        assert_eq!(show(&mut m, "2026-10-12")["summary"], "1 power cut, off for 47 min.");
        let v = show(&mut m, "2026-10-11");
        assert_eq!((v["summary"].as_str(), v["rows"].as_array().map(Vec::len)), (Some("No power cuts."), Some(0)));
        // a 4-minute dip and an unloaded run are no cuts (the panel's rules)
        let mut g = grid(&[(100, 103)], 1439, vec![]);
        g.off.extend((300..=400).map(|m| (m, false)));
        let mut m = station(vec![("2026-10-10", g)]);
        assert_eq!(show(&mut m, "2026-10-10")["summary"], "No power cuts.");
    }

    #[test]
    fn today_with_a_cut_still_running() {
        let mut m = station(vec![("2026-10-19", grid(&[(72, 124), (778, 852)], 852, vec![]))]);
        m.stats_day = None;
        let v = serde_json::to_value(view(&m, Instant::now())).unwrap();
        assert_eq!((v["date"].as_str(), v["isToday"].as_bool(), v["next"].as_bool()), (Some("Monday, 19 October 2026"), Some(true), Some(false)));
        assert_eq!(v["summary"], "So far today: 2 power cuts, off for 2h 9m.");
        assert_eq!(v["strip"], json!({ "to": 854, "cuts": [[72, 124], [778, 854]] }));
        assert_eq!(v["rows"], json!([row("1:12 AM", "2:05 AM", "53 min"), with(row("12:58 PM", "", "1h 16m so far"), json!({ "running": true }))]));
        assert_eq!(v["note"], "Updated 2:14\u{a0}PM.");
        // SEMS unreachable: today as of the last reading, and the footer says why
        m.failures = 3;
        let v = serde_json::to_value(view(&m, Instant::now())).unwrap();
        assert_eq!((v["note"].as_str(), v["problem"]["text"].as_str()), (Some("Today as of 2:14\u{a0}PM."), Some(OFFLINE)));
    }

    #[test]
    fn panel_strip_is_today_whatever_stats_shows() {
        // T-020: the panel's strip follows today, never the day or calendar picked in Stats.
        let mut m = station(vec![("2026-10-18", grid(&[(600, 700)], 1439, vec![])), ("2026-10-19", grid(&[(72, 124), (778, 852)], 852, vec![]))]);
        m.stats_day = Some(day("2026-10-18"));
        m.stats_cal = Some((day("2026-10-01"), None));
        let c = serde_json::to_value(today(&m, Instant::now())).unwrap();
        assert_eq!(c["summary"], "So far today: 2 power cuts, off for 2h 9m.");
        assert_eq!(c["strip"], json!({ "to": 854, "cuts": [[72, 124], [778, 854]] }));
        assert_eq!(c["rows"][1]["running"], true);
        for k in ["date", "calendar", "note", "backfill", "msg"] {
            assert!(c.get(k).is_none(), "{k}: {c}");
        }
        assert_eq!(m.stats_day, Some(day("2026-10-18")), "Stats keeps its day");
        // no cuts yet today; loading today; no battery and signed out send none
        let mut m = station(vec![("2026-10-19", grid(&[], 852, vec![]))]);
        assert_eq!(today(&m, Instant::now()).map(|c| c.summary), Some("So far today: no power cuts.".into()));
        let mut m2 = station(vec![]);
        let c = today(&m2, Instant::now()).unwrap();
        assert_eq!((c.status, c.msg.as_deref(), c.strip.from), ("loading", Some("Loading 19 October from SEMS…"), Some(1440)));
        m2.last.as_mut().unwrap().battery = None;
        assert_eq!(today(&m2, Instant::now()), None);
        m.reset(false);
        assert_eq!(today(&m, Instant::now()), None);
    }

    #[test]
    fn cuts_across_midnight_on_both_days() {
        let days = || {
            vec![
                ("2026-10-14", grid(&[(1413, 1439)], 1439, vec![])),
                ("2026-10-15", grid(&[(0, 40), (1368, 1439)], 1439, vec![])),
                ("2026-10-16", grid(&[(0, 79)], 1439, vec![])),
            ]
        };
        let mut m = station(days());
        let v = show(&mut m, "2026-10-15");
        assert_eq!(v["summary"], "2 power cuts, off for 1h 53m on this day.");
        assert_eq!(v["strip"]["cuts"], json!([[0, 40], [1368, 1439]]));
        assert_eq!(v["rows"], json!([with(row("11:33 PM", "12:41 AM", "1h 8m"), json!({ "offDay": "Wed" })), with(row("10:48 PM", "1:20 AM", "2h 32m"), json!({ "onDay": "Fri" }))]));
        assert_eq!(v["note"], "Cuts across midnight show on both days; the total counts only this day’s part.");
        // the same cut on the next day
        let v = show(&mut m, "2026-10-16");
        assert_eq!(v["rows"], json!([with(row("10:48 PM", "1:20 AM", "2h 32m"), json!({ "offDay": "Thu" }))]));
        assert_eq!((v["summary"].as_str(), v["strip"]["cuts"].clone()), (Some("1 power cut, off for 1h 20m on this day."), json!([[0, 79]])));
        // D-O3: the neighbours not loaded yet: Before / After midnight, "at least", never 12:00 AM
        let mut m = station(days().into_iter().filter(|d| d.0 == "2026-10-15").collect());
        let v = show(&mut m, "2026-10-15");
        assert_eq!(
            v["rows"],
            json!([with(row("Before midnight", "12:41 AM", "at least 41 min"), json!({ "offUnknown": true })), with(row("10:48 PM", "After midnight", "at least 1h 12m"), json!({ "onUnknown": true }))])
        );
        assert_eq!((v["summary"].as_str(), v["note"].as_str()), (Some("2 power cuts, off for 1h 53m on this day."), Some("Loading the days before and after, to finish the cuts across midnight.")));
        // ...and the walk fetches them first (monitor::walk_order_day_on_screen_first)
        let now = day("2026-10-19") * DAY + 854;
        assert_eq!(m.walk_next(now), Some(day("2026-10-14")));
        m.on_day(day("2026-10-14"), grid(&[(1413, 1439)], 1439, vec![]), now);
        assert_eq!(m.walk_next(now), Some(day("2026-10-16")));
    }

    #[test]
    fn a_gap_is_never_grid_on() {
        let mut m = station(vec![("2026-10-07", grid(&[(318, 362), (1101, 1239)], 1439, vec![(1240, 1379)]))]);
        let v = show(&mut m, "2026-10-07");
        assert_eq!(v["summary"], "2 power cuts, off for at least 3h 4m.");
        assert_eq!(v["strip"], json!({ "cuts": [[318, 362], [1101, 1239]], "gaps": [[1240, 1379]] }));
        assert_eq!(v["rows"][1], with(row("6:21 PM", "No readings", "at least 2h 19m"), json!({ "onUnknown": true })));
        assert_eq!(v["note"], "No readings from 8:40\u{a0}PM to 11:00\u{a0}PM.");
        // a cut right after a gap starts "at least" there; two gaps: their total
        let mut m = station(vec![("2026-10-06", grid(&[(381, 400)], 1439, vec![(300, 380), (1000, 1059)]))]);
        let v = show(&mut m, "2026-10-06");
        assert_eq!((v["rows"][0]["dur"].as_str(), v["note"].as_str()), (Some("at least 20 min"), Some("No readings for 2h 21m in total.")));
    }

    #[test]
    fn first_day_starts_at_set_up() {
        let mut m = station(vec![("2026-09-18", grid(&[(1215, 1261)], 1439, vec![(0, 1107)]))]);
        let v = show(&mut m, "2026-09-18");
        assert_eq!(v["date"], "Friday, 18 September 2026");
        assert_eq!((v["prev"].as_bool(), v["prevTip"].as_str()), (Some(false), Some("History starts 18 September 2026")));
        assert_eq!(v["strip"], json!({ "from": 1108, "cuts": [[1215, 1261]] }));
        assert_eq!((v["summary"].as_str(), v["rows"][0].clone()), (Some("1 power cut, off for 47 min."), row("8:15 PM", "9:02 PM", "47 min")));
        assert_eq!(v["note"], "History starts at 6:28\u{a0}PM on this day, when the station was set up.");
        // the walk never goes earlier, nor do the arrows or a stale stats_day
        assert_eq!(show(&mut m, "2026-09-01")["date"], "Friday, 18 September 2026");
    }

    #[test]
    fn arrows_skip_days_without_data() {
        let mut m = station(vec![("2026-10-04", grid(&[], 1439, vec![]))]);
        // 3 October: SEMS answered empty on 3 launches (D-O11), so it's "no data"
        for _ in 0..3 {
            m.on_day(day("2026-10-03"), GridDay::default(), day("2026-10-19") * DAY);
            m.empty_tried_clear_for_test();
        }
        let today = day("2026-10-19");
        assert_eq!(step_day(&m, day("2026-10-04"), -1, today), Some(day("2026-10-02")));
        assert_eq!(step_day(&m, day("2026-10-02"), 1, today), Some(day("2026-10-04")));
        assert_eq!(step_day(&m, day("2026-09-18"), -1, today), None);
        assert_eq!(step_day(&m, today, 1, today), None);
        assert_eq!(step_day(&m, day("2026-10-01"), 1, today), Some(day("2026-10-02")), "a day not loaded yet can be shown");
    }

    #[test]
    fn states_without_a_day_to_show() {
        // not loaded yet: the walk brings it; can't reach SEMS: say so, with Try Again
        let mut m = station(vec![]);
        let v = show(&mut m, "2026-10-01");
        assert_eq!((v["status"].as_str(), v["msg"].as_str(), v["strip"].clone()), (Some("loading"), Some("Loading 1 October from SEMS…"), json!({ "from": 1440, "cuts": [] })));
        m.failures = 3;
        let v = show(&mut m, "2026-10-01");
        assert_eq!((v["status"].as_str(), v["msg"].as_str()), (Some("unsaved"), Some("1 October isn’t saved yet, and SEMS can’t be reached right now.")));
        assert_eq!(v["problem"]["text"], OFFLINE);
        // no battery: no history at all
        m.last.as_mut().unwrap().battery = None;
        let v = show(&mut m, "2026-10-01");
        assert_eq!((v["status"].as_str(), v["prev"].as_bool(), v["next"].as_bool()), (Some("nobattery"), Some(false), Some(false)));
        assert_eq!(v["msg"], "Power cuts are worked out from the battery’s readings, and this station has no battery.");
        // signed out: Sign Out deleted it all
        m.reset(false);
        let v = show(&mut m, "2026-10-01");
        assert_eq!((v["status"].as_str(), v["msg"].as_str(), v["retry"].clone()), (Some("unsaved"), Some("Sign in to SEMS Portal to see your power cuts."), json!(["signin", "Sign In…"])));
    }

    #[test]
    fn backfill_footer() {
        let mut m = station(vec![]);
        let now = day("2026-10-19") * DAY + 854;
        m.walk_next(now);
        for d in 6..=17 {
            m.on_day(day(&format!("2026-10-{d:02}")), grid(&[], 1439, vec![]), now);
        }
        m.walk_next(now);
        assert_eq!(show(&mut m, "2026-10-17")["backfill"], json!({ "done": 12, "total": 31 }));
        m.walk.paused_until = LocalTs::parse("10/19/2026 14:31:00");
        assert_eq!(
            show(&mut m, "2026-10-17")["backfill"]["paused"],
            "SEMS asked the app to slow down. History loading resumes at 2:31\u{a0}PM (12 of 31 days)."
        );
    }

    #[test]
    fn calendar_and_day_keys() {
        let mut m = station(vec![("2026-10-17", grid(&[(547, 722)], 1439, vec![])), ("2026-10-16", grid(&[], 1439, vec![])), ("2026-10-04", grid(&[], 1439, vec![])), ("2026-10-02", grid(&[], 1439, vec![]))]);
        m.history_final.insert(day("2026-10-03")); // SEMS answered empty on 3 launches
        let now = Instant::now();
        let a = |m: &mut Monitor, v: Value| act(m, &v, now);
        m.stats_day = Some(day("2026-10-17"));
        assert!(!a(&mut m, json!({ "id": "calendar" })));
        let c = show(&mut m, "2026-10-17")["calendar"].clone();
        assert_eq!((c["month"].as_str(), c["lead"].as_u64(), c["today"].as_i64(), c["selected"].as_i64()), (Some("October 2026"), Some(4), Some(19), Some(17)));
        assert_eq!(c["letters"], json!(["S", "M", "T", "W", "T", "F", "S"]));
        let k = |i: usize| c["days"][i - 1].as_str().unwrap().to_owned();
        assert_eq!([k(17), k(16), k(3), k(10), k(20)], ["cut", "clear", "none", "todo", "none"]);
        assert_eq!((c["prev"].as_bool(), c["next"].as_bool(), c["days"].as_array().unwrap().len()), (Some(true), Some(false), 31));
        // Monday first: the lead moves, the letters start on M
        let mon = calendar(&m, (day("2026-10-01"), None), day("2026-10-17"), day("2026-09-18"), day("2026-10-19"), 1);
        assert_eq!((mon.lead, mon.letters[0], mon.letters[6]), (3, "M", "S"));
        // ← from the 1st: September, focus on its last day; nothing before history
        a(&mut m, json!({ "id": "cal_month", "step": -1, "focus": 0 }));
        let c = show(&mut m, "2026-10-17")["calendar"].clone();
        assert_eq!((c["month"].as_str(), c["focus"].as_i64(), c["selected"].as_i64(), c["prev"].as_bool()), (Some("September 2026"), Some(30), Some(0), Some(false)));
        assert_eq!((c["days"][16].as_str(), c["days"][17].as_str()), (Some("none"), Some("todo")));
        a(&mut m, json!({ "id": "cal_month", "step": -1 }));
        assert_eq!(m.stats_cal, Some((day("2026-09-01"), Some(day("2026-09-30")))), "no month before history");
        // picking a day shows it and closes; a no-data day can't be picked
        assert!(a(&mut m, json!({ "id": "cal_day", "day": 18 })));
        assert_eq!((m.stats_day, m.stats_cal), (Some(day("2026-09-18")), None));
        a(&mut m, json!({ "id": "calendar" }));
        a(&mut m, json!({ "id": "cal_month", "step": 1 }));
        assert!(!a(&mut m, json!({ "id": "cal_day", "day": 3 })));
        // ← → stop at the first day and today, and skip the empty day
        m.stats_day = Some(day("2026-09-18"));
        assert!(!a(&mut m, json!({ "id": "day", "step": -1 })));
        m.stats_day = Some(day("2026-10-04"));
        assert!(a(&mut m, json!({ "id": "day", "step": -1 })));
        assert_eq!(m.stats_day, Some(day("2026-10-02")));
        m.stats_day = Some(day("2026-10-18"));
        assert!(a(&mut m, json!({ "id": "day", "step": 1 })));
        assert_eq!(m.stats_day, None, "today follows the station's day");
        assert!(!a(&mut m, json!({ "id": "day", "step": 1 })));
        m.stats_day = Some(day("2026-10-02"));
        assert!(a(&mut m, json!({ "id": "today" })) && m.stats_day.is_none());
        // today with no cuts gets its own box line
        m.on_day(day("2026-10-19"), grid(&[], 853, vec![]), day("2026-10-19") * DAY + 854);
        assert_eq!(show(&mut m, "2026-10-19")["msg"], "No power cuts so far today.");
    }
}
