//! Heart rate streaming over AACP (RTBuddy `SensorDataWX`, service `HEARTRATE(19)`).
//!
//! This is a port of the working Android (Xposed) implementation in
//! https://github.com/tomppi/airpods_rtbuddy_v37_probe (`runLiveHrStream`, v39). The packets,
//! their order and the delays between them are copied verbatim:
//!
//! 1. Open a dedicated L2CAP channel on PSM `0x1001`.
//! 2. AACP Connect service 0, Capabilities Request service 0, AACP Connect service 4,
//!    Capabilities Request service 4 (180/220/180/220 ms apart).
//! 3. Control command `0x30` (Heart Rate Monitor) = enabled, wait 120 ms.
//! 4. One RTBuddy `SensorDataWX` `ServiceSetting(HEARTRATE(19), interval = 1 s)`, seq `0x2363`.
//! 5. Listen passively. Valid samples have service `HEARTRATE(19)`, an 18 byte command
//!    payload, outer log type `3` and status tail `10 00 00`; `payload[1]` is the bpm.
//! 6. On stop, send the same `ServiceSetting` with interval `0` (seq `0x236D`) and close.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

/// AACP Connect: `<type 0000><service><major 0001><minor 0003><features64>`.
pub const AACP_CONNECT_SERVICE0: [u8; 16] = [
    0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
pub const AACP_CAPS_REQ_SERVICE0: [u8; 7] = [0x04, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00];
pub const AACP_CONNECT_SERVICE4: [u8; 16] = [
    0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
pub const AACP_CAPS_REQ_SERVICE4: [u8; 7] = [0x04, 0x00, 0x04, 0x00, 0x01, 0x00, 0x00];
/// Control command `0x30` (HrmState) = `0x01`.
pub const HRM_ENABLE: [u8; 11] = [
    0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x30, 0x01, 0x00, 0x00, 0x00,
];
/// `SensorDataWX { seq: 0x2363, ServiceSetting { HEARTRATE, 2, [01, 1_000_000 us LE32] } }`.
pub const HR_START_1S: [u8; 28] = [
    0x04, 0x00, 0x04, 0x00, 0x17, 0x00, 0x00, 0x00, 0x10, 0x00, 0x10, 0x00, 0x08, 0xE3, 0x46, 0x42,
    0x0B, 0x08, 0x13, 0x10, 0x02, 0x1A, 0x05, 0x01, 0x40, 0x42, 0x0F, 0x00,
];
/// `SensorDataWX { seq: 0x236D, ServiceSetting { HEARTRATE, 2, [01, 0 us] } }`.
pub const HR_STOP: [u8; 28] = [
    0x04, 0x00, 0x04, 0x00, 0x17, 0x00, 0x00, 0x00, 0x10, 0x00, 0x10, 0x00, 0x08, 0xED, 0x46, 0x42,
    0x0B, 0x08, 0x13, 0x10, 0x02, 0x1A, 0x05, 0x01, 0x00, 0x00, 0x00, 0x00,
];

/// Packets sent to start streaming, each followed by the given delay.
pub const START_SEQUENCE: [(&[u8], Duration, &str); 6] = [
    (
        &AACP_CONNECT_SERVICE0,
        Duration::from_millis(180),
        "AACP connect service 0",
    ),
    (
        &AACP_CAPS_REQ_SERVICE0,
        Duration::from_millis(220),
        "AACP capabilities request service 0",
    ),
    (
        &AACP_CONNECT_SERVICE4,
        Duration::from_millis(180),
        "AACP connect service 4",
    ),
    (
        &AACP_CAPS_REQ_SERVICE4,
        Duration::from_millis(220),
        "AACP capabilities request service 4",
    ),
    (
        &HRM_ENABLE,
        Duration::from_millis(120),
        "heart rate monitor enable",
    ),
    (
        &HR_START_1S,
        Duration::ZERO,
        "HEARTRATE(19) start, interval 1s",
    ),
];

pub const RECV_BUFFER_SIZE: usize = 4096;

const RTBUDDY_OPCODE: u16 = 0x0017;
const DESCRIPTOR_SENSOR_DATA_WX: i32 = 0x0010_0000;
const SERVICE_HEARTRATE: i64 = 19;

/// Number of samples kept for the history graph.
pub const HISTORY_LEN: usize = 120;

fn le16(data: &[u8], off: usize) -> i32 {
    match data.get(off..off + 2) {
        Some(b) => u16::from_le_bytes([b[0], b[1]]) as i32,
        None => -1,
    }
}

fn le32(data: &[u8], off: usize) -> i32 {
    match data.get(off..off + 4) {
        Some(b) => i32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        None => -1,
    }
}

/// Reads a varint from `data[start..end]`, returning `(value, next)`.
fn read_varint(data: &[u8], start: usize, end: usize) -> Option<(u64, usize)> {
    let mut value = 0u64;
    let mut shift = 0;
    let mut i = start;
    while i < end && shift < 64 {
        let b = data[i];
        i += 1;
        value |= ((b & 0x7F) as u64) << shift;
        if b & 0x80 == 0 {
            return Some((value, i));
        }
        shift += 7;
    }
    None
}

/// Splits a received chunk into AACP frames (`04 00 04 00`, 12 byte header with the length at
/// offset 10). A trailing partial frame is returned as-is.
pub fn extract_aacp_frames(rx: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    if rx.len() < 12 {
        return out;
    }
    let mut pos = 0;
    while pos + 12 <= rx.len() {
        let Some(off) = (pos..=rx.len() - 12).find(|&i| rx[i..i + 4] == [0x04, 0x00, 0x04, 0x00])
        else {
            break;
        };
        let frame_len = 12 + u16::from_le_bytes([rx[off + 10], rx[off + 11]]) as usize;
        if off + frame_len <= rx.len() {
            out.push(&rx[off..off + frame_len]);
            pos = off + frame_len;
        } else {
            out.push(&rx[off..]);
            break;
        }
    }
    out
}

#[derive(Debug, Default)]
struct RtBuddyFrame {
    descriptor: i32,
    log_type: i64,
    cmd_service: i64,
    cmd_payload: Option<Vec<u8>>,
    decoded_bpm: i32,
    decoded_valid: bool,
}

fn parse_rtbuddy_frame(frame: &[u8]) -> Option<RtBuddyFrame> {
    if frame.len() < 12 {
        return None;
    }
    if le16(frame, 0) != 0x0004 || le16(frame, 2) != 0x0004 || le16(frame, 4) != 0x0017 {
        return None;
    }
    let mut rt = RtBuddyFrame {
        descriptor: le32(frame, 6),
        log_type: -1,
        cmd_service: -1,
        decoded_bpm: -1,
        ..Default::default()
    };
    let declared = le16(frame, 10);
    let start = 12;
    let mut end = frame.len().min(start + declared.max(0) as usize);
    if end < start {
        end = frame.len();
    }
    if rt.descriptor == DESCRIPTOR_SENSOR_DATA_WX {
        parse_sensor_data_wx(frame, start, end, &mut rt);
    }
    Some(rt)
}

fn parse_sensor_data_wx(data: &[u8], start: usize, end: usize, rt: &mut RtBuddyFrame) {
    let mut i = start;
    while i < end {
        let Some((key, next)) = read_varint(data, i, end) else {
            break;
        };
        i = next;
        let field = key >> 3;
        match key & 7 {
            0 => {
                let Some((val, next)) = read_varint(data, i, end) else {
                    break;
                };
                i = next;
                if field == 2 {
                    rt.log_type = val as i64;
                }
            }
            2 => {
                let Some((len, next)) = read_varint(data, i, end) else {
                    break;
                };
                i = next;
                let sub_end = end.min(i.saturating_add(len as usize));
                if field == 7 {
                    parse_command(data, i, sub_end, rt);
                }
                i = sub_end;
            }
            1 => i = end.min(i + 8),
            5 => i = end.min(i + 4),
            _ => break,
        }
    }
}

fn parse_command(data: &[u8], start: usize, end: usize, rt: &mut RtBuddyFrame) {
    let mut i = start;
    while i < end {
        let Some((key, next)) = read_varint(data, i, end) else {
            return;
        };
        i = next;
        let field = key >> 3;
        match key & 7 {
            0 => {
                let Some((val, next)) = read_varint(data, i, end) else {
                    return;
                };
                i = next;
                if field == 1 {
                    rt.cmd_service = val as i64;
                }
            }
            2 => {
                let Some((len, next)) = read_varint(data, i, end) else {
                    return;
                };
                i = next;
                let sub_end = end.min(i.saturating_add(len as usize));
                if field == 3 && sub_end > i && rt.cmd_payload.is_none() {
                    rt.cmd_payload = Some(data[i..sub_end].to_vec());
                }
                i = sub_end;
            }
            1 => i = end.min(i + 8),
            5 => i = end.min(i + 4),
            _ => return,
        }
    }
    decode_heart_rate(rt);
}

fn decode_heart_rate(rt: &mut RtBuddyFrame) {
    let Some(payload) = &rt.cmd_payload else {
        return;
    };
    let n = payload.len();
    let bpm = if n > 1 { payload[1] as i32 } else { -1 };
    rt.decoded_bpm = bpm;
    let good_tail = n >= 18 && payload[15] == 0x10 && payload[16] == 0x00 && payload[17] == 0x00;
    rt.decoded_valid = rt.descriptor == DESCRIPTOR_SENSOR_DATA_WX
        && rt.cmd_service == SERVICE_HEARTRATE
        && n == 18
        && rt.log_type == 3
        && good_tail
        && (30..=220).contains(&bpm);
}

/// Returns every validated heart rate sample (bpm) contained in a received chunk.
pub fn parse_heart_rate_samples(rx: &[u8]) -> Vec<u8> {
    extract_aacp_frames(rx)
        .into_iter()
        .filter(|frame| frame.len() >= 12 && le16(frame, 4) == RTBUDDY_OPCODE as i32)
        .filter_map(parse_rtbuddy_frame)
        .filter(|rt| rt.decoded_valid)
        .map(|rt| rt.decoded_bpm as u8)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartRateSample {
    pub bpm: u8,
    pub time: SystemTime,
}

/// Running heart rate statistics for the current monitoring session.
#[derive(Debug, Clone, Default)]
pub struct HeartRateStats {
    pub monitoring: bool,
    pub count: u64,
    pub sum: u64,
    pub min: Option<u8>,
    pub max: Option<u8>,
    pub started_at: Option<SystemTime>,
    pub history: VecDeque<HeartRateSample>,
    /// Why monitoring last failed or stopped unexpectedly.
    pub error: Option<String>,
}

impl HeartRateStats {
    pub fn add_sample(&mut self, bpm: u8) {
        let time = SystemTime::now();
        self.count += 1;
        self.sum += bpm as u64;
        self.min = Some(self.min.map_or(bpm, |m| m.min(bpm)));
        self.max = Some(self.max.map_or(bpm, |m| m.max(bpm)));
        self.started_at.get_or_insert(time);
        if self.history.len() == HISTORY_LEN {
            self.history.pop_front();
        }
        self.history.push_back(HeartRateSample { bpm, time });
    }

    /// Clears collected samples, keeping the monitoring state.
    pub fn reset(&mut self) {
        *self = HeartRateStats {
            monitoring: self.monitoring,
            ..Default::default()
        };
    }

    pub fn current(&self) -> Option<u8> {
        self.history.back().map(|s| s.bpm)
    }

    pub fn average(&self) -> Option<f64> {
        (self.count > 0).then(|| self.sum as f64 / self.count as f64)
    }

    /// Average of the last `n` samples.
    pub fn recent_average(&self, n: usize) -> Option<f64> {
        let n = n.min(self.history.len());
        (n > 0).then(|| {
            self.history
                .iter()
                .rev()
                .take(n)
                .map(|s| s.bpm as f64)
                .sum::<f64>()
                / n as f64
        })
    }

    /// Difference between the newest sample and the sample `n` samples earlier.
    pub fn trend(&self, n: usize) -> Option<i16> {
        let len = self.history.len();
        if len < 2 {
            return None;
        }
        let first = self.history[len.saturating_sub(n.max(2))].bpm as i16;
        Some(self.history[len - 1].bpm as i16 - first)
    }

    pub fn duration(&self) -> Option<Duration> {
        let start = self.started_at?;
        let end = self.history.back()?.time;
        end.duration_since(start).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        hex::decode(s.replace(' ', "")).unwrap()
    }

    /// Builds a streamed heart rate frame the way the AirPods send it.
    fn hr_frame(log_type: u8, bpm: u8, tail: [u8; 3]) -> Vec<u8> {
        let mut hr = vec![0u8; 18];
        hr[1] = bpm;
        hr[15..18].copy_from_slice(&tail);
        let mut command = vec![0x08, 0x13, 0x1A, hr.len() as u8];
        command.extend_from_slice(&hr);
        let mut payload = vec![0x08, 0x80, 0x48, 0x10, log_type, 0x3A, command.len() as u8];
        payload.extend_from_slice(&command);
        let mut frame = vec![0x04, 0x00, 0x04, 0x00, 0x17, 0x00, 0x00, 0x00, 0x10, 0x00];
        frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        frame.extend_from_slice(&payload);
        frame
    }

    #[test]
    fn packets_match_android_implementation() {
        assert_eq!(
            AACP_CONNECT_SERVICE0.to_vec(),
            hex("00 00 00 00 01 00 03 00 00 00 00 00 00 00 00 00")
        );
        assert_eq!(
            AACP_CONNECT_SERVICE4.to_vec(),
            hex("00 00 04 00 01 00 03 00 00 00 00 00 00 00 00 00")
        );
        assert_eq!(AACP_CAPS_REQ_SERVICE0.to_vec(), hex("04 00 00 00 01 00 00"));
        assert_eq!(AACP_CAPS_REQ_SERVICE4.to_vec(), hex("04 00 04 00 01 00 00"));
        assert_eq!(HRM_ENABLE.to_vec(), hex("04 00 04 00 09 00 30 01 00 00 00"));
        assert_eq!(
            HR_START_1S.to_vec(),
            hex(
                "04 00 04 00 17 00 00 00 10 00 10 00 08 E3 46 42 0B 08 13 10 02 1A 05 01 40 42 0F 00"
            )
        );
        assert_eq!(
            HR_STOP.to_vec(),
            hex(
                "04 00 04 00 17 00 00 00 10 00 10 00 08 ED 46 42 0B 08 13 10 02 1A 05 01 00 00 00 00"
            )
        );
    }

    #[test]
    fn parses_valid_sample() {
        let frame = hr_frame(3, 72, [0x10, 0x00, 0x00]);
        assert_eq!(parse_heart_rate_samples(&frame), vec![72]);
    }

    #[test]
    fn parses_concatenated_frames() {
        let mut rx = hr_frame(3, 72, [0x10, 0x00, 0x00]);
        rx.extend_from_slice(&[
            0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ]);
        rx.extend_from_slice(&hr_frame(3, 75, [0x10, 0x00, 0x00]));
        assert_eq!(parse_heart_rate_samples(&rx), vec![72, 75]);
    }

    #[test]
    fn rejects_transient_frames() {
        for frame in [
            hr_frame(3, 72, [0x10, 0x82, 0x81]),
            hr_frame(3, 72, [0x10, 0x02, 0x81]),
            hr_frame(2, 72, [0x10, 0x00, 0x00]),
            hr_frame(3, 29, [0x10, 0x00, 0x00]),
            hr_frame(3, 221, [0x10, 0x00, 0x00]),
        ] {
            assert!(parse_heart_rate_samples(&frame).is_empty());
        }
    }

    #[test]
    fn rejects_other_packets() {
        assert!(parse_heart_rate_samples(&HR_START_1S).is_empty());
        assert!(parse_heart_rate_samples(&HRM_ENABLE).is_empty());
        let mut truncated = hr_frame(3, 72, [0x10, 0x00, 0x00]);
        truncated.truncate(truncated.len() - 4);
        assert!(parse_heart_rate_samples(&truncated).is_empty());
    }

    #[test]
    fn statistics() {
        let mut stats = HeartRateStats::default();
        assert_eq!(stats.current(), None);
        assert_eq!(stats.average(), None);
        for bpm in [60, 70, 80, 90] {
            stats.add_sample(bpm);
        }
        assert_eq!(stats.current(), Some(90));
        assert_eq!(stats.min, Some(60));
        assert_eq!(stats.max, Some(90));
        assert_eq!(stats.average(), Some(75.0));
        assert_eq!(stats.recent_average(2), Some(85.0));
        assert_eq!(stats.trend(10), Some(30));
        stats.monitoring = true;
        stats.reset();
        assert!(stats.monitoring);
        assert_eq!(stats.count, 0);

        for i in 0..(HISTORY_LEN + 10) {
            stats.add_sample(60 + (i % 10) as u8);
        }
        assert_eq!(stats.history.len(), HISTORY_LEN);
        assert_eq!(stats.count, (HISTORY_LEN + 10) as u64);
    }
}
