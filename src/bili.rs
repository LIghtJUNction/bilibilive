//! Bilibili Live REST adapter. Protocol compatibility was checked against
//! StartLive 0b5a9830a8305d9cd6ac15378e46bee05b894c11 (GPL-3.0-only).
//! API requests never retry mutations, follow redirects, or disable TLS checks.
use crate::auth::CookieJar;
use crate::protocol_profile::ProtocolProfile;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use reqwest::blocking::{Client, RequestBuilder, Response, multipart};
use reqwest::header::{CONTENT_ENCODING, CONTENT_TYPE, COOKIE, SET_COOKIE};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type Fields = BTreeMap<String, String>;
pub type BiliResult<T> = Result<T, String>;

const MAX_RESPONSE: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct BiliOptions {
    pub proxy: Option<String>,
    pub use_system_proxy: bool,
    pub timeout_secs: u64,
}

impl Default for BiliOptions {
    fn default() -> Self {
        Self {
            proxy: None,
            use_system_proxy: false,
            timeout_secs: 15,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Operation {
    pub name: &'static str,
    pub description: &'static str,
    pub read_only: bool,
}

pub fn operations() -> Vec<Operation> {
    [
        (
            "auth.qr.create",
            "Create a Bilibili login QR challenge",
            false,
        ),
        (
            "auth.qr.poll",
            "Poll one login challenge; credentials remain private",
            false,
        ),
        ("auth.validate", "Validate the current account", true),
        (
            "auth.refresh",
            "Refresh browser ticket and device identifiers",
            false,
        ),
        (
            "room.get",
            "Read room, pre-live, announcement and delay state",
            true,
        ),
        ("room.update", "Update title, area and announcement", false),
        (
            "room.cover",
            "Upload and apply a live-room cover image",
            false,
        ),
        (
            "room.cover.preview",
            "Read the current platform cover from a verified Bilibili image host",
            true,
        ),
        ("room.audit", "Read room title audit result", true),
        (
            "room.precheck",
            "Check whether the account can start a live room",
            true,
        ),
        ("area.list", "List available live areas", true),
        ("area.recent", "Read recently used live areas", true),
        ("delay.get", "Read stream delay and platform limits", true),
        (
            "delay.set",
            "Set stream delay within platform limits",
            false,
        ),
        (
            "live.start",
            "Start a live room and obtain its stream endpoint",
            false,
        ),
        ("live.stop", "Stop the live room", false),
        (
            "live.stream",
            "Fetch the existing upstream address without starting a room",
            true,
        ),
        (
            "face.register",
            "Resolve a platform-issued verification challenge",
            false,
        ),
        (
            "face.poll",
            "Poll a platform-issued verification challenge once",
            false,
        ),
        (
            "face.report",
            "Report a platform-issued face challenge",
            false,
        ),
        ("pk.status", "Read current PK round status", true),
        ("pk.search", "Search PK candidates by UID or room", true),
        (
            "pk.match",
            "Request PK matching after a successful precheck",
            false,
        ),
        ("pk.cancel", "Cancel pending PK matching", false),
        ("pk.invite", "Invite a specific PK candidate", false),
        (
            "pk.accept",
            "Accept an identified incoming PK invitation",
            false,
        ),
        (
            "pk.reject",
            "Reject an identified incoming PK invitation",
            false,
        ),
        ("pk.end", "End the current PK round", false),
        (
            "pk.settings.get",
            "Read the complete PK preference snapshot",
            true,
        ),
        (
            "pk.settings.update",
            "Change one PK preference with native coupling and read-back verification",
            false,
        ),
        (
            "pk.continuous.get",
            "Read continuous PK matching state",
            true,
        ),
        (
            "pk.continuous.set",
            "Change continuous PK and read back the server state",
            false,
        ),
        (
            "pk.invitation.cancel",
            "Cancel an outgoing directed video PK invitation",
            false,
        ),
        (
            "pk.link.panel",
            "Prepare the video-link panel and read eligible candidates",
            false,
        ),
        (
            "pk.link.list",
            "Read current video-link invitations and participants",
            true,
        ),
        (
            "pk.link.connect",
            "Invite an eligible video-link candidate",
            false,
        ),
        (
            "pk.link.cancel",
            "Cancel an outgoing video-link invitation",
            false,
        ),
        (
            "pk.link.start",
            "Convert the identified active video link into a PK round",
            false,
        ),
        (
            "pk.link.disconnect",
            "End the round when requested, then quit the exact video link",
            false,
        ),
        (
            "pk.request",
            "Call one of the closed, audited PK REST operations",
            false,
        ),
        (
            "danmaku.send",
            "Send a chat message to the current live room",
            false,
        ),
    ]
    .into_iter()
    .map(|(name, description, read_only)| Operation {
        name,
        description,
        read_only,
    })
    .collect()
}

#[derive(Clone)]
struct Origins {
    api: String,
    live: String,
    passport: String,
}

pub struct BiliClient {
    client: Client,
    profile: ProtocolProfile,
    cookies: Mutex<CookieJar>,
    origins: Origins,
    buvid: String,
}

impl BiliClient {
    /// Native device identity is protocol metadata, separate from credentials.
    /// JOIN may name this value or the refreshed Cookie `buvid3` value.
    pub fn device_buvid(&self) -> &str {
        &self.buvid
    }

    pub fn new(cookie_header: &str) -> BiliResult<Self> {
        Self::with_options(cookie_header, BiliOptions::default())
    }

    pub fn with_options(cookie_header: &str, options: BiliOptions) -> BiliResult<Self> {
        Self::with_profile(cookie_header, options, ProtocolProfile::default())
    }

    pub fn with_profile(
        cookie_header: &str,
        options: BiliOptions,
        profile: ProtocolProfile,
    ) -> BiliResult<Self> {
        // Standalone clients share one process identity. Application frontends
        // supply the profile's persisted identity through the constructor below.
        static DEVICE_BUVID: OnceLock<String> = OnceLock::new();
        Self::with_profile_and_buvid(
            cookie_header,
            options,
            profile,
            DEVICE_BUVID.get_or_init(generated_buvid),
        )
    }

    pub fn with_profile_and_buvid(
        cookie_header: &str,
        options: BiliOptions,
        profile: ProtocolProfile,
        native_buvid: &str,
    ) -> BiliResult<Self> {
        if native_buvid.is_empty()
            || native_buvid.len() > 128
            || !native_buvid
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err("Invalid native device identity".into());
        }
        let cookies = CookieJar::parse(cookie_header)?;
        let mut builder = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(options.timeout_secs.clamp(1, 120)))
            .redirect(reqwest::redirect::Policy::none());
        if !options.use_system_proxy {
            builder = builder.no_proxy();
        }
        if let Some(proxy) = options.proxy.as_deref().filter(|p| !p.is_empty()) {
            if !(proxy.starts_with("http://") || proxy.starts_with("https://")) {
                return Err("Custom proxy must use http:// or https://".into());
            }
            builder = builder.proxy(reqwest::Proxy::all(proxy).map_err(|_| "Invalid proxy URL")?);
        }
        let client = builder
            .build()
            .map_err(|_| "Could not initialize the HTTPS client")?;
        Ok(Self {
            client,
            profile,
            cookies: Mutex::new(cookies),
            buvid: native_buvid.to_owned(),
            origins: Origins {
                api: "https://api.bilibili.com".into(),
                live: "https://api.live.bilibili.com".into(),
                passport: "https://passport.bilibili.com".into(),
            },
        })
    }

    pub fn call(&self, operation: &str, args: &Value) -> BiliResult<Value> {
        if !args.is_object() && !args.is_null() {
            return Err("Arguments must be a JSON object".into());
        }
        match operation {
            "auth.qr.create" => self.login_generate(),
            "auth.qr.poll" => self.login_poll(required_text(args, "qrcode_key")?),
            "auth.validate" => self.validate_account(),
            "auth.refresh" => self.refresh_ticket(),
            "room.get" => self.room_get(),
            "room.update" => self.room_update(args),
            "room.cover" => self.upload_cover(required_text(args, "path")?),
            "room.cover.preview" => self.cover_preview(),
            "room.audit" => self.room_audit(args),
            "room.precheck" => {
                let mut fields = self.signed(Fields::new(), false);
                fields.remove("access_key");
                resign(&mut fields, false, &self.profile);
                self.get_live("/xlive/app-blink/v1/index/GetRoomPreLiveStatus", &fields)
            }
            "area.list" => self.get_live(
                "/xlive/app-blink/v1/preLive/GetAreaListForLive",
                &self.signed(Fields::new(), false),
            ),
            "area.recent" => self.get_live(
                "/room/v1/Area/getMyChooseArea",
                &self.signed(fields([("roomid", self.room_id(args)?)]), false),
            ),
            "delay.get" => self.delay_get(args),
            "delay.set" => self.delay_set(args),
            "live.start" => self.start_live(args),
            "live.stop" => self.stop_live(args),
            "live.stream" => self.fetch_stream(args),
            "face.register" => self.face_register(args),
            "face.poll" => self.face_poll(args),
            "face.report" => self.face_report(args),
            "pk.status" => self.pk_status(args),
            "pk.search" => self.pk_search(args),
            "pk.match" => self.pk_match(args, false),
            "pk.cancel" => self.pk_match(args, true),
            "pk.invite" => self.pk_invite(args),
            "pk.accept" => self.pk_handle(args, true),
            "pk.reject" => self.pk_handle(args, false),
            "pk.end" => self.pk_request("exit", fields([("stop_pk", "1".into())]), Fields::new()),
            "pk.settings.get" => Ok(PkSettings::parse(&self.pk_request(
                "user_settings",
                Fields::new(),
                Fields::new(),
            )?)?
            .value()),
            "pk.settings.update" => self.pk_settings_update(args),
            "pk.continuous.get" => parse_continuous(self.pk_request(
                "continuous_status",
                Fields::new(),
                Fields::new(),
            )?),
            "pk.continuous.set" => self.pk_continuous_set(args),
            "pk.invitation.cancel" => self.pk_cancel_invitation(args, false),
            "pk.link.panel" => self.pk_link_panel(),
            "pk.link.list" => self.pk_request("interact_list", Fields::new(), Fields::new()),
            "pk.link.connect" => self.pk_link_connect(args),
            "pk.link.cancel" => self.pk_cancel_invitation(args, true),
            "pk.link.start" => self.pk_link_start(args),
            "pk.link.disconnect" => self.pk_link_disconnect(args),
            "pk.request" => self.pk_request(
                required_text(args, "operation")?,
                json_fields(args.get("fields"))?,
                json_fields(args.get("after_sign"))?,
            ),
            "danmaku.send" => self.send_danmaku(args),
            _ => Err(format!("Unknown Bilibili operation: {operation}")),
        }
    }

    /// Bootstrap the native TCP business-event connection. The returned login
    /// contains an ephemeral token and belongs only to the socket worker.
    pub fn danmaku_connection(&self) -> BiliResult<Value> {
        self.authenticated()?;
        let uid = self
            .uid()?
            .parse::<u64>()
            .map_err(|_| "Invalid account UID")?;
        let room = self
            .room_id(&Value::Null)?
            .parse::<u64>()
            .map_err(|_| "Invalid live room ID")?;
        let mut query = self.signed(
            fields([
                ("room_id", room.to_string()),
                ("user_type", "0".into()),
                ("is_anchor", "true".into()),
            ]),
            true,
        );
        query.remove("build");
        resign(&mut query, true, &self.profile);
        // Bootstrap retains the signer's insertion order (signature last),
        // unlike the PK transport's fully sorted final form.
        let signature = query.remove("sign").ok_or("Missing bootstrap signature")?;
        let path = format!(
            "/xlive/app-room/v1/index/getDanmuInfo?{}&sign={signature}",
            encode_fields(&query, true)
        );
        let data = self.data(
            self.send_raw(self.prepare(
                "GET",
                &self.origins.live,
                &path,
                &Fields::new(),
                None,
                false,
                true,
                true,
            )?)?
            .0,
        )?;
        let token = required_text(&data, "token")?;
        let endpoint = data
            .get("ip_list")
            .and_then(Value::as_array)
            .and_then(|list| {
                list.iter().find_map(|row| {
                    let host = row.get("host")?.as_str()?;
                    let port = row.get("port")?.as_u64()?;
                    if host.is_empty()
                        || !host
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b".-:".contains(&b))
                        || !(1..=65535).contains(&port)
                    {
                        return None;
                    }
                    Some((host.to_owned(), port))
                })
            });
        let (host, port) =
            endpoint.unwrap_or_else(|| ("broadcastlv.chat.bilibili.com".into(), 2243));
        let address = if host.contains(':') {
            format!("tcp://[{host}]:{port}")
        } else {
            format!("tcp://{host}:{port}")
        };
        let buvid = self.jar()?.get("buvid3").unwrap_or("").to_owned();
        let login = json!({"roomid":room,"uid":uid,"key":token,"protover":3,"app_id":102,"build":self.profile.build_number(),"buvid":buvid,"mobi_app":"pc_link","platform":"pc_link","channel":"bili","version_name":self.profile.version(),"device":"Windows Version: 10.0 x86_64","osver":"Windows Version: 10.0 Build 26100 (release: unavailable; revision: 0; 64-bit)","support_ack":true,"queue_uuid":random_uuid()});
        Ok(json!({"host":host,"port":port,"endpoint":address,"login":login}))
    }

