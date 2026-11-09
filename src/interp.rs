// src/interp.rs — Уровень 2: Lisp-интерпретатор (+ макросы, TCO, замыкания).
// Минимальный, но достаточный диалект по ТЗ §4.2:
//   числа/строки/символы/булевы, списки/векторы/хеш-таблицы, define/lambda/if/let/begin,
//   замыкания, defmacro + quasiquote/unquote, set!, TCO, eval, FFI-нативы HAL.
// Особенность: тип F32Vec — «горячие» данные DSL хранятся в плотных f32-массивах,
// чтобы SIMD-примитивы HAL работали без конверсии (см. ТЗ §4.3).
// Этап 2: cons-пары (Value::Cons) СОСЕДСТВУЮТ с Vec-списками, а владение
// парами/замыканиями/окружениями — у tracing-GC (src/gc.rs), не у Rc:
// циклы «замыкание ↔ окружение» собираются.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::gc;

pub use crate::gc::{Gc, LambdaData};

pub type NativeFn = Rc<dyn Fn(&[Value]) -> Result<Value, String>>;

#[derive(Clone)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Sym(String),
    Str(String),
    /// Vec-список: код от reader'а и «обычные» списки (как в v0.1–v0.3).
    List(Vec<Value>),
    /// Этап 2: cons-ячейка в GC-куче. Печать — стандартная: (a b c) и (a . b).
    Cons(Gc),
    Vector(Rc<RefCell<Vec<Value>>>),
    F32Vec(Rc<RefCell<Vec<f32>>>),
    Table(Rc<RefCell<HashMap<String, Value>>>),
    /// Замыкание/макрос — объект в GC-куче (env — тоже GC-объект).
    Lambda(Gc),
    Native(NativeFn, String),
    Ptr(u64),
}

// Этап 2: окружения и замыкания живут в GC-куче (src/gc.rs).
pub type GlobalCell = Rc<RefCell<Gc>>;
pub type LoadRegistry = Rc<RefCell<Vec<PathBuf>>>;

#[derive(Clone)]
pub struct CoreCtx {
    pub global: GlobalCell,
    pub registry: LoadRegistry,
    pub root: PathBuf,
}

// ——————————————————————————— окружения ———————————————————————————

pub fn env_new(parent: Option<Gc>) -> Gc {
    gc::alloc_env(parent)
}

pub fn env_get(env: Gc, name: &str) -> Option<Value> {
    gc::env_get(env, name)
}

pub fn env_set(env: Gc, name: &str, val: Value) -> Result<(), String> {
    gc::env_set(env, name, val)
}

pub fn env_define(env: Gc, name: &str, val: Value) {
    gc::env_define(env, name, val)
}

// ——————————————————————————— отображение ———————————————————————————

pub fn fmt_float(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 {
            "+inf".into()
        } else {
            "-inf".into()
        };
    }
    let s = format!("{:.6}", f);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" {
        "0.0".into()
    } else if s.contains('.') {
        s.into()
    } else {
        format!("{}.0", s)
    }
}

pub fn fmt_value(v: &Value) -> String {
    fmt_value_path(v, &mut HashSet::new())
}

