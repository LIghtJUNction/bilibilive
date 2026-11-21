//! Loopback control and MCP transport. One bounded request per TCP connection.
use crate::{app::App, mcp::Session};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_BODY: usize = 1024 * 1024;
type Sessions = Arc<Mutex<BTreeMap<String, (Instant, Arc<Mutex<Session>>)>>>;

#[derive(Debug)]
struct Request {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn read_request(stream: &TcpStream) -> Result<Request, String> {
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    reader
        .by_ref()
        .take(8193)
        .read_line(&mut first)
        .map_err(|e| e.to_string())?;
    if first.len() > 8192 || !first.ends_with("\r\n") {
        return Err("Invalid HTTP request line".into());
    }
    let parts: Vec<_> = first.trim_end().split(' ').collect();
    if parts.len() != 3 || parts[2] != "HTTP/1.1" || !parts[1].starts_with('/') {
        return Err("Expected HTTP/1.1 origin-form request".into());
    }
    let method = parts[0].to_owned();
    let path = parts[1].to_owned();
    let mut headers = BTreeMap::new();
    let mut total = first.len();
    loop {
        let mut line = String::new();
        reader
            .by_ref()
            .take(8193)
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        total += line.len();
        if total > 32768 || line.len() > 8192 || !line.ends_with("\r\n") {
            return Err("Invalid or oversized HTTP headers".into());
        }
        if line == "\r\n" {
            break;
        }
        let (name, value) = line
            .trim_end()
            .split_once(':')
            .ok_or("Invalid HTTP header")?;
        let name = name.to_ascii_lowercase();
        if headers.insert(name, value.trim().to_owned()).is_some() {
            return Err("Duplicate HTTP header".into());
        }
    }
    if headers.contains_key("transfer-encoding") {
        return Err("Transfer-Encoding is not accepted; use Content-Length".into());
    }
    let length = headers
        .get("content-length")
        .map(|s| s.parse::<usize>().map_err(|_| "Invalid Content-Length"))
        .transpose()?
        .unwrap_or(0);
    if length > MAX_BODY {
        return Err("HTTP body exceeds 1 MiB".into());
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).map_err(|e| e.to_string())?;
    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    body: &[u8],
    session: Option<&str>,
) -> Result<(), String> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        429 => "Too Many Requests",
        _ => "Internal Server Error",
    };
    write!(stream,"HTTP/1.1 {status} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n",body.len()).map_err(|e|e.to_string())?;
    if let Some(session) = session {
        write!(stream, "Mcp-Session-Id: {session}\r\n").map_err(|e| e.to_string())?;
    }
    stream
        .write_all(b"\r\n")
        .and_then(|_| stream.write_all(body))
        .and_then(|_| stream.flush())
        .map_err(|e| e.to_string())
}

fn random_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "OS random source unavailable")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn equal_token(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.as_bytes()
        .iter()
        .zip(b.as_bytes())
        .fold(0u8, |d, (a, b)| d | (a ^ b))
        == 0
}

