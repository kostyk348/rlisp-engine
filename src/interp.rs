// src/interp.rs — Уровень 2: Lisp-интерпретатор (+ макросы, TCO, замыкания).
// Минимальный, но достаточный диалект по ТЗ §4.2:
//   числа/строки/символы/булевы, списки/векторы/хеш-таблицы, define/lambda/if/let/begin,
//   замыкания, defmacro + quasiquote/unquote, set!, TCO, eval, FFI-нативы HAL.
// Особенность: тип F32Vec — «горячие» данные DSL хранятся в плотных f32-массивах,
// чтобы SIMD-примитивы HAL работали без конверсии (см. ТЗ §4.3).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

pub type NativeFn = Rc<dyn Fn(&[Value]) -> Result<Value, String>>;

#[derive(Clone)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Sym(String),
    Str(String),
    List(Vec<Value>),
    Vector(Rc<RefCell<Vec<Value>>>),
    F32Vec(Rc<RefCell<Vec<f32>>>),
    Table(Rc<RefCell<HashMap<String, Value>>>),
    Lambda(Rc<Lambda>),
    Native(NativeFn, String),
    Ptr(u64),
}

pub struct Lambda {
    pub params: Vec<String>,
    pub rest: Option<String>,
    pub body: Vec<Value>,
    pub env: Rc<Env>,
    pub is_macro: bool,
}

pub struct Env {
    pub vars: RefCell<HashMap<String, Value>>,
    pub parent: Option<Rc<Env>>,
}

pub type GlobalCell = Rc<RefCell<Rc<Env>>>;
pub type LoadRegistry = Rc<RefCell<Vec<PathBuf>>>;

#[derive(Clone)]
pub struct CoreCtx {
    pub global: GlobalCell,
    pub registry: LoadRegistry,
    pub root: PathBuf,
}

// ——————————————————————————— окружения ———————————————————————————

pub fn env_new(parent: Option<Rc<Env>>) -> Rc<Env> {
    Rc::new(Env {
        vars: RefCell::new(HashMap::new()),
        parent,
    })
}

pub fn env_get(env: &Rc<Env>, name: &str) -> Option<Value> {
    let mut cur = Some(env.clone());
    while let Some(e) = cur {
        if let Some(v) = e.vars.borrow().get(name) {
            return Some(v.clone());
        }
        cur = e.parent.clone();
    }
    None
}

pub fn env_set(env: &Rc<Env>, name: &str, val: Value) -> Result<(), String> {
    let mut cur = Some(env.clone());
    while let Some(e) = cur {
        if e.vars.borrow().contains_key(name) {
            e.vars.borrow_mut().insert(name.to_string(), val);
            return Ok(());
        }
        cur = e.parent.clone();
    }
    Err(format!("set!: переменная не объявлена: {}", name))
}

