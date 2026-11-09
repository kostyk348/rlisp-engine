#!/usr/bin/env python3
"""Собирает island.gif / filmstrip / стиллы из demo/frames_island + PIL-сверка."""
import glob
import os

from PIL import Image

fs = sorted(glob.glob("demo/frames_island/frame_*.png"))
assert fs, "нет кадров demo/frames_island"
frames = [Image.open(f).convert("RGB") for f in fs]
sub = frames[::2]
sub[0].save("demo/island.gif", save_all=True, append_images=sub[1:],
            duration=120, loop=0, optimize=True)
w, h = frames[0].size
strip = Image.new("RGB", (w * 3, h * 3), (16, 16, 24))
for k, i in enumerate([min(int(len(frames) * j / 9), len(frames) - 1) for j in range(9)]):
    strip.paste(frames[i], ((k % 3) * w, (k // 3) * h))
strip.save("demo/island_filmstrip.png")
frames[len(frames) // 3].save("demo/island_still1.png")
frames[2 * len(frames) // 3].save("demo/island_still2.png")
for name in ("demo/island.gif", "demo/island_filmstrip.png"):
    im = Image.open(name).convert("L")
    hh = im.histogram()
    r = 1 - hh[255] / sum(hh)
    assert r > 0.05, name + " выглядит пустым (non-white %.3f)" % r
    print("ok - %s: non-white %.3f, %d bytes" % (name, r, os.path.getsize(name)))
