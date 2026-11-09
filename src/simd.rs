#![allow(clippy::map_identity)]

// src/simd.rs — SAL (SIMD Abstraction Layer) по ТЗ §4.3–4.5.
// Портативный API: vec-add / vec-dot / mat4-mul … с runtime-диспетчеризацией:
//   AVX-512 → AVX2 → SSE2 → скалярный fallback.
//
// Диспетчеризация двуступенчатая (по ТЗ §4.4 и честнее):
//   1) по флагам CPU (is_x86_feature_detected!),
//   2) калибровка micro-kernel на старте: на виртуалках флаг есть, а быстрого
//      исполнения может не быть — поэтому уровень выбирается по замеру.
// Скалярный fallback компилируется с автой-векторизацией LLVM (SSE2 baseline) —
// это и есть честный «без нашего SIMD» бенчмарк.

use crate::interp::{env_define, fmt_value, need_args, num_val, Gc, Value};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SimdLevel {
    // на x86_64 Scalar не выбирается (минимум Sse2), но нужен другим архам
    #[allow(dead_code)]
    Scalar,
    Sse2,
    Avx2,
    Avx512,
}

pub fn detect() -> SimdLevel {
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::is_x86_feature_detected;
        // AVX-512 код требует поддержки stdarch в компиляторе (stable 1.89+,
        // см. build.rs: cfg has_avx512). Без неё честно отдаём AVX2.
        if is_x86_feature_detected!("avx512f") && cfg!(has_avx512) {
            SimdLevel::Avx512
        } else if is_x86_feature_detected!("avx2") {
            SimdLevel::Avx2
        } else {
            SimdLevel::Sse2
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        SimdLevel::Scalar
    }
}

static ACTIVE: OnceLock<SimdLevel> = OnceLock::new();

pub fn active_level() -> SimdLevel {
    match ACTIVE.get() {
        Some(l) => *l,
        None => {
            let l = detect();
            let _ = ACTIVE.set(l);
            l
        }
    }
}

/// Калибровка: меряем vec-add на кэш-резидентных данных для каждого доступного
/// уровня и выбираем самый быстрый (защита от «флаг есть, скорости нет»).
pub fn calibrate() -> SimdLevel {
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::is_x86_feature_detected;
        let n = 2048usize;
        let a: Vec<f32> = (0..n).map(|i| (i % 97) as f32 * 0.5).collect();
        let b: Vec<f32> = (0..n).map(|i| (i % 89) as f32 * 0.25).collect();
        let mut out = vec![0f32; n];
        let mut candidates: Vec<SimdLevel> = vec![SimdLevel::Sse2];
        if is_x86_feature_detected!("avx2") {
            candidates.push(SimdLevel::Avx2);
        }
        if is_x86_feature_detected!("avx512f") && cfg!(has_avx512) {
            candidates.push(SimdLevel::Avx512);
        }
        let mut best = SimdLevel::Sse2;
        let mut best_t = f64::MAX;
        for lvl in candidates {
            // прогрев
            level_add(lvl, &a, &b, &mut out);
            let iters = 3000usize;
            let t0 = Instant::now();
            for _ in 0..iters {
                level_add(lvl, &a, &b, &mut out);
            }
            let t = t0.elapsed().as_secs_f64();
            if t < best_t {
                best_t = t;
                best = lvl;
            }
        }
        let _ = ACTIVE.set(best);
        best
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = ACTIVE.set(SimdLevel::Scalar);
        SimdLevel::Scalar
    }
}

pub fn level_name(l: SimdLevel) -> &'static str {
    match l {
        SimdLevel::Avx512 => "AVX-512",
        SimdLevel::Avx2 => "AVX2",
        SimdLevel::Sse2 => "SSE2",
        SimdLevel::Scalar => "SCALAR",
    }
}

