//! Offline LiveHime scene-collection conversion. Source files are never modified.
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MAX_CONFIG: u64 = 32 * 1024 * 1024;
const SOURCES: &[&str] = &[
    "scene",
    "group",
    "image_source",
    "color_source",
    "slideshow",
    "browser_source",
    "ffmpeg_source",
    "vlc_source",
    "text_gdiplus",
    "text_ft2_source",
    "monitor_capture",
    "window_capture",
    "game_capture",
    "dshow_input",
    "wasapi_input_capture",
    "wasapi_output_capture",
    "wasapi_process_output_capture",
];
const FILTERS: &[&str] = &[
    "mask_filter",
    "crop_filter",
    "gain_filter",
    "color_filter",
    "scale_filter",
    "scroll_filter",
    "color_key_filter",
    "chroma_key_filter",
    "sharpness_filter",
    "noise_gate_filter",
    "noise_suppress_filter",
    "compressor_filter",
    "limiter_filter",
    "expander_filter",
    "luma_key_filter",
    "clut_filter",
    "async_delay_filter",
    "gpu_delay",
    "invert_polarity_filter",
    "upward_compressor_filter",
    "hdr_tonemap_filter",
    "basic_eq_filter",
];
const TRANSITIONS: &[&str] = &[
    "cut_transition",
    "fade_transition",
    "swipe_transition",
    "slide_transition",
    "fade_to_color_transition",
    "wipe_transition",
    "obs_stinger_transition",
];
const SOURCE_FIELDS: &[&str] = &[
    "name",
    "uuid",
    "id",
    "versioned_id",
    "settings",
    "mixers",
    "sync",
    "flags",
    "volume",
    "balance",
    "enabled",
    "muted",
    "push-to-mute",
    "push-to-mute-delay",
    "push-to-talk",
    "push-to-talk-delay",
    "hotkeys",
    "deinterlace_mode",
    "deinterlace_field_order",
    "monitoring_type",
];
const ITEM_FIELDS: &[&str] = &[
    "name",
    "source_uuid",
    "visible",
    "locked",
    "rot",
    "align",
    "pos",
    "scale",
    "scale_ref",
    "bounds",
    "bounds_type",
    "bounds_align",
    "bounds_crop",
    "crop_left",
    "crop_top",
    "crop_right",
    "crop_bottom",
    "id",
    "group_item_backup",
    "scale_filter",
    "blend_method",
    "blend_type",
];
const AUDIO: &[(&str, &str)] = &[
    ("Desktop Audio", "DesktopAudioDevice1"),
    ("Desktop Audio 2", "DesktopAudioDevice2"),
    ("Mic/Aux", "AuxAudioDevice1"),
    ("Mic/Aux 2", "AuxAudioDevice2"),
    ("Mic/Aux 3", "AuxAudioDevice3"),
    ("Mic/Aux 4", "AuxAudioDevice4"),
];

