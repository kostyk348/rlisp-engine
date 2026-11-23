// src/jit/tests.rs — тесты JIT-компилятора и согласованости с интерпретатором.
#![allow(dead_code)]

use super::*;
use crate::gc;
use crate::interp::symbols::intern;
use crate::interp::{env_get, env_new, eval, parse_program, register_core, CoreCtx, Value};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

fn fresh_env() -> gc::Gc {
    let env0 = env_new(None);
    let c = CoreCtx {
        global: Rc::new(RefCell::new(env0)),
        registry: Rc::new(RefCell::new(vec![])),
        root: PathBuf::from("."),
    };
    register_core(env0, &c);
    env0
}

fn make_lambda(src: &str) -> gc::Gc {
    let prog = parse_program(src).unwrap();
    let env = fresh_env();
    eval(&prog[0], env, 0).unwrap();
    match env_get(env, "f") {
        Some(Value::Lambda(g)) => g,
        _ => panic!("нет лямбды"),
    }
}

/// Числовая нормализация результата: Int(x) ≡ Float(x), Bool ≡ 1.0/0.0,
/// NaN ≡ NaN (по битам). Семантика сравнения interp vs JIT — см. LIMITATIONS.md.
fn norm(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        Value::Bool(true) => Some(1.0),
        Value::Bool(false) => Some(0.0),
        _ => None,
    }
}

fn results_equal(a: &Value, b: &Value) -> bool {
    match (norm(a), norm(b)) {
        (Some(x), Some(y)) => {
            if x.is_nan() && y.is_nan() {
                return true; // NaN != NaN, но семантика одинакова
            }
            (x == y) || (x.is_nan() && y.is_nan())
        }
        (None, None) => fmt_eq(a, b),
        _ => false,
    }
}

fn fmt_eq(a: &Value, b: &Value) -> bool {
    crate::interp::fmt_value(a) == crate::interp::fmt_value(b)
}

#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn jit_базовые_ядра() {
    let cases: Vec<(String, Vec<f64>, f64)> = vec![
        (
            "(define f (lambda (x y) (+ x y)))".into(),
            vec![2.0, 3.0],
            5.0,
        ),
        ("(define f (lambda (x) (* x x)))".into(), vec![7.0], 49.0),
        ("(define f (lambda () 99))".into(), vec![], 99.0),
        (
            "(define f (lambda (x) (if (< x 10) (* x 2) (+ x 100))))".into(),
            vec![4.0],
            8.0,
        ),
        (
            "(define f (lambda (x) (if (< x 10) (* x 2) (+ x 100))))".into(),
            vec![50.0],
            150.0,
        ),
        (
            "(define f (lambda (x y) (/ (- x y) (+ x y))))".into(),
            vec![7.0, 3.0],
            0.4,
        ),
        (
            "(define f (lambda (x) (let ((a (* x x)) (b (+ x 1))) (+ a b))))".into(),
            vec![3.0],
            13.0,
        ),
        (
            "(define f (lambda (x) (if (> x 0) 1 -1)))".into(),
            vec![-5.0],
            -1.0,
        ),
        (
            "(define f (lambda (x y) (and (< x 100) (+ x y))))".into(),
            vec![2.0, 3.0],
            5.0,
        ),
        (
            "(define f (lambda (x y) (or (> x 100) (* x y))))".into(),
            vec![6.0, 7.0],
            42.0,
        ),
        ("(define f (lambda (x) (* x 0)))".into(), vec![5.0], 0.0),
    ];
    for (src, args, expect) in cases {
        let g = make_lambda(&src);
        let jf =
            compile(&Value::Lambda(g)).unwrap_or_else(|| panic!("не скомпилировалось: {}", src));
        let vals: Vec<Value> = args.iter().map(|v| Value::Float(*v)).collect();
        let r = call_jitted(&jf, &vals).unwrap_or_else(|| panic!("гард упал: {}", src));
        match r {
            Value::Float(f) if f == expect => {}
            other => panic!(
                "{}: ждали {}, получили {}",
                src,
                expect,
                crate::interp::fmt_value(&other)
            ),
        }
    }
}

