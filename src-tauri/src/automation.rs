//! The device automation: on a grid change, each saved Tuya device is told its grid-off or
//! grid-on action (`{code, value}`, e.g. a light's `work_mode` or a fan's switch), after its own
//! delay. A grid change while a delay runs cancels it. Holds the saved settings (a file in the
//! app support folder, never the keys), the Tuya connection and each device's last result, and
//! builds the Automation pane's part of `settings-state`. Failures show only there.
//! Tauri-free apart from its spawn: the shell passes hooks in.

use crate::clock::LocalTs;
use crate::rules::GridState;
use crate::tuya::{self, Action, Client, Dev, Keys, TuyaError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::async_runtime::{self, JoinHandle};

/// A saved device. `choices` come from its `/functions` (kept so Settings can show the row
/// without a call); `off` / `on` are among them. `delay_s` waits after the grid change, both ways.
/// `enabled`: the row's own switch (Change 3); off, the device never changes with the grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub choices: Vec<Action>,
    pub off: Action,
    pub on: Action,
    #[serde(default)]
    pub delay_s: u32,
    #[serde(default)]
    pub enabled: bool,
}

/// The settings file. Never the Tuya keys (they live in the Keychain).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    /// Before Change 3 one switch covered every device: read once into each device's `enabled`, never written again.
    #[serde(rename = "automation_on", skip_serializing)]
    legacy_on: Option<bool>,
    pub devices: Vec<Device>,
    /// The Settings pane shown last ("general" / "automation").
    pub pane: Option<String>,
    /// Settings › General › Low battery alert, in % (unset: `rules::LOW_BATTERY_DEFAULT`).
    pub low_battery_pct: Option<u8>,
    /// Settings › General › Show temperatures (unset: on, T-015).
    pub show_temps: Option<bool>,
    /// Settings › General › Show today's power cuts (unset: on, T-020).
    pub show_cuts: Option<bool>,
}

