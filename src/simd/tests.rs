// src/simd/tests.rs — Модульные тесты SIMD и сканера

use super::kernels::*;
use super::lisp::*;
use super::types::*;
use crate::interp::{env_new, Value};
use std::cell::RefCell;
use std::rc::Rc;

#[test]
fn test_simd_level_and_detect() {
    let lvl = detect();
    assert!(!level_name(lvl).is_empty());

    // контракт OnceLock: active_level() отдаёт уровень, записанный ПЕРВЫМ
    // инициатором — это либо detect() (без калибровки), либо calibrate(),
    // если та уже отработала в параллельном тесте (общая статика процесса).
    // calibrate() сам пишет в ACTIVE, поэтому после него равенство
    // гарантировано детерминированно; сравнивать active с detect напрямую
    // нельзя — на машинах, где замер калибровки выбрал не самый «старший»
    // уровень, тест становится зависимым от порядка тестов.
    let cal = calibrate();
    let act2 = active_level();
    assert_eq!(cal, act2, "после калибровки active обязан совпасть с ней");
}

#[test]
fn test_calibrate() {
    let lvl = calibrate();
    let name = level_name(lvl);
    assert!(!name.is_empty());
}

#[test]
fn test_vector_ops_equivalence() {
    let test_sizes = [0, 1, 3, 4, 7, 8, 15, 16, 31, 32, 47, 64, 127, 256];

    for &n in &test_sizes {
        let a: Vec<f32> = (0..n).map(|i| (i as f32 * 0.7) - 10.0).collect();
        let b: Vec<f32> = (0..n).map(|i| (i as f32 * 1.3) + 5.0).collect();

        // add
        let mut out_simd = vec![0.0f32; n];
        let mut out_scalar = vec![0.0f32; n];
        vadd(&a, &b, &mut out_simd);
        scalar::add(&a, &b, &mut out_scalar);
        assert_eq!(out_simd, out_scalar, "vadd failed at size {}", n);

        // sub
        vsub(&a, &b, &mut out_simd);
        scalar::sub(&a, &b, &mut out_scalar);
        assert_eq!(out_simd, out_scalar, "vsub failed at size {}", n);

        // mul
        vmul(&a, &b, &mut out_simd);
        scalar::mul(&a, &b, &mut out_scalar);
        assert_eq!(out_simd, out_scalar, "vmul failed at size {}", n);

        // min
        vmin(&a, &b, &mut out_simd);
        scalar::min(&a, &b, &mut out_scalar);
        assert_eq!(out_simd, out_scalar, "vmin failed at size {}", n);

        // max
        vmax(&a, &b, &mut out_simd);
        scalar::max(&a, &b, &mut out_scalar);
        assert_eq!(out_simd, out_scalar, "vmax failed at size {}", n);

        // dot
        let dot_simd = vdot(&a, &b);
        let dot_scalar = scalar::dot(&a, &b);
        let diff = (dot_simd - dot_scalar).abs();
        let max_val = dot_simd.abs().max(dot_scalar.abs()).max(1.0);
        assert!(
            diff / max_val < 1e-4,
            "vdot failed at size {}: simd={}, scalar={}",
            n,
            dot_simd,
            dot_scalar
        );
    }
}

#[test]
fn test_lisp_to_f32s() {
    let fvec = Value::F32Vec(Rc::new(RefCell::new(vec![1.0, 2.5, -3.0])));
    assert_eq!(to_f32s(&fvec).unwrap(), vec![1.0, 2.5, -3.0]);

    let list = Value::List(vec![Value::Int(1), Value::Float(2.5)]);
    assert_eq!(to_f32s(&list).unwrap(), vec![1.0, 2.5]);

    let invalid = Value::Sym("foo".into());
    assert!(to_f32s(&invalid).is_err());
}

#[test]
fn test_register_simd_env() {
    let env = env_new(None);
    register_simd(env);
    // Проверяем, что символы определены в env
    assert!(crate::interp::env_get(env, "vec-add").is_some());
    assert!(crate::interp::env_get(env, "vec-sub").is_some());
    assert!(crate::interp::env_get(env, "vec-mul").is_some());
    assert!(crate::interp::env_get(env, "vec-min").is_some());
    assert!(crate::interp::env_get(env, "vec-max").is_some());
    assert!(crate::interp::env_get(env, "vec-dot").is_some());
    assert!(crate::interp::env_get(env, "mat4-mul").is_some());
    assert!(crate::interp::env_get(env, "mat4-mul-vec4").is_some());
}
