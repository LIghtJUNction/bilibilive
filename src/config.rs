//! Non-secret preferences and account metadata. Credentials live in the OS vault.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub struct ProfileLock(fs::File);
impl Drop for ProfileLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
pub fn lock_profile(directory: &Path) -> Result<ProfileLock, String> {
    fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(directory.join("profile.lock"))
        .map_err(|e| e.to_string())?;
    file.lock()
        .map_err(|e| format!("Cannot lock the configuration profile: {e}"))?;
    Ok(ProfileLock(file))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    pub name: String,
    pub uid: String,
    #[serde(default)]
    pub uname: String,
    #[serde(default)]
    pub room_id: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub accounts: Vec<Account>,
    pub active: Option<String>,
    pub obs_url: String,
    pub obs_password_set: bool,
    pub obs_auto_connect: bool,
    pub obs_auto_start: bool,
    pub obs_auto_stop: bool,
    pub obs_link: bool,
    pub auto_pk_media: bool,
    pub pk_media: Value,
    pub protocol: String,
    pub strategy: String,
    pub delay: u64,
    pub default_area: u64,
    pub theme: String,
    pub font_family: String,
    pub background_image: String,
    pub background_mode: String,
    pub background_opacity: u64,
    pub background_blur: u64,
    pub tray_enabled: bool,
    pub close_to_tray: bool,
    pub tray_icon: String,
    pub tray_tooltip: String,
    pub single_instance: bool,
    pub title_history: BTreeMap<String, Vec<String>>,
    pub proxy: String,
    pub proxy_credentials_set: bool,
    pub use_system_proxy: bool,
    pub timeout_secs: u64,
    pub update_check: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            accounts: Vec::new(),
            active: None,
            obs_url: "ws://127.0.0.1:4455".into(),
            obs_password_set: false,
            obs_auto_connect: false,
            obs_auto_start: false,
            obs_auto_stop: false,
            obs_link: false,
            auto_pk_media: true,
            pk_media: json!({"codec":"h264","encoder":"libx264","quality":"balanced","width":1280,"height":720,"fps":30,"bitrate_kbps":2500}),
            protocol: "rtmp".into(),
            strategy: "fallback".into(),
            delay: 0,
            default_area: 0,
            theme: "dark".into(),
            proxy: String::new(),
            proxy_credentials_set: false,
            font_family: String::new(),
            background_image: String::new(),
            background_mode: "cover".into(),
            background_opacity: 15,
            background_blur: 0,
            tray_enabled: true,
            close_to_tray: false,
            tray_icon: String::new(),
            tray_tooltip: "bilibilive".into(),
            single_instance: true,
            title_history: BTreeMap::new(),
            use_system_proxy: false,
            timeout_secs: 15,
            update_check: true,
        }
    }
}

pub fn default_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("BILIBILIVE_CONFIG_DIR") {
        return path.into();
    }
    #[cfg(target_os = "windows")]
    if let Some(path) = std::env::var_os("APPDATA") {
        return PathBuf::from(path).join("bilibilive");
    }
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(path).join("bilibilive");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    #[cfg(target_os = "macos")]
    return home.join("Library/Application Support/bilibilive");
    #[cfg(not(target_os = "macos"))]
    home.join(".config/bilibilive")
}

impl Config {
    pub fn load(directory: &Path) -> Result<Self, String> {
        match fs::read(directory.join("config.json")) {
            Ok(bytes) => {
                if bytes.len() > 1024 * 1024 {
                    return Err("Configuration exceeds 1 MiB".into());
                }
                serde_json::from_slice(&bytes)
                    .map_err(|e| format!("Invalid bilibilive config: {e}"))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("Cannot read bilibilive configuration: {e}")),
        }
    }

