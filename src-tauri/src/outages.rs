//! Grid outages from history and the rules on them, knowing nothing about SEMS: when the
//! current outage began (T-002 spec › Since rule), when it usually ends (› Back-on rule) and
//! when the next one usually comes (› Next-cut rule). It sees only off minutes per day.
//! Every time here is an absolute station-local minute (`LocalTs.0 / 60`), never a bare clock
//! time, so nothing wraps at midnight.

use crate::rules;
use std::collections::BTreeMap;

pub const DAY: i64 = 1440;
/// The predictions look at this many days before today.
pub const PAST_DAYS: i64 = 14;
/// Off minutes merge when the next one comes at most this many minutes after the last (flicker,
/// gaps in the series). The research table keeps 13:11 and 13:17 apart: 6 is not merged.
const MERGE_GAP: i64 = 5;
/// A run shorter than this is not an outage.
const MIN_OUTAGE: i64 = 5;
/// "Off now" needs this many consecutive off minutes (a lone zero minute is not a cut).
const OFF_NOW_STREAK: i64 = 3;
/// History whose newest point is older than this can't say what happens now.
const STALE_MIN: i64 = 10;
/// Only cuts at least this long count for the predictions.
const MIN_CUT: i64 = 45;
/// Back-on: a past cut must start within this many clock minutes of the current one.
const MATCH_MIN: i64 = 90;
/// Times "agree" when they fit in this many minutes...
const WINDOW: i64 = 30;
/// ...on at least this many days.
const AGREE_DAYS: usize = 3;

/// One station-local day of history. Deserialize: the v0.1.0 history file held these (T-012);
/// the file now stores runs (`to_runs`, T-015 § 4.2).
#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize)]
pub struct GridDay {
    /// Minutes of the day that were off, sorted, each with "the house drew load" then.
    pub off: Vec<(u32, bool)>,
    /// Minutes with no reading, (first, last), more than 5 in a row (T-015 § 4.4).
    #[serde(default)]
    pub gaps: Vec<(u32, u32)>,
    /// The newest minute with a reading; `None` when the day came back empty.
    pub newest: Option<u32>,
}

impl GridDay {
    /// `off` as (first, last, loaded) runs of consecutive minutes with the same load flag.
    pub fn to_runs(&self) -> Vec<(u32, u32, bool)> {
        let mut out: Vec<(u32, u32, bool)> = Vec::new();
        for &(m, l) in &self.off {
            match out.last_mut() {
                Some(r) if r.1 + 1 == m && r.2 == l => r.1 = m,
                _ => out.push((m, m, l)),
            }
        }
        out
    }
    pub fn from_runs(runs: &[(u32, u32, bool)], gaps: Vec<(u32, u32)>, newest: Option<u32>) -> GridDay {
        GridDay { off: runs.iter().flat_map(|&(a, b, l)| (a..=b.min(DAY as u32 - 1)).map(move |m| (m, l))).collect(), gaps, newest }
    }
    pub fn gap_minutes(&self) -> u32 {
        self.gaps.iter().map(|g| g.1 + 1 - g.0).sum()
    }
}

/// `end` is the last off minute (09:07 to 12:03 is 177 minutes inclusive, length 176).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outage {
    pub start: i64,
    pub end: i64,
    /// Still on at history's newest point: no end yet, never a candidate.
    pub running: bool,
}

impl Outage {
    fn len(&self) -> i64 {
        self.end - self.start
    }
}

/// What history says about the start of the outage happening now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistStart {
    /// S2 / S3.
    Start(i64),
    /// S4: still off at yesterday 00:00 (two days at most).
    AtLeastYesterday,
    /// S5: off at today 00:00 and yesterday's history is missing.
    AtLeastToday,
    /// History is fresh but its newest point is not part of an outage (yet).
    NotOff,
    /// No history for today, or its newest point is more than 10 minutes old.
    Stale,
}

struct Run {
    start: i64,
    end: i64,
    loaded: bool,
    streak: i64,
}

/// Every day's off minutes, joined across midnight and merged over short gaps.
fn runs(days: &BTreeMap<i64, GridDay>) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    let mut streak = 0;
    for (&d, g) in days {
        for &(m, loaded) in &g.off {
            let t = d * DAY + m as i64;
            match out.last_mut() {
                Some(r) if t - r.end <= MERGE_GAP => {
                    streak = if t - r.end == 1 { streak + 1 } else { 1 };
                    (r.end, r.loaded, r.streak) = (t, r.loaded || loaded, r.streak.max(streak));
                }
                _ => {
                    streak = 1;
                    out.push(Run { start: t, end: t, loaded, streak });
                }
            }
        }
    }
    out
}

fn newest(days: &BTreeMap<i64, GridDay>, today: i64) -> Option<i64> {
    days.get(&today)?.newest.map(|m| today * DAY + m as i64)
}

/// The outages in history (≥ 5 min with some load), `running` when one reaches today's newest point.
pub fn outages(days: &BTreeMap<i64, GridDay>, today: i64) -> Vec<Outage> {
    let last = newest(days, today);
    runs(days)
        .into_iter()
        .filter(|r| r.end - r.start + 1 >= MIN_OUTAGE && r.loaded)
        .map(|r| Outage { start: r.start, end: r.end, running: Some(r.end) == last })
        .collect()
}