/// Печать с защитой от циклов в Cons-цепочках: seen — путь DFS.
/// Разделяемые (DAG) подструктуры печатаются полностью, цикл — как «…».
fn fmt_value_path(v: &Value, seen: &mut HashSet<Gc>) -> String {
    match v {
        Value::Nil => "()".into(),
        Value::Bool(b) => (if *b { "#t" } else { "#f" }).into(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => fmt_float(*f),
        Value::Sym(s) => s.clone(),
        Value::Str(s) => s.clone(),
        Value::List(items) => {
            let inner: Vec<String> = items
                .iter()
                .take(24)
                .map(|x| fmt_value_path(x, seen))
                .collect();
            let more = if items.len() > 24 {
                format!(" …+{}", items.len() - 24)
            } else {
                String::new()
            };
            format!("({}{})", inner.join(" "), more)
        }
        Value::Cons(g) => fmt_cons(*g, seen),
        Value::Vector(r) => {
            let b = r.borrow();
            let inner: Vec<String> = b.iter().take(12).map(|x| fmt_value_path(x, seen)).collect();
            let tail = if b.len() > 12 { " …" } else { "" };
            format!("#vec({}{}) ; {}", inner.join(" "), tail, b.len())
        }
        Value::F32Vec(r) => {
            let b = r.borrow();
            let inner: Vec<String> = b.iter().take(12).map(|x| fmt_float(*x as f64)).collect();
            let tail = if b.len() > 12 { " …" } else { "" };
            format!("#f32({}{}) ; {}", inner.join(" "), tail, b.len())
        }
        Value::Table(t) => format!("#table({})", t.borrow().len()),
        Value::Lambda(g) => {
            let l = gc::lambda_clone(*g);
            let mut ps = l.params.join(" ");
            if let Some(r) = &l.rest {
                ps.push_str(&format!(" . {}", r));
            }
            format!("#{}({})", if l.is_macro { "macro" } else { "lambda" }, ps)
        }
        Value::Native(_, name) => format!("#native-{}", name),
        Value::Ptr(p) => format!("#ptr-{}", p),
    }
}

/// Печать Cons-цепочки: (a b c) для правильных, (a . b) — для точечных,
/// «…» при цикле. Ограничение 24 элементов — как у Vec-списков.
fn fmt_cons(g: Gc, seen: &mut HashSet<Gc>) -> String {
    if seen.contains(&g) {
        return "…".into();
    }
    let mut chain: Vec<Gc> = vec![g];
    seen.insert(g);
    let mut parts: Vec<String> = Vec::new();
    let mut cur = gc::pair_cdr(g);
    let (first, _) = gc::pair_get(g);
    parts.push(fmt_value_path(&first, seen));
    let mut extra = 0usize;
    let mut tail_str = String::new();
    loop {
        match &cur {
            Value::Cons(h) => {
                if seen.contains(h) {
                    tail_str = " …".into();
                    break;
                }
                seen.insert(*h);
                chain.push(*h);
                if parts.len() >= 24 {
                    extra += 1;
                    cur = gc::pair_cdr(*h);
                    continue;
                }
                let (a, d) = gc::pair_get(*h);
                parts.push(fmt_value_path(&a, seen));
                cur = d;
            }
            Value::List(l) => {
                // хвост — Vec-список: печатаем его элементы продолжением
                // (стандартная печать Lisp: (cons 1 (list 2 3)) → (1 2 3))
                for x in l {
                    if parts.len() >= 24 {
                        extra += 1;
                    } else {
                        parts.push(fmt_value_path(x, seen));
                    }
                }
                break;
            }
            Value::Nil => break,
            other => {
                parts.push(".".into());
                parts.push(fmt_value_path(other, seen));
                break;
            }
        }
    }
    for g in chain {
        seen.remove(&g);
    }
    let more = if extra > 0 {
        format!(" …+{}", extra)
    } else {
        String::new()
    };
    format!("({}{}{})", parts.join(" "), more, tail_str)
}

// ——————————————————————————— утилиты значений ———————————————————————————

pub fn truthy(v: &Value) -> bool {
    !matches!(v, Value::Nil | Value::Bool(false))
}

pub fn is_sym(v: &Value, s: &str) -> bool {
    matches!(v, Value::Sym(x) if x == s)
}

fn as_sym(v: &Value) -> Result<String, String> {
    match v {
        Value::Sym(s) => Ok(s.clone()),
        other => Err(format!("ожидался символ: {}", fmt_value(other))),
    }
}

fn as_list(v: &Value) -> Result<&Vec<Value>, String> {
    match v {
        Value::List(l) => Ok(l),
        other => Err(format!("ожидался список: {}", fmt_value(other))),
    }
}

pub fn num_val(v: &Value) -> Result<f64, String> {
    match v {
        Value::Int(i) => Ok(*i as f64),
        Value::Float(f) => Ok(*f),
        other => Err(format!("ожидалось число: {}", fmt_value(other))),
    }
}

pub fn int_val(v: &Value) -> Result<i64, String> {
    match v {
        Value::Int(i) => Ok(*i),
        Value::Float(f) => Ok(*f as i64),
        other => Err(format!("ожидалось целое: {}", fmt_value(other))),
    }
}

pub fn need_args(args: &[Value], n: usize) -> Result<(), String> {
    if args.len() < n {
        Err(format!("мало аргументов: нужно ≥ {}", n))
    } else {
        Ok(())
    }
}

// ————————————————————— Cons-цепочки (Этап 2) —————————————————————

/// Разбор Cons-цепочки с защитой от циклов: (элементы, хвост, цикл?).
/// Хвост None — список правильный (закончился Nil); Some(v) — точечный хвост.
/// Vec-список в хвосте — продолжение списка: (cons 1 (list 2 3)) — это три
/// элемента, '() — правильный конец (пустой список = nil).
pub fn chain_parts(g: Gc) -> (Vec<Value>, Option<Value>, bool) {
    let mut items = Vec::new();
    let mut seen: HashSet<Gc> = HashSet::new();
    let mut cur = Value::Cons(g);
    loop {
        match &cur {
            Value::Cons(h) => {
                if !seen.insert(*h) {
                    return (items, None, true);
                }
                let (a, d) = gc::pair_get(*h);
                items.push(a);
                cur = d;
            }
            Value::List(l) => {
                items.extend(l.iter().cloned());
                return (items, None, false);
            }
            Value::Nil => return (items, None, false),
            other => return (items, Some(other.clone()), false),
        }
    }
}

/// Построить цепочку из элементов и хвоста (хвост Nil → правильный список).
pub fn vec_into_chain(items: Vec<Value>, tail: Value) -> Value {
    items
        .into_iter()
        .rev()
        .fold(tail, |acc, x| Value::Cons(gc::alloc_pair(x, acc)))
}

pub fn value_eq(a: &Value, b: &Value) -> bool {
    fn go(a: &Value, b: &Value, path: &mut HashSet<(Gc, Gc)>) -> bool {
        match (a, b) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Int(x), Value::Int(y)) => x == y,
            (Value::Float(x), Value::Float(y)) => x == y,
            (Value::Int(x), Value::Float(y)) => (*x as f64) == *y,
            (Value::Float(x), Value::Int(y)) => *x == (*y as f64),
            (Value::Sym(x), Value::Sym(y)) => x == y,
            (Value::Str(x), Value::Str(y)) => x == y,
            (Value::List(x), Value::List(y)) => {
                x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| go(p, q, path))
            }
            (Value::Cons(x), Value::Cons(y)) => {
                if x == y {
                    return true;
                }
                // защита от циклов: пара (x,y) на текущем пути — считаем равными
                if !path.insert((*x, *y)) {
                    return true;
                }
                let (ax, dx) = gc::pair_get(*x);
                let (ay, dy) = gc::pair_get(*y);
                let ok = go(&ax, &ay, path) && go(&dx, &dy, path);
                path.remove(&(*x, *y));
                ok
            }
            // Vec-список и правильная Cons-цепочка равны поэлементно
            (Value::List(x), Value::Cons(y)) | (Value::Cons(y), Value::List(x)) => {
                let (ys, tail, cyclic) = chain_parts(*y);
                if cyclic || tail.is_some() || x.len() != ys.len() {
                    return false;
                }
                x.iter().zip(ys.iter()).all(|(p, q)| go(p, q, path))
            }
            _ => false,
        }
    }
    go(a, b, &mut HashSet::new())
}

// ——————————————————————————— парсер (Reader) ———————————————————————————

pub fn parse_program(src: &str) -> Result<Vec<Value>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut pos = 0usize;
    let mut out = Vec::new();
    loop {
        skip_ws(&chars, &mut pos)?;
        if pos >= chars.len() {
            break;
        }
        out.push(parse_expr(&chars, &mut pos)?);
    }
    Ok(out)
}

fn skip_ws(chars: &[char], pos: &mut usize) -> Result<(), String> {
    while *pos < chars.len() {
        let c = chars[*pos];
        if c.is_whitespace() {
            *pos += 1;
        } else if c == ';' {
            while *pos < chars.len() && chars[*pos] != '\n' {
                *pos += 1;
            }
        } else {
            break;
        }
    }
    Ok(())
}

