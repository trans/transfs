"""Make XCF edit versions for the chunk study (whitepaper §6, gate 2).

Runs inside GIMP's Python batch interpreter:

  XCF_LIST=files.txt XCF_OUT=out_dir gimp-console -i \
    --batch-interpreter=python-fu-eval \
    --quit -b 'exec(open("make_xcf_edits.py").read())'

For each source file it saves:

  resave/N.xcf    the file opened and saved with no change except clearing any
                  saved selection, which would confine the paint (the baseline)
  resave2/N.xcf   resave/N.xcf opened and saved again (is saving deterministic?)
  <edit>/N.xcf    resave/N.xcf with one edit applied

The sources are only read.
"""
import os

import gi

gi.require_version("Gimp", "3.0")
gi.require_version("Gegl", "0.4")
from gi.repository import Gegl, Gimp, Gio  # noqa: E402


def load(path):
    return Gimp.file_load(Gimp.RunMode.NONINTERACTIVE, Gio.File.new_for_path(path))


def save(image, path):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    Gimp.file_save(Gimp.RunMode.NONINTERACTIVE, image, Gio.File.new_for_path(path), None)


def largest_layer(image):
    layers = [l for l in image.get_layers() if isinstance(l, Gimp.Layer) and not l.is_group()]
    return max(layers, key=lambda l: l.get_width() * l.get_height())


def stroke(drawable, points, size):
    # An unusual colour, so the paint differs from whatever it lands on.
    Gimp.context_set_foreground(Gegl.Color.new("#13a57c"))
    Gimp.context_set_brush_size(size)
    Gimp.paintbrush_default(drawable, points)


def small_stroke(image):
    layer = largest_layer(image)
    w, h = layer.get_width(), layer.get_height()
    stroke(layer, [w * 0.45, h * 0.45, w * 0.5, h * 0.5], 20)


def vertical_stroke(image):
    layer = largest_layer(image)
    w, h = layer.get_width(), layer.get_height()
    stroke(layer, [w * 0.5, 0, w * 0.5, h], 20)


def new_layer(image):
    w, h = image.get_width(), image.get_height()
    layer = Gimp.Layer.new(image, "added", w, h, Gimp.ImageType.RGBA_IMAGE, 100.0, Gimp.LayerMode.NORMAL)
    image.insert_layer(layer, None, 0)
    stroke(layer, [w * 0.2, h * 0.2, w * 0.8, h * 0.8], 40)


def move_layer(image):
    layer = largest_layer(image)
    _, x, y = layer.get_offsets()
    layer.set_offsets(x + 16, y + 16)


def colour_change(image):
    largest_layer(image).brightness_contrast(0.1, 0.0)


EDITS = {
    "small-stroke": small_stroke,
    "vertical-stroke": vertical_stroke,
    "new-layer": new_layer,
    "move-layer": move_layer,
    "colour-change": colour_change,
}

out = os.environ["XCF_OUT"]
sources = [line.strip() for line in open(os.environ["XCF_LIST"]) if line.strip()]
for n, src in enumerate(sources):
    name = f"{n:02d}.xcf"
    try:
        image = load(src)
        Gimp.Selection.none(image)
        save(image, os.path.join(out, "resave", name))
        image.delete()
        base = os.path.join(out, "resave", name)
        image = load(base)
        save(image, os.path.join(out, "resave2", name))
        image.delete()
        for edit, apply in EDITS.items():
            image = load(base)
            apply(image)
            save(image, os.path.join(out, edit, name))
            image.delete()
        print(f"ok {name} {src}", flush=True)
    except Exception as e:  # keep going; report the file
        print(f"FAILED {name} {src}: {e}", flush=True)
