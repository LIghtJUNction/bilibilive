//! SLPK v2 capture and native FFmpeg RTP encoding.
use super::native::{Functions, Track};
use serde_json::{Value, json};
use std::{
    ffi::CString,
    io::{Read, Write},
    net::UdpSocket,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
const MAX_VIDEO: usize = 3840 * 2160 * 4;
const SHARED_SIZE: usize = 64 + MAX_VIDEO;

#[derive(Clone, Debug)]
pub struct Frame {
    pub timestamp: u64,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub colorspace: u32,
    pub range: u32,
    pub bytes: Vec<u8>,
}
#[derive(Debug)]
pub enum Packet {
    Shared(String),
    Video(u64),
    Audio {
        timestamp: u64,
        frames: u32,
        bytes: Vec<u8>,
    },
}
pub fn decode_packet(header: &[u8; 36], payload: Vec<u8>) -> Result<Packet, String> {
    let read32 = |at| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
    if &header[..4] != b"SLPK" || header[4] != 2 || header[6..8] != [0, 0] {
        return Err("OBS PK media protocol mismatch (expected SLPK v2)".into());
    }
    let size = read32(8) as usize;
    let timestamp = u64::from_le_bytes(header[12..20].try_into().unwrap());
    let width = read32(20);
    let height = read32(24);
    let rate = read32(28);
    let frames = read32(32);
    if size != payload.len() {
        return Err("Incomplete OBS media payload".into());
    }
    match header[5] {
        2 if width == 0
            && height == 0
            && rate == 48000
            && (1..=4800).contains(&frames)
            && size == frames as usize * 4 =>
        {
            Ok(Packet::Audio {
                timestamp,
                frames,
                bytes: payload,
            })
        }
        1 | 3 if timestamp == 0 && width == 0 && height == 0 && rate == 0 && frames == 0 => {
            if header[5] == 1 && size == 8 {
                let sequence = u64::from_le_bytes(payload.try_into().unwrap());
                if sequence == 0 {
                    return Err("OBS frame sequence is zero".into());
                }
                Ok(Packet::Video(sequence))
            } else if header[5] == 3
                && size == 32
                && payload
                    .iter()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
            {
                Ok(Packet::Shared(String::from_utf8(payload).unwrap()))
            } else {
                Err("Invalid OBS shared-memory message".into())
            }
        }
        _ => Err("Invalid OBS media header fields".into()),
    }
}
fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("Media capture cancelled".into())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
pub struct Transport(std::os::unix::net::UnixStream);
#[cfg(unix)]
impl Transport {
    pub fn connect(cancel: &AtomicBool, audio: bool, video: bool) -> Result<Self, String> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let uid = unsafe { libc::getuid() };
        let directory = std::path::PathBuf::from(format!("/tmp/startlive-pk-{uid}"));
        let path = directory.join("media-v2.sock");
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            cancelled(cancel)?;
            match (
                std::fs::symlink_metadata(&directory),
                std::fs::symlink_metadata(&path),
            ) {
                (Ok(dir), Ok(socket)) => {
                    if !dir.is_dir()
                        || dir.uid() != uid
                        || dir.mode() & 0o077 != 0
                        || !socket.file_type().is_socket()
                        || socket.uid() != uid
                    {
                        return Err("OBS media socket must belong to the current user in a private directory".into());
                    }
                    if let Ok(socket) = std::os::unix::net::UnixStream::connect(&path) {
                        socket
                            .set_read_timeout(Some(Duration::from_millis(100)))
                            .map_err(|e| e.to_string())?;
                        socket
                            .set_write_timeout(Some(Duration::from_secs(3)))
                            .map_err(|e| e.to_string())?;
                        let mut transport = Self(socket);
                        transport.select(cancel, audio, video)?;
                        return Ok(transport);
                    }
                }
                _ => {}
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "OBS PK capture unavailable at {}; enable Bilibilive PK capture in OBS Tools",
                    path.display()
                ));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(bytes)
    }
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.0.write_all(bytes)
    }
}
#[cfg(windows)]
pub struct Transport(std::fs::File);
#[cfg(windows)]
impl Transport {
    pub fn connect(cancel: &AtomicBool, audio: bool, video: bool) -> Result<Self, String> {
        use std::os::windows::io::AsRawHandle;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn SetNamedPipeHandleState(
                handle: *mut std::ffi::c_void,
                mode: *const u32,
                max: *const u32,
                timeout: *const u32,
            ) -> i32;
        }
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            cancelled(cancel)?;
            if let Ok(file) = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(r"\\.\pipe\StartLive.PkMedia.v2")
            {
                let mode = 1u32;
                if unsafe {
                    SetNamedPipeHandleState(
                        file.as_raw_handle(),
                        &mode,
                        std::ptr::null(),
                        std::ptr::null(),
                    )
                } == 0
                {
                    return Err(std::io::Error::last_os_error().to_string());
                }
                let mut transport = Self(file);
                transport.select(cancel, audio, video)?;
                return Ok(transport);
            }
            if Instant::now() > deadline {
                return Err(
                    "OBS PK named pipe unavailable; enable Bilibilive PK capture in OBS Tools"
                        .into(),
                );
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(bytes)
    }
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.0.write_all(bytes)
    }
}
impl Transport {
    fn exact(&mut self, cancel: &AtomicBool, bytes: &mut [u8]) -> Result<(), String> {
        let mut offset = 0;
        let mut deadline = Instant::now() + Duration::from_secs(10);
        while offset < bytes.len() {
            cancelled(cancel)?;
            match self.read(&mut bytes[offset..]) {
                Ok(0) => return Err("OBS capture channel closed".into()),
                Ok(count) => {
                    offset += count;
                    deadline = Instant::now() + Duration::from_secs(10);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) || error.raw_os_error() == Some(232) =>
                {
                    if Instant::now() > deadline {
                        return Err("OBS capture frame timed out".into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(())
    }
    fn select(&mut self, cancel: &AtomicBool, audio: bool, video: bool) -> Result<(), String> {
        let mask = u8::from(audio) | (u8::from(video) << 1);
        if mask == 0 {
            return Err("Select at least one capture stream".into());
        }
        let request = [b'S', b'L', b'P', b'C', 1, mask, 0, 0];
        self.write(&request).map_err(|e| e.to_string())?;
        let mut reply = [0; 8];
        self.exact(cancel, &mut reply)?;
        if reply != [b'S', b'L', b'P', b'A', 1, mask, 0, 0] {
            return Err("OBS capture selection was not acknowledged; update the plugin".into());
        }
        Ok(())
    }
    pub fn packet(&mut self, cancel: &AtomicBool) -> Result<Packet, String> {
        let mut header = [0u8; 36];
        self.exact(cancel, &mut header)?;
        let size = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
        if size > 4800 * 4 {
            return Err("OBS media packet exceeds bounds".into());
        }
        let mut payload = vec![0; size];
        self.exact(cancel, &mut payload)?;
        decode_packet(&header, payload)
    }
}

pub struct SharedReader {
    mapping: *mut std::ffi::c_void,
    lock: *mut std::ffi::c_void,
    #[cfg(unix)]
    fd: i32,
    #[cfg(windows)]
    file: *mut std::ffi::c_void,
    sequence: u64,
}
impl SharedReader {
    pub fn open(token: &str) -> Result<Self, String> {
        if token.len() != 32
            || !token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("Invalid OBS shared-memory token".into());
        }
        Self::open_platform(token)
    }
    #[cfg(unix)]
    fn open_platform(token: &str) -> Result<Self, String> {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::fs::MetadataExt;
        let name = CString::new(format!("/slpk-m-{}", &token[..22])).unwrap();
        let lock_name = CString::new(format!("/slpk-l-{}", &token[..22])).unwrap();
        let fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDONLY, 0) };
        if fd < 0 {
            return Err(format!(
                "Open OBS shared memory: {}",
                std::io::Error::last_os_error()
            ));
        }
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        let uid = unsafe { libc::getuid() };
        if metadata.uid() != uid
            || metadata.mode() & 0o077 != 0
            || metadata.len() < SHARED_SIZE as u64
            || metadata.len() > SHARED_SIZE as u64 + 65536
        {
            return Err("OBS shared memory ownership, permissions or size mismatch".into());
        }
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                SHARED_SIZE,
                libc::PROT_READ,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if mapping == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let lock = unsafe { libc::sem_open(lock_name.as_ptr(), 0) };
        if lock == libc::SEM_FAILED {
            unsafe {
                libc::munmap(mapping, SHARED_SIZE);
            }
            return Err(std::io::Error::last_os_error().to_string());
        }
        use std::os::fd::IntoRawFd;
        let fd = file.into_raw_fd();
        let reader = Self {
            mapping,
            lock: lock.cast(),
            fd,
            sequence: 0,
        };
        if unsafe { libc::shm_unlink(name.as_ptr()) } != 0
            || unsafe { libc::sem_unlink(lock_name.as_ptr()) } != 0
        {
            return Err("Could not retire OBS shared-memory names".into());
        }
        Ok(reader)
    }
    #[cfg(windows)]
    fn open_platform(token: &str) -> Result<Self, String> {
        let name = format!("Local\\StartLive.PkVideo.{token}");
        let mut wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let file = unsafe { windows::OpenFileMappingW(4, 0, wide.as_ptr()) };
        if file.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        wide = format!("{name}.lock")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let lock = unsafe { windows::OpenMutexW(0x100001, 0, wide.as_ptr()) };
        if lock.is_null() {
            unsafe {
                windows::CloseHandle(file);
            }
            return Err(std::io::Error::last_os_error().to_string());
        }
        let mapping = unsafe { windows::MapViewOfFile(file, 4, 0, 0, SHARED_SIZE) };
        if mapping.is_null() {
            unsafe {
                windows::CloseHandle(file);
                windows::CloseHandle(lock);
            }
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(Self {
            mapping,
            lock,
            file,
            sequence: 0,
        })
    }
    fn acquire(&self) -> Result<bool, String> {
        #[cfg(unix)]
        {
            let result = unsafe { libc::sem_trywait(self.lock.cast()) };
            if result == 0 {
                return Ok(true);
            }
            let error = std::io::Error::last_os_error();
            if matches!(error.raw_os_error(), Some(libc::EAGAIN) | Some(libc::EINTR)) {
                Ok(false)
            } else {
                Err(error.to_string())
            }
        }
        #[cfg(windows)]
        {
            match unsafe { windows::WaitForSingleObject(self.lock, 0) } {
                0 => Ok(true),
                0x102 => Ok(false),
                0x80 => {
                    unsafe {
                        windows::ReleaseMutex(self.lock);
                    }
                    Err("OBS stopped while writing shared video".into())
                }
                _ => Err(std::io::Error::last_os_error().to_string()),
            }
        }
    }
    fn release(&self) {
        #[cfg(unix)]
        unsafe {
            libc::sem_post(self.lock.cast());
        }
        #[cfg(windows)]
        unsafe {
            windows::ReleaseMutex(self.lock);
        }
    }
    pub fn snapshot(&mut self, notice: u64) -> Result<Option<Frame>, String> {
        if !self.acquire()? {
            return Ok(None);
        }
        let result = self.copy_frame(notice);
        self.release();
        result
    }
    fn copy_frame(&mut self, notice: u64) -> Result<Option<Frame>, String> {
        let bytes = unsafe { std::slice::from_raw_parts(self.mapping.cast::<u8>(), SHARED_SIZE) };
        let r32 = |at| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let sequence = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let timestamp = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let width = r32(24);
        let height = r32(28);
        let format = r32(32);
        let colorspace = r32(36);
        let range = r32(40);
        let size = r32(44) as usize;
        if &bytes[..4] != b"SLVF"
            || r32(4) != 2
            || sequence < notice
            || width == 0
            || width > 3840
            || height == 0
            || height > 2160
            || !(1..=2).contains(&format)
            || !(1..=2).contains(&colorspace)
            || !(1..=2).contains(&range)
        {
            return Err("Invalid OBS shared video frame header".into());
        }
        let expected = if format == 1 {
            width as usize * height as usize * 4
        } else {
            if width % 2 != 0 || height % 2 != 0 {
                return Err("NV12 dimensions must be even".into());
            }
            width as usize * height as usize * 3 / 2
        };
        if size != expected || size > MAX_VIDEO {
            return Err("OBS shared frame length mismatch".into());
        }
        if sequence <= self.sequence {
            return Ok(None);
        }
        self.sequence = sequence;
        Ok(Some(Frame {
            timestamp,
            width,
            height,
            format,
            colorspace,
            range,
            bytes: bytes[64..64 + size].to_vec(),
        }))
    }
}
impl Drop for SharedReader {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::munmap(self.mapping, SHARED_SIZE);
            libc::close(self.fd);
            libc::sem_close(self.lock.cast());
        }
        #[cfg(windows)]
        unsafe {
            windows::UnmapViewOfFile(self.mapping);
            windows::CloseHandle(self.file);
            windows::CloseHandle(self.lock);
        }
    }
}
#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn OpenFileMappingW(access: u32, inherit: i32, name: *const u16) -> *mut c_void;
        pub fn OpenMutexW(access: u32, inherit: i32, name: *const u16) -> *mut c_void;
        pub fn MapViewOfFile(
            file: *mut c_void,
            access: u32,
            hi: u32,
            lo: u32,
            size: usize,
        ) -> *mut c_void;
        pub fn UnmapViewOfFile(address: *mut c_void) -> i32;
        pub fn CloseHandle(handle: *mut c_void) -> i32;
        pub fn WaitForSingleObject(handle: *mut c_void, timeout: u32) -> u32;
        pub fn ReleaseMutex(handle: *mut c_void) -> i32;
    }
}