// ——— скалярный fallback (эталон эквивалентности; LLVM может автовекторизовать) ———
// На x86_64 часть функций не вызывается (там минимум SSE2) — это fallback
// для других архитектур, оставляем полный набор.
#[allow(dead_code)]
pub mod scalar {
    pub fn add(a: &[f32], b: &[f32], out: &mut [f32]) {
        for i in 0..a.len() {
            out[i] = a[i] + b[i];
        }
    }
    pub fn sub(a: &[f32], b: &[f32], out: &mut [f32]) {
        for i in 0..a.len() {
            out[i] = a[i] - b[i];
        }
    }
    pub fn mul(a: &[f32], b: &[f32], out: &mut [f32]) {
        for i in 0..a.len() {
            out[i] = a[i] * b[i];
        }
    }
    pub fn min(a: &[f32], b: &[f32], out: &mut [f32]) {
        for i in 0..a.len() {
            out[i] = a[i].min(b[i]);
        }
    }
    pub fn max(a: &[f32], b: &[f32], out: &mut [f32]) {
        for i in 0..a.len() {
            out[i] = a[i].max(b[i]);
        }
    }
    pub fn dot(a: &[f32], b: &[f32]) -> f32 {
        let mut s = 0f32;
        for i in 0..a.len() {
            s += a[i] * b[i];
        }
        s
    }
}

#[cfg(target_arch = "x86_64")]
mod sse {
    use std::arch::x86_64::*;
    pub unsafe fn add(a: &[f32], b: &[f32], out: &mut [f32]) {
        let n = a.len() - a.len() % 4;
        let mut i = 0;
        while i < n {
            _mm_storeu_ps(
                out.as_mut_ptr().add(i),
                _mm_add_ps(
                    _mm_loadu_ps(a.as_ptr().add(i)),
                    _mm_loadu_ps(b.as_ptr().add(i)),
                ),
            );
            i += 4;
        }
        while i < a.len() {
            out[i] = a[i] + b[i];
            i += 1;
        }
    }
    pub unsafe fn sub(a: &[f32], b: &[f32], out: &mut [f32]) {
        let n = a.len() - a.len() % 4;
        let mut i = 0;
        while i < n {
            _mm_storeu_ps(
                out.as_mut_ptr().add(i),
                _mm_sub_ps(
                    _mm_loadu_ps(a.as_ptr().add(i)),
                    _mm_loadu_ps(b.as_ptr().add(i)),
                ),
            );
            i += 4;
        }
        while i < a.len() {
            out[i] = a[i] - b[i];
            i += 1;
        }
    }
    pub unsafe fn mul(a: &[f32], b: &[f32], out: &mut [f32]) {
        let n = a.len() - a.len() % 4;
        let mut i = 0;
        while i < n {
            _mm_storeu_ps(
                out.as_mut_ptr().add(i),
                _mm_mul_ps(
                    _mm_loadu_ps(a.as_ptr().add(i)),
                    _mm_loadu_ps(b.as_ptr().add(i)),
                ),
            );
            i += 4;
        }
        while i < a.len() {
            out[i] = a[i] * b[i];
            i += 1;
        }
    }
    pub unsafe fn min(a: &[f32], b: &[f32], out: &mut [f32]) {
        let n = a.len() - a.len() % 4;
        let mut i = 0;
        while i < n {
            _mm_storeu_ps(
                out.as_mut_ptr().add(i),
                _mm_min_ps(
                    _mm_loadu_ps(a.as_ptr().add(i)),
                    _mm_loadu_ps(b.as_ptr().add(i)),
                ),
            );
            i += 4;
        }
        while i < a.len() {
            out[i] = a[i].min(b[i]);
            i += 1;
        }
    }
    pub unsafe fn max(a: &[f32], b: &[f32], out: &mut [f32]) {
        let n = a.len() - a.len() % 4;
        let mut i = 0;
        while i < n {
            _mm_storeu_ps(
                out.as_mut_ptr().add(i),
                _mm_max_ps(
                    _mm_loadu_ps(a.as_ptr().add(i)),
                    _mm_loadu_ps(b.as_ptr().add(i)),
                ),
            );
            i += 4;
        }
        while i < a.len() {
            out[i] = a[i].max(b[i]);
            i += 1;
        }
    }
    pub unsafe fn dot(a: &[f32], b: &[f32]) -> f32 {
        let n = a.len() - a.len() % 4;
        let mut acc = _mm_setzero_ps();
        let mut i = 0;
        while i < n {
            acc = _mm_add_ps(
                acc,
                _mm_mul_ps(
                    _mm_loadu_ps(a.as_ptr().add(i)),
                    _mm_loadu_ps(b.as_ptr().add(i)),
                ),
            );
            i += 4;
        }
        let mut t = [0f32; 4];
        _mm_storeu_ps(t.as_mut_ptr(), acc);
        let mut s = t[0] + t[1] + t[2] + t[3];
        while i < a.len() {
            s += a[i] * b[i];
            i += 1;
        }
        s
    }
}

