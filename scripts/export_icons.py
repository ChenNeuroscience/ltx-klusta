"""Export committed app icons from assets/ltxk.svg.

Development-only tools: Pillow, Node.js and @resvg/resvg-js (on NODE_PATH).
Release builds consume the exported assets and do not need these tools.
"""
from pathlib import Path
import subprocess

from PIL import Image


ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"


def export():
    subprocess.run(["node", "-e", """
const fs = require('node:fs');
const { Resvg } = require('@resvg/resvg-js');
const svg = fs.readFileSync(process.argv[1]);
const renderer = new Resvg(svg, { fitTo: { mode: 'width', value: 1024 } });
fs.writeFileSync(process.argv[2], renderer.render().asPng());
""", str(ASSETS / "ltxk.svg"), str(ASSETS / "ltxk.png")], check=True)
    with Image.open(ASSETS / "ltxk.png") as source:
        image = source.convert("RGBA")
    image.save(ASSETS / "ltxk.ico", sizes=[(size, size) for size in (16, 20, 24, 32, 40, 48, 64, 128, 256)])
    image.save(ASSETS / "ltxk.icns")
    windows = ASSETS / "windows"
    windows.mkdir(exist_ok=True)
    for name, size in (("StoreLogo", 50), ("Square44x44Logo", 44), ("Square150x150Logo", 150)):
        image.resize((size, size), Image.Resampling.LANCZOS).save(windows / f"{name}.png")
    print("Exported PNG, Windows ICO/MSIX logos and macOS ICNS from assets/ltxk.svg")


if __name__ == "__main__":
    export()
