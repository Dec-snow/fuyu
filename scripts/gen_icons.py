"""Generate the app / tray icons used by the Tauri bundle.

Run with:  python scripts/gen_icons.py
Output:    src-tauri/icons/*.png + icon.ico
"""

from __future__ import annotations

import os

from PIL import Image, ImageDraw

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "src-tauri", "icons")

# Indigo -> violet, matching the frontend accent gradient.
TOP = (99, 102, 241)
BOTTOM = (139, 92, 246)

SS = 4  # supersampling factor for smooth edges


def rounded_mask(size: int, radius_ratio: float) -> Image.Image:
    mask = Image.new("L", (size * SS, size * SS), 0)
    draw = ImageDraw.Draw(mask)
    draw.rounded_rectangle(
        (0, 0, size * SS - 1, size * SS - 1),
        radius=int(size * SS * radius_ratio),
        fill=255,
    )
    return mask.resize((size, size), Image.LANCZOS)


def gradient(size: int) -> Image.Image:
    img = Image.new("RGB", (1, size))
    px = img.load()
    for y in range(size):
        t = y / max(size - 1, 1)
        px[0, y] = (
            round(TOP[0] + (BOTTOM[0] - TOP[0]) * t),
            round(TOP[1] + (BOTTOM[1] - TOP[1]) * t),
            round(TOP[2] + (BOTTOM[2] - TOP[2]) * t),
        )
    return img.resize((size, size), Image.NEAREST)


def chat_bubble(size: int) -> Image.Image:
    """White rounded speech bubble with three dots, drawn at 4x then scaled."""
    s = size * SS
    layer = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    draw = ImageDraw.Draw(layer)

    w, h = int(s * 0.66), int(s * 0.54)
    x0, y0 = (s - w) // 2, int(s * 0.20)
    draw.rounded_rectangle(
        (x0, y0, x0 + w, y0 + h), radius=int(s * 0.16), fill=(255, 255, 255, 255)
    )

    # Little tail at the bottom-left of the bubble.
    t = int(s * 0.13)
    draw.polygon(
        [
            (x0 + int(w * 0.16), y0 + h - int(s * 0.02)),
            (x0 + int(w * 0.16) + t, y0 + h - int(s * 0.02)),
            (x0 + int(w * 0.12), y0 + h + int(s * 0.15)),
        ],
        fill=(255, 255, 255, 255),
    )

    # Three accent dots.
    dot = int(s * 0.072)
    gap = int(s * 0.105)
    cy = y0 + h // 2
    cx = x0 + w // 2
    for i in (-1, 0, 1):
        dx = cx + i * gap
        draw.ellipse(
            (dx - dot // 2, cy - dot // 2, dx + dot // 2, cy + dot // 2),
            fill=(109, 92, 231, 255),
        )

    return layer.resize((size, size), Image.LANCZOS)


def app_icon(size: int) -> Image.Image:
    base = gradient(size).convert("RGBA")
    base.putalpha(rounded_mask(size, 0.22))
    base.alpha_composite(chat_bubble(size))
    return base


def tray_icon(size: int) -> Image.Image:
    """Tray icons want a transparent background, not a filled tile."""
    layer = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    bubble = chat_bubble(size)
    # Recolour the white bubble to the accent so it reads on a light taskbar.
    px = bubble.load()
    for y in range(size):
        for x in range(size):
            r, g, b, a = px[x, y]
            if a and r > 200 and g > 200 and b > 200:
                px[x, y] = (79, 70, 229, a)
    layer.alpha_composite(bubble)
    return layer


def main() -> None:
    os.makedirs(OUT, exist_ok=True)

    for size, name in ((32, "32x32.png"), (128, "128x128.png"), (256, "128x128@2x.png")):
        app_icon(size).save(os.path.join(OUT, name))
    app_icon(512).save(os.path.join(OUT, "icon.png"))

    tray_icon(32).save(os.path.join(OUT, "tray.png"))

    ico_sizes = [16, 24, 32, 48, 64, 128, 256]
    app_icon(256).save(
        os.path.join(OUT, "icon.ico"),
        format="ICO",
        sizes=[(s, s) for s in ico_sizes],
    )

    for f in sorted(os.listdir(OUT)):
        print(f, os.path.getsize(os.path.join(OUT, f)))


if __name__ == "__main__":
    main()
