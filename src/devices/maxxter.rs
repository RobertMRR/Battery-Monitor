//! Mice on the Maxxter-chip receiver `248A:5B2F` (USB cable `248A:5B2E`): some Ajazz AJ159P versions and the Delux
//! M800 Pro share this receiver and its firmware; the reply names the model.
//!
//! Protocol facts as documented by Wireless-Device-Battery-Monitor (github.com/krisgfx9/Wireless-Device-Battery-Monitor,
//! GPL-3.0; confirmed there on a Delux M800 Pro); independent implementation.
//!
//! Feature report `0x0C` (33 bytes) on the vendor collection (usage page 0xFF00, usage 0x01). The read-only status
//! query `[0C][01][20][00][seq]` (the receiver wants an incrementing sequence number) is answered via GET_FEATURE 0x0C:
//! `[0]` 0x0C, `[2]` 0x20 (echo), `[6]` mouse connected, `[8..12]` ASCII model code (e.g. "M802"), `[18]` battery %,
//! `[19]` 0 on battery, 1 charging, 2 full.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::thread::sleep;
use std::time::Duration;

use super::{Battery, Kind, Provider};
use crate::hid::{self, HidInfo};

const VID: u16 = 0x248A;
const CABLE_PID: u16 = 0x5B2E;
const RECEIVER_PID: u16 = 0x5B2F;
const REPORT: u8 = 0x0C;
const QUERY: u8 = 0x20;

static SEQUENCE: AtomicU8 = AtomicU8::new(0);

/// Display name for the model code in the reply.
fn model_name(code: &str) -> String {
    match code {
        "M802" => "Delux M800 Pro".into(),
        _ => format!("Wireless mouse {code}"), // e.g. an AJ159P: see `--probe --verbose` for its code
    }
}

#[derive(Debug, PartialEq)]
struct Status {
    model: String,
    connected: bool,
    percent: u8,
    charging: bool,
}

#[derive(Default)]
pub struct Maxxter {
    last_known: HashMap<String, Battery>,
}

impl Provider for Maxxter {
    fn name(&self) -> &str {
        "Maxxter receiver"
    }

    fn poll(&mut self) -> Vec<Battery> {
        let mut out: Vec<Battery> = Vec::new();
        // Cable first: while it's plugged in, the mouse answers there and the receiver reports it as not connected.
        for pid in [CABLE_PID, RECEIVER_PID] {
            let collections: Vec<HidInfo> = hid::enumerate(|v, p| v == VID && p == pid)
                .into_iter()
                .filter(|c| c.usage_page == 0xFF00 && c.usage == 0x01 && c.feature_len >= 33)
                .collect();
            let Some(status) = collections.iter().find_map(|c| query(c, pid == RECEIVER_PID)) else { continue };
            let id = format!("maxxter:{}", status.model.to_lowercase());
            if out.iter().any(|b| b.id == id) {
                continue; // already read over the cable
            }
            if status.connected {
                let b = Battery {
                    id: id.clone(),
                    name: model_name(&status.model),
                    kind: Kind::Mouse,
                    percent: status.percent,
                    charging: status.charging,
                    stale: false,
                };
                self.last_known.insert(id, b.clone());
                out.push(b);
            } else if let Some(last) = self.last_known.get(&id) {
                out.push(Battery { stale: true, ..last.clone() }); // asleep / out of range
            }
        }
        out
    }
}

/// One status query on a collection. The receiver sometimes answers the first query with "not connected" while the
/// mouse is waking up, so it gets one more try.
fn query(c: &HidInfo, receiver: bool) -> Option<Status> {
    let h = hid::open(&c.path, 0, false)?;
    let mut status = None;
    for _ in 0..if receiver { 2 } else { 1 } {
        let mut req = vec![0u8; c.feature_len];
        req[..5].copy_from_slice(&[
            REPORT,
            0x01,
            QUERY,
            0x00,
            SEQUENCE.fetch_add(1, Ordering::Relaxed).wrapping_add(1),
        ]);
        if !hid::set_feature(&h, &req) {
            crate::vlog!("maxxter {:04X}: SetFeature failed", c.pid);
            return None;
        }
        sleep(Duration::from_millis(20));
        let mut resp = vec![0u8; c.feature_len];
        resp[0] = REPORT;
        if !hid::get_feature(&h, &mut resp) {
            return None;
        }
        crate::vlog!("maxxter {:04X} <- {}", c.pid, hid::hex(&resp, 24));
        status = parse(&resp);
        if status.as_ref().is_some_and(|s| s.connected) {
            break;
        }
    }
    status
}

fn parse(r: &[u8]) -> Option<Status> {
    if r.len() < 20 || r[0] != REPORT || r[2] != QUERY {
        return None;
    }
    let code = &r[8..12];
    if !code.iter().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let (percent, state) = (r[18], r[19]);
    if percent > 100 || state > 2 {
        return None;
    }
    Some(Status {
        model: String::from_utf8_lossy(code).into_owned(),
        connected: r[6] != 0 || percent > 0,
        percent: if state == 2 { 100 } else { percent },
        charging: state != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::{Status, model_name, parse};

    fn reply(connected: u8, percent: u8, state: u8) -> Vec<u8> {
        let mut r = vec![0u8; 33];
        r[..3].copy_from_slice(&[0x0C, 0x01, 0x20]);
        r[6] = connected;
        r[8..12].copy_from_slice(b"M802");
        r[18] = percent;
        r[19] = state;
        r
    }

    #[test]
    fn status_reply() {
        let s = parse(&reply(1, 64, 0)).unwrap();
        assert_eq!(s, Status { model: "M802".into(), connected: true, percent: 64, charging: false });
        assert!(parse(&reply(1, 64, 1)).unwrap().charging);
        assert_eq!(parse(&reply(1, 97, 2)).unwrap().percent, 100); // full on the charger
        assert!(!parse(&reply(0, 0, 0)).unwrap().connected); // receiver without a mouse
    }

    #[test]
    fn rejects_foreign_replies() {
        let mut other = reply(1, 50, 0);
        other[2] = 0x21; // answer to another command
        assert!(parse(&other).is_none());
        let mut garbage = reply(1, 50, 0);
        garbage[9] = 0x00; // model code not ASCII
        assert!(parse(&garbage).is_none());
        assert!(parse(&reply(1, 150, 0)).is_none());
        assert!(parse(&[0x0C, 0x01]).is_none());
    }

    #[test]
    fn names() {
        assert_eq!(model_name("M802"), "Delux M800 Pro");
        assert_eq!(model_name("P631"), "Wireless mouse P631");
    }
}