    /// HTTP sequence ACK is independent of the operation-24 socket ACK.
    pub fn ack_sequences(&self, sequences: &[u32]) -> BiliResult<()> {
        self.authenticated()?;
        let body = sequence_ack_form(sequences, &self.csrf()?, now(), &self.profile)?;
        let request = self
            .prepare(
                "POST",
                &self.origins.live,
                "/xlive/open-interface/v1/dm/message_ack",
                &Fields::new(),
                None,
                false,
                true,
                true,
            )?
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body);
        self.data(self.send_raw(request)?.0)?;
        Ok(())
    }

    fn jar(&self) -> BiliResult<CookieJar> {
        self.cookies
            .lock()
            .map(|c| c.clone())
            .map_err(|_| "Credential state lock failed".into())
    }

    fn csrf(&self) -> BiliResult<String> {
        Ok(self.jar()?.csrf()?.to_owned())
    }
    fn uid(&self) -> BiliResult<String> {
        Ok(self.jar()?.uid()?.to_owned())
    }
    fn authenticated(&self) -> BiliResult<()> {
        self.jar()?.validate()
    }

    fn signed(&self, values: Fields, native: bool) -> Fields {
        sign_fields(values, now(), native, &self.profile)
    }

    fn signed_live_body(&self, values: Fields, csrf_in_signature: bool) -> BiliResult<Fields> {
        if csrf_in_signature {
            Ok(self.signed(self.with_csrf(values)?, false))
        } else {
            self.with_csrf(self.signed(values, false))
        }
    }

    fn with_csrf(&self, mut values: Fields) -> BiliResult<Fields> {
        let csrf = self.csrf()?;
        values.insert("csrf".into(), csrf.clone());
        values.insert("csrf_token".into(), csrf);
        Ok(values)
    }

    fn prepare(
        &self,
        method: &str,
        origin: &str,
        path: &str,
        query: &Fields,
        body: Option<&Fields>,
        web: bool,
        authenticated: bool,
        native: bool,
    ) -> BiliResult<RequestBuilder> {
        // Query bytes stay canonical; form and native PK encodings differ.
        let url = if query.is_empty() {
            format!("{origin}{path}")
        } else {
            format!("{origin}{path}?{}", encode_fields(query, native))
        };
        let url = reqwest::Url::parse(&url).map_err(|_| "Invalid API path")?;
        let mut req = match method {
            "GET" => self.client.get(url),
            "POST" => self.client.post(url),
            _ => return Err("Unsupported HTTP method".into()),
        };
        req = req
            .header("Accept", "*/*")
            .headers(self.profile.headers(web).clone());
        // StartLive attaches public device metadata even to anonymous QR
        // sessions. Account cookies must never leak into those requests.
        let mut cookies = if authenticated {
            self.jar()?
        } else {
            CookieJar::default()
        };
        cookies.insert("appkey", self.profile.app_key())?;
        cookies.insert("device_name", "bilibilive")?;
        cookies.insert("device_platform", "Windows Version: 10.0 x86_64")?;
        if !cookies.get("buvid3").is_some_and(|value| !value.is_empty()) {
            cookies.insert("buvid3", &self.buvid)?;
        }
        req = req
            .header(COOKIE, cookies.header())
            .header("buvid", &self.buvid);
        if let Some(body) = body {
            req = req
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(encode_fields(body, native));
        }
        Ok(req)
    }

    fn send_raw(&self, req: RequestBuilder) -> BiliResult<(Value, CookieJar)> {
        let response = req.send().map_err(|e| {
            if e.is_timeout() {
                "Bilibili request timed out"
            } else if e.is_connect() {
                "Could not connect to Bilibili (check network/proxy)"
            } else {
                "Bilibili HTTPS request failed"
            }
        })?;
        self.read_response(response)
    }

    fn read_response(&self, response: Response) -> BiliResult<(Value, CookieJar)> {
        let status = response.status();
        if !status.is_success() {
            return Err(format!("Bilibili returned HTTP {}", status.as_u16()));
        }
        let mut cookies = CookieJar::default();
        for header in response.headers().get_all(SET_COOKIE) {
            cookies.add_set_cookie(
                header
                    .to_str()
                    .map_err(|_| "Invalid response Cookie encoding")?,
            )?;
        }
        let bytes = response_bytes(response)?;
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| "Bilibili returned invalid JSON")?;
        if !value.is_object() {
            return Err("Bilibili returned a non-object response".into());
        }
        Ok((value, cookies))
    }

    fn data(&self, response: Value) -> BiliResult<Value> {
        let code = response
            .get("code")
            .and_then(Value::as_i64)
            .ok_or("Bilibili response is missing a numeric code")?;
        if code == 0 {
            return Ok(response.get("data").cloned().unwrap_or(Value::Null));
        }
        let message = response
            .get("message")
            .or_else(|| response.get("msg"))
            .and_then(Value::as_str)
            .unwrap_or("Request rejected");
        let safe = self.jar()?.redact_text(message);
        Err(format!(
            "Bilibili error {code}: {}",
            safe.chars().take(500).collect::<String>()
        ))
    }

    fn get_live(&self, path: &str, query: &Fields) -> BiliResult<Value> {
        self.data(
            self.send_raw(self.prepare(
                "GET",
                &self.origins.live,
                path,
                query,
                None,
                false,
                true,
                false,
            )?)?
            .0,
        )
    }

    fn post_live(&self, path: &str, query: &Fields, body: &Fields) -> BiliResult<Value> {
        self.data(
            self.send_raw(self.prepare(
                "POST",
                &self.origins.live,
                path,
                query,
                Some(body),
                false,
                true,
                false,
            )?)?
            .0,
        )
    }

    pub fn login_generate(&self) -> BiliResult<Value> {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let query = fields([
            ("source", "live_pc".into()),
            (
                "go_url",
                format!(
                    "https://live.bilibili.com/p/html/live-pc-blink/mini-login-v2/?livehime_create_ts={millis}&livehime_ts={}",
                    millis / 1000
                ),
            ),
            ("web_location", "0.0".into()),
        ]);
        self.data(
            self.send_raw(self.prepare(
                "GET",
                &self.origins.passport,
                "/x/passport-login/web/qrcode/generate",
                &query,
                None,
                true,
                false,
                false,
            )?)?
            .0,
        )
    }

    /// `_private_cookie` is an internal handoff to the credential store and
    /// MUST be removed by every public dispatcher before rendering a result.
    pub fn login_poll(&self, key: &str) -> BiliResult<Value> {
        if key.is_empty() || key.len() > 512 {
            return Err("Invalid QR challenge key".into());
        }
        let query = fields([
            ("qrcode_key", key.into()),
            ("source", "live_pc".into()),
            ("web_location", "0.0".into()),
        ]);
        let (response, mut cookies) = self.send_raw(self.prepare(
            "GET",
            &self.origins.passport,
            "/x/passport-login/web/qrcode/poll",
            &query,
            None,
            true,
            false,
            false,
        )?)?;
        let data = self.data(response)?;
        let code = data
            .get("code")
            .and_then(Value::as_i64)
            .ok_or("QR response is missing status")?;
        let status = match code {
            0 => "authenticated",
            86101 => "waiting_scan",
            86090 => "waiting_confirmation",
            86038 => "expired",
            _ => "failed",
        };
        let mut result = json!({"code": code, "status": status, "message": data.get("message").and_then(Value::as_str).unwrap_or(status)});
        if code == 0 {
            // Some passport deployments return credentials in the redirect URL
            // as well as Set-Cookie. Parse only the known cookie keys, and never
            // navigate to the redirect or expose its query to logs.
            if let Some(url) = data
                .get("url")
                .and_then(Value::as_str)
                .and_then(|s| reqwest::Url::parse(s).ok())
            {
                for (name, value) in url.query_pairs() {
                    if [
                        "SESSDATA",
                        "bili_jct",
                        "DedeUserID",
                        "DedeUserID__ckMd5",
                        "sid",
                    ]
                    .contains(&name.as_ref())
                        && cookies.get(&name).is_none()
                    {
                        cookies.insert(&name, &value)?;
                    }
                }
            }
            cookies.validate()?;
            result["uid"] = json!(cookies.uid()?);
            result["_private_cookie"] = json!(cookies.header());
        }
        Ok(result)
    }

    fn validate_account(&self) -> BiliResult<Value> {
        self.authenticated()?;
        let data = self.data(
            self.send_raw(self.prepare(
                "GET",
                &self.origins.api,
                "/x/web-interface/nav",
                &Fields::new(),
                None,
                true,
                true,
                false,
            )?)?
            .0,
        )?;
        if data.get("isLogin").and_then(Value::as_bool) != Some(true) {
            return Err("Bilibili login has expired".into());
        }
        let mid =
            number_text(data.get("mid")).ok_or("Bilibili account response has no valid UID")?;
        if mid != self.uid()? {
            return Err("Bilibili account identity does not match stored cookies".into());
        }
        Ok(
            json!({"uid": mid, "name": data.get("uname"), "avatar": data.get("face"), "authenticated": true}),
        )
    }

    fn refresh_ticket(&self) -> BiliResult<Value> {
        self.authenticated()?;
        let mut jar = self.jar()?;
        if jar
            .get("bili_ticket_expires")
            .and_then(|x| x.parse::<u64>().ok())
            .unwrap_or(0)
            <= now()
        {
            let timestamp = now();
            let query = fields([
                ("key_id", "ec02".into()),
                (
                    "hexsign",
                    hex(&hmac_sha256(
                        b"XgwSnGZ1p",
                        format!("ts{timestamp}").as_bytes(),
                    )),
                ),
                ("context[ts]", timestamp.to_string()),
                ("csrf", self.csrf()?),
            ]);
            let data = self.data(
                self.send_raw(self.prepare(
                    "POST",
                    &self.origins.api,
                    "/bapis/bilibili.api.ticket.v1.Ticket/GenWebTicket",
                    &query,
                    Some(&Fields::new()),
                    true,
                    true,
                    false,
                )?)?
                .0,
            )?;
            jar.insert("bili_ticket", required_text(&data, "ticket")?)?;
            let created = data
                .get("created_at")
                .and_then(Value::as_u64)
                .ok_or("Ticket response has no creation time")?;
            let ttl = data
                .get("ttl")
                .and_then(Value::as_u64)
                .ok_or("Ticket response has no TTL")?;
            jar.insert(
                "bili_ticket_expires",
                &created.saturating_add(ttl).to_string(),
            )?;
        }
        if ["buvid3", "buvid4"]
            .iter()
            .any(|name| jar.get(name).is_none_or(|value| value.is_empty()))
        {
            let data = self.data(
                self.send_raw(self.prepare(
                    "GET",
                    &self.origins.api,
                    "/x/frontend/finger/spi",
                    &Fields::new(),
                    None,
                    true,
                    true,
                    false,
                )?)?
                .0,
            )?;
            jar.insert("buvid3", required_text(&data, "b_3")?)?;
            jar.insert(
                "buvid4",
                &percent_encode(required_text(&data, "b_4")?, false),
            )?;
        }
        *self
            .cookies
            .lock()
            .map_err(|_| "Credential state lock failed")? = jar.clone();
        Ok(json!({"refreshed": true, "_private_cookie": jar.header()}))
    }

    fn room_info(&self) -> BiliResult<Value> {
        self.authenticated()?;
        self.get_live(
            "/xlive/app-blink/v1/room/GetInfo",
            &self.signed(fields([("uId", self.uid()?)]), false),
        )
    }

    fn room_id(&self, args: &Value) -> BiliResult<String> {
        if let Some(room) = args.get("room_id") {
            return positive_id(room, "room_id");
        }
        let data = self.room_info()?;
        positive_id(data.get("room_id").unwrap_or(&Value::Null), "room_id")
    }

    fn pre_live(&self) -> BiliResult<Value> {
        self.get_live(
            "/xlive/app-blink/v1/preLive/PreLive",
            &self.signed(
                fields([
                    ("area", "true".into()),
                    ("cover", "true".into()),
                    ("coverVertical", "true".into()),
                    ("liveDirectionType", "0".into()),
                    ("mobi_app", "pc_link".into()),
                    ("schedule", "true".into()),
                    ("title", "true".into()),
                ]),
                false,
            ),
        )
    }

    fn room_get(&self) -> BiliResult<Value> {
        let room = self.room_info()?;
        let pre = self.pre_live()?;
        let mut output = room.clone();
        if !output.is_object() {
            return Err("Room information is malformed".into());
        }
        for key in [
            "title",
            "cover",
            "coverVertical",
            "schedule",
            "liveDirectionType",
        ] {
            if let Some(value) = pre.get(key) {
                output[key] = value.clone();
            }
        }
        if let Some(cover) = pre.get("cover").filter(|cover| cover.is_object()) {
            output["cover_url"] = cover.get("url").cloned().unwrap_or(Value::Null);
            output["cover_status"] = cover.get("auditStatus").cloned().unwrap_or(Value::Null);
            output["cover_audit_reason"] = cover.get("auditReason").cloned().unwrap_or(Value::Null);
        }
        output["area_id"] = room.get("area_v2_id").cloned().unwrap_or(Value::Null);
        output["area_name"] = room.get("area_v2_name").cloned().unwrap_or(Value::Null);
        let mut warnings = Vec::new();
        match self.get_live(
            "/xlive/app-blink/v1/room/AnnounceInfo",
            &self.signed(Fields::new(), false),
        ) {
            Ok(value) => {
                output["announcement"] = value
                    .pointer("/announces/1/content")
                    .cloned()
                    .unwrap_or(json!(""))
            }
            Err(error) => warnings.push(error),
        }
        match self.delay_get(&json!({"room_id": room.get("room_id")})) {
            Ok(value) => output["delay"] = value,
            Err(error) => warnings.push(error),
        }
        output["warnings"] = json!(warnings);
        Ok(output)
    }

    fn room_update(&self, args: &Value) -> BiliResult<Value> {
        self.authenticated()?;
        let room_id = self.room_id(args)?;
        if let Some(title) = args.get("title") {
            let title = title.as_str().ok_or("title must be text")?;
            if title.trim().is_empty() || title.chars().count() > 60 {
                return Err("title must contain 1–60 characters".into());
            }
        }
        let area = args
            .get("area_id")
            .map(|v| positive_id(v, "area_id"))
            .transpose()?;
        if let Some(announcement) = args.get("announcement") {
            if !announcement.is_string()
                || announcement.as_str().unwrap_or_default().chars().count() > 2000
            {
                return Err("announcement must contain at most 2000 characters".into());
            }
        }
        let mut applied = Vec::new();
        let result = (|| {
            let mut results = serde_json::Map::new();
            if let Some(title) = args.get("title").and_then(Value::as_str) {
                let body = self.with_csrf(fields([
                    ("mobi_app", "pc_link".into()),
                    ("room_id", room_id.clone()),
                    ("title", title.into()),
                ]))?;
                let value = self.post_live(
                    "/xlive/app-blink/v1/preLive/UpdatePreLiveInfo",
                    &self.signed(Fields::new(), false),
                    &body,
                )?;
                applied.push("title");
                results.insert("title".into(), value);
            }
            if let Some(area_id) = area {
                let body = self.with_csrf(fields([
                    ("area_id", area_id),
                    ("build", self.profile.build().into()),
                    ("platform", "pc_link".into()),
                    ("room_id", room_id.clone()),
                ]))?;
                let value = self.post_live(
                    "/xlive/app-blink/v2/room/AnchorChangeRoomArea",
                    &self.signed(Fields::new(), false),
                    &body,
                )?;
                applied.push("area");
                results.insert("area".into(), value);
            }
            if let Some(announcement) = args.get("announcement").and_then(Value::as_str) {
                let mut body = self.with_csrf(self.signed(Fields::new(), false))?;
                body.insert("content".into(), announcement.into());
                body.insert("type".into(), "1".into());
                let value = self.post_live(
                    "/xlive/app-blink/v1/room/AnnounceCommit",
                    &Fields::new(),
                    &body,
                )?;
                applied.push("announcement");
                results.insert("announcement".into(), value);
            }
            if results.is_empty() {
                return Err("Specify title, area_id or announcement".into());
            }
            Ok(json!({"room_id": room_id, "updated": applied, "results": results}))
        })();
        result.map_err(|e: String| {
            if applied.is_empty() {
                e
            } else {
                format!("{e}; already applied: {}", applied.join(", "))
            }
        })
    }

    fn upload_cover(&self, path: &str) -> BiliResult<Value> {
        self.authenticated()?;
        let path = Path::new(path);
        let metadata = path
            .metadata()
            .map_err(|_| "Could not read the selected cover image")?;
        if !metadata.is_file() || metadata.len() > 10 * 1024 * 1024 {
            return Err("Cover must be a regular image file no larger than 10 MiB".into());
        }
        let bytes = std::fs::read(path).map_err(|_| "Could not read cover bytes")?;
        let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            "image/jpeg"
        } else {
            return Err("Cover must be a PNG or JPEG image".into());
        };
        let form = multipart::Form::new()
            .text("bucket", "live")
            .text("dir", "new_room_cover")
            .part(
                "file",
                multipart::Part::bytes(bytes)
                    .file_name("cover")
                    .mime_str(mime)
                    .map_err(|_| "Invalid cover media type")?,
            );
        let request = self
            .prepare(
                "POST",
                &self.origins.api,
                "/x/upload/web/image",
                &fields([("csrf", self.csrf()?)]),
                None,
                true,
                true,
                false,
            )?
            .multipart(form);
        let data = self.data(self.send_raw(request)?.0)?;
        let location = required_text(&data, "location")?;
        let url =
            reqwest::Url::parse(location).map_err(|_| "Upload returned an invalid image URL")?;
        if !["http", "https"].contains(&url.scheme()) {
            return Err("Upload returned an unsupported image URL".into());
        }
        let body = self.with_csrf(fields([
            ("platform", "pc_link".into()),
            ("mobi_app", "pc_link".into()),
            ("build", "1".into()),
            ("cover", location.into()),
            ("coverVertical", "".into()),
            ("liveDirectionType", "1".into()),
            ("visit_id", "".into()),
        ]))?;
        let applied = self.post_live(
            "/xlive/app-blink/v1/preLive/UpdatePreLiveInfo",
            &Fields::new(),
            &body,
        )?;
        Ok(json!({"cover_url": location, "audit": applied}))
    }

    fn cover_preview(&self) -> BiliResult<Value> {
        self.authenticated()?;
        // The URL is obtained from the account's real PreLive response. This
        // operation deliberately accepts no caller-supplied URL.
        let pre = self.pre_live()?;
        let source = pre
            .pointer("/cover/url")
            .and_then(Value::as_str)
            .or_else(|| pre["cover_url"].as_str())
            .ok_or("The current room has no cover URL")?;
        let url = verified_cover_url(source)?;
        // Use the configured proxy, timeout and TLS client, without forwarding
        // account cookies or API credentials to an image CDN.
        let response = self
            .client
            .get(url.clone())
            .headers(self.profile.headers(true).clone())
            .header("Referer", "https://live.bilibili.com/")
            .send()
            .map_err(|_| "Could not fetch the current Bilibili cover")?;
        if !response.status().is_success() {
            return Err(format!(
                "Cover host returned HTTP {}",
                response.status().as_u16()
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE)
        {
            return Err("Current cover exceeds 8 MiB".into());
        }
        let bytes = response_bytes(response)?;
        let mime = cover_mime(&bytes)?;
        Ok(
            json!({"url":url.as_str(),"mime":mime,"base64":BASE64.encode(&bytes),"bytes":bytes.len(),"audit_status":pre.pointer("/cover/auditStatus"),"audit_reason":pre.pointer("/cover/auditReason")}),
        )
    }

    fn room_audit(&self, args: &Value) -> BiliResult<Value> {
        let mut query = self.signed(self.with_csrf(Fields::new())?, false);
        query.insert("content_type".into(), "1".into());
        query.insert("room_id".into(), self.room_id(args)?);
        let mut audit =
            self.get_live("/xlive/app-blink/v1/room/GetRoomContentAuditInfo", &query)?;
        if !audit.is_object() {
            audit = json!({"title_audit_info":audit});
        }
        let pre = self.pre_live()?;
        audit["cover"] = pre.get("cover").cloned().unwrap_or(Value::Null);
        Ok(audit)
    }

    fn delay_get(&self, args: &Value) -> BiliResult<Value> {
        let query = self.signed(
            self.with_csrf(fields([("room_id", self.room_id(args)?)]))?,
            false,
        );
        self.get_live(
            "/xlive/app-blink/v1/upStreamConfig/GetAnchorSelfStreamTimeShift",
            &query,
        )
    }

    fn delay_set(&self, args: &Value) -> BiliResult<Value> {
        let raw = args
            .get("seconds")
            .or_else(|| args.get("time_shift"))
            .ok_or("seconds is required")?;
        let seconds: i64 = number_text(Some(raw))
            .and_then(|s| s.parse().ok())
            .ok_or("seconds must be an integer")?;
        let room_id = self.room_id(args)?;
        let limits = self.delay_get(&json!({"room_id":room_id}))?;
        let min = limits
            .get("min_time_shift")
            .and_then(Value::as_i64)
            .ok_or("Platform omitted minimum delay")?;
        let max = limits
            .get("max_time_shift")
            .and_then(Value::as_i64)
            .ok_or("Platform omitted maximum delay")?;
        if !(min..=max).contains(&seconds) {
            return Err(format!("Delay must be between {min} and {max} seconds"));
        }
        let body = self.signed(
            self.with_csrf(fields([
                ("room_id", room_id),
                ("time_shift", seconds.to_string()),
            ]))?,
            false,
        );
        self.post_live(
            "/xlive/app-blink/v1/upStreamConfig/SetAnchorSelfStreamTimeShift",
            &Fields::new(),
            &body,
        )
    }

    fn start_live(&self, args: &Value) -> BiliResult<Value> {
        self.authenticated()?;
        if let Some(expected) = args.get("expected_uid") {
            if positive_id(expected, "expected_uid")? != self.uid()? {
                return Err("Account changed during face verification; start again with the selected account".into());
            }
        }
        let room_id = self.room_id(args)?;
        let area = if let Some(value) = args.get("area_id") {
            positive_id(value, "area_id")?
        } else {
            positive_id(
                self.room_info()?.get("area_v2_id").unwrap_or(&Value::Null),
                "area_id",
            )?
        };
        // Validate the protocol policy before performing the mutation.
        protocol_policy(args)?;
        let body = self.signed_live_body(
            fields([
                ("area_v2", area.clone()),
                ("room_id", room_id.clone()),
                ("type", "2".into()),
            ]),
            self.profile.start_csrf(),
        )?;
        let (raw, _) = self.send_raw(self.prepare(
            "POST",
            &self.origins.live,
            "/room/v1/Room/startLive",
            &Fields::new(),
            Some(&body),
            false,
            true,
            false,
        )?)?;
        if let Some(kind @ (60024 | 60043)) = raw.get("code").and_then(Value::as_i64) {
            let message = required_text(&raw, "message")?;
            let mut verification = if kind == 60024 {
                let url = raw
                    .pointer("/data/qr")
                    .and_then(Value::as_str)
                    .filter(|url| !url.is_empty())
                    .ok_or("Bilibili omitted the face verification URL")?;
                json!({"type":kind,"url":url})
            } else {
                let voucher = raw
                    .pointer("/data/risk_extra/v_voucher")
                    .and_then(Value::as_str)
                    .filter(|voucher| !voucher.is_empty())
                    .ok_or("Bilibili omitted the face verification voucher")?;
                // V2 must register its voucher before the challenge is reported
                // and displayed, matching StartLive's popup_face_widget flow.
                self.face_register(&json!({"v_voucher":voucher}))
                    .map_err(|error| {
                        format!("Could not prepare Bilibili face verification: {error}")
                    })?
            };
            // Reporting initializes the platform's verification flow. Reporting
            // only after a successful scan leaves the phone on a system error
            // (StartLive issue #21). Preserve the original code and message.
            self.face_report(&json!({
                "type":kind,"message":message,"room_id":room_id,"area_id":area
            }))
            .map_err(|error| format!("Could not initialize Bilibili face verification: {error}"))?;
            let mut start_args = args.clone();
            start_args["room_id"] = json!(room_id);
            start_args["area_id"] = json!(area);
            start_args["expected_uid"] = json!(self.uid()?);
            verification["room_id"] = json!(room_id);
            verification["area_id"] = json!(area);
            verification["message"] = json!(message);
            verification["reported"] = json!(true);
            verification["start_args"] = start_args;
            return Ok(json!({
                "status":"verification_required","room_id":room_id,"verification":verification
            }));
        }
        let data = self.data(raw)?;
        let mut result = select_stream(&data, args).map_err(|error| {
            format!("The live room started, but its stream endpoint could not be read: {error}")
        })?;
        result["room_id"] = json!(room_id);
        if result.get("status").is_none() {
            result["status"] = json!("live");
        }
        result["change"] = data.get("change").cloned().unwrap_or(Value::Null);
        Ok(result)
    }

    fn stop_live(&self, args: &Value) -> BiliResult<Value> {
        self.authenticated()?;
        let room_id = self.room_id(args)?;
        let body = self.signed_live_body(
            fields([("room_id", room_id.clone())]),
            self.profile.stop_csrf(),
        )?;
        let data = self.post_live("/room/v1/Room/stopLive", &Fields::new(), &body)?;
        Ok(json!({"status":"stopped", "room_id": room_id, "result": data}))
    }

    fn fetch_stream(&self, args: &Value) -> BiliResult<Value> {
        self.authenticated()?;
        protocol_policy(args)?;
        let body = self.with_csrf(self.signed(fields([("backup_stream", "0".into())]), false))?;
        let data = self.post_live(
            "/xlive/app-blink/v1/live/FetchWebUpStreamAddr",
            &Fields::new(),
            &body,
        )?;
        if data.get("rtmp").is_some() || data.get("protocols").is_some() {
            return select_stream(&data, args);
        }
        if let Some(addr) = data.get("addr") {
            return select_stream(&json!({"rtmp":addr}), args);
        }
        Err("The platform did not return an existing stream endpoint; start the room explicitly first".into())
    }

    fn face_register(&self, args: &Value) -> BiliResult<Value> {
        let voucher = required_text(args, "v_voucher")?;
        let body = fields([
            ("v_voucher", voucher.into()),
            ("dm_track", "[]".into()),
            ("csrf", self.csrf()?),
        ]);
        let data = self.data(
            self.send_raw(self.prepare(
                "POST",
                &self.origins.api,
                "/x/gaia-vgate/v2/register",
                &Fields::new(),
                Some(&body),
                true,
                true,
                false,
            )?)?
            .0,
        )?;
        let decoded = decode_risk(voucher, required_text(&data, "content")?)?;
        if decoded.get("type").and_then(Value::as_str) != Some("realname") {
            return Err(
                "The platform requires an unsupported verification type; complete it in Bilibili"
                    .into(),
            );
        }
        let token = obfuscate_token(required_text(&decoded, "token")?)?;
        let url = format!(
            "https://www.bilibili.com/h5/risk-control/realname?t={}",
            percent_encode(&percent_encode(&token, true), true)
        );
        Ok(json!({"status":"verification_required", "type":60043,"url":url,"token":token}))
    }

    fn face_poll(&self, args: &Value) -> BiliResult<Value> {
        let kind = args.get("type").and_then(Value::as_i64).unwrap_or(60043);
        let data = if kind == 60024 {
            let body = self.with_csrf(fields([
                ("room_id", self.room_id(args)?),
                ("face_auth_code", "60024".into()),
                ("visit_id", "".into()),
            ]))?;
            self.post_live(
                "/xlive/app-blink/v1/preLive/IsUserIdentifiedByFaceAuth",
                &Fields::new(),
                &body,
            )?
        } else if kind == 60043 {
            let body = fields([
                ("token", required_text(args, "token")?.into()),
                ("dm_track", dm_track()),
                ("csrf", self.csrf()?),
            ]);
            self.data(
                self.send_raw(self.prepare(
                    "POST",
                    &self.origins.api,
                    "/x/gaia-vgate/v2/validatePreCheck",
                    &Fields::new(),
                    Some(&body),
                    true,
                    true,
                    false,
                )?)?
                .0,
            )?
        } else {
            return Err("Verification type must be 60024 or 60043".into());
        };
        let done = if kind == 60024 {
            data.get("is_identified")
                .is_some_and(|v| v.as_bool() == Some(true) || v.as_i64() == Some(1))
        } else {
            data.get("status").and_then(Value::as_i64) == Some(1)
        };
        let failed = kind == 60043
            && data
                .get("status")
                .and_then(Value::as_i64)
                .is_some_and(|v| v >= 2);
        Ok(
            json!({"status":if done {"verified"} else if failed {"expired_or_failed"} else {"pending"},"verified":done,"data":data}),
        )
    }

    fn face_report(&self, args: &Value) -> BiliResult<Value> {
        let mut body = self.with_csrf(self.signed(Fields::new(), false))?;
        body.extend(fields([
            (
                "area_v2_id",
                positive_id(args.get("area_id").unwrap_or(&Value::Null), "area_id")?,
            ),
            (
                "face_auth_code",
                number_text(args.get("type")).ok_or("Verification type is required")?,
            ),
            ("face_auth_message", required_text(args, "message")?.into()),
            ("room_id", self.room_id(args)?),
            ("scene", "startLive".into()),
        ]));
        self.post_live(
            "/xlive/app-blink/v1/preLive/ReportFaceRecognition",
            &Fields::new(),
            &body,
        )
    }

    fn pk_status(&self, args: &Value) -> BiliResult<Value> {
        let room = self.room_id(args)?;
        let pk_id = number_text(args.get("pk_id")).unwrap_or_else(|| "0".into());
        if args.get("modern").and_then(Value::as_bool).unwrap_or(false) {
            self.pk_request(
                "info",
                fields([("room_id", room), ("pk_id", pk_id)]),
                Fields::new(),
            )
        } else {
            self.pk_request(
                "classic_info",
                fields([
                    ("room_id", room),
                    ("pk_id", pk_id),
                    ("is_anchor", "1".into()),
                ]),
                Fields::new(),
            )
        }
    }

    fn pk_search(&self, args: &Value) -> BiliResult<Value> {
        let search = args
            .get("search")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| number_text(args.get("uid")))
            .ok_or("search or uid is required")?;
        self.pk_request(
            "search_candidates",
            fields([
                ("search", search),
                ("type", "1".into()),
                (
                    "page",
                    number_text(args.get("page")).unwrap_or_else(|| "1".into()),
                ),
                ("label", "invite_pk".into()),
            ]),
            Fields::new(),
        )
    }

    fn pk_match(&self, args: &Value, cancel: bool) -> BiliResult<Value> {
        let room = self.room_id(args)?;
        let mode = args
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("traditional");
        let battle = match mode {
            "traditional" | "classic" => "1",
            "video" => "2",
            "video_link" | "link" => "10000",
            _ => return Err("PK mode must be classic, video or video_link".into()),
        };
        let mut body = fields([("room_id", room.clone())]);
        if battle == "10000" {
            body.extend(fields([
                ("biz_type", "1".into()),
                ("connection_type", "1".into()),
            ]));
        } else {
            body.insert("battle_type".into(), battle.into());
        }
        let extra = fields([("client_traceid", random_hex(16))]);
        if cancel {
            body.insert("sub_type".into(), "0".into());
            body.insert("need_warn".into(), "0".into());
            return self.pk_request("cancel_match", body, extra);
        }
        let mut pre = fields([
            ("room_id", room),
            ("pk_version", battle.into()),
            ("mobi_app", "pc_link".into()),
        ]);
        if battle == "1" {
            pre.insert("uid".into(), self.uid()?);
        }
        let checked = self.pk_request("precheck", pre, extra.clone())?;
        check_pk_precheck(&checked)?;
        self.pk_request("match", body, extra)
    }

    fn pk_invite(&self, args: &Value) -> BiliResult<Value> {
        let peer = positive_id(args.get("uid").unwrap_or(&Value::Null), "uid")?;
        if peer == self.uid()? {
            return Err("Cannot invite the current account".into());
        }
        let search = self.pk_search(&json!({"uid":peer}))?;
        let candidate = search
            .get("users")
            .and_then(Value::as_array)
            .and_then(|users| {
                users
                    .iter()
                    .find(|u| number_text(u.pointer("/user/uid")).as_deref() == Some(peer.as_str()))
            })
            .ok_or("No matching PK candidate was returned by Bilibili")?;
        let label = candidate
            .get("label_type")
            .and_then(Value::as_str)
            .unwrap_or("");
        let room = self.room_id(args)?;
        let pre = self.pk_request(
            "precheck",
            fields([
                ("room_id", room),
                ("pk_version", "6".into()),
                ("mobi_app", "pc_link".into()),
            ]),
            fields([("client_traceid", random_hex(16))]),
        )?;
        check_pk_precheck(&pre)?;
        let mut body = fields([
            ("mutipk_type", "4".into()),
            ("invite_uid", peer),
            ("invited_type", "1".into()),
        ]);
        if !label.is_empty() {
            body.insert("invited_type_label".into(), label.into());
        }
        self.pk_request("start", body, Fields::new())
    }

    fn pk_handle(&self, args: &Value, accept: bool) -> BiliResult<Value> {
        let peer = positive_id(
            args.get("uid")
                .or_else(|| args.get("peer_uid"))
                .unwrap_or(&Value::Null),
            "peer_uid",
        )?;
        let source = number_text(args.get("source"))
            .ok_or("Invitation source from the received event is required")?;
        let session = args
            .get("session_id")
            .or_else(|| args.get("biz_session_id"))
            .and_then(Value::as_str)
            .ok_or("Invitation session_id is required")?;
        let body = fields([
            ("peer_uid", peer),
            ("action", if accept { "1" } else { "2" }.into()),
            ("action_type", "1".into()),
            ("source", source),
            ("biz_session_id", session.into()),
        ]);
        self.pk_request(
            "handle",
            body,
            fields([(
                "client_traceid",
                args.get("client_traceid")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
            )]),
        )
    }

    fn pk_settings_update(&self, args: &Value) -> BiliResult<Value> {
        let mut desired =
            PkSettings::parse(&self.pk_request("user_settings", Fields::new(), Fields::new())?)?;
        desired.change(
            required_text(args, "field")?,
            args.get("value").ok_or("value is required")?,
        )?;
        let written = self.pk_request("update_user_settings", desired.fields(), Fields::new())?;
        if !written.is_null() && !written.is_object() {
            return Err(
                "PK settings write returned invalid data; read current settings before retrying"
                    .into(),
            );
        }
        if written.get("msg").is_some_and(|value| !value.is_string()) {
            return Err("PK settings write returned a malformed message; read current settings before retrying".into());
        }
        let confirmed = PkSettings::parse(
            &self
                .pk_request("user_settings", Fields::new(), Fields::new())
                .map_err(|error| {
                    format!("PK settings were submitted but read-back failed: {error}")
                })?,
        )?;
        let verified = confirmed.preferences == desired.preferences;
        Ok(
            json!({"ok":verified,"status":if verified{"updated"}else{"server_mismatch"},"submitted":true,"verified":verified,"settings":confirmed.value()}),
        )
    }

    fn pk_continuous_set(&self, args: &Value) -> BiliResult<Value> {
        let enabled = args["enabled"]
            .as_bool()
            .ok_or("enabled must be a boolean")?;
        let kind = match args["mode"].as_str().unwrap_or("classic") {
            "classic" | "traditional" => "1",
            "video" => "2",
            _ => return Err("Continuous PK supports classic or video matching".into()),
        };
        let before = parse_continuous(self.pk_request(
            "continuous_status",
            Fields::new(),
            Fields::new(),
        )?)?;
        if before["button_disable"].as_bool() == Some(true) {
            return Err("The platform currently disables continuous PK changes".into());
        }
        self.pk_request(
            "continuous_update",
            fields([
                ("ruid", self.uid()?),
                ("status", if enabled { "1" } else { "0" }.into()),
                ("pk_version", kind.into()),
                (
                    "client_traceid",
                    args["client_traceid"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| random_hex(16)),
                ),
            ]),
            Fields::new(),
        )?;
        parse_continuous(
            self.pk_request("continuous_status", Fields::new(), Fields::new())
                .map_err(|error| {
                    format!("Continuous PK change was submitted but read-back failed: {error}")
                })?,
        )
    }

    fn pk_cancel_invitation(&self, args: &Value, link: bool) -> BiliResult<Value> {
        let peer = positive_id(args.get("uid").unwrap_or(&Value::Null), "uid")?;
        let body = fields([
            ("peer_uid", peer),
            ("action", "3".into()),
            ("action_type", if link { "0" } else { "1" }.into()),
            ("source", "0".into()),
            ("biz_session_id", String::new()),
        ]);
        let extra = args["client_traceid"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|trace| fields([("client_traceid", trace.into())]))
            .unwrap_or_default();
        self.pk_request("handle", body, extra)
    }

    fn pk_link_panel(&self) -> BiliResult<Value> {
        let layout = self.pk_request("layout_data", Fields::new(), Fields::new())?;
        let matches = self.pk_request(
            "match_list",
            Fields::new(),
            fields([("type", "0".into()), ("last_id", "0".into())]),
        )?;
        let selection = self.pk_request("get_match_select", Fields::new(), Fields::new())?;
        self.pk_request(
            "in_conn_panel",
            fields([("room_id", self.room_id(&Value::Null)?)]),
            Fields::new(),
        )?;
        let response = self.pk_request(
            "batch_recommend_list",
            fields([(
                "label_map",
                r#"{"conn_random":"","universal_multi_conn_v2":""}"#.into(),
            )]),
            Fields::new(),
        )?;
        let list = response["batch_data_map"]["universal_multi_conn_v2"]
            .get("users")
            .ok_or("Video-link panel has no candidate list")?;
        let users = if list.is_null() {
            Vec::new()
        } else {
            list.as_array()
                .ok_or("Video-link candidate list is malformed")?
                .clone()
        };
        let own = self.uid()?;
        let mut eligible = Vec::new();
        for candidate in users {
            let uid = positive_id(&candidate["user"]["uid"], "candidate UID")?;
            let label = candidate
                .get("label_type")
                .map(|v| {
                    v.as_str()
                        .ok_or("Video-link candidate has malformed label_type")
                })
                .transpose()?
                .unwrap_or("");
            let session = candidate
                .get("biz_session_id")
                .map(|v| {
                    v.as_str()
                        .ok_or("Video-link candidate has malformed session")
                })
                .transpose()?
                .unwrap_or("");
            let occupied = candidate
                .get("other_users_num")
                .map(|v| {
                    v.as_u64()
                        .ok_or("Video-link candidate has malformed occupancy")
                })
                .transpose()?
                .unwrap_or(0);
            if uid != own && !label.is_empty() && session.is_empty() && occupied == 0 {
                eligible.push(candidate);
            }
        }
        Ok(
            json!({"users":eligible,"page":1,"type":1,"next":false,"layout":layout,"match_list":matches,"selection":selection}),
        )
    }

    fn pk_link_connect(&self, args: &Value) -> BiliResult<Value> {
        let peer = positive_id(args.get("uid").unwrap_or(&Value::Null), "uid")?;
        let panel = self.pk_link_panel()?;
        let candidate = panel["users"]
            .as_array()
            .and_then(|list| {
                list.iter().find(|item| {
                    number_text(item.pointer("/user/uid")).as_deref() == Some(peer.as_str())
                })
            })
            .ok_or("This UID is not an eligible candidate in the current video-link panel")?;
        let label = required_text(candidate, "label_type")?;
        let value = self.pk_request(
            "connect",
            fields([
                ("business_label", "universal_multi_conn".into()),
                ("cancel_current", "0".into()),
                ("invited_type", "3".into()),
                ("invited_type_label", label.into()),
                ("peer_uid", peer.clone()),
            ]),
            Fields::new(),
        )?;
        let session = required_text(&value, "biz_session_id")?.to_owned();
        Ok(json!({"status":"invited","uid":peer,"session_id":session,"data":value}))
    }

    fn pk_link_start(&self, args: &Value) -> BiliResult<Value> {
        let session = required_text(args, "session_id")?;
        let result = self.pk_request(
            "start",
            fields([
                ("biz_session_id", session.into()),
                ("mutipk_type", "0".into()),
            ]),
            Fields::new(),
        )?;
        positive_id(&result["pk_basic"]["pk_id"], "started PK round")?;
        if result["pk_basic"]["biz_session_id"].as_str() != Some(session) {
            return Err("Start-PK response does not match the current video-link session".into());
        }
        Ok(result)
    }

    fn pk_link_disconnect(&self, args: &Value) -> BiliResult<Value> {
        let session = required_text(args, "session_id")?;
        let end_round = args
            .get("end_round")
            .map(|v| v.as_bool().ok_or("end_round must be boolean"))
            .transpose()?
            .unwrap_or(false);
        let ended = if end_round {
            Some(self.pk_request("exit", fields([("stop_pk", "1".into())]), Fields::new()))
        } else {
            None
        };
        // Quit is still required when Exit failed. Neither mutation is retried.
        let quit = self.pk_request(
            "quit",
            fields([("biz_session_id", session.into())]),
            Fields::new(),
        );
        let successful = ended.as_ref().is_none_or(|result| result.is_ok()) && quit.is_ok();
        let as_receipt = |result: BiliResult<Value>| match result {
            Ok(value) => json!({"ok":true,"result":value}),
            Err(error) => json!({"ok":false,"error":error}),
        };
        Ok(
            json!({"ok":successful,"status":if successful{"quit_requested"}else{"partial_failure"},"session_id":session,"end_round":ended.map(as_receipt),"quit":as_receipt(quit)}),
        )
    }

    fn pk_request(&self, operation: &str, values: Fields, mut extra: Fields) -> BiliResult<Value> {
        self.authenticated()?;
        let spec = PK_OPERATIONS
            .iter()
            .find(|spec| spec.name == operation)
            .ok_or("Unknown PK operation")?;
        let reserved = [
            "access_key",
            "appkey",
            "build",
            "platform",
            "ts",
            "version",
            "sign",
            "csrf",
            "csrf_token",
            "cookie",
            "authorization",
        ];
        if values.keys().any(|k| reserved.contains(&k.as_str())) {
            return Err("PK fields cannot override identity or signing fields".into());
        }
        if extra.keys().any(|k| !spec.post_sign.contains(&k.as_str()))
            || values.keys().any(|k| spec.post_sign.contains(&k.as_str()))
        {
            return Err("PK fields must respect the operation's post-sign field rules".into());
        }
        let own_uid = self.uid()?;
        for key in ["ruid", "anchor_uid"] {
            if values.get(key).is_some_and(|v| v != &own_uid) {
                return Err("PK identity does not match the current account".into());
            }
        }
        if operation == "precheck" && values.get("uid").is_some_and(|v| v != &own_uid) {
            return Err("PK precheck UID does not match the current account".into());
        }
        if let Some(room) = values.get("room_id") {
            let own_room = self.room_id(&Value::Null)?;
            if room != &own_room {
                return Err("PK room does not match the current account".into());
            }
        }
        if ["match", "cancel_match"].contains(&operation) {
            let modern = values.contains_key("biz_type") || values.contains_key("connection_type");
            if modern
                && (!values.contains_key("biz_type")
                    || !values.contains_key("connection_type")
                    || values.contains_key("battle_type"))
            {
                return Err("PK matching must use one complete mode branch".into());
            }
        }
        if spec.csrf {
            extra = self.with_csrf(extra)?;
        }
        let mut body = if spec.signed {
            self.signed(values, true)
        } else {
            base_fields(values, now(), &self.profile)
        };
        body.extend(extra);
        let (query, form) = if spec.method == "GET" {
            (body, None)
        } else {
            (Fields::new(), Some(body))
        };
        let (mut raw, _) = self.send_raw(self.prepare(
            spec.method,
            &self.origins.live,
            spec.path,
            &query,
            form.as_ref(),
            false,
            true,
            true,
        )?)?;
        if operation == "precheck"
            && let Some(code) = raw
                .get("code")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<i64>().ok())
        {
            raw["code"] = json!(code);
        }
        if operation == "classic_info" && raw.get("code").and_then(Value::as_i64) == Some(6204) {
            return Ok(json!({"code":6204,"status":"no_active_classic_pk","data":raw.get("data")}));
        }
        self.data(raw)
    }

    fn send_danmaku(&self, args: &Value) -> BiliResult<Value> {
        let message = required_text(args, "message")?;
        if message.chars().count() > 1000 {
            return Err("Chat message is too long".into());
        }
        let color = args
            .get("color")
            .map(|v| v.as_u64().ok_or("color must be an integer"))
            .transpose()?
            .unwrap_or(0xff_ffff);
        let mode = args
            .get("mode")
            .map(|v| v.as_u64().ok_or("mode must be an integer"))
            .transpose()?
            .unwrap_or(1);
        let size = args
            .get("fontsize")
            .map(|v| v.as_u64().ok_or("fontsize must be an integer"))
            .transpose()?
            .unwrap_or(25);
        if color > 0xff_ffff || ![1, 4, 5].contains(&mode) || !(1..=72).contains(&size) {
            return Err("Chat color must be 0..16777215, mode 1/4/5, and fontsize 1..72".into());
        }
        let body = self.with_csrf(fields([
            ("bubble", "0".into()),
            ("msg", message.into()),
            ("color", color.to_string()),
            ("mode", mode.to_string()),
            ("fontsize", size.to_string()),
            ("rnd", now().to_string()),
            ("roomid", self.room_id(args)?),
        ]))?;
        self.data(
            self.send_raw(self.prepare(
                "POST",
                &self.origins.live,
                "/msg/send",
                &Fields::new(),
                Some(&body),
                true,
                true,
                false,
            )?)?
            .0,
        )
    }
}

