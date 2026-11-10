//! src/particles.rs — Этап 14 «Частицы»: CPU-пул 3D-частиц с детерминированным
//! LCG-разбросом (без rand — тот же вход даёт побитово тот же кадр).
//! Рендер про частицы не знает: натив отдаёт плоский буфер
//! [x y z size r g b life]×n, сцена сама собирает из него инстансы
//! (draw-batch). Пул фиксированный (4096): при переполнении перезаписывается
//! случайный слот по LCG — выделений нет, OOM нет.

// нативы (part-emit!/part-burst!) — широкие Lisp-сигнатуры по дизайну API
#![allow(clippy::too_many_arguments)]

pub const MAX_P: usize = 4096;

use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
pub struct Particles {
    pub x: Vec<f32>,
    pub y: Vec<f32>,
    pub z: Vec<f32>,
    pub vx: Vec<f32>,
    pub vy: Vec<f32>,
    pub vz: Vec<f32>,
    pub ttl: Vec<f32>,
    pub age: Vec<f32>,
    pub size: Vec<f32>,
    pub r: Vec<f32>,
    pub g: Vec<f32>,
    pub b: Vec<f32>,
    /// LCG: seed = seed·1664525 + 1013904223 (Numerical Recipes)
    pub seed: u32,
}

impl Particles {
    pub fn new() -> Self {
        Particles {
            x: Vec::new(),
            y: Vec::new(),
            z: Vec::new(),
            vx: Vec::new(),
            vy: Vec::new(),
            vz: Vec::new(),
            ttl: Vec::new(),
            age: Vec::new(),
            size: Vec::new(),
            r: Vec::new(),
            g: Vec::new(),
            b: Vec::new(),
            seed: 0x9E37_79B9,
        }
    }

    /// равномерное в [-1, 1]
    #[inline]
    fn rnd(&mut self) -> f32 {
        self.seed = self.seed.wrapping_mul(1664525).wrapping_add(1013904223);
        ((self.seed >> 8) as f32 / 8388608.0) - 1.0 // 2^24
    }

    fn emit(
        &mut self,
        x: f32,
        y: f32,
        z: f32,
        vx: f32,
        vy: f32,
        vz: f32,
        ttl: f32,
        size: f32,
        r: f32,
        g: f32,
        b: f32,
    ) {
        if self.x.len() >= MAX_P {
            // переполнение: перезаписать детерминированный слот
            self.seed = self.seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let i = ((self.seed >> 8) as usize) % MAX_P;
            self.x[i] = x;
            self.y[i] = y;
            self.z[i] = z;
            self.vx[i] = vx;
            self.vy[i] = vy;
            self.vz[i] = vz;
            self.ttl[i] = ttl;
            self.age[i] = 0.0;
            self.size[i] = size;
            self.r[i] = r;
            self.g[i] = g;
            self.b[i] = b;
            return;
        }
        self.x.push(x);
        self.y.push(y);
        self.z.push(z);
        self.vx.push(vx);
        self.vy.push(vy);
        self.vz.push(vz);
        self.ttl.push(ttl);
        self.age.push(0.0);
        self.size.push(size);
        self.r.push(r);
        self.g.push(g);
        self.b.push(b);
    }

    /// залп: n частиц из точки, направление — равномерно по сфере
    /// (отбраковка куба по сфере, ограничено 20 попытками — детерминизм),
    /// скорость = базовая + dir·speed·(1 + spread·u), u ∈ [-1, 1]
    #[allow(clippy::too_many_arguments)]
    pub fn burst(
        &mut self,
        n: u32,
        x: f32,
        y: f32,
        z: f32,
        bvx: f32,
        bvy: f32,
        bvz: f32,
        speed: f32,
        spread: f32,
        ttl: f32,
        size: f32,
        r: f32,
        g: f32,
        b: f32,
    ) {
        for _ in 0..n {
            let (mut dx, mut dy, mut dz) = (0.0f32, 0.0f32, 0.0f32);
            for _ in 0..20 {
                dx = self.rnd();
                dy = self.rnd();
                dz = self.rnd();
                let l2 = dx * dx + dy * dy + dz * dz;
                if l2 > 0.01 && l2 <= 1.0 {
                    let l = l2.sqrt();
                    dx /= l;
                    dy /= l;
                    dz /= l;
                    break;
                }
            }
            let k = speed * (1.0 + spread * self.rnd());
            let t = ttl * (0.75 + 0.25 * (self.rnd() + 1.0) * 0.5);
            self.emit(
                x,
                y,
                z,
                bvx + dx * k,
                bvy + dy * k,
                bvz + dz * k,
                t,
                size,
                r,
                g,
                b,
            );
        }
    }

