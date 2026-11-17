//! Raw HID access through hid.dll / setupapi, with hidapi's byte conventions for output and input reports so
//! protocol notes from hidapi-based projects (HeadsetControl, Solaar) apply with their offsets unchanged:
//! * writes: `data[0]` is the report ID (0 for unnumbered devices); the rest is zero-padded to the report length;
//! * reads: a leading report ID of 0 (unnumbered device) is stripped.

use std::ffi::c_void;
use std::fmt;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
use windows_sys::Win32::Devices::HumanInterfaceDevice::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::IO::*;
use windows_sys::Win32::System::Threading::*;

use crate::vlog;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

pub fn hex(bytes: &[u8], max: usize) -> String {
    bytes.iter().take(max.max(1)).map(|b| format!("{b:02X}")).collect()
}

/// One HID top-level collection (Windows exposes each collection as its own device path).
#[derive(Clone, Debug)]
pub struct HidInfo {
    pub path: String,
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub input_len: usize,
    pub output_len: usize,
    pub feature_len: usize,
    pub product: Option<String>,
}

impl HidInfo {
    /// Reached over Bluetooth Classic (HID profile UUID 0x1124).
    pub fn is_bluetooth(&self) -> bool {
        self.path.to_ascii_lowercase().contains("{00001124-0000-1000-8000-00805f9b34fb}")
    }

    pub fn is_vendor_defined(&self) -> bool {
        self.usage_page >= 0xFF00
    }

    /// USB interface number (the `&mi_XX` part of the path), or None for single-interface devices.
    pub fn interface(&self) -> Option<u8> {
        let p = self.path.to_ascii_lowercase();
        let i = p.find("&mi_")?;
        u8::from_str_radix(p.get(i + 4..i + 6)?, 16).ok()
    }

    /// Same value for every top-level collection of one USB interface. A path looks like
    /// `\\?\hid#vid_046d&pid_c539&mi_02&col01#8&2a3b&0&0000#{guid}`: the collection shows up both as `&colXX` and
    /// as the last `&XXXX` of the instance part, so both are dropped.
    pub fn interface_key(&self) -> String {
        let p = self.path.to_ascii_lowercase();
        let parts: Vec<&str> = p.split('#').collect();
        if parts.len() < 3 {
            return p;
        }
        let mut hwid = parts[1].to_string();
        if let Some(i) = hwid.find("&col") {
            hwid.replace_range(i..(i + 6).min(hwid.len()), "");
        }
        let instance = parts[2].rsplit_once('&').map_or(parts[2], |(head, _)| head);
        format!("{hwid}#{instance}")
    }
}

impl fmt::Display for HidInfo {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{:04X}:{:04X} page=0x{:04X} usage=0x{:02X} in={} out={} feat={} \"{}\"",
            self.vid,
            self.pid,
            self.usage_page,
            self.usage,
            self.input_len,
            self.output_len,
            self.feature_len,
            self.product.as_deref().unwrap_or("")
        )
    }
}

