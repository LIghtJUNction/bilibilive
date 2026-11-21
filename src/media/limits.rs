//! Validate server codec selection before any encoder emits media.
//! Level tables and policy follow StartLive's rtc_h264.py / rtc_av1.py (GPL-3.0).
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum VideoLimits {
    H264 {
        level: u8,
        mbps: u64,
        fs: u64,
        dpb: u64,
        bitrate_kbps: u64,
    },
    Av1 {
        level: u8,
        pixels: u64,
        width: u64,
        height: u64,
        pixel_rate: u64,
        bitrate_kbps: u64,
    },
}

fn rank(level: u8) -> u16 {
    if level == 9 {
        105
    } else {
        u16::from(level) * 10
    }
}
fn h264(level: u8) -> Result<VideoLimits, String> {
    let (mbps, fs, dpb, bitrate) = match level {
        10 => (1485, 99, 396, 64),
        9 => (1485, 99, 396, 128),
        11 => (3000, 396, 900, 192),
        12 => (6000, 396, 2376, 384),
        13 => (11880, 396, 2376, 768),
        20 => (11880, 396, 2376, 2000),
        21 => (19800, 792, 4752, 4000),
        22 => (20250, 1620, 8100, 4000),
        30 => (40500, 1620, 8100, 10000),
        31 => (108000, 3600, 18000, 14000),
        32 => (216000, 5120, 20480, 20000),
        40 => (245760, 8192, 32768, 20000),
        41 => (245760, 8192, 32768, 50000),
        42 => (522240, 8704, 34816, 50000),
        50 => (589824, 22080, 110400, 135000),
        51 => (983040, 36864, 184320, 240000),
        52 => (2073600, 36864, 184320, 240000),
        _ => return Err("RTC answer selected an unsupported H.264 level".into()),
    };
    Ok(VideoLimits::H264 {
        level,
        mbps,
        fs,
        dpb,
        bitrate_kbps: bitrate,
    })
}
fn av1(level: u8) -> Result<VideoLimits, String> {
    let (pixels, width, height, pixel_rate, bitrate) = match level {
        0 => (147456, 2048, 1152, 4423680, 1500),
        1 => (278784, 2816, 1584, 8363520, 3000),
        4 => (665856, 4352, 2448, 19975680, 6000),
        5 => (1065024, 5504, 3096, 31950720, 10000),
        8 => (2359296, 6144, 3456, 70778880, 12000),
        9 => (2359296, 6144, 3456, 141557760, 20000),
        12 => (8912896, 8192, 4352, 267386880, 30000),
        13 => (8912896, 8192, 4352, 534773760, 40000),
        14 | 15 => (8912896, 8192, 4352, 1069547520, 60000),
        16 => (35651584, 16384, 8704, 1069547520, 60000),
        17 => (35651584, 16384, 8704, 2139095040, 100000),
        18 | 19 => (35651584, 16384, 8704, 4278190080, 160000),
        _ => return Err("RTC answer selected an undefined AV1 level".into()),
    };
    Ok(VideoLimits::Av1 {
        level,
        pixels,
        width,
        height,
        pixel_rate,
        bitrate_kbps: bitrate,
    })
}

fn decimal(parameters: &BTreeMap<String, String>, key: &str, default: u64) -> Result<u64, String> {
    match parameters.get(key) {
        None => Ok(default),
        Some(value) if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
            value
                .parse()
                .map_err(|_| format!("RTC {key} exceeds numeric bounds"))
        }
        _ => Err(format!("RTC {key} must be an unsigned decimal integer")),
    }
}

