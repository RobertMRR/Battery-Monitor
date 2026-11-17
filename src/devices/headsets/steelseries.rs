//! SteelSeries Arctis wireless headsets (protocol facts as documented by HeadsetControl).

use super::{Read, Spec, TIMEOUT, ask, map};
use crate::hid::Channel;

const VID: u16 = 0x1038;

pub fn specs() -> Vec<Spec> {
    vec![
        Spec::new(
            "SteelSeries",
            "Arctis 1 / 7X / 7P",
            VID,
            &[0x12B3, 0x12B6, 0x12D7, 0x12D5],
            0xFF43,
            0x0202,
            Some(3),
            legacy_arctis1,
        ),
        Spec::new(
            "SteelSeries",
            "Arctis 7 / Pro",
            VID,
            &[0x1260, 0x12AD, 0x1252, 0x1280],
            0xFF43,
            0x0202,
            Some(5),
            legacy_arctis7,
        ),
        Spec::new("SteelSeries", "Arctis 9", VID, &[0x12C2, 0x12C4], 0, 0, None, arctis9),
        Spec::new("SteelSeries", "Arctis Pro Wireless", VID, &[0x1290], 0, 0, None, arctis_pro_wireless),
        Spec::new(
            "SteelSeries",
            "Arctis 7+",
            VID,
            &[0x220E, 0x2212, 0x2216, 0x2236],
            0xFFC0,
            0x01,
            Some(3),
            arctis7_plus,
        ),
        Spec::new(
            "SteelSeries",
            "Arctis Nova 7",
            VID,
            &[
                0x2202, 0x22A1, 0x227E, 0x2206, 0x2258, 0x229E, 0x22AD, 0x223A, 0x22A9, 0x227A, 0x22A4, 0x22A5, 0x220A,
                0x22A7, 0x2298,
            ], // incl. Nova 7P
            0xFFC0,
            0x01,
            Some(3),
            nova7,
        ),
        Spec::new("SteelSeries", "Arctis Nova 5", VID, &[0x2232, 0x2253], 0xFFC0, 0x01, Some(3), nova5_or_3p),
        Spec::new(
            "SteelSeries",
            "Arctis Nova 3P/3X Wireless",
            VID,
            &[0x2269, 0x226D],
            0xFFC0,
            0x01,
            Some(3),
            nova5_or_3p,
        ),
        Spec::new("SteelSeries", "Arctis Nova Pro Wireless", VID, &[0x12E0, 0x12E5], 0, 0, Some(4), nova_pro_wireless),
        Spec::new("SteelSeries", "Arctis GameBuds", VID, &[0x230A], 0xFFC0, 0x01, Some(3), gamebuds),
    ]
}

/// Nova-generation status request: report 0, command 0xB0. The reply starts with the echoed 0xB0.
fn nova_status(ch: &mut Channel) -> Option<Vec<u8>> {
    ask(ch, &[0x00, 0xB0], |r| r[0] == 0xB0, TIMEOUT)
}

fn legacy_arctis1(ch: &mut Channel) -> Option<Read> {
    let r = ask(ch, &[0x06, 0x12], |r| r.len() >= 4 && r[0] == 0x06 && r[1] == 0x12, TIMEOUT)?;
    if r.len() < 4 {
        return None;
    }
    if r[2] == 0x01 {
        return Read::off();
    }
    if r[3] <= 100 { Read::level(r[3], false) } else { None }
}

fn legacy_arctis7(ch: &mut Channel) -> Option<Read> {
    let r = ask(ch, &[0x06, 0x18], |r| r.len() >= 3 && r[0] == 0x06 && r[1] == 0x18, TIMEOUT)?;
    if r.len() >= 3 && r[2] <= 100 { Read::level(r[2], false) } else { None }
}

fn arctis9(ch: &mut Channel) -> Option<Read> {
    let r = ask(ch, &[0x00, 0x20], |_| true, TIMEOUT)?;
    if r.len() < 5 {
        return None;
    }
    Read::level(map(r[3], 0x64, 0x9A), r[4] == 0x01)
}

fn arctis_pro_wireless(ch: &mut Channel) -> Option<Read> {
    let status = ask(ch, &[0x41, 0xAA], |_| true, TIMEOUT)?;
    if status[0] == 0x02 {
        return Read::off();
    }
    let r = ask(ch, &[0x40, 0xAA], |_| true, TIMEOUT)?;
    if r[0] > 4 { None } else { Read::level(map(r[0], 0, 4), false) }
}

fn arctis7_plus(ch: &mut Channel) -> Option<Read> {
    let r = nova_status(ch)?;
    if r.len() < 4 {
        return None;
    }
    if r[1] == 0x01 {
        return Read::off();
    }
    Read::level(map(r[2], 0, 4), r[3] == 0x01)
}

/// Older Nova 7 firmware reports 0..4 bars; newer firmware (other product IDs) reports a percentage.
const NOVA7_DISCRETE: [u16; 6] = [0x2202, 0x2206, 0x220A, 0x223A, 0x227A, 0x22A4];

fn nova7(ch: &mut Channel) -> Option<Read> {
    let r = nova_status(ch)?;
    if r.len() < 4 {
        return None;
    }
    if r[3] == 0x00 {
        return Read::off();
    }
    let level = if NOVA7_DISCRETE.contains(&ch.info.pid) { map(r[2], 0, 4) } else { r[2].min(100) };
    Read::level(level, r[3] == 0x01 || r[3] == 0x02)
}

fn nova5_or_3p(ch: &mut Channel) -> Option<Read> {
    let r = nova_status(ch)?;
    if r.len() < 5 {
        return None;
    }
    if r[1] == 0x02 {
        return Read::off();
    }
    if r[3] <= 100 { Read::level(r[3], r[4] == 0x01) } else { None }
}

fn nova_pro_wireless(ch: &mut Channel) -> Option<Read> {
    let r = ask(ch, &[0x06, 0xB0], |r| r.len() >= 16 && r[0] == 0x06 && r[1] == 0xB0, TIMEOUT)?;
    if r.len() < 16 {
        return None;
    }
    if r[15] == 0x01 {
        return Read::off();
    }
    Read::level(map(r[6], 0, 8), r[15] == 0x02)
}

fn gamebuds(ch: &mut Channel) -> Option<Read> {
    let r = nova_status(ch)?;
    if r.len() < 7 {
        return None;
    }
    const OUT_OF_CASE: u8 = 0x03;
    let left = (r[3] == OUT_OF_CASE).then_some(r[5]);
    let right = (r[4] == OUT_OF_CASE).then_some(r[6]);
    match (left, right) {
        (Some(l), Some(r)) => Read::level(l.min(r), false),
        (Some(x), None) | (None, Some(x)) => Read::level(x, false),
        (None, None) => Read::off(), // both buds docked
    }
}
