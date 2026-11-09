# Архитектура

«Игровой движок как Lisp-машина: Rust только как HAL, всё остальное — DSL».
Каждый слой знает только о слое под ним; Lisp-уровень не знает о выборе
рендер-бэкенда.

## Четыре слоя

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
│  УРОВЕНЬ 1: Rust HAL (src/hal.rs, render3d.rs, gpu.rs, png.rs,       │
│  editor.rs, mcp.rs, json.rs) — окно/растр │ бэкенды рендера:         │
│  software │ wgpu (offscreen+readback) │ 3D-«GPU»: T&L+z-буфер+батчи  │
│  │ ввод │ память │ время │ PNG-кодек │ HTTP │ MCP                    │
└──────────────────────────────────────────────────────────────────────┘
```

## Карта модулей

| Модуль | Слой | Роль |
|---|---|---|
| `src/main.rs` | ядро | игровой цикл (update → draw → present), hot reload, `--serve/--mcp/--bench/--wgpu` |
| `src/interp.rs` | 2 | reader, eval+TCO, макросы, таблицы, F32Vec |
| `src/simd.rs` | 2 | SAL: AVX-512/AVX2/SSE2/scalar + калибровка + mat4 |
| `src/hal.rs` | 1 | фасад HAL: растровые команды, ввод, память, файлы, время, реестр текстур, **диспетчеризация бэкендов** |
| `src/gpu.rs` | 1 | **бэкенды рендера (этап 1)**: `enum Backend` + трейт `GpuBackend`, менеджер ресурсов `GpuResources`, wgpu-рендерер (фича `wgpu`) |
| `src/render3d.rs` | 1 | 3D fixed-function: T&L, near-clip (+UV), куллинг, z-буфер, батч-вершинник |
| `src/png.rs` | 1 | PNG-кодек без зависимостей (писатель + полный inflate) |
| `src/editor.rs` | 1 | HTTP-сервер редактора (std::net), мост кадр/сцена/команды |
| `src/mcp.rs`, `src/json.rs` | 1 | MCP (JSON-RPC 2.0, stdio), JSON |
| `build.rs` | — | cfg `has_avx512` по версии rustc (stdarch AVX-512 стабилен с 1.89) |

## Рендер-бэкенды за фасадом HAL (этап 1)

```
 Lisp: (gpu-clear) (gpu-fill-rect) (gpu-blit) (gpu-blit-tex) (gpu-present)
                     │
                     ▼
   Hal::clear_all / fill_rect / blit / blit_tex / flush_gpu   ← фасад
                     │
        ┌────────────┴─────────────┐
        ▼                          ▼
  Backend::Software          Backend::Wgpu(Box<dyn GpuBackend>)
  программный растр:         (фича `wgpu`, wgpu 0.20):
  команды пишут прямо        команды → offscreen-текстура RGBA8
  в Hal::fb (как в           (alpha = маска покрытия), при flush:
  v0.1–v0.3)                 copy-to-buffer → map-read → композит
                             в Hal::fb по альфе-покрытию
```

Контракт `GpuBackend`: `clear / fill_rect / blit / blit_tex / resize /
tex_upload / tex_free / flush(fb, readback) -> (submit_ns, readback_ns)`.
`Hal::flush_gpu()` — единственная точка, где GPU-слой попадает в fb; она
вызывается из `gpu-present`, `gpu-screenshot`, перед каждой 3D-командой
(порядок художника = как у программного растра) и перед публикацией кадра
веб-редактору.

### Композит по альфе-покрытию

Кадр wgpu-бэкенда живёт в offscreen-текстуре, где alpha — маска «GPU
рисовал сюда» (clear заливает alpha=1 весь кадр, квады пишут alpha=1,
не покрытые пиксели остаются alpha=0). Readback копирует текстуру в
выровненный по 256 байт буфер, `composite_readback()` переносит в fb только
покрытые пиксели — под ними может быть результат программного 3D-конвейера.
Так PNG/ASCII/редактор читают один и тот же fb при любом бэкенде.

### Честно о покрытии этапа 1

- wgpu закрывает **2D-растр** (clear / fill-rect / blit / blit-tex /
  present);
- 3D-конвейер (`render3d.rs`: T&L, z-буфер, батчи) остаётся **программным**;
  перенос батчей на GPU — этап 5 роадмапа;
- критерий «`--wgpu` кратно быстрее» требует GPU-железа — см.
  `docs/benchmark.md`.

## Менеджер GPU-ресурсов

`GpuResources` (без устройства — работает и на software-пути):

- `register/free/live/bytes/generation` — книга хэндлов, зеркалит реестр
  `hal.tex` (id = индекс+1);
- перезагрузка живого id замещает старое поколение (free+create) — счётчики
  честные: `tex_created − tex_freed == живые хэндлы`;
- hot-reload: `Hal::invalidate_textures_except(keep)` освобождает всё,
  кроме id, которые встречаются среди persistent-значений
  (`gpu::collect_int_refs` обходит таблицы/списки/векторы). Тест-критерий
  этапа: 100 create/reload циклов без роста памяти;
- на wgpu-стороне рядом живёт `HashMap<id, wgpu::Texture>` — удаляется
  вместе со слотом (утечек нет: drop текстуры возвращает память GPU).

## Zero-dependency по умолчанию

`[dependencies]` пуст; wgpu — `optional = true` за фичей `wgpu`. Cargo.lock
содержит и опциональный граф (optional-зависимости резолвятся всегда),
поэтому CI проверяет ноль зависимостей через `cargo tree -e normal
--prefix none` (в дефолтной сборке виден только корень), а не через grep
лока.
