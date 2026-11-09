#!/usr/bin/env bash
# materials_proof.sh — визуальное доказательство этапа 7 «материалы».
# Сцена (земля + 100 кустов-билбордов) рендерится в 4 вариантах
# (scripts/materials_scene.lisp, один --eval): base / +alpha-test /
# +блик / весь материал через (use-material! …). PIL-сверка: каждый вариант
# ОБЯЗАН отличаться от базы; сквозь прорези alpha-test видно цвет неба.
# Коллаж — demo/stage7-materials.png.
#
# Запуск: bash scripts/materials_proof.sh  (нужен target/release/rlisp-engine)
set -euo pipefail
cd "$(dirname "$0")/.."
BIN="${BIN:-target/release/rlisp-engine}"
OUT=/tmp/stage7-proof
rm -rf "$OUT" && mkdir -p "$OUT"

"$BIN" --eval "$(cat scripts/materials_scene.lisp)"

# ——— PIL-сверка: варианты обязаны отличаться от базы ———
python3 - "$OUT" <<'PYEOF'
import sys
from PIL import Image

out = sys.argv[1]
names = ["1-base", "2-alpha", "3-spec", "4-full"]
imgs = {n: Image.open(f"{out}/{n}.png").convert("RGB") for n in names}
base = imgs["1-base"].load()
ok = True
for n in names[1:]:
    p = imgs[n].load()
    diff = sum(1 for y in range(imgs[n].height) for x in range(imgs[n].width)
               if base[x, y] != p[x, y])
    total = imgs[n].width * imgs[n].height
    pct = 100.0 * diff / total
    status = "ok" if diff > 1000 else "FAIL"
    if diff <= 1000:
        ok = False
    print(f"{status} - {n}: {diff}/{total} пикселей ({pct:.1f}%) отличаются от base")
# alpha-тест: сквозь прорези видно цвет неба (0.55 0.7 0.9 → 140 179 230)
sky = imgs["2-alpha"].load()
near_sky = sum(1 for y in range(imgs["2-alpha"].height) for x in range(imgs["2-alpha"].width)
               if abs(sky[x, y][0] - 140) < 8 and abs(sky[x, y][1] - 179) < 8 and abs(sky[x, y][2] - 230) < 12)
print(f"{'ok' if near_sky > 500 else 'FAIL'} - 2-alpha: {near_sky} пикселей неба сквозь прорези (alpha-test)")
if near_sky <= 500:
    ok = False
# коллаж 2x2
W, H = imgs["1-base"].size
collage = Image.new("RGB", (W * 2 + 12, H * 2 + 12), (24, 24, 28))
pos = [(4, 4), (W + 8, 4), (4, H + 8), (W + 8, H + 8)]
for n, xy in zip(names, pos):
    collage.paste(imgs[n], xy)
collage.save("demo/stage7-materials.png")
print("ok - коллаж сохранён в demo/stage7-materials.png")
sys.exit(0 if ok else 1)
PYEOF
echo "визуальное доказательство этапа 7 пройдено"