fn objects<'a>(value: &'a Value, label: &str) -> Result<&'a [Value], String> {
    if value.is_null() {
        return Ok(&[]);
    }
    let array = value
        .as_array()
        .ok_or_else(|| format!("{label} must be an array of objects"))?;
    if array.iter().any(|item| !item.is_object()) {
        return Err(format!("{label} must contain only objects"));
    }
    Ok(array)
}
fn dimensions(value: &Value) -> Option<(u32, u32)> {
    let (x, y) = (value["x"].as_f64()?, value["y"].as_f64()?);
    if (1.0..=16384.0).contains(&x)
        && (1.0..=16384.0).contains(&y)
        && x.fract() == 0.0
        && y.fract() == 0.0
    {
        Some((x as u32, y as u32))
    } else {
        None
    }
}
fn keep(value: &Value, fields: &[&str]) -> Map<String, Value> {
    value
        .as_object()
        .map(|map| {
            map.iter()
                .filter(|(key, _)| fields.contains(&key.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

pub fn read_config(path: &Path) -> Result<Value, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(MAX_CONFIG + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_CONFIG {
        return Err("LiveHime configuration exceeds 32 MiB".into());
    }
    let text = if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        if bytes.len() % 2 != 0 {
            return Err("Incomplete UTF-16 configuration".into());
        }
        let little = bytes[0] == 0xff;
        let words: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|pair| {
                if little {
                    u16::from_le_bytes(pair.try_into().unwrap())
                } else {
                    u16::from_be_bytes(pair.try_into().unwrap())
                }
            })
            .collect();
        String::from_utf16(&words).map_err(|e| e.to_string())?
    } else {
        String::from_utf8(
            bytes
                .strip_prefix(&[0xef, 0xbb, 0xbf])
                .unwrap_or(&bytes)
                .to_vec(),
        )
        .map_err(|e| e.to_string())?
    };
    let value: Value =
        serde_json::from_str(&text).map_err(|e| format!("Invalid LiveHime JSON: {e}"))?;
    if !value.is_object() || !value["sources"].is_array() {
        return Err("LiveHime configuration requires a sources array".into());
    }
    Ok(value)
}

pub fn discover_configs(root: Option<&Path>) -> Result<Vec<PathBuf>, String> {
    let base = match root {
        Some(path) => path.to_path_buf(),
        None => match std::env::var_os("LOCALAPPDATA") {
            Some(path) => PathBuf::from(path).join("bililive/User Data"),
            None => return Ok(Vec::new()),
        },
    };
    if base.is_file() {
        return Ok(vec![base]);
    }
    let mut found = BTreeSet::new();
    for directory in [base.clone(), base.join("User Data")] {
        if !directory.is_dir() {
            continue;
        }
        let mut accounts = vec![directory.clone()];
        for entry in fs::read_dir(&directory)
            .map_err(|e| e.to_string())?
            .take(2048)
        {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_dir()
                && path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .chars()
                    .all(|c| c.is_ascii_digit())
            {
                accounts.push(path);
            }
        }
        for account in accounts {
            for file in ["untitled.json", "untitledv2.json"] {
                let path = account.join(file);
                if path.is_file() {
                    found.insert(path);
                }
            }
            let collection = account.join("Scene Collection");
            for folder in [collection, account]
                .iter()
                .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n == "Scene Collection"))
            {
                for entry in fs::read_dir(folder).map_err(|e| e.to_string())?.take(4096) {
                    let path = entry.map_err(|e| e.to_string())?.path();
                    if path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
                    {
                        found.insert(path);
                    }
                }
            }
        }
    }
    let mut found: Vec<_> = found.into_iter().collect();
    found.sort_by_key(|p| {
        (
            p.file_name()
                .is_none_or(|n| n != "support_group_collection.json"),
            std::cmp::Reverse(p.metadata().and_then(|m| m.modified()).ok()),
            p.clone(),
        )
    });
    Ok(found)
}

struct Converter<'a> {
    warnings: Vec<String>,
    skipped: usize,
    source_dir: Option<&'a Path>,
}
impl Converter<'_> {
    fn warn(&mut self, message: String) {
        if !self.warnings.contains(&message) {
            self.warnings.push(message);
        }
    }
    fn paths(&mut self, settings: &mut Value, label: &str) {
        for field in ["file", "local_file", "text_file", "image_path", "path"] {
            if let Some(value) = settings[field].as_str().map(str::to_owned) {
                if value.is_empty() || value.contains("://") {
                    continue;
                }
                let path = Path::new(&value);
                if path.is_absolute() {
                    if !path.exists() {
                        self.warn(format!("{label}: asset not found: {value}"));
                    }
                } else if let Some(candidate) = self
                    .source_dir
                    .map(|directory| directory.join(path))
                    .filter(|p| p.is_file())
                {
                    if let Ok(absolute) = candidate.canonicalize() {
                        settings[field] = absolute.to_string_lossy().as_ref().into();
                    }
                } else {
                    self.warn(format!(
                        "{label}: relative asset requires reassignment in OBS: {value}"
                    ));
                }
            }
        }
        for field in ["files", "playlist"] {
            if let Some(entries) = settings[field].as_array_mut() {
                for entry in entries {
                    if let Some(value) = entry["value"].as_str() {
                        let mut proxy = json!({"file":value});
                        self.paths(&mut proxy, label);
                        entry["value"] = proxy["file"].clone();
                    }
                }
            }
        }
    }
    fn plugin(
        &mut self,
        raw: &Value,
        allowed: &[&str],
        kind: &str,
        depth: usize,
    ) -> Result<Option<Value>, String> {
        if depth > 32 {
            return Err("LiveHime filter nesting exceeds 32".into());
        }
        let identifier = raw["id"].as_str().ok_or("Source lacks an id")?;
        let label = raw["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("Source lacks a name")?;
        let settings = raw.get("settings").cloned().unwrap_or_else(|| json!({}));
        if !settings.is_object() {
            return Err(format!("{label}: settings must be an object"));
        }
        if !allowed.contains(&identifier) {
            self.warn(format!(
                "Skipped {kind} {label} ({identifier}): unsupported OBS plugin"
            ));
            if kind == "source" {
                self.skipped += 1;
            }
            return Ok(None);
        }
        let mut output = Value::Object(keep(raw, SOURCE_FIELDS));
        output["settings"] = settings;
        for field in [
            "source_id",
            "hotkey_value",
            "audio_render_error",
            "audio_device_name",
        ] {
            output["settings"].as_object_mut().unwrap().remove(field);
        }
        if let Some(versioned) = output["versioned_id"].as_str() {
            let compatible = versioned == identifier
                || versioned
                    .strip_prefix(identifier)
                    .and_then(|s| s.strip_prefix("_v"))
                    .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()));
            if !compatible {
                output["versioned_id"] = identifier.into();
                self.warn(format!(
                    "{label}: restored compatible plugin id {identifier}"
                ));
            }
        }
        if let Some(mixers) = output["mixers"].as_u64() {
            output["mixers"] = (mixers & 63).into();
        }
        let mut filters = Vec::new();
        for filter in objects(&raw["filters"], "filters")? {
            if let Some(value) = self.plugin(filter, FILTERS, "filter", depth + 1)? {
                filters.push(value);
            }
        }
        if !filters.is_empty() {
            output["filters"] = filters.into();
        }
        self.paths(&mut output["settings"], label);
        if identifier == "vlc_source" {
            self.warn(format!("{label}: OBS requires the VLC source plugin"));
        }
        if identifier == "browser_source" {
            self.warn(format!(
                "{label}: browser login sessions are not included in the export"
            ));
        }
        Ok(Some(output))
    }
}

