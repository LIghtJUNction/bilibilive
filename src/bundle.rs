//! Verified, versioned native payloads. No download or global environment change.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
};

const MANIFEST_LIMIT: u64 = 1024 * 1024;
const PAYLOAD_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
pub const PLUGIN_LIMIT: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileRecord {
    pub size: u64,
    pub sha256: String,
    #[serde(default)]
    pub executable: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Plugin {
    pub root: String,
    pub entrypoint: String,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    #[serde(default = "bundled_mode")]
    pub mode: String,
    #[serde(default)]
    pub executables: BTreeMap<String, String>,
    #[serde(default)]
    pub libraries: BTreeMap<String, String>,
}
fn bundled_mode() -> String {
    "bundled".into()
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub package: String,
    pub version: String,
    pub target: String,
    pub plugin: Plugin,
    pub runtime: Runtime,
    pub files: BTreeMap<String, FileRecord>,
    /// Public source URLs, versions, immutable commits, licenses and build checks.
    pub provenance: Value,
}
#[derive(Clone, Debug)]
pub struct Bundle {
    pub root: PathBuf,
    pub sha256: String,
    pub manifest: Manifest,
}

pub fn target() -> &'static str {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else {
        "unsupported"
    }
}
pub fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn relative(value: &str) -> Result<PathBuf, String> {
    if value.is_empty() || value.contains(['\\', ':', '\0']) || value.len() > 512 {
        return Err("Invalid native payload relative path".into());
    }
    let path = Path::new(value);
    if path
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
        || value.split('/').any(|part| {
            let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
            part.is_empty()
                || part == "."
                || part == ".."
                || part.ends_with(['.', ' '])
                || part.chars().any(char::is_control)
                || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                    && stem.len() == 4
                    && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        })
    {
        return Err("Native payload paths cannot escape their root".into());
    }
    Ok(path.to_owned())
}
/// Inspect every existing path component, including a dangling symlink.
pub fn no_links(path: &Path) -> Result<(), String> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("An absolute path without parent traversal is required".into());
    }
    #[cfg(windows)]
    if !matches!(path.components().next(), Some(Component::Prefix(prefix))
        if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
        || !matches!(path.components().nth(1), Some(Component::RootDir))
    {
        return Err("Native payload paths must use a fully rooted local Windows drive, not a network or device namespace".into());
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        // A Windows drive prefix (C: or \\?\C:) is not the complete root.
        // Inspect the root after RootDir adds its separator, then every child.
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(meta) if is_link(&meta) => {
                return Err(format!(
                    "Native payload path contains a symlink: {}",
                    current.display()
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Inspect {}: {e}", current.display())),
        }
    }
    Ok(())
}
fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
pub fn bounded_read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    no_links(path)?;
    let file = File::open(path).map_err(|e| format!("Read {}: {e}", path.display()))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > limit {
        return Err(format!("{} is not a bounded regular file", path.display()));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 != meta.len() || bytes.len() as u64 > limit {
        return Err("Native payload changed while being read".into());
    }
    Ok(bytes)
}

impl Bundle {
    pub fn open(root: &Path, expected_hash: &str) -> Result<Self, String> {
        if !valid_hash(expected_hash) {
            return Err("A lowercase SHA-256 from the application build or an explicit trusted source is required".into());
        }
        no_links(root)?;
        let bytes = bounded_read(&root.join("bundle-manifest.json"), MANIFEST_LIMIT)?;
        if hash(&bytes) != expected_hash {
            return Err("Bundled native manifest does not match its trusted SHA-256".into());
        }
        let manifest: Manifest =
            serde_json::from_slice(&bytes).map_err(|e| format!("Native manifest: {e}"))?;
        if manifest.schema != 1
            || manifest.package != "bilibilive"
            || manifest.version != crate::VERSION
            || manifest.target != target()
            || target() == "unsupported"
        {
            return Err(
                "Native bundle package, version or architecture differs from this application"
                    .into(),
            );
        }
        let plugin_root = relative(&manifest.plugin.root)?;
        if !plugin_root.starts_with("resources/obs-plugin") || plugin_root.components().count() != 3
        {
            return Err("The plugin must have its own directory under resources/obs-plugin".into());
        }
        let entry = format!("{}/{}", manifest.plugin.root, manifest.plugin.entrypoint);
        relative(&manifest.plugin.entrypoint)?;
        if !manifest.files.contains_key(&entry) {
            return Err("Native manifest does not contain the plugin entrypoint".into());
        }
        if manifest.files.is_empty() || manifest.files.len() > 4096 {
            return Err("Native manifest has an invalid file count".into());
        }
        let mut total = 0u64;
        let mut names = BTreeSet::new();
        for (name, record) in &manifest.files {
            let path = relative(name)?;
            if !(path.starts_with("runtime") || path.starts_with(&plugin_root))
                || !valid_hash(&record.sha256)
                || record.size > 512 * 1024 * 1024
                || !names.insert(name.to_ascii_lowercase())
            {
                return Err("Native manifest has an unsafe, duplicate or oversized file".into());
            }
            total = total
                .checked_add(record.size)
                .ok_or("Native bundle size overflow")?;
        }
        if total > PAYLOAD_LIMIT {
            return Err("Native bundle exceeds its total size limit".into());
        }
        if !matches!(manifest.runtime.mode.as_str(), "bundled" | "system")
            || (manifest.runtime.mode == "system"
                && (!manifest.runtime.executables.is_empty()
                    || !manifest.runtime.libraries.is_empty()))
        {
            return Err("Invalid native runtime delivery mode".into());
        }
        for (name, path) in &manifest.runtime.executables {
            if !matches!(name.as_str(), "ffmpeg" | "ffplay")
                || !relative(path)?.starts_with("runtime/bin")
                || !manifest
                    .files
                    .get(path)
                    .is_some_and(|entry| entry.executable)
            {
                return Err("Native manifest has an invalid executable mapping".into());
            }
        }
        for (name, path) in &manifest.runtime.libraries {
            if name != "datachannel"
                || !relative(path)?.starts_with("runtime/lib")
                || !manifest.files.contains_key(path)
            {
                return Err("Native manifest has an invalid library mapping".into());
            }
        }
        Ok(Self {
            root: root.to_owned(),
            sha256: expected_hash.to_owned(),
            manifest,
        })
    }

    pub fn verified_path(&self, name: &str) -> Result<PathBuf, String> {
        let record = self
            .manifest
            .files
            .get(name)
            .ok_or("File is absent from native manifest")?;
        let path = self.root.join(relative(name)?);
        let bytes = bounded_read(&path, record.size)?;
        if bytes.len() as u64 != record.size || hash(&bytes) != record.sha256 {
            return Err(format!("Native payload integrity mismatch: {name}"));
        }
        #[cfg(unix)]
        if record.executable {
            use std::os::unix::fs::PermissionsExt;
            if fs::metadata(&path)
                .map_err(|e| e.to_string())?
                .permissions()
                .mode()
                & 0o111
                == 0
            {
                return Err(format!(
                    "Native executable has lost its executable mode: {name}"
                ));
            }
        }
        Ok(path)
    }

    pub fn verify_tree(
        &self,
        prefix: &str,
        limit: u64,
        count_limit: usize,
    ) -> Result<Vec<(String, PathBuf)>, String> {
        let prefix_path = relative(prefix)?;
        let wanted: BTreeSet<String> = self
            .manifest
            .files
            .keys()
            .filter(|p| Path::new(p).starts_with(&prefix_path))
            .cloned()
            .collect();
        if wanted.is_empty() || wanted.len() > count_limit {
            return Err(format!("Invalid native payload file count for {prefix}"));
        }
        let total: u64 = wanted.iter().map(|p| self.manifest.files[p].size).sum();
        if total > limit {
            return Err(format!(
                "Native payload exceeds the size limit for {prefix}"
            ));
        }
        let mut actual = BTreeSet::new();
        list_files(
            &self.root,
            &self.root.join(prefix_path),
            &mut actual,
            count_limit,
        )?;
        if actual != wanted {
            return Err(format!(
                "Native payload file list differs from its manifest: {prefix}"
            ));
        }
        wanted
            .into_iter()
            .map(|name| self.verified_path(&name).map(|p| (name, p)))
            .collect()
    }
}

pub fn list_files(
    root: &Path,
    path: &Path,
    output: &mut BTreeSet<String>,
    limit: usize,
) -> Result<(), String> {
    let mut entries_left = limit.saturating_add(1).saturating_mul(20);
    list_files_inner(root, path, output, limit, &mut entries_left)
}
fn list_files_inner(
    root: &Path,
    path: &Path,
    output: &mut BTreeSet<String>,
    limit: usize,
    entries_left: &mut usize,
) -> Result<(), String> {
    if *entries_left == 0 {
        return Err("Native payload directory entry count exceeds its limit".into());
    }
    *entries_left -= 1;
    no_links(path)?;
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if metadata.is_dir() {
        let relative = path.strip_prefix(root).map_err(|e| e.to_string())?;
        if relative.components().count() > 16 {
            return Err("Native payload directory nesting exceeds its limit".into());
        }
        let entries = fs::read_dir(path).map_err(|e| e.to_string())?;
        for (i, entry) in entries.enumerate() {
            if i > limit {
                return Err("Native payload directory has too many entries".into());
            }
            list_files_inner(
                root,
                &entry.map_err(|e| e.to_string())?.path(),
                output,
                limit,
                entries_left,
            )?;
        }
    } else if metadata.is_file() {
        let relative = path.strip_prefix(root).map_err(|e| e.to_string())?;
        let name = relative
            .to_str()
            .ok_or("Native payload path must be UTF-8")?
            .replace('\\', "/");
        self::relative(&name)?;
        output.insert(name);
        if output.len() > limit {
            return Err("Native payload file count exceeds its limit".into());
        }
    } else {
        return Err("Native payload contains a special file".into());
    }
    Ok(())
}

pub fn roots(executable: &Path) -> Vec<PathBuf> {
    let Some(parent) = executable.parent() else {
        return vec![];
    };
    let mut result = vec![parent.to_owned()];
    if parent.file_name().is_some_and(|p| p == "MacOS")
        && let Some(contents) = parent.parent()
    {
        result.insert(0, contents.join("Resources/bilibilive"));
    }
    result
}
pub fn discover() -> Result<Option<Bundle>, String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    for root in roots(&executable) {
        let manifest = root.join("bundle-manifest.json");
        if fs::symlink_metadata(&manifest).is_ok() {
            let trusted = option_env!("BILIBILIVE_BUNDLE_MANIFEST_SHA256")
                .ok_or("This application build has no trusted bundled manifest; use the package manager or an explicit bundle SHA-256")?;
            return Bundle::open(&root, trusted).map(Some);
        }
    }
    Ok(None)
}
pub fn selected(args: &Value) -> Result<Bundle, String> {
    if let Some(root) = args.get("bundle_root") {
        let root = root.as_str().ok_or("bundle_root must be a path string")?;
        let digest = args["manifest_sha256"]
            .as_str()
            .ok_or("An explicit bundle_root requires its trusted manifest_sha256")?;
        return Bundle::open(Path::new(root), digest);
    }
    if args.get("manifest_sha256").is_some() {
        return Err("manifest_sha256 requires an explicit bundle_root".into());
    }
    discover()?.ok_or("No verified native payload is bundled with this installation; use the system package manager or provide an explicit same-version bundle and trusted manifest SHA-256".into())
}

pub fn command(program: &str) -> Result<Command, String> {
    if !matches!(program, "ffmpeg" | "ffplay") {
        return Err("Unknown native runtime executable".into());
    }
    if let Some(bundle) = discover()? {
        if bundle.manifest.runtime.mode == "system" {
            return Ok(Command::new(program));
        }
        let executable = bundle
            .manifest
            .runtime
            .executables
            .get(program)
            .ok_or_else(|| format!("Verified bundle does not provide {program}"))?;
        bundle.verify_tree("runtime", PAYLOAD_LIMIT, 4096)?;
        let mut command = Command::new(bundle.verified_path(executable)?);
        // Only the child receives an explicit dependency search path. The
        // application and the user's shell environment remain unchanged.
        let mut paths = vec![
            bundle.root.join("runtime/bin"),
            bundle.root.join("runtime/lib"),
        ];
        if let Some(path) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&path));
        }
        command.env(
            "PATH",
            std::env::join_paths(paths).map_err(|e| e.to_string())?,
        );
        #[cfg(target_os = "linux")]
        command.env("LD_LIBRARY_PATH", bundle.root.join("runtime/lib"));
        // macOS builds use @loader_path install names, not DYLD_* overrides.
        Ok(command)
    } else {
        Ok(Command::new(program))
    }
}

