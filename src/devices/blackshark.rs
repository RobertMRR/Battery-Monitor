//! Razer BlackShark headsets. Unlike Razer mice they don't use the 90-byte Razer feature report; each generation has
//! its own vendor protocol:
//!
//! * **V2 Pro (2023)** (`1532:0555` HyperSpeed dongle, `0556`): "PA" frames in 64-byte output report 0x02,
//!   `[02][80][len][00 00]['P']['A'][type][00][body…]`. The dongle relays commands to the headset only in "remote"
//!   mode (`type 0E, body 02 E1 01/00`), which Synapse switches on around every query and off again; the first frame
//!   after an idle pause only wakes the link. Customer GET: `type 08, body 03 <cmd> 00 00` with cmd 0x21 = battery %,
//!   0x2A = charging. Reply: `[12]` cmd echo, `[13]` 0x01 = reply, `[14]` length, `[15]` value.
//!   Sources: OpenRazer PR #2862 (from a USB capture of this dongle), HaloBattery, Wireless-Device-Battery-Monitor.
//! * **V2 HyperSpeed** (`1532:0565`): `[02][00][60|seq][00 00 00][04][00 00][80][cmd]…[62]=XOR` on usage page 0xFF14
//!   (0xFF00 on some firmware); reply `[2]` seq, `[10]` cmd, `[12]` length, `[13]` value, `[62]` XOR of `[1..62]`.
//!   Source: justik13/razer-blackshark-v2-hyperspeed-webhid (MIT), via Wireless-Device-Battery-Monitor.
//! * **V3 / V3 Pro** (`0579`/`057A`, `0576`/`0577`), interface 5: `[02][00][60][00 00][00][04][00 00][dir][class]…
//!   [62]=XOR of [0..62]`; class 0x21 → `[13]` battery %, `[14]` charging. Source: OpenRazer PR #2794.
//! * **V3 X** (`057D` dongle, `057C` cable), interface 0: 64-byte feature report 0x07, Razer-style
//!   `[07][status][txid][00 00 00][02][07][cmd]…[62]=XOR of [3..=60]`; cmd 0x80 → battery %, 0x84 → charging, both
//!   at `[10]`. Source: OpenRazer PR #2899 (verified on a 057D dongle).
//!
//! The V3 generation drops its 2.4 GHz link if asked every few seconds, so it is queried at most every 5 minutes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

use super::{Battery, Kind, Provider};
use crate::hid::{self, Channel, HidInfo};

const RAZER: u16 = 0x1532;
const V3_INTERVAL: Duration = Duration::from_secs(5 * 60);

fn model(pid: u16) -> Option<&'static str> {
    match pid {
        0x0555 | 0x0556 => Some("Razer BlackShark V2 Pro (2023)"),
        0x0565 => Some("Razer BlackShark V2 HyperSpeed"),
        0x0576 | 0x0577 => Some("Razer BlackShark V3 Pro"),
        0x0579 | 0x057A => Some("Razer BlackShark V3"),
        0x057C | 0x057D => Some("Razer BlackShark V3 X"),
        _ => None,
    }
}

#[derive(Default)]
pub struct BlackShark {
    /// Last reading per product id, with when it was taken (reused for the V3 generation's 5-minute interval).
    cache: HashMap<u16, (Instant, Battery)>,
}

impl Provider for BlackShark {
    fn name(&self) -> &str {
        "Razer BlackShark"
    }

    fn poll(&mut self) -> Vec<Battery> {
        let mut by_pid: HashMap<u16, Vec<HidInfo>> = HashMap::new();
        for c in hid::enumerate(|v, p| v == RAZER && model(p).is_some()) {
            by_pid.entry(c.pid).or_default().push(c);
        }
        let mut pids: Vec<u16> = by_pid.keys().copied().collect();
        pids.sort();

        let mut out = Vec::new();
        for pid in pids {
            let collections = &by_pid[&pid];
            let slow = matches!(pid, 0x0576 | 0x0577 | 0x0579 | 0x057A | 0x057C | 0x057D);
            if slow {
                if let Some((at, b)) = self.cache.get(&pid) {
                    if at.elapsed() < V3_INTERVAL {
                        out.push(b.clone());
                        continue;
                    }
                }
            }
            let reading = match pid {
                0x0555 | 0x0556 => read_v2_pro(collections),
                0x0565 => read_v2_hyperspeed(collections),
                0x057C | 0x057D => read_v3_x(collections),
                _ => read_v3(collections),
            };
            match reading {
                Some((percent, charging)) => {
                    let b = Battery {
                        id: format!("blackshark:{pid:04x}"),
                        name: model(pid).unwrap_or("Razer BlackShark").to_string(),
                        kind: Kind::Headset,
                        percent,
                        charging,
                        stale: false,
                    };
                    self.cache.insert(pid, (Instant::now(), b.clone()));
                    out.push(b);
                }
                None => {
                    self.cache.remove(&pid); // headset off / out of range: don't show an old level
                }
            }
        }
        out
    }
}