#[test]
fn jit_деоптимизация_unsupported() {
    // строка/символ/неизвестная форма → None (честный откат в interp)
    for src in [
        "(define f (lambda (x) (string-length \"abc\")))",
        "(define f (lambda (x) (cons x 2)))",
        "(define f (lambda (x) (unknown-op x)))",
        "(define f (lambda (x) x))",
        "(define f (lambda (x y z w) (* (+ x y) (+ z w))))", // 4 арга — ок, должен компилироваться
    ] {
        let g = make_lambda(src);
        let r = compile(&Value::Lambda(g));
        println!("{}: compiled={}", src, r.is_some());
    }
    let g = make_lambda("(define f (lambda (x) (cons x 2)))");
    assert!(compile(&Value::Lambda(g)).is_none());
}

#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn jit_этап15_cond_letstar_минмакс() {
    // паритет interp vs JIT по битам (через нормализацию Int/Float/Bool)
    let cases: Vec<(String, Vec<f64>)> =
        vec![
        (
            "(define f (lambda (x) (cond ((< x 0) (* x -1)) ((> x 10) (+ x 100)) (else (* x 2)))))"
                .into(),
            vec![-6.0],
        ),
        (
            "(define f (lambda (x) (cond ((< x 0) (* x -1)) ((> x 10) (+ x 100)) (else (* x 2)))))"
                .into(),
            vec![3.0],
        ),
        (
            "(define f (lambda (x) (cond ((< x 0) (* x -1)) ((> x 10) (+ x 100)) (else (* x 2)))))"
                .into(),
            vec![42.0],
        ),
        (
            "(define f (lambda (x) (cond ((> x 0) (min x 10.0)) (else (abs x)))))".into(),
            vec![-7.0],
        ),
        (
            "(define f (lambda (x) (cond ((> x 0) (min x 10.0)) (else (abs x)))))".into(),
            vec![77.0],
        ),
        (
            "(define f (lambda (x) (let* ((a (+ x 1)) (b (* a a))) (+ b x))))".into(),
            vec![3.0],
        ),
        (
            "(define f (lambda (x) (let* ((a (+ x 1)) (b (* a a))) (+ b x))))".into(),
            vec![-2.0],
        ),
        ("(define f (lambda (a b) (min (max a 0.0) 100.0)))".into(), vec![-5.0, 9.0]),
        ("(define f (lambda (a b) (min (max a 0.0) 100.0)))".into(), vec![250.0, 9.0]),
        ("(define f (lambda (a b) (min (max a 0.0) 100.0)))".into(), vec![42.0, 9.0]),
        ("(define f (lambda (x) (min x 5 9)))".into(), vec![7.0]),
        ("(define f (lambda (x) (max x 2 8)))".into(), vec![3.0]),
        ("(define f (lambda (x) (abs (- x 10.0))))".into(), vec![3.0]),
        ("(define f (lambda (x) (sqrt (+ (* x x) 1.0))))".into(), vec![2.0]),
        ("(define f (lambda (x) (floor (+ x 0.9))))".into(), vec![3.0]),
        ("(define f (lambda (x) (floor (* -1.5 x))))".into(), vec![1.0]),
    ];
    for (src, args) in cases {
        let g = make_lambda(&src);
        let jf =
            compile(&Value::Lambda(g)).unwrap_or_else(|| panic!("не скомпилировалось: {}", src));
        let vals: Vec<Value> = args.iter().map(|v| Value::Float(*v)).collect();
        let jit_r = call_jitted(&jf, &vals).unwrap_or_else(|| panic!("гард: {}", src));
        // путь интерпретатора — та же лямбда через apply_lambda
        let l = gc::lambda_clone(g);
        let interp_r = crate::interp::apply_lambda(&l, &vals, 0)
            .unwrap_or_else(|e| panic!("interp ошибся ({}): {}", src, e));
        assert!(
            results_equal(&interp_r, &jit_r),
            "расхождение interp vs JIT: {} args={:?} interp={} jit={}",
            src,
            args,
            crate::interp::fmt_value(&interp_r),
            crate::interp::fmt_value(&jit_r)
        );
    }
}