pub(crate) fn from_answer(
    codec: &str,
    mid: &str,
    payload: u8,
    sdp: &str,
) -> Result<VideoLimits, String> {
    let mut sections = Vec::<Vec<&str>>::new();
    for line in sdp.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if line.starts_with("m=") {
            sections.push(Vec::new());
        }
        if let Some(section) = sections.last_mut() {
            section.push(line);
        }
    }
    let mid_line = format!("a=mid:{mid}");
    let section = sections
        .iter()
        .find(|section| section.contains(&mid_line.as_str()))
        .ok_or("RTC video answer omitted the publication MID")?;
    let media = section[0].split_whitespace().collect::<Vec<_>>();
    if media.len() < 4
        || media[0] != "m=video"
        || media[1] == "0"
        || !media[3..].contains(&payload.to_string().as_str())
    {
        return Err("RTC answer rejected the requested video payload".into());
    }
    if section.contains(&"a=inactive") || section.contains(&"a=sendonly") {
        return Err("RTC server did not accept receiving the local publication".into());
    }
    let expected = format!("a=rtpmap:{payload} {codec}/90000");
    if !section
        .iter()
        .any(|line| line.eq_ignore_ascii_case(&expected))
    {
        return Err("RTC answer changed the selected codec, payload or clock".into());
    }
    let prefix = format!("a=fmtp:{payload} ");
    let mut parameters = BTreeMap::new();
    for line in section
        .iter()
        .filter_map(|line| line.strip_prefix(prefix.as_str()))
    {
        for field in line.split(';') {
            if let Some((name, value)) = field.trim().split_once('=') {
                parameters.insert(name.trim().to_owned(), value.trim().to_owned());
            }
        }
    }
    if codec == "av1" {
        if decimal(&parameters, "profile", 0)? > 2 || decimal(&parameters, "tier", 0)? > 1 {
            return Err("RTC answer contains invalid AV1 profile/tier".into());
        }
        let remote = decimal(&parameters, "level-idx", 5)?;
        if remote > 31 {
            return Err("RTC answer contains invalid AV1 level-idx".into());
        }
        if remote != 31 {
            av1(remote as u8)?;
        }
        return av1(remote.min(9) as u8);
    }
    if codec != "h264" {
        return Err("Unsupported publication codec".into());
    }
    let profile = parameters
        .get("profile-level-id")
        .map(String::as_str)
        .unwrap_or("42000a");
    if profile.len() != 6 || !profile.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("RTC H.264 profile-level-id is invalid".into());
    }
    let value = u32::from_str_radix(profile, 16).map_err(|_| "Invalid H.264 profile")?;
    let iop = ((value >> 8) & 255) as u8;
    let level = (value & 255) as u8;
    if value >> 16 != 66 || iop & 0x4f != 0 {
        return Err("RTC server changed the offered H.264 Baseline profile".into());
    }
    let level = if level == 11 && iop & 16 != 0 {
        9
    } else {
        level
    };
    h264(level)?;
    if decimal(&parameters, "packetization-mode", 0)? != 1 {
        return Err("RTC server does not support H.264 packetization-mode 1".into());
    }
    let asymmetric = decimal(&parameters, "level-asymmetry-allowed", 0)?;
    if asymmetric > 1 || asymmetric == 0 && rank(level) > rank(31) {
        return Err("RTC H.264 level violates symmetric negotiation".into());
    }
    let mut receive_level = level;
    if let Some(value) = parameters.get("max-recv-level") {
        if value.len() != 4 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("RTC H.264 max-recv-level must contain four hexadecimal digits".into());
        }
        let raw = u16::from_str_radix(value, 16).map_err(|_| "Invalid max-recv-level")?;
        let iop = (raw >> 8) as u8;
        let idc = raw as u8;
        if idc == 9 && iop & 16 != 0 {
            return Err("RTC H.264 max-recv-level has an invalid level 1b marker".into());
        }
        receive_level = if idc == 9 || idc == 11 && iop & 16 != 0 {
            9
        } else {
            idc
        };
        h264(receive_level)?;
        if rank(receive_level) <= rank(level) {
            return Err("RTC H.264 max-recv-level does not increase the base level".into());
        }
    }
    let selected = if asymmetric == 1 {
        receive_level
    } else {
        level
    };
    h264(if rank(selected) < rank(31) {
        selected
    } else {
        31
    })
}