// ---- V2 Pro (2023): "PA" frames ------------------------------------------------------------------

fn pa_frame(len: u8, kind: u8, body: &[u8]) -> Vec<u8> {
    let mut f = vec![0u8; 64];
    f[..9].copy_from_slice(&[0x02, 0x80, len, 0x00, 0x00, b'P', b'A', kind, 0x00]);
    f[9..9 + body.len()].copy_from_slice(body);
    f
}

fn pa_remote(on: bool) -> Vec<u8> {
    pa_frame(0x07, 0x0E, &[0x02, 0xE1, on as u8])
}

fn pa_query(cmd: u8) -> Vec<u8> {
    pa_frame(0x08, 0x08, &[0x03, cmd, 0x00, 0x00])
}

fn pa_value(r: &[u8], cmd: u8) -> Option<u8> {
    (r.len() >= 16 && r[0] == 0x02 && r[12] == cmd && r[13] == 0x01 && r[14] >= 1).then(|| r[15])
}

/// Remote mode is the dongle's "relay to the headset" state; local is its normal state. Dropping the guard always
/// switches it back, also when a query fails half-way.
struct Remote<'a>(&'a mut Channel);

impl<'a> Remote<'a> {
    fn on(ch: &'a mut Channel) -> Remote<'a> {
        ch.discard();
        ch.write(&pa_remote(true)); // sacrificial: the first frame after an idle pause only wakes the link
        sleep(Duration::from_millis(35));
        Remote(ch)
    }

    fn query(&mut self, cmd: u8) -> Option<u8> {
        for _ in 0..3 {
            self.0.write(&pa_remote(true));
            sleep(Duration::from_millis(35));
            if !self.0.write(&pa_query(cmd)) {
                return None;
            }
            if let Some(r) = self.0.read_matching(150, |r| pa_value(r, cmd).is_some()) {
                return pa_value(&r, cmd);
            }
        }
        None
    }
}

impl Drop for Remote<'_> {
    fn drop(&mut self) {
        self.0.write(&pa_remote(false));
        sleep(Duration::from_millis(35));
    }
}

fn read_v2_pro(collections: &[HidInfo]) -> Option<(u8, bool)> {
    for c in collections.iter().filter(|c| c.usage_page == 0xFF00 && c.output_len >= 64 && c.input_len >= 16) {
        let Some(mut ch) = Channel::open(c, &[]) else { continue };
        let mut link = Remote::on(&mut ch);
        let Some(level) = link.query(0x21).filter(|&l| l <= 100) else { continue };
        let charging = link.query(0x2A).is_some_and(|v| v > 0);
        return Some((level, charging));
    }
    None
}

// ---- V2 HyperSpeed --------------------------------------------------------------------------------

static HS_SEQ: AtomicU8 = AtomicU8::new(0);

fn hs_frame(cmd: u8, seq: u8) -> Vec<u8> {
    let mut f = vec![0u8; 64];
    f[0] = 0x02;
    f[2] = seq;
    f[6] = 0x04;
    f[9] = 0x80; // route: dongle → headset
    f[10] = cmd;
    f[62] = f[..62].iter().fold(0, |a, b| a ^ b);
    f
}

fn hs_value(r: &[u8], cmd: u8, seq: u8) -> Option<u8> {
    if r.len() < 64 || r[0] != 0x02 || r[2] != seq || r[10] != cmd || r[12] < 1 {
        return None;
    }
    (r[1..62].iter().fold(0, |a, b| a ^ b) == r[62]).then(|| r[13])
}

fn read_v2_hyperspeed(collections: &[HidInfo]) -> Option<(u8, bool)> {
    // The control collection is on usage page 0xFF14; some firmware uses 0xFF00.
    for page in [0xFF14, 0xFF00] {
        for c in collections
            .iter()
            .filter(|c| c.usage_page == page && c.usage == 0x01 && c.output_len >= 64 && c.input_len >= 64)
        {
            let Some(mut ch) = Channel::open(c, &[]) else { continue };
            let mut query = |cmd: u8| {
                let seq = 0x60 | (HS_SEQ.fetch_add(1, Ordering::Relaxed) & 0x1F);
                let r = ch.request(&hs_frame(cmd, seq), 650, |r| hs_value(r, cmd, seq).is_some())?;
                hs_value(&r, cmd, seq)
            };
            let Some(level) = query(0x21).filter(|&l| l <= 100) else { continue };
            let charging = query(0x2A).is_some_and(|v| v > 0);
            return Some((level, charging));
        }
    }
    None
}

// ---- V3 / V3 Pro ----------------------------------------------------------------------------------

fn v3_packet(dir: u8, class: u8) -> Vec<u8> {
    let mut f = vec![0u8; 64];
    f[0] = 0x02;
    f[2] = 0x60;
    f[6] = 0x04;
    f[9] = dir;
    f[10] = class;
    f[62] = f[..62].iter().fold(0, |a, b| a ^ b);
    f
}

