use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::gc::{self, Gc};
use crate::interp::builtins_vec::register_vector_builtins;
use crate::interp::env::{env_define, env_get, CoreCtx};
use crate::interp::eval::{apply, eval};
use crate::interp::loader::{load_file, out_line_stdout, resolve_path};
use crate::interp::value::{
    as_sym, chain_parts, fmt_float, fmt_value, int_val, need_args as need, num_val, truthy,
    value_eq, vec_into_chain, Value,
};

// ——————————————————————————— встроенные функции ———————————————————————————

// ГПСЧ живёт в interp/rng.rs (детерминизм — отдельная ответственность).
use crate::interp::rng::next_random;

static GSYM: AtomicU64 = AtomicU64::new(0);

pub(crate) fn key_str(v: &Value) -> Result<String, String> {
    match v {
        Value::Sym(s) => Ok(s.clone()),
        Value::Str(s) => Ok(s.clone()),
        Value::Int(i) => Ok(i.to_string()),
        Value::Float(f) => Ok(fmt_float(*f)),
        Value::Bool(b) => Ok((if *b { "#t" } else { "#f" }).into()),
        other => Err(format!("неподходящий ключ таблицы: {}", fmt_value(other))),
    }
}

/// Итерация элементов списка (Vec-список или правильная Cons-цепочка).
pub(crate) fn list_elems(v: &Value, what: &str) -> Result<Vec<Value>, String> {
    match v {
        Value::List(l) => Ok(l.clone()),
        Value::Nil => Ok(vec![]),
        Value::Cons(g) => {
            let (items, tail, cyclic) = chain_parts(*g);
            if cyclic || tail.is_some() {
                return Err(format!("{}: нужен правильный список", what));
            }
            Ok(items)
        }
        other => Err(format!("{}: нужен список: {}", what, fmt_value(other))),
    }
}