fn parse_expr(chars: &[char], pos: &mut usize) -> Result<Value, String> {
    skip_ws(chars, pos)?;
    if *pos >= chars.len() {
        return Err("неожиданный конец файла".into());
    }
    let c = chars[*pos];
    match c {
        '(' | '[' => {
            let close = if c == '(' { ')' } else { ']' };
            *pos += 1;
            let mut items = Vec::new();
            let mut tail: Option<Value> = None;
            loop {
                skip_ws(chars, pos)?;
                if *pos >= chars.len() {
                    return Err("не закрыта скобка".into());
                }
                if chars[*pos] == close {
                    *pos += 1;
                    break;
                }
                let item = parse_expr(chars, pos)?;
                // точечная пара: (a . b) — Этап 2
                if is_sym(&item, ".") {
                    if items.is_empty() {
                        return Err("«.» без элемента перед ним".into());
                    }
                    if tail.is_some() {
                        return Err("лишний «.» в списке".into());
                    }
                    tail = Some(parse_expr(chars, pos)?);
                    skip_ws(chars, pos)?;
                    if *pos >= chars.len() || chars[*pos] != close {
                        return Err("после «.» — один элемент и закрывающая скобка".into());
                    }
                    *pos += 1;
                    break;
                }
                items.push(item);
            }
            Ok(match tail {
                None => Value::List(items),
                Some(t) => vec_into_chain(items, t),
            })
        }
        ')' | ']' => Err("неожиданная закрывающая скобка".into()),
        '\'' => {
            *pos += 1;
            let e = parse_expr(chars, pos)?;
            Ok(Value::List(vec![Value::Sym("quote".into()), e]))
        }
        '`' => {
            *pos += 1;
            let e = parse_expr(chars, pos)?;
            Ok(Value::List(vec![Value::Sym("quasiquote".into()), e]))
        }
        ',' => {
            *pos += 1;
            if *pos < chars.len() && chars[*pos] == '@' {
                *pos += 1;
                let e = parse_expr(chars, pos)?;
                Ok(Value::List(vec![Value::Sym("unquote-splicing".into()), e]))
            } else {
                let e = parse_expr(chars, pos)?;
                Ok(Value::List(vec![Value::Sym("unquote".into()), e]))
            }
        }
        '"' => {
            *pos += 1;
            let mut s = String::new();
            while *pos < chars.len() {
                let ch = chars[*pos];
                if ch == '"' {
                    *pos += 1;
                    return Ok(Value::Str(s));
                }
                if ch == '\\' {
                    *pos += 1;
                    if *pos >= chars.len() {
                        break;
                    }
                    match chars[*pos] {
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        'r' => s.push('\r'),
                        other => s.push(other),
                    }
                    *pos += 1;
                } else {
                    s.push(ch);
                    *pos += 1;
                }
            }
            Err("не закрыта строка".into())
        }
        _ => {
            let start = *pos;
            while *pos < chars.len() {
                let ch = chars[*pos];
                if ch.is_whitespace() || "()[]'`,;\"".contains(ch) {
                    break;
                }
                *pos += 1;
            }
            let atom: String = chars[start..*pos].iter().collect();
            parse_atom(&atom)
        }
    }
}

fn parse_atom(atom: &str) -> Result<Value, String> {
    match atom {
        "#t" | "#true" => return Ok(Value::Bool(true)),
        "#f" | "#false" => return Ok(Value::Bool(false)),
        _ => {}
    }
    if let Ok(i) = atom.parse::<i64>() {
        return Ok(Value::Int(i));
    }
    if let Ok(f) = atom.parse::<f64>() {
        return Ok(Value::Float(f));
    }
    Ok(Value::Sym(atom.to_string()))
}

// ——————————————————————————— вычислитель ———————————————————————————

pub fn eval(expr: &Value, env: Gc, depth: usize) -> Result<Value, String> {
    // Guard считает глубину Rust-стека вычислителя: сборка GC разрешена
    // только при нуле (значения в локальных переменных eval недоступны
    // обходу корней) — см. gc::maybe_collect.
    let _guard = gc::EvalGuard::enter();
    eval_inner(expr, env, depth)
}