#[cfg(target_arch = "x86_64")]
mod avx {
    use std::arch::x86_64::*;
    // разворот ×4: 32 f32 за итерацию
    pub unsafe fn add(a: &[f32], b: &[f32], out: &mut [f32]) {
        let n = a.len() - a.len() % 32;
        let mut i = 0;
        while i < n {
            for k in 0..4 {
                let p = i + k * 8;
                _mm256_storeu_ps(
                    out.as_mut_ptr().add(p),
                    _mm256_add_ps(
                        _mm256_loadu_ps(a.as_ptr().add(p)),
                        _mm256_loadu_ps(b.as_ptr().add(p)),
                    ),
                );
            }
            i += 32;
        }
        while i < a.len() {
            out[i] = a[i] + b[i];
            i += 1;
        }
    }
    pub unsafe fn sub(a: &[f32], b: &[f32], out: &mut [f32]) {
        let n = a.len() - a.len() % 32;
        let mut i = 0;
        while i < n {
            for k in 0..4 {
                let p = i + k * 8;
                _mm256_storeu_ps(
                    out.as_mut_ptr().add(p),
                    _mm256_sub_ps(
                        _mm256_loadu_ps(a.as_ptr().add(p)),
                        _mm256_loadu_ps(b.as_ptr().add(p)),
                    ),
                );
            }
            i += 32;
        }
        while i < a.len() {
            out[i] = a[i] - b[i];
            i += 1;
        }
    }
    pub unsafe fn mul(a: &[f32], b: &[f32], out: &mut [f32]) {
        let n = a.len() - a.len() % 32;
        let mut i = 0;
        while i < n {
            for k in 0..4 {
                let p = i + k * 8;
                _mm256_storeu_ps(
                    out.as_mut_ptr().add(p),
                    _mm256_mul_ps(
                        _mm256_loadu_ps(a.as_ptr().add(p)),
                        _mm256_loadu_ps(b.as_ptr().add(p)),
                    ),
                );
            }
            i += 32;
        }
        while i < a.len() {
            out[i] = a[i] * b[i];
            i += 1;
        }
    }
    pub unsafe fn dot(a: &[f32], b: &[f32]) -> f32 {
        let n = a.len() - a.len() % 16;
        let mut acc0 = _mm256_setzero_ps();
        let mut acc1 = _mm256_setzero_ps();
        let mut i = 0;
        while i < n {
            acc0 = _mm256_add_ps(
                acc0,
                _mm256_mul_ps(
                    _mm256_loadu_ps(a.as_ptr().add(i)),
                    _mm256_loadu_ps(b.as_ptr().add(i)),
                ),
            );
            acc1 = _mm256_add_ps(
                acc1,
                _mm256_mul_ps(
                    _mm256_loadu_ps(a.as_ptr().add(i + 8)),
                    _mm256_loadu_ps(b.as_ptr().add(i + 8)),
                ),
            );
            i += 16;
        }
        let mut t = [0f32; 8];
        _mm256_storeu_ps(t.as_mut_ptr(), _mm256_add_ps(acc0, acc1));
        let mut s = 0f32;
        for x in t.iter() {
            s += *x;
        }
        while i < a.len() {
            s += a[i] * b[i];
            i += 1;
        }
        s
    }
}

