//! Corsair wireless devices. Two protocols:
//! * legacy VOID-family headsets: output report 0xC9 0x64, status input report 0x64;
//! * "Bragi" (Virtuoso XT/SE, VOID V2, HS80 MAX, and probably Slipstream mice/keyboards): property reads; battery
//!   level is property 0x0F in tenths of a percent, charge state is property 0x10.
//!
//! Protocol facts as documented by HeadsetControl, ckb-next and the Linux `hid-corsair-void` driver.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use super::{Read, Spec, TIMEOUT, ask};
use crate::devices::Kind;
use crate::hid::Channel;

const VID: u16 = 0x1B1C;

const VOID_PIDS: &[u16] = &[
    0x0A0C, 0x0A2B, 0x1B23, 0x1B25, 0x1B27, // VOID Wireless
    0x0A14, 0x0A16, 0x0A1A, // VOID PRO Wireless
    0x0A51, 0x0A55, 0x0A75, // VOID ELITE Wireless
    0x0A38, 0x0A4F, // HS70 / HS70 PRO Wireless
    0x1B29, 0x1B2A, 0x0A17, 0x0A1D, 0x0A52, 0x0A56, // wired USB models: answer "not connected", so never shown
];

/// The dongles report a generic USB name ("HID Interface"), so the model comes from the product ID.
fn void_model(pid: u16) -> Option<&'static str> {
    match pid {
        0x0A0C | 0x0A2B | 0x1B23 | 0x1B25 | 0x1B27 => Some("Corsair VOID Wireless"),
        0x0A14 | 0x0A16 | 0x0A1A => Some("Corsair VOID PRO Wireless"),
        0x0A51 | 0x0A55 | 0x0A75 => Some("Corsair VOID ELITE Wireless"),
        0x0A38 => Some("Corsair HS70 Wireless"),
        0x0A4F => Some("Corsair HS70 PRO Wireless"),
        _ => None,
    }
}

pub fn specs() -> Vec<Spec> {
    vec![
        Spec::new("Corsair", "Corsair headset", VID, VOID_PIDS, 0xFFC5, 0x01, Some(3), void),
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
    parse_void(&r).map(|read| Read { model: void_model(ch.info.pid), ..read })
}

const VOID_CONNECTED: u8 = 0xB1; // 177

/// Status report 0x64: `[2]` level (bit 7 = microphone boom up), `[3]` 0xB1 while the headset is connected,
/// `[4]` 0 unknown, 1 normal, 2 low, 3 critical, 4 full (on the charger), 5 charging.
fn parse_void(r: &[u8]) -> Option<Read> {
    if r.len() < 5 || r[0] != 0x64 {
        return None;
    }
    let level = r[2] & 0x7F;
    if r[3] != VOID_CONNECTED || r[4] == 0 {
        return Read::off(); // headset off / out of range (or a wired model, which has no battery)
    }
    match r[4] {
        1..=3 if level <= 100 => Read::level(level, false),
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

#[cfg(test)]
mod tests {
    use super::{parse_void, void_model};

    fn status(level: u8, connection: u8, state: u8) -> [u8; 5] {
        [0x64, 0x00, level, connection, state]
    }

    #[test]
    fn void_levels_and_states() {
        let r = parse_void(&status(0x80 | 55, 0xB1, 1)).unwrap(); // mic boom up flag is stripped
        assert_eq!((r.percent, r.charging, r.offline), (Some(55), false, false));
        assert_eq!(parse_void(&status(4, 0xB1, 3)).unwrap().percent, Some(4)); // critical: still shown
        assert!(parse_void(&status(80, 0xB1, 5)).unwrap().charging);
        assert!(parse_void(&status(100, 0xB1, 4)).unwrap().charging); // full, on the charger
    }

    #[test]
    fn void_headset_off_or_unknown() {
        assert!(parse_void(&status(55, 0x00, 1)).unwrap().offline); // receiver without headset
        assert!(parse_void(&status(55, 0xB1, 0)).unwrap().offline); // no battery information
        assert!(parse_void(&status(55, 0xB1, 9)).is_none()); // unknown state
        assert!(parse_void(&[0x66, 0, 0, 0, 0]).is_none()); // firmware report, not status
    }

    #[test]
    fn void_model_names() {
        assert_eq!(void_model(0x0A55), Some("Corsair VOID ELITE Wireless"));
        assert_eq!(void_model(0x0A14), Some("Corsair VOID PRO Wireless"));
        assert_eq!(void_model(0x0A17), None); // wired USB model
    }
}