/// Research rule (a): the newest point belongs to an off run with 3 consecutive off minutes;
/// its start, walking back through today and then yesterday.
pub fn history_start(days: &BTreeMap<i64, GridDay>, now: i64) -> HistStart {
    let today = now.div_euclid(DAY);
    let Some(last) = newest(days, today).filter(|n| now - n <= STALE_MIN) else { return HistStart::Stale };
    let Some(r) = runs(days).pop().filter(|r| r.end == last && r.streak >= OFF_NOW_STREAK) else { return HistStart::NotOff };
    let yesterday_known = days.get(&(today - 1)).is_some_and(|g| g.newest.is_some());
    if r.start <= (today - 1) * DAY {
        HistStart::AtLeastYesterday
    } else if r.start == today * DAY && !yesterday_known {
        HistStart::AtLeastToday
    } else {
        HistStart::Start(r.start)
    }
}

/// S2 / S3 copy: "Since 9:07 AM (2h 15m)" / "Since yesterday 11:33 PM (1h 7m)".
pub fn since_text(start: i64, now: i64) -> String {
    let dur = rules::fmt_since_duration(now - start);
    if start.div_euclid(DAY) < now.div_euclid(DAY) {
        format!("Since yesterday {} {dur}", hm(start))
    } else {
        format!("Since {} {dur}", hm(start))
    }
}

/// "12:05 PM" for an absolute minute.
fn hm(t: i64) -> String {
    crate::clock::LocalTs(t * 60).hhmm12()
}

/// Nearest 5 minutes, halves up (12:02.5 → 12:05).
fn round5(t: f64) -> i64 {
    ((t + 2.5) / 5.0).floor() as i64 * 5
}

/// Clock distance over midnight: the smaller of |a − b| and 1440 − |a − b|.
fn clock_dist(a: i64, b: i64) -> i64 {
    let x = (a.rem_euclid(DAY) - b.rem_euclid(DAY)).abs();
    x.min(DAY - x)
}

/// Odd count: the middle one; even: the mean of the middle two. `v` sorted, not empty.
fn median(v: &[i64]) -> f64 {
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2] as f64
    } else {
        (v[n / 2 - 1] + v[n / 2]) as f64 / 2.0
    }
}

/// The largest run of sorted times inside `WINDOW` minutes (tie: the earlier one).
fn agreed(sorted: &[i64]) -> &[i64] {
    let mut best: &[i64] = &[];
    for i in 0..sorted.len() {
        let j = sorted[i..].iter().take_while(|&&x| x - sorted[i] <= WINDOW).count();
        if j > best.len() {
            best = &sorted[i..i + j];
        }
    }
    best
}

/// Back-on step 1: the 14 past days' finished outages, owned by their start day; the run already
/// open at day −14 00:00 is dropped (it began on a day that isn't fetched).
fn candidates(outs: &[Outage], today: i64) -> impl Iterator<Item = &Outage> {
    let oldest = (today - PAST_DAYS) * DAY;
    outs.iter().filter(move |o| !o.running && (oldest..today * DAY).contains(&o.start) && o.start != oldest)
}

/// Back-on rule: `s` = the current outage's start, `now` = station now in minutes.
/// "Usually back around 12:05 PM (last 14 days)" or nothing. Until its time passes the line follows
/// the rule (it may move as the cut grows); once a shown time passes with the grid still off, that
/// line stays until the grid is back (T-023, owner). Replaying the cut minute by minute keeps this
/// stateless, so it holds across a relaunch or a panel first opened after the time.
pub fn back_on(outs: &[Outage], s: i64, now: f64) -> Option<String> {
    let mut shown = None;
    for t in (s..now.ceil() as i64).map(|m| m as f64).chain([now]) {
        if shown.is_some_and(|raw| t >= raw) {
            break; // its time passed: hold it
        }
        shown = usual_end(outs, s, t).filter(|&raw| t < raw);
    }
    shown.map(|raw| format!("Usually back around {} (last 14 days)", hm(round5(raw))))
}

/// Back-on rule, steps 2–8, as seen at `now`: the raw median end, before rounding.
fn usual_end(outs: &[Outage], s: i64, now: f64) -> Option<f64> {
    let today = (now.floor() as i64).div_euclid(DAY);
    let elapsed = now - s as f64;
    let mut ends: Vec<i64> = (today - PAST_DAYS..today)
        .filter_map(|d| {
            candidates(outs, today)
                .filter(|o| o.start.div_euclid(DAY) == d && o.len() >= MIN_CUT && o.len() as f64 > elapsed)
                .filter(|o| clock_dist(o.start, s) <= MATCH_MIN)
                .min_by_key(|o| (clock_dist(o.start, s), o.start))
                // align by whole days so the start lands nearest S
                .map(|o| o.end + ((s - o.start) as f64 / DAY as f64).round() as i64 * DAY)
        })
        .collect();
    ends.sort_unstable();
    let set = agreed(&ends);
    if set.len() < AGREE_DAYS {
        return None;
    }
    Some(median(set))
}

/// A cut placed on yesterday, today or tomorrow: (start, end, the day it really happened).
type Placed = (i64, i64, i64);

