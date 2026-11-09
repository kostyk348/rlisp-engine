# rlisp-engine

**Игровой движок как Lisp-машина: Rust только как HAL, всё остальное — DSL.**

Прототип по [ТЗ v1.0](#): движок не содержит игровых систем — он содержит только
доступ к «железу». Рендерер, физика, ECS, ввод, текстуры, редактор, MCP —
Lisp-библиотеки поверх тонкого Rust-слоя.

**v0.3 — текстуры, батч-вершинник, редактор аля Unity, AI Ready/MCP:**

- **Текстуры**: PNG-декодер без зависимостей (полный inflate: stored/fixed/dynamic
  Huffman), процедурные текстуры на Lisp (canvas на F32Vec), перспектива-корректный
  UV-семплинг (интерполяция `u/w` и `1/w`, клиппинг атрибутов вместе с вершиной);
- **Батч-вершинник**: сотни объектов одним draw call — 12 f32 на инстанс
  (translate/scale/yaw/pitch/roll/tint), T&L всех инстансов в узком Rust-цикле —
  **~3× ускорение** отрисовки против по-объектных вызовов;
- **Редактор аля Unity** (`--serve`): иерархия сцены, инспектор свойств, живой
  вид (PNG-стрим кадра), REPL, редактор кода ассетов с hot reload — весь UI
  общается с движком через Lisp (`editor.lisp`);
- **AI Ready / MCP** (`--mcp`): Model Context Protocol (JSON-RPC 2.0, stdio) —
  ИИ-агент видит сцену, спавнит сущности, правит свойства, исполняет Lisp,
  шагает симуляцию и **получает скриншоты** движка.

```
┌──────────────────────────────────────────────────────────────────────┐
│  УРОВЕНЬ 4: Игровые DSL (жанровые)                                   │
│  (defgame survival …) (defenemy grunt …)       «ORBITAL» (game3d.lisp)│
├──────────────────────────────────────────────────────────────────────┤
│  УРОВЕНЬ 3: Системные DSL — всё на Lisp (assets/lisp/)               │
│  graphics │ input │ ecs │ physics │ render3d │ textures │ editor     │
├──────────────────────────────────────────────────────────────────────┤
│  УРОВЕНЬ 2: Lisp-интерпретатор + SAL (src/interp.rs, src/simd.rs)    │
│  Reader │ Evaluator (TCO) │ defmacro/quasiquote │ F32Vec │ GC=Rc     │
├──────────────────────────────────────────────────────────────────────┤
│  УРОВЕНЬ 1: Rust HAL (src/hal.rs, render3d.rs, png.rs, editor.rs,    │
│  mcp.rs, json.rs) — окно/растр │ 3D-«GPU»: T&L+текстуры+батчи+       │
│  z-буфер │ ввод │ память │ время │ PNG-кодек │ HTTP │ MCP            │
└──────────────────────────────────────────────────────────────────────┘
```

## Быстрый старт

```bash
cargo build --release

# 3D-демо «ORBITAL» (без экрана — кадры PNG):
./target/release/rlisp-engine --script assets/lisp/main3d.lisp \
    --capture demo/frames3d --every 12 --frames 240 --fast

# играть в 3D в терминале (ANSI, wasd, q — выход):
./target/release/rlisp-engine --script assets/lisp/main3d.lisp --ascii --stdin-keys

# РЕДАКТОР аля Unity (браузер): иерархия · инспектор · вид · REPL · ассеты
./target/release/rlisp-engine --serve --port 7300 --script assets/lisp/main3d.lisp
#   → открой http://127.0.0.1:7300
bash scripts/demo_editor.sh          # то же, одной командой

# AI READY: MCP-сервер (JSON-RPC 2.0 поверх stdio) — подключи любого ИИ-агента
./target/release/rlisp-engine --mcp --script assets/lisp/main3d.lisp

# 2D-демо «Выживание»:
./target/release/rlisp-engine --ascii --stdin-keys

# бенчмарк SIMD-слоя: ./target/release/rlisp-engine --bench
# проверки (TCO, макросы, текстуры, батчи, PNG-кодек): bash scripts/smoke.sh

# ДЕМО HOT RELOAD: на 9-й секунде правит game3d.lisp на лету:
bash scripts/demo3d.sh   # → demo/orbital.gif + demo/filmstrip3d.png
```

## Рендер-бэкенды (этап 1 роадмапа)

За фасадом HAL — два бэкенда с одним профилем API (`gpu-clear` /
`gpu-fill-rect` / `gpu-blit` / `gpu-blit-tex` / `gpu-present`), Lisp-код
один и тот же:

| Бэкенд | Как включить | Что делает |
|---|---|---|
| **программный растр** (дефолт) | ничего; ноль зависимостей | команды пишут прямо в фреймбуфер |
| **wgpu** | `cargo build --release --features wgpu` + флаг `--wgpu` | offscreen-рендер RGBA8 (WGSL-шейдер, nearest, discard alpha=0) → map-read → композит в фреймбуфер; без адаптера — предупреждение и откат на софт |

Бенчмарк кадра: `--bench` (софт) и `--bench --wgpu` (GPU, разбивка
submit/readback). Честно: wgpu закрывает 2D-растр, 3D-конвейер (T&L,
z-буфер, батчи) пока программный; критерий «wgpu кратно быстрее» требует
GPU-железа — `docs/benchmark.md` содержит только реальные замеры с метками
окружения. Подробности — `docs/architecture.md`, роадмап — `docs/roadmap.md`.

## Демо

3D «ORBITAL» (v0.3): пол — процедурная текстура «неоновая схема» (один
текстурный draw call вместо сотен линий), враги/игрок — текстура «панель» с
тинтом, враги/пули/пилоны рисуются **батчами** (по одному draw call на группу).
На t=9с `game3d.lisp` правится на диске: враги становятся оранжевыми и вдвое
быстрее — без перезапуска (`demo/run3d.log`, «12 persistent-значений
восстановлено»).

![orbital](demo/orbital.gif)

2D «Выживание» (v0.1): ![survival](demo/survival.gif)

Весь игровой код — Lisp (`assets/lisp/game.lisp`, `assets/lisp/game3d.lisp`):
игрок, волны, автоприцел, коллизии, счёт, GAME OVER + рестарт на `R`.

## Редактор аля Unity (--serve)

Ноль зависимостей: HTTP-сервер на `std::net`, страница редактора зашита в
бинарь (`assets/editor/index.html`). Игровая нить публикует кадр/сцену/лог в
мост (`src/editor.rs`), браузер опрашивает `/api/state` и `/frame.png`.

| Панель | Что умеет |
|---|---|
| **Иерархия** | живой список сущностей сцены — игрок, враги, пули, эффекты; клик — выбор |
| **Инспектор** | свойства выбранной сущности; «Применить» исполняет `(table-set! REF 'prop value)` в живом мире |
| **Вид** | кадр движка PNG-стримом (обновление ~5 Гц), бейдж ПАУЗА |
| **Консоль + REPL** | любой Lisp в живом окружении: `(spawn-enemy 'brute)`, `(* 6 7)` → ответ в консоли |
| **Код** | ассеты (все .lisp файлы), правка, Ctrl+S → `/api/save` → **движок сам делает hot reload**, persistent-состояние переносится |
| **Тулбар** | пауза/шаг кадра/перезагрузка Lisp, кадр, fps, треугольники |

API (им же пользуется любой скрипт/агент):

```
GET  /                 страница редактора      GET  /frame.png      текущий кадр
GET  /api/state        сцена+лог+результаты    GET  /api/file?path= текст ассета
POST /api/eval {code}  исполнить Lisp          POST /api/save       сохранить ассет
POST /api/key {key,down} клавиша в игру        POST /api/ctl {cmd}  pause|resume|step|reload
```

Сцена для иерархии — Lisp: `(editor-scene)` (editor.lisp) вызывает хук игры
`(scene-entities)` (game3d.lisp) — таблицы превращаются в JSON нативно
(`src/json.rs`, HAL-уровень).

## AI Ready: MCP (--mcp)

Model Context Protocol, JSON-RPC 2.0 поверх stdio — стандарт для инструментов
ИИ (Claude Desktop, любые MCP-клиенты). Протокольная обвязка — Rust
(`src/mcp.rs`), семантика инструментов — Lisp (editor.lisp + хуки игры).

| Инструмент | Что делает |
|---|---|
| `status` | кадры, fps, счёт, волна, враги, draw calls, SIMD-уровень |
| `scene_get` | сцена JSON: игрок/враги/пули с координатами |
| `lisp_eval` | **полный доступ**: любой код Lisp в живом окружении |
| `entity_spawn` / `entity_set` / `entity_remove` | операции над сущностями |
| `step {frames}` | прошагать симуляцию (до 600 кадров) |
| `screenshot` | **PNG кадра (base64, image content)** — агент видит игру |
| `hot_reload` | перечитать .lisp (состояние defpersistent переносится) |
| `key` | событие клавиатуры |

Пример сессии:

```
→ {"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
← {"result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"rlisp-engine","version":"0.3.0"}}}
→ {"id":2,"method":"tools/call","params":{"name":"step","arguments":{"frames":90}}}
← {"result":{"content":[{"text":"выполнено 90 кадров (102.9 fps headless)"}]}}
→ {"id":3,"method":"tools/call","params":{"name":"screenshot"}}
← {"result":{"content":[{"type":"image","data":"iVBORw0KGgo…","mimeType":"image/png"}]}}
```

## 3D-конвейер (v0.2 → v0.3)

Разделение труда — то же, что между CPU и GPU:

| Слой | Что делает |
|---|---|
| **Rust `render3d.rs`** («железо», fixed-function) | T&L (model·view·proj), Sutherland–Hodgman по near-плоскости **вместе с UV-атрибутами**, backface cull, растеризация с z-буфером, flat-затенение Ламберта, **текстурирование: перспектива-корректный UV** (`u/w`, `1/w` — аффинны в экранном пространстве), **батч: развёртка инстансов в общие буферы + одна растеризация**, DDA-линии |
| **Lisp `render3d.lisp`** (пайплайн) | матрицы 4×4, `mat-lookat`, `mat-perspective`, модели, примитивы (куб/UV-сфера/тор/октаэдр/плоскость), `camera-mvp`, `draw-mesh`, **`draw-batch`/`draw-batch-tex`, `inst-row` (12 f32: t·s·yaw·pitch·roll·tint)** |
| **Lisp `textures.lisp`** (v0.3) | `tex-canvas`/`tex-px!` — процедурные текстуры (`tex-circuit`, `tex-panel`, `tex-checker`, `tex-noise`, `tex-gradient`), `load-texture` (PNG), UV-развёртки примитивов, `draw-sprite` 2D |
| **Lisp `game3d.lisp`** (сцена/игра) | камера за игроком, спавн, погоня, пули, FX, HUD, хуки редактора |

HAL-нативы 3D — «сырые команды GPU»:

| Натив | Сигнатура |
|---|---|
| `gpu3d-clear` | `(gpu3d-clear r g b)` |
| `gpu3d-light` | `(gpu3d-light dx dy dz ambient [diffuse])` |
| `gpu3d-mesh` | `(gpu3d-mesh pos idx model mvp r g b)` |
| `gpu3d-mesh-tex` | `(gpu3d-mesh-tex pos uv idx tex model mvp r g b)` |
| `gpu3d-batch` | `(gpu3d-batch pos idx insts mvp)` — инстансы: 12 f32 |
| `gpu3d-batch-tex` | `(gpu3d-batch-tex pos uv idx tex insts mvp)` |
| `gpu3d-wire` | `(gpu3d-wire pos pairs model mvp r g b)` |
| `gpu3d-stats` | `(gpu3d-stats)` → `(нарисовано отброшено)` |

Текстуры HAL: `texture-load` (PNG с диска), `texture-upload` (RGBA из
Lisp-буфера), `texture-size`, `texture-pixels`, `texture-free`, `gpu-blit-tex`
(2D-спрайт с тинтом). PNG-кодек (`src/png.rs`) — свой: писатель (stored
deflate) + **читатель** (полный RFC 1951 inflate + фильтры 0–4, цветовые типы
0/2/3/4/6).

Один кадр «ORBITAL» v0.3 ≈ **7–9 draw calls 3D** (v0.2: 60–80), ~700–800
треугольников, 320×240 — **~100 fps** headless на тестовой VM (в кадре
текстурный растр — дороже попиксельно, чем flat). Замер батча: 30 кубов × 60
повторов — **батч 13.0 мс против 39.5 мс по-объектно (3.0×)**.

## Hot reload (ТЗ §8)

Ядро следит за `.lisp`-файлами каждый кадр. При изменении: из старого мира
снимается `*persistent*`-состояние (12 значений в 3D), строится новое
Lisp-окружение, все файлы перечитываются, значения переносятся. Работает и из
редактора (Ctrl+S в вкладке «Код»), и по MCP (`hot_reload`). Проверено:
`demo/run3d.log` (кадр 255), лог редактора (кадр 1124/5462).

## SIMD-слой (SAL, ТЗ §4.3–4.5)

`src/simd.rs`: AVX-512 → AVX2 → SSE2 → скаляр, runtime-диспетчеризация +
калибровка micro-kernel на старте. `--bench`: vec-dot 2048 f32 — 4.1×;
интеграция 400 тел через `vec-add` — ~1100× против поэлементного Lisp
(горячие данные — плотные `F32Vec`, интерпретатор в цикле не участвует).

## Что реализовано (сверка с ТЗ)

| Фаза ТЗ | Статус | Артефакт |
|---|---|---|
| Фаза 0: свой интерпретатор | ✅ | полный контроль над макросами/TCO |
| Фаза 1: HAL окно+GPU | ✅* | программный растр с профилем API wgpu |
| Фаза 2: ввод/время/память/SIMD | ✅ | `input-poll`, `time-now`, `mem-*`, `vec-*` + тесты эквивалентности |
| Фаза 3: макросы, TCO, eval | ✅ | `defmacro`, quasiquote, замыкания, hot reload |
| Фаза 4: Graphics DSL | ✅* | `with-frame`, `draw-text`; **v0.3: текстуры и спрайты** |
| Фаза 5: ECS + Physics DSL | ✅ | `defcomponent/defsystem`, `aabb-collide?`, SIMD-интеграция |
| Фаза 6: Game Loop + демо | ✅ | `(defgame survival …)`, hot reload |
| 3D (v0.2) | ✅ | z-буфер, куллинг, flat-shading, «ORBITAL» на Lisp |
| **Текстуры (v0.3)** | ✅ | PNG-кодек, процедурные текстуры, перспектива-корректный UV |
| **Батч-вершинник (v0.3)** | ✅ | `gpu3d-batch[-tex]`, 12 f32/инстанс, ~3× |
| **Редактор (v0.3)** | ✅ | `--serve`: иерархия, инспектор, REPL, ассеты, вид |
| **AI Ready/MCP (v0.3)** | ✅ | `--mcp`: 10 инструментов, скриншоты, stdio JSON-RPC |

`*` — отличие от ТЗ (осознанное для прототипа): вместо `wgpu` — программный
растр с тем же профилем API. wgpu-бэкенд подключается за тем же фасадом HAL,
Lisp-уровень не изменится.

## Честные отличия и ограничения прототипа

- **Рендер**: софтверный фреймбуфер по умолчанию; wgpu-бэкенд — за фичей
  `wgpu` (этап 1 роадмапа), headless через offscreen+readback. На GPU-железе
  не проверялся (в песочнице нет адаптера).
- **3D**: flat-затенение, Ламберт; текстуры — nearest-семплинг (без мипов и
  фильтрации); клиппинг — только near-плоскость (камеру не приближать вплотную
  к геометрии); прозрачность — только в 2D-спрайтах (альфа-блинт), в 3D нет
  сортировки прозрачного.
- **Редактор**: одностраничный, без drag-n-drop и гизмо-манипуляторов; вид —
  PNG-поллинг (~5 Гц), не видеопоток; сервер слушает только 127.0.0.1.
- **MCP**: tools, resources/prompts не реализованы; сессия однопоточная —
  симуляция идёт, когда агент вызывает `step` (это же честный headless-режим).
- **Списки**: `Vec<Value>` без cons-пар; GC — Rc-подсчёт ссылок.
- **Потоки/аудио**: API заложен, не реализован (MVP допускает один поток).
- **Текстуры и hot reload**: процедурные текстуры создаются при каждой
  перезагрузке мира заново; с этапа 1 менеджер GPU-ресурсов освобождает
  старые текстуры при reload (утечка «64 КБ/текстуру» из v0.3 устранена).

## Структура

```
src/
  main.rs      ядро: игровой цикл, hot reload, режимы (--serve/--mcp/--bench/--wgpu)
  interp.rs    Lisp: reader, eval+TCO, макросы, замыкания, таблицы, F32Vec
  hal.rs       HAL: растр (фасад бэкендов), ввод, память, файлы, время (+ реестр текстур)
  gpu.rs       этап 1: бэкенды рендера (software | wgpu), менеджер GPU-ресурсов
  render3d.rs  «GPU»: T&L, near-clip (+UV), куллинг, z-буфер, текстурирование,
               батч-вершинник, линии
  editor.rs    v0.3: HTTP-сервер редактора (std::net), мост кадр/сцена/команды
  mcp.rs       v0.3: MCP-сервер (JSON-RPC 2.0, stdio), 10 инструментов
  json.rs      v0.3: JSON-парсер/сериализатор (+ Value↔Json для сцены)
  simd.rs      SAL: AVX-512/AVX2/SSE2/scalar + калибровка + mat4-нативы
  png.rs       PNG-кодек: писатель (stored) + читатель (полный inflate)
build.rs       cfg has_avx512 по версии rustc (stdarch AVX-512 стабилен с 1.89)
assets/lisp/
  core.lisp      базовая библиотека: defpersistent, dotimes, get-kv…
  font.lisp      шрифт 3×5 как данные
  graphics.lisp  with-frame, draw-text (уровень 3)
  input.lisp     key-down?/key-pressed? (уровень 3)
  ecs.lisp       defcomponent/defsystem/entities (уровень 3)
  physics.lisp   aabb-collide?, integrate-batch! (уровень 3)
  render3d.lisp  3D-DSL: матрицы, камера, меши, draw + батчи (уровень 3)
  textures.lisp  v0.3: процедурные текстуры, PNG, UV, спрайты (уровень 3)
  editor.lisp    v0.3: мост редактора/MCP: editor-scene, mcp-* (уровень 3)
  game.lisp      игра «Выживание» (уровень 4)
  game3d.lisp    3D-игра «ORBITAL» + хуки сцены (уровень 4)
  main.lisp      бутстрап 2D-стека
  main3d.lisp    бутстрап 3D-стека
assets/editor/index.html   страница редактора (зашита в бинарь)
scripts/       smoke.sh, demo3d.sh, demo_hot_reload.sh, demo_editor.sh, …
demo/          orbital.gif, survival.gif, filmstrip*.png, *.log
```

## Дальше

1. ~~wgpu-бэкенд за текущим фасадом HAL (Lisp не меняется)~~ — сделано в
   этапе 1 (2D-растр; 3D-батчи — этап 5, см. `docs/roadmap.md`).
2. Редактор: гизмо-манипуляторы в виде (drag по осям), drag-n-drop префабов,
   сохранение сцены в Lisp-файл.
3. 3D: мип-фильтрация текстур, прозрачность с сортировкой, фрустум-клиппинг
   по X/Y, скелетная анимация поверх `mat4-batch`.
4. MCP: resources (лог движка как поток), события сцены (on-kill и т.п.).
5. SIMD-токенизатор парсера и GC-сканирование.
6. JIT для eval (ТЗ §10.1).
