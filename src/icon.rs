//! Draws a tray icon: the percentage as large digits, with a level bar along the bottom.

use std::ptr::{null, null_mut};
use std::sync::LazyLock;

use ab_glyph::{Font, FontVec, OutlinedGlyph, PxScale, Rect, ScaleFont, VariableFont, point};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::devices::Kind;
use crate::hid::wide;

pub const LOW_THRESHOLD: u8 = 20;

type Rgb = (u8, u8, u8);
const GOOD: Rgb = (0x3F, 0xB9, 0x50);
const WARN: Rgb = (0xE3, 0xA0, 0x08);
const LOW: Rgb = (0xF8, 0x51, 0x49);
const CHARGING: Rgb = (0x4C, 0xA8, 0xFF);
const GRAY: Rgb = (0x80, 0x80, 0x80);
const WHITE: Rgb = (0xFF, 0xFF, 0xFF);
const INK: Rgb = (0x1A, 0x1A, 0x1A);

pub fn level_color(percent: Option<u8>, charging: bool) -> Rgb {
    match (percent, charging) {
        (_, true) => CHARGING,
        (None, _) => GRAY,
        (Some(p), _) if p < LOW_THRESHOLD => LOW,
        (Some(p), _) if p < 50 => WARN,
        _ => GOOD,
    }
}

/// Bahnschrift (condensed, bold) fits "100" into a 16 px icon legibly; Segoe UI Bold / Arial Bold as fallbacks.
static FONT: LazyLock<Option<FontVec>> = LazyLock::new(|| {
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    for (file, variable) in [("bahnschrift.ttf", true), ("segoeuib.ttf", false), ("arialbd.ttf", false)] {
        let Ok(bytes) = std::fs::read(format!(r"{dir}\Fonts\{file}")) else { continue };
        let Ok(mut font) = FontVec::try_from_vec(bytes) else { continue };
        if variable {
            font.set_variation(b"wght", 700.0);
            font.set_variation(b"wdth", 85.0);
        }
        return Some(font);
    }
    None
});

/// Windows' icon font: Segoe Fluent Icons (Windows 11), else Segoe MDL2 Assets (Windows 10).
static ICON_FONT: LazyLock<Option<FontVec>> = LazyLock::new(|| {
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    ["SegoeIcons.ttf", "segmdl2.ttf"]
        .iter()
        .find_map(|f| FontVec::try_from_vec(std::fs::read(format!(r"{dir}\Fonts\{f}")).ok()?).ok())
});

/// Device symbol in the icon font (same code points in both fonts).
fn symbol(kind: Kind) -> char {
    match kind {
        Kind::Mouse => '\u{E962}',
        Kind::Keyboard => '\u{E765}',
        Kind::Gamepad => '\u{E7FC}',
        Kind::Headset => '\u{E7F6}',
        Kind::Other => '\u{E772}',
    }
}

#[derive(Clone, Copy)]
struct Area {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// RGBA pixels with straight (non-premultiplied) alpha, as f32 0..1.
pub struct Canvas {
    pub size: usize,
    px: Vec<[f32; 4]>,
}

impl Canvas {
    fn new(size: usize) -> Canvas {
        Canvas { size, px: vec![[0.0; 4]; size * size] }
    }

    /// Source-over blend of `color` at `alpha` (0..1) onto one pixel.
    fn blend(&mut self, x: usize, y: usize, color: Rgb, alpha: f32) {
        if x >= self.size || y >= self.size || alpha <= 0.0 {
            return;
        }
        let d = &mut self.px[y * self.size + x];
        let out_a = alpha + d[3] * (1.0 - alpha);
        let src = [color.0, color.1, color.2].map(|c| c as f32 / 255.0);
        for i in 0..3 {
            d[i] = (src[i] * alpha + d[i] * d[3] * (1.0 - alpha)) / out_a;
        }
        d[3] = out_a;
    }

    /// Fills a rectangle; a fractional right edge gets partial coverage.
    fn fill(&mut self, x: f32, y: usize, w: f32, h: usize, color: Rgb, alpha: f32) {
        for row in y..(y + h).min(self.size) {
            let mut col = x.floor() as usize;
            while (col as f32) < x + w && col < self.size {
                let cover = ((x + w) - col as f32).min(1.0);
                self.blend(col, row, color, alpha * cover);
                col += 1;
            }
        }
    }

