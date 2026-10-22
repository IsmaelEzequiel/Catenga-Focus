//! The HTTP half of M9a sync: `POST /api/sync` with the Bearer app token, and the
//! loop that pushes what changed locally and pulls what changed elsewhere
//! (`sync::AppDb` does the rows; this only moves them).

use crate::app_db::AppDb;
use crate::file_sync::{ChangesPage, Put, Remote};
use crate::sync::{Applied, BATCH, Item, KeyRef};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Safety net against a server that never settles (25,000 rows per sync).
const MAX_ROUNDS: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncError {
    /// 401: the token is no longer valid. Nothing is deleted.
    Unauthorized,
    /// The server could not be reached (offline, timeout): retry later.
    Offline(String),
    /// The server answered with a failure worth retrying (5xx, 429).
    Server(String),
    /// The server refused the request itself (400/413): retrying the same data won't help.
    Rejected(String),
    /// The server has no notes storage configured (503): notes sync is off, the rest works.
    Unavailable(String),
    /// The local database failed.
    Local(String),
}

impl SyncError {
    /// Worth trying again after a pause.
    pub fn retryable(&self) -> bool {
        matches!(self, SyncError::Offline(_) | SyncError::Server(_))
    }
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Unauthorized => f.write_str("Sign in again"),
            SyncError::Offline(m)
            | SyncError::Server(m)
            | SyncError::Rejected(m)
            | SyncError::Unavailable(m) => f.write_str(m),
            SyncError::Local(m) => f.write_str(m),
        }
    }
}

#[derive(Serialize)]
struct Request<'a> {
    cursor: i64,
    changes: &'a [Item],
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub cursor: i64,
    pub changes: Vec<Item>,
    pub has_more: bool,
    #[serde(default)]
    pub accepted: Vec<KeyRef>,
}

pub struct SyncClient {
    http: reqwest::Client,
}

fn net_err(e: reqwest::Error) -> SyncError {
    SyncError::Offline(format!("couldn't reach the server: {}", e.without_url()))
}

/// The server's `{error}` text, else the HTTP status.
fn failure(status: u16, body: &str) -> SyncError {
    let text = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| format!("server error ({status})"));
    match status {
        401 => SyncError::Unauthorized,
        503 if text.contains("isn't configured") => SyncError::Unavailable(text),
        400 | 413 => SyncError::Rejected(text),
        _ => SyncError::Server(text),
    }
}

impl SyncClient {
    pub fn new() -> Result<SyncClient, String> {
        reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map(|http| SyncClient { http })
            .map_err(|e| format!("creating HTTP client: {e}"))
    }

    /// Who the token belongs to: the server's `id` if it sends one, else the email.
    /// Used to notice a different account than the one the local data was synced with.
    pub async fn identity(&self, base: &str, token: &str) -> Result<String, SyncError> {
        let response = self
            .http
            .get(format!("{base}/api/me"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(net_err)?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(net_err)?;
        if status != 200 {
            return Err(failure(status, &text));
        }
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| SyncError::Server(format!("unexpected answer: {e}")))?;
        v["id"]
            .as_str()
            .map(str::to_string)
            .or_else(|| v["email"].as_str().map(str::to_lowercase))
            .ok_or_else(|| SyncError::Server("unexpected answer: no account".into()))
    }

    pub async fn exchange(
        &self,
        base: &str,
        token: &str,
        cursor: i64,
        changes: &[Item],
    ) -> Result<Response, SyncError> {
        let response = self
            .http
            .post(format!("{base}/api/sync"))
            .bearer_auth(token)
            .json(&Request { cursor, changes })
            .send()
            .await
            .map_err(net_err)?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(net_err)?;
        if status != 200 {
            return Err(failure(status, &text));
        }
        serde_json::from_str(&text)
            .map_err(|e| SyncError::Server(format!("unexpected answer: {e}")))
    }
}

/// The notes endpoints (§1.3) with the Bearer app token.
pub struct HttpRemote<'a> {
    pub client: &'a SyncClient,
    pub base: &'a str,
    pub token: &'a str,
}

