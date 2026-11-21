//! Validated, immutable snapshots of StartLive's public LiveHime protocol data.
//! Only an explicit refresh contacts the fixed upstream source. Account cookies
//! and arbitrary request destinations are never part of this format.
use crate::bili::BiliOptions;
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const SOURCE_URL: &str = "https://raw.githubusercontent.com/Radekyspec/StartLive/refs/heads/dev/startlive/resources/version.json";
const MAX_DOCUMENT: u64 = 32 * 1024;
const BUILTIN: &str = r#"{
  "ak":"aae92bc66f3edfab","as":"af125a0d5279fd576c1b4418a3e8276d",
  "b":"11081","v":"8.7.0.11081",
  "hw":{"Accept":"*/*","Accept-Encoding":"gzip, deflate","Accept-Language":"zh-CN,zh;q=0.9","Connection":"keep-alive","Origin":"https://live.bilibili.com","Referer":"https://live.bilibili.com/","sec-ch-ua":"\"Chromium\";v=\"105\"","sec-ch-ua-mobile":"?0","sec-ch-ua-platform":"\"Windows\"","Sec-Fetch-Site":"same-site","Sec-Fetch-Mode":"cors","Sec-Fetch-Dest":"empty","User-Agent":"Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/105.0.0.0 Safari/537.36 pc_app/livehime build/11081"},
  "ha":{"Accept-Encoding":"gzip,deflate","Connection":"keep-alive","User-Agent":"LiveHime/8.7.0.11081 os/Windows pc_app/livehime build/11081 osVer/10.0_x86_64"},
  "start_ac":true,"stop_ac":false
}"#;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireProfile {
    ak: String,
    #[serde(rename = "as")]
    secret: String,
    b: String,
    v: String,
    #[serde(deserialize_with = "header_document")]
    hw: BTreeMap<String, String>,
    #[serde(deserialize_with = "header_document")]
    ha: BTreeMap<String, String>,
    start_ac: bool,
    stop_ac: bool,
}

fn header_document<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    struct Headers;
    impl<'de> de::Visitor<'de> for Headers {
        type Value = BTreeMap<String, String>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a bounded map of unique HTTP header names to strings")
        }
        fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut result = BTreeMap::new();
            let mut names = BTreeSet::new();
            while let Some((name, value)) = map.next_entry::<String, String>()? {
                if result.len() >= 24 || !names.insert(name.to_ascii_lowercase()) {
                    return Err(de::Error::custom("duplicate or excessive protocol headers"));
                }
                result.insert(name, value);
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Headers)
}

/// Validated once, then copied as a unit into a BiliClient. No field is publicly
/// writable, so a refresh cannot partially change an in-flight request's data.
#[derive(Clone)]
pub struct ProtocolProfile {
    wire: WireProfile,
    web: HeaderMap,
    app: HeaderMap,
    digest: String,
    build: u32,
}

impl Default for ProtocolProfile {
    fn default() -> Self {
        Self::parse(BUILTIN.as_bytes()).expect("the embedded protocol profile is validated")
    }
}

impl std::fmt::Debug for ProtocolProfile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProtocolProfile")
            .field("build", &self.wire.b)
            .field("version", &self.wire.v)
            .field("sha256", &self.digest)
            .finish_non_exhaustive()
    }
}

