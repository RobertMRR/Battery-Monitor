//! HyperX wireless headsets (Kingston VID 0x0951 for older models, HP VID 0x03F0 since the HP acquisition).
//! Protocol facts as documented by HeadsetControl.

use super::{Read, Spec, TIMEOUT, ask};
use crate::hid::Channel;

const KINGSTON: u16 = 0x0951;
const HP: u16 = 0x03F0;

pub fn specs() -> Vec<Spec> {
    vec![
        Spec::new("HyperX", "HyperX Cloud II Wireless", HP, &[0x0696, 0x018B], 0xFF90, 0x0303, None, cloud2_wireless),
        Spec::new("HyperX", "HyperX Cloud Alpha Wireless", HP, &[0x098D], 0, 0, None, cloud_alpha_wireless),
        Spec::new(
            "HyperX",
            "HyperX Cloud Alpha 2 Wireless",
            HP,
            &[0x08BE],
            0xFF13,
            0xFF00,
            Some(2),
            cloud_alpha2_wireless,
        ),
        Spec::new("HyperX", "HyperX Cloud Flight", KINGSTON, &[0x16C4, 0x1723], 0, 0, None, cloud_flight),
    ]
}

fn cloud2_wireless(ch: &mut Channel) -> Option<Read> {
    let reply = |cmd: u8| move |r: &[u8]| r.len() >= 8 && r[..4] == [0x06, 0xFF, 0xBB, cmd];
    let level = ch.request(&[0x06, 0xFF, 0xBB, 0x02], TIMEOUT, reply(0x02))?;
    if level[7] > 100 {
        return None;
    }
    let charging = ch.request(&[0x06, 0xFF, 0xBB, 0x03], TIMEOUT, reply(0x03));
    Read::level(level[7], charging.is_some_and(|c| c[4] == 1))
}

fn cloud_alpha_wireless(ch: &mut Channel) -> Option<Read> {
    let reply = |cmd: u8| move |r: &[u8]| r.len() >= 4 && r[..3] == [0x21, 0xBB, cmd];
    let conn = ch.request(&[0x21, 0xBB, 0x03], 2000, reply(0x03))?;
    if conn[3] == 0x01 {
        return Read::off();
    }
    let charge = ch.request(&[0x21, 0xBB, 0x0C], 2000, reply(0x0C));
    if charge.is_some_and(|c| c[3] == 0x01) {
        return Read::charging_unknown_level(); // level isn't reported while charging
    }
    let level = ch.request(&[0x21, 0xBB, 0x0B], 2000, reply(0x0B))?;
    if level[3] <= 100 { Read::level(level[3], false) } else { None }
}

fn cloud_alpha2_wireless(ch: &mut Channel) -> Option<Read> {
    // Replies come back as [command + 1, sub-command, data...]; acknowledgements and notifications are skipped.
    let conn = ch.request(&[0x52, 0x01], TIMEOUT, |r| r.len() >= 3 && r[0] == 0x53 && r[1] == 0x01)?;
    if conn[2] == 0x00 {
        return Read::off();
    }
    let r = ch.request(&[0x50, 0x02], TIMEOUT, |r| r.len() >= 4 && r[0] == 0x51 && r[1] == 0x02)?;
    if r[2] <= 100 { Read::level(r[2], r[3] == 0x01) } else { None }
}

fn cloud_flight(ch: &mut Channel) -> Option<Read> {
    let r = ask(ch, &[0x21, 0xFF, 0x05], |_| true, 2000)?;
    if r.len() < 5 {
        return None;
    }
    let mv = (r[3] as i32) << 8 | r[4] as i32;
    if !(3000..=4500).contains(&mv) {
        return None; // not a battery reply
    }
    if mv > 0x100B {
        return Read::charging_unknown_level(); // above ~4.1 V the headset is on the charger
    }
    Read::level(estimate_flight(mv), false)
}

/// Cloud Flight discharge curve (polynomial fit published with HeadsetControl).
fn estimate_flight(mv: i32) -> u8 {
    if mv <= 3648 {
        return (0.00125 * mv as f64).round() as u8;
    }
    if mv > 3975 {
        return 100;
    }
    let v = mv as f64;
    let pct = 0.00000002547505 * v.powi(4) - 0.0003900299 * v.powi(3) + 2.238321 * v * v - 5706.256 * v + 5452299.0;
    pct.round().clamp(0.0, 100.0) as u8
}