// AVX-512: интринсики _mm512_* стабилизированы в stable 1.89 (stdarch_x86_avx512).
// cfg has_avx512 выставляет build.rs по версии rustc — на старых stable модуль
// исключается, и уровень Avx512 никогда не выбирается (см. detect/calibrate).
#[cfg(all(target_arch = "x86_64", has_avx512))]
mod avx512 {
    use std::arch::x86_64::*;
    // разворот ×2: 32 f32 за итерацию
    pub unsafe fn add(a: &[f32], b: &[f32], out: &mut [f32]) {
        let n = a.len() - a.len() % 32;
        let mut i = 0;
        while i < n {
            for k in 0..2 {
                let p = i + k * 16;
                _mm512_storeu_ps(
                    out.as_mut_ptr().add(p),
                    _mm512_add_ps(
                        _mm512_loadu_ps(a.as_ptr().add(p)),
                        _mm512_loadu_ps(b.as_ptr().add(p)),
                    ),
                );
            }
            i += 32;
        }
        while i < a.len() {
            out[i] = a[i] + b[i];
            i += 1;
        }
    }
    pub unsafe fn sub(a: &[f32], b: &[f32], out: &mut [f32]) {
        let n = a.len() - a.len() % 32;
        let mut i = 0;
        while i < n {
            for k in 0..2 {
                let p = i + k * 16;
                _mm512_storeu_ps(
                    out.as_mut_ptr().add(p),
                    _mm512_sub_ps(
                        _mm512_loadu_ps(a.as_ptr().add(p)),
                        _mm512_loadu_ps(b.as_ptr().add(p)),
                    ),
                );
            }
            i += 32;
        }
        while i < a.len() {
            out[i] = a[i] - b[i];
            i += 1;
        }
    }
    pub unsafe fn mul(a: &[f32], b: &[f32], out: &mut [f32]) {
        let n = a.len() - a.len() % 32;
        let mut i = 0;
        while i < n {
            for k in 0..2 {
                let p = i + k * 16;
                _mm512_storeu_ps(
                    out.as_mut_ptr().add(p),
                    _mm512_mul_ps(
                        _mm512_loadu_ps(a.as_ptr().add(p)),
                        _mm512_loadu_ps(b.as_ptr().add(p)),
                    ),
                );
            }
            i += 32;
        }
        while i < a.len() {
            out[i] = a[i] * b[i];
            i += 1;
        }
    }
    pub unsafe fn dot(a: &[f32], b: &[f32]) -> f32 {
        let n = a.len() - a.len() % 32;
        let mut acc0 = _mm512_setzero_ps();
        let mut acc1 = _mm512_setzero_ps();
        let mut i = 0;
        while i < n {
            acc0 = _mm512_add_ps(
                acc0,
                _mm512_mul_ps(
                    _mm512_loadu_ps(a.as_ptr().add(i)),
                    _mm512_loadu_ps(b.as_ptr().add(i)),
                ),
            );
            acc1 = _mm512_add_ps(
                acc1,
                _mm512_mul_ps(
                    _mm512_loadu_ps(a.as_ptr().add(i + 16)),
                    _mm512_loadu_ps(b.as_ptr().add(i + 16)),
                ),
            );
            i += 32;
        }
        let s = _mm512_reduce_add_ps(_mm512_add_ps(acc0, acc1));
        let mut tail = s;
        while i < a.len() {
            tail += a[i] * b[i];
            i += 1;
        }
        tail
    }
}

#[cfg(target_arch = "x86_64")]
pub fn level_add(l: SimdLevel, a: &[f32], b: &[f32], out: &mut [f32]) {
    match l {
        // без stdarch avx512 (rustc < 1.89) — тот же профиль на AVX2
        #[cfg(has_avx512)]
        SimdLevel::Avx512 => unsafe { avx512::add(a, b, out) },
        #[cfg(not(has_avx512))]
        SimdLevel::Avx512 => unsafe { avx::add(a, b, out) },
        SimdLevel::Avx2 => unsafe { avx::add(a, b, out) },
        SimdLevel::Sse2 => unsafe { sse::add(a, b, out) },
        SimdLevel::Scalar => scalar::add(a, b, out),
    }
}
#[cfg(not(target_arch = "x86_64"))]
pub fn level_add(l: SimdLevel, a: &[f32], b: &[f32], out: &mut [f32]) {
    let _ = l;
    scalar::add(a, b, out)
}

