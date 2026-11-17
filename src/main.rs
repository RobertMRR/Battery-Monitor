//! Battery Monitor: shows the battery level of wireless devices as tray icons on the Windows taskbar.
//!
//! Diagnostic modes (run from a terminal; pipe to a file in PowerShell so it waits for the output):
//!   --probe [--verbose]   what each reader sees (with raw protocol traffic)
//!   --list-hid            every HID collection: VID:PID, usage, report sizes, product name
//!   --preview <file.bmp>  render sample icons (dev aid)
//!   --preview-widget <file.bmp>  render the taskbar widget in every style (dev aid)

#![windows_subsystem = "windows"]

mod devices;
mod diag;
mod glyphs;
mod hid;
mod icon;
mod settings;
mod startup;
mod tray;
mod widget;

use std::ptr::null;

use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows_sys::Win32::System::Threading::CreateMutexW;

/// Release builds set BM_VERSION (= the Cargo.toml version, checked against the git tag); local builds are "-dev".
/// DPI awareness, Windows compatibility and the icon come from the resources embedded by build.rs.
pub const VERSION: &str = match option_env!("BM_VERSION") {
    Some(v) => v,
    None => concat!(env!("CARGO_PKG_VERSION"), "-dev"),
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 2 && args[0] == "--preview" {
        std::process::exit(if icon::write_preview(&args[1]).is_ok() { 0 } else { 1 });
    }
    if args.len() == 2 && args[0] == "--preview-widget" {
        std::process::exit(if icon::write_widget_preview(&args[1]).is_ok() { 0 } else { 1 });
    }
    if args.iter().any(|a| a == "--probe" || a == "--list-hid") {
        // A GUI-subsystem exe has no console. Without redirection, borrow the terminal's; with `| Out-File`
        // the inherited pipe is already our stdout.
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
        diag::VERBOSE.store(args.iter().any(|a| a == "--verbose"), std::sync::atomic::Ordering::Relaxed);
        if args.iter().any(|a| a == "--list-hid") {
            let mut all = hid::enumerate(|_, _| true);
            all.sort_by_key(|d| (d.vid, d.pid));
            for d in all {
                println!("{d}");
            }
        } else {
            probe();
        }
        return;
    }

    let name = hid::wide(r"Local\BatteryMonitor.SingleInstance");
    let _mutex = unsafe { CreateMutexW(null(), 1, name.as_ptr()) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return;
    }
    tray::run(devices::all());
}

fn probe() {
    println!("Battery Monitor {VERSION}");
    for mut p in devices::all() {
        let found = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.poll()));
        match found {
            Ok(found) => {
                println!("[{}] {} device(s)", p.name(), found.len());
                for b in found {
                    println!(
                        "  {:<32} {:>3}%{}{}  [{:?}] {}",
                        b.name,
                        b.percent,
                        if b.charging { " charging" } else { "" },
                        if b.stale { " (stale)" } else { "" },
                        b.kind,
                        b.id
                    );
                }
            }
            Err(_) => println!("[{}] ERROR: reader crashed", p.name()),
        }
    }
}