#[derive(Default)]
pub struct Counters {
    pub video_frames: AtomicU64,
    pub audio_frames: AtomicU64,
    pub rtp_packets: AtomicU64,
    pub rtp_bytes: AtomicU64,
    pub audio_muted: AtomicBool,
    pub video_muted: AtomicBool,
    pub error: Mutex<Option<String>>,
    pub diagnostics: Mutex<String>,
}
pub struct Capture {
    cancel: Arc<AtomicBool>,
    pub counters: Arc<Counters>,
    video_config: Value,
    children: Arc<Mutex<Vec<Arc<Mutex<Child>>>>>,
    worker: Option<thread::JoinHandle<()>>,
}
struct Encoder {
    stdin: Option<ChildStdin>,
    child: Arc<Mutex<Child>>,
    registry: Arc<Mutex<Vec<Arc<Mutex<Child>>>>>,
    counters: Arc<Counters>,
    cancel: Arc<AtomicBool>,
    forwarder: Option<thread::JoinHandle<()>>,
    stderr: Option<thread::JoinHandle<()>>,
}

fn video_encoder_name<'a>(codec: &str, args: &'a Value) -> Result<&'a str, String> {
    let encoder = args["encoder"].as_str().unwrap_or(if codec == "av1" {
        "libsvtav1"
    } else {
        "libx264"
    });
    let valid = match codec {
        "h264" => matches!(
            encoder,
            "libx264" | "h264_nvenc" | "h264_qsv" | "h264_amf" | "h264_vaapi" | "h264_videotoolbox"
        ),
        "av1" => matches!(encoder, "libsvtav1" | "av1_nvenc" | "av1_qsv" | "av1_amf"),
        _ => false,
    };
    if valid {
        Ok(encoder)
    } else {
        Err(format!(
            "Encoder {encoder} does not implement the selected {codec} publication"
        ))
    }
}

