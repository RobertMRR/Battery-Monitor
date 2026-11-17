//! Logitech devices speaking HID++ 2.0: mice and keyboards behind Lightspeed / Unifying / Bolt receivers (device
//! slots 1..6), directly connected devices (slot 0xFF), and the older G-series wireless headsets.
//! Instead of per-model tables, the device's root feature (0x0000) is asked where its battery feature lives.
//! Protocol facts as documented by Solaar and HeadsetControl.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use super::headsets::from_voltage;
use super::{Battery, Kind, Provider, guess_kind};
use crate::hid::{self, Channel, HidInfo};

const VID: u16 = 0x046D;
const SHORT: u8 = 0x10;
const LONG: u8 = 0x11;
const SW: u8 = 0x0E;
const DIRECT: u8 = 0xFF;
const TIMEOUT: u32 = 500;

const FEAT_DEVICE_NAME: u16 = 0x0005;
const FEAT_BATTERY_STATUS: u16 = 0x1000;
const FEAT_BATTERY_VOLTAGE: u16 = 0x1001;
const FEAT_UNIFIED_BATTERY: u16 = 0x1004;
const FEAT_ADC_MEASUREMENT: u16 = 0x1F20;
const BATTERY_FEATURES: [u16; 4] =
    [FEAT_UNIFIED_BATTERY, FEAT_BATTERY_STATUS, FEAT_BATTERY_VOLTAGE, FEAT_ADC_MEASUREMENT];

/// Headsets whose battery feature index HeadsetControl documents, used if feature discovery gets no answer.
fn known_headset_index(pid: u16) -> Option<u8> {
    match pid {
        0x0A66 => Some(0x07),                                              // G533
        0x0AC4 => Some(0x05),                                              // G535
        0x0A5C | 0x0A89 | 0x0A5B | 0x0A87 | 0x0AB5 | 0x0AFE => Some(0x08), // G633/G635/G933/G935/G733
        0x0AA7 | 0x0AAA | 0x0ABA | 0x0AFB | 0x0AFC => Some(0x06),          // G PRO / PRO X / PRO X2 (non-Lightspeed)
        _ => None,
    }
}

// Discharge curves (mV, %) for voltage-only devices: headset curves per HeadsetControl, generic Li-ion per Solaar.
const G533: &[(i32, i32)] = &[(4200, 100), (3850, 50), (3790, 30), (3750, 20), (3680, 5), (3330, 0)];
const G535: &[(i32, i32)] = &[(4175, 100), (3817, 50), (3766, 30), (3730, 20), (3664, 5), (3310, 0)];
const G633: &[(i32, i32)] =
    &[(4100, 100), (3950, 80), (3850, 60), (3750, 40), (3650, 20), (3500, 10), (3300, 5), (3150, 0)];
const GPRO: &[(i32, i32)] = &[(4150, 100), (3830, 50), (3780, 30), (3740, 20), (3670, 5), (3320, 0)];
const GENERIC: &[(i32, i32)] = &[
    (4186, 100),
    (4067, 90),
    (3989, 80),
    (3922, 70),
    (3859, 60),
    (3811, 50),
    (3778, 40),
    (3751, 30),
    (3717, 20),
    (3671, 10),
    (3646, 5),
    (3579, 2),
    (3500, 0),
];

fn curve_for(pid: u16) -> &'static [(i32, i32)] {
    match pid {
        0x0A66 => G533,
        0x0AC4 => G535,
        0x0A5C | 0x0A89 | 0x0A5B | 0x0A87 | 0x0AB5 | 0x0AFE => G633,
        0x0AA7 | 0x0AAA | 0x0ABA | 0x0AFB | 0x0AFC => GPRO,
        _ => GENERIC,
    }
}

struct Slot {
    index: u8,
    feature: u16,
    feature_index: u8,
    name: String,
    kind: Kind,
    last: Option<Battery>,
}

#[derive(Default)]
struct Interface {
    slots: BTreeMap<u8, Slot>,
    next_discovery: Option<Instant>,
}

#[derive(Default)]
pub struct Logitech {
    interfaces: HashMap<String, Interface>,
}

#[derive(PartialEq)]
enum Reply {
    Ok(Vec<u8>),
    Error,
    Silent,
}

