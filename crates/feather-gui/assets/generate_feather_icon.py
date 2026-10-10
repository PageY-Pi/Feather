#!/usr/bin/env python3
"""Generate Feather app icon assets from one canonical vector description.

Outputs feather-logo.png, feather.ico, feather-logo.svg. Requires Pillow.
"""
from __future__ import annotations

from pathlib import Path
from PIL import Image, ImageDraw

OUT = Path(__file__).resolve().parent
SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
CANVAS = 1024
SS = 4

SAGE = "#78927f"
INK = "#26312d"
STEM = "#42584d"

# Canonical geometry in a 1024 viewBox.  The silhouette is solid: no cutouts.
# A broad, smooth feather runs lower-left to upper-right; the quill protrudes below the body.
SILHOUETTE = (
    "M 250 840 "
    "C 315 610 465 315 815 105 "
    "C 880 285 800 520 610 705 "
    "C 480 835 340 875 250 840 Z"
)
STEM_PATH = "M 185 945 C 300 730 480 505 815 105"
BARBS = [
    "M 430 545 C 520 505 615 435 705 335",
    "M 500 665 C 585 640 675 585 745 505",
    "M 345 760 C 430 715 520 690 610 680",
]

# The SVG and rasterizer below use the same path strings.

def svg_text() -> str:
    barb_paths = "\n".join(
        f'    <path d="{p}"/>' for p in BARBS
    )
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">
  <defs>
    <clipPath id="bodyClip">
      <path d="{SILHOUETTE}"/>
    </clipPath>
  </defs>
  <path d="{SILHOUETTE}" fill="{SAGE}" stroke="{INK}" stroke-width="44" stroke-linejoin="round"/>
  <g clip-path="url(#bodyClip)" fill="none" stroke="{INK}" stroke-width="32" stroke-linecap="round">
{barb_paths}
  </g>
  <path d="{STEM_PATH}" fill="none" stroke="{INK}" stroke-width="62" stroke-linecap="round"/>
  <path d="{STEM_PATH}" fill="none" stroke="{STEM}" stroke-width="34" stroke-linecap="round"/>
</svg>
'''

# Tiny cubic path parser for the limited M/C/Z commands above.
def points_from_path(path: str, samples: int = 48):
    toks = path.replace(",", " ").split()
    pts = []
    i = 0
    cur = None
    start = None
    while i < len(toks):
        cmd = toks[i]; i += 1
        if cmd == "M":
            cur = (float(toks[i]), float(toks[i+1])); i += 2
            start = cur
            pts.append(cur)
        elif cmd == "C":
            p0 = cur
            p1 = (float(toks[i]), float(toks[i+1]))
            p2 = (float(toks[i+2]), float(toks[i+3]))
            p3 = (float(toks[i+4]), float(toks[i+5])); i += 6
            for n in range(1, samples + 1):
                t = n / samples; u = 1 - t
                pts.append((
                    u**3*p0[0] + 3*u*u*t*p1[0] + 3*u*t*t*p2[0] + t**3*p3[0],
                    u**3*p0[1] + 3*u*u*t*p1[1] + 3*u*t*t*p2[1] + t**3*p3[1],
                ))
            cur = p3
        elif cmd == "Z":
            if start:
                pts.append(start)
        else:
            raise ValueError(f"unsupported path command {cmd!r}")
    return pts


def scaled(points, scale):
    return [(x * scale, y * scale) for x, y in points]


def hex_rgba(h: str):
    h = h.lstrip("#")
    return tuple(int(h[i:i+2], 16) for i in (0, 2, 4)) + (255,)


def icon(size: int) -> Image.Image:
    scale = size * SS / CANVAS
    W = size * SS
    img = Image.new("RGBA", (W, W), (0, 0, 0, 0))

    body_pts = scaled(points_from_path(SILHOUETTE), scale)
    stem_pts = scaled(points_from_path(STEM_PATH), scale)
    barb_pts = [scaled(points_from_path(p), scale) for p in BARBS]

    mask = Image.new("L", (W, W), 0)
    md = ImageDraw.Draw(mask)
    md.polygon(body_pts, fill=255)

    d = ImageDraw.Draw(img)
    d.polygon(body_pts, fill=hex_rgba(SAGE))
    outline_w = max(2, round(44 * size * SS / CANVAS))
    d.line(body_pts, fill=hex_rgba(INK), width=outline_w, joint="curve")

    # Internal dark barb lines stay clipped well inside the body and never alter the outer contour.
    if size >= 16:
        barb_layer = Image.new("RGBA", (W, W), (0, 0, 0, 0))
        bd = ImageDraw.Draw(barb_layer)
        count = 1 if size <= 20 else (2 if size < 32 else 3)
        barb_w = max(2, round(32 * size * SS / CANVAS))
        for pts in barb_pts[:count]:
            bd.line(pts, fill=hex_rgba(INK), width=barb_w)
            r = barb_w // 2
            for x, y in (pts[0], pts[-1]):
                bd.ellipse((x-r, y-r, x+r, y+r), fill=hex_rgba(INK))
        barb_layer.putalpha(Image.composite(barb_layer.getchannel("A"), Image.new("L", (W, W), 0), mask))
        img.alpha_composite(barb_layer)

    # Draw the continuous quill/stem last, with rounded caps. It clearly extends below the body.
    stem_outer = max(3, round(62 * size * SS / CANVAS))
    stem_inner = max(1, round(34 * size * SS / CANVAS))
    d.line(stem_pts, fill=hex_rgba(INK), width=stem_outer)
    d.line(stem_pts, fill=hex_rgba(STEM), width=stem_inner)
    r = stem_outer // 2
    for x, y in (stem_pts[0], stem_pts[-1]):
        d.ellipse((x-r, y-r, x+r, y+r), fill=hex_rgba(INK))
    r = stem_inner // 2
    for x, y in (stem_pts[0], stem_pts[-1]):
        d.ellipse((x-r, y-r, x+r, y+r), fill=hex_rgba(STEM))

    return img.resize((size, size), Image.Resampling.LANCZOS)


def main():
    (OUT / "feather-logo.svg").write_text(svg_text(), encoding="utf-8", newline="\n")
    icon(CANVAS).save(OUT / "feather-logo.png")
    frames = [icon(s) for s in SIZES]
    frames[-1].save(OUT / "feather.ico", sizes=[(s, s) for s in SIZES], append_images=frames[:-1])
    print("wrote feather-logo.png, feather.ico, feather-logo.svg")

if __name__ == "__main__":
    main()
