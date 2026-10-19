//! The app's live state and the poll loop. Plain Rust: the tray, panel and notifications
//! read a `Monitor` and react to `PollReport`s; tests drive it with a fake `Source`.

use crate::clock::LocalTs;
use crate::outages::{self, GridDay, HistStart, Outage, DAY, PAST_DAYS};
use crate::rules::{self, ChartSource, GridState};
use crate::sems::{self, ChartSeries, Day, GridSignal, Reading, SemsError};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Notify};

/// Where the readings come from. `sems::Client` in the app, a fake in tests.
pub trait Source: Send {
    fn signed_in(&self) -> bool;
    fn sign_out(&mut self);
    fn reading(&mut self) -> impl Future<Output = Result<Reading, SemsError>> + Send;
    /// One station-local day: today's chart and grid history, or a past day's history.
    fn day(&mut self, date: LocalTs, utc_offset_h: Option<f64>) -> impl Future<Output = Result<Day, SemsError>> + Send;
    /// The history walk's past-day call: started while holding the source, run after letting it
    /// go (T-015 § 4.7). It still waits for any other SEMS+ day call. `None`: no session yet.
    fn day_detached(&mut self, date: LocalTs, utc_offset_h: Option<f64>) -> Option<impl Future<Output = Result<Day, SemsError>> + Send + 'static>;
    /// The walk saw the SEMS+ session expire: the poll's next day call signs in again.
    fn plus_expired(&mut self);
    /// The signed-in station's id. Never logged.
    fn station(&self) -> Option<String>;
}

impl Source for sems::Client {
    fn signed_in(&self) -> bool {
        sems::Client::signed_in(self)
    }
    fn sign_out(&mut self) {
        sems::Client::sign_out(self)
    }
    async fn reading(&mut self) -> Result<Reading, SemsError> {
        sems::Client::reading(self).await
    }
    async fn day(&mut self, date: LocalTs, utc_offset_h: Option<f64>) -> Result<Day, SemsError> {
        sems::Client::day(self, date, utc_offset_h).await
    }
    fn day_detached(&mut self, date: LocalTs, utc_offset_h: Option<f64>) -> Option<impl Future<Output = Result<Day, SemsError>> + Send + 'static> {
        sems::Client::day_detached(self, date, utc_offset_h)
    }
    fn plus_expired(&mut self) {
        sems::Client::plus_expired(self)
    }
    fn station(&self) -> Option<String> {
        sems::Client::station(self)
    }
}

/// A past day is fetched for good once the station clock is this far past its end (the reply
/// runs about 2 minutes behind, so a fetch at 00:01 would miss 23:58 and 23:59).
const DAY_FINAL_AFTER_MIN: i64 = 10;
/// Past days fetched per poll at most (cold start: 14 days over five polls).
const DAYS_PER_POLL: usize = 3;
/// S7a: the On reading before the first Off one must be this close for an exact "Since 7:42 AM"
/// (an On reading hours older, e.g. the Mac slept, doesn't show when the cut began).
const SEEN_LIVE_MAX_GAP_S: i64 = 600;
/// D-O11: a day inside the history range that SEMS answers empty on this many launches is kept as "no data".
const EMPTY_LAUNCHES: u8 = 3;
/// Temperature readings older than this can't give a trend (the Mac slept): T-015 § 1.
const TREND_WITHIN_S: i64 = 40 * 60;
/// The history walk's pace, and its back-off when SEMS asks to slow down (T-015 § 4.9; spike:
/// no GY0429 at one call per ~4 s).
const WALK_GAP: Duration = Duration::from_secs(2);
const WALK_BACKOFF: Duration = Duration::from_secs(60);
const WALK_BACKOFF_MAX: Duration = Duration::from_secs(600);
/// Failed walk calls in a row that stop it until the next poll (or launch).
const WALK_FAILS: u32 = 3;

/// The history walk's state (T-015 § 4.6), read by Stats.
#[derive(Debug, Default)]
pub struct Walk {
    /// Days still to fetch when this pass started (the footer's total); 0 = idle.
    pub total: usize,
    /// Days still to fetch now.
    pub left: usize,
    /// SEMS said slow down (GY0429): the walk goes on at this station time.
    pub paused_until: Option<LocalTs>,
    /// Days whose call failed this pass: fetched on a later pass.
    skip: BTreeSet<i64>,
}

/// `history.json` (T-015 § 4.2): the station it belongs to and its days as runs.
#[derive(Serialize, Deserialize)]
struct HistoryFile {
    station: Option<String>,
    days: BTreeMap<i64, FileDay>,
}

#[derive(Serialize, Deserialize)]
struct FileDay {
    runs: Vec<(u32, u32, bool)>,
    #[serde(default)]
    gaps: Vec<(u32, u32)>,
    newest: Option<u32>,
    /// Launches that got an empty answer for this day (D-O11).
    #[serde(default, skip_serializing_if = "is_zero")]
    empty: u8,
}

fn is_zero(n: &u8) -> bool {
    *n == 0
}

/// When the app first saw the grid off. `exact` is false when it was already off at the
/// first reading after launch, or the reading before wasn't a recent On ("Since at least 8:20 AM").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OffSince {
    pub at: LocalTs,
    pub exact: bool,
}

/// A grid change worth a notification.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub grid: GridState,
    pub soc: Option<f64>,
}

impl Notice {
    /// "Grid is off · battery 86%" / "Grid is back · battery 62%" / "Grid is off".
    pub fn text(&self) -> String {
        let head = if self.grid == GridState::Off { "Grid is off" } else { "Grid is back" };
        match self.soc {
            Some(s) => format!("{head} · battery {}%", s.round()),
            None => head.into(),
        }
    }
}

/// The chart as panel.js takes it (`state.chart`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChartView {
    pub status: &'static str,
    pub now: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// Station-local date of the chart, "18 Oct" (read-out line 1).
    pub day: String,
    pub gap: u32,
    pub points: Vec<(u32, f64, f64)>,
}

#[derive(Debug)]
pub struct Monitor {
    pub signed_in: bool,
    pub grid: GridState,
    pub off_since: Option<OffSince>,
    pub last: Option<Reading>,
    /// App-clock moment `last.time` was first seen.
    seen_at: Option<Instant>,
    pub failures: u32,
    /// `info.time` of the first reading since launch.
    first_reading: Option<LocalTs>,
    /// One point per new `info.time`, today only: the chart's fallback.
    own: Vec<(LocalTs, f64, f64)>,
    plus: Option<ChartSeries>,
    plus_failed: bool,
    /// SEMS+ grid history per station-local day (`LocalTs::day`): today and every past day loaded.
    pub(crate) history: BTreeMap<i64, GridDay>,
    /// Past days fetched after they ended: never fetched again.
    pub(crate) history_final: BTreeSet<i64>,
    /// Empty answers per day inside the history range, counted once per launch (D-O11).
    history_empty: BTreeMap<i64, u8>,
    /// Days answered empty this launch: not asked again until the next one.
    empty_tried: BTreeSet<i64>,
    /// Days read from v0.1.0's file (no gap data): fetched once more, the new answer replaces them.
    history_migrated: BTreeSet<i64>,
    /// The station the history belongs to (classic login; the file's `station`). Never logged.
    station: Option<String>,
    /// The day Stats shows (None: today); the walk fetches it and its neighbours first (§ 4.8).
    pub stats_day: Option<i64>,
    /// Stats' calendar while open: the month shown (its 1st) and the day keys moved focus to.
    pub stats_cal: Option<(i64, Option<i64>)>,
    pub walk: Walk,
    /// The first history call of this run has answered or failed (Since rule S1).
    pub history_tried: bool,
    /// What history said at the last poll about the current outage's start.
    pub hist_start: HistStart,
    /// The current outage's start from history (S2/S3), kept while history is stale (S6).
    pub since_start: Option<i64>,
    /// The signed-in SEMS account (Credentials.account) for the panel's footer. Never logged.
    pub account: Option<String>,
    /// A poll is running.
    pub polling: bool,
    /// The running (or next) poll was started by the user (Refresh, ⌘R, sign-in): the icon spins.
    pub user_poll: bool,
    /// Bumped on sign-out and on a new account: a poll that began before drops its result.
    gen: u64,
    /// `app_data_dir()/history.json`: the finished past days, kept across launches (T-012).
    history_file: Option<PathBuf>,
    /// A day became final, or one was pruned, since the file was written.
    history_unsaved: bool,
    /// Settings › General › Low battery alert, in % (T-008).
    pub low_pct: u8,
    /// Low battery levels already alerted during this outage.
    low_fired: Vec<u8>,
    /// A low battery alert for the shell (the battery %), taken by `poll_once`.
    low_alert: Option<f64>,
    /// Settings › General › Show temperatures (T-015, default on).
    pub show_temps: bool,
    /// (time, inverter °C, battery °C) per new reading, back to the newest one 30 min old: the trend.
    pub(crate) temps: Vec<(LocalTs, Option<f64>, Option<f64>)>,
}

impl Monitor {
    pub fn new(signed_in: bool) -> Monitor {
        Monitor {
            signed_in,
            grid: GridState::Unknown,
            off_since: None,
            last: None,
            seen_at: None,
            failures: 0,
            first_reading: None,
            own: Vec::new(),
            plus: None,
            plus_failed: false,
            history: BTreeMap::new(),
            history_final: BTreeSet::new(),
            history_empty: BTreeMap::new(),
            empty_tried: BTreeSet::new(),
            history_migrated: BTreeSet::new(),
            station: None,
            stats_day: None,
            stats_cal: None,
            walk: Walk::default(),
            history_tried: false,
            hist_start: HistStart::Stale,
            since_start: None,
            account: None,
            polling: false,
            user_poll: false,
            gen: 0,
            history_file: None,
            history_unsaved: false,
            low_pct: rules::LOW_BATTERY_DEFAULT,
            low_fired: Vec::new(),
            low_alert: None,
            show_temps: true,
            temps: Vec::new(),
        }
    }