/// Returns the OBS collection and a structured report, suitable for CLI, MCP and GUI.
pub fn convert_collection(
    data: &Value,
    name: &str,
    source_dir: Option<&Path>,
    filter_prefixes: bool,
    resolution: Option<(u32, u32)>,
) -> Result<Value, String> {
    if !data.is_object() || !data["sources"].is_array() {
        return Err("Configuration requires a sources array".into());
    }
    if name.trim().is_empty() {
        return Err("Collection name must not be empty".into());
    }
    let mut converter = Converter {
        warnings: Vec::new(),
        skipped: 0,
        source_dir,
    };
    let mut raw = objects(&data["sources"], "sources")?.to_vec();
    raw.extend_from_slice(objects(&data["groups"], "groups")?);
    if filter_prefixes {
        let before = raw.len();
        raw.retain(|s| {
            s["id"] != "scene"
                || !s["name"].as_str().is_some_and(|n| {
                    ["Vertical", "wDouble", "xDouble"]
                        .iter()
                        .any(|prefix| n.starts_with(prefix))
                })
        });
        converter.warn(format!(
            "Filtered {} generated vertical/double-screen scenes",
            before - raw.len()
        ));
    }
    let mut sources = Vec::new();
    for source in &raw {
        if let Some(value) = converter.plugin(source, SOURCES, "source", 0)? {
            sources.push(value);
        }
    }
    let mut audio = Map::new();
    for (legacy, standard) in AUDIO {
        if let Some(value) = data.get(*standard).or_else(|| data.get(*legacy)) {
            if let Some(converted) = converter.plugin(value, SOURCES, "source", 0)? {
                audio.insert(standard.to_string(), converted);
            }
        }
    }
    let mut names = BTreeMap::<String, Option<String>>::new();
    let mut uuids = BTreeMap::<String, String>::new();
    for source in sources.iter().chain(audio.values()) {
        let name = source["name"].as_str().unwrap().to_string();
        let uuid = match source.get("uuid") {
            Some(Value::String(uuid)) => Some(uuid.clone()),
            None | Some(Value::Null) => None,
            _ => return Err(format!("{name}: UUID must be text")),
        };
        if names.insert(name.clone(), uuid.clone()).is_some() {
            return Err(format!("Duplicate source name: {name}"));
        }
        if let Some(uuid) = uuid.filter(|s| !s.is_empty()) {
            if uuids.insert(uuid, name.clone()).is_some() {
                return Err(format!("Duplicate source UUID: {name}"));
            }
        }
    }
    let scene_names: Vec<String> = sources
        .iter()
        .filter(|s| s["id"] == "scene")
        .map(|s| s["name"].as_str().unwrap().to_owned())
        .collect();
    if scene_names.is_empty() {
        return Err("Configuration contains no compatible scenes".into());
    }
    for source in &mut sources {
        if source["id"] != "scene" && source["id"] != "group" {
            continue;
        }
        let label = source["name"].as_str().unwrap().to_owned();
        let mut items = Vec::new();
        let raw_items = objects(&source["settings"]["items"], "scene items")?;
        for item in raw_items {
            if item["group_item_backup"].as_bool().unwrap_or(false) {
                continue;
            }
            for key in ["source_uuid", "name"] {
                if item
                    .get(key)
                    .is_some_and(|v| !v.is_string() && !v.is_null())
                {
                    return Err(format!("Scene item {key} must be text"));
                }
            }
            let target = item["source_uuid"]
                .as_str()
                .and_then(|uuid| uuids.get(uuid))
                .cloned()
                .or_else(|| {
                    item["name"]
                        .as_str()
                        .filter(|name| names.contains_key(*name))
                        .map(str::to_owned)
                });
            let Some(target) = target else {
                converter.warn(format!("{label}: skipped missing source reference"));
                continue;
            };
            for (absolute, relative) in [
                ("pos", "pos_rel"),
                ("scale", "scale_rel"),
                ("bounds", "bounds_rel"),
            ] {
                if item.get(relative).is_some() && item.get(absolute).is_none() {
                    return Err(
                        "Relative-only scene coordinates require the original canvas conversion"
                            .into(),
                    );
                }
            }
            let mut cleaned = Value::Object(keep(item, ITEM_FIELDS));
            cleaned["name"] = target.clone().into();
            if let Some(uuid) = &names[&target] {
                cleaned["source_uuid"] = uuid.clone().into();
            } else {
                cleaned.as_object_mut().unwrap().remove("source_uuid");
            }
            for field in ["show_transition", "hide_transition"] {
                if item[field]["id"].is_string() {
                    let mut transition = item[field].clone();
                    transition["settings"] = transition
                        .get("transition")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    if let Some(mut value) =
                        converter.plugin(&transition, TRANSITIONS, "transition", 0)?
                    {
                        let settings = value.as_object_mut().unwrap().remove("settings").unwrap();
                        value["transition"] = settings;
                        value["duration"] =
                            transition.get("duration").cloned().unwrap_or(json!(300));
                        cleaned[field] = value;
                    }
                }
            }
            items.push(cleaned);
        }
        if items.is_empty() && !raw_items.is_empty() {
            converter.warn(format!("{label}: all scene items were skipped"));
        }
        let mut settings = keep(
            &source["settings"],
            &["custom_size", "cx", "cy", "id_counter"],
        );
        settings.insert("items".into(), items.into());
        source["settings"] = settings.into();
    }
    let adjacency: BTreeMap<String, Vec<String>> = sources
        .iter()
        .filter(|s| s["id"] == "scene" || s["id"] == "group")
        .map(|s| {
            (
                s["name"].as_str().unwrap().to_owned(),
                s["settings"]["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|i| i["name"].as_str().unwrap().to_owned())
                    .collect(),
            )
        })
        .collect();
    fn visit(
        name: &str,
        graph: &BTreeMap<String, Vec<String>>,
        active: &mut BTreeSet<String>,
        done: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        if done.contains(name) {
            return Ok(());
        }
        if !active.insert(name.into()) {
            return Err(format!("Scene/group reference cycle: {name}"));
        }
        if active.len() > 128 {
            return Err("Scene nesting exceeds 128".into());
        }
        if let Some(children) = graph.get(name) {
            for child in children {
                visit(child, graph, active, done)?;
            }
        }
        active.remove(name);
        done.insert(name.into());
        Ok(())
    }
    let mut done = BTreeSet::new();
    for name in adjacency.keys() {
        visit(name, &adjacency, &mut BTreeSet::new(), &mut done)?;
    }
    let mut order = Vec::<String>::new();
    for entry in objects(&data["scene_order"], "scene_order")? {
        if let Some(name) = entry["name"].as_str() {
            if scene_names.iter().any(|s| s == name) && !order.iter().any(|s| s == name) {
                order.push(name.into());
            }
        }
    }
    for name in &scene_names {
        if !order.contains(name) {
            order.push(name.clone());
        }
    }
    let current = data["current_scene"]
        .as_str()
        .filter(|n| scene_names.iter().any(|s| s == n))
        .unwrap_or(&order[0]);
    if data["current_scene"] != current {
        converter.warn(
            "Selected the first retained scene because the current scene was unavailable".into(),
        );
    }
    let mut transitions = Vec::new();
    for raw in objects(&data["transitions"], "transitions")? {
        if let Some(value) = converter.plugin(raw, TRANSITIONS, "transition", 0)? {
            transitions.push(value);
        }
    }
    if transitions.is_empty() {
        transitions.push(json!({"name":"Fade","id":"fade_transition","settings":{}}));
    }
    let transition = data["current_transition"]
        .as_str()
        .filter(|name| transitions.iter().any(|t| t["name"] == *name))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            transitions
                .iter()
                .find(|t| t["id"] == "fade_transition")
                .unwrap_or(&transitions[0])["name"]
                .as_str()
                .unwrap()
                .to_owned()
        });
    let duration = data
        .get("transition_duration_v2")
        .or_else(|| data.get("transition_duration"))
        .and_then(Value::as_u64)
        .filter(|n| *n <= 20000)
        .unwrap_or(300);
    let source_count = sources
        .iter()
        .filter(|s| s["id"] != "scene" && s["id"] != "group")
        .count()
        + audio.len();
    let mut collection = json!({"name":name.trim(),"current_scene":current,"current_program_scene":current,"scene_order":order.iter().map(|name|json!({"name":name})).collect::<Vec<_>>(),"sources":sources.iter().filter(|s|s["id"]!="group").collect::<Vec<_>>(),"groups":sources.iter().filter(|s|s["id"]=="group").collect::<Vec<_>>(),"transitions":transitions,"current_transition":transition,"transition_duration":duration,"quick_transitions":[],"saved_projectors":[],"version":1});
    collection.as_object_mut().unwrap().extend(audio);
    let inferred = dimensions(&data["resolution"]).or_else(|| {
        let sizes: BTreeSet<_> = raw
            .iter()
            .filter(|s| s["id"] == "scene" && s["settings"]["custom_size"] != true)
            .flat_map(|s| s["settings"]["items"].as_array().into_iter().flatten())
            .filter_map(|i| dimensions(&i["scale_ref"]))
            .collect();
        if sizes.len() == 1 {
            sizes.into_iter().next()
        } else {
            None
        }
    });
    if let Some((width, height)) = resolution.or(inferred) {
        if !(1..=16384).contains(&width) || !(1..=16384).contains(&height) {
            return Err("Canvas dimensions must be within 1..16384".into());
        }
        collection["resolution"] = json!({"x":width,"y":height});
    } else {
        converter.warn(
            "Set OBS base canvas to the original LiveHime canvas; dimensions could not be inferred"
                .into(),
        );
    }
    if raw.iter().any(|s| s.get("private_settings").is_some()) {
        converter.warn("Private LiveHime source settings were excluded".into());
    }
    Ok(
        json!({"collection":collection,"warnings":converter.warnings,"scenes":scene_names.len(),"sources":source_count,"skipped":converter.skipped}),
    )
}

