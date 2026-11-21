//! Small private operation receipts. Arguments, responses and credentials are never logged.
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const LIMIT: u64 = 1024 * 1024;

fn open(directory: &Path) -> Result<fs::File, String> {
    fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut options = fs::OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options
        .open(directory.join("operations.lock"))
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    Ok(lock)
}

pub fn record(directory: &Path, operation: &str, ok: bool) -> Result<(), String> {
    let _lock = open(directory)?;
    let path = directory.join("operations.jsonl");
    if fs::metadata(&path).is_ok_and(|m| m.len() >= LIMIT) {
        let previous = directory.join("operations.previous.jsonl");
        if previous.exists() {
            fs::remove_file(&previous).map_err(|e| e.to_string())?;
        }
        fs::rename(&path, previous).map_err(|e| e.to_string())?;
    }
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|e| e.to_string())?;
    let receipt = json!({"time_unix_ms":SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e|e.to_string())?.as_millis() as u64,"operation":operation,"ok":ok});
    writeln!(file, "{receipt}").map_err(|e| e.to_string())
}

pub fn recent(directory: &Path, limit: usize) -> Result<Value, String> {
    let _lock = open(directory)?;
    let mut records = Vec::new();
    for name in ["operations.previous.jsonl", "operations.jsonl"] {
        let path = directory.join(name);
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.to_string()),
        };
        if file.metadata().map_err(|e| e.to_string())?.len() > LIMIT + 4096 {
            return Err("Operation log exceeds its size limit".into());
        }
        let mut content = String::new();
        file.take(LIMIT + 4096)
            .read_to_string(&mut content)
            .map_err(|e| e.to_string())?;
        for line in content.lines() {
            if let Ok(value) = serde_json::from_str::<Value>(line) {
                records.push(value);
            }
        }
    }
    let keep = records.len().saturating_sub(limit.min(200));
    records.drain(..keep);
    Ok(json!({"directory":directory,"records":records,"arguments_logged":false}))
}

pub fn clear(directory: &Path) -> Result<Value, String> {
    let _lock = open(directory)?;
    for name in ["operations.previous.jsonl", "operations.jsonl"] {
        match fs::remove_file(directory.join(name)) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(json!({"cleared":true}))
}