    /// Sign Out (or another account signs in): a fresh monitor. Keeps the poll flag, and bumps
    /// `gen` so a poll already running drops its result instead of signing the panel back in.
    /// The saved history belongs to the old account too: its file goes.
    pub fn reset(&mut self, signed_in: bool) {
        if let Some(p) = &self.history_file {
            let _ = std::fs::remove_file(p);
        }
        *self = Monitor {
            polling: self.polling,
            gen: self.gen + 1,
            history_file: self.history_file.take(),
            low_pct: self.low_pct,
            show_temps: self.show_temps,
            ..Monitor::new(signed_in)
        };
    }

    /// At launch: the past days saved by an earlier run. Missing or unreadable = none; true when
    /// a file was there but unreadable (it is replaced at the next save). The station is checked
    /// at the first reading (`set_station`).
    pub fn load_history(&mut self, path: PathBuf) -> bool {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut unreadable = false;
        let (station, days): (Option<String>, Vec<(i64, GridDay, u8)>) = match serde_json::from_str::<HistoryFile>(&text) {
            Ok(f) => (f.station, f.days.into_iter().map(|(d, x)| (d, GridDay::from_runs(&x.runs, x.gaps, x.newest), x.empty)).collect()),
            Err(_) => {
                // v0.1.0's bare {day: {off, newest}} map (T-015 § 4.2): no gap data, so its days
                // stay shown but not final, and the walk fetches each once more (B-O1).
                let old = serde_json::from_str::<BTreeMap<i64, GridDay>>(&text);
                unreadable = old.is_err() && !text.trim().is_empty();
                let old = old.unwrap_or_default();
                self.history_unsaved = !old.is_empty();
                self.history_migrated = old.keys().copied().collect();
                for (d, g) in old {
                    self.history.insert(d, g);
                }
                (None, vec![])
            }
        };
        for (d, g, empty) in days {
            // An empty answer is final only after EMPTY_LAUNCHES launches (D-O11), and a day kept
            // with data over an empty answer is fetched again.
            if (g.newest.is_some() && empty == 0) || empty >= EMPTY_LAUNCHES {
                self.history_final.insert(d);
            }
            if empty > 0 {
                self.history_empty.insert(d, empty);
            }
            self.history.insert(d, g);
        }
        self.station = station;
        self.history_file = Some(path);
        unreadable
    }

    /// Writes the finished past days and the empty counters (never today, never an account
    /// detail besides the station id this file must name), tmp + rename.
    fn save_history(&self) {
        let Some(path) = &self.history_file else { return };
        let days = self
            .history
            .iter()
            .filter(|(d, _)| self.history_final.contains(d) || self.history_empty.contains_key(d))
            .map(|(d, g)| (*d, FileDay { runs: g.to_runs(), gaps: g.gaps.clone(), newest: g.newest, empty: self.history_empty.get(d).copied().unwrap_or(0) }))
            .collect();
        let file = HistoryFile { station: self.station.clone(), days };
        let tmp = path.with_extension("tmp");
        let ok = path.parent().is_none_or(|d| std::fs::create_dir_all(d).is_ok())
            && std::fs::write(&tmp, serde_json::to_string(&file).expect("history serialises")).is_ok();
        if ok {
            let _ = std::fs::rename(tmp, path);
        }
    }

    /// Saves when a day changed since the last write.
    fn persist(&mut self) {
        if self.history_unsaved {
            self.save_history();
            self.history_unsaved = false;
        }
    }

    /// The signed-in station, at each reading (T-015 § 4.2): a file without one (v0.1.0) is taken
    /// as this station's; one naming another station is dropped and replaced.
    pub fn set_station(&mut self, id: Option<String>) {
        let Some(id) = id else { return };
        match &self.station {
            Some(s) if *s == id => return,
            Some(_) => self.forget_history(),
            None => {}
        }
        self.station = Some(id);
        self.history_unsaved = true;
    }

    /// The reading the temperature trend compares with: 30 to 40 min before the newest (T-015 § 1).
    pub fn temp_base(&self) -> Option<&(LocalTs, Option<f64>, Option<f64>)> {
        let (first, last) = (self.temps.first()?, self.temps.last()?);
        (rules::TREND_AFTER_S..=TREND_WITHIN_S).contains(&(last.0 .0 - first.0 .0)).then_some(first)
    }

    /// A new launch, as far as D-O11's once-per-launch count goes.
    #[cfg(test)]
    pub(crate) fn empty_tried_clear_for_test(&mut self) {
        self.empty_tried.clear();
    }

    /// The station's first day of history (`create_time`); SEMS+ has nothing before it.
    pub fn first_day(&self) -> Option<i64> {
        self.last.as_ref()?.created.map(LocalTs::day)
    }

    /// Refresh click or ⌘R. `None`: ignore it (signed out, or a user poll already runs).
    /// `Some(start)`: show the spin now; `start` = no poll is running, so start one (a running
    /// timer poll becomes the user's instead of queueing a second).
    pub fn ask_refresh(&mut self) -> Option<bool> {
        if !self.signed_in || self.user_poll {
            return None;
        }
        self.user_poll = true;
        Some(!self.polling)
    }

    /// Takes a successful reading. Returns a notice on a confirmed grid change.
    pub fn on_reading(&mut self, r: Reading, now: Instant) -> Option<Notice> {
        self.signed_in = true;
        self.failures = 0;
        if self.last.as_ref().is_some_and(|l| l.time == r.time) {
            return None; // the same reading again; staleness keeps counting from `seen_at`
        }
        self.seen_at = Some(now);
        self.first_reading.get_or_insert(r.time);

        let prev = self.grid;
        let next = rules::next_grid(prev, r.grid);
        // A new outage, or the grid back: forget the history start (it belongs to "this outage").
        match next {
            GridState::Off if prev != GridState::Off => {
                let seen = prev == GridState::On && self.last.as_ref().is_some_and(|l| r.time.0 - l.time.0 <= SEEN_LIVE_MAX_GAP_S);
                self.off_since = Some(OffSince { at: r.time, exact: seen });
                self.since_start = None;
            }
            GridState::On => (self.off_since, self.since_start) = (None, None),
            _ => {}
        }
        self.grid = next;

        if let Some(b) = &r.battery {
            self.own.retain(|p| p.0.day() == r.time.day());
            self.own.push((r.time, b.soc, b.watts / 1000.0));
        }
        // Keep the newest reading at least 30 min old and every one after it (the trend's base).
        self.temps.push((r.time, r.inverter_c, r.battery_c));
        if let Some(i) = self.temps.iter().rposition(|p| r.time.0 - p.0 .0 >= rules::TREND_AFTER_S) {
            self.temps.drain(..i);
        }
        if let Some(b) = &r.battery {
            // Never on the first reading (prev Unknown): levels already passed are marked silently.
            let level = rules::low_battery(&mut self.low_fired, self.low_pct, b.soc, next == GridState::Off);
            if prev != GridState::Unknown && level.is_some() {
                self.low_alert = Some(b.soc);
            }
        }
        let notice = rules::grid_changed(prev, next).then(|| Notice { grid: next, soc: r.battery.as_ref().map(|b| b.soc) });
        self.last = Some(r);
        notice
    }

    pub fn on_failure(&mut self) {
        self.failures += 1;
    }

    pub fn on_chart(&mut self, c: Option<ChartSeries>) {
        match c {
            Some(c) => (self.plus, self.plus_failed) = (Some(c), false),
            None => self.plus_failed = true,
        }
    }

    /// Stores one day's grid history. `now` = station now, absolute minutes. A past day fetched
    /// 10 minutes after its end is final (T-015 § 4.5) and never replaced; an empty answer only
    /// after EMPTY_LAUNCHES launches (D-O11); never known data replaced with less (§ 4.11).
    pub fn on_day(&mut self, day: i64, mut grid: GridDay, now: i64) {
        if self.history_final.contains(&day) {
            return;
        }
        let ended = now >= (day + 1) * DAY + DAY_FINAL_AFTER_MIN;
        let old = self.history.get(&day);
        if grid.newest.is_none() {
            if ended && self.empty_tried.insert(day) {
                let n = self.history_empty.entry(day).or_default();
                *n += 1;
                if *n >= EMPTY_LAUNCHES {
                    self.history_final.insert(day);
                }
                self.history_unsaved = true;
            }
            if old.is_none_or(|o| o.newest.is_none()) {
                self.history.insert(day, grid);
            }
            return;
        }
        // A v0.1.0 day had no gap data: its refetch replaces it, even with less (B-O1).
        let migrated = self.history_migrated.remove(&day);
        if let Some(o) = old.filter(|o| !migrated && (o.newest > grid.newest || o.gap_minutes() < grid.gap_minutes())) {
            grid = o.clone();
        }
        if ended {
            // Data that stopped before 23:59 has a gap at its end, not a refetch every launch.
            let last = grid.newest.unwrap_or(0);
            if DAY as u32 - 1 - last > 5 {
                grid.gaps.push((last + 1, DAY as u32 - 1));
            }
            self.history_final.insert(day);
            self.history_empty.remove(&day);
            self.history_unsaved = true;
        }
        self.history.insert(day, grid);
    }

