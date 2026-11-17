//! Table-driven readers for simple request/reply vendor protocols (mostly headsets, plus a few mice and dongles).
//! Each `Spec` names the USB IDs, which HID collection to talk to, and a function that asks for the battery.
//! Requests are in hidapi form (first byte = report ID), see `hid`.
//!
//! Protocol facts (command bytes, offsets, product IDs) come from HeadsetControl, Solaar, ckb-next and
//! aj179-linux; the code is an independent implementation.

pub mod ajazz;
pub mod centurion;
pub mod corsair;
pub mod hyperx;
pub mod steelseries;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::{Battery, Kind, Provider, guess_kind};
use crate::hid::{self, Channel, HidInfo};

pub const TIMEOUT: u32 = 1000;

/// Result of one battery query.
#[derive(Clone, Copy, Debug)]
pub struct Read {
    /// None when the device answered without a level (some only say "charging").
    pub percent: Option<u8>,
    pub charging: bool,
    /// The receiver answered, but the headset itself is off or out of range.
    pub offline: bool,
    /// The device is asleep and `percent` is the last level the receiver knows (shown faded).
    pub asleep: bool,
    /// Model name reported by the device itself, when one receiver/ID serves several models.
    pub model: Option<&'static str>,
}

impl Read {
    pub fn level(percent: u8, charging: bool) -> Option<Read> {
        Some(Read { percent: Some(percent.min(100)), charging, offline: false, asleep: false, model: None })
    }
    pub fn charging_unknown_level() -> Option<Read> {
        Some(Read { percent: None, charging: true, offline: false, asleep: false, model: None })
    }
    pub fn off() -> Option<Read> {
        Some(Read { percent: None, charging: false, offline: true, asleep: false, model: None })
    }
}

pub type ReadFn = fn(&mut Channel) -> Option<Read>;

pub struct Spec {
    pub brand: &'static str,
    /// Display name; None = the device's USB product string.
    pub name: Option<&'static str>,
    pub vid: u16,
    /// Empty = every product of `vid`.
    pub pids: &'static [u16],
    /// 0 = any collection (vendor-defined ones are tried first).
    pub usage_page: u16,
    pub usage: u16,
    pub interface: Option<u8>,
    /// Returns None when this collection gave no usable answer, so the next candidate is tried.
    pub read: ReadFn,
    pub kind: Kind,
    /// Extra condition on the collection, for specs that match broadly (e.g. a whole vendor).
    pub filter: Option<fn(&HidInfo) -> bool>,
}

impl Spec {
    /// A spec for one model family on a specific collection. Positional on purpose: tables read as one row per model.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        brand: &'static str,
        name: &'static str,
        vid: u16,
        pids: &'static [u16],
        usage_page: u16,
        usage: u16,
        interface: Option<u8>,
        read: ReadFn,
    ) -> Spec {
        Spec {
            brand,
            name: Some(name),
            vid,
            pids,
            usage_page,
            usage,
            interface,
            read,
            kind: Kind::Headset,
            filter: None,
        }
    }

    fn matches(&self, vid: u16, pid: u16) -> bool {
        vid == self.vid && (self.pids.is_empty() || self.pids.contains(&pid))
    }

    fn accepts(&self, c: &HidInfo) -> bool {
        c.output_len > 0
            && c.input_len > 0
            && (self.usage_page == 0 || (c.usage_page == self.usage_page && c.usage == self.usage))
            && (self.interface.is_none() || c.interface() == self.interface)
            && self.filter.is_none_or(|f| f(c))
    }
}

pub struct HeadsetProvider {
    name: &'static str,
    specs: Vec<Spec>,
    working_path: HashMap<String, String>,
    last_percent: HashMap<String, u8>,
    backoff: HashMap<String, (u32, Instant)>,
}

impl HeadsetProvider {
    pub fn new(name: &'static str, specs: Vec<Spec>) -> HeadsetProvider {
        HeadsetProvider {
            name,
            specs,
            working_path: HashMap::new(),
            last_percent: HashMap::new(),
            backoff: HashMap::new(),
        }
    }

    fn read(&mut self, key: &str, spec: &Spec, collections: &[HidInfo]) -> Option<Read> {
        let preferred = self.working_path.get(key).cloned();
        let mut candidates: Vec<&HidInfo> = collections.iter().filter(|c| spec.accepts(c)).collect();
        candidates.sort_by_key(|c| (Some(&c.path) != preferred.as_ref(), !c.is_vendor_defined()));
        for c in candidates {
            let Some(mut ch) = Channel::open(c, &[]) else { continue };
            if let Some(r) = (spec.read)(&mut ch) {
                self.working_path.insert(key.to_string(), c.path.clone());
                return Some(r);
            }
        }
        None
    }
}

impl Provider for HeadsetProvider {
    fn name(&self) -> &str {
        self.name
    }