#[derive(Clone, Copy)]
struct PkOperation {
    name: &'static str,
    method: &'static str,
    path: &'static str,
    csrf: bool,
    signed: bool,
    post_sign: &'static [&'static str],
}

macro_rules! pk {
    ($name:literal,$method:literal,$path:literal,$csrf:literal,$signed:literal,[$($extra:literal),*]) => { PkOperation { name:$name, method:$method, path:$path, csrf:$csrf, signed:$signed, post_sign:&[$($extra),*] } };
}

const PK_OPERATIONS: &[PkOperation] = &[
    pk!(
        "get_match_select",
        "GET",
        "/xlive/app-blink/v2/universalInteract/GetMatchSelect",
        true,
        true,
        []
    ),
    pk!(
        "in_conn_panel",
        "GET",
        "/xlive/app-blink/v1/universalInteract/InConnPanel",
        true,
        true,
        []
    ),
    pk!(
        "batch_recommend_list",
        "GET",
        "/xlive/app-blink/v2/universalInteract/BatchRecommendList",
        true,
        true,
        []
    ),
    pk!(
        "connect",
        "POST",
        "/xlive/app-blink/v2/universalInteract/Connect",
        true,
        true,
        []
    ),
    pk!(
        "interact_list",
        "GET",
        "/xlive/app-blink/v2/universalInteract/InteractList",
        true,
        true,
        []
    ),
    pk!(
        "layout_data",
        "GET",
        "/xlive/app-blink/v2/universalInteract/LayoutData",
        true,
        true,
        []
    ),
    pk!(
        "match_list",
        "POST",
        "/xlive/app-blink/v2/universalInteract/MatchList",
        true,
        true,
        ["type", "last_id"]
    ),
    pk!(
        "mute",
        "POST",
        "/xlive/app-blink/v1/universalInteract/Mute",
        true,
        true,
        []
    ),
    pk!(
        "user_settings",
        "GET",
        "/xlive/app-blink/v2/universalInteract/Config",
        true,
        true,
        []
    ),
    pk!(
        "update_user_settings",
        "POST",
        "/xlive/app-blink/v2/universalInteract/ConfigModify",
        true,
        true,
        []
    ),
    pk!(
        "search_candidates",
        "GET",
        "/xlive/app-blink/v2/universalInteract/Search",
        true,
        true,
        []
    ),
    pk!(
        "precheck",
        "GET",
        "/xlive/general-interface/v1/battle/pkPreCheck",
        false,
        true,
        ["client_traceid"]
    ),
    pk!(
        "match",
        "GET",
        "/xlive/general-interface/v1/battle/join",
        false,
        true,
        ["client_traceid", "theme_id"]
    ),
    pk!(
        "cancel_match",
        "GET",
        "/xlive/general-interface/v1/battle/cancel",
        false,
        true,
        ["client_traceid"]
    ),
    pk!(
        "handle",
        "POST",
        "/xlive/app-blink/v1/universalInteract/Handle",
        true,
        true,
        ["client_traceid"]
    ),
    pk!(
        "handle_room",
        "POST",
        "/xlive/app-room/v1/universalInteract/Handle",
        true,
        true,
        ["client_traceid"]
    ),
    pk!(
        "start",
        "POST",
        "/xlive/general-interface/v2/pk/startMutiPK",
        true,
        true,
        []
    ),
    pk!(
        "exit",
        "POST",
        "/xlive/general-interface/v2/pk/exit",
        true,
        true,
        []
    ),
    pk!(
        "info",
        "GET",
        "/xlive/general-interface/v2/pk/info",
        true,
        true,
        []
    ),
    pk!(
        "classic_info",
        "GET",
        "/xlive/general-interface/v1/battle/getInfoById",
        false,
        true,
        []
    ),
    pk!(
        "universal_info",
        "GET",
        "/xlive/app-blink/v2/universalInteract/UniversalInfo",
        true,
        true,
        []
    ),
    pk!(
        "continuous_status",
        "GET",
        "/xlive/general-interface/v1/battle/autoMatchSwitchStatus",
        false,
        false,
        []
    ),
    pk!(
        "continuous_update",
        "GET",
        "/xlive/general-interface/v1/battle/updateAutoMatchSwitch",
        false,
        false,
        []
    ),
    pk!(
        "quit",
        "POST",
        "/xlive/app-blink/v1/universalInteract/Quit",
        true,
        true,
        ["client_traceid"]
    ),
];