/// Next-cut rule, steps 1–9 (grid on). Two lines, or nothing. `stale` = no live data (N4).
pub fn next_cut(outs: &[Outage], now: f64, stale: bool) -> Option<[String; 2]> {
    let today = (now.floor() as i64).div_euclid(DAY);
    // 1. place every cut ≥ 45 min at its clock start on yesterday, today and tomorrow: (start, end, day)
    let mut pts: Vec<Placed> = candidates(outs, today)
        .filter(|o| o.len() >= MIN_CUT)
        .flat_map(|o| (-1..=1).map(move |k| ((today + k) * DAY + o.start.rem_euclid(DAY), o.len(), o.start.div_euclid(DAY))))
        .map(|(a, len, d)| (a, a + len, d))
        .collect();
    pts.sort_unstable();
    // 2. windows [a, a + 30] holding starts from ≥ 3 days (each day once, its earliest start)
    let mut wins: Vec<(i64, Vec<Placed>)> = Vec::new();
    for &(a, _, _) in &pts {
        let mut per: Vec<Placed> = Vec::new();
        for &p in pts.iter().filter(|p| (a..=a + WINDOW).contains(&p.0)) {
            if !per.iter().any(|q| q.2 == p.2) {
                per.push(p);
            }
        }
        if per.len() >= AGREE_DAYS {
            wins.push((a, per));
        }
    }
    // 3. overlapping windows form one cluster; keep its window with the most days (tie: earlier)
    let mut clusters: Vec<Vec<Placed>> = Vec::new();
    let mut last_a: Option<i64> = None;
    for (a, per) in wins {
        match (last_a, clusters.last_mut()) {
            (Some(l), Some(best)) if a <= l + WINDOW => {
                if per.len() > best.len() {
                    *best = per;
                }
            }
            _ => clusters.push(per),
        }
        last_a = Some(a);
    }
    for c in clusters {
        // 4. start = median of the per-day starts; 5. end = median of ≥ 3 agreeing ends
        let mut starts: Vec<i64> = c.iter().map(|p| p.0).collect();
        let mut ends: Vec<i64> = c.iter().map(|p| p.1).collect();
        starts.sort_unstable();
        ends.sort_unstable();
        let st = median(&starts);
        let set = agreed(&ends);
        let end = (set.len() >= AGREE_DAYS).then(|| median(set));
        if st > now + DAY as f64 {
            continue;
        }
        let back = |e: f64| format!("back around {} (last 14 days)", hm(round5(e)));
        if st <= now {
            // 6. done when a real cut (≥ 45 min or still running) started within 90 min
            let done = outs
                .iter()
                .any(|o| o.start as f64 <= now && (st - o.start as f64).abs() <= MATCH_MIN as f64 && (o.running || o.len() >= MIN_CUT));
            // 7. due only before its usual end; passed or done: skip
            let Some(e) = end.filter(|&e| !done && now < e) else { continue };
            // 8. while stale a due cut shows nothing: the station may be down because of it
            if stale {
                return None;
            }
            let when = if (st.floor() as i64).div_euclid(DAY) == today { "not yet today" } else { "not yet" };
            return Some([format!("Cut usually around {}, {when}", hm(round5(st))), back(e)]);
        }
        let tomorrow = if round5(st).div_euclid(DAY) > today { " tomorrow" } else { "" };
        let l2 = end.map_or_else(|| "end time varies (last 14 days)".to_owned(), back);
        return Some([format!("Next cut usually around {}{tomorrow}", hm(round5(st))), l2]);
    }
    None
}

/// Test helpers shared with view.rs: the fixture's real outage table and series built from it.
#[cfg(test)]
pub mod testdata {
    use super::*;
    use crate::clock::LocalTs;

    /// "2026-10-18 09:07" → absolute minute.
    pub fn at(s: &str) -> i64 {
        LocalTs::parse(&format!("{s}:00")).unwrap().0 / 60
    }

