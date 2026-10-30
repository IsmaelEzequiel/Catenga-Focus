//! Google OAuth token handling and the three Calendar REST calls we need. No Tauri;
//! tests run against a local fake server. The refresh token lives in the OS keychain,
//! the access token only in memory.

use crate::google::{self, CalEvent, CalendarInfo, SeriesInfo, TokenError, Tokens};
use jiff::tz::TimeZone;
use keyring::Entry;
use reqwest::Method;
use serde_json::Value;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::async_runtime::Mutex;

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";
const API_URL: &str = "https://www.googleapis.com/calendar/v3";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const REVOKE_TIMEOUT: Duration = Duration::from_secs(5);
const KEYCHAIN_REFRESH_USER: &str = "google-refresh-token";
/// Single-account era only; read once by the migration.
const KEYCHAIN_EMAIL_USER: &str = "google-email";
const KEYCHAIN_SCOPE_USER: &str = "google-scope";
/// JSON array of the connected account emails, in connection order.
const KEYCHAIN_ACCOUNTS_USER: &str = "google-accounts";
/// Safety net against a server that never stops paging.
const MAX_PAGES: usize = 10;

#[derive(Debug, Clone)]
pub struct Credentials {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone)]
pub struct Endpoints {
    pub token: String,
    pub revoke: String,
    /// Calendar API base, without trailing slash.
    pub api: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints {
            token: TOKEN_URL.into(),
            revoke: REVOKE_URL.into(),
            api: API_URL.into(),
        }
    }
}

#[derive(Debug)]
pub enum Error {
    NotConnected,
    /// The refresh token was rejected; it has been forgotten.
    Disconnected,
    /// The token was granted before the calendar list scope existed.
    MissingScope,
    Message(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotConnected => write!(f, "Google Calendar is not connected"),
            Error::Disconnected => write!(f, "Google Calendar was disconnected"),
            Error::MissingScope => write!(f, "Reconnect Google Calendar to load your calendars"),
            Error::Message(m) => write!(f, "{m}"),
        }
    }
}

/// Where one account's long-lived credentials are kept.
pub trait TokenStore: Send + Sync {
    fn refresh_token(&self) -> Result<Option<String>, String>;
    fn set_refresh_token(&self, token: &str) -> Result<(), String>;
    fn delete_refresh_token(&self) -> Result<(), String>;
    /// The scopes of the last token response (space separated), if known.
    fn scope(&self) -> Option<String>;
    fn set_scope(&self, scope: Option<&str>);
}

/// Tokens that only live in memory: a connection in progress, before its account is known.
#[derive(Default)]
pub struct MemoryStore {
    pub refresh: std::sync::Mutex<Option<String>>,
    scope: std::sync::Mutex<Option<String>>,
}

fn guard<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl TokenStore for MemoryStore {
    fn refresh_token(&self) -> Result<Option<String>, String> {
        Ok(guard(&self.refresh).clone())
    }
    fn set_refresh_token(&self, token: &str) -> Result<(), String> {
        *guard(&self.refresh) = Some(token.to_string());
        Ok(())
    }
    fn delete_refresh_token(&self) -> Result<(), String> {
        *guard(&self.refresh) = None;
        Ok(())
    }
    fn scope(&self) -> Option<String> {
        guard(&self.scope).clone()
    }
    fn set_scope(&self, scope: Option<&str>) {
        *guard(&self.scope) = scope.map(str::to_string);
    }
}

/// The credentials of the single-account era (`google-refresh-token`, `google-email`,
/// `google-scope`), found at startup by the migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyAccount {
    pub refresh: String,
    pub email: Option<String>,
    pub scope: Option<String>,
}

/// All accounts' credentials: the list of connected emails and one `TokenStore` each.
pub trait Vault: Send + Sync {
    /// Connected account emails, in connection order.
    fn accounts(&self) -> Vec<String>;
    fn set_accounts(&self, emails: &[String]) -> Result<(), String>;
    fn store(&self, email: &str) -> Arc<dyn TokenStore>;
    /// The single-account entries, when a refresh token is still there.
    fn legacy(&self) -> Option<LegacyAccount>;
    /// The single-account refresh token and scope as a store (until it is migrated).
    fn legacy_store(&self) -> Arc<dyn TokenStore>;
    /// Removes the single-account entries.
    fn delete_legacy(&self);
}

/// Moves the single-account credentials under `email` and removes the old entries.
/// The new entries and the list are written first: a failure leaves the legacy token
/// where it was. An account that already has a token keeps it (it was reconnected).
pub fn migrate_legacy(vault: &dyn Vault, email: &str) -> Result<(), String> {
    let Some(legacy) = vault.legacy() else {
        return Ok(());
    };
    let store = vault.store(email);
    if store.refresh_token()?.is_none() {
        store.set_refresh_token(&legacy.refresh)?;
        store.set_scope(legacy.scope.as_deref());
    }
    let mut accounts = vault.accounts();
    if !accounts.iter().any(|a| a == email) {
        accounts.push(email.to_string());
        vault.set_accounts(&accounts)?;
    }
    vault.delete_legacy();
    Ok(())
}

/// The OS keychain, same service as the app account token. Verified by hand
/// (tests must not touch the real keychain).
pub struct KeychainVault;

/// One account's entries (`<name>:<email>`), or the single-account ones (`<name>`).
pub struct KeychainStore {
    account: Option<String>,
}

fn entry(user: &str) -> Result<Entry, String> {
    Entry::new(crate::account::KEYCHAIN_SERVICE, user)
        .map_err(|e| format!("keychain unavailable: {e}"))
}

fn get_entry(user: &str) -> Result<Option<String>, String> {
    match entry(user)?.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(crate::account::keychain_error("reading", &e)),
    }
}

fn set_entry(user: &str, value: &str) -> Result<(), String> {
    entry(user)?
        .set_password(value)
        .map_err(|e| crate::account::keychain_error("saving to", &e))
}

fn delete_entry(user: &str) -> Result<(), String> {
    match entry(user)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(crate::account::keychain_error("deleting from", &e)),
    }
}

impl KeychainStore {
    fn key(&self, name: &str) -> String {
        match &self.account {
            Some(email) => format!("{name}:{email}"),
            None => name.to_string(),
        }
    }
}

impl TokenStore for KeychainStore {
    fn refresh_token(&self) -> Result<Option<String>, String> {
        get_entry(&self.key(KEYCHAIN_REFRESH_USER))
    }
    fn set_refresh_token(&self, token: &str) -> Result<(), String> {
        set_entry(&self.key(KEYCHAIN_REFRESH_USER), token)
    }
    fn delete_refresh_token(&self) -> Result<(), String> {
        delete_entry(&self.key(KEYCHAIN_REFRESH_USER))
    }
    fn scope(&self) -> Option<String> {
        get_entry(&self.key(KEYCHAIN_SCOPE_USER)).ok().flatten()
    }
    fn set_scope(&self, scope: Option<&str>) {
        let key = self.key(KEYCHAIN_SCOPE_USER);
        let _ = match scope {
            Some(s) => set_entry(&key, s),
            None => delete_entry(&key),
        };
    }
}

