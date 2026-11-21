//! A single operation catalogue for CLI, MCP, UI and embedded zuse.
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct Spec {
    pub name: &'static str,
    pub description: &'static str,
    pub read_only: bool,
    pub input: Value,
}

fn spec(
    name: &'static str,
    description: &'static str,
    read_only: bool,
    properties: Value,
    required: &[&str],
) -> Spec {
    Spec {
        name,
        description,
        read_only,
        input: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
    }
}

pub fn operations() -> Vec<Spec> {
    let text = json!({"type":"string","maxLength":65536});
    let id = json!({"type":["integer","string"]});
    let number = json!({"type":"integer","minimum":0});
    let boolean = json!({"type":"boolean"});
    let object = json!({"type":"object"});
    let stream = json!({"room_id":id,"area_id":id,"protocol":{"type":"string","enum":["rtmp","srt","srt-only"]},"strategy":{"type":"string","enum":["fallback","prefer","only","strict"]}});
    let mut start = stream.clone();
    start["expected_uid"] = id.clone();
    let mut result = vec![
        spec(
            "doctor",
            "Report local application and dependency capabilities",
            true,
            json!({}),
            &[],
        ),
        spec(
            "config.get",
            "Read non-secret preferences",
            true,
            json!({}),
            &[],
        ),
        spec(
            "protocol.status",
            "Read the active Bilibili request profile and its local cache status",
            true,
            json!({}),
            &[],
        ),
        spec(
            "protocol.refresh",
            "Refresh validated Bilibili request parameters from the official StartLive profile",
            false,
            json!({}),
            &[],
        ),
        spec(
            "config.set",
            "Change one typed preference",
            false,
            json!({"key":text,"value":{}}),
            &["key", "value"],
        ),
        spec(
            "config.reset",
            "Reset one preference section while retaining saved accounts",
            false,
            json!({"section":{"type":"string","enum":["appearance","network","stream","obs"]}}),
            &["section"],
        ),
        spec(
            "logs.read",
            "Read bounded private operation receipts without arguments or credentials",
            true,
            json!({"limit":{"type":"integer","minimum":1,"maximum":200}}),
            &[],
        ),
        spec(
            "logs.clear",
            "Clear local operation receipts",
            false,
            json!({}),
            &[],
        ),
        spec(
            "account.list",
            "List account metadata without credentials",
            true,
            json!({}),
            &[],
        ),
        spec(
            "account.use",
            "Select the account used by all interfaces",
            false,
            json!({"name":text}),
            &["name"],
        ),
        spec(
            "account.remove",
            "Remove a saved account and its vault credential",
            false,
            json!({"name":text}),
            &["name"],
        ),
        spec(
            "auth.import",
            "Validate and save a Cookie header in the OS credential store",
            false,
            json!({"name":text,"cookie":text}),
            &["name", "cookie"],
        ),
        spec(
            "auth.qr.create",
            "Generate a login QR challenge",
            false,
            json!({}),
            &[],
        ),
        spec(
            "auth.qr.poll",
            "Poll a login challenge and save a successful login privately",
            false,
            json!({"qrcode_key":text,"name":text}),
            &["qrcode_key"],
        ),
        spec(
            "auth.validate",
            "Validate the active Bilibili account",
            true,
            json!({}),
            &[],
        ),
        spec(
            "auth.refresh",
            "Refresh ticket and device cookies in the OS credential store",
            false,
            json!({}),
            &[],
        ),
        spec(
            "room.get",
            "Read room, title, category, announcement and platform state",
            true,
            json!({}),
            &[],
        ),
        spec(
            "room.titles",
            "Read the selected account's five most recent successful titles",
            true,
            json!({}),
            &[],
        ),
        spec(
            "room.update",
            "Update the title, category or announcement",
            false,
            json!({"room_id":id,"title":text,"area_id":id,"announcement":text}),
            &[],
        ),
        spec(
            "room.cover",
            "Upload a local cover image and set the room cover",
            false,
            json!({"path":text}),
            &["path"],
        ),
        spec(
            "room.cover.preview",
            "Read a bounded preview of the selected account's current room cover",
            true,
            json!({}),
            &[],
        ),
        spec(
            "room.audit",
            "Read the room title and cover review state",
            true,
            json!({"room_id":id}),
            &[],
        ),
        spec(
            "room.precheck",
            "Read platform permission to start the room",
            true,
            json!({}),
            &[],
        ),
        spec("area.list", "List live categories", true, json!({}), &[]),
        spec(
            "area.recent",
            "List recent room categories",
            true,
            json!({"room_id":id}),
            &[],
        ),
        spec(
            "delay.get",
            "Read current delay and platform limits",
            true,
            json!({"room_id":id}),
            &[],
        ),
        spec(
            "delay.set",
            "Change delay after checking platform limits",
            false,
            json!({"room_id":id,"seconds":{"type":"integer"}}),
            &["seconds"],
        ),
        spec(
            "live.start",
            "Start the room; prepare and report any required verification before returning its URL",
            false,
            start,
            &[],
        ),
        spec(
            "live.stop",
            "Stop the current live room",
            false,
            json!({"room_id":id}),
            &[],
        ),
        spec(
            "live.stream",
            "Read the existing stream endpoint; this does not start a room",
            true,
            stream,
            &[],
        ),
        spec(
            "live.status",
            "Read the live room status",
            true,
            json!({}),
            &[],
        ),
        spec(
            "face.register",
            "Generate the platform verification URL for an issued voucher",
            false,
            json!({"v_voucher":text}),
            &["v_voucher"],
        ),
        spec(
            "face.poll",
            "Poll an existing face-verification challenge once",
            false,
            json!({"type":number,"token":text,"room_id":id}),
            &[],
        ),
        spec(
            "face.report",
            "Report the original platform-issued challenge before the user completes verification",
            false,
            json!({"type":number,"message":text,"area_id":id,"room_id":id}),
            &["type", "message", "area_id"],
        ),
        spec(
            "danmaku.send",
            "Send a chat message to the current live room",
            false,
            json!({"message":text,"room_id":id,"color":number,"fontsize":number,"mode":number}),
            &["message"],
        ),
        spec(
            "danmaku.connect",
            "Connect the room's native Bilibili event socket",
            false,
            json!({}),
            &[],
        ),
        spec(
            "danmaku.read",
            "Read the next bounded event batch from the open socket",
            true,
            json!({"limit":{"type":"integer","minimum":1,"maximum":100}}),
            &[],
        ),
        spec(
            "danmaku.disconnect",
            "Close the current room event socket",
            false,
            json!({}),
            &[],
        ),
        spec(
            "obs.configure",
            "Configure OBS endpoint and store its password privately",
            false,
            json!({"url":text,"password":text}),
            &[],
        ),
        spec(
            "obs.status",
            "Read the live OBS streaming state",
            true,
            json!({}),
            &[],
        ),
        spec(
            "obs.plugin.status",
            "Inspect the verified bundled capture plugin and its local OBS installation",
            true,
            json!({"bundle_root":text,"manifest_sha256":{"type":"string","maxLength":64},"destination":text}),
            &[],
        ),
        spec(
            "obs.plugin.install",
            "Install this version's verified capture plugin; preserve the prior installation if replacement fails",
            false,
            json!({"bundle_root":text,"manifest_sha256":{"type":"string","maxLength":64},"destination":text,"replace":boolean}),
            &[],
        ),
        spec(
            "obs.watch.start",
            "Start a persistent OBS observer with automatic reconnects in this application session",
            false,
            json!({}),
            &[],
        ),
        spec(
            "obs.watch.stop",
            "Stop this session's persistent OBS observer",
            false,
            json!({}),
            &[],
        ),
        spec(
            "obs.watch.status",
            "Read the persistent OBS observer's latest output and reconnect state",
            true,
            json!({}),
            &[],
        ),
        spec(
            "obs.sync",
            "Copy the existing selected stream endpoint to OBS",
            false,
            json!({}),
            &[],
        ),
        spec(
            "obs.start",
            "Configure OBS from the current stream endpoint and start output",
            false,
            json!({}),
            &[],
        ),
        spec(
            "obs.stop",
            "Stop OBS streaming output",
            false,
            json!({}),
            &[],
        ),
        spec(
            "obs.request",
            "Send one explicit OBS WebSocket v5 request",
            false,
            json!({"request_type":text,"data":object}),
            &["request_type"],
        ),
        spec(
            "pk.status",
            "Read the active PK round",
            true,
            json!({"room_id":id,"pk_id":id,"modern":boolean}),
            &[],
        ),
        spec(
            "pk.session",
            "Read the current PK interaction, publication and round state",
            true,
            json!({}),
            &[],
        ),
        spec(
            "pk.search",
            "Find PK candidates by UID or search text",
            true,
            json!({"uid":id,"search":text,"page":number}),
            &[],
        ),
        spec(
            "pk.match",
            "Request classic PK, video PK or video-link matching",
            false,
            json!({"room_id":id,"mode":{"type":"string","enum":["classic","traditional","video","video_link","link"]}}),
            &[],
        ),
        spec(
            "pk.cancel",
            "Cancel pending PK matching",
            false,
            json!({"room_id":id,"mode":text}),
            &[],
        ),
        spec(
            "pk.invite",
            "Invite the exact UID after candidate and permission checks",
            false,
            json!({"room_id":id,"uid":id}),
            &["uid"],
        ),
        spec(
            "pk.accept",
            "Accept a specific invitation with its original source and session",
            false,
            json!({"peer_uid":id,"uid":id,"session_id":text,"source":id,"client_traceid":text}),
            &["session_id", "source"],
        ),
        spec(
            "pk.reject",
            "Reject a specific invitation with its original source and session",
            false,
            json!({"peer_uid":id,"uid":id,"session_id":text,"source":id,"client_traceid":text}),
            &["session_id", "source"],
        ),
        spec("pk.end", "End the active PK round", false, json!({}), &[]),
        spec(
            "pk.settings.get",
            "Read all nine PK switches and server duration options",
            true,
            json!({}),
            &[],
        ),
        spec(
            "pk.settings.update",
            "Change one PK preference, preserve its native coupling and verify read-back",
            false,
            json!({"field":{"type":"string","enum":["duration","accept_pk","accept_unfollowed_pk","classic_pk","classic_opponent_voice","accept_connection","mutual_only","allow_apply_join","allow_auto_join","allow_other_invite"]},"value":{"type":["boolean","integer"]}}),
            &["field", "value"],
        ),
        spec(
            "pk.continuous.get",
            "Read the server's continuous PK switch and eligible modes",
            true,
            json!({}),
            &[],
        ),
        spec(
            "pk.continuous.set",
            "Set continuous classic/video PK and read back server confirmation",
            false,
            json!({"enabled":boolean,"mode":{"type":"string","enum":["classic","traditional","video"]},"client_traceid":text}),
            &["enabled"],
        ),
        spec(
            "pk.invitation.cancel",
            "Cancel an outgoing directed video PK invitation",
            false,
            json!({"uid":id,"client_traceid":text}),
            &["uid"],
        ),
        spec(
            "pk.link.panel",
            "Prepare the video-link panel and return eligible candidates",
            false,
            json!({}),
            &[],
        ),
        spec(
            "pk.link.list",
            "Read video-link invitations and participants",
            true,
            json!({}),
            &[],
        ),
        spec(
            "pk.link.connect",
            "Invite a UID from the current eligible video-link candidates",
            false,
            json!({"uid":id}),
            &["uid"],
        ),
        spec(
            "pk.link.cancel",
            "Cancel an outgoing video-link invitation",
            false,
            json!({"uid":id,"client_traceid":text}),
            &["uid"],
        ),
        spec(
            "pk.link.start",
            "Convert the specified active video link into a PK round",
            false,
            json!({"session_id":text}),
            &["session_id"],
        ),
        spec(
            "pk.link.disconnect",
            "Optionally end PK, then quit the specified video link",
            false,
            json!({"session_id":text,"end_round":boolean}),
            &["session_id"],
        ),
        spec(
            "pk.request",
            "Invoke an audited PK operation by its closed protocol name",
            false,
            json!({"operation":text,"fields":object,"after_sign":object}),
            &["operation"],
        ),
        spec(
            "convert.livehime",
            "Convert a LiveHime collection into a new OBS JSON file",
            false,
            json!({"input":text,"output":text,"name":text,"filter_prefixes":boolean,"width":number,"height":number}),
            &["input", "output"],
        ),
        spec(
            "convert.discover",
            "Find LiveHime scene-collection files",
            true,
            json!({"root":text}),
            &[],
        ),
        spec(
            "zuse.schema",
            "Read the shared native automation command schemas",
            true,
            json!({}),
            &[],
        ),
        spec(
            "zuse.check",
            "Preflight a complete zuse program without executing operations",
            true,
            json!({"script":text,"base":text,"read_only":boolean}),
            &["script"],
        ),
        spec(
            "zuse.run",
            "Run a bounded, fully preflighted zuse program",
            false,
            json!({"script":text,"base":text,"read_only":boolean}),
            &["script"],
        ),
        spec(
            "updates.check",
            "Check the stable release for this installation source without installing code",
            true,
            json!({}),
            &[],
        ),
        spec(
            "updates.status",
            "Read this installation's update state and download progress without network or installation effects",
            true,
            json!({}),
            &[],
        ),
        spec(
            "updates.download",
            "Download and verify the exact update identified by a prior check; package-manager installs remain manual",
            false,
            json!({"staged_id":{"type":"string","maxLength":128},"wait":boolean}),
            &["staged_id"],
        ),
        spec(
            "updates.apply",
            "Schedule the verified update after this host exits; retire local media without stopping the Bilibili room or OBS output",
            false,
            json!({"staged_id":{"type":"string","maxLength":128},"restart":boolean}),
            &["staged_id"],
        ),
    ];
    // RTC operations are provided by the native media worker; token material is
    // accepted as input and is never included in the operation log.
    result.push(spec(
        "rtc.status",
        "Read the native RTC worker state",
        true,
        json!({}),
        &[],
    ));
    result.push(spec(
        "rtc.connect",
        "Connect a platform-issued RTC ticket",
        false,
        json!({"ticket":text,"uid":id,"channel_id":id}),
        &["ticket", "uid", "channel_id"],
    ));
    result.push(spec(
        "rtc.disconnect",
        "Leave and release the native RTC session",
        false,
        json!({}),
        &[],
    ));
    result.push(spec(
        "rtc.request",
        "Send an allowed RTC status or update RPC",
        false,
        json!({"method":text,"params":object}),
        &["method"],
    ));
    for (name, description) in [
        (
            "rtc.doctor",
            "Inspect libdatachannel and FFmpeg capabilities",
        ),
        (
            "rtc.self-test",
            "Exercise native ICE, DTLS, SCTP and SRTP using two local fixture peers",
        ),
        ("rtc.encoders", "List available native media encoders"),
        ("rtc.members", "Read members of the joined RTC channel"),
        ("rtc.events", "Read pending RTC signalling notifications"),
    ] {
        result.push(spec(name, description, true, json!({}), &[]));
    }
    let capture = json!({"video":boolean,"audio":boolean,"codec":{"type":"string","enum":["h264","av1"]},"capture":boolean,"encoder":text,"quality":{"type":"string","enum":["performance","balanced","quality"]},"hw_device":text,"width":number,"height":number,"fps":number,"bitrate_kbps":number,"crop":object});
    result.push(spec(
        "rtc.publish",
        "Publish OBS video and audio to the joined RTC channel",
        false,
        capture.clone(),
        &[],
    ));
    result.push(spec("rtc.encoder.probe","Test an encoder against a local synthetic video fixture and actual RTP output",true,json!({"codec":{"type":"string","enum":["h264","av1"]},"encoder":text,"quality":{"type":"string","enum":["performance","balanced","quality"]},"hw_device":text}),&[]));
    result.push(spec(
        "rtc.capture.start",
        "Start the native OBS shared-memory and FFmpeg capture bridge",
        false,
        capture,
        &[],
    ));
    result.push(spec("rtc.subscribe","Receive a specific peer's media and optionally open its native preview",false,json!({"uid":id,"video":boolean,"audio":boolean,"video_port":number,"audio_port":number,"preview":boolean}),&["uid"]));
    result.push(spec(
        "rtc.subscription.stop",
        "Stop receiving one peer's media",
        false,
        json!({"uid":id}),
        &["uid"],
    ));
    result.push(spec(
        "rtc.subscription.update",
        "Change audio and video reception for one subscribed peer",
        false,
        json!({"uid":id,"video":boolean,"audio":boolean}),
        &["uid", "video", "audio"],
    ));
    result.push(spec(
        "rtc.mute",
        "Mute or unmute a published audio or video track",
        false,
        json!({"kind":{"type":"string","enum":["audio","video"]},"muted":boolean}),
        &["kind", "muted"],
    ));
    result.push(spec(
        "rtc.preview",
        "Open or change the native remote-media preview",
        false,
        json!({"muted":boolean,"volume":{"type":"integer","minimum":0,"maximum":100}}),
        &[],
    ));
    result.push(spec("rtc.local.preview","Preview the local OBS capture without broadcasting; audio monitoring is opt-in",false,json!({"video":boolean,"audio":boolean,"muted":boolean,"volume":{"type":"integer","minimum":0,"maximum":100}}),&[]));
    result.push(spec(
        "rtc.local.preview.stop",
        "Stop the local OBS preview and monitoring",
        false,
        json!({}),
        &[],
    ));
    for (name, description) in [
        ("rtc.capture.stop", "Stop native OBS capture"),
        (
            "rtc.publication.stop",
            "Stop publishing to the current RTC channel",
        ),
        ("rtc.preview.stop", "Close the native remote-media preview"),
    ] {
        result.push(spec(name, description, false, json!({}), &[]));
    }
    result
}