/// Owned Win32 handle.
pub struct Handle(pub HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

// Handles are plain kernel object references; moving them between threads is fine.
unsafe impl Send for Handle {}

pub fn open(path: &str, access: u32, overlapped: bool) -> Option<Handle> {
    let w = wide(path);
    let flags = if overlapped { FILE_FLAG_OVERLAPPED } else { 0 };
    let h = unsafe {
        CreateFileW(w.as_ptr(), access, FILE_SHARE_READ | FILE_SHARE_WRITE, null(), OPEN_EXISTING, flags, null_mut())
    };
    if h == INVALID_HANDLE_VALUE || h.is_null() { None } else { Some(Handle(h)) }
}

pub fn get_feature(h: &Handle, buf: &mut [u8]) -> bool {
    unsafe { HidD_GetFeature(h.0, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) }
}

pub fn set_feature(h: &Handle, buf: &[u8]) -> bool {
    // HidD_SetFeature takes a mutable pointer but doesn't write through it.
    unsafe { HidD_SetFeature(h.0, buf.as_ptr() as *mut c_void, buf.len() as u32) }
}

/// Lists HID collections, filtered by vendor/product before the expensive queries.
pub fn enumerate(filter: impl Fn(u16, u16) -> bool) -> Vec<HidInfo> {
    let mut out = Vec::new();
    for path in device_paths() {
        // Access 0 lets us query attributes/caps and use feature reports even on collections Windows locks
        // for exclusive use (mice, keyboards).
        let Some(h) = open(&path, 0, false) else { continue };
        let mut attrs = HIDD_ATTRIBUTES { Size: size_of::<HIDD_ATTRIBUTES>() as u32, ..unsafe { std::mem::zeroed() } };
        if !unsafe { HidD_GetAttributes(h.0, &mut attrs) } || !filter(attrs.VendorID, attrs.ProductID) {
            continue;
        }
        let mut pre: PHIDP_PREPARSED_DATA = 0;
        if !unsafe { HidD_GetPreparsedData(h.0, &mut pre) } {
            continue;
        }
        let mut caps: HIDP_CAPS = unsafe { std::mem::zeroed() };
        let status = unsafe { HidP_GetCaps(pre, &mut caps) };
        unsafe { HidD_FreePreparsedData(pre) };
        if status != HIDP_STATUS_SUCCESS {
            continue;
        }
        let mut name = [0u16; 128];
        let product = unsafe { HidD_GetProductString(h.0, name.as_mut_ptr() as *mut c_void, 256) }
            .then(|| from_wide(&name).trim().to_string())
            .filter(|s| !s.is_empty());
        out.push(HidInfo {
            path,
            vid: attrs.VendorID,
            pid: attrs.ProductID,
            usage_page: caps.UsagePage,
            usage: caps.Usage,
            input_len: caps.InputReportByteLength as usize,
            output_len: caps.OutputReportByteLength as usize,
            feature_len: caps.FeatureReportByteLength as usize,
            product,
        });
    }
    out
}

fn device_paths() -> Vec<String> {
    let mut paths = Vec::new();
    unsafe {
        let mut guid = std::mem::zeroed();
        HidD_GetHidGuid(&mut guid);
        let set = SetupDiGetClassDevsW(&guid, null(), null_mut(), DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
        if set == INVALID_HANDLE_VALUE as HDEVINFO {
            return paths;
        }
        let mut data =
            SP_DEVICE_INTERFACE_DATA { cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32, ..std::mem::zeroed() };
        let mut i = 0;
        while SetupDiEnumDeviceInterfaces(set, null(), &guid, i, &mut data) != 0 {
            i += 1;
            let mut size = 0u32;
            SetupDiGetDeviceInterfaceDetailW(set, &data, null_mut(), 0, &mut size, null_mut());
            if size == 0 {
                continue;
            }
            // u32 storage keeps the struct aligned; DevicePath follows the 4-byte cbSize.
            let mut buf = vec![0u32; (size as usize).div_ceil(4) + 1];
            let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &data, detail, size, null_mut(), null_mut()) != 0 {
                let chars =
                    std::slice::from_raw_parts((detail as *const u8).add(4) as *const u16, (size as usize - 4) / 2);
                paths.push(from_wide(chars));
            }
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    paths
}

/// One outstanding overlapped read on a handle. The buffer and OVERLAPPED are boxed so their addresses stay
/// fixed while the kernel owns them.
struct Reader {
    handle: Handle,
    event: Handle,
    ov: Box<OVERLAPPED>,
    buf: Box<[u8]>,
    pending: bool,
    dead: bool,
}

impl Reader {
    fn new(handle: Handle, len: usize) -> Option<Reader> {
        let event = unsafe { CreateEventW(null(), 1, 0, null()) };
        if event.is_null() {
            return None;
        }
        Some(Reader {
            handle,
            event: Handle(event),
            ov: Box::new(unsafe { std::mem::zeroed() }),
            buf: vec![0u8; len.max(2)].into_boxed_slice(),
            pending: false,
            dead: false,
        })
    }

    /// Starts a read if none is outstanding. Returns a report if it completed immediately.
    fn start(&mut self) -> Option<Vec<u8>> {
        if self.pending || self.dead {
            return None;
        }
        *self.ov = unsafe { std::mem::zeroed() };
        self.ov.hEvent = self.event.0;
        let ok =
            unsafe { ReadFile(self.handle.0, self.buf.as_mut_ptr(), self.buf.len() as u32, null_mut(), &mut *self.ov) };
        if ok != 0 {
            return self.finish();
        }
        if unsafe { GetLastError() } == ERROR_IO_PENDING {
            self.pending = true;
        } else {
            self.dead = true; // device unplugged
        }
        None
    }

    fn finish(&mut self) -> Option<Vec<u8>> {
        self.pending = false;
        let mut n = 0u32;
        if unsafe { GetOverlappedResult(self.handle.0, &*self.ov, &mut n, 0) } == 0 || n == 0 {
            self.dead = true;
            return None;
        }
        let r = &self.buf[..n as usize];
        // Never hand out an empty report: callers may index [0].
        Some(if r[0] == 0 && r.len() > 1 { r[1..].to_vec() } else { r.to_vec() })
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        if self.pending {
            let mut n = 0u32;
            unsafe {
                CancelIoEx(self.handle.0, &*self.ov);
                GetOverlappedResult(self.handle.0, &*self.ov, &mut n, 1); // wait until the kernel lets go of buf
            }
        }
    }
}

/// Request/response access to a collection via output and input reports. Input reports may also be collected
/// from further collections of the same device (Logitech sends short and long HID++ replies on separate ones).
pub struct Channel {
    pub info: HidInfo,
    writer: Handle,
    write_event: Handle,
    readers: Vec<Reader>,
}

impl Channel {
    pub fn open(write_to: &HidInfo, also_read_from: &[&HidInfo]) -> Option<Channel> {
        if write_to.output_len == 0 {
            return None;
        }
        let writer = open(&write_to.path, GENERIC_READ | GENERIC_WRITE, true)?;
        let event = unsafe { CreateEventW(null(), 1, 0, null()) };
        if event.is_null() {
            return None;
        }
        let mut readers = Vec::new();
        if write_to.input_len > 0 {
            readers.push(Reader::new(open(&write_to.path, GENERIC_READ | GENERIC_WRITE, true)?, write_to.input_len)?);
        }
        for extra in also_read_from {
            if extra.path == write_to.path || extra.input_len == 0 {
                continue;
            }
            if let Some(r) = open(&extra.path, GENERIC_READ, true).and_then(|h| Reader::new(h, extra.input_len)) {
                readers.push(r);
            }
        }
        Some(Channel { info: write_to.clone(), writer, write_event: Handle(event), readers })
    }

    fn tag(&self) -> String {
        format!("{:04X}:{:04X} {:04X}/{:02X}", self.info.vid, self.info.pid, self.info.usage_page, self.info.usage)
    }

    /// Writes an output report in hidapi form (`data[0]` = report ID), padded to the report length.
    pub fn write(&mut self, data: &[u8]) -> bool {
        let mut buf = vec![0u8; self.info.output_len];
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = self.write_event.0;
        let mut ok = unsafe { WriteFile(self.writer.0, buf.as_ptr(), buf.len() as u32, null_mut(), &mut ov) } != 0;
        if !ok && unsafe { GetLastError() } == ERROR_IO_PENDING {
            let mut written = 0u32;
            if unsafe { WaitForSingleObject(self.write_event.0, 1000) } == WAIT_OBJECT_0 {
                ok = unsafe { GetOverlappedResult(self.writer.0, &ov, &mut written, 0) } != 0;
            } else {
                unsafe {
                    CancelIoEx(self.writer.0, &ov);
                    GetOverlappedResult(self.writer.0, &ov, &mut written, 1);
                }
            }
        }
        vlog!("{} → {}{}", self.tag(), hex(&buf, data.len()), if ok { "" } else { " FAILED" });
        ok
    }

    /// Next input report from any of the collections, or None after `timeout_ms`.
    pub fn read(&mut self, timeout_ms: u32) -> Option<Vec<u8>> {
        for r in &mut self.readers {
            if let Some(report) = r.start() {
                return Some(report);
            }
        }
        let live: Vec<usize> = (0..self.readers.len()).filter(|&i| self.readers[i].pending).collect();
        if live.is_empty() {
            return None;
        }
        let events: Vec<HANDLE> = live.iter().map(|&i| self.readers[i].event.0).collect();
        let w = unsafe { WaitForMultipleObjects(events.len() as u32, events.as_ptr(), 0, timeout_ms) };
        let i = w.wrapping_sub(WAIT_OBJECT_0) as usize;
        if i < live.len() { self.readers[live[i]].finish() } else { None }
    }

    /// Next input report accepted by `matches`, logging everything seen.
    pub fn read_matching(&mut self, timeout_ms: u32, matches: impl Fn(&[u8]) -> bool) -> Option<Vec<u8>> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms as u64);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now()).as_millis() as u32;
            if left == 0 {
                vlog!("{} ← (no reply in {timeout_ms} ms)", self.tag());
                return None;
            }
            let r = self.read(left)?;
            let ok = matches(&r);
            vlog!("{} ← {}{}", self.tag(), hex(&r, 24), if ok { "" } else { "  (ignored)" });
            if ok {
                return Some(r);
            }
        }
    }

    /// Drops reports that arrived before now (stale replies, unsolicited status reports).
    pub fn discard(&mut self) {
        while self.read(0).is_some() {}
    }

    /// Discard, write, then wait for a reply accepted by `matches`.
    pub fn request(&mut self, data: &[u8], timeout_ms: u32, matches: impl Fn(&[u8]) -> bool) -> Option<Vec<u8>> {
        self.discard();
        if !self.write(data) {
            return None;
        }
        self.read_matching(timeout_ms, matches)
    }
}

