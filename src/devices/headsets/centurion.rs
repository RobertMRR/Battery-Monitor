//! Logitech LIGHTSPEED headsets on the newer "Centurion" protocol: G PRO X 2 LIGHTSPEED, G522, G PRO X 3.
//! Protocol facts as documented by HeadsetControl.
//!
//! Frames are 64 bytes: `prefix, length, flags, payload…` (prefix = report ID 0x51 on the PRO X 2, 0x50 0x23 on
//! newer models; length counts flags + payload). The USB dongle answers directly; the headset is reached through
//! the dongle's bridge feature (0x0003), which first acknowledges a request and then relays the headset's reply as
//! a message event. Battery is headset feature 0x0104: `[percent, ?, charge state]`.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use super::{Read, Spec, TIMEOUT};
use crate::hid::Channel;

const VID: u16 = 0x046D;
const SW: u8 = 0x01;
const BRIDGE_SEND: u8 = 0x10;
const FEAT_BRIDGE: [u8; 2] = [0x00, 0x03];
const FEAT_BATTERY_SOC: [u8; 2] = [0x01, 0x04];
const MAX_REPORTS: usize = 8;

pub fn specs() -> Vec<Spec> {
    vec![
        Spec::new("Logitech", "Logitech G PRO X 2 LIGHTSPEED", VID, &[0x0AF7], 0xFFA0, 0x01, Some(3), read_battery),
        Spec::new("Logitech", "Logitech G522 LIGHTSPEED", VID, &[0x0B18], 0xFFA0, 0x01, Some(3), read_battery),
        Spec::new("Logitech", "Logitech G PRO X 3 LIGHTSPEED", VID, &[0x0B45], 0xFFA0, 0x01, Some(3), read_battery),
    ]
}

/// Per collection path: the dongle's bridge feature index and the headset's battery feature index.
static INDEXES: LazyLock<Mutex<HashMap<String, (u8, u8)>>> = LazyLock::new(Default::default);

/// Signals "the dongle says the headset is off".
struct Offline;

fn read_battery(ch: &mut Channel) -> Option<Read> {
    let prefix: &[u8] = if ch.info.pid == 0x0AF7 { &[0x51] } else { &[0x50, 0x23] };
    match battery(ch, prefix) {
        Ok(r) => r,
        Err(Offline) => Read::off(),
    }
}

fn battery(ch: &mut Channel, prefix: &[u8]) -> Result<Option<Read>, Offline> {
    let path = ch.info.path.clone();
    let cached = INDEXES.lock().unwrap().get(&path).copied();
    let (bridge, battery) = match cached {
        Some(idx) => idx,
        None => {
            let Some(b) = direct(ch, prefix, 0x00, 0x00, &FEAT_BRIDGE, false).filter(|b| !b.is_empty() && b[0] != 0)
            else {
                return Ok(None);
            };
            // Zero = old firmware without the feature (update it in G HUB).
            let Some(f) =
                bridged(ch, prefix, b[0], 0x00, 0x00, &FEAT_BATTERY_SOC)?.filter(|f| !f.is_empty() && f[0] != 0)
            else {
                return Ok(None);
            };
            INDEXES.lock().unwrap().insert(path.clone(), (b[0], f[0]));
            (b[0], f[0])
        }
    };
    match bridged(ch, prefix, bridge, battery, 0x00, &[])? {
        Some(r) if !r.is_empty() && r[0] <= 100 => Ok(Read::level(r[0], r.len() >= 3 && (r[2] == 1 || r[2] == 2))),
        _ => {
            INDEXES.lock().unwrap().remove(&path);
            Ok(None)
        }
    }
}

fn frame(prefix: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut f = vec![0u8; 64];
    f[..prefix.len()].copy_from_slice(prefix);
    f[prefix.len()] = (payload.len() + 1) as u8; // length covers flags + payload
    f[prefix.len() + 1] = 0x00; // flags: single, unfragmented frame
    f[prefix.len() + 2..prefix.len() + 2 + payload.len()].copy_from_slice(payload);
    f
}

/// Payload of a reply frame, or None if it isn't a Centurion frame with this prefix.
fn payload<'a>(frame: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    let h = prefix.len();
    if frame.len() < h + 3 || &frame[..h] != prefix {
        return None;
    }
    let len = frame[h] as usize;
    if len <= 1 || len + h + 1 > frame.len() {
        return None;
    }
    Some(&frame[h + 2..h + 1 + len])
}

