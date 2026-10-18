//! The app's live state and the poll loop. Plain Rust: the tray, panel and notifications
//! read a `Monitor` and react to `PollReport`s; tests drive it with a fake `Source`.

use crate::clock::LocalTs;
use crate::rules::{self, ChartSource, GridState};
use crate::sems::{self, ChartSeries, GridSignal, Reading, SemsError};
use serde::Serialize;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::mpsc;

/// Where the readings come from. `sems::Client` in the app, a fake in tests.
pub trait Source: Send {
    fn signed_in(&self) -> bool;
    fn sign_out(&mut self);
    fn reading(&mut self) -> impl Future<Output = Result<Reading, SemsError>> + Send;
    fn chart(&mut self, today: LocalTs, utc_offset_h: Option<f64>) -> impl Future<Output = Result<ChartSeries, SemsError>> + Send;
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
    async fn chart(&mut self, today: LocalTs, utc_offset_h: Option<f64>) -> Result<ChartSeries, SemsError> {
        sems::Client::chart(self, today, utc_offset_h).await
    }
}

/// When the app first saw the grid off. `exact` is false when it was already off at the
/// first reading after launch ("Since at least 08:20").
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
        }
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
        match next {
            GridState::Off if prev != GridState::Off => self.off_since = Some(OffSince { at: r.time, exact: prev == GridState::On }),
            GridState::On => self.off_since = None,
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
        let view = |status, now, gap, points, since| Some(ChartView { status, now, since, gap, points });
        let Some(last) = &self.last else { return view("loading", 0, ChartSource::Plus.gap(), vec![], None) };
        last.battery.as_ref()?;
        let today = self.station_now(now)?;
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
                // "Since 07:58" only when the app started today; otherwise the day from 00:00.
                let since = self.first_reading.filter(|f| f.day() == today.day()).map(|f| f.hhmm());
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
    if !src.signed_in() {
        mon.lock().unwrap().signed_in = false;
        return rep;
    }
    let r = match src.reading().await {
        Ok(r) => r,
        Err(e) => {
            rep.errors.push(e.to_string());
            let mut m = mon.lock().unwrap();
            match e {
                SemsError::WrongLogin(_) | SemsError::SignedOut => {
                    src.sign_out();
                    m.signed_in = false;
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
        let mut m = mon.lock().unwrap();
        rep.notice = m.on_reading(r, Instant::now());
        m.station_now(Instant::now())
    };
    if let (true, Some(today)) = (has_battery, today) {
        let c = match src.chart(today, offset).await {
            Ok(c) => Some(c),
            Err(e) => {
                rep.errors.push(format!("chart: {e}"));
                if let SemsError::Parse { reason, reply } = e {
                    rep.bad_replies.push((format!("chart: {reason}"), reply));
                }
                None
            }
        };
        mon.lock().unwrap().on_chart(c);
    }
    rep
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
        on_report(poll_once(&src, &mon).await);
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
        assert_eq!((v.status, v.gap, v.since.as_deref(), v.points.len()), ("fallback", 12, Some("07:58"), 2));
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
        charts: VecDeque<Result<ChartSeries, SemsError>>,
        polls: Arc<Mutex<u32>>,
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
            self.readings.pop_front().unwrap_or(Err(SemsError::Network("empty".into())))
        }
        async fn chart(&mut self, _: LocalTs, _: Option<f64>) -> Result<ChartSeries, SemsError> {
            self.charts.pop_front().unwrap_or(Err(SemsError::Network("empty".into())))
        }
    }

    fn fake(readings: Vec<Result<Reading, SemsError>>, charts: Vec<Result<ChartSeries, SemsError>>) -> (Arc<tokio::sync::Mutex<Fake>>, Arc<Mutex<u32>>) {
        let polls = Arc::new(Mutex::new(0));
        let f = Fake { signed_in: true, readings: readings.into(), charts: charts.into(), polls: polls.clone() };
        (Arc::new(tokio::sync::Mutex::new(f)), polls)
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
        tx.send(()).await.unwrap(); // Refresh now
        tokio::time::sleep(Duration::from_millis(10)).await;
        at(2).await;
        tokio::time::sleep(rules::POLL_EVERY).await;
        at(3).await;
        assert!(mon.lock().unwrap().stale(Instant::now()), "3 failed polls = stale");
        drop(tx);
        tokio::time::sleep(rules::POLL_EVERY).await;
        task.await.unwrap();
    }
}
