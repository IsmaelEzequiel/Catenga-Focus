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
const KEYCHAIN_EMAIL_USER: &str = "google-email";
const KEYCHAIN_SCOPE_USER: &str = "google-scope";
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

/// Where the long-lived credentials are kept.
pub trait TokenStore: Send + Sync {
    fn refresh_token(&self) -> Result<Option<String>, String>;
    fn set_refresh_token(&self, token: &str) -> Result<(), String>;
    fn delete_refresh_token(&self) -> Result<(), String>;
    /// Display only; failures are ignored.
    fn email(&self) -> Option<String>;
    fn set_email(&self, email: Option<&str>);
    /// The scopes of the last token response (space separated), if known.
    fn scope(&self) -> Option<String>;
    fn set_scope(&self, scope: Option<&str>);
}

/// The OS keychain, same service as the account token. Verified by hand
/// (tests must not touch the real keychain).
pub struct KeychainStore;

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

fn delete_entry(user: &str) -> Result<(), String> {
    match entry(user)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(crate::account::keychain_error("deleting from", &e)),
    }
}

impl TokenStore for KeychainStore {
    fn refresh_token(&self) -> Result<Option<String>, String> {
        get_entry(KEYCHAIN_REFRESH_USER)
    }
    fn set_refresh_token(&self, token: &str) -> Result<(), String> {
        entry(KEYCHAIN_REFRESH_USER)?
            .set_password(token)
            .map_err(|e| crate::account::keychain_error("saving to", &e))
    }
    fn delete_refresh_token(&self) -> Result<(), String> {
        delete_entry(KEYCHAIN_REFRESH_USER)
    }
    fn email(&self) -> Option<String> {
        get_entry(KEYCHAIN_EMAIL_USER).ok().flatten()
    }
    fn set_email(&self, email: Option<&str>) {
        let _ = match email {
            Some(e) => entry(KEYCHAIN_EMAIL_USER).and_then(|en| {
                en.set_password(e)
                    .map_err(|e| crate::account::keychain_error("saving to", &e))
            }),
            None => delete_entry(KEYCHAIN_EMAIL_USER),
        };
    }
    fn scope(&self) -> Option<String> {
        get_entry(KEYCHAIN_SCOPE_USER).ok().flatten()
    }
    fn set_scope(&self, scope: Option<&str>) {
        let _ = match scope {
            Some(s) => entry(KEYCHAIN_SCOPE_USER).and_then(|en| {
                en.set_password(s)
                    .map_err(|e| crate::account::keychain_error("saving to", &e))
            }),
            None => delete_entry(KEYCHAIN_SCOPE_USER),
        };
    }
}