fn dispatch2(a: &[f32], b: &[f32], out: &mut [f32], op: u8) {
    #[cfg(target_arch = "x86_64")]
    {
        match active_level() {
            SimdLevel::Avx512 => unsafe {
                let m = avx512_if(op);
                m(a, b, out);
            },
            SimdLevel::Avx2 => unsafe {
                let m = avx_if(op);
                m(a, b, out);
            },
            _ => unsafe {
                let m = sse_if(op);
                m(a, b, out);
            },
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = op;
        scalar::add(a, b, out) // заглушка, не используется (см. ниже)
    }
}

#[cfg(target_arch = "x86_64")]
fn avx_if(op: u8) -> unsafe fn(&[f32], &[f32], &mut [f32]) {
    match op {
        1 => avx::sub,
        2 => avx::mul,
        _ => avx::add,
    }
}
#[cfg(target_arch = "x86_64")]
fn sse_if(op: u8) -> unsafe fn(&[f32], &[f32], &mut [f32]) {
    match op {
        1 => sse::sub,
        2 => sse::mul,
        3 => sse::min,
        4 => sse::max,
        _ => sse::add,
    }
}
#[cfg(all(target_arch = "x86_64", has_avx512))]
fn avx512_if(op: u8) -> unsafe fn(&[f32], &[f32], &mut [f32]) {
    match op {
        1 => avx512::sub,
        2 => avx512::mul,
        _ => avx512::add,
    }
}
#[cfg(all(target_arch = "x86_64", not(has_avx512)))]
fn avx512_if(op: u8) -> unsafe fn(&[f32], &[f32], &mut [f32]) {
    // rustc < 1.89: уровень Avx512 не выбирается (detect), а если всё же
    // пришёл — считаем на AVX2, ничего не ломается
    match op {
        1 => avx::sub,
        2 => avx::mul,
        _ => avx::add,
    }
}

pub fn vadd(a: &[f32], b: &[f32], out: &mut [f32]) {
    dispatch2(a, b, out, 0);
}
pub fn vsub(a: &[f32], b: &[f32], out: &mut [f32]) {
    dispatch2(a, b, out, 1);
}
pub fn vmul(a: &[f32], b: &[f32], out: &mut [f32]) {
    dispatch2(a, b, out, 2);
}
pub fn vmin(a: &[f32], b: &[f32], out: &mut [f32]) {
    #[cfg(target_arch = "x86_64")]
    {
        unsafe { sse::min(a, b, out) };
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        scalar::min(a, b, out)
    }
}
pub fn vmax(a: &[f32], b: &[f32], out: &mut [f32]) {
    #[cfg(target_arch = "x86_64")]
    {
        unsafe { sse::max(a, b, out) };
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        scalar::max(a, b, out)
    }
}

pub fn vdot(a: &[f32], b: &[f32]) -> f32 {
    #[cfg(target_arch = "x86_64")]
    {
        match active_level() {
            #[cfg(has_avx512)]
            SimdLevel::Avx512 => unsafe { avx512::dot(a, b) },
            #[cfg(not(has_avx512))]
            SimdLevel::Avx512 => unsafe { avx::dot(a, b) },
            SimdLevel::Avx2 => unsafe { avx::dot(a, b) },
            _ => unsafe { sse::dot(a, b) },
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        scalar::dot(a, b)
    }
}

// ——— бенчмарк ———

fn timed<F: FnMut()>(iters: usize, mut f: F) -> f64 {
    let t0 = Instant::now();
    for _ in 0..iters {
        f();
    }
    t0.elapsed().as_secs_f64() * 1000.0
}

pub fn bench_run() {
    println!(
        "обнаружено по флагам: {}; выбрано калибровкой: {}",
        level_name(detect()),
        level_name(active_level())
    );

    // Тест A: большой массив — упор в пропускную способность памяти
    // (честно показываем, где SIMD не помогает: риск из ТЗ §13).
    {
        let n = 1usize << 20;
        let a: Vec<f32> = (0..n).map(|i| (i as f64 * 0.001) as f32).collect();
        let b: Vec<f32> = (0..n).map(|i| (i as f64).sqrt() as f32).collect();
        let mut out = vec![0f32; n];
        let iters = 100usize;
        let t_sc = timed(iters, || scalar::add(&a, &b, &mut out));
        let lvl = active_level();
        let t_sim = timed(iters, || level_add(lvl, &a, &b, &mut out));
        println!(
            "[simd] тест A — vec-add 1M f32 × {} (вне кэша): скаляр {:.1} мс, {} {:.1} мс → {:.2}×  (упор в память)",
            iters,
            t_sc,
            level_name(lvl),
            t_sim,
            t_sc / t_sim.max(1e-9)
        );
    }

    // Тест B/C: данные в L1 — сценарий физики (критерий ТЗ §4.5: ≥2×).
    {
        let n = 2048usize;
        let a: Vec<f32> = (0..n).map(|i| (i % 97) as f32 * 0.5).collect();
        let b: Vec<f32> = (0..n).map(|i| (i % 89) as f32 * 0.25).collect();
        let mut out = vec![0f32; n];
        let iters = 20000usize;
        // прогрев + защита от выбросов
        level_add(active_level(), &a, &b, &mut out);
        let t_sc = timed(iters, || scalar::add(&a, &b, &mut out));
        let lvl = active_level();
        let t_sim = timed(iters, || level_add(lvl, &a, &b, &mut out));
        std::hint::black_box(out[0]);
        println!(
            "[simd] тест B — vec-add 2048 f32 (L1) × {}: скаляр {:.2} мс, {} {:.2} мс → {:.2}×",
            iters,
            t_sc,
            level_name(lvl),
            t_sim,
            t_sc / t_sim.max(1e-9)
        );

        let mut sink1 = 0f32;
        let t_sc = timed(iters, || {
            sink1 = scalar::dot(&a, &b);
        });
        let mut sink2 = 0f32;
        let t_sim = timed(iters, || {
            sink2 = vdot(&a, &b);
        });
        println!(
            "[simd] тест C — vec-dot 2048 f32 × {}: скаляр {:.2} мс, {} {:.2} мс → {:.2}×  (контроль сумм: {} / {})",
            iters,
            t_sc,
            level_name(lvl),
            t_sim,
            t_sc / t_sim.max(1e-9),
            sink1, sink2
        );
    }
}

// ——— Lisp-нативы ———

pub fn to_f32s(v: &Value) -> Result<Vec<f32>, String> {
    match v {
        Value::F32Vec(r) => Ok(r.borrow().clone()),
        Value::Vector(r) => {
            let b = r.borrow();
            let mut out = Vec::with_capacity(b.len());
            for x in b.iter() {
                out.push(num_val(x)? as f32);
            }
            Ok(out)
        }
        Value::List(l) => {
            let mut out = Vec::with_capacity(l.len());
            for x in l {
                out.push(num_val(x)? as f32);
            }
            Ok(out)
        }
        other => Err(format!("нужен вектор чисел: {}", fmt_value(other))),
    }
}

fn ret_f32(v: Vec<f32>) -> Value {
    Value::F32Vec(Rc::new(RefCell::new(v)))
}

fn binop(env: Gc, name: &str, f: for<'a> fn(&'a [f32], &'a [f32], &'a mut [f32])) {
    let name_s: String = name.to_string();
    env_define(
        env,
        name,
        Value::Native(
            Rc::new(move |args: &[Value]| {
                need_args(args, 2)?;
                let a = to_f32s(&args[0])?;
                let b = to_f32s(&args[1])?;
                if a.len() != b.len() {
                    return Err(format!(
                        "{}: длины не равны ({} и {})",
                        name_s,
                        a.len(),
                        b.len()
                    ));
                }
                let mut o = vec![0f32; a.len()];
                f(&a, &b, &mut o);
                Ok(ret_f32(o))
            }),
            name.to_string(),
        ),
    );
}

pub fn register_simd(env: Gc) {
    binop(env, "vec-add", vadd);
    binop(env, "vec-sub", vsub);
    binop(env, "vec-mul", vmul);
    binop(env, "vec-min", vmin);
    binop(env, "vec-max", vmax);

    fn def<F: Fn(&[Value]) -> Result<Value, String> + 'static>(env: Gc, name: &str, f: F) {
        env_define(env, name, Value::Native(Rc::new(f), name.to_string()));
    }
    use need_args as need;

    def(env, "vec-scale", |args| {
        need(args, 2)?;
        let a = to_f32s(&args[0])?;
        let s = num_val(&args[1])? as f32;
        let mut o = vec![0f32; a.len()];
        for (i, x) in a.iter().enumerate() {
            o[i] = x * s;
        }
        Ok(ret_f32(o))
    });
    def(env, "vec-dot", |args| {
        need(args, 2)?;
        let a = to_f32s(&args[0])?;
        let b = to_f32s(&args[1])?;
        if a.len() != b.len() {
            return Err("vec-dot: длины не равны".into());
        }
        Ok(Value::Float(vdot(&a, &b) as f64))
    });
    def(env, "vec-length", |args| {
        need(args, 1)?;
        let a = to_f32s(&args[0])?;
        Ok(Value::Float((vdot(&a, &a) as f64).sqrt()))
    });
    def(env, "vec-normalize", |args| {
        need(args, 1)?;
        let a = to_f32s(&args[0])?;
        let len = (vdot(&a, &a) as f64).sqrt() as f32;
        if len < 1e-12 {
            return Ok(ret_f32(vec![0f32; a.len()]));
        }
        let mut o = vec![0f32; a.len()];
        for (i, x) in a.iter().enumerate() {
            o[i] = x / len;
        }
        Ok(ret_f32(o))
    });
    def(env, "vec-clamp", |args| {
        need(args, 3)?;
        let a = to_f32s(&args[0])?;
        let lo = num_val(&args[1])? as f32;
        let hi = num_val(&args[2])? as f32;
        let mut o = vec![0f32; a.len()];
        for (i, x) in a.iter().enumerate() {
            o[i] = x.max(lo).min(hi);
        }
        Ok(ret_f32(o))
    });
    def(env, "mat4-mul", |args| {
        need(args, 2)?;
        let a = to_f32s(&args[0])?;
        let b = to_f32s(&args[1])?;
        if a.len() != 16 || b.len() != 16 {
            return Err("mat4-mul: нужны матрицы 4x4 (16 f32)".into());
        }
        let mut o = [0f32; 16];
        for i in 0..4 {
            for j in 0..4 {
                let mut s = 0f32;
                for k in 0..4 {
                    s += a[i * 4 + k] * b[k * 4 + j];
                }
                o[i * 4 + j] = s;
            }
        }
        Ok(ret_f32(o.to_vec()))
    });
    def(env, "mat4-mul-vec4", |args| {
        need(args, 2)?;
        let m = to_f32s(&args[0])?;
        let v = to_f32s(&args[1])?;
        if m.len() != 16 || v.len() != 4 {
            return Err("mat4-mul-vec4: нужна матрица 4x4 и vec4".into());
        }
        let mut o = [0f32; 4];
        // v0.2: согласовано с mat4-mul — теперь o = M·v (row-major, вектор-столбец).
        // Раньше считалось v·M (транспонированно), что ломало композицию матриц.
        for i in 0..4 {
            o[i] =
                m[i * 4] * v[0] + m[i * 4 + 1] * v[1] + m[i * 4 + 2] * v[2] + m[i * 4 + 3] * v[3];
        }
        Ok(ret_f32(o.to_vec()))
    });
}