/// Request to the dongle itself. Returns the reply parameters.
fn direct(
    ch: &mut Channel,
    prefix: &[u8],
    feature: u8,
    function: u8,
    params: &[u8],
    match_fn: bool,
) -> Option<Vec<u8>> {
    let fn_sw = (function & 0xF0) | SW;
    let mut body = vec![feature, fn_sw];
    body.extend_from_slice(params);
    ch.discard();
    if !ch.write(&frame(prefix, &body)) {
        return None;
    }
    for _ in 0..MAX_REPORTS {
        let f = ch.read_matching(TIMEOUT, |_| true)?;
        let Some(p) = payload(&f, prefix) else { continue };
        if p.len() < 2 || p[0] != feature || (match_fn && p[1] != fn_sw) {
            continue;
        }
        return Some(p[2..].to_vec());
    }
    None
}

/// Request relayed to the headset through the dongle's bridge.
fn bridged(
    ch: &mut Channel,
    prefix: &[u8],
    bridge: u8,
    sub_feature: u8,
    function: u8,
    params: &[u8],
) -> Result<Option<Vec<u8>>, Offline> {
    let mut sub = vec![0x00, sub_feature, (function & 0xF0) | SW];
    sub.extend_from_slice(params);
    let mut body = vec![bridge, BRIDGE_SEND | SW, ((sub.len() >> 8) & 0x0F) as u8, sub.len() as u8];
    body.extend_from_slice(&sub);

    ch.discard();
    if !ch.write(&frame(prefix, &body)) {
        return Ok(None);
    }
    let mut acknowledged = false;
    for _ in 0..MAX_REPORTS {
        let Some(f) = ch.read_matching(TIMEOUT, |_| true) else { break };
        let Some(p) = payload(&f, prefix) else { continue };
        if p.len() < 2 {
            continue;
        }
        // The dongle rejected the request (the PRO X 2 does this when no headset is connected).
        if p.len() >= 4 && p[0] == 0xFF && p[1] == bridge && p[2] == BRIDGE_SEND | SW {
            return probe_offline(ch, prefix, bridge);
        }
        if p[0] != bridge {
            continue;
        }
        // Connection event from the dongle: [bridge, 0x00, 0x00, connected].
        if p[1] == 0x00 && p.len() >= 4 {
            if p[3] == 0x00 {
                return Err(Offline);
            }
            continue;
        }
        if p[1] >> 4 != BRIDGE_SEND >> 4 {
            continue;
        }
        if p[1] & 0x0F == SW {
            acknowledged = true; // ack of our send
            continue;
        }
        if p[1] & 0x0F != 0x00 {
            continue; // someone else's traffic
        }
        // Message event: [bridge, 0x10, size hi, size lo, 0x00, subFeature | 0xFF (error), fn|sw, data...]
        if p.len() < 7 || p[4] != 0x00 {
            continue;
        }
        if p[5] == 0xFF && p[6] == sub_feature {
            return Ok(None); // headset rejected the request
        }
        if p[5] != sub_feature {
            continue;
        }
        return Ok(Some(p[7..].to_vec()));
    }
    if acknowledged { probe_offline(ch, prefix, bridge) } else { Ok(None) }
}

/// Bridge status (function 0): parameters 2-3 count connected headsets; 00 00 = headset off.
fn probe_offline(ch: &mut Channel, prefix: &[u8], bridge: u8) -> Result<Option<Vec<u8>>, Offline> {
    match direct(ch, prefix, bridge, 0x00, &[], true) {
        Some(s) if s.len() >= 4 && s[2] == 0 && s[3] == 0 => Err(Offline),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::{frame, payload};

    #[test]
    fn frame_round_trip() {
        for prefix in [&[0x51u8][..], &[0x50, 0x23][..]] {
            let f = frame(prefix, &[0x0A, 0x11, 0x01, 0x04]);
            assert_eq!(f.len(), 64);
            assert_eq!(&f[..prefix.len()], prefix);
            assert_eq!(f[prefix.len()], 5); // length counts flags + payload
            assert_eq!(payload(&f, prefix), Some(&[0x0A, 0x11, 0x01, 0x04][..]));
        }
    }

    #[test]
    fn foreign_frames_are_rejected() {
        let f = frame(&[0x51], &[1, 2, 3]);
        assert_eq!(payload(&f, &[0x50, 0x23]), None);
        assert_eq!(payload(&[0x51, 0xFF, 0x00], &[0x51]), None); // length beyond the report
    }
}
