// src/render3d/shading.rs — Чистая математика затенения, освещения и теней.

use super::types::{PointLight, R3};
use crate::interp::{need_args, num_val, Value};

#[inline]
pub(crate) fn col(x: f64) -> u8 {
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
pub(crate) fn edge(ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32) -> f32 {
    (bx - ax) * (cy - ay) - (by - ay) * (cx - ax)
}

#[inline]
pub(crate) fn cross3(a: &[f32; 3], b: &[f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
pub(crate) fn dot3(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Вершина клип-пространства: [x y z w] + атрибуты u/w, v/w, 1/w, wx/w, wy/w,
/// wz/w, a/w, r/w, g/w, b/w (этап 9: per-vertex RGBA для mesh-vc).
/// Клиппинг Sutherland–Hodgman по плоскости z + w >= 0 (near).
/// Атрибуты (u/w, 1/w, мир/w, a/w, rgb/w) линейны вдоль ребра → интерполируются
/// тем же t. Мир/w нужен per-pixel half-vector блику, a/w и rgb/w — прозрачности
/// и цветовым градиентам (вода, небо).
pub(crate) fn clip_near6(poly: &[[f32; 17]], out: &mut Vec<[f32; 17]>) {
    out.clear();
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
            let mut v = [0.0f32; 17];
            for k in 0..17 {
                v[k] = a[k] + (b[k] - a[k]) * t;
            }
            out.push(v);
        }
    }
}

pub(crate) fn clip_near_pos(poly: &[[f32; 4]], out: &mut Vec<[f32; 4]>) {
    out.clear();
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
            let mut v = [0.0f32; 4];
            for k in 0..4 {
                v[k] = a[k] + (b[k] - a[k]) * t;
            }
            out.push(v);
        }
    }
}

/// Семплирование карты теней с Percentage Closer Filtering (PCF 2x2).
/// Возвращает коэффициент освещённости: 0.0 (полная тень) .. 1.0 (полный свет).
#[inline]
pub fn shadow_sample(r3: &R3, wx: f32, wy: f32, wz: f32, bias: f32) -> f32 {
    if !r3.shadow_enabled || r3.shadow_map.is_empty() || r3.shadow_w == 0 || r3.shadow_h == 0 {
        return 1.0;
    }
    let m = &r3.light_matrix;
    let lx = m[0] * wx + m[1] * wy + m[2] * wz + m[3];
    let ly = m[4] * wx + m[5] * wy + m[6] * wz + m[7];
    let lz = m[8] * wx + m[9] * wy + m[10] * wz + m[11];
    let lw = m[12] * wx + m[13] * wy + m[14] * wz + m[15];
    if lw <= 1e-6 {
        return 1.0;
    }
    let inv_lw = 1.0 / lw;
    let nx = lx * inv_lw;
    let ny = ly * inv_lw;
    let nz = lz * inv_lw;
    // за пределами [-1,1] по любой оси — вне карты теней (NaN сюда же: освещаем)
    let in_unit_cube =
        (-1.0..=1.0).contains(&nx) && (-1.0..=1.0).contains(&ny) && (-1.0..=1.0).contains(&nz);
    if !in_unit_cube {
        return 1.0;
    }
    let sw = r3.shadow_w;
    let sh = r3.shadow_h;
    let sx = (nx * 0.5 + 0.5) * (sw as f32 - 1.0);
    let sy = (1.0 - (ny * 0.5 + 0.5)) * (sh as f32 - 1.0);
    let sz = nz;

    let x0 = (sx.floor() as usize).min(sw - 1);
    let y0 = (sy.floor() as usize).min(sh - 1);
    let x1 = (x0 + 1).min(sw - 1);
    let y1 = (y0 + 1).min(sh - 1);

    let fx = sx - sx.floor();
    let fy = sy - sy.floor();

    let check = |x: usize, y: usize| -> f32 {
        let depth = r3.shadow_map[y * sw + x];
        if sz - bias > depth {
            0.0
        } else {
            1.0
        }
    };

    let s00 = check(x0, y0);
    let s10 = check(x1, y0);
    let s01 = check(x0, y1);
    let s11 = check(x1, y1);

    let top = s00 * (1.0 - fx) + s10 * fx;
    let bottom = s01 * (1.0 - fx) + s11 * fx;
    top * (1.0 - fy) + bottom * fy
}

pub fn shadow_begin(r3: &mut R3, matrix: [f32; 16], w: usize, h: usize, bias: Option<f32>) {
    r3.light_matrix = matrix;
    r3.shadow_w = w;
    r3.shadow_h = h;
    let total = w * h;
    if r3.shadow_map.len() != total {
        r3.shadow_map = vec![f32::INFINITY; total];
    } else {
        r3.shadow_map.fill(f32::INFINITY);
    }
    r3.shadow_pass = true;
    r3.shadow_enabled = true;
    if let Some(b) = bias {
        r3.shadow_bias = b;
    }
}

