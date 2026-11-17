//! Battery readers. Each `Provider` covers one protocol and returns every device it can currently see.

pub mod blackshark;
pub mod bluetooth;
pub mod dualsense;
pub mod headsets;
pub mod logitech;
pub mod maxxter;
pub mod razer;
pub mod xinput;

use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Gamepad,
    Mouse,
    Keyboard,
    Headset,
    Other,
}

#[derive(Clone, Debug)]
pub struct Battery {
    /// Stable key for the device; one tray icon per id.
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub percent: u8,
    pub charging: bool,
    /// Known device that isn't answering right now (e.g. a mouse asleep); `percent` is the last known value.
    pub stale: bool,
}

pub trait Provider: Send {
    fn name(&self) -> &str;
    /// Every device this provider can currently see. Absent devices are simply not returned.
    fn poll(&mut self) -> Vec<Battery>;
}

pub fn all() -> Vec<Box<dyn Provider>> {
    vec![
        Box::new(xinput::XInput),
        Box::new(dualsense::DualSense),
        Box::new(razer::Razer::default()),
        Box::new(maxxter::Maxxter::default()),
        Box::new(blackshark::BlackShark::default()),
        Box::new(logitech::Logitech::default()),
        Box::new(headsets::HeadsetProvider::new("Logitech LIGHTSPEED headsets", headsets::centurion::specs())),
        Box::new(headsets::HeadsetProvider::new("SteelSeries", headsets::steelseries::specs())),
        Box::new(headsets::HeadsetProvider::new("HyperX", headsets::hyperx::specs())),
        Box::new(headsets::HeadsetProvider::new("Corsair", headsets::corsair::specs())),
        Box::new(headsets::HeadsetProvider::new("Ajazz", headsets::ajazz::specs())),
        Box::new(bluetooth::Bluetooth),
    ]
}

/// Cleans up one poll's combined results from all providers.
/// - The same id twice (e.g. via Bluetooth and a dedicated reader): keep the first.
/// - A device charging on its cable often shows up twice: answering under its cable id, and faded ("asleep", last
///   known level) under its dongle id. Keep only the live one.
pub fn merge(all: &mut Vec<Battery>) {
    let mut seen = HashSet::new();
    all.retain(|b| seen.insert(b.id.clone()));
    let live: HashSet<(Kind, String)> = all.iter().filter(|b| !b.stale).map(|b| (b.kind, b.name.clone())).collect();
    all.retain(|b| !b.stale || !live.contains(&(b.kind, b.name.clone())));
}

/// Best guess from a product name, for readers that cover many kinds of device.
pub fn guess_kind(name: &str, fallback: Kind) -> Kind {
    let n = name.to_lowercase();
    let any = |words: &[&str]| words.iter().any(|w| n.contains(w));
    if any(&[
        "headset",
        "headphone",
        "earbud",
        "buds",
        "virtuoso",
        "void",
        "hs80",
        "hs70",
        "kraken",
        "barracuda",
        "blackshark",
        "arctis",
        "wh-",
        "wf-",
        "pods",
    ]) {
        Kind::Headset
    } else if any(&["keyboard", "k57", "k63", "k70", "k100", "blackwidow", "huntsman", "deathstalker", "keys"]) {
        Kind::Keyboard
    } else if any(&["mouse", "dark core", "harpoon", "m55", "m65", "m75", "ironclaw", "sabre", "mx "]) {
        Kind::Mouse
    } else if any(&["controller", "gamepad"]) {
        Kind::Gamepad
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::{Battery, Kind, guess_kind, merge};

    fn battery(id: &str, stale: bool) -> Battery {
        Battery {
            id: id.into(),
            name: "Razer Viper V3 Pro".into(),
            kind: Kind::Mouse,
            percent: 71,
            charging: !stale,
            stale,
        }
    }

    #[test]
    fn charging_on_the_cable_hides_the_sleeping_dongle_entry() {
        let mut all = vec![battery("razer:00c1", true), battery("razer:00c0", false), battery("razer:00c0", false)];
        merge(&mut all);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, "razer:00c0");
        // A sleeping mouse with no live twin stays (faded).
        let mut asleep = vec![battery("razer:00c1", true)];
        merge(&mut asleep);
        assert_eq!(asleep.len(), 1);
    }

    #[test]
    fn kind_from_product_name() {
        assert_eq!(guess_kind("Corsair VIRTUOSO RGB Wireless", Kind::Other), Kind::Headset);
        assert_eq!(guess_kind("Razer BlackWidow V4 Pro", Kind::Other), Kind::Keyboard);
        assert_eq!(guess_kind("Xbox Wireless Controller", Kind::Other), Kind::Gamepad);
        assert_eq!(guess_kind("MX Master 3S", Kind::Other), Kind::Mouse);
        assert_eq!(guess_kind("Razer Viper V3 Pro", Kind::Mouse), Kind::Mouse); // no hint: fallback
    }
}