    /// шаг: гравитация, линейный drag, старение; умершие вычищаются
    /// swap-удалением (порядок в пуле меняется, но dump идёт по пулу —
    /// при том же входе порядок тот же: детерминизм сохранён)
    pub fn step(&mut self, dt: f32, gx: f32, gy: f32, gz: f32, drag: f32) {
        let f = (1.0 - drag * dt).max(0.0);
        let mut i = 0;
        while i < self.x.len() {
            self.age[i] += dt;
            if self.age[i] >= self.ttl[i] {
                let last = self.x.len() - 1;
                if i != last {
                    self.x[i] = self.x[last];
                    self.y[i] = self.y[last];
                    self.z[i] = self.z[last];
                    self.vx[i] = self.vx[last];
                    self.vy[i] = self.vy[last];
                    self.vz[i] = self.vz[last];
                    self.ttl[i] = self.ttl[last];
                    self.age[i] = self.age[last];
                    self.size[i] = self.size[last];
                    self.r[i] = self.r[last];
                    self.g[i] = self.g[last];
                    self.b[i] = self.b[last];
                }
                self.x.truncate(last);
                self.y.truncate(last);
                self.z.truncate(last);
                self.vx.truncate(last);
                self.vy.truncate(last);
                self.vz.truncate(last);
                self.ttl.truncate(last);
                self.age.truncate(last);
                self.size.truncate(last);
                self.r.truncate(last);
                self.g.truncate(last);
                self.b.truncate(last);
                continue;
            }
            self.vx[i] = self.vx[i] * f + gx * dt;
            self.vy[i] = self.vy[i] * f + gy * dt;
            self.vz[i] = self.vz[i] * f + gz * dt;
            self.x[i] += self.vx[i] * dt;
            self.y[i] += self.vy[i] * dt;
            self.z[i] += self.vz[i] * dt;
            i += 1;
        }
    }

    /// пакетная выгрузка: out ← [x y z size r g b life]×n, life = 1 − age/ttl
    pub fn dump(&self, out: &mut Vec<f32>) -> usize {
        out.clear();
        out.reserve(self.x.len() * 8);
        for i in 0..self.x.len() {
            let life = (1.0 - self.age[i] / self.ttl[i]).clamp(0.0, 1.0);
            out.extend_from_slice(&[
                self.x[i],
                self.y[i],
                self.z[i],
                self.size[i],
                self.r[i],
                self.g[i],
                self.b[i],
                life,
            ]);
        }
        self.x.len()
    }

    pub fn len(&self) -> usize {
        self.x.len()
    }

    #[allow(dead_code)] // тесты используют; ядро — нет
    pub fn is_empty(&self) -> bool {
        self.x.is_empty()
    }

    pub fn clear(&mut self) {
        self.x.clear();
        self.y.clear();
        self.z.clear();
        self.vx.clear();
        self.vy.clear();
        self.vz.clear();
        self.ttl.clear();
        self.age.clear();
        self.size.clear();
        self.r.clear();
        self.g.clear();
        self.b.clear();
    }
}

impl Default for Particles {
    fn default() -> Self {
        Self::new()
    }
}

