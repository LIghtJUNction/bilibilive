//! Native PK/colive media orchestration shared by CLI, MCP and the GUI.
pub mod capture;
mod limits;
pub mod native;
mod preview;
mod rtp;
pub mod wire;
use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Child, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, Sender},
    },
    thread,
    time::Duration,
};

struct Request {
    method: String,
    args: Value,
    reply: Sender<Result<Value, String>>,
}
#[derive(Clone)]
pub struct MediaManager {
    sender: Sender<Request>,
}
impl Default for MediaManager {
    fn default() -> Self {
        Self::new()
    }
}
impl MediaManager {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel::<Request>();
        thread::spawn(move || {
            let mut worker = Worker {
                session: None,
                capture: None,
                capture_request: None,
                local_preview: None,
                preview_input: Arc::new(Mutex::new(None)),
                preview: None,
                receiver_sdp: None,
                last_error: None,
            };
            loop {
                match receiver.recv_timeout(if worker.session.is_some() {
                    Duration::from_millis(5)
                } else {
                    Duration::from_millis(200)
                }) {
                    Ok(request) => {
                        let result = worker.call(&request.method, &request.args);
                        let _ = request.reply.send(result);
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if let Some(error) = worker.capture.as_ref().and_then(capture::Capture::failure) {
                    worker.capture_request = None;
                    worker.capture.take();
                    worker.stop_local_preview();
                    worker.last_error = Some(error);
                    if let Some(session) = &mut worker.session {
                        let _ = session.publish(false, false, "h264");
                    }
                }
                if let Some(session) = &mut worker.session {
                    if let Err(error) = session.poll() {
                        worker.last_error = Some(error);
                        worker.capture_request = None;
                        worker.capture.take();
                        worker.session.take();
                        worker.close_preview();
                        if worker.rebuild_capture().is_err() {
                            worker.stop_local_preview();
                        }
                    }
                }
            }
        });
        Self { sender }
    }
    pub fn call(&self, method: &str, args: &Value) -> Result<Value, String> {
        let (tx, rx) = mpsc::channel();
        self.sender
            .send(Request {
                method: method.strip_prefix("media.").unwrap_or(method).into(),
                args: args.clone(),
                reply: tx,
            })
            .map_err(|_| "Native media worker has stopped")?;
        rx.recv_timeout(Duration::from_secs(50))
            .map_err(|_| "Native media operation timed out")?
    }
}
struct Worker {
    session: Option<native::Session>,
    capture: Option<capture::Capture>,
    capture_request: Option<Value>,
    local_preview: Option<preview::LocalPreview>,
    preview_input: preview::Slot,
    preview: Option<Child>,
    receiver_sdp: Option<String>,
    last_error: Option<String>,
}
impl Worker {
    fn session(&mut self) -> Result<&mut native::Session, String> {
        self.session
            .as_mut()
            .ok_or_else(|| "Connect a PK media ticket first".into())
    }
    fn rebuild_capture(&mut self) -> Result<(), String> {
        self.capture.take();
        if let Some(args) = self.capture_request.clone() {
            let session = self
                .session
                .as_ref()
                .ok_or("Connect and publish before starting capture")?;
            let tracks = session.active_tracks();
            if !tracks
                .iter()
                .any(|track| args[&track.kind].as_bool().unwrap_or(true))
            {
                return Err("No active publication was selected for capture".into());
            }
            self.capture = Some(capture::Capture::start_shared(
                Some(session.native.functions),
                &tracks,
                args,
                self.preview_input.clone(),
            )?);
        } else if self.local_preview.is_some() {
            self.capture = Some(capture::Capture::start_shared(
                None,
                &[],
                json!({}),
                self.preview_input.clone(),
            )?);
        }
        Ok(())
    }
    fn stop_local_preview(&mut self) {
        *self.preview_input.lock().unwrap() = None;
        self.local_preview.take();
        if self.capture_request.is_none() {
            self.capture.take();
        }
    }
    fn call(&mut self, method: &str, args: &Value) -> Result<Value, String> {
        match method {
            "doctor" => Ok(
                json!({"rtc":native::doctor(),"encoders":capture::encoders().unwrap_or_else(|e|json!({"error":e}))}),
            ),
            "encoders" => capture::encoders(),
            "encoder.probe" => capture::probe(args),
            "self-test" => native::self_test(),
            "connect" => {
                let token = args
                    .get("access_token")
                    .or_else(|| args.get("ticket"))
                    .and_then(Value::as_str)
                    .ok_or("access_token is required")?;
                let uid = positive_id(args, "uid")?;
                let channel = args["channel_id"]
                    .as_u64()
                    .or_else(|| args["channel_id"].as_str().and_then(|s| s.parse().ok()))
                    .filter(|n| *n > 0)
                    .ok_or("channel_id is required")?;
                self.capture_request = None;
                self.capture.take();
                self.close_preview();
                if let Some(mut previous) = self.session.take() {
                    let _ = previous.leave();
                }
                let session = match native::Session::connect(token, uid, channel) {
                    Ok(session) => session,
                    Err(error) => {
                        if self.rebuild_capture().is_err() {
                            self.stop_local_preview();
                        }
                        return Err(error);
                    }
                };
                let mut result = json!({"status":session.status(),"members":session.join_response});
                self.session = Some(session);
                self.last_error = None;
                if let Err(error) = self.rebuild_capture() {
                    result["local_preview_error"] = error.into();
                    self.stop_local_preview();
                }
                Ok(result)
            }
            "request" => {
                let kind = args["method"]
                    .as_str()
                    .ok_or("RTC request method is required")?;
                let expected=match kind{"members_req"=>"members_resp","mute_req"=>"user_mute_resp","update_sub_req"=>"update_sub_resp","user_msg_req"=>"user_msg_resp","client_stats_req"=>"client_stats_resp",_=>return Err("Use the dedicated connect, publish, subscribe or leave operation for lifecycle requests".into())};
                self.session()?.request(
                    kind,
                    args.get("params").cloned().unwrap_or_else(|| json!({})),
                    expected,
                )
            }
            "status" => Ok(
                json!({"rtc":self.session.as_ref().map(native::Session::status).unwrap_or_else(||json!({"connected":false,"joined":false})),"capture":self.capture.as_ref().map(capture::Capture::status),"local_preview":self.local_preview.as_ref().map(preview::LocalPreview::status),"preview":self.preview.as_mut().is_some_and(|child|child.try_wait().ok().flatten().is_none()),"error":self.last_error}),
            ),
            "events" => Ok(self.session()?.events()),
            "members" => {
                let callid = args.get("callid").cloned().unwrap_or_else(|| {
                    self.session
                        .as_ref()
                        .map(|s| s.join_response["callid"].clone())
                        .unwrap_or(Value::Null)
                });
                let session = self.session()?;
                session.request("members_req",json!({"channel_id":session.ticket["channel_id"],"uid":session.ticket["uid"],"callid":callid}),"members_resp")
            }
            "publish" => {
                let video = args["video"].as_bool().unwrap_or(true);
                let audio = args["audio"].as_bool().unwrap_or(true);
                let codec = args["codec"].as_str().unwrap_or("h264");
                if !video && !audio {
                    return self.call("publication.stop", args);
                }
                self.capture_request = None;
                self.capture.take();
                let mut result = match self.session()?.publish(video, audio, codec) {
                    Ok(result) => result,
                    Err(error) => {
                        let _ = self.rebuild_capture();
                        return Err(error);
                    }
                };
                if args["capture"].as_bool().unwrap_or(true) {
                    self.capture_request = Some(args.clone());
                }
                if let Err(error) = self.rebuild_capture() {
                    self.capture_request = None;
                    let _ = self.session()?.publish(false, false, codec);
                    if self.rebuild_capture().is_err() {
                        self.stop_local_preview();
                    }
                    return Err(error);
                }
                if let Some(capture) = &self.capture {
                    result["capture"] = capture.status();
                }
                self.last_error = None;
                Ok(result)
            }
            "publication.stop" => {
                self.capture_request = None;
                self.capture.take();
                let result = self.session()?.publish(false, false, "h264");
                let preview = self.rebuild_capture();
                result.and_then(|result| preview.map(|_| result))
            }
            "capture.start" => {
                self.capture_request = Some(args.clone());
                if let Err(error) = self.rebuild_capture() {
                    self.capture_request = None;
                    if self.rebuild_capture().is_err() {
                        self.stop_local_preview();
                    }
                    return Err(error);
                }
                self.last_error = None;
                Ok(self
                    .capture
                    .as_ref()
                    .map(capture::Capture::status)
                    .unwrap_or(Value::Null))
            }
            "capture.stop" => {
                self.capture_request = None;
                self.rebuild_capture()?;
                Ok(json!({"stopped":true}))
            }
            "local.preview" => {
                let preview = preview::LocalPreview::new(args)?;
                self.stop_local_preview();
                *self.preview_input.lock().unwrap() = Some(preview.feed());
                self.local_preview = Some(preview);
                if self.capture.is_none() {
                    if let Err(error) = self.rebuild_capture() {
                        self.stop_local_preview();
                        return Err(error);
                    }
                }
                Ok(self.local_preview.as_ref().unwrap().status())
            }
            "local.preview.stop" => {
                self.stop_local_preview();
                Ok(json!({"stopped":true}))
            }
            "subscribe" => {
                let uid = positive_id(args, "uid")?;
                let video = args["video"].as_bool().unwrap_or(true);
                let audio = args["audio"].as_bool().unwrap_or(true);
                let video_port = port(args, "video_port", 19020)?;
                let audio_port = port(args, "audio_port", 19022)?;
                let result = self
                    .session()?
                    .subscribe(uid, video, audio, video_port, audio_port)?;
                self.receiver_sdp = result["receiver_sdp"].as_str().map(str::to_owned);
                if args["preview"].as_bool().unwrap_or(true) {
                    self.preview(args)?;
                }
                Ok(result)
            }
            "subscription.stop" | "subscription.update" => {
                let uid = positive_id(args, "uid")?;
                let video =
                    method != "subscription.stop" && args["video"].as_bool().unwrap_or(false);
                let audio =
                    method != "subscription.stop" && args["audio"].as_bool().unwrap_or(false);
                if !video && !audio {
                    self.close_preview();
                }
                self.session()?.update_subscription(uid, video, audio)
            }
            "preview" => self.preview(args),
            "preview.stop" => {
                self.close_preview();
                Ok(json!({"stopped":true}))
            }
            "mute" => {
                let kind = args
                    .get("kind")
                    .or_else(|| args.get("media"))
                    .and_then(Value::as_str)
                    .unwrap_or("audio");
                let muted = args["muted"].as_bool().ok_or("muted must be a boolean")?;
                if muted {
                    if let Some(capture) = &self.capture {
                        capture.mute(kind, true);
                    }
                }
                let result = self.session()?.mute(kind, muted)?;
                if !muted {
                    if let Some(capture) = &self.capture {
                        capture.mute(kind, false);
                    }
                }
                Ok(result)
            }
            "leave" | "disconnect" => {
                self.capture_request = None;
                self.capture.take();
                self.close_preview();
                let result = match self.session.take() {
                    Some(mut session) => session.leave(),
                    None => Ok(json!({})),
                };
                self.receiver_sdp = None;
                let local = self.rebuild_capture();
                result.and_then(|_| local.map(|_| json!({"disconnected":true})))
            }
            _ => Err(format!("Unknown native media operation: {method}")),
        }
    }
    fn preview(&mut self, args: &Value) -> Result<Value, String> {
        let sdp = self
            .receiver_sdp
            .clone()
            .ok_or("Subscribe to a peer before opening a preview")?;
        self.close_preview();
        let mut command = crate::bundle::command("ffplay")?;
        command.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-protocol_whitelist",
            "file,udp,rtp,pipe,crypto",
            "-fflags",
            "nobuffer",
            "-flags",
            "low_delay",
            "-window_title",
            "Bilibilive · PK",
            "-f",
            "sdp",
            "-i",
            "pipe:0",
        ]);
        if args["muted"].as_bool().unwrap_or(false) {
            command.arg("-an");
        }
        if let Some(volume) = args["volume"].as_u64() {
            command.args(["-volume", &volume.min(100).to_string()]);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Open native FFplay preview: {e}"))?;
        if let Some(mut input) = child.stdin.take() {
            if let Err(error) = input.write_all(sdp.as_bytes()) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
        }
        self.preview = Some(child);
        Ok(json!({"preview":true,"player":"ffplay"}))
    }
    fn close_preview(&mut self) {
        if let Some(mut child) = self.preview.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.capture_request = None;
        self.capture.take();
        self.stop_local_preview();
        self.close_preview();
        if let Some(mut session) = self.session.take() {
            let _ = session.leave();
        }
    }
}
fn positive_id(args: &Value, key: &str) -> Result<u64, String> {
    args[key]
        .as_u64()
        .or_else(|| args[key].as_str().and_then(|value| value.parse().ok()))
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("{key} must be a positive integer or numeric string"))
}
fn port(args: &Value, key: &str, default: u16) -> Result<u16, String> {
    match args.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .filter(|n| *n > 0 && *n <= 65535)
            .map(|n| n as u16)
            .ok_or_else(|| format!("{key} must be in 1..65535")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manager_status_and_required_ticket() {
        let manager = MediaManager::new();
        assert_eq!(
            manager.call("status", &json!({})).unwrap()["rtc"]["connected"],
            false
        );
        assert!(
            manager
                .call("connect", &json!({}))
                .unwrap_err()
                .contains("access_token")
        );
        assert!(manager.call("publish", &json!({})).is_err());
        assert_eq!(
            manager.call("disconnect", &json!({})).unwrap()["disconnected"],
            true
        );
    }
}
