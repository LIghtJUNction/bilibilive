//! Local OBS preview and explicit audio monitoring on the shared capture input.
use super::capture::Frame;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    process::{Child, ChildStdin, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const AUDIO_BUDGET: usize = 48_000 * 4 / 5;
pub type Slot = Arc<Mutex<Option<Feed>>>;

#[derive(Default)]
struct AudioQueue {
    bytes: usize,
    packets: VecDeque<Vec<u8>>,
}

struct State {
    video: bool,
    audio: bool,
    muted: bool,
    volume: u8,
    cancelled: AtomicBool,
    video_alive: AtomicBool,
    audio_alive: AtomicBool,
    frame: Mutex<Option<Frame>>,
    video_wake: Condvar,
    audio_queue: Mutex<AudioQueue>,
    audio_wake: Condvar,
    epoch: Instant,
    last_video_ms: AtomicU64,
    video_frames: AtomicU64,
    audio_frames: AtomicU64,
    dropped_video: AtomicU64,
    dropped_audio: AtomicU64,
    source: Mutex<Option<(u32, u32, u32)>>,
    error: Mutex<Option<String>>,
    diagnostics: Mutex<String>,
    children: Mutex<Vec<Arc<Mutex<Child>>>>,
    dummy_devices: bool,
}

#[derive(Clone)]
pub struct Feed {
    state: Arc<State>,
}
impl Feed {
    pub fn mask(&self) -> u8 {
        if self.state.cancelled.load(Ordering::Relaxed) {
            return 0;
        }
        u8::from(self.state.audio && self.state.audio_alive.load(Ordering::Relaxed))
            | (u8::from(self.state.video && self.state.video_alive.load(Ordering::Relaxed)) << 1)
    }
    pub fn video(&self, frame: &Frame) {
        if !self.wants_video_frame() {
            return;
        }
        let now = self.state.epoch.elapsed().as_millis() as u64;
        self.state.last_video_ms.store(now, Ordering::Relaxed);
        let mut mailbox = self.state.frame.lock().unwrap();
        if mailbox.replace(frame.clone()).is_some() {
            self.state.dropped_video.fetch_add(1, Ordering::Relaxed);
        }
        self.state.video_wake.notify_one();
    }
    pub fn wants_video_frame(&self) -> bool {
        let previous = self.state.last_video_ms.load(Ordering::Relaxed);
        self.mask() & 2 != 0
            && (previous == u64::MAX
                || self.state.epoch.elapsed().as_millis() as u64 - previous >= 66)
    }
    pub fn audio(&self, bytes: &[u8]) {
        if self.mask() & 1 == 0 || bytes.len() > AUDIO_BUDGET {
            return;
        }
        let mut queue = self.state.audio_queue.lock().unwrap();
        while queue.bytes + bytes.len() > AUDIO_BUDGET {
            let Some(old) = queue.packets.pop_front() else {
                break;
            };
            queue.bytes -= old.len();
            self.state
                .dropped_audio
                .fetch_add(old.len() as u64 / 4, Ordering::Relaxed);
        }
        queue.bytes += bytes.len();
        queue.packets.push_back(bytes.to_vec());
        self.state.audio_wake.notify_one();
    }
}

pub struct LocalPreview {
    feed: Feed,
    workers: Vec<thread::JoinHandle<()>>,
}
impl LocalPreview {
    pub fn new(args: &Value) -> Result<Self, String> {
        Self::create(args, false)
    }
    fn create(args: &Value, dummy_devices: bool) -> Result<Self, String> {
        let video = args["video"].as_bool().unwrap_or(true);
        // Audio output is always an explicit opt-in, including for reused input.
        let audio = args["audio"].as_bool() == Some(true);
        if !video && !audio {
            return Err("Select video or explicitly enable audio monitoring".into());
        }
        let volume = args["volume"].as_u64().unwrap_or(100).min(100) as u8;
        let state = Arc::new(State {
            video,
            audio,
            muted: args["muted"].as_bool().unwrap_or(false),
            volume,
            cancelled: AtomicBool::new(false),
            video_alive: AtomicBool::new(video),
            audio_alive: AtomicBool::new(audio),
            frame: Mutex::new(None),
            video_wake: Condvar::new(),
            audio_queue: Mutex::new(AudioQueue::default()),
            audio_wake: Condvar::new(),
            epoch: Instant::now(),
            last_video_ms: AtomicU64::new(u64::MAX),
            video_frames: AtomicU64::new(0),
            audio_frames: AtomicU64::new(0),
            dropped_video: AtomicU64::new(0),
            dropped_audio: AtomicU64::new(0),
            source: Mutex::new(None),
            error: Mutex::new(None),
            diagnostics: Mutex::new(String::new()),
            children: Mutex::new(Vec::new()),
            dummy_devices,
        });
        let mut workers = Vec::new();
        if video {
            let state = state.clone();
            workers.push(thread::spawn(move || {
                if let Err(error) = play_video(&state) {
                    if !state.cancelled.load(Ordering::Relaxed) {
                        *state.error.lock().unwrap() = Some(error);
                    }
                }
                state.video_alive.store(false, Ordering::Relaxed);
            }));
        }
        if audio {
            let state = state.clone();
            workers.push(thread::spawn(move || {
                if let Err(error) = play_audio(&state) {
                    if !state.cancelled.load(Ordering::Relaxed) {
                        *state.error.lock().unwrap() = Some(error);
                    }
                }
                state.audio_alive.store(false, Ordering::Relaxed);
            }));
        }
        Ok(Self {
            feed: Feed { state },
            workers,
        })
    }
    pub fn feed(&self) -> Feed {
        self.feed.clone()
    }
    pub fn status(&self) -> Value {
        let s = &self.feed.state;
        let dimensions = *s.source.lock().unwrap();
        json!({"running":self.feed.mask()!=0,"video":s.video,"audio":s.audio,"muted":s.muted,"volume":s.volume,
            "video_frames":s.video_frames.load(Ordering::Relaxed),"audio_frames":s.audio_frames.load(Ordering::Relaxed),
            "dropped_video_frames":s.dropped_video.load(Ordering::Relaxed),"dropped_audio_frames":s.dropped_audio.load(Ordering::Relaxed),
            "source":dimensions.map(|(width,height,format)|json!({"width":width,"height":height,"format":if format==1{"bgra"}else{"nv12"}})),
            "error":s.error.lock().unwrap().clone(),"diagnostics":s.diagnostics.lock().unwrap().clone(),"player":"ffplay","input":"shared OBS SLPK v2"})
    }
}
impl Drop for LocalPreview {
    fn drop(&mut self) {
        let state = &self.feed.state;
        state.cancelled.store(true, Ordering::Relaxed);
        state.video_wake.notify_all();
        state.audio_wake.notify_all();
        for child in state.children.lock().unwrap().iter() {
            let _ = child.lock().unwrap().kill();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

struct Player {
    stdin: Option<ChildStdin>,
    child: Arc<Mutex<Child>>,
    stderr: Option<thread::JoinHandle<()>>,
    state: Arc<State>,
}
impl Player {
    fn spawn(state: &Arc<State>, frame: Option<&Frame>) -> Result<Self, String> {
        if state.cancelled.load(Ordering::Relaxed) {
            return Err("Local preview cancelled".into());
        }
        let mut command = crate::bundle::command("ffplay")?;
        command.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-autoexit",
            "-fflags",
            "nobuffer",
            "-flags",
            "low_delay",
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
        ]);
        if state.dummy_devices {
            command
                .env("SDL_VIDEODRIVER", "dummy")
                .env("SDL_AUDIODRIVER", "dummy");
        }
        if let Some(frame) = frame {
            let scale = (f64::from(frame.width) / 1280.0)
                .max(f64::from(frame.height) / 720.0)
                .max(1.0);
            let width = (f64::from(frame.width) / scale) as u32;
            let height = (f64::from(frame.height) / scale) as u32;
            command.args([
                "-f",
                "rawvideo",
                "-pixel_format",
                if frame.format == 1 { "bgra" } else { "nv12" },
                "-video_size",
                &format!("{}x{}", frame.width, frame.height),
                "-framerate",
                "15",
                "-framedrop",
                "-sync",
                "video",
                "-window_title",
                "Bilibilive · Local OBS",
                "-x",
                &width.to_string(),
                "-y",
                &height.to_string(),
                "-an",
            ]);
        } else {
            command.args([
                "-nodisp",
                "-f",
                "s16le",
                "-ar",
                "48000",
                "-ch_layout",
                "stereo",
                "-volume",
                &if state.muted { 0 } else { state.volume }.to_string(),
            ]);
        }
        command.args(["-i", "pipe:0"]);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("Start local FFplay preview: {error}"))?;
        let stdin = child.stdin.take();
        let stderr_pipe = child.stderr.take();
        let child = Arc::new(Mutex::new(child));
        state.children.lock().unwrap().push(child.clone());
        if state.cancelled.load(Ordering::Relaxed) {
            let _ = child.lock().unwrap().kill();
        }
        let diagnostic = state.clone();
        let stderr = stderr_pipe.map(|mut stream| {
            thread::spawn(move || {
                let mut last = Vec::new();
                let mut bytes = [0; 1024];
                while let Ok(count) = stream.read(&mut bytes) {
                    if count == 0 {
                        break;
                    }
                    last.extend_from_slice(&bytes[..count]);
                    if last.len() > 4096 {
                        let excess = last.len() - 4096;
                        last.drain(..excess);
                    }
                    *diagnostic.diagnostics.lock().unwrap() =
                        String::from_utf8_lossy(&last).trim().to_owned();
                }
            })
        });
        Ok(Self {
            stdin,
            child,
            stderr,
            state: state.clone(),
        })
    }
    fn write(&mut self, bytes: &[u8]) -> Result<bool, String> {
        if let Some(status) = self
            .child
            .lock()
            .unwrap()
            .try_wait()
            .map_err(|error| error.to_string())?
        {
            if status.success() {
                return Ok(false);
            }
            return Err(format!(
                "Local preview exited with {status}: {}",
                self.state.diagnostics.lock().unwrap().as_str()
            ));
        }
        self.stdin
            .as_mut()
            .ok_or("Local preview input closed")?
            .write_all(bytes)
            .map(|_| true)
            .map_err(|error| format!("Write local preview: {error}"))
    }
}
impl Drop for Player {
    fn drop(&mut self) {
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        self.stdin.take();
        let _ = child.wait();
        drop(child);
        if let Some(worker) = self.stderr.take() {
            let _ = worker.join();
        }
        self.state
            .children
            .lock()
            .unwrap()
            .retain(|child| !Arc::ptr_eq(child, &self.child));
    }
}

fn play_video(state: &Arc<State>) -> Result<(), String> {
    let mut player: Option<Player> = None;
    let mut shape = None;
    while !state.cancelled.load(Ordering::Relaxed) {
        let frame = {
            let mut mailbox = state.frame.lock().unwrap();
            while mailbox.is_none() && !state.cancelled.load(Ordering::Relaxed) {
                mailbox = state
                    .video_wake
                    .wait_timeout(mailbox, Duration::from_millis(100))
                    .unwrap()
                    .0;
            }
            mailbox.take()
        };
        let Some(frame) = frame else {
            continue;
        };
        let incoming = (frame.width, frame.height, frame.format);
        if shape != Some(incoming) {
            player.take();
            player = Some(Player::spawn(state, Some(&frame))?);
            shape = Some(incoming);
            *state.source.lock().unwrap() = shape;
        }
        if !player.as_mut().unwrap().write(&frame.bytes)? {
            break;
        }
        state.video_frames.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}
fn play_audio(state: &Arc<State>) -> Result<(), String> {
    let mut player = Player::spawn(state, None)?;
    while !state.cancelled.load(Ordering::Relaxed) {
        let bytes = {
            let mut queue = state.audio_queue.lock().unwrap();
            while queue.packets.is_empty() && !state.cancelled.load(Ordering::Relaxed) {
                queue = state
                    .audio_wake
                    .wait_timeout(queue, Duration::from_millis(100))
                    .unwrap()
                    .0;
            }
            let bytes = queue.packets.pop_front();
            if let Some(bytes) = &bytes {
                queue.bytes -= bytes.len();
            }
            bytes
        };
        let Some(bytes) = bytes else {
            continue;
        };
        if !player.write(&bytes)? {
            break;
        }
        state
            .audio_frames
            .fetch_add(bytes.len() as u64 / 4, Ordering::Relaxed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audio_is_explicit_and_queues_are_bounded() {
        let preview = LocalPreview::create(&json!({}), true).unwrap();
        let feed = preview.feed();
        feed.audio(&vec![0; 1920]);
        assert_eq!(feed.state.audio_queue.lock().unwrap().bytes, 0);
        assert_eq!(feed.mask(), 2);
        drop(preview);
        let preview =
            LocalPreview::create(&json!({"video":false,"audio":true,"muted":true}), true).unwrap();
        let feed = preview.feed();
        for _ in 0..100 {
            feed.audio(&vec![0; 19200]);
        }
        assert!(feed.state.audio_queue.lock().unwrap().bytes <= AUDIO_BUDGET);
        drop(preview);
    }
    #[test]
    fn local_video_player_closes_without_a_rtc_connection() {
        if std::process::Command::new("ffplay")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_err()
        {
            return;
        }
        let preview = LocalPreview::create(&json!({"video":true}), true).unwrap();
        let feed = preview.feed();
        let frame = Frame {
            timestamp: 0,
            width: 32,
            height: 32,
            format: 1,
            colorspace: 2,
            range: 2,
            bytes: vec![0; 32 * 32 * 4],
        };
        for _ in 0..3 {
            feed.video(&frame);
            thread::sleep(Duration::from_millis(80));
        }
        assert!(preview.status()["video_frames"].as_u64().unwrap() > 0);
        let started = Instant::now();
        drop(preview);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(feed.mask(), 0);
    }
}