    /// A past day may be asked for: from the station's first day, not final, not answered empty
    /// this launch, and missing or fetched before it had ended once it has.
    fn wants(&self, d: i64, now: i64) -> bool {
        self.first_day().is_none_or(|f| d >= f)
            && d < now.div_euclid(DAY)
            && !self.history_final.contains(&d)
            && !self.empty_tried.contains(&d)
            && (!self.history.contains_key(&d) || now >= (d + 1) * DAY + DAY_FINAL_AFTER_MIN)
    }

    /// Past days still to fetch, newest first (yesterday feeds the Since line): missing ones, and
    /// ones fetched before they had ended once they have. At most `DAYS_PER_POLL`.
    pub fn days_to_fetch(&self, now: i64) -> Vec<i64> {
        let today = now.div_euclid(DAY);
        (today - PAST_DAYS..today).rev().filter(|&d| self.wants(d, now)).take(DAYS_PER_POLL).collect()
    }

    /// The history walk's next day (T-015 § 4.6, § 4.8): the day Stats shows and its neighbours
    /// first, then newest first back to the station's first day. None: nothing to do, or not yet
    /// (no battery, no station or first day known). Keeps `walk`'s counters.
    pub fn walk_next(&mut self, now: i64) -> Option<i64> {
        let first = self.first_day().filter(|_| self.has_history() && self.station.is_some());
        let left: Vec<i64> = first.map_or(vec![], |f| (f..now.div_euclid(DAY)).rev().filter(|&d| self.wants(d, now)).collect());
        let shown = self.stats_day.map_or(vec![], |d| vec![d, d - 1, d + 1]);
        let next = shown.into_iter().chain(left.iter().copied()).find(|d| left.contains(d) && !self.walk.skip.contains(d));
        self.walk.left = left.len();
        match next {
            Some(_) if self.walk.total == 0 => self.walk.total = left.len(),
            Some(_) => {}
            None => (self.walk.total, self.walk.skip) = (0, BTreeSet::new()),
        }
        next
    }

    /// After a poll's history calls: save what changed, then read the current outage's start.
    /// Every day is kept (Stats); the predictions look at the last 14 only.
    pub fn after_history(&mut self, now: i64) {
        self.persist();
        self.history_tried = true;
        self.hist_start = outages::history_start(&self.history, now);
        if let (GridState::Off, HistStart::Start(s)) = (self.grid, self.hist_start) {
            self.since_start = Some(s);
        }
    }

    /// Sign-out or another account: the history belongs to the old station.
    pub fn forget_history(&mut self) {
        self.history.clear();
        self.history_final.clear();
        self.history_empty.clear();
        self.empty_tried.clear();
        self.history_migrated.clear();
        (self.station, self.stats_day, self.stats_cal, self.walk) = (None, None, None, Walk::default());
        (self.history_tried, self.hist_start, self.since_start) = (false, HistStart::Stale, None);
    }

    /// History is read only for stations with a battery (the off rule needs pBat).
    pub fn has_history(&self) -> bool {
        self.last.as_ref().is_some_and(|r| r.battery.is_some())
    }

    /// The 14 days before `today` all loaded with data: only then may a line say "(last 14 days)".
    pub fn past_days_loaded(&self, today: i64) -> bool {
        (today - PAST_DAYS..today).all(|d| self.history.get(&d).is_some_and(|g| g.newest.is_some()))
    }

    pub fn outages(&self, today: i64) -> Vec<Outage> {
        outages::outages(&self.history, today)
    }

    pub fn has_today(&self, today: i64) -> bool {
        self.history.get(&today).is_some_and(|g| g.newest.is_some())
    }

    pub fn stale(&self, now: Instant) -> bool {
        rules::is_stale(self.seen_at.map(|t| now.duration_since(t)), self.failures)
    }

    /// The station's wall clock now: the last `info.time` moved on by the app's clock.
    pub fn station_now(&self, now: Instant) -> Option<LocalTs> {
        let t = self.last.as_ref()?.time;
        Some(t.plus_secs(now.duration_since(self.seen_at?).as_secs() as i64))
    }

    /// `None` when there is no chart to show (signed out, no battery).
    pub fn chart_view(&self, now: Instant) -> Option<ChartView> {
        if !self.signed_in {
            return None;
        }
        let Some(last) = &self.last else {
            return Some(ChartView { status: "loading", now: 0, since: None, day: String::new(), gap: ChartSource::Plus.gap(), points: vec![] });
        };
        last.battery.as_ref()?;
        let today = self.station_now(now)?;
        let view = |status, now, gap, points, since| Some(ChartView { status, now, since, day: today.day_month(), gap, points });
        let now_min = today.minute_of_day();
        match self.plus.as_ref().filter(|p| p.day == today.day()) {
            Some(p) if p.points.is_empty() => view("empty", now_min, ChartSource::Plus.gap(), vec![], None),
            Some(p) => {
                let end = now_min.max(p.points.last().map_or(0, |x| x.0));
                view("ok", end, ChartSource::Plus.gap(), p.points.clone(), None)
            }
            None if !self.plus_failed => view("loading", now_min, ChartSource::Plus.gap(), vec![], None),
            None => {
                let pts: Vec<_> = self.own.iter().filter(|p| p.0.day() == today.day()).map(|p| (p.0.minute_of_day(), p.1, p.2)).collect();
                if pts.is_empty() {
                    return view("failed", now_min, ChartSource::Own.gap(), pts, None);
                }
                // "Since 7:58 AM" only when the app started today; otherwise the day from 00:00.
                let since = self.first_reading.filter(|f| f.day() == today.day()).map(|f| f.hhmm12());
                view("fallback", now_min, ChartSource::Own.gap(), pts, since)
            }
        }
    }
}

/// What one poll did, for the shell (tray, notifications, log).
#[derive(Debug, Default, PartialEq)]
pub struct PollReport {
    pub notice: Option<Notice>,
    /// (reason, raw reply) for each reply that could not be read: dump it redacted.
    pub bad_replies: Vec<(String, String)>,
    /// Raw grid values when voltage and status disagreed: log them.
    pub disagreement: Option<String>,
    /// Errors worth a log line (network, expired, signed out).
    pub errors: Vec<String>,
    /// The battery % when it reached a low battery level while the grid is off (T-008).
    pub low_battery: Option<f64>,
}

/// One poll. The reading's part of the report (grid notice, low battery, disagreement) goes to
/// `live` before any day call, so the alerts and the Tuya switch never wait for today's chart or
/// the past days (B-O1); the returned report holds the rest. A failed reading only returns.
pub async fn poll_once<S: Source>(src: &tokio::sync::Mutex<S>, mon: &Mutex<Monitor>, mut live: impl FnMut(PollReport)) -> PollReport {
    let mut rep = PollReport::default();
    let mut src = src.lock().await;
    let gen = mon.lock().unwrap().gen;
    // The monitor, unless Sign Out (or another account) came while this poll ran: then the
    // result belongs to the old session and is dropped.
    let current = || Some(mon.lock().unwrap()).filter(|m| m.gen == gen);
    if !src.signed_in() {
        mon.lock().unwrap().signed_in = false;
        return rep;
    }
    let r = match src.reading().await {
        Ok(r) => r,
        Err(e) => {
            rep.errors.push(e.to_string());
            let Some(mut m) = current() else { return rep };
            match e {
                SemsError::WrongLogin(_) | SemsError::SignedOut => {
                    src.sign_out();
                    m.reset(false);
                }
                SemsError::Parse { reason, reply } => {
                    rep.bad_replies.push((reason, reply));
                    m.on_failure();
                }
                SemsError::Network(_) | SemsError::Expired | SemsError::RateLimited => m.on_failure(),
            }
            return rep;
        }
    };
    if r.grid == GridSignal::Disagree {
        rep.disagreement = Some(format!("vac1 {} V, grid_conn_status {:?}", r.vac1, r.grid_status));
    }
    let (has_battery, offset) = (r.battery.is_some(), r.utc_offset_h);
    let today = {
        let Some(mut m) = current() else { return rep };
        m.set_station(src.station());
        rep.notice = m.on_reading(r, Instant::now());
        rep.low_battery = m.low_alert.take();
        m.station_now(Instant::now())
    };
    live(std::mem::take(&mut rep));
    if let (true, Some(today)) = (has_battery, today) {
        let now = today.0.div_euclid(60);
        // Today: chart and history are one call, so no extra request.
        let (chart, grid) = match src.day(today, offset).await {
            Ok(d) => {
                let chart = d.chart.map_err(|e| failed(&mut rep, "chart", e)).ok();
                (chart, d.grid.map_err(|e| failed(&mut rep, &format!("history {}", today.date()), e)).ok())
            }
            Err(e) => {
                failed(&mut rep, "chart", e);
                (None, None)
            }
        };
        let todo = {
            let Some(mut m) = current() else { return rep };
            m.on_chart(chart);
            if let Some(g) = grid {
                m.on_day(today.day(), g, now);
            }
            m.days_to_fetch(now)
        };
        // Past days, newest first. A network error is tried once more; a day that still fails is
        // skipped (the next poll tries it again) and the older days still load (T-012). Only a
        // session problem stops this poll's history.
        for d in todo {
            let date = LocalTs(d * 86_400);
            let mut got = src.day(date, offset).await.and_then(|x| x.grid);
            if matches!(got, Err(SemsError::Network(_))) {
                got = src.day(date, offset).await.and_then(|x| x.grid);
            }
            match got {
                Ok(g) => match current() {
                    Some(mut m) => m.on_day(d, g, now),
                    None => return rep,
                },
                Err(e) => {
                    let stop = matches!(e, SemsError::Expired | SemsError::SignedOut | SemsError::WrongLogin(_) | SemsError::RateLimited);
                    failed(&mut rep, &format!("history {}", date.date()), e);
                    if stop {
                        break;
                    }
                }
            }
        }
        if let Some(mut m) = current() {
            m.after_history(now);
        }
    }
    rep
}

