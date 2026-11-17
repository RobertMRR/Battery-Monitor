//! Sony DualSense / DualSense Edge over USB or Bluetooth. Battery lives in the input report's status byte:
//! low nibble = level 0..10, high nibble = 0 discharging, 1 charging, 2 full.
//! USB report 0x01 has it at byte 53; Bluetooth extended report 0x31 at byte 54.

use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};

use super::{Battery, Kind, Provider};
use crate::hid::{self, HidInfo};

const SONY: u16 = 0x054C;

fn model(pid: u16) -> Option<&'static str> {
    match pid {
        0x0CE6 => Some("DualSense"),
        0x0DF2 => Some("DualSense Edge"),
        _ => None,
    }
}

pub struct DualSense;

impl Provider for DualSense {
    fn name(&self) -> &str {
        "DualSense"
    }

    fn poll(&mut self) -> Vec<Battery> {
        let mut pads: Vec<HidInfo> = hid::enumerate(|v, p| v == SONY && model(p).is_some())
            .into_iter()
            .filter(|d| d.usage_page == 0x01 && d.usage == 0x05)
            .collect();
        pads.sort_by(|a, b| a.path.cmp(&b.path));

        let count = pads.len();
        let mut out = Vec::new();
        for (i, info) in pads.into_iter().enumerate() {
            let Some(status) = read_status(&info) else { continue };
            let level = status & 0x0F;
            let state = status >> 4;
            if state > 2 {
                continue; // 0xA/0xB = charging error / temperature fault: no usable level
            }
            let name = model(info.pid).unwrap_or("DualSense");
            out.push(Battery {
                id: format!("dualsense:{}", info.path.to_lowercase()),
                name: if count > 1 { format!("{name} #{}", i + 1) } else { name.to_string() },
                kind: Kind::Gamepad,
                percent: if state == 2 { 100 } else { (level * 10 + 5).min(100) },
                charging: state == 1,
                stale: false,
            });
        }
        out
    }
}

fn read_status(info: &HidInfo) -> Option<u8> {
    let bt = info.is_bluetooth();
    let mut ch = hid::Channel::open(info, &[])?;
    if let Some(s) = read_reports(&mut ch, bt) {
        return Some(s);
    }
    if bt {
        // Over Bluetooth the pad starts in a "simple" 0x01 report without battery data. Reading feature report
        // 0x05 (calibration) switches it to the full 0x31 report, as Steam and games do. It stays in that mode
        // until the pad disconnects.
        let h = hid::open(&info.path, GENERIC_READ | GENERIC_WRITE, false)?;
        let mut feature = vec![0u8; info.feature_len.max(41)];
        feature[0] = 0x05;
        hid::get_feature(&h, &mut feature);
        return read_reports(&mut ch, bt);
    }
    None
}

fn read_reports(ch: &mut hid::Channel, bt: bool) -> Option<u8> {
    // The channel only strips a report ID of 0; DualSense reports are numbered, so these offsets are raw.
    let deadline = Instant::now() + Duration::from_millis(1500);
    for _ in 0..32 {
        let left = deadline.saturating_duration_since(Instant::now()).as_millis() as u32;
        let r = ch.read(left)?;
        if bt && r.len() >= 55 && r[0] == 0x31 {
            return Some(r[54]);
        }
        if !bt && r.len() >= 54 && r[0] == 0x01 {
            return Some(r[53]);
        }
    }
    None
}
