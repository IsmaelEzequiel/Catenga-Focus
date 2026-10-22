//! Tauri commands for Google Calendar (M5): connect/disconnect, the per-day cache
//! and event writes. The pure and HTTP parts live in `google` and `google_client`.

use crate::app_db::{AppDb, Project, ProjectRow};
use crate::commands::{AppDbHandle, IndexHandle, now_secs};
use crate::google::{self, CalEvent};
use crate::google_client::{Client, Credentials, Endpoints, Error, KeychainStore};
use crate::loopback;
use jiff::tz::TimeZone;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;

/// How long the browser consent may take before we stop listening.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const NOT_CONFIGURED: &str = "Google Calendar isn't configured in this build";
const DISCONNECTED: &str = "Google Calendar was disconnected \u{2014} connect again";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleStatus {
    pub configured: bool,
    pub connected: bool,
    pub email: Option<String>,
    /// Connected, but the token predates the calendar list scope: connect again.
    pub needs_reconnect: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarDay {
    pub events: Vec<CalEvent>,
    /// Unix ms of the last successful fetch; `None` when the day was never fetched.
    pub fetched_at: Option<i64>,
}

#[derive(Clone, Serialize)]
struct ChangedPayload {
    date: String,
}

#[derive(Clone, Serialize)]
struct ErrorPayload {
    date: String,
    message: String,
}

/// A runtime (dev) value wins over the compile-time (CI) one; blanks count as unset.
fn pick(runtime: Option<String>, built: Option<&str>) -> Option<String> {
    [runtime.as_deref(), built]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(str::to_string)
}

fn credentials() -> Option<Credentials> {
    let id = pick(
        std::env::var("CATENGA_GOOGLE_CLIENT_ID").ok(),
        option_env!("CATENGA_GOOGLE_CLIENT_ID"),
    )?;
    let secret = pick(
        std::env::var("CATENGA_GOOGLE_CLIENT_SECRET").ok(),
        option_env!("CATENGA_GOOGLE_CLIENT_SECRET"),
    )?;
    Some(Credentials {
        client_id: id,
        client_secret: secret,
    })
}

struct Inner {
    /// `None` when the build has no Google client credentials.
    client: Option<Client>,
    /// Cancel flag of the consent wait in progress, if any.
    cancel: Mutex<Option<Arc<AtomicBool>>>,
    /// Dates whose refresh is in flight.
    refreshing: Mutex<HashSet<String>>,
    /// Remembered keychain lookup, so day navigation does not hit the keychain.
    connected: Mutex<Option<bool>>,
    /// A calendar-list refresh is in flight.
    projects_refreshing: AtomicBool,
    /// Cache used only when the SQLite index is unavailable.
    memory: Mutex<HashMap<String, (Vec<CalEvent>, i64)>>,
}

#[derive(Clone)]
pub struct GoogleState(Arc<Inner>);

impl GoogleState {
    pub fn new() -> GoogleState {
        let client = credentials().and_then(|creds| {
            Client::new(creds, Endpoints::default(), Arc::new(KeychainStore))
                .map_err(|e| eprintln!("google calendar disabled: {e}"))
                .ok()
        });
        GoogleState(Arc::new(Inner {
            client,
            cancel: Mutex::new(None),
            refreshing: Mutex::new(HashSet::new()),
            connected: Mutex::new(None),
            projects_refreshing: AtomicBool::new(false),
            memory: Mutex::new(HashMap::new()),
        }))
    }

    fn client(&self) -> Result<&Client, String> {
        self.0.client.as_ref().ok_or_else(|| NOT_CONFIGURED.into())
    }

    fn connected(&self) -> bool {
        let Some(client) = &self.0.client else {
            return false;
        };
        *lock(&self.0.connected).get_or_insert_with(|| client.is_connected())
    }

    fn set_connected(&self, value: bool) {
        *lock(&self.0.connected) = Some(value);
    }

    fn status(&self) -> GoogleStatus {
        let connected = self.connected();
        GoogleStatus {
            configured: self.0.client.is_some(),
            connected,
            email: self
                .0
                .client
                .as_ref()
                .filter(|_| connected)
                .and_then(Client::stored_email),
            needs_reconnect: connected
                && self.0.client.as_ref().is_some_and(Client::needs_reconnect),
        }
    }

    /// Maps a client error to the user-facing text, noting a revoked connection.
    fn describe(&self, e: &Error) -> String {
        match e {
            Error::Disconnected | Error::NotConnected => {
                self.set_connected(false);
                DISCONNECTED.into()
            }
            Error::MissingScope => e.to_string(),
            Error::Message(m) => m.clone(),
        }
    }

    fn cache_get(&self, index: &IndexHandle, date: &str) -> Option<(Vec<CalEvent>, i64)> {
        match &index.0 {
            Ok(index) => index.calendar_day(date).unwrap_or_else(|e| {
                eprintln!("reading calendar cache: {e}");
                None
            }),
            Err(_) => lock(&self.0.memory).get(date).cloned(),
        }
    }

    fn cache_put(&self, index: &IndexHandle, date: &str, events: &[CalEvent], at: i64) {
        match &index.0 {
            Ok(index) => {
                if let Err(e) = index.replace_calendar_day(date, events, at) {
                    eprintln!("writing calendar cache: {e}");
                }
            }
            Err(_) => {
                lock(&self.0.memory).insert(date.to_string(), (events.to_vec(), at));
            }
        }
    }

    fn cache_clear(&self, index: &IndexHandle) {
        lock(&self.0.memory).clear();
        if let Ok(index) = &index.0
            && let Err(e) = index.clear_calendar()
        {
            eprintln!("clearing calendar cache: {e}");
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// `YYYY-MM-DD`, a real calendar date.
fn valid_date(s: &str) -> bool {
    s.len() == 10 && s.parse::<jiff::civil::Date>().is_ok()
}

fn check_date(s: &str) -> Result<(), String> {
    valid_date(s)
        .then_some(())
        .ok_or_else(|| format!("invalid date {s}"))
}

/// Inserts `event`, replacing the one with the same id, keeping start order.
fn upsert_event(events: &mut Vec<CalEvent>, event: CalEvent) {
    match events.iter_mut().find(|e| e.id == event.id) {
        Some(slot) => *slot = event,
        None => events.push(event),
    }
    events.sort_by_key(|e| (e.start, e.end));
}

/// Calendars whose events the timeline fetches: the primary one always, then
/// every live project the user shows. Without any project row yet, just `primary`.
fn calendars_to_fetch(rows: &[ProjectRow]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    match rows
        .iter()
        .find(|r| r.project.is_primary && !r.project.archived)
    {
        Some(r) => out.push(r.calendar_id.clone()),
        None => out.push("primary".to_string()),
    }
    for r in rows {
        if !r.project.archived && r.project.show_on_timeline && !out.contains(&r.calendar_id) {
            out.push(r.calendar_id.clone());
        }
    }
    out
}

/// The project row an event's calendar stands for; `primary` is the primary row.
fn row_of<'a>(rows: &'a [ProjectRow], calendar_id: &str) -> Option<&'a ProjectRow> {
    rows.iter().find(|r| {
        r.calendar_id == calendar_id || (calendar_id == "primary" && r.project.is_primary)
    })
}

/// Fills `project_id` and `color`, and drops the events of calendars the user hides.
fn enrich(events: Vec<CalEvent>, rows: &[ProjectRow]) -> Vec<CalEvent> {
    events
        .into_iter()
        .filter_map(|mut e| {
            if let Some(r) = row_of(rows, &e.calendar_id) {
                let shown =
                    r.project.is_primary || (r.project.show_on_timeline && !r.project.archived);
                if !shown {
                    return None;
                }
                e.project_id = Some(r.project.id);
                e.color = r.project.color.clone();
            }
            Some(e)
        })
        .collect()
}

fn rows_of(db: &AppDbHandle) -> Vec<ProjectRow> {
    match &db.0 {
        Ok(db) => db.project_rows().unwrap_or_else(|e| {
            eprintln!("reading projects: {e}");
            Vec::new()
        }),
        Err(_) => Vec::new(),
    }
}

/// CSRF `state`: url-safe, 1 to 128 characters.
fn valid_state(s: &str) -> bool {
    (1..=128).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~'))
}

/// The authorization code from the redirect, or why consent failed.
fn code_from_callback(params: &HashMap<String, String>, state: &str) -> Result<String, String> {
    if params.get("state").map(String::as_str) != Some(state) {
        return Err("state mismatch".into());
    }
    if let Some(error) = params.get("error") {
        return Err(if error == "access_denied" {
            "access denied".into()
        } else {
            error.clone()
        });
    }
    params
        .get("code")
        .filter(|c| !c.is_empty())
        .cloned()
        .ok_or_else(|| "Google did not send a code".into())
}

fn today() -> String {
    jiff::Zoned::now().date().to_string()
}

#[tauri::command]
pub fn google_status(google: State<GoogleState>) -> GoogleStatus {
    google.status()
}

#[tauri::command]
pub async fn google_connect(
    app: AppHandle,
    google: State<'_, GoogleState>,
    index: State<'_, IndexHandle>,
    db: State<'_, AppDbHandle>,
    code_challenge: String,
    code_verifier: String,
    state: String,
) -> Result<GoogleStatus, String> {
    let google = google.inner().clone();
    let client = google.client()?;
    if !google::valid_challenge(&code_challenge) {
        return Err("invalid code challenge".into());
    }
    if !google::valid_verifier(&code_verifier) {
        return Err("invalid code verifier".into());
    }
    if !valid_state(&state) {
        return Err("invalid state".into());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    // A new attempt replaces the previous one; its wait ends with "cancelled".
    if let Some(previous) = lock(&google.0.cancel).replace(cancel.clone()) {
        previous.store(true, Ordering::SeqCst);
    }
    let (listener, port) = loopback::bind().map_err(|e| format!("starting sign-in: {e}"))?;
    let redirect = format!("http://127.0.0.1:{port}/callback");
    let url = google::auth_url(&client_id_of(&google)?, &redirect, &code_challenge, &state);
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| format!("opening the browser: {e}"))?;
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let params = tauri::async_runtime::spawn_blocking(move || {
        loopback::wait_for_callback(&listener, deadline, &cancel).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    let code = code_from_callback(&params, &state)?;
    client
        .exchange_code(&code, &code_verifier, &redirect)
        .await
        .map_err(|e| google.describe(&e))?;
    google.set_connected(true);
    // One test call: proves the API works, fills the cache and finds the account email.
    let date = today();
    match client.list_day(&date, &TimeZone::system()).await {
        Ok(day) => {
            client.remember_email(day.email.as_deref());
            google.cache_put(&index, &date, &day.events, now_ms());
            // Best effort: the calendars become projects.
            if let Err(e) = refresh_projects(&app, &google, &db).await {
                eprintln!("loading calendars: {e}");
            }
            Ok(google.status())
        }
        Err(e) => {
            let message = google.describe(&e);
            let _ = client.disconnect().await;
            google.set_connected(false);
            Err(message)
        }
    }
}

/// The OAuth client id, for the consent URL.
fn client_id_of(google: &GoogleState) -> Result<String, String> {
    google.client()?;
    credentials()
        .map(|c| c.client_id)
        .ok_or_else(|| NOT_CONFIGURED.to_string())
}

#[tauri::command]
pub fn google_cancel_connect(google: State<GoogleState>) {
    if let Some(cancel) = lock(&google.0.cancel).take() {
        cancel.store(true, Ordering::SeqCst);
    }
}

#[tauri::command]
pub async fn google_disconnect(
    google: State<'_, GoogleState>,
    index: State<'_, IndexHandle>,
) -> Result<GoogleStatus, String> {
    let google = google.inner().clone();
    google
        .client()?
        .disconnect()
        .await
        .map_err(|e| e.to_string())?;
    google.set_connected(false);
    google.cache_clear(&index);
    Ok(google.status())
}

#[tauri::command]
pub fn calendar_day(
    google: State<GoogleState>,
    index: State<IndexHandle>,
    db: State<AppDbHandle>,
    date: String,
) -> Result<CalendarDay, String> {
    check_date(&date)?;
    let cached = if google.connected() {
        google.cache_get(&index, &date)
    } else {
        None
    };
    Ok(match cached {
        Some((events, at)) => CalendarDay {
            events: enrich(events, &rows_of(&db)),
            fetched_at: Some(at),
        },
        None => CalendarDay {
            events: vec![],
            fetched_at: None,
        },
    })
}

#[tauri::command]
pub fn calendar_refresh(
    app: AppHandle,
    google: State<GoogleState>,
    index: State<IndexHandle>,
    db: State<AppDbHandle>,
    date: String,
) -> Result<(), String> {
    check_date(&date)?;
    if !google.connected() {
        return Ok(());
    }
    // One fetch per date at a time.
    if !lock(&google.0.refreshing).insert(date.clone()) {
        return Ok(());
    }
    let (google, index) = (google.inner().clone(), IndexHandle(index.0.clone()));
    let db = AppDbHandle(db.0.clone());
    tauri::async_runtime::spawn(async move {
        refresh_day(&app, &google, &index, &db, &date).await;
        lock(&google.0.refreshing).remove(&date);
    });
    Ok(())
}

/// The day's events from every calendar to show. The primary one must succeed;
/// another calendar that fails is skipped (and logged). An event that sits in
/// several calendars is kept once, from the first.
async fn fetch_day(
    client: &Client,
    calendars: &[String],
    date: &str,
    tz: &TimeZone,
) -> Result<crate::google_client::DayEvents, Error> {
    let mut all: Vec<CalEvent> = Vec::new();
    let mut email = None;
    for (i, calendar) in calendars.iter().enumerate() {
        match client.list_day_in(calendar, date, tz).await {
            Ok(day) => {
                if i == 0 {
                    email = day.email;
                }
                for e in day.events {
                    if !all.iter().any(|x| x.id == e.id) {
                        all.push(e);
                    }
                }
            }
            Err(e) if i > 0 && matches!(e, Error::Message(_)) => {
                eprintln!("events of calendar {calendar}: {e}");
            }
            Err(e) => return Err(e),
        }
    }
    all.sort_by_key(|e| (e.start, e.end));
    Ok(crate::google_client::DayEvents { events: all, email })
}

async fn refresh_day(
    app: &AppHandle,
    google: &GoogleState,
    index: &IndexHandle,
    db: &AppDbHandle,
    date: &str,
) {
    let result = match google.client() {
        Ok(client) => fetch_day(
            client,
            &calendars_to_fetch(&rows_of(db)),
            date,
            &TimeZone::system(),
        )
        .await
        .map_err(|e| (google.describe(&e), e)),
        Err(m) => {
            emit_error(app, date, m);
            return;
        }
    };
    match result {
        Ok(day) => {
            if let Ok(client) = google.client()
                && day.email.is_some()
            {
                client.remember_email(day.email.as_deref());
            }
            let before = google.cache_get(index, date);
            let changed = before.is_none_or(|(events, _)| events != day.events);
            google.cache_put(index, date, &day.events, now_ms());
            if changed
                && let Err(e) = app.emit(
                    "calendar-changed",
                    ChangedPayload {
                        date: date.to_string(),
                    },
                )
            {
                eprintln!("emitting calendar-changed: {e}");
            }
        }
        Err((message, _)) => emit_error(app, date, message),
    }
}

fn emit_error(app: &AppHandle, date: &str, message: String) {
    let payload = ErrorPayload {
        date: date.to_string(),
        message,
    };
    if let Err(e) = app.emit("calendar-error", payload) {
        eprintln!("emitting calendar-error: {e}");
    }
}

fn check_write(date: &str, start: u32, end: u32, title: &str) -> Result<String, String> {
    check_date(date)?;
    if start >= end || end > 1440 {
        return Err("invalid time range".into());
    }
    let title = title.trim();
    if title.is_empty() {
        return Err("title is empty".into());
    }
    Ok(title.to_string())
}

/// Keeps an already-cached day in step with a write; a never-fetched day is left alone.
fn cache_event(google: &GoogleState, index: &IndexHandle, date: &str, event: &CalEvent) {
    if let Some((mut events, at)) = google.cache_get(index, date) {
        upsert_event(&mut events, event.clone());
        google.cache_put(index, date, &events, at);
    }
}

#[tauri::command]
pub async fn calendar_create(
    app: AppHandle,
    date: String,
    start: u32,
    end: u32,
    title: String,
    project_id: Option<i64>,
) -> Result<CalEvent, String> {
    let (google, index, db) = (
        app.state::<GoogleState>(),
        app.state::<IndexHandle>(),
        app.state::<AppDbHandle>(),
    );
    let title = check_write(&date, start, end, &title)?;
    let rows = rows_of(&db);
    let calendar = match project_id {
        Some(id) => db.get()?.project(id)?.calendar_id,
        None => primary_calendar(&rows),
    };
    let google = google.inner().clone();
    let event = google
        .client()?
        .create_event(&calendar, &date, start, end, &title, &TimeZone::system())
        .await
        .map_err(|e| google.describe(&e))?;
    cache_event(&google, &index, &date, &event);
    Ok(enrich_one(event, &rows_of(&db)))
}

#[tauri::command]
pub async fn calendar_update(
    app: AppHandle,
    date: String,
    event_id: String,
    start: u32,
    end: u32,
    title: String,
) -> Result<CalEvent, String> {
    let (google, index, db) = (
        app.state::<GoogleState>(),
        app.state::<IndexHandle>(),
        app.state::<AppDbHandle>(),
    );
    let title = check_write(&date, start, end, &title)?;
    if event_id.is_empty() {
        return Err("missing event id".into());
    }
    let google = google.inner().clone();
    // The event lives in the calendar the cache says (the primary one if unknown).
    let calendar = cached_calendar(&google, &index, &date, &event_id)
        .unwrap_or_else(|| primary_calendar(&rows_of(&db)));
    let event = google
        .client()?
        .update_event(
            (&calendar, &event_id),
            &date,
            start,
            end,
            &title,
            &TimeZone::system(),
        )
        .await
        .map_err(|e| google.describe(&e))?;
    cache_event(&google, &index, &date, &event);
    Ok(enrich_one(event, &rows_of(&db)))
}

#[tauri::command]
pub async fn calendar_move(
    google: State<'_, GoogleState>,
    index: State<'_, IndexHandle>,
    db: State<'_, AppDbHandle>,
    date: String,
    event_id: String,
    project_id: i64,
) -> Result<CalEvent, String> {
    check_date(&date)?;
    if event_id.is_empty() {
        return Err("missing event id".into());
    }
    let to = db.get()?.project(project_id)?.calendar_id;
    let google = google.inner().clone();
    let rows = rows_of(&db);
    let from = cached_calendar(&google, &index, &date, &event_id)
        .ok_or("Event not found; refresh the day and try again")?;
    let from = row_of(&rows, &from).map_or(from, |r| r.calendar_id.clone());
    let client = google.client()?;
    let event = if from == to {
        // Already there: hand back the cached event.
        google
            .cache_get(&index, &date)
            .and_then(|(events, _)| events.into_iter().find(|e| e.id == event_id))
            .ok_or("Event not found; refresh the day and try again")?
    } else {
        client
            .move_event(&from, &to, &event_id, &date, &TimeZone::system())
            .await
            .map_err(|e| google.describe(&e))?
    };
    cache_event(&google, &index, &date, &event);
    Ok(enrich_one(event, &rows_of(&db)))
}

fn primary_calendar(rows: &[ProjectRow]) -> String {
    rows.iter()
        .find(|r| r.project.is_primary && !r.project.archived)
        .map_or_else(|| "primary".to_string(), |r| r.calendar_id.clone())
}

/// The calendar a cached event belongs to.
fn cached_calendar(
    google: &GoogleState,
    index: &IndexHandle,
    date: &str,
    event_id: &str,
) -> Option<String> {
    google
        .cache_get(index, date)?
        .0
        .into_iter()
        .find(|e| e.id == event_id)
        .map(|e| e.calendar_id)
        .filter(|c| !c.is_empty())
}

/// A just-written event for the frontend: project and color filled, never hidden.
fn enrich_one(mut event: CalEvent, rows: &[ProjectRow]) -> CalEvent {
    if let Some(r) = row_of(rows, &event.calendar_id) {
        event.project_id = Some(r.project.id);
        event.color = r.project.color.clone();
    }
    event
}

// ------------------------------------------------------------------ projects

/// Reads Google's calendar list into the projects; `Ok(false)` when there was
/// nothing to do (not connected, token without the list scope, or a refresh
/// already running).
async fn load_projects(google: &GoogleState, db: &AppDb) -> Result<bool, String> {
    let client = match google.client() {
        Ok(c) if google.connected() => c,
        _ => return Ok(false),
    };
    if client.needs_reconnect() || google.0.projects_refreshing.swap(true, Ordering::SeqCst) {
        return Ok(false);
    }
    let result = match client.list_calendars().await {
        Ok(calendars) => db.upsert_calendars(&calendars, now_secs()).map(|()| true),
        // The status now says `needsReconnect`.
        Err(Error::MissingScope) => Ok(true),
        Err(e) => Err(google.describe(&e)),
    };
    google.0.projects_refreshing.store(false, Ordering::SeqCst);
    result
}

/// Refreshes the projects and tells the frontend (`projects-changed`) when anything ran.
async fn refresh_projects(
    app: &AppHandle,
    google: &GoogleState,
    db: &AppDbHandle,
) -> Result<Vec<Project>, String> {
    let db = db.get()?;
    if load_projects(google, &db).await?
        && let Err(e) = app.emit("projects-changed", serde_json::json!({}))
    {
        eprintln!("emitting projects-changed: {e}");
    }
    db.projects()
}

#[tauri::command]
pub async fn projects_refresh(
    app: AppHandle,
    google: State<'_, GoogleState>,
    db: State<'_, AppDbHandle>,
) -> Result<Vec<Project>, String> {
    refresh_projects(&app, google.inner(), &db).await
}

/// On app start: when connected, load the calendars without blocking anything.
pub fn refresh_projects_in_background(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let google = app.state::<GoogleState>();
        let db = app.state::<AppDbHandle>();
        if let Err(e) = refresh_projects(&app, google.inner(), &db).await {
            eprintln!("loading calendars: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::test_util::Env;

    fn ev(id: &str, start: u32, end: u32) -> CalEvent {
        CalEvent {
            id: id.into(),
            start,
            end,
            title: id.into(),
            calendar_id: String::new(),
            html_link: None,
            project_id: None,
            color: String::new(),
        }
    }

    fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn runtime_credentials_win_and_blanks_are_unset() {
        assert_eq!(pick(Some("dev".into()), Some("ci")).as_deref(), Some("dev"));
        assert_eq!(pick(None, Some("ci")).as_deref(), Some("ci"));
        assert_eq!(pick(Some("  ".into()), Some("ci")).as_deref(), Some("ci"));
        assert_eq!(pick(Some("".into()), Some("")), None);
        assert_eq!(pick(None, None), None);
    }

    #[test]
    fn dates_must_be_real_iso_dates() {
        assert!(valid_date("2026-10-21"));
        for bad in [
            "",
            "2026-13-01",
            "2026-02-30",
            "2026-1-1",
            "21/10/2026",
            "2026-10-21 ",
        ] {
            assert!(!valid_date(bad), "{bad}");
        }
    }

    #[test]
    fn upsert_replaces_by_id_and_keeps_order() {
        let mut events = vec![ev("a", 600, 660), ev("b", 700, 760)];
        upsert_event(&mut events, ev("c", 100, 200));
        upsert_event(&mut events, ev("a", 800, 900));
        let ids: Vec<_> = events.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["c", "b", "a"]);
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn callback_state_must_match() {
        let p = params(&[("code", "4/x"), ("state", "other")]);
        assert_eq!(code_from_callback(&p, "mine"), Err("state mismatch".into()));
        assert_eq!(
            code_from_callback(&params(&[("code", "4/x")]), "mine"),
            Err("state mismatch".into())
        );
    }

    #[test]
    fn callback_maps_errors_and_returns_the_code() {
        let ok = params(&[("code", "4/0Ab"), ("state", "s")]);
        assert_eq!(code_from_callback(&ok, "s"), Ok("4/0Ab".into()));
        let denied = params(&[("error", "access_denied"), ("state", "s")]);
        assert_eq!(
            code_from_callback(&denied, "s"),
            Err("access denied".into())
        );
        let other = params(&[("error", "invalid_scope"), ("state", "s")]);
        assert_eq!(code_from_callback(&other, "s"), Err("invalid_scope".into()));
        let empty = params(&[("code", ""), ("state", "s")]);
        assert!(code_from_callback(&empty, "s").is_err());
    }

    #[test]
    fn state_validation() {
        assert!(valid_state("abcDEF-_123"));
        assert!(!valid_state(""));
        assert!(!valid_state(&"a".repeat(129)));
        assert!(!valid_state("a&b"));
    }

    #[test]
    fn writes_are_validated() {
        assert_eq!(check_write("2026-10-21", 540, 600, "  T "), Ok("T".into()));
        assert!(check_write("2026-10-21", 600, 600, "T").is_err());
        assert!(check_write("2026-10-21", 600, 1441, "T").is_err());
        assert!(check_write("2026-10-21", 540, 600, "  ").is_err());
        assert!(check_write("nope", 540, 600, "T").is_err());
    }

    #[test]
    fn status_without_credentials_is_not_configured() {
        let state = GoogleState(Arc::new(Inner {
            client: None,
            cancel: Mutex::new(None),
            refreshing: Mutex::new(HashSet::new()),
            connected: Mutex::new(None),
            projects_refreshing: AtomicBool::new(false),
            memory: Mutex::new(HashMap::new()),
        }));
        assert_eq!(
            state.status(),
            GoogleStatus {
                configured: false,
                connected: false,
                email: None,
                needs_reconnect: false
            }
        );
        assert_eq!(state.client().err().as_deref(), Some(NOT_CONFIGURED));
    }

    fn with_client(client: Option<Client>) -> GoogleState {
        GoogleState(Arc::new(Inner {
            client,
            cancel: Mutex::new(None),
            refreshing: Mutex::new(HashSet::new()),
            connected: Mutex::new(None),
            projects_refreshing: AtomicBool::new(false),
            memory: Mutex::new(HashMap::new()),
        }))
    }

    fn bare() -> GoogleState {
        with_client(None)
    }

    #[test]
    fn cache_goes_through_the_index_and_falls_back_to_memory() {
        let env = Env::new();
        let with_index = IndexHandle(Ok(Arc::new(env.open())));
        let state = bare();
        assert_eq!(state.cache_get(&with_index, "2026-10-21"), None);
        state.cache_put(&with_index, "2026-10-21", &[ev("a", 1, 2)], 9);
        assert_eq!(
            state.cache_get(&with_index, "2026-10-21"),
            Some((vec![ev("a", 1, 2)], 9))
        );
        state.cache_clear(&with_index);
        assert_eq!(state.cache_get(&with_index, "2026-10-21"), None);

        let without = IndexHandle(Err("broken".into()));
        state.cache_put(&without, "2026-10-21", &[ev("m", 1, 2)], 3);
        assert_eq!(
            state.cache_get(&without, "2026-10-21"),
            Some((vec![ev("m", 1, 2)], 3))
        );
        state.cache_clear(&without);
        assert_eq!(state.cache_get(&without, "2026-10-21"), None);
    }

    #[test]
    fn writes_update_only_days_that_are_cached() {
        let without = IndexHandle(Err("broken".into()));
        let state = bare();
        cache_event(&state, &without, "2026-10-21", &ev("x", 1, 2));
        assert_eq!(state.cache_get(&without, "2026-10-21"), None);
        state.cache_put(&without, "2026-10-21", &[ev("a", 100, 200)], 5);
        cache_event(&state, &without, "2026-10-21", &ev("x", 1, 2));
        let (events, at) = state.cache_get(&without, "2026-10-21").unwrap();
        assert_eq!((events.len(), at), (2, 5));
        assert_eq!(events[0].id, "x");
    }

    #[test]
    fn serialized_shapes_match_the_contract() {
        let status = GoogleStatus {
            configured: true,
            connected: true,
            email: Some("a@b.c".into()),
            needs_reconnect: true,
        };
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::json!({"configured":true,"connected":true,"email":"a@b.c","needsReconnect":true})
        );
        let day = CalendarDay {
            events: vec![ev("a", 1, 2)],
            fetched_at: Some(5),
        };
        assert_eq!(
            serde_json::to_value(&day).unwrap(),
            serde_json::json!({"events":[{"id":"a","start":1,"end":2,"title":"a","projectId":null,"color":""}],"fetchedAt":5})
        );
    }

    // ---- projects

    use crate::app_db::test_util::{cal, fixture};
    use crate::google_client::TokenStore;
    use crate::google_client::test_util::{MemStore, client as fake_client, fake_server};

    fn row(id: i64, cal_id: &str, primary: bool, shown: bool, archived: bool) -> ProjectRow {
        ProjectRow {
            calendar_id: cal_id.into(),
            project: Project {
                id,
                google_calendar_id: cal_id.into(),
                name: cal_id.into(),
                color: format!("#00000{id}"),
                show_on_timeline: shown,
                is_primary: primary,
                archived,
            },
        }
    }

    fn rows() -> Vec<ProjectRow> {
        vec![
            row(1, "me@x", true, true, false),
            row(2, "mest@x", false, true, false),
            row(3, "hidden@x", false, false, false),
            row(4, "old@x", false, true, true),
        ]
    }

    #[test]
    fn timeline_fetches_primary_first_then_shown_live_calendars() {
        assert_eq!(calendars_to_fetch(&rows()), ["me@x", "mest@x"]);
        assert_eq!(calendars_to_fetch(&[]), ["primary"]);
        // a primary the user hid is still fetched; an unknown primary falls back to the alias
        let only_others = vec![row(2, "mest@x", false, true, false)];
        assert_eq!(calendars_to_fetch(&only_others), ["primary", "mest@x"]);
        let hidden_primary = vec![row(1, "me@x", true, false, false)];
        assert_eq!(calendars_to_fetch(&hidden_primary), ["me@x"]);
    }

    fn ev_in(id: &str, calendar: &str) -> CalEvent {
        CalEvent {
            calendar_id: calendar.into(),
            ..ev(id, 1, 2)
        }
    }

    #[test]
    fn events_get_project_and_color_and_hidden_calendars_drop_out() {
        let events = vec![
            ev_in("a", "me@x"),
            ev_in("b", "mest@x"),
            ev_in("c", "hidden@x"),
            ev_in("d", "old@x"),
            ev_in("e", "primary"),
            ev_in("f", "stranger@x"),
        ];
        let out = enrich(events, &rows());
        let got: Vec<_> = out
            .iter()
            .map(|e| (e.id.as_str(), e.project_id, e.color.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("a", Some(1), "#000001"),
                ("b", Some(2), "#000002"),
                ("e", Some(1), "#000001"),
                ("f", None, ""),
            ]
        );
        // a hidden primary is still shown
        let hidden_primary = vec![row(1, "me@x", true, false, false)];
        assert_eq!(enrich(vec![ev_in("a", "me@x")], &hidden_primary).len(), 1);
    }

    fn two_events(id: &str, title: &str) -> (u16, String) {
        (
            200,
            format!(
                r#"{{"summary":"me@example.com","items":[{{"id":"{id}","summary":"{title}","start":{{"dateTime":"2026-10-21T09:00:00-03:00"}},"end":{{"dateTime":"2026-10-21T10:00:00-03:00"}}}}]}}"#
            ),
        )
    }

    fn tok() -> (u16, String) {
        (200, r#"{"access_token":"a","expires_in":3600}"#.into())
    }

    fn connected_client(base: &str) -> Client {
        let store = Arc::new(MemStore::default());
        store.set_refresh_token("r").unwrap();
        fake_client(base, store)
    }

    fn sp() -> TimeZone {
        TimeZone::get("America/Sao_Paulo").unwrap()
    }

    fn run<F: std::future::Future>(f: F) -> F::Output {
        tauri::async_runtime::block_on(f)
    }

    #[test]
    fn fetch_day_merges_calendars_keeps_the_first_duplicate_and_tags_calendar_ids() {
        let (base, seen) = fake_server(vec![
            tok(),
            two_events("same", "Primary copy"),
            two_events("same", "Mestrado copy"),
        ]);
        let c = connected_client(&base);
        let cals = vec!["me@x".to_string(), "mest@x".to_string()];
        let day = run(fetch_day(&c, &cals, "2026-10-21", &sp())).unwrap();
        assert_eq!(day.events.len(), 1);
        assert_eq!(
            (
                day.events[0].title.as_str(),
                day.events[0].calendar_id.as_str()
            ),
            ("Primary copy", "me@x")
        );
        assert_eq!(day.email.as_deref(), Some("me@example.com"));
        let reqs = seen.lock().unwrap();
        assert!(reqs[1].starts_with("GET /calendar/v3/calendars/me@x/events?"));
        assert!(reqs[2].starts_with("GET /calendar/v3/calendars/mest@x/events?"));
    }

    #[test]
    fn fetch_day_unions_distinct_events_in_time_order() {
        let (base, _) = fake_server(vec![tok(), two_events("a", "A"), two_events("b", "B")]);
        let c = connected_client(&base);
        let cals = vec!["me@x".to_string(), "mest@x".to_string()];
        let day = run(fetch_day(&c, &cals, "2026-10-21", &sp())).unwrap();
        let ids: Vec<_> = day
            .events
            .iter()
            .map(|e| (e.id.as_str(), e.calendar_id.as_str()))
            .collect();
        assert_eq!(ids, [("a", "me@x"), ("b", "mest@x")]);
    }

    #[test]
    fn a_failing_secondary_calendar_is_skipped_but_a_failing_primary_is_not() {
        let boom = (500, r#"{"error":{"message":"Backend Error"}}"#.to_string());
        let (base, _) = fake_server(vec![tok(), two_events("a", "A"), boom.clone()]);
        let c = connected_client(&base);
        let cals = vec!["me@x".to_string(), "mest@x".to_string()];
        let day = run(fetch_day(&c, &cals, "2026-10-21", &sp())).unwrap();
        assert_eq!(day.events.len(), 1);
        let (base, _) = fake_server(vec![tok(), boom]);
        let c = connected_client(&base);
        let err = run(fetch_day(&c, &cals, "2026-10-21", &sp())).unwrap_err();
        assert_eq!(err.to_string(), "Backend Error");
    }

    fn cal_list(items: &str, next: Option<&str>) -> (u16, String) {
        let next = next
            .map(|t| format!(r#","nextPageToken":"{t}""#))
            .unwrap_or_default();
        (200, format!(r#"{{"items":[{items}]{next}}}"#))
    }

    #[test]
    fn load_projects_upserts_across_pages_and_archives_the_missing() {
        let f = fixture();
        f.db.upsert_calendars(&[cal("gone@x", "Gone", false, true)], 1)
            .unwrap();
        let (base, _) = fake_server(vec![
            tok(),
            cal_list(
                r##"{"id":"me@x","summary":"me@x","primary":true,"selected":true,"backgroundColor":"#111111"}"##,
                Some("P2"),
            ),
            cal_list(
                r##"{"id":"mest@x","summary":"Mestrado","selected":true,"backgroundColor":"#222222"}"##,
                None,
            ),
        ]);
        let state = with_client(Some(connected_client(&base)));
        assert!(run(load_projects(&state, &f.db)).unwrap());
        let projects = f.db.projects().unwrap();
        let got: Vec<_> = projects
            .iter()
            .map(|p| (p.name.as_str(), p.is_primary, p.archived))
            .collect();
        assert_eq!(
            got,
            [
                ("me@x", true, false),
                ("Mestrado", false, false),
                ("Gone", false, true)
            ]
        );
        assert!(!state.0.projects_refreshing.load(Ordering::SeqCst));
    }

    #[test]
    fn load_projects_does_nothing_when_disconnected_or_in_need_of_reconnect() {
        let f = fixture();
        // not connected
        let (base, seen) = fake_server(vec![]);
        let disconnected = fake_client(&base, Arc::new(MemStore::default()));
        let state = with_client(Some(disconnected));
        assert!(!run(load_projects(&state, &f.db)).unwrap());
        // connected, token without the list scope
        let store = Arc::new(MemStore::default());
        store.set_refresh_token("r").unwrap();
        store.set_scope(Some(crate::google::SCOPE));
        let state = with_client(Some(fake_client(&base, store)));
        assert!(!run(load_projects(&state, &f.db)).unwrap());
        assert!(seen.lock().unwrap().is_empty());
        assert!(state.status().needs_reconnect);
        // not configured
        assert!(!run(load_projects(&bare(), &f.db)).unwrap());
    }

    #[test]
    fn load_projects_reports_google_errors_and_releases_the_guard() {
        let f = fixture();
        let (base, _) = fake_server(vec![
            tok(),
            (500, r#"{"error":{"message":"Backend Error"}}"#.into()),
        ]);
        let state = with_client(Some(connected_client(&base)));
        assert_eq!(
            run(load_projects(&state, &f.db)).unwrap_err(),
            "Backend Error"
        );
        assert!(!state.0.projects_refreshing.load(Ordering::SeqCst));
        assert!(f.db.projects().unwrap().is_empty());
    }

    #[test]
    fn a_missing_scope_answer_is_not_an_error() {
        let f = fixture();
        let (base, _) = fake_server(vec![
            tok(),
            (
                403,
                r#"{"error":{"message":"Request had insufficient authentication scopes."}}"#.into(),
            ),
        ]);
        let state = with_client(Some(connected_client(&base)));
        assert!(run(load_projects(&state, &f.db)).unwrap());
        assert!(state.status().needs_reconnect);
    }

    #[test]
    fn primary_calendar_prefers_the_real_id() {
        assert_eq!(primary_calendar(&rows()), "me@x");
        assert_eq!(primary_calendar(&[]), "primary");
    }

    #[test]
    fn cached_calendar_and_enrich_one() {
        let without = IndexHandle(Err("broken".into()));
        let state = bare();
        state.cache_put(
            &without,
            "2026-10-21",
            &[ev_in("a", "mest@x"), ev("b", 1, 2)],
            5,
        );
        assert_eq!(
            cached_calendar(&state, &without, "2026-10-21", "a").as_deref(),
            Some("mest@x")
        );
        assert_eq!(
            cached_calendar(&state, &without, "2026-10-21", "b"),
            None,
            "no calendar recorded"
        );
        assert_eq!(cached_calendar(&state, &without, "2026-10-21", "zz"), None);
        let e = enrich_one(ev_in("a", "hidden@x"), &rows());
        assert_eq!((e.project_id, e.color.as_str()), (Some(3), "#000003"));
    }
}
