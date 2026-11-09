// src/render3d.rs — Уровень 1 (HAL): «GPU» фиксированного конвейера.
// Философия ТЗ соблюдена: Rust знает только треугольники, глубину и свет —
// ровно то, что в «железе» делает GPU (T&L эпохи GeForce 256 + z-буфер).
// Камера, проекции, матрицы моделей, меши, сцена, игра — Lisp
// (assets/lisp/render3d.lisp, game3d.lisp).
//
// Примитивы:
//   gpu3d-clear     — очистка цвета + z-буфера (+ сброс статистики кадра)
//   gpu3d-light     — направленный свет и фоновая освещённость (fixed-function)
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
use crate::interp::{env_define, need_args, num_val, Env, Value};
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

/// Вершина клип-пространства: [x y z w] + атрибуты u/w, v/w, 1/w.
/// Клиппинг Sutherland–Hodgman по плоскости z + w >= 0 (near).
/// Атрибуты u/w и 1/w линейны вдоль ребра → интерполируются тем же t.
fn clip_near6(poly: &[[f32; 7]]) -> Vec<[f32; 7]> {
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
            out.push([
                a[0] + (b[0] - a[0]) * t,
                a[1] + (b[1] - a[1]) * t,
                a[2] + (b[2] - a[2]) * t,
                a[3] + (b[3] - a[3]) * t,
                a[4] + (b[4] - a[4]) * t,
                a[5] + (b[5] - a[5]) * t,
                a[6] + (b[6] - a[6]) * t,
            ]);
        }
    }
    out
}

struct Vtx {
    sx: f32,
    sy: f32,
    sz: f32,
    uq: f32, // u/w
    vq: f32, // v/w
    iw: f32, // 1/w
}

