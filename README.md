<div align="center">

<img src="docs/banner.svg" alt="rlisp-engine" width="1000">

# rlisp-engine

**Игровой движок, в котором игра — это Lisp-файл, а Rust — только доступ к железу.**

[Сайт](https://kostyk348.github.io/rlisp-engine/) · [Песочница в браузере](https://kostyk348.github.io/rlisp-engine/playground.html) · [Этапы и роадмап](docs/roadmap.md) · [English](README.en.md)

[![CI](https://github.com/kostyk348/rlisp-engine/actions/workflows/ci.yml/badge.svg)](https://github.com/kostyk348/rlisp-engine/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/kostyk348/rlisp-engine?color=ffb347)](https://github.com/kostyk348/rlisp-engine/releases)
![Tests](https://img.shields.io/badge/tests-151%20passing-7ee787)
![Dependencies](https://img.shields.io/badge/dependencies-0-critical)
![License](https://img.shields.io/badge/license-MIT-blue)

![РУИНЕР](docs/img/ruiner_fight.png)
![Остров](demo/island.gif)
![Редактор](docs/img/editor-full.png)

</div>

---

## Что это

Движок, где **24 353 строки Rust** — это HAL: растр, T&L, z-буфер, ввод, время, звук,
PNG, HTTP. Всё остальное — **8 695 строк Lisp**: `update`/`draw`, физика, ECS, ввод,
текстуры, пост-обработка, сам редактор сцены и все демо-игры.

```lisp
(define (update dt)
  (poll-input)
  (when (key-down? "w") (set! *z* (+ *z* (* 4.0 dt))))
  (for-each (lambda (e) (enemy-step e dt)) *enemies*))

(define (draw)
  (gpu3d-clear 0.08 0.09 0.12)
  (gpu3d-batch-mat *wall-mesh* *instances* mvp)
  (gpu-present))
```

Из этого следуют четыре свойства, каждое из которых проверяется, а не декларируется:

| Свойство | Как проверено |
|---|---|
| **Hot reload без перезапуска** | правишь `.lisp` — движок пересобирает окружение на следующем кадре, persistent-значения сохраняются |
| **Прогон воспроизводится побитово** | `--record` / `--replay`: hash каждого кадра, цепочка, номер первого расхождения |
| **Ноль зависимостей** | дефолтная сборка не тянет ни одного крейта; CI это проверяет (`cargo tree`) |
| **Весь движок в ~1.2 МБ WASM** (1 242 983 байта) | `docs/playground.html` — редактор, REPL и звук прямо в браузере, без сети |

## Почему это не «игрушка на Lisp»

Числа из реальных прогонов (`docs/benchmark.md` — там же окружение и методика):

| Что | Результат | Условия |
|---|---|---|
| JIT числового ядра (лямбда с `if`/`+`/`*`/`/`) | **49.07×** против интерпретатора (2296 → 47 нс/вызов) | 200 000 вызовов, release + `--features jit` |
| SIMD `vec-dot` 2048 f32 | **4.22×** против скалярного (22.8 → 5.4 мс) | `--bench`, SSE2-путь |
| Пакетная векторная арифметика против поэлементной в Lisp | **~1107×** | интеграция 400 тел, 20 000 итераций |
| 2D-кадр 320×240, программный растр | **0.38 мс** HAL / **0.65 мс** через Lisp | `--bench`: 120 прямоугольников + 12 блитов + 4 текстуры |
| Игра целиком на Lisp | **714 строк** («РУИНЕР»), **745** («ОАЗИС») | `assets/lisp/` |

Качество держится тестами, а не обещаниями: **151/151** (`cargo test --workspace`),
clippy с `-D warnings`, `cargo fmt --check`, плюс собственный линтер, который не даёт
файлам в `src/` разрастаться (лимит 800 строк — см. [AI_AGENTS.md](AI_AGENTS.md)).

## Быстрый старт

```bash
git clone https://github.com/kostyk348/rlisp-engine && cd rlisp-engine

cargo run --release                                   # игра «Выживание» (весь код — в assets/lisp/)
cargo run --release -- --script assets/lisp/main_ruiner.lisp --ascii --stdin-keys   # DOOM-лайк в терминале
cargo run --release -- --serve                        # редактор: http://127.0.0.1:7300
cargo run --release -- --script assets/lisp/main3d.lisp --capture demo/frames --fast --frames 300
```

Дефолтная сборка — без зависимостей. `--features wgpu` добавляет wgpu-бэкенд,
`--features jit` — copy-and-patch JIT.

## Детерминизм: прогон воспроизводится побитово

Редкое свойство в игровых движках: **у каждого кадра есть hash, кадры сцеплены в цепочку**,
и любой прогон можно записать, а потом сверить кадр в кадр.

```bash
# записать: seed ГПСЧ + поток ввода по кадрам + hash каждого кадра
cargo run --release -- --script assets/lisp/replay_demo.lisp \
    --fast --frames 240 --record saves/demo.rlrp

# воспроизвести: те же кадры и та же цепочка — или конкретный кадр расхождения
cargo run --release -- --script assets/lisp/replay_demo.lisp \
    --fast --frames 240 --replay saves/demo.rlrp
# => replay replay: 240 из 240 кадров сверено, chain 883cd568c4599f32, OK
```

- **hash кадра — FNV-1a по байтам фреймбуфера**: проверяется то, что реально увидел игрок,
  включая пиксели, а не абстрактная «модель мира»;
- **цепочка вместо одного хеша**: `chain_{n+1} = mix(chain_n, кадр, hash)` — расхождение
  любого кадра ломает все последующие, и первый несовпавший называется явно
  (`replay: кадр 0: hash кадра 5d4de… ≠ 05ac6…`, код возврата 1 — годится в CI);
- **живая запись из редактора с любого кадра**: панель «Детерминизм» в консоли
  (`● Запись / ■ Стоп`) или `(replay-record! "saves/live.rlrp")`; в файле остаётся
  `start N`, воспроизведение сверяет начиная с него;
- **случайность — только через `(random-seed n)` / `(random-int a b)` / `(random-range a b)`**,
  поэтому детерминизм не «в теории», а по построению.

Файл прогона — обычный текст (`RLISP-REPLAY 1`, `seed`, `fps`, `frames`, `start`,
построчно `кадр hash ввод…`, финальный `chain`): десятки килобайт на тысячи кадров,
диффабельный, прикладывается к баг-репорту.

## Что умеет

| Подсистема | Что есть |
|---|---|
| **3D** | T&L на Lisp, z-буфер, батч-вершинник (`gpu3d-batch-mat`), туман с LUT, материалы, скелетная анимация |
| **Освещение и пост** | направленный свет + точечные источники, blob-тени, bloom с плечом экспозиции, виньетка, зерно |
| **Физика** | импульсный солвер, NGS-трение качения, сферы/полигоны/heightfield, плавучесть, угловая динамика |
| **ECS** | слоты + поколения (зомби-хендлы невозможны), bulk-обход `ecs-each` / `ecs-col` |
| **Рендер 2D** | PNG-декодер с нуля, процедурные текстуры на Lisp, alpha-test/blend, спрайты-билборды |
| **Звук** | сведение, синтез (saw/noise/sine), 3D-затухание, SFX-синтезатор в редакторе |
| **Ассеты** | именованный реестр с типами и рефкаунтами, явная сборка `asset-gc!` |
| **Скрипты** | 224 натива; всё остальное — Lisp-библиотеки, hot reload |
| **Сборка** | нативный бинарь + WASM (`scripts/export_game.py` — автономный одностраничный HTML) |
| **AI** | MCP-сервер (`--mcp`, JSON-RPC 2.0 stdio): сцена, спавн, свойства, шаг симуляции, кадры |

## Редактор (--serve)

`cargo run --release -- --serve` → `http://127.0.0.1:7300`: иерархия сцены, инспектор
свойств, живой вид (PNG-стрим кадра), гизмо X/Y/Z прямо в игре, drag-n-drop префабов,
сохранение сцены в `.lisp`, REPL, редактор кода с hot reload, панель записи прогона.
UI — `assets/editor/index.html`, семантика сцены — `assets/lisp/editor.lisp`
(сервер ничего не знает про игру).

## AI Ready: MCP

`--mcp` поднимает MCP-сервер: агент видит сцену, спавнит сущности, правит свойства,
исполняет Lisp, шагает симуляцию и получает кадры. Регламент для агентов —
[AI_AGENTS.md](AI_AGENTS.md): модульность, лимит 800 строк на файл, автотест в CI.

## Архитектура

```
┌──────────────────────────────────────────────────────────────┐
│ L4  Игры на Lisp: РУИНЕР · ОАЗИС · ШТОЛЬНЯ · ДОЛИНА · Остров  │
├──────────────────────────────────────────────────────────────┤
│ L3  Системные DSL (assets/lisp/): графика · ввод · ECS ·      │
│     физика · render3d · текстуры · звук · редактор сцены      │
├──────────────────────────────────────────────────────────────┤
│ L2  Интерпретатор: cons + Vec, tracing-GC, JIT, SIMD-сканер   │
├──────────────────────────────────────────────────────────────┤
│ L1  HAL (Rust): растр, T&L, z-буфер, fb, время, ввод, звук,   │
│     PNG, HTTP/редактор, MCP, wgpu-бэкенд за фасадом           │
└──────────────────────────────────────────────────────────────┘
```

Подробнее: [docs/architecture.md](docs/architecture.md).

## Честные ограничения

- **wgpu закрывает только 2D**; 3D-конвейер программный (перенос — отдельный этап);
- **детерминизм гарантирован при фиксированном dt** (`--fast` / `--frames` / `--input`);
  в реальном времени dt плавает — запись предупреждает об этом при старте;
- **нет дерева нод как в Godot**: сцена — ECS + Lisp-структуры, это осознанный выбор;
- **нет редактора анимаций и шейдеров**; анимация — данные в Lisp;
- MSRV — rustc 1.83; сборка `--features jit` — только x86-64 Linux.

Полный список: [LIMITATIONS.md](LIMITATIONS.md).

## Структура

```
src/            Rust-HAL и ядро: hal/ · interp/ · render3d/ · physics/ · gpu/ · jit/ · replay.rs · editor.rs
assets/lisp/    вся логика: core · input · graphics · ecs · physics · render3d · editor · игры
assets/editor/  UI редактора (index.html, ноль зависимостей)
docs/           архитектура, бенчмарки, роадмап, история, сайт, песочница
scripts/        экспорт игр в HTML, проверки, бенчмарки
```

## Документация

[Архитектура](docs/architecture.md) · [Бенчмарки](docs/benchmark.md) · [Роадмап по этапам](docs/roadmap.md) · [История проекта](docs/history.md) · [Ограничения](LIMITATIONS.md) · [CHANGELOG](CHANGELOG.md) · [Регламент для AI-агентов](AI_AGENTS.md)

## Участие

Issues — баг-репорты и идеи, Pull Requests — приветствуются: см. [CONTRIBUTING.md](CONTRIBUTING.md)
и правила в [AI_AGENTS.md](AI_AGENTS.md) (файл ≤ 800 строк, тесты зелёные, clippy без
предупреждений). Обсуждения — в [Discussions](https://github.com/kostyk348/rlisp-engine/discussions).

## Лицензия

[MIT](LICENSE).
