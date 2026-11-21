//! Native desktop services: one window per profile, OS tray and bounded images.
//!
//! Business operations remain in `App`. This module has no Bilibili transport.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageFormat, ImageReader, RgbaImage, imageops::FilterType};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Cursor, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::JoinHandle,
    time::Duration,
};
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};
use zgpui::native::{Canvas, Rect};

pub enum Instance {
    Primary(Activation),
    Activated,
}

/// The file lock is authoritative; the local socket only raises the existing UI.
pub struct Activation {
    lock: File,
    file: PathBuf,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    events: Receiver<()>,
}

fn private_file(path: &Path, new: bool) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if new {
        options.create_new(true);
    } else {
        options.create(true).truncate(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|e| format!("Desktop state: {e}"))
}

fn random_name() -> Result<String, String> {
    let mut value = [0u8; 32];
    getrandom::fill(&mut value).map_err(|_| "OS random source unavailable")?;
    Ok(value.iter().map(|byte| format!("{byte:02x}")).collect())
}

impl Activation {
    pub fn acquire(directory: &Path) -> Result<Instance, String> {
        fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        let lock = private_file(&directory.join("desktop.lock"), false)?;
        let file = directory.join("desktop-activation.json");
        match lock.try_lock() {
            Ok(()) => (),
            Err(fs::TryLockError::WouldBlock) => {
                let mut last = String::new();
                for _ in 0..12 {
                    match activate_existing(&file) {
                        Ok(()) => return Ok(Instance::Activated),
                        Err(error) => last = error,
                    }
                    std::thread::sleep(Duration::from_millis(75));
                }
                return Err(format!(
                    "An existing window owns this profile but could not be activated: {last}"
                ));
            }
            Err(error) => return Err(format!("Cannot lock the desktop profile: {error}")),
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let token = random_name()?;
        let temporary = directory.join(format!(".desktop-{}.tmp", &token[..16]));
        let mut metadata = private_file(&temporary, true)?;
        metadata
            .write_all(
                json!({"port":port,"token":token,"pid":std::process::id()})
                    .to_string()
                    .as_bytes(),
            )
            .map_err(|e| e.to_string())?;
        metadata.sync_all().map_err(|e| e.to_string())?;
        drop(metadata);
        if let Err(error) = fs::rename(&temporary, &file) {
            let _ = fs::remove_file(&temporary);
            return Err(format!("Cannot prepare window activation: {error}"));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let (sender, events) = mpsc::sync_channel(4);
        let worker = std::thread::Builder::new()
            .name("bilibilive-activation".into())
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, address)) if address.ip().is_loopback() => {
                            let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
                            let _ = stream.set_write_timeout(Some(Duration::from_millis(300)));
                            let mut input = String::new();
                            if BufReader::new(&stream)
                                .take(80)
                                .read_line(&mut input)
                                .is_ok()
                                && input.trim_end() == token
                            {
                                let _ = sender.try_send(());
                                let _ = stream.write_all(b"ok\n");
                            }
                        }
                        Ok(_) => (),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(40))
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Instance::Primary(Self {
            lock,
            file,
            stop,
            worker: Some(worker),
            events,
        }))
    }

    pub fn requested(&self) -> bool {
        self.events.try_iter().count() > 0
    }
}

fn activate_existing(path: &Path) -> Result<(), String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(2049)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 2048 {
        return Err("Invalid desktop activation file".into());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let port = value["port"]
        .as_u64()
        .filter(|port| *port > 0 && *port <= 65535)
        .ok_or("Invalid activation port")? as u16;
    let token = value["token"]
        .as_str()
        .filter(|token| token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("Invalid activation token")?;
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(300))
        .map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_millis(300)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_millis(300)))
        .map_err(|e| e.to_string())?;
    writeln!(stream, "{token}").map_err(|e| e.to_string())?;
    let mut reply = [0u8; 3];
    stream.read_exact(&mut reply).map_err(|e| e.to_string())?;
    if reply == *b"ok\n" {
        Ok(())
    } else {
        Err("Window activation was rejected".into())
    }
}

impl Drop for Activation {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = fs::remove_file(&self.file);
        let _ = self.lock.unlock();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    Show,
    Start,
    Stop,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    account: MenuItem,
    start: MenuItem,
    stop: MenuItem,
}