fn handle(
    mut stream: TcpStream,
    app: App,
    sessions: Sessions,
    token: &str,
    compat: bool,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    let request = match read_request(&stream) {
        Ok(request) => request,
        Err(e) => {
            return respond(
                &mut stream,
                400,
                json!({"error":e}).to_string().as_bytes(),
                None,
            );
        }
    };
    let local = stream.local_addr().map_err(|e| e.to_string())?;
    let hosts = [local.to_string(), format!("localhost:{}", local.port())];
    if !request
        .headers
        .get("host")
        .is_some_and(|h| hosts.contains(h))
    {
        return respond(&mut stream, 403, br#"{"error":"Invalid Host"}"#, None);
    }
    if request
        .headers
        .get("origin")
        .is_some_and(|origin| !hosts.iter().any(|h| origin == &format!("http://{h}")))
    {
        return respond(
            &mut stream,
            403,
            br#"{"error":"Cross-origin requests are disabled"}"#,
            None,
        );
    }
    if request.method == "GET" && request.path == "/health" {
        return respond(
            &mut stream,
            200,
            json!({"status":"ok","version":crate::VERSION})
                .to_string()
                .as_bytes(),
            None,
        );
    }
    let legacy = compat && ["/api/startLive", "/api/stopLive"].contains(&request.path.as_str());
    if !legacy
        && !request
            .headers
            .get("authorization")
            .and_then(|s| s.strip_prefix("Bearer "))
            .is_some_and(|s| equal_token(s, token))
    {
        return respond(
            &mut stream,
            401,
            br#"{"error":"Bearer token required"}"#,
            None,
        );
    }
    if request.path == "/mcp" && request.method == "DELETE" {
        if let Some(id) = request.headers.get("mcp-session-id") {
            sessions
                .lock()
                .map_err(|_| "Session lock failed")?
                .remove(id);
        }
        return respond(&mut stream, 204, &[], None);
    }
    if request.method != "POST" {
        return respond(&mut stream, 405, br#"{"error":"Use POST"}"#, None);
    }
    if request.path == "/mcp" {
        let mut pool = sessions.lock().map_err(|_| "Session lock failed")?;
        pool.retain(|_, (time, _)| time.elapsed() < Duration::from_secs(1800));
        let (id, session) = if let Some(id) = request.headers.get("mcp-session-id") {
            let Some((time, session)) = pool.get_mut(id) else {
                return respond(
                    &mut stream,
                    404,
                    br#"{"error":"MCP session expired"}"#,
                    None,
                );
            };
            *time = Instant::now();
            (id.clone(), session.clone())
        } else {
            if pool.len() >= 64 {
                return respond(
                    &mut stream,
                    429,
                    br#"{"error":"Too many MCP sessions"}"#,
                    None,
                );
            }
            let id = random_token()?;
            let session = Arc::new(Mutex::new(Session::new(app.clone())));
            pool.insert(id.clone(), (Instant::now(), session.clone()));
            (id, session)
        };
        drop(pool);
        let mut session = session.lock().map_err(|_| "MCP session lock failed")?;
        let reply = session.handle(&request.body)?;
        let delivered = match reply {
            Some(body) => respond(&mut stream, 200, &body, Some(&id)),
            None => respond(&mut stream, 202, &[], Some(&id)),
        };
        if delivered.is_ok() {
            session.complete_reply();
        }
        return delivered;
    }
    let mut applying_update = false;
    let result = match request.path.as_str() {
        "/api/startLive" => app.call("live.start", json!({})),
        "/api/stopLive" => app.call("live.stop", json!({})),
        "/api/call" => match serde_json::from_slice::<Value>(&request.body) {
            Ok(body) => match body["method"].as_str() {
                Some(method) => {
                    applying_update = matches!(method, "updates.apply" | "zuse.run");
                    app.call(
                        method,
                        body.get("arguments").cloned().unwrap_or_else(|| json!({})),
                    )
                }
                None => Err("method is required".into()),
            },
            Err(e) => Err(format!("Invalid JSON: {e}")),
        },
        _ => return respond(&mut stream, 404, br#"{"error":"Unknown endpoint"}"#, None),
    };
    match result {
        Ok(value) => {
            let delivered = respond(
                &mut stream,
                if crate::app::is_success(&value) {
                    200
                } else {
                    409
                },
                value.to_string().as_bytes(),
                None,
            );
            if delivered.is_ok() && applying_update {
                app.acknowledge_update_reply(&value);
            }
            delivered
        }
        Err(error) => respond(
            &mut stream,
            400,
            json!({"error":error}).to_string().as_bytes(),
            None,
        ),
    }
}

fn bind_listener(bind: &str, token: Option<String>) -> Result<(TcpListener, String), String> {
    let address: SocketAddr = bind
        .parse()
        .map_err(|_| "Listen address must be a literal loopback IP and port")?;
    if !address.ip().is_loopback() {
        return Err("The local API only binds to loopback addresses".into());
    }
    let generated = token.is_none();
    let token = match token {
        Some(token) if token.len() >= 24 && !token.bytes().any(|b| b.is_ascii_control()) => token,
        Some(_) => {
            return Err(
                "API token must contain at least 24 characters without control characters".into(),
            );
        }
        None => random_token()?,
    };
    let listener = TcpListener::bind(address).map_err(|e| e.to_string())?;
    if generated {
        eprintln!("API bearer token: {token}");
    }
    eprintln!(
        "bilibilive listening at http://{}",
        listener.local_addr().map_err(|e| e.to_string())?
    );
    Ok((listener, token))
}

/// Bind synchronously so a GUI can report an unusable endpoint before opening.
pub fn spawn(app: App, bind: &str, token: Option<String>, compat: bool) -> Result<(), String> {
    let (listener, token) = bind_listener(bind, token)?;
    std::thread::Builder::new()
        .name("bilibilive-http".into())
        .spawn(move || {
            if let Err(error) = serve_listener(app, listener, token, compat) {
                eprintln!("Local API: {error}");
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn serve(app: App, bind: &str, token: Option<String>, compat: bool) -> Result<(), String> {
    let (listener, token) = bind_listener(bind, token)?;
    serve_listener(app, listener, token, compat)
}

fn serve_listener(
    app: App,
    listener: TcpListener,
    token: String,
    compat: bool,
) -> Result<(), String> {
    let sessions: Sessions = Arc::new(Mutex::new(BTreeMap::new()));
    let active = Arc::new(AtomicUsize::new(0));
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    while !app.update_exit_requested() {
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.to_string()),
        };
        stream.set_nonblocking(false).map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .map_err(|e| e.to_string())?;
        if active.fetch_add(1, Ordering::AcqRel) >= 16 {
            active.fetch_sub(1, Ordering::AcqRel);
            let _ = respond(&mut stream, 429, br#"{"error":"Busy"}"#, None);
            continue;
        }
        let app = app.clone();
        let sessions = sessions.clone();
        let token = token.clone();
        let active = active.clone();
        std::thread::spawn(move || {
            if let Err(error) = handle(stream, app, sessions, &token, compat) {
                eprintln!("Local API: {error}");
            }
            active.fetch_sub(1, Ordering::AcqRel);
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_request_smuggling_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let thread = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            assert!(read_request(&stream).unwrap_err().contains("Duplicate"));
        });
        let mut client = TcpStream::connect(addr).unwrap();
        client.write_all(b"POST /api/call HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nContent-Length: 8\r\n\r\n").unwrap();
        thread.join().unwrap();
    }

    #[test]
    fn authenticated_api_uses_the_shared_app_and_rejects_missing_token() {
        let directory =
            std::env::temp_dir().join(format!("bilibilive-http-{}", random_token().unwrap()));
        let app = App::new(Some(directory.clone())).unwrap();
        app.set_cookie_override(
            "SESSDATA=local-fixture;bili_jct=0123456789abcdef0123456789abcdef;DedeUserID=42".into(),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let token = "local-fixture-token-0123456789";
        let worker = std::thread::spawn(move || {
            let sessions = Arc::new(Mutex::new(BTreeMap::new()));
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                handle(stream, app.clone(), sessions.clone(), token, false).unwrap();
            }
        });
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let endpoint = format!("http://{address}/api/call");
        let denied = client
            .post(&endpoint)
            .json(&json!({"method":"config.get","arguments":{}}))
            .send()
            .unwrap();
        assert_eq!(denied.status(), reqwest::StatusCode::UNAUTHORIZED);
        let response = client
            .post(&endpoint)
            .bearer_auth(token)
            .json(&json!({"method":"config.get","arguments":{}}))
            .send()
            .unwrap();
        assert!(response.status().is_success());
        let value: Value = response.json().unwrap();
        assert_eq!(value["credential_source"], "override");
        assert_eq!(value["effective_uid"], "42");
        assert_eq!(value["_context"]["uid"], "42");
        worker.join().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