// ————————————————————————————————————————————————————————————————
// Этап 3: SIMD-токенизатор — byte-class скан для горячего пути reader'а.
//
// Старый посимвольный reader (src/interp.rs, parse_program_scalar) на каждый
// символ делал is_whitespace() и contains(ch) по 9-байтной строке-разделителю —
// O(N) с тяжёлой константой. Здесь классификация байтов делается вектором:
// 16 байт за раз (SSE2), точные множества через pcmpeqb + movemask:
//   WS    = {09..0D, 20}  — ASCII-пробелы (ровно как char::is_whitespace у ASCII)
//   DELIM = { ( ) [ ] ' ` , ; " }
//   NONASCII = любой байт >= 0x80 — классифицируется скалярно (UTF-8:
//   юникодный пробел U+00A0/U+2028 и т.п. — по-старому пробел; символ имени —
//   по-старому часть атома). Для 100% ASCII-источников скаляр не выполняется.
// Старый путь сохранён как fallback и для взаимной сверки AST (--bench-parse).

/// Класс байта, на котором останавливается лексический скан.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hit {
    /// ASCII-пробельный байт (09..0D, 20)
    Ws(u8),
    /// разделитель/синтаксис лексем: ( ) [ ] ' ` , ; "
    Delim(u8),
    /// байт >= 0x80 — нужен скалярный разбор (UTF-8)
    NonAscii(u8),
}

