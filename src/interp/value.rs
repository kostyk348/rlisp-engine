// src/interp/value.rs — Значения Lisp-интерпретатора, Cons-пары, предикаты и форматирование.
//
// Типы данных Lisp (по ТЗ §4.2):
//   - атомы: Nil, Bool, Int, Float, Sym, Str
//   - списки/коллекции: List (Vec), Cons (GC), Vector, F32Vec, Table
//   - функции/макросы: Lambda (GC), Native
//   - указатели: Ptr

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::gc::{self, Gc};

pub type NativeFn = Rc<dyn Fn(&[Value]) -> Result<Value, String>>;

/// Символ Lisp: разделяемая (`Rc<str>`) строка из интерна-таблицы reader'а.
///
/// Зачем не `String`: парсер корпуса `assets/lisp` строит ~29k символов на
/// 57k токенов, из них уникальны ~2k — `String` давал по аллокации на каждое
/// вхождение (`malloc` + `free` ≈ половина времени разбора). Интерн отдаёт
/// клон `Rc` (инкремент счётчика) без обращения к аллокатору.
///
/// Семантика — ровно строка: `Deref<Target=str>`, `as_str()`, `Display`,
/// сравнения с `str`/`&str`/`String` в обе стороны, `Borrow<str>` для
/// `HashMap<String, _>::get(sym)`.
#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub struct SymStr(Rc<str>);

impl SymStr {
    pub fn new(s: &str) -> Self {
        SymStr(Rc::from(s))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for SymStr {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}
impl AsRef<str> for SymStr {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
impl std::borrow::Borrow<str> for SymStr {
    fn borrow(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Display for SymStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::fmt::Debug for SymStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&*self.0, f)
    }
}
impl From<&str> for SymStr {
    fn from(s: &str) -> Self {
        SymStr(Rc::from(s))
    }
}
impl From<String> for SymStr {
    fn from(s: String) -> Self {
        SymStr(Rc::from(s))
    }
}
impl From<Rc<str>> for SymStr {
    fn from(s: Rc<str>) -> Self {
        SymStr(s)
    }
}
impl From<SymStr> for String {
    fn from(s: SymStr) -> String {
        s.0.to_string()
    }
}
impl PartialEq<str> for SymStr {
    fn eq(&self, other: &str) -> bool {
        &*self.0 == other
    }
}
impl PartialEq<&str> for SymStr {
    fn eq(&self, other: &&str) -> bool {
        &*self.0 == *other
    }
}
impl PartialEq<String> for SymStr {
    fn eq(&self, other: &String) -> bool {
        &*self.0 == other.as_str()
    }
}
impl PartialEq<SymStr> for str {
    fn eq(&self, other: &SymStr) -> bool {
        self == &*other.0
    }
}
impl PartialEq<SymStr> for &str {
    fn eq(&self, other: &SymStr) -> bool {
        *self == &*other.0
    }
}

#[derive(Clone)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// Символ: разделяемая строка (интерн reader'а), см. [`SymStr`].
    Sym(SymStr),
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
        Value::Sym(s) => s.to_string(),
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

pub fn as_sym(v: &Value) -> Result<String, String> {
    match v {
        Value::Sym(s) => Ok(s.to_string()),
        other => Err(format!("ожидался символ: {}", fmt_value(other))),
    }
}

pub fn as_list(v: &Value) -> Result<&Vec<Value>, String> {
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
