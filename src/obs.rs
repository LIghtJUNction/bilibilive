//! OBS WebSocket v5. One client owns one socket; requests and event reads are serialized.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

fn remaining(deadline: Instant) -> Result<Duration, String> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|value| !value.is_zero())
        .ok_or_else(|| "Connection deadline exceeded".into())
}

/// DNS and all TCP addresses share one deadline. A blocked OS resolver may
/// finish its background lookup, but never holds the application worker.
pub(crate) fn connect_tcp(host: &str, port: u16, deadline: Instant) -> Result<TcpStream, String> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let addresses = match host.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => {
            let host = host.to_owned();
            let (sender, receiver) = mpsc::channel();
            thread::spawn(move || {
                let result = (host.as_str(), port)
                    .to_socket_addrs()
                    .map(|addresses| addresses.take(16).collect::<Vec<_>>())
                    .map_err(|error| error.to_string());
                let _ = sender.send(result);
            });
            receiver
                .recv_timeout(remaining(deadline)?)
                .map_err(|_| "DNS lookup timed out")??
        }
    };
    let mut last = "Host has no usable addresses".to_owned();
    for address in addresses {
        match TcpStream::connect_timeout(&address, remaining(deadline)?) {
            Ok(stream) => {
                stream
                    .set_nodelay(true)
                    .map_err(|error| error.to_string())?;
                let timeout = remaining(deadline)?;
                stream
                    .set_read_timeout(Some(timeout))
                    .map_err(|error| error.to_string())?;
                stream
                    .set_write_timeout(Some(timeout))
                    .map_err(|error| error.to_string())?;
                return Ok(stream);
            }
            Err(error) => last = error.to_string(),
        }
    }
    Err(format!("TCP connection failed: {last}"))
}

/// Set socket deadlines before TLS or the HTTP Upgrade handshake starts.
/// A shutdown watchdog also bounds a peer that trickles an unfinished frame.
pub(crate) fn connect_websocket(
    url: &str,
    deadline: Instant,
    max_message: usize,
) -> Result<WebSocket<MaybeTlsStream<TcpStream>>, String> {
    use tungstenite::client::IntoClientRequest;
    let request = url
        .into_client_request()
        .map_err(|error| error.to_string())?;
    let secure = match request.uri().scheme_str() {
        Some("ws") => false,
        Some("wss") => true,
        _ => return Err("WebSocket address must use ws:// or wss://".into()),
    };
    // The updater and HTTP client can enable different Rustls crypto features
    // together. Choose one before Tungstenite builds its TLS configuration;
    // preserve a provider explicitly installed by an embedding application.
    if secure && rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }
    let host = request
        .uri()
        .host()
        .ok_or("WebSocket address requires a host")?;
    let port = request
        .uri()
        .port_u16()
        .unwrap_or(if secure { 443 } else { 80 });
    let stream = connect_tcp(host, port, deadline)?;
    let interrupt = stream.try_clone().map_err(|error| error.to_string())?;
    let timeout = remaining(deadline)?;
    let (finished, completion) = mpsc::channel::<()>();
    thread::spawn(move || {
        if matches!(
            completion.recv_timeout(timeout),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            let _ = interrupt.shutdown(Shutdown::Both);
        }
    });
    let config = tungstenite::protocol::WebSocketConfig::default()
        .read_buffer_size(16 * 1024)
        .write_buffer_size(16 * 1024)
        .max_message_size(Some(max_message))
        .max_frame_size(Some(max_message));
    let result = tungstenite::client_tls_with_config(request, stream, Some(config), None)
        .map(|(socket, _)| socket)
        .map_err(|error| format!("WebSocket handshake failed: {error}"));
    drop(finished);
    result
}

pub fn authentication(password: &str, salt: &str, challenge: &str) -> String {
    let secret = STANDARD.encode(Sha256::digest(format!("{password}{salt}").as_bytes()));
    STANDARD.encode(Sha256::digest(format!("{secret}{challenge}").as_bytes()))
}

pub struct ObsClient {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    serial: u64,
    events: VecDeque<Value>,
}

