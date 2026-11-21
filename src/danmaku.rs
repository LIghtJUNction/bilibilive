//! Bilibili Goim framing, authenticated delivery, duplicate suppression and ACKs.
use serde_json::{Value, json};
use std::{
    collections::{HashSet, VecDeque},
    io::{Cursor, Read, Write},
    net::TcpStream,
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

pub const MAX_PACKET: usize = 16 * 1024 * 1024;
pub const HEARTBEAT_SECONDS: u64 = 21;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub version: u16,
    pub operation: u32,
    pub sequence: u32,
    pub payload: Vec<u8>,
}

impl Packet {
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let length = self
            .payload
            .len()
            .checked_add(16)
            .ok_or("Danmaku packet overflow")?;
        if length > MAX_PACKET {
            return Err("Danmaku packet exceeds 16 MiB".into());
        }
        let mut bytes = Vec::with_capacity(length);
        bytes.extend_from_slice(&(length as u32).to_be_bytes());
        bytes.extend_from_slice(&16u16.to_be_bytes());
        bytes.extend_from_slice(&self.version.to_be_bytes());
        bytes.extend_from_slice(&self.operation.to_be_bytes());
        bytes.extend_from_slice(&self.sequence.to_be_bytes());
        bytes.extend_from_slice(&self.payload);
        Ok(bytes)
    }
}

#[derive(Default)]
pub struct Decoder {
    buffer: Vec<u8>,
}
impl Decoder {
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Packet>, String> {
        if self.buffer.len() + bytes.len() > MAX_PACKET * 2 {
            self.buffer.clear();
            return Err("Danmaku read buffer limit exceeded".into());
        }
        self.buffer.extend_from_slice(bytes);
        let mut offset = 0;
        let mut packets = Vec::new();
        while self.buffer.len() - offset >= 16 {
            let header = &self.buffer[offset..offset + 16];
            let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
            let header_size = u16::from_be_bytes(header[4..6].try_into().unwrap()) as usize;
            if header_size < 16 || header_size > length || length > MAX_PACKET {
                self.buffer.clear();
                return Err("Invalid danmaku packet header".into());
            }
            if self.buffer.len() - offset < length {
                break;
            }
            packets.push(Packet {
                version: u16::from_be_bytes(header[6..8].try_into().unwrap()),
                operation: u32::from_be_bytes(header[8..12].try_into().unwrap()),
                sequence: u32::from_be_bytes(header[12..16].try_into().unwrap()),
                payload: self.buffer[offset + header_size..offset + length].to_vec(),
            });
            offset += length;
        }
        self.buffer.drain(..offset);
        Ok(packets)
    }
    pub fn finish(&self) -> Result<(), String> {
        if self.buffer.is_empty() {
            Ok(())
        } else {
            Err("Danmaku stream ended in a partial packet".into())
        }
    }
}

pub fn expand(packet: Packet) -> Result<Vec<Packet>, String> {
    fn recurse(
        packet: Packet,
        depth: usize,
        budget: &mut usize,
        out: &mut Vec<Packet>,
    ) -> Result<(), String> {
        if packet.version != 2 && packet.version != 3 {
            out.push(packet);
            return Ok(());
        }
        if depth >= 8 {
            return Err("Danmaku compression nesting exceeds 8".into());
        }
        let input = Cursor::new(&packet.payload);
        let mut reader: Box<dyn Read + '_> = if packet.version == 2 {
            Box::new(flate2::read::ZlibDecoder::new(input))
        } else {
            Box::new(brotli::Decompressor::new(input, 4096))
        };
        let mut unpacked = Vec::new();
        reader
            .by_ref()
            .take((*budget + 1) as u64)
            .read_to_end(&mut unpacked)
            .map_err(|e| format!("Invalid compressed danmaku packet: {e}"))?;
        if unpacked.len() > *budget {
            return Err("Danmaku decompression limit exceeded".into());
        }
        *budget -= unpacked.len();
        let mut decoder = Decoder::default();
        let packets = decoder.feed(&unpacked)?;
        decoder.finish()?;
        for item in packets {
            recurse(item, depth + 1, budget, out)?;
        }
        Ok(())
    }
    let mut out = Vec::new();
    let mut budget = MAX_PACKET;
    recurse(packet, 0, &mut budget, &mut out)?;
    Ok(out)
}

