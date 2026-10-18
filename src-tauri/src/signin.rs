//! The sign-in box: its own small window (label `signin` in tauri.conf.json, hidden until
//! needed). SEMS checks the email and password first; only then are they saved to the
//! Keychain and handed to the poller. Cancel drops the request in flight and saves nothing.

use crate::monitor::Monitor;
use crate::sems::{Client, Credentials, SemsError};
use crate::{creds, logdir, notify, shell};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, WebviewWindow};
use tokio::sync::oneshot;

const WIN: &str = "signin";
const WIN_W: f64 = 320.0;

#[derive(Default)]
pub struct SignIn {
    /// The page has loaded and listens for `signin-open`.
    loaded: AtomicBool,
    /// An open was asked for before the page loaded (first run).
    wanted: AtomicBool,
    /// Fires Cancel on the request in flight.
    cancel: Mutex<Option<oneshot::Sender<()>>>,
    /// No credentials were stored at launch: the first sign-in turns start-at-login on.
    pub first_run: AtomicBool,
}

/// The error line for a failed sign-in, exact approved copy.
pub fn message(e: &SemsError) -> &'static str {
    match e {
        SemsError::WrongLogin(_) => "Wrong email or password. Try again.",
        // Network, or a reply we could not read: either way SEMS did not answer usefully.
        _ => "Can’t reach SEMS. Check your internet connection and try again.",
    }
}

/// Opens the box (first run, the panel's "Sign In…"). Prefills the email in memory, if any.
pub fn open(app: &AppHandle) {
    let st = app.state::<SignIn>();
    if !st.loaded.load(Ordering::SeqCst) {
        st.wanted.store(true, Ordering::SeqCst);
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let email = app.state::<Arc<tokio::sync::Mutex<Client>>>().lock().await.account();
        // The hint shows only until notification permission has been asked.
        let hint = notify::permission().await.is_none_or(|p| p == notify::Permission::NotAsked);
        let _ = app.emit_to(WIN, "signin-open", json!({ "email": email.unwrap_or_default(), "hint": hint }));
    });
}

/// The page is ready; open it now if that was asked for before it loaded.
#[tauri::command]
pub fn signin_ready(app: AppHandle) {
    let st = app.state::<SignIn>();
    st.loaded.store(true, Ordering::SeqCst);
    if st.wanted.swap(false, Ordering::SeqCst) {
        open(&app);
    }
}

/// The page is `h` tall. On open (`show`): also centre, bring forward and make key. The app
/// has no Dock icon (Accessory), so it is activated first or the keys would go elsewhere.
/// Otherwise (the error line wrapped) only the height changes, so the box doesn't jump.
#[tauri::command]
pub fn signin_show(app: AppHandle, win: WebviewWindow, h: f64, show: bool) {
    let _ = win.set_size(LogicalSize::new(WIN_W, h.ceil().clamp(100.0, 900.0)));
    if !show {
        return;
    }
    let _ = win.center();
    #[cfg(target_os = "macos")]
    let _ = app.show();
    let _ = win.show();
    let _ = win.set_focus();
}

/// Cancel or Esc: stop any request in flight, save nothing, close the box.
#[tauri::command]
pub fn signin_cancel(app: AppHandle) {
    if let Some(tx) = app.state::<SignIn>().cancel.lock().unwrap().take() {
        let _ = tx.send(());
    }
    close(&app);
}

fn close(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(WIN) {
        let _ = w.hide();
    }
    // Give the keyboard back to the app the user was in, unless the panel is up.
    #[cfg(target_os = "macos")]
    if !app.get_webview_window("panel").is_some_and(|p| p.is_visible().unwrap_or(false)) {
        let _ = app.hide();
    }
}

/// Sign In. Ok = accepted, saved and the box is closed; Err = the error line to show.
/// "cancelled" means Cancel won the race: the page ignores it.
#[tauri::command]
pub async fn signin_submit(app: AppHandle, email: String, password: String) -> Result<(), String> {
    let creds = Credentials { account: email.trim().to_owned(), password };
    let (tx, rx) = oneshot::channel();
    *app.state::<SignIn>().cancel.lock().unwrap() = Some(tx);
    // A client of its own, so a poll holding the shared one never delays the box.
    let mut fresh = Client::new(None);
    let res = tokio::select! {
        r = fresh.sign_in(creds.clone()) => r,
        _ = rx => return Err("cancelled".into()), // dropping the future drops the request
    };
    app.state::<SignIn>().cancel.lock().unwrap().take();
    let dir = app.state::<shell::Shell>().log_dir.clone();
    if let Err(e) = res {
        logdir::line(&dir, &format!("sign-in failed: {e}"));
        if let SemsError::Parse { reason, reply } = &e {
            logdir::dump_bad_reply(&dir, reason, reply);
        }
        return Err(message(&e).into());
    }
    // Accepted, and Cancel can no longer stop it (its sender is gone).
    if let Err(e) = creds::save(&creds) {
        // Keep the session anyway; the box shows again at the next launch.
        logdir::line(&dir, &format!("keychain save failed: {e}"));
    }
    let shared = app.state::<Arc<tokio::sync::Mutex<Client>>>().inner().clone();
    let mut client = shared.lock().await;
    let same_account = client.account().as_deref() == Some(creds.account.as_str());
    *client = fresh;
    drop(client);
    {
        let mon = app.state::<Arc<Mutex<Monitor>>>();
        let mut m = mon.lock().unwrap();
        if same_account { m.signed_in = true } else { m.reset(true) }
        m.account = Some(creds.account.clone());
        // The first poll after a sign-in is the user's: the Refresh icon spins.
        m.user_poll = true;
    }
    logdir::line(&dir, "signed in");
    close(&app);
    shell::refresh(&app);
    let _ = app.state::<tokio::sync::mpsc::Sender<()>>().try_send(());
    shell::ask_notifications(&app);
    if app.state::<SignIn>().first_run.swap(false, Ordering::SeqCst) {
        start_at_login(&app, &dir);
    }
    Ok(())
}

/// Turns start-at-login on once, after the first sign-in. Only from the bundled .app: a
/// login item pointing at a `cargo tauri dev` binary would be wrong.
fn start_at_login(app: &AppHandle, dir: &std::path::Path) {
    use tauri_plugin_autostart::ManagerExt;
    let bundled = std::env::current_exe().is_ok_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"));
    let al = app.autolaunch();
    if bundled && !al.is_enabled().unwrap_or(false) {
        let r = al.enable();
        logdir::line(dir, &format!("start at login: {}", if r.is_ok() { "on".to_owned() } else { format!("{r:?}") }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_lines() {
        assert_eq!(message(&SemsError::WrongLogin("x".into())), "Wrong email or password. Try again.");
        let unreachable = "Can’t reach SEMS. Check your internet connection and try again.";
        assert_eq!(message(&SemsError::Network("timeout".into())), unreachable);
        assert_eq!(message(&SemsError::Parse { reason: "r".into(), reply: String::new() }), unreachable);
    }
}