    pub fn rgba8(&self, i: usize) -> [u8; 4] {
        self.px[i].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
    }
}

/// How a device is drawn in its tray icon. (The exact percentage is always in the tooltip.)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    /// Big percentage + level bar.
    Digits,
    /// Device symbol on the left, percentage on the right, level bar.
    SymbolDigits,
    /// Big device symbol + level bar (percentage in the tooltip).
    SymbolBar,
    /// Big device symbol, coloured from the bottom up to the battery level.
    SymbolFill,
}

impl Style {
    /// Menu order, with labels.
    pub const ALL: [(Style, &'static str); 4] = [
        (Style::SymbolBar, "Device + level bar"),
        (Style::SymbolFill, "Device, filled by level"),
        (Style::SymbolDigits, "Device + percentage"),
        (Style::Digits, "Percentage"),
    ];
}

/// Renders one icon. `kind` = None (no device, e.g. the "no devices" placeholder) always uses `Style::Digits`.
pub fn render(
    percent: Option<u8>,
    charging: bool,
    stale: bool,
    kind: Option<Kind>,
    style: Style,
    size: usize,
    light: bool,
) -> Canvas {
    let mut c = Canvas::new(size);
    let alpha = if stale { 110.0 / 255.0 } else { 1.0 };
    let fg = if light { INK } else { WHITE };
    let s = size as f32;

    let bar_h = (size / 7).max(2);
    let text_h = (size - bar_h - (size / 16).max(1)) as f32;
    let text = percent.map_or_else(|| "–".to_string(), |p| p.to_string());
    let text_color = if percent.is_some_and(|p| p < LOW_THRESHOLD) && !charging { LOW } else { fg };
    let above_bar = Area { x: 0.0, y: 0.0, w: s, h: text_h };
    let digits_font = FONT.as_ref();
    let symbol_font = ICON_FONT.as_ref();

    let (style, k) = match kind {
        Some(k) => (style, k),
        None => (Style::Digits, Kind::Other),
    };
    let glyph = symbol(k).to_string();
    let mut bar = true;
    match style {
        Style::Digits => draw_fitted(&mut c, digits_font, &text, above_bar, Some("0"), text_color, alpha, false),
        Style::SymbolDigits => {
            let gw = (s * 0.42).round();
            // Icon-font symbols are 1-px line art; at tray sizes their strokes fade, so boost coverage.
            draw_fitted(&mut c, symbol_font, &glyph, Area { w: gw - 1.0, ..above_bar }, None, fg, alpha, true);
            let digits = Area { x: gw, y: 0.0, w: s - gw, h: text_h };
            draw_fitted(&mut c, digits_font, &text, digits, Some("0"), text_color, alpha, false);
        }
        Style::SymbolBar => draw_fitted(&mut c, symbol_font, &glyph, above_bar, None, fg, alpha, true),
        Style::SymbolFill => {
            bar = false;
            let mut mask = Canvas::new(size);
            draw_fitted(&mut mask, symbol_font, &glyph, Area { x: 0.0, y: 0.0, w: s, h: s }, None, WHITE, 1.0, true);
            // Rows of the symbol below the level line take the level colour; the rest is drawn faint.
            let rows: Vec<usize> = (0..size).filter(|&y| (0..size).any(|x| mask.px[y * size + x][3] > 0.05)).collect();
            if let (Some(&top), Some(&bottom)) = (rows.first(), rows.last()) {
                let filled = percent.unwrap_or(0) as f32 / 100.0;
                let level_y = bottom as f32 + 1.0 - (bottom + 1 - top) as f32 * filled;
                let color = level_color(percent, charging);
                for y in 0..size {
                    for x in 0..size {
                        let a = mask.px[y * size + x][3];
                        if a > 0.0 {
                            let lit = ((y as f32 + 1.0) - level_y).clamp(0.0, 1.0); // partial row at the edge
                            c.blend(x, y, fg, a * 0.35 * alpha);
                            c.blend(x, y, color, a * lit * alpha);
                        }
                    }
                }
            }
        }
    }

    if let (true, Some(p)) = (bar, percent) {
        let top = size - bar_h;
        c.fill(0.0, top, s, bar_h, fg, if stale { 40.0 } else { 70.0 } / 255.0);
        c.fill(0.0, top, (s * p as f32 / 100.0).max(1.0), bar_h, level_color(percent, charging), alpha);
    }
    c
}

fn layout(font: &FontVec, text: &str, scale: f32, origin: (f32, f32)) -> Vec<OutlinedGlyph> {
    let px = PxScale::from(scale);
    let sf = font.as_scaled(px);
    let mut x = origin.0;
    let mut prev = None;
    let mut out = Vec::new();
    for ch in text.chars() {
        let id = font.glyph_id(ch);
        if let Some(p) = prev {
            x += sf.kern(p, id);
        }
        if let Some(o) = font.outline_glyph(id.with_scale_and_position(px, point(x, origin.1 + sf.ascent()))) {
            out.push(o);
        }
        x += sf.h_advance(id);
        prev = Some(id);
    }
    out
}

fn union(glyphs: &[OutlinedGlyph]) -> Option<Rect> {
    glyphs.iter().map(|g| g.px_bounds()).reduce(|a, b| Rect {
        min: point(a.min.x.min(b.min.x), a.min.y.min(b.min.y)),
        max: point(a.max.x.max(b.max.x), a.max.y.max(b.max.y)),
    })
}

/// Scales `text` to fill `area`: by the height of `band` (e.g. "0", so all digits share one size) or of the text itself,
/// unless it's too wide (e.g. "100"), then by width. Centred.
#[allow(clippy::too_many_arguments)]
fn draw_fitted(
    c: &mut Canvas,
    font: Option<&FontVec>,
    text: &str,
    area: Area,
    band: Option<&str>,
    color: Rgb,
    alpha: f32,
    bold: bool,
) {
    let Some(font) = font else { return };
    if area.w <= 0.0 || area.h <= 0.0 {
        return;
    }
    let measure = |scale: f32| {
        let t = union(&layout(font, text, scale, (0.0, 0.0)))?;
        let d = union(&layout(font, band.unwrap_or(text), scale, (0.0, 0.0)))?;
        Some((t, d))
    };
    const REF: f32 = 200.0;
    let Some((t, d)) = measure(REF) else { return };
    let scale = REF * (area.w / t.width()).min(area.h / d.height());

    // Measure at the final scale, then lay out again at the centred sub-pixel origin so anti-aliasing is exact.
    let Some((t, d)) = measure(scale) else { return };
    let origin = (area.x + (area.w - t.width()) / 2.0 - t.min.x, area.y + (area.h - d.height()) / 2.0 - d.min.y);
    for g in layout(font, text, scale, origin) {
        let b = g.px_bounds();
        g.draw(|x, y, cover| {
            let (px, py) = (b.min.x as i32 + x as i32, b.min.y as i32 + y as i32);
            if px >= 0 && py >= 0 {
                let cover = if bold { cover.sqrt() } else { cover };
                c.blend(px as usize, py as usize, color, cover * alpha);
            }
        });
    }
}

pub fn taskbar_is_light() -> bool {
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let name = wide("SystemUsesLightTheme");
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
    r == 0 && value == 1
}

/// Converts a canvas into an HICON (caller destroys it with DestroyIcon).
pub fn to_hicon(c: &Canvas) -> HICON {
    let size = c.size as i32;
    unsafe {
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = size;
        bmi.bmiHeader.biHeight = -size; // top-down
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        let mut bits = null_mut();
        let color = CreateDIBSection(null_mut(), &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if color.is_null() {
            return null_mut();
        }
        let out = std::slice::from_raw_parts_mut(bits as *mut u8, c.size * c.size * 4);
        for i in 0..c.size * c.size {
            let [r, g, b, a] = c.rgba8(i);
            out[i * 4..i * 4 + 4].copy_from_slice(&[b, g, r, a]);
        }
        let mask = CreateBitmap(size, size, 1, 1, null());
        let info = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info);
        DeleteObject(color);
        DeleteObject(mask);
        icon
    }
}

/// Dev aid: every style at 16/24/32 px on a dark taskbar, then every style at 16 px on a light one; magnified 4x.
pub fn write_preview(path: &str) -> std::io::Result<()> {
    let samples: [(Option<u8>, bool, bool, Kind); 8] = [
        (Some(85), false, false, Kind::Mouse),
        (Some(42), false, false, Kind::Gamepad),
        (Some(12), false, false, Kind::Headset),
        (Some(100), false, false, Kind::Keyboard),
        (Some(63), true, false, Kind::Other),
        (Some(70), false, true, Kind::Mouse),
        (Some(7), false, false, Kind::Gamepad),
        (None, false, false, Kind::Other),
    ];
    let mut rows: Vec<(usize, Style, bool)> = Vec::new(); // (size, style, light)
    for size in [16usize, 24, 32] {
        rows.extend(Style::ALL.map(|(st, _)| (size, st, false)));
    }
    rows.extend(Style::ALL.map(|(st, _)| (16, st, true)));

    let (zoom, pad) = (4usize, 8usize);
    let cell = 32 * zoom + pad;
    let (w, h) = (samples.len() * cell + pad, rows.len() * cell + pad);
    let mut img = vec![(0u8, 0u8, 0u8); w * h];
    for (row, &(size, style, light)) in rows.iter().enumerate() {
        let bg = if light { (0xEE, 0xEE, 0xEE) } else { (0x20, 0x20, 0x20) };
        for y in row * cell..((row + 1) * cell + pad).min(h) {
            img[y * w..(y + 1) * w].fill(bg);
        }
        for (i, &(p, ch, st, k)) in samples.iter().enumerate() {
            let c = render(p, ch, st, p.map(|_| k), style, size, light);
            for y in 0..size * zoom {
                for x in 0..size * zoom {
                    let [r, g, b, a] = c.rgba8((y / zoom) * size + x / zoom);
                    let d = &mut img[(pad / 2 + row * cell + y) * w + pad + i * cell + x];
                    let mix = |s: u8, d: u8| ((s as u32 * a as u32 + d as u32 * (255 - a as u32)) / 255) as u8;
                    *d = (mix(r, d.0), mix(g, d.1), mix(b, d.2));
                }
            }
        }
    }
    write_bmp(path, &img, w, h)
}

fn write_bmp(path: &str, img: &[(u8, u8, u8)], w: usize, h: usize) -> std::io::Result<()> {
    let row_len = (w * 3).div_ceil(4) * 4;
    let mut bmp = Vec::with_capacity(54 + row_len * h);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&((54 + row_len * h) as u32).to_le_bytes());
    bmp.extend_from_slice(&[0, 0, 0, 0]);
    bmp.extend_from_slice(&54u32.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&(w as i32).to_le_bytes());
    bmp.extend_from_slice(&(h as i32).to_le_bytes()); // bottom-up
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&24u16.to_le_bytes());
    bmp.extend_from_slice(&[0; 24]);
    for y in (0..h).rev() {
        for &(r, g, b) in &img[y * w..(y + 1) * w] {
            bmp.extend_from_slice(&[b, g, r]);
        }
        bmp.resize(bmp.len() + row_len - w * 3, 0);
    }
    std::fs::write(path, bmp)
}