fn hit_of(b: u8) -> Hit {
    match b {
        0x09..=0x0D | 0x20 => Hit::Ws(b),
        b'(' | b')' | b'[' | b']' | b'\'' | b'`' | b',' | b';' | b'"' => Hit::Delim(b),
        _ => Hit::NonAscii(b),
    }
}

/// Скалярный fallback (и хвост < 16 байт): ровно та же семантика классов.
pub fn next_hit_scalar(bytes: &[u8], mut pos: usize) -> Option<(usize, Hit)> {
    while pos < bytes.len() {
        let b = bytes[pos];
        if b < 0x80 {
            if matches!(b, 0x09..=0x0D | 0x20)
                || matches!(
                    b,
                    b'(' | b')' | b'[' | b']' | b'\'' | b'`' | b',' | b';' | b'"'
                )
            {
                return Some((pos, hit_of(b)));
            }
            pos += 1;
        } else {
            return Some((pos, Hit::NonAscii(b)));
        }
    }
    None
}

/// Первый «особый» байт с позиции pos: Ws | Delim | NonAscii; None — конец.
/// Портативно: на x86_64 — SSE2, иначе скаляр.
pub fn next_hit(bytes: &[u8], pos: usize) -> Option<(usize, Hit)> {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("sse2") {
            return next_hit_sse2(bytes, pos);
        }
    }
    next_hit_scalar(bytes, pos)
}

