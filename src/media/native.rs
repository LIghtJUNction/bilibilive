//! libdatachannel 0.24 C ABI, loaded through Project Z's native loader.
//!
//! References: libdatachannel v0.24.0 include/rtc/rtc.h. Every opaque handle is
//! owned by its session, and media workers are joined before the handles close.
use super::wire;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    ffi::{CStr, CString, c_char, c_int},
    net::{SocketAddr, UdpSocket},
    ptr,
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zsys::dylib::Library;

#[repr(C)]
struct Configuration {
    ice_servers: *const *const c_char,
    ice_servers_count: c_int,
    proxy_server: *const c_char,
    bind_address: *const c_char,
    certificate_type: c_int,
    ice_transport_policy: c_int,
    enable_ice_tcp: bool,
    enable_ice_udp_mux: bool,
    disable_auto_negotiation: bool,
    force_media_transport: bool,
    port_range_begin: u16,
    port_range_end: u16,
    mtu: c_int,
    max_message_size: c_int,
}
#[repr(C)]
struct Reliability {
    unordered: bool,
    unreliable: bool,
    max_packet_life_time: u32,
    max_retransmits: u32,
}
#[repr(C)]
struct DataChannelInit {
    reliability: Reliability,
    protocol: *const c_char,
    negotiated: bool,
    manual_stream: bool,
    stream: u16,
}
type CreatePc = unsafe extern "C" fn(*const Configuration) -> c_int;
type CreateChannel = unsafe extern "C" fn(c_int, *const c_char, *const DataChannelInit) -> c_int;
type HandleFn = unsafe extern "C" fn(c_int) -> c_int;
type IsOpen = unsafe extern "C" fn(c_int) -> bool;
type Description = unsafe extern "C" fn(c_int, *mut c_char, c_int) -> c_int;
type SetLocal = unsafe extern "C" fn(c_int, *const c_char) -> c_int;
type SetRemote = unsafe extern "C" fn(c_int, *const c_char, *const c_char) -> c_int;
type AddTrack = unsafe extern "C" fn(c_int, *const c_char) -> c_int;
pub type SendMessage = unsafe extern "C" fn(c_int, *const c_char, c_int) -> c_int;
type ReceiveMessage = unsafe extern "C" fn(c_int, *mut c_char, *mut c_int) -> c_int;
type SetUserPointer = unsafe extern "C" fn(c_int, *mut std::ffi::c_void);
type CreatedCallback = unsafe extern "C" fn(c_int, c_int, *mut std::ffi::c_void);
type SetCreatedCallback = unsafe extern "C" fn(c_int, Option<CreatedCallback>) -> c_int;

#[derive(Clone, Copy)]
pub struct Functions {
    create_pc: CreatePc,
    create_channel: CreateChannel,
    delete_pc: HandleFn,
    delete: HandleFn,
    is_open: IsOpen,
    get_local: Description,
    get_remote: Description,
    set_local: SetLocal,
    set_remote: SetRemote,
    add_track: AddTrack,
    pub send: SendMessage,
    receive: ReceiveMessage,
    available: HandleFn,
    chain_receiving: HandleFn,
    request_keyframe: HandleFn,
    set_user_pointer: SetUserPointer,
    set_channel_callback: SetCreatedCallback,
    set_track_callback: SetCreatedCallback,
}
pub struct Native {
    _library: Library,
    _search: crate::bundle::LibrarySearch,
    source: serde_json::Value,
    pub functions: Functions,
}
fn cstring(value: &str) -> Result<CString, String> {
    CString::new(value).map_err(|_| "Native RTC string contains NUL".into())
}
fn checked(result: c_int, operation: &str) -> Result<c_int, String> {
    if result < 0 {
        Err(format!("libdatachannel {operation} failed ({result})"))
    } else {
        Ok(result)
    }
}
unsafe extern "C" fn quiet_log(_: c_int, _: *const c_char) {}