/// State survives reconnections when the owner reuses this value.
pub struct Protocol {
    pub authenticated: bool,
    support_ack: bool,
    next_sequence: u32,
    sequence_set: HashSet<u32>,
    sequence_order: VecDeque<u32>,
    messages: VecDeque<String>,
}
#[derive(Debug)]
pub struct Delivery {
    pub events: Vec<Value>,
    pub acknowledgements: Vec<Packet>,
    pub sequences: Vec<u32>,
}
impl Protocol {
    pub fn new(support_ack: bool) -> Self {
        Self {
            authenticated: false,
            support_ack,
            next_sequence: 1,
            sequence_set: HashSet::new(),
            sequence_order: VecDeque::new(),
            messages: VecDeque::new(),
        }
    }
    pub fn outgoing(&mut self, operation: u32, payload: Vec<u8>) -> Packet {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        Packet {
            version: 1,
            operation,
            sequence,
            payload,
        }
    }
    pub fn login(&mut self, login: &Value) -> Result<Packet, String> {
        if !login.is_object()
            || login["roomid"].as_u64().unwrap_or(0) == 0
            || !login["key"].is_string()
        {
            return Err("Danmaku login requires roomid and key".into());
        }
        if login["support_ack"] != self.support_ack {
            return Err("Danmaku support_ack must match protocol settings".into());
        }
        self.authenticated = false;
        self.next_sequence = 1;
        Ok(self.outgoing(7, login.to_string().into_bytes()))
    }
    pub fn receive(&mut self, packets: Vec<Packet>) -> Result<Delivery, String> {
        let mut delivery = Delivery {
            events: Vec::new(),
            acknowledgements: Vec::new(),
            sequences: Vec::new(),
        };
        for packet in packets {
            match packet.operation {
                8 => {
                    let response: Value = serde_json::from_slice(&packet.payload)
                        .map_err(|e| format!("Invalid danmaku authentication: {e}"))?;
                    let code = response["code"]
                        .as_i64()
                        .ok_or("Danmaku authentication omitted code")?;
                    self.authenticated = code == 0;
                    if code == -101 {
                        return Err("Danmaku token expired; refresh connection information".into());
                    }
                    if code != 0 {
                        return Err(format!("Danmaku authentication failed ({code})"));
                    }
                }
                3 => {
                    if packet.payload.len() >= 4 {
                        delivery.events.push(json!({"cmd":"POPULARITY","data":u32::from_be_bytes(packet.payload[..4].try_into().unwrap())}));
                    }
                }
                5 => {
                    if !self.authenticated {
                        return Err("Danmaku event arrived before successful authentication".into());
                    }
                    if packet.sequence != 0 {
                        delivery.sequences.push(packet.sequence);
                        if !self.sequence_set.insert(packet.sequence) {
                            continue;
                        }
                        self.sequence_order.push_back(packet.sequence);
                        if self.sequence_order.len() > 8192 {
                            if let Some(old) = self.sequence_order.pop_front() {
                                self.sequence_set.remove(&old);
                            }
                        }
                    }
                    let envelope: Value = serde_json::from_slice(&packet.payload)
                        .map_err(|e| format!("Invalid danmaku event: {e}"))?;
                    if !envelope.is_object() {
                        return Err("Danmaku event must be an object".into());
                    }
                    if self.support_ack {
                        let message_id = envelope["msg_id"].as_str().unwrap_or("").to_string();
                        let message_type = envelope["p_msg_type"].as_i64().unwrap_or(0);
                        if envelope["p_is_ack"].as_bool().unwrap_or(false)
                            || envelope["p_is_ack"].as_i64().unwrap_or(0) != 0
                        {
                            delivery.acknowledgements.push(self.outgoing(24, json!({"msg_id":message_id,"cmd":envelope["cmd"].as_str().unwrap_or(""),"p_msg_type":message_type}).to_string().into_bytes()));
                        }
                        if message_type == 1 {
                            let duplicate = self.messages.iter().position(|id| id == &message_id);
                            if let Some(index) = duplicate {
                                self.messages.remove(index);
                            }
                            self.messages.push_back(message_id);
                            if self.messages.len() >= 2000 {
                                self.messages.pop_front();
                            }
                            if duplicate.is_some() {
                                continue;
                            }
                        }
                    }
                    delivery.events.push(envelope);
                }
                _ => {}
            }
        }
        delivery.sequences.sort_unstable();
        delivery.sequences.dedup();
        Ok(delivery)
    }
}

