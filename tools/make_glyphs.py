"""Generates src/glyphs.rs: Lucide device icons as stroke polylines, for the "Ring" icon style and the taskbar widget.

Run from the repo root:  python tools/make_glyphs.py
Downloads the pinned lucide-static SVGs, flattens every element (paths with lines/curves/arcs, lines, rects,
circles, …) into polylines in Lucide's 24×24 coordinate space, and writes them as Rust constants. The app strokes
them itself at whatever size it needs (width 2 units, round caps, like Lucide), so no SVG or image library is needed.
Lucide is ISC-licensed; its license is copied to assets/licenses/lucide-LICENSE.txt.
"""
import math
import re
import urllib.request
import xml.etree.ElementTree as ET
from pathlib import Path

LUCIDE_VERSION = "1.74.0"
BASE = f"https://cdn.jsdelivr.net/npm/lucide-static@{LUCIDE_VERSION}"
ICONS = {  # Rust constant -> Lucide icon
    "HEADPHONES": "headphones",
    "MOUSE": "mouse",
    "KEYBOARD": "keyboard",
    "GAMEPAD": "gamepad-2",
    "WIRELESS": "radio",
}
CURVE_STEPS = 12
ROOT = Path(__file__).resolve().parent.parent


def fetch(url):
    with urllib.request.urlopen(url, timeout=30) as r:
        return r.read().decode("utf-8")


def arc_points(x1, y1, rx, ry, phi_deg, large, sweep, x2, y2):
    """SVG elliptical arc (endpoint form) -> points after the start point (SVG spec F.6.5)."""
    if rx == 0 or ry == 0 or (x1 == x2 and y1 == y2):
        return [(x2, y2)]
    phi = math.radians(phi_deg)
    cp, sp = math.cos(phi), math.sin(phi)
    dx, dy = (x1 - x2) / 2, (y1 - y2) / 2
    x1p, y1p = cp * dx + sp * dy, -sp * dx + cp * dy
    rx, ry = abs(rx), abs(ry)
    lam = x1p**2 / rx**2 + y1p**2 / ry**2
    if lam > 1:
        rx, ry = rx * math.sqrt(lam), ry * math.sqrt(lam)
    num = rx**2 * ry**2 - rx**2 * y1p**2 - ry**2 * x1p**2
    den = rx**2 * y1p**2 + ry**2 * x1p**2
    coef = math.sqrt(max(0.0, num / den)) * (-1 if large == sweep else 1)
    cxp, cyp = coef * rx * y1p / ry, -coef * ry * x1p / rx
    cx, cy = cp * cxp - sp * cyp + (x1 + x2) / 2, sp * cxp + cp * cyp + (y1 + y2) / 2

    def angle(ux, uy, vx, vy):
        a = math.atan2(ux * vy - uy * vx, ux * vx + uy * vy)
        return a

    t1 = angle(1, 0, (x1p - cxp) / rx, (y1p - cyp) / ry)
    dt = angle((x1p - cxp) / rx, (y1p - cyp) / ry, (-x1p - cxp) / rx, (-y1p - cyp) / ry)
    if not sweep and dt > 0:
        dt -= 2 * math.pi
    elif sweep and dt < 0:
        dt += 2 * math.pi
    steps = max(4, int(abs(dt) / (math.pi / 16)))
    pts = []
    for i in range(1, steps + 1):
        t = t1 + dt * i / steps
        ex, ey = rx * math.cos(t), ry * math.sin(t)
        pts.append((cp * ex - sp * ey + cx, sp * ex + cp * ey + cy))
    return pts


