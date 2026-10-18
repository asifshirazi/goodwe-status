//! The app's live state and the poll loop. Plain Rust: the tray, panel and notifications
//! read a `Monitor` and react to `PollReport`s; tests drive it with a fake `Source`.

use crate::clock::LocalTs;
use crate::outages::{self, GridDay, HistStart, Outage, DAY, PAST_DAYS};
use crate::rules::{self, ChartSource, GridState};
use crate::sems::{self, ChartSeries, Day, GridSignal, Reading, SemsError};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::mpsc;

/// Where the readings come from. `sems::Client` in the app, a fake in tests.
pub trait Source: Send {
    fn signed_in(&self) -> bool;
    fn sign_out(&mut self);
    fn reading(&mut self) -> impl Future<Output = Result<Reading, SemsError>> + Send;
    /// One station-local day: today's chart and grid history, or a past day's history.
    fn day(&mut self, date: LocalTs, utc_offset_h: Option<f64>) -> impl Future<Output = Result<Day, SemsError>> + Send;
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
}

/// A past day is fetched for good once the station clock is this far past its end (the reply
/// runs about 2 minutes behind, so a fetch at 00:01 would miss 23:58 and 23:59).
const DAY_FINAL_AFTER_MIN: i64 = 10;
/// Past days fetched per poll at most (cold start: 5 days over two polls).
const DAYS_PER_POLL: usize = 3;
/// S7a: the On reading before the first Off one must be this close for an exact "Since 7:42 AM"
/// (an On reading hours older, e.g. the Mac slept, doesn't show when the cut began).
const SEEN_LIVE_MAX_GAP_S: i64 = 600;

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
    /// SEMS+ grid history per station-local day (`LocalTs::day`): today and the 5 days before.
    history: BTreeMap<i64, GridDay>,
    /// Past days fetched after they ended: never fetched again.
    history_final: BTreeSet<i64>,
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
            history_tried: false,
            hist_start: HistStart::Stale,
            since_start: None,
            account: None,
            polling: false,
            user_poll: false,
            gen: 0,
        }
    }

    /// Sign Out (or another account signs in): a fresh monitor. Keeps the poll flag, and bumps
    /// `gen` so a poll already running drops its result instead of signing the panel back in.
    pub fn reset(&mut self, signed_in: bool) {
        *self = Monitor { polling: self.polling, gen: self.gen + 1, ..Monitor::new(signed_in) };
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

    /// Stores one day's grid history. `now` = station now, absolute minutes.
    pub fn on_day(&mut self, day: i64, grid: GridDay, now: i64) {
        if now >= (day + 1) * DAY + DAY_FINAL_AFTER_MIN {
            self.history_final.insert(day);
        }
        self.history.insert(day, grid);
    }

    /// Past days still to fetch, newest first (yesterday feeds the Since line): missing ones, and
    /// ones fetched before they had ended once they have. At most `DAYS_PER_POLL`.
    pub fn days_to_fetch(&self, now: i64) -> Vec<i64> {
        let today = now.div_euclid(DAY);
        (today - PAST_DAYS..today)
            .rev()
            .filter(|d| !self.history_final.contains(d))
            .filter(|d| !self.history.contains_key(d) || now >= (d + 1) * DAY + DAY_FINAL_AFTER_MIN)
            .take(DAYS_PER_POLL)
            .collect()
    }

    /// After a poll's history calls: drop old days, then read the current outage's start.
    pub fn after_history(&mut self, now: i64) {
        let oldest = now.div_euclid(DAY) - PAST_DAYS;
        self.history.retain(|d, _| *d >= oldest);
        self.history_final.retain(|d| *d >= oldest);
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
        (self.history_tried, self.hist_start, self.since_start) = (false, HistStart::Stale, None);
    }

    /// History is read only for stations with a battery (the off rule needs pBat).
    pub fn has_history(&self) -> bool {
        self.last.as_ref().is_some_and(|r| r.battery.is_some())
    }

    /// The 5 days before `today` all loaded with data: only then may a line say "(last 5 days)".
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
}