    pub fn save(&self, directory: &Path) -> Result<(), String> {
        fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).map_err(|_| "OS random source unavailable")?;
        let temporary = directory.join(format!(
            ".config-{}-{:016x}.tmp",
            std::process::id(),
            u64::from_ne_bytes(random)
        ));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| {
            let mut file = options.open(&temporary).map_err(|e| e.to_string())?;
            file.write_all(&bytes).map_err(|e| e.to_string())?;
            file.write_all(b"\n").map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            fs::rename(&temporary, directory.join("config.json")).map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    pub fn snapshot(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or_else(|_| json!({}));
        if let Some(accounts) = value["accounts"].as_array_mut() {
            for account in accounts {
                account["active"] = json!(account["name"].as_str() == self.active.as_deref());
            }
        }
        value["version"] = crate::VERSION.into();
        value
    }

    pub fn remember_title(&mut self, uid: &str, title: &str) {
        let title = title.trim();
        if title.is_empty() {
            return;
        }
        let titles = self.title_history.entry(uid.into()).or_default();
        titles.retain(|existing| existing != title);
        titles.insert(0, title.into());
        titles.truncate(5);
    }

    pub fn reset(&mut self, section: &str) -> Result<(), String> {
        let defaults = Self::default();
        match section {
            "appearance" => {
                self.theme = defaults.theme;
                self.font_family = defaults.font_family;
                self.background_image = defaults.background_image;
                self.background_mode = defaults.background_mode;
                self.background_opacity = defaults.background_opacity;
                self.background_blur = defaults.background_blur;
                self.tray_enabled = defaults.tray_enabled;
                self.close_to_tray = defaults.close_to_tray;
                self.tray_icon = defaults.tray_icon;
                self.tray_tooltip = defaults.tray_tooltip;
                self.single_instance = defaults.single_instance;
            }
            "network" => {
                self.proxy = defaults.proxy;
                self.proxy_credentials_set = false;
                self.use_system_proxy = defaults.use_system_proxy;
                self.timeout_secs = defaults.timeout_secs;
            }
            "stream" => {
                self.protocol = defaults.protocol;
                self.strategy = defaults.strategy;
                self.delay = defaults.delay;
                self.default_area = defaults.default_area;
                self.auto_pk_media = defaults.auto_pk_media;
                self.pk_media = defaults.pk_media;
            }
            "obs" => {
                self.obs_url = defaults.obs_url;
                self.obs_password_set = false;
                self.obs_auto_connect = false;
                self.obs_auto_start = false;
                self.obs_auto_stop = false;
                self.obs_link = false;
            }
            _ => return Err("Section must be appearance, network, stream or obs".into()),
        }
        Ok(())
    }

    pub fn set(&mut self, key: &str, value: &Value) -> Result<(), String> {
        let boolean = || {
            value
                .as_bool()
                .ok_or_else(|| format!("{key} must be a boolean"))
        };
        let number = || {
            value
                .as_u64()
                .ok_or_else(|| format!("{key} must be a nonnegative integer"))
        };
        let text = || {
            value
                .as_str()
                .filter(|s| s.len() <= 4096 && !s.contains('\0'))
                .map(str::to_owned)
                .ok_or_else(|| format!("{key} must be text"))
        };
        match key {
            "obs_url" => {
                let v = text()?;
                if !v.starts_with("ws://") && !v.starts_with("wss://") {
                    return Err("OBS URL must use ws:// or wss://".into());
                }
                self.obs_url = v;
            }
            "obs_auto_start" => self.obs_auto_start = boolean()?,
            "obs_auto_connect" => self.obs_auto_connect = boolean()?,
            "obs_auto_stop" => self.obs_auto_stop = boolean()?,
            "obs_link" => self.obs_link = boolean()?,
            "auto_pk_media" => self.auto_pk_media = boolean()?,
            "pk_media" => {
                crate::schema::validate("rtc.publish", value)?;
                let object = value.as_object().ok_or("pk_media must be an object")?;
                if object.keys().any(|k| {
                    !matches!(
                        k.as_str(),
                        "codec"
                            | "encoder"
                            | "quality"
                            | "hw_device"
                            | "width"
                            | "height"
                            | "fps"
                            | "bitrate_kbps"
                            | "crop"
                    )
                }) {
                    return Err("pk_media only accepts encoder, quality, dimensions, frame rate, bitrate and crop preferences".into());
                }
                for (key, min, max) in [
                    ("width", 2, 3840),
                    ("height", 2, 2160),
                    ("fps", 1, 60),
                    ("bitrate_kbps", 100, 20000),
                ] {
                    if let Some(v) = object.get(key) {
                        if v.as_u64().is_none_or(|n| n < min || n > max) {
                            return Err(format!("pk_media.{key} must be {min}..{max}"));
                        }
                    }
                }
                if let Some(crop) = object.get("crop") {
                    let crop = crop.as_object().ok_or("pk_media.crop must be an object")?;
                    if crop.len() != 4
                        || ["x", "y", "width", "height"]
                            .iter()
                            .any(|k| crop.get(*k).and_then(Value::as_u64).is_none())
                        || crop["width"].as_u64().unwrap_or(0) < 2
                        || crop["height"].as_u64().unwrap_or(0) < 2
                    {
                        return Err(
                            "pk_media.crop requires nonnegative x/y and width/height of at least 2"
                                .into(),
                        );
                    }
                }
                self.pk_media = value.clone();
            }
            "use_system_proxy" => self.use_system_proxy = boolean()?,
            "update_check" => self.update_check = boolean()?,
            "protocol" => {
                let v = text()?;
                if !["rtmp", "srt", "srt-only"].contains(&v.as_str()) {
                    return Err("Protocol must be rtmp, srt or srt-only".into());
                }
                self.protocol = v;
            }
            "strategy" => {
                let v = text()?;
                if !["fallback", "prefer", "only", "strict"].contains(&v.as_str()) {
                    return Err("Strategy must be fallback or only".into());
                }
                self.strategy = v;
            }
            "theme" => {
                let v = text()?;
                if !["dark", "light"].contains(&v.as_str()) {
                    return Err("Theme must be dark or light".into());
                }
                self.theme = v;
            }
            "font_family" => self.font_family = text()?,
            "background_image" => self.background_image = text()?,
            "background_mode" => {
                let v = text()?;
                if !["native", "stretch", "cover", "contain"].contains(&v.as_str()) {
                    return Err("Background mode must be native, stretch, cover or contain".into());
                }
                self.background_mode = v;
            }
            "background_opacity" => {
                let v = number()?;
                if v > 100 {
                    return Err("Background opacity must be 0..100".into());
                }
                self.background_opacity = v;
            }
            "background_blur" => {
                let v = number()?;
                if v > 40 {
                    return Err("Background blur must be 0..40".into());
                }
                self.background_blur = v;
            }
            "tray_enabled" => self.tray_enabled = boolean()?,
            "close_to_tray" => self.close_to_tray = boolean()?,
            "tray_icon" => self.tray_icon = text()?,
            "tray_tooltip" => self.tray_tooltip = text()?,
            "single_instance" => self.single_instance = boolean()?,
            "default_area" => self.default_area = number()?,
            "delay" => {
                let v = number()?;
                if v > 86400 {
                    return Err("Delay exceeds one day; live application also checks the platform's own limits".into());
                }
                self.delay = v;
            }
            "timeout_secs" => {
                let v = number()?;
                if !(1..=120).contains(&v) {
                    return Err("Timeout must be 1..120 seconds".into());
                }
                self.timeout_secs = v;
            }
            "proxy" => {
                let v = text()?;
                if !v.is_empty() && !v.starts_with("http://") && !v.starts_with("https://") {
                    return Err("Proxy must use HTTP or HTTPS".into());
                }
                self.proxy = v;
            }
            _ => return Err(format!("Unknown or protected setting: {key}")),
        }
        Ok(())
    }
}

pub fn validate_account_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err("Account name must contain 1..128 bytes without control characters".into());
    }
    Ok(())
}

