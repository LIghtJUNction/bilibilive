//! Installation-aware updates. Checking never writes or launches an installer.
//! Velopack downloads are pinned to one release, and application teardown happens
//! only after its on-disk package has been verified again.
use reqwest::blocking::Client;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use velopack::{
    UpdateInfo, UpdateManager, UpdateOptions, VelopackAsset, VelopackAssetFeed,
    bundle::{self, Manifest},
    locator::VelopackLocatorConfig,
    sources::UpdateSource,
};

const REPOSITORY: &str = "https://github.com/LIghtJUNction/bilibilive";
const RELEASE_API: &str = "https://api.github.com/repos/LIghtJUNction/bilibilive/releases/latest";
const METADATA_LIMIT: u64 = 2 * 1024 * 1024;
const PACKAGE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Handle Velopack installation hooks before parsing the application's CLI.
/// Downloaded updates are applied only by an explicit `updates.apply` request.
pub fn startup_hooks() {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        let hook = args.first().is_some_and(|arg| {
            matches!(
                arg.as_str(),
                "--veloapp-install"
                    | "--veloapp-updated"
                    | "--veloapp-obsolete"
                    | "--veloapp-uninstall"
            )
        });
        // The SDK also cleans old package caches during normal startup. Explicit
        // read-only/help invocations should not perform even that maintenance.
        if !hook
            && args
                .iter()
                .any(|arg| matches!(arg.as_str(), "--read-only" | "--help" | "--version"))
        {
            return;
        }
        velopack::VelopackApp::build()
            .set_args(if hook {
                args.into_iter().take(2).collect()
            } else {
                Vec::new()
            })
            .set_auto_apply_on_startup(false)
            .run();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Installation {
    kind: String,
    executable: PathBuf,
    root: PathBuf,
    channel: Option<String>,
    portable: bool,
    source: Option<String>,
    manual_program: Option<String>,
    manual_args: Vec<String>,
    reason: Option<String>,
    #[serde(skip)]
    managed: Option<VelopackLocatorConfig>,
}
impl Installation {
    fn identity(&self) -> String {
        sha256(
            format!(
                "{}\n{}\n{}\n{}",
                self.kind,
                self.root.display(),
                self.executable.display(),
                self.channel.as_deref().unwrap_or("")
            )
            .as_bytes(),
        )
    }
    fn value(&self, latest: Option<&str>) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or_else(|_| json!({"kind":"unknown"}));
        if latest.is_none() {
            if let Some(object) = value.as_object_mut() {
                object.remove("manual_program");
                object.remove("manual_args");
            }
            return value;
        }
        let args = self
            .manual_args
            .iter()
            .map(|arg| {
                if arg == "{version}" {
                    latest.unwrap_or(crate::VERSION).to_owned()
                } else if arg == "v{version}" {
                    format!("v{}", latest.unwrap_or(crate::VERSION))
                } else {
                    arg.clone()
                }
            })
            .collect::<Vec<_>>();
        value["manual_args"] = json!(args);
        if let Some(program) = &self.manual_program {
            value["manual_command"] = json!(shell_command(program, &args));
        }
        value
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Candidate {
    schema: u8,
    installation_id: String,
    current: String,
    staged_id: String,
    release_url: String,
    package_url: String,
    channel: String,
    asset: VelopackAsset,
}
impl Candidate {
    fn new(installation: &Installation, asset: VelopackAsset) -> Result<Self, String> {
        let channel = installation
            .channel
            .clone()
            .ok_or("Managed installation has no update channel")?;
        validate_asset(&asset, &asset.Version)?;
        let release_url = format!("{REPOSITORY}/releases/tag/v{}", asset.Version);
        let package_url = format!(
            "{REPOSITORY}/releases/download/v{}/{}",
            asset.Version, asset.FileName
        );
        let installation_id = installation.identity();
        let staged_id = sha256(
            serde_json::to_vec(&json!({
                "installation":installation_id,"current":crate::VERSION,"channel":channel,
                "url":package_url,"asset":asset
            }))
            .map_err(|e| e.to_string())?
            .as_slice(),
        );
        Ok(Self {
            schema: 1,
            installation_id,
            current: crate::VERSION.into(),
            staged_id,
            release_url,
            package_url,
            channel,
            asset,
        })
    }
    fn validate(&self, installation: &Installation) -> Result<(), String> {
        let expected = Self::new(installation, self.asset.clone())?;
        if self.schema != 1
            || self.current != crate::VERSION
            || self.installation_id != expected.installation_id
            || self.channel != expected.channel
            || self.staged_id != expected.staged_id
            || self.package_url != expected.package_url
            || self.release_url != expected.release_url
        {
            return Err(
                "Staged update does not belong to this executable, version and installation".into(),
            );
        }
        if Version::parse(&self.asset.Version).map_err(|e| e.to_string())? <= current_version()? {
            return Err("Staged update is not newer than the running version".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DownloadReceipt {
    candidate: Candidate,
    phase: String,
    progress: u8,
    error: Option<String>,
}
struct State {
    installation: Option<Installation>,
    candidate: Option<Candidate>,
    phase: String,
    latest: Option<String>,
    url: String,
    available: bool,
    progress: u8,
    error: Option<String>,
    apply_lock: Option<File>,
}
struct Inner {
    operation: Mutex<()>,
    state: Mutex<State>,
    job: Mutex<Option<JoinHandle<()>>>,
    cancelled: AtomicBool,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

/// Shared by all frontends of one App. Background workers hold only weak state
/// ownership; they cannot keep an application alive or terminate its process.
#[derive(Clone)]
pub struct Updater {
    directory: PathBuf,
    inner: Arc<Inner>,
}
impl Updater {
    pub fn new(state_dir: PathBuf) -> Self {
        // Match the vault's lexical absolute profile identity. A helper restarts
        // from the installation directory, so relative CLI paths cannot be kept.
        let state_dir = std::path::absolute(&state_dir).unwrap_or(state_dir);
        Self {
            directory: state_dir,
            inner: Arc::new(Inner {
                operation: Mutex::new(()),
                state: Mutex::new(State {
                    installation: None,
                    candidate: None,
                    phase: "idle".into(),
                    latest: None,
                    url: format!("{REPOSITORY}/releases/latest"),
                    available: false,
                    progress: 0,
                    error: None,
                    apply_lock: None,
                }),
                job: Mutex::new(None),
                cancelled: AtomicBool::new(false),
            }),
        }
    }
    fn installation(&self) -> Result<Installation, String> {
        if let Some(value) = self
            .inner
            .state
            .lock()
            .map_err(lock_error)?
            .installation
            .clone()
        {
            return Ok(value);
        }
        let value = detect_installation(&std::env::current_exe().map_err(|e| e.to_string())?)?;
        self.inner.state.lock().map_err(lock_error)?.installation = Some(value.clone());
        Ok(value)
    }
    fn receipt_path(&self, installation: &Installation) -> PathBuf {
        self.directory
            .join("updates")
            .join(format!("{}.json", installation.identity()))
    }
    fn load_receipt(&self, installation: &Installation) -> Result<Option<DownloadReceipt>, String> {
        let path = self.receipt_path(installation);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = read_limited(&path, METADATA_LIMIT)?;
        let receipt: DownloadReceipt =
            serde_json::from_slice(&bytes).map_err(|e| format!("Invalid update receipt: {e}"))?;
        // A completed upgrade naturally leaves a receipt from the previous version.
        if receipt.candidate.current != crate::VERSION {
            return Ok(None);
        }
        receipt.candidate.validate(installation)?;
        Ok(Some(receipt))
    }
    pub fn status(&self) -> Result<Value, String> {
        let installation = self.installation()?;
        let mut state = self.inner.state.lock().map_err(lock_error)?;
        if matches!(
            state.phase.as_str(),
            "idle" | "available" | "manual" | "up_to_date"
        ) {
            if let Some(receipt) = self.load_receipt(&installation)? {
                if receipt.phase == "downloaded" {
                    state.latest = Some(receipt.candidate.asset.Version.clone());
                    state.url = receipt.candidate.release_url.clone();
                    state.candidate = Some(receipt.candidate);
                    state.phase = "downloaded".into();
                    state.progress = 100;
                    state.available = true;
                    state.error = None;
                }
            }
        }
        Ok(status_value(&state, &installation))
    }
    /// A bounded, read-only metadata request. A newer check never silently
    /// replaces an already downloaded candidate awaiting a user's decision.
    pub fn check(&self) -> Result<Value, String> {
        let _operation = self
            .inner
            .operation
            .try_lock()
            .map_err(|_| "Another update operation is already running".to_owned())?;
        let deadline = Instant::now() + CHECK_TIMEOUT;
        let installation = self.installation()?;
        {
            let mut state = self.inner.state.lock().map_err(lock_error)?;
            if matches!(
                state.phase.as_str(),
                "checking" | "downloading" | "applying" | "restart_pending"
            ) {
                return Ok(status_value(&state, &installation));
            }
            if state.phase == "downloaded" {
                return Ok(status_value(&state, &installation));
            }
            state.phase = "checking".into();
            state.error = None;
        }
        let result = check_release(&installation, deadline);
        let mut state = self.inner.state.lock().map_err(lock_error)?;
        match result {
            Ok((latest, url, candidate)) => {
                state.available =
                    Version::parse(&latest).map_err(|e| e.to_string())? > current_version()?;
                state.phase = if !state.available {
                    "up_to_date"
                } else if installation.kind != "velopack" {
                    "manual"
                } else {
                    "available"
                }
                .into();
                state.latest = Some(latest);
                state.url = url;
                state.candidate = candidate;
                state.progress = 0;
            }
            Err(error) => {
                state.phase = "failed".into();
                state.error = Some(error);
            }
        }
        Ok(status_value(&state, &installation))
    }
    /// `wait=false` returns immediately and is intended for the native GUI or
    /// persistent hosts. One-shot CLI callers wait by default.
    pub fn download(&self, args: &Value) -> Result<Value, String> {
        let id = staged_id(args)?;
        let wait = boolean(args, "wait", true)?;
        let installation = self.installation()?;
        let config = installation
            .managed
            .clone()
            .ok_or("This installation is managed externally; use its manual update command")?;
        {
            let state = self.inner.state.lock().map_err(lock_error)?;
            if matches!(
                state.phase.as_str(),
                "downloading" | "checking" | "applying" | "restart_pending"
            ) {
                return Err("Another update operation is already running".into());
            }
        }
        if self
            .inner
            .state
            .lock()
            .map_err(lock_error)?
            .candidate
            .is_none()
        {
            self.check()?;
        }
        let _operation = self
            .inner
            .operation
            .try_lock()
            .map_err(|_| "Another update operation is already running".to_owned())?;
        let candidate = {
            let mut state = self.inner.state.lock().map_err(lock_error)?;
            if matches!(
                state.phase.as_str(),
                "checking" | "downloading" | "applying" | "restart_pending"
            ) {
                return Err("Another update operation is already running".into());
            }
            let candidate = state
                .candidate
                .clone()
                .ok_or("Check for a compatible update before downloading")?;
            candidate.validate(&installation)?;
            require_id(&candidate, &id)?;
            state.phase = "downloading".into();
            state.progress = 0;
            state.error = None;
            candidate
        };
        let config = match mutation_config(config) {
            Ok(config) => config,
            Err(error) => {
                self.fail(&error);
                return Err(error);
            }
        };
        let file_lock = match acquire_install_lock(&config.PackagesDir) {
            Ok(value) => value,
            Err(error) => {
                self.fail(&error);
                return Err(error);
            }
        };
        let receipt_path = self.receipt_path(&installation);
        let initial = DownloadReceipt {
            candidate: candidate.clone(),
            phase: "downloading".into(),
            progress: 0,
            error: None,
        };
        if let Err(error) = save_receipt(&receipt_path, &initial) {
            self.fail(&error);
            return Err(error);
        }
        self.inner.cancelled.store(false, Ordering::Release);
        let weak = Arc::downgrade(&self.inner);
        let spawn = thread::Builder::new()
            .name("bilibilive-update-download".into())
            .spawn(move || {
                let _file_lock = file_lock;
                let outcome = download_candidate(&config, &candidate, weak.clone());
                let receipt = DownloadReceipt {
                    candidate: candidate.clone(),
                    phase: if outcome.is_ok() {
                        "downloaded"
                    } else {
                        "failed"
                    }
                    .into(),
                    progress: if outcome.is_ok() { 100 } else { 0 },
                    error: outcome.err(),
                };
                let saved = save_receipt(&receipt_path, &receipt);
                if let Some(inner) = weak.upgrade() {
                    if let Ok(mut state) = inner.state.lock() {
                        state.phase = receipt.phase;
                        state.progress = receipt.progress;
                        state.error = receipt.error;
                        if let Err(error) = saved {
                            state.phase = "failed".into();
                            state.error = Some(error);
                        }
                    }
                }
            });
        let job = match spawn {
            Ok(job) => job,
            Err(error) => {
                self.fail(&error.to_string());
                return Err(format!("Cannot start update worker: {error}"));
            }
        };
        if wait {
            if job.join().is_err() {
                self.fail("Update worker stopped unexpectedly");
            }
        } else {
            *self.inner.job.lock().map_err(lock_error)? = Some(job);
        }
        self.status()
    }
    pub fn apply(&self, args: &Value) -> Result<Value, String> {
        self.apply_with_prepare(args, || Ok(()))
    }
    /// The caller can retire local RTC/OBS observers after validation and before
    /// starting the helper. This never stops an external broadcast itself.
    pub fn apply_with_prepare<F>(&self, args: &Value, prepare: F) -> Result<Value, String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        self.apply_with_prepare_deadline(args, prepare)
            .map(|(value, _)| value)
    }
    /// Internal monotonic deadline starts at helper launch, after all package
    /// verification and local session preparation have finished.
    pub(crate) fn apply_with_prepare_deadline<F>(
        &self,
        args: &Value,
        prepare: F,
    ) -> Result<(Value, Instant), String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        let _operation = self
            .inner
            .operation
            .try_lock()
            .map_err(|_| "Another update operation is already running".to_owned())?;
        let id = staged_id(args)?;
        let restart = boolean(args, "restart", true)?;
        let installation = self.installation()?;
        let config = installation
            .managed
            .clone()
            .ok_or("This installation has no Velopack update helper")?;
        {
            let state = self.inner.state.lock().map_err(lock_error)?;
            if matches!(
                state.phase.as_str(),
                "checking" | "downloading" | "applying" | "restart_pending"
            ) {
                return Err("Another update operation is already running".into());
            }
        }
        let receipt = self
            .load_receipt(&installation)?
            .ok_or("Download the selected update before applying it")?;
        if receipt.phase != "downloaded" {
            return Err("The selected update has not completed downloading".into());
        }
        let candidate = receipt.candidate;
        require_id(&candidate, &id)?;
        candidate.validate(&installation)?;
        let config = mutation_config(config)?;
        let file_lock = acquire_install_lock(&config.PackagesDir)?;
        // Re-read installed identity and every package byte immediately before
        // starting any teardown, including packages the SDK found in its cache.
        validate_managed(&config, &installation.channel.clone().unwrap_or_default())?;
        verify_package(
            &config.PackagesDir.join(&candidate.asset.FileName),
            &candidate,
        )?;
        let source = PinnedSource {
            candidate: candidate.clone(),
            inner: Arc::downgrade(&self.inner),
        };
        let manager = UpdateManager::new(source, Some(update_options()), Some(config))
            .map_err(|e| e.to_string())?;
        {
            let mut state = self.inner.state.lock().map_err(lock_error)?;
            state.phase = "applying".into();
            state.error = None;
        }
        if let Err(error) = prepare() {
            self.fail(&error);
            return Err(error);
        }
        let restart_args = vec![
            "gui".to_owned(),
            "--noupdate".into(),
            "--config-dir".into(),
            self.directory.to_string_lossy().into_owned(),
        ];
        // Acquire the last fallible state lock before launching the helper so a
        // successful spawn can always retain its receipt and installation lock.
        let mut state = self.inner.state.lock().map_err(lock_error)?;
        let exit_deadline = Instant::now() + Duration::from_secs(60);
        if let Err(error) =
            manager.wait_exit_then_apply_updates(&candidate.asset, false, restart, restart_args)
        {
            let error = format!(
                "Local preparation completed, but the update helper could not start: {error}"
            );
            state.phase = "failed".into();
            state.error = Some(error.clone());
            return Err(error);
        }
        state.phase = "restart_pending".into();
        state.apply_lock = Some(file_lock);
        state.candidate = Some(candidate);
        let mut result = status_value(&state, &installation);
        result["quit_requested"] = json!(true);
        result["restart_requested"] = json!(restart);
        result["exit_required"] = json!(true);
        result["restart_pending"] = json!(true);
        result["exit_timeout_seconds"] = json!(60);
        result["message"] = json!(
            "Close this application or host within 60 seconds to let the verified update finish. Persistent MCP/HTTP hosts must exit normally; no process is killed automatically."
        );
        Ok((result, exit_deadline))
    }
    fn fail(&self, message: &str) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.phase = "failed".into();
            state.error = Some(message.into());
        }
    }
}

fn status_value(state: &State, installation: &Installation) -> Value {
    let identity = installation.value(if state.available {
        state.latest.as_deref()
    } else {
        None
    });
    let mut value = json!({"ok":state.phase != "failed","status":state.phase,
        "phase":state.phase,"current":crate::VERSION,"latest":state.latest,
        "update_available":state.available,"installation":identity,"progress":state.progress,
        "can_download":state.phase == "available" && state.candidate.is_some(),
        "can_apply":state.phase == "downloaded" && state.candidate.is_some(),"url":state.url});
    if let Some(candidate) = &state.candidate {
        value["staged_id"] = json!(candidate.staged_id);
    }
    if let Some(error) = &state.error {
        value["error"] = json!(error);
    }
    if let Some(command) = identity.get("manual_command") {
        value["manual_command"] = command.clone();
    }
    value
}
fn lock_error<T>(_: std::sync::PoisonError<T>) -> String {
    "Update state lock failed".into()
}
fn current_version() -> Result<Version, String> {
    Version::parse(crate::VERSION).map_err(|e| e.to_string())
}
fn stable_version(text: &str) -> Result<Version, String> {
    if text.len() > 80 {
        return Err("Release version is too long".into());
    }
    let version = Version::parse(text).map_err(|e| e.to_string())?;
    if !version.pre.is_empty() || !version.build.is_empty() {
        return Err("Only stable releases can be installed by this channel".into());
    }
    Ok(version)
}
fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}
fn staged_id(args: &Value) -> Result<String, String> {
    let id = args["staged_id"]
        .as_str()
        .ok_or("staged_id from updates.check/status is required")?;
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid staged_id".into());
    }
    Ok(id.into())
}
fn boolean(args: &Value, key: &str, default: bool) -> Result<bool, String> {
    match args.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("{key} must be a boolean")),
    }
}
fn require_id(candidate: &Candidate, id: &str) -> Result<(), String> {
    if candidate.staged_id != id {
        return Err("Selected update changed; check again and confirm the new staged_id".into());
    }
    Ok(())
}
fn validate_asset(asset: &VelopackAsset, expected: &str) -> Result<(), String> {
    stable_version(&asset.Version)?;
    let filename = &asset.FileName;
    if asset.PackageId != "bilibilive"
        || asset.Version != expected
        || !asset.Type.eq_ignore_ascii_case("Full")
        || !filename.starts_with("bilibilive-")
        || !filename.ends_with("-full.nupkg")
        || filename.len() > 200
        || filename.contains("..")
        || !filename
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || !(1..=PACKAGE_LIMIT).contains(&asset.Size)
        || asset.SHA256.len() != 64
        || !asset.SHA256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("Release feed has an invalid package identity, path, size or SHA-256".into());
    }
    Ok(())
}
fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err(format!(
            "Metadata exceeds its size limit: {}",
            path.display()
        ));
    }
    Ok(bytes)
}
fn allowed_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(
            url.host_str(),
            Some(
                "github.com"
                    | "api.github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
                    | "crates.io"
                    | "aur.archlinux.org"
            )
        )
}
fn http_client(timeout: Duration) -> Result<Client, String> {
    Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 8 || !allowed_url(attempt.url()) {
                attempt.error("Update redirect is outside its official HTTPS source")
            } else {
                attempt.follow()
            }
        }))
        .user_agent(concat!("bilibilive/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())
}
fn get_json(url: &str, deadline: Instant) -> Result<Value, String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
    if !allowed_url(&parsed) {
        return Err("Update URL is outside the official release source".into());
    }
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or("Update check timed out")?;
    let mut response = http_client(remaining)?
        .get(parsed)
        .send()
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    if response
        .content_length()
        .is_some_and(|n| n > METADATA_LIMIT)
    {
        return Err("Release metadata exceeds 2 MiB".into());
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(METADATA_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > METADATA_LIMIT {
        return Err("Release metadata exceeds 2 MiB".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("Invalid release metadata: {e}"))
}
fn check_release(
    installation: &Installation,
    deadline: Instant,
) -> Result<(String, String, Option<Candidate>), String> {
    if installation.kind == "cargo" && installation.source.as_deref() == Some("registry") {
        let result = get_json("https://crates.io/api/v1/crates/bilibilive", deadline)?;
        let latest = result["crate"]["max_stable_version"]
            .as_str()
            .ok_or("Registry response has no stable version")?;
        stable_version(latest)?;
        return Ok((
            latest.into(),
            format!("https://crates.io/crates/bilibilive/{latest}"),
            None,
        ));
    }
    if installation.kind == "aur" {
        let name = installation
            .source
            .as_deref()
            .ok_or("AUR installation has no package name")?;
        let result = get_json(
            &format!("https://aur.archlinux.org/rpc/v5/info?arg%5B%5D={name}"),
            deadline,
        )?;
        let records = result["results"]
            .as_array()
            .ok_or("AUR response has no results")?;
        let record = records
            .iter()
            .find(|x| x["Name"] == name)
            .ok_or("Installed package is unavailable in AUR")?;
        let published = record["Version"]
            .as_str()
            .ok_or("AUR response has no version")?;
        let base = published.split_once(':').map_or(published, |(_, v)| v);
        let base = base.rsplit_once('-').map_or(base, |(v, _)| v);
        let base = base.split_once(".r").map_or(base, |(v, _)| v);
        stable_version(base)?;
        return Ok((
            base.into(),
            format!("https://aur.archlinux.org/packages/{name}"),
            None,
        ));
    }
    let release = get_json(RELEASE_API, deadline)?;
    if release["draft"] != false || release["prerelease"] != false {
        return Err("Latest release is not a published stable release".into());
    }
    let tag = release["tag_name"].as_str().ok_or("Release has no tag")?;
    let latest = tag.strip_prefix('v').ok_or("Release tag has no v prefix")?;
    stable_version(latest)?;
    let url = format!("{REPOSITORY}/releases/tag/{tag}");
    if release["html_url"] != url {
        return Err("Release belongs to another repository".into());
    }
    if installation.kind != "velopack"
        || Version::parse(latest).map_err(|e| e.to_string())? <= current_version()?
    {
        return Ok((latest.into(), url, None));
    }
    let channel = installation
        .channel
        .as_deref()
        .ok_or("Managed installation has no channel")?;
    let feed_name = format!("releases.{channel}.json");
    let feed_url = format!("{REPOSITORY}/releases/download/{tag}/{feed_name}");
    let assets = release["assets"]
        .as_array()
        .ok_or("Release has no asset list")?;
    if !assets
        .iter()
        .any(|asset| asset["name"] == feed_name && asset["browser_download_url"] == feed_url)
    {
        return Err(format!(
            "Release {tag} has no installer update feed for {channel}; the current installation is unchanged"
        ));
    }
    let feed: VelopackAssetFeed =
        serde_json::from_value(get_json(&feed_url, deadline)?).map_err(|e| e.to_string())?;
    let matching = feed
        .Assets
        .into_iter()
        .filter(|a| a.Type.eq_ignore_ascii_case("Full") && a.Version == latest)
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err("Release feed must contain exactly one matching full package".into());
    }
    let asset = matching
        .into_iter()
        .next()
        .ok_or("Release package missing")?;
    validate_asset(&asset, latest)?;
    let candidate = Candidate::new(installation, asset)?;
    if !assets.iter().any(|asset| {
        asset["name"] == candidate.asset.FileName
            && asset["browser_download_url"] == candidate.package_url
            && asset["size"].as_u64() == Some(candidate.asset.Size)
    }) {
        return Err(
            "Update feed package does not match the public release asset and byte length".into(),
        );
    }
    Ok((latest.into(), url, Some(candidate)))
}

fn platform_channel() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("win-x64"),
        ("macos", "aarch64") => Some("osx-arm64"),
        ("macos", "x86_64") => Some("osx-x64"),
        _ => None,
    }
}
fn detect_installation(executable: &Path) -> Result<Installation, String> {
    let executable = executable.canonicalize().map_err(|e| e.to_string())?;
    let parent = executable
        .parent()
        .ok_or("Executable has no parent directory")?;
    let mut installation = Installation {
        kind: "portable".into(),
        executable: executable.clone(),
        root: parent.to_path_buf(),
        channel: None,
        portable: true,
        source: None,
        manual_program: None,
        manual_args: Vec::new(),
        managed: None,
        reason: Some(
            "Download the matching installer or portable archive from the official release page"
                .into(),
        ),
    };
    if let Some(channel) = platform_channel() {
        if let Some(config) = managed_paths(&executable) {
            match validate_managed(&config, channel) {
                Ok(()) => {
                    installation.kind = "velopack".into();
                    installation.root = config.RootAppDir.clone();
                    installation.channel = Some(channel.into());
                    installation.portable = config.IsPortable;
                    installation.managed = Some(config);
                    installation.reason = None;
                    return Ok(installation);
                }
                Err(error) => {
                    installation.kind = "unknown".into();
                    installation.reason = Some(error);
                    return Ok(installation);
                }
            }
        }
    }
    if parent.file_name().is_some_and(|x| x == "bin") {
        if let Some(root) = parent.parent() {
            let tracking = root.join(".crates2.json");
            if tracking.is_file() {
                match cargo_install(&read_limited(&tracking, METADATA_LIMIT)?, root) {
                    Ok((source, args)) => {
                        installation.kind = "cargo".into();
                        installation.root = root.to_path_buf();
                        installation.portable = false;
                        installation.source = Some(source);
                        installation.manual_program = Some("cargo".into());
                        installation.manual_args = args;
                        installation.reason = Some("Cargo owns this executable; updates must preserve its installed features and prefix".into());
                    }
                    Err(error) => {
                        installation.kind = "unknown".into();
                        installation.reason = Some(error);
                    }
                }
                return Ok(installation);
            }
        }
    }
    #[cfg(target_os = "linux")]
    if Path::new("/usr/bin/pacman").is_file() {
        if let Ok(owner) = bounded_output(
            Command::new("/usr/bin/pacman")
                .args(["-Qqo"])
                .arg(&executable),
            Duration::from_secs(2),
        ) {
            let name = owner.trim();
            if matches!(name, "bilibilive" | "bilibilive-git") {
                installation.kind = "aur".into();
                installation.portable = false;
                installation.source = Some(name.into());
                let helper = ["/usr/bin/paru", "/usr/bin/yay"]
                    .into_iter()
                    .find(|x| Path::new(x).is_file());
                if let Some(helper) = helper {
                    installation.manual_program = Some(helper.into());
                    installation.manual_args = vec!["-S".into(), name.into()];
                }
                installation.reason = Some(format!(
                    "AUR package {name} owns this executable; update it with your AUR helper or rebuild its published PKGBUILD"
                ));
                return Ok(installation);
            }
        }
    }
    if executable.starts_with("/usr")
        || executable.starts_with("/opt")
        || executable.starts_with("/nix")
    {
        installation.kind = "unknown".into();
        installation.portable = false;
        installation.reason = Some("This executable is in a managed system prefix; update it with its original package manager".into());
    }
    Ok(installation)
}
fn managed_paths(executable: &Path) -> Option<VelopackLocatorConfig> {
    #[cfg(target_os = "windows")]
    {
        let current = executable.parent()?;
        let root = current.parent()?;
        if current.file_name()? != "current" || !root.join("Update.exe").is_file() {
            return None;
        }
        return Some(VelopackLocatorConfig {
            RootAppDir: root.to_path_buf(),
            UpdateExePath: root.join("Update.exe"),
            PackagesDir: root.join("packages"),
            ManifestPath: current.join("sq.version"),
            CurrentBinaryDir: current.to_path_buf(),
            IsPortable: root.join(".portable").exists(),
        });
    }
    #[cfg(target_os = "macos")]
    {
        let current = executable.parent()?;
        let contents = current.parent()?;
        let root = contents.parent()?;
        if current.file_name()? != "MacOS"
            || contents.file_name()? != "Contents"
            || root.extension()? != "app"
            || !current.join("UpdateMac").is_file()
        {
            return None;
        }
        let manifest = if current.join("sq.version").is_file() {
            current.join("sq.version")
        } else {
            contents.join("Resources/sq.version")
        };
        let home = std::env::home_dir()?;
        return Some(VelopackLocatorConfig {
            RootAppDir: root.to_path_buf(),
            UpdateExePath: current.join("UpdateMac"),
            PackagesDir: home.join("Library/Caches/velopack/bilibilive/packages"),
            ManifestPath: manifest,
            CurrentBinaryDir: current.to_path_buf(),
            IsPortable: true,
        });
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = executable;
        None
    }
}
fn validate_managed(config: &VelopackLocatorConfig, channel: &str) -> Result<(), String> {
    if !config.UpdateExePath.is_file() {
        return Err("The installation update helper is missing".into());
    }
    let text = String::from_utf8(read_limited(&config.ManifestPath, METADATA_LIMIT)?)
        .map_err(|e| e.to_string())?;
    let manifest = bundle::read_manifest_from_string(&text).map_err(|e| e.to_string())?;
    let main = entry_for_channel(channel)?;
    if manifest.id != "bilibilive"
        || manifest.version != current_version()?
        || manifest.main_exe != main
        || manifest.channel != channel
    {
        return Err("Installed Velopack metadata does not match this bilibilive version, executable and platform channel".into());
    }
    Ok(())
}
fn entry_for_channel(channel: &str) -> Result<&'static str, String> {
    match channel {
        "win-x64" => Ok("bilibilive.exe"),
        "osx-x64" | "osx-arm64" => Ok("Contents/MacOS/bilibilive"),
        _ => Err("Unsupported installer update channel".into()),
    }
}
fn cargo_install(bytes: &[u8], root: &Path) -> Result<(String, Vec<String>), String> {
    let tracking: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let installs = tracking["installs"]
        .as_object()
        .ok_or("Cargo tracking has no installs object")?;
    let owners = installs
        .iter()
        .filter(|(_, v)| {
            v["bins"]
                .as_array()
                .is_some_and(|a| a.iter().any(|b| b == "bilibilive" || b == "bilibilive.exe"))
        })
        .collect::<Vec<_>>();
    if owners.len() != 1 {
        return Err("Cargo metadata does not unambiguously own this executable".into());
    }
    let (identity, record) = owners[0];
    let prefix = format!("bilibilive {} (", crate::VERSION);
    let source = identity
        .strip_prefix(&prefix)
        .and_then(|x| x.strip_suffix(')'))
        .ok_or("Cargo tracks a different package or version for this executable")?;
    let mut args = vec![
        "install".into(),
        "--locked".into(),
        "--root".into(),
        root.to_string_lossy().into_owned(),
    ];
    let kind = if matches!(
        source,
        "registry+https://github.com/rust-lang/crates.io-index"
            | "registry+https://index.crates.io/"
            | "sparse+https://index.crates.io/"
    ) {
        args.extend([
            "--registry".into(),
            "crates-io".into(),
            "--version".into(),
            "{version}".into(),
            "bilibilive".into(),
        ]);
        "registry"
    } else if let Some(source) = source.strip_prefix("git+") {
        let (url, revision) = source
            .rsplit_once('#')
            .ok_or("Cargo Git tracking has no revision")?;
        if revision.len() != 40 || !revision.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("Cargo Git revision is invalid".into());
        }
        let (url, selector) = url
            .split_once('?')
            .map_or((url, None), |(a, b)| (a, Some(b)));
        if !url
            .trim_end_matches('/')
            .trim_end_matches(".git")
            .eq_ignore_ascii_case(REPOSITORY)
            || selector.is_some_and(|s| !s.starts_with("tag=v"))
        {
            return Err("Pinned branches and third-party Cargo Git installations need their original update command".into());
        }
        if let Some(selector) = selector {
            stable_version(selector.trim_start_matches("tag=v"))?;
        }
        args.extend([
            "--git".into(),
            format!("{REPOSITORY}.git"),
            "--tag".into(),
            "v{version}".into(),
            "bilibilive".into(),
        ]);
        "git"
    } else {
        return Err("This Cargo source requires its original manual installation command".into());
    };
    args.extend(["--bin".into(), "bilibilive".into()]);
    for (key, flag) in [
        ("all_features", "--all-features"),
        ("no_default_features", "--no-default-features"),
    ] {
        match record[key].as_bool() {
            Some(true) => args.push(flag.into()),
            Some(false) => {}
            None => {
                return Err(format!(
                    "Cargo tracking omits {key}; cannot preserve installation options"
                ));
            }
        }
    }
    let features = record["features"]
        .as_array()
        .ok_or("Cargo tracking omits installed features")?;
    let features = features
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|x| {
                    !x.is_empty()
                        && x.len() <= 128
                        && x.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_+-./:".contains(&b))
                })
                .map(str::to_owned)
                .ok_or_else(|| "Cargo feature metadata is invalid".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if features.len() > 64 {
        return Err("Cargo feature metadata exceeds its limit".into());
    }
    if !features.is_empty() {
        args.extend(["--features".into(), features.join(",")]);
    }
    for key in ["profile", "target"] {
        let value = record[key]
            .as_str()
            .filter(|x| {
                !x.is_empty()
                    && x.len() <= 128
                    && x.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            })
            .ok_or_else(|| format!("Cargo tracking has no valid {key}"))?;
        args.extend([format!("--{key}"), value.into()]);
    }
    Ok((kind.into(), args))
}
fn shell_command(program: &str, args: &[String]) -> String {
    let quote = |value: &str| {
        if !value.is_empty()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-./:=,".contains(&b))
        {
            value.to_owned()
        } else if cfg!(windows) {
            format!("'{}'", value.replace('\'', "''"))
        } else {
            format!("'{}'", value.replace('\'', "'\\''"))
        }
    };
    let mut result = vec![quote(program)];
    result.extend(args.iter().map(|x| quote(x)));
    if cfg!(windows) {
        result.insert(0, "&".into());
    }
    result.join(" ")
}
#[cfg(target_os = "linux")]
fn bounded_output(command: &mut Command, timeout: Duration) -> Result<String, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            if !status.success() {
                return Err("Package ownership lookup failed".into());
            }
            let mut bytes = Vec::new();
            child
                .stdout
                .take()
                .ok_or("Package lookup has no output")?
                .take(4097)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > 4096 {
                return Err("Package ownership lookup exceeded its output limit".into());
            }
            return String::from_utf8(bytes).map_err(|e| e.to_string());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Package ownership lookup timed out".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}
