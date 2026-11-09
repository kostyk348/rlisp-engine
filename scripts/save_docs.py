#!/usr/bin/env python3
"""Этап 8 (M4): документы — credits.md, demo_island.sh, CHANGELOG,
LIMITATIONS, README×2. Идемпотентно: защита от дублей разделов."""
import os

os.chdir(os.path.join(os.path.dirname(__file__), ".."))

# ——— docs/credits.md ———
credits = """# credits.md — происхождение ассетов (этап 8 «Остров»)

Правило репозитория: лицензия считается проверенной только по файлу
LICENSE/README первоисточника, скачанному в этой же сборке. Ниже — только
то, что проверено; остальное честно помечено.

## Модели (glTF, из интернета)

| Ассет | Источник | Лицензия (проверено по файлу) | Файл-доказательство |
|---|---|---|---|
| WaterBottle | KhronosGroup/glTF-Sample-Assets, Models/WaterBottle (raw.githubusercontent.com) | CC0-1.0 | assets/models/waterbottle/LICENSE.md |
| Lantern | KhronosGroup/glTF-Sample-Assets, Models/Lantern | CC0-1.0 | assets/models/lantern/LICENSE.md |

- Из каждой модели взяты только .gltf + .bin + baseColor PNG (даунскейл до
  256px); normal/roughness/metallic текстуры не используются движком
  (PBR вне подмножества) и в репозиторий не включены.
- Duck (SCEA) и Box/BoxTextured (CC-BY) проверены и ОТКЛОНЕНЫ: первый не
  CC0/CC-BY, вторые требуют атрибуции — предпочтение отдано CC0.

## Текстуры сцены

Все текстуры сцены (трава, вода, кора, листва, камень) — ПРОЦЕДУРНЫЕ,
генерируются assets/lisp/island.lisp при загрузке (как в ORBITAL v0.3).
Причина: ambientcg.com из песочницы сборки недоступен (TCP-connect не
устанавливается, проверено 4 раза); у GitHub-зеркала ambientCG
(fabien-michel/sweethome3d-textures-ambientcg, заявлен CC0-1.0) не удалось
скачать LICENSE.md для проверки — по правилу репозитория лицензия не
считается подтверждённой, поэтому непроверенные CC0-претензии не делаются.
Процедурные текстуры не требуют лицензий вообще.

## Ориджин
- License CC0-1.0: https://creativecommons.org/publicdomain/zero/1.0/legalcode
"""
open("docs/credits.md", "w", encoding="utf-8").write(credits)

# ——— scripts/demo_island.sh ———
demo_lines = [
    "#!/usr/bin/env bash",
    "# demo_island.sh — демо «ОСТРОВ» (этап 8): сборка, захват кадров, GIF.",
    "# Запуск: bash scripts/demo_island.sh",
    "set -euo pipefail",
    'cd "$(dirname "$0")/.."',
    'export PATH="$HOME/.local/rust/bin:$PATH"',
    "BIN=target/release/rlisp-engine",
    '[ -x "$BIN" ] || cargo build --release',
    "rm -rf demo/frames_island && mkdir -p demo/frames_island",
    '"$BIN" --script assets/lisp/main_island.lisp \\',
    "       --capture demo/frames_island --every 6 --frames 240 --fast \\",
    '       2>&1 | tee demo/run_island.log | grep -E "ISLAND fps" || true',
    "python3 /home/z/my-project/work/portfolio/rlisp-engine/scripts/collect_island_demo.py",
    "rm -rf demo/frames_island",
    'echo "[demo] готово: demo/island.gif + filmstrip + стиллы + run_island.log"',
]
open("scripts/demo_island.sh", "w", encoding="utf-8").write("\n".join(demo_lines) + "\n")

collect = """#!/usr/bin/env python3
\"\"\"Собирает island.gif / filmstrip / стиллы из demo/frames_island + PIL-сверка.\"\"\"
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
"""
open("scripts/collect_island_demo.py", "w", encoding="utf-8").write(collect)

