//! The native desktop surface. Every operation uses the same dispatcher as CLI/MCP.
use crate::app::App;
use crate::desktop::{
    self, Asset, AssetRequest, Bitmap, CropRect, Images, Instance, Tray, TrayAction,
};
use qrcodegen::{QrCode, QrCodeEcc};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
};
use std::time::{Duration, Instant};
use zgpui::native::{ButtonStyle, Canvas, Event, Palette, Rect, Theme, Ui, Window, colors};

const BG: u32 = colors::BG;
const PANEL: u32 = colors::SURFACE;
const INK: u32 = colors::TEXT;
const MUTED: u32 = colors::MUTED;
const ACCENT: u32 = colors::BLUE;
const ACCENT_SOFT: u32 = colors::BLUE_PALE;
const HAIRLINE: u32 = colors::LINE;
const SUCCESS: u32 = colors::GREEN;
const RED: u32 = colors::RED;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Overview,
    Room,
    Accounts,
    Obs,
    Pk,
    Automation,
    Settings,
    Advanced,
    Cover,
    Titles,
    Appearance,
    Components,
}

impl Page {
    fn label(self) -> &'static str {
        match self {
            Self::Overview => "直播概览",
            Self::Room => "直播间设置",
            Self::Accounts => "账号",
            Self::Obs => "OBS 联动",
            Self::Pk => "PK 与连线",
            Self::Automation => "自动化",
            Self::Settings => "偏好设置",
            Self::Advanced => "完整操作",
            Self::Cover => "封面裁剪",
            Self::Titles => "最近使用的标题",
            Self::Appearance => "外观与桌面",
            Self::Components => "更新与组件",
        }
    }
    fn eyebrow(self) -> &'static str {
        match self {
            Self::Overview => "YOUR LIVE, IN FOCUS",
            Self::Room => "ROOM DETAILS",
            Self::Accounts => "YOUR ACCOUNTS",
            Self::Obs => "STREAM TOGETHER",
            Self::Pk => "CONNECT & COLLABORATE",
            Self::Automation => "MAKE IT FLOW",
            Self::Settings => "MAKE IT YOURS",
            Self::Advanced => "YOUR TOOLBOX",
            Self::Cover => "FRAME YOUR NEXT LIVE",
            Self::Titles => "PICK UP WHERE YOU LEFT OFF",
            Self::Appearance => "A SPACE OF YOUR OWN",
            Self::Components => "READY FOR WHAT IS NEXT",
        }
    }
}

struct Request {
    method: String,
    args: Value,
    cleanup: Option<PathBuf>,
}
struct Response {
    method: String,
    result: Result<Value, String>,
}

struct Desktop {
    page: Page,
    tx: Sender<Request>,
    rx: Receiver<Response>,
    pending: usize,
    demo: bool,
    read_only: bool,
    config: Value,
    room: Value,
    accounts: Value,
    obs: Value,
    pk: Value,
    stream: Value,
    notice: Option<(String, bool, Instant)>,
    activity: Vec<(String, bool)>,
    title: String,
    announcement: String,
    area_id: String,
    cover_path: String,
    area_search: String,
    areas: Vec<(u64, String)>,
    area_offset: usize,
    account_name: String,
    cookie: String,
    account_offset: usize,
    qr_url: String,
    qr_key: String,
    qr: Option<QrCode>,
    qr_created: Option<Instant>,
    qr_poll: Instant,
    qr_message: String,
    obs_url: String,
    obs_password: String,
    obs_auto_start: bool,
    obs_auto_stop: bool,
    obs_auto_connect: bool,
    obs_link: bool,
    obs_watch_requested: Option<bool>,
    obs_watch_poll: Instant,
    protocol: String,
    strategy: String,
    delay: String,
    show_key: bool,
    opponent_uid: String,
    session_id: String,
    invitation_source: String,
    pk_mode: String,
    danmaku: String,
    script: String,
    script_output: String,
    copy: Option<String>,
    room_loaded: bool,
    config_loaded: bool,
    last_refresh: Instant,
    verification: Value,
    verification_qr: Option<QrCode>,
    verification_poll: Instant,
    verification_created: Option<Instant>,
    verification_message: String,
    events_connected: bool,
    events_poll: Instant,
    advanced_name: String,
    advanced_args: String,
    advanced_search: String,
    advanced_offset: usize,
    advanced_output: String,
    advanced_pending: String,
    advanced_show_args: bool,
    directory: PathBuf,
    images: Option<Images>,
    image_serial: u64,
    cover_image_id: u64,
    background_image_id: u64,
    tray_image_id: u64,
    crop_image_id: u64,
    cover: Option<Bitmap>,
    background: Option<Bitmap>,
    tray_image: Option<Bitmap>,
    cover_identity: String,
    background_identity: String,
    tray_identity: String,
    cover_local: bool,
    cover_loading: bool,
    crop_busy: bool,
    crop: CropRect,
    crop_display: Rect,
    crop_drag: Option<(f32, f32, CropRect, bool)>,
    recent_areas: Vec<(u64, String)>,
    titles: Vec<String>,
    area_recent_only: bool,
    audit: String,
    audit_poll: Instant,
    audit_until: Option<Instant>,
    room_dirty: bool,
    font_family: String,
    background_path: String,
    background_mode: String,
    background_opacity: String,
    background_blur: String,
    tray_path: String,
    tray_tooltip: String,
    tray_enabled: bool,
    close_to_tray: bool,
    single_instance: bool,
    appearance_section: u8,
    appearance_loaded: bool,
    account_generation: u64,
    account_context_seen: bool,
    no_update: bool,
    startup_update_handled: bool,
    updates: Value,
    update_pending: String,
    update_error: String,
    update_poll: Instant,
    update_apply_requested: bool,
    quit_requested: bool,
    local_sessions_retired: bool,
    local_shutdown_attempted: bool,
    plugin: Value,
    plugin_pending: bool,
    plugin_error: String,
    plugin_replace: bool,
    plugin_restart_required: bool,
}

/// Open the native application. Demo mode never invokes the dispatcher.
pub fn run(app: App, demo: bool) -> Result<(), String> {
    run_with_options(app, demo, false)
}

