//! Xbox (and other XInput) controllers. XInput only reports four coarse levels; Windows.Gaming.Input reports the
//! same steps as percentages; the mapping below matches its values (verified: 70% on both).

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::UI::Input::XboxController::*;

use super::{Battery, Kind, Provider};
use crate::hid;

pub struct XInput;

impl Provider for XInput {
    fn name(&self) -> &str {
        "XInput"
    }

    fn poll(&mut self) -> Vec<Battery> {
        let mut pads = Vec::new();
        for slot in 0..4u32 {
            let mut info = XINPUT_BATTERY_INFORMATION { BatteryType: 0, BatteryLevel: 0 };
            if unsafe { XInputGetBatteryInformation(slot, BATTERY_DEVTYPE_GAMEPAD, &mut info) } != ERROR_SUCCESS {
                continue;
            }
            crate::vlog!("xinput slot {slot}: type={} level={}", info.BatteryType, info.BatteryLevel);
            // Disconnected slots and wired pads (incl. Steam's virtual pads) have no battery to show.
            if info.BatteryType == BATTERY_TYPE_DISCONNECTED || info.BatteryType == BATTERY_TYPE_WIRED {
                continue;
            }
            let percent = match info.BatteryLevel {
                BATTERY_LEVEL_EMPTY => 10,
                BATTERY_LEVEL_LOW => 40,
                BATTERY_LEVEL_MEDIUM => 70,
                _ => 100,
            };
            pads.push((slot, percent));
        }
        if pads.is_empty() {
            return Vec::new();
        }

        // XInput has no names. With exactly one Xbox pad, use its HID product string ("Xbox Wireless Controller").
        let names: Vec<String> = hid::enumerate(|v, _| v == 0x045E)
            .into_iter()
            .filter(|d| d.usage_page == 0x01 && d.usage == 0x05)
            .filter_map(|d| d.product)
            .collect();
        pads.iter()
            .map(|&(slot, percent)| Battery {
                id: format!("xinput:{slot}"),
                name: if pads.len() == 1 && names.len() == 1 {
                    names[0].clone()
                } else {
                    format!("Xbox Controller {}", slot + 1)
                },
                kind: Kind::Gamepad,
                percent,
                charging: false,
                stale: false,
            })
            .collect()
    }
}