pub fn env_define(env: &Rc<Env>, name: &str, val: Value) {
    env.vars.borrow_mut().insert(name.to_string(), val);
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
    match v {
        Value::Nil => "()".into(),
        Value::Bool(b) => (if *b { "#t" } else { "#f" }).into(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => fmt_float(*f),
        Value::Sym(s) => s.clone(),
        Value::Str(s) => s.clone(),
        Value::List(items) => {
            let inner: Vec<String> = items.iter().take(24).map(fmt_value).collect();
            let more = if items.len() > 24 {
                format!(" …+{}", items.len() - 24)
            } else {
                String::new()
            };
            format!("({}{})", inner.join(" "), more)
        }
        Value::Vector(r) => {
            let b = r.borrow();
            let inner: Vec<String> = b.iter().take(12).map(fmt_value).collect();
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
        Value::Lambda(l) => {
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

pub fn value_eq(a: &Value, b: &Value) -> bool {
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
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| value_eq(p, q))
        }
        _ => false,
    }
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
            loop {
                skip_ws(chars, pos)?;
                if *pos >= chars.len() {
                    return Err("не закрыта скобка".into());
                }
                if chars[*pos] == close {
                    *pos += 1;
                    break;
                }
                items.push(parse_expr(chars, pos)?);
            }
            Ok(Value::List(items))
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

pub fn eval(expr: &Value, env: &Rc<Env>, depth: usize) -> Result<Value, String> {
    if depth > 3000 {
        return Err("слишком глубокая рекурсия".into());
    }
    let mut expr = expr.clone();
    let mut env = env.clone();
    let mut expansions = 0usize;
    loop {
        match expr {
            Value::Sym(ref s) => {
                return env_get(&env, s).ok_or_else(|| format!("неизвестная переменная: {}", s));
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
                            let c = eval(&items[1], &env, depth + 1)?;
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
                                        &env,
                                        &name,
                                        Value::Lambda(Rc::new(Lambda {
                                            params,
                                            rest,
                                            body: items[2..].to_vec(),
                                            env: env.clone(),
                                            is_macro: false,
                                        })),
                                    );
                                }
                                Value::Sym(name) => {
                                    let v = eval(&items[2], &env, depth + 1)?;
                                    env_define(&env, name, v);
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
                            let v = eval(&items[2], &env, depth + 1)?;
                            env_set(&env, &name, v)?;
                            return Ok(Value::Nil);
                        }
                        "lambda" => {
                            if items.len() < 3 {
                                return Err("lambda: нужна форма (lambda (args) тело)".into());
                            }
                            let (params, rest) = parse_params(&items[1])?;
                            return Ok(Value::Lambda(Rc::new(Lambda {
                                params,
                                rest,
                                body: items[2..].to_vec(),
                                env: env.clone(),
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
                                &env,
                                &name,
                                Value::Lambda(Rc::new(Lambda {
                                    params,
                                    rest,
                                    body: items[3..].to_vec(),
                                    env: env.clone(),
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
                                eval(it, &env, depth + 1)?;
                            }
                            expr = items[items.len() - 1].clone();
                            continue;
                        }
                        "let" | "let*" => {
                            if items.len() < 2 {
                                return Err("let: нужны связывания".into());
                            }
                            let sequential = head == "let*";
                            let child = env_new(Some(env.clone()));
                            match &items[1] {
                                Value::List(bs) => {
                                    for b in bs {
                                        match b {
                                            Value::List(pair) if pair.len() == 2 => {
                                                let name = as_sym(&pair[0])?;
                                                let scope = if sequential { &child } else { &env };
                                                let v = eval(&pair[1], scope, depth + 1)?;
                                                env_define(&child, &name, v);
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
                                eval(it, &child, depth + 1)?;
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
                                    eval(&cl[0], &env, depth + 1)?
                                };
                                if truthy(&test) {
                                    if cl.len() == 1 {
                                        return Ok(test);
                                    }
                                    for b in &cl[1..cl.len() - 1] {
                                        eval(b, &env, depth + 1)?;
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
                                let v = eval(it, &env, depth + 1)?;
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
                                let v = eval(it, &env, depth + 1)?;
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
                                if !truthy(&eval(&test, &env, depth + 1)?) {
                                    return Ok(Value::Nil);
                                }
                                for b in &body {
                                    eval(b, &env, depth + 1)?;
                                }
                            }
                        }
                        "quasiquote" => {
                            if items.len() != 2 {
                                return Err("quasiquote: нужен аргумент".into());
                            }
                            return qq(&items[1], &env, 1, depth);
                        }
                        _ => {
                            // макрос?
                            if let Some(Value::Lambda(l)) = env_get(&env, head) {
                                if l.is_macro {
                                    expansions += 1;
                                    if expansions > 10000 {
                                        return Err("слишком много макрорасширений".into());
                                    }
                                    expr = apply_lambda(&l, &items[1..], depth + 1)?;
                                    continue;
                                }
                            }
                        }
                    }
                }
                // применение функции (хвостовой вызов для lambda)
                let f = eval(&items[0], &env, depth + 1)?;
                let mut args = Vec::with_capacity(items.len() - 1);
                for a in &items[1..] {
                    args.push(eval(a, &env, depth + 1)?);
                }
                match f {
                    Value::Native(nf, name) => {
                        return nf(&args).map_err(|e| format!("{}: {}", name, e));
                    }
                    Value::Lambda(l) => {
                        let child = bind_params(&l, &args)?;
                        if l.body.is_empty() {
                            return Ok(Value::Nil);
                        }
                        for b in &l.body[..l.body.len() - 1] {
                            eval(b, &child, depth + 1)?;
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

fn qq(t: &Value, env: &Rc<Env>, lvl: usize, depth: usize) -> Result<Value, String> {
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
        _ => Err("плохие параметры lambda".into()),
    }
}

fn bind_params(l: &Lambda, args: &[Value]) -> Result<Rc<Env>, String> {
    let child = env_new(Some(l.env.clone()));
    let mut i = 0usize;
    for p in &l.params {
        if i >= args.len() {
            return Err(format!("аргументы: не хватает {}", p));
        }
        env_define(&child, p, args[i].clone());
        i += 1;
    }
    if let Some(r) = &l.rest {
        let mut rest = vec![];
        while i < args.len() {
            rest.push(args[i].clone());
            i += 1;
        }
        env_define(&child, r, Value::List(rest));
    } else if i < args.len() {
        return Err(format!("аргументы: лишних {}", args.len() - i));
    }
    Ok(child)
}

pub fn apply(f: &Value, args: &[Value], depth: usize) -> Result<Value, String> {
    match f {
        Value::Native(nf, name) => nf(args).map_err(|e| format!("{}: {}", name, e)),
        Value::Lambda(l) => apply_lambda(l, args, depth),
        other => Err(format!("не функция: {}", fmt_value(other))),
    }
}

pub fn apply_lambda(l: &Rc<Lambda>, args: &[Value], depth: usize) -> Result<Value, String> {
    let child = bind_params(l, args)?;
    let mut result = Value::Nil;
    for (i, b) in l.body.iter().enumerate() {
        if i + 1 == l.body.len() {
            return eval(b, &child, depth + 1);
        }
        result = eval(b, &child, depth + 1)?;
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
    {
        let mut reg = ctx.registry.borrow_mut();
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if !reg.contains(&canon) {
            reg.push(canon);
        }
    }
    let g = ctx.global.borrow().clone();
    for (i, e) in prog.iter().enumerate() {
        eval(e, &g, 0).map_err(|err| format!("{}: форма {}: {}", path.display(), i + 1, err))?;
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

pub fn register_core(env: &Rc<Env>, ctx: &CoreCtx) {
    fn def<F: Fn(&[Value]) -> Result<Value, String> + 'static>(env: &Rc<Env>, name: &str, f: F) {
        env_define(env, name, Value::Native(Rc::new(f), name.to_string()));
    }
    use need_args as need;

    // — списки —
    def(env, "cons", |args| {
        need(args, 2)?;
        match &args[1] {
            Value::List(l) => {
                let mut out = Vec::with_capacity(l.len() + 1);
                out.push(args[0].clone());
                out.extend(l.iter().cloned());
                Ok(Value::List(out))
            }
            Value::Nil => Ok(Value::List(vec![args[0].clone()])),
            _ => Err("cons: второй аргумент должен быть списком".into()),
        }
    });
    def(env, "car", |args| {
        need(args, 1)?;
        match &args[0] {
            Value::List(l) if !l.is_empty() => Ok(l[0].clone()),
            Value::List(_) | Value::Nil => Ok(Value::Nil),
            _ => Err("car: нужен список".into()),
        }
    });
    def(env, "cdr", |args| {
        need(args, 1)?;
        match &args[0] {
            Value::List(l) if !l.is_empty() => Ok(Value::List(l[1..].to_vec())),
            Value::List(_) | Value::Nil => Ok(Value::Nil),
            _ => Err("cdr: нужен список".into()),
        }
    });
    def(env, "list", |args| Ok(Value::List(args.to_vec())));
    def(env, "length", |args| {
        need(args, 1)?;
        Ok(Value::Int(match &args[0] {
            Value::List(l) => l.len() as i64,
            Value::Vector(v) => v.borrow().len() as i64,
            Value::F32Vec(v) => v.borrow().len() as i64,
            Value::Str(s) => s.chars().count() as i64,
            Value::Nil => 0,
            other => return Err(format!("length: неподходящий тип: {}", fmt_value(other))),
        }))
    });
    def(env, "append", |args| {
        let mut out = vec![];
        for a in args {
            match a {
                Value::List(l) => out.extend(l.iter().cloned()),
                Value::Nil => {}
                other => return Err(format!("append: нужен список: {}", fmt_value(other))),
            }
        }
        Ok(Value::List(out))
    });
    def(env, "reverse", |args| {
        need(args, 1)?;
        match &args[0] {
            Value::List(l) => {
                let mut v = l.clone();
                v.reverse();
                Ok(Value::List(v))
            }
            _ => Err("reverse: нужен список".into()),
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
            _ => Err("nth: нужен список".into()),
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
            matches!(&args[0], Value::List(l) if !l.is_empty()),
        ))
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
        match &args[0] {
            Value::List(l) => Ok(Value::Vector(Rc::new(RefCell::new(l.clone())))),
            _ => Err("list->vector: нужен список".into()),
        }
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
        match &args[0] {
            Value::List(l) => {
                let mut v = Vec::with_capacity(l.len());
                for x in l {
                    v.push(num_val(x)? as f32);
                }
                Ok(Value::F32Vec(Rc::new(RefCell::new(v))))
            }
            _ => Err("f32-from-list: нужен список чисел".into()),
        }
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
        match &args[args.len() - 1] {
            Value::List(l) => call.extend(l.iter().cloned()),
            Value::Nil => {}
            _ => return Err("apply: последний аргумент должен быть списком".into()),
        }
        apply(&f, &call, 0)
    });
    let gcell = ctx.global.clone();
    def(env, "eval", move |args| {
        need(args, 1)?;
        let g = gcell.borrow().clone();
        eval(&args[0], &g, 0)
    });
    let gcell2 = ctx.global.clone();
    def(env, "defined?", move |args| {
        need(args, 1)?;
        let name = as_sym(&args[0])?;
        let g = gcell2.borrow().clone();
        Ok(Value::Bool(env_get(&g, &name).is_some()))
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
