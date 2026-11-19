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

/// The stored login. `signed_out`: the user signed out; it only fills in the sign-in box and
/// never signs in by itself at launch.
pub struct Stored {
    pub creds: Credentials,
    pub signed_out: bool,
}

fn parse(s: &str) -> Option<Stored> {
    let v: Value = serde_json::from_str(s).ok()?;
    let creds = Credentials { account: v["account"].as_str()?.to_owned(), password: v["password"].as_str()?.to_owned() };
    Some(Stored { creds, signed_out: v["signed_out"].as_bool().unwrap_or(false) })
}

fn write(c: &Credentials, signed_out: bool) -> keyring::Result<()> {
    entry()?.set_password(&json!({ "account": c.account, "password": c.password, "signed_out": signed_out }).to_string())
}

/// None when nothing is stored, or when the user denied Keychain access (show the sign-in box).
pub fn load() -> Option<Stored> {
    parse(&entry().ok()?.get_password().ok()?)
}

/// Call only after SEMS accepted the credentials. Replaces what is remembered.
pub fn save(c: &Credentials) -> keyring::Result<()> {
    write(c, false)
}

/// Sign Out: keeps the login for the sign-in box but marks it signed out, so a relaunch does
/// not sign in. Returns the remembered login; Ok(None) when nothing is stored.
pub fn mark_signed_out() -> keyring::Result<Option<Credentials>> {
    let s = match entry()?.get_password() {
        Err(keyring::Error::NoEntry) => return Ok(None),
        r => r?,
    };
    let Some(st) = parse(&s) else { return delete().map(|_| None) };
    write(&st.creds, true)?;
    Ok(Some(st.creds))
}

/// Removes the item (the fallback when Sign Out cannot mark it). Nothing stored is fine.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stored() {
        let s = parse(r#"{"account":"a@b.c","password":"pw"}"#).unwrap();
        assert_eq!((s.creds.account.as_str(), s.creds.password.as_str(), s.signed_out), ("a@b.c", "pw", false));
        assert!(parse(r#"{"account":"a@b.c","password":"pw","signed_out":true}"#).unwrap().signed_out);
        assert!(parse(r#"{"account":"a@b.c"}"#).is_none());
    }
}