fn eval_inner(expr: &Value, env: Gc, depth: usize) -> Result<Value, String> {
    if depth > 3000 {
        return Err("слишком глубокая рекурсия".into());
    }
    let mut expr = expr.clone();
    let mut env = env;
    let mut expansions = 0usize;
    loop {
        match expr {
            Value::Sym(ref s) => {
                return env_get(env, s).ok_or_else(|| format!("неизвестная переменная: {}", s));
            }
            // Этап 2: Cons-цепочка как форма вызова — приводим к Vec-списку
            Value::Cons(g) => {
                let (items, tail, cyclic) = chain_parts(g);
                if cyclic || tail.is_some() {
                    return Err("вызов: некорректная форма (точечная пара)".into());
                }
                expr = Value::List(items);
                continue;
            }
            Value::List(ref items) => {
                if items.is_empty() {
                    return Ok(Value::Nil);
                }
                if let Value::Sym(ref head) = items[0] {
                    match head.as_str() {
                        "quote" => {
                            if items.len() != 2 {
                                return Err("quote: нужна форма (quote x)".into());
                            }
                            return Ok(items[1].clone());
                        }
                        "if" => {
                            if items.len() < 3 {
                                return Err("if: нужна форма (if cond then [else])".into());
                            }
                            let c = eval(&items[1], env, depth + 1)?;
                            if truthy(&c) {
                                expr = items[2].clone();
                            } else if items.len() > 3 {
                                expr = items[3].clone();
                            } else {
                                return Ok(Value::Nil);
                            }
                            continue;
                        }
                        "define" => {
                            if items.len() < 3 {
                                return Err(
                                    "define: нужна форма (define имя значение) или (define (f x) тело)"
                                        .into(),
                                );
                            }
                            match &items[1] {
                                Value::List(sig) if !sig.is_empty() => {
                                    let name = as_sym(&sig[0])?;
                                    let (params, rest) =
                                        parse_params(&Value::List(sig[1..].to_vec()))?;
                                    env_define(
                                        env,
                                        &name,
                                        Value::Lambda(gc::alloc_lambda(LambdaData {
                                            params,
                                            rest,
                                            body: items[2..].to_vec(),
                                            env,
                                            is_macro: false,
                                        })),
                                    );
                                }
                                // Этап 2: сигнатура-Cons-цепочка: (define (f a . rest) …)
                                Value::Cons(g) => {
                                    let (sigv, tail, cyclic) = chain_parts(*g);
                                    if cyclic || sigv.is_empty() {
                                        return Err("define: плохая форма (сигнатура)".into());
                                    }
                                    let name = as_sym(&sigv[0])?;
                                    let pv = match tail {
                                        Some(t) => vec_into_chain(sigv[1..].to_vec(), t),
                                        None => Value::List(sigv[1..].to_vec()),
                                    };
                                    let (params, rest) = parse_params(&pv)?;
                                    env_define(
                                        env,
                                        &name,
                                        Value::Lambda(gc::alloc_lambda(LambdaData {
                                            params,
                                            rest,
                                            body: items[2..].to_vec(),
                                            env,
                                            is_macro: false,
                                        })),
                                    );
                                }
                                Value::Sym(name) => {
                                    let v = eval(&items[2], env, depth + 1)?;
                                    env_define(env, name, v);
                                }
                                other => {
                                    return Err(format!(
                                        "define: плохая форма: {}",
                                        fmt_value(other)
                                    ))
                                }
                            }
                            return Ok(Value::Nil);
                        }
                        "set!" => {
                            if items.len() != 3 {
                                return Err("set!: нужна форма (set! имя значение)".into());
                            }
                            let name = as_sym(&items[1])?;
                            let v = eval(&items[2], env, depth + 1)?;
                            env_set(env, &name, v)?;
                            return Ok(Value::Nil);
                        }
                        "lambda" => {
                            if items.len() < 3 {
                                return Err("lambda: нужна форма (lambda (args) тело)".into());
                            }
                            let (params, rest) = parse_params(&items[1])?;
                            return Ok(Value::Lambda(gc::alloc_lambda(LambdaData {
                                params,
                                rest,
                                body: items[2..].to_vec(),
                                env,
                                is_macro: false,
                            })));
                        }
                        "defmacro" => {
                            if items.len() < 3 {
                                return Err(
                                    "defmacro: нужна форма (defmacro имя (args) тело)".into()
                                );
                            }
                            let name = as_sym(&items[1])?;
                            let (params, rest) = parse_params(&items[2])?;
                            env_define(
                                env,
                                &name,
                                Value::Lambda(gc::alloc_lambda(LambdaData {
                                    params,
                                    rest,
                                    body: items[3..].to_vec(),
                                    env,
                                    is_macro: true,
                                })),
                            );
                            return Ok(Value::Nil);
                        }
                        "begin" => {
                            if items.len() == 1 {
                                return Ok(Value::Nil);
                            }
                            for it in &items[1..items.len() - 1] {
                                eval(it, env, depth + 1)?;
                            }
                            expr = items[items.len() - 1].clone();
                            continue;
                        }
                        "let" | "let*" => {
                            if items.len() < 2 {
                                return Err("let: нужны связывания".into());
                            }
                            let sequential = head == "let*";
                            let child = gc::alloc_env(Some(env));
                            match &items[1] {
                                Value::List(bs) => {
                                    for b in bs {
                                        match b {
                                            Value::List(pair) if pair.len() == 2 => {
                                                let name = as_sym(&pair[0])?;
                                                let scope = if sequential { &child } else { &env };
                                                let v = eval(&pair[1], *scope, depth + 1)?;
                                                env_define(child, &name, v);
                                            }
                                            other => {
                                                return Err(format!(
                                                    "let: плохое связывание: {}",
                                                    fmt_value(other)
                                                ))
                                            }
                                        }
                                    }
                                }
                                Value::Nil => {}
                                other => {
                                    return Err(format!(
                                        "let: плохие связывания: {}",
                                        fmt_value(other)
                                    ))
                                }
                            }
                            if items.len() == 2 {
                                return Ok(Value::Nil);
                            }
                            for it in &items[2..items.len() - 1] {
                                eval(it, child, depth + 1)?;
                            }
                            expr = items[items.len() - 1].clone();
                            env = child;
                            continue;
                        }
                        "cond" => {
                            let mut matched = false;
                            for clause in &items[1..] {
                                let cl = as_list(clause)?;
                                if cl.is_empty() {
                                    continue;
                                }
                                let test = if is_sym(&cl[0], "else") {
                                    Value::Bool(true)
                                } else {
                                    eval(&cl[0], env, depth + 1)?
                                };
                                if truthy(&test) {
                                    if cl.len() == 1 {
                                        return Ok(test);
                                    }
                                    for b in &cl[1..cl.len() - 1] {
                                        eval(b, env, depth + 1)?;
                                    }
                                    expr = cl[cl.len() - 1].clone();
                                    matched = true;
                                    break;
                                }
                            }
                            if matched {
                                continue;
                            }
                            return Ok(Value::Nil);
                        }
                        "and" => {
                            if items.len() == 1 {
                                return Ok(Value::Bool(true));
                            }
                            for it in &items[1..items.len() - 1] {
                                let v = eval(it, env, depth + 1)?;
                                if !truthy(&v) {
                                    return Ok(v);
                                }
                            }
                            expr = items[items.len() - 1].clone();
                            continue;
                        }
                        "or" => {
                            if items.len() == 1 {
                                return Ok(Value::Bool(false));
                            }
                            for it in &items[1..items.len() - 1] {
                                let v = eval(it, env, depth + 1)?;
                                if truthy(&v) {
                                    return Ok(v);
                                }
                            }
                            expr = items[items.len() - 1].clone();
                            continue;
                        }
                        "while" => {
                            if items.len() < 2 {
                                return Err("while: нужно условие".into());
                            }
                            let test = items[1].clone();
                            let body: Vec<Value> = items[2..].to_vec();
                            loop {
                                if !truthy(&eval(&test, env, depth + 1)?) {
                                    return Ok(Value::Nil);
                                }
                                for b in &body {
                                    eval(b, env, depth + 1)?;
                                }
                            }
                        }
                        "quasiquote" => {
                            if items.len() != 2 {
                                return Err("quasiquote: нужен аргумент".into());
                            }
                            return qq(&items[1], env, 1, depth);
                        }
                        _ => {
                            // макрос?
                            if let Some(Value::Lambda(g)) = env_get(env, head) {
                                if gc::lambda_is_macro(g) {
                                    expansions += 1;
                                    if expansions > 10000 {
                                        return Err("слишком много макрорасширений".into());
                                    }
                                    let l = gc::lambda_clone(g);
                                    expr = apply_lambda(&l, &items[1..], depth + 1)?;
                                    continue;
                                }
                            }
                        }
                    }
                }
                // применение функции (хвостовой вызов для lambda)
                let f = eval(&items[0], env, depth + 1)?;
                let mut args = Vec::with_capacity(items.len() - 1);
                for a in &items[1..] {
                    args.push(eval(a, env, depth + 1)?);
                }
                match f {
                    Value::Native(nf, name) => {
                        return nf(&args).map_err(|e| format!("{}: {}", name, e));
                    }
                    Value::Lambda(g) => {
                        let l = gc::lambda_clone(g);
                        let child = bind_params(&l, &args)?;
                        if l.body.is_empty() {
                            return Ok(Value::Nil);
                        }
                        for b in &l.body[..l.body.len() - 1] {
                            eval(b, child, depth + 1)?;
                        }
                        expr = l.body[l.body.len() - 1].clone();
                        env = child;
                        continue;
                    }
                    other => return Err(format!("не функция: {}", fmt_value(&other))),
                }
            }
            other => return Ok(other),
        }
    }
}

