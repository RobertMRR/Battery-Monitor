//! The taskbar widget: a small transparent strip drawn on the taskbar itself, just left of the notification area,
//! showing each device as an icon (in the chosen style) plus its percentage.
//!
//! It is a layered child window of the taskbar (`Shell_TrayWnd`), painted with per-pixel alpha through
//! UpdateLayeredWindow. It is placed next to `TrayNotifyWnd` and re-checked about once a second, because Explorer
//! moves things around (tray icons appearing, DPI changes, the Windows 10 News/Weather button). On Windows 10 the task
//! buttons (`ReBarWindow32`) are narrowed so they never slide underneath it. A vertical or missing taskbar means no
//! widget (the app falls back to tray icons). Approach as in Wireless-Device-Battery-Monitor (GPL-3.0).

use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::hid::wide;
use crate::icon::Canvas;

const CLASS: &str = "BatteryMonitorTaskbarWidget";

/// Where clicks are reported: the app window and the message it gets (as a tray callback with WM_CONTEXTMENU).
static NOTIFY: AtomicIsize = AtomicIsize::new(0);
static CLICK_MSG: AtomicU32 = AtomicU32::new(0);
static REGISTERED: AtomicBool = AtomicBool::new(false);

pub enum State {
    Ok,
    /// The taskbar's DPI changed: render again at the new size.
    DpiChanged,
    /// The taskbar is gone (Explorer restarted or crashed): drop this widget and create a new one.
    Gone,
}

pub struct Widget {
    hwnd: HWND,
    taskbar: HWND,
    /// Windows 10's task-button strip, narrowed to keep it clear of us (None on Windows 11).
    rebar: HWND,
    tip: HWND,
    tip_text: Vec<u16>,
    width: i32,
    height: i32,
    dpi: u32,
    last: Option<(RECT, RECT, i32, i32)>, // tray rect, taskbar rect, our width, our left: skip no-op moves
}

pub fn create(notify: HWND, click_msg: u32) -> Option<Widget> {
    unsafe {
        let taskbar = FindWindowW(wide("Shell_TrayWnd").as_ptr(), null());
        if taskbar.is_null() || FindWindowExW(taskbar, null_mut(), wide("TrayNotifyWnd").as_ptr(), null()).is_null() {
            return None;
        }
        let mut bar: RECT = std::mem::zeroed();
        GetWindowRect(taskbar, &mut bar);
        if bar.bottom - bar.top > bar.right - bar.left {
            return None; // vertical taskbar: tray icons instead
        }
        // Windows 11's taskbar is a XAML island; Windows 10's task buttons live in ReBarWindow32.
        let xaml = FindWindowExW(
            taskbar,
            null_mut(),
            wide("Windows.UI.Composition.DesktopWindowContentBridge").as_ptr(),
            null(),
        );
        let rebar = if xaml.is_null() {
            FindWindowExW(taskbar, null_mut(), wide("ReBarWindow32").as_ptr(), null())
        } else {
            null_mut()
        };

        NOTIFY.store(notify as isize, Ordering::Relaxed);
        CLICK_MSG.store(click_msg, Ordering::Relaxed);
        let instance = GetModuleHandleW(null());
        let class = wide(CLASS);
        if !REGISTERED.swap(true, Ordering::Relaxed) {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                ..std::mem::zeroed()
            };
            RegisterClassW(&wc);
        }
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_NOACTIVATE,
            class.as_ptr(),
            null(),
            WS_CHILD | WS_CLIPSIBLINGS,
            0,
            0,
            1,
            1,
            taskbar,
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return None;
        }

        let icc = INITCOMMONCONTROLSEX { dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_WIN95_CLASSES };
        InitCommonControlsEx(&icc);
        let tip = CreateWindowExW(
            WS_EX_TOPMOST,
            TOOLTIPS_CLASSW,
            null(),
            WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX,
            0,
            0,
            0,
            0,
            notify,
            null_mut(),
            instance,
            null(),
        );
        let mut w = Widget {
            hwnd,
            taskbar,
            rebar,
            tip,
            tip_text: wide("Battery Monitor"),
            width: 0,
            height: 0,
            dpi: GetDpiForWindow(taskbar).max(96),
            last: None,
        };
        if !tip.is_null() {
            let mut ti = w.tool_info();
            SendMessageW(tip, TTM_ADDTOOLW, 0, &mut ti as *mut _ as LPARAM);
            SendMessageW(tip, TTM_SETMAXTIPWIDTH, 0, 600);
        }
        Some(w)
    }
}

impl Widget {
    fn tool_info(&mut self) -> TTTOOLINFOW {
        let mut ti: TTTOOLINFOW = unsafe { std::mem::zeroed() };
        ti.cbSize = size_of::<TTTOOLINFOW>() as u32;
        ti.uFlags = TTF_IDISHWND | TTF_SUBCLASS;
        ti.hwnd = NOTIFY.load(Ordering::Relaxed) as HWND;
        ti.uId = self.hwnd as usize;
        ti.lpszText = self.tip_text.as_mut_ptr();
        ti
    }

    /// Scale factor of the taskbar's monitor (1.0 at 96 DPI).
    pub fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    /// Height to render at: the taskbar's.
    pub fn height(&self) -> usize {
        let mut r: RECT = unsafe { std::mem::zeroed() };
        unsafe { GetClientRect(self.taskbar, &mut r) };
        (r.bottom - r.top).max(1) as usize
    }

    /// Shows `canvas` (width x height pixels, straight alpha) and sets the hover text.
    pub fn show(&mut self, canvas: &Canvas, tooltip: &str) {
        self.paint(canvas);
        if !self.tip.is_null() {
            self.tip_text = wide(tooltip);
            let mut ti = self.tool_info();
            unsafe { SendMessageW(self.tip, TTM_UPDATETIPTEXTW, 0, &mut ti as *mut _ as LPARAM) };
        }
        self.last = None; // size may have changed: place it again
        self.reposition();
    }

