//! User choices from the tray menu, kept in `HKCU\Software\BatteryMonitor`.

use std::ptr::null_mut;

use windows_sys::Win32::System::Registry::*;

use crate::hid::{from_wide, wide};
use crate::icon::Style;

const KEY: &str = r"Software\BatteryMonitor";
const SEPARATE: &str = "OneIconPerDevice";
const ICON_DEVICE: &str = "IconDevice";
const ICON_STYLE: &str = "IconStyle";
const TASKBAR_WIDGET: &str = "TaskbarWidget";

/// One tray icon per device instead of a single icon (default: single).
pub fn separate_icons() -> bool {
    read_flag(SEPARATE)
}

pub fn set_separate_icons(on: bool) {
    write(SEPARATE, REG_DWORD, &(on as u32).to_le_bytes());
}

/// Show on the taskbar itself (widget) instead of in the notification area (default: notification area).
pub fn taskbar_widget() -> bool {
    read_flag(TASKBAR_WIDGET)
}

pub fn set_taskbar_widget(on: bool) {
    write(TASKBAR_WIDGET, REG_DWORD, &(on as u32).to_le_bytes());
}

/// How icons are drawn (default: device symbol + level bar).
pub fn icon_style() -> Style {
    match read_dword(ICON_STYLE) {
        Some(1) => Style::SymbolFill,
        Some(2) => Style::SymbolDigits,
        Some(3) => Style::Digits,
        Some(4) => Style::Ring,
        _ => Style::SymbolBar,
    }
}

pub fn set_icon_style(style: Style) {
    let v: u32 = match style {
        Style::SymbolBar => 0,
        Style::SymbolFill => 1,
        Style::SymbolDigits => 2,
        Style::Digits => 3,
        Style::Ring => 4,
    };
    write(ICON_STYLE, REG_DWORD, &v.to_le_bytes());
}

fn read_flag(name: &str) -> bool {
    read_dword(name).is_some_and(|v| v != 0)
}

fn read_dword(name: &str) -> Option<u32> {
    let (key, name) = (wide(KEY), wide(name));
    let mut value = 0u32;
    let mut len = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            &mut value as *mut u32 as *mut _,
            &mut len,
        )
    };
    (r == 0).then_some(value)
}

/// Device id pinned to the single icon; None = show the lowest battery automatically.
pub fn icon_device() -> Option<String> {
    let (key, name) = (wide(KEY), wide(ICON_DEVICE));
    let mut buf = [0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buf.as_mut_ptr() as *mut _,
            &mut len,
        )
    };
    let s = from_wide(&buf);
    (r == 0 && !s.is_empty()).then_some(s)
}

pub fn set_icon_device(id: Option<&str>) {
    let w = wide(id.unwrap_or(""));
    let bytes: Vec<u8> = w.iter().flat_map(|c| c.to_le_bytes()).collect();
    write(ICON_DEVICE, REG_SZ, &bytes);
}

fn write(name: &str, kind: REG_VALUE_TYPE, data: &[u8]) {
    let (key, name) = (wide(KEY), wide(name));
    unsafe {
        let mut hkey = null_mut();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            0,
            null_mut(),
            0,
            KEY_SET_VALUE,
            null_mut(),
            &mut hkey,
            null_mut(),
        ) == 0
        {
            RegSetValueExW(hkey, name.as_ptr(), 0, kind, data.as_ptr(), data.len() as u32);
            RegCloseKey(hkey);
        }
    }
}
