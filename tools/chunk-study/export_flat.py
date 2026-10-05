"""Export XCF edit versions as PNG and WebP for the chunk study.

Runs inside GIMP's Python batch interpreter, after make_xcf_edits.py:

  XCF_OUT=out_dir gimp-console -i --quit \
    --batch-interpreter=python-fu-eval -b 'exec(open("export_flat.py").read())'

For each N.xcf in resave/ and small-stroke/, writes <dir>-png/N.png and
<dir>-webp/N.webp from the flattened image.
"""
import os

import gi

gi.require_version("Gimp", "3.0")
from gi.repository import Gimp, Gio  # noqa: E402

out = os.environ["XCF_OUT"]
for version in ("resave", "small-stroke"):
    for name in sorted(os.listdir(os.path.join(out, version))):
        stem = name[:-4]
        image = Gimp.file_load(Gimp.RunMode.NONINTERACTIVE, Gio.File.new_for_path(os.path.join(out, version, name)))
        image.flatten()
        for ext in ("png", "webp"):
            d = os.path.join(out, f"{version}-{ext}")
            os.makedirs(d, exist_ok=True)
            Gimp.file_save(Gimp.RunMode.NONINTERACTIVE, image, Gio.File.new_for_path(os.path.join(d, f"{stem}.{ext}")), None)
        image.delete()
        print(f"ok {version} {name}", flush=True)
