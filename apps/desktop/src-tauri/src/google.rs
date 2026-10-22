//! Google Calendar, pure parts: OAuth URL and token parsing, event JSON to
//! timeline events, day bounds and request bodies. No network, no Tauri; every
//! time-zone conversion takes a `&TimeZone` so tests use fixed zones.

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const SCOPE: &str = "https://www.googleapis.com/auth/calendar.events";
/// Lets us list the user's calendars (the projects).
pub const CALENDAR_LIST_SCOPE: &str =
    "https://www.googleapis.com/auth/calendar.calendarlist.readonly";
/// Fallback when Google sends no color.
pub const DEFAULT_CALENDAR_COLOR: &str = "#4285f4";

/// True when a token response's space-separated `scope` includes the calendar list.
pub fn has_list_scope(scope: &str) -> bool {
    scope.split_whitespace().any(|s| s == CALENDAR_LIST_SCOPE)
}
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// Refresh this long before the access token really expires.
const REFRESH_MARGIN_MS: i64 = 60_000;
pub const NO_TITLE: &str = "(no title)";

/// A timed Google event reduced to one day of the timeline (minutes from 00:00).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalEvent {
    pub id: String,
    pub start: u32,
    pub end: u32,
    pub title: String,
    /// Google calendar the event belongs to (`primary` until the primary
    /// calendar's real id is known). Backend only.
    #[serde(skip_serializing, default)]
    pub calendar_id: String,
    /// Cached for later use; not part of the frontend contract.
    #[serde(skip_serializing, default)]
    pub html_link: Option<String>,
    /// The project (DB id) of the event's calendar; filled when the events are
    /// handed to the frontend.
    #[serde(default)]
    pub project_id: Option<i64>,
    /// The project's color (`#rrggbb`), or `""` when the calendar has no project yet.
    #[serde(default)]
    pub color: String,
}

/// A calendar of the user's Google calendar list (a project).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarInfo {
    pub id: String,
    pub name: String,
    pub color: String,
    pub primary: bool,
    /// Whether Google shows it in the user's calendar list.
    pub selected: bool,
}

/// PKCE challenge: 43 base64url characters (SHA-256 digest).
pub fn valid_challenge(s: &str) -> bool {
    s.len() == 43
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// PKCE verifier (RFC 7636): 43 to 128 unreserved characters.
pub fn valid_verifier(s: &str) -> bool {
    (43..=128).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// The browser URL that starts the consent flow. `challenge` is the PKCE S256
/// challenge.
pub fn auth_url(client_id: &str, redirect_uri: &str, challenge: &str, state: &str) -> String {
    let mut url = url::Url::parse(AUTH_URL).expect("constant URL");
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("client_id", client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", &format!("{SCOPE} {CALENDAR_LIST_SCOPE}"));
        q.append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state)
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent");
    }
    url.into()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tokens {
    pub access_token: String,
    /// Seconds until the access token expires.
    pub expires_in: i64,
    /// Only present on the code exchange (and sometimes on refresh).
    pub refresh_token: Option<String>,
    /// The scopes this token carries (space separated), when Google says.
    pub scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    /// The refresh token was revoked or expired: the user must connect again.
    InvalidGrant,
    Other(String),
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenError::InvalidGrant => write!(f, "Google Calendar was disconnected"),
            TokenError::Other(m) => write!(f, "{m}"),
        }
    }
}