/// An events list for one day plus the account email, when the calendar title is one.
#[derive(Debug)]
pub struct DayEvents {
    pub events: Vec<CalEvent>,
    pub email: Option<String>,
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
        })
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.store.refresh_token(), Ok(Some(_)))
    }

    pub fn stored_email(&self) -> Option<String> {
        self.store.email()
    }

    pub fn remember_email(&self, email: Option<&str>) {
        self.store.set_email(email);
    }

    /// Connected with a token that lacks the calendar list scope (granted before it existed).
    pub fn needs_reconnect(&self) -> bool {
        self.is_connected()
            && self
                .store
                .scope()
                .is_some_and(|s| !google::has_list_scope(&s))
    }

    fn remember_scope(&self, tokens: &Tokens) {
        if let Some(scope) = &tokens.scope {
            self.store.set_scope(Some(scope));
        }
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
        self.remember_scope(&tokens);
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
                self.remember_scope(&tokens);
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
                self.store.set_email(None);
                self.store.set_scope(None);
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
                self.store.set_scope(Some(google::SCOPE));
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

    /// All timed events of the local day `date` of the primary calendar.
    pub async fn list_day(&self, date: &str, tz: &TimeZone) -> Result<DayEvents, Error> {
        self.list_day_in("primary", date, tz).await
    }

    /// Same for any calendar; the events carry `calendar_id`.
    pub async fn list_day_in(
        &self,
        calendar_id: &str,
        date: &str,
        tz: &TimeZone,
    ) -> Result<DayEvents, Error> {
        let (min, max) = google::day_bounds(date, tz).map_err(Error::Message)?;
        let mut events = Vec::new();
        let mut summary = None;
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
                        "summary,nextPageToken,items(id,status,summary,start,end,htmlLink,recurringEventId)",
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
            summary = summary.or(page.summary);
            match page.next_page_token {
                Some(t) => page_token = Some(t),
                None => break,
            }
        }
        events.sort_by_key(|e| (e.start, e.end));
        Ok(DayEvents {
            events,
            email: google::account_email(summary.as_deref()),
        })
    }

    pub async fn create_event(
        &self,
        calendar_id: &str,
        date: &str,
        start: u32,
        end: u32,
        title: &str,
        tz: &TimeZone,
    ) -> Result<CalEvent, Error> {
        let payload = google::event_body(date, start, end, title, tz).map_err(Error::Message)?;
        let url = self.calendar_url(calendar_id, &["events"])?;
        let body = self
            .call(Method::POST, url.as_str(), Some(&payload))
            .await?;
        let mut event = google::parse_event(&body, date, tz).map_err(Error::Message)?;
        event.calendar_id = calendar_id.to_string();
        Ok(event)
    }

    pub async fn update_event(
        &self,
        (calendar_id, event_id): (&str, &str),
        date: &str,
        start: u32,
        end: u32,
        title: &str,
        tz: &TimeZone,
    ) -> Result<CalEvent, Error> {
        let payload = google::event_body(date, start, end, title, tz).map_err(Error::Message)?;
        let url = self.calendar_url(calendar_id, &["events", event_id])?;
        let body = self
            .call(Method::PATCH, url.as_str(), Some(&payload))
            .await?;
        let mut event = google::parse_event(&body, date, tz).map_err(Error::Message)?;
        event.calendar_id = calendar_id.to_string();
        Ok(event)
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
        self.store.set_email(None);
        self.store.set_scope(None);
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    pub struct MemStore {
        pub refresh: Mutex<Option<String>>,
        email: Mutex<Option<String>>,
        scope: Mutex<Option<String>>,
    }

    impl TokenStore for MemStore {
        fn refresh_token(&self) -> Result<Option<String>, String> {
            Ok(self.refresh.lock().unwrap().clone())
        }
        fn set_refresh_token(&self, token: &str) -> Result<(), String> {
            *self.refresh.lock().unwrap() = Some(token.to_string());
            Ok(())
        }
        fn delete_refresh_token(&self) -> Result<(), String> {
            *self.refresh.lock().unwrap() = None;
            Ok(())
        }
        fn email(&self) -> Option<String> {
            self.email.lock().unwrap().clone()
        }
        fn set_email(&self, email: Option<&str>) {
            *self.email.lock().unwrap() = email.map(str::to_string);
        }
        fn scope(&self) -> Option<String> {
            self.scope.lock().unwrap().clone()
        }
        fn set_scope(&self, scope: Option<&str>) {
            *self.scope.lock().unwrap() = scope.map(str::to_string);
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

    pub fn client(base: &str, store: Arc<MemStore>) -> Client {
        Client::new(
            Credentials {
                client_id: "cid".into(),
                client_secret: "sec".into(),
            },
            Endpoints {
                token: format!("{base}/token"),
                revoke: format!("{base}/revoke"),
                api: format!("{base}/calendar/v3"),
            },
            store,
        )
        .unwrap()
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
        let day = run(c.list_day("2026-10-21", &tz())).unwrap();
        assert_eq!(day.events.len(), 1);
        assert_eq!(day.events[0].title, "Deep work");
        assert_eq!(day.email.as_deref(), Some("me@example.com"));
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
        run(c.list_day("2026-10-21", &tz())).unwrap();
        run(c.list_day("2026-10-22", &tz())).unwrap();
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
        let day = run(c.list_day("2026-10-21", &tz())).unwrap();
        assert_eq!(day.events.len(), 1);
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
        let err = run(c.list_day("2026-10-21", &tz())).unwrap_err();
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
        store.set_email(Some("me@example.com"));
        let c = client(&base, store.clone());
        let err = run(c.list_day("2026-10-21", &tz())).unwrap_err();
        assert!(matches!(err, Error::Disconnected), "{err}");
        assert_eq!(store.refresh_token().unwrap(), None);
        assert_eq!(store.email(), None);
        assert!(!c.is_connected());
    }

    #[test]
    fn not_connected_makes_no_request() {
        let (base, seen) = fake_server(vec![]);
        let c = client(&base, Arc::new(MemStore::default()));
        let err = run(c.list_day("2026-10-21", &tz())).unwrap_err();
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
        let day = run(c.list_day("2026-10-21", &tz())).unwrap();
        assert_eq!(day.events.len(), 2);
        assert!(seen.lock().unwrap()[2].contains("pageToken=PAGE2"));
    }

    #[test]
    fn create_posts_the_body_and_returns_the_event() {
        let (base, seen) = fake_server(vec![tok("a"), (200, EVENT.to_string())]);
        let c = client(&base, connected_store());
        let e = run(c.create_event("primary", "2026-10-21", 540, 600, "Deep work", &tz())).unwrap();
        assert_eq!((e.id.as_str(), e.start, e.end), ("e1", 540, 600));
        let reqs = seen.lock().unwrap();
        assert!(reqs[1].starts_with("POST /calendar/v3/calendars/primary/events "));
        assert!(reqs[1].contains(r#""summary":"Deep work""#));
        assert!(reqs[1].contains(r#""timeZone":"America/Sao_Paulo""#));
    }

    #[test]
    fn update_patches_the_event_with_an_encoded_id() {
        let (base, seen) = fake_server(vec![tok("a"), (200, EVENT.to_string())]);
        let c = client(&base, connected_store());
        run(c.update_event(
            ("primary", "abc_20261021T120000Z/x"),
            "2026-10-21",
            540,
            600,
            "T",
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
        let err = run(c.list_day("2026-10-21", &tz())).unwrap_err();
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
        store.set_email(Some("me@example.com"));
        let c = client(&base, store.clone());
        run(c.disconnect()).unwrap();
        assert!(seen.lock().unwrap()[0].starts_with("POST /revoke?token=refresh-1"));
        assert_eq!(store.refresh_token().unwrap(), None);
        assert_eq!(store.email(), None);
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
            day.events[0].calendar_id,
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
            540,
            600,
            "T",
            &tz(),
        ))
        .unwrap();
        assert_eq!(e.calendar_id, "mest@group.calendar.google.com");
        let e = run(c.update_event(
            ("mest@group.calendar.google.com", "e1"),
            "2026-10-21",
            540,
            600,
            "T",
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
        let day = run(c.list_day("2026-10-21", &tz())).unwrap();
        assert_eq!(day.events[0].recurring_event_id.as_deref(), Some("s1"));
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
        run(c.list_day("2026-10-21", &tz())).unwrap();
        assert!(c.needs_reconnect());
        // disconnecting forgets it
        run(c.disconnect()).unwrap();
        assert!(!c.needs_reconnect());
    }

    #[test]
    fn a_token_response_without_scope_changes_nothing() {
        let (base, _) = fake_server(vec![tok("a"), page("", None)]);
        let store = connected_store();
        store.set_scope(Some("https://www.googleapis.com/auth/calendar.events"));
        let c = client(&base, store.clone());
        run(c.list_day("2026-10-21", &tz())).unwrap();
        assert!(c.needs_reconnect());
    }
}