#[cfg(test)]
mod tests {
    use super::HidInfo;

    fn info(path: &str) -> HidInfo {
        HidInfo {
            path: path.into(),
            vid: 0,
            pid: 0,
            usage_page: 0,
            usage: 0,
            input_len: 0,
            output_len: 0,
            feature_len: 0,
            product: None,
        }
    }

    const GUID: &str = "{4d1e55b2-f16f-11cf-88cb-001111000030}";

    #[test]
    fn interface_number_from_path() {
        assert_eq!(
            info(&format!(r"\\?\hid#vid_1532&pid_00c1&mi_01&col04#9&319f0407&0&0003#{GUID}")).interface(),
            Some(1)
        );
        assert_eq!(info(&format!(r"\\?\hid#vid_046d&pid_c539&mi_02#8&2a3b&0&0000#{GUID}")).interface(), Some(2));
        assert_eq!(info(&format!(r"\\?\hid#vid_054c&pid_0ce6#7&abc&0&0000#{GUID}")).interface(), None);
    }

    #[test]
    fn collections_of_one_interface_share_a_key() {
        let col4 = info(&format!(r"\\?\hid#vid_1532&pid_00c1&mi_01&col04#9&319f0407&0&0003#{GUID}"));
        let col5 = info(&format!(r"\\?\HID#VID_1532&PID_00C1&MI_01&COL05#9&319F0407&0&0004#{GUID}"));
        let other = info(&format!(r"\\?\hid#vid_1532&pid_00c1&mi_02#9&33542a29&0&0000#{GUID}"));
        assert_eq!(col4.interface_key(), col5.interface_key());
        assert_ne!(col4.interface_key(), other.interface_key());
    }

    #[test]
    fn bluetooth_detection() {
        let bt =
            format!(r"\\?\hid#{{00001124-0000-1000-8000-00805f9b34fb}}_vid&0002054c_pid&0ce6#a&1b2c&0&0000#{GUID}");
        assert!(info(&bt).is_bluetooth());
        assert!(!info(&format!(r"\\?\hid#vid_054c&pid_0ce6#7&abc&0&0000#{GUID}")).is_bluetooth());
    }
}