pub fn shadow_end(r3: &mut R3) {
    r3.shadow_pass = false;
}

pub fn shadow_clear(r3: &mut R3) {
    r3.shadow_pass = false;
    r3.shadow_enabled = false;
    r3.shadow_map.clear();
    r3.shadow_w = 0;
    r3.shadow_h = 0;
}

pub(crate) fn compute_light_matrix(
    light_dir: [f32; 3],
    target: [f32; 3],
    size: f32,
    near: f32,
    far: f32,
) -> [f32; 16] {
    let len = dot3(&light_dir, &light_dir).sqrt().max(1e-6);
    let ld = [light_dir[0] / len, light_dir[1] / len, light_dir[2] / len];
    let dist = 0.5 * (near + far);
    let eye = [
        target[0] + ld[0] * dist,
        target[1] + ld[1] * dist,
        target[2] + ld[2] * dist,
    ];
    let up = if ld[0].abs() < 0.1 && ld[2].abs() < 0.1 {
        [0.0, 0.0, 1.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let f = [target[0] - eye[0], target[1] - eye[1], target[2] - eye[2]];
    let flen = dot3(&f, &f).sqrt().max(1e-6);
    let f = [f[0] / flen, f[1] / flen, f[2] / flen];

    let r = cross3(&f, &up);
    let rlen = dot3(&r, &r).sqrt().max(1e-6);
    let r = [r[0] / rlen, r[1] / rlen, r[2] / rlen];

    let u = cross3(&r, &f);

    let view = [
        r[0],
        r[1],
        r[2],
        -dot3(&r, &eye),
        u[0],
        u[1],
        u[2],
        -dot3(&u, &eye),
        -f[0],
        -f[1],
        -f[2],
        dot3(&f, &eye),
        0.0,
        0.0,
        0.0,
        1.0,
    ];

    let rl = 2.0 * size;
    let tb = 2.0 * size;
    let fn_dist = far - near;
    let proj = [
        2.0 / rl,
        0.0,
        0.0,
        0.0,
        0.0,
        2.0 / tb,
        0.0,
        0.0,
        0.0,
        0.0,
        -2.0 / fn_dist,
        -(far + near) / fn_dist,
        0.0,
        0.0,
        0.0,
        1.0,
    ];

    let mut out = [0.0f32; 16];
    for row in 0..4 {
        for col in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += proj[row * 4 + k] * view[k * 4 + col];
            }
            out[row * 4 + col] = sum;
        }
    }
    out
}

#[inline]
#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_point_lights(
    lights: &[PointLight],
    view: &[f32; 3],
    wx: f32,
    wy: f32,
    wz: f32,
    nrm: &[f32; 3],
    two_sided: bool,
    spec_on: bool,
    shininess: f32,
    spec: f32,
) -> ([f32; 3], [f32; 3]) {
    let mut diff = [0.0f32; 3];
    let mut spec_acc = [0.0f32; 3];
    if lights.is_empty() {
        return (diff, spec_acc);
    }
    let v_norm = if spec_on {
        let v = [view[0] - wx, view[1] - wy, view[2] - wz];
        let vl = dot3(&v, &v).sqrt().max(1e-9);
        [v[0] / vl, v[1] / vl, v[2] / vl]
    } else {
        [0.0, 0.0, 0.0]
    };

    for pl in lights {
        let dx = pl.pos[0] - wx;
        let dy = pl.pos[1] - wy;
        let dz = pl.pos[2] - wz;
        let d2 = dx * dx + dy * dy + dz * dz;
        let r2 = pl.radius * pl.radius;
        if d2 < r2 && d2 > 1e-6 {
            let dist = d2.sqrt();
            let att = ((1.0 - dist / pl.radius).max(0.0) / (1.0 + 0.09 * dist + 0.032 * d2))
                * pl.attenuation;
            let lx = dx / dist;
            let ly = dy / dist;
            let lz = dz / dist;
            let mut p_ndl = nrm[0] * lx + nrm[1] * ly + nrm[2] * lz;
            if two_sided && p_ndl < 0.0 {
                p_ndl = -p_ndl;
            }
            let diff_f = att * p_ndl.max(0.0);
            diff[0] += pl.color[0] * diff_f;
            diff[1] += pl.color[1] * diff_f;
            diff[2] += pl.color[2] * diff_f;

            if spec_on {
                let hv = [lx + v_norm[0], ly + v_norm[1], lz + v_norm[2]];
                let hl = dot3(&hv, &hv).sqrt().max(1e-9);
                let sp = spec_highlight(
                    dot3(nrm, &[hv[0] / hl, hv[1] / hl, hv[2] / hl]),
                    shininess,
                    spec,
                );
                spec_acc[0] += pl.color[0] * sp * att;
                spec_acc[1] += pl.color[1] * sp * att;
                spec_acc[2] += pl.color[2] * sp * att;
            }
        }
    }
    (diff, spec_acc)
}