pub async fn poll_once<S: Source>(src: &tokio::sync::Mutex<S>, mon: &Mutex<Monitor>) -> PollReport {
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
                SemsError::Network(_) | SemsError::Expired => m.on_failure(),
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
        rep.notice = m.on_reading(r, Instant::now());
        m.station_now(Instant::now())
    };
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
        // Past days, newest first; the first error stops this poll's history (the next one retries).
        for d in todo {
            let date = LocalTs(d * 86_400);
            match src.day(date, offset).await.and_then(|x| x.grid) {
                Ok(g) => match current() {
                    Some(mut m) => m.on_day(d, g, now),
                    None => return rep,
                },
                Err(e) => {
                    failed(&mut rep, &format!("history {}", date.date()), e);
                    break;
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
    if let SemsError::Parse { reason, reply } = e {
        rep.bad_replies.push((format!("{what}: {reason}"), reply));
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
        let rep = poll_once(&src, &mon).await;
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
        /// Past days whose next call fails once.
        fail_once: Vec<i64>,
        today: Option<i64>,
        polls: Arc<Mutex<u32>>,
        /// Every day asked for, in order.
        days: Arc<Mutex<Vec<i64>>>,
        /// When set, `reading()` waits for it: a poll held open mid-flight.
        gate: Option<Arc<tokio::sync::Notify>>,
    }

    impl Source for Fake {
        fn signed_in(&self) -> bool {
            self.signed_in
        }
        fn sign_out(&mut self) {
            self.signed_in = false;
        }
        async fn reading(&mut self) -> Result<Reading, SemsError> {
            *self.polls.lock().unwrap() += 1;
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
            let grid = Ok(self.history.get(&d).cloned().unwrap_or(GridDay { off: vec![], newest: Some(1439) }));
            if Some(d) == self.today {
                let chart = self.charts.pop_front().unwrap_or(Err(SemsError::Network("empty".into())))?;
                return Ok(Day { chart: Ok(chart), grid });
            }
            if let Some(i) = self.fail_once.iter().position(|x| *x == d) {
                self.fail_once.remove(i);
                return Err(SemsError::Network("timeout".into()));
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
            fail_once: vec![],
            today: None,
            polls: polls.clone(),
            days: Default::default(),
            gate: None,
        };
        (Arc::new(tokio::sync::Mutex::new(f)), polls)
    }

    fn day_of(s: &str) -> i64 {
        LocalTs::parse(s).unwrap().day()
    }

    #[test]
    fn past_days_fetched_once_after_they_end() {
        let mut m = Monitor::new(true);
        let t = LocalTs::parse("10/18/2026 00:05:00").unwrap().0 / 60;
        let today = t.div_euclid(DAY);
        // newest first, at most 3 per poll
        assert_eq!(m.days_to_fetch(t), vec![today - 1, today - 2, today - 3]);
        for d in (today - 5..today).rev() {
            m.on_day(d, GridDay::default(), t);
        }
        // yesterday came before its end + 10 min: kept, refetched once from 00:10, then never
        assert_eq!(m.days_to_fetch(t), Vec::<i64>::new());
        assert_eq!(m.days_to_fetch(t + 5), vec![today - 1]);
        m.on_day(today - 1, GridDay::default(), t + 5);
        assert_eq!(m.days_to_fetch(t + 600), Vec::<i64>::new());
        // the next day: today's entry (fetched as "today") becomes yesterday; day −5 moves on, old days go
        m.on_day(today, GridDay::default(), t + 600);
        let t2 = t + DAY;
        m.after_history(t2);
        assert!(!m.history.contains_key(&(today - 5)));
        assert_eq!(m.days_to_fetch(t2), Vec::<i64>::new(), "today's partial entry waits for 00:10");
        assert_eq!(m.days_to_fetch(t2 + 5), vec![today]);
        // sign-out forgets everything
        m.forget_history();
        assert_eq!(m.days_to_fetch(t2 + 5).len(), 3);
        assert!(!m.history_tried);
    }

    #[tokio::test]
    async fn history_budget_and_retry() {
        let r = |hhmm: &str| Ok(reading(&format!("10/18/2026 {hhmm}:00"), GridSignal::On, 90.0, 0.0));
        let chart = || Ok(ChartSeries { day: day_of("10/18/2026 00:00:00"), points: vec![] });
        let (src, _) = fake(vec![r("11:22"), r("11:24"), r("11:26"), r("11:28")], vec![chart(), chart(), chart(), chart()]);
        let today = day_of("10/18/2026 00:00:00");
        src.lock().await.fail_once = vec![today - 4];
        let days = src.lock().await.days.clone();
        let mon = Mutex::new(Monitor::new(true));
        let take = || std::mem::take(&mut *days.lock().unwrap());
        // cold start: today (same call as the chart) + 3 past days, newest first
        assert!(poll_once(&src, &mon).await.errors.is_empty());
        assert_eq!(take(), vec![today, today - 1, today - 2, today - 3]);
        assert!(mon.lock().unwrap().history_tried);
        assert!(!mon.lock().unwrap().past_days_loaded(today));
        // day −4 fails: the poll stops there, no inner retry
        let rep = poll_once(&src, &mon).await;
        assert_eq!(rep.errors, vec!["history 2026-10-14: network: timeout".to_owned()]);
        assert_eq!(take(), vec![today, today - 4]);
        // the next poll retries it and goes on
        poll_once(&src, &mon).await;
        assert_eq!(take(), vec![today, today - 4, today - 5]);
        assert!(mon.lock().unwrap().past_days_loaded(today));
        // steady state: only today's call, as in T-001
        poll_once(&src, &mon).await;
        assert_eq!(take(), vec![today]);
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
        assert_eq!(poll_once(&src, &mon).await, PollReport::default());
        let r = poll_once(&src, &mon).await;
        assert!(r.disagreement.unwrap().contains("vac1 230"));
        assert_eq!(r.bad_replies.len(), 1, "the chart's bad reply is reported too");
        let r = poll_once(&src, &mon).await;
        assert_eq!(r.notice.unwrap().text(), "Grid is off · battery 86%");
        let r = poll_once(&src, &mon).await;
        assert_eq!(r.bad_replies[0].0, "field `invert_full.vac1` missing");
        poll_once(&src, &mon).await;
        assert_eq!(mon.lock().unwrap().failures, 2);
        poll_once(&src, &mon).await;
        assert!(!mon.lock().unwrap().signed_in);
        assert!(!src.lock().await.signed_in);
        // signed out: no more calls to SEMS
        poll_once(&src, &mon).await;
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
        poll_once(&src, &mon).await;
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
            async move { poll_once(&src, &mon).await }
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
        poll_once(&src, &mon).await;
        let p = crate::view::panel(&mon.lock().unwrap(), Instant::now(), false);
        // signedIn false → panel.js shows Sign In… (panel_footer_signed_out_has_sign_in); Settings shows no email
        assert!(!p.signed_in && crate::settings::account(&mon.lock().unwrap()).email.is_none());
    }
}
