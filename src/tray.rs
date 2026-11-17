//! The tray UI: by default a single notification-area icon showing the lowest battery (or a device the user pinned),
//! optionally one icon per device. Refreshed every minute and shortly after any device change.
//!
//! A hidden top-level window receives tray callbacks, timers, WM_DEVICECHANGE broadcasts, theme changes and
//! Explorer restarts. Polling runs on a worker thread, which posts the results back to the window.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::devices::{Battery, Provider};
use crate::hid::wide;
use crate::{icon, settings, startup, widget};

const WM_TRAY: u32 = WM_APP + 1;
const WM_RESULTS: u32 = WM_APP + 2;
const TIMER_POLL: usize = 1;
const TIMER_SOON: usize = 2;
const TIMER_WIDGET: usize = 3; // keeps the taskbar widget in place
const TIMER_BALLOON: usize = 4; // removes the temporary icon a low-battery warning needed in widget mode
const POLL_MS: u32 = 60_000;
const SOON_MS: u32 = 2_500;
const NOTIFY_BELOW: u8 = 15;
const REARM_ABOVE: u8 = 25;

const CMD_REFRESH: usize = 1;
const CMD_STARTUP: usize = 2;
const CMD_EXIT: usize = 3;
const CMD_SEPARATE: usize = 4;
const CMD_AUTO: usize = 5;
const CMD_WIDGET: usize = 6;
const CMD_STYLE_BASE: usize = 50; // + index into icon::Style::ALL
const CMD_PIN_BASE: usize = 100; // + index into the device list

/// Keyboard activation of a tray icon (NIN_SELECT | NINF_KEY); not exported by windows-sys.
const NIN_KEYSELECT: u32 = NIN_SELECT | 0x1;

/// The single icon (or, in one-icon-per-device mode, the "no devices" placeholder). Keeping one id means one stable
/// tray slot, so Windows remembers whether the user pinned it to the taskbar.
const MAIN_ID: &str = "\0main";

struct TrayIcon {
    uid: u32,
    hicon: HICON,
}