#[derive(Clone, Debug)]
pub struct LibrarySelection {
    pub candidates: Vec<PathBuf>,
    pub source: &'static str,
    pub directory: Option<PathBuf>,
}
pub fn rtc_library() -> Result<LibrarySelection, String> {
    if let Some(path) = std::env::var_os("BILIBILIVE_RTC_LIBRARY") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err("BILIBILIVE_RTC_LIBRARY must be an explicit absolute path".into());
        }
        no_links(&path)?;
        return Ok(LibrarySelection {
            candidates: vec![path],
            source: "explicit-system-override",
            directory: None,
        });
    }
    if let Some(bundle) = discover()?.filter(|bundle| bundle.manifest.runtime.mode == "bundled") {
        let name = bundle
            .manifest
            .runtime
            .libraries
            .get("datachannel")
            .ok_or("Verified bundle does not provide libdatachannel")?;
        bundle.verify_tree("runtime", PAYLOAD_LIMIT, 4096)?;
        let path = bundle.verified_path(name)?;
        return Ok(LibrarySelection {
            directory: path.parent().map(Path::to_owned),
            candidates: vec![path],
            source: "verified-bundle",
        });
    }
    let names = if cfg!(windows) {
        vec!["datachannel.dll", "libdatachannel.dll"]
    } else if cfg!(target_os = "macos") {
        vec![
            "libdatachannel.0.dylib",
            "/opt/homebrew/lib/libdatachannel.0.dylib",
            "/usr/local/lib/libdatachannel.0.dylib",
            "libdatachannel.0.24.dylib",
        ]
    } else {
        vec!["libdatachannel.so.0.24"]
    };
    Ok(LibrarySelection {
        candidates: names.into_iter().map(PathBuf::from).collect(),
        source: "system-fallback",
        directory: None,
    })
}

