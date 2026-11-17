//! Corsair wireless devices. Two protocols:
//! * legacy VOID-family headsets: output report 0xC9 0x64, status input report 0x64;
//! * "Bragi" (Virtuoso XT/SE, VOID V2, HS80 MAX, and probably Slipstream mice/keyboards): property reads; battery
//!   level is property 0x0F in tenths of a percent, charge state is property 0x10.
//!
//! Protocol facts as documented by HeadsetControl and ckb-next.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use super::{Read, Spec, TIMEOUT, ask};
use crate::devices::Kind;
use crate::hid::Channel;

const VID: u16 = 0x1B1C;

const VOID_PIDS: &[u16] = &[
    0x1B27, 0x0A14, 0x0A16, 0x0A17, 0x0A1D, 0x0A1A, 0x1B2A, 0x1B23, 0x1B29, 0x0A55, 0x0A51, 0x0A52, 0x0A38, 0x0A4F,
    0x0A2B, 0x0A75, 0x0A56,
];

pub fn specs() -> Vec<Spec> {
    vec![
        Spec { name: None, ..Spec::new("Corsair", "", VID, VOID_PIDS, 0xFFC5, 0x01, Some(3), void) },
        // Every other Corsair device: try Bragi on its vendor-defined 64-byte collections.
        Spec {
            name: None,
            kind: Kind::Other,
            filter: Some(|c| c.is_vendor_defined() && c.output_len >= 64),
            ..Spec::new("Corsair", "", VID, &[], 0, 0, None, bragi)
        },
    ]
}

fn void(ch: &mut Channel) -> Option<Read> {
    let r = ask(ch, &[0xC9, 0x64], |r| r.len() >= 5 && r[0] == 0x64, TIMEOUT)?;
    if r.len() < 5 {
        return None;
    }
    let level = r[2] & 0x7F; // bit 7 = microphone boom up
    match r[4] {
        0 => Read::off(),
        1 | 2 if level <= 100 => Read::level(level, false),
        4 | 5 if level <= 100 => Read::level(level, true),
        _ => None,
    }
}

const GET: u8 = 0x02;
const PROP_BATTERY_LEVEL: u8 = 0x0F;
const PROP_BATTERY_STATUS: u8 = 0x10;
const TARGET_SELF: u8 = 0x08;
const TARGET_WIRELESS: u8 = 0x09; // device behind a receiver

#[derive(Clone, Copy, PartialEq)]
enum Framing {
    HeadsetNumbered,
    HeadsetUnnumbered,
    Peripheral,
}

/// Which framing/target each collection accepted, so later polls don't re-probe.
static KNOWN: LazyLock<Mutex<HashMap<String, (Framing, u8)>>> = LazyLock::new(Default::default);

fn bragi(ch: &mut Channel) -> Option<Read> {
    let known = KNOWN.lock().unwrap().get(&ch.info.path).copied();
    let routes: Vec<(Framing, u8)> = match known {
        Some(k) => vec![k],
        None => [TARGET_WIRELESS, TARGET_SELF]
            .iter()
            .flat_map(|&t| [Framing::HeadsetNumbered, Framing::HeadsetUnnumbered, Framing::Peripheral].map(|f| (f, t)))
            .collect(),
    };
    for (framing, target) in routes {
        let Some(tenths) = get_property(ch, framing, target, PROP_BATTERY_LEVEL) else { continue };
        if tenths == 0 || tenths > 1000 {
            continue;
        }
        KNOWN.lock().unwrap().insert(ch.info.path.clone(), (framing, target));
        let state = get_property(ch, framing, target, PROP_BATTERY_STATUS);
        return Read::level(((tenths + 5) / 10) as u8, state == Some(1));
    }
    KNOWN.lock().unwrap().remove(&ch.info.path);
    None
}

/// The little-endian property value, or None on silence / error status.
fn get_property(ch: &mut Channel, framing: Framing, target: u8, property: u8) -> Option<u32> {
    // Headsets: [report 0x02, target, GET, property] -> [0x01, source, GET, status, value LE...]
    //           (some receivers use unnumbered reports, so the same bytes follow a 0x00 report ID).
    // Peripherals (ckb-next): [report 0x00: target, GET, property] -> [.., GET, status, value LE...]
    let request: Vec<u8> = match framing {
        Framing::HeadsetNumbered => vec![0x02, target, GET, property],
        Framing::HeadsetUnnumbered => vec![0x00, 0x02, target, GET, property],
        Framing::Peripheral => vec![0x00, target, GET, property],
    };
    let source = if target == TARGET_SELF { 0x00 } else { 0x01 };
    let peripheral = framing == Framing::Peripheral;
    let reply = ch.request(&request, 300, |r| {
        if peripheral {
            r.len() >= 7 && r[1] == GET
        } else {
            r.len() >= 8 && r[0] == 0x01 && r[1] == source && r[2] == GET
        }
    })?;
    let at = if peripheral { 2 } else { 3 }; // status byte; value follows
    if reply[at] != 0x00 {
        return None; // 0x05 = no such property (e.g. receiver with nothing paired)
    }
    Some(u32::from_le_bytes(reply[at + 1..at + 5].try_into().ok()?))
}