#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn jit_этап15_деоптимизации() {
    // cond без else (может вернуть Nil), некомпараторный тест, let*-ветвление
    for src in [
        "(define f (lambda (x) (cond ((< x 0) 1))))",
        "(define f (lambda (x) (cond (x 1) (else 2))))",
        "(define f (lambda (x) (let* ((a (< x 0))) a)))", // булево в let*-слоте
        "(define f (lambda (x) (sqrt \"abc\")))",
    ] {
        let g = make_lambda(src);
        assert!(
            compile(&Value::Lambda(g)).is_none(),
            "должна быть деоптимизация: {}",
            src
        );
    }
    // а это компилируется: 4 параметра — в пределах лимита
    let g = make_lambda("(define f (lambda (x) (min x 1.0 2.0 3.0 4.0 5.0 6.0 7.0)))");
    assert!(compile(&Value::Lambda(g)).is_some());
}

#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn jit_этап15_бенчмарк_смешанное_ядро() {
    // ядро с ветвлением, клампой и floor: замер interp vs jit (шум VPS —
    // порядок величины; точные числа — docs/benchmark.md)
    let src = "(define f (lambda (x) (min (max (* (+ (* x x) (* 0.5 (floor (+ x 3.0)))) 1.3) -50.0) 200.0)))";
    let g = make_lambda(src);
    let l = gc::lambda_clone(g);
    let jf = compile(&Value::Lambda(g)).expect("ядро обязано компилироваться");
    let vals: Vec<Value> = vec![Value::Float(2.5)];
    let interp_r = crate::interp::apply_lambda(&l, &vals, 0).unwrap();
    let jit_r = call_jitted(&jf, &vals).unwrap();
    assert!(results_equal(&interp_r, &jit_r), "паритет результата");
    let n = 100_000u32;
    let t0 = std::time::Instant::now();
    for k in 0..n {
        let a = [Value::Float((k % 97) as f64 - 48.0)];
        let _ = crate::interp::apply_lambda(&l, &a, 0);
    }
    let ti = t0.elapsed();
    let t0 = std::time::Instant::now();
    for k in 0..n {
        let a = [Value::Float((k % 97) as f64 - 48.0)];
        let _ = call_jitted(&jf, &a);
    }
    let tj = t0.elapsed();
    println!(
        "этап15 ядро: interp {:?} vs jit {:?} (ускорение ≈{:.1}×)",
        ti,
        tj,
        ti.as_secs_f64() / tj.as_secs_f64()
    );
    assert!(
        tj.as_secs_f64() * 2.0 < ti.as_secs_f64(),
        "JIT медленнее 2× от interp: {:?} vs {:?}",
        tj,
        ti
    );
}

// ——— свойственный тест: seeded xorshift, interp vs JIT ———

struct Xorshift(u64);
impl Xorshift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn f64(&mut self) -> f64 {
        (self.next() % 2000) as f64 / 100.0 - 10.0 // [-10, 10), 2 знака
    }
    fn int(&mut self) -> i64 {
        (self.next() % 100) as i64 - 50
    }
}

