# App icon

`ltxk.svg` is the editable source. Its three cluster shapes use the exact blue,
green and red gradients, white outlines and charcoal (`#18181b`) line style
from the Waveform favicon (`img/icon.svg` in the waveform project). The
background is transparent.

The exported PNG, multi-size Windows ICO, macOS ICNS and Windows package logos
are committed so release builds need no image-processing tools. Both Windows
executables embed the ICO; the Mac app uses the ICNS. The main README uses the
SVG directly, including in the downloadable packages.

To regenerate the exports, install Pillow and make Node.js plus
`@resvg/resvg-js` available, then run `python3 scripts/export_icons.py` from the
repository root. `NODE_PATH` can point to an external tools directory containing
the renderer, so these development dependencies do not enter the app package.
