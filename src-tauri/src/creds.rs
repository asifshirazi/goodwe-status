//! The OS credential store (macOS Keychain): the SEMS login (email and password) and the
//! Tuya cloud keys, one item each. Read once per launch and kept in memory, so polls never
//! touch the Keychain.

use crate::sems::Credentials;
use serde_json::{json, Value};

const SERVICE: &str = "dev.goodwestatus.menubar";
const USER: &str = "sems-account";
const TUYA: &str = "tuya";

fn entry() -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, USER)
}

/// None when nothing is stored, or when the user denied Keychain access (show the sign-in box).
pub fn load() -> Option<Credentials> {
    let v: Value = serde_json::from_str(&entry().ok()?.get_password().ok()?).ok()?;
    Some(Credentials { account: v["account"].as_str()?.to_owned(), password: v["password"].as_str()?.to_owned() })
}

/// Call only after SEMS accepted the credentials.
pub fn save(c: &Credentials) -> keyring::Result<()> {
    entry()?.set_password(&json!({ "account": c.account, "password": c.password }).to_string())
}

/// Sign Out: removes the item. Nothing stored is fine.
pub fn delete() -> keyring::Result<()> {
    match entry()?.delete_credential() {
        Err(keyring::Error::NoEntry) => Ok(()),
        r => r,
    }
}

/// The Tuya keys `{access_id, secret, region}`; None when nothing is stored or access was denied.
pub fn load_tuya() -> Option<crate::tuya::Keys> {
    serde_json::from_str(&keyring::Entry::new(SERVICE, TUYA).ok()?.get_password().ok()?).ok()
}

/// Call only after Tuya accepted the keys (Connect).
pub fn save_tuya(k: &crate::tuya::Keys) -> keyring::Result<()> {
    keyring::Entry::new(SERVICE, TUYA)?.set_password(&serde_json::to_string(k).expect("keys serialise"))
}

/// Disconnect: removes the item. Nothing stored is fine.
pub fn delete_tuya() -> keyring::Result<()> {
    match keyring::Entry::new(SERVICE, TUYA)?.delete_credential() {
        Err(keyring::Error::NoEntry) => Ok(()),
        r => r,
    }
}