impl Store {
    pub fn load(path: &std::path::Path) -> Store {
        let mut s: Store = std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        if let Some(on) = s.legacy_on.take() {
            s.devices.iter_mut().for_each(|d| d.enabled = on);
        }
        s
    }
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).expect("store serialises"))?;
        std::fs::rename(tmp, path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Which {
    Off,
    On,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    KeysRejected,
    PlanExpired,
}

/// The grid now (live, known, not stale): which state, the station's time and how long it has been in it.
#[derive(Debug, Clone, Copy)]
pub struct GridNow {
    pub which: Which,
    pub at: LocalTs,
    pub for_s: i64,
}

#[derive(Debug, Clone, PartialEq)]
enum Devices {
    Loading,
    Failed,
    Ok(Vec<Dev>),
}

struct Last {
    action: Action,
    at: LocalTs,
    err: Option<String>,
}

/// One device's state this run (resets at launch).
#[derive(Default)]
struct Run {
    /// Waiting out its delay: the action and when it will go.
    waiting: Option<(Action, LocalTs)>,
    /// The waiting change a grid flip dropped (its action, the flip time, the new direction):
    /// the waiting line's note until the new change begins.
    cancelled: Option<(Action, LocalTs, Which)>,
    /// A command in flight.
    setting: Option<Action>,
    offline: bool,
    last: Option<Last>,
    /// Bumped by every new change: an older one's result is dropped.
    gen: u64,
    task: Option<JoinHandle<()>>,
}

struct State {
    store: Store,
    keys: Option<Keys>,
    client: Option<Arc<Client>>,
    problem: Option<Problem>,
    devices: Devices,
    /// Devices loading their choices (Add Device…).
    pending: Vec<Dev>,
    add_err: Option<String>,
    run: HashMap<String, Run>,
    /// Connect in flight (generation, so a cancelled one drops its result).
    connecting: Option<u64>,
    connect_gen: u64,
    connect_err: Option<String>,
    /// The form's Access ID and data center from the last Connect attempt (never the Secret).
    form: Option<(String, String)>,
    /// One delay second (tests shorten it).
    second: Duration,
}

pub struct Hooks {
    /// Something Settings shows changed.
    pub changed: Box<dyn Fn() + Send + Sync>,
    /// One log line (never ids, names or keys).
    pub log: Box<dyn Fn(&str) + Send + Sync>,
    /// Some while the grid is known now (live On or Off, not stale).
    pub grid_now: Box<dyn Fn() -> Option<GridNow> + Send + Sync>,
}

pub struct Automation {
    st: Mutex<State>,
    hooks: Hooks,
    path: Option<PathBuf>,
}

/// A new device's actions. A light (`work_mode`): grid off Music, else its first mode; grid on
/// Colour, else White, else the first. A switch: grid off On, grid on Off. Else the first choice.
/// None: it offers nothing to switch.
fn defaults(c: &[Action]) -> Option<(Action, Action)> {
    let find = |code: &str, v: &str| c.iter().find(|a| a.code == code && a.value == v).cloned();
    if let Some(first) = c.iter().find(|a| a.code == "work_mode") {
        let off = find("work_mode", "music").unwrap_or_else(|| first.clone());
        let on = find("work_mode", "colour").or_else(|| find("work_mode", "white")).unwrap_or_else(|| first.clone());
        return Some((off, on));
    }
    if let Some(sw) = c.iter().find(|a| a.code.starts_with("switch") && a.value.is_boolean()) {
        let code = sw.code.clone();
        return Some((Action { code: code.clone(), value: Value::Bool(true) }, Action { code, value: Value::Bool(false) }));
    }
    c.first().map(|a| (a.clone(), a.clone()))
}

/// Act right now? Only while the switch is on and the change is for the grid's state now
/// (T-021: a Grid on change acts at once while the grid is on, as a Grid off one does while it is off).
pub fn set_now(now: Option<Which>, on: bool, which: Which) -> bool {
    on && now == Some(which)
}

/// What is left of a device's delay when the grid has been off for `off_for_s` already.
pub fn delay_left(delay_s: u32, off_for_s: i64) -> u32 {
    (delay_s as i64 - off_for_s.max(0)).max(0) as u32
}

/// The intro line's helper (H1–H5) and whether the device switches are disabled. None = H6 (the page's
/// default). `all_on`: every device's switch is on.
pub fn hint(keys: bool, devices: usize, signed_in: bool, problem: Option<Problem>, all_on: bool, off_now: bool) -> (bool, Option<&'static str>) {
    if !keys {
        return (true, Some("Connect Tuya and add a device first."));
    }
    if devices == 0 {
        return (true, Some("Add a device first."));
    }
    let h = if !signed_in {
        "Sign in to SEMS Portal to follow the grid."
    } else if problem == Some(Problem::KeysRejected) {
        "Paused until Tuya accepts the keys again."
    } else if problem == Some(Problem::PlanExpired) {
        "Devices can’t change until the Tuya cloud plan is extended."
    } else if !all_on && off_now {
        "The grid is off now: turning a device on sets its Grid off choice."
    } else {
        return (false, None);
    };
    (false, Some(h))
}

/// The Connect form's error line for a failed Connect (spec › Connect errors).
pub fn connect_error(e: &TuyaError, list_call: bool, dc: &str) -> String {
    match e {
        TuyaError::Code(2007) => wrong_dc(dc),
        // The token works on any data center; the list there answers 28841107 (seen live, EU keys on US).
        TuyaError::Code(1106 | 28841107) if list_call => wrong_dc(dc),
        TuyaError::Code(28841002) => "This Tuya cloud plan has expired. Extend it on platform.tuya.com, then connect again.".into(),
        TuyaError::Network | TuyaError::Timeout => "Can’t reach Tuya. Check your internet connection.".into(),
        e if !list_call || e.keys_refused() => "Tuya didn’t accept this Access ID and Secret.".into(),
        TuyaError::Code(c) => format!("Tuya answered with error {c}."),
        TuyaError::BadReply => "Tuya answered with error 0.".into(),
    }
}
fn wrong_dc(dc: &str) -> String {
    format!("This project isn’t in the {dc} data center. Pick the one your cloud project uses.")
}
const KEYS_NOT_SAVED: &str = "Couldn’t save the keys in your Keychain.";
const NO_DEVICES: &str = "No devices in this project. Link your Smart Life app to it, then connect again.";
const KEYS_REJECTED: &str = "Tuya didn’t accept the saved keys. Enter the Secret again.";
const PLAN_EXPIRED: &str = "Your Tuya cloud plan has expired. Extend it on platform.tuya.com. Your keys stay the same.";

/// `work_mode=music`, `switch_fan=true`: what the log says (no id, no name).
fn log_action(a: &Action) -> String {
    match &a.value {
        Value::String(s) => format!("{}={s}", a.code),
        v => format!("{}={v}", a.code),
    }
}

// ---------- what the Settings page reads (the Automation pane's part of `settings-state`) ----------

#[derive(Debug, Serialize)]
pub struct View {
    pub automation: AutoView,
    pub tuya: TuyaView,
    pub devices: Vec<DeviceView>,
    pub add: AddView,
    #[serde(rename = "addErr", skip_serializing_if = "Option::is_none")]
    pub add_err: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AutoView {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<&'static str>,
}

#[derive(Debug, Default, Serialize)]
pub struct TuyaView {
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<&'static str>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub busy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub err: Option<String>,
    /// Refills the form: Access ID and data center, never the Secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<FormValues>,
}

#[derive(Debug, Serialize)]
pub struct FormValues {
    pub id: String,
    pub dc: String,
}

#[derive(Debug, Default, Serialize)]
pub struct DeviceView {
    pub name: String,
    /// Labels, in Tuya's order; `off` / `on` are positions in it.
    pub choices: Vec<String>,
    pub off: usize,
    pub on: usize,
    pub delay_s: u32,
    /// The row's switch.
    pub enabled: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub loading: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub offline: bool,
    /// "Setting Music…" (in flight, spinner).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setting: Option<String>,
    /// "Will set to On at 9:09 AM", plus "· On cancelled, the grid came back at 9:08 AM" after a flip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last: Option<LastView>,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct LastView {
    pub text: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum AddView {
    Items(Vec<AddItem>),
    /// "loading" / "failed"
    Status(&'static str),
}

#[derive(Debug, PartialEq, Serialize)]
pub struct AddItem {
    pub name: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub offline: bool,
}

/// The status line under a device, first match wins: in flight, waiting, offline, last result.
/// `choices` are the device's, for the labels.
fn status(run: Option<&Run>, choices: &[Action]) -> Status {
    let lbl = |a: &Action| tuya::choice_label(choices, a);
    let Some(r) = run else { return Status::default() };
    if let Some(a) = &r.setting {
        return Status { setting: Some(format!("Setting {}…", lbl(a))), ..Status::default() };
    }
    if let Some((a, at)) = &r.waiting {
        let mut w = format!("Will set to {} at {}", lbl(a), at.hhmm12());
        if let Some((old, flip, which)) = &r.cancelled {
            let how = if *which == Which::On { "the grid came back" } else { "the grid went off again" };
            w += &format!(" · {} cancelled, {how} at {}", lbl(old), flip.hhmm12());
        }
        return Status { waiting: Some(w), ..Status::default() };
    }
    if r.offline {
        return Status { offline: true, ..Status::default() };
    }
    let last = r.last.as_ref().map(|l| match &l.err {
        Some(why) => LastView { text: format!("Couldn’t set {} at {}: {why}", lbl(&l.action), l.at.hhmm12()), failed: true },
        None => LastView { text: format!("Set to {} at {}", lbl(&l.action), l.at.hhmm12()), failed: false },
    });
    Status { last, ..Status::default() }
}

#[derive(Debug, Default, PartialEq)]
struct Status {
    setting: Option<String>,
    waiting: Option<String>,
    offline: bool,
    last: Option<LastView>,
}

impl State {
    /// Devices not yet in the list (nor loading), by name: the Add Device… items.
    fn addable(&self) -> Vec<&Dev> {
        let Devices::Ok(devs) = &self.devices else { return Vec::new() };
        let mut v: Vec<&Dev> =
            devs.iter().filter(|d| !self.store.devices.iter().any(|l| l.id == d.id) && !self.pending.iter().any(|p| p.id == d.id)).collect();
        v.sort_by_key(|d| d.name.to_lowercase());
        v
    }
    /// Commands may go out: connected, keys not refused (each device also needs its switch on).
    fn runs(&self) -> bool {
        self.client.is_some() && self.problem != Some(Problem::KeysRejected)
    }
}

impl Automation {
    pub fn new(path: Option<PathBuf>, hooks: Hooks) -> Arc<Automation> {
        let store = path.as_deref().map(Store::load).unwrap_or_default();
        Arc::new(Automation {
            st: Mutex::new(State {
                store,
                keys: None,
                client: None,
                problem: None,
                devices: Devices::Loading,
                pending: Vec::new(),
                add_err: None,
                run: HashMap::new(),
                connecting: None,
                connect_gen: 0,
                connect_err: None,
                form: None,
                second: Duration::from_secs(1),
            }),
            hooks,
            path,
        })
    }

    fn save(&self, st: &State) {
        if let Some(p) = &self.path {
            if let Err(e) = st.store.save(p) {
                (self.hooks.log)(&format!("settings: not saved: {}", e.kind()));
            }
        }
    }

    /// The keys from the Keychain at launch (None: never connected, or access denied).
    pub fn set_keys(&self, keys: Option<Keys>) {
        let client = keys.as_ref().and_then(Client::new).map(Arc::new);
        self.set_client(keys, client);
    }

    fn set_client(&self, keys: Option<Keys>, client: Option<Arc<Client>>) {
        {
            let mut st = self.st.lock().unwrap();
            st.keys = keys;
            st.client = client;
        }
        (self.hooks.changed)();
    }

    pub fn pane(&self) -> Option<String> {
        self.st.lock().unwrap().store.pane.clone()
    }
    pub fn set_pane(&self, pane: &str) {
        let mut st = self.st.lock().unwrap();
        st.store.pane = Some(pane.to_owned());
        self.save(&st);
    }
    pub fn low_battery_pct(&self) -> u8 {
        self.st.lock().unwrap().store.low_battery_pct.map_or(crate::rules::LOW_BATTERY_DEFAULT, crate::rules::low_battery_pct)
    }
    pub fn show_temps(&self) -> bool {
        self.st.lock().unwrap().store.show_temps.unwrap_or(true)
    }
    pub fn set_show_temps(&self, on: bool) {
        let mut st = self.st.lock().unwrap();
        st.store.show_temps = Some(on);
        self.save(&st);
    }
    pub fn show_cuts(&self) -> bool {
        self.st.lock().unwrap().store.show_cuts.unwrap_or(true)
    }
    pub fn set_show_cuts(&self, on: bool) {
        let mut st = self.st.lock().unwrap();
        st.store.show_cuts = Some(on);
        self.save(&st);
    }
    /// Saves the setting, kept in range; returns the value saved.
    pub fn set_low_battery_pct(&self, pct: u8) -> u8 {
        let mut st = self.st.lock().unwrap();
        let pct = crate::rules::low_battery_pct(pct);
        st.store.low_battery_pct = Some(pct);
        self.save(&st);
        pct
    }

    /// The hook: the grid change that sent the notification (never the first reading, never
    /// Unknown). Each device waits its delay and acts in its own task; this returns at once.
    /// A change still waiting or in flight from the change before is cancelled.
    pub fn grid_changed(self: &Arc<Self>, grid: GridState, at: LocalTs) {
        let which = match grid {
            GridState::Off => Which::Off,
            GridState::On => Which::On,
            GridState::Unknown => return,
        };
        let mut st = self.st.lock().unwrap();
        if !st.runs() {
            return;
        }
        let todo: Vec<(String, Action, u32)> = st
            .store
            .devices
            .iter()
            .filter(|d| d.enabled)
            .map(|d| (d.id.clone(), if which == Which::Off { d.off.clone() } else { d.on.clone() }, d.delay_s))
            .collect();
        for (id, a, delay) in todo {
            self.send(&mut st, &id, a, at, delay, Some(which));
        }
        drop(st);
        (self.hooks.changed)();
    }

    /// Starts one device's change after `wait_s`; a newer one cancels the older for that device.
    /// `flip`: a grid change in that direction (its dropped waiting change gets the note).
    fn send(self: &Arc<Self>, st: &mut State, id: &str, a: Action, at: LocalTs, wait_s: u32, flip: Option<Which>) {
        let Some(client) = st.client.clone() else { return };
        let (n, total) = (st.store.devices.iter().position(|d| d.id == id).map_or(0, |i| i + 1), st.store.devices.len());
        let wait = st.second * wait_s;
        let run = st.run.entry(id.to_owned()).or_default();
        run.gen += 1;
        if let Some(t) = run.task.take() {
            t.abort();
        }
        run.cancelled = match (flip, run.waiting.take()) {
            (Some(w), Some((old, _))) if wait_s > 0 => Some((old, at, w)),
            _ => None,
        };
        let at = at.plus_secs(wait_s as i64);
        (run.waiting, run.setting) = if wait_s == 0 { (None, Some(a.clone())) } else { (Some((a.clone(), at)), None) };
        let (me, id, gen) = (self.clone(), id.to_owned(), run.gen);
        run.task = Some(async_runtime::spawn(async move {
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
                if !me.begin(&id, gen) {
                    return;
                }
            }
            let t0 = Instant::now();
            let r = client.send(&id, &a).await;
            let head = format!("tuya: device {n} of {total} → {}", log_action(&a));
            me.finish(&id, gen, a, at, r, head, t0.elapsed().as_secs_f32());
        }));
    }

    /// The delay is over: waiting → in flight. False when a newer change took over.
    fn begin(&self, id: &str, gen: u64) -> bool {
        {
            let mut st = self.st.lock().unwrap();
            let Some(run) = st.run.get_mut(id).filter(|r| r.gen == gen) else { return false };
            run.setting = run.waiting.take().map(|w| w.0);
            run.cancelled = None;
        }
        (self.hooks.changed)();
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(&self, id: &str, gen: u64, action: Action, at: LocalTs, r: Result<(), TuyaError>, log_head: String, secs: f32) {
        let line = match &r {
            Ok(()) => format!("{log_head}: ok ({secs:.1} s)"),
            Err(e) => format!("{log_head}: failed {}", e.short()),
        };
        (self.hooks.log)(&line);
        {
            let mut st = self.st.lock().unwrap();
            let problem = st.problem;
            let Some(run) = st.run.get_mut(id).filter(|r| r.gen == gen) else { return };
            run.setting = None;
            run.task = None;
            let new_problem = match &r {
                Ok(()) => {
                    run.offline = false;
                    run.last = Some(Last { action, at, err: None });
                    // A call went through: a plan-expired note is out of date.
                    problem.filter(|p| *p != Problem::PlanExpired)
                }
                Err(e) => {
                    if e.code() == Some(2001) {
                        run.offline = true;
                    }
                    run.last = Some(Last { action, at, err: Some(e.reason()) });
                    if e.keys_refused() {
                        Some(Problem::KeysRejected)
                    } else if e.plan_expired() {
                        Some(Problem::PlanExpired)
                    } else {
                        problem
                    }
                }
            };
            st.problem = new_problem;
        }
        (self.hooks.changed)();
    }

    /// A device's switch. Turning it on gives it the action for the grid's state now, after what is
    /// left of its delay; turning it off drops its change still waiting (one in flight finishes).
    pub fn set_on(self: &Arc<Self>, device: usize, on: bool) {
        let now = (self.hooks.grid_now)();
        let mut st = self.st.lock().unwrap();
        let Some(d) = st.store.devices.get_mut(device) else { return };
        d.enabled = on;
        let (id, delay) = (d.id.clone(), d.delay_s);
        let action = now.map(|n| if n.which == Which::Off { d.off.clone() } else { d.on.clone() });
        self.save(&st);
        if let (Some(n), Some(a), true, true) = (now, action, on, st.runs()) {
            self.send(&mut st, &id, a, n.at, delay_left(delay, n.for_s), None);
        }
        if let Some(r) = st.run.get_mut(&id).filter(|r| !on && r.waiting.is_some()) {
            r.gen += 1;
            (r.waiting, r.cancelled) = (None, None);
            if let Some(t) = r.task.take() {
                t.abort();
            }
        }
        drop(st);
        (self.hooks.changed)();
    }

    /// A device's Grid off / Grid on choice: `choice` is a position in its choices.
    pub fn set_action(self: &Arc<Self>, device: usize, which: Which, choice: usize) {
        let now = (self.hooks.grid_now)();
        let mut st = self.st.lock().unwrap();
        let Some(d) = st.store.devices.get_mut(device) else { return };
        let Some(a) = d.choices.get(choice).cloned() else { return };
        match which {
            Which::Off => d.off = a.clone(),
            Which::On => d.on = a.clone(),
        }
        let (id, delay, on) = (d.id.clone(), d.delay_s, d.enabled);
        self.save(&st);
        if let (Some(n), true) = (now, st.runs()) {
            if set_now(Some(n.which), on, which) {
                self.send(&mut st, &id, a, n.at, delay_left(delay, n.for_s), None);
            }
        }
        drop(st);
        (self.hooks.changed)();
    }

    /// A device's delay in seconds (0 = at once). Applies from the next change.
    pub fn set_delay(&self, device: usize, delay_s: u32) {
        {
            let mut st = self.st.lock().unwrap();
            let Some(d) = st.store.devices.get_mut(device) else { return };
            d.delay_s = delay_s;
            self.save(&st);
        }
        (self.hooks.changed)();
    }

    /// Remove: at once, no confirmation; its change waiting or in flight is dropped.
    pub fn remove(&self, device: usize) {
        {
            let mut st = self.st.lock().unwrap();
            if device >= st.store.devices.len() {
                return;
            }
            let d = st.store.devices.remove(device);
            if let Some(t) = st.run.remove(&d.id).and_then(|r| r.task) {
                t.abort();
            }
            self.save(&st);
        }
        (self.hooks.changed)();
    }

    /// Add Device…: `item` is the position in the view's `add` list. Loads its choices, then
    /// saves it with its switch on, and gives it the action for the grid's state now.
    pub fn add(self: &Arc<Self>, item: usize) {
        let (dev, client) = {
            let mut st = self.st.lock().unwrap();
            let Some(dev) = st.addable().get(item).map(|d| (*d).clone()) else { return };
            let Some(client) = st.client.clone() else { return };
            st.pending.push(dev.clone());
            st.add_err = None;
            (dev, client)
        };
        (self.hooks.changed)();
        let me = self.clone();
        async_runtime::spawn(async move {
            let r = client.choices(&dev.id).await;
            let now = (me.hooks.grid_now)();
            {
                let mut st = me.st.lock().unwrap();
                st.pending.retain(|p| p.id != dev.id);
                match r {
                    Ok(choices) => match defaults(&choices) {
                        Some((off, on)) => {
                            let action = now.map(|n| if n.which == Which::Off { off.clone() } else { on.clone() });
                            st.store.devices.push(Device { id: dev.id.clone(), name: dev.name.clone(), choices, off, on, delay_s: 0, enabled: true });
                            st.run.entry(dev.id.clone()).or_default().offline = !dev.online;
                            me.save(&st);
                            if let (Some(n), Some(a), true) = (now, action, st.runs()) {
                                me.send(&mut st, &dev.id, a, n.at, 0, None);
                            }
                        }
                        None => st.add_err = Some(format!("{} has nothing to switch.", dev.name)),
                    },
                    // 2009: no standard functions (an RF fan, seen live).
                    Err(TuyaError::Code(2009)) => st.add_err = Some(format!("{} has nothing to switch.", dev.name)),
                    Err(e) => {
                        st.add_err = Some(format!("Couldn’t load what {} can do: {}.", dev.name, e.reason()));
                        Self::note_problem(&mut st, &e);
                    }
                }
            }
            (me.hooks.changed)();
        });
    }

    fn note_problem(st: &mut State, e: &TuyaError) {
        if e.keys_refused() {
            st.problem = Some(Problem::KeysRejected);
        } else if e.plan_expired() {
            st.problem = Some(Problem::PlanExpired);
        }
    }

    /// The device list (Automation pane shown): Add Device… items, saved names, online flags.
    pub fn refresh_devices(self: &Arc<Self>) {
        let Some(client) = self.st.lock().unwrap().client.clone() else { return };
        let me = self.clone();
        async_runtime::spawn(async move {
            let r = client.devices().await;
            {
                let mut st = me.st.lock().unwrap();
                match r {
                    Ok(devs) => {
                        let mut renamed = false;
                        let st = &mut *st;
                        for d in &devs {
                            if let Some(l) = st.store.devices.iter_mut().find(|l| l.id == d.id) {
                                renamed |= l.name != d.name;
                                l.name = d.name.clone();
                                st.run.entry(d.id.clone()).or_default().offline = !d.online;
                            }
                        }
                        if renamed {
                            me.save(st);
                        }
                        st.devices = Devices::Ok(devs);
                        if st.problem == Some(Problem::PlanExpired) {
                            st.problem = None;
                        }
                    }
                    Err(e) => {
                        st.devices = Devices::Failed;
                        Self::note_problem(&mut st, &e);
                    }
                }
            }
            (me.hooks.changed)();
        });
    }

    /// Connect: a token, then the device list. Ok → the caller saves the keys to the Keychain.
    /// Err → the form's error line. `cancel_connect` (window closed, pane switched) drops it.
    pub async fn connect(self: &Arc<Self>, keys: Keys) -> Result<(), String> {
        let Some(client) = Client::new(&keys) else { return Err("Pick the data center your cloud project uses.".into()) };
        self.connect_with(keys, client).await
    }

    async fn connect_with(self: &Arc<Self>, keys: Keys, client: Client) -> Result<(), String> {
        let gen = {
            let mut st = self.st.lock().unwrap();
            st.connect_gen += 1;
            st.connecting = Some(st.connect_gen);
            st.connect_err = None;
            st.form = Some((keys.access_id.clone(), keys.region.clone()));
            st.connect_gen
        };
        (self.hooks.changed)();
        let dc = keys.region.clone();
        let r = match client.token().await {
            Err(e) => Err(connect_error(&e, false, &dc)),
            Ok(_) => match client.devices().await {
                Err(e) => Err(connect_error(&e, true, &dc)),
                Ok(d) if d.is_empty() => Err(NO_DEVICES.to_owned()),
                Ok(d) => Ok(d),
            },
        };
        let out = {
            let mut st = self.st.lock().unwrap();
            if st.connecting != Some(gen) {
                return Err(String::new()); // cancelled: nothing saved, nothing shown
            }
            st.connecting = None;
            match r {
                Ok(devs) => {
                    st.keys = Some(keys);
                    st.client = Some(Arc::new(client));
                    st.devices = Devices::Ok(devs);
                    st.problem = None;
                    st.form = None;
                    Ok(())
                }
                Err(e) => {
                    st.connect_err = Some(e.clone());
                    Err(e)
                }
            }
        };
        (self.hooks.changed)();
        out
    }

    pub fn cancel_connect(&self) {
        let was = self.st.lock().unwrap().connecting.take().is_some();
        if was {
            (self.hooks.changed)();
        }
    }

    /// Connect worked but the Keychain refused the keys: back to the form with its error, Access ID and data
    /// center kept, so the pane never says the keys are saved when the next launch would lose them (B-Y2).
    pub fn keys_not_saved(&self) {
        {
            let mut st = self.st.lock().unwrap();
            st.form = st.keys.take().map(|k| (k.access_id, k.region));
            st.client = None;
            st.devices = Devices::Loading;
            st.connect_err = Some(KEYS_NOT_SAVED.to_owned());
        }
        (self.hooks.changed)();
    }

    /// Disconnect: forget the keys and the devices. The caller deletes the Keychain item.
    pub fn disconnect(&self) {
        {
            let mut st = self.st.lock().unwrap();
            for t in st.run.drain().filter_map(|(_, r)| r.task) {
                t.abort();
            }
            st.keys = None;
            st.client = None;
            st.problem = None;
            st.devices = Devices::Loading;
            st.pending.clear();
            st.add_err = None;
            st.connect_err = None;
            st.form = None;
            st.store.devices.clear();
            self.save(&st);
        }
        (self.hooks.changed)();
    }

    /// The Automation pane's part of `settings-state`. Never a device id or the Secret.
    pub fn view(&self, signed_in: bool) -> View {
        let off_now = (self.hooks.grid_now)().is_some_and(|n| n.which == Which::Off);
        let st = self.st.lock().unwrap();
        let (disabled, hint) = hint(st.keys.is_some(), st.store.devices.len(), signed_in, st.problem, st.store.devices.iter().all(|d| d.enabled), off_now);
        let rejected = st.problem == Some(Problem::KeysRejected);
        let tuya = match (&st.keys, rejected) {
            (Some(k), false) => TuyaView {
                connected: true,
                region: Some(k.region.clone()),
                problem: (st.problem == Some(Problem::PlanExpired)).then_some(PLAN_EXPIRED),
                ..TuyaView::default()
            },
            _ => {
                let form = st.form.clone().or_else(|| st.keys.as_ref().map(|k| (k.access_id.clone(), k.region.clone())));
                TuyaView {
                    busy: st.connecting.is_some(),
                    err: st.connect_err.clone().or_else(|| rejected.then(|| KEYS_REJECTED.to_owned())),
                    values: form.map(|(id, dc)| FormValues { id, dc }),
                    ..TuyaView::default()
                }
            }
        };
        let mut devices: Vec<DeviceView> = st
            .store
            .devices
            .iter()
            .map(|d| {
                let Status { setting, waiting, offline, last } = status(st.run.get(&d.id), &d.choices);
                let pos = |a: &Action| d.choices.iter().position(|c| c == a).unwrap_or(0);
                DeviceView {
                    name: d.name.clone(),
                    choices: d.choices.iter().map(|a| tuya::choice_label(&d.choices, a)).collect(),
                    off: pos(&d.off),
                    on: pos(&d.on),
                    delay_s: d.delay_s,
                    enabled: d.enabled,
                    setting,
                    waiting,
                    offline,
                    last,
                    ..DeviceView::default()
                }
            })
            .collect();
        devices.extend(st.pending.iter().map(|d| DeviceView { name: d.name.clone(), loading: true, ..DeviceView::default() }));
        let add = match (&st.devices, st.problem) {
            (_, Some(_)) | (Devices::Failed, _) => AddView::Status("failed"),
            (Devices::Loading, _) => AddView::Status("loading"),
            (Devices::Ok(_), _) => AddView::Items(st.addable().iter().map(|d| AddItem { name: d.name.clone(), offline: !d.online }).collect()),
        };
        View { automation: AutoView { disabled, hint }, tuya, devices, add, add_err: st.add_err.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::Monitor;
    use crate::sems::{GridSignal, Reading};
    use crate::tuya::stub::{self, fail, ok, token, Log};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

    const AT: &str = "2026-10-18 09:52:00";
    const GRID_ON: i64 = -2;

    struct T {
        auto: Arc<Automation>,
        log: Log,
        lines: Arc<Mutex<Vec<String>>>,
        /// The grid is off now, for this many seconds; GRID_ON; else (< 0) Unknown or stale.
        off_for: Arc<AtomicI64>,
    }

    impl T {
        /// Every device's switch, one by one (what the old global switch did).
        fn set_all(&self, on: bool) {
            let n = self.auto.st.lock().unwrap().store.devices.len();
            (0..n).for_each(|i| self.auto.set_on(i, on));
        }
        /// < 0: Unknown or stale.
        fn grid_off_for(&self, s: i64) {
            self.off_for.store(s, Ordering::SeqCst);
        }
        fn grid_on(&self) {
            self.off_for.store(GRID_ON, Ordering::SeqCst);
        }
    }

    fn act(code: &str, value: Value) -> Action {
        Action { code: code.into(), value }
    }
    fn light(id: &str, name: &str) -> Device {
        let mut choices = vec![act("switch_led", json!(true)), act("switch_led", json!(false))];
        choices.extend(["white", "colour", "scene", "music"].map(|m| act("work_mode", json!(m))));
        let (off, on) = defaults(&choices).unwrap();
        Device { id: id.into(), name: name.into(), choices, off, on, delay_s: 0, enabled: false }
    }
    fn fan(id: &str, name: &str) -> Device {
        let choices = vec![act("switch_fan", json!(true)), act("switch_fan", json!(false)), act("fan_speed_enum", json!("level_1"))];
        let (off, on) = defaults(&choices).unwrap();
        Device { id: id.into(), name: name.into(), choices, off, on, delay_s: 0, enabled: false }
    }

    fn keys() -> Keys {
        Keys { access_id: "the-access-id".into(), secret: "the-secret-value".into(), region: "Central Europe".into() }
    }

    /// An automation on a stub Tuya, with `devices` saved and each device's switch `on`. A delay second is 50 ms.
    async fn setup(devices: Vec<Device>, on: bool, answer: impl Fn(&stub::Req, usize) -> stub::Answer + Send + Sync + 'static) -> T {
        let (base, log) = stub::start(answer).await;
        let lines: Arc<Mutex<Vec<String>>> = Arc::default();
        let off_for = Arc::new(AtomicI64::new(-1));
        let (l2, o2) = (lines.clone(), off_for.clone());
        let auto = Automation::new(
            None,
            Hooks {
                changed: Box::new(|| {}),
                log: Box::new(move |s| l2.lock().unwrap().push(s.to_owned())),
                grid_now: Box::new(move || {
                    let (s, at) = (o2.load(Ordering::SeqCst), LocalTs::parse(AT).unwrap());
                    match s {
                        GRID_ON => Some(GridNow { which: Which::On, at, for_s: i64::MAX }),
                        s if s >= 0 => Some(GridNow { which: Which::Off, at, for_s: s }),
                        _ => None,
                    }
                }),
            },
        );
        {
            let mut st = auto.st.lock().unwrap();
            st.store.devices = devices.into_iter().map(|d| Device { enabled: on, ..d }).collect();
            st.second = Duration::from_millis(50);
        }
        let mut c = Client::with_base(base, &keys());
        c.retry_wait = Duration::from_millis(10);
        auto.set_client(Some(keys()), Some(Arc::new(c)));
        T { auto, log, lines, off_for }
    }

    /// Answers like Tuya: token, then `cmd` for every command, a list and choices for reads.
    fn tuya(cmd: impl Fn(&stub::Req) -> stub::Answer + Send + Sync + 'static) -> impl Fn(&stub::Req, usize) -> stub::Answer + Send + Sync + 'static {
        move |r, _| {
            if r.path.starts_with("/v1.0/token") {
                (0, token("t"))
            } else if r.path.ends_with("/functions") {
                (0, ok(r#"{"functions":[{"code":"switch_fan","type":"Boolean","values":"{}"},{"code":"countdown_fan","type":"Integer","values":"{}"}]}"#))
            } else if r.path.contains("associated-users") {
                (0, ok(r#"{"devices":[{"id":"dev-a","name":"Table LED","category":"dd","online":false},{"id":"dev-b","name":"Fan","category":"fskg","online":true}],"has_more":false}"#))
            } else {
                cmd(r)
            }
        }
    }

    /// (device id, "code=value") per command sent, in order.
    fn commands(log: &Log) -> Vec<(String, String)> {
        log.lock()
            .unwrap()
            .iter()
            .filter(|r| r.path.ends_with("/commands"))
            .map(|r| {
                let v: Value = serde_json::from_str(&r.body).unwrap();
                let c = &v["commands"][0];
                (r.path.split('/').nth(3).unwrap().to_owned(), log_action(&act(c["code"].as_str().unwrap(), c["value"].clone())))
            })
            .collect()
    }
    fn cmd(id: &str, a: &str) -> (String, String) {
        (id.into(), a.into())
    }

    /// Waits until no device has a change waiting or in flight (or 3 s).
    async fn settle(a: &Automation) {
        for _ in 0..300 {
            {
                let st = a.st.lock().unwrap();
                if st.run.values().all(|r| r.setting.is_none() && r.waiting.is_none()) && st.pending.is_empty() {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("still in flight");
    }

    fn at() -> LocalTs {
        LocalTs::parse(AT).unwrap()
    }

    #[test]
    fn new_device_defaults() {
        let l = light("a", "A");
        assert_eq!((l.off.clone(), l.on.clone()), (act("work_mode", json!("music")), act("work_mode", json!("colour"))));
        let f = fan("b", "B");
        assert_eq!((f.off, f.on), (act("switch_fan", json!(true)), act("switch_fan", json!(false))));
        assert_eq!(defaults(&[act("option", json!("swing"))]), Some((act("option", json!("swing")), act("option", json!("swing")))));
        assert_eq!(defaults(&[]), None);
    }

    #[tokio::test]
    async fn automation_skips_first_reading() {
        // The first reading after launch starts from Unknown: no notice, so no hook call.
        let mut m = Monitor::new(true);
        let r = |t: &str, g| Reading { time: LocalTs::parse(t).unwrap(), grid: g, vac1: 0.0, grid_status: None, battery: None, utc_offset_h: None, inverter_c: None, battery_c: None, created: None };
        let now = std::time::Instant::now();
        assert!(m.on_reading(r("2026-10-18 09:50:00", GridSignal::Off), now).is_none());
        let n = m.on_reading(r("2026-10-18 09:52:00", GridSignal::On), now).unwrap();
        assert_eq!(n.grid, GridState::On);
        // And the hook itself ignores Unknown.
        let t = setup(vec![light("dev-a", "Table LED")], true, tuya(|_| (0, ok("true")))).await;
        t.auto.grid_changed(GridState::Unknown, at());
        settle(&t.auto).await;
        assert!(commands(&t.log).is_empty());
    }

    #[tokio::test]
    async fn automation_runs_on_change_in_parallel() {
        // A light and a fan: each gets its own action, both at the same time.
        let t = setup(vec![light("dev-a", "Table LED"), fan("dev-b", "Fan")], true, tuya(|_| (300, ok("true")))).await;
        t.auto.token_warm().await;
        let t0 = Instant::now();
        t.auto.grid_changed(GridState::Off, at());
        settle(&t.auto).await;
        assert!(t0.elapsed() < Duration::from_millis(550), "{:?}", t0.elapsed());
        let mut c = commands(&t.log);
        c.sort();
        assert_eq!(c, [cmd("dev-a", "work_mode=music"), cmd("dev-b", "switch_fan=true")]);
        t.auto.grid_changed(GridState::On, at());
        settle(&t.auto).await;
        let mut c = commands(&t.log)[2..].to_vec();
        c.sort();
        assert_eq!(c, [cmd("dev-a", "work_mode=colour"), cmd("dev-b", "switch_fan=false")]);
        let v = t.auto.view(true);
        assert_eq!(v.devices[0].last, Some(LastView { text: "Set to Colour at 9:52\u{a0}AM".into(), failed: false }));
        assert_eq!(v.devices[1].last.as_ref().unwrap().text, "Set to Off at 9:52\u{a0}AM");
    }

    #[tokio::test]
    async fn delay_waits_then_acts() {
        // 4 "seconds" (50 ms each) for the fan; the light goes at once.
        let mut f = fan("dev-b", "Fan");
        f.delay_s = 4;
        let t = setup(vec![light("dev-a", "Table LED"), f], true, tuya(|_| (0, ok("true")))).await;
        t.auto.grid_changed(GridState::Off, at());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(commands(&t.log), [cmd("dev-a", "work_mode=music")]);
        assert_eq!(t.auto.view(true).devices[1].waiting.as_deref(), Some("Will set to On at 9:52\u{a0}AM"));
        settle(&t.auto).await;
        assert_eq!(commands(&t.log)[1], cmd("dev-b", "switch_fan=true"));
        // The time is when it went: 4 s after the change.
        assert_eq!(t.auto.view(true).devices[1].last.as_ref().unwrap().text, "Set to On at 9:52\u{a0}AM");
        assert_eq!(delay_left(300, 120), 180);
        assert_eq!(delay_left(60, 120), 0);
        assert_eq!(delay_left(60, -5), 60);
    }

    #[tokio::test]
    async fn grid_flip_cancels_pending_delay() {
        // The grid goes off and comes back inside the fan's delay: it never switches on; the
        // grid-on action then waits its own delay and goes.
        let mut f = fan("dev-b", "Fan");
        f.delay_s = 4;
        let t = setup(vec![f], true, tuya(|_| (0, ok("true")))).await;
        t.auto.grid_changed(GridState::Off, at());
        tokio::time::sleep(Duration::from_millis(80)).await;
        let flip = LocalTs::parse("2026-10-18 09:53:00").unwrap();
        t.auto.grid_changed(GridState::On, flip);
        // While Off waits: the note says what was dropped and why.
        assert_eq!(t.auto.view(true).devices[0].waiting.as_deref(), Some("Will set to Off at 9:53\u{a0}AM · On cancelled, the grid came back at 9:53\u{a0}AM"));
        settle(&t.auto).await;
        assert_eq!(commands(&t.log), [cmd("dev-b", "switch_fan=false")]);
        assert_eq!(t.auto.view(true).devices[0].last.as_ref().unwrap().text, "Set to Off at 9:53\u{a0}AM", "the note is gone once it went");
        assert_eq!(t.lines.lock().unwrap().len(), 1, "the cancelled one never ran");
    }

    #[tokio::test]
    async fn automation_off_sends_nothing() {
        let t = setup(vec![light("dev-a", "Table LED")], false, tuya(|_| (0, ok("true")))).await;
        t.auto.grid_changed(GridState::Off, at());
        t.auto.grid_changed(GridState::On, at());
        settle(&t.auto).await;
        assert!(commands(&t.log).is_empty());
        // Not connected: nothing either.
        let t = setup(vec![light("dev-a", "Table LED")], true, tuya(|_| (0, ok("true")))).await;
        t.auto.set_client(None, None);
        t.auto.grid_changed(GridState::Off, at());
        settle(&t.auto).await;
        assert!(commands(&t.log).is_empty());
    }

    #[tokio::test]
    async fn automation_never_blocks_notification() {
        // Tuya never answers (the 10 s timeout case): the hook still returns at once, so
        // after_poll's alert (sent before it) and the poll go on.
        let t = setup(vec![light("dev-a", "Table LED")], true, tuya(|_| (u64::MAX, String::new()))).await;
        let t0 = Instant::now();
        t.auto.grid_changed(GridState::Off, at());
        assert!(t0.elapsed() < Duration::from_millis(50), "{:?}", t0.elapsed());
        assert_eq!(t.auto.view(true).devices[0].setting.as_deref(), Some("Setting Music…"));
    }

    #[tokio::test]
    async fn turn_on_while_grid_off_sets_off_modes() {
        // Off for 2 "seconds": the fan's 6 s delay has 4 left; the light goes at once.
        let mut f = fan("dev-b", "Fan");
        f.delay_s = 6;
        let t = setup(vec![light("dev-a", "Table LED"), f], false, tuya(|_| (0, ok("true")))).await;
        t.grid_off_for(2);
        t.set_all(true);
        assert_eq!(t.auto.view(true).devices[1].waiting.as_deref(), Some("Will set to On at 9:52\u{a0}AM"));
        let t0 = Instant::now();
        settle(&t.auto).await;
        assert!(t0.elapsed() >= Duration::from_millis(180), "{:?}", t0.elapsed());
        let mut c = commands(&t.log);
        c.sort();
        assert_eq!(c, [cmd("dev-a", "work_mode=music"), cmd("dev-b", "switch_fan=true")]);
    }

    #[tokio::test]
    async fn turn_on_while_grid_unknown_sends_nothing() {
        let t = setup(vec![light("dev-a", "Table LED")], false, tuya(|_| (0, ok("true")))).await;
        t.set_all(true); // grid Unknown or stale: grid_now is None
        settle(&t.auto).await;
        assert!(commands(&t.log).is_empty());
        assert!(t.auto.view(true).devices[0].enabled);
    }

    #[tokio::test]
    async fn settings_change_while_grid_on_sets_it_now() {
        // T-021: Asif changed the Table LED's Grid on choice with the grid on, and nothing was sent.
        let mut f = fan("dev-b", "Fan");
        f.delay_s = 60; // the grid came back long ago: no wait
        let t = setup(vec![light("dev-a", "Table LED"), f], false, tuya(|_| (0, ok("true")))).await;
        t.grid_on();
        t.auto.set_on(0, true); // its Grid on action, Colour
        settle(&t.auto).await;
        t.auto.set_action(0, Which::On, 5); // Music: the current state's choice
        settle(&t.auto).await;
        t.auto.set_action(0, Which::Off, 2); // the other state's choice: nothing now
        t.auto.set_on(1, true); // the fan's Grid on action, Off, at once
        settle(&t.auto).await;
        assert_eq!(commands(&t.log), [cmd("dev-a", "work_mode=colour"), cmd("dev-a", "work_mode=music"), cmd("dev-b", "switch_fan=false")]);
        assert_eq!(t.auto.view(true).devices[0].last.as_ref().unwrap().text, "Set to Music at 9:52\u{a0}AM");
        // A device switched off sends nothing for a change.
        t.auto.set_on(0, false);
        t.auto.set_action(0, Which::On, 3);
        settle(&t.auto).await;
        assert_eq!(commands(&t.log).len(), 3);
    }

    #[tokio::test]
    async fn turn_off_drops_waiting() {
        // The fan waits 4 "seconds"; the switch goes off meanwhile: nothing is sent, no line.
        let mut f = fan("dev-b", "Fan");
        f.delay_s = 4;
        let t = setup(vec![f], true, tuya(|_| (0, ok("true")))).await;
        t.auto.grid_changed(GridState::Off, at());
        assert!(t.auto.view(true).devices[0].waiting.is_some());
        t.set_all(false);
        assert!(t.auto.view(true).devices[0].waiting.is_none());
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(commands(&t.log).is_empty());
        assert!(t.lines.lock().unwrap().is_empty());
        // A command already in flight finishes.
        let t = setup(vec![light("dev-a", "Table LED")], true, tuya(|_| (150, ok("true")))).await;
        t.auto.grid_changed(GridState::Off, at());
        t.set_all(false);
        settle(&t.auto).await;
        assert_eq!(commands(&t.log), [cmd("dev-a", "work_mode=music")]);
        assert_eq!(t.auto.view(true).devices[0].last.as_ref().unwrap().text, "Set to Music at 9:52\u{a0}AM");
    }

    #[tokio::test]
    async fn set_now_waits_rest_of_delay() {
        // Off for 2 s, delay 6 s: the switch turned on now waits only 4 s, from the grid change.
        let mut f = fan("dev-b", "Fan");
        f.delay_s = 6;
        let t = setup(vec![f], false, tuya(|_| (0, ok("true")))).await;
        t.grid_off_for(2);
        let t0 = Instant::now();
        t.set_all(true);
        // Its time: 9:52:00 (the grid change, grid_now.at) + the 4 s left.
        assert_eq!(t.auto.st.lock().unwrap().run["dev-b"].waiting.as_ref().unwrap().1, LocalTs::parse("2026-10-18 09:52:04").unwrap());
        settle(&t.auto).await;
        let e = t0.elapsed();
        assert!(e >= Duration::from_millis(190) && e < Duration::from_millis(290), "{e:?}");
        // Off longer than the delay: at once.
        let t = setup(vec![fan("dev-b", "Fan")], false, tuya(|_| (0, ok("true")))).await;
        t.grid_off_for(600);
        t.set_all(true);
        assert_eq!(t.auto.view(true).devices[0].setting.as_deref(), Some("Setting On…"));
    }

    #[test]
    fn waiting_line_text() {
        let at = LocalTs::parse("2026-10-18 09:10:00").unwrap();
        let flip = LocalTs::parse("2026-10-18 09:08:00").unwrap();
        let fan = [act("switch_fan", json!(true)), act("switch_fan", json!(false))];
        let w = |cancelled| Run { waiting: Some((fan[1].clone(), at)), cancelled, ..Run::default() };
        assert_eq!(status(Some(&w(None)), &fan).waiting.unwrap(), "Will set to Off at 9:10\u{a0}AM");
        assert_eq!(status(Some(&w(Some((fan[0].clone(), flip, Which::On)))), &fan).waiting.unwrap(), "Will set to Off at 9:10\u{a0}AM · On cancelled, the grid came back at 9:08\u{a0}AM");
        assert_eq!(status(Some(&w(Some((fan[0].clone(), flip, Which::Off)))), &fan).waiting.unwrap(), "Will set to Off at 9:10\u{a0}AM · On cancelled, the grid went off again at 9:08\u{a0}AM");
        // A gang switch says which gang.
        let gang = [act("switch_1", json!(true)), act("switch_1", json!(false)), act("switch_2", json!(true)), act("switch_2", json!(false))];
        let r = Run { waiting: Some((gang[1].clone(), at)), ..Run::default() };
        assert_eq!(status(Some(&r), &gang).waiting.unwrap(), "Will set to Switch 1 Off at 9:10\u{a0}AM");
    }

    #[tokio::test]
    async fn turn_off_sends_nothing() {
        let t = setup(vec![light("dev-a", "Table LED")], true, tuya(|_| (0, ok("true")))).await;
        t.grid_off_for(0);
        t.set_all(false);
        settle(&t.auto).await;
        assert!(commands(&t.log).is_empty());
    }

    #[tokio::test]
    async fn add_light_while_grid_off_sets_it() {
        let t = setup(vec![], true, tuya(|_| (0, ok("true")))).await;
        t.auto.refresh_devices();
        for _ in 0..100 {
            if matches!(t.auto.view(true).add, AddView::Items(_)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Any device, sorted by name; offline ones marked.
        assert_eq!(t.auto.view(true).add, AddView::Items(vec![AddItem { name: "Fan".into(), offline: false }, AddItem { name: "Table LED".into(), offline: true }]));
        t.grid_off_for(30);
        t.auto.add(0);
        assert!(t.auto.view(true).devices[0].loading);
        settle(&t.auto).await;
        let v = t.auto.view(true);
        assert_eq!(v.devices[0].name, "Fan");
        assert_eq!(v.devices[0].choices, ["On", "Off"]); // the countdown number is not offered
        assert_eq!((v.devices[0].off, v.devices[0].on, v.devices[0].delay_s), (0, 1, 0));
        assert_eq!(commands(&t.log), [cmd("dev-b", "switch_fan=true")]);
        // Grid unknown or stale: adding sends nothing.
        t.grid_off_for(-1);
        t.auto.add(0);
        settle(&t.auto).await;
        assert_eq!(commands(&t.log).len(), 1);
        // The list said the Table LED is offline: its row says so until a command works.
        assert!(t.auto.view(true).devices[1].offline);
    }

    #[tokio::test]
    async fn add_while_grid_on_sets_its_grid_on_action() {
        // T-021: adding acts at once in both grid states.
        let t = setup(vec![], true, tuya(|_| (0, ok("true")))).await;
        t.auto.refresh_devices();
        for _ in 0..100 {
            if matches!(t.auto.view(true).add, AddView::Items(_)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        t.grid_on();
        t.auto.add(0);
        settle(&t.auto).await;
        assert_eq!(commands(&t.log), [cmd("dev-b", "switch_fan=false")]);
    }

    #[tokio::test]
    async fn add_device_with_nothing_to_switch() {
        let t = setup(vec![], true, |r, _| {
            (0, if r.path.starts_with("/v1.0/token") {
                token("t")
            } else if r.path.contains("associated-users") {
                ok(r#"{"devices":[{"id":"dev-c","name":"IR Fan","category":"rf_fan_light","online":true}],"has_more":false}"#)
            } else {
                fail(2009) // seen live on an RF fan: no standard functions
            })
        })
        .await;
        t.auto.refresh_devices();
        tokio::time::sleep(Duration::from_millis(100)).await;
        t.auto.add(0);
        settle(&t.auto).await;
        let v = t.auto.view(true);
        assert!(v.devices.is_empty());
        assert_eq!(v.add_err.as_deref(), Some("IR Fan has nothing to switch."));
    }

    #[tokio::test]
    async fn change_off_mode_while_grid_off_sets_it() {
        let t = setup(vec![light("dev-a", "Table LED")], true, tuya(|_| (0, ok("true")))).await;
        t.grid_off_for(0);
        t.auto.set_action(0, Which::On, 2); // Grid on never acts now
        t.auto.set_action(0, Which::Off, 4); // Scene
        t.auto.set_action(0, Which::Off, 99); // not one of its choices: ignored
        t.auto.set_delay(0, 30);
        settle(&t.auto).await;
        assert_eq!(commands(&t.log), [cmd("dev-a", "work_mode=scene")]);
        let st = t.auto.st.lock().unwrap();
        let d = &st.store.devices[0];
        assert_eq!((d.off.clone(), d.on.clone(), d.delay_s), (act("work_mode", json!("scene")), act("work_mode", json!("white")), 30));
    }

    #[test]
    fn set_now_rule() {
        assert!(set_now(Some(Which::Off), true, Which::Off));
        assert!(set_now(Some(Which::On), true, Which::On));
        assert!(!set_now(Some(Which::Off), true, Which::On));
        assert!(!set_now(Some(Which::On), true, Which::Off));
        assert!(!set_now(None, true, Which::Off));
        assert!(!set_now(Some(Which::Off), false, Which::Off));
    }

    #[tokio::test]
    async fn newer_change_cancels_older_command() {
        // The Music command hangs; the grid comes back; Colour wins and the line says so.
        let t = setup(vec![light("dev-a", "Table LED")], true, tuya(|r| (if r.body.contains("music") { u64::MAX } else { 0 }, ok("true")))).await;
        t.auto.grid_changed(GridState::Off, at());
        tokio::time::sleep(Duration::from_millis(100)).await;
        t.auto.grid_changed(GridState::On, at());
        settle(&t.auto).await;
        let v = t.auto.view(true);
        assert_eq!(v.devices[0].last.as_ref().unwrap().text, "Set to Colour at 9:52\u{a0}AM");
        assert!(t.auto.st.lock().unwrap().run["dev-a"].task.is_none());
        assert_eq!(t.lines.lock().unwrap().len(), 1, "the cancelled one never finishes");
    }

    #[tokio::test]
    async fn keys_rejected_keeps_switch_value() {
        let t = setup(vec![light("dev-a", "Table LED")], true, tuya(|_| (0, fail(1010)))).await;
        t.auto.grid_changed(GridState::Off, at());
        settle(&t.auto).await;
        let v = t.auto.view(true);
        assert!(v.devices[0].enabled && !v.automation.disabled);
        assert_eq!(v.automation.hint, Some("Paused until Tuya accepts the keys again."));
        assert!(!v.tuya.connected);
        assert_eq!(v.tuya.err.as_deref(), Some(KEYS_REJECTED));
        let f = v.tuya.values.unwrap();
        assert_eq!((f.id.as_str(), f.dc.as_str()), ("the-access-id", "Central Europe"));
        assert_eq!(v.devices.len(), 1);
        assert_eq!(v.devices[0].last.as_ref().unwrap().text, "Couldn’t set Music at 9:52\u{a0}AM: Tuya refused the keys");
        assert_eq!(v.add, AddView::Status("failed"));
        // Paused: the next change sends nothing.
        let n = commands(&t.log).len();
        t.auto.grid_changed(GridState::On, at());
        settle(&t.auto).await;
        assert_eq!(commands(&t.log).len(), n);
    }

    #[tokio::test]
    async fn success_clears_offline() {
        let flip = Arc::new(AtomicBool::new(false));
        let f2 = flip.clone();
        let t = setup(vec![light("dev-a", "Table LED")], true, tuya(move |_| (0, if f2.load(Ordering::SeqCst) { ok("true") } else { fail(2001) }))).await;
        t.auto.grid_changed(GridState::Off, at());
        settle(&t.auto).await;
        let v = t.auto.view(true);
        assert!(v.devices[0].offline && v.devices[0].last.is_none(), "L1 wins over L2");
        flip.store(true, Ordering::SeqCst);
        t.auto.grid_changed(GridState::On, at());
        settle(&t.auto).await;
        let v = t.auto.view(true);
        assert!(!v.devices[0].offline);
        assert_eq!(v.devices[0].last.as_ref().unwrap().text, "Set to Colour at 9:52\u{a0}AM");
    }

    #[test]
    fn status_line_texts() {
        let at = LocalTs::parse("2026-10-18 21:05:00").unwrap();
        let music = || act("work_mode", json!("music"));
        let modes: Vec<Action> = ["white", "colour", "scene", "music", "scene_2"].map(|m| act("work_mode", json!(m))).into();
        let mk = |setting: Option<Action>, offline, last: Option<Last>| Run { setting, offline, last, ..Run::default() };
        let ok_last = || Some(Last { action: music(), at, err: None });
        let failed = |why: &str| Some(Last { action: act("work_mode", json!("scene_2")), at, err: Some(why.into()) });
        let st = |r: &Run| status(Some(r), &modes);
        // In flight wins over all, then waiting, then offline, then the last result; else nothing.
        assert_eq!(st(&mk(Some(music()), true, ok_last())), Status { setting: Some("Setting Music…".into()), ..Status::default() });
        let fan = [act("switch_fan", json!(true)), act("switch_fan", json!(false))];
        let waiting = Run { waiting: Some((fan[0].clone(), at)), offline: true, ..Run::default() };
        assert_eq!(status(Some(&waiting), &fan), Status { waiting: Some("Will set to On at 9:05\u{a0}PM".into()), ..Status::default() });
        assert_eq!(st(&mk(None, true, failed("offline"))), Status { offline: true, ..Status::default() });
        assert_eq!(st(&mk(None, false, ok_last())).last.unwrap().text, "Set to Music at 9:05\u{a0}PM");
        let l2 = st(&mk(None, false, failed("Tuya was busy"))).last.unwrap();
        assert_eq!((l2.text.as_str(), l2.failed), ("Couldn’t set Scene 2 at 9:05\u{a0}PM: Tuya was busy", true));
        let fan_off = Run { last: Some(Last { action: fan[1].clone(), at, err: None }), ..Run::default() };
        assert_eq!(status(Some(&fan_off), &fan).last.unwrap().text, "Set to Off at 9:05\u{a0}PM");
        assert_eq!(status(None, &fan), Status::default());
        assert_eq!(status(Some(&Run::default()), &fan), Status::default());
        // The reasons table.
        let reasons = [
            (TuyaError::Code(2001), "offline"),
            (TuyaError::Code(2008), "not supported"),
            (TuyaError::Code(1106), "not in your Tuya project"),
            (TuyaError::Code(1010), "Tuya refused the keys"),
            (TuyaError::Code(1011), "Tuya refused the keys"),
            (TuyaError::Code(1004), "Tuya refused the keys"),
            (TuyaError::Code(28841002), "plan expired"),
            (TuyaError::Code(28841004), "monthly limit used up"),
            (TuyaError::Code(28841104), "monthly limit used up"),
            (TuyaError::Code(1199), "Tuya was busy"),
            (TuyaError::Code(1110), "Tuya was busy"),
            (TuyaError::Network, "no connection"),
            (TuyaError::Timeout, "no answer"),
            (TuyaError::Code(1234), "error 1234"),
        ];
        for (e, r) in reasons {
            assert_eq!(e.reason(), r, "{e:?}");
        }
        // Connect errors.
        let dc = "Central Europe";
        assert_eq!(connect_error(&TuyaError::Code(1004), false, dc), "Tuya didn’t accept this Access ID and Secret.");
        assert_eq!(connect_error(&TuyaError::Code(2009), false, dc), "Tuya didn’t accept this Access ID and Secret.");
        assert_eq!(connect_error(&TuyaError::Code(2007), false, dc), wrong_dc(dc));
        assert_eq!(connect_error(&TuyaError::Code(28841107), true, dc), wrong_dc(dc));
        assert_eq!(connect_error(&TuyaError::Code(1106), true, dc), "This project isn’t in the Central Europe data center. Pick the one your cloud project uses.");
        assert_eq!(connect_error(&TuyaError::Code(28841002), true, dc), "This Tuya cloud plan has expired. Extend it on platform.tuya.com, then connect again.");
        assert_eq!(connect_error(&TuyaError::Timeout, false, dc), "Can’t reach Tuya. Check your internet connection.");
        assert_eq!(connect_error(&TuyaError::Code(1500), true, dc), "Tuya answered with error 1500.");
    }

    #[test]
    fn automation_helper_texts() {
        assert_eq!(hint(false, 0, true, None, true, false), (true, Some("Connect Tuya and add a device first.")));
        assert_eq!(hint(true, 0, true, None, true, false), (true, Some("Add a device first.")));
        assert_eq!(hint(true, 1, false, None, true, true), (false, Some("Sign in to SEMS Portal to follow the grid.")));
        assert_eq!(hint(true, 1, true, Some(Problem::KeysRejected), true, true), (false, Some("Paused until Tuya accepts the keys again.")));
        assert_eq!(hint(true, 1, true, Some(Problem::PlanExpired), true, true), (false, Some("Devices can’t change until the Tuya cloud plan is extended.")));
        assert_eq!(hint(true, 1, true, None, false, true), (false, Some("The grid is off now: turning a device on sets its Grid off choice.")));
        assert_eq!(hint(true, 1, true, None, true, true), (false, None)); // H6
        assert_eq!(hint(true, 1, true, None, false, false), (false, None));
    }

    #[tokio::test]
    async fn settings_state_never_has_secret_or_device_id() {
        let t = setup(vec![light("dev-a", "Table LED")], true, tuya(|_| (0, fail(1010)))).await;
        let a = serde_json::to_string(&t.auto.view(true)).unwrap();
        t.auto.refresh_devices();
        t.auto.grid_changed(GridState::Off, at());
        settle(&t.auto).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let b = serde_json::to_string(&t.auto.view(true)).unwrap(); // keys-rejected form, values refilled
        assert!(b.contains("the-access-id"), "the form keeps the Access ID: {b}");
        for s in [a, b] {
            for bad in ["the-secret-value", "dev-a", "dev-b", "secret"] {
                assert!(!s.contains(bad), "{bad} in {s}");
            }
        }
    }

    #[test]
    fn settings_file_has_no_secret() {
        let dir = std::env::temp_dir().join(format!("gw-store-{}", std::process::id()));
        let p = dir.join("settings.json");
        let mut f = fan("<device_id>", "Fan");
        f.delay_s = 300;
        let s = Store { devices: vec![light("<device_id>", "Table LED"), f], pane: Some("automation".into()), ..Store::default() };
        s.save(&p).unwrap();
        let raw = std::fs::read_to_string(&p).unwrap();
        for bad in ["secret", "access_id", "region", "local_key"] {
            assert!(!raw.contains(bad), "{bad}");
        }
        // Round trip, delay and pane included (the pane is restored on the next open).
        assert_eq!(Store::load(&p), s);
        assert_eq!(Store::load(&dir.join("missing.json")), Store::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn settings_from_before_change_3_migrate() {
        // The old file: one global switch, devices without their own. On → every device on; off → every device off.
        let dir = std::env::temp_dir().join(format!("gw-store-mig-{}", std::process::id()));
        let p = dir.join("settings.json");
        for on in [true, false] {
            let mut old = serde_json::to_value(Store { devices: vec![light("<device_id>", "Table LED"), fan("<device_id>", "Fan")], ..Store::default() }).unwrap();
            old["devices"].as_array_mut().unwrap().iter_mut().for_each(|d| drop(d.as_object_mut().unwrap().remove("enabled")));
            old["automation_on"] = json!(on);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&p, old.to_string()).unwrap();
            let s = Store::load(&p);
            assert!(s.devices.iter().all(|d| d.enabled == on), "{on}");
            // Saved again: the old key is gone, each device keeps its own.
            s.save(&p).unwrap();
            let raw = std::fs::read_to_string(&p).unwrap();
            assert!(!raw.contains("automation_on") && raw.matches(&format!("\"enabled\": {on}")).count() == 2, "{raw}");
            assert_eq!(Store::load(&p), s);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn device_off_never_acts_and_drops_waiting() {
        // Light on, fan off: a grid change moves only the light.
        let t = setup(vec![light("dev-a", "Table LED"), fan("dev-b", "Fan")], true, tuya(|_| (0, ok("true")))).await;
        t.auto.set_on(1, false);
        t.auto.grid_changed(GridState::Off, at());
        settle(&t.auto).await;
        assert_eq!(commands(&t.log), [cmd("dev-a", "work_mode=music")]);
        let v = t.auto.view(true);
        assert!(v.devices[0].enabled && !v.devices[1].enabled);
        // A device waiting out its delay, switched off: its change is dropped, the other's still goes.
        let mut f = fan("dev-b", "Fan");
        f.delay_s = 4;
        let mut l = light("dev-a", "Table LED");
        l.delay_s = 4;
        let t = setup(vec![l, f], true, tuya(|_| (0, ok("true")))).await;
        t.auto.grid_changed(GridState::Off, at());
        t.auto.set_on(1, false);
        assert!(t.auto.view(true).devices[1].waiting.is_none());
        assert!(t.auto.view(true).devices[0].waiting.is_some());
        settle(&t.auto).await;
        assert_eq!(commands(&t.log), [cmd("dev-a", "work_mode=music")]);
        // While the grid is off, turning one device on sets only that one.
        let t = setup(vec![light("dev-a", "Table LED"), fan("dev-b", "Fan")], false, tuya(|_| (0, ok("true")))).await;
        t.grid_off_for(600);
        t.auto.set_on(1, true);
        settle(&t.auto).await;
        assert_eq!(commands(&t.log), [cmd("dev-b", "switch_fan=true")]);
        assert_eq!(t.auto.view(true).automation.hint, Some("The grid is off now: turning a device on sets its Grid off choice."));
    }

    #[tokio::test]
    async fn log_line_has_no_id_or_name() {
        let t = setup(vec![light("dev-a", "Table LED"), fan("dev-b", "Fan")], true, tuya(|r| (0, if r.path.contains("dev-b") { fail(2001) } else { ok("true") }))).await;
        t.auto.grid_changed(GridState::Off, at());
        settle(&t.auto).await;
        let mut lines = t.lines.lock().unwrap().clone();
        lines.sort();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("tuya: device 1 of 2 → work_mode=music: ok ("), "{}", lines[0]);
        assert_eq!(lines[1], "tuya: device 2 of 2 → switch_fan=true: failed 2001");
        for l in &lines {
            for bad in ["dev-", "Table", "Fan"] {
                assert!(!l.contains(bad), "{l}");
            }
        }
    }

    #[tokio::test]
    async fn connect_then_disconnect() {
        let t = setup(vec![], false, tuya(|_| (0, ok("true")))).await;
        t.auto.disconnect();
        let v = t.auto.view(true);
        assert!(!v.tuya.connected && v.automation.disabled);
        let (base, _) = stub::start(tuya(|_| (0, ok("true")))).await;
        t.auto.connect_with(keys(), Client::with_base(base, &keys())).await.unwrap();
        let v = t.auto.view(true);
        assert!(v.tuya.connected);
        assert_eq!(v.tuya.region.as_deref(), Some("Central Europe"));
        assert_eq!(v.automation.hint, Some("Add a device first."));
        assert!(matches!(v.add, AddView::Items(ref i) if i.len() == 2));
        // An empty project.
        let (base, _) = stub::start(|r, _| (0, if r.path.starts_with("/v1.0/token") { token("t") } else { ok(r#"{"devices":[],"has_more":false}"#) })).await;
        t.auto.disconnect();
        assert_eq!(t.auto.connect_with(keys(), Client::with_base(base, &keys())).await.unwrap_err(), NO_DEVICES);
        let v = t.auto.view(true);
        assert_eq!(v.tuya.err.as_deref(), Some(NO_DEVICES));
        assert!(!v.tuya.connected);
    }

    #[tokio::test]
    async fn keychain_save_failure_keeps_the_form() {
        let t = setup(vec![], false, tuya(|_| (0, ok("true")))).await;
        t.auto.keys_not_saved();
        let v = t.auto.view(true);
        assert!(!v.tuya.connected);
        assert_eq!(v.tuya.err.as_deref(), Some(KEYS_NOT_SAVED));
        assert_eq!(v.tuya.values.map(|f| f.dc), Some(keys().region));
    }

    #[tokio::test]
    async fn plan_expired_still_sends() {
        let flip = Arc::new(AtomicBool::new(false));
        let f2 = flip.clone();
        let t = setup(vec![light("dev-a", "Table LED")], true, tuya(move |_| (0, if f2.load(Ordering::SeqCst) { ok("true") } else { fail(28841002) }))).await;
        t.auto.grid_changed(GridState::Off, at());
        settle(&t.auto).await;
        let v = t.auto.view(true);
        assert!(v.tuya.connected);
        assert_eq!(v.tuya.problem, Some(PLAN_EXPIRED));
        assert_eq!(v.automation.hint, Some("Devices can’t change until the Tuya cloud plan is extended."));
        // Still tried on the next change: the first one after the plan is extended works by itself.
        flip.store(true, Ordering::SeqCst);
        t.auto.grid_changed(GridState::On, at());
        settle(&t.auto).await;
        assert_eq!(commands(&t.log), [cmd("dev-a", "work_mode=music"), cmd("dev-a", "work_mode=colour")]);
        assert_eq!(t.auto.view(true).tuya.problem, None);
    }

    impl Automation {
        /// Fetches the token first, so a timing test measures only the commands.
        async fn token_warm(&self) {
            let c = self.st.lock().unwrap().client.clone().unwrap();
            c.token().await.unwrap();
        }
    }
}
