//! Heart rate streaming over AACP (RTBuddy `SensorDataWX`, service `HEARTRATE(19)`).
//!
//! Protocol reverse engineered in https://github.com/tomppi/airpods_rtbuddy_v37_probe:
//!
//! 1. Enable the heart rate monitor with control command `0x30` (HrmState) = `0x01`.
//! 2. Send an RTBuddy `SensorDataWX` frame (AACP opcode `0x17`, descriptor `0x00100000`)
//!    containing a `ServiceSetting` for `HEARTRATE(19)` with the desired sample interval.
//! 3. The AirPods then stream `SensorDataWX` frames whose `Command` (field 7) carries an
//!    18 byte payload for `HEARTRATE(19)`. `payload[1]` is the heart rate in bpm.
//! 4. To stop, send the same `ServiceSetting` with an interval of `0`.
//!
//! `SensorDataWX` frame layout (after the `04 00 04 00` AACP header):
//!
//! ```text
//! 17 00 | 00 00 10 00 (descriptor, LE32) | LL LL (payload length, LE16) | protobuf payload
//! ```

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

pub const SENSOR_DATA_OPCODE: u8 = 0x17;
pub const DESCRIPTOR_SENSOR_DATA_WX: u32 = 0x0010_0000;
pub const SERVICE_HEARTRATE: u64 = 19;

/// Sequence id the probe used for its (working) start packet; later packets just increment it.
pub const INITIAL_SEQUENCE: u32 = 0x2363;
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(1);

const HR_PAYLOAD_LEN: usize = 18;
const VALID_LOG_TYPE: u64 = 3;
const VALID_STATUS_TAIL: [u8; 3] = [0x10, 0x00, 0x00];
const MIN_BPM: u8 = 30;
const MAX_BPM: u8 = 220;

/// Number of samples kept for the history graph.
pub const HISTORY_LEN: usize = 120;

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

fn read_varint(data: &[u8], pos: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *data.get(*pos)?;
        *pos += 1;
        value |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

enum Field<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Other,
}

/// Iterates the top level fields of a protobuf message, stopping at the first malformed field.
fn fields(data: &[u8]) -> impl Iterator<Item = (u64, Field<'_>)> {
    let mut pos = 0;
    std::iter::from_fn(move || {
        if pos >= data.len() {
            return None;
        }
        let key = read_varint(data, &mut pos)?;
        let field = key >> 3;
        let value = match key & 7 {
            0 => Field::Varint(read_varint(data, &mut pos)?),
            2 => {
                let len = read_varint(data, &mut pos)? as usize;
                let end = pos.checked_add(len).filter(|&e| e <= data.len())?;
                let bytes = &data[pos..end];
                pos = end;
                Field::Bytes(bytes)
            }
            1 => {
                pos += 8;
                Field::Other
            }
            5 => {
                pos += 4;
                Field::Other
            }
            _ => return None,
        };
        Some((field, value))
    })
}

/// Builds the AACP data (without the `04 00 04 00` header) for an RTBuddy `ServiceSetting`
/// for `HEARTRATE(19)`. An interval of zero stops the stream.
pub fn build_service_setting_packet(sequence: u32, interval: Duration) -> Vec<u8> {
    let interval_us = interval.as_micros().min(u32::MAX as u128) as u32;

    // ServiceSetting { 1: service, 2: mode, 3: [0x01, interval_us LE32] }
    let mut setting = Vec::new();
    setting.extend_from_slice(&[0x08]);
    write_varint(&mut setting, SERVICE_HEARTRATE);
    setting.extend_from_slice(&[0x10, 0x02, 0x1A, 0x05, 0x01]);
    setting.extend_from_slice(&interval_us.to_le_bytes());

    // SensorDataWX { 1: sequence, 8: ServiceSetting }
    let mut payload = vec![0x08];
    write_varint(&mut payload, sequence as u64);
    payload.push(0x42);
    write_varint(&mut payload, setting.len() as u64);
    payload.extend_from_slice(&setting);

    let mut packet = vec![SENSOR_DATA_OPCODE, 0x00];
    packet.extend_from_slice(&DESCRIPTOR_SENSOR_DATA_WX.to_le_bytes());
    packet.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    packet.extend_from_slice(&payload);
    packet
}

