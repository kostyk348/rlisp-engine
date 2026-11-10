//! src/post.rs — Этап 19 «Пост-обработка»: экранные эффекты поверх
//! фреймбуфера RGBA8 ПОСЛЕ 3D-прохода и ДО HUD (HUD рисуется отложенным
//! 2D-слоем позже — остаётся чётким).
//!
//! Конвейер: bloom (сбор ярких → downsample ¼ → 2 box-blur → возврат) →
//! экспозиция + мягкое плечо (Reinhard с белой точкой) → виньетка → зерно.
//! Детерминизм: никакого rand — хэш (пиксель, кадр); побитово одинаково
//! при том же входе и номере кадра. Работает и в software, и в wgpu-режиме
//! (fb у обоих программный после readback).

/// параметры одного кадра
#[derive(Clone, Copy)]
pub struct PostParams {
    /// экспозиция (1.0 — без сдвига)
    pub exposure: f32,
    /// сила bloom (0 — выкл); ярче 0.70 по luma собирается в ореолы
    pub bloom: f32,
    /// сила виньетки (0 — выкл)
    pub vignette: f32,
    /// сила зерна (0 — выкл), доля от 255
    pub grain: f32,
}

impl Default for PostParams {
    fn default() -> Self {
        PostParams {
            exposure: 1.0,
            bloom: 0.0,
            vignette: 0.0,
            grain: 0.0,
        }
    }
}

/// порог начала плеча: ниже — тождественность (сцена display-referred,
/// средние тона трогать нельзя), выше — мягкая компрессия к 1.0
const SHOULDER_K: f32 = 0.85;
/// порог luma для bloom
const BLOOM_THR: f32 = 0.70;

/// мягкое плечо: y(x)=x при x≤K; выше — 1−exp-компрессия к единице
#[inline]
fn shoulder(x: f32) -> f32 {
    if x <= SHOULDER_K {
        x
    } else {
        SHOULDER_K + (1.0 - SHOULDER_K) * (1.0 - (-(x - SHOULDER_K) / (1.0 - SHOULDER_K)).exp())
    }
}