fn qq(t: &Value, env: Gc, lvl: usize, depth: usize) -> Result<Value, String> {
    match t {
        Value::List(items) if !items.is_empty() => {
            if items.len() == 2 && is_sym(&items[0], "unquote") {
                if lvl == 1 {
                    return eval(&items[1], env, depth + 1);
                }
                return Ok(Value::List(vec![
                    Value::Sym("unquote".into()),
                    qq(&items[1], env, lvl - 1, depth)?,
                ]));
            }
            if items.len() == 2 && is_sym(&items[0], "quasiquote") {
                return Ok(Value::List(vec![
                    Value::Sym("quasiquote".into()),
                    qq(&items[1], env, lvl + 1, depth)?,
                ]));
            }
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                if let Value::List(inner) = it {
                    if inner.len() == 2 && lvl == 1 && is_sym(&inner[0], "unquote-splicing") {
                        let v = eval(&inner[1], env, depth + 1)?;
                        match v {
                            Value::Nil => {}
                            Value::List(xs) => out.extend(xs),
                            other => out.push(other),
                        }
                        continue;
                    }
                }
                out.push(qq(it, env, lvl, depth)?);
            }
            Ok(Value::List(out))
        }
        // Этап 2: квазицитата проходит и через Cons-цепочки (копирует структуру)
        Value::Cons(g) => {
            let (a, d) = gc::pair_get(*g);
            let na = qq(&a, env, lvl, depth)?;
            let nd = qq(&d, env, lvl, depth)?;
            Ok(Value::Cons(gc::alloc_pair(na, nd)))
        }
        other => Ok(other.clone()),
    }
}

fn parse_params(v: &Value) -> Result<(Vec<String>, Option<String>), String> {
    match v {
        Value::Nil => Ok((vec![], None)),
        Value::Sym(s) => Ok((vec![], Some(s.clone()))),
        Value::List(items) => {
            let mut ps = Vec::new();
            let mut it = items.iter();
            while let Some(x) = it.next() {
                if is_sym(x, ".") {
                    let r = it.next().ok_or("после . нужен символ")?;
                    return Ok((ps, Some(as_sym(r)?)));
                }
                ps.push(as_sym(x)?);
            }
            Ok((ps, None))
        }
        // Этап 2: (lambda (a . rest) …) — reader строит Cons-цепочку
        Value::Cons(g) => {
            let (items, tail, cyclic) = chain_parts(*g);
            if cyclic {
                return Err("плохие параметры lambda: цикл".into());
            }
            let mut ps = Vec::with_capacity(items.len());
            for x in &items {
                ps.push(as_sym(x)?);
            }
            match tail {
                None => Ok((ps, None)),
                Some(t) => Ok((ps, Some(as_sym(&t)?))),
            }
        }
        _ => Err("плохие параметры lambda".into()),
    }
}

fn bind_params(l: &LambdaData, args: &[Value]) -> Result<Gc, String> {
    let child = gc::alloc_env(Some(l.env));
    let mut i = 0usize;
    for p in &l.params {
        if i >= args.len() {
            return Err(format!("аргументы: не хватает {}", p));
        }
        env_define(child, p, args[i].clone());
        i += 1;
    }
    if let Some(r) = &l.rest {
        let mut rest = vec![];
        while i < args.len() {
            rest.push(args[i].clone());
            i += 1;
        }
        env_define(child, r, Value::List(rest));
    } else if i < args.len() {
        return Err(format!("аргументы: лишних {}", args.len() - i));
    }
    Ok(child)
}

pub fn apply(f: &Value, args: &[Value], depth: usize) -> Result<Value, String> {
    match f {
        Value::Native(nf, name) => nf(args).map_err(|e| format!("{}: {}", name, e)),
        Value::Lambda(g) => {
            let l = gc::lambda_clone(*g);
            apply_lambda(&l, args, depth)
        }
        other => Err(format!("не функция: {}", fmt_value(other))),
    }
}

pub fn apply_lambda(l: &LambdaData, args: &[Value], depth: usize) -> Result<Value, String> {
    let child = bind_params(l, args)?;
    let mut result = Value::Nil;
    for (i, b) in l.body.iter().enumerate() {
        if i + 1 == l.body.len() {
            return eval(b, child, depth + 1);
        }
        result = eval(b, child, depth + 1)?;
    }
    Ok(result)
}

