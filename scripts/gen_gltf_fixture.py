#!/usr/bin/env python3
"""Генерирует синтетические glTF-фикстуры для smoke-тестов этапа 8.

  assets/tests/gltf/box_embedded.gltf — куб с ВСТРОЕННЫМ base64-буфером и
                                        встроенной PNG-текстурой (2x2: верх
                                        красный, низ зелёный);
  assets/tests/gltf/box_ext.gltf + box_ext.bin — тот же куб с ВНЕШНИМ .bin
                                        (file-read-bytes путь).

Куб: 8 вершин, 12 треугольников (36 индексов), POSITION+NORMAL+TEXCOORD_0,
два bufferView с byteOffset (в одном .bin), SCALAR/USHORT индексы.
"""
import base64
import json
import struct
from pathlib import Path

import PIL.Image as Image

OUT = Path("assets/tests/gltf")
OUT.mkdir(parents=True, exist_ok=True)

# вершины куба [-1,1] и индексы (CCW-наружу, как требует движок)
faces = [
    (0, 1, 2, 3, (0, 0, 1)),    # +Z: v0..v3 в порядке CCW
    (5, 4, 7, 6, (0, 0, -1)),   # -Z
    (1, 5, 6, 2, (1, 0, 0)),    # +X
    (4, 0, 3, 7, (-1, 0, 0)),   # -X
    (3, 2, 6, 7, (0, 1, 0)),    # +Y
    (4, 5, 1, 0, (0, -1, 0)),   # -Y
]
V = [(-1, -1, 1), (1, -1, 1), (1, 1, 1), (-1, 1, 1),
     (-1, -1, -1), (1, -1, -1), (1, 1, -1), (-1, 1, -1)]
positions, normals, uvs, indices = [], [], [], []
for a, b, c, d, n in faces:
    base = len(positions) // 3
    for vi in (a, b, c, d):
        positions += V[vi]
        normals += n
    uvs += [0, 0, 1, 0, 1, 1, 0, 1]
    indices += [base, base + 1, base + 2, base, base + 2, base + 3]

# 6 граней × 4 вершины × 3 компоненты = 72 f32; индексы 36 (12 треугольников)
assert len(positions) == 72 and len(indices) == 36

# текстура 2x2: строка 0 (v=0, верх) красная, строка 1 зелёная
img = Image.new("RGB", (2, 2))
img.putpixel((0, 0), (255, 0, 0)); img.putpixel((1, 0), (200, 0, 0))
img.putpixel((0, 1), (0, 255, 0)); img.putpixel((1, 1), (0, 160, 0))
import io
png_buf = io.BytesIO()
img.save(png_buf, "PNG")
png_bytes = png_buf.getvalue()

def bin_chunk(pos_uvs=False):
    b = b"".join(struct.pack("<f", x) for x in positions)
    b += b"".join(struct.pack("<f", x) for x in normals)
    b += b"".join(struct.pack("<f", x) for x in uvs)
    b += b"".join(struct.pack("<H", i) for i in indices)
    return b

bin_data = bin_chunk()

# ——— внешний .bin ———
(OUT / "box_ext.bin").write_bytes(bin_data)

def make_gltf(uri_buf, uri_img):
    # смещения bufferView считаются, а не вписываются руками
    o_pos = 0
    o_nrm = o_pos + len(positions) * 4
    o_uv = o_nrm + len(normals) * 4
    o_idx = o_uv + len(uvs) * 4
    return {
        "asset": {"version": "2.0", "generator": "rlisp fixture gen"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0}],
        "meshes": [{"primitives": [{
            "attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2},
            "indices": 3, "material": 0, "mode": 4}]}],
        "materials": [{
            "name": "redgreen",
            "baseColorFactor": [1.0, 1.0, 1.0, 1.0],
            "baseColorTexture": {"index": 0},
        }],
        "buffers": [{"uri": uri_buf, "byteLength": o_idx + len(indices) * 2}],
        "bufferViews": [
            {"buffer": 0, "byteOffset": o_pos, "byteLength": len(positions) * 4},
            {"buffer": 0, "byteOffset": o_nrm, "byteLength": len(normals) * 4},
            {"buffer": 0, "byteOffset": o_uv, "byteLength": len(uvs) * 4},
            {"buffer": 0, "byteOffset": o_idx, "byteLength": len(indices) * 2},
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 24, "type": "VEC3"},
            {"bufferView": 1, "componentType": 5126, "count": 24, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5126, "count": 24, "type": "VEC2"},
            {"bufferView": 3, "componentType": 5123, "count": 36, "type": "SCALAR"},
        ],
        "textures": [{"source": 0}],
        "images": [{"uri": uri_img}],
    }

# ——— встроенный вариант: буфер и PNG как data-URI base64 ———
b64bin = "data:application/octet-stream;base64," + base64.b64encode(bin_data).decode()
b64png = "data:image/png;base64," + base64.b64encode(png_bytes).decode()
(OUT / "box_embedded.gltf").write_text(
    json.dumps(make_gltf(b64bin, b64png), indent=1), encoding="utf-8")

# ——— внешний вариант: .bin + отдельный PNG-файл ———
(OUT / "box_tex.png").write_bytes(png_bytes)
(OUT / "box_ext.gltf").write_text(
    json.dumps(make_gltf("box_ext.bin", "box_tex.png"), indent=1), encoding="utf-8")

sizes = {p.name: p.stat().st_size for p in OUT.iterdir()}
print("фикстуры:", sizes)
assert sizes["box_embedded.gltf"] < 3500, "fixture должен остаться ~1-3KB"
print("ok")
