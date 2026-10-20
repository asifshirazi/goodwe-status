//! Updates from GitHub Releases (T-022). tauri-plugin-updater reads `latest.json` from the newest
//! release (endpoint and public key in tauri.conf.json). A silent check at every launch, Check Now
//! in Settings › About, and a download, install and relaunch only when the user clicks Update and
//! Relaunch ("Ask first"). No notification ("No, just the dot"): while a newer version waits, the
//! panel gear and the About tab carry a dot (`Updates::available`).
//!
//! `Updates` is the pure state machine (tested below); `Updater` runs it on the plugin.

use crate::{clock::LocalTs, logdir, shell};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};
use tauri_plugin_updater::UpdaterExt;

/// The newer release `latest.json` announces, as About shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    /// `pub_date`, Unix seconds (UTC).
    pub date: Option<i64>,
    /// Our own `size` field in latest.json, bytes (not a standard updater field).
    pub size: Option<u64>,
    /// At most 3 bullets from `notes`.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Failure {
    /// The download broke off (network, server).
    Download,
    /// The download's signature didn't match the updater key.
    Verify,
    /// The new bundle couldn't replace this one.
    Install,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Checking,
    Current,
    Available(Release),
    /// Bytes so far and the total (Content-Length, else our `size`, else unknown: spinner).
    Downloading(Release, u64, Option<u64>),
    Installing(Release),
    Relaunching(Release),
    Failed(Release, Failure),
    Offline,
    Unreachable,
}

/// What one check found.
#[derive(Debug, Clone, PartialEq)]
pub enum Found {
    Newer(Release),
    UpToDate,
    Offline,
    Unreachable,
}

/// Kept between launches (`updates.json`): when a check last worked, and the update the app
/// relaunched into, for "Last checked …" and "Updated to 0.2.0 …".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    /// Unix ms of the last check that reached GitHub.
    pub checked: Option<i64>,
    pub updated: Option<Updated>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Updated {
    pub version: String,
    /// Unix ms.
    pub at: i64,
}

#[derive(Debug)]
pub struct Updates {
    /// The running version (Cargo.toml / tauri.conf.json).
    pub current: String,
    pub phase: Phase,
    pub saved: Saved,
}

/// The `update` part of Settings' `about` state (spec › State Rust sends). Every string is
/// formatted here except the clock ones: `checked` and `updated.at` are Unix ms, which the page
/// writes as "today at 9:41 AM" in the Mac's own time zone.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub kind: &'static str,
    /// The newer version (available → failed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The small line under the title, when it needs no clock.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<Vec<String>>,
    /// 0–100 while downloading with a known total; none: the spinner line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pct: Option<u8>,
    /// "3.1 of 7.8 MB".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
    /// Failed: why, and that the old version stays.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<i64>,
    /// Only when the running version is the one the app updated itself to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<Updated>,
}

/// "7.8 MB" (decimal megabytes, like Finder).
pub fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

/// Up to 3 bullets from the release notes: the list items when there are any, else the lines.
pub fn bullets(notes: &str) -> Vec<String> {
    let lines = || notes.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'));
    let items: Vec<String> = lines().filter_map(|l| l.strip_prefix("- ").or_else(|| l.strip_prefix("* "))).map(|l| l.trim().to_owned()).collect();
    let all = if items.is_empty() { lines().map(str::to_owned).collect() } else { items };
    all.into_iter().take(3).collect()
}

impl Updates {
    pub fn new(current: &str, saved: Saved) -> Updates {
        Updates { current: current.to_owned(), phase: Phase::Current, saved }
    }

    fn release(&self) -> Option<&Release> {
        match &self.phase {
            Phase::Available(r) | Phase::Downloading(r, ..) | Phase::Installing(r) | Phase::Relaunching(r) | Phase::Failed(r, _) => Some(r),
            _ => None,
        }
    }

    /// A newer version waits (the dot on the gear and the About tab): from available until it is
    /// installed, also while it downloads, installs, relaunches and after a failed try.
    pub fn available(&self) -> bool {
        self.release().is_some()
    }

    /// Check Now / launch. False while a check runs or an update is on its way.
    pub fn begin_check(&mut self) -> bool {
        if matches!(self.phase, Phase::Checking | Phase::Downloading(..) | Phase::Installing(_) | Phase::Relaunching(_)) {
            return false;
        }
        self.phase = Phase::Checking;
        true
    }

