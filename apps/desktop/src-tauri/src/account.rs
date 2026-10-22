//! Account plumbing: which server to talk to, and the token kept in the OS keychain.
//! Thin wrappers, verified by hand (tests must not touch the real keychain).

use keyring::Entry;

const DEFAULT_SERVER_URL: &str = "https://catenga-focus.fly.dev";
/// Dev builds (`pnpm dev`) keep their own items: macOS ties a keychain item to the build that
/// created it, so a dev login could not be removed by the installed app (or the other way round).
pub(crate) const KEYCHAIN_SERVICE: &str = if cfg!(debug_assertions) {
    "com.ismael.catengafocus.dev"
} else {
    "com.ismael.catengafocus"
};
const KEYCHAIN_USER: &str = "account-token";
/// The notes key fetched from the account (M9b); same service rules as the token.
const KEYCHAIN_NOTES_KEY: &str = "notes-key";

/// `CATENGA_SERVER_URL` if set (dev/test override), else the production server.
/// No trailing slash.
pub fn server_url() -> String {
    let url = match std::env::var("CATENGA_SERVER_URL") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => DEFAULT_SERVER_URL.to_string(),
    };
    url.trim().trim_end_matches('/').to_string()
}

/// `"<action> the keychain: …"`. macOS refuses to change an item created by a differently
/// signed build (until the app has a Developer ID every build signs differently); say how to fix it.
pub(crate) fn keychain_error(action: &str, err: &dyn std::fmt::Display) -> String {
    let text = err.to_string();
    if text.contains("change the owner") {
        return format!(
            "{action} the keychain: this item was saved by another build of Catenga Focus. \
Open Keychain Access, search \"{KEYCHAIN_SERVICE}\", delete its items and try again."
        );
    }
    format!("{action} the keychain: {text}")
}

fn entry_for(user: &str) -> Result<Entry, String> {
    Entry::new(KEYCHAIN_SERVICE, user).map_err(|e| format!("keychain unavailable: {e}"))
}

fn entry() -> Result<Entry, String> {
    entry_for(KEYCHAIN_USER)
}

/// The cached notes key in the OS keychain (`notes-key`). Thin, verified by hand.
pub struct KeychainNotesKey;

impl crate::file_sync::KeyStore for KeychainNotesKey {
    fn get(&self) -> Result<Option<String>, String> {
        match entry_for(KEYCHAIN_NOTES_KEY)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(keychain_error("reading", &e)),
        }
    }

    fn set(&self, value: &str) -> Result<(), String> {
        entry_for(KEYCHAIN_NOTES_KEY)?
            .set_password(value)
            .map_err(|e| keychain_error("saving to", &e))
    }

    fn delete(&self) -> Result<(), String> {
        match entry_for(KEYCHAIN_NOTES_KEY)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(keychain_error("deleting from", &e)),
        }
    }
}

pub fn get_token() -> Result<Option<String>, String> {
    match entry()?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(keychain_error("reading", &e)),
    }
}

pub fn set_token(token: &str) -> Result<(), String> {
    entry()?
        .set_password(token)
        .map_err(|e| keychain_error("saving to", &e))
}

/// Deleting a token that is not there is fine.
pub fn delete_token() -> Result<(), String> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(keychain_error("deleting from", &e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_errors_explain_how_to_fix_them() {
        let msg = keychain_error(
            "deleting from",
            &"Couldn't access platform storage: Invalid attempt to change the owner of this item.",
        );
        assert!(msg.contains("another build of Catenga Focus"), "{msg}");
        assert!(msg.contains("Keychain Access"), "{msg}");
        assert!(msg.contains(KEYCHAIN_SERVICE), "{msg}");
    }

    #[test]
    fn other_errors_keep_their_text() {
        assert_eq!(
            keychain_error("reading", &"boom"),
            "reading the keychain: boom"
        );
    }

    #[test]
    fn dev_builds_use_their_own_keychain_service() {
        // `cargo test` builds with debug assertions, like `pnpm dev`.
        assert_eq!(KEYCHAIN_SERVICE, "com.ismael.catengafocus.dev");
    }
}