impl Vault for KeychainVault {
    fn accounts(&self) -> Vec<String> {
        get_entry(KEYCHAIN_ACCOUNTS_USER)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }
    fn set_accounts(&self, emails: &[String]) -> Result<(), String> {
        if emails.is_empty() {
            return delete_entry(KEYCHAIN_ACCOUNTS_USER);
        }
        let json = serde_json::to_string(emails).map_err(|e| e.to_string())?;
        set_entry(KEYCHAIN_ACCOUNTS_USER, &json)
    }
    fn store(&self, email: &str) -> Arc<dyn TokenStore> {
        Arc::new(KeychainStore {
            account: Some(email.to_string()),
        })
    }
    fn legacy(&self) -> Option<LegacyAccount> {
        let legacy = KeychainStore { account: None };
        let refresh = legacy.refresh_token().ok().flatten()?;
        Some(LegacyAccount {
            refresh,
            email: get_entry(KEYCHAIN_EMAIL_USER).ok().flatten(),
            scope: legacy.scope(),
        })
    }
    fn legacy_store(&self) -> Arc<dyn TokenStore> {
        Arc::new(KeychainStore { account: None })
    }
    fn delete_legacy(&self) {
        for name in [
            KEYCHAIN_REFRESH_USER,
            KEYCHAIN_EMAIL_USER,
            KEYCHAIN_SCOPE_USER,
        ] {
            let _ = delete_entry(name);
        }
    }
}

struct Access {
    token: String,
    expires_at_ms: i64,
}

pub struct Client {
    http: reqwest::Client,
    creds: Credentials,
    endpoints: Endpoints,
    store: Arc<dyn TokenStore>,
    /// Held across a refresh so concurrent callers share one.
    session: Mutex<Option<Access>>,
    /// What the store held at the first look, kept in step with every write the client
    /// makes, so status checks do not go to the keychain each time.
    flags: std::sync::Mutex<Option<Flags>>,
}

#[derive(Clone)]
struct Flags {
    connected: bool,
    scope: Option<String>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

fn net_err(e: reqwest::Error) -> Error {
    Error::Message(format!("couldn't reach Google: {}", e.without_url()))
}

/// Google's `{"error":{"message":…}}` text, else the HTTP status.
fn api_message(status: u16, body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| format!("Google returned HTTP {status}"))
}

fn form_body(pairs: &[(&str, &str)]) -> String {
    let mut s = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        s.append_pair(k, v);
    }
    s.finish()
}