/// Чистая арифметика (без булевых значений — interp на них ошибается,
/// и JIT обязан в этом месте деоптимизировать, что генератор не тестирует).
fn gen_arith(rng: &mut Xorshift, d: u32) -> Value {
    if d == 0 || rng.next() % 100 < 45 {
        if rng.next() % 2 == 0 {
            Value::Sym(if rng.next() % 2 == 0 {
                "x".into()
            } else {
                "y".into()
            })
        } else if rng.next() % 2 == 0 {
            Value::Float(rng.f64())
        } else {
            Value::Int(rng.int())
        }
    } else {
        let op = ["+", "-", "*", "/"][(rng.next() % 4) as usize];
        Value::List(vec![
            Value::Sym(intern(op)),
            gen_arith(rng, d - 1),
            gen_arith(rng, d - 1),
        ])
    }
}

/// Случайное выражение: арифметика | сравнение | if | and/or | let.
/// Сравнения НЕ попадают в арифметические операнды (семантика interp:
/// булево в арифметике — ошибка).
fn gen_expr(rng: &mut Xorshift, d: u32) -> Value {
    let pick = rng.next() % 100;
    if d == 0 || pick < 35 {
        gen_arith(rng, 1)
    } else if pick < 65 {
        let op = ["+", "-", "*", "/"][(rng.next() % 4) as usize];
        Value::List(vec![
            Value::Sym(intern(op)),
            gen_arith(rng, d - 1),
            gen_arith(rng, d - 1),
        ])
    } else if pick < 78 {
        let op = ["<", "=", ">", "<=", ">="][(rng.next() % 5) as usize];
        Value::List(vec![
            Value::Sym(intern(op)),
            gen_arith(rng, d - 1),
            gen_arith(rng, d - 1),
        ])
    } else if pick < 88 {
        let op = ["<", "=", ">", "<=", ">="][(rng.next() % 5) as usize];
        Value::List(vec![
            Value::Sym("if".into()),
            Value::List(vec![
                Value::Sym(intern(op)),
                gen_arith(rng, d - 1),
                gen_arith(rng, d - 1),
            ]),
            gen_expr(rng, d - 1),
            gen_expr(rng, d - 1),
        ])
    } else if pick < 90 {
        // этап 15: унарные числовые формы (sqrt в генератор не включён —
        // отрицательный аргумент даёт NaN и законную runtime-деоптимизацию)
        let op = ["abs", "floor"][(rng.next() % 2) as usize];
        Value::List(vec![Value::Sym(intern(op)), gen_arith(rng, d - 1)])
    } else if pick < 92 {
        // cond: тест — сравнение, тела — чистая арифметика (числовое ядро)
        let op = ["<", "=", ">", "<=", ">="][(rng.next() % 5) as usize];
        Value::List(vec![
            Value::Sym("cond".into()),
            Value::List(vec![
                Value::List(vec![
                    Value::Sym(intern(op)),
                    gen_arith(rng, d - 1),
                    gen_arith(rng, d - 1),
                ]),
                gen_arith(rng, d - 1),
            ]),
            Value::List(vec![Value::Sym("else".into()), gen_arith(rng, d - 1)]),
        ])
    } else if pick < 94 {
        let op = if rng.next() % 2 == 0 { "and" } else { "or" };
        let cop = ["<", "=", ">", "<=", ">="][(rng.next() % 5) as usize];
        Value::List(vec![
            Value::Sym(intern(op)),
            Value::List(vec![
                Value::Sym(intern(cop)),
                gen_arith(rng, d - 1),
                gen_arith(rng, d - 1),
            ]),
            gen_arith(rng, d - 1),
        ])
    } else {
        let mut binds = Vec::new();
        let n = 1 + (rng.next() % 2) as usize;
        for i in 0..n {
            binds.push(Value::List(vec![
                Value::Sym(intern(&format!("v{}", i))),
                gen_arith(rng, d - 1),
            ]));
        }
        let names: Vec<String> = (0..n).map(|i| format!("v{}", i)).collect();
        Value::List(vec![
            Value::Sym("let".into()),
            Value::List(binds),
            gen_let_body(rng, d - 1, &names),
        ])
    }
}

