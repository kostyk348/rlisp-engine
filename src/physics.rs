//! src/physics.rs — Этап 10 «Физика»: сферы + heightfield + плавучесть.
//! Zero-dep, f32, детерминизм: обход контактов строго по индексам (без хеш-
//! таблиц), фиксированные подшаги — те же входы дают побитово те же выходы.
//! HAL не знает про физику: солвер живёт здесь, Lisp — через нативы phys-*.

/// подшаг не длиннее 1/90 c (анти-туннель и стабильность покоя)
pub const MAX_SUB_DT: f32 = 1.0 / 90.0;
const MAX_SUBSTEPS: u32 = 6;
const SLOP: f32 = 0.005; // допустимое проникновение (Baumgarte)
const CORR: f32 = 0.8; // доля коррекции проникновения за подшаг
const MAX_CORR: f32 = 0.05; // потолок выталкивания за подшаг (иначе «телепорт»
                            // после глубокого закапывания даёт гигантские прыжки)
const BOUNCE_THRESH: f32 = 0.4; // |vn| ниже — отскок гасится (покой)
const VEL_MAX: f32 = 60.0; // страховка от разгона
const ITER: usize = 8; // NGS-итерации (накопительные импульсы: больше — точнее сходимость)
const ANGV_MAX: f32 = 60.0; // страховка от раскрутки, рад/с
const PAIR_COOLDOWN: f32 = 0.09; // повторный стук пары — не чаще

/// событие контакта (для звука в песочнице и ряби в сценах)
#[derive(Clone, Copy)]
pub struct Contact {
    pub a: u32,
    /// -1 — ландшафт, иначе id второй сферы
    pub b: i32,
    /// 0 — ландшафт, 1 — сфера-сфера
    pub kind: u8,
    /// скорость удара (>0), м/с
    pub speed: f32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Clone)]
pub struct Sphere {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub vx: f32,
    pub vy: f32,
    pub vz: f32,
    pub r: f32,
    pub inv_m: f32,
    /// коэффициент восстановления
    pub e: f32,
    /// трение Кулона
    pub mu: f32,
    /// накопленный поворот (угол + ось) — интегрируется из угловой скорости
    pub ang: f32,
    pub ax: f32,
    pub ay: f32,
    pub az: f32,
    /// угловая скорость, рад/с (сфера изотропна — скалярная инерция на все оси);
    /// контактная динамика спина — NGS-солвер (этап 12): трение в точке
    /// контакта связывает скольжение и вращение (честное качение)
    pub wx: f32,
    pub wy: f32,
    pub wz: f32,
    /// обратная moments инерции I = 2/5·m·r² → inv_I = 5/(2·m·r²)
    pub inv_i: f32,
}

impl Sphere {
    pub fn new(x: f32, y: f32, z: f32, r: f32, e: f32, mu: f32) -> Self {
        // плотность 1: масса шара 4/3 π r³
        let m = 4.1887902 * r * r * r;
        Sphere {
            x,
            y,
            z,
            vx: 0.0,
            vy: 0.0,
            vz: 0.0,
            r,
            inv_m: 1.0 / m,
            e,
            mu,
            ang: 0.0,
            ax: 0.0,
            ay: 1.0,
            az: 0.0,
            wx: 0.0,
            wy: 0.0,
            wz: 0.0,
            inv_i: 2.5 * (1.0 / m) / (r * r),
        }
    }
}

/// сетка высот: h[row*nx+col], row → z, col → x; вне сетки — край
#[derive(Clone)]
pub struct HeightField {
    pub x0: f32,
    pub z0: f32,
    pub dx: f32,
    pub dz: f32,
    pub nx: u32,
    pub nz: u32,
    pub h: Vec<f32>,
}

impl HeightField {
    pub fn height(&self, x: f32, z: f32) -> f32 {
        let fx = ((x - self.x0) / self.dx).clamp(0.0, (self.nx - 1) as f32);
        let fz = ((z - self.z0) / self.dz).clamp(0.0, (self.nz - 1) as f32);
        let c = fx.floor() as u32;
        let r = fz.floor() as u32;
        let c1 = (c + 1).min(self.nx - 1);
        let r1 = (r + 1).min(self.nz - 1);
        let tx = fx - c as f32;
        let tz = fz - r as f32;
        let h00 = self.h[(r * self.nx + c) as usize];
        let h10 = self.h[(r * self.nx + c1) as usize];
        let h01 = self.h[(r1 * self.nx + c) as usize];
        let h11 = self.h[(r1 * self.nx + c1) as usize];
        h00 * (1.0 - tx) * (1.0 - tz)
            + h10 * tx * (1.0 - tz)
            + h01 * (1.0 - tx) * tz
            + h11 * tx * tz
    }
    /// нормаль поверхности центральными разностями (шаг = полквадрата)
    pub fn normal(&self, x: f32, z: f32) -> (f32, f32, f32) {
        let e = self.dx * 0.5;
        let gx = (self.height(x + e, z) - self.height(x - e, z)) / (2.0 * e);
        let gz = (self.height(x, z + e) - self.height(x, z - e)) / (2.0 * e);
        let inv = 1.0 / (gx * gx + 1.0 + gz * gz).sqrt();
        (-gx * inv, inv, -gz * inv)
    }
}

/// контакт-кортеж солвера
type Contacts = Vec<(u32, i32, f32, f32, f32, f32, f32, f32, f32, f32)>;

/// скорость точки тела: v + ω×r (r — от центра до точки)
#[inline]
fn point_vel(s: &Sphere, rx: f32, ry: f32, rz: f32) -> (f32, f32, f32) {
    (
        s.vx + s.wy * rz - s.wz * ry,
        s.vy + s.wz * rx - s.wx * rz,
        s.vz + s.wx * ry - s.wy * rx,
    )
}

/// контакт NGS-солвера: геометрия, зафиксированная до итераций, + накопленные
/// импульсы (зажим дельт идёт от накопленного — кулоновский лимит не нарушается)
struct NgC {
    i: u32,
    /// -1 — ландшафт
    j: i32,
    nx: f32,
    ny: f32,
    nz: f32,
    /// касательный базис (t1 — по до-солверному скольжению)
    t1x: f32,
    t1y: f32,
    t1z: f32,
    t2x: f32,
    t2y: f32,
    t2z: f32,
    /// плечи от центра до ТОЧКИ контакта на сфере: для пары ±n·r (контакт на
    /// линии центров); для heightfield −n·r (нормаль сферы проходит через
    /// центр — вертикальная точка (x,hy,z) годится только для меры
    /// проникновения, сила же в сфере всегда вдоль радиуса). У нормального
    /// импульса плечо ∥ n → момента нет; трение берёт ω×r в истинной точке.
    rax: f32,
    ray: f32,
    raz: f32,
    rbx: f32,
    rby: f32,
    rbz: f32,
    /// обратные эффективные массы: нормаль inv_mΣ (плечо ∥ n — углового
    /// члена нет); касательные inv_mΣ + inv_I·|r×t|² = 3.5·inv_mΣ для сферы
    k_n: f32,
    k_t1: f32,
    k_t2: f32,
    /// цель по нормальной скорости: −e_eff·vn0, где vn0 — сближение ЦЕНТРОВ
    /// (у пары совпадает со скоростью точки контакта: плечо ∥ n)
    vn_target: f32,
    mu: f32,
    jn_acc: f32,
    jt1_acc: f32,
    jt2_acc: f32,
}