impl Tray {
    /// Create after the native application has processed its first event.
    pub fn new(tooltip: &str) -> Result<Self, String> {
        let menu = Menu::new();
        let show = MenuItem::with_id("bilibilive.show", "打开 bilibilive", true, None);
        let account = MenuItem::with_id("bilibilive.account", "尚未登录", false, None);
        let start = MenuItem::with_id("bilibilive.start", "开始直播", false, None);
        let stop = MenuItem::with_id("bilibilive.stop", "结束直播", false, None);
        let quit = MenuItem::with_id("bilibilive.quit", "退出 bilibilive", true, None);
        menu.append_items(&[
            &show,
            &account,
            &PredefinedMenuItem::separator(),
            &start,
            &stop,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(|e| e.to_string())?;
        let icon = TrayIconBuilder::new()
            .with_id("bilibilive")
            .with_menu(Box::new(menu))
            .with_icon(default_icon()?)
            .with_menu_on_left_click(false)
            .with_tooltip(tooltip)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            icon,
            account,
            start,
            stop,
        })
    }

    pub fn poll(&self) -> Vec<TrayAction> {
        let mut actions = Vec::new();
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            let action = match event.id.as_ref() {
                "bilibilive.show" => Some(TrayAction::Show),
                "bilibilive.start" => Some(TrayAction::Start),
                "bilibilive.stop" => Some(TrayAction::Stop),
                "bilibilive.quit" => Some(TrayAction::Quit),
                _ => None,
            };
            if let Some(action) = action {
                actions.push(action);
            }
        }
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } | TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                }
            ) {
                actions.push(TrayAction::Show);
            }
        }
        actions
    }

    pub fn update(&self, account: &str, live: bool, busy: bool, tooltip: &str) {
        self.account.set_text(if account.is_empty() {
            "尚未登录"
        } else {
            account
        });
        self.start
            .set_enabled(!account.is_empty() && !live && !busy);
        self.stop.set_enabled(!account.is_empty() && live && !busy);
        let _ = self.icon.set_tooltip(Some(tooltip));
    }

    pub fn set_image(&self, image: Option<&RgbaImage>) -> Result<(), String> {
        let icon = match image {
            Some(image) => {
                let resized = image::imageops::resize(image, 32, 32, FilterType::Triangle);
                Icon::from_rgba(resized.into_raw(), 32, 32).map_err(|e| e.to_string())?
            }
            None => default_icon()?,
        };
        self.icon.set_icon(Some(icon)).map_err(|e| e.to_string())
    }
}

fn default_icon() -> Result<Icon, String> {
    // Original geometric mark, generated from source; no embedded bitmap.
    let mut pixels = vec![0u8; 32 * 32 * 4];
    for y in 0..32usize {
        for x in 0..32usize {
            let circle = (x as i32 - 16).pow(2) + (y as i32 - 16).pow(2) < 225;
            let mark = (9..13).contains(&x) && (7..25).contains(&y)
                || (12..21).contains(&x)
                    && ((7..11).contains(&y) || (14..18).contains(&y) || (21..25).contains(&y))
                || (19..23).contains(&x) && ((9..16).contains(&y) || (17..23).contains(&y));
            let live = (x as i32 - 26).pow(2) + (y as i32 - 6).pow(2) < 16;
            let color = if live {
                [85, 202, 157, 255]
            } else if mark {
                [143, 180, 255, 255]
            } else if circle {
                [18, 20, 26, 255]
            } else {
                [0, 0, 0, 0]
            };
            pixels[(y * 32 + x) * 4..(y * 32 + x + 1) * 4].copy_from_slice(&color);
        }
    }
    Icon::from_rgba(pixels, 32, 32).map_err(|e| e.to_string())
}

pub type Bitmap = Arc<RgbaImage>;

/// A display-sized raster, reused while the source and displayed size are stable.
/// The full image remains available to the crop/export path, while repainting a
/// small preview never uploads all pixels of a multi-megapixel original.
#[derive(Default)]
pub struct ImagePreview {
    original: Weak<RgbaImage>,
    source: CropRect,
    size: (u32, u32),
    raster: Option<Bitmap>,
}