const PK_SETTING_FIELDS: [(&str, &str); 9] = [
    ("accept_pk", "pk_accept"),
    ("accept_unfollowed_pk", "pk_follow_accept"),
    ("classic_pk", "classic_pk_open"),
    ("classic_opponent_voice", "classic_voice_open"),
    ("accept_connection", "accept_invitation"),
    ("mutual_only", "accept_follow_invitation"),
    ("allow_apply_join", "allow_apply"),
    ("allow_auto_join", "allow_auto_join"),
    ("allow_other_invite", "accept_invitation_other"),
];

struct PkSettings {
    preferences: Value,
    duration_options: Vec<i64>,
    show_score: i64,
    punish_time: i64,
    punish_options: Vec<i64>,
}
impl PkSettings {
    fn parse(data: &Value) -> BiliResult<Self> {
        let mut preferences = json!({});
        for (semantic, wire) in PK_SETTING_FIELDS {
            let enabled = match data[wire].as_i64() {
                Some(0) => false,
                Some(1) => true,
                _ => return Err(format!("PK settings lack a valid {wire} switch")),
            };
            preferences[semantic] = enabled.into();
        }
        let (duration_options, duration) = pk_duration_options(&data["pk_time_list"])?;
        preferences["duration"] = duration.into();
        let (punish_options, punish_time) = match data.get("pk_punish_time_list") {
            Some(Value::Null) => (Vec::new(), 0),
            Some(value) => pk_duration_options(value)?,
            None => return Err("PK settings omit punishment-time options".into()),
        };
        let show_score = data["show_score"]
            .as_i64()
            .filter(|n| i32::try_from(*n).is_ok())
            .ok_or("PK settings lack a valid show_score")?;
        Ok(Self {
            preferences,
            duration_options,
            show_score,
            punish_time,
            punish_options,
        })
    }
    fn value(&self) -> Value {
        json!({"preferences":self.preferences,"duration_options":self.duration_options,"show_score":self.show_score,"punish_time":self.punish_time,"punish_time_options":self.punish_options})
    }
    fn change(&mut self, field: &str, value: &Value) -> BiliResult<()> {
        if field == "duration" {
            let duration = value
                .as_i64()
                .filter(|duration| self.duration_options.contains(duration))
                .ok_or("PK duration must be one of the server-provided options")?;
            self.preferences["duration"] = duration.into();
            return Ok(());
        }
        if !PK_SETTING_FIELDS
            .iter()
            .any(|(semantic, _)| *semantic == field)
        {
            return Err("Unknown PK preference".into());
        }
        let enabled = value
            .as_bool()
            .ok_or("PK switches require a boolean value")?;
        self.preferences[field] = enabled.into();
        match field {
            "accept_pk" => self.preferences["accept_unfollowed_pk"] = enabled.into(),
            "accept_unfollowed_pk" if enabled => self.preferences["accept_pk"] = true.into(),
            "accept_connection" => {
                self.preferences["mutual_only"] =
                    (enabled && self.preferences["mutual_only"].as_bool().unwrap_or(false)).into()
            }
            "mutual_only" if enabled => self.preferences["accept_connection"] = true.into(),
            "allow_apply_join" => {
                self.preferences["allow_auto_join"] = (enabled
                    && self.preferences["allow_auto_join"]
                        .as_bool()
                        .unwrap_or(false))
                .into()
            }
            "allow_auto_join" if enabled => self.preferences["allow_apply_join"] = true.into(),
            _ => (),
        }
        Ok(())
    }
    fn fields(&self) -> Fields {
        let mut result: Fields = PK_SETTING_FIELDS
            .into_iter()
            .map(|(semantic, wire)| {
                (
                    wire.to_owned(),
                    if self.preferences[semantic].as_bool() == Some(true) {
                        "1"
                    } else {
                        "0"
                    }
                    .into(),
                )
            })
            .collect();
        result.extend(fields([
            (
                "pk_time",
                self.preferences["duration"]
                    .as_i64()
                    .unwrap_or_default()
                    .to_string(),
            ),
            ("show_score", self.show_score.to_string()),
            ("pk_punish_time", self.punish_time.to_string()),
            ("show_pk_play_text", "2".into()),
        ]));
        result
    }
}