// Low-delay policies translated from StartLive's video_encoder_backend.py.
// Selecting a hardware backend still requires that device and driver to exist.
fn encoder_options(command: &mut Command, name: &str, args: &Value, bitrate: u64) {
    let quality = args["quality"].as_str().unwrap_or("balanced");
    let level = args["_encoder_level"].as_str().unwrap_or(
        if name.starts_with("av1") || name == "libsvtav1" {
            "4.1"
        } else {
            "3.1"
        },
    );
    let level = if name == "libsvtav1" || name == "av1_qsv" {
        level.replace('.', "")
    } else {
        level.to_owned()
    };
    command.args(["-level:v", &level]);
    if name != "libsvtav1" {
        command.args(["-maxrate", &format!("{bitrate}k")]);
    }
    match name {
        "libx264" => {
            let preset = match quality {
                "performance" => "ultrafast",
                "quality" => "fast",
                _ => "veryfast",
            };
            command.args(["-preset",preset,"-tune","zerolatency","-profile:v","baseline","-x264-params",&format!("ref=1:bframes=0:repeat-headers=1:scenecut=0:vbv-maxrate={bitrate}:vbv-bufsize={bitrate}:nal-hrd=cbr")]);
        }
        "libsvtav1" => {
            let preset = match quality {
                "performance" => "12",
                "quality" => "8",
                _ => "10",
            };
            command.args([
                "-preset",
                preset,
                "-svtav1-params",
                "rc=2:rtc=1:pred-struct=1:lookahead=0:irefresh-type=2:aq-mode=2",
            ]);
        }
        "h264_nvenc" | "av1_nvenc" => {
            let preset = match quality {
                "performance" => "p1",
                "quality" => "p7",
                _ => "p4",
            };
            command.args([
                "-preset",
                preset,
                "-rc",
                "cbr",
                "-zerolatency",
                "1",
                "-forced-idr",
                "1",
                "-tune",
                "ull",
                "-delay",
                "0",
                "-rc-lookahead",
                "0",
                "-multipass",
                "disabled",
            ]);
            if name == "h264_nvenc" {
                command.args(["-profile:v", "baseline"]);
            } else {
                command.args(["-tier", "0", "-timing-info", "0"]);
            }
        }
        "h264_qsv" | "av1_qsv" => {
            let preset = match quality {
                "performance" => "veryfast",
                "quality" => "slow",
                _ => "medium",
            };
            command.args(["-preset", preset, "-async_depth", "1", "-forced_idr", "1"]);
            if name == "h264_qsv" {
                command.args(["-profile:v", "baseline"]);
            } else {
                command.args(["-profile:v", "main", "-look_ahead_depth", "0"]);
            }
        }
        "h264_amf" | "av1_amf" => {
            let preset = match quality {
                "performance" => "speed",
                "quality" => "quality",
                _ => "balanced",
            };
            command.args([
                "-quality",
                preset,
                "-usage",
                "ultralowlatency",
                "-rc",
                "cbr",
            ]);
            if name == "h264_amf" {
                command.args([
                    "-profile:v",
                    "constrained_baseline",
                    "-header_insertion_mode",
                    "idr",
                ]);
            } else {
                command.args([
                    "-profile:v",
                    "main",
                    "-async_depth",
                    "1",
                    "-preanalysis",
                    "0",
                    "-header_insertion_mode",
                    "frame",
                    "-forced_idr",
                    "1",
                ]);
            }
        }
        "h264_videotoolbox" => {
            command.args([
                "-profile:v",
                "baseline",
                "-realtime",
                "1",
                "-allow_sw",
                "0",
                "-constant_bit_rate",
                "1",
            ]);
        }
        "h264_vaapi" => {
            let level = match quality {
                "performance" => "7",
                "quality" => "1",
                _ => "4",
            };
            command.args([
                "-profile:v",
                "constrained_baseline",
                "-compression_level",
                level,
                "-rc_mode",
                "CBR",
            ]);
        }
        _ => {}
    }
}

