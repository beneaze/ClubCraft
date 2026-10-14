"""Render the ClubCraft app icon (1024 px PNG + macOS .icns + Windows .ico) into crates/app/assets.

Design: "CC" as two decks. Each C is a platter (a ring open to the right around a spindle dot),
with a crossfader underneath; white on a graphite rounded square.
Usage: python3 tools/make_icon.py
"""
import math
import os
import shutil
import subprocess
import tempfile

from PIL import Image, ImageDraw, ImageFilter

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "crates", "app", "assets")
S = 1024
SS = 4  # supersampling
WHITE = (242, 242, 240, 255)
FADER = (110, 112, 120, 255)
# geometry (fractions of the icon size), shared with the in-app mark (`widgets::logo_mark`)
DECK_X, DECK_Y, DECK_R, STROKE, GAP, DOT_R = (0.345, 0.655), 0.44, 0.15, 0.058, 78.0, 0.026
FADER_Y, FADER_X, FADER_W, KNOB_W, KNOB_H = 0.715, (0.28, 0.72), 0.018, 0.05, 0.085


def c_arc(d, cx, cy, r, w, gap, col, rot=0.0):
    """A "C": a ring open by `gap` degrees around angle `rot` (0 = right), with round ends."""
    a0, a1 = rot + gap / 2, rot + 360 - gap / 2
    d.arc([cx - r, cy - r, cx + r, cy + r], start=a0, end=a1, fill=col, width=int(w))
    for a in (a0, a1):
        rr = r - w / 2
        x, y = cx + rr * math.cos(math.radians(a)), cy + rr * math.sin(math.radians(a))
        d.ellipse([x - w / 2, y - w / 2, x + w / 2, y + w / 2], fill=col)


def render(size: int) -> Image.Image:
    n = size * SS
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    # macOS-style rounded square with a subtle vertical gradient and a soft shadow
    pad, r = int(n * 0.085), int(n * 0.2)
    grad = Image.new("RGBA", (n, n))
    gd = ImageDraw.Draw(grad)
    for y in range(n):
        t = y / n
        gd.line([(0, y), (n, y)], fill=(int(30 - 10 * t), int(31 - 10 * t), int(36 - 11 * t), 255))
    mask = Image.new("L", (n, n), 0)
    ImageDraw.Draw(mask).rounded_rectangle([pad, pad, n - pad, n - pad], radius=r, fill=255)
    shadow = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle([pad, pad + n * 0.012, n - pad, n - pad + n * 0.012], radius=r, fill=(0, 0, 0, 120))
    img = Image.alpha_composite(img, shadow.filter(ImageFilter.GaussianBlur(n * 0.015)))
    bg = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    bg.paste(grad, (0, 0), mask)
    img = Image.alpha_composite(img, bg)
    d = ImageDraw.Draw(img)
    y = n * DECK_Y
    for fx in DECK_X:
        x = n * fx
        c_arc(d, x, y, n * DECK_R, n * STROKE, GAP, WHITE)
        d.ellipse([x - n * DOT_R, y - n * DOT_R, x + n * DOT_R, y + n * DOT_R], fill=WHITE)
    yl, w = n * FADER_Y, n * FADER_W
    d.rounded_rectangle([n * FADER_X[0], yl - w / 2, n * FADER_X[1], yl + w / 2], radius=w / 2, fill=FADER)
    kw, kh = n * KNOB_W, n * KNOB_H
    d.rounded_rectangle([n / 2 - kw / 2, yl - kh / 2, n / 2 + kw / 2, yl + kh / 2], radius=n * 0.012, fill=WHITE)
    return img.resize((size, size), Image.LANCZOS)


def main():
    os.makedirs(OUT, exist_ok=True)
    big = render(S)
    big.save(os.path.join(OUT, "icon-1024.png"))
    render(256).save(os.path.join(OUT, "icon-256.png"))
    render(256).save(os.path.join(ROOT, "docs", "images", "icon.png"))
    # Windows .ico
    big.save(os.path.join(OUT, "icon.ico"), sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    # macOS .icns via iconutil
    if shutil.which("iconutil"):
        with tempfile.TemporaryDirectory() as tmp:
            iset = os.path.join(tmp, "ClubCraft.iconset")
            os.makedirs(iset)
            for base in [16, 32, 128, 256, 512]:
                for scale in [1, 2]:
                    px = base * scale
                    name = f"icon_{base}x{base}{'@2x' if scale == 2 else ''}.png"
                    big.resize((px, px), Image.LANCZOS).save(os.path.join(iset, name))
            subprocess.run(["iconutil", "-c", "icns", iset, "-o", os.path.join(OUT, "ClubCraft.icns")], check=True)
    print("icons written to", OUT)


if __name__ == "__main__":
    main()
