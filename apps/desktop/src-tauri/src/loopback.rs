//! One-shot loopback listener for the browser sign-in handoff (RFC 8252).
//! The server redirects the browser to `http://127.0.0.1:{port}/callback?code=…`;
//! this module catches that request and returns its decoded query parameters
//! (`code`, `state`, `error`, …). Used by the M4 sign-in and the M5 Google OAuth.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Requests longer than this are not ours; we never read more.
const MAX_REQUEST: usize = 8 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(100);

const DONE_PAGE: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>Catenga Focus</title></head><body style=\"font-family:system-ui,sans-serif;padding:2rem\">\
<p>All set. You can close this tab and return to Catenga Focus.</p></body></html>";

#[derive(Debug)]
pub enum Error {
    Cancelled,
    TimedOut,
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Cancelled => write!(f, "cancelled"),
            Error::TimedOut => write!(f, "timed out"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// Binds a non-blocking listener on `127.0.0.1` with an OS-chosen port.
pub fn bind() -> io::Result<(TcpListener, u16)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    Ok((listener, port))
}

/// Decoded query of a `GET /callback?…` request line. A callback is ours when it
/// carries `code` or `error`; validating the values is up to the caller.
fn parse_params(request_line: &str) -> Option<HashMap<String, String>> {
    let mut parts = request_line.split(' ');
    if parts.next()? != "GET" {
        return None;
    }
    let query = parts.next()?.strip_prefix("/callback?")?;
    let params: HashMap<String, String> = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    (params.contains_key("code") || params.contains_key("error")).then_some(params)
}

/// Reads the first line of the request (up to `MAX_REQUEST` bytes) and answers it.
/// Returns the query parameters when the request was a callback.
fn handle(mut stream: TcpStream) -> io::Result<Option<HashMap<String, String>>> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while buf.len() < MAX_REQUEST && !buf.windows(2).any(|w| w == b"\r\n") {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8_lossy(&buf);
    let line = text.split("\r\n").next().unwrap_or("");
    let params = parse_params(line);
    let (status, body) = match params {
        Some(_) => ("200 OK", DONE_PAGE),
        None => ("404 Not Found", "Not found"),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(params)
}

/// Waits for the callback request and returns its parameters. Other requests (favicon, probes)
/// get a 404 and the wait continues. Polls `cancel` and `deadline` every ~100 ms.
pub fn wait_for_callback(
    listener: &TcpListener,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<HashMap<String, String>, Error> {
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(Error::TimedOut);
        }
        match listener.accept() {
            Ok((stream, _)) => {
                // A broken or non-matching connection must not end the wait.
                if let Ok(Some(params)) = handle(stream) {
                    return Ok(params);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(POLL),
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    /// Sends raw bytes to the listener and returns the full response.
    fn request(port: u16, raw: &[u8]) -> String {
        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        s.write_all(raw).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    fn get(port: u16, target: &str) -> String {
        request(
            port,
            format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes(),
        )
    }

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    #[test]
    fn parse_params_decodes_the_query() {
        let p = parse_params("GET /callback?code=abc-DEF_123&state=xyz HTTP/1.1").unwrap();
        assert_eq!(p["code"], "abc-DEF_123");
        assert_eq!(p["state"], "xyz");
    }

    #[test]
    fn parse_params_percent_decodes_google_codes() {
        let p = parse_params(
            "GET /callback?state=s&code=4%2F0AbC-d_e&scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcalendar.events HTTP/1.1",
        )
        .unwrap();
        assert_eq!(p["code"], "4/0AbC-d_e");
        assert_eq!(
            p["scope"],
            "https://www.googleapis.com/auth/calendar.events"
        );
    }

    #[test]
    fn parse_params_accepts_error_callback() {
        let p = parse_params("GET /callback?error=access_denied&state=s HTTP/1.1").unwrap();
        assert_eq!(p["error"], "access_denied");
        assert_eq!(p["state"], "s");
        assert!(!p.contains_key("code"));
    }

    #[test]
    fn parse_params_rejects_everything_else() {
        for line in [
            "GET /favicon.ico HTTP/1.1",
            "GET /callback HTTP/1.1",
            "GET /callback?state=s HTTP/1.1",
            "GET /callback?other=1 HTTP/1.1",
            "POST /callback?code=abc HTTP/1.1",
            "GET /x/callback?code=abc HTTP/1.1",
            "",
        ] {
            assert_eq!(parse_params(line), None, "{line}");
        }
    }

    #[test]
    fn callback_returns_params_and_answers_200() {
        let (listener, port) = bind().unwrap();
        let cancel = AtomicBool::new(false);
        let client = thread::spawn(move || get(port, "/callback?code=good_code-1&state=st"));
        let params = wait_for_callback(&listener, far(), &cancel).unwrap();
        assert_eq!(params["code"], "good_code-1");
        assert_eq!(params["state"], "st");
        let response = client.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("close this tab"));
    }

    #[test]
    fn error_callback_is_returned_as_params() {
        let (listener, port) = bind().unwrap();
        let cancel = AtomicBool::new(false);
        let client = thread::spawn(move || get(port, "/callback?error=access_denied&state=st"));
        let params = wait_for_callback(&listener, far(), &cancel).unwrap();
        assert_eq!(params["error"], "access_denied");
        client.join().unwrap();
    }

    #[test]
    fn other_paths_get_404_then_real_callback_is_accepted() {
        let (listener, port) = bind().unwrap();
        let cancel = AtomicBool::new(false);
        let client = thread::spawn(move || {
            let favicon = get(port, "/favicon.ico");
            let bare = get(port, "/callback");
            let good = get(port, "/callback?code=abc");
            (favicon, bare, good)
        });
        let params = wait_for_callback(&listener, far(), &cancel).unwrap();
        assert_eq!(params["code"], "abc");
        let (favicon, bare, good) = client.join().unwrap();
        assert!(favicon.starts_with("HTTP/1.1 404"));
        assert!(bare.starts_with("HTTP/1.1 404"));
        assert!(good.starts_with("HTTP/1.1 200"));
    }

    #[test]
    fn cancel_flag_ends_the_wait() {
        let (listener, _port) = bind().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let setter = thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            flag.store(true, Ordering::SeqCst);
        });
        let result = wait_for_callback(&listener, far(), &cancel);
        setter.join().unwrap();
        assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    }

    #[test]
    fn deadline_ends_the_wait() {
        let (listener, _port) = bind().unwrap();
        let cancel = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_millis(200);
        let result = wait_for_callback(&listener, deadline, &cancel);
        assert!(matches!(result, Err(Error::TimedOut)), "{result:?}");
    }

    #[test]
    fn oversized_request_does_not_hang_and_wait_continues() {
        let (listener, port) = bind().unwrap();
        let cancel = AtomicBool::new(false);
        let client = thread::spawn(move || {
            // 64 KiB with no line break: the server must stop reading at its cap.
            let junk = vec![b'a'; 64 * 1024];
            let first = request(port, &junk);
            let good = get(port, "/callback?code=after");
            (first, good)
        });
        let params = wait_for_callback(&listener, far(), &cancel).unwrap();
        assert_eq!(params["code"], "after");
        let (first, _) = client.join().unwrap();
        assert!(first.starts_with("HTTP/1.1 404"), "{first}");
    }

    #[test]
    fn silent_connection_does_not_block_forever() {
        let (listener, port) = bind().unwrap();
        let cancel = AtomicBool::new(false);
        // Connects and sends nothing; the read timeout releases the server.
        let silent = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        let client = thread::spawn(move || get(port, "/callback?code=late"));
        let params = wait_for_callback(&listener, far(), &cancel).unwrap();
        assert_eq!(params["code"], "late");
        client.join().unwrap();
        drop(silent);
    }
}
