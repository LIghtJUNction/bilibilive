//! Install only an authenticated, same-version OBS payload. Never start/stop OBS.
use crate::bundle::{self, Bundle, FileRecord};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

const NAME: &str = "obs-bilibilive-pk";
const FILE_LIMIT: usize = 64;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    package: String,
    version: String,
    target: String,
    manifest_sha256: String,
    files: BTreeMap<String, FileRecord>,
}
struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
struct Stage(PathBuf);
impl Drop for Stage {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
struct StagedReceipt(PathBuf);
impl Drop for StagedReceipt {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn receipt_path(target: &Path) -> PathBuf {
    target.with_file_name(format!(".{}.bilibilive-installation.json", folder_name()))
}

fn validate_args(args: &Value, installing: bool) -> Result<(), String> {
    let values = args
        .as_object()
        .ok_or("OBS plugin arguments must be an object")?;
    for (name, value) in values {
        match name.as_str() {
            "bundle_root" | "manifest_sha256" | "destination" if value.is_string() => {}
            "replace" if installing && value.is_boolean() => {}
            _ => return Err(format!("Invalid OBS plugin argument: {name}")),
        }
    }
    Ok(())
}
fn folder_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "obs-bilibilive-pk.plugin"
    } else {
        NAME
    }
}
fn entrypoint() -> &'static str {
    if cfg!(windows) {
        "obs-bilibilive-pk.dll"
    } else if cfg!(target_os = "macos") {
        "Contents/MacOS/obs-bilibilive-pk"
    } else {
        "bin/64bit/obs-bilibilive-pk.so"
    }
}
fn home() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("Cannot determine the user's home directory")?;
    let path = PathBuf::from(home);
    bundle::no_links(&path)?;
    Ok(path)
}
fn destination(args: &Value) -> Result<PathBuf, String> {
    let parent = if let Some(path) = args["destination"].as_str() {
        PathBuf::from(path)
    } else if cfg!(windows) {
        PathBuf::from(std::env::var_os("ProgramData").ok_or(
            "Cannot determine the standard OBS plugin directory; provide destination explicitly for a portable installation"
        )?).join("obs-studio/plugins")
    } else if cfg!(target_os = "macos") {
        home()?.join("Library/Application Support/obs-studio/plugins")
    } else if cfg!(target_os = "linux") {
        let config = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => home()?.join(".config"),
        };
        config.join("obs-studio/plugins")
    } else {
        return Err("OBS plugin installation is not supported on this platform".into());
    };
    bundle::no_links(&parent)?;
    let result = parent.join(folder_name());
    bundle::no_links(&result)?;
    Ok(result)
}
fn verify_source(bundle: &Bundle) -> Result<Vec<(String, PathBuf)>, String> {
    let root = &bundle.manifest.plugin.root;
    if Path::new(root).file_name().and_then(|p| p.to_str()) != Some(folder_name())
        || bundle.manifest.plugin.entrypoint != entrypoint()
    {
        return Err("Native bundle has the wrong OBS plugin name or platform layout".into());
    }
    let files = bundle.verify_tree(root, bundle::PLUGIN_LIMIT, FILE_LIMIT)?;
    if cfg!(target_os = "macos")
        && !bundle
            .manifest
            .files
            .contains_key(&format!("{root}/Contents/Info.plist"))
    {
        return Err("The macOS OBS plugin bundle is missing Info.plist".into());
    }
    Ok(files)
}
fn distribution_plugin(args: &Value) -> Option<PathBuf> {
    if !cfg!(target_os = "linux") || args.get("destination").is_some() {
        return None;
    }
    [
        "/usr/lib/obs-plugins/obs-bilibilive-pk.so",
        "/usr/lib64/obs-plugins/obs-bilibilive-pk.so",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| {
        bundle::no_links(path).is_ok()
            && fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
    })
}
fn old_receipt(target: &Path) -> Result<Option<Receipt>, String> {
    let path = receipt_path(target);
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            let data = bundle::bounded_read(&path, 128 * 1024)?;
            let receipt: Receipt = serde_json::from_slice(&data)
                .map_err(|e| format!("Installed plugin receipt: {e}"))?;
            if receipt.schema != 1
                || receipt.package != "bilibilive"
                || receipt.target != bundle::target()
                || !bundle::valid_hash(&receipt.manifest_sha256)
                || receipt.files.len() > FILE_LIMIT
            {
                return Err(
                    "Installed plugin receipt does not identify this package and platform".into(),
                );
            }
            Ok(Some(receipt))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
fn verify_existing(target: &Path, receipt: &Receipt) -> Result<(), String> {
    let mut actual = BTreeSet::new();
    bundle::list_files(target, target, &mut actual, FILE_LIMIT + 1)?;
    let expected: BTreeSet<String> = receipt.files.keys().cloned().collect();
    if actual != expected {
        return Err("Existing plugin contains unrecorded files; it will not be replaced without replace=true".into());
    }
    let mut total = 0u64;
    for (name, record) in &receipt.files {
        total = total
            .checked_add(record.size)
            .ok_or("Installed plugin size overflow")?;
        if total > bundle::PLUGIN_LIMIT || !bundle::valid_hash(&record.sha256) {
            return Err("Installed plugin receipt exceeds bounds".into());
        }
        let bytes = bundle::bounded_read(&target.join(bundle::relative(name)?), record.size)?;
        if bytes.len() as u64 != record.size || bundle::hash(&bytes) != record.sha256 {
            return Err(
                "Existing plugin was modified; it will not be replaced without replace=true".into(),
            );
        }
    }
    Ok(())
}

/// The destination argument is the containing OBS plugins directory, not a DLL
/// or an application directory. All decisions are read-only in this operation.
pub fn status(args: &Value) -> Result<Value, String> {
    validate_args(args, false)?;
    let target = destination(args)?;
    let managed = distribution_plugin(args);
    let plugin = managed.clone().unwrap_or_else(|| target.join(entrypoint()));
    bundle::no_links(&plugin)?;
    let installed = fs::metadata(&plugin).is_ok_and(|m| m.is_file() && m.len() > 0);
    let mut issues = Vec::new();
    let receipt = match if managed.is_some() {
        Ok(None)
    } else {
        old_receipt(&target)
    } {
        Ok(value) => value,
        Err(error) => {
            issues.push(error);
            None
        }
    };
    let verified = if let Some(receipt) = &receipt {
        match verify_existing(&target, receipt) {
            Ok(()) => true,
            Err(error) => {
                issues.push(error);
                false
            }
        }
    } else {
        false
    };
    let mut source = Value::Null;
    let mut digest = Value::Null;
    let available = match bundle::selected(args).and_then(|b| verify_source(&b).map(|_| b)) {
        Ok(bundle) => {
            source = json!(bundle.root);
            digest = json!(bundle.sha256);
            true
        }
        Err(error) => {
            issues.push(error);
            false
        }
    };
    let can_install = available && managed.is_none();
    Ok(json!({
        "state":if installed { "installed" } else if can_install { "not_installed" } else { "unavailable" },
        "installed":installed,"verified":verified,"can_install":can_install,
        "plugin_path":plugin,"installation_directory":target,
        "version":receipt.as_ref().map(|r| &r.version),"source":source,"bundle_sha256":digest,
        "restart_required":false,"loaded_in_obs":Value::Null,
        "package_managed":managed.is_some(),
        "message":if managed.is_some() { "The plugin is installed in the distribution plugin directory; update it with the system package manager. OBS loading has not been inspected." } else if installed { "Plugin files are present. OBS loading has not been inspected; restart OBS after an installation." } else { "Install the verified bundled plugin, then restart OBS and enable Bilibilive PK capture in Tools." },
        "issues":issues
    }))
}

fn id() -> Result<String, String> {
    let mut bytes = [0; 12];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn write_receipt(path: &Path, receipt: &Receipt) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(receipt).map_err(|e| e.to_string())?;
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(&data)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}

/// Every rename is within one parent/filesystem. No old bytes are deleted until
/// the staged tree has been committed. A failed commit restores the old name;
/// if restoration itself is blocked, the exact old tree remains at backup.
fn commit_with<F>(stage: &Path, target: &Path, backup: &Path, mut rename: F) -> Result<bool, String>
where
    F: FnMut(&Path, &Path) -> std::io::Result<()>,
{
    let existed = fs::symlink_metadata(target).is_ok();
    if existed {
        rename(target, backup).map_err(|e| format!("The existing OBS plugin could not be staged for replacement; it is unchanged. Close OBS yourself if it has locked the plugin. {e}"))?;
    }
    if let Err(error) = rename(stage, target) {
        if existed && let Err(restore) = rename(backup, target) {
            return Err(format!(
                "Plugin installation failed: {error}. The original installation is preserved at {}; restoring its original name failed: {restore}",
                backup.display()
            ));
        }
        return Err(format!(
            "Plugin installation failed; the original installation was restored: {error}"
        ));
    }
    Ok(existed)
}

pub fn install(args: &Value) -> Result<Value, String> {
    validate_args(args, true)?;
    if distribution_plugin(args).is_some() {
        return Err("The OBS plugin is managed by the system package manager; update that package instead of installing a second user copy".into());
    }
    let bundle = bundle::selected(args)?;
    let files = verify_source(&bundle)?;
    let target = destination(args)?;
    if target.starts_with(&bundle.root) || bundle.root.starts_with(&target) {
        return Err("Plugin installation must not overlap its verified bundle source".into());
    }
    let parent = target.parent().ok_or("Invalid OBS plugin destination")?;
    fs::create_dir_all(parent).map_err(|e| format!("Cannot create the OBS plugin directory {}: {e}. No privilege elevation is performed; choose a supported writable portable destination or install manually.", parent.display()))?;
    bundle::no_links(parent)?;
    let lock = parent.join(".bilibilive-pk-install.lock");
    let mut lock_file = File::options()
        .write(true)
        .create_new(true)
        .open(&lock)
        .map_err(|e| {
            format!(
                "Cannot acquire the OBS plugin installation lock {}: {e}",
                lock.display()
            )
        })?;
    let _lock = Lock(lock);
    writeln!(lock_file, "{}", std::process::id()).map_err(|e| e.to_string())?;
    lock_file.sync_all().map_err(|e| e.to_string())?;
    bundle::no_links(&target)?;
    let replacing = fs::symlink_metadata(&target).is_ok();
    if replacing {
        if !fs::metadata(&target).map_err(|e| e.to_string())?.is_dir() {
            return Err("The OBS plugin destination is not a directory".into());
        }
        // Even explicit replacement never follows a link or accepts an
        // unbounded/special tree; old bytes remain available in the backup.
        let mut actual = BTreeSet::new();
        bundle::list_files(&target, &target, &mut actual, FILE_LIMIT + 1)?;
        if args["replace"].as_bool() != Some(true) {
            let receipt = old_receipt(&target)?.ok_or("An existing unrecorded plugin requires explicit replace=true; nothing has been changed")?;
            verify_existing(&target, &receipt)?;
        }
    }
    let token = id()?;
    let mut stage = Stage(parent.join(format!(".bilibilive-pk-stage-{token}")));
    let backup = parent.join(format!(".bilibilive-pk-backup-{token}"));
    let staged_receipt = StagedReceipt(parent.join(format!(".bilibilive-pk-receipt-{token}.json")));
    let saved_receipt = parent.join(format!(".bilibilive-pk-backup-{token}.json"));
    let final_receipt = receipt_path(&target);
    bundle::no_links(&final_receipt)?;
    fs::create_dir(&stage.0).map_err(|e| e.to_string())?;
    let mut records = BTreeMap::new();
    for (name, _) in files {
        let relative = Path::new(&name)
            .strip_prefix(&bundle.manifest.plugin.root)
            .map_err(|e| e.to_string())?;
        let output = stage.0.join(relative);
        fs::create_dir_all(output.parent().ok_or("Invalid plugin output path")?)
            .map_err(|e| e.to_string())?;
        bundle::copy_verified(&bundle, &name, &output)?;
        records.insert(
            relative
                .to_str()
                .ok_or("Plugin filename must be UTF-8")?
                .replace('\\', "/"),
            bundle.manifest.files[&name].clone(),
        );
    }
    // The directory is expected even for plugins without translated strings.
    if !cfg!(target_os = "macos") {
        fs::create_dir_all(stage.0.join("data/locale")).map_err(|e| e.to_string())?;
    }
    let receipt = Receipt {
        schema: 1,
        package: "bilibilive".into(),
        version: crate::VERSION.into(),
        target: bundle::target().into(),
        manifest_sha256: bundle.sha256.clone(),
        files: records,
    };
    // The receipt stays outside the plugin directory: signed macOS bundles
    // must retain their exact sealed member set after installation.
    write_receipt(&staged_receipt.0, &receipt)?;
    verify_existing(&stage.0, &receipt)?;
    let previous_metadata = if fs::symlink_metadata(&final_receipt).is_ok() {
        let bytes = bundle::bounded_read(&final_receipt, 128 * 1024)?;
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .open(&saved_receipt)
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        true
    } else {
        false
    };
    bundle::no_links(&target)?;
    let backed_up = commit_with(&stage.0, &target, &backup, |from, to| fs::rename(from, to))?;
    if let Err(error) = fs::rename(&staged_receipt.0, &final_receipt) {
        let moved_new = fs::rename(&target, &stage.0);
        let restored = if moved_new.is_ok() && backed_up {
            fs::rename(&backup, &target)
        } else {
            moved_new
        };
        return Err(if restored.is_ok() {
            format!(
                "Could not commit the plugin receipt; the original installation was restored: {error}"
            )
        } else {
            format!(
                "Could not commit the plugin receipt: {error}; original files remain at {} and original metadata at {}",
                backup.display(),
                saved_receipt.display()
            )
        });
    }
    stage.0 = PathBuf::new();
    // Preserve the previous directory for explicit rollback. It is not named
    // as a loadable plugin (including on macOS) and is never deleted on error.
    Ok(
        json!({"state":"needs_restart","installed":true,"verified":true,"can_install":true,
        "plugin_path":target.join(entrypoint()),"installation_directory":target,
        "version":crate::VERSION,"source":bundle.root,"bundle_sha256":bundle.sha256,
        "restart_required":true,"loaded_in_obs":Value::Null,
        "backup_path":if backed_up { Some(backup) } else { None },
        "backup_receipt_path":if previous_metadata { Some(saved_receipt) } else { None },
        "message":"Plugin files installed and verified. Restart OBS yourself, then enable Bilibilive PK capture in Tools. OBS was not started or stopped; a running OBS instance may still be using its previously loaded module.",
        "issues":[]}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commit_failure_restores_the_original_directory() {
        let parent =
            std::env::temp_dir().join(format!("bilibilive-plugin-rollback-{}", id().unwrap()));
        fs::create_dir(&parent).unwrap();
        let parent = fs::canonicalize(parent).unwrap();
        let stage = parent.join("stage");
        let target = parent.join("target");
        let backup = parent.join("backup");
        fs::create_dir(&stage).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(stage.join("plugin"), b"new").unwrap();
        fs::write(target.join("plugin"), b"old").unwrap();
        let result = commit_with(&stage, &target, &backup, |from, to| {
            if from == stage {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected commit failure",
                ))
            } else {
                fs::rename(from, to)
            }
        });
        assert!(result.unwrap_err().contains("restored"));
        assert_eq!(fs::read(target.join("plugin")).unwrap(), b"old");
        assert_eq!(fs::read(stage.join("plugin")).unwrap(), b"new");
        assert!(!backup.exists());
        fs::remove_dir_all(parent).unwrap();
    }
}