impl HttpRemote<'_> {
    fn url(&self, route: &str, query: &[(&str, &str)]) -> Result<url::Url, SyncError> {
        let full = format!("{}{route}", self.base);
        let parsed = if query.is_empty() {
            url::Url::parse(&full)
        } else {
            url::Url::parse_with_params(&full, query)
        };
        parsed
            .map(|mut u| {
                // Spaces as %20 (a literal `+` is already %2B), for any query decoder.
                let q = u.query().map(|q| q.replace('+', "%20"));
                u.set_query(q.as_deref());
                u
            })
            .map_err(|e| SyncError::Local(format!("bad server address: {e}")))
    }

    async fn call(&self, req: reqwest::RequestBuilder) -> Result<(u16, Vec<u8>), SyncError> {
        let response = req.bearer_auth(self.token).send().await.map_err(net_err)?;
        let status = response.status().as_u16();
        let body = response.bytes().await.map_err(net_err)?;
        Ok((status, body.to_vec()))
    }

    fn text(body: &[u8]) -> String {
        String::from_utf8_lossy(body).into_owned()
    }

    fn json<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, SyncError> {
        serde_json::from_slice(body)
            .map_err(|e| SyncError::Server(format!("unexpected answer: {e}")))
    }

    /// 200 → `Done`, 409 → the server's current rev.
    fn put_result(status: u16, body: &[u8]) -> Result<Put, SyncError> {
        match status {
            200 => Ok(Put::Done(
                serde_json::from_slice::<Value>(body)
                    .ok()
                    .and_then(|v| v["seq"].as_i64())
                    .unwrap_or(0),
            )),
            409 => Self::json::<Value>(body)?["rev"]
                .as_str()
                .map(|r| Put::Conflict(r.to_string()))
                .ok_or_else(|| SyncError::Server("unexpected answer: no rev".into())),
            _ => Err(failure(status, &Self::text(body))),
        }
    }
}

