"""Génère icon-src.png (1024x1024) et tray.png (44x44, template) sans dépendance."""
import struct, zlib, math, sys, os

def png(path, w, h, get_rgba):
    raw = bytearray()
    for y in range(h):
        raw.append(0)
        for x in range(w):
            raw.extend(get_rgba(x, y))
    def chunk(t, d):
        c = struct.pack(">I", len(d)) + t + d
        return c + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
    data = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
    data += chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(data)

def rounded_rect(x, y, cx, cy, hw, hh, r):
    dx = max(abs(x - cx) - (hw - r), 0)
    dy = max(abs(y - cy) - (hh - r), 0)
    return math.hypot(dx, dy) - r  # <0 inside

def bars_coverage(x, y, size, color_bg=None):
    """5 barres arrondies façon onde sonore, centrées."""
    heights = [0.22, 0.40, 0.60, 0.40, 0.22]
    bw = size * 0.085
    gap = size * 0.065
    total = 5 * bw + 4 * gap
    x0 = size / 2 - total / 2 + bw / 2
    d = 1e9
    for i, hfrac in enumerate(heights):
        cx = x0 + i * (bw + gap)
        d = min(d, rounded_rect(x, y, cx, size / 2, bw / 2, size * hfrac / 2, bw / 2))
    return d

def aa(dist_fn, x, y, ss=4):
    cov = 0
    for sy in range(ss):
        for sx in range(ss):
            if dist_fn(x + (sx + 0.5) / ss, y + (sy + 0.5) / ss) < 0:
                cov += 1
    return cov / (ss * ss)

def main(out):
    S = 1024
    def icon(x, y):
        bg = aa(lambda px, py: rounded_rect(px, py, S/2, S/2, S*0.45, S*0.45, S*0.2), x, y)
        # dégradé sombre
        t = y / S
        r, g, b = int(28 + 10*t), int(30 + 12*t), int(40 + 20*t)
        fg = aa(lambda px, py: bars_coverage(px, py, S), x, y)
        # blend bars (white) over bg
        cr = int(r * (1-fg) + 245 * fg); cg = int(g * (1-fg) + 245 * fg); cb = int(b * (1-fg) + 250 * fg)
        return (cr, cg, cb, int(255 * bg))
    png(os.path.join(out, "icon-src.png"), S, S, icon)
    T = 44
    def tray(x, y):
        fg = aa(lambda px, py: bars_coverage(px, py, T), x, y)
        return (0, 0, 0, int(255 * fg))
    png(os.path.join(out, "tray.png"), T, T, tray)

if __name__ == "__main__":
    main(sys.argv[1])
