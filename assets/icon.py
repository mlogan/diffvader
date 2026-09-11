#!/usr/bin/env python3
"""Generates assets/icon.png (512x512): a dark rounded square holding two code panes with
red and green changed lines. Pure Python so the repo needs no image tooling."""
import struct, zlib, math, sys

OUT = 512          # output size
SS = 2             # supersampling factor
N = OUT * SS

def rrect_cov(x, y, x0, y0, x1, y1, r):
    """1 inside a rounded rect, 0 outside (hard edge; AA comes from supersampling)."""
    if x < x0 or x >= x1 or y < y0 or y >= y1:
        return 0.0
    cx = min(max(x, x0 + r), x1 - r)
    cy = min(max(y, y0 + r), y1 - r)
    dx, dy = x - cx, y - cy
    return 1.0 if dx * dx + dy * dy <= r * r else 0.0

def lerp(a, b, t):
    return tuple(a[i] + (b[i] - a[i]) * t for i in range(3))

# Geometry in unit coordinates (0..1), scaled to N.
S = N
outer = (0.06 * S, 0.06 * S, 0.94 * S, 0.94 * S, 0.20 * S)   # squircle-ish app tile
pane_y0, pane_y1 = 0.24 * S, 0.76 * S
rows = 7
row_pitch = (pane_y1 - pane_y0) / rows
bar_h = row_pitch * 0.55
left = (0.16 * S, 0.47 * S)
right = (0.53 * S, 0.84 * S)
# (row, start frac, end frac, color key) per pane
GRAY = (118, 126, 142)
RED = (242, 92, 108)
GREEN = (78, 210, 144)
BLUE = (138, 180, 248)
left_rows = [(0, 0.0, 0.75, GRAY), (1, 0.12, 0.95, RED), (2, 0.12, 0.6, RED), (3, 0.0, 0.5, GRAY),
             (4, 0.12, 0.85, GRAY), (5, 0.12, 0.55, BLUE), (6, 0.0, 0.3, GRAY)]
right_rows = [(0, 0.0, 0.75, GRAY), (1, 0.12, 0.9, GREEN), (2, 0.12, 0.7, GREEN), (3, 0.12, 0.45, GREEN),
              (4, 0.0, 0.5, GRAY), (5, 0.12, 0.55, BLUE), (6, 0.0, 0.3, GRAY)]

bars = []
for pane, spec in ((left, left_rows), (right, right_rows)):
    for row, a, b, col in spec:
        y0 = pane_y0 + row * row_pitch + (row_pitch - bar_h) / 2
        x0 = pane[0] + a * (pane[1] - pane[0])
        x1 = pane[0] + b * (pane[1] - pane[0])
        bars.append((x0, y0, x1, y0 + bar_h, bar_h / 2, col))
divider = (0.495 * S, pane_y0, 0.505 * S, pane_y1, 0.005 * S, (58, 64, 80))
bars.append(divider)

BG_TOP = (38, 41, 52)
BG_BOT = (20, 22, 28)
EDGE = (70, 76, 92)

px = bytearray(N * N * 4)
for y in range(N):
    t = y / N
    bg = lerp(BG_TOP, BG_BOT, t)
    for x in range(N):
        cov = rrect_cov(x + 0.5, y + 0.5, *outer)
        if cov == 0.0:
            continue
        # Subtle lighter rim just inside the edge.
        inner = rrect_cov(x + 0.5, y + 0.5, outer[0] + 3, outer[1] + 3, outer[2] - 3, outer[3] - 3, outer[4] - 3)
        c = bg if inner else lerp(bg, EDGE, 0.7)
        for (x0, y0, x1, y1, r, col) in bars:
            if x0 - 1 <= x <= x1 + 1 and y0 - 1 <= y <= y1 + 1 and rrect_cov(x + 0.5, y + 0.5, x0, y0, x1, y1, r):
                c = col
                break
        i = (y * N + x) * 4
        px[i] = int(c[0]); px[i + 1] = int(c[1]); px[i + 2] = int(c[2]); px[i + 3] = 255

# Downsample SSxSS -> 1 (box filter, premultiplied so the rounded edge stays clean).
out = bytearray(OUT * OUT * 4)
for y in range(OUT):
    for x in range(OUT):
        r = g = b = a = 0
        for dy in range(SS):
            for dx in range(SS):
                i = ((y * SS + dy) * N + (x * SS + dx)) * 4
                al = px[i + 3]
                r += px[i] * al; g += px[i + 1] * al; b += px[i + 2] * al; a += al
        o = (y * OUT + x) * 4
        if a:
            out[o] = r // a; out[o + 1] = g // a; out[o + 2] = b // a
        out[o + 3] = a // (SS * SS)

def chunk(tag, data):
    c = struct.pack(">I", len(data)) + tag + data
    return c + struct.pack(">I", zlib.crc32(tag + data) & 0xffffffff)

raw = b"".join(b"\x00" + bytes(out[y * OUT * 4:(y + 1) * OUT * 4]) for y in range(OUT))
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", OUT, OUT, 8, 6, 0, 0, 0)) \
    + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
path = sys.argv[1] if len(sys.argv) > 1 else "icon.png"
open(path, "wb").write(png)
print(f"wrote {path} ({len(png)} bytes)")