// ——————————————————————————— загрузка файлов ———————————————————————————

pub fn resolve_path(p: &str, root: &Path) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    let c1 = root.join(path);
    if c1.exists() {
        return c1;
    }
    let c2 = PathBuf::from(p);
    if c2.exists() {
        return c2;
    }
    c1
}

pub fn load_file(path: &Path, ctx: &CoreCtx) -> Result<(), String> {
    let src =
        std::fs::read_to_string(path).map_err(|e| format!("load {}: {}", path.display(), e))?;
    let prog = parse_program(&src).map_err(|e| format!("{}: {}", path.display(), e))?;
    // Формы живут до конца загрузки (могут содержать Cons-литералы) —
    // защищаем их от сборки на границах форм.
    let _pin = gc::pin(prog.clone());
    {
        let mut reg = ctx.registry.borrow_mut();
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if !reg.contains(&canon) {
            reg.push(canon);
        }
    }
    let g = *ctx.global.borrow();
    for (i, e) in prog.iter().enumerate() {
        eval(e, g, 0).map_err(|err| format!("{}: форма {}: {}", path.display(), i + 1, err))?;
        // граница формы: авто-сборка по порогу/флагу (стек eval пуст)
        gc::maybe_collect(&[g], &[]);
    }
    Ok(())
}

// ——————————————————————————— встроенные функции ———————————————————————————

static RND_STATE: AtomicU64 = AtomicU64::new(0);
static GSYM: AtomicU64 = AtomicU64::new(0);
/// v0.3: true в --mcp — Lisp-вывод (print/display) уходит в stderr,
/// чтобы не портить JSON-RPC поток протокола.
static MCP_QUIET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_mcp_quiet(on: bool) {
    MCP_QUIET.store(on, std::sync::atomic::Ordering::Relaxed);
}

fn out_line_stdout(s: &str) {
    if MCP_QUIET.load(std::sync::atomic::Ordering::Relaxed) {
        eprint!("{}", s);
    } else {
        print!("{}", s);
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
}

fn next_random() -> f64 {
    let mut x = RND_STATE.load(Ordering::Relaxed);
    if x == 0 {
        x = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E3779B97F4A7C15)
            | 1;
    }
    x = x
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    RND_STATE.store(x, Ordering::Relaxed);
    (x >> 11) as f64 / (1u64 << 53) as f64
}