/// Open the studio, optionally suppressing this launch's automatic version check.
/// Downloads and installation always require an explicit operation.
pub fn run_with_options(app: App, demo: bool, no_update: bool) -> Result<(), String> {
    let directory = app.directory().to_owned();
    let read_only = app.is_read_only();
    let initial = if demo {
        demo_response("config.get")?
    } else {
        app.snapshot()
    };
    let activation = if !demo
        && initial
            .get("single_instance")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    {
        match desktop::Activation::acquire(&directory)? {
            Instance::Primary(owner) => Some(owner),
            Instance::Activated => return Ok(()),
        }
    } else {
        None
    };
    let (tx, requests) = mpsc::channel::<Request>();
    let (responses, rx) = mpsc::channel::<Response>();
    let lifecycle = app.clone();
    let stopping = Arc::new(AtomicBool::new(false));
    let worker_stopping = stopping.clone();
    let worker = std::thread::Builder::new()
        .name("bilibilive-ui-worker".into())
        .spawn(move || {
            while let Ok(request) = requests.recv() {
                if worker_stopping.load(Ordering::Acquire) {
                    if let Some(path) = request.cleanup {
                        let _ = std::fs::remove_file(path);
                    }
                    break;
                }
                let result = if demo {
                    demo_response(&request.method)
                } else {
                    app.call(&request.method, request.args)
                };
                if let Some(path) = request.cleanup {
                    let _ = std::fs::remove_file(path);
                }
                if responses
                    .send(Response {
                        method: request.method,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
            for request in requests.try_iter() {
                if let Some(path) = request.cleanup {
                    let _ = std::fs::remove_file(path);
                }
            }
        })
        .map_err(|e| e.to_string())?;

    let mut desktop = Desktop::new(tx, rx, demo, read_only);
    desktop.no_update = no_update;
    desktop.directory = directory;
    desktop.config = initial;
    desktop.images = Some(Images::new()?);
    desktop.load_config();
    desktop.submit("config.get", json!({}));
    desktop.submit("account.list", json!({}));
    if demo {
        desktop.submit("room.get", json!({}));
        desktop.submit("obs.status", json!({}));
    }
    let mut window = Some(new_window(&mut desktop)?);
    let mut current_theme = if desktop.config["theme"] == "light" {
        Theme::Light
    } else {
        Theme::Dark
    };
    let mut current_font = text(&desktop.config, &["font_family"]);
    let mut ui = Ui::default();
    let frame_limit = std::env::var("BILIBILIVE_GUI_FRAMES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0);
    let mut frames = 0;
    let mut drawn = false;
    let mut visible_notice = false;
    let mut tray: Option<Tray> = None;
    let mut tray_attempted = false;
    let mut tray_state = String::new();
    loop {
        let mut restore = activation.as_ref().is_some_and(|owner| owner.requested());
        let mut event = match window.as_mut() {
            Some(window) => window.event(80),
            None => {
                desktop::pump_tray_events();
                std::thread::sleep(Duration::from_millis(80));
                None
            }
        };
        if matches!(event, Some(Event::Close)) {
            if tray.is_some()
                && desktop
                    .config
                    .get("close_to_tray")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                window = None;
                event = None;
                drawn = false;
                desktop.show_key = false;
            } else {
                break;
            }
        }
        if let Some(Event::Key { code: 27, .. }) = &event {
            desktop.show_key = false;
        }
        if let Some(Event::Scroll(delta)) = &event {
            let step = if *delta > 0 { -1isize } else { 1 };
            if desktop.page == Page::Room {
                desktop.area_offset = desktop.area_offset.saturating_add_signed(step);
            } else if desktop.page == Page::Accounts {
                desktop.account_offset = desktop.account_offset.saturating_add_signed(step);
            } else if desktop.page == Page::Advanced {
                desktop.advanced_offset = desktop.advanced_offset.saturating_add_signed(step);
            }
        }
        let response_changed = desktop.receive();
        if desktop.quit_requested || lifecycle.update_exit_requested() {
            desktop.quit_requested = true;
            break;
        }
        let images_changed = desktop.receive_images();
        let interaction_changed = desktop.handle_event(event.as_ref());
        let wants_tray = !demo
            && desktop
                .config
                .get("tray_enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
        if !wants_tray {
            tray = None;
            tray_attempted = false;
            if window.is_none() {
                restore = true;
            }
        } else if tray.is_none() && !tray_attempted {
            tray_attempted = true;
            let tooltip = text(&desktop.config, &["tray_tooltip"]);
            match Tray::new(if tooltip.is_empty() {
                "bilibilive"
            } else {
                &tooltip
            }) {
                Ok(created) => {
                    if let Err(error) = created.set_image(desktop.tray_image.as_deref()) {
                        desktop.notice(&error, false);
                    }
                    tray = Some(created);
                    tray_state.clear();
                }
                Err(error) => desktop.notice(&format!("托盘暂不可用：{error}"), false),
            }
        }
        let mut quit = false;
        if let Some(tray) = &tray {
            let name = if desktop.account_rows().is_empty() && !desktop.room_loaded {
                String::new()
            } else {
                desktop.active_name()
            };
            let tooltip = text(&desktop.config, &["tray_tooltip"]);
            let tooltip = if tooltip.is_empty() {
                "bilibilive"
            } else {
                &tooltip
            };
            let state = format!(
                "{name}|{}|{}|{tooltip}",
                desktop.is_live(),
                desktop.pending > 0
            );
            if state != tray_state {
                tray.update(&name, desktop.is_live(), desktop.pending > 0, tooltip);
                tray_state = state;
            }
            if images_changed {
                if let Err(error) = tray.set_image(desktop.tray_image.as_deref()) {
                    desktop.notice(&error, false);
                }
            }
            for action in tray.poll() {
                match action {
                    TrayAction::Show => restore = true,
                    TrayAction::Start if desktop.pending == 0 => {
                        desktop.start_live();
                        restore = true;
                    }
                    TrayAction::Stop if desktop.pending == 0 => {
                        desktop.submit("live.stop", json!({}));
                        restore = true;
                    }
                    TrayAction::Quit => quit = true,
                    _ => (),
                }
            }
        }
        if quit {
            break;
        }
        if !desktop.verification.is_null() && window.is_none() {
            restore = true;
        }
        if restore {
            drop(window.take());
            window = Some(new_window(&mut desktop)?);
            current_theme = if desktop.config["theme"] == "light" {
                Theme::Light
            } else {
                Theme::Dark
            };
            current_font = text(&desktop.config, &["font_family"]);
            ui = Ui::default();
            drawn = false;
            event = None;
        }
        let requested_theme =
            if desktop.config.get("theme").and_then(Value::as_str) == Some("light") {
                Theme::Light
            } else {
                Theme::Dark
            };
        let theme_changed = current_theme != requested_theme;
        if theme_changed {
            current_theme = requested_theme;
            if let Some(window) = &mut window {
                window.set_theme(current_theme);
                window.set_palette(palette(current_theme));
            }
        }
        let requested_font = text(&desktop.config, &["font_family"]);
        let font_changed = current_font != requested_font;
        if font_changed {
            if let Some(window) = &mut window {
                if let Err(error) = window.set_font_family(&requested_font) {
                    desktop.notice(&error, false);
                }
            }
            current_font = requested_font;
        }
        let pending_before = desktop.pending;
        let time_changed = desktop.tick();
        let notice_now = desktop
            .notice
            .as_ref()
            .is_some_and(|(_, _, time)| time.elapsed() < Duration::from_secs(9));
        let pointer_changed = match &event {
            Some(Event::Move(x, y)) => ui.pointer_changed(*x, *y),
            _ => false,
        };
        let event_changed = event
            .as_ref()
            .is_some_and(|event| !matches!(event, Event::Move(_, _)))
            || pointer_changed;
        if window.is_none() {
            continue;
        }
        if drawn
            && !event_changed
            && !response_changed
            && !images_changed
            && !interaction_changed
            && !time_changed
            && !theme_changed
            && !font_changed
            && pending_before == desktop.pending
            && notice_now == visible_notice
            && frame_limit.is_none()
        {
            continue;
        }
        visible_notice = notice_now;
        ui.begin(event);
        let page_before_draw = desktop.page;
        let window = window.as_mut().expect("visible window checked above");
        {
            let mut canvas = window.begin(BG);
            desktop.draw(&mut canvas, &mut ui);
        }
        ui.end();
        if let Some(text) = ui.clipboard.take().or_else(|| desktop.copy.take()) {
            window.copy(&text);
        }
        window.present();
        // Page buttons can navigate after the current page has already drawn.
        // Present the destination without waiting for another input event.
        drawn = desktop.page == page_before_draw;
        frames += 1;
        if frame_limit.is_some_and(|limit| frames >= limit) {
            break;
        }
    }
    // Applying an update must release the dispatcher before the restart helper
    // replaces this process. Remote apply also discards unstarted UI requests.
    let applied_update = desktop.quit_requested;
    if applied_update {
        stopping.store(true, Ordering::Release);
    }
    drop(desktop);
    if applied_update {
        // App preparation already retired media before starting the helper. A
        // read-only request in flight must not outlast the helper's exit limit.
        let deadline = Instant::now() + Duration::from_secs(2);
        while !worker.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if worker.is_finished() {
            worker
                .join()
                .map_err(|_| "The background dispatcher failed during update shutdown")?;
        }
    }
    Ok(())
}

fn new_window(desktop: &mut Desktop) -> Result<Window, String> {
    let mut window = Window::new("bilibilive — 直播，由你掌控", 1160, 800)?;
    let theme = if desktop.config["theme"] == "light" {
        Theme::Light
    } else {
        Theme::Dark
    };
    window.set_theme(theme);
    window.set_palette(palette(theme));
    if let Err(error) = window.set_font_family(&text(&desktop.config, &["font_family"])) {
        desktop.notice(&error, false);
    }
    Ok(window)
}

impl Desktop {
    fn new(tx: Sender<Request>, rx: Receiver<Response>, demo: bool, read_only: bool) -> Self {
        Self {
            page: Page::Overview,
            tx,
            rx,
            pending: 0,
            demo,
            read_only,
            config: json!({}),
            room: json!({}),
            accounts: json!({}),
            obs: json!({}),
            pk: json!({}),
            stream: json!({}),
            notice: None,
            activity: Vec::new(),
            title: String::new(),
            announcement: String::new(),
            area_id: String::new(),
            cover_path: String::new(),
            area_search: String::new(),
            areas: Vec::new(),
            area_offset: 0,
            account_name: "default".into(),
            cookie: String::new(),
            account_offset: 0,
            qr_url: String::new(),
            qr_key: String::new(),
            qr: None,
            qr_created: None,
            qr_poll: Instant::now(),
            qr_message: "使用哔哩哔哩客户端扫码登录".into(),
            obs_url: "ws://127.0.0.1:4455".into(),
            obs_password: String::new(),
            obs_auto_start: false,
            obs_auto_stop: false,
            obs_auto_connect: false,
            obs_link: false,
            obs_watch_requested: Some(false),
            obs_watch_poll: Instant::now(),
            protocol: "rtmp".into(),
            strategy: "fallback".into(),
            delay: "0".into(),
            show_key: false,
            opponent_uid: String::new(),
            session_id: String::new(),
            invitation_source: String::new(),
            pk_mode: "classic".into(),
            danmaku: String::new(),
            script: "bilibilive.status()".into(),
            script_output: String::new(),
            copy: None,
            room_loaded: false,
            config_loaded: false,
            last_refresh: Instant::now(),
            verification: Value::Null,
            verification_qr: None,
            verification_poll: Instant::now(),
            verification_created: None,
            verification_message: String::new(),
            events_connected: false,
            events_poll: Instant::now(),
            advanced_name: "doctor".into(),
            advanced_args: "{}".into(),
            advanced_search: String::new(),
            advanced_offset: 0,
            advanced_output: String::new(),
            advanced_pending: String::new(),
            advanced_show_args: false,
            directory: PathBuf::new(),
            images: None,
            image_serial: 0,
            cover_image_id: 0,
            background_image_id: 0,
            tray_image_id: 0,
            crop_image_id: 0,
            cover: None,
            background: None,
            tray_image: None,
            cover_identity: String::new(),
            background_identity: String::new(),
            tray_identity: String::new(),
            cover_local: false,
            cover_loading: false,
            crop_busy: false,
            crop: CropRect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
            crop_display: Rect::default(),
            crop_drag: None,
            recent_areas: Vec::new(),
            titles: Vec::new(),
            area_recent_only: false,
            audit: String::new(),
            audit_poll: Instant::now(),
            audit_until: None,
            room_dirty: false,
            font_family: String::new(),
            background_path: String::new(),
            background_mode: "cover".into(),
            background_opacity: "15".into(),
            background_blur: "0".into(),
            tray_path: String::new(),
            tray_tooltip: "bilibilive".into(),
            tray_enabled: true,
            close_to_tray: false,
            single_instance: true,
            appearance_section: 0,
            appearance_loaded: false,
            account_generation: 0,
            account_context_seen: false,
            no_update: false,
            startup_update_handled: false,
            updates: json!({}),
            update_pending: String::new(),
            update_error: String::new(),
            update_poll: Instant::now(),
            update_apply_requested: false,
            quit_requested: false,
            local_sessions_retired: false,
            local_shutdown_attempted: false,
            plugin: json!({}),
            plugin_pending: false,
            plugin_error: String::new(),
            plugin_replace: false,
            plugin_restart_required: false,
        }
    }

    fn submit(&mut self, method: &str, args: Value) {
        self.submit_cleanup(method, args, None);
    }

    fn submit_cleanup(&mut self, method: &str, args: Value, cleanup: Option<PathBuf>) -> bool {
        let restricted = (self.demo || self.read_only)
            && matches!(
                method,
                "updates.download" | "updates.apply" | "obs.plugin.install"
            );
        if restricted || self.update_apply_requested || self.quit_requested {
            if let Some(path) = cleanup {
                let _ = std::fs::remove_file(path);
            }
            self.notice(
                if restricted {
                    "演示或只读模式不会下载、安装或应用更新。"
                } else {
                    "正在应用更新，请等待应用正常退出。"
                },
                false,
            );
            return false;
        }
        if self.pending >= 12
            || (method.starts_with("updates.") && !self.update_pending.is_empty())
            || (method.starts_with("obs.plugin.") && self.plugin_pending)
            || (method == "updates.apply" && self.pending != 0)
        {
            if let Some(path) = cleanup {
                let _ = std::fs::remove_file(path);
            }
            self.notice("请等待当前操作完成。", false);
            return false;
        }
        match self.tx.send(Request {
            method: method.into(),
            args,
            cleanup,
        }) {
            Ok(()) => {
                self.pending += 1;
                if method.starts_with("updates.") {
                    self.update_pending = method.into();
                    self.update_error.clear();
                    if method == "updates.apply" {
                        self.update_apply_requested = !self.demo && !self.read_only;
                    }
                } else if method.starts_with("obs.plugin.") {
                    self.plugin_pending = true;
                    self.plugin_error.clear();
                }
                true
            }
            Err(error) => {
                if let Some(path) = error.0.cleanup {
                    let _ = std::fs::remove_file(path);
                }
                self.notice("后台任务已结束，请重新打开应用。", false);
                false
            }
        }
    }

    fn notice(&mut self, text: &str, success: bool) {
        let text: String = text.chars().filter(|c| !c.is_control()).take(180).collect();
        self.notice = Some((text, success, Instant::now()));
    }

    fn receive(&mut self) -> bool {
        let mut changed = false;
        while let Ok(response) = self.rx.try_recv() {
            changed = true;
            self.pending = self.pending.saturating_sub(1);
            let method = response.method.as_str();
            let component_operation =
                method.starts_with("updates.") || method.starts_with("obs.plugin.");
            if method.starts_with("updates.") {
                self.update_pending.clear();
            } else if method.starts_with("obs.plugin.") {
                self.plugin_pending = false;
            }
            if let Ok(value) = &response.result
                && !component_operation
            {
                if let Some(generation) = value
                    .pointer("/_context/generation")
                    .and_then(Value::as_u64)
                {
                    if self.account_context_seen && generation < self.account_generation {
                        continue;
                    }
                    if !self.account_context_seen || generation > self.account_generation {
                        if self.account_context_seen {
                            self.clear_account_view();
                            if method != "account.list" {
                                self.submit("account.list", json!({}));
                            }
                            if method != "room.get" {
                                self.submit("room.get", json!({}));
                            }
                        }
                        self.account_generation = generation;
                        self.account_context_seen = true;
                    }
                }
            }
            if self.advanced_pending == method {
                self.advanced_pending.clear();
                self.advanced_output = match &response.result {
                    Ok(value) => {
                        let mut displayed = value.clone();
                        crate::app::redact(&mut displayed);
                        if let Some(encoded) = displayed.get("base64").and_then(Value::as_str) {
                            displayed["base64"] = json!(format!(
                                "[图像数据，{} 个编码字符；直播间页可预览]",
                                encoded.len()
                            ));
                        }
                        let mut output =
                            serde_json::to_string_pretty(&displayed).unwrap_or_default();
                        if output.len() > 65536 {
                            let mut end = 65536;
                            while !output.is_char_boundary(end) {
                                end -= 1;
                            }
                            output.truncate(end);
                            output.push_str("\n…结果较长。可用 CLI --json 查看完整内容。");
                        }
                        output
                    }
                    Err(error) => format!("操作失败：{error}"),
                };
            }
            match response.result {
                Err(error) => {
                    if method.starts_with("updates.") {
                        self.update_error = error.clone();
                        self.update_apply_requested = false;
                    } else if method.starts_with("obs.plugin.") {
                        self.plugin_error = error.clone();
                    }
                    if method == "auth.qr.poll" {
                        self.qr_message = error.clone();
                    }
                    if method == "danmaku.read" {
                        self.events_connected = false;
                    }
                    if method == "room.cover.preview" {
                        self.cover_loading = false;
                    }
                    if method == "room.audit" {
                        self.audit_until = None;
                        self.audit = format!("审核状态暂不可用：{error}");
                    }
                    if method == "room.cover" {
                        self.crop_busy = false;
                    }
                    self.notice(&error, false);
                    self.record(method, false);
                }
                Ok(value) => {
                    let data = payload(&value).clone();
                    match method {
                        "updates.check" | "updates.status" | "updates.download"
                        | "updates.apply" => {
                            let was_downloaded = self.updates["phase"] == "downloaded";
                            self.updates = data;
                            self.update_error = text(&self.updates, &["error"]);
                            self.update_poll = Instant::now();
                            let success =
                                crate::app::is_success(&value) && self.updates["phase"] != "failed";
                            if method == "updates.apply" {
                                self.observe_local_shutdown(&value);
                                self.quit_requested = self.update_apply_requested
                                    && success
                                    && self.updates["quit_requested"] == true
                                    && self.updates["exit_required"] == true;
                                self.update_apply_requested = false;
                                if !self.quit_requested {
                                    self.notice(
                                        if self.local_sessions_retired {
                                            "本地媒体连接已停止；更新未完成，请查看错误后重试。直播间状态保留。"
                                        } else if self.local_shutdown_attempted {
                                            "本地会话关闭未完成；更新未应用，请查看错误后处理。"
                                        } else {
                                            "更新尚未应用，请查看状态后重试。"
                                        },
                                        false,
                                    );
                                }
                            } else if method == "updates.check" {
                                if success && self.updates["update_available"] == true {
                                    self.notice(
                                        "发现新版本。前往「更新与组件」查看并选择更新方式。",
                                        true,
                                    );
                                } else if success && self.updates["phase"] == "up_to_date" {
                                    self.notice("当前已是最新稳定版本。", true);
                                }
                            } else if !was_downloaded && self.updates["phase"] == "downloaded" {
                                self.notice("更新已下载。准备好后，可选择应用并重启。", true);
                            }
                            if !success && method != "updates.apply" {
                                let error = if self.update_error.is_empty() {
                                    "更新操作未完成，请查看详情。".to_owned()
                                } else {
                                    self.update_error.clone()
                                };
                                self.notice(&error, false);
                            }
                        }
                        "obs.plugin.status" | "obs.plugin.install" => {
                            self.plugin = data;
                            self.plugin_error.clear();
                            self.plugin_restart_required |= self.plugin["restart_required"] == true;
                            if method == "obs.plugin.install" {
                                self.plugin_replace = false;
                                if crate::app::is_success(&value)
                                    && self.plugin["installed"] == true
                                {
                                    self.notice("插件文件已安装。请在方便时手动重启 OBS。", true);
                                } else {
                                    self.plugin_error = text(&self.plugin, &["message", "error"]);
                                    self.notice("插件安装未完成，请查看组件状态。", false);
                                }
                            }
                        }
                        "config.get" => {
                            self.config = data;
                            self.load_config();
                            self.load_appearance();
                        }
                        "account.list" => {
                            self.accounts = data;
                            if !self.room_loaded
                                && (!self.account_rows().is_empty()
                                    || value
                                        .pointer("/_context/uid")
                                        .and_then(Value::as_str)
                                        .is_some_and(|uid| !uid.is_empty()))
                            {
                                self.submit("room.get", json!({}));
                            }
                        }
                        "room.get" => {
                            let first = !self.room_loaded;
                            self.room = data;
                            self.room_loaded = true;
                            if !self.room_dirty {
                                self.title = text(&self.room, &["title"]);
                                self.announcement =
                                    text(&self.room, &["announcement", "room_news"]);
                                let area = text(&self.room, &["area_id", "area_v2_id"]);
                                if !area.is_empty() {
                                    self.area_id = area;
                                }
                            }
                            if first {
                                self.submit("room.titles", json!({}));
                                self.submit("area.recent", json!({}));
                                self.submit("room.audit", json!({}));
                            }
                            let cover = cover_identity(&self.room);
                            if !self.cover_local
                                && !cover.is_empty()
                                && self.cover_identity != cover
                            {
                                self.cover_identity = cover;
                                self.cover_loading = true;
                                self.submit("room.cover.preview", json!({}));
                            }
                        }
                        "room.titles" => {
                            self.titles = data
                                .get("titles")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str)
                                .take(5)
                                .map(str::to_owned)
                                .collect()
                        }
                        "area.recent" => self.recent_areas = recent_areas(&data, &self.areas),
                        "room.audit" => {
                            self.audit = audit_description(&data);
                            self.audit_poll = Instant::now();
                            if data.get("pending").and_then(Value::as_bool) == Some(false) {
                                self.audit_until = None;
                            }
                        }
                        "room.cover.preview" => {
                            if !self.cover_local {
                                if let Some(encoded) = data.get("base64").and_then(Value::as_str) {
                                    self.image_serial += 1;
                                    self.cover_image_id = self.image_serial;
                                    self.cover_loading = self.image_request(AssetRequest::Base64 {
                                        id: self.cover_image_id,
                                        kind: "cover",
                                        encoded: encoded.into(),
                                    });
                                } else {
                                    self.cover_loading = false;
                                }
                            }
                        }
                        "area.list" => {
                            self.areas.clear();
                            collect_areas(&data, "", &mut self.areas);
                            self.areas.sort_by_key(|(id, _)| *id);
                            self.areas.dedup_by_key(|(id, _)| *id);
                            if self.room_loaded {
                                self.submit("area.recent", json!({}));
                            }
                        }
                        "auth.qr.create" => {
                            self.qr_url = text(&data, &["url"]);
                            self.qr_key = text(&data, &["qrcode_key", "key"]);
                            self.qr = QrCode::encode_text(&self.qr_url, QrCodeEcc::Medium).ok();
                            self.qr_created = Some(Instant::now());
                            self.qr_poll = Instant::now();
                            self.qr_message = "使用哔哩哔哩客户端扫码并确认".into();
                        }
                        "auth.qr.poll" => {
                            let code = data.get("code").and_then(Value::as_i64).unwrap_or(0);
                            match code {
                                86101 => self.qr_message = "等待扫码".into(),
                                86090 => self.qr_message = "扫码成功，请在手机上确认".into(),
                                86038 => {
                                    self.qr_message = "二维码已过期，请重新生成".into();
                                    self.qr_key.clear();
                                }
                                0 => {
                                    self.qr_key.clear();
                                    self.qr = None;
                                    self.qr_message = "登录成功".into();
                                    self.notice("账号已登录。", true);
                                    self.submit("account.list", json!({}));
                                    self.submit("room.get", json!({}));
                                }
                                _ => {
                                    self.qr_message = text(&data, &["message", "msg"]);
                                }
                            }
                        }
                        "auth.import" | "account.use" | "account.remove" => {
                            self.clear_account_view();
                            self.cookie.clear();
                            self.stream = json!({});
                            self.show_key = false;
                            self.submit("account.list", json!({}));
                            if method != "account.remove" {
                                self.submit("room.get", json!({}));
                            }
                            self.notice("账号设置已更新。", true);
                        }
                        "room.update" | "room.cover" | "live.stop" => {
                            if method == "room.update" && crate::app::is_success(&value) {
                                self.room_dirty = false;
                                self.submit("room.titles", json!({}));
                            }
                            if method == "room.cover" {
                                self.crop_busy = false;
                                if crate::app::is_success(&value) {
                                    self.page = Page::Room;
                                    self.cover_local = false;
                                    self.cover_identity.clear();
                                }
                            }
                            self.submit("room.get", json!({}));
                            if method == "live.stop" {
                                self.stream = json!({});
                                self.show_key = false;
                            } else {
                                self.audit_until = Some(Instant::now() + Duration::from_secs(120));
                                self.audit_poll = Instant::now();
                                self.submit("room.audit", json!({}));
                            }
                            self.notice(
                                if crate::app::is_success(&value) {
                                    "直播间已更新。"
                                } else {
                                    "操作未完全完成，请在完整操作中查看详细结果。"
                                },
                                crate::app::is_success(&value),
                            );
                        }
                        "live.start" | "live.stream" => {
                            if data.get("status").and_then(Value::as_str)
                                == Some("verification_required")
                            {
                                self.verification =
                                    data.get("verification").cloned().unwrap_or(data);
                                self.verification_created = Some(Instant::now());
                                self.verification_message = "请使用手机扫码完成平台验证。".into();
                                let url = text(&self.verification, &["url"]);
                                self.verification_qr = if url.is_empty() {
                                    None
                                } else {
                                    QrCode::encode_text(&url, QrCodeEcc::Medium).ok()
                                };
                                if let Some(voucher) = self
                                    .verification
                                    .get("v_voucher")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned)
                                {
                                    self.submit("face.register", json!({"v_voucher": voucher}));
                                }
                                self.notice("平台要求验证，请按屏幕提示完成。", false);
                            } else {
                                self.stream = data;
                                self.submit("room.get", json!({}));
                                if self.stream.get("available").and_then(Value::as_bool)
                                    == Some(false)
                                {
                                    self.notice("房间已开播，但平台没有提供所选推流协议。请切换协议后重新获取地址。", false);
                                } else if !crate::app::is_success(&self.stream) {
                                    self.notice(
                                        if self.stream["room_remains_live"] == true {
                                            "直播间已开播，但 OBS 自动推流失败。请检查 OBS 的连接和推流状态，再手动处理；无需重复开播。"
                                        } else {
                                            "开播或推流地址请求未完成，请在完整操作中查看详细结果。"
                                        },
                                        false,
                                    );
                                } else {
                                    self.notice(
                                        if method == "live.start" {
                                            "开播请求已完成。"
                                        } else {
                                            "推流地址已更新。"
                                        },
                                        true,
                                    );
                                }
                            }
                        }
                        "face.register" => {
                            if !self.verification.is_null() {
                                self.verification = data;
                                let url = text(&self.verification, &["url"]);
                                self.verification_qr =
                                    QrCode::encode_text(&url, QrCodeEcc::Medium).ok();
                                self.verification_poll = Instant::now();
                            }
                        }
                        "face.poll" => {
                            if self.verification.is_null() {
                                continue;
                            } else if value.get("verified").and_then(Value::as_bool) == Some(true) {
                                let kind = self
                                    .verification
                                    .get("type")
                                    .cloned()
                                    .unwrap_or(json!(60043));
                                if let Ok(area) = self.area_id.parse::<u64>() {
                                    if area > 0 {
                                        self.submit("face.report", json!({"type": kind, "message": "verified", "area_id": area}));
                                    }
                                }
                                self.verification = Value::Null;
                                self.verification_qr = None;
                                self.notice("平台验证已完成，正在继续开播。", true);
                                self.start_live();
                            } else if value.get("status").and_then(Value::as_str)
                                == Some("expired_or_failed")
                            {
                                self.verification_message =
                                    "验证已过期或未通过。请取消后重新开播。".into();
                                self.verification_created = None;
                            } else {
                                self.verification_message = "等待手机上的验证完成。".into();
                            }
                        }
                        "obs.status" => self.obs = data,
                        "obs.watch.start" | "obs.watch.stop" | "obs.watch.status" => {
                            if let Some(output) =
                                data.get("output").filter(|value| value.is_object())
                            {
                                self.obs = output.clone();
                            }
                            self.obs["connected"] =
                                data.get("connected").cloned().unwrap_or(json!(false));
                            self.obs["observer_running"] =
                                data.get("running").cloned().unwrap_or(json!(false));
                            if let Some(error) = data.get("error").filter(|value| !value.is_null())
                            {
                                self.obs["error"] = error.clone();
                            }
                            self.obs_watch_poll = Instant::now();
                        }
                        "obs.configure" | "obs.sync" | "obs.start" | "obs.stop" => {
                            self.obs_password.clear();
                            self.submit("obs.status", json!({}));
                            self.notice("OBS 操作已完成。", true);
                        }
                        "pk.status" => {
                            self.pk = data;
                            let session = text(&self.pk, &["biz_session_id"]);
                            if !session.is_empty() {
                                self.session_id = session;
                            }
                            let peer = text(&self.pk, &["peer_uid", "uid"]);
                            if !peer.is_empty() {
                                self.opponent_uid = peer;
                            }
                            let source = text(&self.pk, &["source"]);
                            if !source.is_empty() {
                                self.invitation_source = source;
                            }
                        }
                        "pk.match" | "pk.cancel" | "pk.invite" | "pk.accept" | "pk.reject"
                        | "pk.end" => {
                            self.submit("pk.status", json!({}));
                            self.notice("PK 操作已提交。", true);
                        }
                        "danmaku.send" => {
                            self.danmaku.clear();
                            self.notice("弹幕已发送。", true);
                        }
                        "danmaku.connect" => {
                            self.events_connected = true;
                            self.notice("已连接直播间事件，等待主播邀请。", true);
                        }
                        "danmaku.disconnect" => self.events_connected = false,
                        "danmaku.read" => {
                            if self.events_connected {
                                if let Some(events) = data.get("events").and_then(Value::as_array) {
                                    for event in events {
                                        self.consume_invitation(event);
                                    }
                                }
                            }
                        }
                        "config.set" => {
                            self.config = data;
                            self.notice("偏好设置已保存。", true);
                        }
                        "config.reset" => {
                            self.config = data;
                            self.appearance_loaded = false;
                            self.load_appearance();
                            self.notice("所选设置已恢复默认值。", true);
                        }
                        "zuse.run" | "zuse.schema" => {
                            self.script_output = serde_json::to_string_pretty(&value)
                                .unwrap_or_else(|_| value.to_string());
                            let success = crate::app::is_success(&value);
                            if method == "zuse.run" {
                                self.observe_local_shutdown(&value);
                                if success
                                    && value["status"] == "completed"
                                    && value["quit_requested"] == true
                                    && value["exit_required"] == true
                                    && !self.demo
                                    && !self.read_only
                                {
                                    self.quit_requested = true;
                                }
                            }
                            self.notice(
                                if success {
                                    "自动化操作已完成。"
                                } else if self.local_sessions_retired {
                                    "本地媒体连接已停止；更新未完成，请查看执行结果。"
                                } else if self.local_shutdown_attempted {
                                    "本地会话关闭未完成；更新未应用，请查看执行结果后处理。"
                                } else {
                                    "自动化操作未完成，请查看执行结果详情。"
                                },
                                success,
                            );
                        }
                        _ => self.notice("操作已完成。", true),
                    }
                    if !matches!(
                        method,
                        "config.get"
                            | "account.list"
                            | "room.get"
                            | "room.titles"
                            | "room.audit"
                            | "room.cover.preview"
                            | "area.recent"
                            | "obs.status"
                            | "obs.watch.status"
                            | "pk.status"
                            | "area.list"
                            | "auth.qr.poll"
                            | "danmaku.read"
                            | "face.poll"
                            | "updates.status"
                            | "obs.plugin.status"
                    ) {
                        let complete = crate::app::is_success(&value)
                            && (!method.starts_with("updates.")
                                || payload(&value)["phase"] != "failed");
                        self.record(method, complete);
                    }
                }
            }
        }
        changed
    }

    fn record(&mut self, method: &str, success: bool) {
        self.activity
            .insert(0, (action_label(method).into(), success));
        self.activity.truncate(5);
    }

    fn observe_local_shutdown(&mut self, value: &Value) {
        for result in [Some(value), value.pointer("/failed_step/result")]
            .into_iter()
            .flatten()
        {
            let retired = result["local_sessions_retired"] == true;
            let attempted = retired || result["local_shutdown_attempted"] == true;
            self.local_sessions_retired |= retired;
            self.local_shutdown_attempted |= attempted;
            if attempted {
                self.events_connected = false;
                self.obs_watch_requested = Some(false);
            }
        }
    }

    fn load_config(&mut self) {
        if self.config_loaded {
            return;
        }
        self.config_loaded = true;
        for (key, destination) in [
            ("protocol", &mut self.protocol),
            ("strategy", &mut self.strategy),
            ("obs_url", &mut self.obs_url),
            ("default_area", &mut self.area_id),
            ("delay", &mut self.delay),
        ] {
            let value = text(&self.config, &[key]);
            if !value.is_empty() {
                *destination = value;
            }
        }
        self.obs_auto_start = self
            .config
            .get("obs_auto_start")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.obs_auto_stop = self
            .config
            .get("obs_auto_stop")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.obs_auto_connect = self
            .config
            .get("obs_auto_connect")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.obs_link = self
            .config
            .get("obs_link")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    }

    fn clear_account_view(&mut self) {
        self.room = json!({});
        self.pk = json!({});
        self.stream = json!({});
        self.room_loaded = false;
        self.room_dirty = false;
        self.show_key = false;
        self.title.clear();
        self.announcement.clear();
        self.area_id = text(&self.config, &["default_area"]);
        self.titles.clear();
        self.recent_areas.clear();
        self.cover = None;
        self.cover_local = false;
        self.cover_loading = false;
        self.cover_path.clear();
        self.cover_identity.clear();
        self.image_serial += 1;
        self.cover_image_id = self.image_serial;
        self.image_serial += 1;
        self.crop_image_id = self.image_serial;
        self.crop_busy = false;
        self.session_id.clear();
        self.opponent_uid.clear();
        self.invitation_source.clear();
        self.events_connected = false;
        self.audit.clear();
        self.audit_until = None;
        self.verification = Value::Null;
        self.verification_qr = None;
    }

    fn load_appearance(&mut self) {
        if self.appearance_loaded {
            return;
        }
        self.appearance_loaded = true;
        self.font_family = text(&self.config, &["font_family"]);
        self.background_path = text(&self.config, &["background_image"]);
        self.background_mode = text(&self.config, &["background_mode"]);
        if self.background_mode.is_empty() {
            self.background_mode = "cover".into();
        }
        self.background_opacity = self
            .config
            .get("background_opacity")
            .and_then(Value::as_u64)
            .unwrap_or(15)
            .to_string();
        self.background_blur = self
            .config
            .get("background_blur")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .to_string();
        self.tray_path = text(&self.config, &["tray_icon"]);
        self.tray_tooltip = text(&self.config, &["tray_tooltip"]);
        if self.tray_tooltip.is_empty() {
            self.tray_tooltip = "bilibilive".into();
        }
        self.tray_enabled = self
            .config
            .get("tray_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        self.close_to_tray = self
            .config
            .get("close_to_tray")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.single_instance = self
            .config
            .get("single_instance")
            .and_then(Value::as_bool)
            .unwrap_or(true);
    }

    fn image_request(&mut self, request: AssetRequest) -> bool {
        let Some(images) = &self.images else {
            return false;
        };
        match images.submit(request) {
            Ok(()) => true,
            Err(error) => {
                self.notice(&error, false);
                false
            }
        }
    }

    fn receive_images(&mut self) -> bool {
        let mut changed = false;
        let background_path = text(&self.config, &["background_image"]);
        let blur = self
            .config
            .get("background_blur")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(40);
        let stamp = format!("{background_path}|{blur}");
        if stamp != self.background_identity {
            if background_path.is_empty() {
                self.background = None;
                self.background_identity = stamp;
                self.image_serial += 1;
                self.background_image_id = self.image_serial;
                changed = true;
            } else {
                self.image_serial += 1;
                self.background_image_id = self.image_serial;
                if self.image_request(AssetRequest::Load {
                    id: self.background_image_id,
                    kind: "background",
                    path: background_path.into(),
                    blur: blur as f32,
                }) {
                    self.background_identity = stamp;
                }
            }
        }
        let tray_path = text(&self.config, &["tray_icon"]);
        if tray_path != self.tray_identity {
            self.image_serial += 1;
            self.tray_image_id = self.image_serial;
            if tray_path.is_empty() {
                self.tray_image = None;
                self.tray_identity.clear();
                changed = true;
            } else if self.image_request(AssetRequest::Load {
                id: self.tray_image_id,
                kind: "tray",
                path: tray_path.clone().into(),
                blur: 0.,
            }) {
                self.tray_identity = tray_path;
            }
        }
        loop {
            let response = self.images.as_ref().and_then(Images::next);
            let Some(response) = response else {
                break;
            };
            let expected = match response.kind {
                "cover" => self.cover_image_id,
                "background" => self.background_image_id,
                "tray" => self.tray_image_id,
                "crop" => self.crop_image_id,
                _ => 0,
            };
            if response.id != expected {
                if let Ok(Asset::File(path)) = response.result {
                    let _ = std::fs::remove_file(path);
                }
                continue;
            }
            changed = true;
            match response.result {
                Ok(Asset::Image(bitmap)) => match response.kind {
                    "cover" => {
                        self.crop = CropRect::centered(&bitmap);
                        self.cover = Some(bitmap);
                        self.cover_loading = false;
                    }
                    "background" => self.background = Some(bitmap),
                    "tray" => self.tray_image = Some(bitmap),
                    _ => (),
                },
                Ok(Asset::File(path)) => {
                    self.crop_busy =
                        self.submit_cleanup("room.cover", json!({"path":path}), Some(path));
                }
                Err(error) => {
                    if response.kind == "cover" {
                        self.cover_loading = false;
                    }
                    if response.kind == "crop" {
                        self.crop_busy = false;
                    }
                    self.notice(&error, false);
                }
            }
        }
        changed
    }

    fn load_cover(&mut self) {
        if self.cover_path.trim().is_empty() {
            self.notice("请先填写本地图片的完整路径。", false);
            return;
        }
        self.image_serial += 1;
        self.cover_image_id = self.image_serial;
        self.cover_loading = self.image_request(AssetRequest::Load {
            id: self.cover_image_id,
            kind: "cover",
            path: self.cover_path.trim().into(),
            blur: 0.,
        });
        if self.cover_loading {
            self.cover_local = true;
            self.cover = None;
            self.page = Page::Cover;
        }
    }

    fn handle_event(&mut self, event: Option<&Event>) -> bool {
        if self.page != Page::Cover || self.crop_busy {
            return false;
        }
        if matches!(event, Some(Event::Key { code: 27, .. })) {
            self.page = Page::Room;
            self.crop_drag = None;
            return true;
        }
        let Some(image) = &self.cover else {
            return false;
        };
        let display = self.crop_display;
        if display.w <= 0. || display.h <= 0. {
            return false;
        }
        let point = |x: f32, y: f32| {
            (
                (x - display.x) / display.w * image.width() as f32,
                (y - display.y) / display.h * image.height() as f32,
            )
        };
        match event {
            Some(Event::Down(x, y)) if display.contains(*x, *y) => {
                let (x, y) = point(*x, *y);
                let inside = x >= self.crop.x as f32
                    && y >= self.crop.y as f32
                    && x < (self.crop.x + self.crop.width) as f32
                    && y < (self.crop.y + self.crop.height) as f32;
                self.crop_drag = Some((x, y, self.crop, inside));
                true
            }
            Some(Event::Move(x, y)) if self.crop_drag.is_some() => {
                let (start_x, start_y, initial, moving) = self.crop_drag.unwrap();
                let (x, y) = point(*x, *y);
                if moving {
                    self.crop = initial.moved(image, x - start_x, y - start_y);
                } else {
                    let ax = start_x.clamp(0., image.width() as f32 - 1.).floor() as u32;
                    let ay = start_y.clamp(0., image.height() as f32 - 1.).floor() as u32;
                    let positive_x = x >= start_x;
                    let positive_y = y >= start_y;
                    let available_x = if positive_x { image.width() - ax } else { ax };
                    let available_y = if positive_y { image.height() - ay } else { ay };
                    let maximum = (available_x / 16).min(available_y / 9);
                    if maximum > 0 {
                        let units = (((x - start_x).abs().max((y - start_y).abs() * 16. / 9.) / 16.).round() as u32).clamp(1, maximum);
                        let (width, height) = (units * 16, units * 9);
                        self.crop = CropRect {
                            x: if positive_x { ax } else { ax - width },
                            y: if positive_y { ay } else { ay - height },
                            width,
                            height,
                        };
                    }
                }
                true
            }
            Some(Event::Up(_, _)) => self.crop_drag.take().is_some(),
            Some(Event::Scroll(delta)) => {
                self.crop = self.crop.scaled(image, if *delta < 0 { 0.9 } else { 1.1 });
                true
            }
            _ => false,
        }
    }

    fn tick(&mut self) -> bool {
        let before = (
            self.qr_key.len(),
            self.verification_created.is_some(),
            self.pending,
        );
        if !self.startup_update_handled && self.config_loaded && self.pending == 0 {
            self.startup_update_handled = true;
            if !self.demo
                && !self.read_only
                && !self.no_update
                && self.config["update_check"] == true
            {
                self.request_update("updates.check");
            }
        }
        if !self.demo
            && self.updates["phase"] == "downloading"
            && self.update_error.is_empty()
            && self.pending == 0
            && self.update_poll.elapsed() >= Duration::from_secs(1)
        {
            self.request_update("updates.status");
        }
        if !self.demo
            && !self.read_only
            && !self.local_shutdown_attempted
            && self.config_loaded
            && self.pending == 0
        {
            let requested = ["obs_auto_connect", "obs_link", "obs_auto_stop"]
                .into_iter()
                .any(|key| {
                    self.config
                        .get(key)
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                });
            if self.obs_watch_requested != Some(requested) {
                if self.submit_cleanup(
                    if requested {
                        "obs.watch.start"
                    } else {
                        "obs.watch.stop"
                    },
                    json!({}),
                    None,
                ) {
                    self.obs_watch_requested = Some(requested);
                    self.obs_watch_poll = Instant::now();
                }
            } else if requested
                && self.page == Page::Obs
                && self.obs_watch_poll.elapsed() >= Duration::from_secs(3)
            {
                self.obs_watch_poll = Instant::now();
                self.submit("obs.watch.status", json!({}));
            }
        }
        if !self.qr_key.is_empty()
            && self.qr_poll.elapsed() >= Duration::from_secs(3)
            && self.pending == 0
        {
            if self
                .qr_created
                .is_some_and(|time| time.elapsed() > Duration::from_secs(180))
            {
                self.qr_key.clear();
                self.qr_message = "二维码已过期，请重新生成".into();
            } else {
                self.qr_poll = Instant::now();
                self.submit(
                    "auth.qr.poll",
                    json!({"qrcode_key": self.qr_key, "name": self.account_name}),
                );
            }
        }
        if !self.verification.is_null()
            && self.verification_poll.elapsed() >= Duration::from_secs(3)
            && self.pending == 0
        {
            if self
                .verification_created
                .is_some_and(|time| time.elapsed() <= Duration::from_secs(180))
            {
                self.verification_poll = Instant::now();
                let kind = self
                    .verification
                    .get("type")
                    .and_then(Value::as_u64)
                    .unwrap_or(60043);
                if kind == 60024 || self.verification.get("token").is_some() {
                    self.poll_verification();
                }
            } else if self.verification_created.is_some() {
                self.verification_created = None;
                self.verification_message = "验证等待已结束。请手动检查，或取消后重新开播。".into();
            }
        }
        if self.last_refresh.elapsed() >= Duration::from_secs(30)
            && self.pending == 0
            && self.room_loaded
        {
            self.last_refresh = Instant::now();
            self.submit("room.get", json!({}));
            self.submit("account.list", json!({}));
            self.submit("config.get", json!({}));
            if self.page == Page::Obs {
                self.submit("obs.status", json!({}));
            } else if self.page == Page::Pk {
                self.submit("pk.status", json!({}));
            }
        }
        if self.events_connected
            && self.events_poll.elapsed() >= Duration::from_secs(2)
            && self.pending == 0
        {
            self.events_poll = Instant::now();
            self.submit("danmaku.read", json!({"limit": 8}));
        }
        if self.page == Page::Room
            && self.pending == 0
            && self
                .audit_until
                .is_some_and(|deadline| Instant::now() < deadline)
            && self.audit_poll.elapsed() >= Duration::from_secs(5)
        {
            self.audit_poll = Instant::now();
            self.submit("room.audit", json!({}));
        }
        before
            != (
                self.qr_key.len(),
                self.verification_created.is_some(),
                self.pending,
            )
    }

    fn account_rows(&self) -> Vec<Value> {
        self.accounts
            .as_array()
            .or_else(|| self.accounts.get("accounts").and_then(Value::as_array))
            .cloned()
            .unwrap_or_default()
    }

    fn active_name(&self) -> String {
        let username = text(&self.room, &["uname", "username"]);
        if !username.is_empty() {
            return username;
        }
        let selected = text(&self.accounts, &["active"]);
        if !selected.is_empty() {
            return selected;
        }
        self.account_rows()
            .into_iter()
            .find(|account| account.get("active").and_then(Value::as_bool) == Some(true))
            .map(|account| text(&account, &["uname", "name"]))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "尚未登录".into())
    }

    fn is_live(&self) -> bool {
        self.room.get("live_status").and_then(Value::as_u64) == Some(1)
            || self.room.get("live").and_then(Value::as_bool) == Some(true)
    }

    fn activate(&mut self, page: Page) {
        if self.page == page {
            return;
        }
        self.page = page;
        match page {
            Page::Room => {
                if self.areas.is_empty() {
                    self.submit("area.list", json!({}));
                }
                self.submit("area.recent", json!({}));
                self.submit("room.titles", json!({}));
                self.submit("room.audit", json!({}));
            }
            Page::Titles => self.submit("room.titles", json!({})),
            Page::Appearance => self.load_appearance(),
            Page::Components => {
                if self.demo {
                    self.updates = demo_response("updates.status").unwrap_or_default();
                    self.plugin = demo_response("obs.plugin.status").unwrap_or_default();
                } else {
                    self.request_update("updates.status");
                    self.submit("obs.plugin.status", json!({}));
                }
            }
            Page::Accounts => self.submit("account.list", json!({})),
            Page::Obs => self.submit("obs.status", json!({})),
            Page::Pk => self.submit("pk.status", json!({})),
            _ => {}
        }
    }

    fn draw(&mut self, canvas: &mut Canvas<'_>, ui: &mut Ui) {
        let width = canvas.width;
        let height = canvas.height;
        let sidebar = 220.;
        if let Some(background) = &self.background {
            let mode = self
                .config
                .get("background_mode")
                .and_then(Value::as_str)
                .unwrap_or("cover");
            let opacity = (self
                .config
                .get("background_opacity")
                .and_then(Value::as_u64)
                .unwrap_or(15)
                .min(100)
                * 255
                / 100) as u8;
            let (source, destination) = desktop::fit_image(
                background,
                Rect::new(sidebar, 0., width - sidebar, height),
                mode,
            );
            desktop::paint_image(canvas, background, source, destination, opacity);
        }
        canvas.rect(Rect::new(0., 0., sidebar, height), 0., PANEL);
        canvas.text("bili", 31., 34., 28, true, INK);
        canvas.text("bilive", 72., 34., 28, false, ACCENT);
        canvas.text("LIVE ON YOUR TERMS", 32., 74., 10, false, MUTED);
        for (index, page) in [
            Page::Overview,
            Page::Room,
            Page::Accounts,
            Page::Obs,
            Page::Pk,
            Page::Automation,
        ]
        .into_iter()
        .enumerate()
        {
            let y = 132. + index as f32 * 53.;
            if page == self.page {
                canvas.rect(Rect::new(19., y, 182., 43.), 11., ACCENT_SOFT);
                canvas.rect(Rect::new(19., y + 13., 3., 17.), 1.5, ACCENT);
            }
            nav_icon(
                canvas,
                39.,
                y + 21.,
                index,
                if page == self.page { ACCENT } else { MUTED },
            );
            if ui.button(
                canvas,
                10 + index as u64,
                Rect::new(61., y, 132., 43.),
                page.label(),
                ButtonStyle::Quiet,
                true,
            ) {
                self.activate(page);
            }
        }
        if ui.button(
            canvas,
            20,
            Rect::new(30., height - 149., 160., 40.),
            "偏好设置",
            ButtonStyle::Quiet,
            true,
        ) {
            self.activate(Page::Settings);
        }
        if self.updates["update_available"] == true
            && ui.button(
                canvas,
                22,
                Rect::new(30., height - 198., 160., 36.),
                if self.updates["phase"] == "downloaded" {
                    "更新已准备好"
                } else {
                    "新版本可用"
                },
                ButtonStyle::Secondary,
                true,
            )
        {
            self.activate(Page::Components);
        }
        canvas.line((30., height - 91.), (190., height - 91.), 1., HAIRLINE);
        let active_name = self.active_name();
        canvas.rect(Rect::new(31., height - 68., 29., 29.), 14.5, ACCENT_SOFT);
        let initial = active_name.chars().next().unwrap_or('B').to_string();
        canvas.text(&initial, 40., height - 63., 14, true, ACCENT);
        canvas.ellipsis(&active_name, 72., height - 69., 123., 13, true, INK);
        canvas.text(
            if self.demo {
                "演示模式"
            } else {
                "本地工作空间"
            },
            72.,
            height - 49.,
            11,
            false,
            MUTED,
        );

        let x = sidebar + 42.;
        let content_width = (width - x - 42.).max(380.);
        canvas.text(self.page.eyebrow(), x, 35., 10, false, MUTED);
        canvas.text(self.page.label(), x, 62., 28, true, INK);
        if self.demo {
            pill(canvas, width - 128., 38., "DEMO", ACCENT, ACCENT_SOFT);
        } else if self.updates["phase"] == "downloading" {
            pill(canvas, width - 140., 38., "正在下载", ACCENT, ACCENT_SOFT);
        } else if self.pending > 0 {
            pill(canvas, width - 140., 38., "正在处理", ACCENT, ACCENT_SOFT);
        } else {
            canvas.rect(Rect::new(width - 120., 45., 5., 5.), 2.5, SUCCESS);
            canvas.text("准备就绪", width - 106., 39., 11, false, MUTED);
        }
        if self.verification.is_null() {
            match self.page {
                Page::Overview => self.overview(canvas, ui, x, content_width, height),
                Page::Room => self.room_page(canvas, ui, x, content_width, height),
                Page::Accounts => self.accounts_page(canvas, ui, x, content_width, height),
                Page::Obs => self.obs_page(canvas, ui, x, content_width),
                Page::Pk => self.pk_page(canvas, ui, x, content_width),
                Page::Automation => self.automation_page(canvas, ui, x, content_width, height),
                Page::Settings => self.settings_page(canvas, ui, x, content_width),
                Page::Advanced => self.advanced_page(canvas, ui, x, content_width, height),
                Page::Cover => self.cover_page(canvas, ui, x, content_width, height),
                Page::Titles => self.titles_page(canvas, ui, x, content_width),
                Page::Appearance => self.appearance_page(canvas, ui, x, content_width),
                Page::Components => self.components_page(canvas, ui, x, content_width),
            }
        } else {
            self.verification_panel(canvas, ui, x, content_width, height);
        }
        if let Some((message, success, time)) = &self.notice {
            if time.elapsed() < Duration::from_secs(9) {
                let rect = Rect::new(x, height - 57., content_width, 35.);
                canvas.rect(rect, 9., if *success { 0xe6f4eaff } else { 0xffeeedff });
                canvas.ellipsis(
                    message,
                    x + 13.,
                    height - 47.,
                    content_width - 26.,
                    12,
                    false,
                    if *success { SUCCESS } else { RED },
                );
            }
        }
    }

    fn overview(&mut self, canvas: &mut Canvas<'_>, ui: &mut Ui, x: f32, w: f32, height: f32) {
        let live = self.is_live();
        let busy = self.pending > 0;
        let title = if self.title.is_empty() {
            "准备下一场好直播。"
        } else {
            &self.title
        };
        canvas.rect(Rect::new(x, 125., w, 202.), 20., PANEL);
        pill(
            canvas,
            x + 27.,
            149.,
            if live { "正在直播" } else { "待开播" },
            if live { SUCCESS } else { MUTED },
            if live { 0xe6f4eaff } else { 0xf0f4faff },
        );
        let room_id = text(&self.room, &["room_id", "roomid"]);
        if !room_id.is_empty() {
            canvas.text(&format!("ROOM {room_id}"), x + 129., 156., 10, false, MUTED);
        }
        canvas.ellipsis(title, x + 27., 196., w - 54., 27, true, INK);
        let area = text(&self.room, &["area_name", "area_v2_name"]);
        canvas.text(
            if area.is_empty() {
                "连接账号，选好分区，然后开始。"
            } else {
                &area
            },
            x + 28.,
            244.,
            13,
            false,
            MUTED,
        );
        if ui.button(
            canvas,
            100,
            Rect::new(x + 27., 276., 143., 36.),
            if live { "结束直播" } else { "开始直播" },
            if live {
                ButtonStyle::Danger
            } else {
                ButtonStyle::Primary
            },
            !busy && !self.demo,
        ) {
            if live {
                self.submit("live.stop", json!({}));
            } else {
                self.start_live();
            }
        }
        if ui.button(
            canvas,
            101,
            Rect::new(x + 184., 276., 118., 36.),
            "编辑直播间",
            ButtonStyle::Quiet,
            true,
        ) {
            self.activate(Page::Room);
        }
        if ui.button(
            canvas,
            102,
            Rect::new(x + w - 122., 276., 95., 36.),
            "刷新状态",
            ButtonStyle::Quiet,
            !busy,
        ) {
            self.submit("room.get", json!({}));
        }

        let stat_width = (w - 32.) / 3.;
        let followers = text(&self.room, &["followers", "attention"]);
        let online = text(&self.room, &["online", "watched_show"]);
        let obs_connected = self
            .obs
            .get("connected")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        stat(
            canvas,
            x,
            354.,
            stat_width,
            "直播分区",
            if area.is_empty() {
                "尚未选择"
            } else {
                &area
            },
            "开播前可随时调整",
        );
        stat(
            canvas,
            x + stat_width + 16.,
            354.,
            stat_width,
            "直播间关注",
            if followers.is_empty() {
                "—"
            } else {
                &followers
            },
            if online.is_empty() {
                "连接后同步房间信息"
            } else {
                "已同步直播间信息"
            },
        );
        stat(
            canvas,
            x + (stat_width + 16.) * 2.,
            354.,
            stat_width,
            "OBS",
            if obs_connected {
                "已连接"
            } else {
                "待连接"
            },
            "推流配置与开停播联动",
        );

        let bottom = 484.;
        let left_width = (w * 0.62).min(w - 250.);
        canvas.text("推流连接", x, bottom, 17, true, INK);
        canvas.text(
            "将地址与密钥交给你喜欢的推流工具。",
            x,
            bottom + 28.,
            12,
            false,
            MUTED,
        );
        let (address, key) = self.stream_values();
        let shown_address = if address.is_empty() {
            "获取后显示服务器地址"
        } else {
            &address
        };
        canvas.rect(Rect::new(x, bottom + 62., left_width, 49.), 10., PANEL);
        canvas.ellipsis(
            shown_address,
            x + 14.,
            bottom + 79.,
            left_width - 28.,
            12,
            false,
            MUTED,
        );
        canvas.rect(Rect::new(x, bottom + 122., left_width, 49.), 10., PANEL);
        let shown_key = if key.is_empty() {
            "推流密钥默认隐藏".into()
        } else if self.show_key {
            key.clone()
        } else {
            "••••••••••••••••••••••••".into()
        };
        canvas.ellipsis(
            &shown_key,
            x + 14.,
            bottom + 139.,
            left_width - 84.,
            12,
            false,
            MUTED,
        );
        if ui.button(
            canvas,
            103,
            Rect::new(x + left_width - 64., bottom + 129., 54., 34.),
            if self.show_key { "隐藏" } else { "显示" },
            ButtonStyle::Quiet,
            !key.is_empty(),
        ) {
            self.show_key = !self.show_key;
        }
        if ui.button(
            canvas,
            104,
            Rect::new(x, bottom + 185., 116., 37.),
            "获取推流地址",
            ButtonStyle::Secondary,
            !busy && !self.demo,
        ) {
            self.submit(
                "live.stream",
                json!({"protocol": self.protocol, "strategy": self.strategy}),
            );
        }
        if ui.button(
            canvas,
            105,
            Rect::new(x + 126., bottom + 185., 89., 37.),
            "复制地址",
            ButtonStyle::Quiet,
            !address.is_empty(),
        ) {
            self.copy = Some(address);
            self.notice("服务器地址已复制。", true);
        }
        if ui.button(
            canvas,
            106,
            Rect::new(x + 222., bottom + 185., 89., 37.),
            "复制密钥",
            ButtonStyle::Quiet,
            !key.is_empty(),
        ) {
            self.copy = Some(key);
            self.notice("推流密钥已复制。", true);
        }
        let activity_x = x + left_width + 36.;
        canvas.text("最近操作", activity_x, bottom, 17, true, INK);
        if self.activity.is_empty() {
            canvas.text(
                "一切从按下开始出发。",
                activity_x,
                bottom + 61.,
                12,
                false,
                MUTED,
            );
            canvas.text(
                "你的每次操作会显示在这里。",
                activity_x,
                bottom + 84.,
                11,
                false,
                MUTED,
            );
        } else {
            for (index, (label, success)) in self.activity.iter().enumerate() {
                let y = bottom + 57. + index as f32 * 31.;
                if y > height - 75. {
                    break;
                }
                canvas.rect(
                    Rect::new(activity_x, y + 5., 5., 5.),
                    2.5,
                    if *success { SUCCESS } else { RED },
                );
                canvas.ellipsis(
                    label,
                    activity_x + 14.,
                    y,
                    w - left_width - 50.,
                    12,
                    false,
                    MUTED,
                );
            }
        }
    }

    fn stream_values(&self) -> (String, String) {
        let protocol = self.stream.get(&self.protocol).unwrap_or(&self.stream);
        let address = text(protocol, &["addr", "address", "server", "url"]);
        let key = text(protocol, &["code", "key", "stream_key"]);
        (address, key)
    }

    fn room_page(&mut self, canvas: &mut Canvas<'_>, ui: &mut Ui, x: f32, w: f32, height: f32) {
        let busy = self.pending > 0;
        let left = (w * 0.55).min(w - 300.);
        let right_x = x + left + 38.;
        let right = w - left - 38.;
        canvas.text("让观众一眼找到你。", x, 119., 13, false, MUTED);
        self.room_dirty |= field(
            canvas,
            ui,
            200,
            x,
            165.,
            left,
            "直播标题",
            &mut self.title,
            "写下这一场直播的主题",
            false,
        );
        if ui.button(
            canvas,
            211,
            Rect::new(x + left - 99., 157., 99., 30.),
            "最近标题",
            ButtonStyle::Quiet,
            true,
        ) {
            self.activate(Page::Titles);
        }
        self.room_dirty |= field(
            canvas,
            ui,
            201,
            x,
            258.,
            left,
            "直播间公告",
            &mut self.announcement,
            "向观众介绍你的直播",
            false,
        );
        self.room_dirty |= field(
            canvas,
            ui,
            202,
            x,
            351.,
            left,
            "分区 ID",
            &mut self.area_id,
            "从右侧选择，或输入分区 ID",
            false,
        );
        field(
            canvas,
            ui,
            203,
            x,
            444.,
            left,
            "封面图片",
            &mut self.cover_path,
            "图片文件的完整路径",
            false,
        );
        if ui.button(
            canvas,
            204,
            Rect::new(x, 535., 123., 39.),
            "预览与裁剪",
            ButtonStyle::Secondary,
            !self.cover_loading && !self.cover_path.is_empty(),
        ) {
            self.load_cover();
        }
        if ui.button(
            canvas,
            205,
            Rect::new(x + 138., 535., 135., 39.),
            "保存直播间",
            ButtonStyle::Primary,
            !busy && !self.demo,
        ) {
            let mut args = json!({"title": self.title, "announcement": self.announcement});
            if self.area_id.trim().is_empty() {
                self.submit("room.update", args);
            } else if let Ok(id) = self.area_id.trim().parse::<u64>() {
                if id > 0 {
                    args["area_id"] = id.into();
                    self.submit("room.update", args);
                } else {
                    self.notice("请选择有效的分区，或清空分区字段后保存。", false);
                }
            } else {
                self.notice("请选择有效的分区，或清空分区字段后保存。", false);
            }
        }
        canvas.ellipsis(
            if self.audit.is_empty() {
                "封面与标题的显示以平台审核结果为准。"
            } else {
                &self.audit
            },
            x,
            593.,
            left,
            11,
            false,
            MUTED,
        );
        let preview = Rect::new(x, 623., left.min(176.), left.min(176.) * 9. / 16.);
        canvas.rect(preview, 9., PANEL);
        if let Some(cover) = &self.cover {
            let (source, destination) = desktop::fit_image(cover, preview, "contain");
            desktop::paint_image(canvas, cover, source, destination, 255);
            canvas.text(
                if self.cover_local {
                    "本地预览"
                } else {
                    "当前封面"
                },
                x + preview.w + 18.,
                632.,
                12,
                true,
                INK,
            );
            if ui.button(
                canvas,
                212,
                Rect::new(x + preview.w + 12., 661., 100., 33.),
                "调整选区",
                ButtonStyle::Quiet,
                !self.cover_loading,
            ) {
                self.page = Page::Cover;
            }
        } else {
            canvas.text(
                if self.cover_loading {
                    "正在加载封面…"
                } else {
                    "尚未选择封面"
                },
                x + 14.,
                655.,
                12,
                false,
                MUTED,
            );
        }
        canvas.text("选择分区", right_x, 167., 17, true, INK);
        if ui.input(
            canvas,
            206,
            Rect::new(right_x, 205., right, 42.),
            &mut self.area_search,
            "搜索分区",
            false,
            true,
        ) {
            self.area_offset = 0;
        }
        if ui.button(
            canvas,
            213,
            Rect::new(right_x, 261., 82., 31.),
            "全部分区",
            if self.area_recent_only {
                ButtonStyle::Quiet
            } else {
                ButtonStyle::Primary
            },
            true,
        ) {
            self.area_recent_only = false;
            self.area_offset = 0;
        }
        if ui.button(
            canvas,
            214,
            Rect::new(right_x + 94., 261., 82., 31.),
            "最近开播",
            if self.area_recent_only {
                ButtonStyle::Primary
            } else {
                ButtonStyle::Quiet
            },
            true,
        ) {
            self.area_recent_only = true;
            self.area_offset = 0;
        }
        let query = self.area_search.to_lowercase();
        let source = if self.area_recent_only {
            &self.recent_areas
        } else {
            &self.areas
        };
        let filtered: Vec<(u64, String)> = source
            .iter()
            .filter(|(id, name)| {
                name.to_lowercase().contains(&query) || id.to_string().contains(&query)
            })
            .cloned()
            .collect();
        let rows = (((height - 404.) / 44.) as usize).max(1);
        self.area_offset = self.area_offset.min(filtered.len().saturating_sub(rows));
        for (index, (id, name)) in filtered
            .iter()
            .skip(self.area_offset)
            .take(rows)
            .enumerate()
        {
            let y = 308. + index as f32 * 44.;
            let selected = self.area_id == id.to_string();
            if selected {
                canvas.rect(Rect::new(right_x, y, right, 39.), 9., ACCENT_SOFT);
            }
            let label = shorten(name, 23);
            if ui.button(
                canvas,
                300 + index as u64,
                Rect::new(right_x, y, right, 39.),
                &label,
                ButtonStyle::Quiet,
                true,
            ) {
                self.area_id = id.to_string();
                self.room_dirty = true;
            }
        }
        if filtered.is_empty() {
            canvas.text(
                if busy {
                    "正在加载分区…"
                } else {
                    "没有匹配的分区"
                },
                right_x,
                325.,
                12,
                false,
                MUTED,
            );
        }
        if ui.button(
            canvas,
            208,
            Rect::new(right_x, height - 96., 100., 34.),
            "刷新分区",
            ButtonStyle::Quiet,
            !busy,
        ) {
            self.submit("area.list", json!({}));
            self.submit("area.recent", json!({}));
        }
        if ui.button(
            canvas,
            209,
            Rect::new(right_x + right - 80., height - 96., 36., 34.),
            "↑",
            ButtonStyle::Quiet,
            self.area_offset > 0,
        ) {
            self.area_offset = self.area_offset.saturating_sub(rows);
        }
        if ui.button(
            canvas,
            210,
            Rect::new(right_x + right - 39., height - 96., 36., 34.),
            "↓",
            ButtonStyle::Quiet,
            self.area_offset + rows < filtered.len(),
        ) {
            self.area_offset += rows;
        }
    }

    fn cover_page(&mut self, canvas: &mut Canvas<'_>, ui: &mut Ui, x: f32, w: f32, height: f32) {
        canvas.text(
            "拖动选区调整位置，在选区外拖动重新选取。",
            x,
            119.,
            13,
            false,
            MUTED,
        );
        if ui.button(
            canvas,
            1100,
            Rect::new(x + w - 105., 111., 105., 33.),
            "返回直播间",
            ButtonStyle::Quiet,
            !self.crop_busy,
        ) {
            self.page = Page::Room;
        }
        let panel = Rect::new(x, 164., w, (height - 340.).max(260.));
        canvas.rect(panel, 16., PANEL);
        if self.cover_loading {
            canvas.text("正在读取图片…", x + 24., 195., 14, false, MUTED);
        }
        if let Some(image) = self.cover.clone() {
            let (source, destination) = desktop::fit_image(&image, panel.inset(18.), "contain");
            desktop::paint_image(canvas, &image, source, destination, 255);
            self.crop_display = destination;
            let sx = destination.w / image.width() as f32;
            let sy = destination.h / image.height() as f32;
            let selected = Rect::new(
                destination.x + self.crop.x as f32 * sx,
                destination.y + self.crop.y as f32 * sy,
                self.crop.width as f32 * sx,
                self.crop.height as f32 * sy,
            );
            for shade in [
                Rect::new(
                    destination.x,
                    destination.y,
                    destination.w,
                    (selected.y - destination.y).max(0.),
                ),
                Rect::new(
                    destination.x,
                    selected.y + selected.h,
                    destination.w,
                    (destination.y + destination.h - selected.y - selected.h).max(0.),
                ),
                Rect::new(
                    destination.x,
                    selected.y,
                    (selected.x - destination.x).max(0.),
                    selected.h,
                ),
                Rect::new(
                    selected.x + selected.w,
                    selected.y,
                    (destination.x + destination.w - selected.x - selected.w).max(0.),
                    selected.h,
                ),
            ] {
                canvas.rect(shade, 0., 0x00000088);
            }
            for (a, b) in [
                (
                    (selected.x, selected.y),
                    (selected.x + selected.w, selected.y),
                ),
                (
                    (selected.x, selected.y + selected.h),
                    (selected.x + selected.w, selected.y + selected.h),
                ),
                (
                    (selected.x, selected.y),
                    (selected.x, selected.y + selected.h),
                ),
                (
                    (selected.x + selected.w, selected.y),
                    (selected.x + selected.w, selected.y + selected.h),
                ),
            ] {
                canvas.line(a, b, 2., ACCENT);
            }
            for part in [1., 2.] {
                canvas.line(
                    (selected.x + selected.w * part / 3., selected.y),
                    (selected.x + selected.w * part / 3., selected.y + selected.h),
                    1.,
                    0xffffff55,
                );
                canvas.line(
                    (selected.x, selected.y + selected.h * part / 3.),
                    (selected.x + selected.w, selected.y + selected.h * part / 3.),
                    1.,
                    0xffffff55,
                );
            }
            let controls = height - 151.;
            if ui.button(
                canvas,
                1101,
                Rect::new(x, controls, 111., 39.),
                "居中选取",
                ButtonStyle::Secondary,
                !self.crop_busy,
            ) {
                self.crop = CropRect::centered(&image);
            }
            if ui.button(
                canvas,
                1102,
                Rect::new(x + 123., controls, 97., 39.),
                "缩小选区",
                ButtonStyle::Quiet,
                !self.crop_busy,
            ) {
                self.crop = self.crop.scaled(&image, 0.9);
            }
            if ui.button(
                canvas,
                1103,
                Rect::new(x + 232., controls, 97., 39.),
                "放大选区",
                ButtonStyle::Quiet,
                !self.crop_busy,
            ) {
                self.crop = self.crop.scaled(&image, 1.1);
            }
            if ui.button(
                canvas,
                1104,
                Rect::new(x + w - 150., controls, 150., 39.),
                if self.crop_busy {
                    "正在上传…"
                } else {
                    "上传裁剪封面"
                },
                ButtonStyle::Primary,
                !self.demo
                    && !self.crop_busy
                    && !self.cover_loading
                    && self.pending == 0
                    && self.crop.valid(&image),
            ) {
                self.image_serial += 1;
                self.crop_image_id = self.image_serial;
                self.crop_busy = self.image_request(AssetRequest::Crop {
                    id: self.crop_image_id,
                    image: image.clone(),
                    crop: self.crop,
                    directory: self.directory.join("cache/covers"),
                });
            }
            canvas.text(
                &format!(
                    "选区 {} × {}   ·   输出 704 × 396 PNG",
                    self.crop.width, self.crop.height
                ),
                x,
                height - 91.,
                12,
                false,
                MUTED,
            );
            canvas.text(
                "上传后等待平台审核，原图片保持不变。",
                x + w - 258.,
                height - 91.,
                11,
                false,
                MUTED,
            );
        } else if !self.cover_loading {
            canvas.text(
                "请返回直播间设置，选择一张 PNG、JPEG 或 WebP 图片。",
                x + 24.,
                208.,
                13,
                false,
                MUTED,
            );
        }
    }

    fn titles_page(&mut self, canvas: &mut Canvas<'_>, ui: &mut Ui, x: f32, w: f32) {
        canvas.text(
            "当前账号最近成功保存的五个标题。",
            x,
            119.,
            13,
            false,
            MUTED,
        );
        if ui.button(
            canvas,
            1120,
            Rect::new(x + w - 105., 111., 105., 33.),
            "返回直播间",
            ButtonStyle::Quiet,
            true,
        ) {
            self.page = Page::Room;
        }
        for (index, title) in self.titles.clone().iter().enumerate() {
            let y = 171. + index as f32 * 80.;
            canvas.rect(Rect::new(x, y, w, 64.), 12., PANEL);
            canvas.ellipsis(title, x + 20., y + 22., w - 132., 15, false, INK);
            if ui.button(
                canvas,
                1121 + index as u64,
                Rect::new(x + w - 95., y + 13., 76., 38.),
                "使用",
                ButtonStyle::Secondary,
                true,
            ) {
                self.title = title.clone();
                self.room_dirty = true;
                self.page = Page::Room;
            }
        }
        if self.titles.is_empty() {
            canvas.text(
                "保存一个直播标题后，它会出现在这里。",
                x,
                199.,
                14,
                false,
                MUTED,
            );
        }
    }

    fn appearance_page(&mut self, canvas: &mut Canvas<'_>, ui: &mut Ui, x: f32, w: f32) {
        let busy = self.pending > 0;
        canvas.text("让界面适合你的桌面与工作习惯。", x, 119., 13, false, MUTED);
        if ui.button(
            canvas,
            1140,
            Rect::new(x + w - 103., 111., 103., 33.),
            "返回设置",
            ButtonStyle::Quiet,
            true,
        ) {
            self.page = Page::Settings;
        }
        for (index, name) in ["显示外观", "托盘与窗口"].into_iter().enumerate() {
            if ui.button(
                canvas,
                1141 + index as u64,
                Rect::new(x + index as f32 * 142., 167., 130., 37.),
                name,
                if self.appearance_section == index as u8 {
                    ButtonStyle::Primary
                } else {
                    ButtonStyle::Quiet
                },
                true,
            ) {
                self.appearance_section = index as u8;
            }
        }
        if self.appearance_section == 0 {
            field(
                canvas,
                ui,
                1143,
                x,
                236.,
                w,
                "字体家族",
                &mut self.font_family,
                "留空使用系统默认；也可填写 monospace 或已安装字体名",
                false,
            );
            field(
                canvas,
                ui,
                1144,
                x,
                328.,
                w,
                "背景图片",
                &mut self.background_path,
                "本地 PNG / JPEG / WebP 文件；留空使用纯色背景",
                false,
            );
            canvas.text("图片布局", x, 422., 12, false, MUTED);
            for (index, (name, label)) in [
                ("native", "原始尺寸"),
                ("stretch", "拉伸填充"),
                ("cover", "等比填充"),
                ("contain", "等比适应"),
            ]
            .into_iter()
            .enumerate()
            {
                if ui.button(
                    canvas,
                    1145 + index as u64,
                    Rect::new(x + index as f32 * 120., 450., 108., 35.),
                    label,
                    if self.background_mode == name {
                        ButtonStyle::Primary
                    } else {
                        ButtonStyle::Quiet
                    },
                    true,
                ) {
                    self.background_mode = name.into();
                }
            }
            field(
                canvas,
                ui,
                1149,
                x,
                518.,
                180.,
                "背景不透明度（0–100）",
                &mut self.background_opacity,
                "15",
                false,
            );
            field(
                canvas,
                ui,
                1150,
                x + 210.,
                518.,
                180.,
                "模糊半径（0–40）",
                &mut self.background_blur,
                "0",
                false,
            );
            if ui.button(
                canvas,
                1151,
                Rect::new(x, 625., 128., 41.),
                "应用外观",
                ButtonStyle::Primary,
                !busy && !self.demo,
            ) {
                match (
                    self.background_opacity.parse::<u64>(),
                    self.background_blur.parse::<u64>(),
                ) {
                    (Ok(opacity), Ok(blur)) if opacity <= 100 && blur <= 40 => {
                        self.set("font_family", json!(self.font_family.trim()));
                        self.set("background_image", json!(self.background_path.trim()));
                        self.set("background_mode", json!(self.background_mode));
                        self.set("background_opacity", json!(opacity));
                        self.set("background_blur", json!(blur));
                    }
                    _ => self.notice("不透明度应为 0–100，模糊半径应为 0–40。", false),
                }
            }
            canvas.text(
                "字体与背景在保存后立即生效。",
                x + 151.,
                639.,
                12,
                false,
                MUTED,
            );
        } else {
            toggle(
                canvas,
                ui,
                1152,
                Rect::new(x, 232., w, 44.),
                "显示系统托盘图标",
                &mut self.tray_enabled,
            );
            toggle(
                canvas,
                ui,
                1153,
                Rect::new(x, 288., w, 44.),
                "关闭窗口后保留到托盘",
                &mut self.close_to_tray,
            );
            toggle(
                canvas,
                ui,
                1154,
                Rect::new(x, 344., w, 44.),
                "同一配置只打开一个窗口",
                &mut self.single_instance,
            );
            field(
                canvas,
                ui,
                1155,
                x,
                418.,
                w,
                "托盘图标",
                &mut self.tray_path,
                "本地图片路径；留空使用 bilibilive 原生图标",
                false,
            );
            field(
                canvas,
                ui,
                1156,
                x,
                512.,
                w,
                "托盘提示",
                &mut self.tray_tooltip,
                "bilibilive",
                false,
            );
            if ui.button(
                canvas,
                1157,
                Rect::new(x, 625., 128., 41.),
                "保存桌面设置",
                ButtonStyle::Primary,
                !busy && !self.demo,
            ) {
                self.set("tray_enabled", json!(self.tray_enabled));
                self.set("close_to_tray", json!(self.close_to_tray));
                self.set("single_instance", json!(self.single_instance));
                self.set("tray_icon", json!(self.tray_path.trim()));
                self.set("tray_tooltip", json!(self.tray_tooltip.trim()));
            }
            canvas.text(
                "单实例设置在下次启动时生效。",
                x + 151.,
                639.,
                12,
                false,
                MUTED,
            );
        }
        if ui.button(
            canvas,
            1158,
            Rect::new(x + w - 111., 687., 111., 33.),
            "恢复默认外观",
            ButtonStyle::Quiet,
            !busy && !self.demo,
        ) {
            self.submit("config.reset", json!({"section":"appearance"}));
        }
    }

    fn accounts_page(&mut self, canvas: &mut Canvas<'_>, ui: &mut Ui, x: f32, w: f32, height: f32) {
        let busy = self.pending > 0;
        let left = w * 0.5 - 20.;
        let right_x = x + left + 40.;
        let right = w - left - 40.;
        canvas.text("一个入口，管理你的直播账号。", x, 119., 13, false, MUTED);
        field(
            canvas,
            ui,
            400,
            x,
            166.,
            left,
            "账号名称",
            &mut self.account_name,
            "例如：日常直播",
            false,
        );
        if ui.button(
            canvas,
            401,
            Rect::new(x, 258., 148., 40.),
            "生成登录二维码",
            ButtonStyle::Primary,
            !busy && !self.demo,
        ) {
            self.submit("auth.qr.create", json!({}));
        }
        if let Some(qr) = &self.qr {
            draw_qr(canvas, qr, x, 322., 194.);
            canvas.ellipsis(&self.qr_message, x, 531., left, 12, false, MUTED);
            if ui.button(
                canvas,
                402,
                Rect::new(x + 207., 384., (left - 207.).max(70.), 36.),
                "复制链接",
                ButtonStyle::Quiet,
                !self.qr_url.is_empty(),
            ) {
                self.copy = Some(self.qr_url.clone());
            }
        } else {
            canvas.rect(Rect::new(x, 325., left, 138.), 15., PANEL);
            canvas.text("在你的手机上完成登录", x + 23., 351., 16, true, INK);
            canvas.text(
                "生成二维码后，使用哔哩哔哩 App 扫码。",
                x + 23.,
                389.,
                12,
                false,
                MUTED,
            );
            canvas.text("登录状态保存在本机。", x + 23., 416., 12, false, MUTED);
        }
        field(
            canvas,
            ui,
            403,
            x,
            572.,
            left,
            "已有 Cookie",
            &mut self.cookie,
            "粘贴登录 Cookie",
            true,
        );
        if ui.button(
            canvas,
            404,
            Rect::new(x, 661., 116., 37.),
            "导入账号",
            ButtonStyle::Secondary,
            !busy && !self.cookie.is_empty() && !self.demo,
        ) {
            self.submit(
                "auth.import",
                json!({"name": self.account_name, "cookie": self.cookie}),
            );
        }
        canvas.text("已保存账号", right_x, 168., 17, true, INK);
        let accounts = self.account_rows();
        let rows = (((height - 320.) / 109.) as usize).max(1);
        self.account_offset = self.account_offset.min(accounts.len().saturating_sub(rows));
        for (index, account) in accounts
            .iter()
            .skip(self.account_offset)
            .take(rows)
            .enumerate()
        {
            let y = 211. + index as f32 * 109.;
            let name = text(account, &["name", "uname"]);
            let uname = text(account, &["uname", "name"]);
            let active = account
                .get("active")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || text(&self.accounts, &["active"]) == name;
            canvas.rect(Rect::new(right_x, y, right, 94.), 14., PANEL);
            canvas.ellipsis(&uname, right_x + 17., y + 14., right - 112., 15, true, INK);
            canvas.ellipsis(
                &name,
                right_x + 17.,
                y + 39.,
                right - 130.,
                11,
                false,
                MUTED,
            );
            if active {
                pill(
                    canvas,
                    right_x + right - 88.,
                    y + 12.,
                    "使用中",
                    SUCCESS,
                    0xe6f4eaff,
                );
            }
            if ui.button(
                canvas,
                410 + index as u64 * 2,
                Rect::new(right_x + 9., y + 58., 73., 28.),
                "切换",
                ButtonStyle::Quiet,
                !busy && !active && !self.demo,
            ) {
                self.submit("account.use", json!({"name": name}));
            }
            if ui.button(
                canvas,
                411 + index as u64 * 2,
                Rect::new(right_x + right - 80., y + 58., 69., 28.),
                "移除",
                ButtonStyle::Quiet,
                !busy && !self.demo,
            ) {
                self.submit("account.remove", json!({"name": name}));
            }
        }
        if accounts.is_empty() {
            canvas.text("还没有保存的账号。", right_x, 219., 13, false, MUTED);
        }
    }

    fn obs_page(&mut self, canvas: &mut Canvas<'_>, ui: &mut Ui, x: f32, w: f32) {
        let busy = self.pending > 0;
        let connected = self
            .obs
            .get("connected")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let streaming = self
            .obs
            .get("outputActive")
            .or_else(|| self.obs.get("streaming"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        canvas.text("让开播、推流与收播保持同步。", x, 119., 13, false, MUTED);
        canvas.rect(Rect::new(x, 163., w, 91.), 16., PANEL);
        canvas.text(
            if connected {
                "OBS 已连接"
            } else {
                "等待 OBS 连接"
            },
            x + 23.,
            183.,
            19,
            true,
            INK,
        );
        canvas.text(
            if streaming {
                "正在推流"
            } else {
                "开启 OBS 的 WebSocket 服务器后连接。"
            },
            x + 23.,
            217.,
            12,
            false,
            MUTED,
        );
        if ui.button(
            canvas,
            500,
            Rect::new(x + w - 125., 186., 102., 40.),
            "检查连接",
            ButtonStyle::Secondary,
            !busy,
        ) {
            self.submit("obs.status", json!({}));
        }
        let half = (w - 24.) / 2.;
        field(
            canvas,
            ui,
            501,
            x,
            291.,
            half,
            "WebSocket 地址",
            &mut self.obs_url,
            "ws://127.0.0.1:4455",
            false,
        );
        field(
            canvas,
            ui,
            502,
            x + half + 24.,
            291.,
            half,
            "WebSocket 密码",
            &mut self.obs_password,
            "留空以保留已保存的密码",
            true,
        );
        if ui.button(
            canvas,
            503,
            Rect::new(x, 382., 119., 39.),
            "保存连接",
            ButtonStyle::Primary,
            !busy && !self.demo,
        ) {
            let mut args = json!({"url": self.obs_url});
            if !self.obs_password.is_empty() {
                args["password"] = self.obs_password.clone().into();
            }
            self.submit("obs.configure", args);
        }
        canvas.text("开播联动", x, 460., 17, true, INK);
        toggle(
            canvas,
            ui,
            504,
            Rect::new(x, 497., half, 45.),
            "开播后启动 OBS 推流",
            &mut self.obs_auto_start,
        );
        toggle(
            canvas,
            ui,
            505,
            Rect::new(x + half + 24., 497., half, 45.),
            "收播后停止 OBS 推流",
            &mut self.obs_auto_stop,
        );
        toggle(
            canvas,
            ui,
            510,
            Rect::new(x, 553., half, 45.),
            "自动保持 OBS 连接",
            &mut self.obs_auto_connect,
        );
        toggle(
            canvas,
            ui,
            511,
            Rect::new(x + half + 24., 553., half, 45.),
            "OBS 推流时联动开播",
            &mut self.obs_link,
        );
        if ui.button(
            canvas,
            506,
            Rect::new(x, 619., 115., 39.),
            "保存联动",
            ButtonStyle::Secondary,
            !busy && !self.demo,
        ) {
            self.set("obs_auto_start", self.obs_auto_start.into());
            self.set("obs_auto_stop", self.obs_auto_stop.into());
            self.set("obs_auto_connect", self.obs_auto_connect.into());
            self.set("obs_link", self.obs_link.into());
        }
        if ui.button(
            canvas,
            507,
            Rect::new(x + 129., 619., 115., 39.),
            "同步推流配置",
            ButtonStyle::Secondary,
            !busy && !self.demo,
        ) {
            self.submit("obs.sync", json!({}));
        }
        if ui.button(
            canvas,
            508,
            Rect::new(x + 258., 619., 109., 39.),
            "启动推流",
            ButtonStyle::Quiet,
            !busy && !self.demo,
        ) {
            self.submit("obs.start", json!({}));
        }
        if ui.button(
            canvas,
            509,
            Rect::new(x + 374., 619., 109., 39.),
            "停止推流",
            ButtonStyle::Quiet,
            !busy && !self.demo,
        ) {
            self.submit("obs.stop", json!({}));
        }
    }

    fn pk_page(&mut self, canvas: &mut Canvas<'_>, ui: &mut Ui, x: f32, w: f32) {
        let busy = self.pending > 0;
        canvas.text("与另一位主播，把现场连在一起。", x, 119., 13, false, MUTED);
        let state = text(&self.pk, &["state", "status"]);
        canvas.rect(Rect::new(x, 162., w, 96.), 17., PANEL);
        canvas.text(
            if state.is_empty() {
                "准备连接"
            } else {
                &state
            },
            x + 24.,
            183.,
            21,
            true,
            INK,
        );
        let opponent = text(&self.pk, &["opponent", "opponent_name"]);
        canvas.text(
            if opponent.is_empty() {
                "匹配对手，或向指定主播发起邀请。"
            } else {
                &opponent
            },
            x + 24.,
            223.,
            12,
            false,
            MUTED,
        );
        if ui.button(
            canvas,
            600,
            Rect::new(x + w - 116., 188., 92., 39.),
            "刷新状态",
            ButtonStyle::Secondary,
            !busy,
        ) {
            self.submit("pk.status", json!({}));
        }
        canvas.text("匹配模式", x, 294., 13, false, MUTED);
        for (i, (value, label)) in [
            ("classic", "普通 PK"),
            ("video", "视频 PK"),
            ("video_link", "视频连线"),
        ]
        .iter()
        .enumerate()
        {
            let rect = Rect::new(x + i as f32 * 124., 322., 112., 38.);
            if ui.button(
                canvas,
                601 + i as u64,
                rect,
                label,
                if self.pk_mode == *value {
                    ButtonStyle::Primary
                } else {
                    ButtonStyle::Secondary
                },
                true,
            ) {
                self.pk_mode = value.to_string();
            }
        }
        if ui.button(
            canvas,
            605,
            Rect::new(x + w - 232., 322., 108., 38.),
            "开始匹配",
            ButtonStyle::Primary,
            !busy && !self.demo,
        ) {
            self.submit("pk.match", json!({"mode": self.pk_mode}));
        }
        if ui.button(
            canvas,
            606,
            Rect::new(x + w - 112., 322., 112., 38.),
            "取消匹配",
            ButtonStyle::Quiet,
            !busy && !self.demo,
        ) {
            self.submit("pk.cancel", json!({"mode": self.pk_mode}));
        }
        let half = (w - 24.) / 2.;
        field(
            canvas,
            ui,
            607,
            x,
            401.,
            half,
            "对方主播 UID",
            &mut self.opponent_uid,
            "邀请或接受连线的主播 UID",
            false,
        );
        field(
            canvas,
            ui,
            608,
            x + half + 24.,
            401.,
            half - 112.,
            "连线会话 ID",
            &mut self.session_id,
            "来自邀请事件",
            false,
        );
        field(
            canvas,
            ui,
            615,
            x + w - 96.,
            401.,
            96.,
            "邀请来源",
            &mut self.invitation_source,
            "source",
            false,
        );
        if ui.button(
            canvas,
            609,
            Rect::new(x, 492., 109., 39.),
            "发送邀请",
            ButtonStyle::Secondary,
            !busy && !self.opponent_uid.is_empty() && !self.demo,
        ) {
            match self.opponent_uid.parse::<u64>() {
                Ok(uid) if uid > 0 => self.submit("pk.invite", json!({"uid": uid})),
                _ => self.notice("请输入有效的主播 UID。", false),
            }
        }
        let has_invitation = !self.session_id.is_empty()
            && !self.opponent_uid.is_empty()
            && !self.invitation_source.is_empty();
        if ui.button(
            canvas,
            610,
            Rect::new(x + half + 24., 492., 102., 39.),
            "接受连线",
            ButtonStyle::Secondary,
            !busy && has_invitation && !self.demo,
        ) {
            self.handle_invitation(true);
        }
        if ui.button(
            canvas,
            611,
            Rect::new(x + half + 137., 492., 92., 39.),
            "拒绝",
            ButtonStyle::Quiet,
            !busy && has_invitation && !self.demo,
        ) {
            self.handle_invitation(false);
        }
        if ui.button(
            canvas,
            612,
            Rect::new(x + half + 241., 492., 110., 39.),
            "结束 PK",
            ButtonStyle::Danger,
            !busy && !self.demo,
        ) {
            self.submit("pk.end", json!({}));
        }
        if ui.button(
            canvas,
            616,
            Rect::new(x + 121., 492., 115., 39.),
            if self.events_connected {
                "停止接收邀请"
            } else {
                "接收主播邀请"
            },
            ButtonStyle::Quiet,
            !busy && !self.demo,
        ) {
            if self.events_connected {
                self.events_connected = false;
                self.submit("danmaku.disconnect", json!({}));
            } else {
                self.submit("danmaku.connect", json!({}));
            }
        }
        if ui.button(
            canvas,
            617,
            Rect::new(x + 246., 492., 110., 39.),
            "断开连线",
            ButtonStyle::Quiet,
            !busy && !self.session_id.is_empty() && !self.demo,
        ) {
            self.submit(
                "pk.link.disconnect",
                json!({"session_id": self.session_id, "end_round": true}),
            );
        }
        let automatic = self
            .config
            .get("auto_pk_media")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if ui.button(
            canvas,
            618,
            Rect::new(x, 542., 158., 30.),
            if automatic {
                "原生媒体 · 自动连接"
            } else {
                "原生媒体 · 手动连接"
            },
            ButtonStyle::Quiet,
            !busy && !self.demo,
        ) {
            self.set("auto_pk_media", json!(!automatic));
        }
        for (index, (label, name)) in [
            ("PK 偏好", "pk.settings.get"),
            ("连续 PK", "pk.continuous.set"),
            ("媒体控制", "rtc.status"),
        ]
        .iter()
        .enumerate()
        {
            if ui.button(
                canvas,
                619 + index as u64,
                Rect::new(x + 176. + index as f32 * 114., 542., 104., 30.),
                label,
                ButtonStyle::Quiet,
                true,
            ) {
                self.advanced_name = name.to_string();
                self.advanced_args = if *name == "pk.continuous.set" {
                    "{\"enabled\":true,\"mode\":\"classic\"}".into()
                } else {
                    "{}".into()
                };
                self.advanced_search = if name.starts_with("pk.") {
                    "pk.".into()
                } else {
                    "rtc.".into()
                };
                self.advanced_offset = 0;
                self.advanced_output.clear();
                self.advanced_show_args = false;
                self.activate(Page::Advanced);
            }
        }
        field(
            canvas,
            ui,
            613,
            x,
            579.,
            w - 126.,
            "直播间弹幕",
            &mut self.danmaku,
            "向直播间发送一条弹幕",
            false,
        );
        if ui.button(
            canvas,
            614,
            Rect::new(x + w - 111., 606., 111., 43.),
            "发送",
            ButtonStyle::Primary,
            !busy && !self.danmaku.is_empty() && !self.demo,
        ) {
            self.submit("danmaku.send", json!({"message": self.danmaku}));
        }
    }

    fn automation_page(
        &mut self,
        canvas: &mut Canvas<'_>,
        ui: &mut Ui,
        x: f32,
        w: f32,
        height: f32,
    ) {
        let busy = self.pending > 0;
        canvas.text(
            "把常用的直播操作，写成可重复执行的步骤。",
            x,
            119.,
            13,
            false,
            MUTED,
        );
        canvas.rect(Rect::new(x, 164., w, 107.), 16., PANEL);
        canvas.text("zuse", x + 23., 184., 23, true, ACCENT);
        canvas.text(
            "与命令行、MCP 共用同一组直播操作。",
            x + 23.,
            225.,
            13,
            false,
            MUTED,
        );
        if ui.button(
            canvas,
            700,
            Rect::new(x + w - 130., 196., 106., 40.),
            "查看命令",
            ButtonStyle::Secondary,
            !busy,
        ) {
            self.submit("zuse.schema", json!({}));
        }
        field(
            canvas,
            ui,
            701,
            x,
            310.,
            w,
            "运行脚本",
            &mut self.script,
            "bilibilive.status()",
            false,
        );
        if ui.button(
            canvas,
            702,
            Rect::new(x, 403., 115., 40.),
            "运行",
            ButtonStyle::Primary,
            !busy && !self.script.is_empty() && !self.demo,
        ) {
            self.submit("zuse.run", json!({"script": self.script}));
        }
        canvas.text(
            "脚本中的开播、发弹幕等操作会立即执行。",
            x + 132.,
            417.,
            12,
            false,
            MUTED,
        );
        canvas.text("执行结果", x, 486., 16, true, INK);
        let box_height = (height - 585.).max(96.);
        canvas.rect(Rect::new(x, 522., w, box_height), 12., PANEL);
        let output = if self.script_output.is_empty() {
            "运行后，这里将显示每一步的结果。"
        } else {
            &self.script_output
        };
        let lines = ((box_height - 24.) / 21.) as usize;
        for (index, line) in output.lines().take(lines).enumerate() {
            canvas.ellipsis(
                line,
                x + 16.,
                537. + index as f32 * 21.,
                w - 32.,
                12,
                false,
                MUTED,
            );
        }
        if !self.script_output.is_empty()
            && ui.button(
                canvas,
                703,
                Rect::new(x + w - 98., 479., 98., 34.),
                "复制结果",
                ButtonStyle::Quiet,
                true,
            )
        {
            self.copy = Some(self.script_output.clone());
        }
    }

    fn settings_page(&mut self, canvas: &mut Canvas<'_>, ui: &mut Ui, x: f32, w: f32) {
        let busy = self.pending > 0;
        canvas.text(
            "保留熟悉的习惯，把重复的事情交给工具。",
            x,
            119.,
            13,
            false,
            MUTED,
        );
        canvas.text("推流协议", x, 176., 16, true, INK);
        for (index, (value, label)) in [("rtmp", "RTMP"), ("srt", "SRT")].into_iter().enumerate() {
            if ui.button(
                canvas,
                800 + index as u64,
                Rect::new(x + index as f32 * 127., 214., 114., 41.),
                label,
                if self.protocol == value {
                    ButtonStyle::Primary
                } else {
                    ButtonStyle::Secondary
                },
                true,
            ) {
                self.protocol = value.into();
            }
        }
        canvas.text("推流地址策略", x, 302., 16, true, INK);
        for (index, (value, label)) in [("fallback", "允许 RTMP 回退"), ("only", "仅使用所选协议")]
            .into_iter()
            .enumerate()
        {
            if ui.button(
                canvas,
                810 + index as u64,
                Rect::new(x + index as f32 * 172., 340., 159., 41.),
                label,
                if self.strategy == value {
                    ButtonStyle::Primary
                } else {
                    ButtonStyle::Secondary
                },
                true,
            ) {
                self.strategy = value.into();
            }
        }
        let half = (w - 24.) / 2.;
        field(
            canvas,
            ui,
            820,
            x,
            428.,
            half,
            "默认分区 ID",
            &mut self.area_id,
            "开播默认选择的分区",
            false,
        );
        field(
            canvas,
            ui,
            821,
            x + half + 24.,
            428.,
            half,
            "推流延迟（秒）",
            &mut self.delay,
            "0",
            false,
        );
        if ui.button(
            canvas,
            822,
            Rect::new(x, 530., 132., 41.),
            "保存偏好",
            ButtonStyle::Primary,
            !busy && !self.demo,
        ) {
            match (self.area_id.parse::<u64>(), self.delay.parse::<u64>()) {
                (Ok(area), Ok(delay)) => {
                    self.set("protocol", self.protocol.clone().into());
                    self.set("strategy", self.strategy.clone().into());
                    self.set("default_area", area.into());
                    self.set("delay", delay.into());
                }
                _ => self.notice("默认分区和延迟需要填写有效的非负整数。", false),
            }
        }
        if ui.button(
            canvas,
            823,
            Rect::new(x + 148., 530., 146., 41.),
            "应用直播延迟",
            ButtonStyle::Secondary,
            !busy && !self.demo,
        ) {
            match self.delay.parse::<u64>() {
                Ok(seconds) => self.submit("delay.set", json!({"seconds": seconds})),
                Err(_) => self.notice("延迟需要填写有效的非负整数。", false),
            }
        }
        if ui.button(
            canvas,
            824,
            Rect::new(x + w - 150., 530., 150., 41.),
            "打开完整操作",
            ButtonStyle::Quiet,
            true,
        ) {
            self.activate(Page::Advanced);
        }
        canvas.text("外观", x, 588., 12, false, MUTED);
        let light = self.config.get("theme").and_then(Value::as_str) == Some("light");
        if ui.button(
            canvas,
            825,
            Rect::new(x + 51., 580., 83., 31.),
            "深色",
            if light {
                ButtonStyle::Quiet
            } else {
                ButtonStyle::Primary
            },
            !busy && !self.demo,
        ) {
            self.set("theme", json!("dark"));
        }
        if ui.button(
            canvas,
            826,
            Rect::new(x + 145., 580., 83., 31.),
            "浅色",
            if light {
                ButtonStyle::Primary
            } else {
                ButtonStyle::Quiet
            },
            !busy && !self.demo,
        ) {
            self.set("theme", json!("light"));
        }
        if ui.button(
            canvas,
            827,
            Rect::new(x + w - 150., 578., 150., 34.),
            "字体、背景与托盘",
            ButtonStyle::Quiet,
            true,
        ) {
            self.activate(Page::Appearance);
        }
        canvas.line((x, 622.), (x + w, 622.), 1., HAIRLINE);
        canvas.text("bilibilive", x, 650., 15, true, INK);
        canvas.text(
            concat!("版本 ", env!("CARGO_PKG_VERSION")),
            x,
            677.,
            11,
            false,
            MUTED,
        );
        if ui.button(
            canvas,
            828,
            Rect::new(x + 184., 643., 148., 39.),
            "更新与组件",
            ButtonStyle::Secondary,
            true,
        ) {
            self.activate(Page::Components);
        }
        canvas.text(
            "用 Rust 与 Project Z 构建。",
            x + w - 196.,
            650.,
            12,
            false,
            MUTED,
        );
        canvas.text(
            "自由、开源，专注你的每一场直播。",
            x + w - 196.,
            677.,
            11,
            false,
            MUTED,
        );
    }

    fn set(&mut self, key: &str, value: Value) {
        self.submit("config.set", json!({"key": key, "value": value}));
    }

    fn request_update(&mut self, method: &str) -> bool {
        if self.demo {
            self.notice("演示模式不会检查网络或安装更新。", false);
            return false;
        }
        let args = match method {
            "updates.check" | "updates.status" => json!({}),
            "updates.download" | "updates.apply" => {
                let capability = if method == "updates.download" {
                    "can_download"
                } else {
                    "can_apply"
                };
                let staged_id = text(&self.updates, &["staged_id"]);
                if self.read_only
                    || self.pending > 0
                    || self.updates[capability] != true
                    || staged_id.is_empty()
                {
                    self.notice("请先检查版本，确认当前安装方式与更新状态。", false);
                    return false;
                }
                if method == "updates.download" {
                    json!({"staged_id":staged_id,"wait":false})
                } else {
                    json!({"staged_id":staged_id,"restart":true})
                }
            }
            _ => return false,
        };
        let submitted = self.submit_cleanup(method, args, None);
        if submitted {
            self.update_poll = Instant::now();
            if method == "updates.check" {
                self.startup_update_handled = true;
            }
        }
        submitted
    }

    fn install_plugin(&mut self) -> bool {
        if self.demo || self.read_only || self.pending > 0 || self.plugin["can_install"] != true {
            self.notice("当前模式或随包组件尚不支持安装。请查看组件状态。", false);
            return false;
        }
        self.submit_cleanup(
            "obs.plugin.install",
            json!({"replace":self.plugin_replace}),
            None,
        )
    }

    fn component_details(&mut self, method: &str, result: &Value) {
        self.advanced_name = method.into();
        self.advanced_args = "{}".into();
        self.advanced_output = serde_json::to_string_pretty(result).unwrap_or_default();
        self.activate(Page::Advanced);
    }

    fn components_page(&mut self, canvas: &mut Canvas<'_>, ui: &mut Ui, x: f32, w: f32) {
        let busy = self.pending > 0;
        let mutable = !busy && !self.demo && !self.read_only;
        canvas.text(
            "选择适合当前安装方式的更新，保持工作空间从容。",
            x,
            120.,
            13,
            false,
            MUTED,
        );
        if ui.button(
            canvas,
            1300,
            Rect::new(x + w - 116., 108., 116., 34.),
            "返回偏好设置",
            ButtonStyle::Quiet,
            true,
        ) {
            self.activate(Page::Settings);
        }

        canvas.rect(Rect::new(x, 164., w, 300.), 18., PANEL);
        canvas.text("bilibilive", x + 24., 187., 18, true, INK);
        let kind = self
            .updates
            .pointer("/installation/kind")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let channel = match kind {
            "velopack" => "安装器管理",
            "cargo" => "Cargo 安装",
            "aur" => "Arch 包管理",
            "portable" => "便携版本",
            "demo" => "离线演示",
            _ => "安装来源待确认",
        };
        pill(canvas, x + w - 160., 185., channel, MUTED, BG);
        let current = text(&self.updates, &["current"]);
        let current = if current.is_empty() {
            env!("CARGO_PKG_VERSION")
        } else {
            &current
        };
        let latest = text(&self.updates, &["latest"]);
        canvas.ellipsis(
            &format!(
                "当前 {current}  ·  最新 {}",
                if latest.is_empty() {
                    "尚未检查"
                } else {
                    &latest
                }
            ),
            x + 24.,
            222.,
            w - 48.,
            12,
            false,
            MUTED,
        );
        let phase = text(&self.updates, &["phase"]);
        let status = if self.update_pending == "updates.check" {
            "正在检查最新稳定版本…"
        } else if self.demo {
            "演示模式 · 所有下载与安装操作均已关闭"
        } else if self.local_sessions_retired && !self.quit_requested {
            "本地媒体连接已停止；更新未完成，直播间状态保留"
        } else if self.local_shutdown_attempted && !self.quit_requested {
            "本地会话关闭未完成；更新未应用，请查看错误"
        } else {
            match phase.as_str() {
                "available" => "发现新版本，可选择下载。",
                "downloading" => "正在下载经过校验的安装包…",
                "downloaded" => "更新已准备好，点击应用后将退出并重新打开。",
                "up_to_date" => "当前无需更新。",
                "checking" => "正在检查最新稳定版本…",
                "failed" => "更新未完成。请查看错误后重新检查。",
                "manual" => "请使用当前安装渠道提供的命令完成更新。",
                "restart_pending" => "正在退出并应用更新…",
                _ => "检查版本后选择下一步。下载与应用均由你确认。",
            }
        };
        canvas.ellipsis(status, x + 24., 254., w - 48., 13, true, INK);
        let command = text(&self.updates, &["manual_command"]);
        let release_url = text(&self.updates, &["url"]);
        if phase == "downloading" {
            let progress = self.updates["progress"]
                .as_f64()
                .unwrap_or(0.)
                .clamp(0., 100.) as f32;
            canvas.rect(Rect::new(x + 24., 282., w - 112., 5.), 2.5, HAIRLINE);
            canvas.rect(
                Rect::new(x + 24., 282., (w - 112.) * progress / 100., 5.),
                2.5,
                ACCENT,
            );
            canvas.text(
                &format!("{progress:.0}%"),
                x + w - 69.,
                276.,
                11,
                false,
                MUTED,
            );
        } else if !command.is_empty() {
            canvas.ellipsis(&command, x + 24., 284., w - 166., 12, false, MUTED);
            if ui.button(
                canvas,
                1301,
                Rect::new(x + w - 126., 276., 102., 30.),
                "复制更新命令",
                ButtonStyle::Quiet,
                true,
            ) {
                self.copy = Some(command);
            }
        } else if !release_url.is_empty() && matches!(kind, "portable" | "unknown") {
            canvas.ellipsis(
                "从正式发布页面选择匹配平台的安装包。",
                x + 24.,
                284.,
                w - 166.,
                11,
                false,
                MUTED,
            );
            if ui.button(
                canvas,
                1311,
                Rect::new(x + w - 126., 276., 102., 30.),
                "复制发布链接",
                ButtonStyle::Quiet,
                true,
            ) {
                self.copy = Some(release_url);
            }
        } else {
            canvas.ellipsis(
                "下载完成后仍会等待确认。应用更新会先释放本地媒体连接。",
                x + 24.,
                284.,
                w - 48.,
                11,
                false,
                MUTED,
            );
        }
        if !self.update_error.is_empty() {
            canvas.ellipsis(&self.update_error, x + 24., 312., w - 48., 11, false, RED);
        } else if self.read_only {
            canvas.text(
                "只读模式可手动检查版本；下载、应用与安装已关闭。",
                x + 24.,
                312.,
                11,
                false,
                MUTED,
            );
        }
        let startup_check = self.config["update_check"] == true;
        canvas.text(
            if self.no_update {
                "启动时检查版本（本次已由 --noupdate 暂停）"
            } else {
                "启动时检查版本"
            },
            x + 24.,
            350.,
            12,
            false,
            MUTED,
        );
        if ui.button(
            canvas,
            1302,
            Rect::new(x + w - 110., 341., 86., 31.),
            if startup_check {
                "已开启"
            } else {
                "已关闭"
            },
            if startup_check {
                ButtonStyle::Primary
            } else {
                ButtonStyle::Secondary
            },
            mutable,
        ) {
            self.set("update_check", json!(!startup_check));
        }
        if ui.button(
            canvas,
            1303,
            Rect::new(x + 24., 399., 118., 39.),
            "检查新版本",
            ButtonStyle::Secondary,
            !busy && !self.demo,
        ) {
            self.request_update("updates.check");
        }
        if ui.button(
            canvas,
            1304,
            Rect::new(x + 154., 399., 118., 39.),
            "下载更新",
            ButtonStyle::Secondary,
            mutable && self.updates["can_download"] == true,
        ) {
            self.request_update("updates.download");
        }
        if ui.button(
            canvas,
            1305,
            Rect::new(x + 284., 399., 144., 39.),
            "应用并重启",
            ButtonStyle::Primary,
            mutable && self.updates["can_apply"] == true,
        ) {
            self.request_update("updates.apply");
        }
        if ui.button(
            canvas,
            1306,
            Rect::new(x + w - 126., 399., 102., 39.),
            "查看详情",
            ButtonStyle::Quiet,
            true,
        ) {
            self.component_details("updates.status", &self.updates.clone());
        }

        canvas.rect(Rect::new(x, 482., w, 228.), 18., PANEL);
        canvas.text("OBS · PK 媒体组件", x + 24., 505., 17, true, INK);
        let plugin_version = text(&self.plugin, &["version"]);
        if !plugin_version.is_empty() {
            canvas.ellipsis(&plugin_version, x + w - 150., 510., 126., 11, false, MUTED);
        }
        let plugin_state = plugin_status_label(&self.plugin, self.plugin_restart_required);
        canvas.text(plugin_state, x + 24., 538., 13, true, INK);
        let message = if self.plugin_error.is_empty() {
            text(&self.plugin, &["message"])
        } else {
            self.plugin_error.clone()
        };
        canvas.ellipsis(
            &message,
            x + 24.,
            564.,
            w - 48.,
            11,
            false,
            if self.plugin_error.is_empty() {
                MUTED
            } else {
                RED
            },
        );
        let plugin_path = text(&self.plugin, &["plugin_path"]);
        canvas.ellipsis(
            if plugin_path.is_empty() {
                "此处检查插件文件；OBS 的实际加载状态请在 OBS 内确认。"
            } else {
                &plugin_path
            },
            x + 24.,
            589.,
            w - 48.,
            11,
            false,
            MUTED,
        );
        canvas.text(
            "安装后请手动重启 OBS。应用不会关闭 OBS 或启动推流。",
            x + 24.,
            621.,
            11,
            false,
            MUTED,
        );
        if ui.button(
            canvas,
            1307,
            Rect::new(x + w - 190., 612., 166., 31.),
            if self.plugin_replace {
                "允许替换已有文件"
            } else {
                "保留已有文件"
            },
            if self.plugin_replace {
                ButtonStyle::Primary
            } else {
                ButtonStyle::Quiet
            },
            mutable,
        ) {
            self.plugin_replace = !self.plugin_replace;
        }
        if ui.button(
            canvas,
            1308,
            Rect::new(x + 24., 659., 118., 33.),
            "刷新组件状态",
            ButtonStyle::Secondary,
            !busy && !self.demo,
        ) {
            self.submit("obs.plugin.status", json!({}));
        }
        if ui.button(
            canvas,
            1309,
            Rect::new(x + 154., 659., 144., 33.),
            "安装随包组件",
            ButtonStyle::Primary,
            mutable && self.plugin["can_install"] == true,
        ) {
            self.install_plugin();
        }
        if ui.button(
            canvas,
            1310,
            Rect::new(x + w - 126., 659., 102., 33.),
            "查看详情",
            ButtonStyle::Quiet,
            true,
        ) {
            self.component_details("obs.plugin.status", &self.plugin.clone());
        }
    }

    fn advanced_page(&mut self, canvas: &mut Canvas<'_>, ui: &mut Ui, x: f32, w: f32, height: f32) {
        canvas.text(
            "配置转换、连续 PK、媒体连接与全部原生操作。",
            x,
            119.,
            13,
            false,
            MUTED,
        );
        let left = 255.;
        let right_x = x + left + 28.;
        let right = w - left - 28.;
        if ui.input(
            canvas,
            1000,
            Rect::new(x, 166., left, 41.),
            &mut self.advanced_search,
            "查找操作名称",
            false,
            true,
        ) {
            self.advanced_offset = 0;
        }
        let operations = crate::schema::operations();
        let search = self.advanced_search.to_lowercase();
        let matching: Vec<_> = operations
            .iter()
            .filter(|operation| {
                operation.name.contains(&search)
                    || action_label(operation.name).contains(&search)
                    || operation.description.to_lowercase().contains(&search)
            })
            .collect();
        let rows = (((height - 331.) / 41.) as usize).max(1);
        self.advanced_offset = self
            .advanced_offset
            .min(matching.len().saturating_sub(rows));
        for (index, operation) in matching
            .iter()
            .skip(self.advanced_offset)
            .take(rows)
            .enumerate()
        {
            let rect = Rect::new(x, 222. + index as f32 * 41., left, 36.);
            if operation.name == self.advanced_name {
                canvas.rect(rect, 9., ACCENT_SOFT);
            }
            if ui.button(
                canvas,
                1010 + index as u64,
                rect,
                action_label(operation.name),
                ButtonStyle::Quiet,
                true,
            ) {
                self.advanced_name = operation.name.into();
                self.advanced_args = if operation.name == "pk.settings.update" {
                    json!({"field":"accept_pk","value":true})
                } else {
                    schema_example(&operation.input)
                }
                .to_string();
                self.advanced_output.clear();
                self.advanced_show_args = false;
            }
        }
        if ui.button(
            canvas,
            1040,
            Rect::new(x, height - 98., 115., 34.),
            "上一页",
            ButtonStyle::Quiet,
            self.advanced_offset > 0,
        ) {
            self.advanced_offset = self.advanced_offset.saturating_sub(rows);
        }
        if ui.button(
            canvas,
            1041,
            Rect::new(x + 136., height - 98., 115., 34.),
            "下一页",
            ButtonStyle::Quiet,
            self.advanced_offset + rows < matching.len(),
        ) {
            self.advanced_offset += rows;
        }
        let selected = operations
            .iter()
            .find(|operation| operation.name == self.advanced_name);
        if let Some(operation) = selected {
            canvas.ellipsis(
                action_label(operation.name),
                right_x,
                169.,
                right,
                21,
                true,
                INK,
            );
            canvas.text(operation.name, right_x, 207., 12, false, MUTED);
            let properties = operation.input.get("properties").and_then(Value::as_object);
            let names = properties
                .map(|properties| properties.keys().cloned().collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            canvas.text("接受参数", right_x, 248., 12, true, INK);
            canvas.ellipsis(
                if names.is_empty() {
                    "无需参数"
                } else {
                    &names
                },
                right_x,
                272.,
                right,
                12,
                false,
                MUTED,
            );
            let secret = properties.is_some_and(|properties| {
                [
                    "cookie",
                    "password",
                    "ticket",
                    "token",
                    "v_voucher",
                    "qrcode_key",
                    "access_token",
                ]
                .iter()
                .any(|name| properties.contains_key(*name))
            }) || (operation.name == "config.set"
                && serde_json::from_str::<Value>(&self.advanced_args)
                    .ok()
                    .is_some_and(|args| args["key"] == "proxy"));
            canvas.text("JSON 参数", right_x, 319., 13, false, MUTED);
            ui.input(
                canvas,
                1042,
                Rect::new(right_x, 345., right, 44.),
                &mut self.advanced_args,
                "{}",
                secret && !self.advanced_show_args,
                true,
            );
            if ui.button(
                canvas,
                1043,
                Rect::new(right_x, 406., 112., 39.),
                if operation.read_only {
                    "读取"
                } else {
                    "执行操作"
                },
                ButtonStyle::Primary,
                self.pending == 0 && (!self.demo || operation.read_only),
            ) {
                match serde_json::from_str::<Value>(&self.advanced_args) {
                    Ok(args) if args.is_object() => {
                        if self.submit_cleanup(operation.name, args, None) {
                            self.advanced_pending = operation.name.into();
                        }
                    }
                    _ => self.notice("请提供一个完整的 JSON 对象作为参数。", false),
                }
            }
            if secret
                && ui.button(
                    canvas,
                    1044,
                    Rect::new(right_x + 125., 406., 100., 39.),
                    if self.advanced_show_args {
                        "隐藏参数"
                    } else {
                        "显示参数"
                    },
                    ButtonStyle::Quiet,
                    true,
                )
            {
                self.advanced_show_args = !self.advanced_show_args;
            }
            if ui.button(
                canvas,
                1045,
                Rect::new(right_x + right - 109., 406., 109., 39.),
                "复制完整定义",
                ButtonStyle::Quiet,
                true,
            ) {
                self.copy =
                    Some(serde_json::to_string_pretty(&operation.input).unwrap_or_default());
            }
            canvas.text("操作结果", right_x, 485., 15, true, INK);
            let h = (height - 587.).max(91.);
            canvas.rect(Rect::new(right_x, 520., right, h), 11., PANEL);
            let output = if self.advanced_output.is_empty() {
                "结果将在这里显示。密钥和凭据会自动隐藏。"
            } else {
                &self.advanced_output
            };
            for (index, line) in output.lines().take(((h - 24.) / 20.) as usize).enumerate() {
                canvas.ellipsis(
                    line,
                    right_x + 13.,
                    533. + index as f32 * 20.,
                    right - 26.,
                    12,
                    false,
                    MUTED,
                );
            }
            if ui.button(
                canvas,
                1046,
                Rect::new(right_x + right - 100., 478., 100., 33.),
                "复制结果",
                ButtonStyle::Quiet,
                !self.advanced_output.is_empty(),
            ) {
                self.copy = Some(self.advanced_output.clone());
            }
        }
    }

    fn start_live(&mut self) {
        let mut args = json!({"protocol": self.protocol, "strategy": self.strategy});
        if !self.area_id.trim().is_empty() {
            match self.area_id.trim().parse::<u64>() {
                Ok(id) if id > 0 => args["area_id"] = id.into(),
                _ => {
                    self.notice("请选择有效的直播分区。", false);
                    return;
                }
            }
        }
        self.submit("live.start", args);
    }

    fn handle_invitation(&mut self, accept: bool) {
        let peer = self.opponent_uid.trim().parse::<u64>();
        let source = self.invitation_source.trim().parse::<i64>();
        match (peer, source) {
            (Ok(peer), Ok(source)) if peer > 0 => self.submit(
                if accept { "pk.accept" } else { "pk.reject" },
                json!({"peer_uid": peer, "source": source, "session_id": self.session_id}),
            ),
            _ => self.notice("请填写邀请事件中的有效主播 UID 与来源编号。", false),
        }
    }

    fn consume_invitation(&mut self, event: &Value) {
        let command = text(event, &["cmd"]);
        let data = event.get("data").unwrap_or(&Value::Null);
        let invitation = command == "UNIVERSAL_INTERACT_INVITATION"
            || (command == "UNIVERSAL_INTERACT_OPERATION"
                && data.get("operation_type").and_then(Value::as_i64) == Some(0)
                && data.get("biz_label").and_then(Value::as_str) == Some("universal_multi_conn"));
        if !invitation {
            return;
        }
        let active = text(&self.accounts, &["active"]);
        let own = self
            .account_rows()
            .iter()
            .find(|account| text(account, &["name"]) == active)
            .map(|account| text(account, &["uid"]))
            .unwrap_or_default();
        if own.is_empty() || text(data, &["uid"]) != own {
            return;
        }
        let session = text(data, &["biz_session_id"]);
        let mut peer = text(data, &["peer_uid"]);
        if peer.parse::<u64>().unwrap_or(0) == 0 {
            peer = text(data, &["initiator"]);
        }
        let source = data
            .get("source")
            .and_then(|v| v.as_i64().map(|x| x as u64).or_else(|| v.as_u64()));
        if session.is_empty() || peer.parse::<u64>().unwrap_or(0) == 0 || source.is_none() {
            return;
        }
        self.session_id = session;
        self.opponent_uid = peer;
        self.invitation_source = (source.unwrap_or(0) as u32 as i32).to_string();
        self.notice("收到主播邀请。可在 PK 与连线页面接受或拒绝。", true);
    }

    fn poll_verification(&mut self) {
        let mut args =
            json!({"type": self.verification.get("type").cloned().unwrap_or(json!(60043))});
        if let Some(token) = self.verification.get("token") {
            args["token"] = token.clone();
        }
        if let Some(room) = self.verification.get("room_id") {
            args["room_id"] = room.clone();
        }
        self.submit("face.poll", args);
    }

    fn verification_panel(
        &mut self,
        canvas: &mut Canvas<'_>,
        ui: &mut Ui,
        x: f32,
        w: f32,
        height: f32,
    ) {
        canvas.rect(Rect::new(x, 128., w, (height - 222.).max(430.)), 19., PANEL);
        canvas.text("完成平台验证", x + 28., 158., 24, true, INK);
        canvas.text(
            "哔哩哔哩要求验证当前账号后继续开播。",
            x + 29.,
            201.,
            13,
            false,
            MUTED,
        );
        if let Some(qr) = &self.verification_qr {
            draw_qr(canvas, qr, x + 29., 249., 233.);
        } else {
            canvas.text("正在准备验证二维码…", x + 29., 285., 14, false, MUTED);
        }
        let text_x = x + 294.;
        canvas.text("1  使用手机扫描二维码", text_x, 262., 14, true, INK);
        canvas.text("2  在哔哩哔哩页面完成验证", text_x, 307., 14, true, INK);
        canvas.text("3  验证通过后将自动继续开播", text_x, 352., 14, true, INK);
        canvas.ellipsis(
            &self.verification_message,
            x + 29.,
            510.,
            w - 58.,
            13,
            false,
            MUTED,
        );
        if ui.button(
            canvas,
            900,
            Rect::new(x + 28., 551., 132., 39.),
            "检查验证结果",
            ButtonStyle::Primary,
            self.pending == 0,
        ) {
            self.verification_poll = Instant::now();
            self.poll_verification();
        }
        let url = text(&self.verification, &["url"]);
        if ui.button(
            canvas,
            901,
            Rect::new(x + 174., 551., 116., 39.),
            "复制验证链接",
            ButtonStyle::Secondary,
            !url.is_empty(),
        ) {
            self.copy = Some(url);
        }
        if ui.button(
            canvas,
            902,
            Rect::new(x + w - 103., 551., 76., 39.),
            "取消",
            ButtonStyle::Quiet,
            true,
        ) {
            self.verification = Value::Null;
            self.verification_qr = None;
            self.verification_created = None;
            self.notice("已取消等待验证。", true);
        }
    }
}

fn payload(value: &Value) -> &Value {
    value
        .get("data")
        .filter(|v| v.is_object() || v.is_array())
        .unwrap_or(value)
}

fn palette(theme: Theme) -> Palette {
    let mut palette = Palette::for_theme(theme);
    if theme == Theme::Dark {
        palette.background = 0x121110ff;
        palette.surface = 0x1b1917ff;
        palette.text = 0xf1eae2ff;
        palette.muted = 0xa69b90ff;
        palette.line = 0x342e28ff;
        palette.accent = 0xd59b76ff;
        palette.accent_hover = 0xe3af8bff;
        palette.accent_soft = 0x3b2a22ff;
        palette.hover = 0x29241fff;
        palette.success = 0xc6b18cff;
        palette.success_soft = 0x30291eff;
        palette.danger = 0xd9a294ff;
        palette.danger_soft = 0x3b2722ff;
        palette.disabled_surface = 0x23201cff;
        palette.disabled_text = 0x7c7268ff;
        palette.on_accent = 0x241a14ff;
    } else {
        palette.background = 0xf5f2edff;
        palette.surface = 0xfffcf7ff;
        palette.text = 0x2c2824ff;
        palette.muted = 0x766b63ff;
        palette.line = 0xddd5caff;
        palette.accent = 0x974f33ff;
        palette.accent_hover = 0x834127ff;
        palette.accent_soft = 0xf0e1d6ff;
        palette.hover = 0xebe5ddff;
        palette.success = 0x795c30ff;
        palette.success_soft = 0xf0e7d7ff;
        palette.danger = 0xa64b3dff;
        palette.danger_soft = 0xf4e1dbff;
        palette.disabled_surface = 0xede8e1ff;
        palette.disabled_text = 0x9a8e82ff;
        palette.on_accent = 0xfff9f1ff;
    }
    palette
}

fn schema_example(schema: &Value) -> Value {
    let mut object = serde_json::Map::new();
    for name in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let property = &schema["properties"][name];
        let value = property
            .get("enum")
            .and_then(Value::as_array)
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_else(|| {
                let kind = property.get("type").and_then(|value| {
                    value.as_str().or_else(|| {
                        value
                            .as_array()
                            .and_then(|values| values.first())
                            .and_then(Value::as_str)
                    })
                });
                match kind {
                    Some("integer" | "number") => {
                        property.get("minimum").cloned().unwrap_or(json!(0))
                    }
                    Some("boolean") => json!(false),
                    Some("object") => json!({}),
                    Some("array") => json!([]),
                    _ => json!(""),
                }
            });
        object.insert(name.into(), value);
    }
    Value::Object(object)
}

fn text(value: &Value, names: &[&str]) -> String {
    for name in names {
        if let Some(value) = value.get(*name) {
            match value {
                Value::String(s) if !s.is_empty() => return s.clone(),
                Value::Number(number) => return number.to_string(),
                _ => {}
            }
        }
    }
    String::new()
}

fn collect_areas(value: &Value, parent: &str, output: &mut Vec<(u64, String)>) {
    if let Some(array) = value.as_array() {
        for entry in array {
            collect_areas(entry, parent, output);
        }
        return;
    }
    let name = text(value, &["name", "area_name"]);
    if let Some(children) = value
        .get("area_v1_info")
        .or_else(|| value.get("list"))
        .or_else(|| value.get("children"))
    {
        collect_areas(
            children,
            if name.is_empty() { parent } else { &name },
            output,
        );
    } else if let Some(id) = value
        .get("id")
        .or_else(|| value.get("area_id"))
        .and_then(|id| {
            id.as_u64()
                .or_else(|| id.as_str().and_then(|s| s.parse().ok()))
        })
    {
        if !name.is_empty() {
            output.push((
                id,
                if parent.is_empty() {
                    name
                } else {
                    format!("{parent} · {name}")
                },
            ));
        }
    }
}

fn recent_areas(value: &Value, areas: &[(u64, String)]) -> Vec<(u64, String)> {
    let rows = value
        .as_array()
        .or_else(|| value.get("list").and_then(Value::as_array))
        .or_else(|| value.get("areas").and_then(Value::as_array));
    let mut result = Vec::new();
    for row in rows.into_iter().flatten() {
        let name = text(row, &["name", "area_name"]);
        let parent = text(row, &["parent_name", "parent_area_name"]);
        let label = if parent.is_empty() {
            name.clone()
        } else {
            format!("{parent} · {name}")
        };
        let id = text(row, &["id", "area_id", "area_v2_id"])
            .parse::<u64>()
            .ok()
            .filter(|id| *id > 0)
            .or_else(|| {
                areas
                    .iter()
                    .find(|(_, title)| title == &label)
                    .map(|(id, _)| *id)
            });
        if let Some(id) = id {
            if !name.is_empty() && !result.iter().any(|(existing, _)| *existing == id) {
                result.push((id, label));
            }
        }
    }
    result
}

fn cover_identity(room: &Value) -> String {
    let flat = text(room, &["cover_url"]);
    if !flat.is_empty() {
        return flat;
    }
    if let Some(cover) = room.get("cover") {
        if let Some(value) = cover.as_str() {
            return value.to_owned();
        }
        return text(cover, &["url"]);
    }
    String::new()
}

fn audit_description(value: &Value) -> String {
    if let Some(summary) = value.get("summary").and_then(Value::as_str) {
        return summary.into();
    }
    let title = value.get("title_audit_info").unwrap_or(&Value::Null);
    let title_state = match title.get("audit_status").and_then(Value::as_i64) {
        Some(2) => "标题已通过",
        Some(3) => "标题未通过",
        Some(_) => "标题审核中",
        None => "标题暂无待审记录",
    };
    let cover = value.get("cover").unwrap_or(&Value::Null);
    let cover_state = match cover.get("auditStatus").and_then(Value::as_i64) {
        Some(1) => "封面已通过",
        Some(-1) => "封面未通过",
        Some(0) => "封面审核中",
        _ => "封面暂无审核结果",
    };
    let reason = [
        text(title, &["audit_reason"]),
        text(cover, &["auditReason"]),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join("；");
    if reason.is_empty() {
        format!("{title_state} · {cover_state}")
    } else {
        format!("{title_state} · {cover_state}：{reason}")
    }
}

#[allow(clippy::too_many_arguments)]
fn field(
    canvas: &mut Canvas<'_>,
    ui: &mut Ui,
    id: u64,
    x: f32,
    y: f32,
    w: f32,
    label: &str,
    value: &mut String,
    placeholder: &str,
    secret: bool,
) -> bool {
    canvas.text(label, x, y, 13, false, MUTED);
    ui.input(
        canvas,
        id,
        Rect::new(x, y + 27., w, 43.),
        value,
        placeholder,
        secret,
        true,
    )
}

fn stat(canvas: &mut Canvas<'_>, x: f32, y: f32, w: f32, label: &str, value: &str, hint: &str) {
    canvas.text(label, x + 1., y, 11, false, MUTED);
    canvas.ellipsis(value, x, y + 28., w, 22, true, INK);
    canvas.ellipsis(hint, x + 1., y + 65., w, 11, false, MUTED);
}

fn pill(canvas: &mut Canvas<'_>, x: f32, y: f32, label: &str, ink: u32, fill: u32) {
    let width = canvas.measure(label, 10, false) + 22.;
    canvas.rect(Rect::new(x, y, width, 24.), 12., fill);
    canvas.text(label, x + 11., y + 6., 10, false, ink);
}

fn toggle(
    canvas: &mut Canvas<'_>,
    ui: &mut Ui,
    id: u64,
    rect: Rect,
    label: &str,
    value: &mut bool,
) {
    canvas.text(label, rect.x, rect.y + 12., 13, false, INK);
    if ui.button(
        canvas,
        id,
        Rect::new(rect.x + rect.w - 77., rect.y + 3., 77., 36.),
        if *value { "已开启" } else { "已关闭" },
        if *value {
            ButtonStyle::Primary
        } else {
            ButtonStyle::Secondary
        },
        true,
    ) {
        *value = !*value;
    }
}

fn shorten(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        value.to_string()
    } else {
        format!(
            "{}…",
            value
                .chars()
                .take(limit.saturating_sub(1))
                .collect::<String>()
        )
    }
}

fn draw_qr(canvas: &mut Canvas<'_>, code: &QrCode, x: f32, y: f32, max_size: f32) {
    let size = code.size();
    let cell = (max_size / (size + 8) as f32).floor().max(1.);
    let actual = (size + 8) as f32 * cell;
    canvas.rect(Rect::new(x, y, actual, actual), 10., 0xfffffeff);
    for row in 0..size {
        for column in 0..size {
            if code.get_module(column, row) {
                canvas.rect(
                    Rect::new(
                        x + (column + 4) as f32 * cell,
                        y + (row + 4) as f32 * cell,
                        cell,
                        cell,
                    ),
                    0.,
                    0x211d19ff,
                );
            }
        }
    }
}

fn nav_icon(canvas: &mut Canvas<'_>, x: f32, y: f32, kind: usize, ink: u32) {
    match kind {
        0 => {
            canvas.rect(Rect::new(x - 7., y - 7., 14., 14.), 4., ink);
            canvas.rect(Rect::new(x - 4., y - 4., 8., 8.), 2., PANEL);
        }
        1 => {
            for offset in [-5., 0., 5.] {
                canvas.line((x - 7., y + offset), (x + 7., y + offset), 1.3, ink);
            }
        }
        2 => {
            canvas.rect(Rect::new(x - 3.5, y - 8., 7., 7.), 3.5, ink);
            canvas.rect(Rect::new(x - 7., y + 1., 14., 8.), 4., ink);
        }
        3 => {
            canvas.rect(Rect::new(x - 8., y - 6., 16., 11.), 3., ink);
            canvas.rect(Rect::new(x - 6., y - 4., 12., 7.), 1., PANEL);
            canvas.line((x, y + 5.), (x, y + 9.), 1.3, ink);
            canvas.line((x - 4., y + 9.), (x + 4., y + 9.), 1.3, ink);
        }
        4 => {
            canvas.line((x - 8., y - 4.), (x + 7., y - 4.), 1.3, ink);
            canvas.line((x + 3., y - 8.), (x + 7., y - 4.), 1.3, ink);
            canvas.line((x - 7., y + 4.), (x + 8., y + 4.), 1.3, ink);
            canvas.line((x - 7., y + 4.), (x - 3., y + 8.), 1.3, ink);
        }
        _ => {
            canvas.line((x - 7., y - 5.), (x - 2., y), 1.5, ink);
            canvas.line((x - 2., y), (x - 7., y + 5.), 1.5, ink);
            canvas.line((x + 1., y + 6.), (x + 8., y + 6.), 1.5, ink);
        }
    }
}

fn action_label(method: &str) -> &str {
    match method {
        "doctor" => "检查本机环境",
        "config.get" => "读取偏好设置",
        "config.reset" => "恢复一组默认设置",
        "logs.read" => "查看本地操作日志",
        "logs.clear" => "清理本地操作日志",
        "account.list" => "查看保存的账号",
        "live.start" => "开始直播",
        "live.stop" => "结束直播",
        "live.stream" => "获取推流连接",
        "live.status" => "读取直播状态",
        "room.get" => "读取直播间信息",
        "room.titles" => "读取当前账号最近标题",
        "room.update" => "更新直播间信息",
        "room.cover" => "上传直播间封面",
        "room.cover.preview" => "读取当前直播间封面",
        "room.audit" => "查看标题与封面审核",
        "room.precheck" => "检查直播间开播权限",
        "area.list" => "查看全部直播分区",
        "area.recent" => "查看最近使用的分区",
        "auth.qr.create" => "生成登录二维码",
        "auth.qr.poll" => "检查扫码登录结果",
        "auth.validate" => "验证当前账号",
        "auth.refresh" => "刷新账号会话",
        "auth.import" => "导入账号",
        "account.use" => "切换账号",
        "account.remove" => "移除账号",
        "obs.status" => "读取 OBS 推流状态",
        "obs.watch.start" => "持续观察 OBS 推流状态",
        "obs.watch.stop" => "停止 OBS 状态观察",
        "obs.watch.status" => "读取 OBS 观察器状态",
        "obs.sync" => "同步 OBS 推流配置",
        "obs.configure" => "保存 OBS 连接",
        "obs.start" => "OBS 开始推流",
        "obs.stop" => "OBS 停止推流",
        "obs.request" => "调用 OBS 原生操作",
        "config.set" => "保存偏好设置",
        "delay.get" => "读取直播延迟与限制",
        "delay.set" => "应用直播延迟",
        "face.register" => "生成平台验证链接",
        "face.poll" => "检查平台验证结果",
        "face.report" => "提交平台验证结果",
        "danmaku.send" => "发送直播间弹幕",
        "danmaku.connect" => "接收直播间事件",
        "danmaku.read" => "读取直播间事件",
        "danmaku.disconnect" => "停止接收直播间事件",
        "zuse.check" => "检查自动化脚本",
        "zuse.run" => "运行自动化脚本",
        "zuse.schema" => "查看自动化命令",
        "pk.status" => "读取 PK 轮次状态",
        "pk.search" => "搜索可邀请的主播",
        "pk.match" => "开始 PK 匹配",
        "pk.cancel" => "取消 PK 匹配",
        "pk.invite" => "邀请主播连线",
        "pk.accept" => "接受主播连线",
        "pk.reject" => "拒绝主播连线",
        "pk.end" => "结束 PK",
        "pk.session" => "查看本地连线状态",
        "pk.settings.get" => "读取 PK 偏好",
        "pk.settings.update" => "更新 PK 偏好",
        "pk.continuous.get" => "读取连续 PK 设置",
        "pk.continuous.set" => "设置连续 PK",
        "pk.invitation.cancel" => "撤回主播邀请",
        "pk.link.panel" => "查看可连线主播",
        "pk.link.list" => "查看当前连线",
        "pk.link.connect" => "申请主播连线",
        "pk.link.cancel" => "取消连线申请",
        "pk.link.start" => "开始连线 PK",
        "pk.link.disconnect" => "断开主播连线",
        "pk.request" => "调用 PK 协议操作",
        "convert.discover" => "查找直播姬场景配置",
        "convert.livehime" => "将直播姬场景转换为 OBS",
        "rtc.doctor" => "检查原生媒体环境",
        "rtc.self-test" => "测试本机媒体连接",
        "rtc.encoders" => "查看可用媒体编码器",
        "rtc.status" => "读取原生媒体状态",
        "rtc.connect" => "连接原生媒体频道",
        "rtc.disconnect" => "断开原生媒体频道",
        "rtc.request" => "调用原生媒体协议",
        "rtc.members" => "查看媒体频道成员",
        "rtc.events" => "读取媒体频道事件",
        "rtc.publish" => "发送本机音视频",
        "rtc.publication.stop" => "停止发送本机音视频",
        "rtc.subscribe" => "接收对方音视频",
        "rtc.subscription.stop" => "停止接收对方音视频",
        "rtc.subscription.update" => "调整接收的音视频",
        "rtc.capture.start" => "开始采集 OBS 画面",
        "rtc.capture.stop" => "停止采集 OBS 画面",
        "rtc.mute" => "调整音视频静音状态",
        "rtc.preview" => "打开对方画面预览",
        "rtc.preview.stop" => "关闭对方画面预览",
        "rtc.local.preview" => "预览本机 OBS 画面",
        "rtc.local.preview.stop" => "关闭本机 OBS 预览",
        "updates.check" => "检查最新稳定版本",
        "updates.status" => "查看更新状态",
        "updates.download" => "下载应用更新",
        "updates.apply" => "应用更新并退出",
        "obs.plugin.status" => "查看 OBS 插件文件状态",
        "obs.plugin.install" => "安装 OBS PK 插件文件",
        _ => method,
    }
}

fn plugin_status_label(plugin: &Value, restart_required: bool) -> &'static str {
    if plugin["state"] == "needs_restart" || (restart_required && plugin["installed"] == true) {
        "插件文件已安装 · 请重启 OBS 后使用"
    } else {
        match plugin["state"].as_str().unwrap_or("") {
            "installed" => "插件文件已安装 · OBS 加载状态未检查",
            "not_installed" => "尚未安装 OBS PK 插件",
            "unavailable" => "当前随包组件不可用",
            _ => "正在读取组件文件状态…",
        }
    }
}

fn demo_response(method: &str) -> Result<Value, String> {
    match method {
        "config.get" => Ok(
            json!({"protocol":"rtmp","strategy":"fallback","default_area":371,"obs_auto_start":true,"obs_auto_stop":true}),
        ),
        "account.list" => Ok(
            json!({"active":"创作空间","accounts":[{"name":"创作空间","uname":"lightjunction","uid":10001,"active":true}]}),
        ),
        "room.get" => Ok(
            json!({"room_id":271828,"title":"写代码，也聊聊新的想法。","area_id":371,"area_name":"知识 · 科技人文","live_status":0,"uname":"lightjunction","followers":1280}),
        ),
        "room.titles" => Ok(
            json!({"titles":["写代码，也聊聊新的想法。","从一个想法，到一行代码。","今晚一起做点有意思的东西。"]}),
        ),
        "room.audit" => {
            Ok(json!({"title_audit_info":{"audit_status":2},"cover":{"auditStatus":1}}))
        }
        "area.recent" => Ok(
            json!([{"id":371,"name":"科技人文","parent_name":"知识"},{"id":21,"name":"日常分享","parent_name":"生活"}]),
        ),
        "obs.status" => Ok(json!({"connected":true,"outputActive":false})),
        "updates.status" => Ok(json!({
            "phase":"idle","current":env!("CARGO_PKG_VERSION"),
            "installation":{"kind":"demo"},"update_available":false,
            "can_download":false,"can_apply":false,"progress":0
        })),
        "obs.plugin.status" => Ok(json!({
            "state":"unavailable","installed":false,"verified":false,"can_install":false,
            "restart_required":false,"message":"演示模式不会检查或修改实际 OBS 插件文件。"
        })),
        "pk.status" => Ok(json!({"state":"准备连接"})),
        "area.list" => Ok(
            json!({"area_v1_info":[{"id":11,"name":"知识","list":[{"id":371,"name":"科技人文"},{"id":377,"name":"职业技能"}]},{"id":1,"name":"生活","list":[{"id":21,"name":"日常分享"}]}]}),
        ),
        "zuse.schema" => Ok(
            json!({"commands":["bilibilive.status()","bilibilive.start(area_id=371)","bilibilive.stop()"]}),
        ),
        _ => Err("演示模式下不会执行账号或直播操作。".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component_fixture(
        demo: bool,
        read_only: bool,
    ) -> (Desktop, Receiver<Request>, Sender<Response>) {
        let (tx, requests) = mpsc::channel();
        let (responses, rx) = mpsc::channel();
        (Desktop::new(tx, rx, demo, read_only), requests, responses)
    }

    fn component_reply(
        desktop: &mut Desktop,
        responses: &Sender<Response>,
        method: &str,
        value: Value,
    ) {
        responses
            .send(Response {
                method: method.into(),
                result: Ok(value),
            })
            .unwrap();
        desktop.receive();
    }

    #[test]
    fn startup_version_check_runs_once_only_when_enabled_and_allowed() {
        for demo in [false, true] {
            for read_only in [false, true] {
                for no_update in [false, true] {
                    for enabled in [false, true] {
                        let (mut desktop, requests, responses) = component_fixture(demo, read_only);
                        desktop.no_update = no_update;
                        desktop.config = json!({"update_check":enabled});
                        desktop.load_config();
                        desktop.tick();
                        let expected = enabled && !demo && !read_only && !no_update;
                        let queued: Vec<_> =
                            requests.try_iter().map(|request| request.method).collect();
                        assert_eq!(
                            queued,
                            if expected {
                                vec!["updates.check"]
                            } else {
                                vec![]
                            }
                        );
                        if expected {
                            component_reply(
                                &mut desktop,
                                &responses,
                                "updates.check",
                                json!({"phase":"up_to_date"}),
                            );
                        }
                        desktop.tick();
                        assert!(requests.try_recv().is_err());
                    }
                }
            }
        }
    }

    #[test]
    fn component_page_reads_local_status_without_starting_network_checks_or_installs() {
        for read_only in [false, true] {
            let (mut desktop, requests, _) = component_fixture(false, read_only);
            desktop.activate(Page::Components);
            let queued: Vec<_> = requests.try_iter().map(|request| request.method).collect();
            assert_eq!(queued, ["updates.status", "obs.plugin.status"]);
        }
        let (mut desktop, requests, _) = component_fixture(true, false);
        desktop.activate(Page::Components);
        assert!(requests.try_recv().is_err());
        assert_eq!(desktop.updates["installation"]["kind"], "demo");
        assert_eq!(desktop.plugin["can_install"], false);
    }

    #[test]
    fn manual_check_remains_available_in_read_only_and_noupdate_mode() {
        let (mut desktop, requests, _) = component_fixture(false, true);
        desktop.no_update = true;
        assert!(desktop.request_update("updates.check"));
        assert_eq!(requests.try_recv().unwrap().method, "updates.check");
        assert!(desktop.startup_update_handled);
    }

    #[test]
    fn downloads_require_a_staged_identity_and_explicit_mutable_action() {
        for (demo, read_only, allowed, staged) in [
            (true, false, true, "checked-identity"),
            (false, true, true, "checked-identity"),
            (false, false, false, "checked-identity"),
            (false, false, true, ""),
        ] {
            let (mut desktop, requests, _) = component_fixture(demo, read_only);
            desktop.updates = json!({"can_download":allowed,"staged_id":staged});
            assert!(!desktop.request_update("updates.download"));
            assert!(requests.try_recv().is_err());
        }
        let (mut desktop, requests, responses) = component_fixture(false, false);
        desktop.updates =
            json!({"phase":"available","can_download":true,"staged_id":"version-and-digest"});
        assert!(desktop.request_update("updates.download"));
        let request = requests.try_recv().unwrap();
        assert_eq!(request.method, "updates.download");
        assert_eq!(
            request.args,
            json!({"staged_id":"version-and-digest","wait":false})
        );
        component_reply(
            &mut desktop,
            &responses,
            "updates.download",
            json!({"phase":"downloading","progress":35}),
        );
        desktop.startup_update_handled = true;
        desktop.update_poll = Instant::now() - Duration::from_secs(2);
        desktop.tick();
        assert_eq!(requests.try_recv().unwrap().method, "updates.status");
        component_reply(
            &mut desktop,
            &responses,
            "updates.status",
            json!({"phase":"downloaded","can_apply":true,"staged_id":"version-and-digest"}),
        );
        desktop.tick();
        assert!(requests.try_recv().is_err());
        assert!(!desktop.quit_requested);
    }

    #[test]
    fn explicit_apply_freezes_further_ui_work_and_exits_only_after_success() {
        let (mut desktop, requests, responses) = component_fixture(false, false);
        desktop.updates =
            json!({"phase":"downloaded","can_apply":true,"staged_id":"checked-identity"});
        assert!(desktop.request_update("updates.apply"));
        let request = requests.try_recv().unwrap();
        assert_eq!(
            request.args,
            json!({"staged_id":"checked-identity","restart":true})
        );
        assert!(!desktop.submit_cleanup("room.get", json!({}), None));
        assert!(requests.try_recv().is_err());
        assert!(!desktop.quit_requested);
        component_reply(
            &mut desktop,
            &responses,
            "updates.apply",
            json!({
                "phase":"restart_pending","quit_requested":true,"exit_required":true,"restart_requested":true
            }),
        );
        assert!(desktop.quit_requested);
        assert!(!desktop.submit_cleanup("config.get", json!({}), None));
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn failed_apply_preserves_window_and_does_not_restart_retired_media() {
        let (mut desktop, requests, responses) = component_fixture(false, false);
        desktop.config = json!({"obs_auto_connect":true,"obs_link":true});
        desktop.load_config();
        desktop.startup_update_handled = true;
        desktop.updates = json!({"can_apply":true,"staged_id":"checked-identity"});
        assert!(desktop.request_update("updates.apply"));
        requests.try_recv().unwrap();
        component_reply(
            &mut desktop,
            &responses,
            "updates.apply",
            json!({
                "ok":false,"status":"failed","phase":"failed","local_sessions_retired":true,
                "error":"fixture helper could not start","quit_requested":true,"exit_required":true
            }),
        );
        assert!(!desktop.quit_requested);
        assert!(!desktop.update_apply_requested);
        assert!(desktop.local_sessions_retired);
        assert!(desktop.update_error.contains("helper could not start"));
        assert!(!desktop.notice.as_ref().unwrap().1);
        desktop.tick();
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn terminal_zuse_update_exits_while_partial_shutdown_failures_keep_the_window() {
        let (mut desktop, requests, responses) = component_fixture(false, false);
        component_reply(
            &mut desktop,
            &responses,
            "zuse.run",
            json!({
                "ok":true,"status":"completed","completed_steps":1,
                "quit_requested":true,"exit_required":true,"restart_pending":true,
                "local_sessions_retired":true
            }),
        );
        assert!(desktop.quit_requested);
        assert!(desktop.local_sessions_retired);
        assert!(requests.try_recv().is_err());

        for method in ["updates.apply", "zuse.run"] {
            let (mut desktop, requests, responses) = component_fixture(false, false);
            desktop.config = json!({"obs_auto_connect":true});
            desktop.load_config();
            desktop.startup_update_handled = true;
            desktop.events_connected = true;
            let failure = json!({
                "ok":false,"status":"failed","phase":"failed",
                "local_shutdown_attempted":true,"local_sessions_retired":false,
                "error":"fixture media shutdown incomplete"
            });
            let receipt = if method == "zuse.run" {
                json!({"ok":false,"status":"failed","completed_steps":0,
                    "failed_step":{"operation":"updates.apply","ok":false,"result":failure},
                    "quit_requested":true,"exit_required":true})
            } else {
                failure
            };
            component_reply(&mut desktop, &responses, method, receipt);
            assert!(!desktop.quit_requested);
            assert!(desktop.local_shutdown_attempted);
            assert!(!desktop.local_sessions_retired);
            assert!(!desktop.events_connected);
            desktop.tick();
            assert!(requests.try_recv().is_err());
        }
    }

    #[test]
    fn update_transport_failure_stops_polling_without_quitting() {
        let (mut desktop, requests, responses) = component_fixture(false, false);
        desktop.startup_update_handled = true;
        desktop.updates = json!({"phase":"downloading"});
        assert!(desktop.request_update("updates.status"));
        requests.try_recv().unwrap();
        responses
            .send(Response {
                method: "updates.status".into(),
                result: Err("fixture status failure".into()),
            })
            .unwrap();
        desktop.receive();
        desktop.update_poll = Instant::now() - Duration::from_secs(2);
        desktop.tick();
        assert!(requests.try_recv().is_err());
        assert!(!desktop.quit_requested);
        assert_eq!(desktop.update_error, "fixture status failure");
        assert!(desktop.request_update("updates.status"));
        assert!(desktop.update_error.is_empty());
    }

    #[test]
    fn plugin_install_preserves_files_unless_replacement_is_selected() {
        let (mut desktop, requests, responses) = component_fixture(false, false);
        desktop.plugin = json!({"state":"not_installed","can_install":true});
        assert!(desktop.install_plugin());
        assert_eq!(requests.try_recv().unwrap().args, json!({"replace":false}));
        component_reply(
            &mut desktop,
            &responses,
            "obs.plugin.install",
            json!({
                "state":"needs_restart","installed":true,"restart_required":true,"can_install":true
            }),
        );
        assert!(desktop.plugin_restart_required);
        desktop.plugin_replace = true;
        assert!(desktop.install_plugin());
        assert_eq!(requests.try_recv().unwrap().args, json!({"replace":true}));
        assert!(!desktop.quit_requested);
    }

    #[test]
    fn readonly_and_demo_reject_component_mutations_even_from_advanced_actions() {
        for (demo, read_only) in [(true, false), (false, true)] {
            let (mut desktop, requests, _) = component_fixture(demo, read_only);
            desktop.plugin = json!({"can_install":true});
            assert!(!desktop.install_plugin());
            for method in ["updates.download", "updates.apply", "obs.plugin.install"] {
                assert!(!desktop.submit_cleanup(method, json!({}), None));
            }
            assert!(requests.try_recv().is_err());
        }
    }

    #[test]
    fn filesystem_plugin_status_does_not_claim_obs_loaded_the_plugin() {
        let installed = json!({"state":"installed","installed":true,"restart_required":false});
        assert!(plugin_status_label(&installed, false).contains("加载状态未检查"));
        assert!(plugin_status_label(&installed, true).contains("请重启 OBS"));
        assert_eq!(
            plugin_status_label(&json!({"state":"not_installed","installed":false}), true),
            "尚未安装 OBS PK 插件"
        );
        let (mut desktop, _, responses) = component_fixture(false, false);
        desktop.plugin_restart_required = true;
        component_reply(&mut desktop, &responses, "obs.plugin.status", installed);
        assert!(desktop.plugin_restart_required);
    }

    #[test]
    fn component_results_are_not_discarded_by_account_switch_generations() {
        let (mut desktop, _, responses) = component_fixture(false, false);
        desktop.account_context_seen = true;
        desktop.account_generation = 12;
        component_reply(
            &mut desktop,
            &responses,
            "updates.check",
            json!({
                "phase":"available","update_available":true,"latest":"9.9.9","_context":{"generation":11}
            }),
        );
        assert_eq!(desktop.updates["latest"], "9.9.9");
        assert_eq!(desktop.account_generation, 12);
    }

    #[test]
    fn reads_the_platform_area_v1_info_envelope() {
        let mut areas = Vec::new();
        collect_areas(
            &json!({"area_v1_info":[{"name":"知识","list":[{"id":"371","name":"科技人文"}]}]}),
            "",
            &mut areas,
        );
        assert_eq!(areas, vec![(371, "知识 · 科技人文".into())]);
    }

    #[test]
    fn incoming_invitation_must_belong_to_the_selected_account() {
        let (tx, _requests) = mpsc::channel();
        let (_responses, rx) = mpsc::channel();
        let mut desktop = Desktop::new(tx, rx, false, false);
        desktop.accounts = json!({"active":"daily","accounts":[{"name":"daily","uid":123}]});
        let mut event = json!({"cmd":"UNIVERSAL_INTERACT_INVITATION","data":{"uid":999,"peer_uid":0,"initiator":456,"source":4,"biz_session_id":"actual-invitation"}});
        desktop.consume_invitation(&event);
        assert!(desktop.session_id.is_empty());
        event["data"]["uid"] = json!(123);
        desktop.consume_invitation(&event);
        assert_eq!(desktop.session_id, "actual-invitation");
        assert_eq!(desktop.opponent_uid, "456");
        assert_eq!(desktop.invitation_source, "4");
    }

    #[test]
    fn round_id_cannot_become_an_invitation_session_id() {
        let (tx, _requests) = mpsc::channel();
        let (responses, rx) = mpsc::channel();
        let mut desktop = Desktop::new(tx, rx, false, false);
        responses
            .send(Response {
                method: "pk.status".into(),
                result: Ok(json!({"pk_id":12345})),
            })
            .unwrap();
        desktop.receive();
        assert!(desktop.session_id.is_empty());
    }

    #[test]
    fn account_generation_zero_is_valid_and_old_results_are_discarded() {
        let (tx, _requests) = mpsc::channel();
        let (responses, rx) = mpsc::channel();
        let mut desktop = Desktop::new(tx, rx, false, false);
        responses
            .send(Response {
                method: "room.get".into(),
                result: Ok(json!({"title":"Account A", "_context":{"generation":0,"uid":"1"}})),
            })
            .unwrap();
        desktop.receive();
        assert_eq!(desktop.title, "Account A");
        desktop.room_dirty = true;
        responses
            .send(Response {
                method: "room.get".into(),
                result: Ok(json!({"title":"Account B", "_context":{"generation":1,"uid":"2"}})),
            })
            .unwrap();
        responses
            .send(Response {
                method: "room.get".into(),
                result: Ok(
                    json!({"title":"Late account A", "_context":{"generation":0,"uid":"1"}}),
                ),
            })
            .unwrap();
        desktop.receive();
        assert_eq!(desktop.title, "Account B");
        assert_eq!(desktop.room["title"], "Account B");
        assert!(!desktop.room_dirty);
    }

    #[test]
    fn background_refresh_preserves_an_unsaved_title_for_the_same_account() {
        let (tx, _requests) = mpsc::channel();
        let (responses, rx) = mpsc::channel();
        let mut desktop = Desktop::new(tx, rx, false, false);
        desktop.title = "Unsaved draft".into();
        desktop.room_dirty = true;
        responses
            .send(Response {
                method: "room.get".into(),
                result: Ok(json!({"title":"Published title"})),
            })
            .unwrap();
        desktop.receive();
        assert_eq!(desktop.title, "Unsaved draft");
        assert_eq!(desktop.room["title"], "Published title");
    }

    #[test]
    fn automation_failure_keeps_its_receipt_without_reporting_success_or_retrying() {
        let (tx, requests) = mpsc::channel();
        let (responses, rx) = mpsc::channel();
        let mut desktop = Desktop::new(tx, rx, false, false);
        let receipt = json!({
            "ok": false,
            "status": "failed",
            "completed_steps": 0,
            "failed_step": {
                "operation": "live.start",
                "result": {
                    "ok": false,
                    "status": "partial_failure",
                    "room_remains_live": true,
                    "obs": {"ok": false, "error": "OBS fixture failure"},
                },
            },
        });
        responses
            .send(Response {
                method: "zuse.run".into(),
                result: Ok(receipt.clone()),
            })
            .unwrap();
        desktop.receive();
        assert_eq!(
            serde_json::from_str::<Value>(&desktop.script_output).unwrap(),
            receipt
        );
        let (message, success, _) = desktop.notice.as_ref().unwrap();
        assert!(!success);
        assert_eq!(message, "自动化操作未完成，请查看执行结果详情。");
        assert_eq!(desktop.activity, vec![("运行自动化脚本".into(), false)]);
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn obs_start_partial_failure_is_visible_without_stopping_or_restarting_the_room() {
        let (tx, requests) = mpsc::channel();
        let (responses, rx) = mpsc::channel();
        let mut desktop = Desktop::new(tx, rx, false, false);
        responses
            .send(Response {
                method: "live.start".into(),
                result: Ok(json!({
                    "ok": false,
                    "status": "partial_failure",
                    "room_id": "123",
                    "room_remains_live": true,
                    "obs": {"ok": false, "error": "OBS fixture failure"},
                })),
            })
            .unwrap();
        desktop.receive();
        assert_eq!(desktop.stream["room_remains_live"], true);
        let (message, success, _) = desktop.notice.as_ref().unwrap();
        assert!(!success);
        assert!(message.contains("直播间已开播"));
        assert!(message.contains("OBS 自动推流失败"));
        assert!(message.contains("手动处理"));
        assert_eq!(desktop.activity, vec![("开始直播".into(), false)]);
        let requested: Vec<_> = requests.try_iter().map(|request| request.method).collect();
        assert_eq!(requested, ["room.get"]);
    }

    #[test]
    fn background_obs_watch_starts_only_when_requested_and_stops_afterward() {
        let (tx, requests) = mpsc::channel();
        let (responses, rx) = mpsc::channel();
        let mut desktop = Desktop::new(tx, rx, false, false);
        desktop.load_config();
        desktop.tick();
        assert!(requests.try_recv().is_err());
        assert!(desktop.activity.is_empty());

        for (enabled, method) in [(true, "obs.watch.start"), (false, "obs.watch.stop")] {
            desktop.config["obs_auto_connect"] = json!(enabled);
            desktop.tick();
            assert_eq!(requests.try_recv().unwrap().method, method);
            responses
                .send(Response {
                    method: method.into(),
                    result: Ok(json!({"running": enabled})),
                })
                .unwrap();
            desktop.receive();
            desktop.tick();
            assert!(requests.try_recv().is_err());
        }
    }

    #[test]
    fn read_only_skips_background_obs_watch_but_preserves_explicit_errors() {
        for enabled in [false, true] {
            let (tx, requests) = mpsc::channel();
            let (responses, rx) = mpsc::channel();
            let mut desktop = Desktop::new(tx, rx, false, true);
            desktop.config = json!({
                "obs_auto_connect": enabled,
                "obs_link": enabled,
                "obs_auto_stop": enabled,
            });
            desktop.load_config();
            desktop.tick();
            assert!(requests.try_recv().is_err());
            assert!(desktop.notice.is_none());
            assert!(desktop.activity.is_empty());

            desktop.submit("obs.watch.start", json!({}));
            let request = requests.try_recv().unwrap();
            assert_eq!(request.method, "obs.watch.start");
            let error = "obs.watch.start is disabled in read-only mode";
            responses
                .send(Response {
                    method: request.method,
                    result: Err(error.into()),
                })
                .unwrap();
            desktop.receive();
            let (message, success, _) = desktop.notice.as_ref().unwrap();
            assert_eq!(message, error);
            assert!(!success);
            assert_eq!(
                desktop.activity,
                vec![("持续观察 OBS 推流状态".into(), false)]
            );
        }
    }

    #[test]
    fn demo_never_starts_the_obs_observer() {
        let (tx, requests) = mpsc::channel();
        let (_responses, rx) = mpsc::channel();
        let mut desktop = Desktop::new(tx, rx, true, false);
        desktop.config_loaded = true;
        desktop.config = json!({"obs_auto_connect":true,"obs_link":true,"obs_auto_stop":true});
        desktop.tick();
        assert!(requests.try_recv().is_err());
    }
}