type SequenceAck = Box<dyn FnMut(&[u32]) -> Result<(), String> + Send>;
enum Socket {
    WebSocket(WebSocket<MaybeTlsStream<TcpStream>>),
    Tcp(TcpStream),
}
pub struct DanmakuClient {
    socket: Socket,
    decoder: Decoder,
    protocol: Protocol,
    events: VecDeque<Value>,
    sequence_ack: SequenceAck,
    last_heartbeat: Instant,
    started: Instant,
    last_received: Instant,
}
impl DanmakuClient {
    pub fn connect(
        endpoint: &str,
        login: Value,
        sequence_ack: SequenceAck,
    ) -> Result<Self, String> {
        let support_ack = login["support_ack"]
            .as_bool()
            .ok_or("Danmaku login requires explicit support_ack")?;
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut socket = if endpoint.starts_with("tcp://") {
            let address = reqwest::Url::parse(endpoint).map_err(|error| error.to_string())?;
            if !address.username().is_empty()
                || address.password().is_some()
                || address.query().is_some()
                || address.fragment().is_some()
                || !matches!(address.path(), "" | "/")
            {
                return Err("Danmaku TCP endpoint requires only a host and port".into());
            }
            let host = address.host_str().ok_or("Danmaku TCP host is required")?;
            let port = address
                .port()
                .filter(|port| *port > 0)
                .ok_or("Danmaku TCP port is required")?;
            Socket::Tcp(crate::obs::connect_tcp(host, port, deadline)?)
        } else if endpoint.starts_with("wss://") || endpoint.starts_with("ws://127.0.0.1:") {
            Socket::WebSocket(crate::obs::connect_websocket(
                endpoint, deadline, MAX_PACKET,
            )?)
        } else {
            return Err("Danmaku endpoint must use tcp:// or wss://".into());
        };
        let tcp = match &mut socket {
            Socket::Tcp(stream) => Some(stream),
            Socket::WebSocket(ws) => match ws.get_mut() {
                MaybeTlsStream::Plain(stream) => Some(stream),
                MaybeTlsStream::Rustls(stream) => Some(&mut stream.sock),
                _ => None,
            },
        };
        if let Some(tcp) = tcp {
            tcp.set_read_timeout(Some(Duration::from_secs(1)))
                .map_err(|e| e.to_string())?;
            tcp.set_write_timeout(Some(Duration::from_secs(8)))
                .map_err(|e| e.to_string())?;
        }
        let mut client = Self {
            socket,
            decoder: Decoder::default(),
            protocol: Protocol::new(support_ack),
            events: VecDeque::new(),
            sequence_ack,
            last_heartbeat: Instant::now(),
            started: Instant::now(),
            last_received: Instant::now(),
        };
        let packet = client.protocol.login(&login)?;
        client.send(packet)?;
        client.heartbeat()?;
        Ok(client)
    }
    fn send(&mut self, packet: Packet) -> Result<(), String> {
        let bytes = packet.encode()?;
        match &mut self.socket {
            Socket::Tcp(stream) => stream.write_all(&bytes).map_err(|e| e.to_string()),
            Socket::WebSocket(ws) => ws
                .send(Message::Binary(bytes.into()))
                .map_err(|e| format!("Danmaku send failed: {e}")),
        }
    }
    pub fn heartbeat(&mut self) -> Result<(), String> {
        let packet = self.protocol.outgoing(2, Vec::new());
        self.send(packet)?;
        self.last_heartbeat = Instant::now();
        Ok(())
    }
    /// Returns None on a one-second idle tick, allowing cancellation by the owner.
    pub fn next_event(&mut self) -> Result<Option<Value>, String> {
        if let Some(event) = self.events.pop_front() {
            return Ok(Some(event));
        }
        if !self.protocol.authenticated && self.started.elapsed() >= Duration::from_secs(15) {
            return Err("Danmaku authentication timed out".into());
        }
        if self.protocol.authenticated
            && self.last_received.elapsed() >= Duration::from_secs(HEARTBEAT_SECONDS * 3)
        {
            return Err("Danmaku heartbeat response timed out".into());
        }
        if self.last_heartbeat.elapsed() >= Duration::from_secs(HEARTBEAT_SECONDS) {
            self.heartbeat()?;
        }
        let incoming = match &mut self.socket {
            Socket::WebSocket(ws) => ws.read(),
            Socket::Tcp(stream) => {
                let mut buffer = vec![0; 65_536];
                match stream.read(&mut buffer) {
                    Ok(0) => Ok(Message::Close(None)),
                    Ok(count) => {
                        buffer.truncate(count);
                        Ok(Message::Binary(buffer.into()))
                    }
                    Err(error) => Err(tungstenite::Error::Io(error)),
                }
            }
        };
        let message = match incoming {
            Ok(message) => {
                self.last_received = Instant::now();
                message
            }
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(format!("Danmaku receive failed: {error}")),
        };
        match message {
            Message::Binary(bytes) => {
                let mut packets = Vec::new();
                for packet in self.decoder.feed(&bytes)? {
                    packets.extend(expand(packet)?);
                }
                let delivery = self.protocol.receive(packets)?;
                if !delivery.sequences.is_empty() {
                    (self.sequence_ack)(&delivery.sequences)?;
                }
                for packet in delivery.acknowledgements {
                    self.send(packet)?;
                }
                self.events.extend(delivery.events);
                Ok(self.events.pop_front())
            }
            Message::Ping(_) => {
                if let Socket::WebSocket(ws) = &mut self.socket {
                    ws.flush().map_err(|e| e.to_string())?;
                }
                Ok(None)
            }
            Message::Close(_) => {
                self.decoder.finish()?;
                Err("Danmaku transport closed".into())
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn packet(operation: u32, sequence: u32, value: Value) -> Packet {
        Packet {
            version: 1,
            operation,
            sequence,
            payload: value.to_string().into_bytes(),
        }
    }
    #[test]
    fn split_frame_and_nested_zlib() {
        let message = packet(5, 42, json!({"cmd":"PK_BATTLE_START"}));
        let raw = message.encode().unwrap();
        for split in 0..raw.len() {
            let mut decoder = Decoder::default();
            let mut out = decoder.feed(&raw[..split]).unwrap();
            out.extend(decoder.feed(&raw[split..]).unwrap());
            assert_eq!(out, vec![message.clone()]);
            decoder.finish().unwrap();
        }
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&raw).unwrap();
        assert_eq!(
            expand(Packet {
                version: 2,
                operation: 5,
                sequence: 0,
                payload: encoder.finish().unwrap()
            })
            .unwrap(),
            vec![message]
        );
    }
    #[test]
    fn authentication_and_both_ack_paths() {
        let mut protocol = Protocol::new(true);
        assert!(protocol.receive(vec![packet(5, 1, json!({}))]).is_err());
        protocol
            .receive(vec![packet(8, 0, json!({"code":0}))])
            .unwrap();
        let event = json!({"cmd":"PK_BATTLE_START","msg_id":"one","p_msg_type":1,"p_is_ack":true});
        let delivery = protocol
            .receive(vec![packet(5, 42, event.clone()), packet(5, 43, event)])
            .unwrap();
        assert_eq!(delivery.events.len(), 1);
        assert_eq!(delivery.acknowledgements.len(), 2);
        assert_eq!(delivery.sequences, vec![42, 43]);
        assert_eq!(delivery.acknowledgements[0].operation, 24);
        assert!(
            protocol
                .receive(vec![packet(8, 0, json!({"code":-101}))])
                .unwrap_err()
                .contains("expired")
        );
        assert!(!protocol.authenticated);
    }
    #[test]
    fn rejects_partial_and_invalid_headers() {
        let mut decoder = Decoder::default();
        decoder.feed(&[0, 0]).unwrap();
        assert!(decoder.finish().is_err());
        assert!(Decoder::default().feed(&[0; 16]).is_err());
    }
}