fn project6(c: &[f32; 7], w: usize, h: usize) -> Option<Vtx> {
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
        uq: c[4], // u/w
        vq: c[5], // v/w
        iw: c[6], // 1/w
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
    let (cy, syw) = ins[6].sin_cos();
    let (cx, sxi) = ins[7].sin_cos();
    let (cz, szi) = ins[8].sin_cos();
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
            idx,
            None,
            (r as f32, g as f32, b as f32),
            ambient,
            diffuse,
            &light,
            tex.as_ref(),
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
#[allow(clippy::too_many_arguments)]
fn batch_draw(
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
        &idx_all,
        Some(&tint),
        (0.0, 0.0, 0.0),
        ambient,
        diffuse,
        &light,
        tex.as_ref(),
        &mut drawn,
        &mut culled,
    )?;
    r3.drawn += drawn;
    r3.culled += culled;
    Ok(drawn)
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
    idx: &[f32],
    tint: Option<&[f32]>,   // rgb на вершину (батч: цвет инстанса)
    solid: (f32, f32, f32), // один цвет (обычный mesh)
    ambient: f32,
    diffuse: f32,
    light: &[f32; 3],
    tex: Option<&TexRef>,
    drawn: &mut u64,
    culled: &mut u64,
) -> Result<(), String> {
    let n = world.len() / 3;
    let textured = tex.is_some();
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
        // полигон в клип-пространстве + атрибуты u/w, v/w, 1/w
        let mut poly: Vec<[f32; 7]> = Vec::with_capacity(3);
        for &t in &tri {
            let cw = clip[t * 4 + 3];
            let iw = if cw > 1e-6 { 1.0 / cw } else { 0.0 };
            // перспективно-корректные атрибуты: в полигон идут u/w и v/w
            // (они, как и 1/w, аффинны на ребре — клиппинг и растр корректны)
            let (uq, vq) = if let Some(u) = uvq {
                (u[t * 2] * iw, u[t * 2 + 1] * iw)
            } else {
                (0.0, 0.0)
            };
            poly.push([
                clip[t * 4],
                clip[t * 4 + 1],
                clip[t * 4 + 2],
                cw,
                uq,
                vq,
                iw,
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

        // вентилятор: (0, k, k+1)
        for kk in 1..pts.len() - 1 {
            let (p0, p1, p2) = (&pts[0], &pts[kk], &pts[kk + 1]);
            // backface cull: фронт = CCW в NDC → после y-flip площадь < 0
            let area = edge(p0.sx, p0.sy, p1.sx, p1.sy, p2.sx, p2.sy);
            if area >= -1e-6 {
                *culled += 1;
                continue;
            }
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
            let shade = if nl < 1e-9 {
                ambient
            } else {
                let ndl = dot3(&[nrm[0] / nl, nrm[1] / nl, nrm[2] / nl], light);
                (ambient + diffuse * ndl.max(0.0)).clamp(0.0, 1.0)
            };
            // цвет грани: тинт первой вершины (у батча — свой на инстанс)
            let (tr, tg, tb) = match tint {
                Some(t) => (t[tri[0] * 3], t[tri[0] * 3 + 1], t[tri[0] * 3 + 2]),
                None => solid,
            };

            let (tw, th) = tex.map(|t| (t.w, t.h)).unwrap_or((0, 0));
            let cr = if textured {
                0u8
            } else {
                col(tr as f64 * shade as f64)
            };
            let cg = if textured {
                0u8
            } else {
                col(tg as f64 * shade as f64)
            };
            let cb = if textured {
                0u8
            } else {
                col(tb as f64 * shade as f64)
            };

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
                            r3.zbuf[pi] = z;
                            let o = pi * 4;
                            if let Some(t) = tex {
                                // перспектива-корректный UV: (u/w)/(1/w)
                                let iw = (w0 * p0.iw + w1 * p1.iw + w2 * p2.iw) / sum;
                                let uq = (w0 * p0.uq + w1 * p1.uq + w2 * p2.uq) / sum;
                                let vq = (w0 * p0.vq + w1 * p1.vq + w2 * p2.vq) / sum;
                                if iw.abs() > 1e-12 {
                                    let uu = ((uq / iw).clamp(0.0, 0.99999) * tw as f32) as usize;
                                    let vv = ((vq / iw).clamp(0.0, 0.99999) * th as f32) as usize;
                                    let ti = (vv * tw + uu) * 4;
                                    let texel = &t.px[ti..ti + 3];
                                    // lambert · тинт · тексель
                                    fb[o] = col(texel[0] as f64 / 255.0 * tr as f64 * shade as f64);
                                    fb[o + 1] =
                                        col(texel[1] as f64 / 255.0 * tg as f64 * shade as f64);
                                    fb[o + 2] =
                                        col(texel[2] as f64 / 255.0 * tb as f64 * shade as f64);
                                    fb[o + 3] = 255;
                                }
                            } else {
                                fb[o] = cr;
                                fb[o + 1] = cg;
                                fb[o + 2] = cb;
                                fb[o + 3] = 255;
                            }
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

pub fn register_render3d(env: &Rc<Env>, hal: &Rc<RefCell<Hal>>) {
    fn def<F: Fn(&[Value]) -> Result<Value, String> + 'static>(env: &Rc<Env>, name: &str, f: F) {
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
        h.fill_all(r, g, b);
        h.r3.zbuf.fill(f32::INFINITY);
        h.r3.drawn = 0;
        h.r3.culled = 0;
        Ok(Value::Nil)
    });

    let hc = hal.clone();
    def(env, "gpu3d-light", move |args| {
        need(args, 4)?;
        let mut h = hc.borrow_mut();
        h.r3.light = [
            num_val(&args[0])? as f32,
            num_val(&args[1])? as f32,
            num_val(&args[2])? as f32,
        ];
        h.r3.ambient = num_val(&args[3])? as f32;
        if let Some(v) = args.get(4) {
            h.r3.diffuse = num_val(v)? as f32;
        }
        Ok(Value::Nil)
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