impl ProtocolProfile {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_DOCUMENT {
            return Err("Protocol profile exceeds 32 KiB".into());
        }
        let wire: WireProfile = serde_json::from_slice(bytes)
            .map_err(|_| "Protocol profile must contain exactly the eight typed upstream fields")?;
        Self::validate(wire)
    }

    fn validate(wire: WireProfile) -> Result<Self, String> {
        if wire.ak.len() != 16
            || !wire.ak.bytes().all(|byte| byte.is_ascii_hexdigit())
            || wire.secret.len() != 32
            || !wire.secret.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(
                "Protocol application key or signing constant has an invalid format".into(),
            );
        }
        let build = wire
            .b
            .parse::<u32>()
            .ok()
            .filter(|value| *value > 0)
            .filter(|value| value.to_string() == wire.b)
            .ok_or("Protocol build must be a canonical positive uint32")?;
        let components: Vec<_> = wire.v.split('.').collect();
        if wire.v.len() > 64
            || !(2..=4).contains(&components.len())
            || components.iter().any(|part| {
                part.is_empty()
                    || !part.bytes().all(|b| b.is_ascii_digit())
                    || part.parse::<u32>().is_err()
            })
        {
            return Err("Protocol version must contain two to four numeric components".into());
        }
        let web = validated_headers(&wire.hw)?;
        let app = validated_headers(&wire.ha)?;
        let bytes = serde_json::to_vec(&wire).map_err(|_| "Could not encode protocol profile")?;
        let digest = sha(&bytes);
        Ok(Self {
            wire,
            web,
            app,
            digest,
            build,
        })
    }

    pub(crate) fn app_key(&self) -> &str {
        &self.wire.ak
    }
    pub(crate) fn app_secret(&self) -> &str {
        &self.wire.secret
    }
    pub(crate) fn build(&self) -> &str {
        &self.wire.b
    }
    pub(crate) fn build_number(&self) -> u32 {
        self.build
    }
    pub(crate) fn version(&self) -> &str {
        &self.wire.v
    }
    pub(crate) fn headers(&self, web: bool) -> &HeaderMap {
        if web { &self.web } else { &self.app }
    }
    pub(crate) fn start_csrf(&self) -> bool {
        self.wire.start_ac
    }
    pub(crate) fn stop_csrf(&self) -> bool {
        self.wire.stop_ac
    }
}

fn validated_headers(headers: &BTreeMap<String, String>) -> Result<HeaderMap, String> {
    let mut result = HeaderMap::new();
    let mut total = 0;
    for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        if !matches!(
            lower.as_str(),
            "user-agent"
                | "accept"
                | "accept-encoding"
                | "accept-language"
                | "connection"
                | "origin"
                | "referer"
                | "sec-ch-ua"
                | "sec-ch-ua-mobile"
                | "sec-ch-ua-platform"
                | "sec-fetch-site"
                | "sec-fetch-mode"
                | "sec-fetch-dest"
        ) {
            return Err("Protocol profile contains a disallowed HTTP header".into());
        }
        total += name.len() + value.len();
        if value.is_empty()
            || value.len() > 1024
            || total > 8192
            || !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
        {
            return Err(
                "Protocol HTTP header value is empty, oversized, or contains control bytes".into(),
            );
        }
        if (lower == "origin" && value != "https://live.bilibili.com")
            || (lower == "referer" && value != "https://live.bilibili.com/")
            || (lower == "connection" && !["keep-alive", "close"].contains(&value.as_str()))
        {
            return Err(
                "Protocol HTTP header changes a fixed Bilibili origin or transport policy".into(),
            );
        }
        if lower == "accept-encoding"
            && value
                .split(',')
                .any(|item| !["gzip", "deflate", "br", "identity"].contains(&item.trim()))
        {
            return Err("Protocol HTTP header requests an unsupported content encoding".into());
        }
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| "Invalid protocol HTTP header name")?;
        let value =
            HeaderValue::from_str(value).map_err(|_| "Invalid protocol HTTP header value")?;
        if result.insert(name, value).is_some() {
            return Err("Protocol profile contains duplicate HTTP header names".into());
        }
    }
    if !result.contains_key("user-agent") {
        return Err("Each protocol header set must declare a User-Agent".into());
    }
    Ok(result)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CachedProfile {
    schema: u32,
    source: String,
    fetched_at: u64,
    sha256: String,
    document: WireProfile,
}

struct State {
    profile: ProtocolProfile,
    source: &'static str,
    fetched_at: Option<u64>,
    cache_error: Option<String>,
    refresh_error: Option<String>,
}

/// Per-configuration-directory cache. Reading never starts network activity.
pub struct ProtocolProfiles {
    directory: PathBuf,
    state: Mutex<State>,
    refresh_gate: Mutex<()>,
}

impl ProtocolProfiles {
    pub fn new(directory: impl AsRef<Path>) -> Self {
        Self {
            directory: directory.as_ref().join("protocol-profile"),
            state: Mutex::new(State {
                profile: ProtocolProfile::default(),
                source: "builtin",
                fetched_at: None,
                cache_error: None,
                refresh_error: None,
            }),
            refresh_gate: Mutex::new(()),
        }
    }

