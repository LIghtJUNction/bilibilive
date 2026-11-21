//! PK business events and native RTC admission, kept separate from PK rounds.
//!
//! A battle ending does not end the interaction. Incoming media is admitted only
//! after the platform ticket, current identity and business membership agree.
use crate::{
    bili::BiliClient,
    media::{MediaManager, wire},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, String>;
type Effect<'a> = dyn FnMut(&str, &Value) -> Result<Value> + 'a;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Stream {
    audio: bool,
    video: bool,
}

#[derive(Clone, Debug)]
struct Control {
    kind: &'static str,
    version: i64,
    local: Option<bool>,
    remote: Option<BTreeSet<u64>>,
}

struct Context {
    uid: u64,
    channel: u64,
    session: String,
    label: String,
    ticket_fingerprint: String,
    default_sub_mode: u8,
    publication: Stream,
    business_channel: Option<String>,
    room_id: u64,
    version: i64,
    members: BTreeSet<u64>,
    publications: BTreeMap<u64, Stream>,
    selected: Option<(u64, Stream)>,
    controls: BTreeMap<&'static str, Control>,
    local_muted: BTreeMap<&'static str, bool>,
    stop_polling: bool,
    next_poll: Instant,
}

/// Own one interaction per account. The caller must serialize this object and
/// invoke its effectful methods only after explicit PK-media automation opt-in.
#[derive(Default)]
pub struct PkSession {
    current: Option<Context>,
    retired: VecDeque<(u64, String, u64)>,
    expected_peer: Option<u64>,
    expected_session: Option<String>,
    admission_blocked: bool,
    media_preferences: Value,
    generation: u64,
    round: Value,
    last_error: Option<String>,
}

impl PkSession {
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate with the same preference contract used by CLI/MCP/GUI. Changes
    /// are applied to the next publication, never to a running encoder silently.
    pub fn set_media_preferences(&mut self, preferences: Value) -> Result<()> {
        let mut config = crate::config::Config::default();
        config.set("pk_media", &preferences)?;
        self.media_preferences = config.pk_media;
        Ok(())
    }

    /// Call before accepting an invitation; clear it if the HTTP acceptance fails.
    pub fn expect_invitation(&mut self, session: &str, peer_uid: &str) -> Result<()> {
        if session.is_empty() || session.len() > 1024 {
            return Err("Invitation requires its original business session".into());
        }
        self.expected_peer = Some(decimal(peer_uid, "peer UID")?);
        self.expected_session = Some(session.into());
        self.admission_blocked = false;
        Ok(())
    }

    /// Outgoing invitations know the peer before the platform assigns a session.
    pub fn expect_peer(&mut self, peer_uid: &str) -> Result<()> {
        self.expected_peer = Some(decimal(peer_uid, "peer UID")?);
        self.expected_session = None;
        self.admission_blocked = false;
        Ok(())
    }

    pub fn clear_expectation(&mut self) {
        self.expected_peer = None;
        self.expected_session = None;
    }

    /// A fresh explicit random-match request can admit a new server-selected peer.
    pub fn expect_match(&mut self) {
        self.clear_expectation();
        self.admission_blocked = false;
    }

    /// Stop accepting pending JOINs while leaving an established RTC owner and
    /// its peer/session checks intact. Only a new explicit request reopens this.
    pub fn cancel_pending(&mut self) {
        self.admission_blocked = true;
        if self.current.is_none() {
            self.clear_expectation();
        }
    }

    pub fn snapshot(&self) -> Value {
        let interaction=self.current.as_ref().map(|context|json!({
            "uid":context.uid,"channel_id":context.channel,"session_id":context.session,"business":context.label,
            "generation":self.generation,"joined":true,"publication":{"audio":context.publication.audio,"video":context.publication.video},
            "business_channel":context.business_channel,"room_version":context.version,"members":context.members,
            "peer":context.selected.as_ref().map(|(uid,stream)|json!({"uid":uid,"audio":stream.audio,"video":stream.video})),
            "polling":!context.stop_polling,
        }));
        let mut value = json!({"interaction":interaction,"round":self.round,"accepting_join":!self.admission_blocked,"expected_peer":self.expected_peer,
            "expected_session":self.expected_session,"last_error":self.last_error});
        sanitize(&mut value);
        value
    }

    pub fn handle_event(
        &mut self,
        media: &MediaManager,
        bili: &BiliClient,
        event: &Value,
        uid: &str,
        buvid: &str,
    ) -> Result<Value> {
        let event_buvid = event
            .pointer("/data/config/buvid")
            .and_then(Value::as_str)
            .unwrap_or("");
        // Native admits either its app device ID or the account's Cookie buvid3.
        let device = if bili.device_buvid().is_empty() {
            ""
        } else if event_buvid == bili.device_buvid() {
            bili.device_buvid()
        } else {
            buvid
        };
        self.handle_with(
            &mut |method, args| media.call(method, args),
            &mut |method, args| bili.call(method, args),
            event,
            uid,
            device,
        )
    }

    /// `pk.accept`/Handle HTTP success can carry the JOIN without a push event.
    /// A success without a ticket remains an accepted invitation, not a connection.
    pub fn handle_response(
        &mut self,
        media: &MediaManager,
        bili: &BiliClient,
        response: &Value,
        uid: &str,
        buvid: &str,
    ) -> Result<Value> {
        match join_from_response(response) {
            Some(event) => self.handle_event(media, bili, &event, uid, buvid),
            None => Ok(json!({"handled":false,"status":"awaiting_join","state":self.snapshot()})),
        }
    }

    /// Drain native notifications and reconcile the platform room snapshot at
    /// most every two seconds. The caller drives this from its active event loop.
    pub fn poll_media(
        &mut self,
        media: &MediaManager,
        bili: &BiliClient,
        uid: &str,
        buvid: &str,
    ) -> Result<Value> {
        let result = self.poll_with(
            &mut |method, args| media.call(method, args),
            &mut |method, args| bili.call(method, args),
            uid,
            buvid,
        );
        if let Err(error) = &result {
            self.last_error = Some(error.clone());
            // Lost signaling or an invalid current business snapshot cannot
            // leave an unowned capture/publication running in the background.
            let _ = self.retire(&mut |method, args| media.call(method, args));
        }
        result
    }

    /// Account switches, explicit disconnection and same-session LEAVE all retire
    /// the current owner; later duplicate JOINs cannot resurrect that owner.
    pub fn reset(&mut self, media: &MediaManager) -> Result<Value> {
        let result = self.retire(&mut |method, args| media.call(method, args));
        self.clear_expectation();
        self.round = Value::Null;
        result.map(|_| json!({"disconnected":true,"state":self.snapshot()}))
    }

    fn handle_with(
        &mut self,
        media: &mut Effect<'_>,
        rest: &mut Effect<'_>,
        event: &Value,
        uid: &str,
        buvid: &str,
    ) -> Result<Value> {
        let uid = decimal(uid, "current account UID")?;
        if self
            .current
            .as_ref()
            .is_some_and(|context| context.uid != uid)
        {
            return Err("PK account changed; retire the previous media session first".into());
        }
        let command = event["cmd"]
            .as_str()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("");
        let mut actions = Vec::new();
        let handled = match command {
            "UNIVERSAL_INTERACT_JOIN" => {
                self.join(media, rest, &event["data"], uid, buvid, &mut actions)?
            }
            "UNIVERSAL_INTERACT_LEAVE" => {
                let data = &event["data"];
                let accepted = self.current.as_ref().is_some_and(|context| {
                    id(&data["rtc_channel_id"], "RTC channel").ok() == Some(context.channel)
                        && optional_text_matches(data.get("biz_session_id"), &context.session)
                        && optional_text_matches(data.get("biz_label"), &context.label)
                });
                if accepted {
                    self.retire(media)?;
                    actions
                        .push(json!({"operation":"rtc.disconnect","reason":"interaction_leave"}));
                }
                accepted
            }
            "PK_BATTLE_PRE" | "PK_BATTLE_START" | "PK_BATTLE_END" => {
                // The round ID is at the root, not inside the business session.
                let pk_id = event.get("pk_id").cloned().unwrap_or(Value::Null);
                self.round = json!({"event":command,"pk_id":pk_id,"battle_type":event["data"].get("battle_type").cloned().unwrap_or(json!(0)),
                    "ended":command=="PK_BATTLE_END"});
                true
            }
            "PK_INFO" => {
                self.round = event["data"].clone();
                true
            }
            "UNIVERSAL_INTERACT_STREAM_CONTROL" => {
                let data = &event["data"];
                let accepted = self.current.as_ref().is_some_and(|context| {
                    (id(&data["rtc_channel_id"], "RTC channel").ok() == Some(context.channel)
                        || data["biz_session_id"].as_str() == Some(context.session.as_str()))
                        && (data.get("uid").is_none_or(Value::is_null)
                            || id(&data["uid"], "UID").ok() == Some(context.uid))
                });
                if accepted {
                    let controls = parse_controls(&data["stream_control"])?;
                    self.apply_controls(media, controls, &mut actions)?;
                }
                accepted
            }
            _ => false,
        };
        let mut result =
            json!({"handled":handled,"event":command,"actions":actions,"state":self.snapshot()});
        sanitize(&mut result);
        Ok(result)
    }

    fn join(
        &mut self,
        media: &mut Effect<'_>,
        rest: &mut Effect<'_>,
        data: &Value,
        uid: u64,
        buvid: &str,
        actions: &mut Vec<Value>,
    ) -> Result<bool> {
        if self.admission_blocked {
            return Ok(false);
        }
        let config = data
            .get("config")
            .filter(|v| v.is_object())
            .ok_or("PK JOIN lacks config")?;
        let event_buvid = config.get("buvid").and_then(Value::as_str).unwrap_or("");
        if !event_buvid.is_empty() && !buvid.is_empty() && event_buvid != buvid {
            return Ok(false);
        }
        if id(&config["uid"], "JOIN UID")? != uid {
            return Err("PK JOIN belongs to a different account".into());
        }
        let channel = id(&config["rtc_channel_id"], "RTC channel")?;
        let session = nonempty(config, "biz_session_id")?.to_owned();
        let label = nonempty(config, "biz_label")?.to_owned();
        if self.retired.contains(&(uid, session.clone(), channel)) {
            return Ok(false);
        }
        if self
            .expected_session
            .as_deref()
            .is_some_and(|expected| expected != session)
        {
            return Err("PK JOIN does not match the accepted invitation session".into());
        }
        let token = nonempty(config, "join_token")?;
        wire::decode_ticket(token, uid, channel)?;
        let fingerprint = Sha256::digest(token.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if let Some(context) = &self.current {
            if context.session == session
                && context.channel == channel
                && context.ticket_fingerprint == fingerprint
            {
                return Ok(false);
            }
            return Err(
                "A different PK media session is already active; leave it before joining another"
                    .into(),
            );
        }
        let extra = data
            .get("biz_extra")
            .filter(|v| v.is_object())
            .ok_or("PK JOIN lacks biz_extra")?;
        match label.as_str() {
            "classic_pk" => (),
            "universal_multi_conn" => {
                let template = &extra["interact_scene_configuration"]["interact_template"];
                if !matches!(
                    template["template_id"].as_str(),
                    Some("multi_conn_grid" | "multi_conn_spotlight")
                ) || !["width", "height"].iter().all(|key| {
                    template["layout_data"][*key]
                        .as_u64()
                        .is_some_and(|n| n > 0 && n < (1 << 31))
                }) {
                    return Err(
                        "PK JOIN has an unsupported video template or invalid dimensions".into(),
                    );
                }
            }
            _ => return Err("PK JOIN uses an unsupported interaction business".into()),
        }
        let control = data
            .get("stream_control")
            .filter(|v| v.is_object())
            .ok_or("PK JOIN lacks stream_control")?;
        let baseline = parse_controls(control)?;
        let publication = publication(control)?;
        if label == "classic_pk" && (!publication.audio || publication.video) {
            return Err("Classic PK requires audio-only publication".into());
        }
        let default_sub_mode = config["default_sub_mode"]
            .as_u64()
            .filter(|n| *n <= 3)
            .ok_or("PK JOIN lacks a valid default_sub_mode (0..3)")?
            as u8;
        // Validate the complete JOIN before loading media libraries or publishing.
        let connected = media(
            "connect",
            &json!({"ticket":token,"uid":uid,"channel_id":channel}),
        )?;
        push_action(actions, "rtc.connect", connected);
        self.generation = self.generation.saturating_add(1);
        self.current = Some(Context {
            uid,
            channel,
            session,
            label,
            ticket_fingerprint: fingerprint,
            default_sub_mode,
            publication,
            business_channel: None,
            room_id: 0,
            version: 0,
            members: BTreeSet::new(),
            publications: BTreeMap::new(),
            selected: None,
            controls: BTreeMap::new(),
            local_muted: BTreeMap::new(),
            stop_polling: false,
            next_poll: Instant::now(),
        });
        let bootstrap: Result<()> = (|| {
            let mut publish = if self.media_preferences.is_object() {
                self.media_preferences.clone()
            } else {
                json!({})
            };
            publish["audio"] = publication.audio.into();
            publish["video"] = publication.video.into();
            publish["capture"] = true.into();
            publish
                .as_object_mut()
                .unwrap()
                .entry("codec")
                .or_insert(json!("h264"));
            let published = media("publish", &publish)?;
            push_action(actions, "rtc.publish", published);
            // JoinResp.users is explicitly NOT treated as a publication callback.
            let members = media("members", &json!({}))?;
            self.accept_members(&members)?;
            let room = rest("room.get", &json!({}))?;
            self.current.as_mut().unwrap().room_id = id(&room["room_id"], "current room")?;
            self.refresh_room(media, rest, actions)?;
            self.apply_controls(media, baseline, actions)?;
            self.reconcile(media, actions)?;
            if self.current.as_ref().unwrap().label == "universal_multi_conn" {
                // Post-publication native action4 intentionally has empty session.
                match rest(
                    "pk.request",
                    &json!({"operation":"handle","fields":{"peer_uid":"0","action":"4","action_type":"0","source":"0","biz_session_id":""}}),
                ) {
                    Ok(value) => {
                        if let Some(join) = join_from_response(&value) {
                            // A duplicate is normal. A replacement must not silently
                            // take over a media owner that was just established.
                            self.join(media, rest, &join["data"], uid, buvid, actions)?;
                        }
                        push_action(actions, "pk.publication_ready", value);
                    }
                    Err(error) => actions
                        .push(json!({"operation":"pk.publication_ready","ok":false,"error":error})),
                }
            }
            Ok(())
        })();
        if let Err(error) = bootstrap {
            self.last_error = Some(error.clone());
            let _ = self.retire(media);
            return Err(format!("PK media setup stopped: {error}"));
        }
        self.last_error = None;
        Ok(true)
    }

    fn accept_members(&mut self, value: &Value) -> Result<()> {
        let users = value["users"]
            .as_array()
            .ok_or("RTC MembersResp lacks users")?;
        if users.len() > 256 {
            return Err("RTC member count exceeds 256".into());
        }
        let context = self.current.as_mut().ok_or("PK session is not active")?;
        for user in users {
            let uid = id(&user["uid"], "RTC member UID")?;
            if uid != context.uid
                && let Some(stream) = user.get("stream")
            {
                context.publications.insert(uid, stream_flags(stream)?);
            }
        }
        Ok(())
    }

    fn refresh_room(
        &mut self,
        media: &mut Effect<'_>,
        rest: &mut Effect<'_>,
        actions: &mut Vec<Value>,
    ) -> Result<()> {
        let Some(context) = self.current.as_mut() else {
            return Ok(());
        };
        if context.stop_polling {
            return Ok(());
        }
        context.next_poll = Instant::now() + Duration::from_secs(2);
        let response = rest(
            "pk.request",
            &json!({"operation":"universal_info","fields":{"biz_session_id":context.session,"room_id":context.room_id}}),
        )?;
        self.accept_room(media, &response, true, actions)
    }

    fn accept_room(
        &mut self,
        media: &mut Effect<'_>,
        data: &Value,
        from_http: bool,
        actions: &mut Vec<Value>,
    ) -> Result<()> {
        let context = self.current.as_ref().ok_or("PK session is not active")?;
        let matches = data["biz_session_id"].as_str() == Some(context.session.as_str())
            && data["business_label"].as_str() == Some(context.label.as_str());
        if !matches {
            return if from_http {
                Err("PK room snapshot does not match the current business session".into())
            } else {
                Ok(())
            };
        }
        let channel = nonempty(data, "interact_channel_id")?;
        if context
            .business_channel
            .as_deref()
            .is_some_and(|current| current != channel)
        {
            return if from_http {
                Err("PK business channel changed during an active interaction".into())
            } else {
                Ok(())
            };
        }
        if !from_http && context.business_channel.is_none() {
            return Ok(());
        }
        // room_status=2 stops room polling only. It never synthesizes RTC leave.
        if from_http && data["room_status"].as_i64() == Some(2) {
            let context = self.current.as_mut().unwrap();
            context.business_channel = Some(channel.into());
            context.stop_polling = true;
            return Ok(());
        }
        let version = data["version"]
            .as_i64()
            .ok_or("PK room version must be an integer")?;
        if version <= context.version {
            if from_http {
                self.current.as_mut().unwrap().business_channel = Some(channel.into());
            }
            return Ok(());
        }
        let entries = data["members"]
            .as_array()
            .ok_or("PK room snapshot lacks members")?;
        if entries.len() > 256 {
            return Err("PK room member count exceeds 256".into());
        }
        let mut members = BTreeSet::new();
        for member in entries {
            let uid = id(&member["uid"], "business member UID")?;
            if uid != context.uid {
                if self.expected_peer.is_some_and(|expected| expected != uid) {
                    return Err("PK room member differs from the invited peer".into());
                }
                members.insert(uid);
            }
        }
        if members.len() > 1 {
            return Err(
                "This PK media view supports one remote participant per interaction".into(),
            );
        }
        let controls = match data.get("stream_control") {
            Some(value) => parse_controls(value)?,
            None => Vec::new(),
        };
        let context = self.current.as_mut().unwrap();
        context.business_channel = Some(channel.into());
        context.members = members;
        context.version = version;
        self.apply_controls(media, controls, actions)?;
        self.reconcile(media, actions)
    }

    fn reconcile(&mut self, media: &mut Effect<'_>, actions: &mut Vec<Value>) -> Result<()> {
        let Some(context) = self.current.as_mut() else {
            return Ok(());
        };
        for uid in &context.members {
            let Some(published) = context.publications.get(uid) else {
                continue;
            };
            let mut desired = Stream {
                audio: context.default_sub_mode & 1 != 0,
                video: context.default_sub_mode & 2 != 0,
            };
            if context.label == "classic_pk" {
                desired.video = false;
                desired.audio &= published.audio;
            }
            // Published capabilities are never broadened beyond the peer's offer.
            desired.video &= published.video;
            desired.audio &= published.audio;
            for (kind, control) in &context.controls {
                if control
                    .remote
                    .as_ref()
                    .is_some_and(|uids| uids.contains(uid))
                {
                    if *kind == "audio" {
                        desired.audio = false;
                    } else {
                        desired.video = false;
                    }
                }
            }
            if context.selected.as_ref() == Some(&(*uid, desired)) {
                continue;
            }
            if context
                .selected
                .as_ref()
                .is_some_and(|(previous, _)| previous != uid)
            {
                return Err("A different remote peer is already selected".into());
            }
            let operation = if context.selected.is_some() {
                "subscription.update"
            } else {
                "subscribe"
            };
            if desired.audio || desired.video || context.selected.is_some() {
                let value = media(
                    operation,
                    &json!({"uid":uid,"audio":desired.audio,"video":desired.video,"preview":true}),
                )?;
                push_action(actions, &format!("rtc.{operation}"), value);
            }
            context.selected = Some((*uid, desired));
        }
        Ok(())
    }

    fn apply_controls(
        &mut self,
        media: &mut Effect<'_>,
        controls: Vec<Control>,
        actions: &mut Vec<Value>,
    ) -> Result<()> {
        let Some(context) = self.current.as_mut() else {
            return Ok(());
        };
        for control in controls {
            if context
                .controls
                .get(control.kind)
                .is_some_and(|previous| previous.version >= control.version)
            {
                continue;
            }
            // The recovered business control only instructs local audio mute.
            if control.kind == "audio"
                && context.publication.audio
                && let Some(muted) = control.local
            {
                if context.local_muted.get("audio") != Some(&muted) {
                    let value = media("mute", &json!({"kind":"audio","muted":muted}))?;
                    push_action(actions, "rtc.mute", value);
                    context.local_muted.insert("audio", muted);
                }
            }
            context.controls.insert(control.kind, control);
        }
        self.reconcile(media, actions)
    }

    fn poll_with(
        &mut self,
        media: &mut Effect<'_>,
        rest: &mut Effect<'_>,
        uid: &str,
        _buvid: &str,
    ) -> Result<Value> {
        let uid = decimal(uid, "current account UID")?;
        let Some(context) = &self.current else {
            return Ok(json!({"handled":false,"state":self.snapshot()}));
        };
        if context.uid != uid {
            return Err("PK account changed; retire the previous media session first".into());
        }
        let notifications = media("events", &json!({}))?;
        let notifications = notifications
            .as_array()
            .ok_or("RTC events must be an array")?;
        if notifications.len() > 256 {
            return Err("RTC notification batch exceeds 256".into());
        }
        let mut actions = Vec::new();
        for notification in notifications {
            self.consume_notification(media, notification, &mut actions)?;
        }
        if self
            .current
            .as_ref()
            .is_some_and(|context| !context.stop_polling && Instant::now() >= context.next_poll)
        {
            self.refresh_room(media, rest, &mut actions)?;
        }
        let mut result = json!({"handled":true,"notifications":notifications.len(),"actions":actions,"state":self.snapshot()});
        sanitize(&mut result);
        Ok(result)
    }

    fn consume_notification(
        &mut self,
        media: &mut Effect<'_>,
        notification: &Value,
        actions: &mut Vec<Value>,
    ) -> Result<()> {
        let Some(context) = self.current.as_mut() else {
            return Ok(());
        };
        if let Some(value) = notification.get("pub_notify") {
            let uid = id(&value["uid"], "published UID")?;
            if uid != context.uid {
                // Duplicate publish notices never add a second set of tracks.
                context
                    .publications
                    .entry(uid)
                    .or_insert(stream_flags(value.get("stream").unwrap_or(&json!({})))?);
            }
            return self.reconcile(media, actions);
        }
        if let Some(value) = notification
            .get("un_pub_notify")
            .or_else(|| notification.get("leave_notify"))
        {
            let uid = id(&value["uid"], "departing UID")?;
            context.publications.remove(&uid);
            if context
                .selected
                .as_ref()
                .is_some_and(|(selected, _)| *selected == uid)
            {
                let value = media("subscription.stop", &json!({"uid":uid}))?;
                push_action(actions, "rtc.subscription.stop", value);
                context.selected = None;
            }
            return Ok(());
        }
        if let Some(value) = notification.get("message_notify") {
            let kind = value["message_type"].as_i64().unwrap_or(0);
            if kind != 0 && !(1001..=1007).contains(&kind) {
                return Ok(());
            }
            let Some(encoded) = value["payload"]["hex"].as_str() else {
                return Ok(());
            };
            if encoded.len() > 2 * 1024 * 1024 || encoded.len() % 2 != 0 {
                return Err("RTC business message exceeds bounds or has malformed hex".into());
            }
            let mut bytes = Vec::with_capacity(encoded.len() / 2);
            for pair in encoded.as_bytes().chunks_exact(2) {
                let text = std::str::from_utf8(pair).map_err(|_| "Malformed RTC business hex")?;
                bytes.push(u8::from_str_radix(text, 16).map_err(|_| "Malformed RTC business hex")?);
            }
            let message: Value =
                serde_json::from_slice(&bytes).map_err(|_| "Malformed RTC business JSON")?;
            if message["cmd"].as_str() == Some("UNIVERSAL_INFO_UPDATE_V2") {
                self.accept_room(media, &message["data"], false, actions)?;
            }
        }
        // join_notify/mute_notify are metadata, not permission to subscribe,
        // publish, mute local capture or terminate the business interaction.
        Ok(())
    }

    fn retire(&mut self, media: &mut Effect<'_>) -> Result<()> {
        let result = if self.current.is_some() {
            media("disconnect", &json!({})).map(|_| ())
        } else {
            Ok(())
        };
        if let Some(context) = self.current.take() {
            if self.retired.len() == 256 {
                self.retired.pop_front();
            }
            self.retired
                .push_back((context.uid, context.session, context.channel));
            self.generation = self.generation.saturating_add(1);
        }
        result
    }
}

fn join_from_response(response: &Value) -> Option<Value> {
    let data = response
        .get("data")
        .filter(|v| v.is_object())
        .unwrap_or(response);
    let message = data.get("interact_join_message")?;
    message["config"]["join_token"]
        .as_str()
        .filter(|token| !token.is_empty())?;
    Some(json!({"cmd":"UNIVERSAL_INTERACT_JOIN","data":message}))
}

fn publication(controls: &Value) -> Result<Stream> {
    let status = match controls.get("pub_sub_control") {
        None => 1,
        Some(value) if value.is_object() => match value.get("pub_status") {
            None => 1,
            Some(status) => status
                .as_u64()
                .filter(|n| *n <= 3)
                .ok_or("JOIN pub_status must be an integer in 0..3")?,
        },
        Some(_) => return Err("JOIN pub_sub_control must be an object".into()),
    };
    Ok(Stream {
        audio: status & 1 != 0,
        video: status & 2 != 0,
    })
}

fn parse_controls(value: &Value) -> Result<Vec<Control>> {
    let object = value
        .as_object()
        .ok_or("PK stream_control must be an object")?;
    let mut controls = Vec::new();
    for (key, kind) in [("audio_control", "audio"), ("video_control", "video")] {
        let Some(control) = object.get(key) else {
            continue;
        };
        let version = control["version"]
            .as_i64()
            .ok_or("PK mute control version must be an integer")?;
        let local = match control.get("mute_local_mode") {
            None => None,
            Some(value) => Some(match value.as_u64() {
                Some(0) => false,
                Some(1) => true,
                _ => return Err("PK mute_local_mode must be 0 or 1".into()),
            }),
        };
        let remote = match control.get("mute_remote_uids") {
            None => None,
            Some(Value::Null) => Some(BTreeSet::new()),
            Some(Value::Array(values)) => {
                if values.len() > 256 {
                    return Err("PK muted peer list exceeds 256".into());
                }
                Some(
                    values
                        .iter()
                        .map(|v| {
                            v.as_u64().filter(|n| *n > 0).ok_or_else(|| {
                                "PK muted peers must be positive integer UIDs".to_owned()
                            })
                        })
                        .collect::<Result<BTreeSet<_>>>()?,
                )
            }
            Some(_) => return Err("PK mute_remote_uids must be an array or null".into()),
        };
        controls.push(Control {
            kind,
            version,
            local,
            remote,
        });
    }
    Ok(controls)
}

fn stream_flags(value: &Value) -> Result<Stream> {
    if !value.is_object() {
        return Err("RTC stream must be an object".into());
    }
    let field = |name| match value.get(name) {
        None => Ok(false),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("RTC stream {name} must be boolean")),
    };
    Ok(Stream {
        audio: field("audio")?,
        video: field("video")?,
    })
}
fn decimal(text: &str, label: &str) -> Result<u64> {
    if text.is_empty() || !text.bytes().all(|c| c.is_ascii_digit()) {
        return Err(format!("Invalid {label}"));
    }
    text.parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("Invalid {label}"))
}
fn id(value: &Value, label: &str) -> Result<u64> {
    match value {
        Value::String(text) => decimal(text, label),
        _ => value
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or_else(|| format!("Invalid {label}")),
    }
}
fn nonempty<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|text| {
            !text.is_empty() && text.len() <= 2 * 1024 * 1024 && !text.contains(['\r', '\n', '\0'])
        })
        .ok_or_else(|| format!("PK message lacks valid {key}"))
}
fn optional_text_matches(value: Option<&Value>, expected: &str) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::String(text)) => text.is_empty() || text == expected,
        _ => false,
    }
}
fn push_action(actions: &mut Vec<Value>, operation: &str, mut value: Value) {
    sanitize(&mut value);
    actions.push(json!({"operation":operation,"ok":true,"result":value}));
}
fn sanitize(value: &mut Value) {
    crate::app::redact(value);
    match value {
        Value::Object(values) => {
            for (key, value) in values {
                if matches!(
                    key.as_str(),
                    "join_token" | "access_token" | "qrcode_key" | "fingerprint"
                ) {
                    *value = "[redacted]".into();
                } else {
                    sanitize(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                sanitize(value);
            }
        }
        _ => (),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use std::cell::RefCell;
    fn join_event() -> Value {
        let token=wire::encode("AccessToken",&json!({"channel_id":9,"uid":123,"token":"fixture-token","fingerprint":"fixture-fingerprint","password":"fixture-password","addr":["127.0.0.1:9"]})).unwrap();
        json!({"cmd":"UNIVERSAL_INTERACT_JOIN","data":{"config":{"uid":123,"buvid":"fixture-device","biz_session_id":"session-a","biz_label":"universal_multi_conn","rtc_channel_id":"9","join_token":STANDARD.encode(token),"default_sub_mode":3},"biz_extra":{"interact_scene_configuration":{"interact_template":{"template_id":"multi_conn_grid","layout_data":{"width":1920,"height":1080}}}},"stream_control":{"pub_sub_control":{"pub_status":3}}}})
    }
    fn room() -> Value {
        json!({"biz_session_id":"session-a","business_label":"universal_multi_conn","interact_channel_id":"business-7","version":1,"members":[{"uid":123},{"uid":456}],"stream_control":{}})
    }
    fn rest(method: &str, args: &Value) -> Result<Value> {
        match (method, args["operation"].as_str()) {
            ("room.get", _) => Ok(json!({"room_id":88})),
            ("pk.request", Some("universal_info")) => Ok(room()),
            ("pk.request", Some("handle")) => {
                assert_eq!(args["fields"]["action"], "4");
                assert_eq!(args["fields"]["biz_session_id"], "");
                Ok(json!({}))
            }
            _ => Err(format!("unexpected fixture REST {method}")),
        }
    }

    #[test]
    fn validated_join_cross_checks_members_deduplicates_and_preserves_round_end() {
        let calls = RefCell::new(Vec::new());
        let mut media = |method: &str, args: &Value| {
            calls.borrow_mut().push((method.to_owned(), args.clone()));
            Ok(match method {
                "members" => json!({"users":[{"uid":456,"stream":{"audio":true,"video":true}}]}),
                _ => json!({}),
            })
        };
        let mut session = PkSession::new();
        session.expect_invitation("session-a", "456").unwrap();
        session.set_media_preferences(json!({"codec":"av1","encoder":"libsvtav1","quality":"performance","width":1280,"height":720})).unwrap();
        let event = join_event();
        let first = session
            .handle_with(&mut media, &mut rest, &event, "123", "fixture-device")
            .unwrap();
        assert_eq!(first["handled"], true);
        assert_eq!(session.snapshot()["interaction"]["peer"]["uid"], 456);
        assert!(calls.borrow().iter().any(|(name, _)| name == "subscribe"));
        let published = calls
            .borrow()
            .iter()
            .find(|(name, _)| name == "publish")
            .unwrap()
            .1
            .clone();
        assert_eq!(published["codec"], "av1");
        assert_eq!(published["encoder"], "libsvtav1");
        assert_eq!(published["audio"], true);
        assert_eq!(published["video"], true);
        assert!(!first.to_string().contains("fixture-token"));
        let count = calls.borrow().len();
        assert_eq!(
            session
                .handle_with(&mut media, &mut rest, &event, "123", "fixture-device")
                .unwrap()["handled"],
            false
        );
        session.cancel_pending();
        assert_eq!(session.snapshot()["accepting_join"], false);
        assert!(session.current.is_some());
        session
            .handle_with(
                &mut media,
                &mut rest,
                &json!({"cmd":"PK_BATTLE_END","pk_id":77}),
                "123",
                "fixture-device",
            )
            .unwrap();
        assert_eq!(calls.borrow().len(), count);
        assert!(session.current.is_some());
        session.handle_with(&mut media,&mut rest,&json!({"cmd":"UNIVERSAL_INTERACT_LEAVE","data":{"biz_session_id":"other","rtc_channel_id":"9"}}),"123","fixture-device").unwrap();
        assert!(session.current.is_some());
        session.handle_with(&mut media,&mut rest,&json!({"cmd":"UNIVERSAL_INTERACT_LEAVE","data":{"biz_session_id":"session-a","rtc_channel_id":"9"}}),"123","fixture-device").unwrap();
        assert!(session.current.is_none());
        assert_eq!(
            session
                .handle_with(&mut media, &mut rest, &event, "123", "fixture-device")
                .unwrap()["handled"],
            false
        );
        let mut late = event.clone();
        late["data"]["config"]["biz_session_id"] = "cancelled-late-session".into();
        assert_eq!(
            session
                .handle_with(&mut media, &mut rest, &late, "123", "fixture-device")
                .unwrap()["handled"],
            false
        );
        session.expect_match();
        assert_eq!(session.snapshot()["accepting_join"], true);
    }

    #[test]
    fn foreign_or_malformed_join_never_touches_media() {
        let mut session = PkSession::new();
        let mut media =
            |_: &str, _: &Value| -> Result<Value> { panic!("unvalidated JOIN reached media") };
        let event = join_event();
        assert_eq!(
            session
                .handle_with(&mut media, &mut rest, &event, "123", "another-device")
                .unwrap()["handled"],
            false
        );
        assert!(
            session
                .handle_with(&mut media, &mut rest, &event, "999", "fixture-device")
                .is_err()
        );
        for status in [json!(4), json!("1"), json!(true)] {
            let mut bad = event.clone();
            bad["data"]["stream_control"]["pub_sub_control"]["pub_status"] = status;
            assert!(
                session
                    .handle_with(&mut media, &mut rest, &bad, "123", "fixture-device")
                    .is_err()
            );
        }
        let mut bad = event.clone();
        bad["data"]["config"]["rtc_channel_id"] = "10".into();
        assert!(
            session
                .handle_with(&mut media, &mut rest, &bad, "123", "fixture-device")
                .is_err()
        );
        session
            .expect_invitation("different-session", "456")
            .unwrap();
        assert!(
            session
                .handle_with(&mut media, &mut rest, &event, "123", "fixture-device")
                .is_err()
        );
    }

    #[test]
    fn publication_and_business_identity_are_both_required_for_subscription() {
        let calls = RefCell::new(Vec::new());
        let mut media = |method: &str, args: &Value| {
            calls.borrow_mut().push((method.to_owned(), args.clone()));
            Ok(if method == "members" {
                json!({"users":[{"uid":456}]})
            } else {
                json!({})
            })
        };
        let mut session = PkSession::new();
        session
            .handle_with(
                &mut media,
                &mut rest,
                &join_event(),
                "123",
                "fixture-device",
            )
            .unwrap();
        assert!(!calls.borrow().iter().any(|(name, _)| name == "subscribe"));
        let mut actions = Vec::new();
        session
            .consume_notification(
                &mut media,
                &json!({"pub_notify":{"uid":999,"stream":{"audio":true,"video":true}}}),
                &mut actions,
            )
            .unwrap();
        assert!(!calls.borrow().iter().any(|(name, _)| name == "subscribe"));
        session
            .consume_notification(
                &mut media,
                &json!({"pub_notify":{"uid":456,"stream":{"audio":true,"video":true}}}),
                &mut actions,
            )
            .unwrap();
        assert!(calls.borrow().iter().any(|(name, _)| name == "subscribe"));
        session
            .consume_notification(
                &mut media,
                &json!({"leave_notify":{"uid":456}}),
                &mut actions,
            )
            .unwrap();
        assert!(
            calls
                .borrow()
                .iter()
                .any(|(name, _)| name == "subscription.stop")
        );
        assert!(session.current.is_some());
    }

    #[test]
    fn rejected_bootstrap_cleans_up_and_all_pub_statuses_are_exact() {
        for (status, audio, video) in [
            (0, false, false),
            (1, true, false),
            (2, false, true),
            (3, true, true),
        ] {
            assert_eq!(
                publication(&json!({"pub_sub_control":{"pub_status":status}})).unwrap(),
                Stream { audio, video }
            );
        }
        assert_eq!(
            publication(&json!({})).unwrap(),
            Stream {
                audio: true,
                video: false
            }
        );
        let calls = RefCell::new(Vec::new());
        let mut media = |method: &str, _: &Value| {
            calls.borrow_mut().push(method.to_owned());
            if method == "publish" {
                Err("fixture capture unavailable".into())
            } else {
                Ok(json!({}))
            }
        };
        let mut session = PkSession::new();
        assert!(
            session
                .handle_with(
                    &mut media,
                    &mut rest,
                    &join_event(),
                    "123",
                    "fixture-device"
                )
                .is_err()
        );
        assert_eq!(&*calls.borrow(), &["connect", "publish", "disconnect"]);
        assert!(session.current.is_none());
    }
}
