//! Tauri commands for Google Calendar (M5): connect/disconnect, the per-day cache
//! and event writes. The pure and HTTP parts live in `google` and `google_client`.

use crate::app_db::{AppDb, Calendar, Project, ProjectKind, ProjectRow, project_key};
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
    /// A calendar/series refresh is in flight.
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

/// Removes the event with this id; false when it was not there.
fn remove_event(events: &mut Vec<CalEvent>, id: &str) -> bool {
    let before = events.len();
    events.retain(|e| e.id != id);
    events.len() != before
}

/// Calendars whose events the timeline fetches: the primary one always, then
/// every live calendar the user shows. Without any calendar row yet, just `primary`.
fn calendars_to_fetch(calendars: &[Calendar]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    match calendars.iter().find(|c| c.is_primary && !c.archived) {
        Some(c) => out.push(c.google_calendar_id.clone()),
        None => out.push("primary".to_string()),
    }
    for c in calendars {
        if !c.archived && c.show_on_timeline && !out.contains(&c.google_calendar_id) {
            out.push(c.google_calendar_id.clone());
        }
    }
    out
}

/// The calendar row an event's calendar stands for; `primary` is the primary row.
fn calendar_of<'a>(calendars: &'a [Calendar], calendar_id: &str) -> Option<&'a Calendar> {
    calendars
        .iter()
        .find(|c| c.google_calendar_id == calendar_id || (calendar_id == "primary" && c.is_primary))
}

/// What the app database knows, for decorating events.
#[derive(Default)]
struct Known {
    calendars: Vec<Calendar>,
    projects: Vec<ProjectRow>,
}

fn known(db: &AppDbHandle) -> Known {
    match &db.0 {
        Ok(db) => Known {
            calendars: db.calendars().unwrap_or_else(|e| {
                eprintln!("reading calendars: {e}");
                Vec::new()
            }),
            projects: db.project_rows().unwrap_or_else(|e| {
                eprintln!("reading projects: {e}");
                Vec::new()
            }),
        },
        Err(_) => Known::default(),
    }
}

/// Fills `color` (the calendar's), `project_key` and `project_id`. An instance of
/// a series always has its series key; a one-off event has one only once the
/// user made it a project.
fn decorate(event: &mut CalEvent, known: &Known) {
    if let Some(c) = calendar_of(&known.calendars, &event.calendar_id) {
        event.color = c.color.clone();
    }
    let key = match &event.recurring_event_id {
        Some(rid) => Some(project_key(ProjectKind::Series, rid)),
        None => {
            let key = project_key(ProjectKind::Event, &event.id);
            known
                .projects
                .iter()
                .any(|p| p.project.key == key)
                .then_some(key)
        }
    };
    event.project_id = key.as_ref().and_then(|k| {
        known
            .projects
            .iter()
            .find(|p| &p.project.key == k)
            .map(|p| p.project.id)
    });
    event.project_key = key;
}

/// Decorates the events and drops the ones of calendars the user hides.
fn enrich(events: Vec<CalEvent>, known: &Known) -> Vec<CalEvent> {
    events
        .into_iter()
        .filter_map(|mut e| {
            if let Some(c) = calendar_of(&known.calendars, &e.calendar_id) {
                let shown = c.is_primary || (c.show_on_timeline && !c.archived);
                if !shown {
                    return None;
                }
            }
            decorate(&mut e, known);
            Some(e)
        })
        .collect()
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
            events: enrich(events, &known(&db)),
            fetched_at: Some(at),
        },
        None => CalendarDay {
            events: vec![],
            fetched_at: None,
        },
    })
}