impl ObsClient {
    pub fn connect(url: &str, password: &str) -> Result<Self, String> {
        Self::connect_timeout(url, password, Duration::from_secs(8))
    }

    fn connect_timeout(url: &str, password: &str, timeout: Duration) -> Result<Self, String> {
        if !url.starts_with("ws://") && !url.starts_with("wss://") {
            return Err("OBS address must begin with ws:// or wss://".into());
        }
        let deadline = Instant::now() + timeout;
        let socket = connect_websocket(url, deadline, 16 * 1024 * 1024)?;
        let mut client = Self {
            socket,
            serial: 0,
            events: VecDeque::new(),
        };
        client.timeout(remaining(deadline)?)?;
        let hello = client.read_json()?;
        if hello["op"] != 0 || hello["d"]["rpcVersion"].as_u64().unwrap_or(0) < 1 {
            return Err("OBS did not provide a WebSocket v5 Hello".into());
        }
        let mut identify = json!({"rpcVersion":1,"eventSubscriptions": 1 | 64 | 128 | 256});
        if let Some(auth) = hello["d"].get("authentication") {
            let salt = auth["salt"]
                .as_str()
                .ok_or("OBS Hello omitted authentication salt")?;
            let challenge = auth["challenge"]
                .as_str()
                .ok_or("OBS Hello omitted authentication challenge")?;
            identify["authentication"] = authentication(password, salt, challenge).into();
        }
        client.timeout(remaining(deadline)?)?;
        client.write_json(json!({"op":1,"d":identify}))?;
        let identified = client.read_json()?;
        if identified["op"] != 2 {
            return Err("OBS refused WebSocket identification".into());
        }
        client.timeout(Duration::from_secs(8))?;
        Ok(client)
    }