fn key_str(v: &Value) -> Result<String, String> {
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
fn list_elems(v: &Value, what: &str) -> Result<Vec<Value>, String> {
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
    use need_args as need;

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
    def(env, "sqrt", |args| {
        need(args, 1)?;
        Ok(Value::Float(num_val(&args[0])?.sqrt()))
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

    // — векторы —
    def(env, "make-vector", |args| {
        need(args, 1)?;
        let n = int_val(&args[0])?.max(0) as usize;
        let fill = args.get(1).cloned().unwrap_or(Value::Int(0));
        Ok(Value::Vector(Rc::new(RefCell::new(vec![fill; n]))))
    });
    def(env, "vector", |args| {
        Ok(Value::Vector(Rc::new(RefCell::new(args.to_vec()))))
    });
    def(env, "vref", |args| {
        need(args, 2)?;
        let i = int_val(&args[1])? as usize;
        match &args[0] {
            Value::Vector(r) => r
                .borrow()
                .get(i)
                .cloned()
                .ok_or_else(|| "vref: индекс за границей".to_string()),
            _ => Err("vref: нужен вектор".into()),
        }
    });
    def(env, "vset!", |args| {
        need(args, 3)?;
        let i = int_val(&args[1])? as usize;
        match &args[0] {
            Value::Vector(r) => {
                let mut b = r.borrow_mut();
                let n = b.len();
                b.get_mut(i)
                    .map(|slot| *slot = args[2].clone())
                    .ok_or_else(|| format!("vset!: индекс за границей (len {})", n))?;
                Ok(Value::Nil)
            }
            _ => Err("vset!: нужен вектор".into()),
        }
    });
    def(env, "vector-length", |args| {
        need(args, 1)?;
        match &args[0] {
            Value::Vector(r) => Ok(Value::Int(r.borrow().len() as i64)),
            _ => Err("vector-length: нужен вектор".into()),
        }
    });
    def(env, "vector->list", |args| {
        need(args, 1)?;
        match &args[0] {
            Value::Vector(r) => Ok(Value::List(r.borrow().clone())),
            _ => Err("vector->list: нужен вектор".into()),
        }
    });
    def(env, "list->vector", |args| {
        need(args, 1)?;
        // Этап 2: и Vec-список, и Cons-цепочка
        let elems = list_elems(&args[0], "list->vector")?;
        Ok(Value::Vector(Rc::new(RefCell::new(elems))))
    });

    // — f32-векторы (горячие данные для SIMD, см. ТЗ §4.3) —
    def(env, "make-f32vec", |args| {
        need(args, 1)?;
        let n = int_val(&args[0])?.max(0) as usize;
        let fill = match args.get(1) {
            Some(v) => num_val(v)? as f32,
            None => 0.0,
        };
        Ok(Value::F32Vec(Rc::new(RefCell::new(vec![fill; n]))))
    });
    def(env, "f32-from-list", |args| {
        need(args, 1)?;
        // Этап 2: и Vec-список, и Cons-цепочка
        let elems = list_elems(&args[0], "f32-from-list")?;
        let mut v = Vec::with_capacity(elems.len());
        for x in &elems {
            v.push(num_val(x)? as f32);
        }
        Ok(Value::F32Vec(Rc::new(RefCell::new(v))))
    });
    def(env, "f32vec-length", |args| {
        need(args, 1)?;
        match &args[0] {
            Value::F32Vec(r) => Ok(Value::Int(r.borrow().len() as i64)),
            _ => Err("f32vec-length: нужен f32-вектор".into()),
        }
    });
    def(env, "f32ref", |args| {
        need(args, 2)?;
        let i = int_val(&args[1])? as usize;
        match &args[0] {
            Value::F32Vec(r) => r
                .borrow()
                .get(i)
                .map(|x| Value::Float(*x as f64))
                .ok_or_else(|| "f32ref: индекс за границей".to_string()),
            _ => Err("f32ref: нужен f32-вектор".into()),
        }
    });
    def(env, "f32set!", |args| {
        need(args, 3)?;
        let i = int_val(&args[1])? as usize;
        let v = num_val(&args[2])? as f32;
        match &args[0] {
            Value::F32Vec(r) => {
                let mut b = r.borrow_mut();
                let n = b.len();
                b.get_mut(i)
                    .map(|slot| *slot = v)
                    .ok_or_else(|| format!("f32set!: индекс за границей (len {})", n))?;
                Ok(Value::Nil)
            }
            _ => Err("f32set!: нужен f32-вектор".into()),
        }
    });
    def(env, "f32vec->list", |args| {
        need(args, 1)?;
        match &args[0] {
            Value::F32Vec(r) => Ok(Value::List(
                r.borrow().iter().map(|x| Value::Float(*x as f64)).collect(),
            )),
            _ => Err("f32vec->list: нужен f32-вектор".into()),
        }
    });

    // — таблицы (ECS-компоненты и прочее состояние) —
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
}

// ——————————————————————————— тесты этапа 2 ———————————————————————————

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc;
    use std::path::PathBuf;

    /// Изолированный контекст: тесты cargo исполняются в разных потоках,
    /// thread_local heap у каждого свой — контексты не пересекаются.
    fn ctx() -> (CoreCtx, GlobalCell) {
        let env0 = env_new(None);
        let cell: GlobalCell = Rc::new(RefCell::new(env0));
        let c = CoreCtx {
            global: cell.clone(),
            registry: Rc::new(RefCell::new(vec![])),
            root: PathBuf::from("."),
        };
        register_core(env0, &c);
        (c, cell)
    }

    /// Вычислить формы по одной (сборка — на границах форм, как в драйверах).
    fn ev_res(cell: &GlobalCell, code: &str) -> Result<String, String> {
        let prog = parse_program(code)?;
        let _pin = gc::pin(prog.clone());
        let g = *cell.borrow();
        let mut last = Value::Nil;
        let pin = gc::pin(vec![last.clone()]);
        for e in &prog {
            last = eval(e, g, 0)?;
            gc::pin_replace(&pin, vec![last.clone()]);
            gc::maybe_collect(&[g], &[]);
        }
        drop(pin);
        Ok(fmt_value(&last))
    }

    fn ev(cell: &GlobalCell, code: &str) -> String {
        ev_res(cell, code).unwrap()
    }

    #[test]
    fn cons_пара_печатается_и_читается() {
        let (_c, cell) = ctx();
        assert_eq!(ev(&cell, "(cons 1 2)"), "(1 . 2)");
        assert_eq!(ev(&cell, "(car (cons 1 2))"), "1");
        assert_eq!(ev(&cell, "(cdr (cons 1 2))"), "2");
        // читается точечная запись
        assert_eq!(ev(&cell, "(quote (a . b))"), "(a . b)");
        assert_eq!(ev(&cell, "(equal? (quote (1 . 2)) (cons 1 2))"), "#t");
        // хвост — Vec-список: печать стандартная, плоская
        assert_eq!(ev(&cell, "(cons 1 (list 2 3))"), "(1 2 3)");
        assert_eq!(ev(&cell, "(cons 1 (quote ()))"), "(1)");
        // car/cdr работают и по цепочке пар
        assert_eq!(ev(&cell, "(car (cdr (cons 1 (cons 2 3))))"), "2");
        assert_eq!(ev(&cell, "(cdr (cdr (cons 1 (cons 2 3))))"), "3");
    }

    #[test]
    fn set_car_set_cdr_и_циклы_без_зависания() {
        let (_c, cell) = ctx();
        assert_eq!(
            ev(
                &cell,
                "(define p (cons 1 2)) (set-car! p 10) (set-cdr! p (list 20 30)) p"
            ),
            "(10 20 30)"
        );
        // циклическая печать не зависает и помечается «…»
        assert_eq!(ev(&cell, "(define x (cons 1 2)) (set-cdr! x x) x"), "(1 …)");
        assert_eq!(
            ev(
                &cell,
                "(define a (cons 1 2)) (define b (cons a a)) (set-car! a b) b"
            ),
            "((… . 2) … . 2)"
        );
        // set-car!/set-cdr! на не-паре — ошибка
        assert!(ev_res(&cell, "(set-car! (list 1 2) 3)")
            .unwrap_err()
            .contains("нужна пара"));
    }

    #[test]
    fn предикаты_пар_и_списков() {
        let (_c, cell) = ctx();
        assert_eq!(
            ev(
                &cell,
                "(list (cons? (cons 1 2)) (cons? (list 1)) (pair? (cons 1 2)) \
                 (pair? (list 1)) (pair? (list)) (null? (list)) (null? (cons 1 2)) \
                 (list? (cons 1 (list 2 3))) (list? (cons 1 2)) (list? (list 1 2)))"
            ),
            "(#t #f #t #t #f #t #f #t #f #t)"
        );
    }

    #[test]
    fn списочные_операции_работают_с_цепочками() {
        let (_c, cell) = ctx();
        assert_eq!(ev(&cell, "(length (cons 1 (cons 2 (list 3))))"), "3");
        assert_eq!(
            ev(
                &cell,
                "(equal? (list 1 2 3) (cons 1 (cons 2 (cons 3 (quote ())))))"
            ),
            "#t"
        );
        assert_eq!(
            ev(&cell, "(reverse (cons 1 (cons 2 (cons 3 (quote ())))))"),
            "(3 2 1)"
        );
        assert_eq!(
            ev(&cell, "(append (list 1) (cons 2 (list 3)) (list 4))"),
            "(1 2 3 4)"
        );
        // Scheme-семантика append: последний аргумент — произвольный хвост
        assert_eq!(ev(&cell, "(append (list 1 2) 3)"), "(1 2 . 3)");
        assert_eq!(ev(&cell, "(nth (cons 10 (cons 20 30)) 1)"), "20");
        assert_eq!(ev(&cell, "(apply + 1 2 (cons 3 (list 4)))"), "10");
        assert_eq!(
            ev(&cell, "(list->vector (cons 1 (list 2 3)))"),
            "#vec(1 2 3) ; 3"
        );
        assert_eq!(
            ev(
                &cell,
                "(f32vec->list (f32-from-list (cons 1.0 (list 2.0))))"
            ),
            "(1.0 2.0)"
        );
        // map/filter из core-уровня поверх цепочек (проверим здесь примитивами)
        assert_eq!(
            ev(&cell, "(define (sq x) (* x x)) (define (my-map f lst) (if (null? lst) (quote ()) (cons (f (car lst)) (my-map f (cdr lst))))) (my-map sq (cons 1 (cons 2 (cons 3 (quote ())))) )"),
            "(1 4 9)"
        );
        // varargs с точечной сигнатурой: reader строит Cons
        assert_eq!(
            ev(&cell, "(define (f a . rest) (list a rest)) (f 1 2 3)"),
            "(1 (2 3))"
        );
        // квазицитата проходит через пары
        assert_eq!(ev(&cell, "(define t 5) (quasiquote (0 . ,t))"), "(0 . 5)");
    }

    /// Критерий этапа 2: циклы «замыкание ↔ окружение» собираются.
    /// 1000 итераций: строим пару замыканий, захватывающих друг друга через
    /// окружение let, снимаем корень, собираем — живых объектов НЕ прибавляется
    /// (assert по счётчику heap: live возвращается к базовой линии каждый круг).
    #[test]
    fn gc_собирает_циклы_замыканий_1000_циклов_без_роста() {
        let (_c, cell) = ctx();
        ev(&cell, "(define (mk) (let ((a #f) (b #f)) (set! a (lambda () b)) (set! b (lambda () a)) (list a b)))");
        ev(&cell, "(define tmp #f)");
        let g = *cell.borrow();
        gc::collect(&[g], &[]);
        let live0 = gc::live_count();
        let freed0 = gc::stats().freed;
        for _ in 0..1000 {
            ev(&cell, "(define tmp (mk))"); // цикл замыканий под корнем tmp
            ev(&cell, "(set! tmp #f)"); // корень снят
            let st = gc::collect(&[g], &[]); // стек eval пуст — сборка честная
                                             // КРИТЕРИЙ: рост кучи от циклов замыканий = 0 на каждой итерации
            assert_eq!(gc::live_count(), live0, "куча выросла: циклы не собираются");
            assert!(st.freed > freed0, "сборка ничего не освободила");
        }
        let st = gc::stats();
        assert!(
            st.freed - freed0 >= 1000 * 3,
            "ожидали ≥3000 освобождённых объектов (2 замыкания + окружение на цикл), освободили {}",
            st.freed - freed0
        );
        // и замыкания по-прежнему работают: счётчик жив
        assert_eq!(
            ev(
                &cell,
                "(define (mkc) (let ((n 0)) (lambda () (set! n (+ n 1)) n))) \
                       (define c (mkc)) (c) (c) (c) (c)"
            ),
            "4"
        );
    }

    /// Циклы пар тоже собираются: (set-cdr! x x) — 1 пара, корень снят.
    #[test]
    fn gc_собирает_циклы_пар() {
        let (_c, cell) = ctx();
        ev(&cell, "(define x #f)");
        let g = *cell.borrow();
        gc::collect(&[g], &[]);
        let live0 = gc::live_count();
        for i in 0..100 {
            ev(&cell, "(define x (cons 1 2)) (set-cdr! x x) (set! x #f)");
            gc::collect(&[g], &[]);
            assert_eq!(
                gc::live_count(),
                live0,
                "итерация {}: цикл пар не собран",
                i
            );
        }
    }

    /// Сборка откладывается во время вычисления: (gc) внутри формы не собирает,
    /// а ставит флаг; следующая граница форм собирает.
    #[test]
    fn gc_откладывается_во_время_eval() {
        let (_c, cell) = ctx();
        let g = *cell.borrow();
        let before = gc::stats().collections;
        // внутри begin стек eval активен — (gc) только просит сборку
        let prog = parse_program("(begin (gc) 42)").unwrap();
        let v = eval(&prog[0], g, 0).unwrap();
        assert_eq!(fmt_value(&v), "42");
        assert_eq!(
            gc::stats().collections,
            before,
            "сборка прошла во время eval"
        );
        assert!(gc::stats().requested, "флаг запроса не выставлен");
        // граница форм (как в ev/драйверах) выполняет отложенную сборку
        gc::maybe_collect(&[g], &[]);
        assert!(
            gc::stats().collections > before,
            "отложенная сборка не выполнена"
        );
        // примитив gc-stats отдаёт счётчики
        assert_eq!(
            ev(&cell, "(table-get (gc-stats) (quote live) -1)"),
            (gc::live_count() as i64).to_string()
        );
    }

    /// Пары/замыкания внутри Rc-контейнеров (вектор/таблица) не собираются,
    /// пока контейнер достижим: маркировка прозрачна для Rc.
    #[test]
    fn пары_внутри_контейнеров_выживают_сборку() {
        let (_c, cell) = ctx();
        ev(
            &cell,
            "(define v (make-vector 1 0)) (vset! v 0 (cons 41 (quote ()))) \
            (define t (make-table)) (table-set! t (quote p) (cons 7 8))",
        );
        let g = *cell.borrow();
        gc::collect(&[g], &[]);
        assert_eq!(ev(&cell, "(car (vref v 0))"), "41");
        assert_eq!(ev(&cell, "(cdr (table-get t (quote p) 0))"), "8");
        // замыкание в таблице вызывается после сборки
        assert_eq!(
            ev(&cell,
               "(define t2 (make-table)) (table-set! t2 (quote f) (lambda () 99))                 (table-count t2)"),
            "1"
        );
        gc::collect(&[g], &[]);
        assert_eq!(ev(&cell, "((table-get t2 (quote f) 0))"), "99");
    }
}