#[derive(Deserialize)]
struct TokenBody {
    access_token: Option<String>,
    expires_in: Option<i64>,
    refresh_token: Option<String>,
    scope: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Interprets a response of Google's token endpoint.
pub fn parse_token_response(status: u16, body: &str) -> Result<Tokens, TokenError> {
    let parsed: TokenBody = serde_json::from_str(body).map_err(|_| {
        TokenError::Other(format!("unexpected response from Google (HTTP {status})"))
    })?;
    if let Some(error) = parsed.error {
        if error == "invalid_grant" {
            return Err(TokenError::InvalidGrant);
        }
        let text = parsed.error_description.unwrap_or(error);
        return Err(TokenError::Other(text));
    }
    match parsed.access_token {
        Some(access_token) if (200..300).contains(&status) => Ok(Tokens {
            access_token,
            expires_in: parsed.expires_in.unwrap_or(3600),
            refresh_token: parsed.refresh_token,
            scope: parsed.scope,
        }),
        _ => Err(TokenError::Other(format!(
            "unexpected response from Google (HTTP {status})"
        ))),
    }
}

/// True when there is no access token, or it expires within the margin.
pub fn needs_refresh(expires_at_ms: Option<i64>, now_ms: i64) -> bool {
    match expires_at_ms {
        Some(at) => at - now_ms <= REFRESH_MARGIN_MS,
        None => true,
    }
}

fn parse_date(date: &str) -> Result<Date, String> {
    date.parse::<Date>()
        .map_err(|e| format!("invalid date {date}: {e}"))
}

fn rfc3339(z: &jiff::Zoned) -> String {
    z.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

/// `(timeMin, timeMax)` for the local day `date` (`YYYY-MM-DD`) in `tz`, as
/// RFC 3339 with offset. DST days are 23 or 25 hours long.
pub fn day_bounds(date: &str, tz: &TimeZone) -> Result<(String, String), String> {
    let day = parse_date(date)?;
    let start = day.to_zoned(tz.clone()).map_err(|e| e.to_string())?;
    let next = day
        .tomorrow()
        .map_err(|e| e.to_string())?
        .to_zoned(tz.clone())
        .map_err(|e| e.to_string())?;
    Ok((rfc3339(&start), rfc3339(&next)))
}

#[derive(Deserialize)]
struct EventTime {
    #[serde(rename = "dateTime")]
    date_time: Option<String>,
}

#[derive(Deserialize)]
struct Item {
    id: Option<String>,
    status: Option<String>,
    summary: Option<String>,
    start: Option<EventTime>,
    end: Option<EventTime>,
    #[serde(rename = "htmlLink")]
    html_link: Option<String>,
}

#[derive(Deserialize)]
struct ListBody {
    summary: Option<String>,
    #[serde(default)]
    items: Vec<Item>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct DayList {
    pub events: Vec<CalEvent>,
    /// Calendar title (`summary`); for a primary calendar usually the account email.
    pub summary: Option<String>,
    pub next_page_token: Option<String>,
}

fn wall_minutes(z: &jiff::Zoned) -> u32 {
    u32::try_from(z.hour()).unwrap_or(0) * 60 + u32::try_from(z.minute()).unwrap_or(0)
}

/// One timed, non-cancelled event clamped to `day`; `None` for anything the
/// timeline does not show (all-day, cancelled, empty, outside the day).
fn to_cal_event(item: Item, day: Date, tz: &TimeZone) -> Option<CalEvent> {
    if item.status.as_deref() == Some("cancelled") {
        return None;
    }
    let start_ts: Timestamp = item.start?.date_time?.parse().ok()?;
    let end_ts: Timestamp = item.end?.date_time?.parse().ok()?;
    let start = start_ts.to_zoned(tz.clone());
    let end = end_ts.to_zoned(tz.clone());
    if start.date() > day || end.date() < day {
        return None;
    }
    let start_min = if start.date() < day {
        0
    } else {
        wall_minutes(&start)
    };
    let end_min = if end.date() > day {
        1440
    } else {
        wall_minutes(&end)
    };
    if end_min <= start_min {
        return None;
    }
    let title = item
        .summary
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| NO_TITLE.to_string());
    Some(CalEvent {
        id: item.id?,
        start: start_min,
        end: end_min,
        title,
        calendar_id: String::new(),
        html_link: item.html_link,
        project_id: None,
        color: String::new(),
    })
}

/// Parses one page of `events.list` for the local day `date`.
pub fn parse_event_list(body: &str, date: &str, tz: &TimeZone) -> Result<DayList, String> {
    let day = parse_date(date)?;
    let list: ListBody =
        serde_json::from_str(body).map_err(|e| format!("unexpected events response: {e}"))?;
    let events = list
        .items
        .into_iter()
        .filter_map(|item| to_cal_event(item, day, tz))
        .collect();
    Ok(DayList {
        events,
        summary: list.summary,
        next_page_token: list.next_page_token,
    })
}

/// Parses the event resource returned by create/patch.
pub fn parse_event(body: &str, date: &str, tz: &TimeZone) -> Result<CalEvent, String> {
    let day = parse_date(date)?;
    let item: Item =
        serde_json::from_str(body).map_err(|e| format!("unexpected event response: {e}"))?;
    to_cal_event(item, day, tz).ok_or_else(|| "Google returned an event outside the day".into())
}

#[derive(Deserialize)]
struct CalendarItem {
    id: Option<String>,
    summary: Option<String>,
    #[serde(rename = "summaryOverride")]
    summary_override: Option<String>,
    #[serde(rename = "backgroundColor")]
    background_color: Option<String>,
    #[serde(default)]
    primary: bool,
    #[serde(default)]
    selected: bool,
    #[serde(default)]
    deleted: bool,
}

#[derive(Deserialize)]
struct CalendarListBody {
    #[serde(default)]
    items: Vec<CalendarItem>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct CalendarPage {
    pub calendars: Vec<CalendarInfo>,
    pub next_page_token: Option<String>,
}

/// Parses one page of `calendarList.list`. The name is the user's own rename
/// when there is one; deleted calendars are dropped.
pub fn parse_calendar_list(body: &str) -> Result<CalendarPage, String> {
    let list: CalendarListBody =
        serde_json::from_str(body).map_err(|e| format!("unexpected calendar list: {e}"))?;
    let nonblank = |s: Option<String>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let calendars = list
        .items
        .into_iter()
        .filter(|c| !c.deleted)
        .filter_map(|c| {
            let id = c.id?;
            let name = nonblank(c.summary_override)
                .or_else(|| nonblank(c.summary))
                .unwrap_or_else(|| id.clone());
            Some(CalendarInfo {
                color: nonblank(c.background_color)
                    .unwrap_or_else(|| DEFAULT_CALENDAR_COLOR.to_string()),
                id,
                name,
                primary: c.primary,
                selected: c.selected,
            })
        })
        .collect();
    Ok(CalendarPage {
        calendars,
        next_page_token: list.next_page_token,
    })
}

/// The account email when the calendar title looks like one (a renamed primary
/// calendar shows a name instead, and then we only say "Connected").
pub fn account_email(summary: Option<&str>) -> Option<String> {
    let s = summary?.trim();
    (s.contains('@') && !s.contains(char::is_whitespace)).then(|| s.to_string())
}

fn wall_clock(day: Date, minutes: u32) -> Result<jiff::civil::DateTime, String> {
    day.at(0, 0, 0, 0)
        .checked_add(SignedDuration::from_mins(i64::from(minutes)))
        .map_err(|e| e.to_string())
}

/// Body of `events.insert` / `events.patch`: local wall-clock times in the
/// system zone (`timeZone` is omitted, and an offset sent, when the zone has no IANA name).
pub fn event_body(
    date: &str,
    start: u32,
    end: u32,
    title: &str,
    tz: &TimeZone,
) -> Result<Value, String> {
    if start >= end || end > 1440 {
        return Err("invalid time range".into());
    }
    let day = parse_date(date)?;
    let time = |minutes: u32| -> Result<Value, String> {
        let local = wall_clock(day, minutes)?;
        match tz.iana_name() {
            Some(name) => Ok(json!({
                "dateTime": local.strftime("%Y-%m-%dT%H:%M:%S").to_string(),
                "timeZone": name,
            })),
            None => {
                let zoned = local.to_zoned(tz.clone()).map_err(|e| e.to_string())?;
                Ok(json!({ "dateTime": rfc3339(&zoned) }))
            }
        }
    };
    Ok(json!({ "summary": title, "start": time(start)?, "end": time(end)? }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> TimeZone {
        TimeZone::get("America/Sao_Paulo").unwrap()
    }
    fn berlin() -> TimeZone {
        TimeZone::get("Europe/Berlin").unwrap()
    }

    fn list(items: &str) -> String {
        format!(r#"{{"summary":"me@example.com","items":[{items}]}}"#)
    }

    fn timed(id: &str, title: &str, start: &str, end: &str) -> String {
        format!(
            r#"{{"id":"{id}","status":"confirmed","summary":"{title}","start":{{"dateTime":"{start}"}},"end":{{"dateTime":"{end}"}},"htmlLink":"https://l/{id}"}}"#
        )
    }

    #[test]
    fn auth_url_has_every_parameter_encoded() {
        let u = auth_url("cid.apps", "http://127.0.0.1:5555/callback", "CHAL", "st_1");
        let url = url::Url::parse(&u).unwrap();
        assert_eq!(url.host_str(), Some("accounts.google.com"));
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], "cid.apps");
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:5555/callback");
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["scope"], format!("{SCOPE} {CALENDAR_LIST_SCOPE}"));
        assert_eq!(q["code_challenge"], "CHAL");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["state"], "st_1");
        assert_eq!(q["access_type"], "offline");
        assert_eq!(q["prompt"], "consent");
        assert!(u.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5555%2Fcallback"));
    }

    #[test]
    fn pkce_values_are_validated() {
        let ch = "E9Melhoa2OwvFrEMTJguCHoeRZt8hn-a8_6sCjgJELE";
        assert!(valid_challenge(ch));
        assert!(!valid_challenge(&ch[1..]));
        assert!(!valid_challenge(&format!("{}+", &ch[1..])));
        let v43 = "a".repeat(43);
        assert!(valid_verifier(&v43));
        assert!(valid_verifier(&"a-._~".repeat(25)));
        assert!(!valid_verifier(&"a".repeat(42)));
        assert!(!valid_verifier(&"a".repeat(129)));
        assert!(!valid_verifier(&format!("{}/", "a".repeat(43))));
        assert!(!valid_verifier(&format!("{}&", "a".repeat(43))));
    }

    #[test]
    fn token_response_ok_with_and_without_refresh_token() {
        let t = parse_token_response(
            200,
            r#"{"access_token":"a","expires_in":3599,"refresh_token":"r","token_type":"Bearer"}"#,
        )
        .unwrap();
        assert_eq!(t.access_token, "a");
        assert_eq!(t.expires_in, 3599);
        assert_eq!(t.refresh_token.as_deref(), Some("r"));
        let t = parse_token_response(200, r#"{"access_token":"b","expires_in":10}"#).unwrap();
        assert_eq!(t.refresh_token, None);
    }

    #[test]
    fn token_response_invalid_grant_is_distinct() {
        let r = parse_token_response(
            400,
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
        );
        assert_eq!(r, Err(TokenError::InvalidGrant));
    }

    #[test]
    fn token_response_other_errors_carry_google_text() {
        let r = parse_token_response(
            400,
            r#"{"error":"invalid_client","error_description":"The OAuth client was not found."}"#,
        );
        assert_eq!(
            r,
            Err(TokenError::Other("The OAuth client was not found.".into()))
        );
        assert!(matches!(
            parse_token_response(502, "<html>bad gateway</html>"),
            Err(TokenError::Other(_))
        ));
        assert!(matches!(
            parse_token_response(500, r#"{"foo":1}"#),
            Err(TokenError::Other(_))
        ));
    }

    #[test]
    fn refresh_needed_when_missing_expired_or_near_expiry() {
        assert!(needs_refresh(None, 1_000));
        assert!(needs_refresh(Some(500), 1_000));
        assert!(needs_refresh(Some(1_000 + 60_000), 1_000));
        assert!(!needs_refresh(Some(1_000 + 60_001), 1_000));
    }

    #[test]
    fn day_bounds_in_sao_paulo() {
        assert_eq!(
            day_bounds("2026-10-21", &sp()).unwrap(),
            (
                "2026-10-21T00:00:00-03:00".to_string(),
                "2026-10-22T00:00:00-03:00".to_string()
            )
        );
    }

    #[test]
    fn day_bounds_follow_dst_changes_in_berlin() {
        // Clocks go back on 2026-10-25 (25 h day) and forward on 2026-03-29 (23 h day).
        assert_eq!(
            day_bounds("2026-10-25", &berlin()).unwrap(),
            (
                "2026-10-25T00:00:00+02:00".to_string(),
                "2026-10-26T00:00:00+01:00".to_string()
            )
        );
        assert_eq!(
            day_bounds("2026-03-29", &berlin()).unwrap(),
            (
                "2026-03-29T00:00:00+01:00".to_string(),
                "2026-03-30T00:00:00+02:00".to_string()
            )
        );
    }

    #[test]
    fn day_bounds_rejects_bad_dates() {
        assert!(day_bounds("2026-13-01", &sp()).is_err());
        assert!(day_bounds("nope", &sp()).is_err());
    }

    #[test]
    fn timed_events_become_minutes_and_keep_the_link() {
        let body = list(&timed(
            "e1",
            "Deep work",
            "2026-10-21T09:00:00-03:00",
            "2026-10-21T10:30:00-03:00",
        ));
        let r = parse_event_list(&body, "2026-10-21", &sp()).unwrap();
        assert_eq!(
            r.events,
            vec![CalEvent {
                id: "e1".into(),
                start: 540,
                end: 630,
                title: "Deep work".into(),
                calendar_id: String::new(),
                html_link: Some("https://l/e1".into()),
                project_id: None,
                color: String::new(),
            }]
        );
        assert_eq!(r.summary.as_deref(), Some("me@example.com"));
        assert_eq!(r.next_page_token, None);
    }

    #[test]
    fn offsets_of_other_zones_are_converted_to_the_local_zone() {
        // 12:00Z = 09:00 in Sao Paulo; +01:00 13:00 = 09:00 too.
        let body = list(&format!(
            "{},{}",
            timed("a", "A", "2026-10-21T12:00:00Z", "2026-10-21T13:00:00Z"),
            timed(
                "b",
                "B",
                "2026-10-21T13:00:00+01:00",
                "2026-10-21T14:30:00+01:00"
            ),
        ));
        let r = parse_event_list(&body, "2026-10-21", &sp()).unwrap();
        assert_eq!((r.events[0].start, r.events[0].end), (540, 600));
        assert_eq!((r.events[1].start, r.events[1].end), (540, 630));
    }

    #[test]
    fn berlin_events_use_wall_clock_on_the_dst_switch_day() {
        // 2026-10-25: 08:00 is +01:00 after the change.
        let body = list(&timed(
            "d",
            "Run",
            "2026-10-25T08:00:00+01:00",
            "2026-10-25T09:00:00+01:00",
        ));
        let r = parse_event_list(&body, "2026-10-25", &berlin()).unwrap();
        assert_eq!((r.events[0].start, r.events[0].end), (480, 540));
        // 00:30 is still +02:00, before the repeated hour.
        let body = list(&timed(
            "e",
            "Late",
            "2026-10-25T00:30:00+02:00",
            "2026-10-25T01:00:00+02:00",
        ));
        let r = parse_event_list(&body, "2026-10-25", &berlin()).unwrap();
        assert_eq!((r.events[0].start, r.events[0].end), (30, 60));
    }

    #[test]
    fn all_day_cancelled_and_empty_events_are_skipped() {
        let all_day = r#"{"id":"x","status":"confirmed","summary":"Holiday","start":{"date":"2026-10-21"},"end":{"date":"2026-10-22"}}"#;
        let cancelled = r#"{"id":"y","status":"cancelled"}"#;
        let zero = timed(
            "z",
            "Zero",
            "2026-10-21T09:00:00-03:00",
            "2026-10-21T09:00:00-03:00",
        );
        let negative = timed(
            "n",
            "Neg",
            "2026-10-21T10:00:00-03:00",
            "2026-10-21T09:00:00-03:00",
        );
        let body = list(&format!("{all_day},{cancelled},{zero},{negative}"));
        let r = parse_event_list(&body, "2026-10-21", &sp()).unwrap();
        assert!(r.events.is_empty());
    }

    #[test]
    fn events_crossing_midnight_are_clamped_to_the_day() {
        let body = list(&timed(
            "n",
            "Night",
            "2026-10-21T22:00:00-03:00",
            "2026-10-22T02:00:00-03:00",
        ));
        let first = parse_event_list(&body, "2026-10-21", &sp()).unwrap();
        assert_eq!((first.events[0].start, first.events[0].end), (1320, 1440));
        let second = parse_event_list(&body, "2026-10-22", &sp()).unwrap();
        assert_eq!((second.events[0].start, second.events[0].end), (0, 120));
        // Ending exactly at midnight does not leak into the next day.
        let body = list(&timed(
            "m",
            "Till midnight",
            "2026-10-21T23:00:00-03:00",
            "2026-10-22T00:00:00-03:00",
        ));
        let next = parse_event_list(&body, "2026-10-22", &sp()).unwrap();
        assert!(next.events.is_empty());
        let same = parse_event_list(&body, "2026-10-21", &sp()).unwrap();
        assert_eq!((same.events[0].start, same.events[0].end), (1380, 1440));
    }

    #[test]
    fn untitled_events_get_a_placeholder_and_titles_are_trimmed() {
        let a = r#"{"id":"u","summary":"  ","start":{"dateTime":"2026-10-21T09:00:00-03:00"},"end":{"dateTime":"2026-10-21T10:00:00-03:00"}}"#;
        let b = r#"{"id":"v","start":{"dateTime":"2026-10-21T10:00:00-03:00"},"end":{"dateTime":"2026-10-21T11:00:00-03:00"}}"#;
        let c = timed(
            "w",
            "  Study  ",
            "2026-10-21T11:00:00-03:00",
            "2026-10-21T12:00:00-03:00",
        );
        let r = parse_event_list(&list(&format!("{a},{b},{c}")), "2026-10-21", &sp()).unwrap();
        assert_eq!(r.events[0].title, "(no title)");
        assert_eq!(r.events[1].title, "(no title)");
        assert_eq!(r.events[2].title, "Study");
    }

    #[test]
    fn next_page_token_is_reported_and_bad_json_errors() {
        let r =
            parse_event_list(r#"{"items":[],"nextPageToken":"tok"}"#, "2026-10-21", &sp()).unwrap();
        assert_eq!(r.next_page_token.as_deref(), Some("tok"));
        assert!(parse_event_list("nope", "2026-10-21", &sp()).is_err());
    }

    #[test]
    fn single_event_response_is_parsed() {
        let body = timed(
            "c",
            "New",
            "2026-10-21T09:00:00-03:00",
            "2026-10-21T09:30:00-03:00",
        );
        let e = parse_event(&body, "2026-10-21", &sp()).unwrap();
        assert_eq!((e.id.as_str(), e.start, e.end), ("c", 540, 570));
        assert!(parse_event(&body, "2026-10-23", &sp()).is_err());
    }

    #[test]
    fn email_only_when_the_title_looks_like_one() {
        assert_eq!(
            account_email(Some(" me@example.com ")),
            Some("me@example.com".into())
        );
        assert_eq!(account_email(Some("Ismael's calendar")), None);
        assert_eq!(account_email(Some("My @ work")), None);
        assert_eq!(account_email(None), None);
    }

    #[test]
    fn event_body_uses_wall_clock_and_iana_zone() {
        let b = event_body("2026-10-21", 540, 630, "Deep work", &sp()).unwrap();
        assert_eq!(
            b,
            json!({
                "summary": "Deep work",
                "start": {"dateTime": "2026-10-21T09:00:00", "timeZone": "America/Sao_Paulo"},
                "end": {"dateTime": "2026-10-21T10:30:00", "timeZone": "America/Sao_Paulo"},
            })
        );
    }

    #[test]
    fn event_body_end_at_midnight_rolls_to_the_next_day() {
        let b = event_body("2026-10-21", 1380, 1440, "Late", &berlin()).unwrap();
        assert_eq!(b["end"]["dateTime"], "2026-10-22T00:00:00");
        assert_eq!(b["end"]["timeZone"], "Europe/Berlin");
    }

    #[test]
    fn event_body_without_iana_name_sends_an_offset() {
        let tz = TimeZone::fixed(jiff::tz::offset(-3));
        let b = event_body("2026-10-21", 540, 600, "X", &tz).unwrap();
        assert_eq!(b["start"]["dateTime"], "2026-10-21T09:00:00-03:00");
        assert!(b["start"].get("timeZone").is_none());
    }

    #[test]
    fn event_body_rejects_bad_ranges() {
        assert!(event_body("2026-10-21", 600, 600, "X", &sp()).is_err());
        assert!(event_body("2026-10-21", 600, 540, "X", &sp()).is_err());
        assert!(event_body("2026-10-21", 600, 1441, "X", &sp()).is_err());
    }

    #[test]
    fn cal_event_serializes_without_the_link() {
        let e = CalEvent {
            id: "i".into(),
            start: 1,
            end: 2,
            title: "t".into(),
            calendar_id: "cal@x".into(),
            html_link: Some("l".into()),
            project_id: Some(3),
            color: "#16a765".into(),
        };
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            json!({"id":"i","start":1,"end":2,"title":"t","projectId":3,"color":"#16a765"})
        );
    }

    #[test]
    fn list_scope_is_detected_in_a_scope_string() {
        assert!(has_list_scope(&format!(
            "openid {SCOPE} {CALENDAR_LIST_SCOPE}"
        )));
        assert!(!has_list_scope(SCOPE));
        assert!(!has_list_scope(""));
        assert!(!has_list_scope(
            "https://www.googleapis.com/auth/calendar.calendarlist"
        ));
    }

    #[test]
    fn token_response_carries_the_granted_scope() {
        let t = parse_token_response(
            200,
            &format!(r#"{{"access_token":"a","expires_in":10,"scope":"{SCOPE}"}}"#),
        )
        .unwrap();
        assert_eq!(t.scope.as_deref(), Some(SCOPE));
        let t = parse_token_response(200, r#"{"access_token":"a"}"#).unwrap();
        assert_eq!(t.scope, None);
    }

    #[test]
    fn calendar_list_prefers_the_users_name_and_drops_deleted() {
        let body = r##"{"items":[
            {"id":"me@x","summary":"me@x","summaryOverride":"  Personal ","backgroundColor":"#9fe1e7","primary":true,"selected":true},
            {"id":"mest@x","summary":"Mestrado","backgroundColor":"#16a765","selected":true},
            {"id":"gone@x","summary":"Gone","deleted":true},
            {"id":"bare@x"},
            {"summary":"no id"}
          ],"nextPageToken":"P2"}"##;
        let page = parse_calendar_list(body).unwrap();
        assert_eq!(page.next_page_token.as_deref(), Some("P2"));
        let got: Vec<_> = page
            .calendars
            .iter()
            .map(|c| {
                (
                    c.id.as_str(),
                    c.name.as_str(),
                    c.color.as_str(),
                    c.primary,
                    c.selected,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("me@x", "Personal", "#9fe1e7", true, true),
                ("mest@x", "Mestrado", "#16a765", false, true),
                ("bare@x", "bare@x", DEFAULT_CALENDAR_COLOR, false, false),
            ]
        );
        assert!(parse_calendar_list("nope").is_err());
        assert_eq!(parse_calendar_list("{}").unwrap().calendars.len(), 0);
    }
}
