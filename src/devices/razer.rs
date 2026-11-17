//! Razer wireless mice/keyboards via Razer's 90-byte HID feature report protocol (as documented by OpenRazer).
//! Battery level: command class 0x07, id 0x80 -> arguments[1] in 0..255. Charging: 0x07/0x84 -> arguments[1].

use std::collections::{HashMap, HashSet};
use std::thread::sleep;
use std::time::Duration;

use super::{Battery, Kind, Provider, guess_kind};
use crate::hid::{self, Handle, HidInfo};

const RAZER: u16 = 0x1532;
const REPORT_LEN: usize = 91; // report ID 0 + 90-byte payload

const STATUS_BUSY: u8 = 0x01;
const STATUS_OK: u8 = 0x02;
const STATUS_TIMEOUT: u8 = 0x04; // dongle present, device asleep / out of range

// Transaction IDs differ between models; try the common ones and remember what worked.
const TRANSACTION_IDS: [u8; 3] = [0x1F, 0xFF, 0x3F];

#[derive(Default)]
pub struct Razer {
    routes: HashMap<u16, (String, u8)>, // pid -> (collection path, transaction id)
    unsupported: HashSet<u16>,
    last_known: HashMap<u16, Battery>,
}

impl Provider for Razer {
    fn name(&self) -> &str {
        "Razer"
    }

    fn poll(&mut self) -> Vec<Battery> {
        let mut by_pid: HashMap<u16, Vec<HidInfo>> = HashMap::new();
        for c in hid::enumerate(|v, _| v == RAZER).into_iter().filter(|c| c.feature_len == REPORT_LEN) {
            by_pid.entry(c.pid).or_default().push(c);
        }

        let mut pids: Vec<u16> = by_pid.keys().copied().collect();
        pids.sort();
        let mut out = Vec::new();
        for pid in pids {
            if self.unsupported.contains(&pid) {
                continue;
            }
            let collections = &by_pid[&pid];
            let name = collections.iter().find_map(|c| c.product.clone()).unwrap_or_else(|| format!("Razer {pid:04X}"));
            match self.read(pid, collections) {
                Some((level, charging)) => {
                    let b = Battery {
                        id: format!("razer:{pid:04x}"),
                        kind: guess_kind(&name, Kind::Mouse),
                        name,
                        percent: (level as f32 * 100.0 / 255.0).round() as u8,
                        charging,
                        stale: false,
                    };
                    self.last_known.insert(pid, b.clone());
                    out.push(b);
                }
                None => {
                    if let Some(last) = self.last_known.get(&pid) {
                        out.push(Battery { stale: true, ..last.clone() });
                    }
                }
            }
        }
        self.last_known.retain(|pid, _| by_pid.contains_key(pid));
        out
    }
}

impl Razer {
    fn read(&mut self, pid: u16, collections: &[HidInfo]) -> Option<(u8, bool)> {
        if let Some((path, tx)) = self.routes.get(&pid).cloned() {
            if collections.iter().any(|c| c.path == path) {
                let (status, level, charging) = query(&path, tx);
                if status == STATUS_OK {
                    return Some((level, charging));
                }
                if status == STATUS_TIMEOUT {
                    return None;
                }
            }
            self.routes.remove(&pid); // re-plugged or stopped answering: rediscover
        }

        let mut saw_timeout = false;
        'collections: for c in collections {
            for tx in TRANSACTION_IDS {
                let (status, level, charging) = query(&c.path, tx);
                if status == STATUS_OK {
                    self.routes.insert(pid, (c.path.clone(), tx));
                    return Some((level, charging));
                }
                if status == STATUS_TIMEOUT {
                    // The dongle accepted the request; the device just isn't answering.
                    saw_timeout = true;
                    continue 'collections;
                }
            }
        }
        // Nothing answered and nothing reported "asleep": no battery here (e.g. a microphone).
        if !saw_timeout {
            self.unsupported.insert(pid);
        }
        None
    }
}

fn query(path: &str, tx: u8) -> (u8, u8, bool) {
    let Some(h) = hid::open(path, 0, false) else { return (0, 0, false) };
    let (status, level) = command(&h, tx, 0x07, 0x80);
    if status != STATUS_OK {
        return (status, 0, false);
    }
    let (charge_status, charging) = command(&h, tx, 0x07, 0x84);
    (STATUS_OK, level, charge_status == STATUS_OK && charging == 1)
}

/// A "get" request: [0] = report ID 0, then the 90-byte payload: status, transaction id, remaining packets (2),
/// protocol type, data size, command class, command id, arguments[80], crc, reserved.
fn request(tx: u8, class: u8, id: u8) -> [u8; REPORT_LEN] {
    let mut req = [0u8; REPORT_LEN];
    req[2] = tx;
    req[6] = 0x02;
    req[7] = class;
    req[8] = id;
    req[89] = req[3..=88].iter().fold(0, |a, b| a ^ b); // XOR of payload bytes 2..87
    req
}

/// Returns the response status byte and arguments[1].
fn command(h: &Handle, tx: u8, class: u8, id: u8) -> (u8, u8) {
    let req = request(tx, class, id);
    if !hid::set_feature(h, &req) {
        crate::vlog!("razer tx={tx:02X} SetFeature failed");
        return (0, 0);
    }

    // Wireless dongles relay the request over the air, so the answer can take a while: keep re-reading while
    // the dongle says "busy" or "no response yet".
    let mut status = 0;
    for delay in [30, 60, 120, 250] {
        sleep(Duration::from_millis(delay));
        let mut resp = [0u8; REPORT_LEN];
        if !hid::get_feature(h, &mut resp) {
            return (0, 0);
        }
        crate::vlog!("razer tx={tx:02X} cmd={class:02X}/{id:02X} +{delay}ms -> {}", hid::hex(&resp, 12));
        status = resp[1];
        if status == STATUS_BUSY || status == STATUS_TIMEOUT {
            continue;
        }
        if status == STATUS_OK && (resp[2] != tx || resp[7] != class || resp[8] != id) {
            return (0, 0); // answer to someone else's request (e.g. Synapse polling at the same time)
        }
        return (status, resp[10]);
    }
    (status, 0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn battery_request_layout_and_checksum() {
        let r = super::request(0x1F, 0x07, 0x80);
        assert_eq!(r.len(), 91);
        assert_eq!((r[0], r[1], r[2]), (0x00, 0x00, 0x1F)); // report id, status, transaction id
        assert_eq!((r[6], r[7], r[8]), (0x02, 0x07, 0x80)); // data size, class, command
        assert_eq!(r[89], 0x02 ^ 0x07 ^ 0x80); // XOR of payload bytes 2..87 (the transaction id is excluded)
    }
}