fn pk_duration_options(value: &Value) -> BiliResult<(Vec<i64>, i64)> {
    let map = value
        .as_object()
        .filter(|map| !map.is_empty())
        .ok_or("PK settings require duration options")?;
    let mut options = std::collections::BTreeSet::new();
    let mut selected = None;
    for (key, marker) in map {
        let digits = key.strip_prefix('-').unwrap_or(key);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let number = key
            .parse::<i64>()
            .map_err(|_| "PK duration option exceeds int64")?;
        if !options.insert(number) {
            return Err("PK duration options contain duplicate numeric values".into());
        }
        let marker = marker
            .as_i64()
            .filter(|n| i32::try_from(*n).is_ok())
            .ok_or("PK duration selection marker must be int32")?;
        if marker == 1 && selected.replace(number).is_some() {
            return Err("PK duration has more than one selected option".into());
        }
    }
    Ok((
        options.into_iter().collect(),
        selected.ok_or("PK duration has no selected numeric option")?,
    ))
}

fn parse_continuous(mut data: Value) -> BiliResult<Value> {
    let status = data["status"]
        .as_i64()
        .filter(|n| i32::try_from(*n).is_ok())
        .ok_or("Continuous PK status is malformed")?;
    let effect = data["effect_pk_version"]
        .as_i64()
        .ok_or("Continuous PK effect version is malformed")?;
    if data.get("pk_version") == Some(&Value::Null) && status == 0 && effect == 0 {
        data["pk_version"] = json!([]);
    }
    if !data["button_disable"].is_boolean()
        || !data["pk_version"]
            .as_array()
            .is_some_and(|versions| versions.iter().all(|v| v.as_i64().is_some()))
        || data.get("toast").is_some_and(|value| !value.is_string())
    {
        return Err("Continuous PK response fields are malformed".into());
    }
    if data.get("toast").is_none() {
        data["toast"] = "".into();
    }
    Ok(data)
}