#[cfg(test)]
mod tests {
    use super::{Style, render};
    use crate::devices::Kind;

    /// Visible pixels in rows `from..to` (faint counts: SymbolFill draws the part above the level at 35%).
    fn ink(c: &super::Canvas, from: usize, to: usize) -> usize {
        (from * c.size..to * c.size).filter(|&i| c.rgba8(i)[3] >= 48).count()
    }

    #[test]
    fn every_style_draws_something_at_every_size() {
        for size in [16, 24, 32] {
            for (style, _) in Style::ALL {
                let c = render(Some(42), false, false, Some(Kind::Mouse), style, size, false);
                assert!(ink(&c, 0, size * 2 / 3) > size, "{style:?} at {size}px left the top of the icon empty");
            }
        }
    }

    #[test]
    fn level_bar_fills_proportionally() {
        let size = 32;
        let full = render(Some(100), false, false, None, Style::Digits, size, false);
        let half = render(Some(50), false, false, None, Style::Digits, size, false);
        let bar = |c: &super::Canvas| {
            (0..size)
                .filter(|&x| c.rgba8((size - 1) * size + x)[1] > 150 && c.rgba8((size - 1) * size + x)[0] < 120)
                .count()
        };
        assert_eq!(bar(&full), size);
        assert!((size / 2 - 1..=size / 2 + 1).contains(&bar(&half)));
    }
}