    pub fn fixture() -> serde_json::Value {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sems-plus-grid-history.json");
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    /// The fixture's `outages_station_local_last_12_days` (real, 2026-10-07 to 10-18 12:10).
    pub fn table() -> Vec<(i64, i64)> {
        fixture()["outages_station_local_last_12_days"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| (at(o["start"].as_str().unwrap()), at(o["end"].as_str().unwrap())))
            .collect()
    }

    /// Outages history shows at `t`: started before it; running when the end is still ahead.
    pub fn known(table: &[(i64, i64)], t: i64) -> Vec<Outage> {
        table.iter().filter(|o| o.0 < t).map(|&(start, end)| Outage { start, end: end.min(t), running: end > t }).collect()
    }

    /// The outage running at `t` (start ≤ t < end), as the replay's `current`.
    pub fn current(table: &[(i64, i64)], t: i64) -> Option<i64> {
        table.iter().find(|o| o.0 <= t && t < o.1).map(|o| o.0)
    }

    /// Table-driven (not series-real) history: every minute inside an outage is off and loaded;
    /// days `first..=today`, today up to `upto`.
    pub fn days_from(table: &[(i64, i64)], first: i64, upto: i64) -> BTreeMap<i64, GridDay> {
        let today = upto.div_euclid(DAY);
        (first..=today)
            .map(|d| {
                let last = if d == today { upto - d * DAY } else { DAY - 1 };
                let off = (0..=last)
                    .filter(|m| table.iter().any(|o| (o.0..=o.1).contains(&(d * DAY + m))))
                    .map(|m| (m as u32, true))
                    .collect();
                (d, GridDay { off, newest: Some(last as u32), ..Default::default() })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::testdata::*;
    use super::*;

    /// Back-on on the real table at `t` (S = the running outage's start).
    fn back(t: &str) -> Option<String> {
        let (tb, t) = (table(), at(t));
        back_on(&known(&tb, t), current(&tb, t)?, t as f64)
    }
    fn next(t: &str, stale: bool) -> Option<[String; 2]> {
        let (tb, t) = (table(), at(t));
        assert_eq!(current(&tb, t), None, "grid is off at that moment");
        next_cut(&known(&tb, t), t as f64, stale)
    }
    fn lines(a: &str, b: &str) -> Option<[String; 2]> {
        Some([a.to_owned(), b.to_owned()])
    }
    fn usual(hhmm: &str) -> Option<String> {
        Some(format!("Usually back around {hhmm} (last 14 days)"))
    }
    /// Made-up outages from (start, end) strings; none running.
    fn made(v: &[(&str, &str)]) -> Vec<Outage> {
        v.iter().map(|(s, e)| Outage { start: at(s), end: at(e), running: false }).collect()
    }

    // ---- outages from series ----

    #[test]
    fn runs_merge_short_gaps_and_need_load() {
        let mut days = BTreeMap::new();
        // 09:07–09:13 and 09:18–09:20 (5 apart: merged), 09:26–09:29 (6 apart: its own run, 4 min, dropped),
        // 10:00–10:10 unloaded (dropped)
        let off: Vec<(u32, bool)> = (547..=553)
            .chain(558..=560)
            .map(|m| (m, true))
            .chain((566..=569).map(|m| (m, true)))
            .chain((600..=610).map(|m| (m, false)))
            .collect();
        days.insert(20_000, GridDay { off, newest: Some(610), ..Default::default() });
        let o = outages(&days, 20_000);
        assert_eq!(o, vec![Outage { start: 20_000 * DAY + 547, end: 20_000 * DAY + 560, running: false }]);
    }

    #[test]
    fn runs_round_trip() {
        let off: Vec<(u32, bool)> = vec![(0, true), (1, true), (2, false), (5, false), (6, false), (1439, true)];
        let g = GridDay { off, gaps: vec![(100, 200)], newest: Some(1439) };
        assert_eq!(g.to_runs(), vec![(0, 1, true), (2, 2, false), (5, 6, false), (1439, 1439, true)]);
        assert_eq!(GridDay::from_runs(&g.to_runs(), g.gaps.clone(), g.newest), g);
        assert_eq!(g.gap_minutes(), 101);
        assert!(GridDay::from_runs(&[(1430, 99_999, true)], vec![], None).off.len() == 10, "an edited file can't grow a day past 23:59");
    }

    #[test]
    fn series_outages_match_the_table() {
        // the table fed back as minute series comes out as the same 55 outages
        let tb = table();
        let upto = at("2026-10-18 12:10");
        let days = days_from(&tb, at("2026-10-07 00:00").div_euclid(DAY), upto);
        let got: Vec<(i64, i64)> = outages(&days, upto.div_euclid(DAY)).iter().map(|o| (o.start, o.end)).collect();
        assert_eq!(got, tb);
    }

    /// Both rules at every 5 minutes of 10-12..10-18 12:10, one JSON line each, for a diff with
    /// tasks/T-002/mockups/render/replay_rules.py. Run: GW_SWEEP=<file> cargo test … replay_sweep -- --ignored
    #[test]
    #[ignore]
    fn replay_sweep() {
        let tb = table();
        let mut out = String::new();
        let mut t = at("2026-10-12 00:00");
        while t <= at("2026-10-18 12:10") {
            let k = known(&tb, t);
            let (back, next) = match current(&tb, t) {
                Some(s) => (back_on(&k, s, t as f64), None),
                None => (None, next_cut(&k, t as f64, false)),
            };
            let line = serde_json::json!({ "t": crate::clock::LocalTs(t * 60).date() + " " + &crate::clock::LocalTs(t * 60).hhmm(), "back": back, "next": next });
            out += &format!("{line}\n");
            t += 5;
        }
        std::fs::write(std::env::var("GW_SWEEP").unwrap(), out).unwrap();
    }

    #[test]
    fn outage_joined_across_midnight_and_running() {
        let tb = table();
        let days = days_from(&tb, at("2026-10-17 00:00").div_euclid(DAY), at("2026-10-18 00:40"));
        let o = outages(&days, at("2026-10-18 00:40").div_euclid(DAY));
        assert_eq!(o.last(), Some(&Outage { start: at("2026-10-17 23:33"), end: at("2026-10-18 00:40"), running: true }));
    }

    // ---- back-on (spec › Back-on rule tests) ----

    #[test]
    fn back_absent_when_days_disagree() {
        // A, made-up: ends 12:03, 12:03, 13:11, 16:32 → only 2 agree
        let o = made(&[
            ("2026-10-17 09:25", "2026-10-17 16:32"),
            ("2026-10-15 10:06", "2026-10-15 13:11"),
            ("2026-10-14 09:08", "2026-10-14 12:03"),
            ("2026-10-13 09:07", "2026-10-13 12:03"),
        ]);
        assert_eq!(back_on(&o, at("2026-10-18 09:07"), at("2026-10-18 09:27") as f64), None);
    }

    #[test]
    fn back_real_ten_eighteen_morning() {
        // A, real: the last 5 days alone disagree (12:03, 12:03, 13:11, 16:32); with 10-07..10-12 too they agree
        assert_eq!(back("2026-10-18 09:27"), usual("12:05\u{a0}PM"));
        assert_eq!(back("2026-10-18 11:22"), usual("12:05\u{a0}PM"));
    }

    #[test]
    fn back_today_case_fourteen_days() {
        // T-019, the owner's 2026-10-19 cut from 9:07 AM, asked at 10:26 AM. The stored 14 days'
        // cuts near 9 AM (10-16 has none within 90 min): only 2 of the last 5 days agree, 10 of 14 do.
        let o = made(&[
            ("2026-10-05 09:08", "2026-10-05 12:03"),
            ("2026-10-06 09:07", "2026-10-06 12:02"),
            ("2026-10-07 09:06", "2026-10-07 12:03"),
            ("2026-10-08 09:06", "2026-10-08 12:03"),
            ("2026-10-09 09:05", "2026-10-09 12:02"),
            ("2026-10-10 10:32", "2026-10-10 12:06"),
            ("2026-10-11 09:08", "2026-10-11 12:02"),
            ("2026-10-12 09:05", "2026-10-12 12:02"),
            ("2026-10-13 09:07", "2026-10-13 12:03"),
            ("2026-10-14 09:08", "2026-10-14 12:03"),
            ("2026-10-15 10:06", "2026-10-15 13:11"),
            ("2026-10-16 06:03", "2026-10-16 08:28"),
            ("2026-10-16 11:28", "2026-10-16 13:31"),
            ("2026-10-17 09:25", "2026-10-17 16:32"),
            ("2026-10-18 09:07", "2026-10-18 12:03"),
        ]);
        let s = at("2026-10-19 09:07");
        assert_eq!(back_on(&o, s, at("2026-10-19 10:26") as f64), Some("Usually back around 12:05\u{a0}PM (last 14 days)".to_owned()));
        // the same cuts seen 5 days back only: no line (the bug the owner saw)
        let last5: Vec<Outage> = o.iter().filter(|x| x.start >= at("2026-10-14 00:01")).copied().collect();
        assert_eq!(back_on(&last5, s, at("2026-10-19 10:26") as f64), None);
    }

    #[test]
    fn back_four_agree() {
        // R1, real (10-07..10-13 all agree around 12:03)
        assert_eq!(back("2026-10-14 11:22"), usual("12:05\u{a0}PM"));
    }

    #[test]
    fn back_held_once_raw_median_passed() {
        // T-023 (owner): past the usual time with the grid still off, the line stays as it was
        assert_eq!(back("2026-10-14 12:02"), usual("12:05\u{a0}PM")); // R1, raw median 12:03
        assert_eq!(back("2026-10-14 12:03"), None); // the grid came back at 12:03: the line goes
        assert_eq!(back("2026-10-15 12:02"), usual("12:05\u{a0}PM")); // R2, raw median 12:03
        assert_eq!(back("2026-10-15 12:03"), usual("12:05\u{a0}PM"));
        assert_eq!(back("2026-10-15 12:20"), usual("12:05\u{a0}PM")); // still off: no rerun, no hiding
    }

    #[test]
    fn back_reruns_as_elapsed_grows() {
        // R3, made-up: 3 short cuts (75-79 min) and 3 long ones from 13:30; once 80 min have passed
        // only the long ones are longer than the current cut
        let o = made(&[
            ("2026-10-17 13:30", "2026-10-17 14:45"),
            ("2026-10-16 13:30", "2026-10-16 14:47"),
            ("2026-10-15 13:30", "2026-10-15 14:49"),
            ("2026-10-14 13:30", "2026-10-14 16:30"),
            ("2026-10-13 13:30", "2026-10-13 16:32"),
            ("2026-10-12 13:30", "2026-10-12 16:34"),
        ]);
        let s = at("2026-10-18 13:30");
        assert_eq!(back_on(&o, s, at("2026-10-18 14:00") as f64), usual("2:45\u{a0}PM"));
        assert_eq!(back_on(&o, s, at("2026-10-18 14:50") as f64), usual("4:30\u{a0}PM"));
        // real
        assert_eq!(back("2026-10-14 15:00"), usual("4:30\u{a0}PM"));
    }

    #[test]
    fn back_since_yesterday_two_days() {
        // R4, real: in the last 5 days picks only on 10-14 and 10-13; 10-07..10-12 add enough
        assert_eq!(back("2026-10-18 00:40"), usual("2:05\u{a0}AM"));
    }

    #[test]
    fn back_excludes_current_cut() {
        // R4, real: the running 10-17 23:33 cut is no candidate, though yesterday is one of the 14 days
        let (tb, t) = (table(), at("2026-10-18 00:40"));
        let k = known(&tb, t);
        assert!(k.iter().any(|o| o.start == at("2026-10-17 23:33") && o.running));
        assert!(candidates(&k, t.div_euclid(DAY)).all(|o| o.start != at("2026-10-17 23:33")));
    }

    #[test]
    fn back_kept_while_stale_on_the_rule() {
        // R1 at 11:40 (the rule has no stale input: history-based, it stays)
        assert_eq!(back("2026-10-14 11:40"), usual("12:05\u{a0}PM"));
    }

    #[test]
    fn back_shows_when_three_agree() {
        // B, made-up: ends 12:01, 12:03, 12:05, 13:11 → median 12:03 → 12:05; held from 12:03 (T-023)
        let o = made(&[
            ("2026-10-17 08:00", "2026-10-17 12:01"),
            ("2026-10-16 08:00", "2026-10-16 12:03"),
            ("2026-10-15 08:00", "2026-10-15 12:05"),
            ("2026-10-14 08:00", "2026-10-14 13:11"),
        ]);
        let s = at("2026-10-18 09:00");
        assert_eq!(back_on(&o, s, at("2026-10-18 10:00") as f64), usual("12:05\u{a0}PM"));
        assert_eq!(back_on(&o, s, at("2026-10-18 12:03") as f64), usual("12:05\u{a0}PM"));
    }

    #[test]
    fn back_even_count_median_mean() {
        // C, made-up: S yesterday 23:33, now 00:40; ends 02:01, 02:03, 02:05, 02:06 → 02:04 → 02:05
        let o = made(&[
            ("2026-10-16 23:00", "2026-10-17 02:01"),
            ("2026-10-15 23:00", "2026-10-16 02:03"),
            ("2026-10-14 23:00", "2026-10-15 02:05"),
            ("2026-10-13 23:00", "2026-10-14 02:06"),
        ]);
        let s = at("2026-10-17 23:33");
        assert_eq!(back_on(&o, s, at("2026-10-18 00:40") as f64), usual("2:05\u{a0}AM"));
        assert_eq!(back_on(&o, s, at("2026-10-18 02:03") as f64), usual("2:05\u{a0}AM"));
        assert_eq!(back_on(&o, s, at("2026-10-18 02:04") as f64), usual("2:05\u{a0}AM")); // held (T-023)
    }

    #[test]
    fn back_window_spans_midnight() {
        // D, made-up: ends 23:50, 23:58, 00:10 next day → spread 20 → median 23:58 → 00:00
        let o = made(&[
            ("2026-10-17 21:00", "2026-10-17 23:50"),
            ("2026-10-16 21:00", "2026-10-16 23:58"),
            ("2026-10-15 21:00", "2026-10-16 00:10"),
        ]);
        assert_eq!(back_on(&o, at("2026-10-18 21:00"), at("2026-10-18 22:00") as f64), usual("12:00\u{a0}AM"));
    }

    #[test]
    fn back_filters_before_picking() {
        // E, made-up: 08:55–09:30 (35 min) is dropped first, so 09:40–12:04 is picked, not 08:55
        let o = made(&[
            ("2026-10-17 08:55", "2026-10-17 09:30"),
            ("2026-10-17 09:40", "2026-10-17 12:04"),
            ("2026-10-16 09:40", "2026-10-16 12:04"),
            ("2026-10-15 09:40", "2026-10-15 12:04"),
        ]);
        assert_eq!(back_on(&o, at("2026-10-18 09:00"), at("2026-10-18 09:10") as f64), usual("12:05\u{a0}PM"));
        // without its 09:40 cut, 10-17 has no match (the 35-min cut never qualifies): 2 days, none
        let o2: Vec<_> = o.iter().copied().filter(|x| x.start != at("2026-10-17 09:40")).collect();
        assert_eq!(back_on(&o2, at("2026-10-18 09:00"), at("2026-10-18 09:10") as f64), None);
    }

    #[test]
    fn back_held_after_usual_time() {
        // F, made-up: S 09:07, now 12:20 (E 193). Before T-023 the ~12:03 cuts (176 min) dropped here
        // and the line went (or moved to 4:30 PM); now 12:05 PM passed with the grid off, so it stays.
        let mut o = made(&[
            ("2026-10-17 09:07", "2026-10-17 12:03"),
            ("2026-10-16 09:07", "2026-10-16 12:03"),
            ("2026-10-15 09:07", "2026-10-15 12:03"),
        ]);
        let (s, now) = (at("2026-10-18 09:07"), at("2026-10-18 12:20") as f64);
        assert_eq!(back_on(&o, s, now), usual("12:05\u{a0}PM"));
        o.extend(made(&[
            ("2026-10-14 08:30", "2026-10-14 16:28"),
            ("2026-10-13 08:40", "2026-10-13 16:31"),
            ("2026-10-17 10:00", "2026-10-17 16:33"),
        ]));
        // 10-17 now has two cuts; the rule alone at 12:20 would say 4:30 PM, but 12:05 PM was shown first
        assert_eq!(usual_end(&o, s, now).map(round5), Some(at("2026-10-18 16:30")));
        assert_eq!(back_on(&o, s, now), usual("12:05\u{a0}PM"));
        // the next day it stays too (S yesterday, still off)
        assert_eq!(back_on(&o, s, at("2026-10-19 08:00") as f64), usual("12:05\u{a0}PM"));
    }

    #[test]
    fn back_rounds_half_up() {
        // made-up: two days end 12:02 and 12:03 plus two more → median 12:02.5 → 12:05
        let o = made(&[
            ("2026-10-17 09:00", "2026-10-17 12:01"),
            ("2026-10-16 09:00", "2026-10-16 12:02"),
            ("2026-10-15 09:00", "2026-10-15 12:03"),
            ("2026-10-14 09:00", "2026-10-14 12:04"),
        ]);
        assert_eq!(back_on(&o, at("2026-10-18 09:00"), at("2026-10-18 10:00") as f64), usual("12:05\u{a0}PM"));
        assert_eq!(round5(at("2026-10-18 12:02") as f64 + 0.4) % DAY, 12 * 60);
    }

    #[test]
    fn back_circular_start_match() {
        // made-up: S 10-17 23:50; a 10-15 00:10–02:05 cut is 20 min away on the clock; k = 3 → 10-18 02:05
        let o = made(&[
            ("2026-10-15 00:10", "2026-10-15 02:05"),
            ("2026-10-16 00:05", "2026-10-16 02:05"),
            ("2026-10-14 23:45", "2026-10-15 02:00"),
        ]);
        let s = at("2026-10-17 23:50");
        assert_eq!(back_on(&o, s, at("2026-10-18 00:30") as f64), usual("2:05\u{a0}AM"));
        let k = ((s - o[0].start) as f64 / DAY as f64).round() as i64;
        assert_eq!((k, o[0].end + k * DAY), (3, at("2026-10-18 02:05")));
    }

    #[test]
    fn back_drops_run_open_at_oldest_day() {
        // made-up: today 10-27, day −14 = 10-13 opens off at 00:00 → not a candidate; its later cut is
        let o = made(&[
            ("2026-10-13 00:00", "2026-10-13 02:03"),
            ("2026-10-13 09:05", "2026-10-13 12:02"),
            ("2026-10-14 09:05", "2026-10-14 12:02"),
        ]);
        let c: Vec<_> = candidates(&o, at("2026-10-27 00:00").div_euclid(DAY)).map(|x| x.start).collect();
        assert_eq!(c, vec![at("2026-10-13 09:05"), at("2026-10-14 09:05")]);
    }

    #[test]
    fn back_tie_picks_earlier_start() {
        // made-up: S 10:00; one day has 09:30–12:00 and 10:30–13:30, both 30 min away → 09:30 (12:00)
        let o = made(&[
            ("2026-10-17 09:30", "2026-10-17 12:00"),
            ("2026-10-17 10:30", "2026-10-17 13:30"),
            ("2026-10-16 10:00", "2026-10-16 12:05"),
            ("2026-10-15 10:00", "2026-10-15 12:10"),
        ]);
        assert_eq!(back_on(&o, at("2026-10-18 10:00"), at("2026-10-18 10:05") as f64), usual("12:05\u{a0}PM"));
    }

    #[test]
    fn back_needs_three_days() {
        // made-up: 2 agreeing of 2 matched → none
        let o = made(&[("2026-10-17 09:00", "2026-10-17 12:00"), ("2026-10-16 09:00", "2026-10-16 12:00")]);
        assert_eq!(back_on(&o, at("2026-10-18 09:00"), at("2026-10-18 10:00") as f64), None);
    }

    // ---- next cut (spec › Next-cut rule tests) ----

    #[test]
    fn next_cut_upcoming_with_end() {
        // N-A, real
        assert_eq!(next("2026-10-18 12:10", false), lines("Next cut usually around 1:35\u{a0}PM", "back around 4:30\u{a0}PM (last 14 days)"));
        // real: the last 5 days' 9 AM ends disagree, the 14 days' agree
        assert_eq!(next("2026-10-18 08:55", false), lines("Next cut usually around 9:05\u{a0}AM", "back around 12:05\u{a0}PM (last 14 days)"));
    }

    #[test]
    fn next_cut_due_not_yet_today() {
        // N-B, real
        assert_eq!(next("2026-10-16 10:28", false), lines("Cut usually around 9:05\u{a0}AM, not yet today", "back around 12:05\u{a0}PM (last 14 days)"));
    }

    #[test]
    fn next_cut_due_after_midnight() {
        // N-B2, real
        assert_eq!(next("2026-10-15 23:50", false), lines("Cut usually around 11:35\u{a0}PM, not yet today", "back around 2:05\u{a0}AM (last 14 days)"));
        assert_eq!(next("2026-10-16 00:05", false), lines("Cut usually around 11:35\u{a0}PM, not yet", "back around 2:05\u{a0}AM (last 14 days)"));
        assert_eq!(next("2026-10-16 02:02", false), lines("Cut usually around 11:35\u{a0}PM, not yet", "back around 2:05\u{a0}AM (last 14 days)"));
        assert_eq!(next("2026-10-16 02:05", false), lines("Next cut usually around 9:05\u{a0}AM", "back around 12:05\u{a0}PM (last 14 days)"));
    }

    #[test]
    fn next_cut_moves_on_after_end_passed() {
        // N-C, real
        assert_eq!(next("2026-10-16 02:30", false), lines("Next cut usually around 9:05\u{a0}AM", "back around 12:05\u{a0}PM (last 14 days)"));
    }

    #[test]
    fn next_cut_skips_cut_done() {
        // N-C2, real: today's 18:09 cut ended early (21:07)
        assert_eq!(next("2026-10-16 21:10", false), lines("Next cut usually around 11:35\u{a0}PM", "back around 2:05\u{a0}AM (last 14 days)"));
    }

    #[test]
    fn next_cut_flicker_is_not_done() {
        // real: the 12:59–13:24 flicker doesn't mark the 13:35 cut done
        assert_eq!(next("2026-10-13 13:35", false), lines("Cut usually around 1:35\u{a0}PM, not yet today", "back around 4:30\u{a0}PM (last 14 days)"));
    }

    #[test]
    fn next_cut_start_only_when_ends_disagree() {
        // N-D, real
        assert_eq!(next("2026-10-12 22:33", false), lines("Next cut usually around 11:35\u{a0}PM", "end time varies (last 14 days)"));
    }

    #[test]
    fn next_cut_due_shows_nothing_while_stale() {
        // N-G, real days: 10-16 10:28 with no live data since 10:10 → no lines, not 13:35
        assert_eq!(next("2026-10-16 10:28", true), None);
    }

    #[test]
    fn next_cut_upcoming_shown_while_stale() {
        assert_eq!(next("2026-10-18 12:10", true), lines("Next cut usually around 1:35\u{a0}PM", "back around 4:30\u{a0}PM (last 14 days)"));
    }

    /// Made-up days 10-13..10-17 with one cut each at the given start, ending 3 h later.
    fn daily(starts: &[(&str, &str)]) -> Vec<Outage> {
        starts.iter().map(|(d, hm)| at(&format!("2026-10-{d} {hm}"))).map(|s| Outage { start: s, end: s + 180, running: false }).collect()
    }

    #[test]
    fn next_cut_window_tie_picks_earlier() {
        // made-up: windows from 17:41 and 18:03 each hold 4 days → the 17:41 window: (18:03 + 18:06) / 2 = 18:04.5 → 18:05
        let o = daily(&[("17", "17:41"), ("13", "18:03"), ("14", "18:06"), ("15", "18:09"), ("16", "18:31")]);
        let got = next_cut(&o, at("2026-10-18 12:00") as f64, false).unwrap();
        assert_eq!(got[0], "Next cut usually around 6:05\u{a0}PM");
    }

    #[test]
    fn next_cut_rounds_half_up() {
        let o = daily(&[("17", "17:41"), ("13", "18:03"), ("14", "18:06"), ("15", "18:09"), ("16", "18:31")]);
        assert_eq!(next_cut(&o, at("2026-10-18 12:00") as f64, false).unwrap()[0], "Next cut usually around 6:05\u{a0}PM"); // made-up 18:04.5
    }

    #[test]
    fn next_cut_none_when_days_disagree() {
        // N-E, made-up: starts 3 h apart, no 30-min window holds 3 days
        let o = daily(&[("13", "06:00"), ("14", "09:00"), ("15", "12:00"), ("16", "15:00"), ("17", "18:00")]);
        assert_eq!(next_cut(&o, at("2026-10-18 05:00") as f64, false), None);
    }

    #[test]
    fn next_cut_tomorrow_suffix() {
        // N-T, made-up: last cuts start 21:00 but disagree; 09:05–09:08 starts agree, ends 12:02–12:03
        let mut o = made(&[
            ("2026-10-13 09:05", "2026-10-13 12:02"),
            ("2026-10-14 09:07", "2026-10-14 12:03"),
            ("2026-10-15 09:08", "2026-10-15 12:02"),
            ("2026-10-16 09:08", "2026-10-16 12:03"),
        ]);
        o.extend(made(&[("2026-10-16 21:00", "2026-10-16 22:00"), ("2026-10-17 21:00", "2026-10-17 23:30")]));
        assert_eq!(
            next_cut(&o, at("2026-10-18 23:50") as f64, false),
            lines("Next cut usually around 9:10\u{a0}AM tomorrow", "back around 12:05\u{a0}PM (last 14 days)")
        );
    }

    #[test]
    fn next_cut_counts_days_not_cuts() {
        // made-up: two cuts of 10-17 in one window count once: 2 days → no cluster
        let o = made(&[
            ("2026-10-17 14:00", "2026-10-17 14:50"),
            ("2026-10-17 14:10", "2026-10-17 15:30"),
            ("2026-10-16 14:05", "2026-10-16 16:00"),
        ]);
        assert_eq!(next_cut(&o, at("2026-10-18 10:00") as f64, false), None);
    }

    #[test]
    fn next_cut_ignores_short_cuts() {
        // real: the 10-13 12:18–12:24 flicker is in history but never a candidate
        let (tb, t) = (table(), at("2026-10-14 08:00"));
        let k = known(&tb, t);
        assert!(k.iter().any(|o| o.start == at("2026-10-13 12:18")));
        let starts: Vec<i64> = candidates(&k, t.div_euclid(DAY)).filter(|o| o.len() >= MIN_CUT).map(|o| o.start).collect();
        assert!(!starts.contains(&at("2026-10-13 12:18")));
    }

    #[test]
    fn next_cut_drops_run_open_at_oldest_day() {
        // made-up: three days have a 00:00 cut, but on day −14 (10-13) it is the open run → 2 days, none
        let o = made(&[
            ("2026-10-13 00:00", "2026-10-13 02:00"),
            ("2026-10-14 00:00", "2026-10-14 02:00"),
            ("2026-10-15 00:00", "2026-10-15 02:00"),
        ]);
        assert_eq!(next_cut(&o, at("2026-10-27 20:00") as f64, false), None);
        let o2: Vec<_> = o.iter().map(|x| Outage { start: x.start + 1, ..*x }).collect(); // 00:01: a real cut
        assert!(next_cut(&o2, at("2026-10-27 20:00") as f64, false).is_some());
    }

    // ---- since from history (rule (a)) ----

    #[test]
    fn history_start_cases() {
        let tb = table();
        let d0 = at("2026-10-13 00:00").div_euclid(DAY);
        let start = |t: &str, days: &BTreeMap<i64, GridDay>| history_start(days, at(t));
        let days = days_from(&tb, d0, at("2026-10-18 11:22"));
        assert_eq!(start("2026-10-18 11:22", &days), HistStart::Start(at("2026-10-18 09:07")));
        // more than 10 min since history's newest point
        assert_eq!(start("2026-10-18 11:33", &days), HistStart::Stale);
        assert_eq!(start("2026-10-18 11:32", &days), HistStart::Start(at("2026-10-18 09:07")));
        // only 2 off minutes at the newest point: not off yet
        let days = days_from(&tb, d0, at("2026-10-18 09:08"));
        assert_eq!(start("2026-10-18 09:08", &days), HistStart::NotOff);
        let days = days_from(&tb, d0, at("2026-10-18 09:09"));
        assert_eq!(start("2026-10-18 09:09", &days), HistStart::Start(at("2026-10-18 09:07")));
    }

    #[test]
    fn texts() {
        assert_eq!(since_text(at("2026-10-18 09:07"), at("2026-10-18 11:22")), "Since 9:07\u{a0}AM (2h 15m)");
        assert_eq!(since_text(at("2026-10-17 23:33"), at("2026-10-18 00:40")), "Since yesterday 11:33\u{a0}PM (1h 7m)");
        assert_eq!(hm(at("2026-10-18 00:00") - 1), "11:59\u{a0}PM");
        assert_eq!(clock_dist(at("2026-10-17 23:50"), at("2026-10-15 00:10")), 20);
        assert_eq!(median(&[1, 2, 3, 4]), 2.5);
        assert_eq!(agreed(&[0, 10, 30, 31, 40, 61]), &[10, 30, 31, 40]);
    }
}
