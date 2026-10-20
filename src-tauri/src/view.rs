//! What the tray item and the panel show, built from the `Monitor`. Pure: the shell only
//! applies it. Every string is formatted here; panel.js only lays them out (hand-off § 1, § 2).

use crate::monitor::{ChartView, Monitor};
use crate::outages::{self, HistStart, DAY};
use crate::rules::{self, GridState};
use serde::Serialize;
use std::time::Instant;

/// `state` for `GW.renderPanel` (shape documented at the top of ui/panel.js).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelState {
    pub status: Status,
    pub battery: Option<BatteryView>,
    pub updated: Option<Updated>,
    pub notifications_off: bool,
    pub chart: Option<ChartView>,
    pub signed_in: bool,
    /// A poll the user started runs (Refresh, ⌘R, sign-in): the Refresh icon spins.
    pub refreshing: bool,
    /// T-015 § 1: `null` when the switch is off or neither reading is real.
    pub temps: Option<TempsView>,
    /// T-020 § 3: today's cut strip; `null` when the switch is off, signed out or no battery.
    pub cuts: Option<crate::stats::Cuts>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TempsView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inverter: Option<Temp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battery: Option<Temp>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Temp {
    pub text: String,
    pub level: f64,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trend: Option<&'static str>,
}

/// Both temperatures of the reading shown; level, state and trend on the unrounded values. No
/// trend while stale (the values, words and bars stay, like the battery %).
fn temps(m: &Monitor, r: &crate::sems::Reading, stale: bool) -> Option<TempsView> {
    let base = m.temp_base().filter(|_| !stale);
    let one = |c: Option<f64>, then: Option<f64>, lim| {
        c.map(|c| Temp { text: rules::fmt_temp(c), level: rules::temp_level(c, lim), state: rules::temp_state(c, lim), trend: then.and_then(|t| rules::temp_trend(t, c)) })
    };
    let v = TempsView {
        inverter: one(r.inverter_c, base.and_then(|b| b.1), rules::INVERTER_TEMP),
        battery: one(r.battery_c, base.and_then(|b| b.2), rules::BATTERY_TEMP),
    };
    (v.inverter.is_some() || v.battery.is_some()).then_some(v)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Status {
    pub kind: &'static str,
    pub title: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Grid off: "Usually back around 12:05 PM (last 14 days)" when the past days agree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub back: Option<String>,
    /// Grid on: the next usual cut, two lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<[String; 2]>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BatteryView {
    pub soc: String,
    pub power: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left: Option<Left>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Left {
    pub time: String,
    pub rest: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Updated {
    pub stale: bool,
    pub text: String,
}

/// The four tray images in `icons/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayIcon {
    Plug,
    OffRed,
    Dim,
    OffDim,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrayLook {
    pub icon: TrayIcon,
    pub title: Option<String>,
    pub tooltip: String,
}

const SIGNED_OUT_DETAIL: &str = "Sign in with your SEMS Portal account to see your battery and grid.";

/// Stale only means something once there is a reading to call old.
fn stale(m: &Monitor, now: Instant) -> bool {
    m.last.is_some() && m.stale(now)
}

/// Grid off: the Since line (spec › Since rule S1–S7a) and the back-on line (› Back-on rule).
fn off_lines(m: &Monitor, now: Instant) -> (Option<String>, Option<String>) {
    let (Some(o), Some(t)) = (m.off_since, m.station_now(now)) else { return (None, None) };
    let now_min = t.0.div_euclid(60);
    // S7a / S7: the app's own reading. Exact when it saw the grid go off, else "at least".
    let seen = || match o.exact {
        true => (Some(format!("Since {} {}", o.at.hhmm12(), rules::fmt_since_duration(now_min - o.at.0 / 60))), None),
        false => (Some(format!("Since at least {}", o.at.hhmm12())), None),
    };
    if !m.has_history() {
        return seen(); // no battery, so no history (the off rule needs pBat)
    }
    if !m.history_tried {
        return (None, None); // S1: title only while the first history call runs
    }
    match m.hist_start {
        HistStart::AtLeastYesterday => return (Some("Since at least yesterday 12:00\u{a0}AM".into()), None),
        HistStart::AtLeastToday => return (Some("Since at least 12:00\u{a0}AM".into()), None),
        HistStart::Start(_) | HistStart::Stale | HistStart::NotOff => {}
    }
    // S2/S3, S6 keeps it, or (T-025) the start the live reading saw before history has the cut, or
    // after a launch mid-cut the first off reading: "Since at least", but the back-on line still shows.
    let Some((s, known)) = m.cut_start_known() else { return seen() };
    let today = now_min.div_euclid(DAY);
    let back = m
        .past_days_loaded(today)
        .then(|| outages::back_on(&m.outages(today), s, t.0 as f64 / 60.0))
        .flatten();
    (if known { Some(outages::since_text(s, now_min)) } else { seen().0 }, back)
}

/// Grid on: the next usual cut (spec › Next-cut rule), only with today and the 14 past days loaded.
fn next_lines(m: &Monitor, now: Instant) -> Option<[String; 2]> {
    let t = m.station_now(now)?;
    let today = t.0.div_euclid(60).div_euclid(DAY);
    if !(m.has_history() && m.has_today(today) && m.past_days_loaded(today)) {
        return None;
    }
    outages::next_cut(&m.outages(today), t.0 as f64 / 60.0, stale(m, now))
}

fn status(m: &Monitor, now: Instant) -> Status {
    let s = |kind, title| Status { kind, title, detail: None, back: None, next: None };
    if !m.signed_in {
        return Status { detail: Some(SIGNED_OUT_DETAIL.into()), ..s("signedout", "Not signed in") };
    }
    if m.last.is_none() {
        return s("first", "Getting the first reading…");
    }
    match m.grid {
        GridState::Unknown => s("unknown", "Grid status not clear yet"),
        GridState::On => Status { next: next_lines(m, now), ..s("on", "Grid on") },
        GridState::Off => {
            // While stale, "Since …" keeps counting on the station clock and the back line stays.
            let (detail, back) = off_lines(m, now);
            Status { detail, back, ..s("off", "Grid off") }
        }
    }
}

fn soc_text(soc: f64) -> String {
    format!("{}%", soc.round())
}

pub fn panel(m: &Monitor, now: Instant, notifications_off: bool) -> PanelState {
    let stale = stale(m, now);
    let shown = m.last.as_ref().filter(|_| m.signed_in);
    let battery = shown.and_then(|r| r.battery.as_ref()).map(|b| BatteryView {
        soc: soc_text(b.soc),
        power: rules::fmt_power(b.watts),
        // T-003: no "· as of" while stale: the warning row under it carries the time, and 12 h text wrapped.
        left: rules::time_left_min(b.capacity_kwh, b.soc, b.watts)
            .map(|min| Left { time: rules::fmt_left(min), rest: format!("left at {} kW", rules::fmt_kw(b.watts)) }),
    });
    let refreshing = m.user_poll && m.signed_in;
    let updated = shown.map(|r| Updated {
        stale,
        // T-024: no " · refreshing…" suffix; the footer shows Refreshing… from `refreshing` instead.
        text: format!("{} {}", if stale { "No new data since" } else { "Updated" }, r.time.hhmm12()),
    });
    PanelState {
        status: status(m, now),
        battery,
        updated,
        notifications_off: notifications_off && m.signed_in,
        chart: m.chart_view(now),
        signed_in: m.signed_in,
        refreshing,
        temps: shown.filter(|_| m.show_temps).and_then(|r| temps(m, r, stale)),
        cuts: if m.show_cuts { crate::stats::today(m, now) } else { None },
    }
}

pub fn tray(m: &Monitor, now: Instant) -> TrayLook {
    let look = |icon, title: Option<String>, tooltip: String| TrayLook { icon, title, tooltip };
    if !m.signed_in {
        return look(TrayIcon::Dim, None, "Not signed in".into());
    }
    let Some(r) = &m.last else { return look(TrayIcon::Plug, None, "Getting the first reading…".into()) };
    let soc = r.battery.as_ref().map(|b| b.soc);
    let title = soc.map(soc_text);
    if stale(m, now) {
        let icon = if m.grid == GridState::Off { TrayIcon::OffDim } else { TrayIcon::Dim };
        return look(icon, title, format!("No new data since {}", r.time.hhmm12()));
    }
    let icon = if m.grid == GridState::Off { TrayIcon::OffRed } else { TrayIcon::Plug };
    let grid = status(m, now).title;
    let tooltip = match &title {
        Some(t) => format!("{grid} · battery {t}"),
        None => grid.into(),
    };
    look(icon, title, tooltip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::LocalTs;
    use crate::sems::{Battery, GridSignal, Reading};
    use std::time::Duration;

    fn reading(time: &str, grid: GridSignal, battery: Option<(f64, f64)>) -> Reading {
        Reading {
            time: LocalTs::parse(time).unwrap(),
            grid,
            vac1: 0.0,
            grid_status: None,
            battery: battery.map(|(soc, watts)| Battery { soc, watts, capacity_kwh: Some(5.1) }),
            utc_offset_h: Some(5.0),
            inverter_c: None,
            battery_c: None,
            created: None,
        }
    }
    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn signed_out_and_first_reading() {
        let t0 = Instant::now();
        let m = Monitor::new(false);
        let p = panel(&m, t0, true);
        assert_eq!((p.status.kind, p.status.title, p.status.detail.as_deref()), ("signedout", "Not signed in", Some(SIGNED_OUT_DETAIL)));
        assert!(p.battery.is_none() && p.updated.is_none() && p.chart.is_none() && !p.signed_in);
        assert!(!p.notifications_off, "the notifications section waits for sign-in");
        assert_eq!(tray(&m, t0), TrayLook { icon: TrayIcon::Dim, title: None, tooltip: "Not signed in".into() });

        let m = Monitor::new(true);
        let p = panel(&m, t0, true);
        assert_eq!((p.status.kind, p.updated, p.notifications_off), ("first", None, true));
        assert_eq!(p.chart.unwrap().status, "loading");
        assert_eq!(tray(&m, t0), TrayLook { icon: TrayIcon::Plug, title: None, tooltip: "Getting the first reading…".into() });
    }

    #[test]
    fn panel_temperatures() {
        let t0 = Instant::now();
        let mut m = Monitor::new(true);
        let at = |m: &mut Monitor, hhmm: &str, inv: Option<f64>, bat: Option<f64>| {
            let mut r = reading(&format!("10/18/2026 {hhmm}:00"), GridSignal::On, Some((90.0, 0.0)));
            (r.inverter_c, r.battery_c) = (inv, bat);
            m.on_reading(r, t0);
        };
        let temps = |m: &Monitor, t| serde_json::to_value(panel(m, t, false).temps).unwrap();
        assert_eq!(panel(&m, t0, false).temps, None, "no section before the first reading");
        at(&mut m, "08:00", Some(40.0), Some(42.0));
        assert_eq!(
            temps(&m, t0),
            serde_json::json!({ "inverter": { "text": "40°C", "level": 0.5, "state": "ok" }, "battery": { "text": "42°C", "level": 0.7, "state": "warm" } })
        );
        // 30 min later, moved 2 °C or more: arrows; the unrounded value decides
        at(&mut m, "08:30", Some(42.5), Some(43.9));
        let v = temps(&m, t0);
        assert_eq!((v["inverter"]["text"].as_str(), v["inverter"]["trend"].as_str(), v["battery"].get("trend")), (Some("43°C"), Some("up"), None));
        // stale: values and words stay, arrows go
        let v = temps(&m, t0 + 16 * MIN);
        assert_eq!((v["inverter"]["text"].as_str(), v["inverter"].get("trend"), v["battery"]["state"].as_str()), (Some("43°C"), None, Some("warm")));
        // one reading missing: its pair goes; neither, or the switch off: no section
        at(&mut m, "08:35", Some(72.0), None);
        assert_eq!(temps(&m, t0), serde_json::json!({ "inverter": { "text": "72°C", "level": 0.9, "state": "hot", "trend": "up" } }));
        at(&mut m, "08:40", None, Some(-12.0));
        assert_eq!(temps(&m, t0)["battery"], serde_json::json!({ "text": "\u{2212}12°C", "level": 0.04, "state": "cold", "trend": "down" }));
        m.show_temps = false;
        assert_eq!(panel(&m, t0, false).temps, None);
        m.show_temps = true;
        at(&mut m, "08:45", None, None);
        assert_eq!(panel(&m, t0, false).temps, None);
        // the switch survives Sign Out, and Settings' helper reads the last reading
        assert_eq!(crate::settings::temps_sent(&m), "none");
        at(&mut m, "08:50", Some(36.0), None);
        assert_eq!(crate::settings::temps_sent(&m), "inverter");
        m.show_temps = false;
        m.reset(false);
        assert!(!m.show_temps && m.temps.is_empty());
        assert_eq!(crate::settings::temps_sent(&m), "both", "before the first reading");
    }

    #[test]
    fn grid_off_live_then_stale() {
        let t0 = Instant::now();
        let mut m = Monitor::new(true);
        m.on_reading(reading("10/18/2026 07:37:00", GridSignal::On, Some((90.0, -500.0))), t0);
        m.on_reading(reading("10/18/2026 07:42:00", GridSignal::Off, Some((88.0, 998.28))), t0);
        let t = t0 + 38 * MIN;
        m.on_reading(reading("10/18/2026 08:20:00", GridSignal::Off, Some((86.0, 998.28))), t);
        assert_eq!(panel(&m, t, false).status.detail, None, "S1: history not answered yet");
        m.after_history(LocalTs::parse("10/18/2026 08:20:00").unwrap().0 / 60); // history failed: S7a (seen live)
        let p = panel(&m, t, false);
        assert_eq!((p.status.kind, p.status.title), ("off", "Grid off"));
        assert_eq!(p.status.detail.as_deref(), Some("Since 7:42\u{a0}AM (38 min)"));
        let b = p.battery.unwrap();
        assert_eq!((b.soc.as_str(), b.power.as_str()), ("86%", "Discharging 998 W"));
        assert_eq!(b.left, Some(Left { time: "~4h 20m".into(), rest: "left at 1.0 kW".into() }));
        assert_eq!(p.updated, Some(Updated { stale: false, text: "Updated 8:20\u{a0}AM".into() }));
        assert_eq!(tray(&m, t), TrayLook { icon: TrayIcon::OffRed, title: Some("86%".into()), tooltip: "Grid off · battery 86%".into() });

        // no new info.time for 15 min: grey, values kept, no "as of" (T-003)
        let t = t0 + 135 * MIN;
        let p = panel(&m, t, false);
        assert_eq!(p.status.detail.as_deref(), Some("Since 7:42\u{a0}AM (2h 15m)"));
        assert_eq!(p.battery.unwrap().left.unwrap().rest, "left at 1.0 kW");
        assert_eq!(p.updated, Some(Updated { stale: true, text: "No new data since 8:20\u{a0}AM".into() }));
        assert_eq!(tray(&m, t), TrayLook { icon: TrayIcon::OffDim, title: Some("86%".into()), tooltip: "No new data since 8:20\u{a0}AM".into() });
    }

    #[test]
    fn grid_on_unknown_after_restart_and_no_battery() {
        let t0 = Instant::now();
        // first reading during an outage: "Since at least"
        let mut m = Monitor::new(true);
        m.on_reading(reading("10/18/2026 08:20:00", GridSignal::Off, Some((86.0, 0.0))), t0);
        m.after_history(LocalTs::parse("10/18/2026 08:20:00").unwrap().0 / 60);
        let p = panel(&m, t0, false);
        assert_eq!(p.status.detail.as_deref(), Some("Since at least 8:20\u{a0}AM"));
        assert_eq!(p.battery.unwrap(), BatteryView { soc: "86%".into(), power: "Battery idle".into(), left: None });

        // first reading disagrees: not clear yet, plain plug
        let mut m = Monitor::new(true);
        m.on_reading(reading("10/18/2026 08:20:00", GridSignal::Disagree, Some((90.6, -1200.0))), t0);
        assert_eq!(panel(&m, t0, false).status.title, "Grid status not clear yet");
        assert_eq!(tray(&m, t0).tooltip, "Grid status not clear yet · battery 91%");
        assert_eq!(tray(&m, t0).icon, TrayIcon::Plug);

        // grid on, stale: dim plug
        m.on_reading(reading("10/18/2026 08:25:00", GridSignal::On, Some((91.0, -1200.0))), t0);
        assert_eq!(tray(&m, t0).tooltip, "Grid on · battery 91%");
        assert_eq!(tray(&m, t0 + 15 * MIN).icon, TrayIcon::Dim);

        // no battery: no title, grid title alone, no battery section or chart
        let mut m = Monitor::new(true);
        m.on_reading(reading("10/18/2026 08:20:00", GridSignal::Off, None), t0);
        assert_eq!(tray(&m, t0), TrayLook { icon: TrayIcon::OffRed, title: None, tooltip: "Grid off".into() });
        let p = panel(&m, t0, false);
        assert!(p.battery.is_none() && p.chart.is_none() && p.updated.is_some());
        // no battery, no history: T-001's own reading ("since at least", or exact when seen going off)
        assert_eq!(p.status.detail.as_deref(), Some("Since at least 8:20\u{a0}AM"));
        let (m, el) = off_no_battery(t0, &|hm: &str| format!("10/18/2026 {hm}:00"));
        assert_eq!(panel(&m, el, false).status.detail.as_deref(), Some("Since 7:42\u{a0}AM (38 min)"));
    }

    // ---- T-002: since / back / next through Monitor (spec › Since rule, Back-on, Next-cut tests) ----

    use crate::outages::testdata::{at, days_from, fixture, table};
    use crate::outages::GridDay;
    use std::collections::BTreeMap;

    /// "2026-10-18 11:22" → the reading's time string.
    fn ts(s: &str) -> String {
        format!("{s}:00")
    }
    fn day(s: &str) -> i64 {
        at(s).div_euclid(DAY)
    }
    /// Feeds one poll's history at station minute `now`, as poll_once does.
    fn history(m: &mut Monitor, days: BTreeMap<i64, GridDay>, now: i64) {
        for (d, g) in days {
            m.on_day(d, g, now);
        }
        m.after_history(now);
    }
    /// A monitor with live readings `live` (time, signal; one minute apart on the app clock is not
    /// needed: each is seen at its own station time) and the panel's instant at the last one.
    fn live(v: &[(&str, GridSignal)]) -> (Monitor, Instant) {
        let t0 = Instant::now();
        let first = at(v[0].0);
        let mut m = Monitor::new(true);
        let mut el = t0;
        for (t, g) in v {
            el = t0 + Duration::from_secs(((at(t) - first) * 60) as u64);
            m.on_reading(reading(&ts(t), *g, Some((80.0, 900.0))), el);
        }
        (m, el)
    }
    /// The fixture's real series (trimmed windows), day `d` cut at minute `upto` of that day.
    fn sample_day(d: &str, upto: Option<u32>) -> GridDay {
        let g = crate::sems::parse_grid_day(&fixture()["reply"], day(&format!("{d} 00:00")), false).unwrap();
        match upto {
            Some(u) => GridDay { off: g.off.into_iter().filter(|p| p.0 <= u).collect(), newest: Some(u), ..Default::default() },
            None => g,
        }
    }
    fn detail(m: &Monitor, now: Instant) -> Option<String> {
        panel(m, now, false).status.detail
    }
    use GridSignal::{Off as OFF, On as ON};

    #[test]
    fn since_from_history_today() {
        // table-driven series (not series-real): 10-18 09:07–12:03
        let (mut m, now) = live(&[("2026-10-18 11:22", OFF)]);
        history(&mut m, days_from(&table(), day("2026-10-04 00:00"), at("2026-10-18 11:22")), at("2026-10-18 11:22"));
        let st = panel(&m, now, false).status;
        assert_eq!(st.detail.as_deref(), Some("Since 9:07\u{a0}AM (2h 15m)"));
        assert_eq!(st.back.as_deref(), Some("Usually back around 12:05\u{a0}PM (last 14 days)"), "A: 5 days disagree, 14 agree");
    }

    #[test]
    fn since_from_yesterday() {
        // series-real: the sample, yesterday off from 23:33, today off from 00:00, now 00:40
        let (mut m, now) = live(&[("2026-10-18 00:40", OFF)]);
        let days = BTreeMap::from([(day("2026-10-17 00:00"), sample_day("2026-10-17", None)), (day("2026-10-18 00:00"), sample_day("2026-10-18", Some(40)))]);
        history(&mut m, days, at("2026-10-18 00:40"));
        assert_eq!(detail(&m, now).as_deref(), Some("Since yesterday 11:33\u{a0}PM (1h 7m)"));
    }

    #[test]
    fn since_two_day_cap() {
        // made-up: off since 10-16 20:00, still off at 10-18 01:00
        let (mut m, now) = live(&[("2026-10-18 01:00", OFF)]);
        let tb = [(at("2026-10-16 20:00"), at("2026-10-18 05:00"))];
        history(&mut m, days_from(&tb, day("2026-10-16 00:00"), at("2026-10-18 01:00")), at("2026-10-18 01:00"));
        assert_eq!(detail(&m, now).as_deref(), Some("Since at least yesterday 12:00\u{a0}AM"));
        assert_eq!(panel(&m, now, false).status.back, None);
    }

    #[test]
    fn since_yesterday_call_failed() {
        // series-real today (off from 00:00), yesterday's call failed
        let (mut m, now) = live(&[("2026-10-18 00:40", OFF)]);
        history(&mut m, BTreeMap::from([(day("2026-10-18 00:00"), sample_day("2026-10-18", Some(40)))]), at("2026-10-18 00:40"));
        assert_eq!(detail(&m, now).as_deref(), Some("Since at least 12:00\u{a0}AM"));
        assert_eq!(panel(&m, now, false).status.back, None);
    }

    #[test]
    fn since_history_lagging() {
        // made-up: live off at 11:58; history's newest point 11:56 is still on
        let (mut m, now) = live(&[("2026-10-18 11:53", ON), ("2026-10-18 11:58", OFF)]);
        let tb = [(at("2026-10-18 11:57"), at("2026-10-18 13:00"))];
        history(&mut m, days_from(&tb, day("2026-10-18 00:00"), at("2026-10-18 11:56")), at("2026-10-18 11:58"));
        // seen live (On 5 min before): S7a
        let now = now + Duration::from_secs(120);
        assert_eq!(detail(&m, now).as_deref(), Some("Since 11:58\u{a0}AM (2 min)"));
        // next poll: history shows off from 11:57
        m.on_reading(reading(&ts("2026-10-18 12:00"), OFF, Some((80.0, 900.0))), now);
        history(&mut m, days_from(&tb, day("2026-10-18 00:00"), at("2026-10-18 12:00")), at("2026-10-18 12:00"));
        assert_eq!(detail(&m, now).as_deref(), Some("Since 11:57\u{a0}AM (3 min)"));
    }

    #[test]
    fn live_cut_before_history_has_it() {
        // T-025, the owner's 9:04 AM: live saw the grid go off, history has no off minute yet (its cut starts 9:07)
        let tb = table();
        let (mut m, now) = live(&[("2026-10-18 09:00", ON), ("2026-10-18 09:04", OFF)]);
        history(&mut m, days_from(&tb, day("2026-10-04 00:00"), at("2026-10-18 09:03")), at("2026-10-18 09:04"));
        assert_eq!(m.hist_start, HistStart::NotOff);
        let p = panel(&m, now, false);
        assert_eq!(p.status.detail.as_deref(), Some("Since 9:04\u{a0}AM (0 min)"));
        assert_eq!(p.status.back.as_deref(), Some("Usually back around 12:05\u{a0}PM (last 14 days)"));
        // today already had two cuts (12:00–2:06 AM, 7:06–8:40 AM); the live one is the third
        let cuts = p.cuts.unwrap();
        assert_eq!((cuts.strip.cuts[2], cuts.rows.len(), cuts.rows[2].running), ((544, 544), 3, true));
        assert_eq!((cuts.rows[2].off.as_str(), cuts.rows[2].dur.as_str()), ("9:04\u{a0}AM", "0 min so far"));
        assert!(cuts.summary.starts_with("So far today: 3 power cuts"), "{}", cuts.summary);
        assert_eq!(crate::stats::view(&m, now).rows, cuts.rows, "Stats today: the same rows");
        // 9:10: history's start (9:07–9:10, 3 off minutes) but no outage yet (under 5 min): one row, from 9:07
        let now = now + Duration::from_secs(6 * 60);
        m.on_reading(reading(&ts("2026-10-18 09:10"), OFF, Some((80.0, 900.0))), now);
        history(&mut m, days_from(&tb, day("2026-10-04 00:00"), at("2026-10-18 09:10")), at("2026-10-18 09:10"));
        let p = panel(&m, now, false);
        let rows = p.cuts.unwrap().rows;
        assert_eq!((rows.len(), rows[2].dur.as_str(), p.status.detail.as_deref()), (3, "3 min so far", Some("Since 9:07\u{a0}AM (3 min)")));
        // 9:12: history's own running cut takes over, no second row
        let now = now + Duration::from_secs(2 * 60);
        m.on_reading(reading(&ts("2026-10-18 09:12"), OFF, Some((80.0, 900.0))), now);
        history(&mut m, days_from(&tb, day("2026-10-04 00:00"), at("2026-10-18 09:12")), at("2026-10-18 09:12"));
        let p = panel(&m, now, false);
        let rows = p.cuts.unwrap().rows;
        assert_eq!((rows.len(), rows[2].off.as_str(), rows[2].running), (3, "9:07\u{a0}AM", true));
        assert_eq!(p.status.back.as_deref(), Some("Usually back around 12:05\u{a0}PM (last 14 days)"));
        // grid back: the live cut goes with it
        let (mut m2, now2) = live(&[("2026-10-18 09:00", ON), ("2026-10-18 09:04", OFF), ("2026-10-18 09:06", ON)]);
        history(&mut m2, days_from(&tb, day("2026-10-04 00:00"), at("2026-10-18 09:03")), at("2026-10-18 09:06"));
        assert_eq!(panel(&m2, now2, false).cuts.unwrap().rows.len(), 2);
        // T-025 reopened, the owner's 9:29 install mid-cut: first reading already off ("at least"); the cut
        // is still drawn from that reading, with the back-on line, in the strip and in Stats
        let (mut m, now) = live(&[("2026-10-18 09:29", OFF)]);
        history(&mut m, days_from(&tb, day("2026-10-04 00:00"), at("2026-10-18 09:03")), at("2026-10-18 09:29"));
        assert_eq!(m.hist_start, HistStart::Stale, "history stopped at 9:03");
        let p = panel(&m, now, false);
        assert_eq!((p.status.detail.as_deref(), p.status.back.as_deref()), (Some("Since at least 9:29\u{a0}AM"), Some("Usually back around 12:05\u{a0}PM (last 14 days)")));
        let cuts = p.cuts.unwrap();
        assert_eq!((cuts.rows.len(), cuts.rows[2].off.as_str(), cuts.rows[2].running, cuts.strip.cuts[2]), (3, "9:29\u{a0}AM", true, (569, 569)));
        assert_eq!(crate::stats::view(&m, now).rows, cuts.rows, "Stats today: the same rows");
        // 9:35: history catches up (its cut from 9:08) and takes over the start, in one row
        let now = now + Duration::from_secs(6 * 60);
        m.on_reading(reading(&ts("2026-10-18 09:35"), OFF, Some((80.0, 900.0))), now);
        let mut days = days_from(&tb, day("2026-10-04 00:00"), at("2026-10-18 09:03"));
        let today = days.get_mut(&day("2026-10-18 00:00")).unwrap();
        today.off.extend((548..=575).map(|m| (m, true)));
        today.newest = Some(575);
        history(&mut m, days, at("2026-10-18 09:35"));
        let p = panel(&m, now, false);
        let rows = p.cuts.unwrap().rows;
        assert_eq!((rows.len(), rows[2].off.as_str(), p.status.detail.as_deref()), (3, "9:08\u{a0}AM", Some("Since 9:08\u{a0}AM (27 min)")));
    }

    #[test]
    fn since_lone_zero_minute_ignored() {
        // series-real: 12:10 is one off minute after the 12:03 end → not an outage → S7
        let (mut m, now) = live(&[("2026-10-18 12:10", OFF)]);
        let days = BTreeMap::from([(day("2026-10-17 00:00"), sample_day("2026-10-17", None)), (day("2026-10-18 00:00"), sample_day("2026-10-18", Some(730)))]);
        history(&mut m, days, at("2026-10-18 12:10"));
        assert_eq!(m.hist_start, HistStart::NotOff);
        assert_eq!(detail(&m, now).as_deref(), Some("Since at least 12:10\u{a0}PM"), "first reading already off: S7");
        // D-Y16: the same history with live 12:05 On, 12:10 Off → S7a
        let (mut m, now) = live(&[("2026-10-18 12:05", ON), ("2026-10-18 12:10", OFF)]);
        let days = BTreeMap::from([(day("2026-10-17 00:00"), sample_day("2026-10-17", None)), (day("2026-10-18 00:00"), sample_day("2026-10-18", Some(730)))]);
        history(&mut m, days, at("2026-10-18 12:10"));
        assert_eq!(detail(&m, now).as_deref(), Some("Since 12:10\u{a0}PM (0 min)"));
    }

    #[test]
    fn since_seen_live_exact() {
        // S7a: history call errors; 07:37 On, 07:42 Off, now 08:20
        let (mut m, now) = live(&[("2026-10-18 07:37", ON), ("2026-10-18 07:42", OFF), ("2026-10-18 08:20", OFF)]);
        history(&mut m, BTreeMap::new(), at("2026-10-18 08:20"));
        let st = panel(&m, now, false).status;
        assert_eq!((st.detail.as_deref(), st.back), (Some("Since 7:42\u{a0}AM (38 min)"), None));
    }

    #[test]
    fn since_seen_live_after_gap_is_at_least() {
        // 08:00 On, next reading 11:00 Off (3 h gap, e.g. the Mac slept)
        let (mut m, now) = live(&[("2026-10-18 08:00", ON), ("2026-10-18 11:00", OFF)]);
        history(&mut m, BTreeMap::new(), at("2026-10-18 11:00"));
        assert_eq!(detail(&m, now).as_deref(), Some("Since at least 11:00\u{a0}AM"));
        // the boundary: 10 min is still "seen live", 11 is not
        let (mut m, now) = live(&[("2026-10-18 10:50", ON), ("2026-10-18 11:00", OFF)]);
        history(&mut m, BTreeMap::new(), at("2026-10-18 11:00"));
        assert_eq!(detail(&m, now).as_deref(), Some("Since 11:00\u{a0}AM (0 min)"));
        let (mut m, now) = live(&[("2026-10-18 10:49", ON), ("2026-10-18 11:00", OFF)]);
        history(&mut m, BTreeMap::new(), at("2026-10-18 11:00"));
        assert_eq!(detail(&m, now).as_deref(), Some("Since at least 11:00\u{a0}AM"));
    }

    #[test]
    fn since_unknown_then_off_is_at_least() {
        let (mut m, now) = live(&[("2026-10-18 07:37", GridSignal::Disagree), ("2026-10-18 07:42", OFF)]);
        history(&mut m, BTreeMap::new(), at("2026-10-18 07:42"));
        assert_eq!(detail(&m, now).as_deref(), Some("Since at least 7:42\u{a0}AM"));
    }

    #[test]
    fn since_no_battery_station() {
        let t0 = Instant::now();
        let at = |hm: &str| format!("10/18/2026 {hm}:00");
        let (m, el) = off_no_battery(t0, &at);
        assert_eq!(detail(&m, el).as_deref(), Some("Since 7:42\u{a0}AM (38 min)"), "seen live");
        let mut m = Monitor::new(true);
        m.on_reading(reading(&at("08:20"), GridSignal::Off, None), t0);
        assert_eq!(detail(&m, t0).as_deref(), Some("Since at least 8:20\u{a0}AM"), "first reading off");
    }

    #[test]
    fn since_history_failed() {
        let (mut m, now) = live(&[("2026-10-18 11:58", OFF)]);
        history(&mut m, BTreeMap::new(), at("2026-10-18 11:58"));
        assert_eq!(detail(&m, now).as_deref(), Some("Since at least 11:58\u{a0}AM"));
    }

    #[test]
    fn since_history_stale_keeps_start() {
        let (mut m, now) = live(&[("2026-10-18 11:22", OFF)]);
        history(&mut m, days_from(&table(), day("2026-10-04 00:00"), at("2026-10-18 11:22")), at("2026-10-18 11:22"));
        // live goes on to 11:37, history's newest point stays 11:22 (15 min old)
        let now = now + Duration::from_secs(15 * 60);
        m.on_reading(reading(&ts("2026-10-18 11:37"), OFF, Some((70.0, 900.0))), now);
        history(&mut m, BTreeMap::new(), at("2026-10-18 11:37"));
        assert_eq!(m.hist_start, HistStart::Stale);
        assert_eq!(detail(&m, now).as_deref(), Some("Since 9:07\u{a0}AM (2h 30m)"));
    }

    #[test]
    fn since_history_stale_no_start() {
        let (mut m, now) = live(&[("2026-10-18 11:22", OFF)]);
        history(&mut m, days_from(&table(), day("2026-10-04 00:00"), at("2026-10-18 11:00")), at("2026-10-18 11:22"));
        assert_eq!(detail(&m, now).as_deref(), Some("Since at least 11:22\u{a0}AM"));
        // seen live: S7a
        let (mut m, now) = live(&[("2026-10-18 11:17", ON), ("2026-10-18 11:22", OFF)]);
        history(&mut m, days_from(&table(), day("2026-10-04 00:00"), at("2026-10-18 11:00")), at("2026-10-18 11:22"));
        assert_eq!(detail(&m, now).as_deref(), Some("Since 11:22\u{a0}AM (0 min)"));
    }

    #[test]
    fn since_loading_title_only() {
        let (m, now) = live(&[("2026-10-18 11:22", OFF)]);
        let st = panel(&m, now, false).status;
        assert_eq!((st.title, st.detail, st.back), ("Grid off", None, None));
    }

    #[test]
    fn since_live_on_wins() {
        let (mut m, now) = live(&[("2026-10-18 11:22", ON)]);
        history(&mut m, days_from(&table(), day("2026-10-04 00:00"), at("2026-10-18 11:22")), at("2026-10-18 11:22"));
        let st = panel(&m, now, false).status;
        assert_eq!((st.kind, st.detail, st.back), ("on", None, None));
        assert_eq!(m.since_start, None);
    }

    #[test]
    fn since_forgotten_after_grid_back() {
        // table-driven 10-14: 09:08–12:03, then 13:35–16:31
        let tb = table();
        let (mut m, now) = live(&[("2026-10-14 11:22", OFF)]);
        history(&mut m, days_from(&tb, day("2026-09-30 00:00"), at("2026-10-14 11:22")), at("2026-10-14 11:22"));
        assert_eq!(detail(&m, now).as_deref(), Some("Since 9:08\u{a0}AM (2h 14m)"));
        let now = now + Duration::from_secs(48 * 60);
        m.on_reading(reading(&ts("2026-10-14 12:10"), ON, Some((60.0, -3000.0))), now);
        assert_eq!(m.since_start, None);
        let now = now + Duration::from_secs(95 * 60);
        m.on_reading(reading(&ts("2026-10-14 13:45"), OFF, Some((90.0, 900.0))), now);
        history(&mut m, days_from(&tb, day("2026-09-30 00:00"), at("2026-10-14 13:45")), at("2026-10-14 13:45"));
        assert_eq!(detail(&m, now).as_deref(), Some("Since 1:35\u{a0}PM (10 min)"));
    }

    #[test]
    fn since_flicker_missed_by_live() {
        // made-up: first off reading 09:08; history off 09:07–09:59, on 10:00–10:07, off from 10:08
        let (mut m, now) = live(&[("2026-10-18 09:00", ON), ("2026-10-18 09:08", OFF), ("2026-10-18 10:20", OFF)]);
        let tb = [(at("2026-10-18 09:07"), at("2026-10-18 09:59")), (at("2026-10-18 10:08"), at("2026-10-18 11:00"))];
        history(&mut m, days_from(&tb, day("2026-10-18 00:00"), at("2026-10-18 10:20")), at("2026-10-18 10:20"));
        assert_eq!(detail(&m, now).as_deref(), Some("Since 10:08\u{a0}AM (12 min)"));
    }

    #[test]
    fn back_kept_while_stale() {
        // R1, real (table-driven series): 10-14 11:22 → 12:05; at 11:40 with no data since 11:22 → kept
        let (mut m, now) = live(&[("2026-10-14 11:22", OFF)]);
        history(&mut m, days_from(&table(), day("2026-09-30 00:00"), at("2026-10-14 11:22")), at("2026-10-14 11:22"));
        let st = panel(&m, now, false).status;
        assert_eq!(st.back.as_deref(), Some("Usually back around 12:05\u{a0}PM (last 14 days)"));
        let later = now + Duration::from_secs(18 * 60);
        let p = panel(&m, later, false);
        assert!(p.updated.unwrap().stale);
        assert_eq!(p.status.detail.as_deref(), Some("Since 9:08\u{a0}AM (2h 32m)"));
        assert_eq!(p.status.back.as_deref(), Some("Usually back around 12:05\u{a0}PM (last 14 days)"));
    }

    #[test]
    fn back_absent_without_history_start() {
        let tb = table();
        let r1 = || days_from(&tb, day("2026-09-30 00:00"), at("2026-10-14 11:22"));
        // S1: the first history call is still out
        let (m, now) = live(&[("2026-10-14 11:22", OFF)]);
        assert_eq!(panel(&m, now, false).status.back, None);
        // S7: today's call failed, the 14 past days are there
        let (mut m, now) = live(&[("2026-10-14 11:22", OFF)]);
        let mut d = r1();
        d.remove(&day("2026-10-14 00:00"));
        history(&mut m, d, at("2026-10-14 11:22"));
        let st = panel(&m, now, false).status;
        assert_eq!((st.detail.as_deref(), st.back), (Some("Since at least 11:22\u{a0}AM"), None));
        // a past day missing: no "(last 14 days)" line
        let (mut m, now) = live(&[("2026-10-14 11:22", OFF)]);
        let mut d = r1();
        d.remove(&day("2026-10-11 00:00"));
        history(&mut m, d, at("2026-10-14 11:22"));
        let st = panel(&m, now, false).status;
        assert_eq!((st.detail.as_deref(), st.back), (Some("Since 9:08\u{a0}AM (2h 14m)"), None));
        // S4 and S5 are covered in since_two_day_cap and since_yesterday_call_failed
    }

    #[test]
    fn back_absent_when_grid_on() {
        let (mut m, now) = live(&[("2026-10-14 11:22", ON)]);
        history(&mut m, days_from(&table(), day("2026-09-30 00:00"), at("2026-10-14 11:22")), at("2026-10-14 11:22"));
        assert_eq!(panel(&m, now, false).status.back, None);
    }

    #[test]
    fn next_cut_absent_when_grid_off_or_unknown() {
        let d = || days_from(&table(), day("2026-10-04 00:00"), at("2026-10-18 12:10"));
        let (mut m, now) = live(&[("2026-10-18 12:10", ON)]);
        history(&mut m, d(), at("2026-10-18 12:10"));
        let st = panel(&m, now, false).status;
        assert_eq!(st.next, Some(["Next cut usually around 1:35\u{a0}PM".to_owned(), "back around 4:30\u{a0}PM (last 14 days)".to_owned()]));
        for g in [OFF, GridSignal::Disagree] {
            let (mut m, now) = live(&[("2026-10-18 12:10", g)]);
            history(&mut m, d(), at("2026-10-18 12:10"));
            assert_eq!(panel(&m, now, false).status.next, None, "{g:?}");
        }
        // signed out / first reading: no next
        assert_eq!(panel(&Monitor::new(false), now, false).status.next, None);
        assert_eq!(panel(&Monitor::new(true), now, false).status.next, None);
    }

    #[test]
    fn next_cut_absent_without_history() {
        let d = || days_from(&table(), day("2026-10-04 00:00"), at("2026-10-18 12:10"));
        // not loaded yet
        let (m, now) = live(&[("2026-10-18 12:10", ON)]);
        assert_eq!(panel(&m, now, false).status.next, None);
        // one past day missing
        let (mut m, now) = live(&[("2026-10-18 12:10", ON)]);
        let mut x = d();
        x.remove(&day("2026-10-15 00:00"));
        history(&mut m, x, at("2026-10-18 12:10"));
        assert_eq!(panel(&m, now, false).status.next, None);
        // today's history missing (a "done" cut couldn't be seen)
        let (mut m, now) = live(&[("2026-10-18 12:10", ON)]);
        let mut x = d();
        x.remove(&day("2026-10-18 00:00"));
        history(&mut m, x, at("2026-10-18 12:10"));
        assert_eq!(panel(&m, now, false).status.next, None);
    }

    #[test]
    fn json_shape_matches_panel_js() {
        let mut m = Monitor::new(true);
        m.on_reading(reading("10/18/2026 08:20:00", GridSignal::On, Some((91.0, 0.0))), Instant::now());
        let v = serde_json::to_value(panel(&m, Instant::now(), true)).unwrap();
        for k in ["status", "battery", "updated", "notificationsOff", "chart", "signedIn", "refreshing", "cuts"] {
            assert!(v.get(k).is_some(), "missing {k}");
        }
        // T-020: today's strip while the switch is on (loading here: no history yet); off sends null.
        assert_eq!(v["cuts"]["status"], "loading");
        m.show_cuts = false;
        assert_eq!(panel(&m, Instant::now(), true).cuts, None);
        assert!(v.get("account").is_none(), "the account is in Settings, never the panel");
        assert_eq!(v["chart"]["day"], "18 Oct");
        assert_eq!(v["status"], serde_json::json!({ "kind": "on", "title": "Grid on" }), "back and next only when set");
        let st = Status { kind: "off", title: "Grid off", detail: Some("d".into()), back: Some("b".into()), next: Some(["1".into(), "2".into()]) };
        assert_eq!(serde_json::to_value(st).unwrap(), serde_json::json!({ "kind": "off", "title": "Grid off", "detail": "d", "back": "b", "next": ["1", "2"] }));
        assert!(v["battery"].get("left").is_none());
    }

    /// The designer's demo day (mockups/panel/demo-data.js `day()`), as SEMS+ chart input.
    fn demo_day(end: u32, soc: f64, kw: f64, from: u32) -> Vec<(u32, f64, f64)> {
        let s0 = (soc + kw * ((end - from) as f64 / 60.0) / 5.1 * 100.0).clamp(0.0, 100.0);
        let c_rate = (100.0 - s0) / 100.0 * 5.1 / ((from - 166) as f64 / 60.0);
        (0..=end).step_by(2).map(|m| {
            let mf = m as f64;
            let (s, k) = if m < 120 { (93.0 - 30.0 * mf / 120.0, 0.76 + 0.14 * (mf / 7.0).sin()) }
                else if m < 166 { (63.0 + 37.0 * (mf - 120.0) / 46.0, if m < 124 { -1.9 } else { -3.8 }) }
                else if m < from { (100.0 - (100.0 - s0) * (mf - 166.0) / (from - 166) as f64, c_rate + 0.03 * (mf / 5.0).sin()) }
                else { (s0 + (soc - s0) * (mf - from as f64) / (end - from).max(1) as f64, kw + 0.05 * (mf / 3.0).sin()) };
            (m, s.round(), (k * 100.0).round() / 100.0)
        }).collect()
    }

    /// B-R4: every hand-off panel and menu bar state, built through Monitor + view, written as
    /// JSON for the shot renderer. Run: GW_FIXTURES=<file> cargo test … write_state_fixtures -- --ignored
    #[test]
    #[ignore]
    fn write_state_fixtures() {
        use crate::sems::ChartSeries;
        let t0 = Instant::now();
        let at = |hhmm: &str| format!("10/18/2026 {hhmm}:00");
        let day = LocalTs::parse(&at("08:20")).unwrap().day();
        let plus = |pts: Vec<(u32, f64, f64)>| Some(ChartSeries { day, points: pts });
        // Grid on at 07:30, off at 07:42, then a reading at `hhmm` (minutes after 07:42 on the app clock).
        let off = |hhmm: &str, soc: f64, w: f64| {
            let mut m = Monitor::new(true);
            m.on_reading(reading(&at("07:30"), GridSignal::On, Some((95.0, 0.0))), t0);
            m.on_reading(reading(&at("07:42"), GridSignal::Off, Some((90.0, w))), t0);
            let t = LocalTs::parse(&at(hhmm)).unwrap();
            let el = t0 + Duration::from_secs((t.0 - LocalTs::parse(&at("07:42")).unwrap().0) as u64);
            m.on_reading(reading(&at(hhmm), GridSignal::Off, Some((soc, w))), el);
            (m, el)
        };
        let on = |hhmm: &str, battery: Option<(f64, f64)>, sig: GridSignal| {
            let mut m = Monitor::new(true);
            m.on_reading(reading(&at(hhmm), sig, battery), t0);
            (m, t0)
        };
        let ok = |mut x: (Monitor, Instant), pts| { x.0.on_chart(plus(pts)); x };
        let mut out = serde_json::Map::new();
        let mut put = |name: &str, (m, now): (Monitor, Instant), later: u64, notif_off: bool| {
            let now = now + Duration::from_secs(later * 60);
            let t = tray(&m, now);
            let tray = serde_json::json!({ "icon": format!("{:?}", t.icon), "title": t.title, "tooltip": t.tooltip });
            out.insert(name.into(), serde_json::json!({ "panel": panel(&m, now, notif_off), "tray": tray }));
        };
        let d = |soc, kw| demo_day(500, soc, kw, 462);
        put("grid-off-discharging-hover", ok(off("08:20", 86.0, 998.28), d(86.0, 0.998)), 0, false);
        put("grid-on-charging", ok(on("08:20", Some((91.0, -1200.0)), GridSignal::On), d(91.0, -1.2)), 0, false);
        put("grid-on-idle", ok(on("08:20", Some((100.0, 0.0)), GridSignal::On), d(100.0, 0.0)), 0, false);
        put("grid-on-discharging", ok(on("08:20", Some((91.0, 640.0)), GridSignal::On), d(91.0, 0.64)), 0, false);
        put("grid-off-after-restart", ok(on("08:20", Some((86.0, 998.28)), GridSignal::Off), d(86.0, 0.998)), 0, false);
        put("grid-off-charging", ok(off("08:20", 86.0, -1400.0), d(86.0, -1.4)), 0, false);
        put("grid-off-idle", ok(off("08:20", 100.0, 0.0), d(100.0, 0.0)), 0, false);
        put("grid-off-over-1h", ok(off("09:57", 62.0, 1100.0), demo_day(596, 62.0, 1.1, 462)), 0, false);
        put("grid-unknown", ok(on("08:20", Some((91.0, -1200.0)), GridSignal::Disagree), d(91.0, -1.2)), 0, false);
        put("stale-grid-off", ok(off("08:20", 86.0, 998.28), d(86.0, 0.998)), 130, false);
        put("stale-grid-on", ok(on("08:20", Some((91.0, -1200.0)), GridSignal::On), d(91.0, -1.2)), 130, false);
        put("notifications-off", ok(on("08:20", Some((91.0, -1200.0)), GridSignal::On), d(91.0, -1.2)), 0, true);
        put("no-battery-grid-on", on("08:20", None, GridSignal::On), 0, false);
        put("no-battery-grid-off", off_no_battery(t0, &at), 0, false);
        put("first-reading", (Monitor::new(true), t0), 0, false);
        put("signed-out", (Monitor::new(false), t0), 0, false);
        put("chart-loading", off("08:20", 86.0, 998.28), 0, false);
        put("chart-empty-after-midnight", ok(on("00:01", Some((91.0, 640.0)), GridSignal::On), vec![]), 0, false);
        let gap: Vec<_> = demo_day(560, 80.0, 1.0, 462).into_iter().filter(|p| p.0 <= 500 || p.0 >= 540).collect();
        put("chart-gap", ok(off("09:20", 80.0, 1000.0), gap), 0, false);
        // SEMS+ failed: own readings every 5 min (the inverter's pace), from yesterday 23:58 or from 07:58.
        let own = |first: &str| {
            let mut m = Monitor::new(true);
            let (start, end) = (LocalTs::parse(first).unwrap(), LocalTs::parse(&at("08:20")).unwrap());
            let demo = d(86.0, 0.998);
            let mut t = start;
            while t.0 <= end.0 {
                let p = demo[((t.minute_of_day().min(500) / 2) as usize).min(demo.len() - 1)];
                let p = if t.day() == day { p } else { demo[0] };
                let sig = if t.day() == day && t.minute_of_day() >= 462 { GridSignal::Off } else { GridSignal::On };
                m.on_reading(reading(&fmt_ts(t), sig, Some((p.1, p.2 * 1000.0))), t0 + Duration::from_secs((t.0 - start.0) as u64));
                t = t.plus_secs(300);
            }
            m.on_chart(None);
            let now = t0 + Duration::from_secs((t.0 - 300 - start.0) as u64);
            (m, now)
        };
        put("chart-fallback-since-yesterday", own("10/17/2026 23:58:00"), 0, false);
        put("chart-fallback-since-launch", own("10/18/2026 07:58:00"), 0, false);
        // Reachable "failed": the one own reading is from yesterday (23:58), now 00:08, SEMS+ failed.
        let mut m = Monitor::new(true);
        m.on_reading(reading("10/17/2026 23:58:00", GridSignal::Off, Some((86.0, 998.28))), t0);
        m.on_chart(None);
        put("chart-failed-nothing-yet", (m, t0), 10, false);
        // menu bar: the same monitors (grid on = 91 % charging, grid off = 86 % live)
        for (k, v) in [("grid-on", "grid-on-charging"), ("grid-off", "grid-off-discharging-hover"), ("stale-grid-on", "stale-grid-on"), ("stale-grid-off", "stale-grid-off"),
            ("grid-unknown", "grid-unknown"), ("no-battery-grid-on", "no-battery-grid-on"), ("no-battery-grid-off", "no-battery-grid-off"), ("signed-out", "signed-out")] {
            let tray = out[v]["tray"].clone();
            out.insert(format!("menubar-{k}"), serde_json::json!({ "tray": tray }));
        }
        let path = std::env::var("GW_FIXTURES").unwrap_or_else(|_| std::env::temp_dir().join("gw-fixtures.json").to_string_lossy().into_owned());
        std::fs::write(&path, serde_json::to_string_pretty(&out).unwrap()).unwrap();
        println!("{} states → {path}", out.len());
    }

    #[test]
    fn refreshing_keeps_freshness_text() {
        let (mut m, now) = live(&[("2026-10-18 11:22", ON)]);
        assert_eq!(m.ask_refresh(), Some(true));
        let p = panel(&m, now, false);
        assert!(p.refreshing);
        assert_eq!(p.updated.unwrap().text, "Updated 11:22\u{a0}AM");
        let p = panel(&m, now + 15 * MIN, false);
        assert_eq!(p.updated.unwrap().text, "No new data since 11:22\u{a0}AM");
        m.user_poll = false;
        let p = panel(&m, now, false);
        assert!(!p.refreshing);
        assert_eq!(p.updated.unwrap().text, "Updated 11:22\u{a0}AM");
        // signed out never spins
        let mut m = Monitor::new(false);
        m.user_poll = true;
        assert!(!panel(&m, now, false).refreshing);
    }

    /// The designer's battery story for a day (mockups/panel/demo-data.js `series`): grid on/off
    /// from the real table; off → the battery carries ~0.9 kW, on → charges at 3.8 kW until full,
    /// then idles at +0.03 kW. 2-min points from 00:00 to minute `upto`, minus `holes`.
    fn demo_series(tb: &[(i64, i64)], d0: i64, upto: u32, holes: &[(u32, u32)]) -> Vec<(u32, f64, f64)> {
        let (mut soc, mut full, mut pts) = (70.0_f64, false, vec![]);
        let mut t = d0 - DAY;
        while t <= d0 + upto as i64 {
            let off = tb.iter().any(|o| t >= o.0 && t < o.1);
            let m = t - d0;
            if off { full = false } else if soc >= 99.5 { full = true }
            let tf = t as f64;
            let k = if off { if soc <= 10.0 { 0.0 } else { ((0.9 + 0.06 * (tf / 3.0).sin() + 0.04 * (tf / 11.0).sin()) * 100.0).round() / 100.0 } } else if full { 0.03 } else { -3.8 };
            if !(full && !off) { soc = (soc - k * (2.0 / 60.0) / 5.1 * 100.0).clamp(10.0, 100.0) }
            if m >= 0 && !holes.iter().any(|&(a, b)| m > a as i64 && m < b as i64) { pts.push((m as u32, soc.round(), k)) }
            t += 2;
        }
        pts
    }

    /// T-002 Build shots: every hand-off state built through Monitor + view (real table history,
    /// the rules' own lines), with the shot harness's action. Account is made up.
    /// Run: GW_FIXTURES=<file> cargo test … write_t002_fixtures -- --ignored
    #[test]
    #[ignore]
    fn write_t002_fixtures() {
        use crate::sems::ChartSeries;
        use serde_json::json;
        let tb = table();
        // Monitor at `date time`; `last` = time of the last reading (stale when earlier); `hist` = the
        // history table (None: the first call hasn't answered); chart `None` = loading.
        let build = |date: &str, time: &str, last: Option<&str>, sig: GridSignal, hist: Option<&[(i64, i64)]>, holes: &[(u32, u32)]| {
            let (now_abs, d0) = (at(&format!("{date} {}", last.unwrap_or(time))), at(&format!("{date} 00:00")));
            let pts = demo_series(&tb, d0, (now_abs - d0) as u32, holes);
            let p = *pts.last().unwrap();
            let t0 = Instant::now();
            let mut m = Monitor::new(true);
            m.account = Some("name@example.com".into());
            m.on_reading(reading(&ts(&format!("{date} {}", last.unwrap_or(time))), sig, Some((p.1, p.2 * 1000.0))), t0);
            if let Some(h) = hist {
                history(&mut m, days_from(h, now_abs.div_euclid(DAY) - outages::PAST_DAYS, now_abs), now_abs);
                m.on_chart(Some(ChartSeries { day: now_abs.div_euclid(DAY), points: pts }));
            }
            (m, t0 + Duration::from_secs(((at(&format!("{date} {time}")) - now_abs) * 60) as u64))
        };
        let off = |date: &str, time: &str, last: Option<&str>| build(date, time, last, OFF, Some(&tb), &[]);
        let on = |date: &str, time: &str, last: Option<&str>| build(date, time, last, ON, Some(&tb), &[]);
        let main = || off("2026-10-14", "11:22", None);
        let mut out = serde_json::Map::new();
        let mut put = |name: &str, (m, now): (Monitor, Instant), act: serde_json::Value| {
            out.insert(name.into(), json!({ "panel": panel(&m, now, false), "act": act }));
        };
        let none = json!({});
        put("grid-off-since-history", main(), none.clone());
        put("grid-off-since-yesterday", off("2026-10-18", "00:40", None), none.clone());
        // S7: history failed, the app's first reading already off; chart falls back to own readings
        let (mut m, t0) = build("2026-10-14", "09:27", None, OFF, None, &[]);
        let el = t0 + 3 * MIN;
        m.on_reading(reading(&ts("2026-10-14 09:30"), OFF, Some((96.0, 900.0))), el);
        history(&mut m, BTreeMap::new(), at("2026-10-14 09:30"));
        m.on_chart(None);
        put("grid-off-since-at-least", (m, el), none.clone());
        put("grid-off-history-loading", build("2026-10-14", "11:22", None, OFF, None, &[]), none.clone());
        put("grid-off-days-disagree", off("2026-10-18", "11:22", None), none.clone());
        put("grid-off-usual-time-passed", off("2026-10-15", "12:20", None), none.clone());
        put("grid-off-stale", off("2026-10-14", "11:40", Some("11:22")), none.clone());
        for (n, min) in [("discharging", 551), ("charging", 150), ("middle", 336), ("right-edge", 664)] {
            put(&format!("chart-hover-{n}"), main(), json!({ "chart": min }));
        }
        put("chart-hover-gap", build("2026-10-14", "06:30", None, ON, Some(&tb), &[(260, 320)]), json!({ "chart": 290 }));
        put("chart-keyboard-focus", main(), json!({ "focus": "chart" }));
        put("refresh-hover", main(), json!({ "refresh": "hover" }));
        put("refresh-pressed", main(), json!({ "refresh": "press" }));
        put("refresh-focus", main(), json!({ "focus": "refresh" }));
        let (mut m, el) = main();
        m.ask_refresh();
        put("refreshing", (m, el), none.clone());
        put("grid-on-next-cut", on("2026-10-18", "12:10", None), none.clone());
        put("grid-on-next-cut-start-only", on("2026-10-18", "08:55", None), none.clone());
        put("grid-on-cut-due", on("2026-10-16", "10:28", None), none.clone());
        put("grid-on-cut-due-after-midnight", on("2026-10-16", "00:05", None), none.clone());
        put("grid-on-cut-passed", on("2026-10-16", "02:30", None), none.clone());
        put("grid-on-cut-done", on("2026-10-16", "21:10", None), none.clone());
        put("grid-on-stale-cut-due", on("2026-10-16", "10:28", Some("10:10")), none.clone());
        // made-up history: one cut a day, never 3 days in one 30-min window
        let mixed: Vec<(i64, i64)> = [("13", "03:00", "04:00"), ("14", "08:00", "09:30"), ("15", "13:00", "14:30"), ("16", "17:00", "18:30"), ("17", "21:00", "22:00")]
            .iter().map(|(d, a, b)| (at(&format!("2026-10-{d} {a}")), at(&format!("2026-10-{d} {b}")))).collect();
        put("grid-on-days-disagree", build("2026-10-18", "08:55", None, ON, Some(&mixed), &[]), none.clone());
        // T-007: the account and Sign Out moved into Settings (its own UI tests); the panel keeps Sign In… while signed out.
        put("signed-out", (Monitor::new(false), Instant::now()), none.clone());
        put("settings-row-hover", main(), json!({ "row": "settings" }));
        let path = std::env::var("GW_FIXTURES").unwrap_or_else(|_| std::env::temp_dir().join("gw-t002.json").to_string_lossy().into_owned());
        std::fs::write(&path, serde_json::to_string_pretty(&out).unwrap()).unwrap();
        println!("{} states → {path}", out.len());
    }

    fn off_no_battery(t0: Instant, at: &dyn Fn(&str) -> String) -> (Monitor, Instant) {
        let mut m = Monitor::new(true);
        m.on_reading(reading(&at("07:37"), GridSignal::On, None), t0);
        m.on_reading(reading(&at("07:42"), GridSignal::Off, None), t0);
        let el = t0 + Duration::from_secs(38 * 60);
        m.on_reading(reading(&at("08:20"), GridSignal::Off, None), el);
        (m, el)
    }

    fn fmt_ts(t: LocalTs) -> String {
        let (date, hm) = (t.date(), t.hhmm()); // date = YYYY-MM-DD
        format!("{}/{}/{} {hm}:00", &date[5..7], &date[8..10], &date[0..4])
    }
}