    fn timeout(&mut self, timeout: Duration) -> Result<(), String> {
        let stream = match self.socket.get_mut() {
            MaybeTlsStream::Plain(stream) => stream,
            MaybeTlsStream::Rustls(stream) => &mut stream.sock,
            _ => return Ok(()),
        };
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|e| e.to_string())
    }

    fn write_json(&mut self, value: Value) -> Result<(), String> {
        self.socket
            .send(Message::Text(value.to_string().into()))
            .map_err(|e| format!("OBS send failed: {e}"))
    }

    fn read_json(&mut self) -> Result<Value, String> {
        loop {
            match self
                .socket
                .read()
                .map_err(|e| format!("OBS receive failed: {e}"))?
            {
                Message::Text(text) => {
                    return serde_json::from_str(&text)
                        .map_err(|e| format!("Invalid OBS JSON: {e}"));
                }
                Message::Ping(_) => self.socket.flush().map_err(|e| e.to_string())?,
                Message::Pong(_) => {}
                Message::Close(frame) => {
                    return Err(format!("OBS closed the connection: {frame:?}"));
                }
                _ => return Err("OBS sent a non-text protocol message".into()),
            }
        }
    }

    pub fn request(&mut self, request_type: &str, data: Value) -> Result<Value, String> {
        if request_type.is_empty() || request_type.len() > 128 || !data.is_object() {
            return Err("OBS request requires a name and JSON object".into());
        }
        self.serial = self.serial.wrapping_add(1);
        let id = format!("bilibilive-{}", self.serial);
        self.write_json(
            json!({"op":6,"d":{"requestType":request_type,"requestId":id,"requestData":data}}),
        )?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let reply = self.read_json()?;
            if reply["op"] == 5 {
                if self.events.len() == 256 {
                    self.events.pop_front();
                }
                self.events.push_back(reply["d"].clone());
                continue;
            }
            if reply["op"] != 7 || reply["d"]["requestId"] != id {
                continue;
            }
            let status = &reply["d"]["requestStatus"];
            if status["result"] != true {
                return Err(format!(
                    "OBS {request_type} failed ({}): {}",
                    status["code"],
                    status["comment"].as_str().unwrap_or("request rejected")
                ));
            }
            return Ok(reply["d"]
                .get("responseData")
                .cloned()
                .unwrap_or_else(|| json!({})));
        }
        Err(format!("OBS {request_type} timed out"))
    }

    pub(crate) fn pending_events(&mut self) -> Vec<Value> {
        self.events.drain(..).collect()
    }

    pub fn configure(&mut self, server: &str, key: &str) -> Result<Value, String> {
        if !(server.starts_with("rtmp://")
            || server.starts_with("rtmps://")
            || server.starts_with("srt://"))
        {
            return Err("OBS stream server must use RTMP, RTMPS or SRT".into());
        }
        self.request("SetStreamServiceSettings", json!({"streamServiceType":"rtmp_custom","streamServiceSettings":{"server":server,"key":key,"use_auth":false}}))
    }
    pub fn start(&mut self) -> Result<Value, String> {
        self.request("StartStream", json!({}))
    }
    pub fn stop(&mut self) -> Result<Value, String> {
        self.request("StopStream", json!({}))
    }
    pub fn status(&mut self) -> Result<Value, String> {
        self.request("GetStreamStatus", json!({}))
    }
    pub fn next_event(&mut self) -> Result<Value, String> {
        if let Some(value) = self.events.pop_front() {
            return Ok(value);
        }
        loop {
            let value = self.read_json()?;
            if value["op"] == 5 {
                return Ok(value["d"].clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, thread};

    #[test]
    fn stalled_plain_and_tls_handshakes_are_bounded() {
        for scheme in ["ws", "wss"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let (release, released) = mpsc::channel::<()>();
            let server = thread::spawn(move || {
                let (_stream, _) = listener.accept().unwrap();
                let _ = released.recv_timeout(Duration::from_secs(2));
            });
            let started = Instant::now();
            let result = connect_websocket(
                &format!("{scheme}://{address}"),
                started + Duration::from_millis(150),
                4096,
            );
            assert!(result.is_err(), "unfinished handshake must fail");
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "deadline must fire before server closes"
            );
            let _ = release.send(());
            server.join().unwrap();
        }
    }

    #[test]
    fn stalled_obs_hello_is_bounded() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (release, released) = mpsc::channel::<()>();
        let server = thread::spawn(move || {
            let _websocket = tungstenite::accept(listener.accept().unwrap().0).unwrap();
            let _ = released.recv_timeout(Duration::from_secs(2));
        });
        let started = Instant::now();
        assert!(
            ObsClient::connect_timeout(&format!("ws://{address}"), "", Duration::from_millis(150))
                .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        let _ = release.send(());
        server.join().unwrap();
    }

    #[test]
    fn authenticated_socket_and_request_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut ws = tungstenite::accept(listener.accept().unwrap().0).unwrap();
            ws.send(Message::Text(json!({"op":0,"d":{"rpcVersion":1,"authentication":{"salt":"salt","challenge":"challenge"}}}).to_string().into())).unwrap();
            let identify: Value =
                serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(
                identify["d"]["authentication"],
                authentication("password", "salt", "challenge")
            );
            ws.send(Message::Text(
                json!({"op":2,"d":{"negotiatedRpcVersion":1}})
                    .to_string()
                    .into(),
            ))
            .unwrap();
            let request: Value =
                serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(request["d"]["requestType"], "GetStreamStatus");
            ws.send(Message::Text(json!({"op":5,"d":{"eventType":"StreamStateChanged","eventData":{"outputActive":true}}}).to_string().into())).unwrap();
            ws.send(Message::Text(json!({"op":7,"d":{"requestId":request["d"]["requestId"],"requestStatus":{"result":true,"code":100},"responseData":{"outputActive":true}}}).to_string().into())).unwrap();
            let request: Value =
                serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            ws.send(Message::Text(json!({"op":7,"d":{"requestId":request["d"]["requestId"],"requestStatus":{"result":false,"code":500,"comment":"already active"}}}).to_string().into())).unwrap();
        });
        let mut client = ObsClient::connect(&format!("ws://{address}"), "password").unwrap();
        assert_eq!(client.status().unwrap()["outputActive"], true);
        assert_eq!(
            client.next_event().unwrap()["eventType"],
            "StreamStateChanged"
        );
        assert!(client.start().unwrap_err().contains("already active"));
        server.join().unwrap();
    }
}