/// Windows zsys uses LOAD_LIBRARY_SEARCH_DEFAULT_DIRS. A verified dependency
/// directory must be explicitly added; an absolute DLL path alone is insufficient.
pub struct LibrarySearch {
    #[cfg(windows)]
    cookie: *mut std::ffi::c_void,
}
impl LibrarySearch {
    pub fn new(selection: &LibrarySelection) -> Result<Self, String> {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            let cookie = if let Some(directory) = &selection.directory {
                let wide: Vec<u16> = directory.as_os_str().encode_wide().chain(Some(0)).collect();
                let value = unsafe { AddDllDirectory(wide.as_ptr()) };
                if value.is_null() {
                    return Err(format!(
                        "Register verified DLL directory: {}",
                        std::io::Error::last_os_error()
                    ));
                }
                value
            } else {
                std::ptr::null_mut()
            };
            Ok(Self { cookie })
        }
        #[cfg(not(windows))]
        {
            let _ = selection;
            Ok(Self {})
        }
    }
}
#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn AddDllDirectory(path: *const u16) -> *mut std::ffi::c_void;
    fn RemoveDllDirectory(cookie: *mut std::ffi::c_void) -> i32;
}
impl Drop for LibrarySearch {
    fn drop(&mut self) {
        #[cfg(windows)]
        if !self.cookie.is_null() {
            unsafe {
                RemoveDllDirectory(self.cookie);
            }
        }
    }
}