/// Parses a full AACP packet (including the `04 00 04 00` header) and returns the heart rate if
/// it is a validated RTBuddy `HEARTRATE(19)` sample. Startup/transient frames are rejected.
pub fn parse_heart_rate_packet(packet: &[u8]) -> Option<u8> {
    if packet.len() < 12 || packet[4] != SENSOR_DATA_OPCODE || packet[5] != 0x00 {
        return None;
    }
    let descriptor = u32::from_le_bytes(packet[6..10].try_into().ok()?);
    if descriptor != DESCRIPTOR_SENSOR_DATA_WX {
        return None;
    }
    let declared = u16::from_le_bytes([packet[10], packet[11]]) as usize;
    let end = packet.len().min(12 + declared);
    let payload = &packet[12..end];

    let mut log_type = None;
    let mut command = None;
    for (field, value) in fields(payload) {
        match (field, value) {
            (2, Field::Varint(v)) => log_type = Some(v),
            (7, Field::Bytes(b)) if command.is_none() => command = Some(b),
            _ => {}
        }
    }
    if log_type != Some(VALID_LOG_TYPE) {
        return None;
    }

    // Command { 1: service, 3: payload }
    let mut service = None;
    let mut hr_payload = None;
    for (field, value) in fields(command?) {
        match (field, value) {
            (1, Field::Varint(v)) => service = Some(v),
            (3, Field::Bytes(b)) if hr_payload.is_none() && !b.is_empty() => hr_payload = Some(b),
            _ => {}
        }
    }
    let hr_payload = hr_payload?;
    if service != Some(SERVICE_HEARTRATE)
        || hr_payload.len() != HR_PAYLOAD_LEN
        || hr_payload[15..18] != VALID_STATUS_TAIL
    {
        return None;
    }
    let bpm = hr_payload[1];
    (MIN_BPM..=MAX_BPM).contains(&bpm).then_some(bpm)
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

    fn header(data: &[u8]) -> Vec<u8> {
        [&[0x04, 0x00, 0x04, 0x00][..], data].concat()
    }

    /// Builds a streamed heart rate frame the way the AirPods send it.
    fn hr_frame(seq: u32, log_type: u8, bpm: u8, tail: [u8; 3]) -> Vec<u8> {
        let mut hr = vec![0u8; HR_PAYLOAD_LEN];
        hr[1] = bpm;
        hr[15..18].copy_from_slice(&tail);
        let mut command = vec![0x08, SERVICE_HEARTRATE as u8, 0x1A, hr.len() as u8];
        command.extend_from_slice(&hr);
        let mut payload = vec![0x08];
        write_varint(&mut payload, seq as u64);
        payload.extend_from_slice(&[0x10, log_type, 0x3A, command.len() as u8]);
        payload.extend_from_slice(&command);
        let mut data = vec![SENSOR_DATA_OPCODE, 0x00, 0x00, 0x00, 0x10, 0x00];
        data.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        data.extend_from_slice(&payload);
        header(&data)
    }

    #[test]
    fn start_packet_matches_probe() {
        let expected =
            hex::decode("170000001000100008e346420b0813100 21a050140420f00".replace(' ', ""))
                .unwrap();
        assert_eq!(
            build_service_setting_packet(0x2363, Duration::from_secs(1)),
            expected
        );
    }

    #[test]
    fn stop_packet_matches_probe() {
        let expected =
            hex::decode("170000001000100008ed46420b08131002 1a05010000 0000".replace(' ', ""))
                .unwrap();
        assert_eq!(
            build_service_setting_packet(0x236D, Duration::ZERO),
            expected
        );
    }

    #[test]
    fn parses_valid_sample() {
        let frame = hr_frame(0x2400, 3, 72, VALID_STATUS_TAIL);
        assert_eq!(parse_heart_rate_packet(&frame), Some(72));
    }

    #[test]
    fn rejects_transient_frames() {
        assert_eq!(
            parse_heart_rate_packet(&hr_frame(1, 3, 72, [0x10, 0x82, 0x81])),
            None
        );
        assert_eq!(
            parse_heart_rate_packet(&hr_frame(1, 3, 72, [0x10, 0x02, 0x81])),
            None
        );
        assert_eq!(
            parse_heart_rate_packet(&hr_frame(1, 2, 72, VALID_STATUS_TAIL)),
            None
        );
        assert_eq!(
            parse_heart_rate_packet(&hr_frame(1, 3, 0, VALID_STATUS_TAIL)),
            None
        );
        assert_eq!(
            parse_heart_rate_packet(&hr_frame(1, 3, 250, VALID_STATUS_TAIL)),
            None
        );
    }

    #[test]
    fn rejects_other_packets() {
        let start = header(&build_service_setting_packet(1, DEFAULT_INTERVAL));
        assert_eq!(parse_heart_rate_packet(&start), None);
        assert_eq!(
            parse_heart_rate_packet(&[0x04, 0x00, 0x04, 0x00, 0x17]),
            None
        );
        let mut truncated = hr_frame(1, 3, 72, VALID_STATUS_TAIL);
        truncated.truncate(truncated.len() - 4);
        assert_eq!(parse_heart_rate_packet(&truncated), None);
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