# ——— CHANGELOG ———
ch = open("CHANGELOG.md", encoding="utf-8").read()
marker = "## [Unreleased]"
add = """## [Unreleased]
### Этап 8: демо «Остров» (ветка stage8/island)
- glTF-пайплайн: нативы json-parse, base64-decode (RFC 4648 + data-URI),
  file-read-bytes, texture-load-png (ноль зависимостей, юнит-тесты);
  загрузчик assets/lisp/load-gltf.lisp (уровень 3) — подмножество glTF 2.0
  ASCII: сцены/узлы (matrix/TRS), POSITION/NORMAL/TEXCOORD_0, индексы
  FLOAT/USHORT/UINT/UBYTE, bufferView+byteStride, буферы .bin/data-URI,
  PNG-текстуры файл/data-URI, IEEE-754 на Lisp, кэш по пути; fixture
  assets/tests/gltf + 11 smoke-проверок; CC0-модели WaterBottle и Lantern
  (KhronosGroup/glTF-Sample-Assets, лицензии проверены по LICENSE.md)
- сцена assets/lisp/island.lisp + main_island.lisp: вода с волнами (3
  синуса, обновление каждый 2-й кадр), высотное поле острова, 16 деревьев
  (батч стволов + кроны alpha-test 0.5), камни-батч, тёплый свет, туман,
  орбита камеры, честный FPS-замер (headless 320x240 --fast: 45.9 fps)
- демо-артефакты: demo/island.gif, island_filmstrip.png, стиллы,
  run_island.log; docs/credits.md — происхождение ассетов

"""
if add not in ch:
    ch = ch.replace(marker, add + marker, 1) if marker in ch else add + ch
    open("CHANGELOG.md", "w", encoding="utf-8").write(ch)
    print("CHANGELOG: ok")

# ——— LIMITATIONS ———
lim = open("LIMITATIONS.md", encoding="utf-8").read()
if "Этап 8 «Остров»" not in lim:
    lim += """

## Этап 8 «Остров» (glTF-пайплайн)

- Поддержано ПОДМНОЖЕСТВО glTF 2.0 (ASCII): сцены/узлы с matrix/TRS,
  POSITION/NORMAL/TEXCOORD_0, индексы FLOAT/USHORT/UINT/UBYTE,
  bufferView+byteStride, буферы .bin или data-URI base64, PNG-текстуры
  (файл или data-URI), материалы — только baseColorFactor/baseColorTexture.
- НЕТ: .glb-контейнер, skinning/анимации, камеры/света glTF, PBR-текстур,
  изображений из bufferView.
- Нормали читаются, но шейдинг движка FLAT (per-face) — per-vertex N
  конвейер не использует.
- Alpha-test есть, alpha-BLENDING нет: вода непрозрачна. Туман opaque-only.
- Волны воды обновляются каждый 2-й кадр — сознательный компромисс: полный
  цикл вершин на Lisp каждый кадр съедал бюджет (с компромиссом headless
  320x240 --fast ≈ 46 fps).
- Текстуры сцены процедурные: ambientcg.com из песочницы сборки
  недоступен, лицензию зеркала проверить не удалось (docs/credits.md).
"""
    open("LIMITATIONS.md", "w", encoding="utf-8").write(lim)
    print("LIMITATIONS: ok")

# ——— README.md ———
sec_ru = """
## Демо: Остров (этап 8, ветка stage8/island)

Сцена assets/lisp/island.lisp — вода с волнами (3 синуса, материал с
бликом), высотное поле острова, 16 деревьев с alpha-test кронами, камни,
тёплый свет и туман цвета неба, glTF-модели WaterBottle и Lantern (CC0-1.0,
KhronosGroup/glTF-Sample-Assets). Загрузчик моделей — load-gltf.lisp
(подмножество glTF 2.0 ASCII; нативы json-parse / base64-decode /
file-read-bytes / texture-load-png, ноль зависимостей). Headless-замер:
45.9 fps на 320x240 (--fast). Артефакты: demo/island.gif,
demo/island_filmstrip.png. Происхождение ассетов и лицензии —
docs/credits.md, ограничения — LIMITATIONS.md.
"""
r = open("README.md", encoding="utf-8").read()
if "## Демо: Остров" not in r:
    open("README.md", "w", encoding="utf-8").write(r.rstrip() + "\n" + sec_ru)
    print("README.md: ok")

sec_en = """
## Demo: Island (stage 8, branch stage8/island)

assets/lisp/island.lisp — animated water (3 sine waves, specular material),
radial heightfield island, 16 trees with alpha-tested canopies, rocks, warm
light and sky-colored fog, plus glTF models WaterBottle and Lantern
(CC0-1.0, KhronosGroup/glTF-Sample-Assets). The model pipeline is
load-gltf.lisp (a subset of glTF 2.0 ASCII on top of zero-dependency
natives json-parse / base64-decode / file-read-bytes / texture-load-png).
Headless measurement: 45.9 fps at 320x240 (--fast). Artifacts:
demo/island.gif, demo/island_filmstrip.png. Asset provenance and licenses:
docs/credits.md; subset limitations: LIMITATIONS.md.
"""
r = open("README.en.md", encoding="utf-8").read()
if "## Demo: Island" not in r:
    open("README.en.md", "w", encoding="utf-8").write(r.rstrip() + "\n" + sec_en)
    print("README.en.md: ok")

print("done")