impl Remote for HttpRemote<'_> {
    async fn key(&self) -> Result<String, SyncError> {
        let url = self.url("/api/notes/key", &[])?;
        let (status, body) = self.call(self.client.http.get(url)).await?;
        if status != 200 {
            return Err(failure(status, &Self::text(&body)));
        }
        Self::json::<Value>(&body)?["key"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| SyncError::Server("unexpected answer: no key".into()))
    }

    async fn changes(&self, cursor: i64) -> Result<ChangesPage, SyncError> {
        let url = self.url("/api/files/changes", &[("cursor", &cursor.to_string())])?;
        let (status, body) = self.call(self.client.http.get(url)).await?;
        if status != 200 {
            return Err(failure(status, &Self::text(&body)));
        }
        Self::json(&body)
    }

    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, SyncError> {
        let url = self.url("/api/files/content", &[("path", path)])?;
        let (status, body) = self.call(self.client.http.get(url)).await?;
        match status {
            200 => Ok(Some(body)),
            404 => Ok(None),
            _ => Err(failure(status, &Self::text(&body))),
        }
    }

    async fn put(
        &self,
        path: &str,
        base_rev: &str,
        rev: &str,
        blob: Vec<u8>,
    ) -> Result<Put, SyncError> {
        let url = self.url("/api/files/content", &[("path", path)])?;
        let req = self
            .client
            .http
            .put(url)
            .header("X-Base-Rev", base_rev)
            .header("X-Rev", rev)
            .header("Content-Type", "application/octet-stream")
            .body(blob);
        let (status, body) = self.call(req).await?;
        Self::put_result(status, &body)
    }

    async fn delete(&self, path: &str, base_rev: &str) -> Result<Put, SyncError> {
        let url = self.url("/api/files/content", &[("path", path)])?;
        let req = self.client.http.delete(url).header("X-Base-Rev", base_rev);
        let (status, body) = self.call(req).await?;
        Self::put_result(status, &body)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Outcome {
    pub applied: Applied,
    pub pushed: usize,
}

/// One full sync for `user`: push what changed locally, pull what changed
/// elsewhere, repeat while the server has more. Rows stay dirty on any failure.
pub async fn sync_once(
    db: &AppDb,
    client: &SyncClient,
    base: &str,
    token: &str,
    user: &str,
) -> Result<Outcome, SyncError> {
    let local = SyncError::Local;
    let mut cursor = db.sync_begin(user).map_err(local)?;
    let mut out = Outcome::default();
    for _ in 0..MAX_ROUNDS {
        let pushed = db.sync_collect().map_err(SyncError::Local)?;
        let response = client.exchange(base, token, cursor, &pushed).await?;
        db.sync_ack(&pushed, &response.accepted)
            .map_err(SyncError::Local)?;
        out.pushed += response.accepted.len();
        out.applied
            .merge(db.sync_apply(response.changes).map_err(SyncError::Local)?);
        cursor = response.cursor;
        db.sync_save_cursor(user, cursor)
            .map_err(SyncError::Local)?;
        if !response.has_more && pushed.len() < BATCH {
            break;
        }
    }
    if db.resolve_pending_projects().map_err(SyncError::Local)? {
        out.applied.goals = true;
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod test_util {
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// What the fake server saw: `(request line, raw headers+body)`.
    pub type Seen = Arc<Mutex<Vec<(String, String)>>>;

    /// A tiny HTTP server: `handler(request_line, headers, body) -> (status, json body)`,
    /// one connection at a time, until the test process ends.
    pub fn serve(
        mut handler: impl FnMut(&str, &str, &str) -> (u16, String) + Send + 'static,
    ) -> (String, Seen) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            while let Ok((mut s, _)) = listener.accept() {
                let mut raw = Vec::new();
                let mut chunk = [0u8; 8192];
                let (head, body) = loop {
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
                            break (text[..i].to_string(), text[i + 4..].to_string());
                        }
                    } else if n == 0 {
                        break (text, String::new());
                    }
                };
                let line = head.lines().next().unwrap_or("").to_string();
                log.lock()
                    .unwrap()
                    .push((line.clone(), format!("{head}\n\n{body}")));
                let (status, reply) = handler(&line, &head, &body);
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = s.write_all(resp.as_bytes());
            }
        });
        (base, seen)
    }

    struct Row {
        data: Value,
        changed_at: i64,
        deleted: bool,
        seq: i64,
    }

    /// The server's side of §2.2 for one account: last-write-wins (ties keep the
    /// server row), a per-account `seq`, paging by `page`.
    #[derive(Default)]
    pub struct Store {
        rows: BTreeMap<(String, String), Row>,
        counter: i64,
        pub page: usize,
    }

    impl Store {
        pub fn new(page: usize) -> Store {
            Store {
                page,
                ..Store::default()
            }
        }

        pub fn put(&mut self, kind: &str, key: &str, data: Value, changed_at: i64, deleted: bool) {
            self.counter += 1;
            self.rows.insert(
                (kind.into(), key.into()),
                Row {
                    data,
                    changed_at,
                    deleted,
                    seq: self.counter,
                },
            );
        }

        pub fn len(&self) -> usize {
            self.rows.len()
        }

        pub fn handle(&mut self, body: &str) -> (u16, String) {
            let req: Value = serde_json::from_str(body).unwrap();
            let cursor = req["cursor"].as_i64().unwrap();
            let mut accepted = Vec::new();
            for c in req["changes"].as_array().unwrap() {
                let (kind, key) = (c["kind"].as_str().unwrap(), c["key"].as_str().unwrap());
                let at = c["changedAt"].as_i64().unwrap();
                let newer = self
                    .rows
                    .get(&(kind.to_string(), key.to_string()))
                    .is_none_or(|r| at > r.changed_at);
                if newer {
                    let deleted = c["deleted"].as_bool().unwrap();
                    assert_eq!(
                        c["data"].is_null(),
                        deleted,
                        "data is null exactly for tombstones"
                    );
                    self.put(kind, key, c["data"].clone(), at, deleted);
                    accepted.push(json!({"kind": kind, "key": key}));
                }
            }
            let mut newer: Vec<_> = self.rows.iter().filter(|(_, r)| r.seq > cursor).collect();
            newer.sort_by_key(|(_, r)| r.seq);
            let has_more = newer.len() > self.page;
            newer.truncate(self.page);
            let last = newer.last().map_or(cursor, |(_, r)| r.seq);
            let changes: Vec<Value> = newer
                .iter()
                .map(|((k, key), r)| {
                    json!({"kind": k, "key": key, "data": r.data, "changedAt": r.changed_at,
                           "deleted": r.deleted})
                })
                .collect();
            (
                200,
                json!({"cursor": last, "changes": changes, "hasMore": has_more,
                       "accepted": accepted})
                .to_string(),
            )
        }
    }

    /// A server around a shared `Store`; any request without `Bearer tok` gets 401.
    pub fn store_server(store: Arc<Mutex<Store>>) -> (String, Seen) {
        serve(move |line, head, body| {
            if !head.contains("authorization: Bearer tok")
                && !head.contains("Authorization: Bearer tok")
            {
                return (401, r#"{"error":"Not signed in"}"#.into());
            }
            if line.starts_with("GET /api/me") {
                return (200, r#"{"name":"Ana","email":"Ana@Example.com"}"#.into());
            }
            store.lock().unwrap().handle(body)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::*;
    use super::*;
    use crate::sync::tests::{clock, dev, draft};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn run<F: std::future::Future>(f: F) -> F::Output {
        tauri::async_runtime::block_on(f)
    }

    fn sync(db: &AppDb, base: &str, user: &str) -> Result<Outcome, SyncError> {
        run(async {
            let client = SyncClient::new().unwrap();
            sync_once(db, &client, base, "tok", user).await
        })
    }

    #[test]
    fn pushing_sends_the_contract_and_clears_dirty_for_accepted_rows() {
        let store = Arc::new(Mutex::new(Store::new(500)));
        let (base, seen) = store_server(store.clone());
        let d = dev(&clock());
        d.db.goal_create(&draft("Rust", &["rust"], None), 1)
            .unwrap();
        assert_eq!(d.db.sync_pending().unwrap(), 2);
        let out = sync(&d.db, &base, "u1").unwrap();
        assert_eq!(out.pushed, 2);
        assert_eq!(d.db.sync_pending().unwrap(), 0);
        assert!(
            !out.applied.any(),
            "the echo of our own rows changes nothing"
        );
        assert_eq!(store.lock().unwrap().len(), 2);
        let (line, raw) = seen.lock().unwrap()[0].clone();
        assert_eq!(line, "POST /api/sync HTTP/1.1");
        assert!(raw.to_lowercase().contains("authorization: bearer tok"));
        let body: Value = serde_json::from_str(raw.split("\n\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["cursor"], 0);
        let tag = &body["changes"][0];
        assert_eq!(tag["kind"], "tag");
        assert_eq!(tag["key"], "rust");
        assert_eq!(tag["data"], json!({"name": "rust", "color": "sun"}));
        assert_eq!(tag["deleted"], false);
        assert!(tag["changedAt"].as_i64().unwrap() > 0);
        assert_eq!(body["changes"][1]["kind"], "goal");
        // The cursor was saved: the next sync starts from it and sends nothing.
        assert!(sync(&d.db, &base, "u1").unwrap().pushed == 0);
        let (_, raw) = seen.lock().unwrap()[1].clone();
        let body: Value = serde_json::from_str(raw.split("\n\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["cursor"], 2);
        assert_eq!(body["changes"], json!([]));
    }

    #[test]
    fn pull_applies_newer_server_rows_and_keeps_newer_local_ones() {
        let store = Arc::new(Mutex::new(Store::new(500)));
        let d = dev(&clock());
        d.db.goal_create(&draft("Local", &[], None), 1).unwrap();
        let g = d.db.sync_collect().unwrap().remove(0);
        {
            let mut s = store.lock().unwrap();
            // Newer on the server for this goal; a tag; an OLD calendar choice we ignore.
            s.put("goal", &g.key, json!({"projectKey":null,"tagNames":[],"title":"Server","minutesPerDay":30,"start":"2026-10-01","days":null,"archived":false}), g.changed_at + 5, false);
            s.put("tag", "go", json!({"name":"Go","color":"sky"}), 7, false);
        }
        let (base, _) = store_server(store);
        let out = sync(&d.db, &base, "u1").unwrap();
        assert!(out.applied.goals && out.applied.tags);
        assert_eq!(
            d.db.goal_views(false).unwrap()[0].goal.title.as_deref(),
            Some("Server")
        );
        assert_eq!(d.db.saved_tags().unwrap()[0].name, "Go");
        assert_eq!(d.db.sync_pending().unwrap(), 0);

        // Local newer than the server: pushed and accepted, server updated.
        let id = d.db.goal_views(false).unwrap()[0].goal.id;
        d.db.goal_edit(id, &draft("Newest", &[], None), 2).unwrap();
        let out = sync(&d.db, &base, "u1").unwrap();
        assert_eq!(out.pushed, 1);
    }

    #[test]
    fn paging_follows_has_more_until_everything_is_pulled() {
        let store = Arc::new(Mutex::new(Store::new(2)));
        {
            let mut s = store.lock().unwrap();
            for i in 0..5 {
                s.put(
                    "tag",
                    &format!("t{i}"),
                    json!({"name": format!("t{i}"), "color": "sun"}),
                    10 + i,
                    false,
                );
            }
        }
        let (base, seen) = store_server(store);
        let d = dev(&clock());
        sync(&d.db, &base, "u1").unwrap();
        assert_eq!(d.db.saved_tags().unwrap().len(), 5);
        assert_eq!(seen.lock().unwrap().len(), 3, "2 + 2 + 1");
        assert_eq!(d.db.sync_begin("u1").unwrap(), 5);
    }

    #[test]
    fn offline_keeps_the_rows_dirty_and_a_later_sync_succeeds() {
        let d = dev(&clock());
        d.db.tag_save("a", "sun", 1).unwrap();
        // Nothing listens here.
        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://127.0.0.1:{}", l.local_addr().unwrap().port())
        };
        let err = sync(&d.db, &closed, "u1").unwrap_err();
        assert!(matches!(err, SyncError::Offline(_)), "{err:?}");
        assert!(err.retryable());
        assert_eq!(d.db.sync_pending().unwrap(), 1);
        let (base, _) = store_server(Arc::new(Mutex::new(Store::new(500))));
        sync(&d.db, &base, "u1").unwrap();
        assert_eq!(d.db.sync_pending().unwrap(), 0);
    }

    #[test]
    fn errors_map_to_states_and_never_lose_data() {
        let d = dev(&clock());
        d.db.tag_save("a", "sun", 1).unwrap();
        for (status, body, want) in [
            (401, r#"{"error":"Not signed in"}"#, SyncError::Unauthorized),
            (
                400,
                r#"{"error":"bad kind"}"#,
                SyncError::Rejected("bad kind".into()),
            ),
            (
                429,
                r#"{"error":"Too many sync requests"}"#,
                SyncError::Server("Too many sync requests".into()),
            ),
            (500, "oops", SyncError::Server("server error (500)".into())),
        ] {
            let (base, _) = serve(move |_, _, _| (status, body.to_string()));
            let err = sync(&d.db, &base, "u1").unwrap_err();
            assert_eq!(err, want);
            assert_eq!(d.db.sync_pending().unwrap(), 1);
            assert_eq!(d.db.saved_tags().unwrap().len(), 1);
        }
        assert!(!SyncError::Unauthorized.retryable());
        assert!(!SyncError::Rejected(String::new()).retryable());
        assert_eq!(SyncError::Unauthorized.to_string(), "Sign in again");
    }

    #[test]
    fn identity_prefers_an_id_and_falls_back_to_the_lowercased_email() {
        let (base, _) = store_server(Arc::new(Mutex::new(Store::new(1))));
        let c = SyncClient::new().unwrap();
        assert_eq!(run(c.identity(&base, "tok")).unwrap(), "ana@example.com");
        assert_eq!(
            run(c.identity(&base, "wrong")).unwrap_err(),
            SyncError::Unauthorized
        );
        let (base, _) = serve(|_, _, _| (200, r#"{"id":"u-9","email":"a@b.c"}"#.into()));
        assert_eq!(run(c.identity(&base, "tok")).unwrap(), "u-9");
    }

    #[test]
    fn two_computers_converge_through_the_server_including_deletes() {
        let c = clock();
        let (a, b) = (dev(&c), dev(&c));
        let (base, _) = store_server(Arc::new(Mutex::new(Store::new(500))));
        b.db.goal_create(&draft("B local", &[], None), 1).unwrap(); // makes B's ids differ
        a.db.goal_create(&draft("Learn Rust", &["rust"], None), 1)
            .unwrap();
        a.db.tag_save("rust", "leaf", 1).unwrap();
        sync(&a.db, &base, "u1").unwrap();
        sync(&b.db, &base, "u1").unwrap(); // B has local data: pushed too
        sync(&a.db, &base, "u1").unwrap();
        let titles = |d: &crate::sync::tests::Dev| {
            let mut t: Vec<_> =
                d.db.goal_views(false)
                    .unwrap()
                    .into_iter()
                    .filter_map(|v| v.goal.title)
                    .collect();
            t.sort();
            t
        };
        assert_eq!(titles(&a), ["B local", "Learn Rust"]);
        assert_eq!(titles(&b), ["B local", "Learn Rust"]);
        assert_eq!(b.db.saved_tags().unwrap()[0].color, "leaf");
        // Edit on B, delete on A.
        let b_goal =
            b.db.goal_views(false)
                .unwrap()
                .into_iter()
                .find(|v| v.goal.title.as_deref() == Some("Learn Rust"))
                .unwrap()
                .goal
                .id;
        b.db.goal_edit(b_goal, &draft("Learn Rust 2", &["rust"], None), 2)
            .unwrap();
        let a_local =
            a.db.goal_views(false)
                .unwrap()
                .into_iter()
                .find(|v| v.goal.title.as_deref() == Some("B local"))
                .unwrap()
                .goal
                .id;
        a.db.goal_delete(a_local).unwrap();
        sync(&b.db, &base, "u1").unwrap();
        sync(&a.db, &base, "u1").unwrap();
        sync(&b.db, &base, "u1").unwrap();
        assert_eq!(titles(&a), ["Learn Rust 2"]);
        assert_eq!(titles(&b), ["Learn Rust 2"]);
        assert_eq!(
            a.db.sync_pending().unwrap() + b.db.sync_pending().unwrap(),
            0
        );
    }

    #[test]
    fn signing_into_another_account_pushes_the_local_data_there() {
        let d = dev(&clock());
        d.db.goal_create(&draft("Mine", &["t"], None), 1).unwrap();
        let first = Arc::new(Mutex::new(Store::new(500)));
        let (base1, _) = store_server(first);
        sync(&d.db, &base1, "u1").unwrap();
        let second = Arc::new(Mutex::new(Store::new(500)));
        let (base2, _) = store_server(second.clone());
        sync(&d.db, &base2, "u2").unwrap();
        assert_eq!(
            second.lock().unwrap().len(),
            2,
            "tag + goal arrived in the new account"
        );
        assert_eq!(
            d.db.goal_views(false).unwrap().len(),
            1,
            "nothing was duplicated or lost"
        );
    }

    fn remote<'a>(c: &'a SyncClient, base: &'a str) -> HttpRemote<'a> {
        HttpRemote {
            client: c,
            base,
            token: "tok",
        }
    }

    #[test]
    fn notes_requests_follow_the_contract() {
        let (base, seen) = serve(|line, _, _| {
            match line.split(' ').nth(1).unwrap_or("") {
            p if p.starts_with("/api/notes/key") => (200, r#"{"key":"AAEC"}"#.into()),
            p if p.starts_with("/api/files/changes") => (
                200,
                r#"{"cursor":7,"hasMore":true,"changes":[{"path":"config.json","rev":"ab","size":3,"deleted":false,"seq":7}]}"#.into(),
            ),
            p if line.starts_with("GET") && p.contains("missing") => {
                (404, r#"{"error":"Not found"}"#.into())
            }
            _ if line.starts_with("GET") => (200, "CTG1blob".into()),
            _ if line.starts_with("PUT") => (200, r#"{"seq":9}"#.into()),
            _ => (200, "{}".into()),
        }
        });
        let c = SyncClient::new().unwrap();
        let r = remote(&c, &base);
        assert_eq!(run(r.key()).unwrap(), "AAEC");
        let page = run(r.changes(5)).unwrap();
        assert_eq!((page.cursor, page.has_more), (7, true));
        assert_eq!(page.changes[0].path, "config.json");
        assert_eq!(
            run(r.get("2026/10/2026-10-20.md")).unwrap().unwrap(),
            b"CTG1blob"
        );
        assert_eq!(run(r.get("missing.md")).unwrap(), None);
        assert_eq!(
            run(r.put("2026/10/2026-10-20.md", "base1", "rev2", b"CTG1x".to_vec())).unwrap(),
            Put::Done(9)
        );
        assert_eq!(run(r.delete("config.json", "base3")).unwrap(), Put::Done(0));
        let log = seen.lock().unwrap().clone();
        assert_eq!(log[0].0, "GET /api/notes/key HTTP/1.1");
        assert_eq!(log[1].0, "GET /api/files/changes?cursor=5 HTTP/1.1");
        assert_eq!(
            log[2].0,
            "GET /api/files/content?path=2026%2F10%2F2026-10-20.md HTTP/1.1"
        );
        let put = log[4].1.to_lowercase();
        assert_eq!(
            log[4].0,
            "PUT /api/files/content?path=2026%2F10%2F2026-10-20.md HTTP/1.1"
        );
        assert!(put.contains("x-base-rev: base1"), "{put}");
        assert!(put.contains("x-rev: rev2"), "{put}");
        assert!(put.contains("authorization: bearer tok"), "{put}");
        assert!(put.ends_with("ctg1x"), "{put}");
        assert!(log[5].1.to_lowercase().contains("x-base-rev: base3"));
    }

    #[test]
    fn notes_409_carries_the_servers_rev_and_errors_map_to_states() {
        let (base, _) = serve(|_, _, _| (409, r#"{"rev":"cur"}"#.into()));
        let c = SyncClient::new().unwrap();
        let r = remote(&c, &base);
        assert_eq!(
            run(r.put("config.json", "a", "b", vec![1])).unwrap(),
            Put::Conflict("cur".into())
        );
        assert_eq!(
            run(r.delete("config.json", "a")).unwrap(),
            Put::Conflict("cur".into())
        );
        let (base, _) = serve(|_, _, _| {
            (
                503,
                r#"{"error":"Notes sync isn't configured on this server"}"#.into(),
            )
        });
        let err = run(remote(&c, &base).key()).unwrap_err();
        assert_eq!(
            err,
            SyncError::Unavailable("Notes sync isn't configured on this server".into())
        );
        assert!(!err.retryable());
        let (base, _) = serve(|_, _, _| (503, "down".into()));
        assert!(matches!(
            run(remote(&c, &base).key()),
            Err(SyncError::Server(_))
        ));
        let (base, _) = serve(|_, _, _| (401, r#"{"error":"Not signed in"}"#.into()));
        assert_eq!(
            run(remote(&c, &base).changes(0)).unwrap_err(),
            SyncError::Unauthorized
        );
        let (base, _) = serve(|_, _, _| (413, r#"{"error":"too big"}"#.into()));
        assert_eq!(
            run(remote(&c, &base).put("config.json", "", "r", vec![])).unwrap_err(),
            SyncError::Rejected("too big".into())
        );
    }

    #[test]
    fn conflict_copy_paths_are_url_encoded() {
        let (base, seen) = serve(|_, _, _| (200, "{}".into()));
        let c = SyncClient::new().unwrap();
        let r = remote(&c, &base);
        run(r.delete("2026/10/2026-10-20 (conflict 2026-10-22 0930) 2.md", "b")).unwrap();
        let line = seen.lock().unwrap()[0].0.clone();
        assert_eq!(
            line.split(' ').count(),
            3,
            "no raw space in the target: {line}"
        );
        assert!(
            line.starts_with("DELETE /api/files/content?path=2026%2F10%2F2026-10-20"),
            "{line}"
        );
        assert!(
            line.contains("%28conflict") || line.contains("(conflict"),
            "{line}"
        );
    }
}
