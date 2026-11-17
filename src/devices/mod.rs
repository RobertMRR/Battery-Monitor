//! Battery readers. Each `Provider` covers one protocol and returns every device it can currently see.

pub mod bluetooth;
pub mod dualsense;
pub mod headsets;
pub mod logitech;
pub mod razer;
pub mod xinput;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
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
        Box::new(logitech::Logitech::default()),
        Box::new(headsets::HeadsetProvider::new("Logitech LIGHTSPEED headsets", headsets::centurion::specs())),
        Box::new(headsets::HeadsetProvider::new("SteelSeries", headsets::steelseries::specs())),
        Box::new(headsets::HeadsetProvider::new("HyperX", headsets::hyperx::specs())),
        Box::new(headsets::HeadsetProvider::new("Corsair", headsets::corsair::specs())),
        Box::new(headsets::HeadsetProvider::new("Ajazz", headsets::ajazz::specs())),
        Box::new(bluetooth::Bluetooth),
    ]
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
    use super::{Kind, guess_kind};

    #[test]
    fn kind_from_product_name() {
        assert_eq!(guess_kind("Corsair VIRTUOSO RGB Wireless", Kind::Other), Kind::Headset);
        assert_eq!(guess_kind("Razer BlackWidow V4 Pro", Kind::Other), Kind::Keyboard);
        assert_eq!(guess_kind("Xbox Wireless Controller", Kind::Other), Kind::Gamepad);
        assert_eq!(guess_kind("MX Master 3S", Kind::Other), Kind::Mouse);
        assert_eq!(guess_kind("Razer Viper V3 Pro", Kind::Mouse), Kind::Mouse); // no hint: fallback
    }
}
