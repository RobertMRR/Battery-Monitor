//! Any Bluetooth device whose battery Windows already knows (the number shown in Settings > Bluetooth): most BLE
//! mice/keyboards and many headphones/earbuds that implement the Battery Service or HFP battery reporting.

use std::ptr::null_mut;

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
use windows_sys::Win32::Devices::Properties::DEVPROPTYPE;
use windows_sys::Win32::Foundation::{DEVPROPKEY, INVALID_HANDLE_VALUE};
use windows_sys::core::GUID;

use super::{Battery, Kind, Provider, guess_kind};
use crate::hid::from_wide;

/// DEVPKEY_Bluetooth_Battery: percentage as a single byte.
const BATTERY_KEY: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x104EA319_6EE2_4701_BD47_8DDBF425BBE5), pid: 2 };
const DN_DEVICE_DISCONNECTED: u32 = 0x0200_0000;

pub struct Bluetooth;

impl Provider for Bluetooth {
    fn name(&self) -> &str {
        "Bluetooth"
    }

    fn poll(&mut self) -> Vec<Battery> {
        let mut out = Vec::new();
        unsafe {
            let set = SetupDiGetClassDevsW(&GUID_DEVCLASS_BLUETOOTH, std::ptr::null(), null_mut(), DIGCF_PRESENT);
            if set == INVALID_HANDLE_VALUE as HDEVINFO {
                return out;
            }
            let mut dev = SP_DEVINFO_DATA { cbSize: size_of::<SP_DEVINFO_DATA>() as u32, ..std::mem::zeroed() };
            let mut i = 0;
            while SetupDiEnumDeviceInfo(set, i, &mut dev) != 0 {
                i += 1;
                let mut ty: DEVPROPTYPE = 0;
                let mut percent = 0u8;
                if SetupDiGetDevicePropertyW(set, &dev, &BATTERY_KEY, &mut ty, &mut percent, 1, null_mut(), 0) == 0 {
                    continue;
                }
                // Paired devices stay "present" while switched off; the devnode is flagged disconnected then.
                let (mut status, mut problem) = (0u32, 0u32);
                if CM_Get_DevNode_Status(&mut status, &mut problem, dev.DevInst, 0) == CR_SUCCESS
                    && status & DN_DEVICE_DISCONNECTED != 0
                {
                    continue;
                }
                let mut name = [0u16; 256];
                let got_name = SetupDiGetDeviceRegistryPropertyW(
                    set,
                    &dev,
                    SPDRP_FRIENDLYNAME,
                    null_mut(),
                    name.as_mut_ptr() as *mut u8,
                    512,
                    null_mut(),
                ) != 0;
                let name = if got_name { from_wide(&name) } else { "Bluetooth device".into() };
                let mut instance = [0u16; 512];
                SetupDiGetDeviceInstanceIdW(set, &dev, instance.as_mut_ptr(), 512, null_mut());
                out.push(Battery {
                    id: format!("bt:{}", from_wide(&instance).to_lowercase()),
                    kind: guess_kind(&name, Kind::Other),
                    name,
                    percent: percent.min(100),
                    charging: false,
                    stale: false,
                });
            }
            SetupDiDestroyDeviceInfoList(set);
        }
        out
    }
}