impl Provider for Logitech {
    fn name(&self) -> &str {
        "Logitech"
    }

    fn poll(&mut self) -> Vec<Battery> {
        let mut groups: BTreeMap<String, Vec<HidInfo>> = BTreeMap::new();
        for c in hid::enumerate(|v, _| v == VID) {
            groups.entry(c.interface_key()).or_default().push(c);
        }

        let mut out = Vec::new();
        let mut present = HashSet::new();
        for (key, group) in &groups {
            // Requests go to the long-report (20-byte) collection; replies and errors may come back on the
            // short-report (7-byte) collection of the same interface, so read both.
            let Some(long) = group
                .iter()
                .find(|c| c.output_len == 20 && c.input_len == 20 && (c.is_vendor_defined() || c.usage_page == 0x000C))
            else {
                continue;
            };
            let readers: Vec<&HidInfo> =
                group.iter().filter(|c| c.is_vendor_defined() && (c.input_len == 7 || c.input_len == 20)).collect();

            present.insert(key.clone());
            let state = self.interfaces.entry(key.clone()).or_default();
            let Some(mut ch) = Channel::open(long, &readers) else { continue };

            if state.next_discovery.is_none_or(|t| Instant::now() >= t) {
                discover(&mut ch, state, long);
                let wait = if state.slots.is_empty() { 120 } else { 300 };
                state.next_discovery = Some(Instant::now() + Duration::from_secs(wait));
            }

            for slot in state.slots.values_mut() {
                match read_battery(&mut ch, slot) {
                    Some((percent, charging)) => {
                        let b = Battery {
                            id: format!("logitech:{key}:{:02x}", slot.index),
                            name: slot.name.clone(),
                            kind: slot.kind,
                            percent,
                            charging,
                            stale: false,
                        };
                        slot.last = Some(b.clone());
                        out.push(b);
                    }
                    // Asleep / out of range: keep the last value, faded.
                    None => out.extend(slot.last.iter().map(|b| Battery { stale: true, ..b.clone() })),
                }
            }
        }
        self.interfaces.retain(|k, _| present.contains(k));
        out
    }
}

fn discover(ch: &mut Channel, state: &mut Interface, info: &HidInfo) {
    // A device answers HID++ 2.0 at index 0xFF itself; a receiver answers there with a HID++ 1.0 error, and its
    // paired devices live at 1..6.
    let (is_hidpp20, slot) = discover_slot(ch, DIRECT, info);
    if is_hidpp20 {
        if let Some(s) = slot {
            state.slots.insert(DIRECT, s);
        }
        return;
    }
    if let Some(index) = known_headset_index(info.pid) {
        state.slots.insert(
            DIRECT,
            Slot {
                index: DIRECT,
                feature: FEAT_ADC_MEASUREMENT,
                feature_index: index,
                name: info.product.clone().unwrap_or_else(|| "Logitech headset".into()),
                kind: Kind::Headset,
                last: None,
            },
        );
        return;
    }
    for i in 1..=6u8 {
        if state.slots.contains_key(&i) {
            continue; // known already; just possibly asleep right now
        }
        if let (_, Some(s)) = discover_slot(ch, i, info) {
            state.slots.insert(i, s);
        }
    }
}

fn discover_slot(ch: &mut Channel, index: u8, info: &HidInfo) -> (bool, Option<Slot>) {
    let mut answered = false;
    for feature in BATTERY_FEATURES {
        let Reply::Ok(p) = request(ch, index, 0x00, 0, &feature.to_be_bytes()) else {
            return (false, None); // HID++ 1.0 error, empty slot, or silence
        };
        answered = true;
        let feature_index = p[0];
        if feature_index == 0 {
            continue;
        }
        let (mut name, mut kind) = device_name(ch, index);
        if index == DIRECT {
            name = name.or_else(|| info.product.clone());
            if info.pid & 0xFF00 == 0x0A00 {
                kind = Some(Kind::Headset); // Logitech audio product range
            }
        }
        let name = name.unwrap_or_else(|| format!("Logitech device {index}"));
        let kind = kind.unwrap_or_else(|| guess_kind(&name, Kind::Other));
        return (true, Some(Slot { index, feature, feature_index, name, kind, last: None }));
    }
    (answered, None)
}

