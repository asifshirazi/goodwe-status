//! The SEMS login in the OS credential store (macOS Keychain), one item holding email and
//! password. Read once per launch and kept in memory, so polls never touch the Keychain.

use crate::sems::Credentials;
use serde_json::{json, Value};

const SERVICE: &str = "dev.goodwestatus.menubar";
const USER: &str = "sems-account";

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
