// src/simd/types.rs — Определение уровней SIMD, детекция и калибровка

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
        use super::kernels::level_add;
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
        // Возвращаем не «что намерили», а «что действительно в силе»: если
        // ACTIVE уже инициализирован (например, active_level() вызвали из
        // другого потока тестов), set() молча ничего не делает, и прежний
        // возврат `best` расходился с реальностью — тест падал на ровном месте.
        active_level()
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = ACTIVE.set(SimdLevel::Scalar);
        active_level()
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
