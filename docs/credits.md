# credits.md — происхождение ассетов (этап 8 «Остров»)

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

## Текстуры сцены (этап 9)

Все текстуры сцены — ПРОЦЕДУРНЫЕ, генерируются скриптом
scripts/gen_textures.py (numpy + Pillow, seed 20261110 — детерминированы) в
assets/textures/*.png: grass/sand/rock/bark 256², leaf/palm/cloud/glow/tuft
RGBA-спрайты (премультиплицированные). Внешних источников нет — лицензия
совпадает с лицензией кода. В engines-загрузке читаются нативом
`texture-load-png` из `file-read-bytes`.

Историческая справка (этап 8): текстуры генерировались самим island.lisp
при загрузке (64² синусные заглушки).
Причина: ambientcg.com из песочницы сборки недоступен (TCP-connect не
устанавливается, проверено 4 раза); у GitHub-зеркала ambientCG
(fabien-michel/sweethome3d-textures-ambientcg, заявлен CC0-1.0) не удалось
скачать LICENSE.md для проверки — по правилу репозитория лицензия не
считается подтверждённой, поэтому непроверенные CC0-претензии не делаются.
Процедурные текстуры не требуют лицензий вообще.

## Ориджин
- License CC0-1.0: https://creativecommons.org/publicdomain/zero/1.0/legalcode
