"""Render the ClubCraft app icon (1024 px PNG + macOS .icns + Windows .ico) into crates/app/assets.

Design: a dark rounded square holding a vinyl record whose grooves are four arcs in the stem colours
(drums, bass, melody, vocals), a pink label in the centre and a small play triangle.
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

STEMS = [(255, 176, 32), (235, 64, 80), (64, 200, 255), (120, 230, 110)]  # drums, bass, melody, vocals
PINK = (255, 92, 140)


def render(size: int) -> Image.Image:
    n = size * SS
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    # macOS-style squircle-ish rounded square with a subtle vertical gradient
    pad = int(n * 0.085)
    r = int(n * 0.2)
    bg = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    grad = Image.new("RGBA", (n, n))
    gd = ImageDraw.Draw(grad)
    for y in range(n):
        t = y / n
        c = (int(28 + 10 * (1 - t)), int(30 + 10 * (1 - t)), int(38 + 14 * (1 - t)), 255)
        gd.line([(0, y), (n, y)], fill=c)
    mask = Image.new("L", (n, n), 0)
    ImageDraw.Draw(mask).rounded_rectangle([pad, pad, n - pad, n - pad], radius=r, fill=255)
    bg.paste(grad, (0, 0), mask)
    # soft shadow
    shadow = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle([pad, pad + n * 0.012, n - pad, n - pad + n * 0.012], radius=r, fill=(0, 0, 0, 110))
    shadow = shadow.filter(ImageFilter.GaussianBlur(n * 0.015))
    img = Image.alpha_composite(img, shadow)
    img = Image.alpha_composite(img, bg)
    d = ImageDraw.Draw(img)

    cx = cy = n / 2
    rec = n * 0.33
    # record body
    d.ellipse([cx - rec, cy - rec, cx + rec, cy + rec], fill=(12, 13, 16, 255))
    # grooves: faint rings
    for k in range(10):
        rr = rec * (0.42 + 0.055 * k)
        d.ellipse([cx - rr, cy - rr, cx + rr, cy + rr], outline=(40, 42, 50, 255), width=max(1, int(n * 0.002)))
    # four stem arcs, each a ring segment, rotating outward
    w = int(n * 0.034)
    for i, col in enumerate(STEMS):
        rr = rec * (0.93 - 0.135 * i)
        start = -90 + i * 38
        sweep = 250 - i * 30
        d.arc([cx - rr, cy - rr, cx + rr, cy + rr], start=start, end=start + sweep, fill=col + (255,), width=w)
    # label
    lab = rec * 0.32
    d.ellipse([cx - lab, cy - lab, cx + lab, cy + lab], fill=PINK + (255,))
    # play triangle on the label
    tr = lab * 0.5
    pts = [(cx - tr * 0.45, cy - tr * 0.62), (cx - tr * 0.45, cy + tr * 0.62), (cx + tr * 0.62, cy)]
    d.polygon(pts, fill=(20, 20, 24, 255))
    return img.resize((size, size), Image.LANCZOS)


def main():
    os.makedirs(OUT, exist_ok=True)
    big = render(S)
    big.save(os.path.join(OUT, "icon-1024.png"))
    render(256).save(os.path.join(OUT, "icon-256.png"))
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