/// ——— нативы Lisp: (part-*) — реестр в Vec<Particles>, id — индекс ———
pub fn register(env: crate::interp::Gc, store: Rc<RefCell<Vec<Particles>>>) {
    use crate::interp::{env_define, need_args as need, num_val, Gc, Value as V};

    fn def<F: Fn(&[V]) -> Result<V, String> + 'static>(env: Gc, name: &str, f: F) {
        env_define(env, name, V::Native(Rc::new(f), name.to_string()));
    }

    fn pool_of<'a>(
        store: &'a Rc<RefCell<Vec<Particles>>>,
        v: &V,
    ) -> Result<std::cell::RefMut<'a, Particles>, String> {
        let id = num_val(v)? as usize;
        let mut ss = store.borrow_mut();
        if id >= ss.len() {
            ss.push(Particles::new());
        }
        Ok(std::cell::RefMut::map(ss, |ss| &mut ss[id]))
    }

    fn f32_of(v: &V) -> Result<f32, String> {
        num_val(v).map(|x| x as f32)
    }

    let st = store.clone();
    def(env, "part-new", move |args| {
        need(args, 0)?;
        let mut ss = st.borrow_mut();
        ss.push(Particles::new());
        Ok(V::Int((ss.len() - 1) as i64))
    });

    // (part-emit! p x y z vx vy vz ttl size r g b) — одна частица
    let st = store.clone();
    def(env, "part-emit!", move |args| {
        need(args, 12)?;
        let mut vals = [0.0f32; 11];
        for (i, a) in args[1..].iter().enumerate() {
            vals[i] = f32_of(a)?;
        }
        pool_of(&st, &args[0])?.emit(
            vals[0], vals[1], vals[2], vals[3], vals[4], vals[5], vals[6], vals[7], vals[8],
            vals[9], vals[10],
        );
        Ok(V::Nil)
    });

    // (part-burst! p n x y z bvx bvy bvz speed spread ttl size r g b) — залп
    let st = store.clone();
    def(env, "part-burst!", move |args| {
        need(args, 15)?;
        let n = num_val(&args[1])? as u32;
        let mut vals = [0.0f32; 13];
        for (i, a) in args[2..].iter().enumerate() {
            vals[i] = f32_of(a)?;
        }
        pool_of(&st, &args[0])?.burst(
            n, vals[0], vals[1], vals[2], vals[3], vals[4], vals[5], vals[6], vals[7], vals[8],
            vals[9], vals[10], vals[11], vals[12],
        );
        Ok(V::Nil)
    });

    // (part-step! p dt gx gy gz drag)
    let st = store.clone();
    def(env, "part-step!", move |args| {
        need(args, 6)?;
        let dt = f32_of(&args[1])?;
        let mut vals = [0.0f32; 4];
        for (i, a) in args[2..].iter().enumerate() {
            vals[i] = f32_of(a)?;
        }
        pool_of(&st, &args[0])?.step(dt, vals[0], vals[1], vals[2], vals[3]);
        Ok(V::Nil)
    });

    // (part-dump! p out) → n; out ← [x y z size r g b life]×n
    let st = store.clone();
    def(env, "part-dump!", move |args| {
        need(args, 2)?;
        let out = match &args[1] {
            V::F32Vec(r) => r.clone(),
            _ => return Err("part-dump!: ожидался f32-вектор".into()),
        };
        let n = pool_of(&st, &args[0])?.dump(&mut out.borrow_mut());
        Ok(V::Int(n as i64))
    });

    let st = store.clone();
    def(env, "part-n", move |args| {
        need(args, 1)?;
        Ok(V::Int(pool_of(&st, &args[0])?.len() as i64))
    });

    let st = store.clone();
    def(env, "part-clear!", move |args| {
        need(args, 1)?;
        pool_of(&st, &args[0])?.clear();
        Ok(V::Nil)
    });

    // (part-seed! p seed) — воспроизводимость залпов сцены
    let st = store.clone();
    def(env, "part-seed!", move |args| {
        need(args, 2)?;
        let seed = num_val(&args[1])? as u32;
        pool_of(&st, &args[0])?.seed = seed;
        Ok(V::Nil)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_step_expire() {
        let mut p = Particles::new();
        p.emit(0.0, 5.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.1, 1.0, 0.5, 0.0);
        assert_eq!(p.len(), 1);
        p.step(0.3, 0.0, -10.0, 0.0, 0.0);
        assert_eq!(p.len(), 1);
        // semi-implicit Эйлер: v сначала, позиция новой скоростью
        assert!((p.y[0] - (5.0 - 3.0 * 0.3)).abs() < 1e-5, "y {}", p.y[0]);
        assert!((p.vy[0] + 3.0).abs() < 1e-5, "vy {}", p.vy[0]);
        p.step(0.3, 0.0, 0.0, 0.0, 0.0); // age 0.6 > ttl 0.5 — умерла
        assert!(p.is_empty());
    }

    #[test]
    fn drag_damps_velocity() {
        let mut p = Particles::new();
        p.emit(0.0, 0.0, 0.0, 10.0, 0.0, 0.0, 10.0, 0.1, 1.0, 1.0, 1.0);
        p.step(0.1, 0.0, 0.0, 0.0, 5.0); // f = 1 − 0.5
        assert!((p.vx[0] - 5.0).abs() < 1e-5, "drag: {}", p.vx[0]);
    }

    #[test]
    fn burst_deterministic_bitwise() {
        let mk = |seed: u32| {
            let mut p = Particles::new();
            p.seed = seed;
            p.burst(
                50, 1.0, 2.0, 3.0, 0.0, 1.0, 0.0, 4.0, 0.8, 0.7, 0.15, 1.0, 0.6, 0.2,
            );
            p
        };
        let (a, b) = (mk(42), mk(42));
        assert_eq!(a.len(), b.len());
        for i in 0..a.len() {
            assert_eq!(a.x[i].to_bits(), b.x[i].to_bits());
            assert_eq!(a.vz[i].to_bits(), b.vz[i].to_bits());
        }
        // другой seed — другой разброс (не совпадает побитово)
        let c = mk(43);
        // x у всех частиц — точка эмита; случайность живёт в скоростях
        let same =
            a.vz.iter()
                .zip(c.vz.iter())
                .all(|(u, v)| u.to_bits() == v.to_bits());
        assert!(!same, "разные seed дали одинаковый залп");
    }

    #[test]
    fn burst_speeds_bounded() {
        let mut p = Particles::new();
        p.burst(
            200, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 5.0, 0.5, 1.0, 0.1, 1.0, 1.0, 1.0,
        );
        assert_eq!(p.len(), 200);
        for i in 0..p.len() {
            let v2 = p.vx[i] * p.vx[i] + p.vy[i] * p.vy[i] + p.vz[i] * p.vz[i];
            assert!(v2.sqrt() <= 5.0 * 1.5 + 1e-4, "скорость |v|={}", v2.sqrt());
            assert!(p.ttl[i] > 0.0 && p.ttl[i] <= 1.0 + 1e-5);
        }
    }

    #[test]
    fn pool_overflow_overwrites_no_panic() {
        let mut p = Particles::new();
        p.burst(
            MAX_P as u32 + 100,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
            0.0,
            1.0,
            0.1,
            1.0,
            1.0,
            1.0,
        );
        assert_eq!(p.len(), MAX_P, "пул не растёт сверх MAX_P");
        p.step(0.016, 0.0, 0.0, 0.0, 0.0);
        assert_eq!(p.len(), MAX_P);
    }

    #[test]
    fn dump_flat_format_and_life() {
        let mut p = Particles::new();
        p.emit(1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 2.0, 0.25, 0.9, 0.5, 0.1);
        p.step(0.5, 0.0, 0.0, 0.0, 0.0); // age 0.5/2.0 → life 0.75
        let mut out = Vec::new();
        let n = p.dump(&mut out);
        assert_eq!(n, 1);
        assert_eq!(out.len(), 8);
        assert_eq!(out[0], 1.0);
        assert_eq!(out[3], 0.25);
        assert_eq!(out[4], 0.9);
        assert!((out[7] - 0.75).abs() < 1e-6, "life {}", out[7]);
    }

    #[test]
    fn swap_remove_keeps_pool_consistent() {
        let mut p = Particles::new();
        // 3 частицы с разным ttl: 0.1, 10, 0.1 → после шага средней 10 остаётся
        p.emit(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.1, 0.1, 1.0, 1.0, 1.0);
        p.emit(1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 10.0, 0.1, 1.0, 1.0, 1.0);
        p.emit(2.0, 2.0, 2.0, 0.0, 0.0, 0.0, 0.1, 0.1, 1.0, 1.0, 1.0);
        p.step(0.2, 0.0, 0.0, 0.0, 0.0);
        assert_eq!(p.len(), 1);
        assert_eq!(p.x[0], 1.0, "выжившая переехала swap-удалением в слот 0");
        let mut out = Vec::new();
        assert_eq!(p.dump(&mut out), 1);
    }
}