pub fn convert_file(
    input: &Path,
    output: &Path,
    name: &str,
    filter_prefixes: bool,
    resolution: Option<(u32, u32)>,
) -> Result<Value, String> {
    if !output
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
    {
        return Err("Export destination must end in .json".into());
    }
    let value = read_config(input)?;
    let mut result = convert_collection(&value, name, input.parent(), filter_prefixes, resolution)?;
    let bytes = serde_json::to_vec_pretty(&result["collection"]).map_err(|e| e.to_string())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| format!("Cannot create export {}: {e}", output.display()))?;
    if let Err(error) = file
        .write_all(&bytes)
        .and_then(|_| file.write_all(b"\n"))
        .and_then(|_| file.sync_all())
    {
        drop(file);
        let _ = fs::remove_file(output);
        return Err(error.to_string());
    }
    result["output"] = output.to_string_lossy().as_ref().into();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keeps_references_filters_and_audio() {
        let value = json!({"sources":[{"id":"scene","name":"Main","settings":{"items":[{"name":"Camera","source_uuid":"cam","pos":{"x":1,"y":2},"scale_ref":{"x":1920,"y":1080}},{"name":"Unknown"}]}},{"id":"dshow_input","name":"Camera","uuid":"cam","mixers":255,"settings":{"source_id":"private","video_device_id":"native-device"},"filters":[{"id":"gain_filter","name":"Gain","settings":{"db":3}}]},{"id":"unverified","name":"Unknown","settings":{}}],"current_scene":"Main","Desktop Audio":{"id":"wasapi_output_capture","name":"Desktop","settings":{}}});
        let result = convert_collection(&value, "Converted", None, false, None).unwrap();
        assert_eq!(result["scenes"], 1);
        assert_eq!(result["sources"], 2);
        assert_eq!(result["skipped"], 1);
        assert_eq!(
            result["collection"]["sources"][0]["settings"]["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(result["collection"]["sources"][1]["mixers"], 63);
        assert!(
            result["collection"]["sources"][1]["settings"]
                .get("source_id")
                .is_none()
        );
        assert_eq!(
            result["collection"]["resolution"],
            json!({"x":1920,"y":1080})
        );
    }
    #[test]
    fn refuses_cycles_and_duplicate_ids() {
        let cycle = json!({"sources":[{"id":"scene","name":"A","settings":{"items":[{"name":"B"}]}},{"id":"group","name":"B","settings":{"items":[{"name":"A"}]}}]});
        assert!(
            convert_collection(&cycle, "Test", None, false, None)
                .unwrap_err()
                .contains("cycle")
        );
        let duplicate = json!({"sources":[{"id":"scene","name":"A","settings":{}},{"id":"scene","name":"A","settings":{}}]});
        assert!(
            convert_collection(&duplicate, "Test", None, false, None)
                .unwrap_err()
                .contains("Duplicate")
        );
    }
}