pub fn find(name: &str) -> Option<Spec> {
    operations().into_iter().find(|s| s.name == name)
}

pub fn validate(name: &str, arguments: &Value) -> Result<Spec, String> {
    let spec = find(name).ok_or_else(|| format!("Unknown operation: {name}"))?;
    let args = arguments
        .as_object()
        .ok_or("Arguments must be a JSON object")?;
    for key in spec.input["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !args.contains_key(key) {
            return Err(format!("{name} requires {key}"));
        }
    }
    for (name, value) in args {
        let rule = spec.input["properties"]
            .get(name)
            .ok_or_else(|| format!("Unknown argument: {name}"))?;
        let matches = |kind: &str| match kind {
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "object" => value.is_object(),
            "array" => value.is_array(),
            "null" => value.is_null(),
            _ => false,
        };
        let valid = match rule.get("type") {
            Some(Value::String(t)) => matches(t),
            Some(Value::Array(types)) => types.iter().filter_map(Value::as_str).any(matches),
            None => true,
            _ => false,
        };
        if !valid {
            return Err(format!("Invalid type for {name}"));
        }
        if let Some(allowed) = rule["enum"].as_array() {
            if !allowed.contains(value) {
                return Err(format!("Invalid value for {name}"));
            }
        }
        if let Some(min) = rule["minimum"].as_f64() {
            if value.as_f64().is_some_and(|v| v < min) {
                return Err(format!("{name} is below its minimum"));
            }
        }
        if let Some(max) = rule["maximum"].as_f64() {
            if value.as_f64().is_some_and(|v| v > max) {
                return Err(format!("{name} exceeds its maximum"));
            }
        }
        if let Some(max) = rule["maxLength"].as_u64() {
            if value.as_str().is_some_and(|s| s.len() as u64 > max) {
                return Err(format!("{name} is too long"));
            }
        }
    }
    Ok(spec)
}

pub fn catalogue() -> Value {
    json!({"name":"bilibilive","version":crate::VERSION,"tools":operations().into_iter().map(|s| json!({"name":s.name,"description":s.description,"inputSchema":s.input,"annotations":{"readOnlyHint":s.read_only,"destructiveHint":!s.read_only,"openWorldHint":true}})).collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_validation_prevents_misspelled_mutations() {
        assert!(validate("live.start", &json!({"protocol":"file"})).is_err());
        assert!(validate("room.update", &json!({"titel":"wrong"})).is_err());
        assert!(validate("pk.accept", &json!({"session_id":"x"})).is_err());
        assert!(validate("room.update", &json!({"title":"测试"})).is_ok());
        let names: std::collections::BTreeSet<_> =
            operations().into_iter().map(|s| s.name).collect();
        assert_eq!(names.len(), operations().len());
    }
}