/// детерминированный хэш (пиксель, кадр) → [0,1)
#[inline]
fn hash01(i: usize, frame: u64) -> f32 {
    let mut x = (i as u64) ^ (frame.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 33;
    ((x >> 11) & 0x7FFFFF) as f32 / 0x7FFFFF as f32
}

#[inline]
fn luma(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// полный проход пост-обработки по fb
pub fn apply(hal: &mut Hal, p: &PostParams) {
    let (w, h) = (hal.w, hal.h);
    let frame = hal.frame;
    let fb = &mut hal.fb;
    let n = w * h;

    // ——— bloom: сбор ярких в буфер ¼ разрешения ———
    if p.bloom > 0.0 {
        let (w2, h2) = (w.max(7) / 4, h.max(7) / 4);
        let mut small = vec![0.0f32; w2 * h2 * 3];
        // downsample: среднее 4×4 блока, только пиксели ярче порога
        for y2 in 0..h2 {
            for x2 in 0..w2 {
                let mut acc = [0.0f32; 3];
                let mut cnt = 0.0f32;
                for dy in (0..4).step_by(2) {
                    let y = y2 * 4 + dy;
                    if y >= h {
                        break;
                    }
                    for dx in (0..4).step_by(2) {
                        let x = x2 * 4 + dx;
                        if x >= w {
                            break;
                        }
                        let pi = (y * w + x) * 4;
                        let r = fb[pi] as f32 / 255.0;
                        let g = fb[pi + 1] as f32 / 255.0;
                        let b = fb[pi + 2] as f32 / 255.0;
                        if luma(r, g, b) > BLOOM_THR {
                            acc[0] += r;
                            acc[1] += g;
                            acc[2] += b;
                            cnt += 1.0;
                        }
                    }
                }
                if cnt > 0.0 {
                    let si = (y2 * w2 + x2) * 3;
                    small[si] = acc[0] / cnt;
                    small[si + 1] = acc[1] / cnt;
                    small[si + 2] = acc[2] / cnt;
                }
            }
        }
        // два сепарабельных box-blur (радиус 2) по малому буферу
        let mut tmp = vec![0.0f32; w2 * h2 * 3];
        for _ in 0..2 {
            // горизонталь
            for y in 0..h2 {
                for x in 0..w2 {
                    let mut acc = [0.0f32; 3];
                    let mut c = 0.0f32;
                    for k in -2i32..=2 {
                        let xx = x as i32 + k;
                        if xx < 0 || xx >= w2 as i32 {
                            continue;
                        }
                        let si = ((y * w2) + xx as usize) * 3;
                        acc[0] += small[si];
                        acc[1] += small[si + 1];
                        acc[2] += small[si + 2];
                        c += 1.0;
                    }
                    let di = (y * w2 + x) * 3;
                    tmp[di] = acc[0] / c;
                    tmp[di + 1] = acc[1] / c;
                    tmp[di + 2] = acc[2] / c;
                }
            }
            // вертикаль (результат обратно в small)
            for y in 0..h2 {
                for x in 0..w2 {
                    let mut acc = [0.0f32; 3];
                    let mut c = 0.0f32;
                    for k in -2i32..=2 {
                        let yy = y as i32 + k;
                        if yy < 0 || yy >= h2 as i32 {
                            continue;
                        }
                        let si = ((yy as usize) * w2 + x) * 3;
                        acc[0] += tmp[si];
                        acc[1] += tmp[si + 1];
                        acc[2] += tmp[si + 2];
                        c += 1.0;
                    }
                    let di = (y * w2 + x) * 3;
                    small[di] = acc[0] / c;
                    small[di + 1] = acc[1] / c;
                    small[di + 2] = acc[2] / c;
                }
            }
        }
        // возврат: fb += bloom * blurred
        for y in 0..h {
            let y2 = (y / 4).min(h2 - 1);
            for x in 0..w {
                let x2 = (x / 4).min(w2 - 1);
                let si = (y2 * w2 + x2) * 3;
                let pi = (y * w + x) * 4;
                for ch in 0..3 {
                    let base = fb[pi + ch] as f32 / 255.0;
                    let v = base + p.bloom * small[si + ch];
                    fb[pi + ch] = (v.min(1.0) * 255.0 + 0.5) as u8;
                }
            }
        }
    }

    // ——— экспозиция + мягкое плечо + виньетка + зерно: один проход ———
    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;
    let max_d = (cx * cx + cy * cy).sqrt();
    for i in 0..n {
        let pi = i * 4;
        let x = (i % w) as f32;
        let y = (i / w) as f32;
        // виньетка: затемнение к углам
        let dx = x - cx;
        let dy = y - cy;
        let d = (dx * dx + dy * dy).sqrt() / max_d;
        let vig = if p.vignette > 0.0 {
            (1.0 - p.vignette * d * d).max(0.0)
        } else {
            1.0
        };
        for ch in 0..3 {
            let mut v = fb[pi + ch] as f32 / 255.0;
            // экспозиция → плечо: средние тоны линейны, яркое мягко жмётся
            // к 1.0 вместо плоского клипа (солнце/блики сохраняют градиент)
            v = shoulder(v * p.exposure);
            v *= vig;
            if p.grain > 0.0 {
                let g = (hash01(pi + ch, frame) - 0.5) * p.grain;
                v += g;
            }
            fb[pi + ch] = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
    }
}

use crate::hal::Hal;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hal::OutputMode;

    fn mk(w: usize, h: usize) -> Hal {
        Hal::new(OutputMode::Silent, w, h)
    }

    #[test]
    fn determinism_bitwise() {
        let mut a = mk(64, 48);
        let mut b = mk(64, 48);
        for i in (0..a.fb.len()).step_by(7) {
            let v = ((i * 37) % 251) as u8;
            a.fb[i] = v;
            b.fb[i] = v;
        }
        a.frame = 1234;
        b.frame = 1234;
        let p = PostParams {
            exposure: 1.15,
            bloom: 0.6,
            vignette: 0.35,
            grain: 0.05,
        };
        apply(&mut a, &p);
        apply(&mut b, &p);
        assert_eq!(a.fb, b.fb, "пост недетерминирован");
    }

    #[test]
    fn bloom_brightens_neighbours_of_bright_spot() {
        let mut a = mk(64, 48);
        let center = (32 * 4) + 24 * 4 * 64;
        for ch in 0..3 {
            a.fb[center + ch] = 255;
        }
        let neighbour = center + 4 * 12; // на 12 пикселей правее
        let before = a.fb[neighbour];
        apply(
            &mut a,
            &PostParams {
                bloom: 0.8,
                ..Default::default()
            },
        );
        assert!(a.fb[neighbour] > before, "bloom не осветлил соседей");
        assert!(
            a.fb[center] >= 240,
            "яркий центр остался ярким (плечо жмёт до ~241)"
        );
    }

    #[test]
    fn shoulder_compresses_highlights_keeps_midtones() {
        let mut a = mk(32, 32);
        // левая половина — 100 (средний тон), правая — 250 (яркое)
        for y in 0..32 {
            for x in 0..32 {
                let v = if x < 16 { 100 } else { 250 };
                let p = (y * 32 + x) * 4;
                a.fb[p] = v;
                a.fb[p + 1] = v;
                a.fb[p + 2] = v;
            }
        }
        apply(
            &mut a,
            &PostParams {
                exposure: 1.3,
                ..Default::default()
            },
        );
        let mid = a.fb[(10 * 32 + 8) * 4] as i32;
        let hi = a.fb[(10 * 32 + 24) * 4] as i32;
        // средний тон строго линейный: 100·1.3 = 130 (плечо не трогает ≤0.85)
        assert!((mid - 130).abs() <= 4, "mid {}", mid);
        // яркое сжато плечом: 0.980·1.3 = 1.274 → ~0.991 (253), а не клип 255
        assert!((250 - hi).abs() <= 3, "hi {}", hi);
    }

    #[test]
    fn vignette_darkens_corners() {
        let mut a = mk(64, 64);
        for i in 0..(64 * 64) {
            let p = i * 4;
            a.fb[p] = 200;
            a.fb[p + 1] = 200;
            a.fb[p + 2] = 200;
        }
        apply(
            &mut a,
            &PostParams {
                vignette: 0.45,
                ..Default::default()
            },
        );
        let center = a.fb[(32 * 64 + 32) * 4];
        let corner = a.fb[(2 * 64 + 2) * 4];
        assert!(
            corner < center,
            "угол {} не темнее центра {}",
            corner,
            center
        );
        assert!(center >= 195, "центр не должен темнеть: {}", center);
    }

    #[test]
    fn grain_varies_between_frames_but_bounded() {
        let mkfb = |frame: u64| {
            let mut h = mk(48, 48);
            for i in 0..(48 * 48) {
                let p = i * 4;
                h.fb[p] = 120;
                h.fb[p + 1] = 120;
                h.fb[p + 2] = 120;
                h.frame = frame;
            }
            h
        };
        let mut a = mkfb(1);
        let mut b = mkfb(2);
        let p = PostParams {
            grain: 0.06,
            ..Default::default()
        };
        apply(&mut a, &p);
        apply(&mut b, &p);
        // зерно меняется между кадрами
        assert_ne!(a.fb[..96], b.fb[..96], "зерно одинаково между кадрами");
        // но ограничено: |diff| ≤ grain·255 + округление
        let mut maxd = 0i32;
        for k in 0..a.fb.len() {
            maxd = maxd.max((a.fb[k] as i32 - b.fb[k] as i32).abs());
        }
        assert!(maxd <= 20, "зерно слишком сильное: {}", maxd);
        // и не уводит за границы при базе 120 и grain 0.06 —
        // только цветовые каналы (альфа-байт пост не трогает)
        for i in 0..(48 * 48) {
            for ch in 0..3 {
                let v = a.fb[i * 4 + ch];
                assert!((95..=146).contains(&v), "v {}", v);
            }
        }
    }

    #[test]
    fn bench_cost_400x300() {
        let mut h = mk(400, 300);
        for i in 0..h.fb.len() {
            h.fb[i] = ((i % 251) as u8).max(40);
        }
        h.frame = 7;
        let p = PostParams {
            exposure: 1.1,
            bloom: 0.55,
            vignette: 0.3,
            grain: 0.03,
        };
        let t0 = std::time::Instant::now();
        for _ in 0..30 {
            apply(&mut h, &p);
        }
        let dt = t0.elapsed() / 30;
        // информационный замер (тайминг в тесте — флейк на загруженной
        // машине); честный release-протокол — docs/benchmark.md
        println!("gpu3d-post 400×300: {:?} на кадр (debug-профиль)", dt);
        assert!(dt.as_secs() < 10, "пост не выполняется вовсе");
    }
}
