// src/render3d/lisp.rs — Биндинги 3D-конвейера к Lisp-окружению (gpu3d-*).

use crate::interp::symbols::intern;
use std::cell::RefCell;
use std::rc::Rc;

use crate::hal::Hal;
use crate::interp::{env_define, int_val, need_args, num_val, Gc, Value};
use crate::simd::to_f32s;

use super::primitives::*;
use super::raster::*;
use super::shading::*;
use super::skinning::*;
use super::types::*;

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

fn val_to_mat4(val: &Value) -> Result<[f32; 16], String> {
    let list = to_f32s(val)?;
    if list.len() != 16 {
        return Err(format!(
            "матрица 4x4 должна содержать 16 элементов, получено {}",
            list.len()
        ));
    }
    let mut m = [0.0f32; 16];
    m.copy_from_slice(&list[..16]);
    Ok(m)
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
        h.clear_3d(r, g, b);
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
            "shading".into(),
            Value::Sym(if r3.smooth {
                "smooth".into()
            } else {
                "flat".into()
            }),
        );
        t.insert(
            "filter".into(),
            Value::Sym(match r3.tex_filter {
                TexFilter::Nearest => "nearest".into(),
                TexFilter::Bilinear => "bilinear".into(),
            }),
        );
        t.insert("mipmaps".into(), Value::Bool(r3.tex_mipmaps));
        t.insert(
            "fog".into(),
            Value::List(vec![
                Value::Float(r3.fog[0] as f64),
                Value::Float(r3.fog[1] as f64),
                Value::Float(r3.fog[2] as f64),
                Value::Float(r3.fog[3] as f64),
            ]),
        );
        t.insert("shadow-enabled".into(), Value::Bool(r3.shadow_enabled));
        t.insert("shadow-bias".into(), Value::Float(r3.shadow_bias as f64));
        t.insert(
            "point-lights".into(),
            Value::Int(r3.point_lights.len() as i64),
        );
        Ok(Value::Table(Rc::new(RefCell::new(t))))
    });

    // — этап 4 «освещение и тени»: Shadow Mapping и Point Lights —

    // подготовка камеры источника света и прохода теней:
    // (gpu3d-shadow-begin light-matrix [w [h [bias]]]) или (gpu3d-shadow-begin dx dy dz size [res [bias]])
    let hc = hal.clone();
    def(env, "gpu3d-shadow-begin", move |args| {
        need(args, 1)?;
        let mut h = hc.borrow_mut();
        if args.len() >= 4 && matches!(&args[0], Value::Float(_) | Value::Int(_)) {
            let dx = num_val(&args[0])? as f32;
            let dy = num_val(&args[1])? as f32;
            let dz = num_val(&args[2])? as f32;
            let size = num_val(&args[3])? as f32;
            let res = if let Some(v) = args.get(4) {
                int_val(v)? as usize
            } else {
                512
            };
            let bias = if let Some(v) = args.get(5) {
                Some(num_val(v)? as f32)
            } else {
                None
            };
            let m = compute_light_matrix([dx, dy, dz], [0.0, 0.0, 0.0], size, 1.0, 200.0);
            shadow_begin(&mut h.r3, m, res, res, bias);
        } else {
            let m = val_to_mat4(&args[0])?;
            let w = if let Some(v) = args.get(1) {
                int_val(v)? as usize
            } else {
                512
            };
            let h_res = if let Some(v) = args.get(2) {
                int_val(v)? as usize
            } else {
                w
            };
            let bias = if let Some(v) = args.get(3) {
                Some(num_val(v)? as f32)
            } else {
                None
            };
            shadow_begin(&mut h.r3, m, w, h_res, bias);
        }
        Ok(Value::Nil)
    });

    // завершение записи теневой карты
    let hc = hal.clone();
    def(env, "gpu3d-shadow-end", move |_: &[Value]| {
        let mut h = hc.borrow_mut();
        shadow_end(&mut h.r3);
        Ok(Value::Nil)
    });

    // сброс/выключение теней
    let hc = hal.clone();
    def(env, "gpu3d-shadow-clear", move |_: &[Value]| {
        let mut h = hc.borrow_mut();
        shadow_clear(&mut h.r3);
        Ok(Value::Nil)
    });

    // управление отображением теней: (gpu3d-shadow-enable [bool]) -> bool
    let hc = hal.clone();
    def(env, "gpu3d-shadow-enable", move |args| {
        let mut h = hc.borrow_mut();
        if !args.is_empty() {
            h.r3.shadow_enabled = match &args[0] {
                Value::Bool(b) => *b,
                Value::Int(i) => *i != 0,
                Value::Nil => false,
                _ => true,
            };
        }
        Ok(Value::Bool(h.r3.shadow_enabled))
    });

    // проверка попадания точки в тень (для тестирования или геймплея на Lisp)
    let hc = hal.clone();
    def(env, "gpu3d-shadow-sample", move |args| {
        need(args, 1)?;
        let h = hc.borrow();
        let (wx, wy, wz, bias) = if args.len() == 1
            || (args.len() == 2 && matches!(&args[0], Value::List(_) | Value::F32Vec(_)))
        {
            let v = to_f32s(&args[0])?;
            if v.len() < 3 {
                return Err("gpu3d-shadow-sample: массив должен содержать xyz".into());
            }
            let b = if let Some(bv) = args.get(1) {
                num_val(bv)? as f32
            } else {
                h.r3.shadow_bias
            };
            (v[0], v[1], v[2], b)
        } else {
            need(args, 3)?;
            let x = num_val(&args[0])? as f32;
            let y = num_val(&args[1])? as f32;
            let z = num_val(&args[2])? as f32;
            let b = if let Some(bv) = args.get(3) {
                num_val(bv)? as f32
            } else {
                h.r3.shadow_bias
            };
            (x, y, z, b)
        };
        let s = shadow_sample(&h.r3, wx, wy, wz, bias);
        Ok(Value::Float(s as f64))
    });

    // смещение глубины тени (shadow bias)
    let hc = hal.clone();
    def(env, "gpu3d-shadow-bias", move |args| {
        let mut h = hc.borrow_mut();
        if !args.is_empty() {
            h.r3.shadow_bias = num_val(&args[0])? as f32;
        }
        Ok(Value::Float(h.r3.shadow_bias as f64))
    });

    // добавление точечного источника: (gpu3d-add-light ['point] x y z r g b radius [attenuation])
    let hc = hal.clone();
    def(env, "gpu3d-add-light", move |args| {
        let offset = if !args.is_empty() && matches!(&args[0], Value::Sym(_) | Value::Str(_)) {
            1
        } else {
            0
        };
        if args.len() < offset + 7 {
            return Err("gpu3d-add-light: ['point] x y z r g b radius [attenuation]".into());
        }
        let x = num_val(&args[offset])? as f32;
        let y = num_val(&args[offset + 1])? as f32;
        let z = num_val(&args[offset + 2])? as f32;
        let r = num_val(&args[offset + 3])? as f32;
        let g = num_val(&args[offset + 4])? as f32;
        let b = num_val(&args[offset + 5])? as f32;
        let radius = num_val(&args[offset + 6])? as f32;
        let attenuation = if let Some(av) = args.get(offset + 7) {
            num_val(av)? as f32
        } else {
            1.0
        };
        let mut h = hc.borrow_mut();
        h.r3.point_lights.push(PointLight {
            pos: [x, y, z],
            color: [r, g, b],
            radius,
            attenuation,
        });
        Ok(Value::Int(h.r3.point_lights.len() as i64))
    });

    let hc = hal.clone();
    def(env, "gpu3d-add-point-light", move |args| {
        if args.len() < 7 {
            return Err("gpu3d-add-point-light: x y z r g b radius [attenuation]".into());
        }
        let x = num_val(&args[0])? as f32;
        let y = num_val(&args[1])? as f32;
        let z = num_val(&args[2])? as f32;
        let r = num_val(&args[3])? as f32;
        let g = num_val(&args[4])? as f32;
        let b = num_val(&args[5])? as f32;
        let radius = num_val(&args[6])? as f32;
        let attenuation = if let Some(av) = args.get(7) {
            num_val(av)? as f32
        } else {
            1.0
        };
        let mut h = hc.borrow_mut();
        h.r3.point_lights.push(PointLight {
            pos: [x, y, z],
            color: [r, g, b],
            radius,
            attenuation,
        });
        Ok(Value::Int(h.r3.point_lights.len() as i64))
    });

    // очистка точечных источников: (gpu3d-clear-lights)
    let hc = hal.clone();
    def(env, "gpu3d-clear-lights", move |_: &[Value]| {
        let mut h = hc.borrow_mut();
        let count = h.r3.point_lights.len();
        h.r3.point_lights.clear();
        Ok(Value::Int(count as i64))
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

    // режим шейдинга: (gpu3d-shading ['flat | 'smooth]) —
    // по умолчанию 'flat (ретро-стиль), 'smooth (сглаженный Фонг)
    let hc = hal.clone();
    def(env, "gpu3d-shading", move |args| {
        let mut h = hc.borrow_mut();
        if args.is_empty() {
            let s = if h.r3.smooth { "smooth" } else { "flat" };
            return Ok(Value::Sym(intern(s)));
        }
        let smooth = match &args[0] {
            Value::Sym(s) => s.as_ref() == "smooth",
            Value::Str(s) => s == "smooth",
            Value::Bool(b) => *b,
            _ => false,
        };
        h.r3.smooth = smooth;
        Ok(Value::Sym(if smooth { "smooth" } else { "flat" }.into()))
    });

    // режим фильтрации текстур: (gpu3d-filter ['nearest | 'bilinear])
    let hc = hal.clone();
    def(env, "gpu3d-filter", move |args| {
        let mut h = hc.borrow_mut();
        if args.is_empty() {
            let s = match h.r3.tex_filter {
                TexFilter::Nearest => "nearest",
                TexFilter::Bilinear => "bilinear",
            };
            return Ok(Value::Sym(intern(s)));
        }
        let filter = match &args[0] {
            Value::Sym(s) if s.as_ref() == "bilinear" => TexFilter::Bilinear,
            Value::Str(s) if s == "bilinear" => TexFilter::Bilinear,
            _ => TexFilter::Nearest,
        };
        h.r3.tex_filter = filter;
        let s = match filter {
            TexFilter::Nearest => "nearest",
            TexFilter::Bilinear => "bilinear",
        };
        Ok(Value::Sym(intern(s)))
    });

    // использование пирамиды мипмапов: (gpu3d-mipmaps [#t | #f])
    let hc = hal.clone();
    def(env, "gpu3d-mipmaps", move |args| {
        let mut h = hc.borrow_mut();
        if args.is_empty() {
            return Ok(Value::Bool(h.r3.tex_mipmaps));
        }
        let mm = match &args[0] {
            Value::Bool(b) => *b,
            Value::Sym(s) => s.as_ref() != "nil" && s.as_ref() != "#f" && s.as_ref() != "false",
            Value::Int(i) => *i != 0,
            _ => true,
        };
        h.r3.tex_mipmaps = mm;
        Ok(Value::Bool(mm))
    });

    // расчёт сглаженных нормалей вершин сетки (усреднение нормалей смежных треугольников)
    def(env, "gpu3d-compute-normals", |args| {
        need(args, 2)?;
        let pos = to_f32s(&args[0])?;
        let idx = to_f32s(&args[1])?;
        let nrm = compute_normals(&pos, &idx)?;
        Ok(Value::F32Vec(Rc::new(RefCell::new(nrm))))
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
        let texid = if n == 8 { int_val(&args[6])? } else { 0 };
        let mut h = hc.borrow_mut();
        let drawn = mesh_draw(
            &mut h,
            &pos,
            Some(&uv),
            Some(&rgba),
            None,
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
        let n = args.len();
        if n != 7 && n != 8 {
            return Err("gpu3d-mesh: (pos idx model mvp r g b [nrm])".into());
        }
        let pos = to_f32s(&args[0])?;
        let idx = to_f32s(&args[1])?;
        let model = to_f32s(&args[2])?;
        let mvp = to_f32s(&args[3])?;
        let r = num_val(&args[4])?;
        let g = num_val(&args[5])?;
        let b = num_val(&args[6])?;
        let nrm = if n == 8 {
            Some(to_f32s(&args[7])?)
        } else {
            None
        };
        let mut h = hc.borrow_mut();
        let n = mesh_draw(
            &mut h,
            &pos,
            None,
            None,
            nrm.as_deref(),
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

    // текстурированный mesh: (gpu3d-mesh-tex pos uv idx tex model mvp r g b [nrm])
    let hc = hal.clone();
    def(env, "gpu3d-mesh-tex", move |args| {
        let n = args.len();
        if n != 9 && n != 10 {
            return Err("gpu3d-mesh-tex: (pos uv idx tex model mvp r g b [nrm])".into());
        }
        let pos = to_f32s(&args[0])?;
        let uv = to_f32s(&args[1])?;
        let idx = to_f32s(&args[2])?;
        let texid = int_val(&args[3])?;
        let model = to_f32s(&args[4])?;
        let mvp = to_f32s(&args[5])?;
        let r = num_val(&args[6])?;
        let g = num_val(&args[7])?;
        let b = num_val(&args[8])?;
        let nrm = if n == 10 {
            Some(to_f32s(&args[9])?)
        } else {
            None
        };
        check_tex_uv(Some(&uv), pos.len() / 3)?;
        let mut h = hc.borrow_mut();
        let n = mesh_draw(
            &mut h,
            &pos,
            Some(&uv),
            None,
            nrm.as_deref(),
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

    // скиннинг по весам (этап 21): (gpu3d-mesh-skin pos joints weights jmat
    // model mvp uv texid r g b) — поза суставов уже посчитана на Lisp
    // (мир узла × inverseBind), здесь — блендинг до 4 влияний на вершину
    // (нормировка весов по сумме) и растеризация текстурированного меша
    let hc = hal.clone();
    def(env, "gpu3d-mesh-skin", move |args| {
        need(args, 11)?;
        let pos = to_f32s(&args[0])?;
        let joints = to_f32s(&args[1])?;
        let weights = to_f32s(&args[2])?;
        let jmat = to_f32s(&args[3])?;
        let model = to_f32s(&args[4])?;
        let mvp = to_f32s(&args[5])?;
        let uv = to_f32s(&args[6])?;
        let texid = int_val(&args[7])?;
        let r = num_val(&args[8])?;
        let g = num_val(&args[9])?;
        let b = num_val(&args[10])?;
        let out = skin_blend(&pos, &joints, &weights, &jmat)?;
        let nv = pos.len() / 3;
        // двусторонность: mesh-путь куll'ит backfaces, а скиннед-модель
        // игрок видит с ЛЮБОЙ стороны (нос к камере = все грани CW) —
        // добавляем реверсивные треугольники (2× tris, честно дёшево)
        let mut seq: Vec<f32> = (0..nv).map(|i| i as f32).collect();
        let mut i = 0;
        while i + 2 < nv {
            seq.push((i + 2) as f32);
            seq.push((i + 1) as f32);
            seq.push(i as f32);
            i += 3;
        }
        check_tex_uv(Some(&uv), nv)?;
        let mut h = hc.borrow_mut();
        let n = mesh_draw(
            &mut h,
            &out,
            Some(&uv),
            None,
            None, // нормали не используются (flat-шейдинг по граням)
            &seq,
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

    // батч: (gpu3d-batch pos idx insts mvp [nrm]) — цвет/трансформ в инстансах
    let hc = hal.clone();
    def(env, "gpu3d-batch", move |args| {
        let n = args.len();
        if n != 4 && n != 5 {
            return Err("gpu3d-batch: (pos idx insts mvp [nrm])".into());
        }
        let pos = to_f32s(&args[0])?;
        let idx = to_f32s(&args[1])?;
        let insts = to_f32s(&args[2])?;
        let mvp = to_f32s(&args[3])?;
        let nrm = if n == 5 {
            Some(to_f32s(&args[4])?)
        } else {
            None
        };
        let mut h = hc.borrow_mut();
        let n = batch_draw(&mut h, &pos, None, nrm.as_deref(), &idx, &insts, &mvp, 0)?;
        Ok(Value::Int(n as i64))
    });

    // матричный батч (этап 16): (gpu3d-batch-mat pos idx mats mvp) —
    // инстансы по 20 f32: матрица 4×4 + цвет; источник — skel-batch!
    let hc = hal.clone();
    def(env, "gpu3d-batch-mat", move |args| {
        need(args, 4)?;
        let pos = to_f32s(&args[0])?;
        let idx = to_f32s(&args[1])?;
        let mats = to_f32s(&args[2])?;
        let mvp = to_f32s(&args[3])?;
        let mut h = hc.borrow_mut();
        let n = batch_draw_mat(&mut h, &pos, &idx, &mats, &mvp)?;
        Ok(Value::Int(n as i64))
    });

    // текстурный батч: (gpu3d-batch-tex pos uv idx tex insts mvp [nrm])
    let hc = hal.clone();
    def(env, "gpu3d-batch-tex", move |args| {
        let n = args.len();
        if n != 6 && n != 7 {
            return Err("gpu3d-batch-tex: (pos uv idx tex insts mvp [nrm])".into());
        }
        let pos = to_f32s(&args[0])?;
        let uv = to_f32s(&args[1])?;
        let idx = to_f32s(&args[2])?;
        let texid = int_val(&args[3])?;
        let insts = to_f32s(&args[4])?;
        let mvp = to_f32s(&args[5])?;
        let nrm = if n == 7 {
            Some(to_f32s(&args[6])?)
        } else {
            None
        };
        check_tex_uv(Some(&uv), pos.len() / 3)?;
        let mut h = hc.borrow_mut();
        let n = batch_draw(
            &mut h,
            &pos,
            Some(&uv),
            nrm.as_deref(),
            &idx,
            &insts,
            &mvp,
            texid,
        )?;
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
        let n = wire_draw(&mut h, &pos, &idx, &model, &mvp, r, g, b)?;
        Ok(Value::Int(n as i64))
    });

    // пост-обработка (этап 19): (gpu3d-post exposure bloom vignette grain)
    let hc = hal.clone();
    def(env, "gpu3d-post", move |args| {
        need(args, 4)?;
        let p = crate::post::PostParams {
            exposure: num_val(&args[0])? as f32,
            bloom: num_val(&args[1])? as f32,
            vignette: num_val(&args[2])? as f32,
            grain: num_val(&args[3])? as f32,
        };
        let mut h = hc.borrow_mut();
        let _ = h.flush_gpu(true); // отложенный 2D-слой — до поста (HUD рисуется позже)
        crate::post::apply(&mut h, &p);
        Ok(Value::Nil)
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
