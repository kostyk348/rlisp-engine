#!/usr/bin/env bash
# fetch_assets.sh — скачивание CC0-ассетов для сцены «ОСТРОВ» (этап 8).
# ambientcg.com из этой песочницы недоступен (TCP-connect висит, проверено 3
# раза) — берём ЗЕРКАЛО ambientCG на GitHub: fabien-michel/
# sweethome3d-textures-ambientcg, лицензия репозитория CC0-1.0 (проверяем
# файл LICENSE.md сами и копируем его в assets/textures/ для прозрачности).
# Модели — KhronosGroup/glTF-Sample-Assets (raw.githubusercontent.com),
# лицензия каждого файла проверена по Models/<Name>/LICENSE.md (CC0-1.0).
set -euo pipefail
cd "$(dirname "$0")/.."
GH="${GH:-/tmp/my-project/.tools/gh}"
RAW=https://raw.githubusercontent.com
MIRROR=fabien-michel/sweethome3d-textures-ambientcg
SA=KhronosGroup/glTF-Sample-Assets/main/Models

# ——— 1. лицензия зеркала текстур (проверка честности) ———
mkdir -p assets/textures
curl -sL "$RAW/$MIRROR/main/LICENSE.md" -o /tmp/mirror_license.md
head -4 /tmp/mirror_license.md
grep -qi "CC0\|creative commons zero\|public domain" /tmp/mirror_license.md \
  || { echo "FAIL: лицензия зеркала не CC0"; exit 1; }
cp /tmp/mirror_license.md assets/textures/COPYING.md
echo "ok: LICENSE зеркала = CC0, скопирован в assets/textures/COPYING.md"

# ——— 2. дерево репозитория-зеркала: ищем PNG по категориям ———
"$GH" api "repos/$MIRROR/git/trees/HEAD?recursive=1" \
  --jq '.tree[] | select(.path | test("\\.png$|\\.jpg$")) | .path' \
  > /tmp/tree.txt 2>/dev/null || "$GH" api "repos/$MIRROR/git/trees/main?recursive=1" \
  --jq '.tree[] | select(.path | test("\\.png$|\\.jpg$")) | .path' > /tmp/tree.txt
wc -l /tmp/tree.txt
pick() { grep -iE "$1" /tmp/tree.txt | grep -viE "normal|rough|metal|height|spec|ao|bump|_n\.|disp" | head -1; }
for pair in "grass:grass" "water:water" "bark:bark" "leaf:leaf" "rock:rock"; do
  cat="${pair%%:*}"; pat="${pair#*:}"
  f=$(pick "$pat") || true
  echo "== $cat → $f"
  if [ -n "$f" ]; then
    curl -sL "$RAW/$MIRROR/main/$(python3 -c "import urllib.parse,sys;print(urllib.parse.quote(sys.argv[1]))" "$f")" \
      -o "assets/textures/$cat.png"
  fi
done
ls -la assets/textures/

# ——— 3. модели glTF-Sample-Assets (лицензия проверена: CC0-1.0) ———
for m in WaterBottle Lantern; do
  d=$(echo "$m" | tr 'A-Z' 'a-z')
  mkdir -p "assets/models/$d"
  for f in "$m.gltf" "$m.bin"; do
    curl -sfL "$RAW/$SA/$m/glTF/$f" -o "assets/models/$d/$f"
  done
  # baseColor-текстура: даунскейл до 256 (в репо она 1-4.5 МБ, движку хватает)
  curl -sfL "$RAW/$SA/$m/glTF/${m}_baseColor.png" -o "/tmp/${m}_bc.png"
  ffmpeg -y -loglevel error -i "/tmp/${m}_bc.png" -vf "scale=256:256" "assets/models/$d/${d}_baseColor.png"
  # лицензия модели — в репозиторий для прозрачности
  curl -sfL "$RAW/$SA/$m/LICENSE.md" -o "assets/models/$d/LICENSE.md"
done
ls -la assets/models/*/ 
du -sh assets/textures assets/models