impl Native {
    pub fn load() -> Result<Self, String> {
        let selection = crate::bundle::rtc_library()?;
        let search = crate::bundle::LibrarySearch::new(&selection)?;
        let mut library = None;
        let mut selected_path = None;
        for path in &selection.candidates {
            #[cfg(windows)]
            let loaded = {
                use std::os::windows::ffi::OsStrExt;
                let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
                unsafe { Library::open_wide(&wide) }
            };
            #[cfg(unix)]
            let loaded = {
                use std::os::unix::ffi::OsStrExt;
                let name = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
                unsafe { Library::open(&name) }
            };
            if let Ok(loaded) = loaded {
                library = Some(loaded);
                selected_path = Some(path.clone());
                break;
            }
        }
        let library = library.ok_or_else(|| format!("libdatachannel 0.24 could not be loaded from {}: {:?}; install the native media runtime or use BILIBILIVE_RTC_LIBRARY", selection.source, selection.candidates))?;
        let source = json!({"source":selection.source,"path":selected_path});
        macro_rules! sym {
            ($name:literal,$kind:ty) => {{
                let address = library
                    .symbol(CStr::from_bytes_with_nul(concat!($name, "\0").as_bytes()).unwrap())
                    .map_err(|e| format!("RTC symbol {}: {e}", $name))?;
                unsafe { std::mem::transmute::<*mut std::ffi::c_void, $kind>(address) }
            }};
        }
        let logger = sym!(
            "rtcInitLogger",
            unsafe extern "C" fn(c_int, Option<unsafe extern "C" fn(c_int, *const c_char)>)
        );
        unsafe {
            logger(2, Some(quiet_log));
        }
        let functions = Functions {
            create_pc: sym!("rtcCreatePeerConnection", CreatePc),
            create_channel: sym!("rtcCreateDataChannelEx", CreateChannel),
            delete_pc: sym!("rtcDeletePeerConnection", HandleFn),
            delete: sym!("rtcDelete", HandleFn),
            is_open: sym!("rtcIsOpen", IsOpen),
            get_local: sym!("rtcGetLocalDescription", Description),
            get_remote: sym!("rtcGetRemoteDescription", Description),
            set_local: sym!("rtcSetLocalDescription", SetLocal),
            set_remote: sym!("rtcSetRemoteDescription", SetRemote),
            add_track: sym!("rtcAddTrack", AddTrack),
            send: sym!("rtcSendMessage", SendMessage),
            receive: sym!("rtcReceiveMessage", ReceiveMessage),
            available: sym!("rtcGetAvailableAmount", HandleFn),
            chain_receiving: sym!("rtcChainRtcpReceivingSession", HandleFn),
            request_keyframe: sym!("rtcRequestKeyframe", HandleFn),
            set_user_pointer: sym!("rtcSetUserPointer", SetUserPointer),
            set_channel_callback: sym!("rtcSetDataChannelCallback", SetCreatedCallback),
            set_track_callback: sym!("rtcSetTrackCallback", SetCreatedCallback),
        };
        Ok(Self {
            _library: library,
            _search: search,
            source,
            functions,
        })
    }
    fn description(&self, id: c_int, remote: bool) -> Result<String, String> {
        let function = if remote {
            self.functions.get_remote
        } else {
            self.functions.get_local
        };
        let length = checked(
            unsafe { function(id, ptr::null_mut(), 0) },
            "description length",
        )?;
        if length as usize > wire::MAX_MESSAGE {
            return Err("Native RTC description exceeds limit".into());
        }
        let mut bytes = vec![0u8; length as usize + 1];
        let count = checked(
            unsafe { function(id, bytes.as_mut_ptr().cast(), bytes.len() as c_int) },
            "description",
        )?;
        bytes.truncate(count as usize);
        while bytes.last() == Some(&0) {
            bytes.pop();
        }
        String::from_utf8(bytes).map_err(|e| e.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct Track {
    pub id: c_int,
    pub mid: String,
    pub kind: String,
    pub codec: String,
    pub ssrc: u32,
    pub payload_type: u8,
    pub outgoing: bool,
    pub(crate) limits: Option<super::limits::VideoLimits>,
    pub(crate) sender: Arc<super::rtp::SenderState>,
}
struct Receiver {
    uid: u64,
    enabled: bool,
    negotiated: bool,
    track: Track,
    socket: UdpSocket,
    destination: SocketAddr,
    packets: u64,
    bytes: u64,
}
pub struct Session {
    pub native: Native,
    pub peer: c_int,
    pub channel: c_int,
    unreliable: c_int,
    pub ticket: Value,
    next_request: u64,
    pub joined: bool,
    pub published_video: bool,
    pub published_audio: bool,
    pub tracks: Vec<Track>,
    receivers: Vec<Receiver>,
    notifications: VecDeque<Value>,
    pub join_response: Value,
    remote_sdp: String,
    receive_buffer: Vec<u8>,
}
impl Session {
    pub fn connect(encoded: &str, uid: u64, channel: u64) -> Result<Self, String> {
        let ticket = wire::decode_ticket(encoded, uid, channel)?;
        let native = Native::load()?;
        let mut servers = Vec::new();
        let relay = ticket["turn"]["turn_only"].as_bool().unwrap_or(false);
        if let Some(addresses) = ticket["turn"]["addr"].as_array() {
            for address in addresses {
                let address = address.as_str().ok_or("Invalid TURN address")?;
                for transport in ticket["turn"]["transport"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_i64)
                {
                    let Some((scheme, protocol)) = (match transport {
                        0 => Some(("turn", "udp")),
                        1 => Some(("turn", "tcp")),
                        2 => Some(("turns", "tcp")),
                        _ => None,
                    }) else {
                        continue;
                    };
                    let username = ticket["turn"]["username"].as_str().unwrap_or("");
                    let password = ticket["turn"]["password"].as_str().unwrap_or("");
                    servers.push(cstring(&format!(
                        "{scheme}:{}:{}@{address}?transport={protocol}",
                        urlencode(username),
                        urlencode(password)
                    ))?);
                }
            }
        }
        if relay && servers.is_empty() {
            return Err("RTC ticket requires TURN relay but provides no usable transport".into());
        }
        let pointers: Vec<_> = servers.iter().map(|s| s.as_ptr()).collect();
        let configuration = Configuration {
            ice_servers: pointers.as_ptr(),
            ice_servers_count: pointers.len() as c_int,
            proxy_server: ptr::null(),
            bind_address: ptr::null(),
            certificate_type: 0,
            ice_transport_policy: if relay { 1 } else { 0 },
            enable_ice_tcp: false,
            enable_ice_udp_mux: false,
            disable_auto_negotiation: true,
            force_media_transport: true,
            port_range_begin: 0,
            port_range_end: 0,
            mtu: 1200,
            max_message_size: wire::MAX_MESSAGE as c_int,
        };
        let peer = checked(
            unsafe { (native.functions.create_pc)(&configuration) },
            "create peer",
        )?;
        let mut session = Self {
            native,
            peer,
            channel: -1,
            unreliable: -1,
            ticket,
            next_request: 1,
            joined: false,
            published_video: false,
            published_audio: false,
            tracks: Vec::new(),
            receivers: Vec::new(),
            notifications: VecDeque::new(),
            join_response: json!({}),
            remote_sdp: String::new(),
            receive_buffer: Vec::new(),
        };
        session.channel = session.create_channel(101, false)?;
        session.unreliable = session.create_channel(102, true)?;
        checked(
            unsafe { (session.native.functions.set_local)(peer, c"offer".as_ptr()) },
            "initial offer",
        )?;
        let local = session.native.description(peer, false)?;
        let mid = wire::application_mid(&local)?;
        session.remote_sdp = wire::initial_answer(&session.ticket, &mid)?;
        session.set_answer(&session.remote_sdp.clone())?;
        let deadline = Instant::now() + Duration::from_secs(20);
        while !unsafe { (session.native.functions.is_open)(session.channel) } {
            if Instant::now() > deadline {
                return Err("RTC ICE/DTLS/SCTP data channel timed out".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
        let response=session.request("join_req",json!({"user":{"uid":uid,"role":0,"platform":"pc_link","sdk_version":"2.7.5.60","codec_capability":{}},"field2_unknown":true,"token":session.ticket["token"]}),"join_resp")?;
        session.join_response = response;
        session.joined = true;
        Ok(session)
    }
    fn create_channel(&self, stream: u16, unreliable: bool) -> Result<c_int, String> {
        let init = DataChannelInit {
            reliability: Reliability {
                unordered: unreliable,
                unreliable,
                max_packet_life_time: 0,
                max_retransmits: 0,
            },
            protocol: ptr::null(),
            negotiated: false,
            manual_stream: true,
            stream,
        };
        let label = cstring(&format!("sctp_data_channel_{stream}"))?;
        checked(
            unsafe { (self.native.functions.create_channel)(self.peer, label.as_ptr(), &init) },
            "create data channel",
        )
    }
    fn set_answer(&self, sdp: &str) -> Result<(), String> {
        let sdp = cstring(sdp)?;
        checked(
            unsafe {
                (self.native.functions.set_remote)(self.peer, sdp.as_ptr(), c"answer".as_ptr())
            },
            "apply answer",
        )?;
        Ok(())
    }
    pub fn receive(&mut self) -> Result<Option<Value>, String> {
        let available = checked(
            unsafe { (self.native.functions.available)(self.channel) },
            "signaling queue",
        )?;
        if available == 0 {
            return Ok(None);
        }
        if self.receive_buffer.is_empty() {
            self.receive_buffer.resize(wire::MAX_MESSAGE, 0);
        }
        let mut size = self.receive_buffer.len() as c_int;
        let result = unsafe {
            (self.native.functions.receive)(
                self.channel,
                self.receive_buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if result == -3 {
            return Ok(None);
        }
        checked(result, "receive signaling")?;
        if size < 0 || size as usize > self.receive_buffer.len() {
            return Err("RTC signaling requires a bounded binary message".into());
        }
        wire::decode("ResponseMessage", &self.receive_buffer[..size as usize]).map(Some)
    }
    pub fn request(&mut self, kind: &str, payload: Value, expected: &str) -> Result<Value, String> {
        if !unsafe { (self.native.functions.is_open)(self.channel) } {
            return Err("RTC data channel is closed".into());
        }
        let id = self.next_request;
        self.next_request = self
            .next_request
            .checked_add(2)
            .ok_or("RTC request counter overflow")?;
        let mut message = json!({"request_id":id,"channel_id":self.ticket["channel_id"],"uid":self.ticket["uid"]});
        message[kind] = payload;
        let bytes = wire::encode("RequestMessage", &message)?;
        checked(
            unsafe {
                (self.native.functions.send)(
                    self.channel,
                    bytes.as_ptr().cast(),
                    bytes.len() as c_int,
                )
            },
            "send signaling",
        )?;
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            self.pump_media()?;
            if let Some(response) = self.receive()? {
                let response_id = response["request_id"].as_u64().unwrap_or(0);
                if response_id == id {
                    if response["code"].as_u64().unwrap_or(0) != 0 {
                        return Err(format!(
                            "RTC {kind} rejected ({}): {}",
                            response["code"],
                            response["message"].as_str().unwrap_or("")
                        ));
                    }
                    return response
                        .get(expected)
                        .cloned()
                        .ok_or_else(|| format!("RTC {kind} reply omitted {expected}"));
                }
                if response_id == 0 {
                    if self.notifications.len() >= 256 {
                        return Err("RTC notification queue exceeded capacity".into());
                    }
                    self.notifications.push_back(response);
                }
            }
            if Instant::now() > deadline {
                return Err(format!("RTC {kind} request timed out"));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    pub fn add_track(&mut self, kind: &str, codec: &str, outgoing: bool) -> Result<Track, String> {
        if !self.joined {
            return Err("RTC has not joined a channel".into());
        }
        let mid = (self.tracks.len() + 1).to_string();
        let ssrc = (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
            ^ (self.tracks.len() as u32 + 1).wrapping_mul(0x9e3779b9))
        .max(1);
        let (payload, rtpmap, fmtp) = match (kind, codec) {
            ("video", "h264") => (
                96,
                "H264/90000",
                "a=fmtp:96 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001f\r\n",
            ),
            ("video", "av1") => (
                98,
                "AV1/90000",
                "a=fmtp:98 profile=0;level-idx=9;tier=0\r\n",
            ),
            ("audio", "opus") => (
                111,
                "opus/48000/2",
                "a=fmtp:111 minptime=10;useinbandfec=1\r\n",
            ),
            _ => return Err("RTC codec must be h264/av1 video or opus audio".into()),
        };
        let direction = if outgoing { "sendonly" } else { "recvonly" };
        let sdp = format!(
            "m={kind} 9 UDP/TLS/RTP/SAVPF {payload}\r\nc=IN IP4 0.0.0.0\r\na=mid:{mid}\r\na={direction}\r\na=rtcp-mux\r\na=rtpmap:{payload} {rtpmap}\r\n{fmtp}a=rtcp-fb:{payload} nack\r\na=rtcp-fb:{payload} nack pli\r\na=rtcp-fb:{payload} ccm fir\r\na=ssrc:{ssrc} cname:bilibilive\r\na=ssrc:{ssrc} msid:bilibilive {kind}-{mid}\r\n"
        );
        let raw = cstring(&sdp)?;
        let id = checked(
            unsafe { (self.native.functions.add_track)(self.peer, raw.as_ptr()) },
            "add media track",
        )?;
        if !outgoing {
            checked(
                unsafe { (self.native.functions.chain_receiving)(id) },
                "attach RTCP receiver",
            )?;
        }
        let track = Track {
            id,
            mid,
            kind: kind.into(),
            codec: codec.into(),
            ssrc,
            payload_type: payload,
            outgoing,
            limits: None,
            sender: Arc::new(super::rtp::SenderState::new(ssrc)),
        };
        self.tracks.push(track.clone());
        Ok(track)
    }
    pub fn active_tracks(&self) -> Vec<Track> {
        self.tracks
            .iter()
            .filter(|track| {
                track.outgoing
                    && if track.kind == "video" {
                        self.published_video
                    } else {
                        self.published_audio
                    }
            })
            .cloned()
            .collect()
    }
    pub fn publish(&mut self, video: bool, audio: bool, codec: &str) -> Result<Value, String> {
        let mut added = Vec::new();
        for (kind, codec, enabled) in [("video", codec, video), ("audio", "opus", audio)] {
            if !enabled {
                continue;
            }
            if let Some(track) = self.tracks.iter().find(|t| t.outgoing && t.kind == kind) {
                if track.codec != codec {
                    return Err("Reconnect RTC before changing publication codec".into());
                }
                added.push(track.clone());
            } else {
                added.push(self.add_track(kind, codec, true)?);
            }
        }
        let mids = added.iter().map(|t| t.mid.clone()).collect::<Vec<_>>();
        let local = self.offer()?;
        let fragment = wire::media_fragment(&local, &mids)?;
        let response=self.request("pub_req",json!({"sdp":fragment,"stream":{"video":video,"audio":audio,"audio_muted":false,"video_muted":false,"spatial_layers":u8::from(video),"temporal_layers":u8::from(video)}}),"pub_resp")?;
        let answer = response["sdp"].as_str().unwrap_or("");
        if (video || audio) && answer.trim().is_empty() {
            return Err("RTC publication reply has no SDP".into());
        }
        for track in added.iter_mut().filter(|track| track.kind == "video") {
            let limits =
                super::limits::from_answer(&track.codec, &track.mid, track.payload_type, answer)?;
            if track
                .limits
                .as_ref()
                .is_some_and(|previous| previous != &limits)
            {
                return Err("RTC server changed an active publication's codec limits; reconnect the media session".into());
            }
            track.limits = Some(limits);
        }
        let merged = wire::merge_answer(&self.remote_sdp, &local, answer)?;
        self.set_answer(&merged)?;
        self.remote_sdp = merged;
        self.published_video = video;
        self.published_audio = audio;
        for track in &added {
            if let Some(saved) = self.tracks.iter_mut().find(|saved| saved.id == track.id) {
                saved.limits = track.limits.clone();
            }
        }
        Ok(
            json!({"published":true,"video":video,"audio":audio,"codec":codec,"tracks":added.iter().map(track_value).collect::<Vec<_>>()}),
        )
    }
    fn offer(&self) -> Result<String, String> {
        checked(
            unsafe { (self.native.functions.set_local)(self.peer, c"offer".as_ptr()) },
            "media offer",
        )?;
        self.native.description(self.peer, false)
    }
    pub fn subscribe(
        &mut self,
        uid: u64,
        video: bool,
        audio: bool,
        video_port: u16,
        audio_port: u16,
    ) -> Result<Value, String> {
        if uid == 0 || self.ticket["uid"].as_u64() == Some(uid) {
            return Err("RTC peer UID must identify another user".into());
        }
        if (video && video_port == 0)
            || (audio && audio_port == 0)
            || (video && audio && video_port == audio_port)
        {
            return Err("RTP ports must be nonzero and distinct".into());
        }
        if !video && !audio {
            return self.update_subscription(uid, false, false);
        }
        let mut selected = Vec::new();
        for (kind, codec, enabled, port) in [
            ("video", "h264", video, video_port),
            ("audio", "opus", audio, audio_port),
        ] {
            if !enabled {
                continue;
            }
            if let Some(receiver) = self
                .receivers
                .iter_mut()
                .find(|r| r.uid == uid && r.track.kind == kind)
            {
                receiver.destination = SocketAddr::from(([127, 0, 0, 1], port));
                selected.push(receiver.track.clone());
            } else {
                let socket = UdpSocket::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
                let track = self.add_track(kind, codec, false)?;
                self.receivers.push(Receiver {
                    uid,
                    enabled: false,
                    negotiated: false,
                    track: track.clone(),
                    socket,
                    destination: SocketAddr::from(([127, 0, 0, 1], port)),
                    packets: 0,
                    bytes: 0,
                });
                selected.push(track);
            }
        }
        let reused = self
            .receivers
            .iter()
            .filter(|r| r.uid == uid && selected.iter().any(|t| t.id == r.track.id))
            .all(|r| r.negotiated);
        if !reused {
            let local = self.offer()?;
            let fragment = wire::media_fragment(
                &local,
                &selected.iter().map(|t| t.mid.clone()).collect::<Vec<_>>(),
            )?;
            let response=self.request("sub_req",json!({"sdp":fragment,"users":[{"uid":uid,"video":video,"audio":audio,"field4_unknown":0}],"field3_unknown":true}),"sub_resp")?;
            let answer = response["sdp"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .ok_or("RTC subscription reply has no SDP")?;
            let merged = wire::merge_answer(&self.remote_sdp, &local, answer)?;
            self.set_answer(&merged)?;
            self.remote_sdp = merged;
            for receiver in self
                .receivers
                .iter_mut()
                .filter(|r| r.uid == uid && selected.iter().any(|t| t.id == r.track.id))
            {
                receiver.negotiated = true;
            }
        }
        self.update_subscription(uid, video, audio)?;
        for track in &selected {
            if track.kind == "video" {
                unsafe {
                    (self.native.functions.request_keyframe)(track.id);
                }
            }
        }
        Ok(
            json!({"subscribed":uid,"video":video,"audio":audio,"receiver_sdp":receiver_sdp(&selected,video_port,audio_port),"video_port":video_port,"audio_port":audio_port,"reused":reused}),
        )
    }
    pub fn update_subscription(
        &mut self,
        uid: u64,
        video: bool,
        audio: bool,
    ) -> Result<Value, String> {
        if !self.receivers.iter().any(|r| r.uid == uid) {
            return Err("RTC peer has no existing subscription".into());
        }
        if video
            && !self
                .receivers
                .iter()
                .any(|r| r.uid == uid && r.negotiated && r.track.kind == "video")
            || audio
                && !self
                    .receivers
                    .iter()
                    .any(|r| r.uid == uid && r.negotiated && r.track.kind == "audio")
        {
            return Err("Use subscribe to negotiate the additional media track".into());
        }
        // Local delivery stops immediately; a timeout must not keep an old peer visible.
        for receiver in self.receivers.iter_mut().filter(|r| r.uid == uid) {
            if !(if receiver.track.kind == "video" {
                video
            } else {
                audio
            }) {
                receiver.enabled = false;
            }
        }
        self.request(
            "update_sub_req",
            json!({"uid":uid,"audio":audio,"video":video}),
            "update_sub_resp",
        )?;
        for receiver in self.receivers.iter_mut().filter(|r| r.uid == uid) {
            receiver.enabled = if receiver.track.kind == "video" {
                video
            } else {
                audio
            };
        }
        Ok(json!({"uid":uid,"video":video,"audio":audio,"active":video||audio}))
    }
    pub fn pump_media(&mut self) -> Result<(), String> {
        let mut buffer = [0u8; 65536];
        for receiver in &mut self.receivers {
            for _ in 0..128 {
                let mut size = buffer.len() as c_int;
                let status = unsafe {
                    (self.native.functions.receive)(
                        receiver.track.id,
                        buffer.as_mut_ptr().cast(),
                        &mut size,
                    )
                };
                if status == -3 {
                    break;
                }
                checked(status, "receive RTP")?;
                if size <= 0 || size as usize > buffer.len() {
                    return Err("Invalid native RTP packet size".into());
                }
                if receiver.enabled {
                    receiver
                        .socket
                        .send_to(&buffer[..size as usize], receiver.destination)
                        .map_err(|e| e.to_string())?;
                    receiver.packets += 1;
                    receiver.bytes += size as u64;
                }
            }
        }
        for track in self.tracks.iter().filter(|track| track.outgoing) {
            for _ in 0..16 {
                let mut size = buffer.len() as c_int;
                let status = unsafe {
                    (self.native.functions.receive)(track.id, buffer.as_mut_ptr().cast(), &mut size)
                };
                if status == -3 {
                    break;
                }
                checked(status, "receive sender feedback")?;
                if size <= 0 || size as usize > buffer.len() {
                    return Err("Invalid RTCP feedback packet size".into());
                }
                for repair in track.sender.feedback(&buffer[..size as usize])? {
                    checked(
                        unsafe {
                            (self.native.functions.send)(
                                track.id,
                                repair.as_ptr().cast(),
                                repair.len() as c_int,
                            )
                        },
                        "retransmit RTP",
                    )?;
                }
            }
        }
        Ok(())
    }
    pub fn poll(&mut self) -> Result<(), String> {
        self.pump_media()?;
        for _ in 0..16 {
            let Some(message) = self.receive()? else {
                break;
            };
            if self.notifications.len() == 256 {
                self.notifications.pop_front();
            }
            self.notifications.push_back(message);
        }
        Ok(())
    }
    pub fn events(&mut self) -> Value {
        Value::Array(self.notifications.drain(..).collect())
    }
    pub fn status(&self) -> Value {
        json!({"connected":unsafe{(self.native.functions.is_open)(self.channel)},"joined":self.joined,"publication":{"video":self.published_video,"audio":self.published_audio},"uid":self.ticket["uid"],"channel_id":self.ticket["channel_id"],"backend":"libdatachannel-0.24","tracks":self.tracks.iter().map(track_value).collect::<Vec<_>>(),"received":self.receivers.iter().map(|r|json!({"uid":r.uid,"enabled":r.enabled,"negotiated":r.negotiated,"mid":r.track.mid,"packets":r.packets,"bytes":r.bytes,"destination":r.destination.to_string()})).collect::<Vec<_>>()})
    }
    pub fn mute(&mut self, kind: &str, muted: bool) -> Result<Value, String> {
        if !matches!(kind, "audio" | "video") {
            return Err("RTC mute kind must be audio or video".into());
        }
        let mut payload = json!({});
        payload[format!("{kind}_muted")] = muted.into();
        self.request("mute_req", payload, "user_mute_resp")
    }
    pub fn leave(&mut self) -> Result<Value, String> {
        if self.joined {
            self.joined = false;
            self.request("leave_req", json!({}), "leave_resp")
        } else {
            Ok(json!({}))
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        for track in &self.tracks {
            unsafe {
                (self.native.functions.delete)(track.id);
            }
        }
        if self.channel >= 0 {
            unsafe {
                (self.native.functions.delete)(self.channel);
            }
        }
        if self.unreliable >= 0 {
            unsafe {
                (self.native.functions.delete)(self.unreliable);
            }
        }
        unsafe {
            (self.native.functions.delete_pc)(self.peer);
        }
    }
}
fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn track_value(track: &Track) -> Value {
    json!({"id":track.id,"mid":track.mid,"kind":track.kind,"codec":track.codec,"ssrc":track.ssrc,"payload_type":track.payload_type,"outgoing":track.outgoing,"limits":track.limits.as_ref().map(super::limits::VideoLimits::snapshot)})
}
fn receiver_sdp(tracks: &[Track], video_port: u16, audio_port: u16) -> String {
    let mut sdp =
        "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=Bilibilive PK\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\n"
            .to_string();
    for t in tracks {
        let port = if t.kind == "video" {
            video_port
        } else {
            audio_port
        };
        let codec = if t.kind == "video" {
            "H264/90000"
        } else {
            "opus/48000/2"
        };
        sdp += &format!(
            "m={} {port} RTP/AVP {}\r\na=rtpmap:{} {codec}\r\na=recvonly\r\n",
            t.kind, t.payload_type, t.payload_type
        );
        if t.kind == "video" {
            sdp += &format!("a=fmtp:{} packetization-mode=1\r\n", t.payload_type);
        }
    }
    sdp
}

pub fn doctor() -> Value {
    match Native::load() {
        Ok(native) => {
            json!({"available":true,"backend":"libdatachannel-0.24","data_channel_ids":[101,102],"native":true,"runtime":native.source})
        }
        Err(error) => json!({"available":false,"backend":"libdatachannel-0.24","error":error}),
    }
}

/// Exercises real ICE, DTLS, SCTP and SRTP between two local native peers.
/// No Bilibili account or external service is contacted.
pub fn self_test() -> Result<Value, String> {
    use std::sync::atomic::{AtomicI32, Ordering};
    struct Accepted {
        channel: AtomicI32,
        track: AtomicI32,
    }
    unsafe extern "C" fn channel_created(_: c_int, id: c_int, ptr: *mut std::ffi::c_void) {
        if !ptr.is_null() {
            unsafe { &*ptr.cast::<Accepted>() }
                .channel
                .store(id, Ordering::Release);
        }
    }
    unsafe extern "C" fn track_created(_: c_int, id: c_int, ptr: *mut std::ffi::c_void) {
        if !ptr.is_null() {
            unsafe { &*ptr.cast::<Accepted>() }
                .track
                .store(id, Ordering::Release);
        }
    }
    struct PeerGuard {
        functions: Functions,
        id: c_int,
        children: Vec<c_int>,
    }
    impl Drop for PeerGuard {
        fn drop(&mut self) {
            unsafe {
                (self.functions.set_channel_callback)(self.id, None);
                (self.functions.set_track_callback)(self.id, None);
            }
            for id in &self.children {
                unsafe {
                    (self.functions.delete)(*id);
                }
            }
            unsafe {
                (self.functions.delete_pc)(self.id);
            }
        }
    }
    fn wait_for(mut condition: impl FnMut() -> bool, reason: &str) -> Result<(), String> {
        let until = Instant::now() + Duration::from_secs(10);
        while !condition() {
            if Instant::now() > until {
                return Err(format!("Native RTC fixture timed out: {reason}"));
            }
            thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }
    fn receive(functions: Functions, id: c_int) -> Result<Vec<u8>, String> {
        let mut data = vec![0; wire::MAX_MESSAGE];
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let mut size = data.len() as c_int;
            let status = unsafe { (functions.receive)(id, data.as_mut_ptr().cast(), &mut size) };
            if status != -3 {
                checked(status, "fixture receive")?;
                if size < 0 || size as usize > data.len() {
                    return Err("Fixture received invalid binary size".into());
                }
                data.truncate(size as usize);
                return Ok(data);
            }
            if Instant::now() > until {
                return Err("Native RTC fixture receive timed out".into());
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    let native = Native::load()?;
    let f = native.functions;
    let state = Box::new(Accepted {
        channel: AtomicI32::new(-1),
        track: AtomicI32::new(-1),
    });
    let config = Configuration {
        ice_servers: ptr::null(),
        ice_servers_count: 0,
        proxy_server: ptr::null(),
        bind_address: ptr::null(),
        certificate_type: 0,
        ice_transport_policy: 0,
        enable_ice_tcp: false,
        enable_ice_udp_mux: false,
        disable_auto_negotiation: true,
        force_media_transport: true,
        port_range_begin: 0,
        port_range_end: 0,
        mtu: 1200,
        max_message_size: wire::MAX_MESSAGE as c_int,
    };
    let mut client = PeerGuard {
        functions: f,
        id: checked(unsafe { (f.create_pc)(&config) }, "fixture client")?,
        children: Vec::new(),
    };
    let mut server = PeerGuard {
        functions: f,
        id: checked(unsafe { (f.create_pc)(&config) }, "fixture server")?,
        children: Vec::new(),
    };
    unsafe {
        (f.set_user_pointer)(server.id, (&*state as *const Accepted).cast_mut().cast());
    }
    checked(
        unsafe { (f.set_channel_callback)(server.id, Some(channel_created)) },
        "fixture channel callback",
    )?;
    checked(
        unsafe { (f.set_track_callback)(server.id, Some(track_created)) },
        "fixture track callback",
    )?;
    let init = DataChannelInit {
        reliability: Reliability {
            unordered: false,
            unreliable: false,
            max_packet_life_time: 0,
            max_retransmits: 0,
        },
        protocol: ptr::null(),
        negotiated: false,
        manual_stream: true,
        stream: 101,
    };
    let channel = checked(
        unsafe { (f.create_channel)(client.id, c"sctp_data_channel_101".as_ptr(), &init) },
        "fixture channel 101",
    )?;
    client.children.push(channel);
    checked(
        unsafe { (f.set_local)(client.id, c"offer".as_ptr()) },
        "fixture offer",
    )?;
    wait_for(
        || {
            native
                .description(client.id, false)
                .is_ok_and(|s| s.contains("a=candidate:"))
        },
        "client ICE candidates",
    )?;
    let offer = cstring(&native.description(client.id, false)?)?;
    checked(
        unsafe { (f.set_remote)(server.id, offer.as_ptr(), c"offer".as_ptr()) },
        "fixture remote offer",
    )?;
    checked(
        unsafe { (f.set_local)(server.id, c"answer".as_ptr()) },
        "fixture answer",
    )?;
    wait_for(
        || {
            native
                .description(server.id, false)
                .is_ok_and(|s| s.contains("a=candidate:"))
        },
        "server ICE candidates",
    )?;
    let answer = cstring(&native.description(server.id, false)?)?;
    checked(
        unsafe { (f.set_remote)(client.id, answer.as_ptr(), c"answer".as_ptr()) },
        "fixture remote answer",
    )?;
    wait_for(
        || unsafe { (f.is_open)(channel) } && state.channel.load(Ordering::Acquire) >= 0,
        "in-band DCEP stream 101",
    )?;
    let remote_channel = state.channel.load(Ordering::Acquire);
    server.children.push(remote_channel);
    let request = wire::encode(
        "RequestMessage",
        &json!({"request_id":1,"channel_id":42,"uid":7,"leave_req":{}}),
    )?;
    checked(
        unsafe { (f.send)(channel, request.as_ptr().cast(), request.len() as c_int) },
        "fixture SCTP request",
    )?;
    let received = wire::decode("RequestMessage", &receive(f, remote_channel)?)?;
    if received["request_id"] != 1 || received.get("leave_req").is_none() {
        return Err("RTC fixture protobuf request mismatch".into());
    }
    let response = wire::encode(
        "ResponseMessage",
        &json!({"request_id":1,"leave_resp":{"hex":""}}),
    )?;
    checked(
        unsafe {
            (f.send)(
                remote_channel,
                response.as_ptr().cast(),
                response.len() as c_int,
            )
        },
        "fixture SCTP reply",
    )?;
    let received = wire::decode("ResponseMessage", &receive(f, channel)?)?;
    if received.get("leave_resp").is_none() {
        return Err("RTC fixture reply mismatch".into());
    }
    let track_sdp=c"m=video 9 UDP/TLS/RTP/SAVPF 96\r\nc=IN IP4 0.0.0.0\r\na=mid:1\r\na=sendonly\r\na=rtcp-mux\r\na=rtpmap:96 H264/90000\r\na=fmtp:96 packetization-mode=1;profile-level-id=42001f\r\na=ssrc:1234 cname:bilibilive-fixture\r\n";
    let track = checked(
        unsafe { (f.add_track)(client.id, track_sdp.as_ptr()) },
        "fixture video track",
    )?;
    client.children.push(track);
    checked(
        unsafe { (f.set_local)(client.id, c"offer".as_ptr()) },
        "fixture media offer",
    )?;
    let offer = cstring(&native.description(client.id, false)?)?;
    checked(
        unsafe { (f.set_remote)(server.id, offer.as_ptr(), c"offer".as_ptr()) },
        "fixture media remote offer",
    )?;
    checked(
        unsafe { (f.set_local)(server.id, c"answer".as_ptr()) },
        "fixture media answer",
    )?;
    let answer = cstring(&native.description(server.id, false)?)?;
    checked(
        unsafe { (f.set_remote)(client.id, answer.as_ptr(), c"answer".as_ptr()) },
        "fixture media remote answer",
    )?;
    wait_for(
        || unsafe { (f.is_open)(track) } && state.track.load(Ordering::Acquire) >= 0,
        "SRTP video track",
    )?;
    let remote_track = state.track.load(Ordering::Acquire);
    server.children.push(remote_track);
    let rtp = [
        0x80, 0xe0, 0, 1, 0, 0, 0, 1, 0, 0, 4, 0xd2, 0x65, 0x88, 0x84, 0x00,
    ];
    checked(
        unsafe { (f.send)(track, rtp.as_ptr().cast(), rtp.len() as c_int) },
        "fixture SRTP packet",
    )?;
    let received = receive(f, remote_track)?;
    if received != rtp {
        return Err("Native SRTP round trip changed the RTP packet".into());
    }
    let encoded = super::capture::encode_fixture(
        f,
        &Track {
            id: track,
            mid: "1".into(),
            kind: "video".into(),
            codec: "h264".into(),
            ssrc: 1234,
            payload_type: 96,
            outgoing: true,
            limits: Some(super::limits::VideoLimits::h264_default()),
            sender: Arc::new(super::rtp::SenderState::new(1234)),
        },
    )?;
    let encoded_rtp = loop {
        let packet = receive(f, remote_track)?;
        if !super::rtp::is_rtcp(&packet) {
            break packet;
        }
    };
    if encoded_rtp.len() < 13 || encoded_rtp[0] >> 6 != 2 || encoded_rtp[1] & 127 != 96 {
        return Err("Encoded H.264 did not arrive through the native SRTP track".into());
    }
    Ok(
        json!({"passed":true,"backend":"libdatachannel-0.24","in_band_data_channel":101,"protobuf_request_reply":true,"ice_dtls_sctp":true,"srtp_rtp_roundtrip":true,"ffmpeg":encoded}),
    )
}
