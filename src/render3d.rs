// src/render3d.rs — Уровень 1 (HAL): «GPU» фиксированного конвейера.
// Философия ТЗ соблюдена: Rust знает только треугольники, глубину и свет —
// ровно то, что в «железе» делает GPU (T&L эпохи GeForce 256 + z-буфер).
// Камера, проекции, матрицы моделей, меши, сцена, игра — Lisp
// (assets/lisp/render3d.lisp, game3d.lisp).
//
// Примитивы:
//   gpu3d-clear     — очистка цвета + z-буфера (+ сброс статистики кадра)
//   gpu3d-light     — направленный свет и фоновая освещённость (fixed-function)
//                     этап 7: + блик Blinn-Phong (spec, shininess — опц. аргументы)
//   gpu3d-view      — позиция камеры в мире (для half-vector блика)
//   gpu3d-mat-tiling— множители UV перед семплингом (repeat-текстуры)
//   gpu3d-alpha-test— порог alpha (cutout): тексель с alpha < порога отбрасывается
//                     ДО записи в z-буфер (семантика discard у GPU)
//   gpu3d-fog       — экспоненциальный туман: 1 - exp(-density * глубина),
//                     смешение ПОСЛЕ шейдинга, только для непрозрачных поверхностей
//   gpu3d-state     — таблица текущего состояния конвейера (свет/блик/туман/…)
//   gpu3d-mesh      — сетка: T&L → near-clip → backface cull → проекция →
//                     z-буфер → flat-затенение (Lambert)
//   gpu3d-mesh-tex  — то же + текстурирование: перспектива-корректный UV
//                     (интерполяция u/w и 1/w, как в железе), nearest-семплинг
//   gpu3d-batch     — БАТЧ-ВЕРШИННИК: много инстансов одним draw call
//                     (12 f32 на инстанс: t.xyz s.xyz yaw pitch roll rgb)
//   gpu3d-batch-tex — батч + текстура + тинт из инстанса
//   gpu3d-wire      — линии (пары индексов) с clip по near-плоскости
//   gpu3d-stats     — (нарисовано треугольников, отброшено куллингом)
//
// Глубина: интерполируется NDC-z линейно в экранных координатах — это
// корректная перспектива для глубины (так же делает железный depth buffer).
// UV: u/w и 1/w аффинны в экранном пространстве → семплинг корректен и после
// клиппинга (атрибуты режутся в однородных координатах вместе с вершиной).

use crate::hal::Hal;
use crate::interp::{env_define, need_args, num_val, Gc, Value};
use crate::simd::to_f32s;
use std::cell::RefCell;
use std::rc::Rc;

pub struct R3 {
    pub zbuf: Vec<f32>,
    pub w: usize,
    pub h: usize,
    pub light: [f32; 3], // направление К ИСТОЧНИКУ (нормировано)
    pub ambient: f32,
    pub diffuse: f32,
    pub drawn: u64,
    pub culled: u64,
    // — этап 7 «материалы»: состояние конвейера, как у fixed-function GPU —
    /// Порог alpha (cutout): 0 = выкл (дефолт); 0.1..1 — тексели текстурных
    /// граней с alpha < порога отбрасываются целиком (и цвет, и z-запись).
    pub alpha_test: f32,
    /// Сила блика Blinn-Phong: 0 = выкл (дефолт, старое поведение Lambert).
    pub spec: f32,
    /// Показатель степени блика (шершавость): больше — пятно блика уже.
    pub shininess: f32,
    /// Позиция камеры в мире (gpu3d-view) — нужен для half-vector блика.
    pub view: [f32; 3],
    /// Множители UV перед семплингом: (1,1) — как раньше (clamp к краю),
    /// (u,v) ≠ (1,1) — repeat через fract (тайлинг текстуры).
    pub tiling: [f32; 2],
    /// Туман: [r g b density]; density 0 = выкл (дефолт).
    pub fog: [f32; 4],
    /// Равномерная альфа материала (gpu3d-alpha-blend): 1 = непрозрачно
    /// (дефолт); < 1 — треугольники с итоговой альфой < 1 уходят в
    /// прозрачную очередь (src-alpha блендинг, сорт far→near, без z-записи).
    pub blend: f32,
    /// Прозрачная очередь: собирается непрозрачными вызовами, дренируется
    /// gpu3d-flush-alpha (сортировка far→near — художник-порядок для крон,
    /// воды, облаков).
    pub(crate) transq: Vec<TransTri>,
}

/// Треугольник прозрачной очереди: экранные вершины со ВСЕМИ атрибутами
/// (u/w v/w 1/w мир/w a/w r/w g/w b/w — уже спроецированы), мировая нормаль,
/// цвет-множитель и снимок состояния материалов на момент постановки.
pub(crate) struct TransTri {
    /// Видовая глубина центра (w_clip, больше = дальше) — ключ сортировки.
    pub depth: f32,
    pub pts: [Vtx; 3],
    pub nrm: [f32; 3],
    pub tex: i64,
    pub spec: f32,
    pub shininess: f32,
    pub fog: [f32; 4],
}

impl Default for R3 {
    fn default() -> Self {
        Self::new()
    }
}

impl R3 {
    pub fn new() -> R3 {
        R3 {
            zbuf: Vec::new(),
            w: 0,
            h: 0,
            light: [0.45, 0.8, 0.35],
            ambient: 0.35,
            diffuse: 0.75,
            drawn: 0,
            culled: 0,
            alpha_test: 0.0,
            spec: 0.0,
            shininess: 32.0,
            view: [0.0, 0.0, 0.0],
            tiling: [1.0, 1.0],
            fog: [0.7, 0.8, 0.9, 0.0],
            blend: 1.0,
            transq: Vec::new(),
        }
    }

    pub fn reset(&mut self, w: usize, h: usize) {
        if self.w != w || self.h != h || self.zbuf.len() != w * h {
            self.w = w;
            self.h = h;
            self.zbuf = vec![f32::INFINITY; w * h];
        }
    }
}

fn col(x: f64) -> u8 {
    (x * 255.0).round().clamp(0.0, 255.0) as u8
}

// ——— этап 7 «материалы»: чистые функции ядра шейдинга (покрыты юнит-тестами) ———

/// Фактор экспоненциального тумана: 1 - exp(-density * глубина).
/// density 0 или глубина 0 → 0 (без тумана); растёт монотонно к 1.
#[inline]
pub(crate) fn fog_factor(density: f32, depth: f32) -> f32 {
    (1.0 - (-(density.max(0.0) * depth.max(0.0))).exp()).clamp(0.0, 1.0)
}

/// Предикат alpha-test: тексель с alpha < порога отбрасывается.
/// threshold <= 0 — тест выключен (всё проходит); граница включительная:
/// alpha/255 == порог — тексель ОСТАЁТСЯ (как `if (a >= cutoff) discard` нет:
/// у GPU обычно `alpha < cutoff → discard`, т.е. равный порогу проходит).
#[inline]
pub(crate) fn alpha_keep(a: u8, threshold: f32) -> bool {
    threshold <= 0.0 || (a as f32 / 255.0) >= threshold
}

/// Координата семплинга с учётом тайлинга: tiling == 1 — координата как есть
/// (старое поведение: clamp к краю текстуры), иначе repeat через fract.
#[inline]
pub(crate) fn tiled_coord(u: f32, tiling: f32) -> f32 {
    if tiling == 1.0 {
        u
    } else {
        (u * tiling).fract()
    }
}

/// Интенсивность блика Blinn-Phong: strength * max(N·H, 0)^shininess.
#[inline]
pub(crate) fn spec_highlight(ndh: f32, shininess: f32, strength: f32) -> f32 {
    if strength <= 0.0 || ndh <= 0.0 {
        0.0
    } else {
        strength * ndh.powf(shininess.max(1.0))
    }
}

/// Разбор аргументов gpu3d-light с честным фолбэком:
/// (dx dy dz ambient [diffuse [spec [shininess]]]) —
/// отсутствующие опциональные аргументы сохраняют прежнее состояние
/// (старые вызовы с 4-5 аргументами работают байт-в-байт как раньше).
pub(crate) fn apply_light_args(r3: &mut R3, args: &[Value]) -> Result<(), String> {
    need_args(args, 4)?;
    r3.light = [
        num_val(&args[0])? as f32,
        num_val(&args[1])? as f32,
        num_val(&args[2])? as f32,
    ];
    r3.ambient = num_val(&args[3])? as f32;
    if let Some(v) = args.get(4) {
        r3.diffuse = num_val(v)? as f32;
    }
    if let Some(v) = args.get(5) {
        r3.spec = num_val(v)?.max(0.0) as f32;
    }
    if let Some(v) = args.get(6) {
        r3.shininess = num_val(v)?.max(1.0) as f32;
    }
    Ok(())
}

#[inline]
fn edge(ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32) -> f32 {
    (bx - ax) * (cy - ay) - (by - ay) * (cx - ax)
}