def parse_path(d):
    tokens = re.findall(r"[MmLlHhVvCcSsQqTtAaZz]|-?(?:\d+\.?\d*|\.\d+)(?:e-?\d+)?", d)
    polylines, cur, i = [], [], 0
    x = y = sx = sy = 0.0
    last_ctrl = None
    cmd = None

    def num():
        nonlocal i
        v = float(tokens[i])
        i += 1
        return v

    def flag():
        # Arc flags may be packed ("a2 2 0 0 1 …" or "0 01"): a single token of "0"/"1" each.
        return num() != 0

    while i < len(tokens):
        if re.match(r"[A-Za-z]", tokens[i]):
            cmd = tokens[i]
            i += 1
            if cmd in "Zz":
                if cur:
                    cur.append((sx, sy))
                    polylines.append(cur)
                    cur = [(sx, sy)]
                x, y = sx, sy
                last_ctrl = None
                continue
        rel = cmd.islower()
        c = cmd.upper()
        ox, oy = (x, y) if rel else (0.0, 0.0)
        if c == "M":
            if len(cur) > 1:
                polylines.append(cur)
            x, y = num() + ox, num() + oy
            sx, sy = x, y
            cur = [(x, y)]
            cmd = "l" if rel else "L"  # further pairs are line-tos
            last_ctrl = None
        elif c == "L":
            x, y = num() + ox, num() + oy
            cur.append((x, y))
            last_ctrl = None
        elif c == "H":
            x = num() + (x if rel else 0)
            cur.append((x, y))
            last_ctrl = None
        elif c == "V":
            y = num() + (y if rel else 0)
            cur.append((x, y))
            last_ctrl = None
        elif c in "CS":
            if c == "C":
                c1 = (num() + ox, num() + oy)
            else:
                c1 = (2 * x - last_ctrl[0], 2 * y - last_ctrl[1]) if last_ctrl else (x, y)
            c2 = (num() + ox, num() + oy)
            end = (num() + ox, num() + oy)
            p0 = (x, y)
            for s in range(1, CURVE_STEPS + 1):
                t = s / CURVE_STEPS
                mt = 1 - t
                cur.append((
                    mt**3 * p0[0] + 3 * mt**2 * t * c1[0] + 3 * mt * t**2 * c2[0] + t**3 * end[0],
                    mt**3 * p0[1] + 3 * mt**2 * t * c1[1] + 3 * mt * t**2 * c2[1] + t**3 * end[1],
                ))
            last_ctrl = c2
            x, y = end
        elif c in "QT":
            if c == "Q":
                c1 = (num() + ox, num() + oy)
            else:
                c1 = (2 * x - last_ctrl[0], 2 * y - last_ctrl[1]) if last_ctrl else (x, y)
            end = (num() + ox, num() + oy)
            p0 = (x, y)
            for s in range(1, CURVE_STEPS + 1):
                t = s / CURVE_STEPS
                mt = 1 - t
                cur.append((mt**2 * p0[0] + 2 * mt * t * c1[0] + t**2 * end[0],
                            mt**2 * p0[1] + 2 * mt * t * c1[1] + t**2 * end[1]))
            last_ctrl = c1
            x, y = end
        elif c == "A":
            rx, ry, phi = num(), num(), num()
            large, sweep = flag(), flag()
            end = (num() + ox, num() + oy)
            cur.extend(arc_points(x, y, rx, ry, phi, large, sweep, *end))
            x, y = end
            last_ctrl = None
        else:
            raise ValueError(f"unsupported path command {cmd}")
    if len(cur) > 1:
        polylines.append(cur)
    return polylines


def rect(x, y, w, h, rx, ry):
    if rx == 0 and ry == 0:
        return [[(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)]]
    rx, ry = min(rx or ry, w / 2), min(ry or rx, h / 2)
    d = (f"M{x + rx} {y}H{x + w - rx}A{rx} {ry} 0 0 1 {x + w} {y + ry}V{y + h - ry}"
         f"A{rx} {ry} 0 0 1 {x + w - rx} {y + h}H{x + rx}A{rx} {ry} 0 0 1 {x} {y + h - ry}"
         f"V{y + ry}A{rx} {ry} 0 0 1 {x + rx} {y}Z")
    return parse_path(d)


def ellipse(cx, cy, rx, ry):
    n = 32
    pts = [(cx + rx * math.cos(2 * math.pi * k / n), cy + ry * math.sin(2 * math.pi * k / n)) for k in range(n + 1)]
    return [pts]


def flatten(svg_text):
    root = ET.fromstring(svg_text)
    out = []
    for el in root.iter():
        tag = el.tag.split("}")[-1]
        a = {k: v for k, v in el.attrib.items()}
        f = lambda k, default=0.0: float(a.get(k, default))
        if tag == "path":
            out += parse_path(a["d"])
        elif tag == "line":
            out.append([(f("x1"), f("y1")), (f("x2"), f("y2"))])
        elif tag == "rect":
            out += rect(f("x"), f("y"), f("width"), f("height"), f("rx"), f("ry"))
        elif tag == "circle":
            out += ellipse(f("cx"), f("cy"), f("r"), f("r"))
        elif tag == "ellipse":
            out += ellipse(f("cx"), f("cy"), f("rx"), f("ry"))
        elif tag in ("polyline", "polygon"):
            nums = [float(n) for n in re.findall(r"-?\d*\.?\d+", a["points"])]
            pts = list(zip(nums[::2], nums[1::2]))
            if tag == "polygon":
                pts.append(pts[0])
            out.append(pts)
    return out


def main():
    lines = [
        "//! Lucide icons (https://lucide.dev), lucide-static " + LUCIDE_VERSION + ", as stroke polylines in Lucide's 24x24",
        "//! space (stroke width 2, round caps and joins). GENERATED by tools/make_glyphs.py, do not edit.",
        "//!",
        "//! Copyright (c) 2026 Lucide Icons and Contributors. ISC License; see assets/licenses/lucide-LICENSE.txt.",
        "",
        "pub type Glyph = &'static [&'static [(f32, f32)]];",
        "",
    ]
    for const, name in ICONS.items():
        polys = flatten(fetch(f"{BASE}/icons/{name}.svg"))
        lines.append(f"/// Lucide `{name}`.")
        lines.append(f"pub const {const}: Glyph = &[")
        for p in polys:
            pts = ", ".join(f"({x:.3f}, {y:.3f})".replace(".000", ".0") for x, y in p)
            lines.append(f"    &[{pts}],")
        lines.append("];")
        lines.append("")
    (ROOT / "src" / "glyphs.rs").write_text("\n".join(lines), encoding="utf-8", newline="\n")
    lic = ROOT / "assets" / "licenses" / "lucide-LICENSE.txt"
    lic.parent.mkdir(parents=True, exist_ok=True)
    lic.write_text(fetch(f"{BASE}/LICENSE"), encoding="utf-8", newline="\n")
    print(f"wrote src/glyphs.rs ({len(ICONS)} glyphs, lucide-static {LUCIDE_VERSION}) and {lic.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
