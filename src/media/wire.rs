//! Recovered Bilibili RTC protobuf schema, ported from StartLive (GPL-3.0-only).
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Map, Value, json};
use std::sync::OnceLock;
pub const MAX_MESSAGE: usize = 262_144;
fn schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::from_str(include_str!("schema.json")).expect("embedded RTC schema")
    })
}
fn fields(name: &str) -> Result<&'static [Value], String> {
    schema()[name]
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| format!("Unknown RTC message type {name}"))
}
fn varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 128 {
        out.push((value as u8) | 128);
        value >>= 7;
    }
    out.push(value as u8);
}
fn read_varint(bytes: &[u8], at: &mut usize) -> Result<u64, String> {
    let mut value = 0u64;
    for i in 0..10 {
        let byte = *bytes.get(*at).ok_or("Truncated RTC varint")?;
        *at += 1;
        if i == 9 && byte > 1 {
            return Err("RTC varint overflow".into());
        }
        value |= u64::from(byte & 127) << (7 * i);
        if byte < 128 {
            return Ok(value);
        }
    }
    Err("Invalid RTC varint".into())
}
fn unhex(value: &str) -> Result<Vec<u8>, String> {
    if value.len() % 2 != 0 {
        return Err("Odd RTC hex length".into());
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|p| {
            let h = (p[0] as char).to_digit(16).ok_or("Invalid RTC hex")?;
            let l = (p[1] as char).to_digit(16).ok_or("Invalid RTC hex")?;
            Ok((h * 16 + l) as u8)
        })
        .collect()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn scalar(kind: &str, value: &Value) -> Result<u64, String> {
    match kind {
        "bool" => value
            .as_bool()
            .map(u64::from)
            .ok_or_else(|| "RTC boolean required".into()),
        "int32" => value
            .as_i64()
            .filter(|v| i32::try_from(*v).is_ok())
            .map(|v| v as u64)
            .ok_or_else(|| "RTC int32 out of range".into()),
        "int64" => value
            .as_i64()
            .map(|v| v as u64)
            .ok_or_else(|| "RTC int64 out of range".into()),
        "uint32" => value
            .as_u64()
            .filter(|v| u32::try_from(*v).is_ok())
            .ok_or_else(|| "RTC uint32 out of range".into()),
        "uint64" => value
            .as_u64()
            .ok_or_else(|| "RTC uint64 out of range".into()),
        _ => Err("RTC scalar type invalid".into()),
    }
}
pub fn encode(name: &str, value: &Value) -> Result<Vec<u8>, String> {
    encode_at(name, value, 0)
}
fn encode_at(name: &str, value: &Value, depth: usize) -> Result<Vec<u8>, String> {
    if depth > 64 {
        return Err("RTC message nesting exceeds 64".into());
    }
    let fields = fields(name)?;
    let object = value.as_object().ok_or("RTC message must be an object")?;
    for key in object.keys() {
        if key != "_unknown_wire" && !fields.iter().any(|f| f[1] == key.as_str()) {
            return Err(format!("Unknown RTC field {name}.{key}"));
        }
    }
    let mut out = Vec::new();
    let mut oneof = false;
    for field in fields {
        let number = field[0].as_u64().unwrap();
        let key = field[1].as_str().unwrap();
        let kind = field[2].as_str().unwrap();
        let mode = field.get(3).and_then(Value::as_str).unwrap_or("");
        let Some(value) = object.get(key) else {
            continue;
        };
        if mode == "oneof" {
            if oneof {
                return Err("Multiple RTC oneof payloads".into());
            }
            oneof = true;
        }
        let items: Vec<&Value> = if matches!(mode, "repeated" | "packed") {
            value
                .as_array()
                .ok_or("RTC repeated field must be an array")?
                .iter()
                .collect()
        } else {
            vec![value]
        };
        if mode == "packed" {
            let mut packed = Vec::new();
            for item in items {
                varint(scalar(kind, item)?, &mut packed);
            }
            if !packed.is_empty() {
                varint(number * 8 + 2, &mut out);
                varint(packed.len() as u64, &mut out);
                out.extend(packed);
            }
            continue;
        }
        for item in items {
            if matches!(kind, "bool" | "int32" | "int64" | "uint32" | "uint64") {
                let n = scalar(kind, item)?;
                if n == 0 && !matches!(mode, "optional" | "oneof" | "repeated") {
                    continue;
                }
                varint(number * 8, &mut out);
                varint(n, &mut out);
            } else {
                let bytes = match kind {
                    "string" => item
                        .as_str()
                        .ok_or("RTC string required")?
                        .as_bytes()
                        .to_vec(),
                    "bytes" => unhex(
                        item["hex"]
                            .as_str()
                            .ok_or("RTC bytes require a hex object")?,
                    )?,
                    _ => encode_at(kind, item, depth + 1)?,
                };
                if bytes.is_empty()
                    && matches!(kind, "string" | "bytes")
                    && !matches!(mode, "optional" | "oneof" | "repeated")
                {
                    continue;
                }
                varint(number * 8 + 2, &mut out);
                varint(bytes.len() as u64, &mut out);
                out.extend(bytes);
            }
        }
    }
    if let Some(unknown) = object.get("_unknown_wire") {
        for fragment in unknown
            .as_array()
            .ok_or("RTC unknown wire must be an array")?
        {
            out.extend(unhex(
                fragment
                    .as_str()
                    .ok_or("RTC unknown wire fragment must be hex text")?,
            )?);
        }
    }
    if out.len() > MAX_MESSAGE {
        return Err("RTC message exceeds 256 KiB".into());
    }
    Ok(out)
}
pub fn decode(name: &str, bytes: &[u8]) -> Result<Value, String> {
    decode_at(name, bytes, 0)
}
fn decode_at(name: &str, bytes: &[u8], depth: usize) -> Result<Value, String> {
    if bytes.len() > MAX_MESSAGE || depth > 64 {
        return Err("RTC message exceeds bounds".into());
    }
    let fields = fields(name)?;
    let mut at = 0;
    let mut output = Map::new();
    let mut unknown = Vec::new();
    while at < bytes.len() {
        let start = at;
        let tag = read_varint(bytes, &mut at)?;
        let number = tag >> 3;
        let wire = tag & 7;
        if number == 0 {
            return Err("RTC field zero is invalid".into());
        }
        let mut number_value = 0;
        let data: &[u8];
        match wire {
            0 => {
                number_value = read_varint(bytes, &mut at)?;
                data = &[];
            }
            1 | 5 => {
                let len = if wire == 1 { 8 } else { 4 };
                data = bytes.get(at..at + len).ok_or("Truncated RTC fixed field")?;
                at += len;
            }
            2 => {
                let len = usize::try_from(read_varint(bytes, &mut at)?)
                    .map_err(|_| "RTC field length overflow")?;
                let end = at.checked_add(len).ok_or("RTC field length overflow")?;
                data = bytes
                    .get(at..end)
                    .ok_or("Truncated RTC length-delimited field")?;
                at = end;
            }
            _ => return Err("Unsupported RTC wire type".into()),
        }
        let Some(field) = fields.iter().find(|f| f[0].as_u64() == Some(number)) else {
            unknown.push(Value::String(hex(&bytes[start..at])));
            continue;
        };
        let key = field[1].as_str().unwrap();
        let kind = field[2].as_str().unwrap();
        let mode = field.get(3).and_then(Value::as_str).unwrap_or("");
        let scalar_kind = matches!(kind, "bool" | "int32" | "int64" | "uint32" | "uint64");
        if !(wire == 0 && scalar_kind || wire == 2 && (!scalar_kind || mode == "packed")) {
            unknown.push(Value::String(hex(&bytes[start..at])));
            continue;
        }
        fn convert_scalar(kind: &str, n: u64) -> Value {
            match kind {
                "bool" => (n != 0).into(),
                "int32" => (n as i32).into(),
                "int64" => (n as i64).into(),
                "uint32" => (n as u32).into(),
                _ => n.into(),
            }
        }
        let values = if mode == "packed" && wire == 2 {
            let mut pos = 0;
            let mut list = Vec::new();
            while pos < data.len() {
                list.push(convert_scalar(kind, read_varint(data, &mut pos)?));
            }
            list
        } else {
            vec![if scalar_kind {
                convert_scalar(kind, number_value)
            } else {
                match kind {
                    "string" => std::str::from_utf8(data)
                        .map_err(|_| "Invalid RTC UTF-8")?
                        .into(),
                    "bytes" => json!({"hex":hex(data)}),
                    _ => decode_at(kind, data, depth + 1)?,
                }
            }]
        };
        if mode == "oneof" {
            for other in fields
                .iter()
                .filter(|f| f.get(3).and_then(Value::as_str) == Some("oneof"))
            {
                output.remove(other[1].as_str().unwrap());
            }
        }
        if matches!(mode, "packed" | "repeated") {
            let array = output
                .entry(key.to_string())
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .unwrap();
            array.extend(values);
        } else {
            output.insert(key.to_string(), values.into_iter().next().unwrap());
        }
    }
    if !unknown.is_empty() {
        output.insert("_unknown_wire".into(), unknown.into());
    }
    Ok(output.into())
}
pub fn decode_ticket(encoded: &str, uid: u64, channel_id: u64) -> Result<Value, String> {
    if encoded.len() > MAX_MESSAGE * 2 {
        return Err("RTC access token exceeds bounds".into());
    }
    let raw = STANDARD
        .decode(encoded)
        .map_err(|_| "Invalid RTC access-token base64")?;
    let value = decode("AccessToken", &raw)?;
    if value["uid"].as_u64() != Some(uid) || value["channel_id"].as_u64() != Some(channel_id) {
        return Err("RTC ticket account/channel mismatch".into());
    }
    for key in ["token", "fingerprint", "password"] {
        if !value[key]
            .as_str()
            .is_some_and(|v| !v.is_empty() && !v.contains(['\r', '\n']))
        {
            return Err(format!("RTC ticket lacks valid {key}"));
        }
    }
    if !value["addr"]
        .as_array()
        .is_some_and(|v| !v.is_empty() && v.iter().all(Value::is_string))
    {
        return Err("RTC ticket has no server addresses".into());
    }
    Ok(value)
}

#[derive(Clone, Debug)]
struct Section {
    lines: Vec<String>,
    mid: String,
    kind: String,
    direction: String,
}
fn parse(sdp: &str) -> (Vec<String>, Vec<Section>) {
    let mut header = Vec::new();
    let mut sections = Vec::<Section>::new();
    for line in sdp.lines().map(str::trim).filter(|s| !s.is_empty()) {
        if line.starts_with("m=") {
            sections.push(Section {
                lines: Vec::new(),
                mid: String::new(),
                kind: line[2..].split_whitespace().next().unwrap_or("").into(),
                direction: String::new(),
            });
        }
        if let Some(section) = sections.last_mut() {
            section.lines.push(line.into());
            if let Some(mid) = line.strip_prefix("a=mid:") {
                section.mid = mid.into();
            }
            if ["a=sendonly", "a=recvonly", "a=sendrecv", "a=inactive"].contains(&line) {
                section.direction = line[2..].into();
            }
        } else {
            header.push(line.into());
        }
    }
    (header, sections)
}
fn combine(mut header: Vec<String>, sections: &[Section]) -> String {
    let mids = sections
        .iter()
        .filter(|s| s.direction != "inactive")
        .map(|s| s.mid.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(bundle) = header.iter_mut().find(|s| s.starts_with("a=group:BUNDLE")) {
        *bundle = format!("a=group:BUNDLE {mids}");
    }
    for section in sections {
        header.extend(section.lines.clone());
    }
    header.join("\r\n") + "\r\n"
}
pub fn application_mid(sdp: &str) -> Result<String, String> {
    parse(sdp)
        .1
        .into_iter()
        .find(|s| s.kind == "application")
        .map(|s| s.mid)
        .ok_or_else(|| "RTC SDP lacks data channel section".into())
}
pub fn media_fragment(sdp: &str, mids: &[String]) -> Result<String, String> {
    let (header, all) = parse(sdp);
    let selected: Vec<_> = all.into_iter().filter(|s| mids.contains(&s.mid)).collect();
    if selected.len() != mids.len() {
        return Err("RTC offer omitted a requested media MID".into());
    }
    Ok(combine(header, &selected))
}
pub fn merge_answer(cached: &str, local: &str, fragment: &str) -> Result<String, String> {
    let (header, mut pool) = parse(cached);
    if header.is_empty() || pool.is_empty() {
        return Err("RTC remote baseline is unavailable".into());
    }
    for incoming in parse(fragment)
        .1
        .into_iter()
        .filter(|s| matches!(s.kind.as_str(), "audio" | "video") && s.direction != "inactive")
    {
        pool.retain(|old| old.mid != incoming.mid || old.kind != incoming.kind);
        pool.push(incoming);
    }
    let mut out = Vec::new();
    for mut offer in parse(local).1 {
        let pair = |remote: &Section| {
            matches!(
                (offer.direction.as_str(), remote.direction.as_str()),
                ("sendonly", "recvonly")
                    | ("recvonly", "sendonly")
                    | ("sendrecv", "sendrecv")
                    | ("inactive", "inactive")
                    | ("", "")
            )
        };
        if let Some(index) = pool
            .iter()
            .position(|remote| remote.mid == offer.mid && pair(remote))
        {
            out.push(pool.remove(index));
            continue;
        }
        let direction = match offer.direction.as_str() {
            "sendonly" | "sendrecv" => "recvonly",
            "recvonly" => "inactive",
            other => other,
        }
        .to_owned();
        if offer
            .lines
            .iter()
            .any(|s| s == "a=setup:active" || s == "a=setup:passive")
        {
            return Err("RTC missing answer cannot determine DTLS role".into());
        }
        for line in &mut offer.lines {
            if line == &format!("a={}", offer.direction) {
                *line = format!("a={direction}");
            }
            if line == "a=setup:actpass" {
                *line = "a=setup:active".into();
            }
            if direction == "inactive" && line.starts_with("m=") {
                let mut parts = line
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if parts.len() > 1 {
                    parts[1] = "0".into();
                    *line = parts.join(" ");
                }
            }
        }
        if direction == "inactive" {
            offer
                .lines
                .retain(|s| !s.starts_with("a=ssrc:") && !s.starts_with("a=ssrc-group:"));
        }
        offer.direction = direction;
        out.push(offer);
    }
    if pool
        .iter()
        .any(|s| matches!(s.kind.as_str(), "audio" | "video") && s.direction != "inactive")
    {
        return Err("RTC answer media does not match the local offer".into());
    }
    Ok(combine(header, &out))
}
pub fn initial_answer(ticket: &Value, mid: &str) -> Result<String, String> {
    use std::net::ToSocketAddrs;
    if mid.is_empty() || mid.contains(char::is_whitespace) {
        return Err("Invalid RTC application MID".into());
    }
    let mut lines = vec![
        "v=0".to_string(),
        "o=- 0 0 IN IP4 127.0.0.1".into(),
        "s=BRTC".into(),
        "t=0 0".into(),
        format!("a=group:BUNDLE {mid}"),
        "a=msid-semantic: WMS *".into(),
        "a=extmap-allow-mixed".into(),
        "m=application 9 UDP/DTLS/SCTP webrtc-datachannel".into(),
        "c=IN IP4 0.0.0.0".into(),
        format!(
            "a=ice-ufrag:{}",
            ticket["token"].as_str().ok_or("RTC token missing")?
        ),
        format!(
            "a=ice-pwd:{}",
            ticket["password"].as_str().ok_or("RTC password missing")?
        ),
        format!(
            "a=fingerprint:{}",
            ticket["fingerprint"]
                .as_str()
                .ok_or("RTC fingerprint missing")?
        ),
    ];
    let mut addresses = Vec::new();
    for address in ticket["addr"].as_array().ok_or("RTC addresses missing")? {
        let address = address.as_str().ok_or("RTC address invalid")?;
        if let Ok(resolved) = address.to_socket_addrs() {
            for socket in resolved {
                if !addresses.contains(&socket) {
                    addresses.push(socket);
                }
            }
        }
    }
    if addresses.is_empty() {
        return Err("No RTC server address could be resolved".into());
    }
    for (index, address) in addresses.into_iter().take(32).enumerate() {
        lines.push(format!(
            "a=candidate:1 1 udp {} {} {} typ host generation 0",
            0x7effffffu32 - ((index as u32) << 8),
            address.ip(),
            address.port()
        ));
    }
    lines.extend([
        "a=setup:passive".into(),
        format!("a=mid:{mid}"),
        "a=sctp-port:5000".into(),
        "a=max-message-size:262144".into(),
    ]);
    Ok(lines.join("\r\n") + "\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presence_unknown_and_repeated_roundtrip() {
        for (name, value) in [
            ("MuteReq", json!({"video_muted":false})),
            (
                "RequestMessage",
                json!({"request_id":1,"channel_id":2,"uid":3,"leave_req":{}}),
            ),
            (
                "TurnAddr",
                json!({"transport":[0,1,2],"addr":["one","two"]}),
            ),
        ] {
            let encoded = encode(name, &value).unwrap();
            assert_eq!(decode(name, &encoded).unwrap(), value);
        }
        assert!(encode("MuteReq", &json!({"audio_muted":true,"video_muted":true})).is_err());
    }
    #[test]
    fn invalid_tokens_and_unknown_wire() {
        let raw = vec![0xa0, 0x06, 0x07];
        let decoded = decode("LeaveReq", &raw).unwrap();
        assert_eq!(encode("LeaveReq", &decoded).unwrap(), raw);
        assert!(decode_ticket("invalid", 1, 2).is_err());
        assert!(decode("AccessToken", &[0]).is_err());
    }
    #[test]
    fn fragments_retain_application() {
        let data = "v=0\r\na=group:BUNDLE 0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=mid:0\r\na=setup:passive\r\n";
        let local = format!(
            "{data}m=video 9 UDP/TLS/RTP/SAVPF 96\r\na=mid:1\r\na=sendonly\r\na=setup:actpass\r\n"
        );
        let fragment =
            "m=video 9 UDP/TLS/RTP/SAVPF 96\r\na=mid:1\r\na=recvonly\r\na=setup:passive\r\n";
        let merged = merge_answer(data, &local, fragment).unwrap();
        assert!(merged.contains("a=group:BUNDLE 0 1"));
        assert!(merged.contains("m=application"));
        assert!(
            !media_fragment(&local, &["1".into()])
                .unwrap()
                .contains("m=application")
        );
    }
}