pub fn diagnostics() -> Value {
    match discover() {
        Ok(Some(bundle)) if bundle.manifest.runtime.mode == "system" => {
            json!({"source":"system-fallback","verified_manifest":true,"version":bundle.manifest.version,"target":bundle.manifest.target,"manifest_sha256":bundle.sha256,"message":"This package explicitly uses distribution-managed FFmpeg/FFplay and libdatachannel"})
        }
        Ok(Some(bundle)) => match bundle.verify_tree("runtime", PAYLOAD_LIMIT, 4096) {
            Ok(_) => {
                json!({"source":"verified-bundle","verified":true,"version":bundle.manifest.version,"target":bundle.manifest.target,"manifest_sha256":bundle.sha256,"root":bundle.root,"executables":bundle.manifest.runtime.executables,"libraries":bundle.manifest.runtime.libraries})
            }
            Err(error) => json!({"source":"invalid-bundle","verified":false,"error":error}),
        },
        Ok(None) => {
            json!({"source":"system-fallback","verified":false,"message":"No native payload is bundled; FFmpeg/FFplay use PATH and libdatachannel uses the system loader or BILIBILIVE_RTC_LIBRARY"})
        }
        Err(error) => json!({"source":"invalid-bundle","verified":false,"error":error}),
    }
}

/// Copy verified bytes using create_new; source changes cannot enter the stage.
pub fn copy_verified(bundle: &Bundle, name: &str, output: &Path) -> Result<(), String> {
    let record = bundle
        .manifest
        .files
        .get(name)
        .ok_or("Unlisted native payload")?;
    let bytes = bounded_read(&bundle.root.join(relative(name)?), record.size)?;
    if hash(&bytes) != record.sha256 || bytes.len() as u64 != record.size {
        return Err(format!(
            "Native payload changed before installation: {name}"
        ));
    }
    no_links(output)?;
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            output,
            fs::Permissions::from_mode(if record.executable { 0o755 } else { 0o644 }),
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}