fn acquire_install_lock(packages: &Path) -> Result<File, String> {
    fs::create_dir_all(packages).map_err(|e| e.to_string())?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(packages.join(".bilibilive-update.lock"))
        .map_err(|e| e.to_string())?;
    file.try_lock()
        .map_err(|_| "Another bilibilive process owns the installation update lock".to_owned())?;
    Ok(file)
}
fn save_receipt(path: &Path, receipt: &DownloadReceipt) -> Result<(), String> {
    let parent = path.parent().ok_or("Update receipt has no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut nonce = [0; 8];
    getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
    let temporary = parent.join(format!(
        ".update-{}-{}.tmp",
        std::process::id(),
        u64::from_ne_bytes(nonce)
    ));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec(receipt).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        fs::rename(&temporary, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
fn update_options() -> UpdateOptions {
    UpdateOptions {
        AllowVersionDowngrade: false,
        ExplicitChannel: None,
        MaximumDeltasBeforeFallback: -1,
    }
}
fn mutation_config(mut config: VelopackLocatorConfig) -> Result<VelopackLocatorConfig, String> {
    // The official Windows locator supports protected installation directories
    // by moving its package cache and helper to the user's application data.
    // Only an explicitly authorized mutation initializes this SDK fallback.
    let locator = velopack::locator::VelopackLocator::new(&config).map_err(|e| e.to_string())?;
    config.PackagesDir = locator.get_packages_dir();
    config.UpdateExePath = locator.get_update_path();
    Ok(config)
}
fn download_candidate(
    config: &VelopackLocatorConfig,
    candidate: &Candidate,
    inner: Weak<Inner>,
) -> Result<(), String> {
    let path = config.PackagesDir.join(&candidate.asset.FileName);
    if path.exists() {
        // Velopack skips cached files without checking them. Never accept this
        // fast path unless the exact expected bytes and manifest already match.
        if verify_package(&path, candidate).is_ok() {
            return Ok(());
        }
        if !fs::symlink_metadata(&path)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_file()
        {
            return Err("Refusing to replace a non-regular cached package".into());
        }
        // An explicitly requested re-download may repair this one corrupt
        // application cache entry. No package can be applied through this path.
        fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    let source = PinnedSource {
        candidate: candidate.clone(),
        inner,
    };
    let manager = UpdateManager::new(source, Some(update_options()), Some(config.clone()))
        .map_err(|e| e.to_string())?;
    manager
        .download_updates(
            &UpdateInfo {
                TargetFullRelease: candidate.asset.clone(),
                BaseRelease: None,
                DeltasToTarget: Vec::new(),
                IsDowngrade: false,
            },
            None,
        )
        .map_err(|e| e.to_string())?;
    verify_package(&path, candidate)
}
fn verify_package(path: &Path, candidate: &Candidate) -> Result<(), String> {
    verify_file(path, &candidate.asset)?;
    let mut bundle = bundle::load_bundle_from_file(path)
        .map_err(|e| format!("Invalid downloaded update archive: {e}"))?;
    let names = bundle.get_file_names().map_err(|e| e.to_string())?;
    let mut seen = std::collections::BTreeSet::new();
    for name in &names {
        if name.len() > 1024
            || name.starts_with('/')
            || name.contains('\\')
            || name.contains(':')
            || name.split('/').any(|part| part == ".." || part == ".")
            || !seen.insert(name)
        {
            return Err("Downloaded update archive contains an unsafe or duplicate path".into());
        }
    }
    let manifest = bundle.read_manifest().map_err(|e| e.to_string())?;
    if manifest.id != "bilibilive"
        || manifest.version.to_string() != candidate.asset.Version
        || manifest.channel != candidate.channel
        || manifest.main_exe != entry_for_channel(&candidate.channel)?
    {
        return Err(
            "Downloaded update manifest does not match its confirmed version and channel".into(),
        );
    }
    Ok(())
}
fn verify_file(path: &Path, asset: &VelopackAsset) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.file_type().is_file() || metadata.len() != asset.Size {
        return Err("Downloaded update has an unexpected file type or byte length".into());
    }
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 65536];
    let mut size = 0;
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > asset.Size {
            return Err("Downloaded update grew during verification".into());
        }
        hasher.update(&buffer[..n]);
    }
    let digest = hasher
        .finalize()
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect::<String>();
    if size != asset.Size || !digest.eq_ignore_ascii_case(&asset.SHA256) {
        return Err("Downloaded update SHA-256 differs from the confirmed release".into());
    }
    Ok(())
}
#[derive(Clone)]
struct PinnedSource {
    candidate: Candidate,
    inner: Weak<Inner>,
}
impl UpdateSource for PinnedSource {
    fn get_release_feed(
        &self,
        channel: &str,
        _: &Manifest,
        _: &str,
    ) -> Result<VelopackAssetFeed, velopack::Error> {
        if channel != self.candidate.channel {
            return Err(velopack::Error::Io(std::io::Error::other(
                "Update channel changed",
            )));
        }
        Ok(VelopackAssetFeed {
            Assets: vec![self.candidate.asset.clone()],
        })
    }
    fn download_release_entry(
        &self,
        asset: &VelopackAsset,
        path: &Path,
        progress: Option<Sender<i16>>,
    ) -> Result<(), velopack::Error> {
        self.download(asset, path, progress)
            .map_err(|e| velopack::Error::Io(std::io::Error::other(e)))
    }
}
impl PinnedSource {
    fn download(
        &self,
        asset: &VelopackAsset,
        path: &Path,
        progress: Option<Sender<i16>>,
    ) -> Result<(), String> {
        if serde_json::to_value(asset).map_err(|e| e.to_string())?
            != serde_json::to_value(&self.candidate.asset).map_err(|e| e.to_string())?
        {
            return Err("Download attempted to select a different staged package".into());
        }
        let mut response = http_client(DOWNLOAD_TIMEOUT)?
            .get(&self.candidate.package_url)
            .send()
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?;
        if response.content_length().is_some_and(|n| n != asset.Size) {
            return Err("Download byte length differs from its confirmed release".into());
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        let mut buffer = [0; 65536];
        let mut size = 0;
        let deadline = Instant::now() + DOWNLOAD_TIMEOUT;
        loop {
            let inner = self
                .inner
                .upgrade()
                .ok_or("Application closed; download cancelled")?;
            if inner.cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
                return Err("Update download cancelled or timed out".into());
            }
            let n = response.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            size += n as u64;
            if size > asset.Size {
                return Err("Download exceeds the confirmed package size".into());
            }
            file.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
            let percent = ((size * 100) / asset.Size).min(99) as u8;
            if let Ok(mut state) = inner.state.lock() {
                state.progress = percent;
            }
            if let Some(progress) = &progress {
                let _ = progress.send(percent as i16);
            }
        }
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        verify_file(path, asset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temporary() -> PathBuf {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!(
            "bilibilive-updater-{}-{}",
            std::process::id(),
            u64::from_ne_bytes(nonce)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }
    fn installation() -> Installation {
        Installation {
            kind: "velopack".into(),
            executable: "/private/app/bilibilive".into(),
            root: "/private/app".into(),
            channel: Some("win-x64".into()),
            portable: false,
            source: None,
            manual_program: None,
            manual_args: Vec::new(),
            reason: None,
            managed: None,
        }
    }
    fn asset() -> VelopackAsset {
        VelopackAsset {
            PackageId: "bilibilive".into(),
            Version: "999.0.0".into(),
            Type: "Full".into(),
            FileName: "bilibilive-999.0.0-win-x64-full.nupkg".into(),
            SHA256: sha256(b"verified package"),
            Size: 16,
            ..Default::default()
        }
    }
    #[test]
    fn staged_identity_binds_version_hash_channel_url_and_installation() {
        let installation = installation();
        let candidate = Candidate::new(&installation, asset()).unwrap();
        candidate.validate(&installation).unwrap();
        let mut changed = candidate.clone();
        changed.asset.SHA256 = "a".repeat(64);
        assert!(changed.validate(&installation).is_err());
        changed = candidate.clone();
        changed.package_url = "https://example.com/update".into();
        assert!(changed.validate(&installation).is_err());
        changed = candidate.clone();
        changed.channel = "osx-arm64".into();
        assert!(changed.validate(&installation).is_err());
        let mut another = installation.clone();
        another.root = "/other/install".into();
        assert!(candidate.validate(&another).is_err());
        assert!(require_id(&candidate, &"0".repeat(64)).is_err());
    }
    #[test]
    fn feed_rejects_paths_unhashed_packages_and_downgrade_receipts() {
        for filename in [
            "../bilibilive-full.nupkg",
            "bilibilive-%2e-full.nupkg",
            "bilibilive-\\evil-full.nupkg",
        ] {
            let mut asset = asset();
            asset.FileName = filename.into();
            assert!(validate_asset(&asset, "999.0.0").is_err());
        }
        let mut modified = asset();
        modified.SHA256.clear();
        assert!(validate_asset(&modified, "999.0.0").is_err());
        modified = asset();
        modified.PackageId = "other".into();
        assert!(validate_asset(&modified, "999.0.0").is_err());
        modified = asset();
        modified.Version = "0.0.1".into();
        assert!(
            Candidate::new(&installation(), modified)
                .unwrap()
                .validate(&installation())
                .is_err()
        );
    }
    #[test]
    fn cached_package_is_rehashed_and_real_install_lock_excludes_other_actors() {
        let root = temporary();
        let path = root.join("package.nupkg");
        fs::write(&path, b"verified package").unwrap();
        verify_file(&path, &asset()).unwrap();
        fs::write(&path, b"corrupt! package").unwrap();
        assert!(verify_file(&path, &asset()).is_err());
        let lock = acquire_install_lock(&root).unwrap();
        assert!(acquire_install_lock(&root).is_err());
        drop(lock);
        drop(acquire_install_lock(&root).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn receipt_is_atomic_and_bound_to_the_current_installation() {
        let root = temporary();
        let updater = Updater::new(root.clone());
        let installation = installation();
        let candidate = Candidate::new(&installation, asset()).unwrap();
        let mut receipt = DownloadReceipt {
            candidate,
            phase: "downloading".into(),
            progress: 0,
            error: None,
        };
        save_receipt(&updater.receipt_path(&installation), &receipt).unwrap();
        receipt.phase = "downloaded".into();
        receipt.progress = 100;
        save_receipt(&updater.receipt_path(&installation), &receipt).unwrap();
        assert_eq!(
            updater
                .load_receipt(&installation)
                .unwrap()
                .unwrap()
                .progress,
            100
        );
        receipt.candidate.package_url = "https://untrusted.invalid".into();
        save_receipt(&updater.receipt_path(&installation), &receipt).unwrap();
        assert!(updater.load_receipt(&installation).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cargo_tracking_keeps_root_features_profile_target_and_never_force() {
        let tracking = json!({"installs":{format!("bilibilive {} (registry+https://github.com/rust-lang/crates.io-index)",crate::VERSION):{
            "bins":["bilibilive"],"features":["gui"],"all_features":false,"no_default_features":true,
            "profile":"release","target":"x86_64-unknown-linux-gnu"}}});
        let (source, args) = cargo_install(
            &serde_json::to_vec(&tracking).unwrap(),
            Path::new("/private prefix"),
        )
        .unwrap();
        assert_eq!(source, "registry");
        assert!(args.contains(&"/private prefix".into()));
        assert!(args.contains(&"--no-default-features".into()));
        assert!(args.contains(&"gui".into()));
        assert!(args.contains(&"x86_64-unknown-linux-gnu".into()));
        assert!(!args.contains(&"--force".into()));
        let mut other = tracking;
        other["installs"].as_object_mut().unwrap().insert(
            "another 1.0.0 (registry+x)".into(),
            json!({"bins":["bilibilive"]}),
        );
        assert!(cargo_install(&serde_json::to_vec(&other).unwrap(), Path::new("/prefix")).is_err());
    }
    #[test]
    fn checking_rules_allow_only_official_https_and_mutations_require_confirmation() {
        for url in [
            "http://github.com/file",
            "https://github.com.evil.invalid/file",
            "https://user@github.com/file",
            "file:///tmp/pkg",
        ] {
            assert!(!allowed_url(&reqwest::Url::parse(url).unwrap()));
        }
        assert!(allowed_url(
            &reqwest::Url::parse("https://release-assets.githubusercontent.com/package").unwrap()
        ));
        let root = temporary();
        let updater = Updater::new(root.clone());
        assert!(updater.download(&json!({})).is_err());
        let prepared = std::cell::Cell::new(false);
        assert!(
            updater
                .apply_with_prepare(&json!({}), || {
                    prepared.set(true);
                    Ok(())
                })
                .is_err()
        );
        assert!(!prepared.get());
        assert!(!root.join("updates").exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn apply_validates_real_archive_before_retiring_sessions() {
        use base64::Engine;
        let root = temporary();
        let current = root.join("current");
        fs::create_dir_all(&current).unwrap();
        fs::write(
            root.join("Update.exe"),
            b"private fixture, not an executable",
        )
        .unwrap();
        fs::write(current.join("sq.version"), format!("<package><metadata><id>bilibilive</id><version>{}</version><channel>win-x64</channel><mainExe>bilibilive.exe</mainExe></metadata></package>", crate::VERSION)).unwrap();
        let config = VelopackLocatorConfig {
            RootAppDir: root.clone(),
            UpdateExePath: root.join("Update.exe"),
            PackagesDir: root.join("packages"),
            ManifestPath: current.join("sq.version"),
            CurrentBinaryDir: current.clone(),
            IsPortable: true,
        };
        fs::create_dir_all(&config.PackagesDir).unwrap();
        let mut installation = installation();
        installation.root = root.clone();
        installation.executable = current.join("bilibilive.exe");
        installation.managed = Some(config.clone());
        let bytes = base64::engine::general_purpose::STANDARD.decode("UEsDBBQAAAAIAM1DdV0MhEjwYAAAAJAAAAARAAAAYmlsaWJpbGl2ZS5udXNwZWNNjsEKgDAMQ7/IzYMIg9CbH1K1aFGrqOg+33mYeAgk75AEG3cTD0JY5OSeTyZoT63O+uoS+BRxyX7oahRCcKUr4TNAN7KZzHSrFbGu4DPAwmpNlF+Xk5j6Mk/um/T5xgNQSwMEFAAAAAgAzUN1XdE983wkAAAAIgAAABMAAABsaWIvYXBwL2ZpeHR1cmUudHh0CyjKLEssSVUoSS0uUUhJLEm0VshLLUstUkitSE0uLUlN0QMAUEsBAhQDFAAAAAgAzUN1XQyESPBgAAAAkAAAABEAAAAAAAAAAAAAAIABAAAAAGJpbGliaWxpdmUubnVzcGVjUEsBAhQDFAAAAAgAzUN1XdE983wkAAAAIgAAABMAAAAAAAAAAAAAAIABjwAAAGxpYi9hcHAvZml4dHVyZS50eHRQSwUGAAAAAAIAAgCAAAAA5AAAAAAA").unwrap();
        let mut package = asset();
        package.Size = bytes.len() as u64;
        package.SHA256 = sha256(&bytes);
        let candidate = Candidate::new(&installation, package).unwrap();
        let path = config.PackagesDir.join(&candidate.asset.FileName);
        fs::write(&path, &bytes).unwrap();
        verify_package(&path, &candidate).unwrap();
        let updater = Updater::new(root.join("profile"));
        updater.inner.state.lock().unwrap().installation = Some(installation.clone());
        save_receipt(
            &updater.receipt_path(&installation),
            &DownloadReceipt {
                candidate: candidate.clone(),
                phase: "downloaded".into(),
                progress: 100,
                error: None,
            },
        )
        .unwrap();
        let called = std::cell::Cell::new(false);
        let result = updater.apply_with_prepare(&json!({"staged_id":candidate.staged_id}), || {
            called.set(true);
            Err("fixture refuses to launch any helper".into())
        });
        assert!(called.get());
        assert!(result.unwrap_err().contains("fixture refuses"));
        fs::write(&path, vec![0; bytes.len()]).unwrap();
        called.set(false);
        assert!(
            updater
                .apply_with_prepare(&json!({"staged_id":candidate.staged_id}), || {
                    called.set(true);
                    Ok(())
                })
                .is_err()
        );
        assert!(!called.get());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn structured_failure_is_rejected_by_every_shared_frontend() {
        let updater = Updater::new(PathBuf::from("private-relative-profile"));
        assert!(updater.directory.is_absolute());
        let installation = installation();
        let mut state = updater.inner.state.lock().unwrap();
        for phase in [
            "idle",
            "manual",
            "up_to_date",
            "available",
            "downloading",
            "downloaded",
            "restart_pending",
        ] {
            state.phase = phase.into();
            assert!(
                crate::app::is_success(&status_value(&state, &installation)),
                "{phase}"
            );
        }
        state.phase = "failed".into();
        state.error = Some("private transport fixture".into());
        let value = status_value(&state, &installation);
        assert_eq!(value["ok"], false);
        assert_eq!(value["status"], "failed");
        assert!(!crate::app::is_success(&value));
        assert!(!updater.directory.exists());
    }
    #[test]
    fn manual_installations_never_suggest_a_premature_or_downgrade_command() {
        let updater = Updater::new(PathBuf::from("unused-profile"));
        let mut installation = installation();
        installation.kind = "cargo".into();
        installation.manual_program = Some("cargo".into());
        installation.manual_args = vec![
            "install".into(),
            "--version".into(),
            "{version}".into(),
            "bilibilive".into(),
        ];
        let mut state = updater.inner.state.lock().unwrap();
        state.phase = "up_to_date".into();
        state.latest = Some("0.0.1".into());
        state.available = false;
        let value = status_value(&state, &installation);
        assert!(value.get("manual_command").is_none());
        assert!(value["installation"].get("manual_args").is_none());
        state.latest = Some("999.0.0".into());
        state.available = true;
        state.phase = "manual".into();
        let value = status_value(&state, &installation);
        assert!(
            value["manual_command"]
                .as_str()
                .unwrap()
                .contains("--version 999.0.0")
        );
        assert!(crate::app::is_success(&value));
    }
}