/// Today's cached events as the timeline shows them, for the menu bar item. Empty
/// when Google is not connected or the day was never fetched.
pub fn cached_events(app: &AppHandle, date: &str) -> Vec<CalEvent> {
    let google = app.state::<GoogleState>();
    if !google.connected() {
        return Vec::new();
    }
    let index = app.state::<IndexHandle>();
    let db = app.state::<AppDbHandle>();
    google
        .cache_get(&index, date)
        .map(|(events, _)| enrich(events, &known(&db)))
        .unwrap_or_default()
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
            &calendars_to_fetch(&known(db).calendars),
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
            // Projects come only from `projects_refresh`: an instance doesn't say how often
            // its series repeats, and only series repeating at least weekly are projects.
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

fn emit_changed(app: &AppHandle, event: &str) {
    if let Err(e) = app.emit(event, serde_json::json!({})) {
        eprintln!("emitting {event}: {e}");
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

/// Drops a deleted event from an already-cached day; a never-fetched day is left alone.
fn uncache_event(google: &GoogleState, index: &IndexHandle, date: &str, event_id: &str) {
    if let Some((mut events, at)) = google.cache_get(index, date)
        && remove_event(&mut events, event_id)
    {
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
) -> Result<CalEvent, String> {
    let (google, index, db) = (
        app.state::<GoogleState>(),
        app.state::<IndexHandle>(),
        app.state::<AppDbHandle>(),
    );
    let title = check_write(&date, start, end, &title)?;
    let calendar = primary_calendar(&known(&db).calendars);
    let google = google.inner().clone();
    let event = google
        .client()?
        .create_event(&calendar, &date, start, end, &title, &TimeZone::system())
        .await
        .map_err(|e| google.describe(&e))?;
    cache_event(&google, &index, &date, &event);
    Ok(enrich_one(event, &known(&db)))
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
        .unwrap_or_else(|| primary_calendar(&known(&db).calendars));
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
    Ok(enrich_one(event, &known(&db)))
}

/// Deletes the event from Google Calendar (only that occurrence of a repeating series), then from the day cache.
#[tauri::command]
pub async fn calendar_delete(app: AppHandle, date: String, event_id: String) -> Result<(), String> {
    let (google, index, db) = (
        app.state::<GoogleState>(),
        app.state::<IndexHandle>(),
        app.state::<AppDbHandle>(),
    );
    check_date(&date)?;
    if event_id.is_empty() {
        return Err("missing event id".into());
    }
    let google = google.inner().clone();
    let calendar = cached_calendar(&google, &index, &date, &event_id)
        .unwrap_or_else(|| primary_calendar(&known(&db).calendars));
    google
        .client()?
        .delete_event(&calendar, &event_id)
        .await
        .map_err(|e| google.describe(&e))?;
    uncache_event(&google, &index, &date, &event_id);
    if let Err(e) = app.emit("calendar-changed", ChangedPayload { date }) {
        eprintln!("emitting calendar-changed: {e}");
    }
    Ok(())
}

fn primary_calendar(calendars: &[Calendar]) -> String {
    calendars
        .iter()
        .find(|c| c.is_primary && !c.archived)
        .map_or_else(|| "primary".to_string(), |c| c.google_calendar_id.clone())
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

/// A just-written event for the frontend: color and project filled, never hidden.
fn enrich_one(mut event: CalEvent, known: &Known) -> CalEvent {
    decorate(&mut event, known);
    event
}

// ------------------------------------------------------------------ projects

/// How far back repeating series are looked for.
const SERIES_LOOKBACK_DAYS: i64 = 90;

/// `timeMin` for the series search: the start of the day `SERIES_LOOKBACK_DAYS`
/// before `today`, as RFC 3339 with offset.
fn series_time_min(today: jiff::civil::Date, tz: &TimeZone) -> Result<String, String> {
    use jiff::ToSpan;
    let from = today
        .checked_sub(SERIES_LOOKBACK_DAYS.days())
        .map_err(|e| e.to_string())?;
    google::day_bounds(&from.to_string(), tz).map(|(min, _)| min)
}

/// Reads Google's calendar list into the calendars, then the repeating series of
/// the calendars the timeline shows into the projects. `Ok(false)` when there was
/// nothing to do (not connected, token without the list scope, or a refresh
/// already running). A calendar whose series can't be read is skipped (its
/// projects stay as they are); only when none could be read is it an error.
async fn load_projects(google: &GoogleState, db: &AppDb) -> Result<bool, String> {
    let client = match google.client() {
        Ok(c) if google.connected() => c,
        _ => return Ok(false),
    };
    if client.needs_reconnect() || google.0.projects_refreshing.swap(true, Ordering::SeqCst) {
        return Ok(false);
    }
    let result = load_projects_inner(google, client, db).await;
    google.0.projects_refreshing.store(false, Ordering::SeqCst);
    result
}

async fn load_projects_inner(
    google: &GoogleState,
    client: &Client,
    db: &AppDb,
) -> Result<bool, String> {
    let calendars = match client.list_calendars().await {
        Ok(calendars) => calendars,
        // The status now says `needsReconnect`.
        Err(Error::MissingScope) => return Ok(true),
        Err(e) => return Err(google.describe(&e)),
    };
    let now = now_secs();
    db.upsert_calendars(&calendars, now)?;
    let time_min = series_time_min(jiff::Zoned::now().date(), &TimeZone::system())?;
    let to_fetch = calendars_to_fetch(&db.calendars()?);
    let mut found = Vec::new();
    let mut first_error = None;
    for calendar in to_fetch {
        match client.list_series(&calendar, &time_min).await {
            Ok(series) => found.push((calendar, series)),
            Err(Error::MissingScope) => return Ok(true),
            Err(e @ (Error::Disconnected | Error::NotConnected)) => {
                return Err(google.describe(&e));
            }
            Err(e) => {
                eprintln!("series of calendar {calendar}: {e}");
                first_error.get_or_insert(google.describe(&e));
            }
        }
    }
    if found.is_empty()
        && let Some(message) = first_error
    {
        return Err(message);
    }
    db.sync_series(&found, now)?;
    Ok(true)
}

/// Refreshes calendars and projects and tells the frontend (`calendars-changed`,
/// `projects-changed`) when anything ran.
async fn refresh_projects(
    app: &AppHandle,
    google: &GoogleState,
    db: &AppDbHandle,
) -> Result<Vec<Project>, String> {
    let db = db.get()?;
    if load_projects(google, &db).await? {
        emit_changed(app, "calendars-changed");
        emit_changed(app, "projects-changed");
    }
    // Goals synced from another computer may have been waiting for their project.
    if db.resolve_pending_projects()? {
        emit_changed(app, "goals-changed");
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

/// Makes the cached one-off event `event_id` of `date` a project.
fn event_project(
    google: &GoogleState,
    index: &IndexHandle,
    db: &AppDb,
    date: &str,
    event_id: &str,
    now: i64,
) -> Result<Vec<Project>, String> {
    check_date(date)?;
    let event = google
        .cache_get(index, date)
        .and_then(|(events, _)| events.into_iter().find(|e| e.id == event_id))
        .ok_or("Event not found; refresh the day and try again")?;
    if event.recurring_event_id.is_some() {
        return Err("This event repeats; its series is already a project".into());
    }
    let calendar = if event.calendar_id.is_empty() {
        "primary"
    } else {
        &event.calendar_id
    };
    db.project_from_event(calendar, &event.id, &event.title, now)?;
    db.projects()
}

#[tauri::command]
pub fn project_from_event(
    app: AppHandle,
    google: State<GoogleState>,
    index: State<IndexHandle>,
    db: State<AppDbHandle>,
    date: String,
    event_id: String,
) -> Result<Vec<Project>, String> {
    let projects = event_project(&google, &index, &*db.get()?, &date, &event_id, now_secs())?;
    emit_changed(&app, "projects-changed");
    Ok(projects)
}

/// On app start: when connected, load the calendars and projects without blocking anything.
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
            recurring_event_id: None,
            project_key: None,
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
    fn removing_an_event_drops_only_that_one() {
        let mut events = vec![ev("a", 1, 2), ev("b", 3, 4)];
        assert!(remove_event(&mut events, "a"));
        assert_eq!(events, vec![ev("b", 3, 4)]);
        assert!(!remove_event(&mut events, "zzz"));
        assert_eq!(events, vec![ev("b", 3, 4)]);
    }

    #[test]
    fn uncache_event_updates_only_days_that_are_cached() {
        let without = IndexHandle(Err("broken".into()));
        let state = bare();
        uncache_event(&state, &without, "2026-10-21", "a");
        assert_eq!(state.cache_get(&without, "2026-10-21"), None);
        state.cache_put(
            &without,
            "2026-10-21",
            &[ev("a", 100, 200), ev("b", 300, 400)],
            5,
        );
        uncache_event(&state, &without, "2026-10-21", "a");
        assert_eq!(
            state.cache_get(&without, "2026-10-21"),
            Some((vec![ev("b", 300, 400)], 5))
        );
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
            serde_json::json!({"events":[{"id":"a","start":1,"end":2,"title":"a","projectKey":null,"projectId":null,"color":""}],"fetchedAt":5})
        );
    }

    // ---- calendars and projects

    use crate::app_db::test_util::{add_series, cal, fixture};
    use crate::google_client::TokenStore;
    use crate::google_client::test_util::{MemStore, client as fake_client, fake_server};

    fn calendar(id: i64, cal_id: &str, primary: bool, shown: bool, archived: bool) -> Calendar {
        Calendar {
            id,
            google_calendar_id: cal_id.into(),
            name: format!("name {cal_id}"),
            color: format!("#00000{id}"),
            show_on_timeline: shown,
            is_primary: primary,
            archived,
        }
    }

    fn calendars() -> Vec<Calendar> {
        vec![
            calendar(1, "me@x", true, true, false),
            calendar(2, "mest@x", false, true, false),
            calendar(3, "hidden@x", false, false, false),
            calendar(4, "old@x", false, true, true),
        ]
    }

    fn project(id: i64, kind: ProjectKind, google_id: &str) -> ProjectRow {
        ProjectRow {
            project: Project {
                id,
                kind,
                key: project_key(kind, google_id),
                name: google_id.into(),
                color: "#000000".into(),
                calendar_name: "c".into(),
                archived: false,
            },
            google_id: google_id.into(),
            calendar_id: "me@x".into(),
        }
    }

    fn known_fixture() -> Known {
        Known {
            calendars: calendars(),
            projects: vec![
                project(10, ProjectKind::Series, "s1"),
                project(11, ProjectKind::Event, "one"),
            ],
        }
    }

    #[test]
    fn timeline_fetches_primary_first_then_shown_live_calendars() {
        assert_eq!(calendars_to_fetch(&calendars()), ["me@x", "mest@x"]);
        assert_eq!(calendars_to_fetch(&[]), ["primary"]);
        // a primary the user hid is still fetched; an unknown primary falls back to the alias
        let only_others = vec![calendar(2, "mest@x", false, true, false)];
        assert_eq!(calendars_to_fetch(&only_others), ["primary", "mest@x"]);
        let hidden_primary = vec![calendar(1, "me@x", true, false, false)];
        assert_eq!(calendars_to_fetch(&hidden_primary), ["me@x"]);
    }

    fn ev_in(id: &str, calendar: &str) -> CalEvent {
        CalEvent {
            calendar_id: calendar.into(),
            ..ev(id, 1, 2)
        }
    }

    fn instance(id: &str, calendar: &str, series: &str) -> CalEvent {
        CalEvent {
            recurring_event_id: Some(series.into()),
            ..ev_in(id, calendar)
        }
    }

    #[test]
    fn events_get_calendar_color_and_hidden_calendars_drop_out() {
        let events = vec![
            ev_in("a", "me@x"),
            ev_in("b", "mest@x"),
            ev_in("c", "hidden@x"),
            ev_in("d", "old@x"),
            ev_in("e", "primary"),
            ev_in("f", "stranger@x"),
        ];
        let out = enrich(events, &known_fixture());
        let got: Vec<_> = out
            .iter()
            .map(|e| (e.id.as_str(), e.color.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("a", "#000001"),
                ("b", "#000002"),
                ("e", "#000001"),
                ("f", "")
            ]
        );
        // a hidden primary is still shown
        let hidden_primary = Known {
            calendars: vec![calendar(1, "me@x", true, false, false)],
            projects: vec![],
        };
        assert_eq!(enrich(vec![ev_in("a", "me@x")], &hidden_primary).len(), 1);
    }

    #[test]
    fn instances_get_their_series_key_and_the_project_id_when_the_row_exists() {
        let k = known_fixture();
        let out = enrich(
            vec![
                instance("s1_1", "me@x", "s1"),
                // renamed occurrence: still its series
                CalEvent {
                    title: "Renamed".into(),
                    ..instance("s1_2", "me@x", "s1")
                },
                // a series that is not a project (yet): key without id
                instance("n_1", "mest@x", "newseries"),
            ],
            &k,
        );
        let got: Vec<_> = out
            .iter()
            .map(|e| (e.project_key.as_deref(), e.project_id))
            .collect();
        assert_eq!(
            got,
            [
                (Some("series:s1"), Some(10)),
                (Some("series:s1"), Some(10)),
                (Some("series:newseries"), None)
            ]
        );
    }

    #[test]
    fn one_off_events_have_a_key_only_when_made_a_project() {
        let k = known_fixture();
        let out = enrich(vec![ev_in("one", "me@x"), ev_in("other", "me@x")], &k);
        assert_eq!(
            (out[0].project_key.as_deref(), out[0].project_id),
            (Some("event:one"), Some(11))
        );
        assert_eq!(
            (out[1].project_key.clone(), out[1].project_id),
            (None, None)
        );
    }

    #[test]
    fn enrich_one_never_hides_and_fills_color_and_project() {
        let e = enrich_one(instance("x", "hidden@x", "s1"), &known_fixture());
        assert_eq!(
            (e.color.as_str(), e.project_key.as_deref(), e.project_id),
            ("#000003", Some("series:s1"), Some(10))
        );
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

    fn run<F: std::future::Future>(f: F) -> F::Output {
        tauri::async_runtime::block_on(f)
    }

    fn sp() -> TimeZone {
        TimeZone::get("America/Sao_Paulo").unwrap()
    }

    fn tok() -> (u16, String) {
        (
            200,
            r#"{"access_token":"access-1","expires_in":3600}"#.to_string(),
        )
    }

    fn connected_client(base: &str) -> Client {
        let store = Arc::new(MemStore::default());
        store.set_refresh_token("refresh-1").unwrap();
        fake_client(base, store)
    }

    /// A day page from the primary calendar (`me@example.com`) with two items: the timed
    /// event `id` and an all-day event, which the parser skips.
    fn two_events(id: &str, title: &str) -> (u16, String) {
        (
            200,
            format!(
                r#"{{"summary":"me@example.com","items":[{{"id":"{id}","summary":"{title}","start":{{"dateTime":"2026-10-21T09:00:00-03:00"}},"end":{{"dateTime":"2026-10-21T10:00:00-03:00"}}}},{{"id":"{id}-allday","summary":"All day","start":{{"date":"2026-10-21"}},"end":{{"date":"2026-10-22"}}}}]}}"#
            ),
        )
    }

    fn cal_list(items: &str, next: Option<&str>) -> (u16, String) {
        let next = next
            .map(|t| format!(r#","nextPageToken":"{t}""#))
            .unwrap_or_default();
        (200, format!(r#"{{"items":[{items}]{next}}}"#))
    }

    const CALS: &str = r##"{"id":"me@x","summary":"me@x","primary":true,"selected":true,"backgroundColor":"#111111"},{"id":"mest@x","summary":"Mestrado","selected":true,"backgroundColor":"#222222"},{"id":"hidden@x","summary":"Hidden","selected":false,"backgroundColor":"#333333"}"##;

    fn seen_series(f: &crate::app_db::test_util::Fixture) -> Vec<(String, String, bool)> {
        f.db.projects()
            .unwrap()
            .into_iter()
            .map(|p| (p.name, p.calendar_name, p.archived))
            .collect()
    }

    #[test]
    fn series_window_starts_ninety_days_back_in_the_local_zone() {
        let today: jiff::civil::Date = "2026-10-21".parse().unwrap();
        assert_eq!(
            series_time_min(today, &sp()).unwrap(),
            "2026-07-23T00:00:00-03:00"
        );
    }

    #[test]
    fn load_projects_reads_calendars_then_series_of_shown_calendars_and_drops_the_missing() {
        let f = fixture();
        // Known before: a series that has since disappeared, one on a calendar that
        // is not fetched (hidden), and one the user archived but Google still has.
        f.db.upsert_calendars(&[cal("me@x", "me@x", true, true)], 1)
            .unwrap();
        add_series(&f.db, "me@x", "gone", "Old habit");
        add_series(&f.db, "hidden@x", "keep", "Hidden calendar series");
        let user_hid = add_series(&f.db, "me@x", "s1", "Learn Rust");
        f.db.project_set_archived(user_hid, true, 2).unwrap();
        let (base, seen) = fake_server(vec![
            tok(),
            cal_list(CALS, None),
            // me@x (primary): two pages, with a one-off and an exception in between
            (
                200,
                r#"{"items":[{"id":"s1","summary":"Learn Rust","recurrence":["RRULE:FREQ=DAILY"]},{"id":"one","summary":"Dentist"}],"nextPageToken":"P2"}"#.into(),
            ),
            (
                200,
                r#"{"items":[{"id":"s2","summary":"Deep work","recurrence":["RRULE:FREQ=WEEKLY"]},{"id":"s1_x","recurringEventId":"s1","summary":"moved"}]}"#.into(),
            ),
            // mest@x
            (
                200,
                r#"{"items":[{"id":"s3","summary":"Thesis","recurrence":["RRULE:FREQ=DAILY"]}]}"#.into(),
            ),
        ]);
        let state = with_client(Some(connected_client(&base)));
        assert!(run(load_projects(&state, &f.db)).unwrap());
        assert_eq!(
            seen_series(&f),
            [
                ("Deep work".to_string(), "me@x".to_string(), false),
                (
                    "Hidden calendar series".to_string(),
                    "Hidden".to_string(),
                    false
                ),
                ("Thesis".to_string(), "Mestrado".to_string(), false),
                ("Learn Rust".to_string(), "me@x".to_string(), true),
                // "Old habit" is gone from Google and no goal uses it: removed from the list.
            ]
        );
        let reqs = seen.lock().unwrap();
        assert_eq!(
            reqs.len(),
            5,
            "token, calendar list, 2 pages + 1 page of series"
        );
        assert!(reqs[1].starts_with("GET /calendar/v3/users/me/calendarList?"));
        assert!(reqs[2].starts_with("GET /calendar/v3/calendars/me@x/events?"));
        assert!(reqs[2].contains("singleEvents=false"));
        assert!(reqs[2].contains("timeMin=")); // 90 days back (value tested separately)
        assert!(reqs[3].contains("pageToken=P2"));
        assert!(reqs[4].starts_with("GET /calendar/v3/calendars/mest@x/events?"));
        assert!(!state.0.projects_refreshing.load(Ordering::SeqCst));
        // Calendars were refreshed too.
        assert_eq!(f.db.calendars().unwrap().len(), 3);
    }

    #[test]
    fn a_calendar_whose_series_cannot_be_read_keeps_its_projects() {
        let f = fixture();
        f.db.upsert_calendars(&[cal("me@x", "me@x", true, true)], 1)
            .unwrap();
        add_series(&f.db, "mest@x", "s3", "Thesis");
        let boom = (500, r#"{"error":{"message":"Backend Error"}}"#.to_string());
        let (base, _) = fake_server(vec![
            tok(),
            cal_list(CALS, None),
            (
                200,
                r#"{"items":[{"id":"s1","summary":"Learn Rust","recurrence":["RRULE:FREQ=DAILY"]}]}"#.into(),
            ),
            boom,
        ]);
        let state = with_client(Some(connected_client(&base)));
        assert!(run(load_projects(&state, &f.db)).unwrap());
        let got = seen_series(&f);
        assert!(got.contains(&("Learn Rust".to_string(), "me@x".to_string(), false)));
        assert!(
            got.iter()
                .any(|(n, _, archived)| n == "Thesis" && !archived),
            "{got:?}"
        );
    }

    #[test]
    fn load_projects_fails_only_when_no_calendar_could_be_read() {
        let f = fixture();
        let boom = (500, r#"{"error":{"message":"Backend Error"}}"#.to_string());
        let (base, _) = fake_server(vec![tok(), cal_list(CALS, None), boom.clone(), boom]);
        let state = with_client(Some(connected_client(&base)));
        assert_eq!(
            run(load_projects(&state, &f.db)).unwrap_err(),
            "Backend Error"
        );
        assert!(!state.0.projects_refreshing.load(Ordering::SeqCst));
        assert!(f.db.projects().unwrap().is_empty());
    }

    #[test]
    fn a_cancelled_or_missing_series_page_does_not_archive_anything_it_never_saw() {
        let f = fixture();
        let (base, _) = fake_server(vec![
            tok(),
            cal_list(
                r##"{"id":"me@x","summary":"me@x","primary":true,"selected":true}"##,
                None,
            ),
            (200, r#"{"items":[]}"#.into()),
        ]);
        add_series(&f.db, "other@x", "s9", "Elsewhere");
        let state = with_client(Some(connected_client(&base)));
        assert!(run(load_projects(&state, &f.db)).unwrap());
        assert!(!f.db.projects().unwrap()[0].archived);
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
        assert_eq!(primary_calendar(&calendars()), "me@x");
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
    }

    #[test]
    fn making_a_one_off_event_a_project_uses_the_cached_event() {
        let f = fixture();
        f.db.upsert_calendars(&[cal("me@x", "Personal", true, true)], 1)
            .unwrap();
        let without = IndexHandle(Err("broken".into()));
        let state = bare();
        let mut dentist = ev_in("dentist", "primary");
        dentist.title = "Dentist".into();
        state.cache_put(
            &without,
            "2026-10-21",
            &[dentist, instance("s1_1", "me@x", "s1")],
            5,
        );
        let made = event_project(&state, &without, &f.db, "2026-10-21", "dentist", 9).unwrap();
        assert_eq!(made.len(), 1);
        assert_eq!(
            (
                made[0].key.as_str(),
                made[0].name.as_str(),
                made[0].calendar_name.as_str()
            ),
            ("event:dentist", "Dentist", "Personal")
        );
        // idempotent
        assert_eq!(
            event_project(&state, &without, &f.db, "2026-10-21", "dentist", 10)
                .unwrap()
                .len(),
            1
        );
        // events of a series, unknown events and bad dates are refused
        assert_eq!(
            event_project(&state, &without, &f.db, "2026-10-21", "s1_1", 9).unwrap_err(),
            "This event repeats; its series is already a project"
        );
        assert_eq!(
            event_project(&state, &without, &f.db, "2026-10-21", "nope", 9).unwrap_err(),
            "Event not found; refresh the day and try again"
        );
        assert_eq!(
            event_project(&state, &without, &f.db, "2026-10-22", "dentist", 9).unwrap_err(),
            "Event not found; refresh the day and try again"
        );
        assert!(event_project(&state, &without, &f.db, "nope", "dentist", 9).is_err());
        // and the event now carries the key
        let known = Known {
            calendars: f.db.calendars().unwrap(),
            projects: f.db.project_rows().unwrap(),
        };
        let out = enrich(vec![ev_in("dentist", "me@x")], &known);
        assert_eq!(out[0].project_key.as_deref(), Some("event:dentist"));
        assert_eq!(out[0].project_id, Some(made[0].id));
    }
}