fn read_battery(ch: &mut Channel, slot: &Slot) -> Option<(u8, bool)> {
    let function = if slot.feature == FEAT_UNIFIED_BATTERY { 1 } else { 0 }; // 0x1004 get_status is function 1
    let Reply::Ok(p) = request(ch, slot.index, slot.feature_index, function, &[]) else { return None };
    match slot.feature {
        FEAT_UNIFIED_BATTERY => {
            let mut pct = p[0];
            if pct == 0 && p[1] != 0 {
                // Device only reports coarse levels.
                pct = if p[1] & 0x08 != 0 {
                    90
                } else if p[1] & 0x04 != 0 {
                    50
                } else if p[1] & 0x02 != 0 {
                    20
                } else {
                    5
                };
            }
            Some((pct.min(100), matches!(p[2], 1..=3)))
        }
        FEAT_BATTERY_STATUS => {
            if p[2] == 3 {
                Some((100, true))
            } else {
                Some((p[0].min(100), matches!(p[2], 1 | 2 | 4)))
            }
        }
        _ => {
            // 0x1001 battery voltage / 0x1F20 ADC measurement: millivolts big-endian, then flags.
            let mv = (p[0] as i32) << 8 | p[1] as i32;
            if mv < 2500 {
                return None;
            }
            let charging = if slot.feature == FEAT_BATTERY_VOLTAGE { p[2] & 0x80 != 0 } else { p[2] & 0x03 == 0x03 };
            Some((from_voltage(mv, curve_for(ch.info.pid)), charging))
        }
    }
}

fn device_name(ch: &mut Channel, device: u8) -> (Option<String>, Option<Kind>) {
    let Reply::Ok(f) = request(ch, device, 0x00, 0, &FEAT_DEVICE_NAME.to_be_bytes()) else { return (None, None) };
    let index = f[0];
    if index == 0 {
        return (None, None);
    }
    let Reply::Ok(count) = request(ch, device, index, 0, &[]) else { return (None, None) };
    let len = count[0] as usize;
    let mut name = Vec::new();
    while name.len() < len {
        let Reply::Ok(chunk) = request(ch, device, index, 1, &[name.len() as u8]) else { break };
        let take = (len - name.len()).min(chunk.len());
        if take == 0 {
            break;
        }
        name.extend_from_slice(&chunk[..take]);
    }
    let kind = match request(ch, device, index, 2, &[]) {
        Reply::Ok(t) => match t[0] {
            0 | 2 => Some(Kind::Keyboard),
            3..=5 => Some(Kind::Mouse),
            8 => Some(Kind::Headset),
            _ => None,
        },
        _ => None,
    };
    let text = String::from_utf8_lossy(&name).trim_end_matches(['\0', ' ']).to_string();
    ((!text.is_empty()).then_some(text), kind)
}

/// One HID++ 2.0 request. `Reply::Ok` carries the parameter bytes (everything after the 4-byte header).
fn request(ch: &mut Channel, device: u8, feature_index: u8, function: u8, params: &[u8]) -> Reply {
    let fn_sw = (function << 4) | SW;
    let mut req = [0u8; 20];
    req[..4].copy_from_slice(&[LONG, device, feature_index, fn_sw]);
    req[4..4 + params.len()].copy_from_slice(params);

    // Reply:           [report, device, featureIndex, fn|sw, params...]
    // HID++ 2.0 error: [report, device, 0xFF, featureIndex, fn|sw, code]
    // HID++ 1.0 error: [0x10,   device, 0x8F, featureIndex, fn|sw, code]
    let is_error = |r: &[u8]| (r[2] == 0xFF || r[2] == 0x8F) && r[3] == feature_index && r[4] == fn_sw;
    let reply = ch.request(&req, TIMEOUT, |r| {
        r.len() >= 7
            && (r[0] == SHORT || r[0] == LONG)
            && r[1] == device
            && ((r[2] == feature_index && r[3] == fn_sw) || is_error(r))
    });
    match reply {
        None => Reply::Silent,
        Some(r) if is_error(&r) => Reply::Error,
        // Pad short replies so callers can index the first few parameters freely.
        Some(mut r) => {
            r.resize(20, 0);
            Reply::Ok(r[4..].to_vec())
        }
    }
}