impl ImagePreview {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn raster(&mut self, image: &Bitmap, source: CropRect, destination: Rect) -> Bitmap {
        let size = (
            source.width.min(destination.w.ceil().max(1.) as u32),
            source.height.min(destination.h.ceil().max(1.) as u32),
        );
        let same_image = self
            .original
            .upgrade()
            .is_some_and(|original| Arc::ptr_eq(&original, image));
        if same_image && self.source == source && self.size == size {
            if let Some(raster) = &self.raster {
                return raster.clone();
            }
        }
        let raster = if source == CropRect::full(image) && size == image.dimensions() {
            image.clone()
        } else {
            let selected = image::imageops::crop_imm(
                image.as_ref(),
                source.x,
                source.y,
                source.width,
                source.height,
            );
            Arc::new(image::imageops::resize(
                &*selected,
                size.0,
                size.1,
                FilterType::Triangle,
            ))
        };
        self.original = Arc::downgrade(image);
        self.source = source;
        self.size = size;
        self.raster = Some(raster.clone());
        raster
    }

    pub fn paint(
        &mut self,
        canvas: &mut Canvas<'_>,
        image: &Bitmap,
        source: CropRect,
        destination: Rect,
        opacity: u8,
    ) {
        if opacity == 0 || !source.valid(image) || destination.w <= 0. || destination.h <= 0. {
            return;
        }
        let raster = self.raster(image, source, destination);
        paint_image(
            canvas,
            &raster,
            CropRect::full(&raster),
            destination,
            opacity,
        );
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CropRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl CropRect {
    pub fn full(image: &RgbaImage) -> Self {
        Self {
            x: 0,
            y: 0,
            width: image.width(),
            height: image.height(),
        }
    }
    pub fn centered(image: &RgbaImage) -> Self {
        let units = (image.width() / 16).min(image.height() / 9);
        let (width, height) = (units * 16, units * 9);
        Self {
            x: (image.width() - width) / 2,
            y: (image.height() - height) / 2,
            width,
            height,
        }
    }
    pub fn moved(self, image: &RgbaImage, dx: f32, dy: f32) -> Self {
        Self {
            x: (self.x as f32 + dx)
                .round()
                .clamp(0., image.width().saturating_sub(self.width) as f32) as u32,
            y: (self.y as f32 + dy)
                .round()
                .clamp(0., image.height().saturating_sub(self.height) as f32) as u32,
            ..self
        }
    }
    pub fn scaled(self, image: &RgbaImage, scale: f32) -> Self {
        let max_units = (image.width() / 16).min(image.height() / 9);
        if max_units == 0 {
            return Self::centered(image);
        }
        let units = ((self.width as f32 * scale / 16.).round() as u32).clamp(1, max_units);
        let (width, height) = (units * 16, units * 9);
        let x = (self.x as f32 + self.width as f32 / 2. - width as f32 / 2.)
            .round()
            .clamp(0., (image.width() - width) as f32) as u32;
        let y = (self.y as f32 + self.height as f32 / 2. - height as f32 / 2.)
            .round()
            .clamp(0., (image.height() - height) as f32) as u32;
        Self {
            x,
            y,
            width,
            height,
        }
    }
    pub fn valid(self, image: &RgbaImage) -> bool {
        self.width > 0
            && self.height > 0
            && self
                .x
                .checked_add(self.width)
                .is_some_and(|end| end <= image.width())
            && self
                .y
                .checked_add(self.height)
                .is_some_and(|end| end <= image.height())
    }
}

pub enum AssetRequest {
    Load {
        id: u64,
        kind: &'static str,
        path: PathBuf,
        blur: f32,
    },
    Decode {
        id: u64,
        kind: &'static str,
        bytes: Vec<u8>,
    },
    Base64 {
        id: u64,
        kind: &'static str,
        encoded: String,
    },
    Crop {
        id: u64,
        image: Bitmap,
        crop: CropRect,
        directory: PathBuf,
    },
}
pub enum Asset {
    Image(Bitmap),
    File(PathBuf),
}
pub struct AssetResponse {
    pub id: u64,
    pub kind: &'static str,
    pub result: Result<Asset, String>,
}
pub struct Images {
    sender: SyncSender<AssetRequest>,
    receiver: Receiver<AssetResponse>,
}

impl Images {
    pub fn new() -> Result<Self, String> {
        let (sender, requests) = mpsc::sync_channel::<AssetRequest>(4);
        let (responses, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("bilibilive-images".into())
            .spawn(move || {
                while let Ok(request) = requests.recv() {
                    let (id, kind, result) = match request {
                        AssetRequest::Load {
                            id,
                            kind,
                            path,
                            blur,
                        } => (id, kind, load_image(&path, blur).map(Asset::Image)),
                        AssetRequest::Decode { id, kind, bytes } => {
                            (id, kind, decode_image(&bytes).map(Asset::Image))
                        }
                        AssetRequest::Base64 { id, kind, encoded } => (
                            id,
                            kind,
                            if encoded.len() > 12 * 1024 * 1024 {
                                Err("Cover preview exceeds its encoded limit".into())
                            } else {
                                STANDARD
                                    .decode(encoded)
                                    .map_err(|e| e.to_string())
                                    .and_then(|bytes| decode_image(&bytes))
                                    .map(Asset::Image)
                            },
                        ),
                        AssetRequest::Crop {
                            id,
                            image,
                            crop,
                            directory,
                        } => (
                            id,
                            "crop",
                            save_crop(&image, crop, &directory).map(Asset::File),
                        ),
                    };
                    if let Err(error) = responses.send(AssetResponse { id, kind, result }) {
                        if let Ok(Asset::File(path)) = error.0.result {
                            let _ = fs::remove_file(path);
                        }
                        break;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { sender, receiver })
    }
    pub fn submit(&self, request: AssetRequest) -> Result<(), String> {
        self.sender
            .try_send(request)
            .map_err(|_| "Image worker is busy; try again after the current image finishes".into())
    }
    pub fn next(&self) -> Option<AssetResponse> {
        self.receiver.try_recv().ok()
    }
}

fn load_image(path: &Path, blur: f32) -> Result<Bitmap, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| format!("Cannot open image: {e}"))?
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let bitmap = decode_image(&bytes)?;
    if blur > 0. {
        let scale = (1600. / bitmap.width() as f32)
            .min(1200. / bitmap.height() as f32)
            .min(1.);
        let image = image::imageops::resize(
            &*bitmap,
            (bitmap.width() as f32 * scale).round().max(1.) as u32,
            (bitmap.height() as f32 * scale).round().max(1.) as u32,
            FilterType::Triangle,
        );
        Ok(Arc::new(image::imageops::blur(&image, blur.min(40.))))
    } else {
        Ok(bitmap)
    }
}

fn decode_image(bytes: &[u8]) -> Result<Bitmap, String> {
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("Image file exceeds 32 MiB".into());
    }
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|e| format!("Cannot read PNG/JPEG/WebP image: {e}"))?;
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || u64::from(width) * u64::from(height) * 4 > 64 * 1024 * 1024
    {
        return Err("Image dimensions exceed the 8192-pixel / 64 MiB decoded limit".into());
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    Ok(Arc::new(
        reader
            .decode()
            .map_err(|e| format!("Cannot decode image: {e}"))?
            .into_rgba8(),
    ))
}

fn save_crop(image: &RgbaImage, crop: CropRect, directory: &Path) -> Result<PathBuf, String> {
    if !crop.valid(image) || crop.width as u64 * 9 != crop.height as u64 * 16 {
        return Err("Cover crop must be a valid 16:9 rectangle inside the image".into());
    }
    let selected =
        image::imageops::crop_imm(image, crop.x, crop.y, crop.width, crop.height).to_image();
    let resized = image::imageops::resize(&selected, 704, 396, FilterType::Lanczos3);
    fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let path = directory.join(format!("cover-{}.png", random_name()?));
    let mut file = private_file(&path, true)?;
    let result = DynamicImage::ImageRgba8(resized)
        .write_to(&mut file, ImageFormat::Png)
        .map_err(|e| e.to_string());
    if let Err(error) = result {
        drop(file);
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    file.sync_all().map_err(|e| e.to_string())?;
    Ok(path)
}

pub fn fit_image(image: &RgbaImage, target: Rect, mode: &str) -> (CropRect, Rect) {
    let full = CropRect::full(image);
    if image.width() == 0 || image.height() == 0 || target.w <= 0. || target.h <= 0. {
        return (full, target);
    }
    let sx = target.w / image.width() as f32;
    let sy = target.h / image.height() as f32;
    match mode {
        "stretch" => (full, target),
        "cover" => {
            let scale = sx.max(sy);
            let width = (target.w / scale).round().clamp(1., image.width() as f32) as u32;
            let height = (target.h / scale).round().clamp(1., image.height() as f32) as u32;
            (
                CropRect {
                    x: (image.width() - width) / 2,
                    y: (image.height() - height) / 2,
                    width,
                    height,
                },
                target,
            )
        }
        "native" => {
            let width = image.width().min(target.w as u32).max(1);
            let height = image.height().min(target.h as u32).max(1);
            (
                CropRect {
                    x: (image.width() - width) / 2,
                    y: (image.height() - height) / 2,
                    width,
                    height,
                },
                Rect::new(
                    target.x + (target.w - width as f32) / 2.,
                    target.y + (target.h - height as f32) / 2.,
                    width as f32,
                    height as f32,
                ),
            )
        }
        _ => {
            let scale = sx.min(sy);
            let (width, height) = (image.width() as f32 * scale, image.height() as f32 * scale);
            (
                full,
                Rect::new(
                    target.x + (target.w - width) / 2.,
                    target.y + (target.h - height) / 2.,
                    width,
                    height,
                ),
            )
        }
    }
}

/// Blit while zgpui's frame/context is active, before its native presenter runs.
/// The supplied geometry is contained in the window; no GPU object survives a frame.
pub fn paint_image(
    canvas: &mut Canvas<'_>,
    image: &RgbaImage,
    source: CropRect,
    destination: Rect,
    opacity: u8,
) {
    if opacity == 0 || !source.valid(image) || destination.w <= 0. || destination.h <= 0. {
        return;
    }
    #[cfg(all(target_os = "linux", feature = "linux-software"))]
    software_image(canvas, image, source, destination, opacity);
    #[cfg(not(all(target_os = "linux", feature = "linux-software")))]
    {
        let _ = canvas;
        // zgpui uses an OpenGL compatibility context on all three native targets.
        // Preserve both server and unpack state used by its text/shape renderer.
        unsafe {
            gl::glPushAttrib(0x0000_2000 | 0x0000_0020 | 0x0000_4000 | 0x0000_0001 | 0x0004_0000);
            gl::glPushClientAttrib(0x0000_0001);
            gl::glDisable(0x0DE1);
            gl::glEnable(0x0BE2);
            gl::glBlendFunc(0x0302, 0x0303);
            gl::glPixelStorei(0x0CF5, 4);
            gl::glPixelStorei(0x0CF2, image.width() as i32);
            gl::glPixelStorei(0x0CF3, source.y as i32);
            gl::glPixelStorei(0x0CF4, source.x as i32);
            gl::glPixelTransferf(0x0D1C, opacity as f32 / 255.);
            gl::glPixelZoom(
                destination.w / source.width as f32,
                -destination.h / source.height as f32,
            );
            gl::glRasterPos2f(destination.x, destination.y);
            gl::glDrawPixels(
                source.width as i32,
                source.height as i32,
                0x1908,
                0x1401,
                image.as_raw().as_ptr().cast(),
            );
            gl::glPopClientAttrib();
            gl::glPopAttrib();
        }
    }
}

#[cfg(not(all(target_os = "linux", feature = "linux-software")))]
mod gl {
    use std::ffi::c_void;
    #[cfg_attr(target_os = "linux", link(name = "GL"))]
    #[cfg_attr(target_os = "windows", link(name = "opengl32"))]
    #[cfg_attr(target_os = "macos", link(name = "OpenGL", kind = "framework"))]
    unsafe extern "system" {
        pub fn glPushAttrib(mask: u32);
        pub fn glPopAttrib();
        pub fn glPushClientAttrib(mask: u32);
        pub fn glPopClientAttrib();
        pub fn glEnable(cap: u32);
        pub fn glDisable(cap: u32);
        pub fn glBlendFunc(source: u32, destination: u32);
        pub fn glPixelStorei(parameter: u32, value: i32);
        pub fn glPixelTransferf(parameter: u32, value: f32);
        pub fn glPixelZoom(x: f32, y: f32);
        pub fn glRasterPos2f(x: f32, y: f32);
        pub fn glDrawPixels(width: i32, height: i32, format: u32, kind: u32, pixels: *const c_void);
    }
}

#[cfg(all(target_os = "linux", feature = "linux-software"))]
fn software_image(
    canvas: &mut Canvas<'_>,
    image: &RgbaImage,
    source: CropRect,
    destination: Rect,
    opacity: u8,
) {
    unsafe extern "C" {
        fn zgpui_software_pixels(width: *mut i32, height: *mut i32) -> *mut u32;
    }
    let (mut width, mut height) = (0, 0);
    // The native C API owns a writable framebuffer until present. Canvas pins
    // this operation to the window thread and prevents concurrent presentation.
    let pixels = unsafe { zgpui_software_pixels(&mut width, &mut height) };
    if pixels.is_null()
        || width <= 0
        || height <= 0
        || width as f32 != canvas.width
        || height as f32 != canvas.height
    {
        return;
    }
    let pixels =
        unsafe { std::slice::from_raw_parts_mut(pixels, width as usize * height as usize) };
    let x0 = destination.x.floor().max(0.) as i32;
    let y0 = destination.y.floor().max(0.) as i32;
    let x1 = (destination.x + destination.w).ceil().min(width as f32) as i32;
    let y1 = (destination.y + destination.h).ceil().min(height as f32) as i32;
    for y in y0..y1 {
        let sy = source.y
            + (((y as f32 + 0.5 - destination.y) / destination.h * source.height as f32) as u32)
                .min(source.height - 1);
        for x in x0..x1 {
            let sx = source.x
                + (((x as f32 + 0.5 - destination.x) / destination.w * source.width as f32) as u32)
                    .min(source.width - 1);
            let [r, g, b, a] = image.get_pixel(sx, sy).0;
            let alpha = (a as u32 * opacity as u32 + 127) / 255;
            let dst = &mut pixels[(y * width + x) as usize];
            let inv = 255 - alpha;
            let r = (r as u32 * alpha + ((*dst >> 16) & 255) * inv + 127) / 255;
            let g = (g as u32 * alpha + ((*dst >> 8) & 255) * inv + 127) / 255;
            let b = (b as u32 * alpha + (*dst & 255) * inv + 127) / 255;
            *dst = (r << 16) | (g << 8) | b;
        }
    }
}

/// Pump native tray messages when there is no zgpui window to pump them.
pub fn pump_tray_events() {
    #[cfg(target_os = "windows")]
    unsafe {
        windows_pump();
    }
    #[cfg(target_os = "macos")]
    unsafe {
        macos_pump();
    }
}

#[cfg(target_os = "windows")]
unsafe fn windows_pump() {
    use std::ffi::c_void;
    #[repr(C)]
    struct Message {
        hwnd: *mut c_void,
        message: u32,
        wparam: usize,
        lparam: isize,
        time: u32,
        x: i32,
        y: i32,
        private: u32,
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn PeekMessageW(
            message: *mut Message,
            window: *mut c_void,
            minimum: u32,
            maximum: u32,
            remove: u32,
        ) -> i32;
        fn TranslateMessage(message: *const Message) -> i32;
        fn DispatchMessageW(message: *const Message) -> isize;
    }
    let mut message: Message = unsafe { std::mem::zeroed() };
    for _ in 0..64 {
        if unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, 1) } == 0 {
            break;
        }
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

#[cfg(target_os = "macos")]
unsafe fn macos_pump() {
    use std::ffi::{c_char, c_void};
    type Object = *mut c_void;
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Object;
        fn sel_registerName(name: *const c_char) -> Object;
        fn objc_msgSend();
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: Object;
    }
    unsafe fn send0(receiver: Object, name: &'static [u8]) -> Object {
        let send: unsafe extern "C" fn(Object, Object) -> Object =
            unsafe { std::mem::transmute(objc_msgSend as *const ()) };
        unsafe { send(receiver, sel_registerName(name.as_ptr().cast())) }
    }
    let pool = unsafe { send0(objc_getClass(c"NSAutoreleasePool".as_ptr()), b"new\0") };
    let app = unsafe {
        send0(
            objc_getClass(c"NSApplication".as_ptr()),
            b"sharedApplication\0",
        )
    };
    let date = unsafe { send0(objc_getClass(c"NSDate".as_ptr()), b"distantPast\0") };
    let next: unsafe extern "C" fn(Object, Object, usize, Object, Object, bool) -> Object =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    let deliver: unsafe extern "C" fn(Object, Object, Object) =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    for _ in 0..64 {
        let event = unsafe {
            next(
                app,
                sel_registerName(c"nextEventMatchingMask:untilDate:inMode:dequeue:".as_ptr()),
                usize::MAX,
                date,
                kCFRunLoopDefaultMode,
                true,
            )
        };
        if event.is_null() {
            break;
        }
        unsafe {
            deliver(app, sel_registerName(c"sendEvent:".as_ptr()), event);
        }
    }
    unsafe {
        send0(app, b"updateWindows\0");
        send0(pool, b"drain\0");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_preview_reuses_a_display_sized_raster_during_dragging() {
        let original = Arc::new(RgbaImage::from_pixel(
            1920,
            1080,
            image::Rgba([240, 120, 60, 255]),
        ));
        let mut preview = ImagePreview::default();
        let source = CropRect::full(&original);
        let first = preview.raster(&original, source, Rect::new(0., 0., 640., 360.));
        assert_eq!(first.dimensions(), (640, 360));
        assert_eq!(first.as_raw().len() * 9, original.as_raw().len());
        for position in 0..128 {
            let frame = preview.raster(
                &original,
                source,
                Rect::new(position as f32, 12., 640., 360.),
            );
            assert!(Arc::ptr_eq(&first, &frame));
        }
        assert_eq!(original.dimensions(), (1920, 1080));
        assert_eq!(Arc::strong_count(&original), 1);
        let resized = preview.raster(&original, source, Rect::new(0., 0., 320., 180.));
        assert_eq!(resized.dimensions(), (320, 180));
        assert!(!Arc::ptr_eq(&first, &resized));
        preview.clear();
        assert!(preview.raster.is_none());
        assert!(preview.original.upgrade().is_none());
    }

    #[test]
    fn image_preview_invalidates_for_a_new_source_and_uses_the_requested_crop() {
        let mut original = RgbaImage::from_pixel(64, 36, image::Rgba([255, 0, 0, 255]));
        for y in 18..36 {
            for x in 32..64 {
                original.put_pixel(x, y, image::Rgba([0, 0, 255, 255]));
            }
        }
        let original = Arc::new(original);
        let mut preview = ImagePreview::default();
        let source = CropRect {
            x: 32,
            y: 18,
            width: 32,
            height: 18,
        };
        let destination = Rect::new(0., 0., 16., 9.);
        let first = preview.raster(&original, source, destination);
        assert!(first.pixels().all(|pixel| pixel.0 == [0, 0, 255, 255]));
        let replacement = Arc::new(RgbaImage::from_pixel(64, 36, image::Rgba([0, 255, 0, 255])));
        let next = preview.raster(&replacement, source, destination);
        assert!(!Arc::ptr_eq(&first, &next));
        assert!(next.pixels().all(|pixel| pixel.0 == [0, 255, 0, 255]));
    }

    #[test]
    fn crop_uses_the_selected_pixels_and_exact_export_size() {
        let directory =
            std::env::temp_dir().join(format!("bilibilive-crop-{}", random_name().unwrap()));
        let mut image = RgbaImage::from_pixel(64, 36, image::Rgba([255, 0, 0, 255]));
        for y in 18..36 {
            for x in 32..64 {
                image.put_pixel(x, y, image::Rgba([0, 0, 255, 255]));
            }
        }
        let crop = CropRect {
            x: 32,
            y: 18,
            width: 32,
            height: 18,
        };
        let path = save_crop(&image, crop, &directory).unwrap();
        let exported = image::open(&path).unwrap().into_rgba8();
        assert_eq!(exported.dimensions(), (704, 396));
        assert!(exported.pixels().all(|pixel| pixel.0 == [0, 0, 255, 255]));
        assert!(save_crop(&image, CropRect { x: 63, ..crop }, &directory).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn second_window_activates_the_first_profile_owner() {
        let directory =
            std::env::temp_dir().join(format!("bilibilive-activation-{}", random_name().unwrap()));
        let Instance::Primary(primary) = Activation::acquire(&directory).unwrap() else {
            panic!("missing primary");
        };
        assert!(matches!(
            Activation::acquire(&directory).unwrap(),
            Instance::Activated
        ));
        assert!(primary.events.recv_timeout(Duration::from_secs(2)).is_ok());
        drop(primary);
        assert!(!directory.join("desktop-activation.json").exists());
        fs::remove_dir_all(directory).unwrap();
    }
}