    /// The check's answer; `now` in Unix ms. Only an answer from GitHub moves "Last checked".
    pub fn checked(&mut self, found: Found, now: i64) {
        if !matches!(self.phase, Phase::Checking) {
            return;
        }
        if matches!(found, Found::Newer(_) | Found::UpToDate) {
            self.saved.checked = Some(now);
        }
        self.phase = match found {
            Found::Newer(r) => Phase::Available(r),
            Found::UpToDate => Phase::Current,
            Found::Offline => Phase::Offline,
            Found::Unreachable => Phase::Unreachable,
        };
    }

    /// Update and Relaunch, or Retry after a failure. False when there is nothing to download.
    pub fn begin_download(&mut self) -> bool {
        match &self.phase {
            Phase::Available(r) | Phase::Failed(r, _) => {
                let size = r.size;
                self.phase = Phase::Downloading(r.clone(), 0, size);
                true
            }
            _ => false,
        }
    }

    /// A chunk of `n` bytes arrived; `total` is the Content-Length. True when the line on screen
    /// changes (one redraw per 0.1 MB, not one per chunk).
    pub fn progress(&mut self, n: u64, total: Option<u64>) -> bool {
        let before = self.view().progress;
        if let Phase::Downloading(_, got, t) = &mut self.phase {
            *got += n;
            if total.is_some() {
                *t = total;
            }
        }
        self.view().progress != before
    }

    /// Cancel while downloading: back to available. False in any other phase.
    pub fn cancel(&mut self) -> bool {
        match &self.phase {
            Phase::Downloading(r, ..) => {
                self.phase = Phase::Available(r.clone());
                true
            }
            _ => false,
        }
    }

    /// Downloaded and verified: installing. False when the download was cancelled meanwhile.
    pub fn downloaded(&mut self) -> bool {
        match &self.phase {
            Phase::Downloading(r, ..) => {
                self.phase = Phase::Installing(r.clone());
                true
            }
            _ => false,
        }
    }

    /// The new bundle is in place: relaunching. Remembers the update for "Updated to …".
    pub fn installed(&mut self, now: i64) {
        if let Phase::Installing(r) = &self.phase {
            self.saved.updated = Some(Updated { version: r.version.clone(), at: now });
            self.phase = Phase::Relaunching(r.clone());
        }
    }

    pub fn failed(&mut self, why: Failure) {
        if let Phase::Downloading(r, ..) | Phase::Installing(r) = &self.phase {
            self.phase = Phase::Failed(r.clone(), why);
        }
    }

    pub fn view(&self) -> View {
        let cur = &self.current;
        let mut v = View {
            kind: "current",
            version: self.release().map(|r| r.version.clone()),
            detail: None,
            notes: self.release().map(|r| r.notes.clone()),
            pct: None,
            progress: None,
            reason: None,
            checked: self.saved.checked,
            updated: self.saved.updated.clone().filter(|u| &u.version == cur),
        };
        match &self.phase {
            Phase::Checking => {
                v.kind = "checking";
                v.detail = Some("Looking for a new release on GitHub.".into());
            }
            Phase::Current => {}
            Phase::Available(r) => {
                v.kind = "available";
                let mut d = format!("You have {cur}.");
                let extra: Vec<String> = [r.date.map(|s| format!("Released {}", LocalTs(s).day_month_year())), r.size.map(mb)].into_iter().flatten().collect();
                if !extra.is_empty() {
                    d = format!("{d} {}.", extra.join(" · "));
                }
                v.detail = Some(d);
            }
            Phase::Downloading(_, got, total) => {
                v.kind = "downloading";
                if let Some(t) = total.filter(|t| *t > 0) {
                    let got = (*got).min(t);
                    v.pct = Some((got * 100 / t) as u8);
                    // Both rounded to 0.1 MB, so "7.8 of 7.8 MB" only at the very end.
                    v.progress = Some(format!("{:.1} of {}", got as f64 / 1e6, mb(t)));
                }
            }
            Phase::Installing(_) => {
                v.kind = "installing";
                v.detail = Some("GoodWe Status will relaunch in a moment.".into());
            }
            Phase::Relaunching(_) => {
                v.kind = "relaunching";
                v.detail = Some("The menu bar icon comes back in a few seconds.".into());
            }
            Phase::Failed(_, why) => {
                v.kind = "failed";
                v.reason = Some(format!(
                    "{} You still have {cur}.",
                    match why {
                        Failure::Download => "The download didn’t finish.",
                        Failure::Verify => "The download couldn’t be verified.",
                        Failure::Install => "The new version couldn’t replace this one.",
                    }
                ));
            }
            Phase::Offline => {
                v.kind = "offline";
                v.detail = Some("You’re offline.".into());
            }
            Phase::Unreachable => {
                v.kind = "unreachable";
                v.detail = Some("Try again later.".into());
            }
        }
        v
    }
}