pub fn register_core(env: Gc, ctx: &CoreCtx) {
    fn def<F: Fn(&[Value]) -> Result<Value, String> + 'static>(env: Gc, name: &str, f: F) {
        env_define(env, name, Value::Native(Rc::new(f), name.to_string()));
    }

    // — навигация (A*) и «игровые» нативы: сортировка, интерполяция, строки —
    crate::interp::builtins_nav::register_nav_builtins(env);
    crate::interp::builtins_misc::register_misc_builtins(env);

    // — списки и пары (Этап 2: cons строит GC-пару; Vec-списки сосуществуют) —
    def(env, "cons", |args| {
        need(args, 2)?;
        Ok(Value::Cons(gc::alloc_pair(
            args[0].clone(),
            args[1].clone(),
        )))
    });
    def(env, "car", |args| {
        need(args, 1)?;
        match &args[0] {
            Value::Cons(g) => Ok(gc::pair_car(*g)),
            Value::List(l) if !l.is_empty() => Ok(l[0].clone()),
            Value::List(_) | Value::Nil => Ok(Value::Nil),
            _ => Err("car: нужен список или пара".into()),
        }
    });
    def(env, "cdr", |args| {
        need(args, 1)?;
        match &args[0] {
            Value::Cons(g) => Ok(gc::pair_cdr(*g)),
            Value::List(l) if !l.is_empty() => Ok(Value::List(l[1..].to_vec())),
            Value::List(_) | Value::Nil => Ok(Value::Nil),
            _ => Err("cdr: нужен список или пара".into()),
        }
    });
    def(env, "set-car!", |args| {
        need(args, 2)?;
        match &args[0] {
            Value::Cons(g) => {
                gc::pair_set_car(*g, args[1].clone());
                Ok(Value::Nil)
            }
            other => Err(format!("set-car!: нужна пара: {}", fmt_value(other))),
        }
    });
    def(env, "set-cdr!", |args| {
        need(args, 2)?;
        match &args[0] {
            Value::Cons(g) => {
                gc::pair_set_cdr(*g, args[1].clone());
                Ok(Value::Nil)
            }
            other => Err(format!("set-cdr!: нужна пара: {}", fmt_value(other))),
        }
    });
    def(env, "cons?", |args| {
        need(args, 1)?;
        Ok(Value::Bool(matches!(&args[0], Value::Cons(_))))
    });
    def(env, "list", |args| Ok(Value::List(args.to_vec())));
    def(env, "length", |args| {
        need(args, 1)?;
        Ok(Value::Int(match &args[0] {
            Value::List(l) => l.len() as i64,
            Value::Cons(g) => {
                let (items, tail, cyclic) = chain_parts(*g);
                if cyclic || tail.is_some() {
                    return Err("length: нужен правильный список".into());
                }
                items.len() as i64
            }
            Value::Vector(v) => v.borrow().len() as i64,
            Value::F32Vec(v) => v.borrow().len() as i64,
            Value::Str(s) => s.chars().count() as i64,
            Value::Nil => 0,
            other => return Err(format!("length: неподходящий тип: {}", fmt_value(other))),
        }))
    });
    def(env, "list?", |args| {
        need(args, 1)?;
        Ok(Value::Bool(match &args[0] {
            Value::Nil | Value::List(_) => true,
            Value::Cons(g) => {
                let (_, tail, cyclic) = chain_parts(*g);
                !cyclic && tail.is_none()
            }
            _ => false,
        }))
    });
    def(env, "append", |args| {
        let mut elems: Vec<Value> = vec![];
        let mut tail: Option<Value> = None;
        let mut any_pair = false;
        for (i, a) in args.iter().enumerate() {
            let last = i + 1 == args.len();
            if tail.is_some() {
                return Err("append: аргументы после точечного хвоста".into());
            }
            match a {
                Value::List(l) => elems.extend(l.iter().cloned()),
                Value::Nil => {}
                Value::Cons(g) => {
                    let (items, t, cyclic) = chain_parts(*g);
                    if cyclic {
                        return Err("append: циклический список".into());
                    }
                    elems.extend(items);
                    any_pair = true;
                    if let Some(t) = t {
                        if last {
                            tail = Some(t);
                        } else {
                            return Err("append: нужен правильный список".into());
                        }
                    }
                }
                other if last => {
                    // Scheme: последний аргумент — произвольный объект (хвост)
                    tail = Some(other.clone());
                    any_pair = true;
                }
                other => return Err(format!("append: нужен список: {}", fmt_value(other))),
            }
        }
        Ok(match tail {
            Some(t) => vec_into_chain(elems, t),
            None if any_pair => vec_into_chain(elems, Value::Nil),
            None => Value::List(elems),
        })
    });
    def(env, "reverse", |args| {
        need(args, 1)?;
        match &args[0] {
            Value::List(l) => {
                let mut v = l.clone();
                v.reverse();
                Ok(Value::List(v))
            }
            Value::Cons(g) => {
                let (items, tail, cyclic) = chain_parts(*g);
                if cyclic || tail.is_some() {
                    return Err("reverse: нужен правильный список".into());
                }
                let mut acc = Value::Nil;
                for x in items {
                    acc = Value::Cons(gc::alloc_pair(x, acc));
                }
                Ok(acc)
            }
            other => Err(format!("reverse: нужен список: {}", fmt_value(other))),
        }
    });
    def(env, "nth", |args| {
        need(args, 2)?;
        let i = int_val(&args[1])?;
        match &args[0] {
            Value::List(l) => l
                .get(i as usize)
                .cloned()
                .ok_or_else(|| "nth: индекс за границей".to_string()),
            Value::Cons(g) => {
                let mut cur = Value::Cons(*g);
                for _ in 0..i {
                    cur = match cur {
                        Value::Cons(h) => gc::pair_cdr(h),
                        _ => return Err("nth: индекс за границей".into()),
                    };
                }
                match cur {
                    Value::Cons(h) => Ok(gc::pair_car(h)),
                    _ => Err("nth: индекс за границей".into()),
                }
            }
            other => Err(format!("nth: нужен список: {}", fmt_value(other))),
        }
    });
    def(env, "null?", |args| {
        need(args, 1)?;
        Ok(Value::Bool(match &args[0] {
            Value::Nil => true,
            // '() парсится как пустой список — в Lisp-смысли это тоже null
            Value::List(l) => l.is_empty(),
            _ => false,
        }))
    });
    def(env, "pair?", |args| {
        need(args, 1)?;
        Ok(Value::Bool(
            matches!(&args[0], Value::List(l) if !l.is_empty())
                || matches!(&args[0], Value::Cons(_)),
        ))
    });
    def(env, "number?", |args| {
        need(args, 1)?;
        Ok(Value::Bool(matches!(
            &args[0],
            Value::Int(_) | Value::Float(_)
        )))
    });
    def(env, "symbol?", |args| {
        need(args, 1)?;
        Ok(Value::Bool(matches!(&args[0], Value::Sym(_))))
    });
    def(env, "string?", |args| {
        need(args, 1)?;
        Ok(Value::Bool(matches!(&args[0], Value::Str(_))))
    });
    def(env, "boolean?", |args| {
        need(args, 1)?;
        Ok(Value::Bool(matches!(&args[0], Value::Bool(_))))
    });
    def(env, "gc", |_: &[Value]| {
        // сборка происходит на границе форм (стек eval пуст) — сразу после
        // текущей формы; результат виден в (gc-stats) следующей формы
        gc::request_gc();
        Ok(Value::Nil)
    });
    def(env, "gc-stats", |_: &[Value]| {
        let s = gc::stats();
        let mut t = HashMap::new();
        t.insert("collections".into(), Value::Int(s.collections as i64));
        t.insert("allocs".into(), Value::Int(s.allocs as i64));
        t.insert("freed".into(), Value::Int(s.freed as i64));
        t.insert("live".into(), Value::Int(s.live as i64));
        t.insert("peak-live".into(), Value::Int(s.peak_live as i64));
        t.insert("threshold".into(), Value::Int(s.threshold as i64));
        t.insert("requested".into(), Value::Bool(s.requested));
        Ok(Value::Table(Rc::new(RefCell::new(t))))
    });

    // — арифметика —
    def(env, "+", |args| {
        let mut acc = 0f64;
        let mut integer = true;
        for a in args {
            match a {
                Value::Int(i) => acc += *i as f64,
                Value::Float(f) => {
                    acc += *f;
                    integer = false;
                }
                other => return Err(format!("+: не число: {}", fmt_value(other))),
            }
        }
        Ok(if integer {
            Value::Int(acc as i64)
        } else {
            Value::Float(acc)
        })
    });
    def(env, "-", |args| {
        if args.is_empty() {
            return Err("-: нужны аргументы".into());
        }
        let integer = args.iter().all(|a| matches!(a, Value::Int(_)));
        let mut acc = num_val(&args[0])?;
        if args.len() == 1 {
            acc = -acc;
        } else {
            for a in &args[1..] {
                acc -= num_val(a)?;
            }
        }
        Ok(if integer {
            Value::Int(acc as i64)
        } else {
            Value::Float(acc)
        })
    });
    def(env, "*", |args| {
        let mut acc = 1f64;
        let mut integer = true;
        for a in args {
            match a {
                Value::Int(i) => acc *= *i as f64,
                Value::Float(f) => {
                    acc *= *f;
                    integer = false;
                }
                other => return Err(format!("*: не число: {}", fmt_value(other))),
            }
        }
        Ok(if integer {
            Value::Int(acc as i64)
        } else {
            Value::Float(acc)
        })
    });
    def(env, "/", |args| {
        if args.len() < 2 {
            return Err("/: нужно ≥ 2 аргументов".into());
        }
        let mut acc = num_val(&args[0])?;
        for a in &args[1..] {
            let d = num_val(a)?;
            if d == 0.0 {
                return Err("/: деление на ноль".into());
            }
            acc /= d;
        }
        Ok(Value::Float(acc))
    });
    def(env, "mod", |args| {
        need(args, 2)?;
        let a = int_val(&args[0])?;
        let b = int_val(&args[1])?;
        if b == 0 {
            return Err("mod: деление на ноль".into());
        }
        Ok(Value::Int(a % b))
    });
    def(env, "abs", |args| {
        need(args, 1)?;
        Ok(match &args[0] {
            Value::Int(i) => Value::Int(i.abs()),
            Value::Float(f) => Value::Float(f.abs()),
            other => return Err(format!("abs: число: {}", fmt_value(other))),
        })
    });
    def(env, "floor", |args| {
        need(args, 1)?;
        Ok(Value::Int(num_val(&args[0])?.floor() as i64))
    });
    // hash-int (v0.19): детерминированный хэш целых аргументов (SplitMix64-финал).
    // Сцены ранее строили «случайность» на |sin(x·k+φ)| — при целом сиде точки
    // ложились на кривую Лиссажу вместо плоскости (лес «весь позади» в ДОЛИНЕ).
    // (hash-int a [b [c [d]]]) → целое [0, 2^32) как Float (точно в f64);
    // в Lisp: (mod (hash-int x z lvl) 100), (/ (hash-int ...) 4294967296.0).
    def(env, "hash-int", |args| {
        if args.is_empty() || args.len() > 4 {
            return Err("hash-int: (hash-int a [b [c [d]]])".into());
        }
        let mut acc: u64 = 0x243F6A8885A308D3;
        for (i, a) in args.iter().enumerate() {
            let v = match a {
                Value::Int(n) => *n as u64,
                Value::Float(f) => {
                    let n = *f as i64;
                    if n as f64 != *f {
                        return Err(format!("hash-int: {} — не целое", f));
                    }
                    n as u64
                }
                other => return Err(format!("hash-int: целые: {}", fmt_value(other))),
            };
            acc = (acc ^ v.rotate_left((i as u32) * 17))
                .wrapping_mul(0x9E3779B97F4A7C15)
                .rotate_left(31);
        }
        // финализатор splitmix64 — лавинность
        let mut x = acc;
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D049BB133111EB);
        x ^= x >> 31;
        Ok(Value::Float(((x >> 32) as u32) as f64))
    });
    def(env, "sqrt", |args| {
        need(args, 1)?;
        Ok(Value::Float(num_val(&args[0])?.sqrt()))
    });
    // atan (v0.19): (atan y) — арктангенс, (atan y x) — угол направления
    // на точку (x, y) с учётом квадранта. Нужен играм: поворот к цели.
    def(env, "atan", |args| match args.len() {
        1 => Ok(Value::Float(num_val(&args[0])?.atan())),
        2 => Ok(Value::Float(num_val(&args[0])?.atan2(num_val(&args[1])?))),
        _ => Err("atan: (atan y [x])".into()),
    });
    // тригонометрия (v0.2): нужна 3D-DSL — матрицы поворота, камера, анимация
    def(env, "sin", |args| {
        need(args, 1)?;
        Ok(Value::Float(num_val(&args[0])?.sin()))
    });
    def(env, "cos", |args| {
        need(args, 1)?;
        Ok(Value::Float(num_val(&args[0])?.cos()))
    });
    def(env, "atan2", |args| {
        need(args, 2)?;
        Ok(Value::Float(num_val(&args[0])?.atan2(num_val(&args[1])?)))
    });
    def(env, "tan", |args| {
        need(args, 1)?;
        Ok(Value::Float(num_val(&args[0])?.tan()))
    });

    // — сравнения (цепочечные) —
    macro_rules! cmp_native {
        ($name:expr, $op:tt) => {
            def(env, $name, |args| {
                if args.len() < 2 {
                    return Ok(Value::Bool(true));
                }
                let mut prev = num_val(&args[0])?;
                for a in &args[1..] {
                    let v = num_val(a)?;
                    // через переменную — чтобы не писать !(a op b) (clippy):
                    // семантика NaN сохраняется точь-в-точь
                    let chain_ok = prev $op v;
                    if !chain_ok {
                        return Ok(Value::Bool(false));
                    }
                    prev = v;
                }
                Ok(Value::Bool(true))
            });
        };
    }
    cmp_native!("=", ==);
    cmp_native!("<", <);
    cmp_native!("<=", <=);
    cmp_native!(">", >);
    cmp_native!(">=", >=);

    def(env, "min", |args| {
        need(args, 1)?;
        let mut m = num_val(&args[0])?;
        for a in &args[1..] {
            m = m.min(num_val(a)?);
        }
        Ok(Value::Float(m))
    });
    def(env, "max", |args| {
        need(args, 1)?;
        let mut m = num_val(&args[0])?;
        for a in &args[1..] {
            m = m.max(num_val(a)?);
        }
        Ok(Value::Float(m))
    });

    // — логика —
    def(env, "not", |args| {
        need(args, 1)?;
        Ok(Value::Bool(!truthy(&args[0])))
    });
    def(env, "eq?", |args| {
        need(args, 2)?;
        Ok(Value::Bool(value_eq(&args[0], &args[1])))
    });
    def(env, "equal?", |args| {
        need(args, 2)?;
        Ok(Value::Bool(value_eq(&args[0], &args[1])))
    });

    // — вывод (v0.3: в MCP-режиме stdout — канал протокола, Lisp-вывод идёт в stderr) —
    def(env, "print", |args| {
        let parts: Vec<String> = args.iter().map(fmt_value).collect();
        out_line_stdout(&format!("{}\n", parts.join(" ")));
        Ok(Value::Nil)
    });
    def(env, "display", |args| {
        let parts: Vec<String> = args.iter().map(fmt_value).collect();
        out_line_stdout(&parts.join(" "));
        Ok(Value::Nil)
    });
    def(env, "newline", |_: &[Value]| {
        out_line_stdout("\n");
        Ok(Value::Nil)
    });

    // — строки —
    def(env, "string-append", |args| {
        let mut s = String::new();
        for a in args {
            match a {
                Value::Str(x) => s.push_str(x),
                other => s.push_str(&fmt_value(other)),
            }
        }
        Ok(Value::Str(s))
    });
    def(env, "string-length", |args| {
        need(args, 1)?;
        match &args[0] {
            Value::Str(s) => Ok(Value::Int(s.chars().count() as i64)),
            _ => Err("string-length: нужна строка".into()),
        }
    });
    def(env, "string=?", |args| {
        need(args, 2)?;
        match (&args[0], &args[1]) {
            (Value::Str(a), Value::Str(b)) => Ok(Value::Bool(a == b)),
            _ => Err("string=?: нужны две строки".into()),
        }
    });
    def(env, "string-at", |args| {
        need(args, 2)?;
        match &args[0] {
            Value::Str(s) => {
                let i = int_val(&args[1])? as usize;
                let chars: Vec<char> = s.chars().collect();
                chars
                    .get(i)
                    .map(|c| Value::Str(c.to_string()))
                    .ok_or_else(|| "string-at: индекс за границей".to_string())
            }
            _ => Err("string-at: нужна строка".into()),
        }
    });
    def(env, "number->string", |args| {
        need(args, 1)?;
        Ok(Value::Str(fmt_value(&args[0])))
    });
    def(env, "string->number", |args| {
        need(args, 1)?;
        match &args[0] {
            Value::Str(s) => {
                if let Ok(i) = s.parse::<i64>() {
                    Ok(Value::Int(i))
                } else if let Ok(f) = s.parse::<f64>() {
                    Ok(Value::Float(f))
                } else {
                    Ok(Value::Bool(false))
                }
            }
            _ => Err("string->number: нужна строка".into()),
        }
    });
    def(env, "symbol->string", |args| {
        need(args, 1)?;
        match &args[0] {
            Value::Sym(s) => Ok(Value::Str(s.clone())),
            _ => Err("symbol->string: нужен символ".into()),
        }
    });
    def(env, "string->symbol", |args| {
        need(args, 1)?;
        match &args[0] {
            Value::Str(s) => Ok(Value::Sym(s.clone())),
            _ => Err("string->symbol: нужна строка".into()),
        }
    });

    // — векторы и векторный ALU —
    register_vector_builtins(env);

    // — детерминизм и запись прогона (random-seed/random-int/replay-*) —
    crate::interp::builtins_replay::register_replay_builtins(env);

    // — таблицы (ECS-компоненты и прочее состояние) —
    def(env, "table?", |args| {
        need(args, 1)?;
        Ok(Value::Bool(matches!(&args[0], Value::Table(_))))
    });
    def(env, "make-table", |_: &[Value]| {
        Ok(Value::Table(Rc::new(RefCell::new(HashMap::new()))))
    });
    def(env, "table-get", |args| {
        need(args, 2)?;
        let k = key_str(&args[1])?;
        match &args[0] {
            Value::Table(t) => Ok(t
                .borrow()
                .get(&k)
                .cloned()
                .or_else(|| args.get(2).cloned())
                .unwrap_or(Value::Nil)),
            _ => Err("table-get: нужна таблица".into()),
        }
    });
    def(env, "table-set!", |args| {
        need(args, 3)?;
        let k = key_str(&args[1])?;
        match &args[0] {
            Value::Table(t) => {
                t.borrow_mut().insert(k, args[2].clone());
                Ok(args[2].clone())
            }
            _ => Err("table-set!: нужна таблица".into()),
        }
    });
    def(env, "table-has?", |args| {
        need(args, 2)?;
        let k = key_str(&args[1])?;
        match &args[0] {
            Value::Table(t) => Ok(Value::Bool(t.borrow().contains_key(&k))),
            _ => Err("table-has?: нужна таблица".into()),
        }
    });
    def(env, "table-remove!", |args| {
        need(args, 2)?;
        let k = key_str(&args[1])?;
        match &args[0] {
            Value::Table(t) => Ok(Value::Bool(t.borrow_mut().remove(&k).is_some())),
            _ => Err("table-remove!: нужна таблица".into()),
        }
    });
    def(env, "table-keys", |args| {
        need(args, 1)?;
        match &args[0] {
            Value::Table(t) => Ok(Value::List(
                t.borrow().keys().map(|k| Value::Str(k.clone())).collect(),
            )),
            _ => Err("table-keys: нужна таблица".into()),
        }
    });
    def(env, "table-values", |args| {
        need(args, 1)?;
        match &args[0] {
            Value::Table(t) => Ok(Value::List(t.borrow().values().cloned().collect())),
            _ => Err("table-values: нужна таблица".into()),
        }
    });
    def(env, "table-entries", |args| {
        need(args, 1)?;
        match &args[0] {
            Value::Table(t) => Ok(Value::List(
                t.borrow()
                    .iter()
                    .map(|(k, v)| Value::List(vec![Value::Str(k.clone()), v.clone()]))
                    .collect(),
            )),
            _ => Err("table-entries: нужна таблица".into()),
        }
    });
    def(env, "table-clear!", |args| {
        need(args, 1)?;
        match &args[0] {
            Value::Table(t) => {
                t.borrow_mut().clear();
                Ok(Value::Nil)
            }
            _ => Err("table-clear!: нужна таблица".into()),
        }
    });
    def(env, "table-count", |args| {
        need(args, 1)?;
        match &args[0] {
            Value::Table(t) => Ok(Value::Int(t.borrow().len() as i64)),
            _ => Err("table-count: нужна таблица".into()),
        }
    });

    // — управление и прочее —
    def(env, "apply", |args| {
        need(args, 2)?;
        let f = args[0].clone();
        let mut call = args[1..args.len() - 1].to_vec();
        // Этап 2: последний аргумент — Vec-список или Cons-цепочка
        call.extend(list_elems(&args[args.len() - 1], "apply")?);
        apply(&f, &call, 0)
    });
    let gcell = ctx.global.clone();
    def(env, "eval", move |args| {
        need(args, 1)?;
        let g = *gcell.borrow();
        eval(&args[0], g, 0)
    });
    let gcell2 = ctx.global.clone();
    def(env, "defined?", move |args| {
        need(args, 1)?;
        let name = as_sym(&args[0])?;
        let g = *gcell2.borrow();
        Ok(Value::Bool(env_get(g, &name).is_some()))
    });
    def(env, "error", |args| {
        let parts: Vec<String> = args.iter().map(fmt_value).collect();
        Err(parts.join(" "))
    });
    def(env, "random", |args| {
        let r = next_random();
        if args.is_empty() {
            Ok(Value::Float(r))
        } else {
            let n = int_val(&args[0])?;
            if n <= 0 {
                return Ok(Value::Int(0));
            }
            Ok(Value::Int((r * n as f64) as i64))
        }
    });
    def(env, "gensym", |_: &[Value]| {
        let n = GSYM.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(Value::Sym(format!("g#{}", n)))
    });
    def(env, "bit?", |args| {
        need(args, 2)?;
        let n = int_val(&args[0])?;
        let i = int_val(&args[1])?;
        Ok(Value::Bool((n >> i) & 1 == 1))
    });
    let lctx = ctx.clone();
    def(env, "load", move |args| {
        need(args, 1)?;
        let p = match &args[0] {
            Value::Str(s) => s.clone(),
            Value::Sym(s) => s.clone(),
            other => return Err(format!("load: нужен путь: {}", fmt_value(other))),
        };
        let full = resolve_path(&p, &lctx.root);
        load_file(&full, &lctx)?;
        Ok(Value::Bool(true))
    });

    crate::interp::builtins_io::register_io_builtins(env, ctx);
}