/// Logs a failed SEMS+ read; an unreadable reply also goes to the redacted dump.
fn failed(rep: &mut PollReport, what: &str, e: SemsError) {
    rep.errors.push(format!("{what}: {e}"));
    // Today's chart and grid come from one reply: an unreadable one is dumped once (T-004).
    if let SemsError::Parse { reason, reply } = e {
        if !rep.bad_replies.iter().any(|b| b.1 == reply) {
            rep.bad_replies.push((format!("{what}: {reason}"), reply));
        }
    }
}

/// The history walk (T-015 § 4.6–4.9): every past day back to the station's first, one call at a
/// time, in its own task beside `run`. It takes the source only to start a call and lets it go
/// while the call runs, so a poll's reading, its alerts and the Tuya switch never wait for it; the
/// call itself queues behind the poll's day calls (one SEMS+ history call at a time). `wake`: each
/// poll's end, and Stats showing another day. `report`: log lines, and a redraw for the footer.
pub async fn walk<S: Source>(src: Arc<tokio::sync::Mutex<S>>, mon: Arc<Mutex<Monitor>>, wake: Arc<Notify>, mut report: impl FnMut(PollReport) + Send) {
    let (mut backoff, mut fails, mut busy) = (WALK_BACKOFF, 0, false);
    loop {
        let job = {
            let mut s = src.lock().await;
            let mut m = mon.lock().unwrap();
            let (gen, offset) = (m.gen, m.last.as_ref().and_then(|r| r.utc_offset_h));
            let now = m.station_now(Instant::now()).map(|t| t.0.div_euclid(60));
            now.and_then(|now| m.walk_next(now)).and_then(|d| Some((d, gen, s.day_detached(LocalTs(d * 86_400), offset)?)))
        };
        let Some((d, gen, call)) = job else {
            if std::mem::take(&mut busy) {
                report(PollReport::default()); // the footer's counter goes
            }
            wake.notified().await;
            continue;
        };
        busy = true;
        let got = call.await.and_then(|x| x.grid);
        let mut rep = PollReport::default();
        let current = || Some(mon.lock().unwrap()).filter(|m| m.gen == gen);
        let what = format!("history {}", LocalTs(d * 86_400).date());
        match got {
            Ok(g) => {
                if let Some(mut m) = current() {
                    if let Some(now) = m.station_now(Instant::now()) {
                        m.on_day(d, g, now.0.div_euclid(60));
                        m.persist();
                    }
                }
                (backoff, fails) = (WALK_BACKOFF, 0);
                report(rep);
                tokio::time::sleep(WALK_GAP).await;
            }
            Err(SemsError::RateLimited) => {
                failed(&mut rep, &what, SemsError::RateLimited);
                if let Some(mut m) = current() {
                    m.walk.paused_until = m.station_now(Instant::now()).map(|t| t.plus_secs(backoff.as_secs() as i64));
                }
                report(rep);
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(WALK_BACKOFF_MAX);
                if let Some(mut m) = current() {
                    m.walk.paused_until = None;
                }
            }
            Err(e) => {
                let expired = e == SemsError::Expired;
                failed(&mut rep, &what, e);
                if expired {
                    src.lock().await.plus_expired();
                } else if let Some(mut m) = current() {
                    m.walk.skip.insert(d);
                }
                fails += 1;
                report(rep);
                // Expired: the next poll signs in again; 3 failures in a row: try again at the next poll.
                if expired || fails >= WALK_FAILS {
                    fails = 0;
                    wake.notified().await;
                } else {
                    tokio::time::sleep(WALK_GAP).await;
                }
            }
        }
    }
}