    fn paint(&mut self, c: &Canvas) {
        let (w, h) = (c.size as i32, c.height as i32);
        unsafe {
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = w;
            bmi.bmiHeader.biHeight = -h; // top-down
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;
            let screen = GetDC(null_mut());
            let mem = CreateCompatibleDC(screen);
            let mut bits = null_mut();
            let dib = CreateDIBSection(screen, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if !dib.is_null() {
                // UpdateLayeredWindow wants premultiplied BGRA.
                let out = std::slice::from_raw_parts_mut(bits as *mut u8, (w * h * 4) as usize);
                for i in 0..(w * h) as usize {
                    let [r, g, b, a] = c.rgba8(i);
                    let pm = |v: u8| ((v as u32 * a as u32 + 127) / 255) as u8;
                    out[i * 4..i * 4 + 4].copy_from_slice(&[pm(b), pm(g), pm(r), a]);
                }
                let old = SelectObject(mem, dib);
                let size = SIZE { cx: w, cy: h };
                let src = POINT { x: 0, y: 0 };
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                UpdateLayeredWindow(self.hwnd, screen, null(), &size, mem, &src, 0, &blend, ULW_ALPHA);
                SelectObject(mem, old);
                DeleteObject(dib);
            }
            DeleteDC(mem);
            ReleaseDC(null_mut(), screen);
        }
        self.width = w;
        self.height = h;
    }

    /// Keeps the widget just left of the notification area. Call about once a second.
    pub fn reposition(&mut self) -> State {
        unsafe {
            if IsWindow(self.hwnd) == 0 || IsWindow(self.taskbar) == 0 {
                return State::Gone;
            }
            let dpi = GetDpiForWindow(self.taskbar).max(96);
            if dpi != self.dpi {
                self.dpi = dpi;
                return State::DpiChanged;
            }
            let tray = FindWindowExW(self.taskbar, null_mut(), wide("TrayNotifyWnd").as_ptr(), null());
            if tray.is_null() {
                return State::Ok;
            }
            let (mut tr, mut bar): (RECT, RECT) = (std::mem::zeroed(), std::mem::zeroed());
            GetWindowRect(tray, &mut tr);
            GetWindowRect(self.taskbar, &mut bar);
            let mut edge = tr.left;
            // Windows 10: News and Interests / Weather sits immediately left of the tray.
            let feeds = FindWindowExW(self.taskbar, null_mut(), wide("ShellFeedsTaskbarViewWindow").as_ptr(), null());
            if !feeds.is_null() && IsWindowVisible(feeds) != 0 {
                let mut fr: RECT = std::mem::zeroed();
                GetWindowRect(feeds, &mut fr);
                if fr.left < edge && fr.right > fr.left {
                    edge = fr.left;
                }
            }
            let mut p = POINT { x: edge, y: bar.top };
            ScreenToClient(self.taskbar, &mut p);
            let left = p.x - self.width - (2.0 * self.scale()) as i32;

            // Windows 10: Explorer re-expands the task buttons on its own layout passes; narrow them again.
            if !self.rebar.is_null() && IsWindow(self.rebar) != 0 {
                let mut rr: RECT = std::mem::zeroed();
                GetWindowRect(self.rebar, &mut rr);
                let mut tl = POINT { x: rr.left, y: rr.top };
                ScreenToClient(self.taskbar, &mut tl);
                if tl.x + (rr.right - rr.left) > left && left > tl.x {
                    MoveWindow(self.rebar, tl.x, tl.y, left - tl.x, rr.bottom - rr.top, 1);
                }
            }

            // Re-place only when something moved, or when another taskbar child got above us.
            let covered = !GetWindow(self.hwnd, GW_HWNDPREV).is_null();
            let now = (tr, bar, self.width, left);
            let same = self
                .last
                .is_some_and(|(a, b, w, l)| rect_eq(&a, &now.0) && rect_eq(&b, &now.1) && w == now.2 && l == now.3);
            if !covered && same {
                return State::Ok;
            }
            self.last = Some(now);
            let y = ((bar.bottom - bar.top) - self.height).max(0) / 2;
            SetWindowPos(self.hwnd, HWND_TOP, left, y, self.width, self.height, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            State::Ok
        }
    }
}

fn rect_eq(a: &RECT, b: &RECT) -> bool {
    (a.left, a.top, a.right, a.bottom) == (b.left, b.top, b.right, b.bottom)
}

impl Drop for Widget {
    fn drop(&mut self) {
        unsafe {
            if !self.rebar.is_null() && IsWindow(self.rebar) != 0 && IsWindow(self.taskbar) != 0 {
                // Let Explorer lay the task buttons out over the full width again.
                let mut r: RECT = std::mem::zeroed();
                GetClientRect(self.taskbar, &mut r);
                PostMessageW(
                    self.taskbar,
                    WM_SIZE,
                    SIZE_RESTORED as WPARAM,
                    ((r.bottom as isize) << 16) | r.right as isize,
                );
            }
            if !self.tip.is_null() {
                DestroyWindow(self.tip);
            }
            DestroyWindow(self.hwnd);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_LBUTTONUP | WM_RBUTTONUP => {
            // Same as clicking the tray icon: the app shows its menu at the cursor.
            let notify = NOTIFY.load(Ordering::Relaxed) as HWND;
            unsafe { PostMessageW(notify, CLICK_MSG.load(Ordering::Relaxed), 0, WM_CONTEXTMENU as LPARAM) };
            0
        }
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
