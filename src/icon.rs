//! Draws the tray icons and taskbar-widget items: device symbols, percentages, level bars and level rings.

use std::ptr::{null, null_mut};
use std::sync::LazyLock;

use ab_glyph::{Font, FontVec, OutlinedGlyph, PxScale, Rect, ScaleFont, VariableFont, point};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::devices::Kind;
use crate::glyphs::{self, Glyph};
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

/// The taskbar's UI font for the widget's percentages: Segoe UI Variable (Windows 11), else Segoe UI.
static TEXT_FONT: LazyLock<Option<FontVec>> = LazyLock::new(|| {
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    ["SegUIVar.ttf", "segoeui.ttf", "arial.ttf"]
        .iter()
        .find_map(|f| FontVec::try_from_vec(std::fs::read(format!(r"{dir}\Fonts\{f}")).ok()?).ok())
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

/// RGBA pixels with straight (non-premultiplied) alpha, as f32 0..1. `size` is the width.
pub struct Canvas {
    pub size: usize,
    pub height: usize,
    px: Vec<[f32; 4]>,
}

impl Canvas {
    fn new(size: usize) -> Canvas {
        Canvas::wide(size, size)
    }

    pub fn wide(width: usize, height: usize) -> Canvas {
        Canvas { size: width, height, px: vec![[0.0; 4]; width * height] }
    }

    /// Source-over blend of `color` at `alpha` (0..1) onto one pixel.
    fn blend(&mut self, x: usize, y: usize, color: Rgb, alpha: f32) {
        if x >= self.size || y >= self.height || alpha <= 0.0 {
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
        for row in y..(y + h).min(self.height) {
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

    /// Copies `other` onto this canvas at (`x`, `y`), blending.
    pub fn draw(&mut self, other: &Canvas, x: usize, y: usize) {
        for oy in 0..other.height {
            for ox in 0..other.size {
                let p = other.px[oy * other.size + ox];
                if p[3] > 0.0 {
                    let rgb = (p[0] * 255.0, p[1] * 255.0, p[2] * 255.0);
                    self.blend(x + ox, y + oy, (rgb.0 as u8, rgb.1 as u8, rgb.2 as u8), p[3]);
                }
            }
        }
    }

    /// Strokes a Lucide glyph (24x24 units) fitted into the square `area`, with round caps and joins.
    fn stroke(&mut self, glyph: Glyph, area: Area, color: Rgb, alpha: f32, width_px: f32) {
        let scale = area.w.min(area.h) / 24.0;
        let (ox, oy) = (area.x + (area.w - 24.0 * scale) / 2.0, area.y + (area.h - 24.0 * scale) / 2.0);
        let segments: Vec<((f32, f32), (f32, f32))> = glyph
            .iter()
            .flat_map(|line| {
                let pts: Vec<(f32, f32)> = line.iter().map(|&(x, y)| (ox + x * scale, oy + y * scale)).collect();
                // A one-point "line" (Lucide's dots) is a zero-length segment: it still gets a round cap.
                if pts.len() == 1 { vec![(pts[0], pts[0])] } else { pts.windows(2).map(|w| (w[0], w[1])).collect() }
            })
            .collect();
        let half = width_px / 2.0;
        let (x0, y0) = ((area.x - half).floor().max(0.0) as usize, (area.y - half).floor().max(0.0) as usize);
        let (x1, y1) = (
            ((area.x + area.w + half).ceil() as usize).min(self.size),
            ((area.y + area.h + half).ceil() as usize).min(self.height),
        );
        for py in y0..y1 {
            for px in x0..x1 {
                let p = (px as f32 + 0.5, py as f32 + 0.5);
                let d = segments.iter().map(|&(a, b)| segment_distance(p, a, b)).fold(f32::MAX, f32::min);
                let cover = (half + 0.5 - d).clamp(0.0, 1.0);
                if cover > 0.0 {
                    self.blend(px, py, color, cover * alpha);
                }
            }
        }
    }

    /// A ring centred at (`cx`, `cy`): the first `fraction` of it (clockwise from 12 o'clock) in `arc`, the rest in
    /// `track`. Anti-aliased at both edges and at the arc's end.
    #[allow(clippy::too_many_arguments)]
    fn ring(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        thickness: f32,
        fraction: f32,
        arc: (Rgb, f32),
        track: (Rgb, f32),
    ) {
        let full = std::f32::consts::TAU;
        for py in 0..self.height {
            for px in 0..self.size {
                let (dx, dy) = (px as f32 + 0.5 - cx, py as f32 + 0.5 - cy);
                let dist = (dx * dx + dy * dy).sqrt();
                let cover = (thickness / 2.0 + 0.5 - (dist - radius).abs()).clamp(0.0, 1.0);
                if cover <= 0.0 {
                    continue;
                }
                let angle = dx.atan2(-dy).rem_euclid(full); // 0 at 12 o'clock, clockwise
                // How far (in pixels along the ring) this pixel is inside the arc; soft over one pixel.
                let along = ((fraction * full - angle) * dist + 0.5).clamp(0.0, 1.0);
                let start = (angle * dist + 0.5).clamp(0.0, 1.0); // soften the 12 o'clock start too
                let lit = if fraction >= 1.0 { 1.0 } else { along * start };
                self.blend(px, py, track.0, cover * track.1 * (1.0 - lit));
                self.blend(px, py, arc.0, cover * arc.1 * lit);
            }
        }
    }
}

fn segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let len2 = abx * abx + aby * aby;
    let t = if len2 == 0.0 { 0.0 } else { (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / len2).clamp(0.0, 1.0) };
    let (qx, qy) = (a.0 + t * abx - p.0, a.1 + t * aby - p.1);
    (qx * qx + qy * qy).sqrt()
}

/// Lucide glyph for each kind of device (used by the Ring style).
fn line_glyph(kind: Kind) -> Glyph {
    match kind {
        Kind::Mouse => glyphs::MOUSE,
        Kind::Keyboard => glyphs::KEYBOARD,
        Kind::Gamepad => glyphs::GAMEPAD,
        Kind::Headset => glyphs::HEADPHONES,
        Kind::Other => glyphs::WIRELESS,
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
    /// Line-art device glyph inside a ring that fills up with the battery level (HaloBattery-style).
    Ring,
}

impl Style {
    /// Menu order, with labels.
    pub const ALL: [(Style, &'static str); 5] = [
        (Style::SymbolBar, "Device + level bar"),
        (Style::Ring, "Device in a level ring"),
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
        Style::Ring => {
            bar = false;
            let level = percent.unwrap_or(0) as f32 / 100.0;
            let (arc_color, track) = ring_colors(percent, charging, fg, light);
            let thickness = (s * 0.085).max(1.4);
            let radius = s / 2.0 - thickness / 2.0 - 0.5;
            c.ring(s / 2.0, s / 2.0, radius, thickness, level, (arc_color, alpha), (track.0, track.1 * alpha));
            // Lucide icons leave ~2 units of padding in their 24-unit box, so this keeps the ink well inside the ring.
            let inner = s * 0.6;
            let area = Area { x: (s - inner) / 2.0, y: (s - inner) / 2.0, w: inner, h: inner };
            c.stroke(line_glyph(k), area, fg, alpha * 0.92, (2.0 * inner / 24.0).max(0.9));
        }
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

/// One device in the taskbar widget.
pub struct WidgetItem {
    pub percent: Option<u8>,
    pub charging: bool,
    pub stale: bool,
    pub kind: Option<Kind>,
}

/// The widget strip: each item as an icon in `style`, plus its percentage unless the style already shows it.
/// `height` = the taskbar's height in pixels, `scale` = its DPI / 96.
pub fn render_widget(items: &[WidgetItem], style: Style, height: usize, scale: f32, light: bool) -> Canvas {
    // The ring style's symbol sits inside the ring, so it gets a larger icon to look as big as the other styles.
    let base = if style == Style::Ring { 29.0 } else { 24.0 };
    let icon = ((base * scale).round() as usize).clamp(12, height.max(12));
    let font_px = 13.0 * scale;
    let (pad, gap, item_gap) = (4.0 * scale, 4.0 * scale, 12.0 * scale);
    let with_text = !matches!(style, Style::Digits | Style::SymbolDigits);
    let fg = if light { INK } else { WHITE };

    let label = |it: &WidgetItem| it.percent.filter(|_| with_text).map(|p| format!("{p}%"));
    let font = TEXT_FONT.as_ref();
    let widths: Vec<f32> = items
        .iter()
        .map(|it| icon as f32 + label(it).map_or(0.0, |t| gap + font.map_or(0.0, |f| text_width(f, &t, font_px))))
        .collect();
    let width = (pad * 2.0 + widths.iter().sum::<f32>() + item_gap * items.len().saturating_sub(1) as f32).ceil();

    let mut c = Canvas::wide(width as usize, height);
    // Windows passes clicks on fully transparent pixels of a layered window through to the taskbar below, so the
    // gaps between and inside the icons would ignore clicks. An invisible (1/255) background makes the whole strip
    // clickable.
    c.fill(0.0, 0, width, height, (0, 0, 0), 1.0 / 255.0);
    let mut x = pad;
    for (it, w) in items.iter().zip(&widths) {
        let img = render(it.percent, it.charging, it.stale, it.kind, style, icon, light);
        c.draw(&img, x.round() as usize, (height - icon) / 2);
        if let (Some(text), Some(f)) = (label(it), font) {
            let low = it.percent.is_some_and(|p| p < LOW_THRESHOLD) && !it.charging;
            let alpha = if it.stale { 110.0 / 255.0 } else { 0.92 };
            draw_text(
                &mut c,
                f,
                &text,
                x + icon as f32 + gap,
                height as f32 / 2.0,
                font_px,
                if low { LOW } else { fg },
                alpha,
            );
        }
        x += w + item_gap;
    }
    c
}

/// Font size in pixels (em) to ab_glyph's scale, which is relative to the font's whole height.
fn em_scale(font: &FontVec, em_px: f32) -> f32 {
    font.units_per_em().map_or(em_px, |upem| em_px * font.height_unscaled() / upem)
}

fn text_width(font: &FontVec, text: &str, em_px: f32) -> f32 {
    let sf = font.as_scaled(PxScale::from(em_scale(font, em_px)));
    text.chars().map(|ch| sf.h_advance(font.glyph_id(ch))).sum()
}

/// Draws `text` starting at `x`, with its digits vertically centred on `center_y`.
#[allow(clippy::too_many_arguments)]
fn draw_text(c: &mut Canvas, font: &FontVec, text: &str, x: f32, center_y: f32, em_px: f32, color: Rgb, alpha: f32) {
    let scale = em_scale(font, em_px);
    let Some(digit) = union(&layout(font, "0", scale, (0.0, 0.0))) else { return };
    let origin = (x, center_y - (digit.min.y + digit.max.y) / 2.0);
    for g in layout(font, text, scale, origin) {
        let b = g.px_bounds();
        g.draw(|gx, gy, cover| {
            let (px, py) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
            if px >= 0 && py >= 0 {
                c.blend(px as usize, py as usize, color, cover * alpha);
            }
        });
    }
}

/// Ring style colours: the arc uses the same level colours as every other style (green, amber, red, blue while
/// charging); the rest of the ring is a faint track. Returns the arc colour and the track (colour, opacity).
fn ring_colors(percent: Option<u8>, charging: bool, fg: Rgb, light: bool) -> (Rgb, (Rgb, f32)) {
    (level_color(percent, charging), (fg, if light { 0.22 } else { 0.28 }))
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

/// Dev aid: the taskbar widget in every style, at 100% and 150% scale, on dark and light taskbars; magnified 2x.
pub fn write_widget_preview(path: &str) -> std::io::Result<()> {
    let items = [
        WidgetItem { percent: Some(85), charging: false, stale: false, kind: Some(Kind::Mouse) },
        WidgetItem { percent: Some(12), charging: false, stale: false, kind: Some(Kind::Headset) },
        WidgetItem { percent: Some(63), charging: true, stale: false, kind: Some(Kind::Gamepad) },
        WidgetItem { percent: Some(70), charging: false, stale: true, kind: Some(Kind::Keyboard) },
    ];
    let mut strips: Vec<(Canvas, bool)> = Vec::new();
    for light in [false, true] {
        for (scale, height) in [(1.0f32, 48usize), (1.5, 72)] {
            for (style, _) in Style::ALL {
                strips.push((render_widget(&items, style, height, scale, light), light));
            }
        }
    }
    let zoom = 2usize;
    let w = strips.iter().map(|(c, _)| c.size).max().unwrap_or(1) * zoom;
    let h: usize = strips.iter().map(|(c, _)| c.height * zoom).sum();
    let mut img = vec![(0u8, 0u8, 0u8); w * h];
    let mut top = 0;
    for (c, light) in &strips {
        let bg = if *light { (0xEE, 0xEE, 0xEE) } else { (0x20, 0x20, 0x20) };
        for y in 0..c.height * zoom {
            for x in 0..w {
                let mut px = bg;
                if x / zoom < c.size {
                    let [r, g, b, a] = c.rgba8((y / zoom) * c.size + x / zoom);
                    let mix = |s: u8, d: u8| ((s as u32 * a as u32 + d as u32 * (255 - a as u32)) / 255) as u8;
                    px = (mix(r, bg.0), mix(g, bg.1), mix(b, bg.2));
                }
                img[(top + y) * w + x] = px;
            }
        }
        top += c.height * zoom;
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
    fn widget_strip_layout() {
        use super::{WidgetItem, render_widget};
        let item = |p| WidgetItem { percent: Some(p), charging: false, stale: false, kind: Some(Kind::Mouse) };
        let one = render_widget(&[item(85)], Style::SymbolBar, 48, 1.0, false);
        let two = render_widget(&[item(85), item(12)], Style::SymbolBar, 48, 1.0, false);
        assert_eq!(one.height, 48);
        assert!(two.size > one.size + 30, "a second device adds an icon and its text");
        // Styles that already draw the number get no extra text, so they're narrower.
        let digits = render_widget(&[item(85)], Style::Digits, 48, 1.0, false);
        assert!(digits.size < one.size);
        // 150% scale: everything grows.
        assert!(render_widget(&[item(85)], Style::Ring, 72, 1.5, false).size > one.size);
    }

    #[test]
    fn ring_arc_follows_the_level() {
        let size = 32;
        // Count bright pixels in the right half (the arc starts at 12 o'clock and runs clockwise).
        let right_half = |p: u8| {
            let c = render(Some(p), false, false, Some(Kind::Mouse), Style::Ring, size, false);
            (0..size * size).filter(|&i| i % size > size / 2 + 1 && c.rgba8(i)[3] > 200).count()
        };
        assert!(right_half(50) > right_half(0) + 10, "half a ring lights up the right side");
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