impl Encoder {
    fn start(
        track: &Track,
        frame: Option<&Frame>,
        args: &Value,
        functions: Functions,
        counters: Arc<Counters>,
        children: &Arc<Mutex<Vec<Arc<Mutex<Child>>>>>,
        parent_stop: &AtomicBool,
    ) -> Result<Self, String> {
        cancelled(parent_stop)?;
        let socket = UdpSocket::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(|e| e.to_string())?;
        let port = socket.local_addr().map_err(|e| e.to_string())?.port();
        let mut command = crate::bundle::command("ffmpeg")?;
        command.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
        ]);
        if let Some(frame) = frame {
            let encoder = video_encoder_name(&track.codec, args)?;
            if encoder == "h264_vaapi" {
                command.args([
                    "-vaapi_device",
                    args["hw_device"].as_str().unwrap_or("/dev/dri/renderD128"),
                ]);
            }
            let fps = args["fps"].as_u64().unwrap_or(30).clamp(1, 60);
            let width = args["width"].as_u64().unwrap_or(1280).clamp(2, 3840) / 2 * 2;
            let height = args["height"].as_u64().unwrap_or(720).clamp(2, 2160) / 2 * 2;
            let pixel = if frame.format == 1 { "bgra" } else { "nv12" };
            command.args([
                "-f",
                "rawvideo",
                "-pixel_format",
                pixel,
                "-video_size",
                &format!("{}x{}", frame.width, frame.height),
                "-framerate",
                &fps.to_string(),
                "-i",
                "pipe:0",
                "-an",
            ]);
            let mut filters = Vec::new();
            if let Some(crop) = args.get("crop") {
                let x = crop["x"].as_u64().ok_or("crop.x required")?;
                let y = crop["y"].as_u64().ok_or("crop.y required")?;
                let w = crop["width"].as_u64().ok_or("crop.width required")?;
                let h = crop["height"].as_u64().ok_or("crop.height required")?;
                if w < 2
                    || h < 2
                    || x.checked_add(w).is_none_or(|n| n > frame.width as u64)
                    || y.checked_add(h).is_none_or(|n| n > frame.height as u64)
                {
                    return Err("Crop rectangle is outside OBS frame".into());
                }
                filters.push(format!("crop={w}:{h}:{x}:{y}"));
            }
            let matrix = if frame.colorspace == 1 {
                "bt601"
            } else {
                "bt709"
            };
            let range = if frame.range == 1 { "tv" } else { "pc" };
            let pixel_output = if encoder == "h264_vaapi" {
                "nv12,hwupload"
            } else if encoder.ends_with("_qsv") {
                "nv12"
            } else {
                "yuv420p"
            };
            filters.push(format!("scale={width}:{height}:flags=lanczos:in_color_matrix={matrix}:out_color_matrix=bt709:in_range={range}:out_range=tv,format={pixel_output}"));
            command.args([
                "-vf",
                &filters.join(","),
                "-c:v",
                encoder,
                "-colorspace",
                "bt709",
                "-color_primaries",
                "bt709",
                "-color_trc",
                "bt709",
                "-color_range",
                "tv",
            ]);
            let bitrate = args["bitrate_kbps"]
                .as_u64()
                .unwrap_or(2500)
                .clamp(16, 20000);
            command.args([
                "-b:v",
                &format!("{bitrate}k"),
                "-bufsize",
                &format!("{bitrate}k"),
                "-g",
                &(fps * 2).to_string(),
                "-bf",
                "0",
            ]);
            encoder_options(&mut command, encoder, args, bitrate);
        } else {
            command.args([
                "-f",
                "s16le",
                "-ar",
                "48000",
                "-ac",
                "2",
                "-i",
                "pipe:0",
                "-vn",
                "-c:a",
                "libopus",
                "-application",
                "lowdelay",
                "-frame_duration",
                "20",
                "-b:a",
                "128k",
            ]);
        }
        if track.codec == "av1" {
            command.args(["-strict", "experimental"]);
        }
        command.args([
            "-f",
            "rtp",
            "-payload_type",
            &track.payload_type.to_string(),
            "-ssrc",
            &(track.ssrc as i32).to_string(),
            "-flush_packets",
            "1",
            &format!("rtp://127.0.0.1:{port}?pkt_size=1100&rtcpport={port}"),
        ]);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|e| format!("Start FFmpeg encoder: {e}"))?;
        let stdin = child.stdin.take();
        let stderr_pipe = child.stderr.take();
        let child = Arc::new(Mutex::new(child));
        children.lock().unwrap().push(child.clone());
        // Cancellation can race with process creation. Once registered, either
        // the owner sees the child or this post-registration check retires it.
        if parent_stop.load(Ordering::Relaxed) {
            let mut process = child.lock().unwrap();
            let _ = process.kill();
            let _ = process.wait();
            return Err("Media capture cancelled".into());
        }
        let stderr_counters = counters.clone();
        let stderr = stderr_pipe.map(|mut pipe| {
            thread::spawn(move || {
                let mut last = String::new();
                let mut bytes = [0; 1024];
                while let Ok(count) = pipe.read(&mut bytes) {
                    if count == 0 {
                        break;
                    }
                    last.push_str(&String::from_utf8_lossy(&bytes[..count]));
                    if last.len() > 8192 {
                        let keep = last.len() - 8192;
                        let cut = (keep..last.len())
                            .find(|i| last.is_char_boundary(*i))
                            .unwrap_or(last.len());
                        last.drain(..cut);
                    }
                    *stderr_counters.diagnostics.lock().unwrap() = last.trim().to_owned();
                }
            })
        });
        let encoder_counters = counters.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let id = track.id;
        let sender = track.sender.clone();
        let clock_rate = if track.kind == "video" { 90000 } else { 48000 };
        let forwarder = thread::spawn(move || {
            let mut offset = None;
            let mut buffer = [0u8; 65536];
            while !stop.load(Ordering::Relaxed) {
                match socket.recv(&mut buffer) {
                    Ok(count) => {
                        let is_rtp =
                            match sender.rewrite(&mut buffer[..count], clock_rate, &mut offset) {
                                Ok(rtp) => rtp,
                                Err(error) => {
                                    *counters.error.lock().unwrap() = Some(error);
                                    break;
                                }
                            };
                        let status =
                            unsafe { (functions.send)(id, buffer.as_ptr().cast(), count as i32) };
                        if status < 0 {
                            *counters.error.lock().unwrap() =
                                Some(format!("Native RTP send failed ({status})"));
                            break;
                        }
                        if is_rtp {
                            counters.rtp_packets.fetch_add(1, Ordering::Relaxed);
                            counters
                                .rtp_bytes
                                .fetch_add(count as u64, Ordering::Relaxed);
                        }
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    Err(error) => {
                        *counters.error.lock().unwrap() = Some(error.to_string());
                        break;
                    }
                }
            }
        });
        Ok(Self {
            stdin,
            child,
            registry: children.clone(),
            counters: encoder_counters,
            cancel,
            forwarder: Some(forwarder),
            stderr,
        })
    }
    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        if let Some(status) = self
            .child
            .lock()
            .unwrap()
            .try_wait()
            .map_err(|e| e.to_string())?
        {
            return Err(format!(
                "FFmpeg encoder exited with {status}: {}",
                self.counters.diagnostics.lock().unwrap().as_str()
            ));
        }
        self.stdin
            .as_mut()
            .ok_or("Encoder input closed")?
            .write_all(bytes)
            .map_err(|e| format!("Feed FFmpeg encoder: {e}"))
    }
}
impl Drop for Encoder {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        self.stdin.take();
        let _ = child.wait();
        drop(child);
        if let Some(worker) = self.forwarder.take() {
            let _ = worker.join();
        }
        if let Some(worker) = self.stderr.take() {
            let _ = worker.join();
        }
        self.registry
            .lock()
            .unwrap()
            .retain(|child| !Arc::ptr_eq(child, &self.child));
    }
}
impl Capture {
    pub fn start(functions: Functions, tracks: &[Track], args: Value) -> Result<Self, String> {
        Self::start_shared(Some(functions), tracks, args, Arc::new(Mutex::new(None)))
    }
    pub(crate) fn start_shared(
        functions: Option<Functions>,
        tracks: &[Track],
        mut args: Value,
        previews: super::preview::Slot,
    ) -> Result<Self, String> {
        let video = tracks
            .iter()
            .find(|t| t.outgoing && t.kind == "video" && args["video"].as_bool().unwrap_or(true))
            .cloned();
        let audio = tracks
            .iter()
            .find(|t| t.outgoing && t.kind == "audio" && args["audio"].as_bool().unwrap_or(true))
            .cloned();
        if let Some(track) = &video {
            video_encoder_name(&track.codec, &args)?;
            track
                .limits
                .as_ref()
                .ok_or("Publish and negotiate video before starting capture")?
                .apply(&mut args)?;
        }
        let video_config = if video.is_some() {
            json!({"width":args["width"],"height":args["height"],"fps":args["fps"],"bitrate_kbps":args["bitrate_kbps"],"limits":args["_negotiated_limits"]})
        } else {
            Value::Null
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let publication_mask = u8::from(audio.is_some()) | (u8::from(video.is_some()) << 1);
        let mut active_mask = publication_mask
            | previews
                .lock()
                .unwrap()
                .as_ref()
                .map_or(0, super::preview::Feed::mask);
        let mut transport = Some(Transport::connect(
            &cancel,
            active_mask & 1 != 0,
            active_mask & 2 != 0,
        )?);
        let counters = Arc::new(Counters::default());
        let children = Arc::new(Mutex::new(Vec::new()));
        let state = counters.clone();
        let stop = cancel.clone();
        let processes = children.clone();
        let worker = thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let mut shared = None;
                let mut video_encoder: Option<Encoder> = None;
                let mut audio_encoder = match &audio {
                    Some(track) => Some(Encoder::start(
                        track,
                        None,
                        &args,
                        functions.ok_or("Native publication transport is unavailable")?,
                        state.clone(),
                        &processes,
                        &stop,
                    )?),
                    None => None,
                };
                let mut dimensions = None;
                let mut refreshed = Instant::now();
                let fps = args["fps"].as_u64().unwrap_or(30).clamp(1, 60);
                let mut next_frame = Instant::now();
                while !stop.load(Ordering::Relaxed) {
                    let preview = previews.lock().unwrap().clone();
                    let wanted =
                        publication_mask | preview.as_ref().map_or(0, super::preview::Feed::mask);
                    if wanted == 0 {
                        break;
                    }
                    if wanted & !active_mask != 0 {
                        // The protocol selects streams at connect time. Close the
                        // sole consumer before negotiating additional streams.
                        transport.take();
                        shared = None;
                        active_mask |= wanted;
                        transport = Some(Transport::connect(
                            &stop,
                            active_mask & 1 != 0,
                            active_mask & 2 != 0,
                        )?);
                    }
                    match transport
                        .as_mut()
                        .ok_or("OBS input is disconnected")?
                        .packet(&stop)?
                    {
                        Packet::Shared(token) => shared = Some(SharedReader::open(&token)?),
                        Packet::Video(sequence) => {
                            let encode_due = video.is_some() && Instant::now() >= next_frame;
                            if !encode_due
                                && !preview
                                    .as_ref()
                                    .is_some_and(super::preview::Feed::wants_video_frame)
                            {
                                continue;
                            }
                            let Some(mut frame) = shared
                                .as_mut()
                                .ok_or("OBS sent video before shared-memory setup")?
                                .snapshot(sequence)?
                            else {
                                continue;
                            };
                            if let Some(preview) = &preview {
                                preview.video(&frame);
                            }
                            if !encode_due {
                                continue;
                            }
                            let track = video
                                .as_ref()
                                .ok_or("Video publication track is unavailable")?;
                            let shape = (frame.width, frame.height, frame.format);
                            if dimensions != Some(shape)
                                || (refreshed.elapsed() > Duration::from_millis(200)
                                    && track.sender.keyframe.swap(false, Ordering::Relaxed))
                            {
                                video_encoder.take();
                                video_encoder = Some(Encoder::start(
                                    track,
                                    Some(&frame),
                                    &args,
                                    functions
                                        .ok_or("Native publication transport is unavailable")?,
                                    state.clone(),
                                    &processes,
                                    &stop,
                                )?);
                                dimensions = Some(shape);
                                refreshed = Instant::now();
                            }
                            if state.video_muted.load(Ordering::Relaxed) {
                                if frame.format == 2 {
                                    let plane = frame.width as usize * frame.height as usize;
                                    frame.bytes[..plane].fill(16);
                                    frame.bytes[plane..].fill(128);
                                } else {
                                    frame.bytes.fill(0);
                                    for pixel in frame.bytes.chunks_exact_mut(4) {
                                        pixel[3] = 255;
                                    }
                                }
                            }
                            video_encoder.as_mut().unwrap().write(&frame.bytes)?;
                            state.video_frames.fetch_add(1, Ordering::Relaxed);
                            next_frame += Duration::from_nanos(1_000_000_000 / fps);
                            if next_frame < Instant::now() {
                                next_frame = Instant::now();
                            }
                        }
                        Packet::Audio {
                            mut bytes, frames, ..
                        } => {
                            if let Some(preview) = &preview {
                                preview.audio(&bytes);
                            }
                            if let Some(encoder) = &mut audio_encoder {
                                if state.audio_muted.load(Ordering::Relaxed) {
                                    bytes.fill(0);
                                }
                                encoder.write(&bytes)?;
                                state
                                    .audio_frames
                                    .fetch_add(u64::from(frames), Ordering::Relaxed);
                            }
                        }
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                if !stop.load(Ordering::Relaxed) {
                    *state.error.lock().unwrap() = Some(error);
                }
            }
        });
        Ok(Self {
            cancel,
            counters,
            video_config,
            children,
            worker: Some(worker),
        })
    }
    pub fn status(&self) -> Value {
        json!({"running":self.worker.as_ref().is_some_and(|w|!w.is_finished()),"video":self.video_config,"video_frames":self.counters.video_frames.load(Ordering::Relaxed),"audio_frames":self.counters.audio_frames.load(Ordering::Relaxed),"rtp_packets":self.counters.rtp_packets.load(Ordering::Relaxed),"rtp_bytes":self.counters.rtp_bytes.load(Ordering::Relaxed),"audio_muted":self.counters.audio_muted.load(Ordering::Relaxed),"video_muted":self.counters.video_muted.load(Ordering::Relaxed),"error":self.counters.error.lock().unwrap().clone(),"diagnostics":self.counters.diagnostics.lock().unwrap().as_str()})
    }
    pub fn failure(&self) -> Option<String> {
        self.counters.error.lock().unwrap().clone()
    }
    pub fn mute(&self, kind: &str, value: bool) {
        if kind == "video" {
            self.counters.video_muted.store(value, Ordering::Relaxed);
        } else if kind == "audio" {
            self.counters.audio_muted.store(value, Ordering::Relaxed);
        }
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        for child in self.children.lock().unwrap().iter() {
            let _ = child.lock().unwrap().kill();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn encoders() -> Result<Value, String> {
    let result = crate::bundle::command("ffmpeg")?
        .args(["-hide_banner", "-encoders"])
        .output()
        .map_err(|e| e.to_string())?;
    if !result.status.success() {
        return Err("FFmpeg encoder discovery failed".into());
    }
    let text = String::from_utf8_lossy(&result.stdout);
    Ok(
        json!({"encoders":text.lines().filter(|line|line.contains("264")||line.contains("av1")||line.contains("opus")).map(str::trim).collect::<Vec<_>>()}),
    )
}

/// Exercise a backend and this FFmpeg build's RTP muxer without opening OBS or
/// connecting to Bilibili. The probe sends a short synthetic picture to loopback.
pub fn probe(args: &Value) -> Result<Value, String> {
    let codec = args["codec"].as_str().unwrap_or("h264");
    let encoder = video_encoder_name(codec, args)?;
    let socket = UdpSocket::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    socket
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let port = socket
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    let payload = if codec == "av1" { 98 } else { 96 };
    let mut command = crate::bundle::command("ffmpeg")?;
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
    if encoder == "h264_vaapi" {
        command.args([
            "-vaapi_device",
            args["hw_device"].as_str().unwrap_or("/dev/dri/renderD128"),
        ]);
    }
    command.args([
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=128x128:rate=10",
        "-frames:v",
        "8",
        "-an",
    ]);
    let pixel = if encoder == "h264_vaapi" {
        "format=nv12,hwupload"
    } else if encoder.ends_with("_qsv") {
        "format=nv12"
    } else {
        "format=yuv420p"
    };
    command.args([
        "-vf", pixel, "-c:v", encoder, "-b:v", "256k", "-bufsize", "256k", "-g", "20", "-bf", "0",
    ]);
    encoder_options(&mut command, encoder, args, 256);
    if codec == "av1" {
        command.args(["-strict", "experimental"]);
    }
    command.args([
        "-f",
        "rtp",
        "-payload_type",
        &payload.to_string(),
        "-ssrc",
        "100",
        "-flush_packets",
        "1",
        &format!("rtp://127.0.0.1:{port}?pkt_size=1100&rtcpport={port}"),
    ]);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Start encoder probe: {error}"))?;
    let diagnostics = Arc::new(Mutex::new(Vec::<u8>::new()));
    let captured = diagnostics.clone();
    let stderr = child.stderr.take().map(|mut stream| {
        thread::spawn(move || {
            let mut chunk = [0; 1024];
            while let Ok(count) = stream.read(&mut chunk) {
                if count == 0 {
                    break;
                }
                let mut text = captured.lock().unwrap();
                text.extend_from_slice(&chunk[..count]);
                if text.len() > 4096 {
                    let excess = text.len() - 4096;
                    text.drain(..excess);
                }
            }
        })
    });
    let started = Instant::now();
    let deadline = started + Duration::from_secs(12);
    let mut packets = 0u64;
    let mut bytes = 0u64;
    let mut reports = 0u64;
    let mut buffer = [0u8; 65536];
    let mut collect = || -> Result<(), String> {
        loop {
            match socket.recv(&mut buffer) {
                Ok(count) => {
                    let packet = &buffer[..count];
                    if super::rtp::is_rtcp(packet) {
                        reports += 1;
                    } else if count >= 13 && packet[0] >> 6 == 2 && packet[1] & 127 == payload {
                        packets += 1;
                        bytes += count as u64;
                    } else {
                        return Err("Encoder probe produced invalid RTP".into());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(error.to_string()),
            }
        }
    };
    let outcome = (|| -> Result<(), String> {
        loop {
            collect()?;
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                collect()?;
                if !status.success() {
                    return Err(format!("Encoder probe exited with {status}"));
                }
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err("Encoder probe exceeded 12 seconds".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
    })();
    let _ = child.kill();
    let _ = child.wait();
    if let Some(worker) = stderr {
        let _ = worker.join();
    }
    if let Err(error) = outcome {
        let diagnostic = String::from_utf8_lossy(&diagnostics.lock().unwrap())
            .trim()
            .to_owned();
        return Err(if diagnostic.is_empty() {
            error
        } else {
            format!("{error}: {diagnostic}")
        });
    }
    if packets == 0 {
        return Err("Encoder probe produced no RTP video packets".into());
    }
    Ok(
        json!({"available":true,"encoder":encoder,"codec":codec,"rtp_packets":packets,"rtp_bytes":bytes,"rtcp_reports":reports,"elapsed_ms":started.elapsed().as_millis(),"picture":"128x128@10","hardware_checked":!encoder.starts_with("lib")}),
    )
}

/// Sends a generated NV12 fixture through the real H.264 encoder and RTP path.
pub(crate) fn encode_fixture(functions: Functions, track: &Track) -> Result<Value, String> {
    let mut bytes = vec![16u8; 32 * 32];
    bytes.extend_from_slice(&vec![128u8; 32 * 32 / 2]);
    let frame = Frame {
        timestamp: 0,
        width: 32,
        height: 32,
        format: 2,
        colorspace: 2,
        range: 1,
        bytes,
    };
    let counters = Arc::new(Counters::default());
    let children = Arc::new(Mutex::new(Vec::new()));
    let args = json!({"fps":25,"width":32,"height":32,"encoder":"libx264","bitrate_kbps":100});
    let mut encoder = Encoder::start(
        track,
        Some(&frame),
        &args,
        functions,
        counters.clone(),
        &children,
        &AtomicBool::new(false),
    )?;
    for _ in 0..5 {
        encoder.write(&frame.bytes)?;
        thread::sleep(Duration::from_millis(40));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while counters.rtp_packets.load(Ordering::Relaxed) == 0 {
        if Instant::now() > deadline {
            return Err(format!(
                "FFmpeg NV12/H.264 fixture emitted no RTP: {}",
                counters
                    .error
                    .lock()
                    .unwrap()
                    .as_deref()
                    .unwrap_or("encoder timeout")
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(
        json!({"nv12_h264_encoding":true,"packets":counters.rtp_packets.load(Ordering::Relaxed),"bytes":counters.rtp_bytes.load(Ordering::Relaxed)}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slpk_pcm_and_invalid_versions() {
        let mut header = [0; 36];
        header[..4].copy_from_slice(b"SLPK");
        header[4] = 2;
        header[5] = 2;
        header[8..12].copy_from_slice(&1920u32.to_le_bytes());
        header[28..32].copy_from_slice(&48000u32.to_le_bytes());
        header[32..36].copy_from_slice(&480u32.to_le_bytes());
        assert!(matches!(
            decode_packet(&header, vec![0; 1920]).unwrap(),
            Packet::Audio { frames: 480, .. }
        ));
        header[4] = 1;
        assert!(decode_packet(&header, vec![0; 1920]).is_err());
    }
}