impl VideoLimits {
    pub fn h264_default() -> Self {
        h264(31).expect("constant H.264 level")
    }
    pub fn snapshot(&self) -> Value {
        match self {
            Self::H264 {
                level,
                mbps,
                fs,
                dpb,
                bitrate_kbps,
            } => {
                json!({"codec":"h264","level":level,"max_mbps":mbps,"max_fs":fs,"max_dpb":dpb,"max_bitrate_kbps":bitrate_kbps})
            }
            Self::Av1 {
                level,
                pixels,
                width,
                height,
                pixel_rate,
                bitrate_kbps,
            } => {
                json!({"codec":"av1","level_idx":level,"max_pixels":pixels,"max_width":width,"max_height":height,"max_pixel_rate":pixel_rate,"max_bitrate_kbps":bitrate_kbps})
            }
        }
    }
    /// Adapt only the outgoing H.264 copy. AV1 keeps its configured geometry.
    pub fn apply(&self, args: &mut Value) -> Result<(), String> {
        let mut width = args["width"].as_u64().unwrap_or(1280).clamp(2, 3840) / 2 * 2;
        let mut height = args["height"].as_u64().unwrap_or(720).clamp(2, 2160) / 2 * 2;
        let mut fps = args["fps"].as_u64().unwrap_or(30).clamp(1, 60);
        let mut bitrate = args["bitrate_kbps"]
            .as_u64()
            .unwrap_or(2500)
            .clamp(16, 20000);
        let (level, av1_codec) = match self {
            Self::H264 {
                level,
                mbps,
                fs,
                dpb,
                bitrate_kbps,
            } => {
                let fits = |w: u64, h: u64| {
                    let mw = w.div_ceil(16);
                    let mh = h.div_ceil(16);
                    mw * mh <= (*fs).min(*dpb).min(*mbps) && mw.max(mh).pow(2) <= 8 * fs
                };
                if !fits(width, height) {
                    let original_width = width;
                    let original_height = height;
                    let (mut low, mut high) = (1, width / 2);
                    while low < high {
                        let half = (low + high + 1) / 2;
                        let w = half * 2;
                        let h = (w * original_height / original_width / 2 * 2).max(2);
                        if fits(w, h) {
                            low = half;
                        } else {
                            high = half - 1;
                        }
                    }
                    width = low * 2;
                    height = (width * original_height / original_width / 2 * 2).max(2);
                }
                if !fits(width, height) {
                    return Err("Negotiated H.264 level cannot contain the outgoing picture".into());
                }
                fps = fps.min(mbps / (width.div_ceil(16) * height.div_ceil(16)));
                bitrate = bitrate.min(*bitrate_kbps);
                (*level, false)
            }
            Self::Av1 {
                level,
                pixels,
                width: max_width,
                height: max_height,
                pixel_rate,
                bitrate_kbps,
            } => {
                if width > *max_width
                    || height > *max_height
                    || width * height > *pixels
                    || width * height * fps > *pixel_rate
                {
                    return Err(
                        "Negotiated AV1 level cannot receive the configured resolution/frame rate"
                            .into(),
                    );
                }
                bitrate = bitrate.min(*bitrate_kbps);
                (*level, true)
            }
        };
        args["width"] = width.into();
        args["height"] = height.into();
        args["fps"] = fps.into();
        args["bitrate_kbps"] = bitrate.into();
        args["_encoder_level"] = if av1_codec {
            format!("{}.{}", 2 + level / 4, level % 4)
        } else if level == 9 {
            "1b".into()
        } else {
            format!("{}.{}", level / 10, level % 10)
        }
        .into();
        args["_negotiated_limits"] = self.snapshot();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn answer(codec: &str, fmtp: &str) -> String {
        format!(
            "m=video 9 UDP/TLS/RTP/SAVPF 96\r\na=mid:1\r\na=recvonly\r\na=rtpmap:96 {codec}/90000\r\na=fmtp:96 {fmtp}\r\n"
        )
    }
    #[test]
    fn h264_honors_receiver_downgrade_and_offer_ceiling() {
        let limits = from_answer(
            "h264",
            "1",
            96,
            &answer(
                "H264",
                "profile-level-id=42001e;packetization-mode=1;level-asymmetry-allowed=1",
            ),
        )
        .unwrap();
        let mut args = json!({"width":1920,"height":1080,"fps":60,"bitrate_kbps":20000});
        limits.apply(&mut args).unwrap();
        let w = args["width"].as_u64().unwrap();
        let h = args["height"].as_u64().unwrap();
        assert!(w.div_ceil(16) * h.div_ceil(16) <= 1620);
        assert!(w.div_ceil(16) * h.div_ceil(16) * args["fps"].as_u64().unwrap() <= 40500);
        assert_eq!(args["bitrate_kbps"], 10000);
        assert_eq!(args["_encoder_level"], "3.0");
        let higher = from_answer(
            "h264",
            "1",
            96,
            &answer(
                "H264",
                "profile-level-id=420028;packetization-mode=1;level-asymmetry-allowed=1",
            ),
        )
        .unwrap();
        assert_eq!(higher, VideoLimits::h264_default());
    }
    #[test]
    fn rejects_changed_codec_and_av1_oversize() {
        assert!(
            from_answer(
                "h264",
                "1",
                96,
                &answer("VP8", "profile-level-id=42001f;packetization-mode=1")
            )
            .is_err()
        );
        assert!(
            from_answer(
                "h264",
                "1",
                96,
                &answer("H264", "profile-level-id=42e01f;packetization-mode=1")
            )
            .is_err()
        );
        let limits = from_answer(
            "av1",
            "1",
            96,
            &answer("AV1", "profile=0;level-idx=5;tier=0"),
        )
        .unwrap();
        assert!(
            limits
                .apply(&mut json!({"width":1920,"height":1080,"fps":60}))
                .is_err()
        );
        assert!(
            limits
                .apply(&mut json!({"width":1280,"height":720,"fps":30}))
                .is_ok()
        );
    }
}