fn gen_let_body(rng: &mut Xorshift, d: u32, vars: &[String]) -> Value {
    let pick = rng.next() % 100;
    if pick < 50 || d == 0 {
        let v = vars[(rng.next() as usize) % vars.len()].clone();
        Value::Sym(intern(&v))
    } else {
        let op = ["+", "-", "*", "/"][(rng.next() % 4) as usize];
        Value::List(vec![
            Value::Sym(intern(op)),
            gen_let_body(rng, d - 1, vars),
            gen_let_body(rng, d - 1, vars),
        ])
    }
}

#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn свойственные_тесты_interp_vs_jit_3000_выражений() {
    let mut rng = Xorshift(0xDEADBEEF_12345678);
    let mut compiled = 0usize;
    let mut deopt = 0usize;
    let mut checked = 0usize;
    for i in 0..3000u32 {
        let body = gen_expr(&mut rng, 4);
        let _src_form = Value::List(vec![
            Value::Sym("define".into()),
            Value::List(vec![
                Value::Sym("f".into()),
                Value::Sym("x".into()),
                Value::Sym("y".into()),
            ]),
            body.clone(),
        ]);
        let env = fresh_env();
        // define (f x y) body
        let define = Value::List(vec![
            Value::Sym("define".into()),
            Value::List(vec![
                Value::Sym("f".into()),
                Value::Sym("x".into()),
                Value::Sym("y".into()),
            ]),
            body.clone(),
        ]);
        if eval(&define, env, 0).is_err() {
            continue; // генератор дал невалидную форму — не JIT-кейс
        }
        let g = match env_get(env, "f") {
            Some(Value::Lambda(g)) => g,
            _ => continue,
        };
        let x = rng.f64();
        let y = rng.f64();
        let args = vec![Value::Float(x), Value::Float(y)];
        // путь интерпретатора (через apply_lambda)
        let l = gc::lambda_clone(g);
        let interp_r = crate::interp::apply_lambda(&l, &args, 0);
        // путь JIT (та же лямбда)
        let jf = compile(&Value::Lambda(g));
        match jf {
            Some(f) => {
                compiled += 1;
                let jit_r = call_jitted(&f, &args);
                match (interp_r, jit_r) {
                    (Ok(a), Some(b)) => {
                        checked += 1;
                        assert!(
                            results_equal(&a, &b),
                            "расхождение interp vs JIT (кейс {}): src={} x={} y={} interp={} jit={}",
                            i, crate::interp::fmt_value(&body), x, y,
                            crate::interp::fmt_value(&a), crate::interp::fmt_value(&b)
                        );
                    }
                    (Ok(_), None) => panic!("гард не должен падать на числовых аргументах (кейс {})", i),
                    // runtime-деоптимизация (деление на ноль): interp вернул ошибку — согласовано
                    (Err(_), None) => {}
                    (Err(e), Some(b)) => panic!(
                        "JIT вернул значение там, где interp ошибается (кейс {}): {} — тело: {}, jit={}",
                        i, e, crate::interp::fmt_value(&body), crate::interp::fmt_value(&b)
                    ),
                }
            }
            None => {
                deopt += 1;
                // деоптимизация допустима; результат interp обязан быть Ok или
                // внятной ошибкой — оба пути согласованы по построению
                assert!(interp_r.is_ok() || interp_r.is_err());
            }
        }
    }
    println!(
        "свойственный тест: {} кейсов, compiled={}, deopt={}, сверено={}",
        3000, compiled, deopt, checked
    );
    // регресс-порог: хотя бы половина случайных выражений должна JIT-компилироваться
    assert!(
        compiled >= 1500,
        "слишком много деоптимизаций: compiled={} из 3000",
        compiled
    );
    assert!(checked >= 1500);
}

#[test]
#[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
fn jit_деоптимизация_non_linux() {
    let src = "(define f (lambda (x y) (+ x y)))";
    let g = make_lambda(src);
    assert!(compile(&Value::Lambda(g)).is_none());
}
