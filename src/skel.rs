//! src/skel.rs — Этап 16 «Скелет»: иерархия костей с локальными трансформами
//! (позиция + эйлеровы углы в том же порядке, что у инстансов батча:
//! Ry·Rx·Rz + масштаб) и вычислением мировых матриц для gpu3d-batch-mat.
//! Скиннинг по весам вершин — не здесь: батч-конвейер принимает матрицу
//! на инстанс, кости отдают матрицы. HAL про скелет не знает.
//!
//! Детерминизм: обход костей строго по индексам, f32 — побитово одинаково
//! при том же входе. Родитель обязан иметь меньший индекс (topological
//! порядок проверяется в add).

#[derive(Clone)]
pub struct Bone {
    /// -1 — корень; иначе индекс родителя (< индекса кости)
    pub parent: i32,
    /// локальная позиция (относительно родителя)
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// локальные эйлеры: R = Ry·Rx·Rz (совпадает с inst_matrix)
    pub ry: f32,
    pub rx: f32,
    pub rz: f32,
    /// локальный масштаб
    pub sx: f32,
    pub sy: f32,
    pub sz: f32,
}

#[derive(Clone, Default)]
pub struct Skeleton {
    pub bones: Vec<Bone>,
}

/// аффинная матрица 3×4: строки [r00 r01 r02 tx; r10 r11 r12 ty; r20 r21 r22 tz]
type Aff = [f32; 12];

impl Skeleton {
    pub fn new() -> Self {
        Skeleton::default()
    }

    /// добавить кость; родитель — индекс уже существующей кости или -1
    pub fn add(&mut self, parent: i32, x: f32, y: f32, z: f32) -> u32 {
        assert!(
            parent < 0 || (parent as usize) < self.bones.len(),
            "skel: родитель должен быть добавлен раньше кости"
        );
        self.bones.push(Bone {
            parent,
            x,
            y,
            z,
            ry: 0.0,
            rx: 0.0,
            rz: 0.0,
            sx: 1.0,
            sy: 1.0,
            sz: 1.0,
        });
        (self.bones.len() - 1) as u32
    }

    pub fn set_pos(&mut self, i: usize, x: f32, y: f32, z: f32) {
        let b = &mut self.bones[i];
        b.x = x;
        b.y = y;
        b.z = z;
    }

    /// эйлеры в порядке батча: ry (ins[6]), rx (ins[7]), rz (ins[8])
    pub fn set_rot(&mut self, i: usize, ry: f32, rx: f32, rz: f32) {
        let b = &mut self.bones[i];
        b.ry = ry;
        b.rx = rx;
        b.rz = rz;
    }

    pub fn set_scale(&mut self, i: usize, sx: f32, sy: f32, sz: f32) {
        let b = &mut self.bones[i];
        b.sx = sx;
        b.sy = sy;
        b.sz = sz;
    }

    /// локальная аффинная матрица кости: T(pos)·R(ry,rx,rz)·S(scale)
    fn local(&self, b: &Bone) -> Aff {
        // ровно та же математика, что inst_matrix в render3d.rs:
        // (sin_cos возвращает (sin, cos)!)
        let (syw, cy) = b.ry.sin_cos();
        let (sxi, cx) = b.rx.sin_cos();
        let (szi, cz) = b.rz.sin_cos();
        // Ry·Rx:
        let a00 = cy;
        let a01 = syw * sxi;
        let a02 = syw * cx;
        let a10 = 0.0;
        let a11 = cx;
        let a12 = -sxi;
        let a20 = -syw;
        let a21 = cy * sxi;
        let a22 = cy * cx;
        // · Rz, затем ·S и ·T
        [
            a00 * cz * b.sx - a01 * szi * b.sx,
            (a00 * szi + a01 * cz) * b.sy,
            a02 * b.sz,
            b.x,
            (a10 * cz - a11 * szi) * b.sx,
            (a10 * szi + a11 * cz) * b.sy,
            a12 * b.sz,
            b.y,
            (a20 * cz - a21 * szi) * b.sx,
            (a20 * szi + a21 * cz) * b.sy,
            a22 * b.sz,
            b.z,
        ]
    }

    /// мировые матрицы всех костей: out ← 16 f32 на кость
    /// ([r00..r02 tx, r10..ty, r20..tz, 0 0 0 1] — формат gpu3d-batch-mat).
    /// Возвращает число костей.
    pub fn matrices(&self, out: &mut Vec<f32>) -> usize {
        out.clear();
        out.reserve(self.bones.len() * 16);
        let mut worlds: Vec<Aff> = Vec::with_capacity(self.bones.len());
        for (i, b) in self.bones.iter().enumerate() {
            let l = self.local(b);
            let w = if b.parent < 0 {
                l
            } else {
                let p = &worlds[b.parent as usize];
                mul_aff(p, &l)
            };
            let _ = i;
            out.extend_from_slice(&[
                w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7], w[8], w[9], w[10], w[11], 0.0, 0.0,
                0.0, 1.0,
            ]);
            worlds.push(w);
        }
        self.bones.len()
    }
}

