//! "Start with Windows" via the per-user Run key (no admin rights needed).

use std::ptr::null_mut;

use windows_sys::Win32::System::Registry::*;

use crate::hid::{from_wide, wide};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "BatteryMonitor";

fn command() -> String {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    format!("\"{exe}\"")
}

pub fn is_enabled() -> bool {
    let (key, name) = (wide(RUN_KEY), wide(VALUE));
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
    r == 0 && from_wide(&buf).eq_ignore_ascii_case(&command())
}

pub fn set_enabled(enabled: bool) {
    let (key, name) = (wide(RUN_KEY), wide(VALUE));
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
        ) != 0
        {
            return;
        }
        if enabled {
            let cmd = wide(&command());
            RegSetValueExW(hkey, name.as_ptr(), 0, REG_SZ, cmd.as_ptr() as *const u8, (cmd.len() * 2) as u32);
        } else {
            RegDeleteValueW(hkey, name.as_ptr());
        }
        RegCloseKey(hkey);
    }
}