fn entry(directory: &Path, name: &str) -> Result<keyring::Entry, String> {
    // Isolated configuration directories must never address another profile's vault entry.
    let directory = std::path::absolute(directory).map_err(|e| e.to_string())?;
    let identity = format!(
        "{:x}/{name}",
        md5::compute(directory.to_string_lossy().as_bytes())
    );
    keyring::Entry::new("bilibilive", &identity).map_err(|e| format!("OS credential store unavailable: {e}. For a headless session use BILIBILIVE_COOKIE or --cookie-file."))
}

pub fn store_secret(directory: &Path, name: &str, value: &str) -> Result<(), String> {
    entry(directory, name)?
        .set_password(value)
        .map_err(|e| format!("Cannot save credential in OS store: {e}"))
}
pub fn load_secret(directory: &Path, name: &str) -> Result<String, String> {
    entry(directory, name)?
        .get_password()
        .map_err(|e| format!("Cannot read credential from OS store: {e}"))
}
pub fn remove_secret(directory: &Path, name: &str) -> Result<(), String> {
    match entry(directory, name)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("Cannot remove credential from OS store: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_are_typed_and_never_contain_secrets() {
        let mut config = Config::default();
        assert!(config.set("obs_auto_start", &json!("false")).is_err());
        assert!(config.set("obs_password", &json!("secret")).is_err());
        assert!(config.set("protocol", &json!("file")).is_err());
        config.set("protocol", &json!("srt")).unwrap();
        assert_eq!(config.snapshot()["protocol"], "srt");
        assert!(config.snapshot().get("obs_password").is_none());
    }

    #[test]
    fn independent_frontends_preserve_each_others_preferences() {
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).unwrap();
        let directory = std::env::temp_dir().join(format!(
            "bilibilive-profile-test-{:x}",
            u64::from_ne_bytes(nonce)
        ));
        let first = crate::app::App::new(Some(directory.clone())).unwrap();
        let second = crate::app::App::new(Some(directory.clone())).unwrap();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..8 {
                    first
                        .call("config.set", json!({"key":"protocol","value":"srt"}))
                        .unwrap();
                }
            });
            scope.spawn(|| {
                for _ in 0..8 {
                    second
                        .call("config.set", json!({"key":"theme","value":"light"}))
                        .unwrap();
                }
            });
        });
        assert_eq!(first.snapshot()["theme"], "light");
        assert_eq!(second.snapshot()["protocol"], "srt");
        assert_eq!(Config::load(&directory).unwrap().protocol, "srt");
        drop(first);
        drop(second);
        fs::remove_dir_all(directory).unwrap();
    }
}