/// What a failed check means for About. An Intel Mac finds no build for its platform in
/// latest.json (Apple silicon only): up to date, not an error. So is a release without
/// latest.json: GitHub answered, nothing is published (the plugin reports any non-2xx so).
pub fn classify(e: &tauri_plugin_updater::Error) -> Found {
    use tauri_plugin_updater::Error as E;
    match e {
        // shortcut: a GitHub 5xx also reads "up to date"; read the status code if that ever misleads.
        E::TargetNotFound(_) | E::TargetsNotFound(_) | E::ReleaseNotFound => Found::UpToDate,
        // shortcut: GitHub down but the Mac online also reads "offline"; tell them apart if users get confused.
        E::Reqwest(r) if r.is_connect() || r.is_timeout() => Found::Offline,
        _ => Found::Unreachable,
    }
}

fn release(u: &tauri_plugin_updater::Update) -> Release {
    Release {
        version: u.version.clone(),
        date: u.date.map(|d| d.unix_timestamp()),
        size: u.raw_json["size"].as_u64(),
        notes: u.body.as_deref().map(bullets).unwrap_or_default(),
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

struct Inner {
    st: Updates,
    /// The plugin's answer behind `Available`, kept for Update and Relaunch / Retry.
    update: Option<tauri_plugin_updater::Update>,
    download: Option<tauri::async_runtime::JoinHandle<()>>,
}

/// Managed state: the machine plus the plugin's `Update` and the running download.
pub struct Updater {
    inner: Mutex<Inner>,
    file: Option<PathBuf>,
    log_dir: PathBuf,
}

impl Updater {
    pub fn new(app: &AppHandle, log_dir: PathBuf) -> Updater {
        let file = app.path().app_data_dir().ok().map(|d| d.join("updates.json"));
        let saved = file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let st = Updates::new(&app.package_info().version.to_string(), saved);
        Updater { inner: Mutex::new(Inner { st, update: None, download: None }), file, log_dir }
    }

    pub fn available(&self) -> bool {
        self.inner.lock().unwrap().st.available()
    }

    pub fn view(&self) -> View {
        self.inner.lock().unwrap().st.view()
    }

    fn save(&self, s: &Saved) {
        if let Some(f) = &self.file {
            let tmp = f.with_extension("tmp");
            let ok = f.parent().is_some_and(|d| std::fs::create_dir_all(d).is_ok())
                && std::fs::write(&tmp, serde_json::to_string(s).expect("saved serialises")).is_ok()
                && std::fs::rename(&tmp, f).is_ok();
            if !ok {
                logdir::line(&self.log_dir, "update: updates.json not written");
            }
        }
    }
}

/// At launch (silent) and Check Now.
pub fn check(app: &AppHandle) {
    if !app.state::<Updater>().inner.lock().unwrap().st.begin_check() {
        return;
    }
    shell::refresh(app);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let res = match app.updater() {
            Ok(u) => u.check().await,
            Err(e) => Err(e),
        };
        let u = app.state::<Updater>();
        let found = match &res {
            Ok(Some(up)) => Found::Newer(release(up)),
            Ok(None) => Found::UpToDate,
            Err(e) => {
                logdir::line(&u.log_dir, &format!("update check: {e}"));
                classify(e)
            }
        };
        let saved = {
            let mut i = u.inner.lock().unwrap();
            i.update = res.ok().flatten();
            i.st.checked(found, now_ms());
            i.st.saved.clone()
        };
        u.save(&saved);
        shell::refresh(&app);
    });
}