#[derive(Clone)]
pub struct World {
    pub spheres: Vec<Sphere>,
    pub hf: Option<HeightField>,
    pub gravity: f32,
    /// вода: уровень, подъёмная сила (м/с²), линейный.drag (1/с)
    pub water: bool,
    pub water_y: f32,
    pub buoy: f32,
    pub drag: f32,
    pub time: f32,
    /// события, накопленные с последнего drain
    pub events: Vec<Contact>,
    pair_cool: Vec<(u64, f32)>,
}

// буфер для JS-песочницы (звук): плоский формат
// [a, kind, speed, x, y, z] × n; чистится только wasm-фасадом
#[cfg(target_arch = "wasm32")]
use std::sync::Mutex;
#[cfg(target_arch = "wasm32")]
static JS_EVENTS: Mutex<Vec<f32>> = Mutex::new(Vec::new());

#[cfg(target_arch = "wasm32")]
fn push_js_event(c: &Contact) {
    if let Ok(mut q) = JS_EVENTS.lock() {
        if q.len() > 4096 {
            q.clear();
        }
        q.extend_from_slice(&[c.a as f32, c.kind as f32, c.speed, c.x, c.y, c.z]);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn push_js_event(_c: &Contact) {}

/// wasm-фасад: забрать накопленные события (a, kind, speed, x, y, z)×n
#[cfg(target_arch = "wasm32")]
pub fn drain_js_events(out: &mut Vec<f32>) {
    if let Ok(mut q) = JS_EVENTS.lock() {
        std::mem::swap(&mut *q, out);
        q.clear();
    }
}

impl World {
    pub fn new() -> Self {
        World {
            spheres: Vec::new(),
            hf: None,
            gravity: -19.6,
            water: false,
            water_y: 0.0,
            buoy: 0.0,
            drag: 0.0,
            time: 0.0,
            events: Vec::new(),
            pair_cool: Vec::new(),
        }
    }

    pub fn add(&mut self, s: Sphere) -> u32 {
        self.spheres.push(s);
        (self.spheres.len() - 1) as u32
    }

    /// шаг: подшаги по ≤1/90 c; возвращает число событий за шаг
    pub fn step(&mut self, dt: f32) -> usize {
        if dt <= 0.0 || dt > 0.25 {
            return 0;
        }
        let n = ((dt / MAX_SUB_DT).ceil() as u32).clamp(1, MAX_SUBSTEPS);
        let h = dt / n as f32;
        self.events.clear();
        for _ in 0..n {
            self.substep(h);
        }
        self.time += dt;
        self.events.len()
    }

    fn substep(&mut self, h: f32) {
        // 1) интегрирование скоростей: гравитация, плавучесть, drag
        for s in self.spheres.iter_mut() {
            s.vy += self.gravity * h;
            if self.water && s.y < self.water_y {
                s.vy += self.buoy * h;
                let f = (1.0 - self.drag * h).max(0.0);
                s.vx *= f;
                s.vy *= f;
                s.vz *= f;
                s.wx *= f;
                s.wy *= f;
                s.wz *= f;
            }
        }
        // 2) контакты (в порядке индексов — детерминизм)
        // (a, b, nx, ny, nz, pen, cx, cy, cz, vn0 — сближение до солвера)
        let mut cn: Contacts = Vec::new();
        if let Some(hf) = &self.hf {
            for (i, s) in self.spheres.iter().enumerate() {
                let hy = hf.height(s.x, s.z);
                let pen = s.r - (s.y - hy);
                if pen > 0.0 {
                    let (nx, ny, nz) = hf.normal(s.x, s.z);
                    let vn0 = s.vx * nx + s.vy * ny + s.vz * nz;
                    cn.push((i as u32, -1, nx, ny, nz, pen, s.x, hy, s.z, vn0));
                }
            }
        }
        for i in 0..self.spheres.len() {
            for j in (i + 1)..self.spheres.len() {
                let (a, b) = (&self.spheres[i], &self.spheres[j]);
                let dx = b.x - a.x;
                let dy = b.y - a.y;
                let dz = b.z - a.z;
                let d2 = dx * dx + dy * dy + dz * dz;
                let rs = a.r + b.r;
                if d2 < rs * rs && d2 > 1e-12 {
                    let d = d2.sqrt();
                    let (nx, ny, nz) = (dx / d, dy / d, dz / d);
                    let pen = rs - d;
                    let cx = a.x + nx * a.r;
                    let cy = a.y + ny * a.r;
                    let cz = a.z + nz * a.r;
                    let vn0 = (b.vx - a.vx) * nx + (b.vy - a.vy) * ny + (b.vz - a.vz) * nz;
                    cn.push((i as u32, j as i32, nx, ny, nz, pen, cx, cy, cz, vn0));
                }
            }
        }
        // 3) NGS-солвер (этап 12): последовательные импульсы с НАКОПЛЕНИЕМ.
        //    Нормаль — чистое ограничение скорости (цель vn = −e·vn0, где vn0 —
        //    до-солверная скорость): проникновение правит только позиционная
        //    фаза (шаг 5) — velocity-bias, накачивавший энергию, отсутствует
        //    (split impulse). Трение — в точке контакта (v + ω×r): дельта
        //    зажата кулоновским лимитом |jt| ≤ μ·jn_нак, поэтому трение строго
        //    диссипативно; спин связан со скольжением — шар переходит на
        //    качение честно. Для сферы плечо r ∥ n: нормальный импульс спин
        //    не меняет; k_t = inv_mΣ + inv_I·|r×t|² по обе стороны контакта.
        let mut sol: Vec<NgC> = Vec::with_capacity(cn.len());
        for c in &cn {
            let (i, j, nx, ny, nz, .., vn0) = *c;
            let a = &self.spheres[i as usize];
            // плечи вдоль нормали (см. NgC): контакт сферы — всегда на линии
            // «центр − r·n»; вертикальная точка (cx,cy,cz) — только мера
            // проникновения и позиция события
            let sgn = if j < 0 { -1.0 } else { 1.0 };
            let (rax, ray, raz) = (sgn * nx * a.r, sgn * ny * a.r, sgn * nz * a.r);
            let (mut rbx, mut rby, mut rbz) = (0.0f32, 0.0f32, 0.0f32);
            let mut rel = point_vel(a, rax, ray, raz);
            let (inv_sum, inv_i_b) = if j >= 0 {
                let b = &self.spheres[j as usize];
                rbx = -nx * b.r;
                rby = -ny * b.r;
                rbz = -nz * b.r;
                let pb = point_vel(b, rbx, rby, rbz);
                rel = (pb.0 - rel.0, pb.1 - rel.1, pb.2 - rel.2);
                (a.inv_m + b.inv_m, b.inv_i)
            } else {
                (a.inv_m, 0.0)
            };
            // t1 — по до-солверному скольжению, иначе стабильный fallback ⊥ n
            let vnn = rel.0 * nx + rel.1 * ny + rel.2 * nz;
            let (tx, ty, tz) = (rel.0 - nx * vnn, rel.1 - ny * vnn, rel.2 - nz * vnn);
            let tl = (tx * tx + ty * ty + tz * tz).sqrt();
            let (t1x, t1y, t1z) = if tl > 1e-6 {
                (tx / tl, ty / tl, tz / tl)
            } else if nx.abs() < 0.7 {
                let inv = (ny * ny + nz * nz).sqrt();
                (0.0, -nz / inv, ny / inv)
            } else {
                let inv = (nz * nz + nx * nx).sqrt();
                (-nz / inv, 0.0, nx / inv)
            };
            let (t2x, t2y, t2z) = (
                ny * t1z - nz * t1y,
                nz * t1x - nx * t1z,
                nx * t1y - ny * t1x,
            );
            let sq = |ax: f32, ay: f32, az: f32| ax * ax + ay * ay + az * az;
            let k_n = inv_sum;
            let k_t1 = inv_sum
                + a.inv_i
                    * sq(
                        ray * t1z - raz * t1y,
                        raz * t1x - rax * t1z,
                        rax * t1y - ray * t1x,
                    )
                + inv_i_b
                    * sq(
                        rby * t1z - rbz * t1y,
                        rbz * t1x - rbx * t1z,
                        rbx * t1y - rby * t1x,
                    );
            let k_t2 = inv_sum
                + a.inv_i
                    * sq(
                        ray * t2z - raz * t2y,
                        raz * t2x - rax * t2z,
                        rax * t2y - ray * t2x,
                    )
                + inv_i_b
                    * sq(
                        rby * t2z - rbz * t2y,
                        rbz * t2x - rbx * t2z,
                        rbx * t2y - rby * t2x,
                    );
            let (e, mu) = if j < 0 {
                (a.e, a.mu)
            } else {
                let b = &self.spheres[j as usize];
                (a.e.max(b.e) * 0.5 + a.e.min(b.e) * 0.5, a.mu.max(b.mu))
            };
            let e_eff = if -vn0 < BOUNCE_THRESH { 0.0 } else { e };
            sol.push(NgC {
                i,
                j,
                nx,
                ny,
                nz,
                t1x,
                t1y,
                t1z,
                t2x,
                t2y,
                t2z,
                rax,
                ray,
                raz,
                rbx,
                rby,
                rbz,
                k_n,
                k_t1,
                k_t2,
                vn_target: -e_eff * vn0,
                mu,
                jn_acc: 0.0,
                jt1_acc: 0.0,
                jt2_acc: 0.0,
            });
        }
        for _ in 0..ITER {
            for s in sol.iter_mut() {
                // скорость в точке контакта на текущей итерации
                let a = &self.spheres[s.i as usize];
                let pv = point_vel(a, s.rax, s.ray, s.raz);
                let rel = if s.j < 0 {
                    pv
                } else {
                    let b = &self.spheres[s.j as usize];
                    let pb = point_vel(b, s.rbx, s.rby, s.rbz);
                    (pb.0 - pv.0, pb.1 - pv.1, pb.2 - pv.2)
                };
                // — нормаль: накопительный импульс, зажим ≥ 0 (без липкости)
                let vn = rel.0 * s.nx + rel.1 * s.ny + rel.2 * s.nz;
                let jn_new = (s.jn_acc + (s.vn_target - vn) / s.k_n).max(0.0);
                let djn = jn_new - s.jn_acc;
                if djn != 0.0 {
                    s.jn_acc = jn_new;
                    let (px, py, pz) = (s.nx * djn, s.ny * djn, s.nz * djn);
                    match s.j {
                        -1 => {
                            let a = &mut self.spheres[s.i as usize];
                            a.vx += px * a.inv_m;
                            a.vy += py * a.inv_m;
                            a.vz += pz * a.inv_m;
                        }
                        jb => {
                            let (l, r) = self.spheres.split_at_mut(jb as usize);
                            let (a, b) = (&mut l[s.i as usize], &mut r[0]);
                            b.vx += px * b.inv_m;
                            b.vy += py * b.inv_m;
                            b.vz += pz * b.inv_m;
                            a.vx -= px * a.inv_m;
                            a.vy -= py * a.inv_m;
                            a.vz -= pz * a.inv_m;
                        }
                    }
                }
                // нормальный импульс ⊥ касательным и без крутящего момента —
                // vt1/vt2 не меняет: трение можно решать по тому же rel
                let lim = s.mu * s.jn_acc;
                if lim > 0.0 {
                    let vt1 = rel.0 * s.t1x + rel.1 * s.t1y + rel.2 * s.t1z;
                    let jt1_new = (s.jt1_acc - vt1 / s.k_t1).clamp(-lim, lim);
                    let djt = jt1_new - s.jt1_acc;
                    if djt != 0.0 {
                        s.jt1_acc = jt1_new;
                        let (px, py, pz) = (s.t1x * djt, s.t1y * djt, s.t1z * djt);
                        match s.j {
                            -1 => {
                                let a = &mut self.spheres[s.i as usize];
                                a.vx += px * a.inv_m;
                                a.vy += py * a.inv_m;
                                a.vz += pz * a.inv_m;
                                a.wx += a.inv_i * (s.ray * pz - s.raz * py);
                                a.wy += a.inv_i * (s.raz * px - s.rax * pz);
                                a.wz += a.inv_i * (s.rax * py - s.ray * px);
                            }
                            jb => {
                                let (l, r) = self.spheres.split_at_mut(jb as usize);
                                let (a, b) = (&mut l[s.i as usize], &mut r[0]);
                                b.vx += px * b.inv_m;
                                b.vy += py * b.inv_m;
                                b.vz += pz * b.inv_m;
                                b.wx += b.inv_i * (s.rby * pz - s.rbz * py);
                                b.wy += b.inv_i * (s.rbz * px - s.rbx * pz);
                                b.wz += b.inv_i * (s.rbx * py - s.rby * px);
                                a.vx -= px * a.inv_m;
                                a.vy -= py * a.inv_m;
                                a.vz -= pz * a.inv_m;
                                a.wx -= a.inv_i * (s.ray * pz - s.raz * py);
                                a.wy -= a.inv_i * (s.raz * px - s.rax * pz);
                                a.wz -= a.inv_i * (s.rax * py - s.ray * px);
                            }
                        }
                    }
                    let vt2 = rel.0 * s.t2x + rel.1 * s.t2y + rel.2 * s.t2z;
                    let jt2_new = (s.jt2_acc - vt2 / s.k_t2).clamp(-lim, lim);
                    let djt = jt2_new - s.jt2_acc;
                    if djt != 0.0 {
                        s.jt2_acc = jt2_new;
                        let (px, py, pz) = (s.t2x * djt, s.t2y * djt, s.t2z * djt);
                        match s.j {
                            -1 => {
                                let a = &mut self.spheres[s.i as usize];
                                a.vx += px * a.inv_m;
                                a.vy += py * a.inv_m;
                                a.vz += pz * a.inv_m;
                                a.wx += a.inv_i * (s.ray * pz - s.raz * py);
                                a.wy += a.inv_i * (s.raz * px - s.rax * pz);
                                a.wz += a.inv_i * (s.rax * py - s.ray * px);
                            }
                            jb => {
                                let (l, r) = self.spheres.split_at_mut(jb as usize);
                                let (a, b) = (&mut l[s.i as usize], &mut r[0]);
                                b.vx += px * b.inv_m;
                                b.vy += py * b.inv_m;
                                b.vz += pz * b.inv_m;
                                b.wx += b.inv_i * (s.rby * pz - s.rbz * py);
                                b.wy += b.inv_i * (s.rbz * px - s.rbx * pz);
                                b.wz += b.inv_i * (s.rbx * py - s.rby * px);
                                a.vx -= px * a.inv_m;
                                a.vy -= py * a.inv_m;
                                a.vz -= pz * a.inv_m;
                                a.wx -= a.inv_i * (s.ray * pz - s.raz * py);
                                a.wy -= a.inv_i * (s.raz * px - s.rax * pz);
                                a.wz -= a.inv_i * (s.rax * py - s.ray * px);
                            }
                        }
                    }
                }
            }
        }
        // 4) интегрирование позиций + крен качением
        for s in self.spheres.iter_mut() {
            s.x += s.vx * h;
            s.y += s.vy * h;
            s.z += s.vz * h;
            // угловая скорость: страховка + накопление поворота из ω; на земле
            // ω ведёт трение качения (NGS, шаг 3) — крен согласован со скольжением
            let w2 = s.wx * s.wx + s.wy * s.wy + s.wz * s.wz;
            if w2 > ANGV_MAX * ANGV_MAX {
                let k = ANGV_MAX / w2.sqrt();
                s.wx *= k;
                s.wy *= k;
                s.wz *= k;
            }
            let wh = w2.sqrt();
            if wh > 1e-4 {
                s.ang += wh * h;
                s.ax = s.wx / wh;
                s.ay = s.wy / wh;
                s.az = s.wz / wh;
            } else {
                let vh = (s.vx * s.vx + s.vz * s.vz).sqrt();
                if vh > 1e-4 {
                    s.ang += vh * h / s.r;
                    s.ax = -s.vz / vh;
                    s.az = s.vx / vh;
                    s.ay = 0.0;
                }
            }
            // страховка
            let v2 = s.vx * s.vx + s.vy * s.vy + s.vz * s.vz;
            if v2 > VEL_MAX * VEL_MAX {
                let k = VEL_MAX / v2.sqrt();
                s.vx *= k;
                s.vy *= k;
                s.vz *= k;
            }
            if !(s.x.is_finite() && s.y.is_finite() && s.z.is_finite()) {
                s.vx = 0.0;
                s.vy = 0.0;
                s.vz = 0.0;
            }
            if !(s.wx.is_finite() && s.wy.is_finite() && s.wz.is_finite()) {
                s.wx = 0.0;
                s.wy = 0.0;
                s.wz = 0.0;
            }
        }
        // 5) коррекция проникновений (позиционная, статика — вся доля шару)
        for c in &cn {
            let (_, j, nx, ny, nz, pen, _, _, _, _) = *c;
            if pen <= SLOP {
                continue;
            }
            let push = ((pen - SLOP) * CORR).min(MAX_CORR);
            match j {
                -1 => {
                    let s = &mut self.spheres[c.0 as usize];
                    s.x += nx * push;
                    s.y += ny * push;
                    s.z += nz * push;
                }
                _ => {
                    let (i, jb) = (c.0 as usize, j as usize);
                    let wa =
                        self.spheres[i].inv_m / (self.spheres[i].inv_m + self.spheres[jb].inv_m);
                    let a = &mut self.spheres[i];
                    a.x -= nx * push * wa;
                    a.y -= ny * push * wa;
                    a.z -= nz * push * wa;
                    let b = &mut self.spheres[jb];
                    b.x += nx * push * (1.0 - wa);
                    b.y += ny * push * (1.0 - wa);
                    b.z += nz * push * (1.0 - wa);
                }
            }
        }
        // 6) события ударов: порог по ДО-солверному vn0 (после импульсов vn≈0)
        for c in &cn {
            let (i, j, _, _, _, _, cx, cy, cz, vn0) = *c;
            let (kind, b) = if j < 0 { (0u8, -1i32) } else { (1u8, j) };
            if vn0 < -BOUNCE_THRESH {
                let key = ((i as u64) << 20) | ((b.max(0) as u64) & 0xFFFFF);
                let last = self
                    .pair_cool
                    .iter()
                    .find(|p| p.0 == key)
                    .map(|p| p.1)
                    .unwrap_or(-10.0);
                if self.time - last > PAIR_COOLDOWN {
                    if let Some(p) = self.pair_cool.iter_mut().find(|p| p.0 == key) {
                        p.1 = self.time;
                    } else {
                        self.pair_cool.push((key, self.time));
                    }
                    let ev = Contact {
                        a: i,
                        b,
                        kind,
                        speed: -vn0,
                        x: cx,
                        y: cy,
                        z: cz,
                    };
                    push_js_event(&ev);
                    self.events.push(ev);
                }
            }
        }
        if self.pair_cool.len() > 256 {
            self.pair_cool.drain(..128);
        }
    }
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

/// ——— нативы Lisp: (phys-*) — мир в Vec<World>, id — индекс ———
pub fn register(env: crate::interp::Gc, worlds: std::rc::Rc<std::cell::RefCell<Vec<World>>>) {
    use crate::interp::{need_args as need, num_val, Gc, Value as V};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn def<F: Fn(&[V]) -> Result<V, String> + 'static>(env: Gc, name: &str, f: F) {
        crate::interp::env_define(env, name, V::Native(Rc::new(f), name.to_string()));
    }

    fn world_of<'a>(
        worlds: &'a Rc<RefCell<Vec<World>>>,
        v: &V,
    ) -> Result<std::cell::RefMut<'a, World>, String> {
        let id = num_val(v)? as usize;
        let ws = worlds.borrow_mut();
        if id >= ws.len() {
            return Err(format!("phys: нет мира {}", id));
        }
        // порядок borrow: worlds.borrow_mut() держим живым через RefMut
        Ok(std::cell::RefMut::map(ws, |ws| &mut ws[id]))
    }

    fn vec_of(v: &V) -> Result<Rc<RefCell<Vec<f32>>>, String> {
        match v {
            V::F32Vec(r) => Ok(r.clone()),
            _ => Err("phys: ожидался f32-вектор".into()),
        }
    }

    let wc = worlds.clone();
    def(env, "phys-new", move |args| {
        need(args, 0)?;
        let mut ws = wc.borrow_mut();
        ws.push(World::new());
        Ok(V::Int((ws.len() - 1) as i64))
    });

    let wc = worlds.clone();
    def(env, "phys-gravity!", move |args| {
        need(args, 2)?;
        let g = num_val(&args[1])? as f32;
        world_of(&wc, &args[0])?.gravity = g;
        Ok(V::Nil)
    });

    let wc = worlds.clone();
    def(env, "phys-water!", move |args| {
        need(args, 4)?;
        let mut w = world_of(&wc, &args[0])?;
        w.water = true;
        w.water_y = num_val(&args[1])? as f32;
        w.buoy = num_val(&args[2])? as f32;
        w.drag = num_val(&args[3])? as f32;
        Ok(V::Nil)
    });

    let wc = worlds.clone();
    def(env, "phys-heightfield!", move |args| {
        need(args, 8)?;
        let x0 = num_val(&args[1])? as f32;
        let z0 = num_val(&args[2])? as f32;
        let dx = num_val(&args[3])? as f32;
        let dz = num_val(&args[4])? as f32;
        let nx = num_val(&args[5])? as u32;
        let nz = num_val(&args[6])? as u32;
        let h = vec_of(&args[7])?;
        let hb = h.borrow();
        if hb.len() < (nx * nz) as usize {
            return Err(format!(
                "phys-heightfield: вектора {} < сетки {}×{}",
                hb.len(),
                nx,
                nz
            ));
        }
        let mut w = world_of(&wc, &args[0])?;
        w.hf = Some(HeightField {
            x0,
            z0,
            dx,
            dz,
            nx,
            nz,
            h: hb[..(nx * nz) as usize].to_vec(),
        });
        Ok(V::Nil)
    });

    let wc = worlds.clone();
    def(env, "phys-add!", move |args| {
        need(args, 7)?;
        let (x, y, z) = (
            num_val(&args[1])? as f32,
            num_val(&args[2])? as f32,
            num_val(&args[3])? as f32,
        );
        let (r, e, mu) = (
            num_val(&args[4])? as f32,
            num_val(&args[5])? as f32,
            num_val(&args[6])? as f32,
        );
        if r <= 0.0 {
            return Err("phys-add!: радиус > 0".into());
        }
        let id = world_of(&wc, &args[0])?.add(Sphere::new(x, y, z, r, e, mu));
        Ok(V::Int(id as i64))
    });

    let wc = worlds.clone();
    def(env, "phys-vel!", move |args| {
        need(args, 5)?;
        let id = num_val(&args[1])? as usize;
        let mut w = world_of(&wc, &args[0])?;
        if id >= w.spheres.len() {
            return Err(format!("phys-vel!: нет тела {}", id));
        }
        let s = &mut w.spheres[id];
        s.vx = num_val(&args[2])? as f32;
        s.vy = num_val(&args[3])? as f32;
        s.vz = num_val(&args[4])? as f32;
        Ok(V::Nil)
    });

    let wc = worlds.clone();
    def(env, "phys-n", move |args| {
        need(args, 1)?;
        Ok(V::Int(world_of(&wc, &args[0])?.spheres.len() as i64))
    });

    let wc = worlds.clone();
    def(env, "phys-clear!", move |args| {
        need(args, 1)?;
        world_of(&wc, &args[0])?.spheres.clear();
        Ok(V::Nil)
    });

    let wc = worlds.clone();
    def(env, "phys-step!", move |args| {
        need(args, 2)?;
        let dt = num_val(&args[1])? as f32;
        let n = world_of(&wc, &args[0])?.step(dt);
        Ok(V::Int(n as i64))
    });

    let wc = worlds.clone();
    def(env, "phys-events!", move |args| {
        need(args, 1)?;
        let mut w = world_of(&wc, &args[0])?;
        let evs: Vec<V> = w
            .events
            .drain(..)
            .map(|c| {
                V::List(vec![
                    V::Int(c.a as i64),
                    V::Int(c.b as i64),
                    V::Int(c.kind as i64),
                    V::Float(c.speed as f64),
                    V::Float(c.x as f64),
                    V::Float(c.y as f64),
                    V::Float(c.z as f64),
                ])
            })
            .collect();
        Ok(V::List(evs))
    });

    // пакетная выгрузка: out ← [x y z r]×n, возвращает n
    let wc = worlds.clone();
    def(env, "phys-positions!", move |args| {
        need(args, 2)?;
        let out = vec_of(&args[1])?;
        let w = world_of(&wc, &args[0])?;
        let n = w.spheres.len();
        {
            let mut ob = out.borrow_mut();
            ob.clear();
            ob.reserve(n * 4);
            for s in &w.spheres {
                ob.extend_from_slice(&[s.x, s.y, s.z, s.r]);
            }
        }
        Ok(V::Int(n as i64))
    });

    // пакетная выгрузка угловой скорости: out ← [wx wy wz]×n
    let wc = worlds.clone();
    def(env, "phys-angvels!", move |args| {
        need(args, 2)?;
        let out = vec_of(&args[1])?;
        let w = world_of(&wc, &args[0])?;
        let n = w.spheres.len();
        {
            let mut ob = out.borrow_mut();
            ob.clear();
            ob.reserve(n * 3);
            for s in &w.spheres {
                ob.extend_from_slice(&[s.wx, s.wy, s.wz]);
            }
        }
        Ok(V::Int(n as i64))
    });

    // ручная раскрутка: (phys-angvel! w id wx wy wz) — рад/с
    let wc = worlds.clone();
    def(env, "phys-angvel!", move |args| {
        need(args, 5)?;
        let mut w = world_of(&wc, &args[0])?;
        let id = num_val(&args[1])? as usize;
        let (wx, wy, wz) = (
            num_val(&args[2])? as f32,
            num_val(&args[3])? as f32,
            num_val(&args[4])? as f32,
        );
        let s = w
            .spheres
            .get_mut(id)
            .ok_or_else(|| format!("phys-angvel!: нет тела {}", id))?;
        s.wx = wx;
        s.wy = wy;
        s.wz = wz;
        Ok(V::Nil)
    });

    // пакетная выгрузка крена: out ← [ang ax ay az]×n (визуальное вращение)
    let wc = worlds.clone();
    def(env, "phys-spin!", move |args| {
        need(args, 2)?;
        let out = vec_of(&args[1])?;
        let w = world_of(&wc, &args[0])?;
        let n = w.spheres.len();
        {
            let mut ob = out.borrow_mut();
            ob.clear();
            ob.reserve(n * 4);
            for s in &w.spheres {
                ob.extend_from_slice(&[s.ang, s.ax, s.ay, s.az]);
            }
        }
        Ok(V::Int(n as i64))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_hf() -> HeightField {
        HeightField {
            x0: -10.0,
            z0: -10.0,
            dx: 1.0,
            dz: 1.0,
            nx: 21,
            nz: 21,
            h: vec![0.0; 21 * 21],
        }
    }

    fn sim(w: &mut World, seconds: f32, dt: f32) {
        let n = (seconds / dt).round() as u32;
        for _ in 0..n {
            w.step(dt);
        }
    }

    #[test]
    fn drop_bounce_restitution() {
        let mut w = World::new();
        w.hf = Some(flat_hf());
        w.add(Sphere::new(0.0, 5.0, 0.0, 0.5, 0.75, 0.4));
        // апекс первого отскока: 0.5 + 0.75²·4.5 ≈ 3.03 (меряем после 1-го удара)
        let mut bounced = false;
        let mut apex = 0.0f32;
        for _ in 0..120 {
            w.step(1.0 / 60.0);
            let s = &w.spheres[0];
            if !bounced {
                if s.vy > 2.0 {
                    bounced = true;
                }
            } else {
                apex = apex.max(s.y);
            }
        }
        assert!(bounced, "шар должен коснуться земли за 2 c");
        assert!((apex - 3.03).abs() < 0.35, "apex {}", apex);
        sim(&mut w, 6.0, 1.0 / 60.0);
        let s = &w.spheres[0];
        assert!((s.y - 0.5).abs() < 0.04, "rest y {}", s.y);
        assert!(s.vy.abs() < 0.35, "rest vy {}", s.vy); // джиттер покоя ~ g·h
    }

    #[test]
    fn energy_nonincreasing() {
        let mut w = World::new();
        w.hf = Some(flat_hf());
        w.add(Sphere::new(0.3, 5.0, 0.0, 0.5, 0.6, 0.35));
        let e0 = 19.6 * 5.0;
        for _ in 0..240 {
            w.step(1.0 / 60.0);
            let s = &w.spheres[0];
            let e = 0.5 * (s.vx * s.vx + s.vy * s.vy + s.vz * s.vz) + 19.6 * s.y;
            assert!(e <= e0 + 0.6, "энергия выросла: {} > {}", e, e0);
        }
    }

    #[test]
    fn exchange_elastic_headon() {
        let mut w = World::new();
        w.gravity = 0.0;
        let mut a = Sphere::new(0.0, 0.0, 0.0, 0.5, 1.0, 0.0);
        a.vx = 2.0;
        let mut b = Sphere::new(3.0, 0.0, 0.0, 0.5, 1.0, 0.0);
        b.vx = -2.0;
        w.add(a);
        w.add(b);
        sim(&mut w, 1.0, 1.0 / 60.0);
        assert!(
            (w.spheres[0].vx - (-2.0)).abs() < 1e-3,
            "va {}",
            w.spheres[0].vx
        );
        assert!(
            (w.spheres[1].vx - 2.0).abs() < 1e-3,
            "vb {}",
            w.spheres[1].vx
        );
    }

    #[test]
    fn stick_inelastic_headon() {
        let mut w = World::new();
        w.gravity = 0.0;
        let mut a = Sphere::new(0.0, 0.0, 0.0, 0.5, 0.0, 0.0);
        a.vx = 2.0;
        let mut b = Sphere::new(3.0, 0.0, 0.0, 0.5, 0.0, 0.0);
        b.vx = -2.0;
        w.add(a);
        w.add(b);
        sim(&mut w, 1.0, 1.0 / 60.0);
        assert!(w.spheres[0].vx.abs() < 1e-3, "va {}", w.spheres[0].vx);
        assert!(w.spheres[1].vx.abs() < 1e-3, "vb {}", w.spheres[1].vx);
    }

    #[test]
    fn slope_roll_downhill() {
        let mut w = World::new();
        let mut hf = HeightField {
            x0: -10.0,
            z0: -10.0,
            dx: 1.0,
            dz: 1.0,
            nx: 21,
            nz: 21,
            h: vec![0.0; 21 * 21],
        };
        // наклон 30°: высота убывает вдоль +x
        for r in 0..21 {
            for c in 0..21 {
                let x = hf.x0 + c as f32 * hf.dx;
                hf.h[r * 21 + c] = -0.5774 * x;
            }
        }
        w.hf = Some(hf);
        let mut s = Sphere::new(0.0, 0.0, 0.0, 0.4, 0.15, 0.12);
        s.y = 0.5774 * 0.0 + 1.0;
        w.add(s);
        sim(&mut w, 1.5, 1.0 / 60.0);
        let s = &w.spheres[0];
        assert!(s.vx > 1.2, "разгон по склону vx {}", s.vx);
        let h = w.hf.as_ref().unwrap().height(s.x, s.z);
        assert!(
            s.y - h > 0.3 && s.y - h < 0.55,
            "на поверхности dy {}",
            s.y - h
        );
    }

    #[test]
    fn buoyancy_floats() {
        let mut w = World::new();
        w.hf = Some(flat_hf());
        w.water = true;
        w.water_y = 0.0;
        w.buoy = 25.0;
        w.drag = 1.6;
        w.gravity = -19.6;
        w.add(Sphere::new(0.0, 3.0, 0.0, 0.5, 0.1, 0.0));
        sim(&mut w, 5.0, 1.0 / 60.0);
        let s = &w.spheres[0];
        assert!(s.y.abs() < 0.6, "плавает y {}", s.y); // buoy>g → сидит высоко
        assert!(s.vy.abs() < 0.15, "скорость мала vy {}", s.vy);
    }

    #[test]
    fn stack_stable_no_nan() {
        let mut w = World::new();
        w.hf = Some(flat_hf());
        let r = 0.5;
        let mut id = 0;
        for (row, n) in [(0usize, 3usize), (1, 2), (2, 1)] {
            for k in 0..n {
                let x = (k as f32 - (n as f32 - 1.0) / 2.0) * (2.0 * r + 0.02);
                let mut s = Sphere::new(
                    x,
                    1.5 + row as f32 * (2.0 * r * 0.87) + 0.05,
                    0.0,
                    r,
                    0.2,
                    0.5,
                );
                s.vx = if id % 2 == 0 { 0.05 } else { -0.05 };
                w.add(s);
                id += 1;
            }
        }
        sim(&mut w, 3.0, 1.0 / 60.0);
        for (i, s) in w.spheres.iter().enumerate() {
            assert!(s.x.is_finite() && s.y.is_finite(), "NaN у тела {}", i);
            assert!(s.y > 0.1, "тело {} под землёй y {}", i, s.y);
            assert!(s.vy.abs() < 1.0, "тело {} vy {}", i, s.vy);
        }
    }

    #[test]
    fn determinism_bitwise() {
        let mk = || {
            let mut w = World::new();
            let mut hf = flat_hf();
            for i in 0..hf.h.len() {
                let x = hf.x0 + (i % 21) as f32;
                let z = hf.z0 + (i / 21) as f32;
                hf.h[i] = 0.4 * (x * 0.3).sin() + 0.3 * (z * 0.5).cos();
            }
            w.hf = Some(hf);
            for k in 0..8u32 {
                let mut s = Sphere::new(
                    (k as f32) * 0.7 - 2.5,
                    2.0 + k as f32 * 0.5,
                    (k as f32) * 0.9 - 3.0,
                    0.35 + 0.05 * k as f32,
                    0.45,
                    0.3,
                );
                s.vx = 0.7 - 0.1 * k as f32;
                s.vz = 0.3 + 0.05 * k as f32;
                w.add(s);
            }
            w
        };
        let (mut wa, mut wb) = (mk(), mk());
        for _ in 0..240 {
            wa.step(1.0 / 60.0);
            wb.step(1.0 / 60.0);
        }
        for (a, b) in wa.spheres.iter().zip(wb.spheres.iter()) {
            assert_eq!(a.x.to_bits(), b.x.to_bits());
            assert_eq!(a.y.to_bits(), b.y.to_bits());
            assert_eq!(a.z.to_bits(), b.z.to_bits());
        }
    }

    #[test]
    fn impact_events_recorded() {
        let mut w = World::new();
        w.hf = Some(flat_hf());
        w.add(Sphere::new(0.0, 3.0, 0.0, 0.5, 0.4, 0.4));
        let mut hits = 0;
        for _ in 0..60 {
            w.step(1.0 / 60.0);
            hits += w.events.len();
        }
        assert!(hits >= 1, "удар должен давать событие");
        assert!(w.events.iter().all(|e| e.kind == 0 && e.b == -1));
    }

    #[test]
    fn spin_conserved_in_flight() {
        let mut w = World::new();
        w.hf = Some(flat_hf());
        w.add(Sphere::new(0.0, 20.0, 0.0, 0.5, 0.5, 0.3));
        w.spheres[0].wx = 10.0;
        w.spheres[0].wy = -7.0;
        w.spheres[0].wz = 3.0;
        // 0.5 c свободного падения с 20 м — контактов нет
        sim(&mut w, 0.5, 1.0 / 60.0);
        let s = &w.spheres[0];
        assert_eq!(s.wx.to_bits(), 10.0f32.to_bits());
        assert_eq!(s.wy.to_bits(), (-7.0f32).to_bits());
        assert_eq!(s.wz.to_bits(), 3.0f32.to_bits());
        // поворот накопился: |ω|·t = sqrt(158)·0.5 ≈ 6.28 рад
        assert!(
            (s.ang - 158.0f32.sqrt() * 0.5).abs() < 1e-4,
            "ang {}",
            s.ang
        );
    }

    #[test]
    fn spin_damped_in_water() {
        let mut w = World::new();
        w.hf = Some(flat_hf());
        w.water = true;
        w.water_y = 5.0;
        w.buoy = 30.0;
        w.drag = 1.5;
        w.add(Sphere::new(0.0, 2.0, 0.0, 0.5, 0.5, 0.3));
        w.spheres[0].wz = 20.0;
        sim(&mut w, 1.0, 1.0 / 60.0);
        let s = &w.spheres[0];
        assert!(s.wz < 20.0 && s.wz.abs() > 0.5, "drag ω: {}", s.wz);
        assert!(s.y > w.water_y - s.r, "не всплыл: {}", s.y);
    }

    #[test]
    fn bitwise_deterministic_with_spin() {
        let mk = || {
            let mut w = World::new();
            w.hf = Some(flat_hf());
            w.add(Sphere::new(0.0, 4.0, 0.0, 0.45, 0.65, 0.45));
            w.add(Sphere::new(0.6, 5.5, 0.2, 0.45, 0.65, 0.45));
            w.spheres[0].wz = 12.0;
            w.spheres[1].wx = -9.0;
            w
        };
        let (mut a, mut b) = (mk(), mk());
        for _ in 0..240 {
            a.step(1.0 / 60.0);
            b.step(1.0 / 60.0);
        }
        for (pa, pb) in a.spheres.iter().zip(b.spheres.iter()) {
            assert_eq!(pa.x.to_bits(), pb.x.to_bits());
            assert_eq!(pa.y.to_bits(), pb.y.to_bits());
            assert_eq!(pa.wx.to_bits(), pb.wx.to_bits());
            assert_eq!(pa.wz.to_bits(), pb.wz.to_bits());
        }
    }

    #[test]
    fn angvel_clamped() {
        let mut w = World::new();
        w.hf = Some(flat_hf());
        w.add(Sphere::new(0.0, 0.5, 0.0, 0.5, 0.0, 0.5));
        w.spheres[0].wx = 1000.0;
        w.step(1.0 / 60.0);
        let s = &w.spheres[0];
        assert!(s.wx.abs() <= ANGV_MAX + 1.0, "ω не зажата: {}", s.wx);
    }

    // ——— этап 12: NGS-солвер, честное качение ———

    fn slope_30() -> HeightField {
        let mut hf = HeightField {
            x0: -10.0,
            z0: -10.0,
            dx: 1.0,
            dz: 1.0,
            nx: 21,
            nz: 21,
            h: vec![0.0; 21 * 21],
        };
        // наклон 30°: высота убывает вдоль +x (tan 0.5774)
        for r in 0..21 {
            for c in 0..21 {
                let x = hf.x0 + c as f32 * hf.dx;
                hf.h[r * 21 + c] = -0.5774 * x;
            }
        }
        hf
    }

    #[test]
    fn spin_converts_to_rolling() {
        // шар крутится (wz=−20), стоит на земле: трение в точке контакта
        // переводит спин в качение без проскальзывания (vx = −wz·r)
        let mut w = World::new();
        w.hf = Some(flat_hf());
        let r = 0.5f32;
        w.add(Sphere::new(0.0, r, 0.0, r, 0.0, 0.5));
        w.spheres[0].wz = -20.0;
        let m = 4.1887902 * r * r * r;
        let e0 = 0.2 * m * r * r * 400.0; // ½·I·ω² = 0.2·m·r²·ω²
        sim(&mut w, 2.5, 1.0 / 60.0);
        let s = &w.spheres[0];
        let slip = s.vx + s.wz * s.r;
        assert!(slip.abs() < 0.1, "качение не установилось: slip {}", slip);
        assert!(
            (2.6..3.1).contains(&s.vx),
            "vx после перекачки: {} (теория ≈2.86)",
            s.vx
        );
        let ef = 0.5 * m * (s.vx * s.vx + s.vy * s.vy + s.vz * s.vz)
            + 0.2 * m * s.r * s.r * (s.wx * s.wx + s.wy * s.wy + s.wz * s.wz);
        assert!(ef <= e0 + 1e-3, "энергия выросла: {} > {}", ef, e0);
    }

    #[test]
    fn slope_no_energy_pump() {
        // прошлый провал: на склоне Baumgarte-импульсы НАКАЧИВАЛИ энергию.
        // NGS: полная энергия (линейная + вращательная + потенциальная)
        // не превышает начальную — ни на одном кадре.
        let mut w = World::new();
        w.hf = Some(slope_30());
        let mut s = Sphere::new(0.0, 1.0, 0.0, 0.4, 0.05, 0.6);
        s.y = 0.5774 * 0.0 + 1.0;
        w.add(s);
        let m = 4.1887902 * 0.4 * 0.4 * 0.4;
        let e0 = 19.6 * m * 1.0;
        let mut rolled = false;
        for _ in 0..180 {
            w.step(1.0 / 60.0);
            let s = &w.spheres[0];
            let e = 0.5 * m * (s.vx * s.vx + s.vy * s.vy + s.vz * s.vz)
                + 0.2 * m * s.r * s.r * (s.wx * s.wx + s.wy * s.wy + s.wz * s.wz)
                + 19.6 * m * s.y;
            assert!(e <= e0 + 0.4, "накачка энергии: {} > {}", e, e0);
            if s.x > 1.5 {
                rolled = true;
            }
        }
        assert!(rolled, "шар должен скатиться по склону");
    }

    #[test]
    fn rolling_not_faster_than_free_slide() {
        // катящийся шар не может разогнаться быстрее скользящего без трения:
        // vx_своб = g·sinθ·cosθ·t = 19.6·0.5·0.866·1.2 ≈ 10.2
        let mut w = World::new();
        w.hf = Some(slope_30());
        let mut s = Sphere::new(0.0, 1.0, 0.0, 0.4, 0.05, 0.6);
        s.y = 1.0;
        w.add(s);
        sim(&mut w, 1.2, 1.0 / 60.0);
        let s = &w.spheres[0];
        assert!(s.vx > 1.5, "скатывается медленно: {}", s.vx);
        assert!(s.vx < 10.0, "быстрее свободного скольжения: {}", s.vx);
    }

    #[test]
    fn sliding_transitions_to_rolling() {
        // скользящий шар: кулоновское трение (μ·g ≈ 7.8 м/с²) гасит
        // проскальзывание и переводит шар на качение; дальше идеальная сфера
        // катится без сопротивления (rolling resistance не моделируется —
        // см. LIMITATIONS), поэтому vx НЕ ноль, а slip = 0
        let mut w = World::new();
        w.hf = Some(flat_hf());
        let mut s = Sphere::new(0.0, 0.5, 0.0, 0.5, 0.0, 0.4);
        s.vx = 6.0;
        w.add(s);
        sim(&mut w, 3.0, 1.0 / 60.0);
        let s = &w.spheres[0];
        let slip = s.vx + s.wz * s.r;
        assert!(slip.abs() < 0.1, "качение не установилось: slip {}", slip);
        // теория: перекачка спина съедает 1/1.4 скорости → vx ≈ 6/1.4 ≈ 4.3
        assert!(
            (3.9..4.7).contains(&s.vx),
            "vx после перекачки: {} (теория ≈4.3)",
            s.vx
        );
    }

    #[test]
    fn bitwise_deterministic_rolling() {
        let mk = || {
            let mut w = World::new();
            let mut hf = slope_30();
            for i in 0..hf.h.len() {
                let z = hf.z0 + (i / 21) as f32;
                hf.h[i] += 0.15 * (z * 0.9).sin();
            }
            w.hf = Some(hf);
            for k in 0..6u32 {
                let mut s = Sphere::new(
                    -1.0 + k as f32 * 0.9,
                    1.0 + k as f32 * 0.6,
                    (k as f32) * 0.5 - 1.2,
                    0.3 + 0.04 * k as f32,
                    0.1,
                    0.55,
                );
                s.wz = 8.0 - 3.0 * k as f32;
                s.vx = 0.4;
                w.add(s);
            }
            w
        };
        let (mut a, mut b) = (mk(), mk());
        for _ in 0..240 {
            a.step(1.0 / 60.0);
            b.step(1.0 / 60.0);
        }
        for (pa, pb) in a.spheres.iter().zip(b.spheres.iter()) {
            assert_eq!(pa.x.to_bits(), pb.x.to_bits());
            assert_eq!(pa.y.to_bits(), pb.y.to_bits());
            assert_eq!(pa.z.to_bits(), pb.z.to_bits());
            assert_eq!(pa.wx.to_bits(), pb.wx.to_bits());
            assert_eq!(pa.wz.to_bits(), pb.wz.to_bits());
        }
    }
}