fn read_v3(collections: &[HidInfo]) -> Option<(u8, bool)> {
    let c = collections.iter().find(|c| c.interface() == Some(5) && c.output_len >= 64 && c.input_len >= 15)?;
    let mut ch = Channel::open(c, &[])?;
    // The handshake the headset expects before reads (init, charging state on both links).
    for (dir, class) in [(0x00, 0x02), (0x00, 0x2A), (0x80, 0x2A)] {
        ch.write(&v3_packet(dir, class));
    }
    sleep(Duration::from_millis(250));
    let r =
        ch.request(&v3_packet(0x80, 0x21), 600, |r| r.len() >= 15 && r[0] == 0x02 && r[2] == 0x60 && r[10] == 0x21)?;
    (r[13] <= 100).then(|| (r[13], r[14] != 0))
}

// ---- V3 X ------------------------------------------------------------------------------------------

static V3X_TXID: AtomicU8 = AtomicU8::new(0x40);

fn v3x_request(txid: u8, cmd: u8) -> Vec<u8> {
    let mut f = vec![0u8; 64];
    f[0] = 0x07;
    f[2] = txid;
    f[6] = 0x02;
    f[7] = 0x07;
    f[8] = cmd;
    f[62] = f[3..=60].iter().fold(0, |a, b| a ^ b);
    f
}

fn read_v3_x(collections: &[HidInfo]) -> Option<(u8, bool)> {
    let c = collections.iter().find(|c| c.interface() == Some(0) && c.feature_len >= 64)?;
    let h = hid::open(&c.path, 0, false)?;
    let get = |cmd: u8| -> Option<u8> {
        for _ in 0..4 {
            let txid = V3X_TXID.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
            if !hid::set_feature(&h, &v3x_request(txid, cmd)) {
                return None;
            }
            sleep(Duration::from_millis(60));
            let mut r = vec![0u8; c.feature_len];
            r[0] = 0x07;
            if !hid::get_feature(&h, &mut r) || r.len() < 63 {
                continue;
            }
            crate::vlog!("blackshark v3x cmd {cmd:02X} <- {}", hid::hex(&r, 16));
            if r[2] != txid || r[7] != 0x07 || r[8] != cmd || r[6] < 2 {
                continue;
            }
            match r[1] {
                0x02 => return Some(r[10]),
                0x01 => continue, // busy: ask again
                _ => return None, // e.g. headset off
            }
        }
        None
    };
    let level = get(0x80).filter(|&l| l <= 100)?;
    Some((level, get(0x84) == Some(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pa_frames_match_the_synapse_capture() {
        assert_eq!(&pa_remote(true)[..12], &[0x02, 0x80, 0x07, 0, 0, b'P', b'A', 0x0E, 0, 0x02, 0xE1, 0x01]);
        assert_eq!(&pa_query(0x21)[..13], &[0x02, 0x80, 0x08, 0, 0, b'P', b'A', 0x08, 0, 0x03, 0x21, 0, 0]);
        assert_eq!(pa_query(0x21).len(), 64);
    }

    #[test]
    fn pa_reply() {
        let mut r = vec![0u8; 64];
        r[0] = 0x02;
        r[12..16].copy_from_slice(&[0x21, 0x01, 0x01, 77]);
        assert_eq!(pa_value(&r, 0x21), Some(77));
        assert_eq!(pa_value(&r, 0x2A), None); // other command
        r[13] = 0x02; // a notification, not the reply
        assert_eq!(pa_value(&r, 0x21), None);
    }

    #[test]
    fn hyperspeed_checksums() {
        let f = hs_frame(0x21, 0x61);
        assert_eq!(f[62], 0x02 ^ 0x61 ^ 0x04 ^ 0x80 ^ 0x21);
        let mut r = vec![0u8; 64];
        r[0] = 0x02;
        r[2] = 0x61;
        r[10] = 0x21;
        r[12] = 0x01;
        r[13] = 55;
        r[62] = r[1..62].iter().fold(0, |a, b| a ^ b);
        assert_eq!(hs_value(&r, 0x21, 0x61), Some(55));
        r[62] ^= 1;
        assert_eq!(hs_value(&r, 0x21, 0x61), None);
    }

    #[test]
    fn v3_and_v3x_checksums() {
        assert_eq!(v3_packet(0x80, 0x21)[62], 0x02 ^ 0x60 ^ 0x04 ^ 0x80 ^ 0x21);
        let r = v3x_request(0x41, 0x80);
        assert_eq!((r[0], r[2], r[6], r[7], r[8]), (0x07, 0x41, 0x02, 0x07, 0x80));
        assert_eq!(r[62], 0x02 ^ 0x07 ^ 0x80); // bytes 3..=60 only
    }

    #[test]
    fn model_names() {
        assert_eq!(model(0x0555), Some("Razer BlackShark V2 Pro (2023)"));
        assert_eq!(model(0x057D), Some("Razer BlackShark V3 X"));
        assert_eq!(model(0x00C1), None);
    }
}