    fn cache_path(&self) -> PathBuf {
        self.directory.join("version.json")
    }

    /// Revalidate the bounded cache each time, including replacements written
    /// by another CLI/GUI process. Invalid data preserves the last good snapshot.
    pub fn snapshot(&self) -> Result<ProtocolProfile, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Protocol profile state lock failed")?;
        match self.read_cache() {
            Ok(Some((profile, fetched_at))) => {
                state.profile = profile;
                state.source = SOURCE_URL;
                state.fetched_at = Some(fetched_at);
                state.cache_error = None;
            }
            Ok(None) => {
                state.cache_error = if state.fetched_at.is_some() {
                    Some("Protocol cache was removed; keeping the last valid snapshot".into())
                } else {
                    None
                };
            }
            Err(error) => state.cache_error = Some(error),
        }
        Ok(state.profile.clone())
    }

    pub fn status(&self) -> Value {
        if let Err(error) = self.snapshot() {
            return json!({"ok":false,"status":"failed","last_error":error,
                "cache_path":self.cache_path().display().to_string()});
        }
        let Ok(state) = self.state.lock() else {
            return json!({"ok":false,"status":"failed","last_error":"Protocol profile state lock failed"});
        };
        self.state_value(&state)
    }

    fn state_value(&self, state: &State) -> Value {
        json!({"ok":true,"status":"ready","source":state.source,
            "sha256":state.profile.digest,"build":state.profile.build(),
            "version":state.profile.version(),"cache_path":self.cache_path().display().to_string(),
            "fetched_at":state.fetched_at,"last_error":state.cache_error.as_ref().or(state.refresh_error.as_ref()),
            "start_csrf_in_signature":state.profile.start_csrf(),
            "stop_csrf_in_signature":state.profile.stop_csrf()})
    }

    pub fn refresh(&self, options: &BiliOptions) -> Result<Value, String> {
        self.refresh_at(options, SOURCE_URL)
    }

    fn refresh_at(&self, options: &BiliOptions, url: &str) -> Result<Value, String> {
        // Production has one source. Tests may inject only an explicit loopback
        // endpoint; no URL is accepted from config, CLI arguments or the cache.
        if url != SOURCE_URL {
            #[cfg(not(test))]
            return Err("Protocol profile source is fixed".into());
            #[cfg(test)]
            {
                let parsed = reqwest::Url::parse(url).map_err(|_| "Invalid test endpoint")?;
                if parsed.scheme() != "http"
                    || parsed.host_str() != Some("127.0.0.1")
                    || !parsed.username().is_empty()
                    || parsed.password().is_some()
                {
                    return Err("Protocol fixture source must be loopback".into());
                }
            }
        }
        let _gate = self
            .refresh_gate
            .try_lock()
            .map_err(|_| "A protocol refresh is already running")?;
        let result = self.refresh_locked(options, url);
        if let Err(error) = &result
            && let Ok(mut state) = self.state.lock()
        {
            state.refresh_error = Some(error.clone());
        }
        result
    }

    fn refresh_locked(&self, options: &BiliOptions, url: &str) -> Result<Value, String> {
        let previous_digest = self.snapshot()?.digest;
        fs::create_dir_all(&self.directory)
            .map_err(|_| "Could not create protocol cache directory")?;
        if fs::symlink_metadata(&self.directory)
            .map_err(|_| "Could not inspect protocol cache directory")?
            .file_type()
            .is_symlink()
        {
            return Err("Protocol cache directory must not be a symbolic link".into());
        }
        let lock_path = self.directory.join("refresh.lock");
        if fs::symlink_metadata(&lock_path).is_ok_and(|metadata| !metadata.file_type().is_file()) {
            return Err("Protocol refresh lock must be a regular file".into());
        }
        let mut open = OpenOptions::new();
        open.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            open.mode(0o600);
        }
        let lock = open
            .open(lock_path)
            .map_err(|_| "Could not open protocol refresh lock")?;
        lock.try_lock()
            .map_err(|_| "Another process is refreshing this protocol cache")?;
        // No CookieJar, account data, Authorization, or response cookie store.
        let mut builder = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(options.timeout_secs.clamp(1, 30)))
            .redirect(reqwest::redirect::Policy::none());
        if !options.use_system_proxy {
            builder = builder.no_proxy();
        }
        if let Some(proxy) = options.proxy.as_deref().filter(|value| !value.is_empty()) {
            if !(proxy.starts_with("http://") || proxy.starts_with("https://")) {
                return Err("Protocol refresh proxy must use HTTP or HTTPS".into());
            }
            builder = builder
                .proxy(reqwest::Proxy::all(proxy).map_err(|_| "Invalid protocol refresh proxy")?);
        }
        let client = builder
            .build()
            .map_err(|_| "Could not initialize protocol HTTPS client")?;
        let response = client
            .get(url)
            .header("Accept", "application/json")
            .header("Cache-Control", "no-cache")
            .header(
                "User-Agent",
                concat!(
                    "bilibilive/",
                    env!("CARGO_PKG_VERSION"),
                    " protocol-profile"
                ),
            )
            .send()
            .map_err(|error| {
                if error.is_timeout() {
                    "Protocol refresh timed out"
                } else {
                    "Could not fetch the official protocol profile over HTTPS"
                }
            })?;
        if !response.status().is_success() {
            return Err(format!(
                "Protocol source returned HTTP {}; cached data was retained",
                response.status().as_u16()
            ));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_DOCUMENT)
        {
            return Err("Protocol response exceeds 32 KiB".into());
        }
        let mut bytes = Vec::new();
        response
            .take(MAX_DOCUMENT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Could not read the complete protocol response")?;
        let profile = ProtocolProfile::parse(&bytes)?;
        let fetched_at = now();
        self.save(&CachedProfile {
            schema: 1,
            source: SOURCE_URL.into(),
            fetched_at,
            sha256: profile.digest.clone(),
            document: profile.wire.clone(),
        })?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Protocol profile state lock failed")?;
        let changed = profile.digest != previous_digest;
        state.profile = profile;
        state.source = SOURCE_URL;
        state.fetched_at = Some(fetched_at);
        state.cache_error = None;
        state.refresh_error = None;
        let mut value = self.state_value(&state);
        value["changed"] = json!(changed);
        Ok(value)
    }

    fn read_cache(&self) -> Result<Option<(ProtocolProfile, u64)>, String> {
        let path = self.cache_path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => {
                return Err(
                    "Could not inspect protocol cache; keeping the last valid snapshot".into(),
                );
            }
        };
        if !metadata.file_type().is_file() || metadata.len() > MAX_DOCUMENT {
            return Err("Protocol cache must be a regular file no larger than 32 KiB".into());
        }
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(|_| "Could not open protocol cache")?
            .take(MAX_DOCUMENT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Could not read protocol cache")?;
        if bytes.len() as u64 > MAX_DOCUMENT {
            return Err("Protocol cache exceeds 32 KiB".into());
        }
        let cached: CachedProfile = serde_json::from_slice(&bytes)
            .map_err(|_| "Protocol cache is malformed; keeping the last valid snapshot")?;
        if cached.schema != 1
            || cached.source != SOURCE_URL
            || cached.fetched_at == 0
            || cached.fetched_at > now().saturating_add(300)
        {
            return Err("Protocol cache identity or timestamp is invalid".into());
        }
        let profile = ProtocolProfile::validate(cached.document)?;
        if profile.digest != cached.sha256 {
            return Err("Protocol cache digest does not match its contents".into());
        }
        Ok(Some((profile, cached.fetched_at)))
    }

    fn save(&self, cached: &CachedProfile) -> Result<(), String> {
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).map_err(|_| "Could not create a protocol cache transaction")?;
        let temporary = self.directory.join(format!(
            ".version-{}-{}.tmp",
            std::process::id(),
            u64::from_ne_bytes(nonce)
        ));
        let result: Result<(), String> = (|| {
            let bytes =
                serde_json::to_vec(cached).map_err(|_| "Could not encode protocol cache")?;
            let mut open = OpenOptions::new();
            open.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                open.mode(0o600);
            }
            let mut file = open
                .open(&temporary)
                .map_err(|_| "Could not create protocol cache transaction")?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| "Could not save protocol cache")?;
            drop(file);
            fs::rename(&temporary, self.cache_path())
                .map_err(|_| "Could not atomically replace protocol cache".to_owned())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    #[cfg(test)]
    pub(crate) fn refresh_fixture(
        &self,
        options: &BiliOptions,
        url: &str,
    ) -> Result<Value, String> {
        self.refresh_at(options, url)
    }
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let mut nonce = [0; 8];
            getrandom::fill(&mut nonce).unwrap();
            Self(std::env::temp_dir().join(format!(
                "bilibilive-protocol-{}-{}",
                std::process::id(),
                u64::from_ne_bytes(nonce)
            )))
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn serve(
        status: &str,
        headers: &str,
        body: Vec<u8>,
        pause: Duration,
    ) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/version.json", listener.local_addr().unwrap());
        let status = status.to_owned();
        let headers = headers.to_owned();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut block = [0u8; 1024];
            while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                let n = socket.read(&mut block).unwrap();
                assert!(n > 0);
                request.extend_from_slice(&block[..n]);
                assert!(request.len() <= 8192);
            }
            thread::sleep(pause);
            let _ = write!(
                socket,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(&body);
            String::from_utf8(request).unwrap()
        });
        (url, server)
    }

    fn updated() -> Value {
        let mut value: Value = serde_json::from_str(BUILTIN).unwrap();
        value["b"] = json!("12000");
        value["v"] = json!("9.0.0.12000");
        value["ha"]["User-Agent"] = json!("LiveHime/9.0.0.12000 build/12000");
        value["start_ac"] = json!(false);
        value["stop_ac"] = json!(true);
        value
    }

    #[test]
    fn real_refresh_is_cookie_free_and_shared_cache_updates_other_frontends() {
        let directory = Directory::new();
        let first = ProtocolProfiles::new(&directory.0);
        let second = ProtocolProfiles::new(&directory.0);
        let previous = second.snapshot().unwrap();
        assert!(!directory.0.exists());
        let (url, server) = serve(
            "200 OK",
            "Set-Cookie: SESSDATA=must-not-be-stored\r\n",
            serde_json::to_vec(&updated()).unwrap(),
            Duration::ZERO,
        );
        let receipt = first
            .refresh_fixture(&BiliOptions::default(), &url)
            .unwrap();
        assert_eq!(receipt["build"], "12000");
        assert_eq!(receipt["changed"], true);
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(request.starts_with("get /version.json http/1.1"));
        assert!(!request.contains("cookie:"));
        assert!(!request.contains("authorization:"));
        assert!(
            !fs::read_to_string(first.cache_path())
                .unwrap()
                .contains("must-not-be-stored")
        );
        assert_eq!(second.snapshot().unwrap().build(), "12000");
        assert_eq!(previous.build(), "11081");
        let status = first.status().to_string();
        for forbidden in [
            previous.app_key(),
            previous.app_secret(),
            "User-Agent",
            "must-not-be-stored",
        ] {
            assert!(!status.contains(forbidden));
        }
    }

    #[test]
    fn invalid_refresh_and_cache_preserve_last_good_snapshot() {
        let directory = Directory::new();
        let manager = ProtocolProfiles::new(&directory.0);
        let (url, server) = serve(
            "200 OK",
            "",
            serde_json::to_vec(&updated()).unwrap(),
            Duration::ZERO,
        );
        manager
            .refresh_fixture(&BiliOptions::default(), &url)
            .unwrap();
        server.join().unwrap();
        let before = fs::read(manager.cache_path()).unwrap();
        let mut invalid = updated();
        invalid["ha"]["Cookie"] = json!("SESSDATA=forbidden");
        let (url, server) = serve(
            "200 OK",
            "",
            serde_json::to_vec(&invalid).unwrap(),
            Duration::ZERO,
        );
        assert!(
            manager
                .refresh_fixture(&BiliOptions::default(), &url)
                .is_err()
        );
        server.join().unwrap();
        assert_eq!(fs::read(manager.cache_path()).unwrap(), before);
        assert_eq!(manager.snapshot().unwrap().build(), "12000");
        assert!(manager.status()["last_error"].is_string());
        fs::write(manager.cache_path(), b"not a profile").unwrap();
        assert_eq!(manager.snapshot().unwrap().build(), "12000");
        assert!(
            manager.status()["last_error"]
                .as_str()
                .unwrap()
                .contains("malformed")
        );
        let fresh = ProtocolProfiles::new(&directory.0);
        assert_eq!(fresh.snapshot().unwrap().build(), "11081");
        assert!(fresh.status()["last_error"].is_string());
    }

    #[test]
    fn redirect_oversize_and_timeout_do_not_replace_cache_or_replay_cookies() {
        let directory = Directory::new();
        let manager = ProtocolProfiles::new(&directory.0);
        let (url, server) = serve(
            "302 Found",
            "Location: https://example.invalid/forbidden\r\n",
            Vec::new(),
            Duration::ZERO,
        );
        assert!(
            manager
                .refresh_fixture(&BiliOptions::default(), &url)
                .unwrap_err()
                .contains("302")
        );
        server.join().unwrap();
        let (url, server) = serve(
            "200 OK",
            "",
            vec![b' '; MAX_DOCUMENT as usize + 1],
            Duration::ZERO,
        );
        assert!(
            manager
                .refresh_fixture(&BiliOptions::default(), &url)
                .is_err()
        );
        server.join().unwrap();
        let (url, server) = serve(
            "200 OK",
            "",
            BUILTIN.as_bytes().to_vec(),
            Duration::from_millis(1200),
        );
        let options = BiliOptions {
            timeout_secs: 1,
            ..BiliOptions::default()
        };
        assert!(
            manager
                .refresh_fixture(&options, &url)
                .unwrap_err()
                .contains("timed out")
        );
        server.join().unwrap();
        assert!(!manager.cache_path().exists());
        assert_eq!(manager.snapshot().unwrap().build(), "11081");
    }

    #[test]
    fn protocol_fields_and_header_boundaries_are_strict() {
        for (key, value) in [
            ("b", json!("001")),
            ("v", json!("1.2\r\nX: bad")),
            ("ak", json!("not-hex")),
            ("as", json!("short")),
            ("start_ac", json!(1)),
        ] {
            let mut doc = updated();
            doc[key] = value;
            assert!(ProtocolProfile::parse(&serde_json::to_vec(&doc).unwrap()).is_err());
        }
        for (key, value) in [
            ("Host", "example.invalid"),
            ("Authorization", "bad"),
            ("Origin", "https://example.invalid"),
            ("User-Agent", "ok\r\nCookie: bad"),
            ("Content-Length", "0"),
            ("Accept-Encoding", "zstd"),
        ] {
            let mut doc = updated();
            doc["hw"][key] = json!(value);
            assert!(ProtocolProfile::parse(&serde_json::to_vec(&doc).unwrap()).is_err());
        }
        let duplicate = BUILTIN.replacen("\"ha\":{", "\"ha\":{\"user-agent\":\"duplicate\",", 1);
        assert!(ProtocolProfile::parse(duplicate.as_bytes()).is_err());
    }

    #[test]
    fn independent_managers_serialize_refreshes_without_hiding_the_old_snapshot() {
        use std::sync::{Arc, mpsc};
        let directory = Directory::new();
        let first = Arc::new(ProtocolProfiles::new(&directory.0));
        let second = ProtocolProfiles::new(&directory.0);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/version.json", listener.local_addr().unwrap());
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0u8; 4096];
            assert!(socket.read(&mut request).unwrap() > 0);
            ready_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let body = serde_json::to_vec(&updated()).unwrap();
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            socket.write_all(&body).unwrap();
        });
        let active = Arc::clone(&first);
        let worker_url = url.clone();
        let worker =
            thread::spawn(move || active.refresh_fixture(&BiliOptions::default(), &worker_url));
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(first.snapshot().unwrap().build(), "11081");
        assert!(
            second
                .refresh_fixture(&BiliOptions::default(), &url)
                .unwrap_err()
                .contains("Another process")
        );
        release_tx.send(()).unwrap();
        worker.join().unwrap().unwrap();
        server.join().unwrap();
        assert_eq!(second.snapshot().unwrap().build(), "12000");
    }
}