/// w·l для аффинных 3×4: вращение — матричное, трансляция — w.R·l.t + w.t
fn mul_aff(w: &Aff, l: &Aff) -> Aff {
    let mut r = [0.0f32; 12];
    for row in 0..3 {
        for col in 0..3 {
            r[row * 4 + col] =
                w[row * 4] * l[col] + w[row * 4 + 1] * l[4 + col] + w[row * 4 + 2] * l[8 + col];
        }
        r[row * 4 + 3] =
            w[row * 4] * l[3] + w[row * 4 + 1] * l[7] + w[row * 4 + 2] * l[11] + w[row * 4 + 3];
    }
    r
}

impl Default for Bone {
    fn default() -> Self {
        Bone {
            parent: -1,
            x: 0.0,
            y: 0.0,
            z: 0.0,
            ry: 0.0,
            rx: 0.0,
            rz: 0.0,
            sx: 1.0,
            sy: 1.0,
            sz: 1.0,
        }
    }
}

/// ——— нативы Lisp: (skel-*) — реестр в Vec<Skeleton>, id — индекс ———
pub fn register(env: crate::interp::Gc, store: std::rc::Rc<std::cell::RefCell<Vec<Skeleton>>>) {
    use crate::interp::{env_define, need_args as need, num_val, Gc, Value as V};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn def<F: Fn(&[V]) -> Result<V, String> + 'static>(env: Gc, name: &str, f: F) {
        env_define(env, name, V::Native(Rc::new(f), name.to_string()));
    }

    fn skel_of<'a>(
        store: &'a Rc<RefCell<Vec<Skeleton>>>,
        v: &V,
    ) -> Result<std::cell::RefMut<'a, Skeleton>, String> {
        let id = num_val(v)? as usize;
        let mut ss = store.borrow_mut();
        if id >= ss.len() {
            ss.push(Skeleton::new());
        }
        Ok(std::cell::RefMut::map(ss, |ss| &mut ss[id]))
    }

    fn f32_of(v: &V) -> Result<f32, String> {
        num_val(v).map(|x| x as f32)
    }

    let st = store.clone();
    def(env, "skel-new", move |args| {
        need(args, 0)?;
        let mut ss = st.borrow_mut();
        ss.push(Skeleton::new());
        Ok(V::Int((ss.len() - 1) as i64))
    });

    // (skel-add! s parent x y z) → индекс кости
    let st = store.clone();
    def(env, "skel-add!", move |args| {
        need(args, 5)?;
        let parent = num_val(&args[1])? as i32;
        let mut vals = [0.0f32; 3];
        for (i, a) in args[2..].iter().enumerate() {
            vals[i] = f32_of(a)?;
        }
        Ok(V::Int(
            skel_of(&st, &args[0])?.add(parent, vals[0], vals[1], vals[2]) as i64,
        ))
    });

    // (skel-pos! s b x y z) / (skel-rot! s b ry rx rz) / (skel-scale! s b sx sy sz)
    let st = store.clone();
    def(env, "skel-pos!", move |args| {
        need(args, 5)?;
        let b = num_val(&args[1])? as usize;
        let mut vals = [0.0f32; 3];
        for (i, a) in args[2..].iter().enumerate() {
            vals[i] = f32_of(a)?;
        }
        skel_of(&st, &args[0])?.set_pos(b, vals[0], vals[1], vals[2]);
        Ok(V::Nil)
    });

    let st = store.clone();
    def(env, "skel-rot!", move |args| {
        need(args, 5)?;
        let b = num_val(&args[1])? as usize;
        let mut vals = [0.0f32; 3];
        for (i, a) in args[2..].iter().enumerate() {
            vals[i] = f32_of(a)?;
        }
        skel_of(&st, &args[0])?.set_rot(b, vals[0], vals[1], vals[2]);
        Ok(V::Nil)
    });

    let st = store.clone();
    def(env, "skel-scale!", move |args| {
        need(args, 5)?;
        let b = num_val(&args[1])? as usize;
        let mut vals = [0.0f32; 3];
        for (i, a) in args[2..].iter().enumerate() {
            vals[i] = f32_of(a)?;
        }
        skel_of(&st, &args[0])?.set_scale(b, vals[0], vals[1], vals[2]);
        Ok(V::Nil)
    });

    // (skel-matrices! s out) → n; out ← 16 f32 × n (формат gpu3d-batch-mat)
    let st = store.clone();
    def(env, "skel-matrices!", move |args| {
        need(args, 2)?;
        let out = match &args[1] {
            V::F32Vec(r) => r.clone(),
            _ => return Err("skel-matrices!: ожидался f32-вектор".into()),
        };
        let n = skel_of(&st, &args[0])?.matrices(&mut out.borrow_mut());
        Ok(V::Int(n as i64))
    });

    // (skel-batch! s out r g b) → n; out ← [матрица 16 + r g b 1.0] × n
    // (готовый буфер для gpu3d-batch-mat)
    let st = store.clone();
    def(env, "skel-batch!", move |args| {
        need(args, 5)?;
        let out = match &args[1] {
            V::F32Vec(r) => r.clone(),
            _ => return Err("skel-batch!: ожидался f32-вектор".into()),
        };
        let cr = f32_of(&args[2])?;
        let cg = f32_of(&args[3])?;
        let cb = f32_of(&args[4])?;
        let sk = skel_of(&st, &args[0])?;
        let mut mats = Vec::new();
        let n = sk.matrices(&mut mats);
        let mut buf = out.borrow_mut();
        buf.clear();
        buf.reserve(n * 20);
        for k in 0..n {
            buf.extend_from_slice(&mats[k * 16..k * 16 + 16]);
            buf.extend_from_slice(&[cr, cg, cb, 1.0]);
        }
        Ok(V::Int(n as i64))
    });

    let st = store.clone();
    def(env, "skel-n", move |args| {
        need(args, 1)?;
        Ok(V::Int(skel_of(&st, &args[0])?.bones.len() as i64))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn identity_hierarchy_is_identity() {
        let mut s = Skeleton::new();
        let root = s.add(-1, 0.0, 0.0, 0.0);
        let child = s.add(root as i32, 0.0, 0.0, 0.0);
        let mut out = Vec::new();
        let n = s.matrices(&mut out);
        assert_eq!(n, 2);
        assert_eq!(out.len(), 32);
        // единичная матрица: [1 0 0 tx | 0 1 0 ty | 0 0 1 tz | 0 0 0 1]
        for k in 0..2 {
            let m = &out[k * 16..k * 16 + 16];
            assert!(approx(m[0], 1.0) && approx(m[5], 1.0) && approx(m[10], 1.0));
            assert!(approx(m[3], 0.0) && approx(m[7], 0.0) && approx(m[11], 0.0));
            assert!(approx(m[15], 1.0));
        }
        let _ = child;
    }

    #[test]
    fn parent_rotation_carries_child() {
        // родитель повёрнут на 90° вокруг Y (ry=π/2): локальный оффсет
        // ребёнка (1, 0, 0) должен перейти в мир как (0, 0, -1)
        let mut s = Skeleton::new();
        let root = s.add(-1, 5.0, 0.0, 3.0);
        s.set_rot(root as usize, std::f32::consts::FRAC_PI_2, 0.0, 0.0);
        let child = s.add(root as i32, 1.0, 0.0, 0.0);
        let mut out = Vec::new();
        s.matrices(&mut out);
        let m = &out[child as usize * 16..child as usize * 16 + 16];
        assert!(approx(m[3], 5.0), "tx {}", m[3]); // 5 + cos90·1 ≈ 5
        assert!(approx(m[11], 2.0), "tz {}", m[11]); // 3 − sin90·1 = 2
    }

    #[test]
    fn scale_multiplies_through_chain() {
        let mut s = Skeleton::new();
        let root = s.add(-1, 0.0, 0.0, 0.0);
        s.set_scale(root as usize, 2.0, 2.0, 2.0);
        let child = s.add(root as i32, 1.0, 0.0, 0.0);
        let mut out = Vec::new();
        s.matrices(&mut out);
        let m = &out[child as usize * 16..child as usize * 16 + 16];
        assert!(
            approx(m[3], 2.0),
            "масштаб родителя носит позицию: {}",
            m[3]
        );
    }

    #[test]
    fn wind_determinism_bitwise() {
        let mk = || {
            let mut s = Skeleton::new();
            let root = s.add(-1, 1.0, 3.0, 2.0);
            s.set_rot(root as usize, 0.12, 0.03, 0.07);
            let f1 = s.add(root as i32, 0.1, 0.0, 0.2);
            s.set_rot(f1 as usize, 1.047, 0.55, 0.0);
            s.set_scale(f1 as usize, 3.0, 1.0, 3.0);
            s
        };
        let (a, b) = (mk(), mk());
        let (mut ma, mut mb) = (Vec::new(), Vec::new());
        a.matrices(&mut ma);
        b.matrices(&mut mb);
        assert_eq!(ma.len(), mb.len());
        for (u, v) in ma.iter().zip(mb.iter()) {
            assert_eq!(u.to_bits(), v.to_bits(), "скелет недетерминирован");
        }
    }

    #[test]
    fn matrices_format_affine_tail() {
        let mut s = Skeleton::new();
        s.add(-1, 9.0, 8.0, 7.0);
        s.set_rot(0, 0.3, 0.4, 0.5);
        let mut out = Vec::new();
        s.matrices(&mut out);
        // последняя строка аффинная: 0 0 0 1
        assert_eq!(&out[12..16], &[0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    #[should_panic(expected = "родитель")]
    fn parent_must_precede_child() {
        let mut s = Skeleton::new();
        s.add(5, 0.0, 0.0, 0.0); // кости 5 ещё нет
    }
}