impl Client {
    pub fn new(
        creds: Credentials,
        endpoints: Endpoints,
        store: Arc<dyn TokenStore>,
    ) -> Result<Client, String> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| format!("creating HTTP client: {e}"))?;
        Ok(Client {
            http,
            creds,
            endpoints,
            store,
            session: Mutex::new(None),
            flags: std::sync::Mutex::new(None),
        })
    }

    fn with_flags<R>(&self, f: impl FnOnce(&mut Flags) -> R) -> R {
        let mut flags = guard(&self.flags);
        f(flags.get_or_insert_with(|| Flags {
            connected: matches!(self.store.refresh_token(), Ok(Some(_))),
            scope: self.store.scope(),
        }))
    }

    fn flags(&self) -> Flags {
        self.with_flags(|f| f.clone())
    }

    fn update_flags(&self, f: impl FnOnce(&mut Flags)) {
        self.with_flags(f);
    }

    #[cfg(test)]
    pub fn is_connected(&self) -> bool {
        self.flags().connected
    }

    /// Without a token (revoked or removed), or with one that lacks the calendar list
    /// scope (granted before it existed): the user has to connect this account again.
    pub fn needs_reconnect(&self) -> bool {
        let flags = self.flags();
        !flags.connected || flags.scope.is_some_and(|s| !google::has_list_scope(&s))
    }

    fn remember_scope(&self, scope: Option<&str>) {
        self.store.set_scope(scope);
        self.update_flags(|f| f.scope = scope.map(str::to_string));
    }

    /// The tokens of a finished consent, for saving under the account once it is known.
    pub fn credentials_to_keep(&self) -> Result<(String, Option<String>), String> {
        let refresh = self
            .store
            .refresh_token()?
            .ok_or("Google Calendar is not connected")?;
        Ok((refresh, self.store.scope()))
    }

    async fn token_post(&self, pairs: &[(&str, &str)]) -> Result<Tokens, TokenError> {
        let response = self
            .http
            .post(&self.endpoints.token)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form_body(pairs))
            .send()
            .await
            .map_err(|e| {
                TokenError::Other(format!("couldn't reach Google: {}", e.without_url()))
            })?;
        let status = response.status().as_u16();
        let body = response.text().await.map_err(|e| {
            TokenError::Other(format!("couldn't reach Google: {}", e.without_url()))
        })?;
        google::parse_token_response(status, &body)
    }

    /// Finishes the consent flow: trades the code for tokens and keeps the refresh token.
    pub async fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<(), Error> {
        let tokens = self
            .token_post(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("code_verifier", verifier),
                ("client_id", &self.creds.client_id),
                ("client_secret", &self.creds.client_secret),
                ("redirect_uri", redirect_uri),
            ])
            .await
            .map_err(|e| Error::Message(e.to_string()))?;
        let refresh = tokens.refresh_token.clone().ok_or_else(|| {
            Error::Message("Google did not return a refresh token; try connecting again".into())
        })?;
        self.store
            .set_refresh_token(&refresh)
            .map_err(Error::Message)?;
        self.update_flags(|f| f.connected = true);
        if let Some(scope) = &tokens.scope {
            self.remember_scope(Some(scope));
        }
        *self.session.lock().await = Some(Access {
            token: tokens.access_token,
            expires_at_ms: now_ms() + tokens.expires_in * 1000,
        });
        Ok(())
    }

    /// A usable access token, refreshing when missing, near expiry, or `force`d (after a 401).
    async fn access_token(&self, force: bool) -> Result<String, Error> {
        let mut session = self.session.lock().await;
        if !force
            && let Some(a) = session.as_ref()
            && !google::needs_refresh(Some(a.expires_at_ms), now_ms())
        {
            return Ok(a.token.clone());
        }
        let refresh = self
            .store
            .refresh_token()
            .map_err(Error::Message)?
            .ok_or(Error::NotConnected)?;
        let result = self
            .token_post(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &refresh),
                ("client_id", &self.creds.client_id),
                ("client_secret", &self.creds.client_secret),
            ])
            .await;
        match result {
            Ok(tokens) => {
                if let Some(scope) = &tokens.scope {
                    self.remember_scope(Some(scope));
                }
                if let Some(new_refresh) = &tokens.refresh_token {
                    self.store
                        .set_refresh_token(new_refresh)
                        .map_err(Error::Message)?;
                }
                let token = tokens.access_token;
                *session = Some(Access {
                    token: token.clone(),
                    expires_at_ms: now_ms() + tokens.expires_in * 1000,
                });
                Ok(token)
            }
            Err(TokenError::InvalidGrant) => {
                *session = None;
                let _ = self.store.delete_refresh_token();
                self.remember_scope(None);
                self.update_flags(|f| f.connected = false);
                Err(Error::Disconnected)
            }
            Err(TokenError::Other(m)) => Err(Error::Message(m)),
        }
    }

    /// Authorised request; one refresh + retry on a 401. Returns the status and body of the final response.
    async fn send(
        &self,
        method: Method,
        url: &str,
        body: Option<&Value>,
    ) -> Result<(u16, String), Error> {
        for attempt in 0..2 {
            let token = self.access_token(attempt > 0).await?;
            let mut request = self.http.request(method.clone(), url).bearer_auth(token);
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = request.send().await.map_err(net_err)?;
            let status = response.status().as_u16();
            let text = response.text().await.map_err(net_err)?;
            if status == 401 && attempt == 0 {
                continue;
            }
            if status == 403 && text.contains("insufficient") && text.contains("cope") {
                return Err(Error::MissingScope);
            }
            return Ok((status, text));
        }
        Err(Error::Message("Google rejected the credentials".into()))
    }

    /// Like `send`, but any non-2xx answer is an error. Returns the response body.
    async fn call(&self, method: Method, url: &str, body: Option<&Value>) -> Result<String, Error> {
        let (status, text) = self.send(method, url, body).await?;
        if !(200..300).contains(&status) {
            return Err(Error::Message(api_message(status, &text)));
        }
        Ok(text)
    }

    /// `<api>/calendars/<id>/<tail…>` with every part percent-encoded.
    fn calendar_url(&self, calendar_id: &str, tail: &[&str]) -> Result<url::Url, Error> {
        let invalid = |e: &dyn fmt::Display| Error::Message(format!("invalid calendar URL: {e}"));
        let mut url = url::Url::parse(&format!("{}/calendars", self.endpoints.api))
            .map_err(|e| invalid(&e))?;
        url.path_segments_mut()
            .map_err(|()| invalid(&"cannot be a base"))?
            .push(calendar_id)
            .extend(tail);
        Ok(url)
    }

    /// The calendar list, every page.
    pub async fn list_calendars(&self) -> Result<Vec<CalendarInfo>, Error> {
        let mut calendars = Vec::new();
        let mut page_token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut url = url::Url::parse(&format!("{}/users/me/calendarList", self.endpoints.api))
                .map_err(|e| Error::Message(format!("invalid calendar URL: {e}")))?;
            {
                let mut q = url.query_pairs_mut();
                q.append_pair("minAccessRole", "reader")
                    .append_pair("maxResults", "250")
                    .append_pair(
                        "fields",
                        "nextPageToken,items(id,summary,summaryOverride,backgroundColor,primary,selected,deleted)",
                    );
                if let Some(t) = &page_token {
                    q.append_pair("pageToken", t);
                }
            }
            let result = self.call(Method::GET, url.as_str(), None).await;
            if matches!(result, Err(Error::MissingScope)) {
                // Remember it so the status says "reconnect" from now on.
                self.remember_scope(Some(google::SCOPE));
            }
            let page = google::parse_calendar_list(&result?).map_err(Error::Message)?;
            calendars.extend(page.calendars);
            match page.next_page_token {
                Some(t) => page_token = Some(t),
                None => break,
            }
        }
        Ok(calendars)
    }

    /// The repeating series (master events) of a calendar that have an occurrence
    /// at or after `time_min` (RFC 3339), every page. Cancelled masters are left out.
    pub async fn list_series(
        &self,
        calendar_id: &str,
        time_min: &str,
    ) -> Result<Vec<SeriesInfo>, Error> {
        let mut series = Vec::new();
        let mut page_token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut url = self.calendar_url(calendar_id, &["events"])?;
            {
                let mut q = url.query_pairs_mut();
                q.append_pair("singleEvents", "false")
                    .append_pair("showDeleted", "false")
                    .append_pair("timeMin", time_min)
                    .append_pair("maxResults", "250")
                    .append_pair(
                        "fields",
                        "items(id,status,summary,recurrence,recurringEventId),nextPageToken",
                    );
                if let Some(t) = &page_token {
                    q.append_pair("pageToken", t);
                }
            }
            let body = self.call(Method::GET, url.as_str(), None).await?;
            let page = google::parse_series_list(&body).map_err(Error::Message)?;
            series.extend(page.series);
            match page.next_page_token {
                Some(t) => page_token = Some(t),
                None => break,
            }
        }
        Ok(series)
    }

    /// The Google account's email: the id of its primary calendar, from the calendar
    /// list, else from `calendars/primary`.
    pub async fn account_email(&self) -> Result<String, Error> {
        match self.list_calendars().await {
            Ok(list) => {
                if let Some(primary) = list.into_iter().find(|c| c.primary) {
                    return Ok(primary.id);
                }
            }
            Err(e @ (Error::Disconnected | Error::NotConnected)) => return Err(e),
            Err(_) => {}
        }
        let mut url = self.calendar_url("primary", &[])?;
        url.query_pairs_mut().append_pair("fields", "id");
        let body = self.call(Method::GET, url.as_str(), None).await?;
        serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v["id"].as_str().map(str::to_string))
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Error::Message("Google did not say which account this is".into()))
    }

    /// All timed events of the local day `date` of a calendar; the events carry `calendar_id`.
    pub async fn list_day_in(
        &self,
        calendar_id: &str,
        date: &str,
        tz: &TimeZone,
    ) -> Result<Vec<CalEvent>, Error> {
        let (min, max) = google::day_bounds(date, tz).map_err(Error::Message)?;
        let mut events = Vec::new();
        let mut page_token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut url = self.calendar_url(calendar_id, &["events"])?;
            {
                let mut q = url.query_pairs_mut();
                q.append_pair("singleEvents", "true")
                    .append_pair("orderBy", "startTime")
                    .append_pair("timeMin", &min)
                    .append_pair("timeMax", &max)
                    .append_pair("maxResults", "250")
                    .append_pair(
                        "fields",
                        "nextPageToken,items(id,status,summary,start,end,htmlLink,recurringEventId,extendedProperties)",
                    );
                if let Some(t) = &page_token {
                    q.append_pair("pageToken", t);
                }
            }
            let body = self.call(Method::GET, url.as_str(), None).await?;
            let page = google::parse_event_list(&body, date, tz).map_err(Error::Message)?;
            events.extend(page.events.into_iter().map(|mut e| {
                e.calendar_id = calendar_id.to_string();
                e
            }));
            match page.next_page_token {
                Some(t) => page_token = Some(t),
                None => break,
            }
        }
        events.sort_by_key(|e| (e.start, e.end));
        Ok(events)
    }

    /// Creates a timed event; `tags` (when any) are kept hidden on it.
    pub async fn create_event(
        &self,
        calendar_id: &str,
        date: &str,
        (start, end): (u32, u32),
        title: &str,
        tags: &[String],
        tz: &TimeZone,
    ) -> Result<CalEvent, Error> {
        let tags = (!tags.is_empty()).then_some(tags);
        let payload =
            google::event_body(date, start, end, title, tags, tz).map_err(Error::Message)?;
        let url = self.calendar_url(calendar_id, &["events"])?;
        let body = self
            .call(Method::POST, url.as_str(), Some(&payload))
            .await?;
        let mut event = google::parse_event(&body, date, tz).map_err(Error::Message)?;
        event.calendar_id = calendar_id.to_string();
        Ok(event)
    }

    /// Patches time, title and tags (an empty list clears them).
    pub async fn update_event(
        &self,
        (calendar_id, event_id): (&str, &str),
        date: &str,
        (start, end): (u32, u32),
        title: &str,
        tags: &[String],
        tz: &TimeZone,
    ) -> Result<CalEvent, Error> {
        let payload =
            google::event_body(date, start, end, title, Some(tags), tz).map_err(Error::Message)?;
        let url = self.calendar_url(calendar_id, &["events", event_id])?;
        let body = self
            .call(Method::PATCH, url.as_str(), Some(&payload))
            .await?;
        let mut event = google::parse_event(&body, date, tz).map_err(Error::Message)?;
        event.calendar_id = calendar_id.to_string();
        Ok(event)
    }

    /// Patches only the hidden tags of an event; title and time are untouched. The
    /// answer is the event as it is on `date` (for a series master, pass the master id
    /// to `set_series_tags` instead: its own date is not the day shown).
    pub async fn set_tags(
        &self,
        (calendar_id, event_id): (&str, &str),
        date: &str,
        tags: &[String],
        tz: &TimeZone,
    ) -> Result<CalEvent, Error> {
        let body = self.patch_tags(calendar_id, event_id, tags).await?;
        let mut event = google::parse_event(&body, date, tz).map_err(Error::Message)?;
        event.calendar_id = calendar_id.to_string();
        Ok(event)
    }

    /// Same on the master of a repeating series, so every occurrence has the tags.
    pub async fn set_series_tags(
        &self,
        (calendar_id, master_id): (&str, &str),
        tags: &[String],
    ) -> Result<(), Error> {
        self.patch_tags(calendar_id, master_id, tags)
            .await
            .map(drop)
    }

    async fn patch_tags(
        &self,
        calendar_id: &str,
        event_id: &str,
        tags: &[String],
    ) -> Result<String, Error> {
        let url = self.calendar_url(calendar_id, &["events", event_id])?;
        self.call(Method::PATCH, url.as_str(), Some(&google::tags_patch(tags)))
            .await
    }

    /// Deletes the event (for an instance id of a repeating series: only that occurrence).
    /// An event that is already gone (404/410) counts as deleted.
    pub async fn delete_event(&self, calendar_id: &str, event_id: &str) -> Result<(), Error> {
        let url = self.calendar_url(calendar_id, &["events", event_id])?;
        let (status, text) = self.send(Method::DELETE, url.as_str(), None).await?;
        if (200..300).contains(&status) || status == 404 || status == 410 {
            return Ok(());
        }
        Err(Error::Message(api_message(status, &text)))
    }

    /// Best-effort revoke at Google, then forget everything locally.
    pub async fn disconnect(&self) -> Result<(), Error> {
        let refresh = self.store.refresh_token().ok().flatten();
        if let Some(token) = refresh
            && let Ok(mut url) = url::Url::parse(&self.endpoints.revoke)
        {
            url.query_pairs_mut().append_pair("token", &token);
            let _ = self
                .http
                .post(url)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .timeout(REVOKE_TIMEOUT)
                .send()
                .await;
        }
        *self.session.lock().await = None;
        self.store.delete_refresh_token().map_err(Error::Message)?;
        self.remember_scope(None);
        self.update_flags(|f| f.connected = false);
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    pub use crate::google_client::MemoryStore as MemStore;

    /// An in-memory `Vault`: tests never touch the real keychain.
    #[derive(Default)]
    pub struct MemVault {
        pub list: Mutex<Vec<String>>,
        pub stores: Mutex<std::collections::HashMap<String, Arc<MemStore>>>,
        pub legacy_store: Arc<MemStore>,
        pub legacy_email: Mutex<Option<String>>,
        /// Makes `set_accounts` fail, to test a migration that cannot finish.
        pub fail_set_accounts: std::sync::atomic::AtomicBool,
    }

    impl MemVault {
        pub fn account(&self, email: &str) -> Arc<MemStore> {
            self.stores
                .lock()
                .unwrap()
                .entry(email.to_string())
                .or_default()
                .clone()
        }
    }

    impl Vault for MemVault {
        fn accounts(&self) -> Vec<String> {
            self.list.lock().unwrap().clone()
        }
        fn set_accounts(&self, emails: &[String]) -> Result<(), String> {
            if self
                .fail_set_accounts
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return Err("keychain unavailable".into());
            }
            *self.list.lock().unwrap() = emails.to_vec();
            Ok(())
        }
        fn store(&self, email: &str) -> Arc<dyn TokenStore> {
            self.account(email)
        }
        fn legacy(&self) -> Option<LegacyAccount> {
            Some(LegacyAccount {
                refresh: self.legacy_store.refresh_token().unwrap()?,
                email: self.legacy_email.lock().unwrap().clone(),
                scope: self.legacy_store.scope(),
            })
        }
        fn legacy_store(&self) -> Arc<dyn TokenStore> {
            self.legacy_store.clone()
        }
        fn delete_legacy(&self) {
            self.legacy_store.delete_refresh_token().unwrap();
            self.legacy_store.set_scope(None);
            *self.legacy_email.lock().unwrap() = None;
        }
    }

    /// Serves the scripted `(status, body)` responses in order, one per connection,
    /// and records every raw request.
    pub fn fake_server(script: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for (status, body) in script {
                let Ok((mut s, _)) = listener.accept() else {
                    return;
                };
                let mut raw = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = s.read(&mut chunk).unwrap_or(0);
                    raw.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&raw).into_owned();
                    if let Some(i) = text.find("\r\n\r\n") {
                        let len = text[..i]
                            .to_ascii_lowercase()
                            .lines()
                            .find_map(|l| {
                                l.strip_prefix("content-length:")
                                    .map(|v| v.trim().parse().unwrap_or(0))
                            })
                            .unwrap_or(0usize);
                        if raw.len() >= i + 4 + len || n == 0 {
                            break;
                        }
                    } else if n == 0 {
                        break;
                    }
                }
                log.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&raw).into_owned());
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes());
            }
        });
        (base, seen)
    }

    pub fn creds() -> Credentials {
        Credentials {
            client_id: "cid".into(),
            client_secret: "sec".into(),
        }
    }

    pub fn endpoints(base: &str) -> Endpoints {
        Endpoints {
            token: format!("{base}/token"),
            revoke: format!("{base}/revoke"),
            api: format!("{base}/calendar/v3"),
        }
    }

    pub fn client(base: &str, store: Arc<MemStore>) -> Client {
        Client::new(creds(), endpoints(base), store).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::*;
    use super::*;
    use std::sync::Arc;

    fn connected_store() -> Arc<MemStore> {
        let store = Arc::new(MemStore::default());
        store.set_refresh_token("refresh-1").unwrap();
        store
    }

    fn tok(access: &str) -> (u16, String) {
        (
            200,
            format!(r#"{{"access_token":"{access}","expires_in":3600}}"#),
        )
    }

    fn tz() -> jiff::tz::TimeZone {
        jiff::tz::TimeZone::get("America/Sao_Paulo").unwrap()
    }

    const EVENT: &str = r#"{"id":"e1","summary":"Deep work","start":{"dateTime":"2026-10-21T09:00:00-03:00"},"end":{"dateTime":"2026-10-21T10:00:00-03:00"}}"#;

    fn page(items: &str, next: Option<&str>) -> (u16, String) {
        let next = next
            .map(|t| format!(r#","nextPageToken":"{t}""#))
            .unwrap_or_default();
        (
            200,
            format!(r#"{{"summary":"me@example.com","items":[{items}]{next}}}"#),
        )
    }

    fn run<F: std::future::Future>(f: F) -> F::Output {
        tauri::async_runtime::block_on(f)
    }

    #[test]
    fn list_day_refreshes_then_calls_the_api_with_bearer() {
        let (base, seen) = fake_server(vec![tok("access-1"), page(EVENT, None)]);
        let store = connected_store();
        let c = client(&base, store.clone());
        let day = run(c.list_day_in("primary", "2026-10-21", &tz())).unwrap();
        assert_eq!(day.len(), 1);
        assert_eq!(day[0].title, "Deep work");
        let reqs = seen.lock().unwrap();
        assert!(reqs[0].starts_with("POST /token"));
        assert!(reqs[0].contains("grant_type=refresh_token"));
        assert!(reqs[0].contains("refresh_token=refresh-1"));
        assert!(reqs[0].contains("client_secret=sec"));
        assert!(reqs[1].starts_with("GET /calendar/v3/calendars/primary/events?"));
        assert!(
            reqs[1]
                .to_ascii_lowercase()
                .contains("authorization: bearer access-1")
        );
        assert!(reqs[1].contains("singleEvents=true"));
        assert!(reqs[1].contains("timeMin=2026-10-21T00%3A00%3A00-03%3A00"));
    }

    #[test]
    fn a_valid_access_token_is_reused() {
        let (base, seen) = fake_server(vec![tok("a"), page("", None), page("", None)]);
        let c = client(&base, connected_store());
        run(c.list_day_in("primary", "2026-10-21", &tz())).unwrap();
        run(c.list_day_in("primary", "2026-10-22", &tz())).unwrap();
        assert_eq!(seen.lock().unwrap().len(), 3);
    }

    #[test]
    fn a_401_triggers_one_refresh_and_retry() {
        let (base, seen) = fake_server(vec![
            tok("old"),
            (401, r#"{"error":{"message":"Invalid Credentials"}}"#.into()),
            tok("new"),
            page(EVENT, None),
        ]);
        let c = client(&base, connected_store());
        let day = run(c.list_day_in("primary", "2026-10-21", &tz())).unwrap();
        assert_eq!(day.len(), 1);
        let reqs = seen.lock().unwrap();
        assert_eq!(reqs.len(), 4);
        assert!(
            reqs[3]
                .to_ascii_lowercase()
                .contains("authorization: bearer new")
        );
    }

    #[test]
    fn a_second_401_is_an_error() {
        let unauthorized = (
            401,
            r#"{"error":{"message":"Invalid Credentials"}}"#.to_string(),
        );
        let (base, _) = fake_server(vec![tok("a"), unauthorized.clone(), tok("b"), unauthorized]);
        let c = client(&base, connected_store());
        let err = run(c.list_day_in("primary", "2026-10-21", &tz())).unwrap_err();
        assert_eq!(err.to_string(), "Invalid Credentials");
    }

    #[test]
    fn invalid_grant_disconnects_and_forgets_the_token() {
        let (base, _) = fake_server(vec![(
            400,
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#
                .into(),
        )]);
        let store = connected_store();
        let c = client(&base, store.clone());
        let err = run(c.list_day_in("primary", "2026-10-21", &tz())).unwrap_err();
        assert!(matches!(err, Error::Disconnected), "{err}");
        assert_eq!(store.refresh_token().unwrap(), None);
        assert!(!c.is_connected());
        assert!(c.needs_reconnect(), "no token: connect this account again");
    }

    #[test]
    fn not_connected_makes_no_request() {
        let (base, seen) = fake_server(vec![]);
        let c = client(&base, Arc::new(MemStore::default()));
        let err = run(c.list_day_in("primary", "2026-10-21", &tz())).unwrap_err();
        assert!(matches!(err, Error::NotConnected));
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn list_day_follows_pagination() {
        let second = r#"{"id":"e2","summary":"B","start":{"dateTime":"2026-10-21T11:00:00-03:00"},"end":{"dateTime":"2026-10-21T12:00:00-03:00"}}"#;
        let (base, seen) = fake_server(vec![
            tok("a"),
            page(EVENT, Some("PAGE2")),
            page(second, None),
        ]);
        let c = client(&base, connected_store());
        let day = run(c.list_day_in("primary", "2026-10-21", &tz())).unwrap();
        assert_eq!(day.len(), 2);
        assert!(seen.lock().unwrap()[2].contains("pageToken=PAGE2"));
    }

    #[test]
    fn create_posts_the_body_and_returns_the_event() {
        let (base, seen) = fake_server(vec![tok("a"), (200, EVENT.to_string())]);
        let c = client(&base, connected_store());
        let e = run(c.create_event("primary", "2026-10-21", (540, 600), "Deep work", &[], &tz()))
            .unwrap();
        assert_eq!((e.id.as_str(), e.start, e.end), ("e1", 540, 600));
        let reqs = seen.lock().unwrap();
        assert!(reqs[1].starts_with("POST /calendar/v3/calendars/primary/events "));
        assert!(reqs[1].contains(r#""summary":"Deep work""#));
        assert!(reqs[1].contains(r#""timeZone":"America/Sao_Paulo""#));
        assert!(
            !reqs[1].contains("extendedProperties"),
            "no tags, nothing hidden is sent on create"
        );
    }

    fn body_of(req: &str) -> serde_json::Value {
        serde_json::from_str(req.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    #[test]
    fn create_and_update_send_the_tags_hidden_on_the_event() {
        let (base, seen) = fake_server(vec![
            tok("a"),
            (200, EVENT.to_string()),
            (200, EVENT.to_string()),
            (200, EVENT.to_string()),
        ]);
        let c = client(&base, connected_store());
        let tags = vec!["rust".to_string(), "mestrado".to_string()];
        run(c.create_event("primary", "2026-10-21", (540, 600), "Deep", &tags, &tz())).unwrap();
        run(c.update_event(
            ("primary", "e1"),
            "2026-10-21",
            (540, 600),
            "Deep",
            &tags,
            &tz(),
        ))
        .unwrap();
        run(c.update_event(
            ("primary", "e1"),
            "2026-10-21",
            (540, 600),
            "Deep",
            &[],
            &tz(),
        ))
        .unwrap();
        let reqs = seen.lock().unwrap();
        for r in &reqs[1..3] {
            assert_eq!(
                body_of(r)["extendedProperties"]["private"]["catengaTags"],
                "rust mestrado"
            );
            assert_eq!(body_of(r)["summary"], "Deep");
        }
        // an update with no tags clears them
        assert_eq!(
            body_of(&reqs[3])["extendedProperties"]["private"]["catengaTags"],
            ""
        );
    }

    #[test]
    fn set_tags_patches_only_the_hidden_property() {
        let tagged = r#"{"id":"e1","summary":"Deep work","start":{"dateTime":"2026-10-21T09:00:00-03:00"},"end":{"dateTime":"2026-10-21T10:00:00-03:00"},"extendedProperties":{"private":{"catengaTags":"rust"}}}"#;
        let master = r#"{"id":"s1","summary":"Deep work","start":{"dateTime":"2026-01-05T09:00:00-03:00"},"end":{"dateTime":"2026-01-05T10:00:00-03:00"}}"#;
        let (base, seen) = fake_server(vec![
            tok("a"),
            (200, master.to_string()),
            (200, tagged.to_string()),
        ]);
        let c = client(&base, connected_store());
        let tags = vec!["rust".to_string()];
        // the master answers with its own first date, which must not matter
        run(c.set_series_tags(("mest@x", "s1"), &tags)).unwrap();
        let e = run(c.set_tags(
            ("mest@x", "s1_20261021T120000Z"),
            "2026-10-21",
            &tags,
            &tz(),
        ))
        .unwrap();
        assert_eq!(e.tags, ["rust"]);
        assert_eq!(e.calendar_id, "mest@x");
        let reqs = seen.lock().unwrap();
        assert!(reqs[1].starts_with("PATCH /calendar/v3/calendars/mest@x/events/s1 "));
        assert!(
            reqs[2].starts_with("PATCH /calendar/v3/calendars/mest@x/events/s1_20261021T120000Z ")
        );
        for r in &reqs[1..] {
            assert_eq!(
                body_of(r),
                serde_json::json!({"extendedProperties":{"private":{"catengaTags":"rust"}}})
            );
        }
    }

    #[test]
    fn the_account_email_is_the_primary_calendars_id() {
        let (base, seen) = fake_server(vec![
            tok("a"),
            cal_page(
                r##"{"id":"other@x","summary":"Other"},{"id":"me@work.com","summary":"Me","primary":true}"##,
                None,
            ),
        ]);
        let c = client(&base, connected_store());
        assert_eq!(run(c.account_email()).unwrap(), "me@work.com");
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[test]
    fn the_account_email_falls_back_to_the_primary_calendar_resource() {
        // The list cannot be read (token without the list scope): ask for calendars/primary.
        let denied = (
            403,
            r#"{"error":{"code":403,"message":"Request had insufficient authentication scopes."}}"#
                .to_string(),
        );
        let (base, seen) = fake_server(vec![
            tok("a"),
            denied,
            (200, r#"{"id":"me@work.com"}"#.to_string()),
        ]);
        let c = client(&base, connected_store());
        assert_eq!(run(c.account_email()).unwrap(), "me@work.com");
        let reqs = seen.lock().unwrap();
        assert!(reqs[2].starts_with("GET /calendar/v3/calendars/primary?"));
        assert!(reqs[2].contains("fields=id"));
        drop(reqs);
        // and says so when neither answers
        let (base, _) = fake_server(vec![tok("a"), cal_page("", None), (200, "{}".into())]);
        let c = client(&base, connected_store());
        assert!(run(c.account_email()).is_err());
    }

    #[test]
    fn credentials_to_keep_are_the_tokens_of_the_consent() {
        let (base, _) = fake_server(vec![(200, LIST_SCOPE_RESPONSE.into())]);
        let c = client(&base, Arc::new(MemStore::default()));
        assert!(c.credentials_to_keep().is_err());
        run(c.exchange_code("c", "v", "http://127.0.0.1:9/callback")).unwrap();
        let (refresh, scope) = c.credentials_to_keep().unwrap();
        assert_eq!(refresh, "R1");
        assert!(google::has_list_scope(&scope.unwrap()));
    }

    fn vault_with_legacy(email: Option<&str>) -> MemVault {
        let v = MemVault::default();
        v.legacy_store.set_refresh_token("legacy-refresh").unwrap();
        v.legacy_store.set_scope(Some("s1 s2"));
        *v.legacy_email.lock().unwrap() = email.map(str::to_string);
        v
    }

    #[test]
    fn legacy_credentials_move_under_the_email_and_the_old_entries_go() {
        let v = vault_with_legacy(Some("me@x"));
        migrate_legacy(&v, "me@x").unwrap();
        assert_eq!(v.accounts(), ["me@x"]);
        let s = v.store("me@x");
        assert_eq!(
            s.refresh_token().unwrap().as_deref(),
            Some("legacy-refresh")
        );
        assert_eq!(s.scope().as_deref(), Some("s1 s2"));
        assert!(v.legacy().is_none());
        assert_eq!(v.legacy_store.refresh_token().unwrap(), None);
        // nothing left to do the second time
        migrate_legacy(&v, "me@x").unwrap();
        assert_eq!(v.accounts(), ["me@x"]);
    }

    #[test]
    fn a_migration_that_cannot_finish_keeps_the_legacy_token() {
        let v = vault_with_legacy(Some("me@x"));
        v.fail_set_accounts
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(migrate_legacy(&v, "me@x").is_err());
        assert_eq!(
            v.legacy_store.refresh_token().unwrap().as_deref(),
            Some("legacy-refresh"),
            "the old token is never deleted before the new entries are saved"
        );
        assert!(v.legacy().is_some() && v.accounts().is_empty());
        // the next start tries again and works
        v.fail_set_accounts
            .store(false, std::sync::atomic::Ordering::SeqCst);
        migrate_legacy(&v, "me@x").unwrap();
        assert_eq!(v.accounts(), ["me@x"]);
        assert!(v.legacy().is_none());
    }

    #[test]
    fn migrating_keeps_a_token_the_account_already_has_and_other_accounts() {
        let v = vault_with_legacy(None);
        v.set_accounts(&["a@x".to_string(), "me@x".to_string()])
            .unwrap();
        v.store("me@x").set_refresh_token("newer").unwrap();
        migrate_legacy(&v, "me@x").unwrap();
        assert_eq!(v.accounts(), ["a@x", "me@x"]);
        assert_eq!(
            v.store("me@x").refresh_token().unwrap().as_deref(),
            Some("newer")
        );
        assert!(v.legacy().is_none());
        // no legacy entries: no-op
        migrate_legacy(&v, "me@x").unwrap();
    }

    #[test]
    fn update_patches_the_event_with_an_encoded_id() {
        let (base, seen) = fake_server(vec![tok("a"), (200, EVENT.to_string())]);
        let c = client(&base, connected_store());
        run(c.update_event(
            ("primary", "abc_20261021T120000Z/x"),
            "2026-10-21",
            (540, 600),
            "T",
            &[],
            &tz(),
        ))
        .unwrap();
        let reqs = seen.lock().unwrap();
        assert!(
            reqs[1].starts_with(
                "PATCH /calendar/v3/calendars/primary/events/abc_20261021T120000Z%2Fx "
            ),
            "{}",
            reqs[1]
        );
    }

    #[test]
    fn delete_sends_a_delete_with_an_encoded_id_on_the_given_calendar() {
        let (base, seen) = fake_server(vec![tok("a"), (204, String::new())]);
        let c = client(&base, connected_store());
        run(c.delete_event("mest@group.calendar.google.com", "abc_20261021T120000Z/x")).unwrap();
        let reqs = seen.lock().unwrap();
        assert!(
            reqs[1].starts_with(
                "DELETE /calendar/v3/calendars/mest@group.calendar.google.com/events/abc_20261021T120000Z%2Fx "
            ),
            "{}",
            reqs[1]
        );
    }

    #[test]
    fn delete_treats_404_and_410_as_already_gone() {
        for status in [404u16, 410] {
            let (base, _) = fake_server(vec![
                tok("a"),
                (
                    status,
                    r#"{"error":{"message":"Resource has been deleted"}}"#.into(),
                ),
            ]);
            let c = client(&base, connected_store());
            run(c.delete_event("primary", "e1")).unwrap();
        }
    }

    #[test]
    fn delete_reports_other_errors() {
        let (base, _) = fake_server(vec![
            tok("a"),
            (403, r#"{"error":{"message":"Forbidden"}}"#.into()),
        ]);
        let c = client(&base, connected_store());
        let err = run(c.delete_event("primary", "e1")).unwrap_err();
        assert_eq!(err.to_string(), "Forbidden");
    }

    #[test]
    fn api_errors_carry_googles_message() {
        let (base, _) = fake_server(vec![
            tok("a"),
            (
                403,
                r#"{"error":{"code":403,"message":"Rate Limit Exceeded"}}"#.into(),
            ),
        ]);
        let c = client(&base, connected_store());
        let err = run(c.list_day_in("primary", "2026-10-21", &tz())).unwrap_err();
        assert_eq!(err.to_string(), "Rate Limit Exceeded");
    }

    #[test]
    fn connect_exchanges_the_code_and_stores_the_refresh_token() {
        let (base, seen) = fake_server(vec![(
            200,
            r#"{"access_token":"a","expires_in":3600,"refresh_token":"R1"}"#.into(),
        )]);
        let store = Arc::new(MemStore::default());
        let c = client(&base, store.clone());
        run(c.exchange_code("4/code", "verifier", "http://127.0.0.1:9/callback")).unwrap();
        assert_eq!(store.refresh_token().unwrap().as_deref(), Some("R1"));
        let reqs = seen.lock().unwrap();
        assert!(reqs[0].contains("grant_type=authorization_code"));
        assert!(reqs[0].contains("code=4%2Fcode"));
        assert!(reqs[0].contains("code_verifier=verifier"));
        assert!(reqs[0].contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A9%2Fcallback"));
        assert!(c.is_connected());
    }

    #[test]
    fn connect_without_a_refresh_token_fails() {
        let (base, _) = fake_server(vec![tok("a")]);
        let store = Arc::new(MemStore::default());
        let c = client(&base, store.clone());
        let err = run(c.exchange_code("c", "v", "http://127.0.0.1:9/callback")).unwrap_err();
        assert!(err.to_string().contains("refresh token"), "{err}");
        assert_eq!(store.refresh_token().unwrap(), None);
    }

    #[test]
    fn disconnect_revokes_best_effort_and_clears_state() {
        let (base, seen) = fake_server(vec![(200, "{}".into())]);
        let store = connected_store();
        let c = client(&base, store.clone());
        run(c.disconnect()).unwrap();
        assert!(seen.lock().unwrap()[0].starts_with("POST /revoke?token=refresh-1"));
        assert_eq!(store.refresh_token().unwrap(), None);
    }

    #[test]
    fn disconnect_still_clears_when_revoke_fails() {
        // Nothing listens on this port.
        let store = connected_store();
        let c = client("http://127.0.0.1:1", store.clone());
        run(c.disconnect()).unwrap();
        assert_eq!(store.refresh_token().unwrap(), None);
    }

    const LIST_SCOPE_RESPONSE: &str = r#"{"access_token":"a","expires_in":3600,"refresh_token":"R1","scope":"https://www.googleapis.com/auth/calendar.events https://www.googleapis.com/auth/calendar.calendarlist.readonly"}"#;

    #[test]
    fn secondary_calendars_are_fetched_by_encoded_id_and_tag_their_events() {
        let (base, seen) = fake_server(vec![tok("a"), page(EVENT, None)]);
        let c = client(&base, connected_store());
        let day = run(c.list_day_in(
            "en.br#holiday@group.v.calendar.google.com",
            "2026-10-21",
            &tz(),
        ))
        .unwrap();
        assert_eq!(
            day[0].calendar_id,
            "en.br#holiday@group.v.calendar.google.com"
        );
        let reqs = seen.lock().unwrap();
        assert!(
            reqs[1].starts_with(
                "GET /calendar/v3/calendars/en.br%23holiday@group.v.calendar.google.com/events?"
            ),
            "{}",
            reqs[1]
        );
    }

    #[test]
    fn create_and_update_target_the_given_calendar() {
        let (base, seen) = fake_server(vec![
            tok("a"),
            (200, EVENT.to_string()),
            (200, EVENT.to_string()),
        ]);
        let c = client(&base, connected_store());
        let e = run(c.create_event(
            "mest@group.calendar.google.com",
            "2026-10-21",
            (540, 600),
            "T",
            &[],
            &tz(),
        ))
        .unwrap();
        assert_eq!(e.calendar_id, "mest@group.calendar.google.com");
        let e = run(c.update_event(
            ("mest@group.calendar.google.com", "e1"),
            "2026-10-21",
            (540, 600),
            "T",
            &[],
            &tz(),
        ))
        .unwrap();
        assert_eq!(e.calendar_id, "mest@group.calendar.google.com");
        let reqs = seen.lock().unwrap();
        assert!(
            reqs[1]
                .starts_with("POST /calendar/v3/calendars/mest@group.calendar.google.com/events ")
        );
        assert!(
            reqs[2].starts_with(
                "PATCH /calendar/v3/calendars/mest@group.calendar.google.com/events/e1 "
            )
        );
    }

    fn cal_page(items: &str, next: Option<&str>) -> (u16, String) {
        let next = next
            .map(|t| format!(r#","nextPageToken":"{t}""#))
            .unwrap_or_default();
        (200, format!(r#"{{"items":[{items}]{next}}}"#))
    }

    #[test]
    fn list_series_asks_for_masters_follows_pages_and_keeps_only_recurring_ones() {
        let (base, seen) = fake_server(vec![
            tok("a"),
            cal_page(
                r#"{"id":"s1","summary":"Learn Rust","recurrence":["RRULE:FREQ=DAILY"]},{"id":"one","summary":"Dentist"}"#,
                Some("P2"),
            ),
            cal_page(
                r#"{"id":"s2","summary":"Gym","recurrence":["RRULE:FREQ=WEEKLY"]},{"id":"x","status":"cancelled","recurrence":["RRULE:FREQ=DAILY"]}"#,
                None,
            ),
        ]);
        let c = client(&base, connected_store());
        let series = run(c.list_series("mest@x", "2026-07-23T00:00:00-03:00")).unwrap();
        let got: Vec<_> = series
            .iter()
            .map(|s| (s.id.as_str(), s.name.as_str()))
            .collect();
        assert_eq!(got, [("s1", "Learn Rust"), ("s2", "Gym")]);
        let reqs = seen.lock().unwrap();
        assert!(
            reqs[1].starts_with("GET /calendar/v3/calendars/mest@x/events?"),
            "{}",
            reqs[1]
        );
        assert!(reqs[1].contains("singleEvents=false"));
        assert!(reqs[1].contains("showDeleted=false"));
        assert!(reqs[1].contains("timeMin=2026-07-23T00%3A00%3A00-03%3A00"));
        assert!(reqs[1].contains("maxResults=250"));
        assert!(
            reqs[1].contains("recurrence"),
            "asks for the recurrence field"
        );
        assert!(reqs[2].contains("pageToken=P2"));
    }

    #[test]
    fn list_day_asks_for_the_recurring_event_id() {
        let (base, seen) = fake_server(vec![
            tok("a"),
            page(
                r#"{"id":"s1_20261021","recurringEventId":"s1","summary":"Learn Rust","start":{"dateTime":"2026-10-21T09:00:00-03:00"},"end":{"dateTime":"2026-10-21T10:00:00-03:00"}}"#,
                None,
            ),
        ]);
        let c = client(&base, connected_store());
        let day = run(c.list_day_in("primary", "2026-10-21", &tz())).unwrap();
        assert_eq!(day[0].recurring_event_id.as_deref(), Some("s1"));
        assert!(seen.lock().unwrap()[1].contains("recurringEventId"));
    }

    #[test]
    fn list_calendars_follows_pages_and_asks_for_readable_ones() {
        let (base, seen) = fake_server(vec![
            tok("a"),
            cal_page(
                r##"{"id":"me@x","summary":"me@x","primary":true,"selected":true,"backgroundColor":"#111111"}"##,
                Some("P2"),
            ),
            cal_page(
                r##"{"id":"mest@x","summary":"Mestrado","backgroundColor":"#222222"}"##,
                None,
            ),
        ]);
        let c = client(&base, connected_store());
        let cals = run(c.list_calendars()).unwrap();
        let ids: Vec<_> = cals.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["me@x", "mest@x"]);
        let reqs = seen.lock().unwrap();
        assert!(
            reqs[1].starts_with("GET /calendar/v3/users/me/calendarList?"),
            "{}",
            reqs[1]
        );
        assert!(reqs[1].contains("minAccessRole=reader"));
        assert!(reqs[2].contains("pageToken=P2"));
    }

    #[test]
    fn a_403_for_missing_scopes_means_reconnect_and_is_remembered() {
        let denied = (
            403,
            r#"{"error":{"code":403,"message":"Request had insufficient authentication scopes.","errors":[{"reason":"insufficientPermissions"}]}}"#.to_string(),
        );
        let (base, _) = fake_server(vec![tok("a"), denied]);
        let store = connected_store();
        let c = client(&base, store.clone());
        assert!(!c.needs_reconnect(), "unknown until a token says otherwise");
        let err = run(c.list_calendars()).unwrap_err();
        assert!(matches!(err, Error::MissingScope), "{err}");
        assert_eq!(
            err.to_string(),
            "Reconnect Google Calendar to load your calendars"
        );
        assert!(c.needs_reconnect());
    }

    #[test]
    fn granted_scopes_drive_needs_reconnect() {
        // connect with both scopes: fine
        let (base, _) = fake_server(vec![(200, LIST_SCOPE_RESPONSE.into())]);
        let store = Arc::new(MemStore::default());
        let c = client(&base, store.clone());
        run(c.exchange_code("c", "v", "http://127.0.0.1:9/callback")).unwrap();
        assert!(c.is_connected() && !c.needs_reconnect());
        // an old token refreshes with the events scope only: reconnect
        let (base, _) = fake_server(vec![(
            200,
            r#"{"access_token":"a","expires_in":3600,"scope":"https://www.googleapis.com/auth/calendar.events"}"#.into(),
        ), page("", None)]);
        let c = client(&base, connected_store());
        run(c.list_day_in("primary", "2026-10-21", &tz())).unwrap();
        assert!(c.needs_reconnect());
        // disconnecting forgets the token
        run(c.disconnect()).unwrap();
        assert!(!c.is_connected());
    }

    #[test]
    fn a_token_response_without_scope_changes_nothing() {
        let (base, _) = fake_server(vec![tok("a"), page("", None)]);
        let store = connected_store();
        store.set_scope(Some("https://www.googleapis.com/auth/calendar.events"));
        let c = client(&base, store.clone());
        run(c.list_day_in("primary", "2026-10-21", &tz())).unwrap();
        assert!(c.needs_reconnect());
    }
}