struct App {
    hwnd: HWND,
    providers: Arc<Mutex<Vec<Box<dyn Provider>>>>,
    polling: Arc<AtomicBool>,
    poll_again: bool,
    icons: HashMap<String, TrayIcon>,
    next_uid: u32,
    latest: Vec<Battery>,
    low_warned: HashSet<String>,
    taskbar_created: u32,
    /// One icon per device instead of a single icon.
    separate: bool,
    /// Device pinned to the single icon; None = lowest battery.
    pinned: Option<String>,
    /// How icons are drawn.
    style: icon::Style,
    /// Show on the taskbar itself (widget) instead of in the notification area.
    use_widget: bool,
    widget: Option<widget::Widget>,
    /// A temporary tray icon is up for a low-battery warning (widget mode has no icon of its own).
    balloon_icon: bool,
    /// No poll has finished yet.
    searching: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` on the app state. Returns None if there's no state yet, or if it's already in use further up the stack
/// (some Win32 calls send messages back to the window synchronously); such re-entrant messages are skipped.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok()?.as_mut().map(f))
}

pub fn run(providers: Vec<Box<dyn Provider>>) {
    unsafe {
        let instance = GetModuleHandleW(null());
        let class = wide("BatteryMonitorWindow");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        // Top-level (not message-only) so it receives broadcast WM_DEVICECHANGE; never shown.
        let title = wide("Battery Monitor");
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return;
        }

        let taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        APP.with(|a| {
            *a.borrow_mut() = Some(App {
                hwnd,
                providers: Arc::new(Mutex::new(providers)),
                polling: Arc::new(AtomicBool::new(false)),
                poll_again: false,
                icons: HashMap::new(),
                next_uid: 1,
                latest: Vec::new(),
                low_warned: HashSet::new(),
                taskbar_created,
                separate: settings::separate_icons(),
                pinned: settings::icon_device(),
                style: settings::icon_style(),
                use_widget: settings::taskbar_widget(),
                widget: None,
                balloon_icon: false,
                searching: true,
            })
        });
        with_app(|app| app.apply(Vec::new()));
        SetTimer(hwnd, TIMER_POLL, POLL_MS, None);
        SetTimer(hwnd, TIMER_WIDGET, 1000, None);
        SetTimer(hwnd, TIMER_SOON, 300, None);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            match wparam {
                TIMER_WIDGET => {
                    with_app(App::tick_widget);
                }
                TIMER_BALLOON => {
                    unsafe { KillTimer(hwnd, TIMER_BALLOON) };
                    with_app(|app| {
                        app.balloon_icon = false;
                        if app.widget.is_some() {
                            app.remove_icon(MAIN_ID);
                        }
                    });
                }
                _ => {
                    if wparam == TIMER_SOON {
                        unsafe { KillTimer(hwnd, TIMER_SOON) };
                    }
                    with_app(App::start_poll);
                }
            }
            0
        }
        WM_RESULTS => {
            // SAFETY: the worker posted a Box<Vec<Battery>> it gave up ownership of.
            let results = *unsafe { Box::from_raw(lparam as *mut Vec<Battery>) };
            with_app(|app| {
                app.polling.store(false, Ordering::SeqCst);
                app.searching = false;
                app.apply(results);
                if std::mem::take(&mut app.poll_again) {
                    app.start_poll();
                }
            });
            0
        }
        WM_TRAY => {
            // NOTIFYICON_VERSION_4: event in LOWORD(lParam); left click = NIN_SELECT, right click = WM_CONTEXTMENU.
            let event = (lparam & 0xFFFF) as u32;
            if event == WM_CONTEXTMENU || event == NIN_SELECT || event == NIN_KEYSELECT {
                show_menu(hwnd);
            }
            0
        }
        WM_DEVICECHANGE => {
            if wparam == DBT_DEVNODES_CHANGED as usize {
                // Devices plugging in, pairing or waking up: refresh a moment later (debounced).
                unsafe { SetTimer(hwnd, TIMER_SOON, SOON_MS, None) };
            }
            1
        }
        WM_SETTINGCHANGE => {
            // Light/dark taskbar switch ("ImmersiveColorSet"): redraw with the right digit color.
            with_app(|app| app.apply(app.latest.clone()));
            0
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => {
            // Explorer restarted: the taskbar forgot our icons, so add them again.
            if with_app(|app| msg == app.taskbar_created) == Some(true) {
                with_app(|app| {
                    for (_, i) in app.icons.drain() {
                        unsafe { DestroyIcon(i.hicon) };
                    }
                    app.widget = None; // it was a child of the old taskbar
                    app.apply(app.latest.clone());
                });
                return 0;
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

impl App {
    fn start_poll(&mut self) {
        if self.polling.swap(true, Ordering::SeqCst) {
            self.poll_again = true;
            return;
        }
        let providers = self.providers.clone();
        let hwnd = self.hwnd as isize; // HWND isn't Send; the value is just an id
        std::thread::spawn(move || {
            let mut all: Vec<Battery> = Vec::new();
            {
                let mut providers = providers.lock().unwrap_or_else(|e| e.into_inner());
                for p in providers.iter_mut() {
                    // One broken reader must not hide the others (or kill polling).
                    if let Ok(found) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.poll())) {
                        all.extend(found);
                    }
                }
            }
            crate::devices::merge(&mut all);
            all.sort_by_key(|b| (b.kind, b.name.to_lowercase()));

            let ptr = Box::into_raw(Box::new(all));
            if unsafe { PostMessageW(hwnd as HWND, WM_RESULTS, 0, ptr as LPARAM) } == 0 {
                drop(unsafe { Box::from_raw(ptr) }); // window gone (exiting)
            }
        });
    }

    fn apply(&mut self, readings: Vec<Battery>) {
        self.low_warned.retain(|id| readings.iter().any(|b| &b.id == id));
        if self.use_widget && self.ensure_widget() {
            // Widget mode: no tray icons (except a temporary one while a low-battery warning is up).
            let ids: Vec<String> =
                self.icons.keys().filter(|id| !(self.balloon_icon && *id == MAIN_ID)).cloned().collect();
            for id in ids {
                self.remove_icon(&id);
            }
            self.show_widget(&readings);
        } else {
            self.widget = None;
            self.apply_tray(&readings);
        }
        for b in &readings {
            self.maybe_warn_low(b);
        }
        self.latest = readings;
    }

    fn ensure_widget(&mut self) -> bool {
        if self.widget.is_none() {
            self.widget = widget::create(self.hwnd, WM_TRAY);
        }
        self.widget.is_some()
    }

    fn placeholder_tip(&self) -> &'static str {
        if self.searching {
            "Battery Monitor — looking for devices…"
        } else {
            "Battery Monitor — no wireless devices found"
        }
    }

    fn show_widget(&mut self, readings: &[Battery]) {
        let item = |b: &Battery| icon::WidgetItem {
            percent: Some(b.percent),
            charging: b.charging,
            stale: b.stale,
            kind: Some(b.kind),
        };
        let shown = self.shown(readings);
        let (items, tip): (Vec<icon::WidgetItem>, String) = match shown {
            None => (
                vec![icon::WidgetItem { percent: None, charging: false, stale: false, kind: None }],
                self.placeholder_tip().to_string(),
            ),
            Some(s) if !self.separate => (vec![item(s)], summary(readings, s)),
            Some(s) => (readings.iter().map(item).collect(), summary(readings, s)),
        };
        let light = icon::taskbar_is_light();
        let style = self.style;
        if let Some(w) = self.widget.as_mut() {
            let canvas = icon::render_widget(&items, style, w.height(), w.scale(), light);
            w.show(&canvas, &tip);
        }
    }

    /// Once a second: keep the widget in place, re-render after a DPI change, and recreate it when the taskbar comes
    /// back (Explorer restart) or becomes available.
    fn tick_widget(&mut self) {
        let state = self.widget.as_mut().map(|w| w.reposition());
        match state {
            Some(widget::State::Ok) => {}
            Some(widget::State::DpiChanged) => self.apply(self.latest.clone()),
            Some(widget::State::Gone) => {
                self.widget = None;
                self.apply(self.latest.clone());
            }
            None if self.use_widget => {
                if let Some(w) = widget::create(self.hwnd, WM_TRAY) {
                    self.widget = Some(w);
                    self.apply(self.latest.clone());
                }
            }
            None => {}
        }
    }

    fn apply_tray(&mut self, readings: &[Battery]) {
        // Per-device icons exist only in one-icon-per-device mode, and only for devices still present.
        let keep: HashSet<&str> =
            if self.separate { readings.iter().map(|b| b.id.as_str()).collect() } else { HashSet::new() };
        let gone: Vec<String> =
            self.icons.keys().filter(|id| *id != MAIN_ID && !keep.contains(id.as_str())).cloned().collect();
        for id in gone {
            self.remove_icon(&id);
        }

        let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) }.max(16) as usize;
        let light = icon::taskbar_is_light();
        if self.separate {
            for b in readings {
                let canvas = icon::render(Some(b.percent), b.charging, b.stale, Some(b.kind), self.style, size, light);
                self.set_icon(&b.id, icon::to_hicon(&canvas), &describe(b));
            }
            if readings.is_empty() {
                self.show_placeholder(self.placeholder_tip());
            } else {
                self.remove_icon(MAIN_ID);
            }
        } else if let Some(shown) = self.shown(readings) {
            let canvas = icon::render(
                Some(shown.percent),
                shown.charging,
                shown.stale,
                Some(shown.kind),
                self.style,
                size,
                light,
            );
            let tip = summary(readings, shown);
            self.set_icon(MAIN_ID, icon::to_hicon(&canvas), &tip);
        } else {
            self.show_placeholder(self.placeholder_tip());
        }
    }

    fn shown<'a>(&self, readings: &'a [Battery]) -> Option<&'a Battery> {
        pick_shown(readings, self.pinned.as_deref())
    }

    fn show_placeholder(&mut self, tip: &str) {
        let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) }.max(16) as usize;
        let canvas = icon::render(None, false, false, None, icon::Style::Digits, size, icon::taskbar_is_light());
        self.set_icon(MAIN_ID, icon::to_hicon(&canvas), tip);
    }

    fn base_data(&self, uid: u32) -> NOTIFYICONDATAW {
        let mut d: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        d.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        d.hWnd = self.hwnd;
        d.uID = uid;
        d
    }

    fn set_icon(&mut self, id: &str, hicon: HICON, tip: &str) {
        let existing = self.icons.get(id).map(|i| (i.uid, i.hicon));
        let uid = existing.map_or_else(
            || {
                self.next_uid += 1;
                self.next_uid
            },
            |(uid, _)| uid,
        );
        let mut d = self.base_data(uid);
        d.uFlags = NIF_ICON | NIF_TIP | NIF_MESSAGE | NIF_SHOWTIP;
        d.uCallbackMessage = WM_TRAY;
        d.hIcon = hicon;
        copy_wide(&mut d.szTip, tip);
        unsafe {
            if existing.is_some() {
                Shell_NotifyIconW(NIM_MODIFY, &d);
            } else {
                Shell_NotifyIconW(NIM_ADD, &d);
                d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                Shell_NotifyIconW(NIM_SETVERSION, &d);
            }
            if let Some((_, old)) = existing {
                DestroyIcon(old);
            }
        }
        self.icons.insert(id.to_string(), TrayIcon { uid, hicon });
    }

    fn remove_icon(&mut self, id: &str) {
        if let Some(i) = self.icons.remove(id) {
            let d = self.base_data(i.uid);
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &d);
                DestroyIcon(i.hicon);
            }
        }
    }

    fn maybe_warn_low(&mut self, b: &Battery) {
        if b.charging || b.percent > REARM_ABOVE {
            self.low_warned.remove(&b.id);
            return;
        }
        if b.stale || b.percent > NOTIFY_BELOW || !self.low_warned.insert(b.id.clone()) {
            return;
        }
        if self.widget.is_some() && !self.icons.contains_key(MAIN_ID) {
            // Widget mode has no tray icon to attach the warning to: show one for a moment.
            let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) }.max(16) as usize;
            let canvas = icon::render(
                Some(b.percent),
                b.charging,
                false,
                Some(b.kind),
                self.style,
                size,
                icon::taskbar_is_light(),
            );
            self.set_icon(MAIN_ID, icon::to_hicon(&canvas), &describe(b));
            self.balloon_icon = true;
            unsafe { SetTimer(self.hwnd, TIMER_BALLOON, 15_000, None) };
        }
        let Some(uid) = self.icons.get(&b.id).or_else(|| self.icons.get(MAIN_ID)).map(|i| i.uid) else { return };
        let mut d = self.base_data(uid);
        d.uFlags = NIF_INFO;
        d.dwInfoFlags = NIIF_WARNING;
        copy_wide(&mut d.szInfoTitle, "Low battery");
        copy_wide(&mut d.szInfo, &format!("{} is at {}%.", b.name, b.percent));
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) };
    }

    fn remove_all(&mut self) {
        self.widget = None; // restores the taskbar layout it adjusted
        let ids: Vec<String> = self.icons.keys().cloned().collect();
        for id in ids {
            self.remove_icon(&id);
        }
    }
}

/// The device the single icon shows: the pinned one if connected, else the lowest battery that needs attention
/// (not charging, not asleep), falling back to any awake device, then to anything.
fn pick_shown<'a>(readings: &'a [Battery], pinned: Option<&str>) -> Option<&'a Battery> {
    if let Some(b) = pinned.and_then(|p| readings.iter().find(|b| b.id == p)) {
        return Some(b);
    }
    let lowest = |f: &dyn Fn(&Battery) -> bool| readings.iter().filter(|b| f(b)).min_by_key(|b| b.percent);
    lowest(&|b| !b.stale && !b.charging).or_else(|| lowest(&|b| !b.stale)).or_else(|| lowest(&|_| true))
}

fn describe(b: &Battery) -> String {
    let state = if b.charging {
        " (charging)"
    } else if b.stale {
        " (asleep — last known)"
    } else {
        ""
    };
    format!("{}: {}%{state}", b.name, b.percent)
}

/// Tooltip for the single icon: the device it shows first, then the others (Windows cuts tooltips at 127 chars).
fn summary(readings: &[Battery], shown: &Battery) -> String {
    std::iter::once(shown)
        .chain(readings.iter().filter(|b| b.id != shown.id))
        .map(describe)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Copies `s` into a fixed-size UTF-16 field, truncating and NUL-terminating.
fn copy_wide(field: &mut [u16], s: &str) {
    let units: Vec<u16> = s.encode_utf16().take(field.len() - 1).collect();
    field[..units.len()].copy_from_slice(&units);
    field[units.len()] = 0;
}

fn show_menu(hwnd: HWND) {
    // Build the menu from current state, then release the state before TrackPopupMenu: it runs a modal loop that
    // keeps dispatching our messages (timers, results).
    let Some((latest, separate, pinned, style, use_widget)) =
        with_app(|app| (app.latest.clone(), app.separate, app.pinned.clone(), app.style, app.use_widget))
    else {
        return;
    };
    unsafe {
        let menu = CreatePopupMenu();
        let add_to = |m: HMENU, flags: u32, id: usize, text: &str| {
            let w = wide(text);
            AppendMenuW(m, flags, id, w.as_ptr());
        };
        let add = |flags: u32, id: usize, text: &str| add_to(menu, flags, id, text);
        let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
        if latest.is_empty() {
            add(MF_STRING | MF_GRAYED, 0, "No devices found");
        }
        for b in &latest {
            add(MF_STRING | MF_DISABLED, 0, &describe(b)); // not clickable, but drawn readable (not greyed)
        }
        AppendMenuW(menu, MF_SEPARATOR, 0, null());

        // Which device the single icon shows. A pinned device that isn't connected counts as automatic.
        if !separate && !latest.is_empty() {
            let shows = CreatePopupMenu();
            let pinned_present = pinned.as_ref().filter(|p| latest.iter().any(|b| &b.id == *p));
            add_to(shows, MF_STRING | check(pinned_present.is_none()), CMD_AUTO, "Lowest battery (automatic)");
            AppendMenuW(shows, MF_SEPARATOR, 0, null());
            for (i, b) in latest.iter().enumerate() {
                add_to(shows, MF_STRING | check(pinned_present == Some(&b.id)), CMD_PIN_BASE + i, &b.name);
            }
            add(MF_STRING | MF_POPUP, shows as usize, "Icon shows");
        }
        let styles = CreatePopupMenu();
        for (i, (st, label)) in icon::Style::ALL.iter().enumerate() {
            add_to(styles, MF_STRING | check(*st == style), CMD_STYLE_BASE + i, label);
        }
        add(MF_STRING | MF_POPUP, styles as usize, "Icon style");
        add(MF_STRING | check(use_widget), CMD_WIDGET, "Show on the taskbar");
        add(MF_STRING | check(separate), CMD_SEPARATE, "One icon per device");
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        add(MF_STRING, CMD_REFRESH, "Refresh now");
        add(
            MF_STRING | if startup::is_enabled() { MF_CHECKED } else { MF_UNCHECKED },
            CMD_STARTUP,
            "Start with Windows",
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        add(MF_STRING | MF_GRAYED, 0, &format!("Battery Monitor v{}", crate::VERSION));
        add(MF_STRING, CMD_EXIT, "Exit");

        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        SetForegroundWindow(hwnd); // required, or the menu doesn't close when clicking elsewhere
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, null());
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);

        match cmd as usize {
            CMD_REFRESH => {
                with_app(App::start_poll);
            }
            CMD_STARTUP => startup::set_enabled(!startup::is_enabled()),
            CMD_SEPARATE => {
                settings::set_separate_icons(!separate);
                with_app(|app| {
                    app.separate = !separate;
                    app.apply(app.latest.clone());
                });
            }
            n if (CMD_STYLE_BASE..CMD_STYLE_BASE + icon::Style::ALL.len()).contains(&n) => {
                let style = icon::Style::ALL[n - CMD_STYLE_BASE].0;
                settings::set_icon_style(style);
                with_app(|app| {
                    app.style = style;
                    app.apply(app.latest.clone());
                });
            }
            CMD_WIDGET => {
                settings::set_taskbar_widget(!use_widget);
                with_app(|app| {
                    app.use_widget = !use_widget;
                    app.apply(app.latest.clone());
                });
            }
            CMD_AUTO => pin(None),
            n if n >= CMD_PIN_BASE && n - CMD_PIN_BASE < latest.len() => pin(Some(latest[n - CMD_PIN_BASE].id.clone())),
            CMD_EXIT => {
                with_app(App::remove_all);
                DestroyWindow(hwnd);
            }
            _ => {}
        }
    }
}

/// Shows `device` on the single icon (None = lowest battery automatically) and remembers the choice.
fn pin(device: Option<String>) {
    settings::set_icon_device(device.as_deref());
    with_app(|app| {
        app.pinned = device;
        app.apply(app.latest.clone());
    });
}

#[cfg(test)]
mod tests {
    use super::pick_shown;
    use crate::devices::{Battery, Kind};

    fn dev(id: &str, percent: u8, charging: bool, stale: bool) -> Battery {
        Battery { id: id.into(), name: id.into(), kind: Kind::Mouse, percent, charging, stale }
    }

    #[test]
    fn shows_the_lowest_battery_that_needs_attention() {
        let r = [dev("pad", 70, false, false), dev("mouse", 40, false, false), dev("headset", 10, true, false)];
        assert_eq!(pick_shown(&r, None).unwrap().id, "mouse"); // the charging headset doesn't need attention
    }

    #[test]
    fn pinned_device_wins_while_connected() {
        let r = [dev("pad", 70, false, false), dev("mouse", 40, false, false)];
        assert_eq!(pick_shown(&r, Some("pad")).unwrap().id, "pad");
        assert_eq!(pick_shown(&r, Some("gone")).unwrap().id, "mouse");
    }

    #[test]
    fn falls_back_to_charging_then_asleep() {
        let r = [dev("pad", 70, true, false), dev("mouse", 20, false, true)];
        assert_eq!(pick_shown(&r, None).unwrap().id, "pad"); // awake beats asleep
        let all_asleep = [dev("pad", 70, false, true), dev("mouse", 20, false, true)];
        assert_eq!(pick_shown(&all_asleep, None).unwrap().id, "mouse");
        assert!(pick_shown(&[], None).is_none());
    }
}