/// Update and Relaunch, and Retry: download (with progress), verify, install, relaunch.
pub fn install(app: &AppHandle) {
    let u = app.state::<Updater>();
    let mut i = u.inner.lock().unwrap();
    let Some(up) = i.update.clone() else { return };
    if !i.st.begin_download() {
        return;
    }
    let app2 = app.clone();
    i.download = Some(tauri::async_runtime::spawn(async move {
        let app = app2;
        let u = app.state::<Updater>();
        let res = up
            .download(
                |n, total| {
                    if u.inner.lock().unwrap().st.progress(n as u64, total) {
                        shell::refresh(&app);
                    }
                },
                || {},
            )
            .await;
        let bytes = match res {
            Ok(b) => b,
            Err(e) => {
                logdir::line(&u.log_dir, &format!("update download: {e}"));
                let why = if matches!(e, tauri_plugin_updater::Error::Minisign(_) | tauri_plugin_updater::Error::SignatureUtf8(_) | tauri_plugin_updater::Error::Base64(_)) { Failure::Verify } else { Failure::Download };
                u.inner.lock().unwrap().st.failed(why);
                return shell::refresh(&app);
            }
        };
        // Cancel may have landed after the last chunk: then nothing is installed.
        if !u.inner.lock().unwrap().st.downloaded() {
            return;
        }
        shell::refresh(&app);
        // Blocking: it unpacks and swaps the .app (and may ask for an admin password).
        match tauri::async_runtime::spawn_blocking(move || up.install(bytes)).await {
            Ok(Ok(())) => {}
            r => {
                logdir::line(&u.log_dir, &format!("update install: {}", r.map_or_else(|e| e.to_string(), |r| r.err().map(|e| e.to_string()).unwrap_or_default())));
                u.inner.lock().unwrap().st.failed(Failure::Install);
                return shell::refresh(&app);
            }
        }
        let saved = {
            let mut i = u.inner.lock().unwrap();
            i.st.installed(now_ms());
            i.st.saved.clone()
        };
        u.save(&saved);
        logdir::line(&u.log_dir, "update installed, relaunching");
        shell::refresh(&app);
        // Long enough for "Relaunching…" to show before the process goes.
        tokio::time::sleep(Duration::from_millis(800)).await;
        app.restart();
    }));
}

