//! Ajazz AJ179 / AJ179P and AJ159P (PAW3950 version) mice, over their 2.4 GHz receiver or the USB cable.
//!
//! Protocol facts as documented by the aj179-linux project (github.com/Rockeyxx/aj179-linux, MIT), which reverse
//! engineered them from Ajazz's driver and verified them on an AJ179, and as used by Ajazz's own web configurator
//! for these models (ajazz.xcgaming.net); independent implementation.
//!
//! Both models share one receiver and cable (same USB IDs) and one protocol; the reply's device ID tells them apart.
//! The config interface (USB interface 2) uses 32-byte reports without report IDs:
//! `[command, status, seq, length, payload[27], checksum]`, checksum = sum of the payload bytes. Command `0x10`
//! ("get device info") is read-only and sent by Ajazz's own software whenever the mouse connects. Its reply carries:
//! `[4..8]` device ID, `[12]` charging, `[13]` battery %, `[14]` mouse connected (0 = asleep / out of range; the
//! receiver keeps reporting the last battery level then).
//!
//! Other Ajazz models use different protocols (and other chip vendors' USB IDs), so only these documented IDs are
//! matched: nothing is ever sent to an unknown device.

use super::{Read, Spec};
use crate::devices::Kind;
use crate::hid::Channel;

const RECEIVER_VID: u16 = 0x249A; // "XCTECH Wireless-Receiver"
const CABLE_VID: u16 = 0x248A; // "Maxxter"
const CMD_INFO: u8 = 0x10;
const REPORT_LEN: usize = 32;

pub fn specs() -> Vec<Spec> {
    let mouse = |vid, pids| Spec {
        kind: Kind::Mouse,
        // The config interface: 32-byte reports + the report-ID byte Windows counts.
        filter: Some(|c| c.input_len == REPORT_LEN + 1 && c.output_len == REPORT_LEN + 1),
        ..Spec::new("Ajazz", "Ajazz mouse", vid, pids, 0, 0, Some(2), read_battery)
    };
    vec![mouse(RECEIVER_VID, &[0x5C2F]), mouse(CABLE_VID, &[0x5C2E, 0x5D2E, 0x5E2E])]
}

/// Model name for the device ID in the info reply.
fn model(device_id: &[u8]) -> Option<&'static str> {
    match device_id {
        b"M179" | b"P179" => Some("Ajazz AJ179"), // M179: dual-mode (PAW3395); P179: PAW3950 version
        b"P630" => Some("Ajazz AJ159"),           // AJ159P, PAW3950 version
        _ => None,
    }
}

fn read_battery(ch: &mut Channel) -> Option<Read> {
    // [report ID 0] + command 0x10 with an empty payload (so the checksum is 0 too).
    let mut request = [0u8; REPORT_LEN + 1];
    request[1] = CMD_INFO;
    // Replies echo the command; unsolicited status frames (0xC0) are skipped.
    let reply = ch.request(&request, 3000, |r| r.len() >= REPORT_LEN && r[0] == CMD_INFO)?;
    crate::vlog!("ajazz device id: {:?}", String::from_utf8_lossy(&reply[4..8.min(reply.len())]));
    parse_info(&reply, ch.info.vid == CABLE_VID)
}

/// Interprets a `0x10` reply. None = not a valid info reply (try another collection / later).
fn parse_info(r: &[u8], wired: bool) -> Option<Read> {
    if r.len() < REPORT_LEN || r[0] != CMD_INFO || r[1] != 0x00 {
        return None;
    }
    let checksum = r[4..REPORT_LEN - 1].iter().fold(0u8, |a, &b| a.wrapping_add(b));
    if checksum != r[REPORT_LEN - 1] || !r[4..8].iter().all(|c| (0x20..=0x7E).contains(c)) {
        return None;
    }
    let (charging, battery, connected) = (r[12] == 0x01, r[13], r[14] != 0);
    if battery > 100 || (!connected && battery == 0) {
        return Read::off(); // receiver has no level for the mouse (yet)
    }
    // Over the cable the mouse itself answers; via the receiver, "not connected" means asleep or out of range.
    let asleep = !connected && !wired;
    Some(Read { percent: Some(battery), charging, offline: false, asleep, model: model(&r[4..8]) })
}

#[cfg(test)]
mod tests {
    use super::parse_info;

    /// Replies captured from a real AJ179 (aj179-linux docs/PROTOCOL.md): 82% asleep, 81% awake.
    fn reply(id: &[u8; 4], battery: u8, connected: u8) -> Vec<u8> {
        let mut r = vec![0u8; 32];
        r[..4].copy_from_slice(&[0x10, 0x00, 0x01, 0x0B]);
        r[4..8].copy_from_slice(id);
        r[8..15].copy_from_slice(&[0x00, 0x00, 0x95, 0x01, 0x00, battery, connected]);
        r[31] = r[4..31].iter().fold(0u8, |a, &b| a.wrapping_add(b));
        r
    }

    #[test]
    fn documented_replies() {
        // Byte-exact with the capture, including its checksum.
        assert_eq!(reply(b"M179", 0x52, 0x00)[31], 0xD6);
        assert_eq!(reply(b"M179", 0x51, 0x01)[31], 0xD6);
        let asleep = parse_info(&reply(b"M179", 0x52, 0x00), false).unwrap();
        assert_eq!((asleep.percent, asleep.asleep, asleep.charging), (Some(82), true, false));
        let awake = parse_info(&reply(b"M179", 0x51, 0x01), false).unwrap();
        assert_eq!((awake.percent, awake.asleep, awake.model), (Some(81), false, Some("Ajazz AJ179")));
    }

    #[test]
    fn model_from_device_id() {
        let model = |id: &[u8; 4]| parse_info(&reply(id, 50, 1), false).unwrap().model;
        assert_eq!(model(b"P630"), Some("Ajazz AJ159"));
        assert_eq!(model(b"P179"), Some("Ajazz AJ179"));
        assert_eq!(model(b"X999"), None); // unknown sibling model: generic name, still read
    }

    #[test]
    fn rejects_corrupt_or_foreign_replies() {
        let mut bad_sum = reply(b"M179", 0x51, 0x01);
        bad_sum[31] ^= 1;
        assert!(parse_info(&bad_sum, false).is_none());
        let mut error = reply(b"M179", 0x51, 0x01);
        error[1] = 0x02;
        assert!(parse_info(&error, false).is_none());
        let mut other_cmd = reply(b"M179", 0x51, 0x01);
        other_cmd[0] = 0xC0;
        assert!(parse_info(&other_cmd, false).is_none());
    }

    #[test]
    fn no_level_yet_is_offline() {
        assert!(parse_info(&reply(b"M179", 0x00, 0x00), false).unwrap().offline);
    }

    #[test]
    fn cable_is_never_asleep() {
        let r = parse_info(&reply(b"P630", 0x40, 0x00), true).unwrap();
        assert!(!r.asleep);
    }
}
