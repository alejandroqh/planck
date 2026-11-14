#!/usr/bin/env python3
"""Generate every Planck logo and icon from extra/images/icon.svg.

Needs ImageMagick 7 (`magick`) with its librsvg SVG renderer.
Usage: python3 extra/generate-icons.py
"""

import struct
import subprocess
import tempfile
import xml.etree.ElementTree as ET
from pathlib import Path

EXTRA = Path(__file__).resolve().parent
ROOT = EXTRA.parent
SOURCE = EXTRA / "images" / "icon.svg"

FULL_BOX = "0 0 1024 1024"  # tile with the macOS margin
TILE_BOX = "100 100 824 824"  # tile edge to edge, for Windows
LINUX_BOX = "54 54 916 916"  # tile at 90%, the size Linux app icons fill
MARK_BOX = "224 226 576 576"  # the mark alone

ICO_SIZES = [16, 24, 32, 48, 64, 96, 128, 256]
# ICNS element types and their pixel sizes; the @2x types share sizes with others.
ICNS_TYPES = [
    (b"icp4", 16), (b"icp5", 32), (b"icp6", 64), (b"ic07", 128), (b"ic08", 256),
    (b"ic09", 512), (b"ic10", 1024), (b"ic11", 32), (b"ic12", 64), (b"ic13", 256),
    (b"ic14", 512),
]

SVG_NS = "http://www.w3.org/2000/svg"
ET.register_namespace("", SVG_NS)


def variant(box, size, mark_only=False):
    """The source SVG cropped to `box` and sized `size` px square."""
    root = ET.parse(SOURCE).getroot()
    root.set("viewBox", box)
    root.set("width", str(size))
    root.set("height", str(size))
    if mark_only:
        for child in list(root):
            if child.get("id") != "mark":
                root.remove(child)
        # The app tints the logo itself, so gradients become currentColor
        # (which falls back to black).
        mark = root.find(f"{{{SVG_NS}}}g[@id='mark']")
        mark.attrib.pop("color", None)
        for el in mark.iter():
            for key, value in el.attrib.items():
                if value.startswith("url("):
                    el.set(key, "currentColor")
    return ET.tostring(root, encoding="unicode") + "\n"


def render(box, size, out):
    with tempfile.NamedTemporaryFile("w", suffix=".svg") as svg:
        svg.write(variant(box, size))
        svg.flush()
        subprocess.run(["magick", "-background", "none", svg.name, "-depth", "8", out], check=True)


def write_icns(pngs, out):
    """Pack {size: png bytes} into an .icns file."""
    body = b"".join(
        kind + struct.pack(">I", 8 + len(pngs[size])) + pngs[size]
        for kind, size in ICNS_TYPES
    )
    data = b"icns" + struct.pack(">I", 8 + len(body)) + body
    out.write_bytes(data)

    # Self-check: walk the elements back.
    pos, kinds = 8, []
    while pos < len(data):
        length = struct.unpack(">I", data[pos + 4 : pos + 8])[0]
        assert data[pos + 8 : pos + 16] == b"\x89PNG\r\n\x1a\n"
        kinds.append(data[pos : pos + 4])
        pos += length
    assert pos == len(data) and kinds == [k for k, _ in ICNS_TYPES]


def main():
    logo = variant(MARK_BOX, 576, mark_only=True)
    for path in ["extra/images/logo.svg", "icons/planck/planck_logo.svg"]:
        (ROOT / path).write_text(logo)

    render(FULL_BOX, 1024, ROOT / "extra/images/logo.png")
    (ROOT / "extra/linux/io.github.alejandroqh.planck.svg").write_text(
        variant(LINUX_BOX, 256)
    )

    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)

        ico_pngs = []
        for size in ICO_SIZES:
            png = tmp / f"tile-{size}.png"
            render(TILE_BOX, size, png)
            ico_pngs.append(png)
        subprocess.run(
            ["magick", *ico_pngs, ROOT / "extra/windows/planck.ico"], check=True
        )

        icns_pngs = {}
        for size in sorted({size for _, size in ICNS_TYPES}):
            png = tmp / f"full-{size}.png"
            render(FULL_BOX, size, png)
            icns_pngs[size] = png.read_bytes()
        write_icns(
            icns_pngs, ROOT / "extra/macos/Planck.app/Contents/Resources/planck.icns"
        )

    print("Generated logo.svg, planck_logo.svg, logo.png, the Linux icon, planck.ico and planck.icns")


if __name__ == "__main__":
    main()