/// Cancel while downloading: the download is dropped (its bytes live only in memory).
pub fn cancel(app: &AppHandle) {
    let u = app.state::<Updater>();
    let mut i = u.inner.lock().unwrap();
    if i.st.cancel() {
        if let Some(t) = i.download.take() {
            t.abort();
        }
        drop(i);
        shell::refresh(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel() -> Release {
        Release { version: "0.2.0".into(), date: Some(1_792_281_600), size: Some(7_800_000), notes: vec!["About tab".into()] } // 2026-10-18
    }

    fn at(phase: Phase) -> Updates {
        Updates { current: "0.1.2".into(), phase, saved: Saved::default() }
    }

    #[test]
    fn check_answers_set_each_state() {
        let mut u = Updates::new("0.1.2", Saved::default());
        assert!(u.begin_check());
        assert_eq!(u.view().kind, "checking");
        assert_eq!(u.view().detail.as_deref(), Some("Looking for a new release on GitHub."));
        assert!(!u.begin_check(), "one check at a time");
        u.checked(Found::UpToDate, 1000);
        assert_eq!((u.view().kind, u.view().checked), ("current", Some(1000)));
        assert!(!u.available());

        for (found, kind, detail) in [(Found::Offline, "offline", "You’re offline."), (Found::Unreachable, "unreachable", "Try again later.")] {
            assert!(u.begin_check());
            u.checked(found, 2000);
            let v = u.view();
            assert_eq!((v.kind, v.detail.as_deref()), (kind, Some(detail)));
            assert_eq!(v.checked, Some(1000), "a failed check keeps the last good time");
        }

        assert!(u.begin_check());
        u.checked(Found::Newer(rel()), 3000);
        let v = u.view();
        assert_eq!((v.kind, v.version.as_deref()), ("available", Some("0.2.0")));
        assert_eq!(v.detail.as_deref(), Some("You have 0.1.2. Released 18 October 2026 · 7.8 MB."));
        assert_eq!(v.notes, Some(vec!["About tab".to_string()]));
        assert!(u.available());
    }

    #[test]
    fn available_line_without_date_or_size() {
        let r = Release { date: None, size: None, ..rel() };
        assert_eq!(at(Phase::Available(r)).view().detail.as_deref(), Some("You have 0.1.2."));
    }

    #[test]
    fn download_install_relaunch() {
        let mut u = at(Phase::Available(rel()));
        assert!(u.begin_download());
        let v = u.view();
        assert_eq!((v.kind, v.pct, v.progress.as_deref()), ("downloading", Some(0), Some("0.0 of 7.8 MB")));
        assert!(!u.begin_check(), "no check while an update is on its way");
        assert!(!u.begin_download(), "one download at a time");
        assert!(u.progress(3_100_000, Some(7_800_000)));
        assert!(!u.progress(10, Some(7_800_000)), "no redraw for a few bytes");
        let v = u.view();
        assert_eq!((v.pct, v.progress.as_deref()), (Some(39), Some("3.1 of 7.8 MB")));
        assert!(u.available(), "the dot stays while downloading");
        u.progress(4_700_000, Some(7_800_000));
        assert_eq!(u.view().pct, Some(100));
        assert!(u.downloaded());
        assert_eq!(u.view().kind, "installing");
        assert_eq!(u.view().detail.as_deref(), Some("GoodWe Status will relaunch in a moment."));
        assert!(!u.cancel(), "no cancel half-way through an install");
        u.installed(5000);
        assert_eq!(u.view().kind, "relaunching");
        assert_eq!(u.saved.updated, Some(Updated { version: "0.2.0".into(), at: 5000 }));
        assert!(u.available());
    }

    #[test]
    fn unknown_total_shows_the_spinner_line() {
        let mut u = at(Phase::Available(Release { size: None, ..rel() }));
        u.begin_download();
        u.progress(1_000_000, None);
        let v = u.view();
        assert_eq!((v.kind, v.pct, v.progress), ("downloading", None, None));
    }

    #[test]
    fn cancel_goes_back_and_a_late_download_is_not_installed() {
        let mut u = at(Phase::Available(rel()));
        u.begin_download();
        assert!(u.cancel());
        assert_eq!(u.view().kind, "available");
        assert!(!u.downloaded(), "a download finishing after Cancel installs nothing");
        assert_eq!(u.view().kind, "available");
    }

    #[test]
    fn failures_keep_the_dot_and_retry() {
        for (why, reason) in [
            (Failure::Download, "The download didn’t finish. You still have 0.1.2."),
            (Failure::Verify, "The download couldn’t be verified. You still have 0.1.2."),
            (Failure::Install, "The new version couldn’t replace this one. You still have 0.1.2."),
        ] {
            let mut u = at(Phase::Available(rel()));
            u.begin_download();
            if why == Failure::Install {
                u.downloaded();
            }
            u.failed(why);
            let v = u.view();
            assert_eq!((v.kind, v.reason.as_deref(), v.version.as_deref()), ("failed", Some(reason), Some("0.2.0")));
            assert!(u.available(), "the dot stays after a failed try");
            assert!(u.begin_download(), "Retry");
            assert_eq!(u.view().kind, "downloading");
        }
    }

    #[test]
    fn after_the_update_only_for_that_version() {
        let saved = Saved { checked: Some(1), updated: Some(Updated { version: "0.2.0".into(), at: 9 }) };
        assert_eq!(Updates::new("0.2.0", saved.clone()).view().updated, saved.updated);
        assert_eq!(Updates::new("0.2.1", saved).view().updated, None, "installed another way since");
    }

    #[test]
    fn a_late_check_answer_is_ignored() {
        let mut u = at(Phase::Available(rel()));
        u.begin_download();
        u.checked(Found::UpToDate, 1);
        assert_eq!(u.view().kind, "downloading");
    }

    #[test]
    fn notes_become_three_bullets() {
        assert_eq!(bullets("## 0.2.0\n- About tab\n- Updates\n\n* Dot\n- Fourth"), ["About tab", "Updates", "Dot"]);
        assert_eq!(bullets("One line.\nTwo."), ["One line.", "Two."]);
        assert!(bullets("").is_empty());
    }

    #[test]
    fn errors_map_to_about_rows() {
        use tauri_plugin_updater::Error as E;
        assert_eq!(classify(&E::TargetNotFound("darwin-x86_64".into())), Found::UpToDate, "Intel: no build, not an error");
        assert_eq!(classify(&E::ReleaseNotFound), Found::UpToDate, "404: no latest.json published yet");
        assert_eq!(classify(&E::UnsupportedOs), Found::Unreachable, "anything else is still an error");
    }

    #[tokio::test]
    async fn refused_connection_is_offline() {
        // Port 1 on this Mac: nothing listens, the connect fails at once (no network needed).
        let e = reqwest::get("http://127.0.0.1:1/latest.json").await.unwrap_err();
        let found = classify(&tauri_plugin_updater::Error::Reqwest(e));
        assert_eq!(found, Found::Offline);
    }

    #[test]
    fn saved_round_trips() {
        let s = Saved { checked: Some(5), updated: Some(Updated { version: "0.2.0".into(), at: 6 }) };
        assert_eq!(serde_json::from_str::<Saved>(&serde_json::to_string(&s).unwrap()).unwrap(), s);
        assert_eq!(serde_json::from_str::<Saved>("{}").unwrap(), Saved::default());
    }
}
