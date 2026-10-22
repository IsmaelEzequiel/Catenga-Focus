//! Account plumbing: which server to talk to, and the token kept in the OS keychain.
//! Thin wrappers, verified by hand (tests must not touch the real keychain).

use keyring::Entry;

const DEFAULT_SERVER_URL: &str = "https://catenga-focus.fly.dev";
pub(crate) const KEYCHAIN_SERVICE: &str = "com.ismael.catengafocus";
const KEYCHAIN_USER: &str = "account-token";

/// `CATENGA_SERVER_URL` if set (dev/test override), else the production server.
/// No trailing slash.
pub fn server_url() -> String {
    let url = match std::env::var("CATENGA_SERVER_URL") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => DEFAULT_SERVER_URL.to_string(),
    };
    url.trim().trim_end_matches('/').to_string()
}

fn entry() -> Result<Entry, String> {
    Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER).map_err(|e| format!("keychain unavailable: {e}"))
}

pub fn get_token() -> Result<Option<String>, String> {
    match entry()?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("reading the keychain: {e}")),
    }
}

pub fn set_token(token: &str) -> Result<(), String> {
    entry()?
        .set_password(token)
        .map_err(|e| format!("saving to the keychain: {e}"))
}

/// Deleting a token that is not there is fine.
pub fn delete_token() -> Result<(), String> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("deleting from the keychain: {e}")),
    }
}
