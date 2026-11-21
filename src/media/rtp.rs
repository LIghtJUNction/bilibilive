//! RTP continuity and RTCP feedback for the external native encoder.
//!
//! Each publication owns its sequence/timestamp space. Replacing FFmpeg for a
//! resize or keyframe therefore does not reuse an SRTP packet index.
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Instant,
};

#[derive(Debug)]
pub(crate) struct SenderState {
    sequence: AtomicU32,
    epoch: Instant,
    timestamp: u32,
    history: Mutex<VecDeque<(u16, Vec<u8>)>>,
    pub keyframe: AtomicBool,
}

pub(crate) fn is_rtcp(packet: &[u8]) -> bool {
    packet.len() >= 4 && packet[0] >> 6 == 2 && (192..=223).contains(&packet[1])
}

impl SenderState {
    pub fn new(seed: u32) -> Self {
        Self {
            sequence: AtomicU32::new(seed & 0xffff),
            epoch: Instant::now(),
            timestamp: seed.rotate_left(13),
            history: Mutex::new(VecDeque::with_capacity(512)),
            keyframe: AtomicBool::new(false),
        }
    }

    fn timestamp_offset(&self, original: u32, clock_rate: u32, offset: &mut Option<u32>) -> u32 {
        *offset.get_or_insert_with(|| {
            let elapsed =
                (self.epoch.elapsed().as_nanos() * u128::from(clock_rate) / 1_000_000_000) as u32;
            self.timestamp.wrapping_add(elapsed).wrapping_sub(original)
        })
    }

    /// Rewrite one encoder instance into the persistent publication clock.
    /// Returns false for RTCP, true for RTP. The caller forwards both.
    pub fn rewrite(
        &self,
        packet: &mut [u8],
        clock_rate: u32,
        offset: &mut Option<u32>,
    ) -> Result<bool, String> {
        if is_rtcp(packet) {
            let mut at = 0;
            while at < packet.len() {
                if at + 4 > packet.len() || packet[at] >> 6 != 2 {
                    return Err("Malformed encoder RTCP header".into());
                }
                let size =
                    (usize::from(u16::from_be_bytes([packet[at + 2], packet[at + 3]])) + 1) * 4;
                if size < 4 || at + size > packet.len() {
                    return Err("Malformed encoder RTCP length".into());
                }
                if packet[at + 1] == 200 {
                    if size < 28 {
                        return Err("Truncated encoder sender report".into());
                    }
                    let original = u32::from_be_bytes(packet[at + 16..at + 20].try_into().unwrap());
                    let adjusted =
                        original.wrapping_add(self.timestamp_offset(original, clock_rate, offset));
                    packet[at + 16..at + 20].copy_from_slice(&adjusted.to_be_bytes());
                }
                at += size;
            }
            return Ok(false);
        }
        if packet.len() < 12 || packet[0] >> 6 != 2 {
            return Err("Malformed encoder RTP packet".into());
        }
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) as u16;
        packet[2..4].copy_from_slice(&sequence.to_be_bytes());
        let original = u32::from_be_bytes(packet[4..8].try_into().unwrap());
        let adjusted = original.wrapping_add(self.timestamp_offset(original, clock_rate, offset));
        packet[4..8].copy_from_slice(&adjusted.to_be_bytes());
        let mut history = self.history.lock().unwrap();
        if history.len() == 512 {
            history.pop_front();
        }
        history.push_back((sequence, packet.to_vec()));
        Ok(true)
    }

    /// Resolve Generic NACKs from the packet history and record PLI/FIR.
    /// RTCP report reception itself is harmless when there is no encoder.
    pub fn feedback(&self, packet: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        if !is_rtcp(packet) {
            return Ok(Vec::new());
        }
        let mut requested = Vec::new();
        let mut at = 0;
        while at < packet.len() {
            if at + 4 > packet.len() || packet[at] >> 6 != 2 {
                return Err("Malformed peer RTCP header".into());
            }
            let size = (usize::from(u16::from_be_bytes([packet[at + 2], packet[at + 3]])) + 1) * 4;
            if size < 4 || at + size > packet.len() {
                return Err("Malformed peer RTCP length".into());
            }
            let kind = packet[at + 1];
            let format = packet[at] & 31;
            if kind == 196 || (kind == 206 && matches!(format, 1 | 4)) {
                self.keyframe.store(true, Ordering::Relaxed);
            }
            if kind == 205 && format == 1 {
                if size < 12 || size % 4 != 0 {
                    return Err("Malformed peer NACK".into());
                }
                for field in packet[at + 12..at + size].chunks_exact(4) {
                    let sequence = u16::from_be_bytes([field[0], field[1]]);
                    requested.push(sequence);
                    let mask = u16::from_be_bytes([field[2], field[3]]);
                    for bit in 0..16 {
                        if mask & (1 << bit) != 0 {
                            requested.push(sequence.wrapping_add(bit + 1));
                        }
                    }
                    if requested.len() > 512 {
                        return Err("Peer NACK exceeds repair budget".into());
                    }
                }
            }
            at += size;
        }
        requested.sort_unstable();
        requested.dedup();
        let history = self.history.lock().unwrap();
        Ok(requested
            .into_iter()
            .filter_map(|sequence| {
                history
                    .iter()
                    .find(|(number, _)| *number == sequence)
                    .map(|(_, bytes)| bytes.clone())
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_restart_preserves_sequence_and_repairs_loss() {
        let state = SenderState::new(65535);
        let mut packet = [0x80, 96, 0, 3, 0, 0, 0, 1, 0, 0, 0, 7, 0x65];
        let mut first = None;
        state.rewrite(&mut packet, 90000, &mut first).unwrap();
        assert_eq!(&packet[2..4], &[0xff, 0xff]);
        let original = packet.to_vec();
        let mut restarted = None;
        state.rewrite(&mut packet, 90000, &mut restarted).unwrap();
        assert_eq!(&packet[2..4], &[0, 0]);
        let nack = [0x81, 205, 0, 3, 0, 0, 0, 9, 0, 0, 0, 7, 0xff, 0xff, 0, 1];
        let repaired = state.feedback(&nack).unwrap();
        assert_eq!(repaired.len(), 2);
        assert!(repaired.contains(&original));
        assert!(repaired.contains(&packet.to_vec()));
    }

    #[test]
    fn reports_share_rtp_clock_and_picture_feedback_requests_keyframe() {
        let state = SenderState::new(7);
        let mut offset = Some(100);
        let mut report = [0u8; 28];
        report[0] = 0x80;
        report[1] = 200;
        report[3] = 6;
        report[19] = 10;
        assert!(!state.rewrite(&mut report, 90000, &mut offset).unwrap());
        assert_eq!(u32::from_be_bytes(report[16..20].try_into().unwrap()), 110);
        assert!(
            state
                .feedback(&[0x81, 206, 0, 2, 0, 0, 0, 9, 0, 0, 0, 7])
                .unwrap()
                .is_empty()
        );
        assert!(state.keyframe.load(Ordering::Relaxed));
        assert!(state.feedback(&[0x81, 205, 0xff, 0xff]).is_err());
    }
}
