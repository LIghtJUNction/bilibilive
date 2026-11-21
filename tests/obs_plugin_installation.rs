//! Local, private-directory fixtures; never launches OBS or a media program.
#![allow(dead_code)]
#[path = "../src/bundle.rs"]
mod bundle;
#[path = "../src/obs_plugin.rs"]
mod obs_plugin;
const VERSION: &str = env!("CARGO_PKG_VERSION");
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

struct Fixture {
    root: PathBuf,
    source: PathBuf,
    destination: PathBuf,
    manifest: Value,
}
impl Fixture {
    fn new() -> Self {
        let mut bytes = [0u8; 12];
        getrandom::fill(&mut bytes).unwrap();
        let path = std::env::temp_dir().join(format!(
            "bilibilive-plugin-fixture-{}",
            bundle::hash(&bytes)
        ));
        fs::create_dir(&path).unwrap();
        let root = fs::canonicalize(path).unwrap();
        let source = root.join("payload");
        let destination = root.join("obs plugins");
        let (folder, entry) = if cfg!(windows) {
            ("obs-bilibilive-pk", "obs-bilibilive-pk.dll")
        } else if cfg!(target_os = "macos") {
            (
                "obs-bilibilive-pk.plugin",
                "Contents/MacOS/obs-bilibilive-pk",
            )
        } else {
            ("obs-bilibilive-pk", "bin/64bit/obs-bilibilive-pk.so")
        };
        let prefix = format!("resources/obs-plugin/{folder}");
        let name = format!("{prefix}/{entry}");
        let path = source.join(&name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"local fixture plugin; never executable").unwrap();
        let mut files = json!({name:{"size":fs::metadata(&path).unwrap().len(),"sha256":bundle::hash(&fs::read(&path).unwrap()),"executable":false}});
        if cfg!(target_os = "macos") {
            let name = format!("{prefix}/Contents/Info.plist");
            fs::write(source.join(&name), b"fixture plist").unwrap();
            files[name] =
                json!({"size":13,"sha256":bundle::hash(b"fixture plist"),"executable":false});
        }
        let manifest = json!({"schema":1,"package":"bilibilive","version":VERSION,"target":bundle::target(),
            "plugin":{"root":prefix,"entrypoint":entry},"runtime":{"executables":{},"libraries":{}},"files":files,"provenance":{"test_fixture":true}});
        Self {
            root,
            source,
            destination,
            manifest,
        }
    }
    fn args(&self) -> Value {
        let bytes = serde_json::to_vec_pretty(&self.manifest).unwrap();
        fs::write(self.source.join("bundle-manifest.json"), &bytes).unwrap();
        json!({"bundle_root":self.source,"manifest_sha256":bundle::hash(&bytes),"destination":self.destination})
    }
    fn payload(&self) -> PathBuf {
        self.source
            .join(self.manifest["plugin"]["root"].as_str().unwrap())
            .join(self.manifest["plugin"]["entrypoint"].as_str().unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn readonly_status_does_not_create_destination() {
    let f = Fixture::new();
    let v = obs_plugin::status(&f.args()).unwrap();
    assert_eq!(v["state"], "not_installed");
    assert_eq!(v["can_install"], true);
    assert!(!f.destination.exists());
}
#[test]
fn install_and_repeat_verify_files_and_keep_previous_installation() {
    let f = Fixture::new();
    let args = f.args();
    let v = obs_plugin::install(&args).unwrap();
    assert_eq!(v["state"], "needs_restart");
    assert_eq!(v["restart_required"], true);
    assert!(v["backup_path"].is_null());
    let path = PathBuf::from(v["plugin_path"].as_str().unwrap());
    assert_eq!(fs::read(&path).unwrap(), fs::read(f.payload()).unwrap());
    let status = obs_plugin::status(&args).unwrap();
    assert_eq!(status["verified"], true);
    assert!(status["loaded_in_obs"].is_null());
    let next = obs_plugin::install(&args).unwrap();
    let backup = PathBuf::from(next["backup_path"].as_str().unwrap());
    assert!(backup.is_dir());
    assert!(path.is_file());
    assert!(!f.destination.join(".bilibilive-pk-install.lock").exists());
}
#[test]
fn changed_source_is_rejected_before_installation() {
    let f = Fixture::new();
    let args = f.args();
    fs::write(f.payload(), b"tampered").unwrap();
    assert!(
        obs_plugin::install(&args)
            .unwrap_err()
            .contains("integrity")
    );
    assert!(!f.destination.exists());
}
#[test]
fn manifest_requires_independent_hash_and_correct_identity() {
    let mut f = Fixture::new();
    let mut args = f.args();
    args.as_object_mut().unwrap().remove("manifest_sha256");
    assert!(obs_plugin::install(&args).unwrap_err().contains("requires"));
    let mut args = f.args();
    args["manifest_sha256"] = json!("0".repeat(64));
    assert!(obs_plugin::install(&args).unwrap_err().contains("SHA-256"));
    f.manifest["version"] = json!("0.0.0");
    assert!(
        obs_plugin::install(&f.args())
            .unwrap_err()
            .contains("version")
    );
    f.manifest["version"] = json!(VERSION);
    f.manifest["target"] = json!("other-target");
    assert!(
        obs_plugin::install(&f.args())
            .unwrap_err()
            .contains("architecture")
    );
}
#[test]
fn extra_or_missing_source_files_fail_closed() {
    let f = Fixture::new();
    let args = f.args();
    let extra = f.payload().parent().unwrap().join("extra.dll");
    fs::write(&extra, b"extra").unwrap();
    assert!(
        obs_plugin::install(&args)
            .unwrap_err()
            .contains("file list")
    );
    fs::remove_file(extra).unwrap();
    fs::remove_file(f.payload()).unwrap();
    assert!(obs_plugin::install(&args).is_err());
    assert!(!f.destination.exists());
}
#[test]
fn unknown_existing_installation_needs_explicit_replacement_and_is_retained() {
    let f = Fixture::new();
    let mut args = f.args();
    let result = obs_plugin::install(&args).unwrap();
    let directory = PathBuf::from(result["installation_directory"].as_str().unwrap());
    fs::write(directory.join("user-note.txt"), b"keep me").unwrap();
    assert!(
        obs_plugin::install(&args)
            .unwrap_err()
            .contains("unrecorded")
    );
    assert_eq!(
        fs::read(directory.join("user-note.txt")).unwrap(),
        b"keep me"
    );
    args["replace"] = json!(true);
    let replacement = obs_plugin::install(&args).unwrap();
    let backup = PathBuf::from(replacement["backup_path"].as_str().unwrap());
    assert_eq!(fs::read(backup.join("user-note.txt")).unwrap(), b"keep me");
}
#[test]
fn concurrent_installation_is_refused_without_removing_other_lock() {
    let f = Fixture::new();
    let args = f.args();
    fs::create_dir(&f.destination).unwrap();
    let path = f.destination.join(".bilibilive-pk-install.lock");
    fs::write(&path, b"other owner").unwrap();
    assert!(obs_plugin::install(&args).unwrap_err().contains("lock"));
    assert_eq!(fs::read(path).unwrap(), b"other owner");
}
#[test]
fn path_escape_and_case_collisions_are_rejected() {
    for name in [
        "../outside",
        "/absolute",
        "a/../b",
        "a\\b",
        "C:ads",
        "a//b",
        "a/NUL.dll",
        "a/com1",
        "a/filename.",
        "a/file ",
        "a/tab\tname",
    ] {
        assert!(bundle::relative(name).is_err(), "{name}");
    }
    let mut f = Fixture::new();
    let name = f.manifest["files"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    f.manifest["files"][name.to_ascii_uppercase()] = f.manifest["files"][&name].clone();
    assert!(obs_plugin::install(&f.args()).is_err());
}
#[test]
fn oversized_payload_is_rejected_without_large_allocation() {
    let mut f = Fixture::new();
    let name = f.manifest["files"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    f.manifest["files"][name]["size"] = json!(bundle::PLUGIN_LIMIT + 1);
    assert!(
        obs_plugin::install(&f.args())
            .unwrap_err()
            .contains("size limit")
    );
}
#[test]
fn source_destination_overlap_is_rejected() {
    let f = Fixture::new();
    let mut args = f.args();
    args["destination"] = json!(f.source.join("resources/obs-plugin"));
    assert!(obs_plugin::install(&args).unwrap_err().contains("overlap"));
}
#[test]
fn mac_application_and_portable_roots_have_identical_relative_layouts() {
    let roots = bundle::roots(std::path::Path::new(
        "/Applications/bilibilive.app/Contents/MacOS/bilibilive",
    ));
    assert_eq!(
        roots[0],
        PathBuf::from("/Applications/bilibilive.app/Contents/Resources/bilibilive")
    );
    assert_eq!(
        bundle::roots(std::path::Path::new("/private/payload/bilibilive")),
        vec![PathBuf::from("/private/payload")]
    );
}
#[cfg(unix)]
#[test]
fn source_and_destination_symlinks_are_rejected() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let args = f.args();
    let path = f.payload();
    fs::remove_file(&path).unwrap();
    symlink("/etc/passwd", &path).unwrap();
    assert!(obs_plugin::install(&args).unwrap_err().contains("symlink"));
    let f = Fixture::new();
    let args = f.args();
    symlink(f.root.join("missing"), &f.destination).unwrap();
    assert!(obs_plugin::install(&args).unwrap_err().contains("symlink"));
}