fn verified_cover_url(source: &str) -> BiliResult<reqwest::Url> {
    if source.len() > 8192 {
        return Err("Cover URL exceeds its size limit".into());
    }
    let absolute = if source.starts_with("//") {
        format!("https:{source}")
    } else {
        source.to_owned()
    };
    let mut url =
        reqwest::Url::parse(&absolute).map_err(|_| "Platform returned an invalid cover URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url
            .port()
            .is_some_and(|port| port != 443 && !(url.scheme() == "http" && port == 80))
    {
        return Err("Platform cover URL has unsupported transport or credentials".into());
    }
    let host = url.host_str().ok_or("Cover URL lacks a host")?;
    if !["hdslb.com", "biliimg.com"]
        .iter()
        .any(|root| host == *root || host.ends_with(&format!(".{root}")))
    {
        return Err("Platform cover URL is outside the supported Bilibili image hosts".into());
    }
    if url.scheme() == "http" {
        url.set_port(None).map_err(|_| "Invalid cover port")?;
        url.set_scheme("https")
            .map_err(|_| "Invalid cover transport")?;
    }
    url.set_fragment(None);
    Ok(url)
}

fn cover_mime(bytes: &[u8]) -> BiliResult<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Ok("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Ok("image/webp")
    } else {
        Err("Current cover is not a supported PNG, JPEG or WebP image".into())
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn fields<const N: usize>(pairs: [(&str, String); N]) -> Fields {
    pairs.into_iter().map(|(k, v)| (k.into(), v)).collect()
}
fn required_text<'a>(args: &'a Value, key: &str) -> BiliResult<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| format!("{key} is required"))
}
fn number_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(v) => Some(v.clone()),
        Value::Number(n) if n.is_i64() || n.is_u64() => Some(n.to_string()),
        _ => None,
    }
}
fn positive_id(value: &Value, name: &str) -> BiliResult<String> {
    number_text(Some(value))
        .filter(|s| s.parse::<u64>().is_ok_and(|n| n > 0))
        .ok_or_else(|| format!("{name} must be a positive integer"))
}
fn json_fields(value: Option<&Value>) -> BiliResult<Fields> {
    let Some(value) = value else {
        return Ok(Fields::new());
    };
    value
        .as_object()
        .ok_or("fields must be an object")?
        .iter()
        .map(|(key, value)| {
            let text = match value {
                Value::String(v) => v.clone(),
                Value::Number(_) => value.to_string(),
                Value::Bool(b) => if *b { "1" } else { "0" }.into(),
                _ => return Err(format!("{key} must be text, number or boolean")),
            };
            Ok((key.clone(), text))
        })
        .collect()
}

fn response_bytes(response: Response) -> BiliResult<Vec<u8>> {
    // Upstream's headers advertise deflate as well as gzip. Reqwest handles
    // gzip/brotli; the existing flate2 dependency handles the zlib-wrapped HTTP
    // deflate format with the same bound on both encoded and decoded bytes.
    let encoding = response
        .headers()
        .get(CONTENT_ENCODING)
        .map(|value| value.to_str().map(str::to_ascii_lowercase))
        .transpose()
        .map_err(|_| "Invalid Bilibili response content encoding")?;
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read Bilibili response")?;
    if bytes.len() as u64 > MAX_RESPONSE {
        return Err("Bilibili response exceeds 8 MiB".into());
    }
    if encoding.as_deref() == Some("deflate") {
        let mut decoded = Vec::new();
        flate2::read::ZlibDecoder::new(bytes.as_slice())
            .take(MAX_RESPONSE + 1)
            .read_to_end(&mut decoded)
            .map_err(|_| "Invalid deflate response from Bilibili")?;
        if decoded.len() as u64 > MAX_RESPONSE {
            return Err("Decoded Bilibili response exceeds 8 MiB".into());
        }
        Ok(decoded)
    } else if encoding.as_deref().is_none_or(|value| value == "identity") {
        Ok(bytes)
    } else {
        Err("Bilibili returned an unsupported content encoding".into())
    }
}

fn base_fields(mut values: Fields, timestamp: u64, profile: &ProtocolProfile) -> Fields {
    for (key, value) in [
        ("access_key", ""),
        ("appkey", profile.app_key()),
        ("build", profile.build()),
        ("platform", "pc_link"),
        ("version", profile.version()),
    ] {
        values.entry(key.into()).or_insert_with(|| value.into());
    }
    values
        .entry("ts".into())
        .or_insert_with(|| timestamp.to_string());
    values
}

fn sign_fields(values: Fields, timestamp: u64, native: bool, profile: &ProtocolProfile) -> Fields {
    let mut values = base_fields(values, timestamp, profile);
    resign(&mut values, native, profile);
    values
}

fn resign(values: &mut Fields, native: bool, profile: &ProtocolProfile) {
    values.remove("sign");
    let preimage = format!("{}{}", encode_fields(values, native), profile.app_secret());
    values.insert(
        "sign".into(),
        format!("{:x}", md5::compute(preimage.as_bytes())),
    );
}

fn encode_fields(values: &Fields, native: bool) -> String {
    values
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                percent_encode(k, native),
                percent_encode(v, native)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_encode(value: &str, native: bool) -> String {
    let mut result = String::with_capacity(value.len());
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || b"-._~".contains(&byte)
            || (native && b"!'()*".contains(&byte))
        {
            result.push(byte as char);
        } else if byte == b' ' && !native {
            result.push('+');
        } else {
            result.push('%');
            result.push(HEX[(byte >> 4) as usize] as char);
            result.push(HEX[(byte & 15) as usize] as char);
        }
    }
    result
}

fn protocol_policy(args: &Value) -> BiliResult<(&str, bool)> {
    let protocol = args
        .get("protocol")
        .and_then(Value::as_str)
        .unwrap_or("rtmp");
    let strategy = args
        .get("strategy")
        .and_then(Value::as_str)
        .unwrap_or("fallback");
    if !["rtmp", "srt", "srt-only", "srt_fallback_rtmp", "srt_only"].contains(&protocol) {
        return Err("protocol must be rtmp or srt".into());
    }
    if ![
        "fallback",
        "prefer",
        "only",
        "strict",
        "srt_fallback_rtmp",
        "srt_only",
    ]
    .contains(&strategy)
    {
        return Err("strategy must be fallback or only".into());
    }
    let strict = ["srt-only", "srt_only"].contains(&protocol)
        || ["only", "strict", "srt_only"].contains(&strategy);
    Ok((if protocol == "rtmp" { "rtmp" } else { "srt" }, strict))
}

fn select_stream(data: &Value, args: &Value) -> BiliResult<Value> {
    let (protocol, strict) = protocol_policy(args)?;
    let srt = data
        .get("protocols")
        .and_then(Value::as_array)
        .and_then(|p| {
            p.iter().find(|p| {
                p.get("protocol")
                    .and_then(Value::as_str)
                    .is_some_and(|x| x.eq_ignore_ascii_case("srt"))
                    && p.get("addr")
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty())
                    && p.get("code")
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty())
            })
        });
    let (selected, actual, fallback) = if protocol == "srt" {
        if let Some(srt) = srt {
            (srt, "srt", false)
        } else if strict {
            return Ok(
                json!({"status":"live_without_requested_protocol","protocol":"srt","available":false,"message":"Bilibili did not offer SRT. RTMP fallback was disabled."}),
            );
        } else {
            (
                data.get("rtmp")
                    .ok_or("Bilibili omitted the RTMP fallback endpoint")?,
                "rtmp",
                true,
            )
        }
    } else {
        (
            data.get("rtmp")
                .ok_or("Bilibili omitted the RTMP endpoint")?,
            "rtmp",
            false,
        )
    };
    let server = required_text(selected, "addr")?;
    let key = required_text(selected, "code")?;
    Ok(
        json!({"protocol":actual,"server":server,"key":key,"stream_addr":server,"stream_key":key,"fallback":fallback,"available":true}),
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn check_pk_precheck(data: &Value) -> BiliResult<()> {
    if !data.is_object() {
        return Err("PK precheck returned malformed data".into());
    }
    let show_type = match data.get("show_type") {
        None => 0,
        Some(v) => v
            .as_i64()
            .ok_or("PK precheck show_type must be an integer")?,
    };
    if let Some(tips) = data.get("tips") {
        if !tips.is_object() {
            return Err("PK precheck tips must be an object".into());
        }
        for key in ["title", "content", "confirm_content"] {
            if tips.get(key).is_some_and(|v| !v.is_string()) {
                return Err("PK precheck contains malformed explanation fields".into());
            }
        }
    }
    if show_type == 1 {
        return Err(data
            .pointer("/tips/content")
            .or_else(|| data.pointer("/tips/title"))
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .unwrap_or("Bilibili blocked this PK request")
            .to_owned());
    }
    Ok(())
}
fn random_hex(size: usize) -> String {
    let mut bytes = vec![0; size];
    if getrandom::fill(&mut bytes).is_err() {
        // Trace and device identifiers are not authentication secrets. A local
        // deterministic fallback still avoids panicking on unusual targets.
        let digest = Sha256::digest(format!("{}-{}", now(), std::process::id()));
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = digest[i % digest.len()];
        }
    }
    hex(&bytes)
}

pub(crate) fn generated_buvid() -> String {
    format!(
        "{}{}user",
        random_uuid().to_ascii_uppercase(),
        std::process::id()
    )
}

fn random_uuid() -> String {
    let mut id = random_hex(16);
    // UUID v4 layout, preserving the upstream device-identifier format.
    id.replace_range(12..13, "4");
    let variant = u8::from_str_radix(&id[16..17], 16).unwrap_or(0) & 3 | 8;
    id.replace_range(16..17, &format!("{variant:x}"));
    format!(
        "{}-{}-{}-{}-{}",
        &id[0..8],
        &id[8..12],
        &id[12..16],
        &id[16..20],
        &id[20..]
    )
}

