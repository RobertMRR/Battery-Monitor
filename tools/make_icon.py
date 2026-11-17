"""Generates assets/app.ico: the exe's icon (a battery with a green level), at every size Windows asks for.

Run from the repo root:  python tools/make_icon.py
Pure Python (no dependencies): shapes are rendered with 4x4 supersampling and stored as PNG images inside the ICO.
"""
import struct
import zlib
from pathlib import Path

SIZES = [16, 20, 24, 32, 40, 48, 64, 256]
SS = 4  # supersampling per axis

RIM = (0xD0, 0xD7, 0xDE)    # light rim: visible on dark backgrounds
BODY = (0x24, 0x29, 0x2F)   # dark body: visible on light backgrounds
LEVEL = (0x3F, 0xB9, 0x50)  # same green as the tray icon's "good" level
LEVEL_FRACTION = 0.7


def rounded_rect(x0, y0, x1, y1, r):
    """Returns a point-in-shape test for a rounded rectangle in unit coordinates."""
    def inside(x, y):
        if not (x0 <= x <= x1 and y0 <= y <= y1):
            return False
        cx = min(max(x, x0 + r), x1 - r)
        cy = min(max(y, y0 + r), y1 - r)
        return (x - cx) ** 2 + (y - cy) ** 2 <= r * r
    return inside


def layers():
    body = (0.04, 0.22, 0.86, 0.78)
    rim_w = 0.065
    gap = 0.05
    inner = (body[0] + rim_w, body[1] + rim_w, body[2] - rim_w, body[3] - rim_w)
    lvl = (inner[0] + gap, inner[1] + gap, 0, inner[3] - gap)
    lvl = (lvl[0], lvl[1], lvl[0] + (inner[2] - gap - lvl[0]) * LEVEL_FRACTION, lvl[3])
    return [
        (rounded_rect(0.84, 0.38, 0.96, 0.62, 0.04), RIM),          # terminal nub
        (rounded_rect(*body, 0.12), RIM),
        (rounded_rect(*inner, 0.07), BODY),
        (rounded_rect(*lvl, 0.04), LEVEL),
    ]


def render(size):
    shapes = layers()
    rows = []
    for py in range(size):
        row = bytearray()
        for px in range(size):
            acc = [0.0, 0.0, 0.0, 0.0]  # premultiplied rgb + alpha
            for sy in range(SS):
                for sx in range(SS):
                    x = (px + (sx + 0.5) / SS) / size
                    y = (py + (sy + 0.5) / SS) / size
                    color = None
                    for test, c in shapes:  # later layers paint over earlier ones
                        if test(x, y):
                            color = c
                    if color:
                        acc[0] += color[0]
                        acc[1] += color[1]
                        acc[2] += color[2]
                        acc[3] += 1
            n = SS * SS
            a = acc[3] / n
            if a > 0:
                row += bytes([round(acc[0] / acc[3]), round(acc[1] / acc[3]), round(acc[2] / acc[3]), round(a * 255)])
            else:
                row += bytes(4)
        rows.append(bytes(row))
    return rows


def png(size, rows):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)
    raw = b"".join(b"\x00" + r for r in rows)  # filter type 0 per row
    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)  # 8-bit RGBA
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def dib(size, rows):
    """Classic ICO image: BITMAPINFOHEADER (double height), bottom-up 32-bit BGRA, then a 1-bit AND mask."""
    header = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    pixels = b"".join(
        b"".join(bytes([r[i + 2], r[i + 1], r[i], r[i + 3]]) for i in range(0, len(r), 4)) for r in reversed(rows)
    )
    mask_row = ((size + 31) // 32) * 4  # all zero: alpha channel decides transparency
    return header + pixels + bytes(mask_row * size)


def main():
    # Bitmaps for the small sizes (readable by every icon loader), PNG only for 256 px, as Windows' own icons do.
    images = [(s, png(s, render(s)) if s >= 256 else dib(s, render(s))) for s in SIZES]
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries = b""
    for s, data in images:
        dim = 0 if s >= 256 else s  # 0 means 256 in ICO directory entries
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    out = Path(__file__).resolve().parent.parent / "assets" / "app.ico"
    out.parent.mkdir(exist_ok=True)
    out.write_bytes(header + entries + b"".join(d for _, d in images))
    print(f"wrote {out} ({out.stat().st_size} bytes, sizes {SIZES})")


if __name__ == "__main__":
    main()