/// Polls now, then every `POLL_EVERY`, and at once on each `refresh` message.
/// Ends when every refresh sender is dropped.
pub async fn run<S: Source>(
    src: Arc<tokio::sync::Mutex<S>>,
    mon: Arc<Mutex<Monitor>>,
    mut refresh: mpsc::Receiver<()>,
    mut on_report: impl FnMut(PollReport) + Send,
) {
    loop {
        mon.lock().unwrap().polling = true;
        let rep = poll_once(&src, &mon, &mut on_report).await;
        // Before the report, whose redraw then shows the spin stopped.
        {
            let mut m = mon.lock().unwrap();
            (m.polling, m.user_poll) = (false, false);
        }
        on_report(rep);
        tokio::select! {
            _ = tokio::time::sleep(rules::POLL_EVERY) => {}
            r = refresh.recv() => if r.is_none() { return },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sems::Battery;
    use std::collections::VecDeque;
    use std::time::Duration;

    fn reading(time: &str, grid: GridSignal, soc: f64, watts: f64) -> Reading {
        Reading {
            time: LocalTs::parse(time).unwrap(),
            grid,
            vac1: if grid == GridSignal::Off { 0.1 } else { 230.0 },
            grid_status: None,
            battery: Some(Battery { soc, watts, capacity_kwh: Some(5.1) }),
            utc_offset_h: Some(5.0),
            inverter_c: None,
            battery_c: None,
            created: LocalTs::parse("09/18/2026 18:28:53"),
        }
    }

    #[test]
    fn notices_and_off_since() {
        let t0 = Instant::now();
        let mut m = Monitor::new(true);
        // first reading during an outage: silent, "since at least"
        assert_eq!(m.on_reading(reading("10/18/2026 08:20:00", GridSignal::Off, 86.0, 998.0), t0), None);
        assert_eq!(m.off_since, Some(OffSince { at: LocalTs::parse("10/18/2026 08:20:00").unwrap(), exact: false }));
        // grid back
        let n = m.on_reading(reading("10/18/2026 08:25:00", GridSignal::On, 62.0, -1200.0), t0).unwrap();
        assert_eq!(n.text(), "Grid is back · battery 62%");
        assert_eq!(m.off_since, None);
        // a disagreement keeps "on", no notice
        assert_eq!(m.on_reading(reading("10/18/2026 08:30:00", GridSignal::Disagree, 62.0, 0.0), t0), None);
        assert_eq!(m.grid, GridState::On);
        // grid off again: exact since
        let n = m.on_reading(reading("10/18/2026 08:35:00", GridSignal::Off, 86.4, 998.0), t0).unwrap();
        assert_eq!(n.text(), "Grid is off · battery 86%");
        assert!(m.off_since.unwrap().exact);
        // the same reading again changes nothing
        assert_eq!(m.on_reading(reading("10/18/2026 08:35:00", GridSignal::On, 86.0, 998.0), t0), None);
        assert_eq!(m.grid, GridState::Off);
        let mut nb = Notice { grid: GridState::Off, soc: None };
        assert_eq!(nb.text(), "Grid is off");
        nb.grid = GridState::On;
        assert_eq!(nb.text(), "Grid is back");
    }

    #[test]
    fn stale_by_unchanged_time_or_failures() {
        let t0 = Instant::now();
        let mut m = Monitor::new(true);
        m.on_reading(reading("10/18/2026 08:20:00", GridSignal::On, 86.0, 0.0), t0);
        // same info.time for 15 min of app clock
        m.on_reading(reading("10/18/2026 08:20:00", GridSignal::On, 86.0, 0.0), t0 + Duration::from_secs(600));
        assert!(!m.stale(t0 + Duration::from_secs(14 * 60)));
        assert!(m.stale(t0 + Duration::from_secs(15 * 60)));
        // a new reading clears it; three failures set it
        m.on_reading(reading("10/18/2026 08:40:00", GridSignal::On, 86.0, 0.0), t0 + Duration::from_secs(16 * 60));
        let t = t0 + Duration::from_secs(16 * 60);
        assert!(!m.stale(t));
        m.on_failure();
        m.on_failure();
        assert!(!m.stale(t));
        m.on_failure();
        assert!(m.stale(t));
        // the station clock moves on by the app clock
        assert_eq!(m.station_now(t + Duration::from_secs(300)).unwrap().hhmm(), "08:45");
    }

    #[test]
    fn chart_states() {
        let t0 = Instant::now();
        let mut m = Monitor::new(true);
        assert_eq!(m.chart_view(t0).unwrap().status, "loading");
        m.on_reading(reading("10/18/2026 07:58:00", GridSignal::On, 90.0, 500.0), t0);
        let day = m.last.as_ref().unwrap().time.day();
        assert_eq!(m.chart_view(t0).unwrap().status, "loading");
        // SEMS+ ok: its points, gap 5, now = the later of the station clock and the last point
        m.on_chart(Some(ChartSeries { day, points: vec![(0, 93.0, 0.8), (478, 90.0, 0.5)] }));
        let v = m.chart_view(t0).unwrap();
        assert_eq!((v.status, v.gap, v.now, v.points.len()), ("ok", 5, 478, 2));
        // a failed refresh keeps the last series on screen
        m.on_chart(None);
        assert_eq!(m.chart_view(t0).unwrap().status, "ok");
        // no SEMS+ series for today and it failed: own readings, gap 12, since launch today
        m.on_chart(Some(ChartSeries { day: day - 1, points: vec![(5, 1.0, 1.0)] }));
        m.on_chart(None);
        m.on_reading(reading("10/18/2026 08:03:12", GridSignal::On, 89.0, 500.0), t0);
        let v = m.chart_view(t0).unwrap();
        assert_eq!((v.status, v.gap, v.since.as_deref(), v.points.len()), ("fallback", 12, Some("7:58\u{a0}AM"), 2));
        // empty day from SEMS+
        m.on_chart(Some(ChartSeries { day, points: vec![] }));
        assert_eq!(m.chart_view(t0).unwrap().status, "empty");
        // signed out or no battery: no chart
        let mut nb = Monitor::new(true);
        let mut r = reading("10/18/2026 08:00:00", GridSignal::On, 0.0, 0.0);
        r.battery = None;
        nb.on_reading(r, t0);
        assert_eq!(nb.chart_view(t0), None);
        assert_eq!(Monitor::new(false).chart_view(t0), None);
    }

    #[test]
    fn failed_chart_with_no_own_points() {
        let t0 = Instant::now();
        let mut m = Monitor::new(true);
        m.on_reading(reading("10/18/2026 08:00:00", GridSignal::On, 90.0, 0.0), t0);
        m.own.clear();
        m.on_chart(None);
        assert_eq!(m.chart_view(t0).unwrap().status, "failed");
    }

    #[test]
    fn own_points_keep_today_only() {
        let t0 = Instant::now();
        let mut m = Monitor::new(true);
        m.on_reading(reading("10/17/2026 23:58:00", GridSignal::On, 90.0, 0.0), t0);
        m.on_reading(reading("10/18/2026 00:03:00", GridSignal::On, 89.0, 0.0), t0);
        assert_eq!(m.own.len(), 1);
        m.on_chart(None);
        // started yesterday: header "Today", no since
        assert_eq!(m.chart_view(t0).unwrap().since, None);
    }

    struct Fake {
        signed_in: bool,
        readings: VecDeque<Result<Reading, SemsError>>,
        /// Today's call (the day of the last reading): chart from here, history from `history`.
        charts: VecDeque<Result<ChartSeries, SemsError>>,
        history: std::collections::HashMap<i64, GridDay>,
        /// Past days whose next call fails with this error (once per entry).
        fail: Vec<(i64, SemsError)>,
        today: Option<i64>,
        polls: Arc<Mutex<u32>>,
        /// Every day asked for, in order.
        days: Arc<Mutex<Vec<i64>>>,
        /// When set, `reading()` waits for it: a poll held open mid-flight.
        gate: Option<Arc<tokio::sync::Notify>>,
        /// How long every `day()` call takes (a hanging SEMS+).
        slow: Duration,
        /// One SEMS+ day call at a time, as sems::Client's gate.
        plus_gate: Arc<tokio::sync::Mutex<()>>,
        /// (start, end) of each walk call, test clock seconds from `t0`.
        walk_calls: Arc<Mutex<Vec<(u64, u64)>>>,
        /// When each reading was asked for, test clock seconds from `t0`.
        read_at: Arc<Mutex<Vec<u64>>>,
        t0: tokio::time::Instant,
        expired: u32,
    }

    impl Source for Fake {
        fn signed_in(&self) -> bool {
            self.signed_in
        }
        fn sign_out(&mut self) {
            self.signed_in = false;
        }
        fn day_detached(&mut self, date: LocalTs, _: Option<f64>) -> Option<impl Future<Output = Result<Day, SemsError>> + Send + 'static> {
            let d = date.day();
            self.days.lock().unwrap().push(d);
            let fail = self.fail.iter().position(|x| x.0 == d).map(|i| self.fail.remove(i).1);
            let grid = self.history.get(&d).cloned().unwrap_or(GridDay { newest: Some(1439), ..Default::default() });
            let (slow, gate, calls, t0) = (self.slow, self.plus_gate.clone(), self.walk_calls.clone(), self.t0);
            Some(async move {
                let _one = gate.lock().await;
                let start = t0.elapsed().as_secs();
                tokio::time::sleep(slow).await;
                calls.lock().unwrap().push((start, t0.elapsed().as_secs()));
                match fail {
                    Some(e) => Err(e),
                    None => Ok(Day { chart: Err(SemsError::Network("past day".into())), grid: Ok(grid) }),
                }
            })
        }
        fn plus_expired(&mut self) {
            self.expired += 1;
        }
        fn station(&self) -> Option<String> {
            Some("station-a".into())
        }
        async fn reading(&mut self) -> Result<Reading, SemsError> {
            *self.polls.lock().unwrap() += 1;
            self.read_at.lock().unwrap().push(self.t0.elapsed().as_secs());
            if let Some(g) = &self.gate {
                g.notified().await;
            }
            let r = self.readings.pop_front().unwrap_or(Err(SemsError::Network("empty".into())));
            if let Ok(r) = &r {
                self.today = Some(r.time.day());
            }
            r
        }
        async fn day(&mut self, date: LocalTs, _: Option<f64>) -> Result<Day, SemsError> {
            let d = date.day();
            self.days.lock().unwrap().push(d);
            let gate = self.plus_gate.clone();
            let _one = gate.lock().await;
            tokio::time::sleep(self.slow).await;
            let grid = Ok(self.history.get(&d).cloned().unwrap_or(GridDay { newest: Some(1439), ..Default::default() }));
            if Some(d) == self.today {
                return match self.charts.pop_front().unwrap_or(Err(SemsError::Network("empty".into()))) {
                    // One unreadable reply: chart and grid both fail on it, as in sems::Client::day.
                    Err(e @ SemsError::Parse { .. }) => Ok(Day { chart: Err(e.clone()), grid: Err(e) }),
                    c => Ok(Day { chart: Ok(c?), grid }),
                };
            }
            if let Some(i) = self.fail.iter().position(|x| x.0 == d) {
                return Err(self.fail.remove(i).1);
            }
            Ok(Day { chart: Err(SemsError::Network("past day".into())), grid })
        }
    }

    fn fake(readings: Vec<Result<Reading, SemsError>>, charts: Vec<Result<ChartSeries, SemsError>>) -> (Arc<tokio::sync::Mutex<Fake>>, Arc<Mutex<u32>>) {
        let polls = Arc::new(Mutex::new(0));
        let f = Fake {
            signed_in: true,
            readings: readings.into(),
            charts: charts.into(),
            history: Default::default(),
            fail: vec![],
            today: None,
            polls: polls.clone(),
            days: Default::default(),
            gate: None,
            slow: Duration::ZERO,
            plus_gate: Default::default(),
            walk_calls: Default::default(),
            read_at: Default::default(),
            t0: tokio::time::Instant::now(),
            expired: 0,
        };
        (Arc::new(tokio::sync::Mutex::new(f)), polls)
    }

    /// `poll_once` with its live part folded back in, for tests that read one report.
    async fn poll(src: &tokio::sync::Mutex<Fake>, mon: &Mutex<Monitor>) -> PollReport {
        let mut l = PollReport::default();
        let mut r = poll_once(src, mon, |x| l = x).await;
        (r.notice, r.disagreement, r.low_battery) = (l.notice, l.disagreement, l.low_battery);
        r
    }

    #[tokio::test(start_paused = true)]
    async fn slow_history_never_delays_the_alerts() {
        // B-O1: SEMS+ takes 60 s per day call (today + 3 past days + retries = minutes). The grid
        // notice (which also runs the Tuya switch in after_poll) and the low battery alert must
        // reach the shell as soon as the reading is in, not when the poll ends.
        let r = |hhmm: &str, grid, soc| Ok(reading(&format!("10/18/2026 {hhmm}:00"), grid, soc, 900.0));
        let (src, _) = fake(vec![r("08:00", GridSignal::On, 25.0), r("08:02", GridSignal::Off, 22.0), r("08:04", GridSignal::Off, 19.6)], vec![]);
        src.lock().await.slow = Duration::from_secs(60);
        let mon = Arc::new(Mutex::new(Monitor::new(true)));
        let (_tx, rx) = mpsc::channel(4);
        let got = Arc::new(Mutex::new(vec![]));
        let t0 = tokio::time::Instant::now();
        let task = tokio::spawn(run(src, mon, rx, {
            let got = got.clone();
            move |rep: PollReport| got.lock().unwrap().push((t0.elapsed().as_secs(), rep.notice.map(|n| n.text()), rep.low_battery))
        }));
        let alerts = |got: &[(u64, Option<String>, Option<f64>)]| got.iter().filter(|g| g.1.is_some() || g.2.is_some()).cloned().collect::<Vec<_>>();
        // poll 1: today + 3 past days, 60 s each, ends at 240 s; poll 2 at 360 s (2 min later),
        // today + 3 more past days, ends at 600 s; poll 3 at 720 s
        tokio::time::sleep(Duration::from_secs(721)).await;
        let got = got.lock().unwrap().clone();
        assert_eq!(
            alerts(&got),
            vec![(360, Some("Grid is off · battery 22%".to_owned()), None), (720, None, Some(19.6))],
            "each alert at its reading, not 3-4 min later at the poll's end"
        );
        assert!(got.iter().any(|g| g.0 == 600), "poll 2 really ran its slow day calls: {got:?}");
        task.abort();
    }

    fn day_of(s: &str) -> i64 {
        LocalTs::parse(s).unwrap().day()
    }

    #[test]
    fn past_days_fetched_once_after_they_end() {
        let mut m = Monitor::new(true);
        let t = LocalTs::parse("10/18/2026 00:05:00").unwrap().0 / 60;
        let today = t.div_euclid(DAY);
        let full = || GridDay { newest: Some(1439), ..Default::default() };
        // newest first, at most 3 per poll
        assert_eq!(m.days_to_fetch(t), vec![today - 1, today - 2, today - 3]);
        for d in (today - PAST_DAYS..today).rev() {
            m.on_day(d, full(), t);
        }
        // yesterday came before its end + 10 min: kept, refetched once from 00:10, then never
        assert_eq!(m.days_to_fetch(t), Vec::<i64>::new());
        assert_eq!(m.days_to_fetch(t + 5), vec![today - 1]);
        m.on_day(today - 1, full(), t + 5);
        assert_eq!(m.days_to_fetch(t + 600), Vec::<i64>::new());
        // the next day: today's entry (fetched as "today") becomes yesterday; day −14 moves on, and
        // the older days stay (T-015 § 4.1: Stats keeps every day)
        m.on_day(today, GridDay { newest: Some(1439), ..Default::default() }, t + 600);
        let t2 = t + DAY;
        m.after_history(t2);
        assert!(m.history_final.contains(&(today - PAST_DAYS)));
        assert_eq!(m.days_to_fetch(t2), Vec::<i64>::new(), "today's partial entry waits for 00:10");
        assert_eq!(m.days_to_fetch(t2 + 5), vec![today]);
        // sign-out forgets everything
        m.forget_history();
        assert_eq!(m.days_to_fetch(t2 + 5).len(), 3);
        assert!(!m.history_tried);
    }

    #[tokio::test]
    async fn history_budget_retry_and_skip() {
        let r = |hhmm: &str| Ok(reading(&format!("10/18/2026 {hhmm}:00"), GridSignal::On, 90.0, 0.0));
        let chart = || Ok(ChartSeries { day: day_of("10/18/2026 00:00:00"), points: vec![] });
        let polls = ["11:22", "11:24", "11:26", "11:28", "11:30", "11:32", "11:34"];
        let (src, _) = fake(polls.iter().map(|t| r(t)).collect(), polls.iter().map(|_| chart()).collect());
        let today = day_of("10/18/2026 00:00:00");
        let net = || SemsError::Network("timeout".into());
        src.lock().await.fail = vec![(today - 1, net()), (today - 1, net()), (today - 4, net())];
        let days = src.lock().await.days.clone();
        let mon = Mutex::new(Monitor::new(true));
        let take = || std::mem::take(&mut *days.lock().unwrap());
        // cold start: today (same call as the chart) + 3 past days, newest first; yesterday fails
        // twice (call + one retry): skipped, and the older days still load (T-012)
        let rep = poll(&src, &mon).await;
        assert_eq!(rep.errors, vec!["history 2026-10-17: network: timeout".to_owned()]);
        assert_eq!(take(), vec![today, today - 1, today - 1, today - 2, today - 3]);
        assert!(mon.lock().unwrap().history_tried);
        assert!(!mon.lock().unwrap().past_days_loaded(today));
        // the next poll: yesterday again, day −4 fails once and its retry gets it, then −5
        assert!(poll(&src, &mon).await.errors.is_empty());
        assert_eq!(take(), vec![today, today - 1, today - 4, today - 4, today - 5]);
        // three more polls for days −6 to −14
        for d in [today - 6, today - 9, today - 12] {
            assert!(!mon.lock().unwrap().past_days_loaded(today));
            poll(&src, &mon).await;
            assert_eq!(take(), vec![today, d, d - 1, d - 2]);
        }
        assert!(mon.lock().unwrap().past_days_loaded(today));
        // steady state: only today's call, as in T-001
        poll(&src, &mon).await;
        assert_eq!(take(), vec![today]);
    }

    #[tokio::test]
    async fn history_unreadable_day_not_retried_and_expired_stops() {
        let r = Ok(reading("10/18/2026 11:22:00", GridSignal::On, 90.0, 0.0));
        let today = day_of("10/18/2026 00:00:00");
        let (src, _) = fake(vec![r], vec![Ok(ChartSeries { day: today, points: vec![] })]);
        let bad = SemsError::Parse { reason: "code X".into(), reply: "{}".into() };
        src.lock().await.fail = vec![(today - 1, bad), (today - 2, SemsError::Expired)];
        let days = src.lock().await.days.clone();
        let mon = Mutex::new(Monitor::new(true));
        let rep = poll(&src, &mon).await;
        assert_eq!(*days.lock().unwrap(), vec![today, today - 1, today - 2], "no retry on a bad reply; Expired ends the poll's history");
        assert_eq!((rep.errors.len(), rep.bad_replies.len()), (2, 1));
    }

    #[test]
    fn history_kept_on_disk() {
        let p = std::env::temp_dir().join(format!("gw-history-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let t = LocalTs::parse("10/18/2026 12:00:00").unwrap().0 / 60;
        let today = t.div_euclid(DAY);
        let launch = || {
            let mut m = Monitor::new(true);
            m.load_history(p.clone());
            m
        };
        let mut m = launch();
        assert!(m.history.is_empty(), "no file: no history");
        let g = GridDay { off: vec![(600, true), (601, false)], newest: Some(1439), ..Default::default() };
        m.on_day(today - 1, g.clone(), t);
        m.on_day(today, GridDay { newest: Some(720), ..Default::default() }, t);
        m.after_history(t);
        // the next launch: yesterday is back and never fetched again; today isn't on disk
        let mut n = launch();
        assert_eq!(n.history.get(&(today - 1)), Some(&g));
        assert!(!n.history.contains_key(&today));
        assert_eq!(n.days_to_fetch(t), vec![today - 2, today - 3, today - 4]);
        // a day older than the 5 stays in the file (T-015 § 4.1)
        n.after_history(t + 6 * DAY);
        assert_eq!(launch().history.get(&(today - 1)), Some(&g));
        // an unreadable file = no history, no error
        std::fs::write(&p, "{not json").unwrap();
        assert!(launch().history.is_empty());
        // Sign Out (or another account): the file goes
        m.reset(false);
        assert!(!p.exists() && m.history_file.is_some());
    }

    /// Signed in to "station-a" with a reading at `at` (station set up 2026-09-18 18:28).
    fn ready(at: &str) -> Monitor {
        let mut m = Monitor::new(true);
        m.set_station(Some("station-a".into()));
        m.on_reading(reading(at, GridSignal::On, 90.0, 0.0), Instant::now());
        m
    }
    fn full() -> GridDay {
        GridDay { newest: Some(1439), ..Default::default() }
    }
    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("gw-{name}-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn history_file_names_its_station_and_reads_v010() {
        let p = tmp("history-v2");
        let t = LocalTs::parse("10/18/2026 12:00:00").unwrap().0 / 60;
        let today = t.div_euclid(DAY);
        let launch = |station: &str| {
            let mut m = Monitor::new(true);
            m.load_history(p.clone());
            m.set_station(Some(station.into()));
            m
        };
        // v0.1.0's bare map, no station: read once as the signed-in station's (D-O2)
        let old = serde_json::json!({ (today - 1).to_string(): { "off": [[600, true], [601, true], [700, false]], "newest": 1439 }, (today - 2).to_string(): { "off": [], "newest": null } });
        std::fs::write(&p, old.to_string()).unwrap();
        let mut m = launch("station-a");
        let y = GridDay { off: vec![(600, true), (601, true), (700, false)], ..full() };
        assert_eq!(m.history.get(&(today - 1)), Some(&y), "shown until refetched");
        // no gap data in v0.1.0: neither day is final, and the walk fetches both once more (B-O1)
        assert!(!m.history_final.contains(&(today - 1)) && !m.history_final.contains(&(today - 2)));
        assert_eq!(m.days_to_fetch(t)[..2], [today - 1, today - 2]);
        // the refetch replaces the old day even though it has less: a hole the old file hid
        let holed = GridDay { off: vec![(600, true), (601, true)], gaps: vec![(650, 699)], ..full() };
        m.on_day(today - 1, holed.clone(), t);
        assert_eq!(m.history.get(&(today - 1)), Some(&holed));
        assert!(m.history_final.contains(&(today - 1)) && m.days_to_fetch(t)[0] == today - 2);
        // later answers for a day once refetched keep rule 11 (never replaced with less)
        m.history_final.remove(&(today - 1));
        m.on_day(today - 1, GridDay { gaps: vec![(0, 1439)], ..full() }, t);
        assert_eq!(m.history.get(&(today - 1)), Some(&holed));
        m.after_history(t);
        // rewritten in the new shape: station, runs, gaps, newest; a day not yet refetched isn't saved
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["station"], "station-a");
        assert_eq!(v["days"][(today - 1).to_string()], serde_json::json!({ "runs": [[600, 601, true]], "gaps": [[650, 699]], "newest": 1439 }));
        assert!(v["days"].get((today - 2).to_string()).is_none());
        let y = holed;
        assert!(!p.with_extension("tmp").exists(), "written to a temporary file, then renamed");
        // the next launch, same station: the same day back, final
        let n = launch("station-a");
        assert_eq!((n.history.get(&(today - 1)), n.history_final.contains(&(today - 1))), (Some(&y), true));
        // another station's file: ignored, and replaced at the next save
        let mut o = launch("station-b");
        assert!(o.history.is_empty() && o.history_final.is_empty());
        o.after_history(t);
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!((v["station"].as_str(), v["days"].as_object().map(|d| d.len())), (Some("station-b"), Some(0)));
        // a corrupt file: read as none, reported once (B-Y1); a missing or empty one isn't
        std::fs::write(&p, "{\"days\": [oops").unwrap();
        let mut c = Monitor::new(true);
        assert!(c.load_history(p.clone()) && c.history.is_empty());
        std::fs::write(&p, "").unwrap();
        assert!(!Monitor::new(true).load_history(p.clone()));
        let _ = std::fs::remove_file(&p);
        assert!(!Monitor::new(true).load_history(p.clone()));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn three_years_of_history_stay_small() {
        let p = tmp("history-size");
        let mut m = Monitor::new(true);
        m.load_history(p.clone());
        m.set_station(Some("00000000-0000-0000-0000-000000000000".into()));
        let (d0, n) = (20_000, 3 * 365);
        let off: Vec<(u32, bool)> = (540..=720).chain(800..=860).chain(1200..=1230).map(|x| (x, true)).collect();
        for d in d0..d0 + n {
            m.on_day(d, GridDay { off: off.clone(), gaps: vec![(100, 140)], newest: Some(1439) }, (d0 + n + 1) * DAY);
        }
        m.after_history((d0 + n + 1) * DAY);
        let size = std::fs::metadata(&p).unwrap().len();
        assert!(size < 150_000, "{size} bytes");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn empty_day_is_final_only_after_three_launches() {
        // D-O11: SEMS once answered an empty list after C0602; one empty answer must not lock a real day.
        let p = tmp("history-empty");
        let t = LocalTs::parse("10/18/2026 12:00:00").unwrap().0 / 60;
        let d = t.div_euclid(DAY) - 2;
        for launch in 1..=4 {
            let mut m = ready("10/18/2026 12:00:00");
            m.load_history(p.clone());
            m.set_station(Some("station-a".into()));
            assert_eq!(m.days_to_fetch(t).contains(&d), launch < 4, "launch {launch}");
            if launch == 4 {
                assert!(m.history_final.contains(&d) && m.history.get(&d).is_some_and(|g| g.newest.is_none()), "kept as no data");
                break;
            }
            m.on_day(d, GridDay::default(), t);
            assert!(!m.days_to_fetch(t).contains(&d), "at most once per launch");
            m.on_day(d, GridDay::default(), t); // a second empty answer this launch doesn't count
            m.after_history(t);
            assert_eq!(m.history_final.contains(&d), launch == 3);
        }
        // a day before the station existed is never asked for
        let m = ready("10/18/2026 12:00:00");
        let first = m.first_day().unwrap();
        assert!(m.wants(first, t) && !m.wants(first - 1, t));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn known_data_never_replaced_with_less() {
        let t = LocalTs::parse("10/18/2026 12:00:00").unwrap().0 / 60;
        let today = t.div_euclid(DAY);
        let after = |d: i64| (d + 1) * DAY + DAY_FINAL_AFTER_MIN;
        let g = |newest, gaps| GridDay { newest: Some(newest), gaps, ..Default::default() };
        let mut m = Monitor::new(true);
        // today up to 12:00; after midnight SEMS sends less (to 10:00): the saved copy stays, final,
        // and its missing tail is a gap, not a refetch every launch (§ 4.5)
        m.on_day(today, g(720, vec![]), t);
        m.on_day(today, g(600, vec![]), after(today));
        assert_eq!((m.history[&today].newest, m.history[&today].gaps.clone()), (Some(720), vec![(721, 1439)]));
        assert!(m.history_final.contains(&today));
        // a final day is never replaced
        m.on_day(today, g(1439, vec![]), after(today) + 60);
        assert_eq!(m.history[&today].newest, Some(720));
        // more gap minutes than the saved copy: the saved one stays
        m.on_day(today - 1, g(1439, vec![]), today * DAY + 5);
        m.on_day(today - 1, g(1439, vec![(100, 200)]), t);
        assert_eq!(m.history[&(today - 1)].gaps, vec![]);
        // an empty answer never replaces data, and the day is asked again later
        m.on_day(today + 1, g(300, vec![]), (today + 1) * DAY + 400);
        m.on_day(today + 1, GridDay::default(), after(today + 1));
        assert_eq!(m.history[&(today + 1)].newest, Some(300));
        assert!(!m.history_final.contains(&(today + 1)));
        // a full day has no tail gap
        m.on_day(today - 3, full(), t);
        assert!(m.history[&(today - 3)].gaps.is_empty());
    }

    #[test]
    fn walk_order_day_on_screen_first() {
        let mut m = ready("10/18/2026 12:00:00");
        let now = m.station_now(Instant::now()).unwrap().0 / 60;
        let (today, first) = (now.div_euclid(DAY), day_of("09/18/2026 00:00:00"));
        m.on_day(today - 1, full(), now);
        m.on_day(today - 2, full(), now);
        let shown = day_of("10/10/2026 00:00:00");
        m.stats_day = Some(shown);
        let mut order = vec![];
        while let Some(d) = m.walk_next(now) {
            if order.len() == 12 {
                assert_eq!((m.walk.total, m.walk.total - m.walk.left), (28, 12), "the counter: days to fetch when it started (30 − 2 saved)");
            }
            order.push(d);
            m.on_day(d, full(), now);
        }
        // the day on screen and its neighbours, then newest first to the station's first day
        assert_eq!(order[..3], [shown, shown - 1, shown + 1]);
        let rest: Vec<i64> = (first..today - 2).rev().filter(|d| !(shown - 1..=shown + 1).contains(d)).collect();
        assert_eq!(order[3..], rest[..]);
        assert_eq!((m.walk.total, *order.iter().min().unwrap()), (0, first));
        // no battery, or no station yet: no walk (§ 4.12)
        let mut nb = Monitor::new(true);
        let mut r = reading("10/18/2026 12:00:00", GridSignal::On, 0.0, 0.0);
        r.battery = None;
        nb.set_station(Some("station-a".into()));
        nb.on_reading(r, Instant::now());
        assert_eq!(nb.walk_next(now), None);
        let mut ns = ready("10/18/2026 12:00:00");
        ns.station = None;
        assert_eq!(ns.walk_next(now), None);
    }

    #[tokio::test(start_paused = true)]
    async fn walk_never_delays_the_alerts() {
        // § 4.7: the walk is mid-call (60 s, a hanging SEMS+) when a poll starts; its reading, the
        // grid notice (and so Tuya) and the low battery alert go at once.
        let r = |hhmm: &str, grid, soc| Ok(reading(&format!("10/18/2026 {hhmm}:00"), grid, soc, 900.0));
        let (src, _) = fake(vec![r("08:00", GridSignal::On, 25.0), r("08:02", GridSignal::Off, 22.0), r("08:04", GridSignal::Off, 19.6)], vec![]);
        src.lock().await.slow = Duration::from_secs(60);
        let (read_at, calls, t0) = {
            let f = src.lock().await;
            (f.read_at.clone(), f.walk_calls.clone(), f.t0)
        };
        let mon = Arc::new(Mutex::new(Monitor::new(true)));
        let wake = Arc::new(Notify::new());
        let (_tx, rx) = mpsc::channel(4);
        let got = Arc::new(Mutex::new(vec![]));
        let w = tokio::spawn(walk(src.clone(), mon.clone(), wake.clone(), |_| {}));
        let p = tokio::spawn(run(src, mon, rx, {
            let got = got.clone();
            move |rep: PollReport| {
                got.lock().unwrap().push((t0.elapsed().as_secs(), rep.notice.map(|n| n.text()), rep.low_battery));
                wake.notify_one();
            }
        }));
        tokio::time::sleep(Duration::from_secs(900)).await;
        let (reads, calls) = (read_at.lock().unwrap().clone(), calls.lock().unwrap().clone());
        let alerts: Vec<_> = got.lock().unwrap().iter().filter(|g| g.1.is_some() || g.2.is_some()).cloned().collect();
        assert_eq!(alerts, vec![(reads[1], Some("Grid is off · battery 22%".to_owned()), None), (reads[2], None, Some(19.6))], "reads {reads:?}, walk {calls:?}");
        for &t in &reads[1..3] {
            assert!(calls.iter().any(|c| c.0 < t && t < c.1), "a walk call was running at the {t} s reading: {calls:?}");
        }
        // one SEMS+ day call at a time: the walk's calls never overlap one another
        assert!(calls.windows(2).all(|w| w[0].1 <= w[1].0));
        w.abort();
        p.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn walk_backs_off_when_sems_says_slow_down() {
        // § 4.9: GY0429 → wait 60 s, doubling up to 10 min; the footer knows when it goes on.
        let (src, _) = fake(vec![], vec![]);
        let mut m = ready("10/18/2026 12:00:00");
        let d = m.station_now(Instant::now()).unwrap().day() - 1;
        m.last.as_mut().unwrap().created = Some(LocalTs(d * 86_400)); // one day to walk
        src.lock().await.fail = vec![(d, SemsError::RateLimited); 6];
        let calls = src.lock().await.walk_calls.clone();
        let mon = Arc::new(Mutex::new(m));
        let w = tokio::spawn(walk(src, mon.clone(), Arc::new(Notify::new()), |_| {}));
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(mon.lock().unwrap().walk.paused_until.map(|t| t.hhmm()), Some("12:01".into()));
        tokio::time::sleep(Duration::from_secs(3000)).await;
        let starts: Vec<u64> = calls.lock().unwrap().iter().map(|c| c.0).collect();
        assert_eq!(starts, vec![0, 60, 180, 420, 900, 1500, 2100]);
        let m = mon.lock().unwrap();
        assert!(m.history_final.contains(&d) && m.walk.paused_until.is_none());
        w.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn walk_stops_after_three_failures_until_the_next_poll() {
        let (src, _) = fake(vec![], vec![]);
        let mut m = ready("10/18/2026 12:00:00");
        let today = m.station_now(Instant::now()).unwrap().day();
        m.last.as_mut().unwrap().created = Some(LocalTs((today - 4) * 86_400));
        let net = || SemsError::Network("timeout".into());
        src.lock().await.fail = vec![(today - 1, net()), (today - 2, net()), (today - 3, net()), (today - 4, SemsError::Expired)];
        let days = src.lock().await.days.clone();
        let take = || std::mem::take(&mut *days.lock().unwrap());
        let wake = Arc::new(Notify::new());
        let mon = Arc::new(Mutex::new(m));
        let w = tokio::spawn(walk(src.clone(), mon.clone(), wake.clone(), |_| {}));
        let step = || async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            take()
        };
        assert_eq!(step().await, vec![today - 1, today - 2, today - 3], "3 failures in a row: stop");
        wake.notify_one(); // the next poll
        assert_eq!(step().await, vec![today - 4], "the failed days wait for a later pass; Expired waits for the poll's sign-in");
        assert_eq!(src.lock().await.expired, 1);
        wake.notify_one();
        assert_eq!(step().await, vec![today - 4]);
        wake.notify_one();
        assert_eq!(step().await, vec![today - 1, today - 2, today - 3]);
        assert_eq!(mon.lock().unwrap().walk.total, 0);
        w.abort();
    }

    #[test]
    fn temperature_trend_needs_thirty_minutes() {
        let t0 = Instant::now();
        let mut m = Monitor::new(true);
        let at = |m: &mut Monitor, hhmm: &str, c: f64| {
            let mut r = reading(&format!("10/18/2026 {hhmm}:00"), GridSignal::On, 90.0, 0.0);
            r.inverter_c = Some(c);
            m.on_reading(r, t0);
            m.temp_base().map(|b| (b.0.hhmm(), b.1))
        };
        assert_eq!(at(&mut m, "08:00", 36.0), None);
        assert_eq!(at(&mut m, "08:25", 37.0), None, "not 30 min of readings yet");
        assert_eq!(at(&mut m, "08:30", 38.0), Some(("08:00".into(), Some(36.0))));
        assert_eq!(at(&mut m, "08:33", 38.0), Some(("08:00".into(), Some(36.0))));
        assert_eq!(at(&mut m, "08:56", 38.0), Some(("08:25".into(), Some(37.0))));
        assert_eq!(m.temps.len(), 4, "older readings are let go");
        assert_eq!(at(&mut m, "11:00", 30.0), None, "the Mac slept: a 2 h old reading is no trend");
    }

    #[test]
    fn low_battery_alert_during_a_cut() {
        let t0 = Instant::now();
        let mut m = Monitor::new(true);
        let mut at = |hhmm: &str, grid, soc| {
            m.on_reading(reading(&format!("10/18/2026 {hhmm}:00"), grid, soc, 900.0), t0);
            m.low_alert.take()
        };
        assert_eq!(at("08:00", GridSignal::Off, 18.0), None, "never on the first reading");
        assert_eq!(at("08:05", GridSignal::Off, 15.0), None, "20 was passed before launch");
        assert_eq!(at("08:10", GridSignal::Off, 10.0), Some(10.0));
        assert_eq!(at("08:15", GridSignal::On, 10.0), None);
        assert_eq!(at("08:20", GridSignal::Off, 20.0), Some(20.0), "a new cut: armed again");
        m.low_pct = 30;
        m.reset(false);
        assert_eq!(m.low_pct, 30, "the setting survives Sign Out");
    }

    #[tokio::test]
    async fn poll_reports_low_battery() {
        let r = |hhmm: &str, soc| Ok(reading(&format!("10/18/2026 {hhmm}:00"), GridSignal::Off, soc, 900.0));
        let (src, _) = fake(vec![r("08:00", 25.0), r("08:02", 19.6)], vec![]);
        let mon = Mutex::new(Monitor::new(true));
        assert_eq!(poll(&src, &mon).await.low_battery, None);
        assert_eq!(poll(&src, &mon).await.low_battery, Some(19.6));
    }

    #[tokio::test]
    async fn poll_once_paths() {
        let bad = || SemsError::Parse { reason: "field `invert_full.vac1` missing".into(), reply: "{}".into() };
        let (src, polls) = fake(
            vec![
                Ok(reading("10/18/2026 08:20:00", GridSignal::On, 90.0, 0.0)),
                Ok(reading("10/18/2026 08:25:00", GridSignal::Disagree, 90.0, 0.0)),
                Ok(reading("10/18/2026 08:30:00", GridSignal::Off, 86.0, 998.0)),
                Err(bad()),
                Err(SemsError::Network("timeout".into())),
                Err(SemsError::WrongLogin("Email or password error".into())),
            ],
            vec![Ok(ChartSeries { day: 0, points: vec![] }), Err(bad()), Err(SemsError::Network("x".into()))],
        );
        let mon = Mutex::new(Monitor::new(true));
        assert_eq!(poll(&src, &mon).await, PollReport::default());
        let r = poll(&src, &mon).await;
        assert!(r.disagreement.unwrap().contains("vac1 230"));
        // T-004: today's one reply fails both chart and grid: dumped once, logged twice.
        assert_eq!(r.bad_replies.len(), 1, "the chart's bad reply is reported once");
        assert_eq!(r.errors.len(), 2);
        let r = poll(&src, &mon).await;
        assert_eq!(r.notice.unwrap().text(), "Grid is off · battery 86%");
        let r = poll(&src, &mon).await;
        assert_eq!(r.bad_replies[0].0, "field `invert_full.vac1` missing");
        poll(&src, &mon).await;
        assert_eq!(mon.lock().unwrap().failures, 2);
        poll(&src, &mon).await;
        assert!(!mon.lock().unwrap().signed_in);
        assert!(!src.lock().await.signed_in);
        // signed out: no more calls to SEMS
        poll(&src, &mon).await;
        assert_eq!(*polls.lock().unwrap(), 6);
    }

    #[tokio::test(start_paused = true)]
    async fn loop_polls_every_two_minutes_and_on_refresh() {
        let (src, polls) = fake(vec![], vec![]);
        let mon = Arc::new(Mutex::new(Monitor::new(true)));
        let (tx, rx) = mpsc::channel(4);
        let task = tokio::spawn(run(src, mon.clone(), rx, |_| {}));
        let at = |n: u32| {
            let polls = polls.clone();
            async move {
                tokio::task::yield_now().await;
                assert_eq!(*polls.lock().unwrap(), n);
            }
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        at(1).await; // polls at start
        tokio::time::sleep(Duration::from_secs(60)).await;
        at(1).await;
        // Refresh now (shell: ask_refresh, then send only when no poll runs)
        assert_eq!(mon.lock().unwrap().ask_refresh(), Some(true));
        assert_eq!(mon.lock().unwrap().ask_refresh(), None, "a second click while it runs does nothing");
        tx.send(()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        at(2).await;
        assert!(!mon.lock().unwrap().user_poll, "the spin stops after the poll, failed or not");
        tokio::time::sleep(rules::POLL_EVERY).await;
        at(3).await;
        assert!(!mon.lock().unwrap().user_poll && !mon.lock().unwrap().polling, "timer polls never spin");
        assert!(mon.lock().unwrap().stale(Instant::now()), "3 failed polls = stale");
        drop(tx);
        tokio::time::sleep(rules::POLL_EVERY).await;
        task.await.unwrap();
    }

    #[test]
    fn refresh_during_timer_poll_joins_it() {
        let mut m = Monitor::new(true);
        m.polling = true;
        assert_eq!(m.ask_refresh(), Some(false), "a timer poll runs: it becomes the user's, no second poll");
        assert!(m.user_poll);
        assert_eq!(Monitor::new(false).ask_refresh(), None, "signed out: Refresh is disabled");
    }

    #[tokio::test]
    async fn sign_out_clears_state() {
        let r = Ok(reading("10/18/2026 11:22:00", GridSignal::Off, 80.0, 900.0));
        let (src, _) = fake(vec![r], vec![Ok(ChartSeries { day: day_of("10/18/2026 00:00:00"), points: vec![(600, 90.0, 0.9)] })]);
        let mon = Mutex::new(Monitor::new(true));
        mon.lock().unwrap().account = Some("name@example.com".into());
        poll(&src, &mon).await;
        {
            let m = mon.lock().unwrap();
            assert!(m.last.is_some() && !m.history.is_empty() && m.off_since.is_some() && m.chart_view(Instant::now()).is_some());
        }
        // shell::sign_out after the Keychain delete: the client, then the monitor
        src.lock().await.sign_out();
        mon.lock().unwrap().reset(false);
        let m = mon.lock().unwrap();
        assert!(!m.signed_in && m.account.is_none() && m.last.is_none() && m.off_since.is_none() && m.since_start.is_none());
        assert!(m.history.is_empty() && !m.history_tried && m.chart_view(Instant::now()).is_none());
        let p = crate::view::panel(&m, Instant::now(), false);
        assert!(!p.signed_in && p.battery.is_none() && p.chart.is_none());
    }

    #[tokio::test]
    async fn poll_after_sign_out_is_dropped() {
        let r = Ok(reading("10/18/2026 11:22:00", GridSignal::Off, 80.0, 900.0));
        let (src, _) = fake(vec![r], vec![]);
        let gate = Arc::new(tokio::sync::Notify::new());
        src.lock().await.gate = Some(gate.clone());
        let mon = Arc::new(Mutex::new(Monitor::new(true)));
        let poll = tokio::spawn({
            let (src, mon) = (src.clone(), mon.clone());
            async move { poll(&src, &mon).await }
        });
        tokio::task::yield_now().await;
        assert!(src.try_lock().is_err(), "the poll holds the client");
        mon.lock().unwrap().reset(false); // Sign Out lands mid-poll
        gate.notify_one();
        poll.await.unwrap();
        let m = mon.lock().unwrap();
        assert!(!m.signed_in && m.last.is_none() && m.history.is_empty(), "its result must not sign the panel back in");
    }

    #[tokio::test]
    async fn wrong_login_shows_sign_in() {
        let (src, _) = fake(vec![Err(SemsError::WrongLogin("Email or password error".into()))], vec![]);
        let mon = Mutex::new(Monitor::new(true));
        mon.lock().unwrap().account = Some("name@example.com".into());
        poll(&src, &mon).await;
        let p = crate::view::panel(&mon.lock().unwrap(), Instant::now(), false);
        // signedIn false → panel.js shows Sign In… (panel_footer_signed_out_has_sign_in); Settings shows no email
        assert!(!p.signed_in && crate::settings::account(&mon.lock().unwrap()).email.is_none());
    }
}