/// memchr-стиль: первый байт `target` с позиции pos (для пропуска ;-комментариев
/// и сканов внутри строк). Портативно, на x86_64 — SSE2.
pub fn find_byte(bytes: &[u8], pos: usize, target: u8) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("sse2") {
            return find_byte_sse2(bytes, pos, target);
        }
    }
    (pos..bytes.len()).find(|&i| bytes[i] == target)
}

#[cfg(target_arch = "x86_64")]
static WS_SET: [u8; 16] = [
    0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
#[cfg(target_arch = "x86_64")]
static DL_SET: [u8; 16] = [
    b'(', b')', b'[', b']', b'\'', b'`', b',', b';', b'"', 0, 0, 0, 0, 0, 0, 0,
];

#[cfg(target_arch = "x86_64")]
#[inline]
fn next_hit_sse2(bytes: &[u8], mut pos: usize) -> Option<(usize, Hit)> {
    use std::arch::x86_64::*;
    unsafe {
        // векторы-наборы читаются из static (без постройки на стеке каждый вызов)
        let ws_v = _mm_loadu_si128(WS_SET.as_ptr() as *const __m128i);
        let dl_v = _mm_loadu_si128(DL_SET.as_ptr() as *const __m128i);
        let ws_vecs: [__m128i; 6] = [
            _mm_set1_epi8(0x09),
            _mm_set1_epi8(0x0A),
            _mm_set1_epi8(0x0B),
            _mm_set1_epi8(0x0C),
            _mm_set1_epi8(0x0D),
            _mm_set1_epi8(0x20),
        ];
        let dl_vecs: [__m128i; 9] = [
            _mm_set1_epi8(b'(' as i8),
            _mm_set1_epi8(b')' as i8),
            _mm_set1_epi8(b'[' as i8),
            _mm_set1_epi8(b']' as i8),
            _mm_set1_epi8(b'\'' as i8),
            _mm_set1_epi8(b'`' as i8),
            _mm_set1_epi8(b',' as i8),
            _mm_set1_epi8(b';' as i8),
            _mm_set1_epi8(b'"' as i8),
        ];
        let zero = _mm_setzero_si128();
        let sign_mask_v = _mm_cmplt_epi8(zero, zero); // все единицы
        while pos + 16 <= bytes.len() {
            let v = _mm_loadu_si128(bytes.as_ptr().add(pos) as *const __m128i);
            // WS: точные 6 сравнений (байт 0 в наборе не даёт ложных срабатываний:
            // в полном блоке текста нулевых байтов нет, а байт 0 — атом-символ)
            let mut m_ws = _mm_movemask_epi8(_mm_cmpeq_epi8(v, ws_vecs[0]));
            for w in ws_vecs[1..].iter() {
                m_ws |= _mm_movemask_epi8(_mm_cmpeq_epi8(v, *w));
            }
            let mut m_dl = _mm_movemask_epi8(_mm_cmpeq_epi8(v, dl_vecs[0]));
            for d in dl_vecs[1..].iter() {
                m_dl |= _mm_movemask_epi8(_mm_cmpeq_epi8(v, *d));
            }
            // не-ASCII (байты >= 0x80 знаковые отрицательные)
            let m_na = _mm_movemask_epi8(_mm_cmplt_epi8(v, zero));
            let m_all = m_ws | m_dl | m_na;
            if m_all == 0 {
                pos += 16;
                continue;
            }
            let _ = (&ws_v, &dl_v, &sign_mask_v);
            let bit = m_all.trailing_zeros() as usize;
            let b = bytes[pos + bit];
            let hit = hit_of(b);
            if b == 0 {
                // нулевой байт — атом-символ (семантика старого пути), не остановка
                return next_hit_scalar(bytes, pos + bit + 1).map(|(i, h)| (i, h));
            }
            return Some((pos + bit, hit));
        }
        next_hit_scalar(bytes, pos)
    }
}

#[cfg(target_arch = "x86_64")]
fn find_byte_sse2(bytes: &[u8], mut pos: usize, target: u8) -> Option<usize> {
    use std::arch::x86_64::*;
    unsafe {
        let t = _mm_set1_epi8(target as i8);
        while pos + 16 <= bytes.len() {
            let v = _mm_loadu_si128(bytes.as_ptr().add(pos) as *const __m128i);
            let m = _mm_movemask_epi8(_mm_cmpeq_epi8(v, t));
            if m != 0 {
                return Some(pos + m.trailing_zeros() as usize);
            }
            pos += 16;
        }
        (pos..bytes.len()).find(|&i| bytes[i] == target)
    }
}