#[inline]
fn cross3(a: &[f32; 3], b: &[f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
fn dot3(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Вершина клип-пространства: [x y z w] + атрибуты u/w, v/w, 1/w, wx/w, wy/w,
/// wz/w, a/w, r/w, g/w, b/w (этап 9: per-vertex RGBA для mesh-vc).
/// Клиппинг Sutherland–Hodgman по плоскости z + w >= 0 (near).
/// Атрибуты (u/w, 1/w, мир/w, a/w, rgb/w) линейны вдоль ребра → интерполируются
/// тем же t. Мир/w нужен per-pixel half-vector блику, a/w и rgb/w — прозрачности
/// и цветовым градиентам (вода, небо).
fn clip_near6(poly: &[[f32; 14]]) -> Vec<[f32; 14]> {
    let mut out = Vec::with_capacity(poly.len() + 1);
    let n = poly.len();
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        let da = a[2] + a[3];
        let db = b[2] + b[3];
        let a_in = da >= 0.0;
        let b_in = db >= 0.0;
        if a_in {
            out.push(a);
        }
        if a_in != b_in {
            let t = da / (da - db);
            let mut v = [0.0f32; 14];
            for k in 0..10 {
                v[k] = a[k] + (b[k] - a[k]) * t;
            }
            out.push(v);
        }
    }
    out
}

#[derive(Clone, Copy)]
pub(crate) struct Vtx {
    sx: f32,
    sy: f32,
    sz: f32,
    uq: f32,  // u/w
    vq: f32,  // v/w
    iw: f32,  // 1/w
    wxq: f32, // мир.x/w (этап 7: блик)
    wyq: f32, // мир.y/w
    wzq: f32, // мир.z/w
    aq: f32,  // альфа/w (этап 9: блендинг; 1/w → альфа 1)
    rq: f32,  // r/w (этап 9: per-vertex цвет)
    gq: f32,  // g/w
    bq: f32,  // b/w
}

fn project6(c: &[f32; 14], w: usize, h: usize) -> Option<Vtx> {
    if c[3] <= 1e-6 {
        return None;
    }
    let nx = c[0] / c[3];
    let ny = c[1] / c[3];
    let nz = c[2] / c[3];
    Some(Vtx {
        sx: (nx * 0.5 + 0.5) * w as f32,
        sy: (1.0 - (ny * 0.5 + 0.5)) * h as f32, // y вниз, как в фреймбуфере
        sz: nz,
        uq: c[4],  // u/w
        vq: c[5],  // v/w
        iw: c[6],  // 1/w
        wxq: c[7], // мир/w
        wyq: c[8],
        wzq: c[9],
        aq: c[10],
        rq: c[11],
        gq: c[12],
        bq: c[13],
    })
}

fn project(c: &[f32; 4], w: usize, h: usize) -> Option<Vtx> {
    if c[3] <= 1e-6 {
        return None;
    }
    let inv = 1.0 / c[3];
    let nx = c[0] * inv;
    let ny = c[1] * inv;
    let nz = c[2] * inv;
    Some(Vtx {
        sx: (nx * 0.5 + 0.5) * w as f32,
        sy: (1.0 - (ny * 0.5 + 0.5)) * h as f32,
        sz: nz,
        uq: 0.0,
        vq: 0.0,
        iw: inv,
        wxq: 0.0,
        wyq: 0.0,
        wzq: 0.0,
        aq: inv,
        rq: 0.0,
        gq: 0.0,
        bq: 0.0,
    })
}

enum Prim {
    Tri,
    Wire,
}

struct TexRef {
    w: usize,
    h: usize,
    px: Vec<u8>,
    id: i64,
}

fn tex_lookup(hal: &Hal, id: i64) -> Result<Option<TexRef>, String> {
    if id == 0 {
        return Ok(None);
    }
    let t = hal
        .tex
        .get((id - 1) as usize)
        .ok_or_else(|| format!("gpu3d: нет текстуры {}", id))?;
    if t.w == 0 {
        return Err("gpu3d: текстура освобождена".into());
    }
    Ok(Some(TexRef {
        w: t.w,
        h: t.h,
        px: t.px.clone(),
        id,
    }))
}

/// T&L: world = M_model · v, clip = M_mvp · world
/// (UV-атрибуты интерполируются отдельно — см. raster_tris/uvq).
fn transform_all(
    pos: &[f32],
    model: &[f32],
    mvp: &[f32],
    world: &mut Vec<f32>,
    clip: &mut Vec<f32>,
) {
    let n = pos.len() / 3;
    world.clear();
    world.resize(n * 3, 0.0);
    clip.clear();
    clip.resize(n * 4, 0.0);
    for i in 0..n {
        let x = pos[i * 3];
        let y = pos[i * 3 + 1];
        let z = pos[i * 3 + 2];
        let wx = model[0] * x + model[1] * y + model[2] * z + model[3];
        let wy = model[4] * x + model[5] * y + model[6] * z + model[7];
        let wz = model[8] * x + model[9] * y + model[10] * z + model[11];
        world[i * 3] = wx;
        world[i * 3 + 1] = wy;
        world[i * 3 + 2] = wz;
        clip[i * 4] = mvp[0] * wx + mvp[1] * wy + mvp[2] * wz + mvp[3];
        clip[i * 4 + 1] = mvp[4] * wx + mvp[5] * wy + mvp[6] * wz + mvp[7];
        clip[i * 4 + 2] = mvp[8] * wx + mvp[9] * wy + mvp[10] * wz + mvp[11];
        clip[i * 4 + 3] = mvp[12] * wx + mvp[13] * wy + mvp[14] * wz + mvp[15];
    }
}

/// M = T · Ryaw · Rpitch · Rroll · S (конвенция render3d.lisp).
fn inst_matrix(ins: &[f32], out: &mut [f32; 16]) {
    let (tx, ty, tz) = (ins[0], ins[1], ins[2]);
    let (sx, sy, sz) = (ins[3], ins[4], ins[5]);
    // ВАЖНО: f32::sin_cos возвращает (sin, cos) — не (cos, sin)!
    // (баг мастер-ветки: перепутанная связка ломала матрицу инстанса —
    // оси подменялись, батчи рисовались «сплющенными»)
    let (syw, cy) = ins[6].sin_cos();
    let (sxi, cx) = ins[7].sin_cos();
    let (szi, cz) = ins[8].sin_cos();
    // R = Ry · Rx · Rz
    // Ry·Rx:
    //   [ cy  sy*sx  sy*cx ]
    //   [ 0   cx    -sx    ]
    //   [ -sy cy*sx  cy*cx ]
    let a00 = cy;
    let a01 = syw * sxi;
    let a02 = syw * cx;
    let a10 = 0.0;
    let a11 = cx;
    let a12 = -sxi;
    let a20 = -syw;
    let a21 = cy * sxi;
    let a22 = cy * cx;
    // · Rz:
    //   [ cz -sz 0 ]
    //   [ sz  cz 0 ]
    //   [ 0   0  1 ]
    let r00 = a00 * cz - a01 * szi;
    let r01 = a00 * szi + a01 * cz;
    let r02 = a02;
    let r10 = a10 * cz - a11 * szi;
    let r11 = a10 * szi + a11 * cz;
    let r12 = a12;
    let r20 = a20 * cz - a21 * szi;
    let r21 = a20 * szi + a21 * cz;
    let r22 = a22;
    // · S и T:
    out.fill(0.0);
    out[0] = r00 * sx;
    out[1] = r01 * sy;
    out[2] = r02 * sz;
    out[3] = tx;
    out[4] = r10 * sx;
    out[5] = r11 * sy;
    out[6] = r12 * sz;
    out[7] = ty;
    out[8] = r20 * sx;
    out[9] = r21 * sy;
    out[10] = r22 * sz;
    out[11] = tz;
    out[12] = 0.0;
    out[13] = 0.0;
    out[14] = 0.0;
    out[15] = 1.0;
}

/// Ядро: трансформация вершин + растеризация. Один вызов = один draw call.
#[allow(clippy::too_many_arguments)]
fn mesh_draw(
    hal: &mut Hal,
    pos: &[f32],
    uv: Option<&[f32]>,
    vca: Option<&[f32]>, // per-vertex rgba (gpu3d-mesh-vc)
    idx: &[f32],
    model: &[f32],
    mvp: &[f32],
    r: f64,
    g: f64,
    b: f64,
    texid: i64,
    prim: Prim,
) -> Result<u64, String> {
    // wgpu-режим: 3D-конвейер рисует в fb — сначала сбрасываем отложенный
    // 2D-слой (порядок художника совпадает с программным растром)
    let _ = hal.flush_gpu();
    if pos.len() % 3 != 0 || pos.is_empty() {
        return Err("gpu3d: позиции — плоский массив xyz (длина кратна 3)".into());
    }
    let n = pos.len() / 3;
    if let Some(u) = uv {
        if u.len() != n * 2 {
            return Err(format!(
                "gpu3d: uv — пары на вершину ({}), вершин {}",
                u.len(),
                n
            ));
        }
    }
    if let Some(vc) = vca {
        if vc.len() != n * 4 {
            return Err(format!(
                "gpu3d: rgba — четвёрки на вершину ({}), вершин {}",
                vc.len(),
                n
            ));
        }
    }
    if let Prim::Wire = prim {
        if idx.len() % 2 != 0 {
            return Err("gpu3d-wire: индексы — пары вершин (длина кратна 2)".into());
        }
    } else if idx.len() % 3 != 0 {
        return Err("gpu3d: индексы — тройки вершин (длина кратна 3)".into());
    }
    if model.len() != 16 || mvp.len() != 16 {
        return Err("gpu3d: матрицы 4x4 = 16 чисел".into());
    }
    let tex = tex_lookup(hal, texid)?;
    if tex.is_some() && uv.is_none() {
        return Err("gpu3d: текстура без uv-массива".into());
    }
    let (w, h) = (hal.w, hal.h);
    if hal.r3.zbuf.len() != w * h {
        hal.r3.reset(w, h);
    }
    let Hal { fb, r3, .. } = &mut *hal;

    let mut world = Vec::with_capacity(n * 3);
    let mut clip = Vec::with_capacity(n * 4);
    transform_all(pos, model, mvp, &mut world, &mut clip);

    let light = {
        let l = r3.light;
        let len = dot3(&l, &l).sqrt().max(1e-6);
        [l[0] / len, l[1] / len, l[2] / len]
    };
    let (ambient, diffuse) = (r3.ambient, r3.diffuse);
    let mut drawn: u64 = 0;
    let mut culled: u64 = 0;

    if let Prim::Wire = prim {
        // каркас: индексы — ПАРЫ вершин; clip по near-плоскости на сегмент
        for e in 0..idx.len() / 2 {
            let mut seg = [0usize; 2];
            for k in 0..2 {
                let vi = idx[e * 2 + k];
                // через переменную: без !(a >= b) — NaN-семантика прежняя
                let idx_ok = vi >= 0.0 && vi.fract() == 0.0 && (vi as usize) < n;
                if !idx_ok {
                    return Err(format!("gpu3d: индекс за границей: {}", vi));
                }
                seg[k] = vi as usize;
            }
            let (p, q) = (
                [
                    clip[seg[0] * 4],
                    clip[seg[0] * 4 + 1],
                    clip[seg[0] * 4 + 2],
                    clip[seg[0] * 4 + 3],
                ],
                [
                    clip[seg[1] * 4],
                    clip[seg[1] * 4 + 1],
                    clip[seg[1] * 4 + 2],
                    clip[seg[1] * 4 + 3],
                ],
            );
            let (pw, qw) = (p[3], q[3]);
            if pw <= 1e-6 && qw <= 1e-6 {
                continue;
            }
            let (pa, qa) = if pw < 1e-6 || qw < 1e-6 {
                let t = (1e-6 - pw) / (qw - pw);
                (
                    [
                        p[0] + (q[0] - p[0]) * t,
                        p[1] + (q[1] - p[1]) * t,
                        p[2] + (q[2] - p[2]) * t,
                        1e-6,
                    ],
                    [
                        q[0] + (p[0] - q[0]) * t,
                        q[1] + (p[1] - q[1]) * t,
                        q[2] + (p[2] - q[2]) * t,
                        1e-6,
                    ],
                )
            } else {
                (p, q)
            };
            if let (Some(v0), Some(v1)) = (project(&pa, w, h), project(&qa, w, h)) {
                draw_line(fb, w, h, v0, v1, col(r), col(g), col(b));
                drawn += 1;
            }
        }
    } else {
        raster_tris(
            fb,
            r3,
            w,
            h,
            &clip,
            &world,
            uv,
            vca,
            idx,
            None,
            (r as f32, g as f32, b as f32),
            ambient,
            diffuse,
            &light,
            tex.as_ref(),
            false,
            &mut drawn,
            &mut culled,
        )?;
    }
    r3.drawn += drawn;
    r3.culled += culled;
    Ok(drawn)
}

/// Батч-вершинник: инстансы разворачиваются в общие world/clip/uv/tint-массивы,
/// индексы смещаются, затем ОДНА растеризация на все инстансы.
/// pub(crate): доступен бенчмарку 3D-шейдинга (main.rs, этап 7).
#[allow(clippy::too_many_arguments)]
pub(crate) fn batch_draw(
    hal: &mut Hal,
    pos: &[f32],
    uv: Option<&[f32]>,
    idx: &[f32],
    insts: &[f32],
    mvp: &[f32],
    texid: i64,
) -> Result<u64, String> {
    // wgpu-режим: сброс отложенного 2D-слоя перед программным 3D-батчем
    let _ = hal.flush_gpu();
    if pos.len() % 3 != 0 || pos.is_empty() {
        return Err("gpu3d-batch: позиции — плоский xyz".into());
    }
    let n = pos.len() / 3;
    if idx.len() % 3 != 0 {
        return Err("gpu3d-batch: индексы — тройки вершин".into());
    }
    if insts.len() % 12 != 0 {
        return Err("gpu3d-batch: инстансы — по 12 f32 (t.xyz s.xyz yaw pitch roll r g b)".into());
    }
    if insts.is_empty() {
        return Ok(0); // пустой батч — ничего не рисуем (врагов пока нет)
    }
    if mvp.len() != 16 {
        return Err("gpu3d-batch: mvp 4x4 = 16 чисел".into());
    }
    let k = insts.len() / 12;
    if k * n * 3 > 8_000_000 {
        return Err("gpu3d-batch: слишком много вершин".into());
    }
    let tex = tex_lookup(hal, texid)?;
    if tex.is_some() && uv.is_none() {
        return Err("gpu3d-batch-tex: текстура без uv".into());
    }
    let (w, h) = (hal.w, hal.h);
    if hal.r3.zbuf.len() != w * h {
        hal.r3.reset(w, h);
    }
    let Hal { fb, r3, .. } = &mut *hal;

    // единые буферы на все инстансы
    let mut world = Vec::with_capacity(k * n * 3);
    let mut clip = Vec::with_capacity(k * n * 4);
    let mut uvq: Vec<f32> = Vec::with_capacity(k * n * 2);
    let mut tint: Vec<f32> = Vec::with_capacity(k * n * 3);
    let mut idx_all: Vec<f32> = Vec::with_capacity(k * idx.len());
    let mut m: [f32; 16] = [0.0; 16];

    for i in 0..k {
        let ins = &insts[i * 12..(i + 1) * 12];
        inst_matrix(ins, &mut m);
        let base = i * n;
        for vi in 0..n {
            let x = pos[vi * 3];
            let y = pos[vi * 3 + 1];
            let z = pos[vi * 3 + 2];
            let wx = m[0] * x + m[1] * y + m[2] * z + m[3];
            let wy = m[4] * x + m[5] * y + m[6] * z + m[7];
            let wz = m[8] * x + m[9] * y + m[10] * z + m[11];
            world.push(wx);
            world.push(wy);
            world.push(wz);
            clip.push(mvp[0] * wx + mvp[1] * wy + mvp[2] * wz + mvp[3]);
            clip.push(mvp[4] * wx + mvp[5] * wy + mvp[6] * wz + mvp[7]);
            clip.push(mvp[8] * wx + mvp[9] * wy + mvp[10] * wz + mvp[11]);
            clip.push(mvp[12] * wx + mvp[13] * wy + mvp[14] * wz + mvp[15]);
            if let Some(u) = uv {
                uvq.push(u[vi * 2]);
                uvq.push(u[vi * 2 + 1]);
            }
            tint.push(ins[9]);
            tint.push(ins[10]);
            tint.push(ins[11]);
        }
        for &j in idx {
            idx_all.push(j + base as f32);
        }
    }

    let light = {
        let l = r3.light;
        let len = dot3(&l, &l).sqrt().max(1e-6);
        [l[0] / len, l[1] / len, l[2] / len]
    };
    let (ambient, diffuse) = (r3.ambient, r3.diffuse);
    let mut drawn: u64 = 0;
    let mut culled: u64 = 0;
    raster_tris(
        fb,
        r3,
        w,
        h,
        &clip,
        &world,
        if uv.is_some() { Some(&uvq) } else { None },
        None,
        &idx_all,
        Some(&tint),
        (0.0, 0.0, 0.0),
        ambient,
        diffuse,
        &light,
        tex.as_ref(),
        false,
        &mut drawn,
        &mut culled,
    )?;
    r3.drawn += drawn;
    r3.culled += culled;
    Ok(drawn)
}

/// Прозрачный проход (этап 9): сортируем очередь far→near по видовой глубине
/// центра и рисуем с src-alpha блендингом (z-тест есть, z-записи нет,
/// двусторонне). Порядок в очереди задаёт художник-видимость для крон, воды
/// и облаков — как у настоящего GPU с painter's sort по треугольникам.
pub(crate) fn flush_transparent(hal: &mut Hal) -> Result<u64, String> {
    if hal.r3.transq.is_empty() {
        return Ok(0);
    }
    let _ = hal.flush_gpu();
    let (w, h) = (hal.w, hal.h);
    if hal.r3.zbuf.len() != w * h {
        hal.r3.reset(w, h);
    }
    let mut q = std::mem::take(&mut hal.r3.transq);
    q.sort_by(|a, b| {
        b.depth
            .partial_cmp(&a.depth)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // текстуры резолвим один раз на id (клон px дорог) — до разбора hal
    let mut texcache: Vec<(i64, TexRef)> = Vec::new();
    {
        let mut ids: Vec<i64> = q.iter().map(|t| t.tex).collect();
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            if id != 0 {
                texcache.push((
                    id,
                    tex_lookup(hal, id)?
                        .ok_or_else(|| format!("gpu3d-flush-alpha: нет текстуры {}", id))?,
                ));
            }
        }
    }
    let mut drawn: u64 = 0;
    let Hal { fb, r3, .. } = &mut *hal;
    for t in &q {
        let tex = texcache
            .iter()
            .find(|(id, _)| *id == t.tex)
            .map(|(_, tr)| tr);
        blend_tri(fb, r3, w, h, t, tex, &mut drawn)?;
    }
    r3.drawn += drawn;
    Ok(drawn)
}

/// Один треугольник прозрачного прохода: блендинг, z-тест без z-записи.
#[allow(clippy::too_many_arguments)]
fn blend_tri(
    fb: &mut [u8],
    r3: &mut R3,
    w: usize,
    h: usize,
    t: &TransTri,
    tex: Option<&TexRef>,
    drawn: &mut u64,
) -> Result<(), String> {
    let light = {
        let l = r3.light;
        let len = dot3(&l, &l).sqrt().max(1e-6);
        [l[0] / len, l[1] / len, l[2] / len]
    };
    let (ambient, diffuse) = (r3.ambient, r3.diffuse);
    let spec_on = t.spec > 0.0;
    let fog_on = t.fog[3] > 0.0;
    let (fog_density, fog_r, fog_g, fog_b) = (t.fog[3], t.fog[0], t.fog[1], t.fog[2]);
    let (tu, tv) = (r3.tiling[0], r3.tiling[1]);
    let nnrm = t.nrm;
    let nl_ok = dot3(&nnrm, &nnrm) > 0.5;
    let shade = if !nl_ok {
        ambient
    } else {
        let ndl = dot3(&nnrm, &light).abs(); // двустороннее освещение
        (ambient + diffuse * ndl.max(0.0)).clamp(0.0, 1.0)
    };
    let (p0, p1, p2) = (&t.pts[0], &t.pts[1], &t.pts[2]);
    let (tw, th) = tex.map(|x| (x.w, x.h)).unwrap_or((0, 0));

    let minx = (p0.sx.min(p1.sx).min(p2.sx).floor() as i64).max(0);
    let maxx = (p0.sx.max(p1.sx).max(p2.sx).ceil() as i64).min(w as i64 - 1);
    let miny = (p0.sy.min(p1.sy).min(p2.sy).floor() as i64).max(0);
    let maxy = (p0.sy.max(p1.sy).max(p2.sy).ceil() as i64).min(h as i64 - 1);
    if minx > maxx || miny > maxy {
        return Ok(());
    }
    for py in miny..=maxy {
        let row = py as usize * w;
        let fy = py as f32 + 0.5;
        for px in minx..=maxx {
            let fx = px as f32 + 0.5;
            let w0 = edge(p1.sx, p1.sy, p2.sx, p2.sy, fx, fy);
            let w1 = edge(p2.sx, p2.sy, p0.sx, p0.sy, fx, fy);
            let w2 = edge(p0.sx, p0.sy, p1.sx, p1.sy, fx, fy);
            // двусторонне: внутри полигона веса одного знака
            if (w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0) || (w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0) {
                let sum = w0 + w1 + w2;
                if sum.abs() < 1e-9 {
                    continue;
                }
                let z = (w0 * p0.sz + w1 * p1.sz + w2 * p2.sz) / sum;
                let pi = row + px as usize;
                if z < r3.zbuf[pi] {
                    let iwp = (w0 * p0.iw + w1 * p1.iw + w2 * p2.iw) / sum;
                    if iwp.abs() <= 1e-12 {
                        continue;
                    }
                    let s = if spec_on {
                        let wx = ((w0 * p0.wxq + w1 * p1.wxq + w2 * p2.wxq) / sum) / iwp;
                        let wy = ((w0 * p0.wyq + w1 * p1.wyq + w2 * p2.wyq) / sum) / iwp;
                        let wz = ((w0 * p0.wzq + w1 * p1.wzq + w2 * p2.wzq) / sum) / iwp;
                        let v = [r3.view[0] - wx, r3.view[1] - wy, r3.view[2] - wz];
                        let vl = dot3(&v, &v).sqrt().max(1e-9);
                        let hv = [
                            light[0] + v[0] / vl,
                            light[1] + v[1] / vl,
                            light[2] + v[2] / vl,
                        ];
                        let hl = dot3(&hv, &hv).sqrt().max(1e-9);
                        spec_highlight(
                            dot3(&nnrm, &[hv[0] / hl, hv[1] / hl, hv[2] / hl]),
                            t.shininess,
                            t.spec,
                        )
                    } else {
                        0.0
                    };
                    let fogf = if fog_on {
                        fog_factor(fog_density, 1.0 / iwp)
                    } else {
                        0.0
                    };
                    // per-vertex rgb/alpha (градиент воды/листвы) — всегда
                    // интерполяция: у mesh-vc цвет живёт в вершинах, тинт
                    // для прозрачного прохода не используется
                    let f = 1.0 / iwp;
                    let rq = ((w0 * p0.rq + w1 * p1.rq + w2 * p2.rq) / sum) * f;
                    let gq = ((w0 * p0.gq + w1 * p1.gq + w2 * p2.gq) / sum) * f;
                    let bq = ((w0 * p0.bq + w1 * p1.bq + w2 * p2.bq) / sum) * f;
                    let (mut c0, mut c1, mut c2) = (rq * shade, gq * shade, bq * shade);
                    let mut al =
                        (((w0 * p0.aq + w1 * p1.aq + w2 * p2.aq) / sum) * f).clamp(0.0, 1.0);
                    if let Some(tx) = tex {
                        let uq = (w0 * p0.uq + w1 * p1.uq + w2 * p2.uq) / sum;
                        let vq = (w0 * p0.vq + w1 * p1.vq + w2 * p2.vq) / sum;
                        let uf = tiled_coord(uq / iwp, tu);
                        let vf = tiled_coord(vq / iwp, tv);
                        let uu = (uf.clamp(0.0, 0.99999) * tw as f32) as usize;
                        let vv = (vf.clamp(0.0, 0.99999) * th as f32) as usize;
                        let ti = (vv * tw + uu) * 4;
                        let texel = &tx.px[ti..ti + 4];
                        al *= texel[3] as f32 / 255.0;
                        if al <= 0.004 {
                            continue;
                        }
                        c0 *= texel[0] as f32 / 255.0;
                        c1 *= texel[1] as f32 / 255.0;
                        c2 *= texel[2] as f32 / 255.0;
                    } else if al <= 0.004 {
                        continue;
                    }
                    if spec_on {
                        c0 = (c0 + s).min(1.0);
                        c1 = (c1 + s).min(1.0);
                        c2 = (c2 + s).min(1.0);
                    }
                    if fog_on {
                        let g = 1.0 - fogf;
                        c0 = c0 * g + fog_r * fogf;
                        c1 = c1 * g + fog_g * fogf;
                        c2 = c2 * g + fog_b * fogf;
                    }
                    let o = pi * 4;
                    let dr = fb[o] as f32 / 255.0;
                    let dg = fb[o + 1] as f32 / 255.0;
                    let db = fb[o + 2] as f32 / 255.0;
                    let iv = 1.0 - al;
                    fb[o] = col((c0 * al + dr * iv) as f64);
                    fb[o + 1] = col((c1 * al + dg * iv) as f64);
                    fb[o + 2] = col((c2 * al + db * iv) as f64);
                    fb[o + 3] = 255;
                }
            }
        }
    }
    *drawn += 1;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn raster_tris(
    fb: &mut [u8],
    r3: &mut R3,
    w: usize,
    h: usize,
    clip: &[f32],
    world: &[f32],
    uvq: Option<&[f32]>,
    vca: Option<&[f32]>, // per-vertex rgba (gpu3d-mesh-vc): n*4 f32
    idx: &[f32],
    tint: Option<&[f32]>,   // rgb на вершину (батч: цвет инстанса)
    solid: (f32, f32, f32), // один цвет (обычный mesh)
    ambient: f32,
    diffuse: f32,
    light: &[f32; 3],
    tex: Option<&TexRef>,
    blend_pass: bool, // true = прозрачный проход (блендинг, без z-записи)
    drawn: &mut u64,
    culled: &mut u64,
) -> Result<(), String> {
    let n = world.len() / 3;
    for f in 0..idx.len() / 3 {
        let mut tri = [0usize; 3];
        for k in 0..3 {
            let vi = idx[f * 3 + k];
            // через переменную: без !(a >= b) — NaN-семантика прежняя
            let idx_ok = vi >= 0.0 && vi.fract() == 0.0 && (vi as usize) < n;
            if !idx_ok {
                return Err(format!("gpu3d: индекс за границей: {}", vi));
            }
            tri[k] = vi as usize;
        }
        // полигон в клип-пространстве + атрибуты u/w, v/w, 1/w, мир/w, a/w, rgb/w
        let blend_base = if r3.blend > 0.0 {
            r3.blend.min(1.0)
        } else {
            1.0
        };
        let base_rgb = match tint {
            Some(t) => [t[tri[0] * 3], t[tri[0] * 3 + 1], t[tri[0] * 3 + 2]],
            None => [solid.0, solid.1, solid.2],
        };
        let mut poly: Vec<[f32; 14]> = Vec::with_capacity(3);
        for &t in &tri {
            let cw = clip[t * 4 + 3];
            let iw = if cw > 1e-6 { 1.0 / cw } else { 0.0 };
            // перспективно-корректные атрибуты: в полигон идут u/w, v/w,
            // мировая позиция /w, альфа/w и rgb/w (аффинны на ребре)
            let (uq, vq) = if let Some(u) = uvq {
                (u[t * 2] * iw, u[t * 2 + 1] * iw)
            } else {
                (0.0, 0.0)
            };
            let (a, rr, gg, bb) = if let Some(vc) = vca {
                (vc[t * 4 + 3], vc[t * 4], vc[t * 4 + 1], vc[t * 4 + 2])
            } else {
                (blend_base, base_rgb[0], base_rgb[1], base_rgb[2])
            };
            poly.push([
                clip[t * 4],
                clip[t * 4 + 1],
                clip[t * 4 + 2],
                cw,
                uq,
                vq,
                iw,
                world[t * 3] * iw,
                world[t * 3 + 1] * iw,
                world[t * 3 + 2] * iw,
                a * iw,
                rr * iw,
                gg * iw,
                bb * iw,
            ]);
        }

        let poly = if poly.iter().any(|v| v[3] < 1e-6) {
            let clipped = clip_near6(&poly);
            if clipped.len() < 3 {
                *culled += 1;
                continue;
            }
            clipped
        } else {
            poly
        };

        let mut pts = Vec::with_capacity(poly.len());
        let mut ok = true;
        for v in &poly {
            match project6(v, w, h) {
                Some(p) => pts.push(p),
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            *culled += 1;
            continue;
        }

        // — состояние материалов (этап 7): выключенные фичи — ноль расходов —
        let spec_on = r3.spec > 0.0;
        let (spec, shininess) = (r3.spec, r3.shininess);
        let fog_on = r3.fog[3] > 0.0;
        let (fog_density, fog_r, fog_g, fog_b) = (r3.fog[3], r3.fog[0], r3.fog[1], r3.fog[2]);
        let fog_snap = r3.fog;
        let alpha_on = r3.alpha_test > 0.0;
        let alpha_thr = r3.alpha_test;
        let (tu, tv) = (r3.tiling[0], r3.tiling[1]);

        // вентилятор: (0, k, k+1)
        for kk in 1..pts.len() - 1 {
            let (p0, p1, p2) = (&pts[0], &pts[kk], &pts[kk + 1]);
            // backface cull: фронт = CCW в NDC → после y-flip площадь < 0.
            // Куллинг ПОЗЖЕ — прозрачные грани двусторонни, задние тоже
            // должны попасть в прозрачную очередь (решение ниже).
            let area = edge(p0.sx, p0.sy, p1.sx, p1.sy, p2.sx, p2.sy);
            // нормаль грани в мире (flat shading)
            let (wa, wb, wc) = (
                [
                    world[tri[0] * 3],
                    world[tri[0] * 3 + 1],
                    world[tri[0] * 3 + 2],
                ],
                [
                    world[tri[1] * 3],
                    world[tri[1] * 3 + 1],
                    world[tri[1] * 3 + 2],
                ],
                [
                    world[tri[2] * 3],
                    world[tri[2] * 3 + 1],
                    world[tri[2] * 3 + 2],
                ],
            );
            let e1 = [wb[0] - wa[0], wb[1] - wa[1], wb[2] - wa[2]];
            let e2 = [wc[0] - wa[0], wc[1] - wa[1], wc[2] - wa[2]];
            let nrm = cross3(&e1, &e2);
            let nl = dot3(&nrm, &nrm).sqrt();
            let nnrm = if nl < 1e-9 {
                [0.0f32; 3]
            } else {
                [nrm[0] / nl, nrm[1] / nl, nrm[2] / nl]
            };
            // прозрачный проход двусторонний: нормаль разворачиваем к свету
            let shade = if nl < 1e-9 {
                ambient
            } else {
                let mut ndl = dot3(&nnrm, light);
                if blend_pass && ndl < 0.0 {
                    ndl = -ndl;
                }
                (ambient + diffuse * ndl.max(0.0)).clamp(0.0, 1.0)
            };

            // — прозрачная очередь (этап 9): альфа треугольника = средняя
            // перспектива-корректная альфа вершин; < 1 → ставим в очередь
            // (сорт far→near и блендинг — в gpu3d-flush-alpha) —
            let va = [
                if p0.iw > 1e-12 { p0.aq / p0.iw } else { 1.0 },
                if p1.iw > 1e-12 { p1.aq / p1.iw } else { 1.0 },
                if p2.iw > 1e-12 { p2.aq / p2.iw } else { 1.0 },
            ];
            let al_avg = (va[0] + va[1] + va[2]) / 3.0;
            if !blend_pass && al_avg < 0.999 {
                let depth = 1.0 / 3.0
                    * (if p0.iw > 1e-12 { 1.0 / p0.iw } else { 0.0 }
                        + if p1.iw > 1e-12 { 1.0 / p1.iw } else { 0.0 }
                        + if p2.iw > 1e-12 { 1.0 / p2.iw } else { 0.0 });
                r3.transq.push(TransTri {
                    depth,
                    pts: [*p0, *p1, *p2],
                    nrm: nnrm,
                    tex: tex.map(|t| t.id).unwrap_or(0),
                    spec,
                    shininess,
                    fog: fog_snap,
                });
                continue;
            }
            // непрозрачный проход: задняя грань — кулим (прозрачные прошли выше)
            if !blend_pass && area >= -1e-6 {
                *culled += 1;
                continue;
            }
            // цвет грани: per-vertex RGB (вода/небо) или тинт первой вершины
            let vc_rgb = !blend_pass
                && p0.rq == p1.rq
                && p0.rq == p2.rq
                && p0.gq == p1.gq
                && p0.gq == p2.gq
                && p0.bq == p1.bq
                && p0.bq == p2.bq;
            let (tr, tg, tb) = if vc_rgb {
                (p0.rq / p0.iw, p0.gq / p0.iw, p0.bq / p0.iw)
            } else {
                match tint {
                    Some(t) => (t[tri[0] * 3], t[tri[0] * 3 + 1], t[tri[0] * 3 + 2]),
                    None => solid,
                }
            };

            let (tw, th) = tex.map(|t| (t.w, t.h)).unwrap_or((0, 0));

            // bbox
            let minx = (p0.sx.min(p1.sx).min(p2.sx).floor() as i64).max(0);
            let maxx = (p0.sx.max(p1.sx).max(p2.sx).ceil() as i64).min(w as i64 - 1);
            let miny = (p0.sy.min(p1.sy).min(p2.sy).floor() as i64).max(0);
            let maxy = (p0.sy.max(p1.sy).max(p2.sy).ceil() as i64).min(h as i64 - 1);
            if minx > maxx || miny > maxy {
                continue;
            }
            for py in miny..=maxy {
                let row = py as usize * w;
                let fy = py as f32 + 0.5;
                for px in minx..=maxx {
                    let fx = px as f32 + 0.5;
                    let w0 = edge(p1.sx, p1.sy, p2.sx, p2.sy, fx, fy);
                    let w1 = edge(p2.sx, p2.sy, p0.sx, p0.sy, fx, fy);
                    let w2 = edge(p0.sx, p0.sy, p1.sx, p1.sy, fx, fy);
                    // площадь < 0 для лицевых → пиксель внутри, когда все веса <= 0
                    if w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0 {
                        let sum = w0 + w1 + w2;
                        let z = if sum.abs() < 1e-9 {
                            p0.sz
                        } else {
                            (w0 * p0.sz + w1 * p1.sz + w2 * p2.sz) / sum
                        };
                        let pi = row + px as usize;
                        if z < r3.zbuf[pi] {
                            // перспектива-корректный 1/w пикселя; истинная
                            // глубина = 1/(1/w) = w_clip = -z_view (перспектива
                            // render3d.lisp), она же — глубина тумана
                            let iwp = (w0 * p0.iw + w1 * p1.iw + w2 * p2.iw) / sum;
                            if iwp.abs() <= 1e-12 {
                                // деградированный iw: z пишется (opaque), цвет нет
                                if !blend_pass {
                                    r3.zbuf[pi] = z;
                                }
                                continue;
                            }
                            // блик Blinn-Phong (этап 7): half-vector на
                            // перспектива-корректной мировой позиции пикселя
                            let s = if spec_on {
                                let wx = ((w0 * p0.wxq + w1 * p1.wxq + w2 * p2.wxq) / sum) / iwp;
                                let wy = ((w0 * p0.wyq + w1 * p1.wyq + w2 * p2.wyq) / sum) / iwp;
                                let wz = ((w0 * p0.wzq + w1 * p1.wzq + w2 * p2.wzq) / sum) / iwp;
                                let v = [r3.view[0] - wx, r3.view[1] - wy, r3.view[2] - wz];
                                let vl = dot3(&v, &v).sqrt().max(1e-9);
                                let hv = [
                                    light[0] + v[0] / vl,
                                    light[1] + v[1] / vl,
                                    light[2] + v[2] / vl,
                                ];
                                let hl = dot3(&hv, &hv).sqrt().max(1e-9);
                                spec_highlight(
                                    dot3(&nnrm, &[hv[0] / hl, hv[1] / hl, hv[2] / hl]),
                                    shininess,
                                    spec,
                                )
                            } else {
                                0.0
                            };
                            // туман (этап 7): фактор по истинной глубине,
                            // смешение ПОСЛЕ шейдинга и блика
                            let fogf = if fog_on {
                                fog_factor(fog_density, 1.0 / iwp)
                            } else {
                                0.0
                            };
                            // базовый цвет пикселя: per-vertex градиент (этап 9)
                            // или равномерный цвет грани × Lambert
                            let (mut c0, mut c1, mut c2) = if !vc_rgb {
                                let f = 1.0 / iwp;
                                let rq = ((w0 * p0.rq + w1 * p1.rq + w2 * p2.rq) / sum) * f;
                                let gq = ((w0 * p0.gq + w1 * p1.gq + w2 * p2.gq) / sum) * f;
                                let bq = ((w0 * p0.bq + w1 * p1.bq + w2 * p2.bq) / sum) * f;
                                (rq * shade, gq * shade, bq * shade)
                            } else {
                                (tr * shade, tg * shade, tb * shade)
                            };
                            let mut al = if blend_pass {
                                let f = 1.0 / iwp;
                                (((w0 * p0.aq + w1 * p1.aq + w2 * p2.aq) / sum) * f).clamp(0.0, 1.0)
                            } else {
                                1.0
                            };
                            if let Some(t) = tex {
                                // перспектива-корректный UV: (u/w)/(1/w)
                                let uq = (w0 * p0.uq + w1 * p1.uq + w2 * p2.uq) / sum;
                                let vq = (w0 * p0.vq + w1 * p1.vq + w2 * p2.vq) / sum;
                                // тайлинг (этап 7): (1,1) — clamp к краю,
                                // иначе repeat через fract
                                let uf = tiled_coord(uq / iwp, tu);
                                let vf = tiled_coord(vq / iwp, tv);
                                let uu = (uf.clamp(0.0, 0.99999) * tw as f32) as usize;
                                let vv = (vf.clamp(0.0, 0.99999) * th as f32) as usize;
                                let ti = (vv * tw + uu) * 4;
                                let texel = &t.px[ti..ti + 4];
                                // alpha-test (этап 7, только opaque): discard
                                // ДО записи z — за дыркой видно то, что позади
                                if alpha_on && !blend_pass && !alpha_keep(texel[3], alpha_thr) {
                                    continue;
                                }
                                if blend_pass {
                                    al *= texel[3] as f32 / 255.0;
                                    if al <= 0.004 {
                                        continue; // полностью прозрачно
                                    }
                                }
                                c0 *= texel[0] as f32 / 255.0;
                                c1 *= texel[1] as f32 / 255.0;
                                c2 *= texel[2] as f32 / 255.0;
                            } else if blend_pass && al <= 0.004 {
                                continue;
                            }
                            if spec_on {
                                c0 = (c0 + s).min(1.0);
                                c1 = (c1 + s).min(1.0);
                                c2 = (c2 + s).min(1.0);
                            }
                            if fog_on {
                                let g = 1.0 - fogf;
                                c0 = c0 * g + fog_r * fogf;
                                c1 = c1 * g + fog_g * fogf;
                                c2 = c2 * g + fog_b * fogf;
                            }
                            let o = pi * 4;
                            if blend_pass {
                                // src-alpha блендинг поверх кадра; z-буфер
                                // НЕ пишется (порядок даёт сорт far→near)
                                let dr = fb[o] as f32 / 255.0;
                                let dg = fb[o + 1] as f32 / 255.0;
                                let db = fb[o + 2] as f32 / 255.0;
                                let iv = 1.0 - al;
                                fb[o] = col((c0 * al + dr * iv) as f64);
                                fb[o + 1] = col((c1 * al + dg * iv) as f64);
                                fb[o + 2] = col((c2 * al + db * iv) as f64);
                            } else {
                                r3.zbuf[pi] = z;
                                fb[o] = col(c0 as f64);
                                fb[o + 1] = col(c1 as f64);
                                fb[o + 2] = col(c2 as f64);
                            }
                            fb[o + 3] = 255;
                        }
                    }
                }
            }
            *drawn += 1;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn draw_line(fb: &mut [u8], w: usize, h: usize, a: Vtx, b: Vtx, r: u8, g: u8, bl: u8) {
    let steps = ((b.sx - a.sx).abs().max((b.sy - a.sy).abs()).ceil() as usize).clamp(1, 4096);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = (a.sx + (b.sx - a.sx) * t).round() as i64;
        let y = (a.sy + (b.sy - a.sy) * t).round() as i64;
        if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
            continue;
        }
        let o = (y as usize * w + x as usize) * 4;
        fb[o] = r;
        fb[o + 1] = g;
        fb[o + 2] = bl;
        fb[o + 3] = 255;
    }
}

fn check_tex_uv(uv: Option<&[f32]>, n: usize) -> Result<(), String> {
    if let Some(u) = uv {
        if u.len() != n * 2 {
            return Err(format!(
                "gpu3d: uv — пары на вершину ({}), вершин {}",
                u.len(),
                n
            ));
        }
    }
    Ok(())
}

pub fn register_render3d(env: Gc, hal: &Rc<RefCell<Hal>>) {
    fn def<F: Fn(&[Value]) -> Result<Value, String> + 'static>(env: Gc, name: &str, f: F) {
        env_define(env, name, Value::Native(Rc::new(f), name.to_string()));
    }
    use need_args as need;

    let hc = hal.clone();
    def(env, "gpu3d-clear", move |args| {
        need(args, 3)?;
        let r = col(num_val(&args[0])?);
        let g = col(num_val(&args[1])?);
        let b = col(num_val(&args[2])?);
        let mut h = hc.borrow_mut();
        let (w, hh) = (h.w, h.h);
        // wgpu-режим: 3D-очистка — программная, но сначала сбрасываем отложенный
        // 2D-слой, чтобы он не «всплыл» посреди 3D-кадра
        let _ = h.flush_gpu();
        h.r3.reset(w, hh);
        h.r3.transq.clear();
        h.fill_all(r, g, b);
        h.r3.zbuf.fill(f32::INFINITY);
        h.r3.drawn = 0;
        h.r3.culled = 0;
        Ok(Value::Nil)
    });

    // свет: (gpu3d-light dx dy dz ambient [diffuse [spec [shininess]]]) —
    // опц. аргументы этапа 7: сила блика Blinn-Phong и его показатель.
    // Прежние вызовы (4-5 аргументов) ведут себя байт-в-байт как раньше.
    let hc = hal.clone();
    def(env, "gpu3d-light", move |args| {
        let mut h = hc.borrow_mut();
        apply_light_args(&mut h.r3, args)?;
        Ok(Value::Nil)
    });

    // позиция камеры в мире (для half-vector блика Blinn-Phong)
    let hc = hal.clone();
    def(env, "gpu3d-view", move |args| {
        need(args, 3)?;
        let mut h = hc.borrow_mut();
        h.r3.view = [
            num_val(&args[0])? as f32,
            num_val(&args[1])? as f32,
            num_val(&args[2])? as f32,
        ];
        Ok(Value::Nil)
    });

    // тайлинг UV: (gpu3d-mat-tiling u v) — множители перед семплингом
    // (repeat-текстуры: трава, стены). (gpu3d-mat-tiling 1 1) — сброс/выкл.
    let hc = hal.clone();
    def(env, "gpu3d-mat-tiling", move |args| {
        need(args, 2)?;
        let mut h = hc.borrow_mut();
        h.r3.tiling = [
            num_val(&args[0])?.clamp(0.0, 1024.0) as f32,
            num_val(&args[1])?.clamp(0.0, 1024.0) as f32,
        ];
        Ok(Value::Nil)
    });

    // alpha-test (cutout): (gpu3d-alpha-test threshold) —
    // 0 = выкл (дефолт); 0.1..1 — тексели с alpha < порога отбрасываются
    // ДО записи в z-буфер (за дыркой в билборде видно то, что позади).
    let hc = hal.clone();
    def(env, "gpu3d-alpha-test", move |args| {
        need(args, 1)?;
        let mut h = hc.borrow_mut();
        h.r3.alpha_test = num_val(&args[0])?.clamp(0.0, 1.0) as f32;
        Ok(Value::Nil)
    });

    // туман: (gpu3d-fog r g b density) — экспоненциальный, по истинной
    // глубине (1/w), смешение после шейдинга; density 0 = выкл (дефолт).
    let hc = hal.clone();
    def(env, "gpu3d-fog", move |args| {
        need(args, 4)?;
        let mut h = hc.borrow_mut();
        h.r3.fog = [
            num_val(&args[0])?.clamp(0.0, 1.0) as f32,
            num_val(&args[1])?.clamp(0.0, 1.0) as f32,
            num_val(&args[2])?.clamp(0.0, 1.0) as f32,
            num_val(&args[3])?.clamp(0.0, 100.0) as f32,
        ];
        Ok(Value::Nil)
    });

    // инспекция состояния конвейера (материалы/редактор/отладка):
    // таблица с ключами light/ambient/diffuse/spec/shininess/view/tiling/
    // alpha-test/fog
    let hc = hal.clone();
    def(env, "gpu3d-state", move |_: &[Value]| {
        let h = hc.borrow();
        let r3 = &h.r3;
        let v3 = |a: [f32; 3]| {
            Value::List(vec![
                Value::Float(a[0] as f64),
                Value::Float(a[1] as f64),
                Value::Float(a[2] as f64),
            ])
        };
        let mut t = std::collections::HashMap::new();
        t.insert("light".into(), v3(r3.light));
        t.insert("ambient".into(), Value::Float(r3.ambient as f64));
        t.insert("diffuse".into(), Value::Float(r3.diffuse as f64));
        t.insert("spec".into(), Value::Float(r3.spec as f64));
        t.insert("shininess".into(), Value::Float(r3.shininess as f64));
        t.insert("view".into(), v3(r3.view));
        t.insert(
            "tiling".into(),
            Value::List(vec![
                Value::Float(r3.tiling[0] as f64),
                Value::Float(r3.tiling[1] as f64),
            ]),
        );
        t.insert("alpha-test".into(), Value::Float(r3.alpha_test as f64));
        t.insert("blend".into(), Value::Float(r3.blend as f64));
        t.insert(
            "fog".into(),
            Value::List(vec![
                Value::Float(r3.fog[0] as f64),
                Value::Float(r3.fog[1] as f64),
                Value::Float(r3.fog[2] as f64),
                Value::Float(r3.fog[3] as f64),
            ]),
        );
        Ok(Value::Table(Rc::new(RefCell::new(t))))
    });

    // alpha-blend (этап 9): (gpu3d-alpha-blend opacity) — равномерная альфа
    // материала; всё с итоговой альфой < 1 уходит в прозрачную очередь
    // (gpu3d-flush-alpha: сорт far→near, src-alpha блендинг, без z-записи).
    // 1 (или 0) = непрозрачно, прежнее поведение.
    let hc = hal.clone();
    def(env, "gpu3d-alpha-blend", move |args| {
        need(args, 1)?;
        let mut h = hc.borrow_mut();
        h.r3.blend = num_val(&args[0])?.clamp(0.0, 1.0) as f32;
        Ok(Value::Nil)
    });

    // дренаж прозрачной очереди: сорт far→near + блендинг
    let hc = hal.clone();
    def(env, "gpu3d-flush-alpha", move |_: &[Value]| {
        let mut h = hc.borrow_mut();
        let n = flush_transparent(&mut h)?;
        Ok(Value::Int(n as i64))
    });

    // mesh с per-vertex RGBA (этап 9):
    // (gpu3d-mesh-vc pos uv rgba idx model mvp [tex]) — rgba: n*4 f32 0..1;
    // альфа/цвет градиентны (вода: френель/пена/глубина; тени: glow).
    // uv обязателен при tex ≠ 0; tex 0 = без текстуры. Вершины с альфой < 1 —
    // в прозрачную очередь.
    let hc = hal.clone();
    def(env, "gpu3d-mesh-vc", move |args| {
        let n = args.len();
        if n != 7 && n != 8 {
            return Err("gpu3d-mesh-vc: (pos uv rgba idx model mvp [tex])".into());
        }
        let pos = to_f32s(&args[0])?;
        let uv = to_f32s(&args[1])?;
        let rgba = to_f32s(&args[2])?;
        let idx = to_f32s(&args[3])?;
        let model = to_f32s(&args[4])?;
        let mvp = to_f32s(&args[5])?;
        let texid = if n == 8 {
            crate::interp::int_val(&args[6])?
        } else {
            0
        };
        let mut h = hc.borrow_mut();
        let drawn = mesh_draw(
            &mut h,
            &pos,
            Some(&uv),
            Some(&rgba),
            &idx,
            &model,
            &mvp,
            1.0,
            1.0,
            1.0,
            texid,
            Prim::Tri,
        )?;
        Ok(Value::Int(drawn as i64))
    });

    let hc = hal.clone();
    def(env, "gpu3d-mesh", move |args| {
        need(args, 7)?;
        let pos = to_f32s(&args[0])?;
        let idx = to_f32s(&args[1])?;
        let model = to_f32s(&args[2])?;
        let mvp = to_f32s(&args[3])?;
        let r = num_val(&args[4])?;
        let g = num_val(&args[5])?;
        let b = num_val(&args[6])?;
        let mut h = hc.borrow_mut();
        let n = mesh_draw(
            &mut h,
            &pos,
            None,
            None,
            &idx,
            &model,
            &mvp,
            r,
            g,
            b,
            0,
            Prim::Tri,
        )?;
        Ok(Value::Int(n as i64))
    });

    // текстурированный mesh: (gpu3d-mesh-tex pos uv idx tex model mvp r g b)
    let hc = hal.clone();
    def(env, "gpu3d-mesh-tex", move |args| {
        need(args, 9)?;
        let pos = to_f32s(&args[0])?;
        let uv = to_f32s(&args[1])?;
        let idx = to_f32s(&args[2])?;
        let texid = crate::interp::int_val(&args[3])?;
        let model = to_f32s(&args[4])?;
        let mvp = to_f32s(&args[5])?;
        let r = num_val(&args[6])?;
        let g = num_val(&args[7])?;
        let b = num_val(&args[8])?;
        check_tex_uv(Some(&uv), pos.len() / 3)?;
        let mut h = hc.borrow_mut();
        let n = mesh_draw(
            &mut h,
            &pos,
            Some(&uv),
            None,
            &idx,
            &model,
            &mvp,
            r,
            g,
            b,
            texid,
            Prim::Tri,
        )?;
        Ok(Value::Int(n as i64))
    });

    // батч: (gpu3d-batch pos idx insts mvp) — цвет/трансформ в инстансах
    let hc = hal.clone();
    def(env, "gpu3d-batch", move |args| {
        need(args, 4)?;
        let pos = to_f32s(&args[0])?;
        let idx = to_f32s(&args[1])?;
        let insts = to_f32s(&args[2])?;
        let mvp = to_f32s(&args[3])?;
        let mut h = hc.borrow_mut();
        let n = batch_draw(&mut h, &pos, None, &idx, &insts, &mvp, 0)?;
        Ok(Value::Int(n as i64))
    });

    // текстурный батч: (gpu3d-batch-tex pos uv idx tex insts mvp)
    let hc = hal.clone();
    def(env, "gpu3d-batch-tex", move |args| {
        need(args, 6)?;
        let pos = to_f32s(&args[0])?;
        let uv = to_f32s(&args[1])?;
        let idx = to_f32s(&args[2])?;
        let texid = crate::interp::int_val(&args[3])?;
        let insts = to_f32s(&args[4])?;
        let mvp = to_f32s(&args[5])?;
        check_tex_uv(Some(&uv), pos.len() / 3)?;
        let mut h = hc.borrow_mut();
        let n = batch_draw(&mut h, &pos, Some(&uv), &idx, &insts, &mvp, texid)?;
        Ok(Value::Int(n as i64))
    });

    let hc = hal.clone();
    def(env, "gpu3d-wire", move |args| {
        need(args, 7)?;
        let pos = to_f32s(&args[0])?;
        let idx = to_f32s(&args[1])?;
        let model = to_f32s(&args[2])?;
        let mvp = to_f32s(&args[3])?;
        let r = num_val(&args[4])?;
        let g = num_val(&args[5])?;
        let b = num_val(&args[6])?;
        let mut h = hc.borrow_mut();
        let n = mesh_draw(
            &mut h,
            &pos,
            None,
            None,
            &idx,
            &model,
            &mvp,
            r,
            g,
            b,
            0,
            Prim::Wire,
        )?;
        Ok(Value::Int(n as i64))
    });

    let hc = hal.clone();
    def(env, "gpu3d-stats", move |_: &[Value]| {
        let h = hc.borrow();
        Ok(Value::List(vec![
            Value::Int(h.r3.drawn as i64),
            Value::Int(h.r3.culled as i64),
        ]))
    });
}

// ————————————————————————————————————————————————————————————————————————
// Тесты этапа 7 «материалы»: чистые функции ядра шейдинга + разбор
// аргументов света с фолбэком. Растеризация покрывается интеграционно
// (smoke.sh + --capture в M3), здесь — математика.
// ————————————————————————————————————————————————————————————————————————

#[cfg(test)]
mod tests {
    use super::*;

    fn близко(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn кривая_тумана_экспоненциальная_и_монотонная() {
        // density 0 или глубина 0 — тумана нет
        assert!(близко(fog_factor(0.0, 100.0), 0.0));
        assert!(близко(fog_factor(0.25, 0.0), 0.0));
        // эталонные точки: 1 - exp(-d*x)
        assert!(близко(fog_factor(0.1, 10.0), 1.0 - (-1.0f32).exp()));
        assert!(близко(fog_factor(1.0, 5.0), 1.0 - (-5.0f32).exp()));
        assert!(близко(fog_factor(2.0, 2.0), 1.0 - (-4.0f32).exp()));
        // монотонность по обеим переменным, насыщение ≤ 1
        assert!(fog_factor(0.1, 20.0) > fog_factor(0.1, 10.0));
        assert!(fog_factor(0.2, 10.0) > fog_factor(0.1, 10.0));
        assert!(fog_factor(100.0, 100.0) <= 1.0);
        // отрицательные входы (мусор от скрипта) не паникуют и не дают < 0
        assert!(близко(fog_factor(-1.0, 10.0), 0.0));
        assert!(близко(fog_factor(0.1, -5.0), 0.0));
    }

    #[test]
    fn альфа_тест_граница_включительная() {
        // выкл: порог 0 пропускает всё, включая полностью прозрачное
        assert!(alpha_keep(0, 0.0));
        // граница: alpha/255 == порог — тексель ОСТАЁТСЯ
        assert!(alpha_keep(26, 0.1)); // 26/255 = 0.10196 ≥ 0.1
        assert!(!alpha_keep(25, 0.1)); // 25/255 = 0.09804 < 0.1
                                       // полный альфа-канал проходит даже при пороге 1
        assert!(alpha_keep(255, 1.0));
        assert!(!alpha_keep(254, 1.0));
        // порог 0.5: граница 127/128
        assert!(!alpha_keep(127, 0.5)); // 0.498 < 0.5
        assert!(alpha_keep(128, 0.5)); // 0.502 ≥ 0.5
    }

    #[test]
    fn тайлинг_математика_repeat_и_пасsthrough() {
        // tiling 1 — координата без изменений (старое поведение clamp)
        assert!(близко(tiled_coord(0.75, 1.0), 0.75));
        assert!(близко(tiled_coord(0.0, 1.0), 0.0));
        assert!(близко(tiled_coord(1.0, 1.0), 1.0));
        // repeat: u=0.75 при tiling 2 → 1.5 → fract 0.5
        assert!(близко(tiled_coord(0.75, 2.0), 0.5));
        // u=1.0 при tiling 2 → 2.0 → fract 0 (шов стыкуется с началом)
        assert!(близко(tiled_coord(1.0, 2.0), 0.0));
        // u=1.3 при tiling 3 → 3.9 → fract 0.9
        assert!(близко(tiled_coord(1.3, 3.0), 0.9));
        // нулевой tiling схлопывает координату в 0
        assert!(близко(tiled_coord(0.42, 0.0), 0.0));
    }

    #[test]
    fn блик_блинн_фонга_нулевые_и_граничные() {
        // сила 0 — блика нет (старый Lambert-путь, ноль накладных расходов)
        assert!(близко(spec_highlight(0.9, 32.0, 0.0), 0.0));
        // N·H ≤ 0 — блик нет (half-vector за гранью)
        assert!(близко(spec_highlight(0.0, 32.0, 1.0), 0.0));
        assert!(близко(spec_highlight(-0.5, 32.0, 1.0), 0.0));
        // идеальное отражение: N·H = 1 → ровно strength (любая shininess)
        assert!(близко(spec_highlight(1.0, 32.0, 0.7), 0.7));
        assert!(близко(spec_highlight(1.0, 1.0, 0.7), 0.7));
        // рост показателя сужает блик: при N·H=0.5 и shininess 1 → 0.5*strength
        assert!(близко(spec_highlight(0.5, 1.0, 1.0), 0.5));
        assert!(spec_highlight(0.5, 64.0, 1.0) < spec_highlight(0.5, 8.0, 1.0));
        // shininess < 1 из скрипта поднимается до 1 (powf корректен)
        assert!(близко(spec_highlight(0.5, 0.0, 1.0), 0.5));
    }

    #[test]
    fn свет_разбор_аргументов_фолбэк_прежнее_состояние() {
        let f = |x: f64| Value::Float(x);
        // старый вызов 4 аргументов: spec остаётся прежним (0 по умолчанию)
        let mut r3 = R3::new();
        apply_light_args(&mut r3, &[f(0.1), f(0.2), f(0.3), f(0.4)]).unwrap();
        assert!(близко(r3.light[0], 0.1));
        assert!(близко(r3.ambient, 0.4));
        assert!(близко(r3.diffuse, 0.75)); // дефолт не тронут
        assert!(близко(r3.spec, 0.0));
        assert!(близко(r3.shininess, 32.0));
        // 5 аргументов (как было в v0.2+): diffuse задаётся, spec не трогается
        let mut r3 = R3::new();
        apply_light_args(&mut r3, &[f(0.0), f(1.0), f(0.0), f(0.2), f(0.9)]).unwrap();
        assert!(близко(r3.diffuse, 0.9));
        assert!(близко(r3.spec, 0.0));
        // 7 аргументов: включает блик
        apply_light_args(
            &mut r3,
            &[f(0.0), f(1.0), f(0.0), f(0.2), f(0.9), f(0.6), f(16.0)],
        )
        .unwrap();
        assert!(близко(r3.spec, 0.6));
        assert!(близко(r3.shininess, 16.0));
        // фолбэк: 6 аргументов — shininess сохраняет прежнее значение
        apply_light_args(&mut r3, &[f(0.0), f(1.0), f(0.0), f(0.2), f(0.9), f(0.3)]).unwrap();
        assert!(близко(r3.spec, 0.3));
        assert!(близко(r3.shininess, 16.0));
        // мусорные значения: отрицательный spec/shininess срезаются
        apply_light_args(
            &mut r3,
            &[f(0.0), f(1.0), f(0.0), f(0.2), f(0.9), f(-1.0), f(0.0)],
        )
        .unwrap();
        assert!(близко(r3.spec, 0.0));
        assert!(близко(r3.shininess, 1.0));
        // мало аргументов — честная ошибка, состояние не меняется
        let mut r3 = R3::new();
        assert!(apply_light_args(&mut r3, &[f(0.1), f(0.2), f(0.3)]).is_err());
        assert!(близко(r3.light[0], 0.45)); // дефолт
    }

    // ——— этап 9: прозрачная очередь, блендинг, mesh-vc ———

    /// Квадрат-во-весь-экран своего цвета на глубине z (плоский свет).
    fn квадрат_цвета(hal: &mut Hal, z: f32, r: f64, g: f64, b: f64) {
        let pos = [-1.0f32, -1.0, z, 1.0, -1.0, z, 1.0, 1.0, z, -1.0, 1.0, z];
        let idx = [0.0f32, 1.0, 2.0, 0.0, 2.0, 3.0];
        let mm = ед_матрицы();
        let (model, mvp) = (&mm[0..16], &mm[16..32]);
        mesh_draw(
            hal,
            &pos,
            None,
            None,
            &idx,
            model,
            mvp,
            r,
            g,
            b,
            0,
            Prim::Tri,
        )
        .unwrap();
    }

    /// Квадрат с обратной (CW) ориентацией — задняя сторона.
    fn квадрат_тыл(hal: &mut Hal, z: f32, r: f64, g: f64, b: f64) {
        let pos = [-1.0f32, -1.0, z, 1.0, -1.0, z, 1.0, 1.0, z, -1.0, 1.0, z];
        let idx = [0.0f32, 2.0, 1.0, 0.0, 3.0, 2.0];
        let mm = ед_матрицы();
        let (model, mvp) = (&mm[0..16], &mm[16..32]);
        mesh_draw(
            hal,
            &pos,
            None,
            None,
            &idx,
            model,
            mvp,
            r,
            g,
            b,
            0,
            Prim::Tri,
        )
        .unwrap();
    }

    #[test]
    fn блендинг_сорт_far_near_сквозной() {
        let mut hal = Hal::new(OutputMode::Silent, 8, 8);
        hal.fill_all(0, 0, 0);
        свет_плоский(&mut hal);
        hal.r3.blend = 0.5;
        // порядок постановки НЕ важен: далёкий зелёный, близкий красный
        квадрат_цвета(&mut hal, 0.9, 0.0, 1.0, 0.0);
        квадрат_цвета(&mut hal, 0.5, 1.0, 0.0, 0.0);
        flush_transparent(&mut hal).unwrap();
        // far→near: зелёный 0.5 поверх чёрного → (0, 128, 0);
        // красный 0.5 поверх → (128, 64, 0). Перепутанный порядок дал бы
        // (128, 128, 0).
        let c = пиксель(&hal, 4, 4);
        assert_eq!(c, [128, 64, 0, 255], "сорт far→near сломан: {:?}", c);
        // z-записи нет: z-буфер в точке остался бесконечным
        assert!(hal.r3.zbuf[4 * 8 + 4].is_infinite());
    }

    #[test]
    fn блендинг_z_тест_в_обе_стороны() {
        let mut hal = Hal::new(OutputMode::Silent, 8, 8);
        hal.fill_all(0, 0, 0);
        свет_плоский(&mut hal);
        // непрозрачный КРАСНЫЙ вписывается первым на z=0.6
        hal.r3.blend = 1.0;
        квадрат_цвета(&mut hal, 0.6, 1.0, 0.0, 0.0);
        // прозрачный ЗЕЛЁНЫЙ ПЕРЕД ним (z=0.4): блендинг поверх
        hal.r3.blend = 0.5;
        квадрат_цвета(&mut hal, 0.4, 0.0, 1.0, 0.0);
        // прозрачный СИНИЙ ЗА ним (z=0.8): z-тест не пропускает
        квадрат_цвета(&mut hal, 0.8, 0.0, 0.0, 1.0);
        flush_transparent(&mut hal).unwrap();
        let c = пиксель(&hal, 4, 4);
        // 0.5 зелёного поверх красного
        assert_eq!(c[0], col(0.5));
        assert_eq!(c[1], col(0.5));
        assert_eq!(c[2], 0, "синий за непрозрачным не должен пройти z-тест");
    }

    #[test]
    fn прозрачный_проход_двусторонний() {
        let mut hal = Hal::new(OutputMode::Silent, 8, 8);
        hal.fill_all(0, 0, 0);
        свет_плоский(&mut hal);
        hal.r3.blend = 0.6;
        // задняя сторона: opaque-проход её отрисовал бы (куллинг), прозрачный —
        // обязан рисовать (листва видна с обеих сторон)
        квадрат_тыл(&mut hal, 0.5, 1.0, 1.0, 0.0);
        flush_transparent(&mut hal).unwrap();
        let c = пиксель(&hal, 4, 4);
        assert_eq!(c[0], col(0.6));
        assert_eq!(c[1], col(0.6));
    }

    #[test]
    fn mesh_vc_градиент_альфы_перспектива_корректна() {
        // альфа 1 слева → 0 справа, цвет красный: пиксель x=1 (u=-0.625) →
        // a = (1-u)/2 = 0.8125 → красный 207 поверх чёрного фона
        let pos = [
            -1.0f32, -1.0, 0.5, 1.0, -1.0, 0.5, 1.0, 1.0, 0.5, -1.0, 1.0, 0.5,
        ];
        let rgba = [
            1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0,
        ];
        let idx = [0.0f32, 1.0, 2.0, 0.0, 2.0, 3.0];
        let mm = ед_матрицы();
        let (model, mvp) = (&mm[0..16], &mm[16..32]);
        let mut hal = Hal::new(OutputMode::Silent, 8, 8);
        hal.fill_all(0, 0, 0);
        свет_плоский(&mut hal);
        mesh_draw(
            &mut hal,
            &pos,
            None,
            Some(&rgba),
            &idx,
            model,
            mvp,
            1.0,
            1.0,
            1.0,
            0,
            Prim::Tri,
        )
        .unwrap();
        flush_transparent(&mut hal).unwrap();
        let c = пиксель(&hal, 1, 4);
        assert_eq!(c[0], col(0.8125), "градиент альфы: {:?}", c);
        assert_eq!(c[1], 0);
        // правый край: центр последнего пикселя u=0.875 → a=(1-u)/2=0.0625
        let c2 = пиксель(&hal, 7, 4);
        assert_eq!(c2, [col(0.0625), 0, 0, 255]);
    }

    #[test]
    fn очередь_чистится_clear_и_flush() {
        let mut hal = Hal::new(OutputMode::Silent, 8, 8);
        hal.fill_all(0, 0, 0);
        свет_плоский(&mut hal);
        hal.r3.blend = 0.5;
        квадрат_цвета(&mut hal, 0.5, 1.0, 1.0, 1.0);
        assert_eq!(hal.r3.transq.len(), 2); // 2 треугольника квада
        flush_transparent(&mut hal).unwrap();
        assert!(hal.r3.transq.is_empty());
        // clear тоже чистит (забытый flush не портит следующий кадр)
        квадрат_цвета(&mut hal, 0.5, 1.0, 1.0, 1.0);
        hal.r3.reset(8, 8);
        hal.r3.transq.clear();
        assert!(hal.r3.transq.is_empty());
    }

    // ——— интеграционные: растр этапа 7 через mesh_draw (программный путь) ——

    use crate::hal::{Hal, OutputMode, Tex};

    /// Единичные матрицы (model, mvp): клип = мир, w = 1.
    fn ед_матрицы() -> Vec<f32> {
        let mut m = vec![0.0f32; 16];
        m[0] = 1.0;
        m[5] = 1.0;
        m[10] = 1.0;
        m[15] = 1.0;
        let mut out = m.clone();
        out.extend_from_slice(&m);
        out
    }

    /// Плоский свет для растровых тестов: ambient 1, diffuse 0 → цвет = тексель·тинт.
    fn свет_плоский(hal: &mut Hal) {
        сброс_света(hal);
        hal.r3.light = [0.0, 0.0, 1.0];
        hal.r3.ambient = 1.0;
        hal.r3.diffuse = 0.0;
    }

    /// Квадрат на глубине z во весь экран 8x8 (CCW-лицевой после y-flip).
    fn рисуем_квадрат(hal: &mut Hal, tex: i64, z: f32, uv: &[f32]) {
        let pos = [-1.0f32, -1.0, z, 1.0, -1.0, z, 1.0, 1.0, z, -1.0, 1.0, z];
        let idx = [0.0f32, 1.0, 2.0, 0.0, 2.0, 3.0];
        let mm = ед_матрицы();
        let (model, mvp) = (&mm[0..16], &mm[16..32]);
        mesh_draw(
            hal,
            &pos,
            Some(uv),
            None,
            &idx,
            model,
            mvp,
            1.0,
            1.0,
            1.0,
            tex,
            Prim::Tri,
        )
        .unwrap();
    }

    fn пиксель(hal: &Hal, x: usize, y: usize) -> [u8; 4] {
        let o = (y * hal.w + x) * 4;
        [hal.fb[o], hal.fb[o + 1], hal.fb[o + 2], hal.fb[o + 3]]
    }

    fn сброс_света(hal: &mut Hal) {
        hal.r3.light = [0.45, 0.8, 0.35];
        hal.r3.ambient = 0.35;
        hal.r3.diffuse = 0.75;
        hal.r3.alpha_test = 0.0;
        hal.r3.spec = 0.0;
        hal.r3.shininess = 32.0;
        hal.r3.view = [0.0, 0.0, 0.0];
        hal.r3.tiling = [1.0, 1.0];
        hal.r3.fog = [0.7, 0.8, 0.9, 0.0];
        hal.r3.blend = 1.0;
        hal.r3.transq.clear();
    }

    #[test]
    fn матрица_инстанса_yaw_по_конвенции() {
        // Регрессия: f32::sin_cos возвращает (sin, cos) — прежняя связка
        // перепутывала оси и ломала все батчи. Проверка конвенции
        // M = T·Ry·Rx·Rz·S: yaw=π/2 переводит +X в -Z (колоночный вектор).
        let pi2 = std::f32::consts::FRAC_PI_2;
        let mut m = [0.0f32; 16];
        inst_matrix(
            &[5.0, 6.0, 7.0, 1.0, 1.0, 1.0, pi2, 0.0, 0.0, 1.0, 1.0, 1.0],
            &mut m,
        );
        // вершина (1, 0, 0): world = R·v + t = (0, 0, -1) + (5, 6, 7)
        let wx = m[0] * 1.0 + m[3];
        let wy = m[4] * 1.0 + m[7];
        let wz = m[8] * 1.0 + m[11];
        assert!(близко(wx, 5.0), "wx = {}", wx);
        assert!(близко(wy, 6.0), "wy = {}", wy);
        assert!(близко(wz, 7.0 - 1.0), "wz = {}", wz);
        // масштаб: вершина (0, 2, 0) → (2, 0) → world y = 6 + 2
        let wy2 = m[5] * 2.0 + m[7];
        assert!(близко(wy2, 8.0), "wy2 = {}", wy2);
        // нулевые углы — диагональная матрица масштаба
        inst_matrix(
            &[0.0, 0.0, 0.0, 0.4, 0.4, 0.4, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            &mut m,
        );
        assert!(близко(m[0], 0.4) && близко(m[5], 0.4) && близко(m[10], 0.4));
        assert!(близко(m[1] + m[2] + m[4] + m[6] + m[8] + m[9], 0.0));
    }

    #[test]
    fn растр_alpha_test_вырезает_и_z_не_пишет() {
        // 2x2 текстура: 3 текселя маджента непрозрачные, угловой (u=1,v=1)
        // полностью прозрачный (alpha 0). Далёкий зелёный квадрат на z=0.9,
        // близкий текстурный с дыркой на z=0.5.
        let px = vec![
            255u8, 0, 255, 255, 255, 0, 255, 255, 255, 0, 255, 255, 9, 9, 9, 0,
        ];
        let зелёный = || Tex {
            w: 1,
            h: 1,
            px: vec![0, 255, 0, 255],
        };
        let uv = [0.0f32, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0];
        let плоский_свет = |hal: &mut Hal| {
            сброс_света(hal);
            hal.r3.light = [0.0, 0.0, 1.0];
            hal.r3.ambient = 1.0;
            hal.r3.diffuse = 0.0; // свет плоский: цвет = тексель·тинт
        };

        // без alpha-test близкий квадрат перекрывает ВСЁ маджентой
        let mut hal = Hal::new(OutputMode::Silent, 8, 8);
        hal.tex.push(Tex {
            w: 2,
            h: 2,
            px: px.clone(),
        });
        hal.tex.push(зелёный());
        hal.fill_all(0, 0, 0);
        свет_плоский(&mut hal);
        рисуем_квадрат(&mut hal, 2, 0.9, &uv);
        assert_eq!(пиксель(&hal, 0, 0), [0, 255, 0, 255]);
        рисуем_квадрат(&mut hal, 1, 0.5, &uv);
        assert_eq!(пиксель(&hal, 0, 0), [255, 0, 255, 255]);
        assert_eq!(пиксель(&hal, 7, 7), [255, 0, 255, 255]);

        // с alpha-test 0.5 прозрачный тексель отброшен — сквозь дыру виден
        // зелёный дальний квадрат: discard не пишет ни цвет, ни z.
        // Ориентация: v=1 → верх экрана (y-flip), u=1 → право; дыра (тексель
        // u=1,v=1) — правый верхний квадрант, напр. пиксель (7,0)
        let mut hal2 = Hal::new(OutputMode::Silent, 8, 8);
        hal2.tex.push(Tex {
            w: 2,
            h: 2,
            px: px.clone(),
        });
        hal2.tex.push(зелёный());
        hal2.fill_all(0, 0, 0);
        плоский_свет(&mut hal2);
        рисуем_квадрат(&mut hal2, 2, 0.9, &uv);
        hal2.r3.alpha_test = 0.5;
        рисуем_квадрат(&mut hal2, 1, 0.5, &uv);
        assert_eq!(пиксель(&hal2, 7, 0), [0, 255, 0, 255]);
        assert_eq!(пиксель(&hal2, 4, 1), [0, 255, 0, 255]);
        // непрозрачные тексели рисуются как раньше
        assert_eq!(пиксель(&hal2, 0, 0), [255, 0, 255, 255]);
        assert_eq!(пиксель(&hal2, 7, 7), [255, 0, 255, 255]);
        assert_eq!(пиксель(&hal2, 0, 7), [255, 0, 255, 255]);
    }

    #[test]
    fn растр_туман_смешивает_после_шейдинга() {
        // белый квадрат, свет вдоль (0, 0.6, 0.8): ndl = 0.8 →
        // shade = 0.35 + 0.75·0.8 = 0.95 → 242
        let свет = |hal: &mut Hal| {
            сброс_света(hal);
            hal.r3.light = [0.0, 0.6, 0.8];
        };
        let uv = [0.0f32, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0];

        let mut hal = Hal::new(OutputMode::Silent, 8, 8);
        hal.fill_all(0, 0, 0);
        свет(&mut hal);
        рисуем_квадрат(&mut hal, 0, 0.5, &uv);
        assert_eq!(пиксель(&hal, 4, 4), [242, 242, 242, 255]);

        // красный туман density 50 на глубине 1.0: фактор ≈ 1 → цвет тумана
        let mut hal2 = Hal::new(OutputMode::Silent, 8, 8);
        hal2.fill_all(0, 0, 0);
        свет(&mut hal2);
        hal2.r3.fog = [1.0, 0.0, 0.0, 50.0];
        рисуем_квадрат(&mut hal2, 0, 0.5, &uv);
        assert_eq!(пиксель(&hal2, 4, 4), [255, 0, 0, 255]);

        // слабый туман: точное значение по формуле на истинной глубине w=1
        let f = fog_factor(0.1, 1.0);
        let ожид_r = 0.95 * (1.0 - f) + f;
        let ожид_g = 0.95 * (1.0 - f);
        let mut hal3 = Hal::new(OutputMode::Silent, 8, 8);
        hal3.fill_all(0, 0, 0);
        свет(&mut hal3);
        hal3.r3.fog = [1.0, 0.0, 0.0, 0.1];
        рисуем_квадрат(&mut hal3, 0, 0.5, &uv);
        let c = пиксель(&hal3, 4, 4);
        assert_eq!(c[0], col(ожид_r as f64));
        assert_eq!(c[1], col(ожид_g as f64));
    }

    #[test]
    fn растр_блик_блинн_фонга_ярче_lambert() {
        let uv = [0.0f32, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0];
        // камера на +Z (view (0,0,5)), квадрат в z=0.5, свет (0, 0.6, 0.8):
        // Lambert = 0.95 → 242; half-vector блик у центра добавляет ~0.16 →
        // 0.95+0.16 > 1 → clamp → белый
        let mut hal = Hal::new(OutputMode::Silent, 8, 8);
        hal.fill_all(0, 0, 0);
        сброс_света(&mut hal);
        hal.r3.light = [0.0, 0.6, 0.8];
        рисуем_квадрат(&mut hal, 0, 0.5, &uv);
        let ламберт = пиксель(&hal, 4, 4);
        assert_eq!(ламберт, [242, 242, 242, 255]);

        let mut hal2 = Hal::new(OutputMode::Silent, 8, 8);
        hal2.fill_all(0, 0, 0);
        сброс_света(&mut hal2);
        hal2.r3.light = [0.0, 0.6, 0.8];
        hal2.r3.view = [0.0, 0.0, 5.0];
        hal2.r3.spec = 1.0;
        hal2.r3.shininess = 32.0;
        рисуем_квадрат(&mut hal2, 0, 0.5, &uv);
        let с_бликом = пиксель(&hal2, 4, 4);
        assert_eq!(с_бликом, [255, 255, 255, 255]);
        assert!(с_бликом[0] > ламберт[0]);
    }

    #[test]
    fn растр_тайлинг_repeat_меняет_семпл() {
        // 4x1 текстура: столбцы красный / зелёный / синий / белый
        let tex = Tex {
            w: 4,
            h: 1,
            px: vec![
                255, 0, 0, 255, // u ∈ [0, 0.25)
                0, 255, 0, 255, // u ∈ [0.25, 0.5)
                0, 0, 255, 255, // u ∈ [0.5, 0.75)
                255, 255, 255, 255, // u ∈ [0.75, 1)
            ],
        };
        let uv = [0.0f32, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0];
        let плоский_свет = |hal: &mut Hal| {
            сброс_света(hal);
            hal.r3.light = [0.0, 0.0, 1.0];
            hal.r3.ambient = 1.0;
            hal.r3.diffuse = 0.0;
        };

        // без тайлинга пиксель (4,y): fx=4.5 → u≈0.5625 → столбец 2 (синий)
        let mut hal = Hal::new(OutputMode::Silent, 8, 8);
        hal.tex.push(tex.clone());
        hal.fill_all(0, 0, 0);
        свет_плоский(&mut hal);
        рисуем_квадрат(&mut hal, 1, 0.5, &uv);
        assert_eq!(пиксель(&hal, 4, 4), [0, 0, 255, 255]);
        // край u=0 → столбец 0 (красный)
        assert_eq!(пиксель(&hal, 0, 4), [255, 0, 0, 255]);

        // tiling (2,1): u=0.5625 → 1.125 → fract 0.125 → столбец 0 (красный)
        let mut hal2 = Hal::new(OutputMode::Silent, 8, 8);
        hal2.tex.push(tex);
        hal2.fill_all(0, 0, 0);
        плоский_свет(&mut hal2);
        hal2.r3.tiling = [2.0, 1.0];
        рисуем_квадрат(&mut hal2, 1, 0.5, &uv);
        assert_eq!(пиксель(&hal2, 4, 4), [255, 0, 0, 255]);
        // шов стыкуется: u≈0.9375 → 1.875 → fract 0.875 → столбец 3 (белый)
        assert_eq!(пиксель(&hal2, 7, 4), [255, 255, 255, 255]);
    }
}