    fn poll(&mut self) -> Vec<Battery> {
        let collections = hid::enumerate(|v, p| self.specs.iter().any(|s| s.matches(v, p)));

        // One device per VID:PID. (Interfaces of a composite USB device have unrelated instance IDs, and two
        // identical wireless headsets on one PC are rare enough not to matter.)
        let mut devices: Vec<(String, Vec<HidInfo>)> = Vec::new();
        for c in collections {
            let key = format!("{:04x}:{:04x}", c.vid, c.pid);
            match devices.iter_mut().find(|(k, _)| *k == key) {
                Some((_, list)) => list.push(c),
                None => devices.push((key, vec![c])),
            }
        }

        let specs = std::mem::take(&mut self.specs);
        let mut out = Vec::new();
        for (key, list) in &devices {
            let Some(spec) = specs.iter().find(|s| s.matches(list[0].vid, list[0].pid)) else { continue };
            let misses = match self.backoff.get(key) {
                Some(&(_, retry)) if Instant::now() < retry => continue,
                Some(&(m, _)) => m,
                None => 0,
            };
            let Some(r) = self.read(key, spec, list) else {
                // Nothing answered (unsupported model, or no battery at all): retry less and less often, up to every
                // 10 minutes, so devices like wired keyboards don't cost a timeout on every poll.
                let m = misses + 1;
                self.backoff
                    .insert(key.clone(), (m, Instant::now() + Duration::from_secs(60 * (m * m).min(10) as u64)));
                continue;
            };
            self.backoff.remove(key);
            if r.offline {
                continue;
            }

            let id = format!("{}:{key}", spec.brand.to_lowercase());
            // Answered without a level (e.g. "charging"): show the last level we saw, if any.
            let Some(percent) = r.percent.or_else(|| self.last_percent.get(&id).copied()) else { continue };
            self.last_percent.insert(id.clone(), percent);

            let name = r
                .model
                .or(spec.name)
                .map(str::to_string)
                .or_else(|| list.iter().find_map(|c| c.product.clone()))
                .unwrap_or_else(|| spec.brand.to_string());
            let kind = if spec.name.is_none() { guess_kind(&name, spec.kind) } else { spec.kind };
            out.push(Battery { id, name, kind, percent, charging: r.charging, stale: r.asleep });
        }
        self.specs = specs;
        out
    }
}

/// Sends `request` and returns the first reply that satisfies `expected`. If none does before the timeout, falls
/// back to the first report that arrived at all: some firmwares don't echo the command, and reading "the next
/// report" is also what hidapi-based tools do.
pub fn ask(ch: &mut Channel, request: &[u8], expected: impl Fn(&[u8]) -> bool, timeout_ms: u32) -> Option<Vec<u8>> {
    ch.discard();
    if !ch.write(request) {
        return None;
    }
    let mut first_seen = None;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
    loop {
        let left = deadline.saturating_duration_since(Instant::now()).as_millis() as u32;
        if left == 0 {
            return first_seen;
        }
        let Some(r) = ch.read_matching(left, |_| true) else { return first_seen };
        if expected(&r) {
            return Some(r);
        }
        first_seen.get_or_insert(r);
    }
}

/// Linear map of a raw device value onto 0..100, clamped.
pub fn map(value: u8, in_min: i32, in_max: i32) -> u8 {
    ((value as i32 - in_min) * 100 / (in_max - in_min).max(1)).clamp(0, 100) as u8
}

/// Piecewise-linear lookup of a Li-ion discharge curve given as (millivolts, percent), highest voltage first.
pub fn from_voltage(mv: i32, curve: &[(i32, i32)]) -> u8 {
    if mv >= curve[0].0 {
        return curve[0].1 as u8;
    }
    for w in curve.windows(2) {
        let ((hi_mv, hi_pct), (lo_mv, lo_pct)) = (w[0], w[1]);
        if mv >= lo_mv {
            return (lo_pct + (mv - lo_mv) * (hi_pct - lo_pct) / (hi_mv - lo_mv).max(1)) as u8;
        }
    }
    curve[curve.len() - 1].1 as u8
}

#[cfg(test)]
mod tests {
    use super::{from_voltage, map};

    #[test]
    fn map_scales_and_clamps() {
        assert_eq!(map(2, 0, 4), 50);
        assert_eq!(map(4, 0, 4), 100);
        assert_eq!(map(0x9A, 0x64, 0x9A), 100);
        assert_eq!(map(0x50, 0x64, 0x9A), 0); // below range
        assert_eq!(map(9, 0, 8), 100); // above range
    }

    #[test]
    fn voltage_curve_interpolates() {
        let curve = [(4200, 100), (3800, 50), (3600, 0)];
        assert_eq!(from_voltage(4300, &curve), 100);
        assert_eq!(from_voltage(4000, &curve), 75);
        assert_eq!(from_voltage(3700, &curve), 25);
        assert_eq!(from_voltage(3000, &curve), 0);
    }
}