fn sequence_ack_form(
    sequences: &[u32],
    csrf: &str,
    timestamp: u64,
    profile: &ProtocolProfile,
) -> BiliResult<String> {
    if sequences.is_empty() || sequences.len() > 4096 || sequences.contains(&0) {
        return Err("Sequence ACK requires 1–4096 nonzero uint32 sequence numbers".into());
    }
    let mut sequences = sequences.to_vec();
    sequences.sort_unstable();
    sequences.dedup();
    let mut pairs = vec![
        ("appkey", profile.app_key().to_owned()),
        ("ts", timestamp.to_string()),
        ("platform", "pc_link".into()),
        ("version", profile.version().into()),
        ("access_key", "".into()),
        ("terminal", "1".into()),
    ];
    pairs.extend(sequences.iter().map(|n| ("sequence", n.to_string())));
    let encode = |pairs: &Vec<(&str, String)>| {
        pairs
            .iter()
            .map(|(k, v)| format!("{}={}", percent_encode(k, true), percent_encode(v, true)))
            .collect::<Vec<_>>()
            .join("&")
    };
    let preimage = format!("{}{}", encode(&pairs), profile.app_secret());
    pairs.push(("sign", format!("{:x}", md5::compute(preimage.as_bytes()))));
    pairs.push(("csrf_token", csrf.into()));
    pairs.push(("csrf", csrf.into()));
    Ok(encode(&pairs))
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(message);
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

fn decode_risk(voucher: &str, content: &str) -> BiliResult<Value> {
    if voucher.len() > 4096 || content.len() > 64 * 1024 {
        return Err("Verification response is oversized".into());
    }
    let outer = BASE64
        .decode(content)
        .map_err(|_| "Invalid verification encoding")?;
    let text = std::str::from_utf8(&outer).map_err(|_| "Invalid verification envelope")?;
    let parts: Vec<_> = text.split('|').collect();
    if parts.len() != 4 {
        return Err("Invalid verification envelope".into());
    }
    let index = parts[2]
        .parse::<u32>()
        .map_err(|_| "Invalid verification key index")?;
    let bits: Vec<_> = (0..32).filter(|i| index & (1 << i) != 0).collect();
    if bits.len() < 2 {
        return Err("Invalid verification key index".into());
    }
    let chars: Vec<_> = voucher.chars().collect();
    let token = if chars.len() > 8 {
        &chars[8..]
    } else {
        &chars[..]
    };
    let key: String = token.iter().skip(bits[0]).take(bits[1] - bits[0]).collect();
    let digest = Sha256::digest(format!("{key}{}", parts[0]));
    let mut encrypted = BASE64
        .decode(parts[3])
        .map_err(|_| "Invalid verification content encoding")?;
    for (i, b) in encrypted.iter_mut().enumerate() {
        *b ^= digest[i % 16];
    }
    serde_json::from_slice(&encrypted).map_err(|_| "Invalid verification challenge content".into())
}

fn obfuscate_token(token: &str) -> BiliResult<String> {
    let mut random = [0u8; 9];
    getrandom::fill(&mut random).map_err(|_| "Secure randomness unavailable for verification")?;
    let mut bytes = Vec::with_capacity(token.len() + 9);
    bytes.push(random[0]);
    bytes.extend(
        token
            .bytes()
            .enumerate()
            .map(|(i, b)| b ^ random[0] ^ random[1 + i % 8]),
    );
    bytes.extend_from_slice(&random[1..]);
    Ok(BASE64.encode(bytes))
}

fn dm_track() -> String {
    // These public browser-profile values match the upstream client. The user
    // still completes the actual identity challenge on Bilibili's own page.
    json!({"dm_img_list":[],"dm_img_str":"V2ViR0wgMS4wIChPcGVuR0wgRVMgMi4wIENocm9taXVtKQ","dm_cover_img_str":"QU5HTEUgKEludGVsLCBJbnRlbChSKSBVSEQgR3JhcGhpY3MgRGlyZWN0M0QxMSB2c181XzAgcHNfNV8wLCBEM0QxMSlHb29nbGUgSW5jLiAoSW50ZW","dm_img_inter":json!({"ds":[],"wh":[1840,1580,0],"of":[0,0,0]}).to_string()}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn live_fixture(body: Value) -> (BiliClient, std::thread::JoinHandle<String>) {
        live_fixture_profile(body, ProtocolProfile::default(), false)
    }
    fn live_fixture_profile(
        body: Value,
        profile: ProtocolProfile,
        deflate: bool,
    ) -> (BiliClient, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                let count = socket.read(&mut buf).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buf[..count]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&request[..end]);
                    let length = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|s| s.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
                assert!(request.len() < 16 * 1024);
            }
            let body = body.to_string().into_bytes();
            let (body, encoding) = if deflate {
                let mut encoder =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(&body).unwrap();
                (encoder.finish().unwrap(), "Content-Encoding: deflate\r\n")
            } else {
                (body, "")
            };
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{encoding}Connection: close\r\n\r\n",body.len()).unwrap();
            socket.write_all(&body).unwrap();
            String::from_utf8(request).unwrap()
        });
        let mut client = BiliClient::with_profile(
            "SESSDATA=fixture-secret; bili_jct=fixture-csrf; DedeUserID=123",
            BiliOptions::default(),
            profile,
        )
        .unwrap();
        client.origins.live = format!("http://{address}");
        (client, server)
    }

    fn verification_fixture(
        bodies: Vec<Value>,
    ) -> (BiliClient, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::Instant;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for body in bodies {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "missing verification request");
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("{error}"),
                    }
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                loop {
                    let count = socket.read(&mut buf).unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buf[..count]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&request[..end]);
                        let length = head
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|value| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                    assert!(request.len() < 16 * 1024);
                }
                let body = body.to_string();
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                requests.push(String::from_utf8(request).unwrap());
            }
            requests
        });
        let mut client = BiliClient::new(
            "SESSDATA=fixture-secret; bili_jct=fixture-csrf; DedeUserID=123; buvid3=fixture-device",
        )
        .unwrap();
        client.origins.live = format!("http://{address}");
        client.origins.api = format!("http://{address}");
        (client, server)
    }

    fn verification_content() -> String {
        let payload = br#"{"type":"realname","token":"fixture-token"}"#;
        let digest = Sha256::digest(b"abc1700000000");
        let bytes: Vec<_> = payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ digest[index % 16])
            .collect();
        BASE64.encode(format!("1700000000|salt|9|{}", BASE64.encode(bytes)))
    }

    fn request_form(request: &str) -> Fields {
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        reqwest::Url::parse(&format!("http://fixture/?{body}"))
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect()
    }

    #[test]
    fn refreshed_snapshot_controls_real_live_signatures_ack_and_headers() {
        use crate::protocol_profile::ProtocolProfiles;
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory = Scratch(
            std::env::temp_dir().join(format!("bilibilive-protocol-wire-{}", random_uuid())),
        );
        let profiles = ProtocolProfiles::new(&directory.0);
        let old = profiles.snapshot().unwrap();
        let document = json!({"ak":"1234567890abcdef","as":"0123456789abcdef0123456789abcdef",
            "b":"12000","v":"9.0.0.12000","start_ac":false,"stop_ac":true,
            "ha":{"User-Agent":"FixtureApp/9","Accept-Encoding":"gzip,deflate","Accept-Language":"zh-CN"},
            "hw":{"User-Agent":"FixtureWeb/9","Referer":"https://live.bilibili.com/"}});
        let (endpoint, server) = live_fixture(document);
        profiles
            .refresh_fixture(
                &BiliOptions::default(),
                &format!("{}/version.json", endpoint.origins.live),
            )
            .unwrap();
        let refresh_request = server.join().unwrap().to_ascii_lowercase();
        assert!(!refresh_request.contains("cookie:"));
        assert!(!refresh_request.contains("authorization:"));
        let snapshot = ProtocolProfiles::new(&directory.0).snapshot().unwrap();
        for (operation, include_csrf) in [("live.start", false), ("live.stop", true)] {
            let body = if operation == "live.start" {
                json!({"code":0,"data":{"rtmp":{"addr":"rtmp://fixture/live/","code":"fixture-key"}}})
            } else {
                json!({"code":0,"data":{}})
            };
            // The real response exercises upstream's advertised HTTP deflate.
            let (client, server) = live_fixture_profile(body, snapshot.clone(), true);
            client
                .call(operation, &json!({"room_id":42,"area_id":371}))
                .unwrap();
            let request = server.join().unwrap();
            let lower = request.to_ascii_lowercase();
            assert!(lower.contains("user-agent: fixtureapp/9\r\n"));
            assert!(lower.contains("accept-language: zh-cn\r\n"));
            assert!(lower.contains("appkey=1234567890abcdef"));
            let body = request.split("\r\n\r\n").nth(1).unwrap();
            let uri = reqwest::Url::parse(&format!("http://fixture/?{body}")).unwrap();
            let mut fields: Fields = uri.query_pairs().into_owned().collect();
            assert_eq!(fields["appkey"], "1234567890abcdef");
            assert_eq!(fields["build"], "12000");
            assert_eq!(fields["version"], "9.0.0.12000");
            assert_eq!(fields["csrf"], "fixture-csrf");
            let signature = fields.remove("sign").unwrap();
            if !include_csrf {
                fields.remove("csrf");
                fields.remove("csrf_token");
            }
            // These fixture values need no percent encoding. This independent
            // preimage asserts the configured secret and CSRF signing order.
            let preimage = fields
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("&")
                + "0123456789abcdef0123456789abcdef";
            assert_eq!(signature, format!("{:x}", md5::compute(preimage)));
        }
        let (mut client, server) =
            live_fixture_profile(json!({"code":0,"data":{}}), snapshot.clone(), false);
        client.origins.passport = client.origins.live.clone();
        client.login_generate().unwrap();
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(request.contains("user-agent: fixtureweb/9\r\n"));
        assert!(request.contains("appkey=1234567890abcdef"));
        assert!(request.contains("buvid3="));
        assert!(!request.contains("sessdata="));
        assert!(!request.contains("bili_jct="));
        assert!(!request.contains("dedeuserid="));
        let (client, server) = live_fixture_profile(json!({"code":0,"data":{}}), snapshot, false);
        client.ack_sequences(&[100, 2, 100]).unwrap();
        let request = server.join().unwrap();
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let (preimage, suffix) = body.split_once("&sign=").unwrap();
        assert!(preimage.starts_with("appkey=1234567890abcdef&ts="));
        assert!(preimage.contains("&version=9.0.0.12000&"));
        assert!(preimage.ends_with("&sequence=2&sequence=100"));
        let expected = format!(
            "{:x}",
            md5::compute(format!("{preimage}0123456789abcdef0123456789abcdef"))
        );
        assert_eq!(
            suffix,
            format!("{expected}&csrf_token=fixture-csrf&csrf=fixture-csrf")
        );
        // A client captured before refresh keeps the original complete profile.
        let (client, server) = live_fixture_profile(json!({"code":0,"data":{}}), old, false);
        client.call("live.stop", &json!({"room_id":42})).unwrap();
        let request = server.join().unwrap();
        assert!(request.contains("appkey=aae92bc66f3edfab"));
        assert!(request.contains("build=11081"));
        assert!(request.contains("version=8.7.0.11081"));
        assert!(request.contains("LiveHime/8.7.0.11081"));
    }
    #[test]
    fn standard_and_native_wire_encodings_are_distinct() {
        let f = fields([("a", "中 文!'()*~".into())]);
        assert_eq!(
            encode_fields(&f, false),
            "a=%E4%B8%AD+%E6%96%87%21%27%28%29%2A~"
        );
        assert_eq!(encode_fields(&f, true), "a=%E4%B8%AD%20%E6%96%87!'()*~");
        assert_eq!(
            sign_fields(f.clone(), 1700000000, false, &ProtocolProfile::default())["sign"],
            "690fdf35858a15ff6b61f3261c7a642a"
        );
        assert_eq!(
            sign_fields(f, 1700000000, true, &ProtocolProfile::default())["sign"],
            "e92e32c01d32a1cadf9073acf43fa0f1"
        );
    }
    #[test]
    fn hmac_matches_rfc4231_vector() {
        assert_eq!(
            hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }
    #[test]
    fn start_and_stop_have_different_csrf_signing_contracts() {
        let csrf = fields([
            ("room_id", "42".into()),
            ("csrf", "fixture".into()),
            ("csrf_token", "fixture".into()),
        ]);
        let start = sign_fields(csrf.clone(), 1700000000, false, &ProtocolProfile::default());
        let mut stop = sign_fields(
            fields([("room_id", "42".into())]),
            1700000000,
            false,
            &ProtocolProfile::default(),
        );
        stop.extend(csrf);
        assert_ne!(start["sign"], stop["sign"]);
        let mut unsigned = stop.clone();
        unsigned.remove("csrf");
        unsigned.remove("csrf_token");
        resign(&mut unsigned, false, &ProtocolProfile::default());
        assert_eq!(unsigned["sign"], stop["sign"]);
    }
    #[test]
    fn malformed_or_blocked_pk_precheck_stops_the_flow() {
        assert!(check_pk_precheck(&json!({"show_type":"1"})).is_err());
        assert!(
            check_pk_precheck(&json!({"show_type":1,"tips":{"content":"Room is not eligible"}}))
                .is_err()
        );
        assert!(check_pk_precheck(&json!({"show_type":0,"tips":{}})).is_ok());
    }
    #[test]
    fn face_challenges_are_registered_and_reported_before_the_url_is_exposed() {
        for (code, data) in [
            (60024, json!({"qr":"https://www.bilibili.com/verify"})),
            (60043, json!({"risk_extra":{"v_voucher":"12345678abcdef"}})),
        ] {
            let message = "目标分区需要人脸验证";
            let mut responses = vec![json!({"code":code,"message":message,"data":data})];
            if code == 60043 {
                responses.push(json!({"code":0,"data":{"content":verification_content()}}));
            }
            responses.push(json!({"code":0,"data":{}}));
            responses.push(if code == 60024 {
                json!({"code":0,"data":{"is_identified":true}})
            } else {
                json!({"code":0,"data":{"status":1}})
            });
            let (client, server) = verification_fixture(responses);
            let result = client
                .call(
                    "live.start",
                    &json!({"room_id":42,"area_id":371,"protocol":"srt","strategy":"prefer"}),
                )
                .unwrap();
            assert_eq!(result["status"], "verification_required");
            let verification = &result["verification"];
            assert_eq!(verification["type"], code);
            assert_eq!(verification["reported"], true);
            assert_eq!(verification["room_id"], "42");
            assert_eq!(verification["area_id"], "371");
            assert_eq!(verification["message"], message);
            assert_eq!(
                verification["start_args"],
                json!({"room_id":"42","area_id":"371","protocol":"srt","strategy":"prefer","expected_uid":"123"})
            );
            crate::schema::validate("live.start", &verification["start_args"]).unwrap();
            assert!(result.get("key").is_none());
            assert!(!crate::app::is_success(&result));
            let poll = client
                .call(
                    "face.poll",
                    &json!({
                        "type":code,"room_id":verification["room_id"],"token":verification["token"]
                    }),
                )
                .unwrap();
            assert_eq!(poll["verified"], true);
            let requests = server.join().unwrap();
            assert!(requests[0].starts_with("POST /room/v1/Room/startLive "));
            let report_index = if code == 60043 { 2 } else { 1 };
            let report = &requests[report_index];
            assert!(report.starts_with("POST /xlive/app-blink/v1/preLive/ReportFaceRecognition "));
            let mut report_form = request_form(report);
            for (key, value) in [
                ("area_v2_id", "371"),
                ("room_id", "42"),
                ("scene", "startLive"),
                ("face_auth_message", message),
                ("csrf", "fixture-csrf"),
                ("csrf_token", "fixture-csrf"),
            ] {
                assert_eq!(report_form.remove(key).as_deref(), Some(value));
            }
            assert_eq!(report_form.remove("face_auth_code"), Some(code.to_string()));
            let signature = report_form.remove("sign").unwrap();
            // Independently verify the upstream signature scope: only public
            // profile fields are signed; challenge fields and CSRF follow it.
            let preimage = report_form
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("&")
                + "af125a0d5279fd576c1b4418a3e8276d";
            assert_eq!(signature, format!("{:x}", md5::compute(preimage)));
            for request in &requests {
                assert!(request.to_ascii_lowercase().contains(&format!(
                    "buvid: {}\r\n",
                    client.device_buvid().to_ascii_lowercase()
                )));
                assert!(request.contains("buvid3=fixture-device"));
            }
            let poll_request = requests.last().unwrap();
            let poll_form = request_form(poll_request);
            if code == 60043 {
                assert!(requests[1].starts_with("POST /x/gaia-vgate/v2/register "));
                let register_form = request_form(&requests[1]);
                assert_eq!(register_form["v_voucher"], "12345678abcdef");
                assert_eq!(register_form["dm_track"], "[]");
                assert!(poll_request.starts_with("POST /x/gaia-vgate/v2/validatePreCheck "));
                assert_eq!(poll_form["token"], verification["token"].as_str().unwrap());
                assert_ne!(poll_form["token"], "fixture-token");
                let tracking: Value = serde_json::from_str(&poll_form["dm_track"]).unwrap();
                assert!(tracking["dm_img_inter"].is_string());
                let url = verification["url"].as_str().unwrap();
                assert!(url.starts_with("https://www.bilibili.com/h5/risk-control/realname?t="));
            } else {
                assert_eq!(verification["url"], "https://www.bilibili.com/verify");
                assert!(
                    poll_request.starts_with(
                        "POST /xlive/app-blink/v1/preLive/IsUserIdentifiedByFaceAuth "
                    )
                );
                assert_eq!(poll_form["room_id"], "42");
                assert_eq!(poll_form["face_auth_code"], "60024");
            }
        }
    }

    #[test]
    fn failed_challenge_report_is_an_initialization_error_without_a_ready_qr() {
        let (client, server) = verification_fixture(vec![
            json!({"code":60024,"message":"verification required","data":{"qr":"https://www.bilibili.com/verify"}}),
            json!({"code":-400,"message":"fixture report rejected"}),
        ]);
        let error = client
            .call("live.start", &json!({"room_id":42,"area_id":371}))
            .unwrap_err();
        assert!(error.contains("initialize Bilibili face verification"));
        assert!(error.contains("-400"));
        assert_eq!(server.join().unwrap().len(), 2);
    }

    #[test]
    fn failed_v2_registration_stops_before_reporting_or_displaying_a_challenge() {
        let (client, server) = verification_fixture(vec![
            json!({"code":60043,"message":"verification required","data":{"risk_extra":{"v_voucher":"12345678abcdef"}}}),
            json!({"code":-400,"message":"fixture voucher expired"}),
        ]);
        let error = client
            .call("live.start", &json!({"room_id":42,"area_id":371}))
            .unwrap_err();
        assert!(error.contains("prepare Bilibili face verification"));
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].starts_with("POST /x/gaia-vgate/v2/register "));
    }

    #[test]
    fn face_poll_requires_platform_confirmation_and_preserves_pending_or_failed_state() {
        for (kind, response, expected) in [
            (60024, json!({"is_identified":false}), "pending"),
            (60024, json!({"is_identified":0}), "pending"),
            (60024, json!({"is_identified":1}), "verified"),
            (60043, json!({"status":0}), "pending"),
            (60043, json!({"status":2}), "expired_or_failed"),
            (60043, json!({"status":3}), "expired_or_failed"),
        ] {
            let (client, server) = verification_fixture(vec![json!({"code":0,"data":response})]);
            let result = client
                .call(
                    "face.poll",
                    &json!({"type":kind,"room_id":42,"token":"fixture-token"}),
                )
                .unwrap();
            assert_eq!(result["status"], expected);
            assert_eq!(result["verified"], expected == "verified");
            assert_eq!(server.join().unwrap().len(), 1);
        }
    }

    #[test]
    fn anonymous_and_authenticated_sessions_keep_distinct_device_and_cookie_identity() {
        let native = "D4A57BAE-7BC0-4DC2-8B6B-65EEB6754A9C1234user";
        let client = BiliClient::with_profile_and_buvid(
            "SESSDATA=fixture-secret; bili_jct=fixture-csrf; DedeUserID=123; buvid3=spi-fixture; buvid4=spi-four; bili_ticket=fixture-ticket",
            BiliOptions::default(),
            ProtocolProfile::default(),
            native,
        ).unwrap();
        for web in [false, true] {
            for authenticated in [false, true] {
                let request = client
                    .prepare(
                        "GET",
                        &client.origins.passport,
                        "/fixture",
                        &Fields::new(),
                        None,
                        web,
                        authenticated,
                        false,
                    )
                    .unwrap()
                    .build()
                    .unwrap();
                let headers = request.headers();
                assert_eq!(headers["buvid"].to_str().unwrap(), native);
                assert_eq!(headers["accept"].to_str().unwrap(), "*/*");
                assert_eq!(headers.contains_key("origin"), web);
                assert_eq!(headers.contains_key("referer"), web);
                assert_eq!(
                    headers["user-agent"],
                    client.profile.headers(web)["user-agent"]
                );
                let cookies = CookieJar::parse(headers[COOKIE].to_str().unwrap()).unwrap();
                assert_eq!(cookies.get("appkey"), Some(client.profile.app_key()));
                assert!(cookies.get("device_name").is_some());
                assert_eq!(
                    cookies.get("device_platform"),
                    Some("Windows Version: 10.0 x86_64")
                );
                assert_eq!(
                    cookies.get("buvid3"),
                    Some(if authenticated { "spi-fixture" } else { native })
                );
                for name in [
                    "SESSDATA",
                    "bili_jct",
                    "DedeUserID",
                    "buvid4",
                    "bili_ticket",
                ] {
                    assert_eq!(cookies.get(name).is_some(), authenticated);
                }
            }
        }
        assert_eq!(client.jar().unwrap().get("buvid3"), Some("spi-fixture"));
    }

    #[test]
    fn initialized_device_cookies_survive_new_clients_through_face_verification() {
        for device_cookies in ["", "; buvid3=; buvid4="] {
            let (initial, server) = verification_fixture(vec![
                json!({"code":0,"data":{"b_3":"official-fixture-device","b_4":"official-fixture-four"}}),
                json!({"code":60024,"message":"verification required","data":{"qr":"https://www.bilibili.com/verify"}}),
                json!({"code":0,"data":{}}),
                json!({"code":0,"data":{"is_identified":true}}),
            ]);
            *initial.cookies.lock().unwrap() = CookieJar::parse(&format!(
                "SESSDATA=fixture-secret; bili_jct=fixture-csrf; DedeUserID=123; bili_ticket=fixture-ticket; bili_ticket_expires={}{}",
                now() + 3600, device_cookies
            )).unwrap();
            let mut refreshed = initial.call("auth.refresh", &json!({})).unwrap();
            let cookie = refreshed
                .as_object_mut()
                .unwrap()
                .remove("_private_cookie")
                .unwrap();
            let cookie = cookie.as_str().unwrap();
            let new_client = || {
                let mut client = BiliClient::with_profile_and_buvid(
                    cookie,
                    BiliOptions::default(),
                    initial.profile.clone(),
                    initial.device_buvid(),
                )
                .unwrap();
                client.origins.live = initial.origins.live.clone();
                client.origins.api = initial.origins.api.clone();
                client
            };
            let start = new_client()
                .call("live.start", &json!({"room_id":42,"area_id":371}))
                .unwrap();
            assert_eq!(start["status"], "verification_required");
            assert_eq!(
                new_client()
                    .call("face.poll", &json!({"type":60024,"room_id":42}))
                    .unwrap()["verified"],
                true
            );
            let requests = server.join().unwrap();
            assert!(requests[0].starts_with("GET /x/frontend/finger/spi "));
            assert!(requests[0].contains(&format!("buvid3={}", initial.device_buvid())));
            for request in &requests {
                assert!(request.to_ascii_lowercase().contains(&format!(
                    "buvid: {}\r\n",
                    initial.device_buvid().to_ascii_lowercase()
                )));
                assert!(!request.contains("buvid: official-fixture-device\r\n"));
            }
            for request in &requests[1..] {
                assert!(request.contains("buvid3=official-fixture-device"));
                assert!(request.contains("buvid4=official-fixture-four"));
            }
            assert!(refreshed.get("_private_cookie").is_none());
        }
    }
    #[test]
    fn server_errors_cannot_echo_stored_credentials_to_the_ui() {
        let (client, server) =
            live_fixture(json!({"code":-101,"message":"expired fixture-secret / fixture-csrf"}));
        let error = client
            .call("live.stop", &json!({"room_id":42}))
            .unwrap_err();
        assert!(error.contains("-101"));
        assert!(!error.contains("fixture-secret"));
        assert!(!error.contains("fixture-csrf"));
        server.join().unwrap();
    }
    #[test]
    fn sequence_ack_preserves_native_order_and_repeated_keys() {
        let body = sequence_ack_form(
            &[100, 2, 100],
            "csrf fixture",
            1700000000,
            &ProtocolProfile::default(),
        )
        .unwrap();
        assert_eq!(
            body,
            "appkey=aae92bc66f3edfab&ts=1700000000&platform=pc_link&version=8.7.0.11081&access_key=&terminal=1&sequence=2&sequence=100&sign=b0e16fd8f5d527999fe40d3326ba969b&csrf_token=csrf%20fixture&csrf=csrf%20fixture"
        );
        assert!(sequence_ack_form(&[0], "csrf", 1700000000, &ProtocolProfile::default()).is_err());
    }
    #[test]
    fn srt_policy_does_not_silently_fallback_when_strict() {
        let data =
            json!({"rtmp":{"addr":"rtmp://example.test/live/","code":"secret"},"protocols":[]});
        assert_eq!(
            select_stream(&data, &json!({"protocol":"srt"})).unwrap()["fallback"],
            true
        );
        let strict = select_stream(&data, &json!({"protocol":"srt","strategy":"only"})).unwrap();
        assert_eq!(strict["available"], false);
        assert!(strict.get("key").is_none());
    }
    #[test]
    fn qr_poll_extracts_cookie_without_following_redirect() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut input = [0u8; 8192];
            let n = socket.read(&mut input).unwrap();
            let request = String::from_utf8_lossy(&input[..n]);
            assert!(request.starts_with("GET /x/passport-login/web/qrcode/poll?"));
            assert!(request.contains("buvid3="));
            assert!(!request.contains("SESSDATA="));
            assert!(!request.contains("bili_jct="));
            assert!(!request.contains("DedeUserID="));
            let body = r#"{"code":0,"data":{"code":0,"message":"ok","url":"https://passport.bilibili.com/ok"}}"#;
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nSet-Cookie: SESSDATA=test-secret; HttpOnly\r\nSet-Cookie: bili_jct=test-csrf\r\nSet-Cookie: DedeUserID=123\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        });
        let mut client = BiliClient::new("").unwrap();
        client.origins.passport = format!("http://{address}");
        let value = client.login_poll("fixture").unwrap();
        assert_eq!(value["status"], "authenticated");
        assert_eq!(value["uid"], "123");
        assert!(
            value["_private_cookie"]
                .as_str()
                .unwrap()
                .contains("SESSDATA=test-secret")
        );
        server.join().unwrap();
    }
    #[test]
    fn risk_codec_decodes_a_known_synthetic_envelope() {
        let voucher = "12345678abcdef";
        let payload = r#"{"type":"realname","token":"fixture-token"}"#;
        let digest = Sha256::digest(b"abc1700000000");
        let bytes: Vec<_> = payload
            .bytes()
            .enumerate()
            .map(|(i, b)| b ^ digest[i % 16])
            .collect();
        let outer = BASE64.encode(format!("1700000000|salt|9|{}", BASE64.encode(bytes)));
        assert_eq!(
            decode_risk(voucher, &outer).unwrap()["token"],
            "fixture-token"
        );
        assert!(decode_risk(voucher, "invalid").is_err());
    }
    #[test]
    fn complete_pk_settings_preserve_native_couplings_and_null_punishment() {
        let mut data = json!({"pk_time_list":{"60":0,"300":1,"600":0,"description":"ignored"},"pk_punish_time_list":null,"show_score":1});
        for (_, wire) in PK_SETTING_FIELDS {
            data[wire] = 0.into();
        }
        let mut settings = PkSettings::parse(&data).unwrap();
        settings
            .change("accept_unfollowed_pk", &json!(true))
            .unwrap();
        assert_eq!(settings.preferences["accept_pk"], true);
        settings.change("accept_pk", &json!(false)).unwrap();
        assert_eq!(settings.preferences["accept_unfollowed_pk"], false);
        settings.change("allow_auto_join", &json!(true)).unwrap();
        assert_eq!(settings.preferences["allow_apply_join"], true);
        settings.change("allow_apply_join", &json!(false)).unwrap();
        assert_eq!(settings.preferences["allow_auto_join"], false);
        settings.change("mutual_only", &json!(true)).unwrap();
        assert_eq!(settings.preferences["accept_connection"], true);
        settings.change("accept_connection", &json!(false)).unwrap();
        assert_eq!(settings.preferences["mutual_only"], false);
        settings.change("duration", &json!(600)).unwrap();
        let body = settings.fields();
        assert_eq!(body["pk_time"], "600");
        assert_eq!(body["pk_punish_time"], "0");
        assert_eq!(body["show_pk_play_text"], "2");
        assert_eq!(body.len(), 13);
        assert!(settings.change("duration", &json!(601)).is_err());
        assert!(settings.change("classic_pk", &json!(1)).is_err());
        assert!(pk_duration_options(&json!({"60":1,"060":0})).is_err());
        assert!(pk_duration_options(&json!({"60":1,"300":1})).is_err());
    }
    #[test]
    fn disabled_continuous_null_list_does_not_invent_a_mode() {
        let data = parse_continuous(
            json!({"status":0,"button_disable":true,"pk_version":null,"effect_pk_version":0}),
        )
        .unwrap();
        assert_eq!(data["pk_version"], json!([]));
        assert!(
            parse_continuous(
                json!({"status":1,"button_disable":false,"pk_version":null,"effect_pk_version":2})
            )
            .is_err()
        );
        assert!(
            parse_continuous(
                json!({"status":1,"button_disable":0,"pk_version":[2],"effect_pk_version":2})
            )
            .is_err()
        );
    }
    #[test]
    fn cover_preview_accepts_only_platform_image_hosts_and_real_image_bytes() {
        assert_eq!(
            verified_cover_url("http://i0.hdslb.com/bfs/live/example.jpg")
                .unwrap()
                .scheme(),
            "https"
        );
        assert!(verified_cover_url("//i1.hdslb.com/bfs/live/example.png").is_ok());
        for source in [
            "https://hdslb.com.evil.test/x",
            "https://example.test/?host=i0.hdslb.com",
            "http://127.0.0.1/x",
            "https://user:secret@i0.hdslb.com/x",
            "https://i0.hdslb.com:8080/x",
            "file:///etc/passwd",
        ] {
            assert!(verified_cover_url(source).is_err(), "{source}");
        }
        assert_eq!(
            cover_mime(b"\x89PNG\r\n\x1a\nfixture").unwrap(),
            "image/png"
        );
        assert!(cover_mime(b"<svg xmlns='http://www.w3.org/2000/svg'/>").is_err());
    }
}
