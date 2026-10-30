//! Tauri commands for Google Calendar (M5): connect/disconnect, the per-day cache
//! and event writes. The pure and HTTP parts live in `google` and `google_client`.

use crate::app_db::{AppDb, Calendar, Project, ProjectKind, ProjectRow, project_key};
use crate::commands::{AppDbHandle, IndexHandle, now_secs};
use crate::google::{self, CalEvent};
use crate::google_client::{
    Client, Credentials, Endpoints, Error, KeychainVault, MemoryStore, Vault, migrate_legacy,
};
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
/// Meta key (app database, local) of the calendar new blocks are created in.
const NEW_EVENT_CALENDAR: &str = "new_event_calendar";
/// The email of an account the migration could not name yet (the single-account token
/// found with no email; resolved in the background, retried at the next start).
const UNNAMED_ACCOUNT: &str = "";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleStatus {
    pub configured: bool,
    /// At least one account is connected.
    pub connected: bool,
    /// Connection order.
    pub accounts: Vec<GoogleAccount>,
    /// Any account needs it.
    pub needs_reconnect: bool,
    /// `google_calendar_id` new blocks are created in (the resolved default).
    pub new_event_calendar_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleAccount {
    pub email: String,
    /// Its token was revoked, or predates the calendar list scope: connect it again.
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

/// One connected Google account and its client.
#[derive(Clone)]
struct Account {
    email: String,
    client: Arc<Client>,
}

struct Inner {
    /// `None` when the build has no Google client credentials.
    creds: Option<Credentials>,
    endpoints: Endpoints,
    vault: Arc<dyn Vault>,
    /// The connected accounts in connection order, read from the vault at first use
    /// so day navigation does not hit the keychain.
    accounts: Mutex<Option<Vec<Account>>>,
    /// Cancel flag of the consent wait in progress, if any.
    cancel: Mutex<Option<Arc<AtomicBool>>>,
    /// Dates whose refresh is in flight.
    refreshing: Mutex<HashSet<String>>,
    /// A calendar/series refresh is in flight.
    projects_refreshing: AtomicBool,
    /// Cache used only when the SQLite index is unavailable.
    memory: Mutex<HashMap<String, (Vec<CalEvent>, i64)>>,
}

#[derive(Clone)]
pub struct GoogleState(Arc<Inner>);

/// An account id is its email: non-empty, no spaces (it is part of a keychain key).
fn valid_email(s: &str) -> bool {
    !s.is_empty() && !s.contains(char::is_whitespace)
}

impl GoogleState {
    pub fn new() -> GoogleState {
        GoogleState::with(credentials(), Endpoints::default(), Arc::new(KeychainVault))
    }

    fn with(
        creds: Option<Credentials>,
        endpoints: Endpoints,
        vault: Arc<dyn Vault>,
    ) -> GoogleState {
        GoogleState(Arc::new(Inner {
            creds,
            endpoints,
            vault,
            accounts: Mutex::new(None),
            cancel: Mutex::new(None),
            refreshing: Mutex::new(HashSet::new()),
            projects_refreshing: AtomicBool::new(false),
            memory: Mutex::new(HashMap::new()),
        }))
    }

    fn configured(&self) -> bool {
        self.0.creds.is_some()
    }

    fn credentials(&self) -> Result<Credentials, String> {
        self.0.creds.clone().ok_or_else(|| NOT_CONFIGURED.into())
    }

    fn build_client(&self, store: Arc<dyn crate::google_client::TokenStore>) -> Option<Client> {
        let creds = self.0.creds.clone()?;
        Client::new(creds, self.0.endpoints.clone(), store)
            .map_err(|e| eprintln!("google calendar disabled: {e}"))
            .ok()
    }

    /// The accounts the vault holds. A single-account token (before several accounts)
    /// whose email is known moves to its per-account entries now; one without an email
    /// stays as an unnamed account until `migrate_legacy_account` names it.
    fn load_accounts(&self) -> Vec<Account> {
        if !self.configured() {
            return Vec::new();
        }
        let vault = &self.0.vault;
        let mut unnamed = false;
        if let Some(legacy) = vault.legacy() {
            match legacy.email.filter(|e| valid_email(e)) {
                Some(email) => {
                    if let Err(e) = migrate_legacy(vault.as_ref(), &email) {
                        eprintln!("moving the Google connection: {e}");
                        unnamed = true;
                    }
                }
                None => unnamed = true,
            }
        }
        let mut out: Vec<Account> = Vec::new();
        for email in vault.accounts() {
            if let Some(client) = self.build_client(vault.store(&email)) {
                out.push(Account {
                    email,
                    client: Arc::new(client),
                });
            }
        }
        if unnamed && let Some(client) = self.build_client(vault.legacy_store()) {
            out.insert(
                0,
                Account {
                    email: UNNAMED_ACCOUNT.into(),
                    client: Arc::new(client),
                },
            );
        }
        out
    }

    fn with_accounts<R>(&self, f: impl FnOnce(&mut Vec<Account>) -> R) -> R {
        let mut accounts = lock(&self.0.accounts);
        f(accounts.get_or_insert_with(|| self.load_accounts()))
    }

    fn accounts(&self) -> Vec<Account> {
        self.with_accounts(|a| a.clone())
    }

    fn emails(&self) -> Vec<String> {
        self.with_accounts(|a| a.iter().map(|x| x.email.clone()).collect())
    }

    fn connected(&self) -> bool {
        self.with_accounts(|a| !a.is_empty())
    }

    fn client_of(&self, email: &str) -> Option<Arc<Client>> {
        self.with_accounts(|a| {
            a.iter()
                .find(|x| x.email == email)
                .map(|x| x.client.clone())
        })
    }

    /// Gives the unnamed single-account token its email (the primary calendar's id).
    /// Best effort: on failure it stays as it is and the next start tries again.
    async fn migrate_legacy_account(&self) {
        let Some(account) = self
            .accounts()
            .into_iter()
            .find(|a| a.email == UNNAMED_ACCOUNT)
        else {
            return;
        };
        let result = match account.client.account_email().await {
            Ok(email) => migrate_legacy(self.0.vault.as_ref(), &email),
            Err(e) => Err(e.to_string()),
        };
        match result {
            Ok(()) => {
                let fresh = self.load_accounts();
                *lock(&self.0.accounts) = Some(fresh);
            }
            Err(e) => eprintln!("naming the Google account: {e}"),
        }
    }

    /// Saves the tokens of a finished consent under `email` and adds the account
    /// (or, when it is already connected, replaces its token: this is also "Reconnect").
    fn adopt(&self, email: &str, consent: &Client) -> Result<(), String> {
        if !valid_email(email) {
            return Err("Google did not say which account this is".into());
        }
        let (refresh, scope) = consent.credentials_to_keep()?;
        let store = self.0.vault.store(email);
        store.set_refresh_token(&refresh)?;
        store.set_scope(scope.as_deref());
        let mut emails = self.0.vault.accounts();
        if !emails.iter().any(|e| e == email) {
            emails.push(email.to_string());
            self.0.vault.set_accounts(&emails)?;
        }
        let client = Arc::new(
            self.build_client(store)
                .ok_or("Google Calendar could not start")?,
        );
        self.with_accounts(
            |accounts| match accounts.iter_mut().find(|a| a.email == email) {
                Some(slot) => slot.client = client,
                None => accounts.push(Account {
                    email: email.to_string(),
                    client,
                }),
            },
        );
        Ok(())
    }

    /// Forgets the account once its client has disconnected it.
    fn forget(&self, email: &str) -> Result<(), String> {
        if email == UNNAMED_ACCOUNT {
            self.0.vault.delete_legacy();
        }
        let emails: Vec<String> = self
            .0
            .vault
            .accounts()
            .into_iter()
            .filter(|e| e != email)
            .collect();
        self.0.vault.set_accounts(&emails)?;
        self.with_accounts(|accounts| accounts.retain(|a| a.email != email));
        Ok(())
    }

    fn status(&self, calendars: &[Calendar], chosen: Option<&str>) -> GoogleStatus {
        let accounts: Vec<GoogleAccount> = self.with_accounts(|a| {
            a.iter()
                .map(|x| GoogleAccount {
                    email: x.email.clone(),
                    needs_reconnect: x.client.needs_reconnect(),
                })
                .collect()
        });
        let emails: Vec<String> = accounts.iter().map(|a| a.email.clone()).collect();
        GoogleStatus {
            configured: self.configured(),
            connected: !accounts.is_empty(),
            needs_reconnect: accounts.iter().any(|a| a.needs_reconnect),
            new_event_calendar_id: new_event_calendar(calendars, &emails, chosen),
            accounts,
        }
    }

    /// The status with what the app database knows (calendars, the chosen new-block calendar).
    fn status_with(&self, db: &AppDbHandle) -> GoogleStatus {
        let chosen =
            db.0.as_ref()
                .ok()
                .and_then(|db| db.meta(NEW_EVENT_CALENDAR).ok().flatten());
        self.status(&known(db).calendars, chosen.as_deref())
    }

    /// Maps a client error to the user-facing text.
    fn describe(&self, e: &Error) -> String {
        match e {
            Error::Disconnected | Error::NotConnected => DISCONNECTED.into(),
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

/// The account a calendar belongs to; rows from before several accounts (no account
/// yet) belong to the first one, the only one there was.
fn owner<'a>(calendar: &'a Calendar, first: &'a str) -> &'a str {
    calendar.account.as_deref().unwrap_or(first)
}

/// Calendars whose events the timeline fetches for `account`: its primary one always,
/// then every live calendar of it the user shows. Without a primary row yet, the
/// `primary` alias (which Google resolves for that account's own token).
fn calendars_to_fetch(calendars: &[Calendar], account: &str, first: &str) -> Vec<String> {
    let mine = |c: &&Calendar| !c.archived && owner(c, first) == account;
    let mut out: Vec<String> = Vec::new();
    match calendars.iter().filter(mine).find(|c| c.is_primary) {
        Some(c) => out.push(c.google_calendar_id.clone()),
        None => out.push("primary".to_string()),
    }
    for c in calendars.iter().filter(mine) {
        if c.show_on_timeline && !out.contains(&c.google_calendar_id) {
            out.push(c.google_calendar_id.clone());
        }
    }
    out
}

/// The calendar new blocks go to: the user's choice when it is a live calendar of a
/// connected account, else the primary calendar of the first account. `None` while
/// the calendars are not known yet.
fn new_event_calendar(
    calendars: &[Calendar],
    accounts: &[String],
    chosen: Option<&str>,
) -> Option<String> {
    let first = accounts.first()?;
    let live = |c: &&Calendar| !c.archived && accounts.iter().any(|a| a == owner(c, first));
    if let Some(id) = chosen
        && calendars
            .iter()
            .filter(live)
            .any(|c| c.google_calendar_id == id)
    {
        return Some(id.to_string());
    }
    calendars
        .iter()
        .filter(live)
        .find(|c| c.is_primary && owner(c, first) == first)
        .map(|c| c.google_calendar_id.clone())
}

/// The email of the account that owns `calendar_id`: its row's account, the first account
/// for the `primary` alias or a calendar the app does not know.
fn account_for(
    calendars: &[Calendar],
    calendar_id: &str,
    accounts: &[Account],
) -> Result<Account, String> {
    let first = accounts.first().ok_or(DISCONNECTED)?;
    let email = match calendar_id {
        "primary" => first.email.as_str(),
        id => calendars
            .iter()
            .find(|c| c.google_calendar_id == id)
            .map_or(first.email.as_str(), |c| owner(c, &first.email)),
    };
    accounts
        .iter()
        .find(|a| a.email == email)
        .cloned()
        .ok_or_else(|| format!("{email} is not connected; connect it again in Settings"))
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
                let shown = !c.archived && (c.is_primary || c.show_on_timeline);
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
pub fn google_status(google: State<GoogleState>, db: State<AppDbHandle>) -> GoogleStatus {
    google.status_with(&db)
}

/// Runs the consent in the browser and adds (or reconnects) the account it was for.
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
    let creds = google.credentials()?;
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
    let url = google::auth_url(&creds.client_id, &redirect, &code_challenge, &state);
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
    connect_with_code(&google, &code, &code_verifier, &redirect).await?;
    // Best effort: the account's calendars become known (and its projects), then today shows its events.
    if let Err(e) = refresh_projects(&app, &google, &db).await {
        eprintln!("loading calendars: {e}");
    }
    refresh_day(&app, &google, &index, &db, &today()).await;
    Ok(google.status_with(&db))
}

/// Trades the consent code for tokens in memory, learns which account they belong to,
/// then saves them under that email.
async fn connect_with_code(
    google: &GoogleState,
    code: &str,
    verifier: &str,
    redirect: &str,
) -> Result<String, String> {
    let creds = google.credentials()?;
    let consent = Client::new(
        creds,
        google.0.endpoints.clone(),
        Arc::new(MemoryStore::default()),
    )?;
    consent
        .exchange_code(code, verifier, redirect)
        .await
        .map_err(|e| google.describe(&e))?;
    let email = consent
        .account_email()
        .await
        .map_err(|e| google.describe(&e))?;
    google.adopt(&email, &consent)?;
    Ok(email)
}

#[tauri::command]
pub fn google_cancel_connect(google: State<GoogleState>) {
    if let Some(cancel) = lock(&google.0.cancel).take() {
        cancel.store(true, Ordering::SeqCst);
    }
}

/// Disconnects one account: revokes it (best effort), forgets its keychain entries and
/// archives its calendars (projects and goals still point at them).
#[tauri::command]
pub async fn google_disconnect(
    app: AppHandle,
    google: State<'_, GoogleState>,
    index: State<'_, IndexHandle>,
    db: State<'_, AppDbHandle>,
    email: String,
) -> Result<GoogleStatus, String> {
    let google = google.inner().clone();
    disconnect_account(&google, &index, &db, &email).await?;
    emit_changed(&app, "calendars-changed");
    emit_changed(&app, "projects-changed");
    if let Err(e) = app.emit("calendar-changed", ChangedPayload { date: today() }) {
        eprintln!("emitting calendar-changed: {e}");
    }
    Ok(google.status_with(&db))
}

async fn disconnect_account(
    google: &GoogleState,
    index: &IndexHandle,
    db: &AppDbHandle,
    email: &str,
) -> Result<(), String> {
    let client = google
        .client_of(email)
        .ok_or_else(|| format!("{email} is not connected"))?;
    client.disconnect().await.map_err(|e| e.to_string())?;
    google.forget(email)?;
    // Its events stop showing because its calendars are archived; with nothing left
    // (or no database to say so) the whole cache goes.
    let archived = match &db.0 {
        Ok(db) => db
            .archive_calendars_except(&google.emails(), now_secs())
            .map_err(|e| eprintln!("archiving calendars: {e}"))
            .is_ok(),
        Err(_) => false,
    };
    if !archived || !google.connected() {
        google.cache_clear(index);
    }
    Ok(())
}

/// Chooses the calendar new blocks are created in (a live calendar of a connected account).
#[tauri::command]
pub fn google_set_new_event_calendar(
    google: State<GoogleState>,
    db: State<AppDbHandle>,
    google_calendar_id: String,
) -> Result<GoogleStatus, String> {
    set_new_event_calendar(&google, &db, &google_calendar_id)
}

fn set_new_event_calendar(
    google: &GoogleState,
    db: &AppDbHandle,
    google_calendar_id: &str,
) -> Result<GoogleStatus, String> {
    let handle = db.get()?;
    let calendars = handle.calendars()?;
    if new_event_calendar(&calendars, &google.emails(), Some(google_calendar_id)).as_deref()
        != Some(google_calendar_id)
    {
        return Err("That calendar isn't available; refresh the calendars and try again".into());
    }
    handle.set_meta(NEW_EVENT_CALENDAR, google_calendar_id)?;
    Ok(google.status_with(db))
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

/// The day's events from every calendar of one account to show. Its primary
/// calendar must succeed; another calendar that fails is skipped (and logged).
/// An event that sits in several calendars is kept once, from the first.
async fn fetch_day(
    client: &Client,
    calendars: &[String],
    date: &str,
    tz: &TimeZone,
) -> Result<Vec<CalEvent>, Error> {
    let mut all: Vec<CalEvent> = Vec::new();
    for (i, calendar) in calendars.iter().enumerate() {
        match client.list_day_in(calendar, date, tz).await {
            Ok(day) => {
                for e in day {
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
    Ok(all)
}

/// What a day fetch across accounts came to.
#[derive(Debug, PartialEq)]
struct DayFetch {
    /// `None` when every account failed: the cache stays as it is.
    events: Option<Vec<CalEvent>>,
    /// One `(email, message)` per account that failed.
    failed: Vec<(String, String)>,
}

/// Puts the accounts' answers together, first account first. An event seen in two
/// accounts shows once. An account that failed keeps the events of its calendars
/// from `previous` (the cache), so one failing account does not empty its part of the
/// day; `owner_of` names the account of a cached event.
fn merge_days(
    answers: Vec<(String, Result<Vec<CalEvent>, String>)>,
    previous: &[CalEvent],
    owner_of: impl Fn(&CalEvent) -> String,
) -> DayFetch {
    let mut events: Vec<CalEvent> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();
    let mut any_ok = false;
    for (email, answer) in answers {
        match answer {
            Ok(day) => {
                any_ok = true;
                for e in day {
                    if !events.iter().any(|x| x.id == e.id) {
                        events.push(e);
                    }
                }
            }
            Err(message) => failed.push((email, message)),
        }
    }
    if !any_ok {
        return DayFetch {
            events: None,
            failed,
        };
    }
    for e in previous {
        let owner = owner_of(e);
        if failed.iter().any(|(email, _)| *email == owner) && !events.iter().any(|x| x.id == e.id) {
            events.push(e.clone());
        }
    }
    events.sort_by_key(|e| (e.start, e.end));
    DayFetch {
        events: Some(events),
        failed,
    }
}

/// The day from every connected account. One failing account does not hide the others.
async fn fetch_all(
    google: &GoogleState,
    calendars: &[Calendar],
    previous: &[CalEvent],
    date: &str,
    tz: &TimeZone,
) -> DayFetch {
    let accounts = google.accounts();
    let first = accounts
        .first()
        .map(|a| a.email.clone())
        .unwrap_or_default();
    let mut answers = Vec::new();
    for account in &accounts {
        let wanted = calendars_to_fetch(calendars, &account.email, &first);
        let answer = fetch_day(&account.client, &wanted, date, tz)
            .await
            .map_err(|e| google.describe(&e));
        answers.push((account.email.clone(), answer));
    }
    merge_days(answers, previous, |e| {
        account_for(calendars, &e.calendar_id, &accounts)
            .map_or_else(|_| first.clone(), |a| a.email)
    })
}

/// `email: message`, so the user can tell which account failed.
fn named(email: &str, message: &str) -> String {
    if email == UNNAMED_ACCOUNT {
        message.to_string()
    } else {
        format!("{email}: {message}")
    }
}

async fn refresh_day(
    app: &AppHandle,
    google: &GoogleState,
    index: &IndexHandle,
    db: &AppDbHandle,
    date: &str,
) {
    let before = google.cache_get(index, date);
    let previous = before
        .as_ref()
        .map(|(e, _)| e.as_slice())
        .unwrap_or_default();
    let day = fetch_all(
        google,
        &known(db).calendars,
        previous,
        date,
        &TimeZone::system(),
    )
    .await;
    for (email, message) in &day.failed {
        emit_error(app, date, named(email, message));
    }
    // Projects come only from `projects_refresh`: an instance doesn't say how often
    // its series repeats, and only series repeating at least weekly are projects.
    let Some(events) = day.events else {
        return;
    };
    let changed = before.is_none_or(|(old, _)| old != events);
    google.cache_put(index, date, &events, now_ms());
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

/// The account and calendar that serve a write on an event: the calendar the cache
/// says it is in (else the primary calendar of the first account).
fn route(
    google: &GoogleState,
    index: &IndexHandle,
    db: &AppDbHandle,
    date: &str,
    event_id: &str,
) -> Result<(Account, String), String> {
    let known = known(db);
    let accounts = google.accounts();
    let first = accounts
        .first()
        .map(|a| a.email.as_str())
        .unwrap_or_default();
    let calendar = cached_calendar(google, index, date, event_id)
        .unwrap_or_else(|| primary_calendar(&known.calendars, first));
    Ok((
        account_for(&known.calendars, &calendar, &accounts)?,
        calendar,
    ))
}

async fn create_event(
    google: &GoogleState,
    index: &IndexHandle,
    db: &AppDbHandle,
    (date, start, end): (&str, u32, u32),
    title: &str,
    tags: &[String],
) -> Result<CalEvent, String> {
    let title = check_write(date, start, end, title)?;
    let known = known(db);
    let accounts = google.accounts();
    let chosen =
        db.0.as_ref()
            .ok()
            .and_then(|db| db.meta(NEW_EVENT_CALENDAR).ok().flatten());
    let calendar = new_event_calendar(&known.calendars, &google.emails(), chosen.as_deref())
        .unwrap_or_else(|| "primary".to_string());
    let account = account_for(&known.calendars, &calendar, &accounts)?;
    let event = account
        .client
        .create_event(
            &calendar,
            date,
            (start, end),
            &title,
            tags,
            &TimeZone::system(),
        )
        .await
        .map_err(|e| google.describe(&e))?;
    cache_event(google, index, date, &event);
    Ok(enrich_one(event, &known))
}

#[tauri::command]
pub async fn calendar_create(
    app: AppHandle,
    date: String,
    start: u32,
    end: u32,
    title: String,
    tags: Vec<String>,
) -> Result<CalEvent, String> {
    let (google, index, db) = (
        app.state::<GoogleState>(),
        app.state::<IndexHandle>(),
        app.state::<AppDbHandle>(),
    );
    create_event(&google, &index, &db, (&date, start, end), &title, &tags).await
}

async fn update_event(
    google: &GoogleState,
    index: &IndexHandle,
    db: &AppDbHandle,
    (date, event_id): (&str, &str),
    (start, end): (u32, u32),
    title: &str,
    tags: &[String],
) -> Result<CalEvent, String> {
    let title = check_write(date, start, end, title)?;
    if event_id.is_empty() {
        return Err("missing event id".into());
    }
    let (account, calendar) = route(google, index, db, date, event_id)?;
    let event = account
        .client
        .update_event(
            (&calendar, event_id),
            date,
            (start, end),
            &title,
            tags,
            &TimeZone::system(),
        )
        .await
        .map_err(|e| google.describe(&e))?;
    cache_event(google, index, date, &event);
    Ok(enrich_one(event, &known(db)))
}

#[tauri::command]
pub async fn calendar_update(
    app: AppHandle,
    date: String,
    event_id: String,
    start: u32,
    end: u32,
    title: String,
    tags: Vec<String>,
) -> Result<CalEvent, String> {
    let (google, index, db) = (
        app.state::<GoogleState>(),
        app.state::<IndexHandle>(),
        app.state::<AppDbHandle>(),
    );
    update_event(
        &google,
        &index,
        &db,
        (&date, &event_id),
        (start, end),
        &title,
        &tags,
    )
    .await
}

/// Gives the event these tags on Google (hidden, title untouched). For an instance of a
/// repeating series the master gets them too, so every occurrence has them; the
/// instance is patched as well, since today's occurrence may be an exception.
async fn set_event_tags(
    google: &GoogleState,
    index: &IndexHandle,
    db: &AppDbHandle,
    (date, event_id): (&str, &str),
    tags: &[String],
) -> Result<CalEvent, String> {
    check_date(date)?;
    if event_id.is_empty() {
        return Err("missing event id".into());
    }
    let cached = google
        .cache_get(index, date)
        .and_then(|(events, _)| events.into_iter().find(|e| e.id == event_id))
        .ok_or("Event not found; refresh the day and try again")?;
    let (account, calendar) = route(google, index, db, date, event_id)?;
    let tz = TimeZone::system();
    if let Some(master) = &cached.recurring_event_id {
        account
            .client
            .set_series_tags((&calendar, master), tags)
            .await
            .map_err(|e| google.describe(&e))?;
    }
    let event = account
        .client
        .set_tags((&calendar, event_id), date, tags, &tz)
        .await
        .map_err(|e| google.describe(&e))?;
    if let Some((mut events, at)) = google.cache_get(index, date) {
        if let Some(master) = &cached.recurring_event_id {
            retag_series(
                &mut events,
                master,
                &google::parse_tags(&google::tags_value(tags)),
            );
        }
        upsert_event(&mut events, event.clone());
        google.cache_put(index, date, &events, at);
    }
    Ok(enrich_one(event, &known(db)))
}

/// Every cached occurrence of the series gets the tags (they inherit the master's).
fn retag_series(events: &mut [CalEvent], series: &str, tags: &[String]) {
    for e in events
        .iter_mut()
        .filter(|e| e.recurring_event_id.as_deref() == Some(series))
    {
        e.tags = tags.to_vec();
    }
}

#[tauri::command]
pub async fn calendar_set_tags(
    app: AppHandle,
    date: String,
    event_id: String,
    tags: Vec<String>,
) -> Result<CalEvent, String> {
    let (google, index, db) = (
        app.state::<GoogleState>(),
        app.state::<IndexHandle>(),
        app.state::<AppDbHandle>(),
    );
    set_event_tags(&google, &index, &db, (&date, &event_id), &tags).await
}

async fn delete_event(
    google: &GoogleState,
    index: &IndexHandle,
    db: &AppDbHandle,
    date: &str,
    event_id: &str,
) -> Result<(), String> {
    check_date(date)?;
    if event_id.is_empty() {
        return Err("missing event id".into());
    }
    let (account, calendar) = route(google, index, db, date, event_id)?;
    account
        .client
        .delete_event(&calendar, event_id)
        .await
        .map_err(|e| google.describe(&e))?;
    uncache_event(google, index, date, event_id);
    Ok(())
}

/// Deletes the event from Google Calendar (only that occurrence of a repeating series), then from the day cache.
#[tauri::command]
pub async fn calendar_delete(app: AppHandle, date: String, event_id: String) -> Result<(), String> {
    let (google, index, db) = (
        app.state::<GoogleState>(),
        app.state::<IndexHandle>(),
        app.state::<AppDbHandle>(),
    );
    delete_event(&google, &index, &db, &date, &event_id).await?;
    if let Err(e) = app.emit("calendar-changed", ChangedPayload { date }) {
        eprintln!("emitting calendar-changed: {e}");
    }
    Ok(())
}

/// The calendar to use when an event's calendar is not known: the primary calendar
/// of the first account.
fn primary_calendar(calendars: &[Calendar], first: &str) -> String {
    calendars
        .iter()
        .find(|c| c.is_primary && !c.archived && owner(c, first) == first)
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

/// Reads every account's calendar list into the calendars, then the repeating series
/// of the calendars the timeline shows into the projects. `Ok(false)` when there was
/// nothing to do (no account that can be read, or a refresh already running).
/// An account whose token lacks the list scope or whose calendars can't be read is
/// skipped (its calendars and projects stay as they are); only when no calendar of
/// any account could be read is it an error.
async fn load_projects(google: &GoogleState, db: &AppDb) -> Result<bool, String> {
    if !google.configured() || google.0.projects_refreshing.swap(true, Ordering::SeqCst) {
        return Ok(false);
    }
    let result = load_projects_inner(google, db).await;
    google.0.projects_refreshing.store(false, Ordering::SeqCst);
    result
}

async fn load_projects_inner(google: &GoogleState, db: &AppDb) -> Result<bool, String> {
    let accounts = google.accounts();
    let first = accounts
        .first()
        .map(|a| a.email.clone())
        .unwrap_or_default();
    let now = now_secs();
    let mut attempted = false;
    let mut list_error = None;
    let mut listed = Vec::new();
    for account in &accounts {
        // The status says `needsReconnect` for it.
        if account.client.needs_reconnect() {
            continue;
        }
        attempted = true;
        match account.client.list_calendars().await {
            Ok(calendars) => {
                let owner = (account.email != UNNAMED_ACCOUNT).then_some(account.email.as_str());
                db.upsert_account_calendars(owner, account.email == first, &calendars, now)?;
                listed.push(account);
            }
            Err(Error::MissingScope) => {}
            Err(e) => {
                eprintln!("calendars of {}: {e}", account.email);
                list_error.get_or_insert(google.describe(&e));
            }
        }
    }
    if !attempted {
        return Ok(false);
    }
    if listed.is_empty() {
        return list_error.map_or(Ok(true), Err);
    }
    // Calendars of an account that is no longer connected are archived, not deleted.
    db.archive_calendars_except(&google.emails(), now)?;
    let time_min = series_time_min(jiff::Zoned::now().date(), &TimeZone::system())?;
    let calendars = db.calendars()?;
    let mut found = Vec::new();
    let mut first_error = None;
    let mut missing_scope = false;
    for account in listed {
        for calendar in calendars_to_fetch(&calendars, &account.email, &first) {
            match account.client.list_series(&calendar, &time_min).await {
                Ok(series) => found.push((calendar, series)),
                Err(Error::MissingScope) => missing_scope = true,
                Err(e) => {
                    eprintln!("series of calendar {calendar}: {e}");
                    first_error.get_or_insert(google.describe(&e));
                }
            }
        }
    }
    if found.is_empty() {
        if let Some(message) = first_error {
            return Err(message);
        }
        if missing_scope {
            return Ok(true);
        }
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
        // A connection from before several accounts gets its email first.
        google.migrate_legacy_account().await;
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
            tags: vec![],
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
        let state = bare();
        assert_eq!(
            state.status(&[], None),
            GoogleStatus {
                configured: false,
                connected: false,
                accounts: vec![],
                needs_reconnect: false,
                new_event_calendar_id: None,
            }
        );
        assert!(!state.connected());
        assert!(state.credentials().is_err());
        assert_eq!(state.credentials().err().as_deref(), Some(NOT_CONFIGURED));
    }

    fn bare() -> GoogleState {
        GoogleState::with(None, Endpoints::default(), Arc::new(MemVault::default()))
    }

    /// A configured state whose accounts are already loaded (never the real keychain).
    fn state_with(accounts: Vec<(&str, Client)>) -> GoogleState {
        let state = GoogleState::with(
            Some(creds()),
            Endpoints::default(),
            Arc::new(MemVault::default()),
        );
        *lock(&state.0.accounts) = Some(
            accounts
                .into_iter()
                .map(|(email, client)| Account {
                    email: email.into(),
                    client: Arc::new(client),
                })
                .collect(),
        );
        state
    }

    /// A state talking to the fake server, with a vault the test can inspect.
    fn state_on(base: &str, vault: Arc<MemVault>) -> GoogleState {
        GoogleState::with(Some(creds()), endpoints(base), vault)
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
            accounts: vec![GoogleAccount {
                email: "a@b.c".into(),
                needs_reconnect: true,
            }],
            needs_reconnect: true,
            new_event_calendar_id: Some("a@b.c".into()),
        };
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::json!({"configured":true,"connected":true,"accounts":[{"email":"a@b.c","needsReconnect":true}],"needsReconnect":true,"newEventCalendarId":"a@b.c"})
        );
        let day = CalendarDay {
            events: vec![ev("a", 1, 2)],
            fetched_at: Some(5),
        };
        assert_eq!(
            serde_json::to_value(&day).unwrap(),
            serde_json::json!({"events":[{"id":"a","start":1,"end":2,"title":"a","projectKey":null,"projectId":null,"color":"","tags":[]}],"fetchedAt":5})
        );
    }

    // ---- calendars and projects

    use crate::app_db::test_util::{add_series, cal, fixture};
    use crate::google_client::test_util::{
        MemStore, MemVault, client as fake_client, creds, endpoints, fake_server,
    };
    use crate::google_client::{TokenStore, Vault};

    fn calendar(id: i64, cal_id: &str, primary: bool, shown: bool, archived: bool) -> Calendar {
        Calendar {
            id,
            google_calendar_id: cal_id.into(),
            name: format!("name {cal_id}"),
            color: format!("#00000{id}"),
            show_on_timeline: shown,
            is_primary: primary,
            archived,
            account: None,
        }
    }

    fn owned(account: &str, c: Calendar) -> Calendar {
        Calendar {
            account: Some(account.into()),
            ..c
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
        let fetch = |c: &[Calendar]| calendars_to_fetch(c, "me@x", "me@x");
        assert_eq!(fetch(&calendars()), ["me@x", "mest@x"]);
        assert_eq!(fetch(&[]), ["primary"]);
        // a primary the user hid is still fetched; an unknown primary falls back to the alias
        let only_others = vec![calendar(2, "mest@x", false, true, false)];
        assert_eq!(fetch(&only_others), ["primary", "mest@x"]);
        let hidden_primary = vec![calendar(1, "me@x", true, false, false)];
        assert_eq!(fetch(&hidden_primary), ["me@x"]);
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
        assert_eq!(day.len(), 1);
        assert_eq!(
            (day[0].title.as_str(), day[0].calendar_id.as_str()),
            ("Primary copy", "me@x")
        );
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
        assert_eq!(day.len(), 1);
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
        let state = state_with(vec![("me@x", connected_client(&base))]);
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
        let state = state_with(vec![("me@x", connected_client(&base))]);
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
        let state = state_with(vec![("me@x", connected_client(&base))]);
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
        let state = state_with(vec![("me@x", connected_client(&base))]);
        assert!(run(load_projects(&state, &f.db)).unwrap());
        assert!(!f.db.projects().unwrap()[0].archived);
    }

    #[test]
    fn load_projects_does_nothing_when_disconnected_or_in_need_of_reconnect() {
        let f = fixture();
        // not connected
        let (base, seen) = fake_server(vec![]);
        let disconnected = fake_client(&base, Arc::new(MemStore::default()));
        let state = state_with(vec![("me@x", disconnected)]);
        assert!(!run(load_projects(&state, &f.db)).unwrap());
        // connected, token without the list scope
        let store = Arc::new(MemStore::default());
        store.set_refresh_token("r").unwrap();
        store.set_scope(Some(crate::google::SCOPE));
        let state = state_with(vec![("me@x", fake_client(&base, store))]);
        assert!(!run(load_projects(&state, &f.db)).unwrap());
        assert!(seen.lock().unwrap().is_empty());
        assert!(state.status(&[], None).needs_reconnect);
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
        let state = state_with(vec![("me@x", connected_client(&base))]);
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
        let state = state_with(vec![("me@x", connected_client(&base))]);
        assert!(run(load_projects(&state, &f.db)).unwrap());
        assert!(state.status(&[], None).needs_reconnect);
    }

    #[test]
    fn primary_calendar_prefers_the_real_id() {
        assert_eq!(primary_calendar(&calendars(), "me@x"), "me@x");
        assert_eq!(primary_calendar(&[], "me@x"), "primary");
        // the first account's, not another account's primary
        let two = vec![
            owned("b@x", calendar(2, "b@x", true, true, false)),
            owned("a@x", calendar(1, "a@x", true, true, false)),
        ];
        assert_eq!(primary_calendar(&two, "a@x"), "a@x");
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

    // ---- several accounts and tags

    fn db_handle(
        f: crate::app_db::test_util::Fixture,
    ) -> (AppDbHandle, crate::config::test_util::TempDir) {
        let crate::app_db::test_util::Fixture { tmp, db, .. } = f;
        (AppDbHandle(Ok(Arc::new(db))), tmp)
    }

    fn no_index() -> IndexHandle {
        IndexHandle(Err("broken".into()))
    }

    /// A token response for the account with this access token.
    fn tok_as(access: &str) -> (u16, String) {
        (
            200,
            format!(r#"{{"access_token":"{access}","expires_in":3600}}"#),
        )
    }

    fn client_for(base: &str, refresh: &str) -> Client {
        let store = Arc::new(MemStore::default());
        store.set_refresh_token(refresh).unwrap();
        fake_client(base, store)
    }

    /// Two accounts on one fake server; their calendars are in the app database.
    fn two_accounts(base: &str) -> (GoogleState, AppDbHandle, crate::config::test_util::TempDir) {
        let f = fixture();
        f.db.upsert_account_calendars(Some("a@x"), true, &[cal("a@x", "A", true, true)], 1)
            .unwrap();
        f.db.upsert_account_calendars(
            Some("b@x"),
            false,
            &[
                cal("b@x", "B", true, true),
                cal("work@x", "Work", false, true),
            ],
            1,
        )
        .unwrap();
        let state = state_with(vec![
            ("a@x", client_for(base, "refresh-a")),
            ("b@x", client_for(base, "refresh-b")),
        ]);
        let (db, tmp) = db_handle(f);
        (state, db, tmp)
    }

    fn auth_of(req: &str) -> String {
        req.to_ascii_lowercase()
            .lines()
            .find_map(|l| l.strip_prefix("authorization: bearer ").map(str::to_string))
            .unwrap_or_default()
    }

    #[test]
    fn calendars_belong_to_their_account_and_old_rows_to_the_first() {
        let cals = vec![
            owned("a@x", calendar(1, "a@x", true, true, false)),
            owned("b@x", calendar(2, "b@x", true, true, false)),
            owned("b@x", calendar(3, "work@x", false, true, false)),
            owned("b@x", calendar(4, "hid@x", false, false, false)),
            owned("b@x", calendar(5, "old@x", false, true, true)),
            calendar(6, "legacy@x", false, true, false),
        ];
        assert_eq!(calendars_to_fetch(&cals, "a@x", "a@x"), ["a@x", "legacy@x"]);
        assert_eq!(calendars_to_fetch(&cals, "b@x", "a@x"), ["b@x", "work@x"]);
        // an account whose primary is not listed yet asks for its own `primary`
        assert_eq!(calendars_to_fetch(&cals, "c@x", "a@x"), ["primary"]);
    }

    #[test]
    fn the_new_block_calendar_is_the_choice_when_live_else_the_first_accounts_primary() {
        let cals = vec![
            owned("a@x", calendar(1, "a@x", true, true, false)),
            owned("b@x", calendar(2, "b@x", true, true, false)),
            owned("b@x", calendar(3, "work@x", false, true, false)),
            owned("b@x", calendar(4, "old@x", false, true, true)),
            owned("gone@x", calendar(5, "ghost@x", false, true, false)),
        ];
        let both = vec!["a@x".to_string(), "b@x".to_string()];
        let pick = |chosen: Option<&str>| new_event_calendar(&cals, &both, chosen);
        assert_eq!(pick(None).as_deref(), Some("a@x"));
        assert_eq!(pick(Some("work@x")).as_deref(), Some("work@x"));
        assert_eq!(pick(Some("old@x")).as_deref(), Some("a@x"), "archived");
        assert_eq!(
            pick(Some("ghost@x")).as_deref(),
            Some("a@x"),
            "disconnected account"
        );
        assert_eq!(pick(Some("nope")).as_deref(), Some("a@x"));
        // the first account is gone: the next one's primary
        let only_b = vec!["b@x".to_string()];
        assert_eq!(
            new_event_calendar(&cals, &only_b, Some("a@x")).as_deref(),
            Some("b@x")
        );
        // nothing known yet / nothing connected
        assert_eq!(new_event_calendar(&[], &both, None), None);
        assert_eq!(new_event_calendar(&cals, &[], None), None);
    }

    #[test]
    fn status_lists_accounts_in_order_with_their_reconnect_state() {
        let (base, _) = fake_server(vec![]);
        let revoked = fake_client(&base, Arc::new(MemStore::default()));
        let state = state_with(vec![("a@x", client_for(&base, "r")), ("b@x", revoked)]);
        let cals = vec![
            owned("a@x", calendar(1, "a@x", true, true, false)),
            owned("b@x", calendar(2, "work@x", false, true, false)),
        ];
        let status = state.status(&cals, Some("work@x"));
        assert_eq!(
            status,
            GoogleStatus {
                configured: true,
                connected: true,
                accounts: vec![
                    GoogleAccount {
                        email: "a@x".into(),
                        needs_reconnect: false
                    },
                    GoogleAccount {
                        email: "b@x".into(),
                        needs_reconnect: true
                    }
                ],
                needs_reconnect: true,
                new_event_calendar_id: Some("work@x".into()),
            }
        );
    }

    #[test]
    fn writes_go_to_the_account_that_owns_the_calendar() {
        let cals = vec![
            owned("a@x", calendar(1, "a@x", true, true, false)),
            owned("b@x", calendar(2, "work@x", false, true, false)),
            calendar(3, "legacy@x", false, true, false),
        ];
        let (base, _) = fake_server(vec![]);
        let accounts = vec![
            Account {
                email: "a@x".into(),
                client: Arc::new(client_for(&base, "r")),
            },
            Account {
                email: "b@x".into(),
                client: Arc::new(client_for(&base, "r")),
            },
        ];
        let who = |id: &str| account_for(&cals, id, &accounts).map(|a| a.email);
        assert_eq!(who("work@x").unwrap(), "b@x");
        assert_eq!(who("a@x").unwrap(), "a@x");
        assert_eq!(who("legacy@x").unwrap(), "a@x", "no account yet: the first");
        assert_eq!(who("primary").unwrap(), "a@x");
        assert_eq!(who("unknown@x").unwrap(), "a@x");
        // the owner is not connected
        let only_a = &accounts[..1];
        assert!(
            account_for(&cals, "work@x", only_a)
                .err()
                .unwrap()
                .contains("b@x")
        );
        assert!(account_for(&cals, "a@x", &[]).is_err());
    }

    fn evs(ids: &[(&str, &str)]) -> Vec<CalEvent> {
        ids.iter()
            .enumerate()
            .map(|(i, (id, cal))| CalEvent {
                calendar_id: (*cal).into(),
                ..ev(id, 100 * (i as u32 + 1), 100 * (i as u32 + 1) + 50)
            })
            .collect()
    }

    #[test]
    fn merging_days_shows_a_shared_event_once_from_the_first_account() {
        let a = evs(&[("same", "a@x"), ("a1", "a@x")]);
        let b = evs(&[("same", "work@x"), ("b1", "work@x")]);
        let out = merge_days(
            vec![("a@x".into(), Ok(a)), ("b@x".into(), Ok(b))],
            &[],
            |_| String::new(),
        );
        assert!(out.failed.is_empty());
        let got: Vec<_> = out
            .events
            .unwrap()
            .iter()
            .map(|e| (e.id.clone(), e.calendar_id.clone()))
            .collect();
        assert_eq!(
            got,
            [
                ("same".to_string(), "a@x".to_string()),
                ("a1".to_string(), "a@x".to_string()),
                ("b1".to_string(), "work@x".to_string())
            ]
        );
    }

    #[test]
    fn a_failing_account_keeps_its_cached_events_and_never_hides_the_others() {
        let previous = evs(&[("a-old", "a@x"), ("b-old", "work@x")]);
        let owner = |e: &CalEvent| {
            if e.calendar_id == "work@x" {
                "b@x"
            } else {
                "a@x"
            }
            .to_string()
        };
        let out = merge_days(
            vec![
                ("a@x".into(), Ok(evs(&[("a-new", "a@x")]))),
                ("b@x".into(), Err("Backend Error".into())),
            ],
            &previous,
            owner,
        );
        assert_eq!(
            out.failed,
            [("b@x".to_string(), "Backend Error".to_string())]
        );
        let ids: Vec<_> = out.events.unwrap().into_iter().map(|e| e.id).collect();
        assert_eq!(ids, ["a-new", "b-old"], "a-old is gone from A, b-old stays");
        // every account failing leaves the cache alone
        let out = merge_days(vec![("a@x".into(), Err("x".into()))], &previous, |_| {
            String::new()
        });
        assert_eq!(out.events, None);
        assert_eq!(out.failed.len(), 1);
    }

    #[test]
    fn errors_name_the_account() {
        assert_eq!(named("a@x", "Backend Error"), "a@x: Backend Error");
        assert_eq!(named(UNNAMED_ACCOUNT, "Backend Error"), "Backend Error");
    }

    #[test]
    fn the_day_is_fetched_from_every_account_with_real_calendar_ids() {
        let (base, seen) = fake_server(vec![
            tok_as("tokA"),
            two_events("a1", "From A"),
            tok_as("tokB"),
            two_events("b1", "From B primary"),
            two_events("b2", "From B work"),
        ]);
        let (state, db, _tmp) = two_accounts(&base);
        let day = run(fetch_all(
            &state,
            &known(&db).calendars,
            &[],
            "2026-10-21",
            &sp(),
        ));
        assert!(day.failed.is_empty());
        let got: Vec<_> = day
            .events
            .unwrap()
            .iter()
            .map(|e| (e.id.clone(), e.calendar_id.clone()))
            .collect();
        assert_eq!(
            got,
            [
                ("a1".to_string(), "a@x".to_string()),
                ("b1".to_string(), "b@x".to_string()),
                ("b2".to_string(), "work@x".to_string())
            ]
        );
        let reqs = seen.lock().unwrap();
        assert!(reqs[1].starts_with("GET /calendar/v3/calendars/a@x/events?"));
        assert_eq!(auth_of(&reqs[1]), "toka");
        assert!(reqs[3].starts_with("GET /calendar/v3/calendars/b@x/events?"));
        assert_eq!(auth_of(&reqs[3]), "tokb");
        assert!(reqs[4].starts_with("GET /calendar/v3/calendars/work@x/events?"));
        assert!(
            !reqs.iter().any(|r| r.contains("/calendars/primary/")),
            "never the literal primary once the real id is known"
        );
    }

    #[test]
    fn one_failing_account_is_reported_by_email_and_the_other_still_shows() {
        let boom = (500, r#"{"error":{"message":"Backend Error"}}"#.to_string());
        let (base, _) = fake_server(vec![
            tok_as("tokA"),
            boom,
            tok_as("tokB"),
            two_events("b1", "From B"),
            two_events("b2", "From B work"),
        ]);
        let (state, db, _tmp) = two_accounts(&base);
        let day = run(fetch_all(
            &state,
            &known(&db).calendars,
            &[],
            "2026-10-21",
            &sp(),
        ));
        assert_eq!(
            day.failed,
            [("a@x".to_string(), "Backend Error".to_string())]
        );
        assert_eq!(day.events.unwrap().len(), 2);
        assert_eq!(
            named(&day.failed[0].0, &day.failed[0].1),
            "a@x: Backend Error"
        );
    }

    #[test]
    fn load_projects_reads_each_accounts_calendars_and_series() {
        let f = fixture();
        let (base, seen) = fake_server(vec![
            tok_as("tokA"),
            cal_list(
                r##"{"id":"a@x","summary":"a@x","primary":true,"selected":true,"backgroundColor":"#111111"}"##,
                None,
            ),
            tok_as("tokB"),
            cal_list(
                r##"{"id":"b@x","summary":"b@x","primary":true,"selected":true,"backgroundColor":"#222222"},{"id":"work@x","summary":"Work","selected":true,"backgroundColor":"#333333"}"##,
                None,
            ),
            (
                200,
                r#"{"items":[{"id":"sa","summary":"Learn Rust","recurrence":["RRULE:FREQ=DAILY"]}]}"#.into(),
            ),
            (
                200,
                r#"{"items":[{"id":"sb","summary":"Standup","recurrence":["RRULE:FREQ=WEEKLY"]}]}"#.into(),
            ),
            (
                200,
                r#"{"items":[{"id":"sw","summary":"Thesis","recurrence":["RRULE:FREQ=DAILY"]}]}"#.into(),
            ),
        ]);
        let state = state_with(vec![
            ("a@x", client_for(&base, "refresh-a")),
            ("b@x", client_for(&base, "refresh-b")),
        ]);
        assert!(run(load_projects(&state, &f.db)).unwrap());
        let cals: Vec<_> =
            f.db.calendars()
                .unwrap()
                .into_iter()
                .map(|c| (c.google_calendar_id, c.account, c.is_primary))
                .collect();
        assert_eq!(
            cals,
            [
                ("a@x".to_string(), Some("a@x".to_string()), true),
                ("b@x".to_string(), Some("b@x".to_string()), true),
                ("work@x".to_string(), Some("b@x".to_string()), false),
            ]
        );
        let names: Vec<_> =
            f.db.projects()
                .unwrap()
                .into_iter()
                .map(|p| p.name)
                .collect();
        assert_eq!(names, ["Learn Rust", "Standup", "Thesis"]);
        let reqs = seen.lock().unwrap();
        // every account's calendar list first, then the series of each account's calendars
        assert!(reqs[1].starts_with("GET /calendar/v3/users/me/calendarList?"));
        assert_eq!(auth_of(&reqs[1]), "toka");
        assert_eq!(auth_of(&reqs[3]), "tokb");
        assert!(reqs[4].starts_with("GET /calendar/v3/calendars/a@x/events?"));
        assert_eq!(auth_of(&reqs[4]), "toka");
        assert!(reqs[5].starts_with("GET /calendar/v3/calendars/b@x/events?"));
        assert_eq!(auth_of(&reqs[5]), "tokb");
        assert!(reqs[6].starts_with("GET /calendar/v3/calendars/work@x/events?"));
    }

    #[test]
    fn an_account_that_cannot_be_read_does_not_stop_the_others_or_lose_its_calendars() {
        let f = fixture();
        f.db.upsert_account_calendars(Some("b@x"), false, &[cal("b@x", "B", true, true)], 1)
            .unwrap();
        let boom = (500, r#"{"error":{"message":"Backend Error"}}"#.to_string());
        let (base, _) = fake_server(vec![
            tok_as("tokA"),
            boom,
            tok_as("tokB"),
            cal_list(
                r##"{"id":"b@x","summary":"b@x","primary":true,"selected":true}"##,
                None,
            ),
            (200, r#"{"items":[]}"#.into()),
        ]);
        let state = state_with(vec![
            ("a@x", client_for(&base, "ra")),
            ("b@x", client_for(&base, "rb")),
        ]);
        assert!(run(load_projects(&state, &f.db)).unwrap());
        assert_eq!(f.db.calendars().unwrap().len(), 1);
        // every account failing is an error
        let (base, _) = fake_server(vec![
            tok_as("tokA"),
            (500, r#"{"error":{"message":"Backend Error"}}"#.into()),
            tok_as("tokB"),
            (500, r#"{"error":{"message":"Backend Error"}}"#.into()),
        ]);
        let state = state_with(vec![
            ("a@x", client_for(&base, "ra")),
            ("b@x", client_for(&base, "rb")),
        ]);
        assert_eq!(
            run(load_projects(&state, &f.db)).unwrap_err(),
            "Backend Error"
        );
    }

    #[test]
    fn calendars_of_an_account_that_left_the_list_are_archived_by_the_refresh() {
        let f = fixture();
        f.db.upsert_account_calendars(Some("gone@x"), false, &[cal("g@x", "G", true, true)], 1)
            .unwrap();
        let (base, _) = fake_server(vec![
            tok_as("tokA"),
            cal_list(
                r##"{"id":"a@x","summary":"a@x","primary":true,"selected":true}"##,
                None,
            ),
            (200, r#"{"items":[]}"#.into()),
        ]);
        let state = state_with(vec![("a@x", client_for(&base, "ra"))]);
        assert!(run(load_projects(&state, &f.db)).unwrap());
        let g =
            f.db.calendars()
                .unwrap()
                .into_iter()
                .find(|c| c.google_calendar_id == "g@x")
                .unwrap();
        assert!(g.archived);
    }

    #[test]
    fn creating_goes_to_the_chosen_calendar_of_its_account_and_carries_the_tags() {
        let created = r#"{"id":"n1","summary":"Deep","start":{"dateTime":"2026-10-21T09:00:00-03:00"},"end":{"dateTime":"2026-10-21T10:00:00-03:00"},"extendedProperties":{"private":{"catengaTags":"rust"}}}"#;
        let (base, seen) = fake_server(vec![
            tok_as("tokA"),
            (200, created.to_string()),
            tok_as("tokB"),
            (200, created.to_string()),
        ]);
        let (state, db, _tmp) = two_accounts(&base);
        let tags = vec!["rust".to_string()];
        let new_block = || {
            run(create_event(
                &state,
                &no_index(),
                &db,
                ("2026-10-21", 540, 600),
                "Deep",
                &tags,
            ))
            .unwrap()
        };
        // by default: the primary calendar of the first account
        assert_eq!(new_block().calendar_id, "a@x");
        // after choosing the work calendar of the second account the block lands there
        let status = set_new_event_calendar(&state, &db, "work@x").unwrap();
        assert_eq!(status.new_event_calendar_id.as_deref(), Some("work@x"));
        let e = new_block();
        assert_eq!(
            (e.calendar_id.as_str(), e.tags.as_slice()),
            ("work@x", ["rust".to_string()].as_slice())
        );
        let reqs = seen.lock().unwrap();
        assert!(
            reqs[1].starts_with("POST /calendar/v3/calendars/a@x/events "),
            "{}",
            reqs[1]
        );
        assert_eq!(auth_of(&reqs[1]), "toka");
        assert!(
            reqs[3].starts_with("POST /calendar/v3/calendars/work@x/events "),
            "{}",
            reqs[3]
        );
        assert_eq!(auth_of(&reqs[3]), "tokb");
        assert_eq!(
            body_json(&reqs[3])["extendedProperties"]["private"]["catengaTags"],
            "rust"
        );
    }

    fn body_json(req: &str) -> serde_json::Value {
        serde_json::from_str(req.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    #[test]
    fn choosing_a_calendar_that_is_not_available_is_refused() {
        let (base, _) = fake_server(vec![]);
        let (state, db, _tmp) = two_accounts(&base);
        assert!(set_new_event_calendar(&state, &db, "stranger@x").is_err());
        assert_eq!(db.get().unwrap().meta(NEW_EVENT_CALENDAR).unwrap(), None);
        set_new_event_calendar(&state, &db, "b@x").unwrap();
        assert_eq!(
            state.status_with(&db).new_event_calendar_id.as_deref(),
            Some("b@x")
        );
        // the choice survives a restart (it is in the database) but not a disconnection of its account
        let only_a = state_with(vec![("a@x", client_for(&base, "ra"))]);
        assert_eq!(
            only_a.status_with(&db).new_event_calendar_id.as_deref(),
            Some("a@x")
        );
    }

    #[test]
    fn update_and_delete_use_the_account_of_the_cached_events_calendar() {
        let updated = r#"{"id":"w1","summary":"Moved","start":{"dateTime":"2026-10-21T09:00:00-03:00"},"end":{"dateTime":"2026-10-21T10:00:00-03:00"}}"#;
        let (base, seen) = fake_server(vec![
            tok_as("tokB"),
            (200, updated.to_string()),
            (204, String::new()),
        ]);
        let (state, db, _tmp) = two_accounts(&base);
        let index = no_index();
        state.cache_put(&index, "2026-10-21", &[ev_in("w1", "work@x")], 5);
        let tags = vec!["go".to_string()];
        let e = run(update_event(
            &state,
            &index,
            &db,
            ("2026-10-21", "w1"),
            (540, 600),
            "Moved",
            &tags,
        ))
        .unwrap();
        assert_eq!(e.calendar_id, "work@x");
        run(delete_event(&state, &index, &db, "2026-10-21", "w1")).unwrap();
        let reqs = seen.lock().unwrap();
        assert!(reqs[1].starts_with("PATCH /calendar/v3/calendars/work@x/events/w1 "));
        assert_eq!(auth_of(&reqs[1]), "tokb");
        assert_eq!(
            body_json(&reqs[1])["extendedProperties"]["private"]["catengaTags"],
            "go"
        );
        assert!(reqs[2].starts_with("DELETE /calendar/v3/calendars/work@x/events/w1 "));
        assert_eq!(auth_of(&reqs[2]), "tokb");
        assert!(state.cache_get(&index, "2026-10-21").unwrap().0.is_empty());
    }

    #[test]
    fn tags_on_an_instance_patch_the_master_then_the_instance_and_the_cached_day() {
        let instance_resp = r#"{"id":"s1_2026","recurringEventId":"s1","summary":"Learn Rust","start":{"dateTime":"2026-10-21T09:00:00-03:00"},"end":{"dateTime":"2026-10-21T10:00:00-03:00"},"extendedProperties":{"private":{"catengaTags":"rust mestrado"}}}"#;
        let master_resp = r#"{"id":"s1","summary":"Learn Rust","start":{"dateTime":"2026-01-05T09:00:00-03:00"},"end":{"dateTime":"2026-01-05T10:00:00-03:00"}}"#;
        let (base, seen) = fake_server(vec![
            tok_as("tokB"),
            (200, master_resp.to_string()),
            (200, instance_resp.to_string()),
        ]);
        let (state, db, _tmp) = two_accounts(&base);
        let index = no_index();
        let mut other = instance("s1_other", "work@x", "s1");
        other.start = 700;
        other.end = 800;
        let mut unrelated = ev_in("x", "work@x");
        unrelated.start = 900;
        unrelated.end = 950;
        state.cache_put(
            &index,
            "2026-10-21",
            &[instance("s1_2026", "work@x", "s1"), other, unrelated],
            7,
        );
        let tags = vec!["rust".to_string(), "#mestrado".to_string()];
        let e = run(set_event_tags(
            &state,
            &index,
            &db,
            ("2026-10-21", "s1_2026"),
            &tags,
        ))
        .unwrap();
        assert_eq!(e.tags, ["rust", "mestrado"]);
        assert_eq!(e.project_key.as_deref(), Some("series:s1"));
        let reqs = seen.lock().unwrap();
        assert_eq!(reqs.len(), 3);
        assert!(
            reqs[1].starts_with("PATCH /calendar/v3/calendars/work@x/events/s1 "),
            "{}",
            reqs[1]
        );
        assert!(
            reqs[2].starts_with("PATCH /calendar/v3/calendars/work@x/events/s1_2026 "),
            "{}",
            reqs[2]
        );
        for r in &reqs[1..] {
            assert_eq!(auth_of(r), "tokb");
            assert_eq!(
                body_json(r),
                serde_json::json!({"extendedProperties":{"private":{"catengaTags":"rust mestrado"}}})
            );
        }
        let (cached, at) = state.cache_get(&index, "2026-10-21").unwrap();
        assert_eq!(at, 7);
        let tags_of = |id: &str| cached.iter().find(|e| e.id == id).unwrap().tags.clone();
        assert_eq!(tags_of("s1_2026"), ["rust", "mestrado"]);
        assert_eq!(
            tags_of("s1_other"),
            ["rust", "mestrado"],
            "same series, same day"
        );
        assert!(tags_of("x").is_empty());
    }

    #[test]
    fn tags_on_a_one_off_event_patch_only_that_event_and_can_be_cleared() {
        let resp = r#"{"id":"one","summary":"Dentist","start":{"dateTime":"2026-10-21T09:00:00-03:00"},"end":{"dateTime":"2026-10-21T10:00:00-03:00"}}"#;
        let (base, seen) = fake_server(vec![tok_as("tokA"), (200, resp.to_string())]);
        let (state, db, _tmp) = two_accounts(&base);
        let index = no_index();
        let mut tagged = ev_in("one", "a@x");
        tagged.tags = vec!["old".into()];
        state.cache_put(&index, "2026-10-21", &[tagged], 7);
        let e = run(set_event_tags(
            &state,
            &index,
            &db,
            ("2026-10-21", "one"),
            &[],
        ))
        .unwrap();
        assert!(e.tags.is_empty());
        let reqs = seen.lock().unwrap();
        assert_eq!(reqs.len(), 2, "no master to patch");
        assert!(reqs[1].starts_with("PATCH /calendar/v3/calendars/a@x/events/one "));
        assert_eq!(
            body_json(&reqs[1]),
            serde_json::json!({"extendedProperties":{"private":{"catengaTags":""}}})
        );
        assert!(
            state.cache_get(&index, "2026-10-21").unwrap().0[0]
                .tags
                .is_empty()
        );
    }

    #[test]
    fn tags_need_a_cached_event_and_valid_input() {
        let (base, seen) = fake_server(vec![]);
        let (state, db, _tmp) = two_accounts(&base);
        let index = no_index();
        let err = |date: &str, id: &str| {
            run(set_event_tags(&state, &index, &db, (date, id), &[])).unwrap_err()
        };
        assert_eq!(
            err("2026-10-21", "nope"),
            "Event not found; refresh the day and try again"
        );
        assert_eq!(err("nope", "x"), "invalid date nope");
        assert_eq!(err("2026-10-21", ""), "missing event id");
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn connecting_names_the_account_by_its_primary_calendar_and_saves_it_under_that_email() {
        let consent = r#"{"access_token":"a","expires_in":3600,"refresh_token":"NEW","scope":"https://www.googleapis.com/auth/calendar.events https://www.googleapis.com/auth/calendar.calendarlist.readonly"}"#;
        let list = cal_list(
            r##"{"id":"me@work.com","summary":"Me","primary":true}"##,
            None,
        );
        let (base, seen) = fake_server(vec![(200, consent.into()), list.clone()]);
        let vault = Arc::new(MemVault::default());
        let state = state_on(&base, vault.clone());
        let email = run(connect_with_code(
            &state,
            "4/code",
            "verifier",
            "http://127.0.0.1:9/callback",
        ))
        .unwrap();
        assert_eq!(email, "me@work.com");
        assert_eq!(vault.accounts(), ["me@work.com"]);
        let store = vault.store("me@work.com");
        assert_eq!(store.refresh_token().unwrap().as_deref(), Some("NEW"));
        assert!(crate::google::has_list_scope(&store.scope().unwrap()));
        assert_eq!(state.emails(), ["me@work.com"]);
        assert!(!state.status(&[], None).needs_reconnect);
        assert!(seen.lock().unwrap()[0].contains("grant_type=authorization_code"));

        // a second account is added after the first
        let consent2 = consent.replace("NEW", "SECOND");
        let list2 = cal_list(
            r##"{"id":"me@home.com","summary":"Me","primary":true}"##,
            None,
        );
        let (base, _) = fake_server(vec![(200, consent2), list2]);
        let state2 = state_on(&base, vault.clone());
        run(connect_with_code(
            &state2,
            "c",
            "v",
            "http://127.0.0.1:9/callback",
        ))
        .unwrap();
        assert_eq!(vault.accounts(), ["me@work.com", "me@home.com"]);
        assert_eq!(state2.emails(), ["me@work.com", "me@home.com"]);

        // connecting an email that is already there replaces its token and keeps its place
        let consent3 = consent.replace("NEW", "RECONNECTED");
        let (base, _) = fake_server(vec![(200, consent3), list]);
        let state3 = state_on(&base, vault.clone());
        assert_eq!(state3.emails().len(), 2);
        run(connect_with_code(
            &state3,
            "c",
            "v",
            "http://127.0.0.1:9/callback",
        ))
        .unwrap();
        assert_eq!(vault.accounts(), ["me@work.com", "me@home.com"]);
        assert_eq!(
            vault
                .store("me@work.com")
                .refresh_token()
                .unwrap()
                .as_deref(),
            Some("RECONNECTED")
        );
        assert_eq!(state3.emails(), ["me@work.com", "me@home.com"]);
    }

    #[test]
    fn a_failed_connection_saves_nothing() {
        // no refresh token in the answer
        let (base, _) = fake_server(vec![tok_as("a")]);
        let vault = Arc::new(MemVault::default());
        let state = state_on(&base, vault.clone());
        assert!(
            run(connect_with_code(
                &state,
                "c",
                "v",
                "http://127.0.0.1:9/callback"
            ))
            .is_err()
        );
        assert!(vault.accounts().is_empty() && state.emails().is_empty());
        // the account cannot be named
        let consent = r#"{"access_token":"a","expires_in":3600,"refresh_token":"NEW"}"#;
        let (base, _) = fake_server(vec![
            (200, consent.into()),
            cal_list("", None),
            (200, "{}".into()),
        ]);
        let state = state_on(&base, vault.clone());
        assert!(
            run(connect_with_code(
                &state,
                "c",
                "v",
                "http://127.0.0.1:9/callback"
            ))
            .is_err()
        );
        assert!(vault.accounts().is_empty());
        assert!(vault.stores.lock().unwrap().is_empty());
    }

    #[test]
    fn disconnecting_one_account_keeps_the_other_and_archives_its_calendars() {
        let (base, seen) = fake_server(vec![(200, "{}".into())]);
        let vault = Arc::new(MemVault::default());
        vault
            .set_accounts(&["a@x".to_string(), "b@x".to_string()])
            .unwrap();
        vault.account("a@x").set_refresh_token("ra").unwrap();
        vault.account("b@x").set_refresh_token("rb").unwrap();
        let state = state_on(&base, vault.clone());
        let f = fixture();
        f.db.upsert_account_calendars(Some("a@x"), true, &[cal("a@x", "A", true, true)], 1)
            .unwrap();
        f.db.upsert_account_calendars(Some("b@x"), false, &[cal("b@x", "B", true, true)], 1)
            .unwrap();
        let (db, _tmp) = db_handle(f);
        let index = no_index();
        state.cache_put(
            &index,
            "2026-10-21",
            &[ev_in("a1", "a@x"), ev_in("b1", "b@x")],
            5,
        );
        assert_eq!(state.emails(), ["a@x", "b@x"]);

        run(disconnect_account(&state, &index, &db, "b@x")).unwrap();
        assert!(seen.lock().unwrap()[0].starts_with("POST /revoke?token=rb"));
        assert_eq!(vault.accounts(), ["a@x"]);
        assert_eq!(vault.store("b@x").refresh_token().unwrap(), None);
        assert_eq!(
            vault.store("a@x").refresh_token().unwrap().as_deref(),
            Some("ra")
        );
        assert_eq!(state.emails(), ["a@x"]);
        let cals = db.get().unwrap().calendars().unwrap();
        assert!(
            cals.iter()
                .find(|c| c.google_calendar_id == "b@x")
                .unwrap()
                .archived
        );
        assert!(
            !cals
                .iter()
                .find(|c| c.google_calendar_id == "a@x")
                .unwrap()
                .archived
        );
        // its events no longer show
        let shown = enrich(
            state.cache_get(&index, "2026-10-21").unwrap().0,
            &known(&db),
        );
        assert_eq!(
            shown.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["a1"]
        );
        assert!(state.status_with(&db).connected);

        // the last one: everything goes, the cache too
        let (base, _) = fake_server(vec![(200, "{}".into())]);
        let state = state_on(&base, vault.clone());
        run(disconnect_account(&state, &index, &db, "a@x")).unwrap();
        assert!(vault.accounts().is_empty());
        assert!(!state.status_with(&db).connected);
        assert_eq!(state.cache_get(&index, "2026-10-21"), None);
        // an unknown account is an error
        assert!(run(disconnect_account(&state, &index, &db, "zzz@x")).is_err());
    }

    #[test]
    fn a_single_account_install_moves_to_the_per_account_entries_without_losing_the_token() {
        let vault = Arc::new(MemVault::default());
        vault
            .legacy_store
            .set_refresh_token("legacy-refresh")
            .unwrap();
        vault.legacy_store.set_scope(Some("scope-a scope-b"));
        *vault.legacy_email.lock().unwrap() = Some("me@x".into());
        let (base, _) = fake_server(vec![]);
        let state = state_on(&base, vault.clone());
        assert_eq!(state.emails(), ["me@x"]);
        assert_eq!(vault.accounts(), ["me@x"]);
        assert_eq!(
            vault.store("me@x").refresh_token().unwrap().as_deref(),
            Some("legacy-refresh")
        );
        assert!(vault.legacy().is_none());
        assert!(state.client_of("me@x").unwrap().is_connected());
    }

    #[test]
    fn a_legacy_token_without_an_email_is_named_in_the_background_and_kept_when_that_fails() {
        let vault = Arc::new(MemVault::default());
        vault
            .legacy_store
            .set_refresh_token("legacy-refresh")
            .unwrap();
        *vault.legacy_email.lock().unwrap() = Some("Ismael's calendar".into());
        // naming fails (offline): the legacy token keeps working as an unnamed account
        let (base, _) = fake_server(vec![
            tok_as("t"),
            (500, r#"{"error":{"message":"Backend Error"}}"#.into()),
            (500, r#"{"error":{"message":"Backend Error"}}"#.into()),
        ]);
        let state = state_on(&base, vault.clone());
        assert_eq!(state.emails(), [UNNAMED_ACCOUNT]);
        assert!(state.status(&[], None).connected);
        run(state.migrate_legacy_account());
        assert_eq!(state.emails(), [UNNAMED_ACCOUNT]);
        assert!(
            vault.legacy().is_some(),
            "never deleted before the new entries exist"
        );
        assert!(vault.accounts().is_empty());
        // the next start names it
        let (base, _) = fake_server(vec![
            tok_as("t"),
            cal_list(r##"{"id":"me@x","summary":"me@x","primary":true}"##, None),
        ]);
        let state = state_on(&base, vault.clone());
        assert_eq!(state.emails(), [UNNAMED_ACCOUNT]);
        run(state.migrate_legacy_account());
        assert_eq!(state.emails(), ["me@x"]);
        assert_eq!(vault.accounts(), ["me@x"]);
        assert_eq!(
            vault.store("me@x").refresh_token().unwrap().as_deref(),
            Some("legacy-refresh")
        );
        assert!(vault.legacy().is_none());
    }

    #[test]
    fn nothing_is_read_without_credentials() {
        let vault = Arc::new(MemVault::default());
        vault
            .legacy_store
            .set_refresh_token("legacy-refresh")
            .unwrap();
        let state = GoogleState::with(None, Endpoints::default(), vault.clone());
        assert!(state.emails().is_empty() && !state.connected());
        assert!(vault.legacy().is_some(), "left alone");
    }
}
